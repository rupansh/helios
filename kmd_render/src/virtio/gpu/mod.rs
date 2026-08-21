//! The virtio-gpu device object, built on the `virtio-drivers` PCI transport.
//!
//! `VirtioGpu` owns the `PciTransport` (discovers/maps the virtio config
//! regions) and the control `VirtQueue`, and layers the virtio-gpu command
//! protocol (`helios_protocol`) on top. Built by `init` from
//! `DxgkDdiStartDevice` and stored in `AdapterContext::virtio`.
//!
//! Bring-up (all in `init`, at PASSIVE_LEVEL):
//!   M1 — `DxgkConfigAccess` → `PciRoot` → `PciTransport::new::<WdkHal,_>`
//!   M2 — feature negotiation via the `Transport` trait
//!   M3 — control `VirtQueue::<WdkHal>` setup + DRIVER_OK
//!   M4 — `GET_DISPLAY_INFO` polled round-trip (Phase-2 smoke test)
//!
//! ## C3/M3.4 async transport (2026-07-04)
//!
//! Every control-queue command is a tracked [`InFlight`] entry that OWNS its
//! device-visible DMA buffers until the device returns its descriptor chain on
//! the used ring ([`VirtioGpu::drain_used`], token-matched `peek_used` →
//! `pop_used` — ported from the proven System-class phase4e model in
//! `kmd/src/virtio/gpu.rs`). Nothing in this module ever waits:
//!
//!   * Fenced `SUBMIT_3D` is ASYNC ([`VirtioGpu::enqueue_async_submit`]): the
//!     KMD assigns a globally-monotonic WIRE fence id, queues the descriptors,
//!     notifies, and returns. Completion signals any registered
//!     [`FenceWaiter`] (KEVENT) and advances the WDDM pending FIFO.
//!   * Synchronous verbs (ctx/blob/map) are enqueued with an optional
//!     [`SyncWaitBlock`] waiter ([`VirtioGpu::enqueue_sync`]); the waiter
//!     blocks at PASSIVE_LEVEL in `virtio::ctrl`, NEVER at DISPATCH under the
//!     device spinlock.
//!   * Completed entries are parked (their `DmaBuffer`s are PASSIVE-only to
//!     free) and reaped by PASSIVE callers via [`VirtioGpu::begin_parked_reap`].
//!
//! The used-ring consumer is [`VirtioGpu::drain_used`], called from the
//! interrupt DPC (`ddi/interrupt.rs`) and opportunistically (under the same
//! spinlock) by enqueue paths and by `virtio::ctrl`'s wait slices, so waits
//! survive a lost interrupt with only slice-granularity latency.

use core::cell::UnsafeCell;
use core::ffi::c_void;
use core::marker::PhantomData;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicU32, AtomicU64, AtomicU8, Ordering};

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::vec::Vec;
use bytemuck::Zeroable;
use helios_kmd_logic::control_ownership::HostRejection;
use helios_protocol::{
    HELIOS_OPTIONAL_FEATURES, HELIOS_REQUIRED_FEATURES, VIRTIO_GPU_CMD_GET_DISPLAY_INFO,
    VIRTIO_GPU_CMD_SET_SCANOUT_BLOB, VIRTIO_GPU_CMD_SUBMIT_3D, VIRTIO_GPU_FLAG_FENCE,
    VIRTIO_GPU_FLAG_INFO_RING_IDX, VIRTIO_GPU_RESP_OK_DISPLAY_INFO,
    VIRTIO_GPU_RESP_OK_NODATA, VirtioGpuCmdSubmit, VirtioGpuCtrlHdr,
    VirtioGpuRespDisplayInfo, VirtioGpuSetScanoutBlob,
};
use virtio_drivers::queue::VirtQueue;
use virtio_drivers::transport::pci::PciTransport;
use virtio_drivers::transport::pci::bus::{DeviceFunction, PciRoot};
use virtio_drivers::transport::{DeviceStatus, Transport};
use wdk_sys::ntddk::{
    KeInitializeEvent, KeSetEvent,
};
use wdk_sys::KEVENT;

mod resource_tables;

use super::config::DxgkConfigAccess;
use super::hal::{DmaBuffer, DmaSpan, WdkHal};
use super::pci_caps::{HostVisibleWindow, map_isr_status_register, scan_host_visible_window};

// R1103: the telemetry atomics moved to `super::counters`. Re-exported here so
// all 53+ external `gpu::<COUNTER>` paths keep compiling unchanged; narrowing
// the re-export is a follow-up, not part of the move.
use super::VirtioError;
pub use super::counters::*;
use crate::dxgk::{BOOLEAN, DXGKCB_SYNCHRONIZE_EXECUTION, DXGKRNL_INTERFACE, HANDLE, STATUS_SUCCESS};

/// Control queue index (virtio-gpu controlq = 0; cursorq = 1 is unused).
const CTRL_QUEUE: u16 = 0;
/// Control-queue ring size — power of two, conservatively ≤ the device's max.
const CTRL_QUEUE_SIZE: usize = 64;
/// One page of contiguous DMA scratch for `init`'s inline polled round-trip.
const SCRATCH_BYTES: usize = 4096;
/// Busy-poll bound for `init`'s inline GET_DISPLAY_INFO round-trip — the ONLY
/// polled wait left (PASSIVE, pre-interrupt, single-threaded bring-up; every
/// runtime wait is a PASSIVE KEVENT wait in `virtio::ctrl`). Each iteration is
/// a volatile used-ring read + `spin_loop` (~10 ns) → bound ≈ 1 s.
const CTRL_POLL_SPINS: u64 = 100_000_000;

// ── KMD-internal host-visible blob mapping ───────────────────────────────────

/// Page granularity for blob window offsets/sizes.
pub(super) const BLOB_PAGE: u64 = 4096;
/// Max concurrently-tracked blobs.
///
/// SIZING (2026-07-03 exhaustion incident): a live desktop legitimately holds
/// hundreds of blobs at once — every venus ring/reply/fence shmem of every
/// process, every host-visible DXVK memory chunk, every exported/shared
/// surface, and every KMD-standard GDI redirection surface is one slot. The
/// old cap of 256 filled after ~2 h of desktop churn, at which point every
/// new venus consumer failed guest-side (`vkCreateInstance` →
/// VK_ERROR_OUT_OF_HOST_MEMORY for new processes; dwm lost its device → no
/// IddCx swapchain offers). 8192 slots ≈ 448 KiB of non-paged pool, reserved
/// once at init. Exhaustion is now counted (`BLOB_FULL_REJECTS`) and visible
/// through the bounded OS diagnostic callback; hitting the cap indicates a leak,
/// not a workload.
pub(crate) const MAX_BLOBS: usize = 8192;
/// Max live virtio resources. This covers both Venus blobs and KMD/WDDM standard
/// allocations, so teardown can suppress duplicate RESOURCE_UNREF commands. Must
/// be ≥ MAX_BLOBS (every blob is a live resource; non-blob resources add more).
pub(super) const MAX_RESOURCES: usize = 16384;
/// Max concurrently-tracked virtio-gpu contexts (one per live device, generous).
pub(super) const MAX_CONTEXTS: usize = 1024;
/// Max coalescing free ranges in the window allocator's free list. Overflow
/// drops the freed range (leaks window offset space) — counted in
/// `WINDOW_RANGE_DROPS`.
const MAX_WINDOW_RANGES: usize = 1024;
/// Per-map size cap (also bounds the `IoAllocateMdl` ULONG length on the caller).
const MAX_BLOB_MAP_BYTES: u64 = 256 << 20;

// Rounds `n` up to the next `BLOB_PAGE` multiple (saturating). The body moved to
// `helios_kmd_logic` (host unit-tested, no `wdk-sys` edge); `BLOB_PAGE` stays
// here because the window allocator still uses it directly at `map_blob_prepare`.
use helios_kmd_logic::round_up_page;

/// Result of the under-lock phase of MAP_BLOB ([`VirtioGpu::map_blob_prepare`]):
/// the guest-physical range and host-requested caching for a bounded KMD map.
#[derive(Clone, Copy)]
pub struct BlobMapPrep {
    /// Guest-physical base of the resource's mapping inside the host-visible window.
    pub gpa: u64,
    /// Page-rounded length to map, in bytes.
    pub size: u64,
    /// Host caching nibble (`VIRTIO_GPU_MAP_CACHE_*`) from `RESP_OK_MAP_INFO`.
    pub map_cache: u32,
}

/// One tracked blob resource.
/// Exact non-null dxgkrnl device identity used by K11 session ownership.
/// `Option<DeviceOwner>` remains niche-optimised: `None` is KMD-owned and
/// `Some` is an exact session/device owner, never a process or name lookup.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct DeviceOwner(core::num::NonZeroUsize);

impl DeviceOwner {
    /// `None` for a null handle — the caller must refuse rather than substitute.
    pub fn new(raw: usize) -> Option<Self> {
        core::num::NonZeroUsize::new(raw).map(Self)
    }
}

/// Which slots a blob lookup may match.
///
/// `Any` is a kernel-only resource-id lookup; `Exactly(None)` selects only
/// KMD-owned backing and `Exactly(Some(owner))` selects one exact K11 owner.
#[derive(Clone, Copy)]
pub enum OwnerFilter {
    Any,
    Exactly(Option<DeviceOwner>),
}

#[derive(Clone, Copy)]
struct BlobSlot {
    /// `Some(device)` is an exact K11 session owner; `None` is KMD-owned Venus
    /// infrastructure or an allocation backing. DestroyDevice drains the
    /// session namespace before sweeping any surviving owner rows.
    owner: Option<DeviceOwner>,
    ctx_id: u32,
    resource_id: u32,
    /// Blob size in bytes (from ALLOC_BLOB; MAP_BLOB needs it to size the MDL).
    size: u64,
    /// RESOURCE_MAP_BLOB succeeded and must be paired with RESOURCE_UNMAP_BLOB.
    mapped: bool,
    /// A RESOURCE_MAP_BLOB round-trip is in flight for this slot (the window
    /// range is reserved; concurrent mappers must wait — see `blob_map_begin`).
    map_pending: bool,
    /// Host caching nibble from RESP_OK_MAP_INFO (valid once `mapped`).
    map_cache: u32,
    /// Host-visible window offset used for RESOURCE_MAP_BLOB.
    map_offset: u64,
    /// Rounded mapped length in the host-visible window.
    map_len: u64,
}

/// A free span in the host-visible window's offset space (bump + coalescing free).
#[derive(Clone, Copy)]
struct WindowRange {
    offset: u64,
    len: u64,
}

/// One tracked virtio-gpu context, tagged with the owning device handle for
/// device-teardown reclamation.
#[derive(Clone, Copy)]
struct ContextSlot {
    /// Owning device, or `None` for the KMD's own persistent venus context.
    owner: Option<DeviceOwner>,
    ctx_id: u32,
}

// ── C3/M3.4 async submission machinery ───────────────────────────────────────

/// Max in-flight control-queue entries. Tokens are descriptor-chain heads, so
/// they are always `< CTRL_QUEUE_SIZE`; each chain uses ≥ 2 descriptors, which
/// caps real concurrency at half this.
pub const MAX_INFLIGHT: usize = CTRL_QUEUE_SIZE;
/// Parked (completed, awaiting PASSIVE free) entry capacity. Enqueues are
/// refused once `parked` crosses [`PARKED_ENQUEUE_GATE`], and one drain can
/// park at most `MAX_INFLIGHT` entries, so this bound is never exceeded.
pub const MAX_PARKED: usize = 4 * MAX_INFLIGHT;
/// Completed command buffers retained for reuse. Count, individual capacity,
/// and total bytes are all bounded: a rare large Venus CS must never pin a
/// correspondingly large physically-contiguous allocation for device lifetime.
const MAX_DMA_POOL: usize = 128;
const MAX_DMA_POOL_BUFFER_BYTES: usize = 64 * 1024;
const MAX_DMA_POOL_BYTES: usize = 2 * 1024 * 1024;
/// Enqueue refusal threshold for the parked table (forces the PASSIVE caller
/// to reap before submitting more).
const PARKED_ENQUEUE_GATE: usize = MAX_PARKED - MAX_INFLIGHT;
/// Max concurrent WAIT_FENCE waiters.
const MAX_FENCE_WAITERS: usize = 64;
/// Max WDDM submissions pending on venus completion.
const MAX_WDDM_PENDING: usize =
    helios_kmd_logic::ordered_engine::MAX_ORDERED_ENGINE_SUBMISSIONS;
const _: () = assert!(
    MAX_WDDM_PENDING
        == helios_protocol::translation_session::HELIOS_HTS1_MAX_HOST_DISPATCH_FIFO_DEPTH
            as usize
);
/// Max response bytes a synchronous command may expect (copied into the
/// waiter's [`SyncWaitBlock`]; the largest runtime response is
/// `VirtioGpuRespMapInfo`. `init`'s big GET_DISPLAY_INFO reply stays on the
/// inline polled path and does not ride this machinery).
pub const SYNC_RESP_MAX: usize = 64;
/// Bytes for an async submit's metadata buffer: the device-read SUBMIT_3D
/// header followed by the device-written ctrl response.
pub const SUBMIT_META_BYTES: usize =
    core::mem::size_of::<VirtioGpuCmdSubmit>() + core::mem::size_of::<VirtioGpuCtrlHdr>();
/// Bytes for one fixed direct-plane command buffer: the device-read
/// `SET_SCANOUT_BLOB` followed by the device-written ctrl response.
pub const BIND_CMD_BYTES: usize =
    core::mem::size_of::<VirtioGpuSetScanoutBlob>() + core::mem::size_of::<VirtioGpuCtrlHdr>();
/// Fixed direct-plane queue depth. Every slot owns one command buffer for its
/// complete transport lifetime; no buffer is shared or recycled through an
/// unrelated control path.
const BIND_CMD_POOL: usize = 4;

/// The D4 half of each transport generation uses the upper half of the ordinary
/// per-instance fence range. This keeps fixed DIRQL SETs disjoint from legacy
/// user/control fence ids without creating a second unbounded namespace.
const D4_FENCE_OFFSET: u64 = WIRE_FENCE_INSTANCE_STRIDE / 2;

#[derive(Clone, Copy)]
struct DirectQueueIdentity {
    instance: u64,
    fence_id: u64,
    sequence: u64,
    resource_id: u32,
}

/// One preallocated descriptor owner for an exact D4 classic/DMA binding.
struct DirectQueueSlot {
    buffer: DmaBuffer,
    token: Option<u16>,
    work: Option<crate::ddi::direct_scanout::QueuedDirectScanoutBinding>,
    identity: Option<DirectQueueIdentity>,
}

/// Result moved out of one fixed slot only after `pop_used` has retired its
/// exact descriptor chain.
pub(crate) struct DirectQueueCompletion {
    pub(crate) work: crate::ddi::direct_scanout::QueuedDirectScanoutBinding,
    pub(crate) instance: u64,
    pub(crate) fence_id: u64,
    pub(crate) sequence: u64,
    pub(crate) resource_id: u32,
    pub(crate) written_length: u32,
    pub(crate) response: [u8; core::mem::size_of::<VirtioGpuCtrlHdr>()],
}

struct InterruptQueueCore {
    transport: PciTransport,
    control: VirtQueue<WdkHal, CTRL_QUEUE_SIZE>,
    direct_slots: Box<[DirectQueueSlot]>,
    next_direct_fence: u64,
    direct_fence_limit: u64,
    transport_instance: u64,
}

/// The one mutable control-queue core. Every mutator takes the same bounded
/// atomic exclusion gate; lower-IRQL callers additionally enter through
/// dxgkrnl's interrupt synchronization callback.
///
/// `enqueue_direct_at_dirql` never assumes that being called at DIRQL also owns
/// dxgkrnl's interrupt lock. Its private proof only grants a one-shot attempt at
/// this nonblocking gate. Contention refuses the flip without spinning.
pub(crate) struct InterruptQueue {
    core: UnsafeCell<InterruptQueueCore>,
    synchronize: DXGKCB_SYNCHRONIZE_EXECUTION,
    device_handle: HANDLE,
    failed: AtomicU32,
    access: AtomicU32,
    notify_pending: AtomicU32,
}

// SAFETY: every access to `core` is serialized by `access`. The DIRQL entry can
// only try that gate with its non-forgeable capability; every other entry is
// hidden behind `DxgkCbSynchronizeExecution` and then takes the same gate.
unsafe impl Sync for InterruptQueue {}

struct InterruptQueueAccess<'a> {
    queue: &'a InterruptQueue,
}

impl InterruptQueueAccess<'_> {
    fn release(self) {
        self.queue.release_access();
    }

    fn release_after_reset(self) {
        // A physical reset invalidates every queued notification. Unlike the
        // ordinary release path, this must not ring a reset device.
        self.queue.notify_pending.store(0, Ordering::Release);
        self.queue.access.store(0, Ordering::Release);
    }
}

#[derive(Clone, Copy)]
enum QueueCallResult {
    Pending,
    Added { token: u16, sequence: u64 },
    QueueFull,
    SequenceExhausted,
    Failed,
    Peek(Option<u16>),
    Popped(u32),
    Direct(bool),
    Holds(bool),
    Notified,
}

enum QueueOperation {
    Add {
        core: *mut InterruptQueueCore,
        reads: [DmaSpan; 2],
        count: usize,
        response: DmaSpan,
        sequence_adapter: *const crate::adapter::AdapterContext,
        sequence_resource: u32,
        result: *mut QueueCallResult,
    },
    Peek {
        core: *mut InterruptQueueCore,
        result: *mut QueueCallResult,
    },
    Pop {
        core: *mut InterruptQueueCore,
        token: u16,
        reads: [DmaSpan; 2],
        count: usize,
        response: DmaSpan,
        result: *mut QueueCallResult,
    },
    PopDirect {
        core: *mut InterruptQueueCore,
        token: u16,
        output: *mut Option<DirectQueueCompletion>,
        result: *mut QueueCallResult,
    },
    EnqueueDirect {
        queue: *const InterruptQueue,
        adapter: *const crate::adapter::AdapterContext,
        work: *mut Option<crate::ddi::direct_scanout::QueuedDirectScanoutBinding>,
        result: *mut QueueCallResult,
    },
    DirectHolds {
        core: *mut InterruptQueueCore,
        handle: usize,
        generation: u64,
        resource_id: u32,
        result: *mut QueueCallResult,
    },
    Notify {
        result: *mut QueueCallResult,
    },
}

struct QueueInvocation {
    queue: *const InterruptQueue,
    operation: *mut QueueOperation,
}

unsafe extern "C" fn interrupt_queue_operation(context: *mut c_void) -> BOOLEAN {
    if context.is_null() {
        return 0;
    }
    // SAFETY: every invocation is stack-owned by `InterruptQueue::synchronize`
    // and both pointees remain live for this synchronous callback.
    let invocation = unsafe { &mut *(context as *mut QueueInvocation) };
    let Some(queue) = (unsafe { invocation.queue.as_ref() }) else {
        return 0;
    };
    let Some(operation) = (unsafe { invocation.operation.as_mut() }) else {
        return 0;
    };
    let Some(access) = queue.try_access() else {
        let result = match operation {
            QueueOperation::Add { result, .. }
            | QueueOperation::Peek { result, .. }
            | QueueOperation::Pop { result, .. }
            | QueueOperation::PopDirect { result, .. }
            | QueueOperation::EnqueueDirect { result, .. }
            | QueueOperation::DirectHolds { result, .. }
            | QueueOperation::Notify { result, .. } => result,
        };
        unsafe { **result = QueueCallResult::QueueFull };
        return 1;
    };
    let handled = (|| {
        match operation {
        QueueOperation::Add {
            core,
            reads,
            count,
            response,
            sequence_adapter,
            sequence_resource,
            result,
        } => {
            let sequence = if sequence_adapter.is_null() {
                0
            } else {
                let Some(sequence) = (unsafe { &**sequence_adapter }).reserve_scanout_bind_seq()
                else {
                    unsafe { **result = QueueCallResult::SequenceExhausted };
                    return 1;
                };
                sequence
            };
            let read_slices = unsafe { [reads[0].as_slice(), reads[1].as_slice()] };
            let added = unsafe {
                (**core)
                    .control
                    .add(&read_slices[..*count], &mut [response.as_mut_slice()])
            };
            unsafe {
                **result = match added {
                    Ok(token) => {
                        if !sequence_adapter.is_null() {
                            (&**sequence_adapter)
                                .commit_scanout_bind_seq(sequence, *sequence_resource);
                        }
                        QueueCallResult::Added { token, sequence }
                    }
                    Err(virtio_drivers::Error::QueueFull) => QueueCallResult::QueueFull,
                    Err(_) => QueueCallResult::Failed,
                }
            };
        }
        QueueOperation::Peek { core, result } => unsafe {
            **result = QueueCallResult::Peek((**core).control.peek_used());
        },
        QueueOperation::Pop {
            core,
            token,
            reads,
            count,
            response,
            result,
        } => {
            let read_slices = unsafe { [reads[0].as_slice(), reads[1].as_slice()] };
            let popped = unsafe {
                (**core).control.pop_used(
                    *token,
                    &read_slices[..*count],
                    &mut [response.as_mut_slice()],
                )
            };
            unsafe {
                **result = match popped {
                    Ok(length) => QueueCallResult::Popped(length),
                    Err(_) => QueueCallResult::Failed,
                }
            };
        }
        QueueOperation::PopDirect {
            core,
            token,
            output,
            result,
        } => unsafe {
            let core = &mut **core;
            let Some(index) = core
                .direct_slots
                .iter()
                .position(|slot| slot.token == Some(*token))
            else {
                **result = QueueCallResult::Direct(false);
                return 1;
            };
            let slot = &mut core.direct_slots[index];
            let in0_len = core::mem::size_of::<VirtioGpuSetScanoutBlob>();
            let resp_len = core::mem::size_of::<VirtioGpuCtrlHdr>();
            let Some(read) = slot.buffer.span(0, in0_len) else {
                **result = QueueCallResult::Failed;
                return 1;
            };
            let Some(response_span) = slot.buffer.span(in0_len, resp_len) else {
                **result = QueueCallResult::Failed;
                return 1;
            };
            let reads = [read.as_slice(), DmaSpan::EMPTY.as_slice()];
            let popped = core
                .control
                .pop_used(*token, &reads[..1], &mut [response_span.as_mut_slice()]);
            let Ok(written_length) = popped else {
                **result = QueueCallResult::Failed;
                return 1;
            };
            if written_length as usize > resp_len {
                **result = QueueCallResult::Failed;
                return 1;
            }
            let Some(work) = slot.work.take() else {
                **result = QueueCallResult::Failed;
                return 1;
            };
            let Some(identity) = slot.identity.take() else {
                // Preserve ambiguous custody rather than drop the candidate when
                // its queue identity is corrupt.
                slot.work = Some(work);
                **result = QueueCallResult::Failed;
                return 1;
            };
            let mut response = [0u8; core::mem::size_of::<VirtioGpuCtrlHdr>()];
            response[..written_length as usize].copy_from_slice(
                &slot.buffer.as_slice()[in0_len..in0_len + written_length as usize],
            );
            slot.token = None;
            **output = Some(DirectQueueCompletion {
                work,
                instance: identity.instance,
                fence_id: identity.fence_id,
                sequence: identity.sequence,
                resource_id: identity.resource_id,
                written_length,
                response,
            });
            **result = QueueCallResult::Direct(true);
        },
        QueueOperation::EnqueueDirect {
            queue,
            adapter,
            work,
            result,
        } => unsafe {
            let Some(work) = (&mut **work).take() else {
                **result = QueueCallResult::Failed;
                return 1;
            };
            **result = match (&**queue).enqueue_direct_locked(&**adapter, work) {
                Ok(()) => QueueCallResult::Direct(true),
                Err(VirtioError::QueueFull) => QueueCallResult::QueueFull,
                Err(VirtioError::BindSequenceExhausted) => {
                    QueueCallResult::SequenceExhausted
                }
                Err(_) => QueueCallResult::Failed,
            };
        },
        QueueOperation::DirectHolds {
            core,
            handle,
            generation,
            resource_id,
            result,
        } => unsafe {
            **result = QueueCallResult::Holds((**core).direct_slots.iter().any(|slot| {
                slot.work.as_ref().is_some_and(|work| {
                    work.matches_exact_allocation(*handle, *generation, *resource_id)
                })
            }));
        },
        QueueOperation::Notify { result, .. } => unsafe {
            **result = QueueCallResult::Notified;
        },
        }
        1
    })();
    access.release();
    handled
}

impl InterruptQueue {
    fn new(
        transport: PciTransport,
        control: VirtQueue<WdkHal, CTRL_QUEUE_SIZE>,
        synchronize: DXGKCB_SYNCHRONIZE_EXECUTION,
        device_handle: HANDLE,
        transport_instance: u64,
        direct_buffers: Vec<DmaBuffer>,
        wire_fence_base: u64,
    ) -> Result<Self, VirtioError> {
        let Some(next_direct_fence) = wire_fence_base.checked_add(D4_FENCE_OFFSET) else {
            return Err(VirtioError::WireFenceNamespaceExhausted);
        };
        let Some(direct_fence_limit) = wire_fence_base.checked_add(WIRE_FENCE_INSTANCE_STRIDE)
        else {
            return Err(VirtioError::WireFenceNamespaceExhausted);
        };
        let direct_slots = direct_buffers
            .into_iter()
            .map(|buffer| DirectQueueSlot {
                buffer,
                token: None,
                work: None,
                identity: None,
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Ok(Self {
            core: UnsafeCell::new(InterruptQueueCore {
                transport,
                control,
                direct_slots,
                next_direct_fence,
                direct_fence_limit,
                transport_instance,
            }),
            synchronize,
            device_handle,
            failed: AtomicU32::new(0),
            access: AtomicU32::new(0),
            notify_pending: AtomicU32::new(0),
        })
    }

    fn try_access(&self) -> Option<InterruptQueueAccess<'_>> {
        self.access
            .compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| InterruptQueueAccess { queue: self })
    }

    fn release_access(&self) {
        // SAFETY: the caller's guard uniquely owns `access`, so it is the only
        // code that may touch the queue core. A notification deferred by a
        // contending lower-IRQL caller is folded into this exact section.
        let core = unsafe { &mut *self.core.get() };
        if self.notify_pending.swap(0, Ordering::AcqRel) != 0
            && core.control.should_notify()
        {
            core.transport.notify(CTRL_QUEUE);
        }
        self.access.store(0, Ordering::Release);
    }

    fn synchronize(&self, operation: &mut QueueOperation) -> Result<QueueCallResult, VirtioError> {
        let Some(synchronize) = self.synchronize else {
            return Err(VirtioError::DeviceError);
        };
        let mut returned: BOOLEAN = 0;
        let mut invocation = QueueInvocation {
            queue: self,
            operation,
        };
        let status = unsafe {
            synchronize(
                self.device_handle,
                Some(interrupt_queue_operation),
                &mut invocation as *mut QueueInvocation as *mut c_void,
                0,
                &mut returned,
            )
        };
        if status != STATUS_SUCCESS || returned == 0 {
            return Err(VirtioError::DeviceError);
        }
        let result = match operation {
            QueueOperation::Add { result, .. }
            | QueueOperation::Peek { result, .. }
            | QueueOperation::Pop { result, .. }
            | QueueOperation::PopDirect { result, .. }
            | QueueOperation::EnqueueDirect { result, .. }
            | QueueOperation::DirectHolds { result, .. }
            | QueueOperation::Notify { result, .. } => unsafe { **result },
        };
        Ok(result)
    }

    fn add(
        &self,
        reads: [DmaSpan; 2],
        count: usize,
        response: DmaSpan,
        sequence: Option<(&crate::adapter::AdapterContext, u32)>,
    ) -> Result<(u16, Option<u64>), VirtioError> {
        let mut result = QueueCallResult::Pending;
        let (sequence_adapter, sequence_resource) = sequence
            .map_or((core::ptr::null(), 0), |(adapter, resource)| {
                (adapter as *const _, resource)
            });
        let mut operation = QueueOperation::Add {
            core: self.core.get(),
            reads,
            count,
            response,
            sequence_adapter,
            sequence_resource,
            result: &mut result,
        };
        match self.synchronize(&mut operation)? {
            QueueCallResult::Added { token, sequence } => {
                Ok((token, (sequence != 0).then_some(sequence)))
            }
            QueueCallResult::QueueFull => Err(VirtioError::QueueFull),
            QueueCallResult::SequenceExhausted => Err(VirtioError::BindSequenceExhausted),
            _ => Err(VirtioError::DeviceError),
        }
    }

    fn notify(&self) -> Result<(), VirtioError> {
        self.notify_pending.store(1, Ordering::Release);
        let mut result = QueueCallResult::Pending;
        let mut operation = QueueOperation::Notify {
            result: &mut result,
        };
        match self.synchronize(&mut operation)? {
            // QueueFull means another CPU owned the queue after this caller set
            // `notify_pending`. That owner normally folds the notification into
            // its release, but there is a boundary race where it may already have
            // sampled the bit. Never report success from that ambiguous edge.
            QueueCallResult::Notified => Ok(()),
            QueueCallResult::QueueFull => Err(VirtioError::QueueFull),
            _ => Err(VirtioError::DeviceError),
        }
    }

    fn peek_used(&self) -> Result<Option<u16>, VirtioError> {
        let mut result = QueueCallResult::Pending;
        let mut operation = QueueOperation::Peek {
            core: self.core.get(),
            result: &mut result,
        };
        match self.synchronize(&mut operation)? {
            QueueCallResult::Peek(token) => Ok(token),
            QueueCallResult::QueueFull => Err(VirtioError::QueueFull),
            _ => Err(VirtioError::DeviceError),
        }
    }

    fn pop_used(
        &self,
        token: u16,
        reads: [DmaSpan; 2],
        count: usize,
        response: DmaSpan,
    ) -> Result<u32, VirtioError> {
        let mut result = QueueCallResult::Pending;
        let mut operation = QueueOperation::Pop {
            core: self.core.get(),
            token,
            reads,
            count,
            response,
            result: &mut result,
        };
        match self.synchronize(&mut operation)? {
            QueueCallResult::Popped(length) => Ok(length),
            QueueCallResult::QueueFull => Err(VirtioError::QueueFull),
            _ => Err(VirtioError::DeviceError),
        }
    }

    pub(crate) fn reset_status_and_poll(
        &self,
        _passive: crate::irql::PassiveLevel,
        expected_instance: u64,
    ) -> Result<(u32, u32), VirtioError> {
        let Some(access) = self.try_access() else {
            return Err(VirtioError::QueueFull);
        };
        // PCI transport status reads/writes may reach DxgkCb{Read,Write}DeviceSpace.
        // They therefore stay at the caller's proven PASSIVE_LEVEL and are
        // deliberately absent from `interrupt_queue_operation`. The same
        // nonblocking gate still excludes a crossed direct queue publisher.
        let core = unsafe { &mut *self.core.get() };
        if expected_instance == 0 || core.transport_instance != expected_instance {
            access.release();
            return Err(VirtioError::DeviceError);
        }
        core.transport.set_status(DeviceStatus::empty());
        let mut spins = 0u32;
        let mut status = core.transport.get_status();
        while !status.is_empty() && spins < 100_000 {
            spins += 1;
            core::hint::spin_loop();
            status = core.transport.get_status();
        }
        // Seal every producer before releasing exclusion. Even if lifecycle
        // serialization were violated, no command may enter the reset device
        // between this PASSIVE phase and under-lock bookkeeping retirement.
        self.mark_failed();
        // Reset invalidates every pending notification. Do not route release
        // through `release_access`, which could ring a queue after device reset.
        access.release_after_reset();
        Ok((status.bits(), spins))
    }

    fn mark_failed(&self) {
        self.failed.store(1, Ordering::Release);
    }

    fn is_failed(&self) -> bool {
        self.failed.load(Ordering::Acquire) != 0
    }

    pub(crate) unsafe fn enqueue_direct_at_dirql(
        &self,
        proof: &crate::ddi::display::SetVidPnDirql<'_>,
        adapter: &crate::adapter::AdapterContext,
        work: crate::ddi::direct_scanout::QueuedDirectScanoutBinding,
    ) -> Result<(), VirtioError> {
        if !proof.authorizes(adapter) || self.is_failed() {
            return Err(VirtioError::DeviceError);
        }
        let Some(access) = self.try_access() else {
            return Err(VirtioError::QueueFull);
        };
        // SAFETY: the private proof confines this call to the classic DDI's
        // above-DISPATCH arm; `access` independently excludes every queue
        // mutator without waiting or assuming an implicit interrupt lock.
        let result = unsafe { self.enqueue_direct_locked(adapter, work) };
        access.release();
        result
    }

    fn enqueue_direct_at_dispatch(
        &self,
        adapter: &crate::adapter::AdapterContext,
        work: crate::ddi::direct_scanout::QueuedDirectScanoutBinding,
    ) -> Result<(), VirtioError> {
        if self.is_failed() {
            return Err(VirtioError::DeviceError);
        }
        let mut result = QueueCallResult::Pending;
        let mut work = Some(work);
        let mut operation = QueueOperation::EnqueueDirect {
            queue: self,
            adapter,
            work: &mut work,
            result: &mut result,
        };
        match self.synchronize(&mut operation)? {
            QueueCallResult::Direct(true) => Ok(()),
            QueueCallResult::QueueFull => Err(VirtioError::QueueFull),
            QueueCallResult::SequenceExhausted => Err(VirtioError::BindSequenceExhausted),
            _ => Err(VirtioError::DeviceError),
        }
    }

    unsafe fn enqueue_direct_locked(
        &self,
        adapter: &crate::adapter::AdapterContext,
        work: crate::ddi::direct_scanout::QueuedDirectScanoutBinding,
    ) -> Result<(), VirtioError> {
        let core = unsafe { &mut *self.core.get() };
        if work.transport_instance() != core.transport_instance
            || core.next_direct_fence >= core.direct_fence_limit
        {
            return Err(VirtioError::DeviceError);
        }
        let Some(slot) = core
            .direct_slots
            .iter_mut()
            .find(|slot| slot.token.is_none() && slot.work.is_none())
        else {
            return Err(VirtioError::QueueFull);
        };
        let resource_id = work.resource_id();
        let in0_len = core::mem::size_of::<VirtioGpuSetScanoutBlob>();
        let resp_len = core::mem::size_of::<VirtioGpuCtrlHdr>();
        if !slot.buffer.reset(in0_len + resp_len) {
            return Err(VirtioError::DeviceError);
        }
        // SAFETY: every DmaBuffer begins at a page-aligned contiguous-memory
        // address, this command starts at offset zero, and reset above proved
        // the complete command fits. Avoid a dependency helper in the audited
        // DIRQL path: this is exactly one aligned typed view of those bytes.
        let command = unsafe {
            &mut *slot
                .buffer
                .as_mut_slice()
                .as_mut_ptr()
                .cast::<VirtioGpuSetScanoutBlob>()
        };
        super::ctrl::fill_set_scanout_blob(
            command,
            resource_id,
            work.width(),
            work.height(),
            work.format(),
            work.stride(),
            work.offset(),
        );
        command.hdr.flags = VIRTIO_GPU_FLAG_FENCE;
        command.hdr.fence_id = core.next_direct_fence;
        let Some(sequence) = adapter.reserve_scanout_bind_seq() else {
            return Err(VirtioError::BindSequenceExhausted);
        };
        let fence_id = core.next_direct_fence;
        let reads = [
            slot.buffer
                .span(0, in0_len)
                .ok_or(VirtioError::DeviceError)?,
            DmaSpan::EMPTY,
        ];
        let response = slot
            .buffer
            .span(in0_len, resp_len)
            .ok_or(VirtioError::DeviceError)?;
        // Retain the move-only candidate before publishing avail.idx. A refused
        // add takes it back below; an accepted descriptor can therefore never
        // exist without exact candidate custody.
        slot.work = Some(work);
        let read_slices = unsafe { [reads[0].as_slice(), reads[1].as_slice()] };
        let added = unsafe {
            core.control
                .add(&read_slices[..1], &mut [response.as_mut_slice()])
        };
        let token = match added {
            Ok(token) => token,
            Err(virtio_drivers::Error::QueueFull) => {
                let _ = slot.work.take();
                return Err(VirtioError::QueueFull);
            }
            Err(_) => {
                let _ = slot.work.take();
                self.mark_failed();
                return Err(VirtioError::DeviceError);
            }
        };
        adapter.commit_scanout_bind_seq(sequence, resource_id);
        slot.token = Some(token);
        slot.identity = Some(DirectQueueIdentity {
            instance: core.transport_instance,
            fence_id,
            sequence,
            resource_id,
        });
        core.next_direct_fence += 1;
        self.notify_pending.store(1, Ordering::Release);
        Ok(())
    }

    fn take_direct_completion(
        &self,
        token: u16,
    ) -> Result<Option<DirectQueueCompletion>, VirtioError> {
        let mut result = QueueCallResult::Pending;
        let mut output = None;
        let mut operation = QueueOperation::PopDirect {
            core: self.core.get(),
            token,
            output: &mut output,
            result: &mut result,
        };
        match self.synchronize(&mut operation)? {
            QueueCallResult::Direct(_) => Ok(output),
            QueueCallResult::QueueFull => Err(VirtioError::QueueFull),
            _ => Err(VirtioError::DeviceError),
        }
    }

    fn direct_holds_allocation(
        &self,
        handle: usize,
        generation: u64,
        resource_id: u32,
    ) -> Result<bool, VirtioError> {
        let mut result = QueueCallResult::Pending;
        let mut operation = QueueOperation::DirectHolds {
            core: self.core.get(),
            handle,
            generation,
            resource_id,
            result: &mut result,
        };
        match self.synchronize(&mut operation)? {
            QueueCallResult::Holds(holds) => Ok(holds),
            _ => Err(VirtioError::DeviceError),
        }
    }
}

/// `NotificationEvent` (`EVENT_TYPE` value 0): stays signaled until cleared —
/// the right semantics for one-shot completion events.
const NOTIFICATION_EVENT: i32 = 0;
/// `IO_NO_INCREMENT` priority boost for `KeSetEvent`.
const IO_NO_INCREMENT: i32 = 0;

/// Exact terminal cause published to one synchronous or fence waiter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum WaitDisposition {
    Pending = 0,
    HostResponseAvailable = 1,
    FenceCompleted = 2,
    TransportAborted = 3,
    MalformedResponse = 4,
}

impl WaitDisposition {
    fn from_raw(raw: u8) -> Self {
        match raw {
            0 => Self::Pending,
            1 => Self::HostResponseAvailable,
            2 => Self::FenceCompleted,
            3 => Self::TransportAborted,
            4 => Self::MalformedResponse,
            _ => Self::TransportAborted,
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct SyncResponseObservation {
    disposition: WaitDisposition,
    written_length: u32,
}

impl SyncResponseObservation {
    pub(crate) const fn disposition(self) -> WaitDisposition {
        self.disposition
    }

    pub(crate) const fn written_length(self) -> u32 {
        self.written_length
    }
}

/// A PASSIVE waiter's completion block. Lives on the waiter's stack; the
/// registered pointer stays valid because the waiter ALWAYS deregisters (or
/// observes completion) under the device spinlock before returning.
pub struct SyncWaitBlock {
    /// Signaled (under the device spinlock) when the entry completes.
    pub event: KEVENT,
    /// First terminal disposition, Release-published before signaling.
    disposition: AtomicU8,
    /// Exact used-ring length for this response descriptor.
    response_written: AtomicU32,
    /// The device-written response bytes, copied out of the entry's DMA buffer
    /// by `drain_used` before the event is signaled.
    resp: UnsafeCell<[u8; SYNC_RESP_MAX]>,
}

impl SyncWaitBlock {
    /// Run `f` with a wait block that is zeroed and initialised in place on
    /// THIS frame and is never nameable by the caller.
    ///
    /// The three invariants — initialised before registration, never moved
    /// after, always deregistered before the frame dies — used to be carried by
    /// comments over a `new_zeroed()` -> `unsafe { init() }` ->
    /// `NonNull::from(&mut block)` dance at two call sites. A KEVENT dispatcher
    /// header is self-referential, so a move after `init` corrupts the wait list
    /// silently, and BOTH misuses compiled with zero `unsafe`, because
    /// `enqueue_sync` and `fence_wait_prepare` are safe fns taking a
    /// `NonNull<SyncWaitBlock>`:
    ///
    ///     let mut b = SyncWaitBlock::new_zeroed();
    ///     enqueue_sync(.., NonNull::from(&mut b));     // never init'ed
    ///
    /// and so did returning the block from a helper between `init` and
    /// registration. Neither is expressible now: the value has no name outside
    /// this function.
    ///
    /// Deregistration still depends on the caller's control flow, which is why
    /// the abandon/cancel logic belongs in the closure's own epilogue.
    pub fn with<R>(f: impl FnOnce(&WaitBlockRef<'_>) -> R) -> R {
        let mut block = Self::new_zeroed();
        // SAFETY: `block` is a local of THIS frame, so it is at its final
        // address, and it is never moved afterwards — `f` only ever sees a
        // `WaitBlockRef` borrowing it. It outlives the call to `f`.
        unsafe { block.init() };
        let block_ref = WaitBlockRef {
            ptr: NonNull::from(&mut block),
            _frame: PhantomData,
        };
        f(&block_ref)
    }

    /// A zeroed block. Private: reachable only through [`Self::with`], which is
    /// what makes "registered but never initialised" unrepresentable.
    fn new_zeroed() -> Self {
        // SAFETY: a zeroed KEVENT/AtomicU8/byte-array is a valid *inert*
        // value; `init` initializes the dispatcher header before any use.
        unsafe { core::mem::zeroed() }
    }

    /// Initialize the embedded KEVENT (NotificationEvent, unsignaled).
    ///
    /// # Safety
    /// `self` must be at its final (pinned) address.
    unsafe fn init(&mut self) {
        // SAFETY: valid, stable KEVENT storage per the fn contract.
        unsafe { KeInitializeEvent(&mut self.event, NOTIFICATION_EVENT, 0) };
        self.disposition
            .store(WaitDisposition::Pending as u8, Ordering::Relaxed);
        self.response_written.store(0, Ordering::Relaxed);
    }

    fn disposition(&self) -> WaitDisposition {
        WaitDisposition::from_raw(self.disposition.load(Ordering::Acquire))
    }

    fn publish_terminal(&self, terminal: WaitDisposition) -> bool {
        if terminal == WaitDisposition::Pending {
            return false;
        }
        self.disposition
            .compare_exchange(
                WaitDisposition::Pending as u8,
                terminal as u8,
                Ordering::Release,
                Ordering::Acquire,
            )
            .is_ok()
    }

    fn copy_host_response(&self, out: &mut [u8]) -> SyncResponseObservation {
        let disposition = self.disposition();
        let written_length = self.response_written.load(Ordering::Acquire);
        if disposition != WaitDisposition::HostResponseAvailable {
            return SyncResponseObservation {
                disposition,
                written_length,
            };
        }
        let n = written_length as usize;
        if n > out.len() || n > SYNC_RESP_MAX {
            return SyncResponseObservation {
                disposition: WaitDisposition::MalformedResponse,
                written_length,
            };
        }
        // SAFETY: the Acquire disposition observes the response copy that
        // preceded its Release publication.
        let src = unsafe { &*self.resp.get() };
        out[..n].copy_from_slice(&src[..n]);
        SyncResponseObservation {
            disposition,
            written_length,
        }
    }
}

/// The only handle a [`SyncWaitBlock::with`] closure gets: a pointer to hand
/// the transport, plus the one read the waiter needs. Borrows the block, so it
/// cannot outlive the frame the block lives on.
pub struct WaitBlockRef<'a> {
    ptr: NonNull<SyncWaitBlock>,
    _frame: PhantomData<&'a SyncWaitBlock>,
}

impl WaitBlockRef<'_> {
    /// The registration pointer for `enqueue_sync` / `fence_wait_prepare`.
    pub fn as_ptr(&self) -> NonNull<SyncWaitBlock> {
        self.ptr
    }

    /// # Safety
    /// The signal must have completed, or deregistration under `virtio_lock`
    /// must have proved that the terminal publisher finished with this block.
    pub(crate) unsafe fn copy_host_response_after_completion(
        &self,
        out: &mut [u8],
    ) -> SyncResponseObservation {
        // SAFETY: the caller contract proves the raw target is no longer touched.
        unsafe { self.ptr.as_ref() }.copy_host_response(out)
    }

    /// # Safety
    /// Same terminal signal/deregistration proof as
    /// [`Self::copy_host_response_after_completion`].
    pub unsafe fn classify_fence_after_completion(&self) -> WaitDisposition {
        // SAFETY: the caller contract proves the raw target is no longer touched.
        unsafe { self.ptr.as_ref() }.disposition()
    }
}

/// What an in-flight entry is.
enum InFlightKind {
    /// A synchronous control command; `waiter` is cleared if its stack owner
    /// abandons the wait before the descriptor completes.
    Sync {
        waiter: Option<NonNull<SyncWaitBlock>>,
    },
    /// A fenced SUBMIT_3D carrying the KMD-assigned wire identity.
    AsyncVenus {
        fence_id: u64,
        ring_idx: u8,
        native_completion: Option<crate::ddi::native_render::NativeHostCompletion>,
    },
}

fn take_native_completion(
    kind: &mut InFlightKind,
) -> Option<crate::ddi::native_render::NativeHostCompletion> {
    match kind {
        InFlightKind::AsyncVenus {
            native_completion,
            ..
        } => native_completion.take(),
        _ => None,
    }
}

fn has_native_completion(kind: &InFlightKind) -> bool {
    matches!(
        kind,
        InFlightKind::AsyncVenus {
            native_completion: Some(_),
            ..
        }
    )
}

/// One outstanding control-queue submission. Owns its device-visible buffers
/// for as long as the device may DMA them (until `pop_used`); afterwards the
/// entry is parked and dropped at PASSIVE_LEVEL (DmaBuffer frees are
/// PASSIVE-only).
pub struct InFlight {
    /// Descriptor-chain head returned by `VirtQueue::add` (the pop_used token).
    token: u16,
    kind: InFlightKind,
    /// `[in0 | in1? | resp]` — request span(s) followed by the device-written
    /// response span, all in one contiguous DMA buffer.
    meta: DmaBuffer,
    /// The exact shape `add` was called with. One value, matched exhaustively,
    /// instead of three independent length fields the drain re-derived by hand.
    chain: Chain,
    resp_len: usize,
    /// Separate device-read venus payload (async submits).
    venus: Option<DmaBuffer>,
}

/// The descriptor-chain shape of one submission.
///
/// `add` and `pop_used` must be handed the SAME buffer list, and virtio-drivers
/// does not validate that: `pop_used` -> `recycle_descriptors` walks the chain
/// against the caller-supplied list and PANICS on a mismatch
/// (`virtio-drivers-0.13.0/src/queue.rs:461,477,479,501`), which under
/// `panic = "abort"` with `wdk_panic` is a `KeBugCheck` from the interrupt DPC
/// while the device spinlock is held.
///
/// The agreement used to be expressed only by three independent length fields
/// plus the comment "exactly the spans `add` was called with", re-derived in
/// four separate blocks of raw-pointer arithmetic. As one exhaustively-matched
/// value it is a compile error to add a shape without teaching both sides.
#[derive(Clone, Copy)]
enum Chain {
    /// `[in0] -> [resp]`. Async control commands.
    Meta1 { in0_len: usize },
    /// `[in0, in1] -> [resp]`. Sync control with a second device-read span.
    Meta2 { in0_len: usize, in1_len: usize },
    /// `[hdr, venus stream] -> [resp]`, the stream in its own buffer.
    MetaPlusVenus { hdr_len: usize, venus_len: usize },
}

impl Chain {
    /// Byte offset of the response span inside `meta`.
    ///
    /// All three shapes place it immediately after the meta-resident request
    /// spans, i.e. at `in0_len + in1_len` — preserved verbatim, because
    /// `drain_used` reads the `VIRTIO_GPU_RESP_*` word and copies the sync
    /// response from exactly this offset.
    fn resp_offset(self) -> usize {
        match self {
            Self::Meta1 { in0_len } => in0_len,
            Self::Meta2 { in0_len, in1_len } => in0_len + in1_len,
            Self::MetaPlusVenus { hdr_len, .. } => hdr_len,
        }
    }

    /// The device-READ spans (up to two) and the device-WRITTEN response span,
    /// or `None` if any of them falls outside its buffer.
    ///
    /// The ONLY producer of the buffer list, so `add` and `pop_used` are handed
    /// literally the same code and the arm selection cannot differ between
    /// them. It also replaces the per-arm ad-hoc `t <= meta.as_slice().len()`
    /// bounds checks: every span is proved in `DmaBuffer::span`.
    fn spans(
        self,
        meta: &DmaBuffer,
        venus: Option<&DmaBuffer>,
        resp_len: usize,
    ) -> Option<([DmaSpan; 2], usize, DmaSpan)> {
        let resp = meta.span(self.resp_offset(), resp_len)?;
        Some(match self {
            Self::Meta1 { in0_len } => ([meta.span(0, in0_len)?, DmaSpan::EMPTY], 1, resp),
            Self::Meta2 { in0_len, in1_len } => (
                [meta.span(0, in0_len)?, meta.span(in0_len, in1_len)?],
                2,
                resp,
            ),
            Self::MetaPlusVenus { hdr_len, venus_len } => (
                [meta.span(0, hdr_len)?, venus?.span(0, venus_len)?],
                2,
                resp,
            ),
        })
    }
}

impl InFlight {
    /// Recover the entry-owned DMA buffers after the device has consumed them.
    /// Called only by the PASSIVE reaper after the entry leaves `parked`.
    pub fn into_dma_buffers(self) -> (DmaBuffer, Option<DmaBuffer>) {
        (self.meta, self.venus)
    }
}

/// A non-`Copy`, non-`Clone` receipt for one registered synchronous submission.
///
/// The token, the `NonNull<SyncWaitBlock>` and the block's own pinned storage
/// are three values the caller had to keep in sync across enqueue / PASSIVE
/// wait / abandon. Making the token move-only means it cannot be reused after
/// the abandonment that consumes it.
pub struct SyncTicket {
    token: u16,
}

impl SyncTicket {
    /// The raw descriptor-chain head, for a diagnostic breadcrumb only. Reading
    /// it does not consume the ticket; `abandon_sync` still does.
    pub fn raw(&self) -> u16 {
        self.token
    }
}

/// What [`VirtioGpu::abandon_sync`] found.
///
/// This replaces a bool whose fall-through returned `true` — "already
/// completed, treat as success" — for EVERY case that was not an exact
/// (token, Sync, same-waiter) match, including a token now owned by a different
/// command and a kind mismatch. The caller then ran `copy_host_response` on a block that
/// may never have been written. That was safe only because (a) a token cannot
/// be re-issued until its chain is popped, which implies the old waiter was
/// signalled, and (b) `new_zeroed` leaves `resp` all-zero and 0 is not a
/// RESP_OK code, so `ctrl_roundtrip_ok` still errored out — i.e. correctness
/// rested on a virtio-ring property and a zero-init accident, neither of them
/// stated at that function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncOutcome {
    /// No in-flight entry holds this token: completion or transport abort
    /// already signalled it, and the wait block names which terminal occurred.
    AlreadyCompleted,
    /// The waiter was deregistered before completion. The response bytes were
    /// never written.
    Abandoned,
    /// The token names an entry that is not this waiter's — a NEW error
    /// population, and the one behaviour change in this item. It converts a
    /// silent read of an unwritten buffer into a counted error.
    NotOurs,
}

/// A registered WAIT_FENCE waiter.
struct FenceWaiter {
    fence_id: u64,
    block: NonNull<SyncWaitBlock>,
}

/// First wire fence id the NEXT transport instance will hand out.
///
/// Driver-global and monotonic across StartDevice/StopDevice cycles. Starts at
/// 1 because 0 is the "no fence" sentinel every predicate tests for.
static NEXT_WIRE_FENCE_BASE: AtomicU64 = AtomicU64::new(1);
static NEXT_SCANOUT_TRANSPORT_INSTANCE: AtomicU64 = AtomicU64::new(1);

fn reserve_wire_fence_base() -> Option<u64> {
    let mut current = NEXT_WIRE_FENCE_BASE.load(Ordering::Relaxed);
    loop {
        let next = current.checked_add(WIRE_FENCE_INSTANCE_STRIDE)?;
        match NEXT_WIRE_FENCE_BASE.compare_exchange_weak(
            current,
            next,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return Some(current),
            Err(observed) => current = observed,
        }
    }
}

fn reserve_scanout_transport_instance() -> Option<u64> {
    let mut current = NEXT_SCANOUT_TRANSPORT_INSTANCE.load(Ordering::Relaxed);
    loop {
        let next = current.checked_add(1)?;
        match NEXT_SCANOUT_TRANSPORT_INSTANCE.compare_exchange_weak(
            current,
            next,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return Some(current),
            Err(observed) => current = observed,
        }
    }
}

/// D3D12 ECL submissions gated on the EXACT wire fence their batch ends at rather
/// than on the prefix below it (A4, `docs/dx12/PENDING.md` §1). Mirrored as
/// `D12Exact`.
///
/// ⚠ NOT KNOB-GATED, and that is deliberate: the prefix was an invariant
/// violation, not a tuning choice, so there is no "restore the superset" arm to
/// keep reachable. The A/B that matters is `D12Zero` — a UMD naming no boundary at
/// all — which is already the documented order-against-nothing lever.
///
/// WHOSE ACTIVITY INCREMENTS IT: only a submission that carried a live
/// `HeliosD3D12SubmitCmd` record, i.e. `helios_umd12.dll`'s. DWM cannot move it —
/// the D3D11 present writer of the same field is routed to `Kind::Prefix` by
/// `wddm_boundary::select`. It is the one counter in this cluster with a clean
/// population.
///
/// ⛔⛔ **THERE IS NO EXACT IDENTITY FOR THIS COUNTER, AND AN EARLIER GRADING
/// ASSERTED ONE**: `D12Exact == D12Rec - D12Zero - D12MrgF - GpuFncClamp -
/// GpuFncGen`, with *"any shortfall means a D3D12 packet took a prefix arm, which
/// is the defect A4 names coming back"*. That arithmetic cannot hold, so its
/// failure would have been reported as a closed defect re-opening. Four
/// independent reasons, each sufficient:
///
///  1. **THE UNITS DIFFER.** `D12Rec`/`D12Zero`/`D12MrgF`/`D12Merged`/`D12Clr`
///     count `DxgkDdiRender` calls — one per ECL record decoded. THIS counts
///     `DxgkDdiSubmitCommand{,Virtual}` packets. dxgkrnl batches Renders into one
///     DMA buffer, so N records can produce one submission.
///  2. **`D12Merged` IS NOT IN THE EXPRESSION** and is exactly the collapse in
///     (1): the k-1 later records in a batch merge into the first (largest fence
///     wins), contributing 0 further exact submissions.
///  3. **REPLAYS ARE UNACCOUNTED.** `PresentSubmissionPrivate::decode` CONSUMES
///     the D3D12 magic by design, so a packet resubmitted out of the same buffer
///     falls back to the prefix on purpose — a `D12Rec` with no `D12Exact`, and
///     the fail-safe direction the consume exists for.
///  4. **`GpuFncClamp` / `GpuFncGen` ARE ADAPTER-GLOBAL.** Both are decided in
///     `wddm_boundary::select` BEFORE the `d3d12` bit is read, so DWM's D3D11
///     present BLT marker moves them. Subtracting them from a D3D12 accounting
///     mixes populations; their own docs in `virtio/counters.rs` say so.
///  (5. `D12Clr` post-dates the expression and is a further term.)
///
/// ⇒ GRADING, in the forms that ARE true:
///
///  * **SOUND BOUND: `D12Exact <= D12Rec - D12Zero`.** Every exact submission
///    consumes one `'HD12'` record whose surviving `gpu_fence_id` is nonzero, and
///    only a nonzero-fence Render can put one there (`mark_d3d12` takes the max,
///    and a zero-fence record is overwritten rather than merged). A VIOLATION is a
///    real defect — a record honoured twice, or a boundary from somewhere
///    `mark_d3d12` did not write. Every term above slackens this bound in the same
///    direction, which is why it survives all of them.
///  * **THE A4 REGRESSION SIGNAL is qualitative: `D12Rec > D12Zero` while
///    `D12Exact == 0`.** Records named real boundaries and NOT ONE reached
///    SubmitCommand as an exact wait. That is A4 coming back, or the record never
///    surviving to SubmitCommand at all — and unlike a shortfall it needs no
///    arithmetic to state.
///  * **HEALTHY = `D12Exact` MOVES**, at roughly the rate of `ExecuteCommandLists`
///    batches that named a fence, and well below `D12Rec`.
///  * ⛔ **A SHORTFALL AGAINST ANY EXPRESSION ATTRIBUTES NOTHING.** At least five
///    independent terms sit between the two counters, four legitimately nonzero
///    and two adapter-global. If the per-term attribution is ever needed it has to
///    be counted at the site, not inferred here.
///
/// ⚠ The old rule's second half — *"`D12Exact > 0` with `EscSubRing == 0` means
/// the ICD is handing the UMD ring-0 fence ids"* — rests on a reading that cannot
/// occur: `EscSubRing` is adapter-global and DWM's own present path makes it
/// nonzero. The ring question is answerable only as a delta against a control arm;
/// see `EscSubRing`'s block in `virtio/counters.rs`.
pub(crate) static D3D12_EXACT_WATERMARK_USED: AtomicU32 = AtomicU32::new(0);
/// Gap between one instance's first id and the next instance's.
///
/// Far more than any instance can consume: at the ~10^5 fences a heavy session
/// produces, 2^32 instances' worth of headroom remains, and a u64 counter
/// cannot wrap in the machine's lifetime.
const WIRE_FENCE_INSTANCE_STRIDE: u64 = 1 << 32;

/// The KMD's allocator for offsets inside the host-visible BAR window.
///
/// Its three pieces — the high-water bump, the coalescing free list and the
/// VidMm reserve — were three loose fields of `VirtioGpu`, and the reserve was
/// installed by a plain setter two statements after `set_virtio` in StartDevice.
/// A SECOND `reserve_window_prefix` with a larger len would silently strand
/// every offset already issued below the new mark: `free_window_range` returns
/// early for `offset < reserve`, so those ranges could never be recycled and
/// nothing would say so.
///
/// The reserve is now immutable once any offset has been issued —
/// [`VirtioGpu::configure_window_reserve`] refuses and counts otherwise. It is a
/// guarded setter rather than a literal construct-with-reserve because the
/// reserve is computed from the window length AFTER `VirtioGpu::init` and
/// installed under the device spinlock, where allocating the free list's `Vec`
/// is forbidden.
struct WindowAllocator {
    /// Total bytes of the host-visible window (0 if the device exposes none).
    window_len: u64,
    /// First `reserve` bytes are OWNED BY VIDMM (the CPU-visible BAR memory
    /// segment, `query_adapter_info`): VidMm's segment allocator assigns offsets
    /// there and `BuildPagingBuffer` maps each allocation's blob at the assigned
    /// offset. This allocator never hands out or reclaims offsets below the mark.
    reserve: u64,
    /// Bump high-water.
    next_offset: u64,
    /// Coalescing free list (bounded by MAX_WINDOW_RANGES).
    free_ranges: Vec<WindowRange>,
}

impl WindowAllocator {
    /// PASSIVE only: reserves the free list up front so `alloc`/`free` under the
    /// device spinlock never allocate.
    fn new(window_len: u64) -> Self {
        Self {
            window_len,
            reserve: 0,
            next_offset: 0,
            free_ranges: Vec::with_capacity(MAX_WINDOW_RANGES),
        }
    }

    /// True while no offset has been issued and nothing has been freed — the
    /// only state in which the reserve may still be set.
    fn is_pristine(&self) -> bool {
        self.next_offset == self.reserve && self.free_ranges.is_empty()
    }

    /// Allocate a page-rounded `len`-byte range: reuse a free range if one fits,
    /// else bump the high-water mark (bounded by `window_len`).
    fn alloc(&mut self, len: u64) -> Result<u64, VirtioError> {
        if let Some(idx) = self.free_ranges.iter().position(|r| r.len >= len) {
            let offset = self.free_ranges[idx].offset;
            if self.free_ranges[idx].len == len {
                self.free_ranges.swap_remove(idx);
            } else {
                self.free_ranges[idx].offset += len;
                self.free_ranges[idx].len -= len;
            }
            return Ok(offset);
        }
        let offset = self.next_offset;
        let end = match offset.checked_add(len) {
            Some(e) if e <= self.window_len => e,
            _ => {
                WINDOW_ALLOC_REJECTS.fetch_add(1, Ordering::Relaxed);
                return Err(VirtioError::OutOfMemory);
            }
        };
        self.next_offset = end;
        Ok(offset)
    }

    /// Return a range: drop the high-water mark if it abuts, else coalesce into
    /// an adjacent free range, else record a new free range (or silently leak if
    /// the bounded free list is full — bring-up acceptable).
    fn free(&mut self, offset: u64, len: u64) {
        if len == 0 {
            return;
        }
        // VidMm-partition offsets are owned by VidMm's segment allocator — they
        // must never enter the KMD free list (a later KMD-side map would collide
        // with a VidMm placement). Every release path funnels here, so this one
        // guard covers DestroyAllocation/ReleaseBlob/teardown of VidMm-placed
        // blobs uniformly. PRESERVED VERBATIM: it is what keeps VidMm-partition
        // offsets out of the KMD free list.
        if offset < self.reserve {
            return;
        }
        if offset.checked_add(len) == Some(self.next_offset) {
            self.next_offset = offset;
            while let Some(idx) = self
                .free_ranges
                .iter()
                .position(|r| r.offset.checked_add(r.len) == Some(self.next_offset))
            {
                let r = self.free_ranges.swap_remove(idx);
                self.next_offset = r.offset;
            }
            return;
        }
        for range in &mut self.free_ranges {
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
        if self.free_ranges.len() < MAX_WINDOW_RANGES {
            self.free_ranges.push(WindowRange { offset, len });
        } else {
            WINDOW_RANGE_DROPS.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Which retirement domain a wait is against.
///
/// The nine-line doc this replaces explained at length that the wait is ring-0
/// only and why counting ring >= 1 fences would be wrong — while sitting on a
/// function whose `wait_gpu: bool` parameter did exactly that, undocumented,
/// and whose three callers picked the mode three different ways (two hardcode
/// true, one derives it from `gpu_completion_fence.is_some()`, one replays a
/// stored value). Both values genuinely occur.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RetireDomain {
    /// Ring-0 only: host DECODE retirement.
    ///
    /// This is the domain the WDDM pending FIFO's contract was built on. It
    /// exists to order DMA_COMPLETED behind the venus escape traffic queued
    /// before it, and ring-0 fences retire at decode. ring >= 1 fences (WS1 #4)
    /// retire at host GPU COMPLETION and stay in flight for the full GPU-work
    /// duration, so counting them here would couple every WDDM DMA fence
    /// (GDI/paging pacing) to unrelated multi-ms GPU work. Consumers that need
    /// GPU completion wait on those fences explicitly (WAIT_FENCE).
    DecodeOnly,
    /// Every ring: the caller genuinely needs host GPU completion, e.g. the
    /// direct-primary refresh marker ordering on a Venus completion watermark.
    IncludingGpu,
}

/// How a [`WddmPending::watermark`] is compared against the in-flight wire fences.
///
/// ⛔ THE DISTINCTION IS A CLAUDE.md INVARIANT, not a tuning choice: *"a WDDM fence
/// may wait on the frame's OWN boundary, never on the whole `next_wire_fence`
/// backlog."* A prefix wait is satisfied only when EVERY async fence below the
/// watermark has retired — every ring, every process, DWM's ring-1 scanout copies
/// included — so it delays the fence by the whole pipeline depth and is the
/// over-wait avoided by the exact D3D12 boundary path.
#[derive(Clone, Copy, PartialEq, Eq)]
enum WireBoundary {
    /// `watermark` is EXCLUSIVE and a PREFIX: every async fence strictly below it
    /// must have retired. Conservative, always eventually satisfied, never a lie,
    /// and the fallback whenever a boundary cannot be trusted.
    Prefix,
    /// `watermark` names ONE wire fence and ONLY that fence must have retired.
    ///
    /// The frame's own boundary. See the D3D12 arm in
    /// [`VirtioGpu::note_wddm_submission`] for why exactness is sound there and
    /// why it is not applied to the legacy Present arm.
    Exact,
}

/// A WDDM submission whose `DXGK_INTERRUPT_DMA_COMPLETED` is gated on venus
/// completion: it may signal once every async wire fence `< watermark` has
/// retired (and strictly in FIFO order — SubmissionFenceIds are watermarks to
/// dxgkrnl, so they must complete monotonically).
/// One WDDM submission waiting for its Venus watermark.
///
/// ⚠ IT CARRIED A SECOND HALF UNTIL 22.22.217.0 — the presentation epoch whose
/// host `RESOURCE_FLUSH` had to complete before dxgkrnl could hand the
/// allocation back to DXGI (ROADMAP defect 0ab-B). The theory was sound and the
/// measurement was not: a 2×2 factorial over 46 681 frames moved whole-flush
/// black by nothing in any cell, because the app's clear of a reclaimed buffer
/// never travels in a WDDM DMA buffer and so waits on no completion this driver
/// controls. `watermark` — "has the app finished WRITING this frame?" — is the
/// only question a WDDM completion can answer, and it is the one asked here
/// again. The epochs live on, deciding the flush executor's ownership gate.
struct WddmPending {
    /// Direct slot/generation authority in K9's adapter-owned ordered engine
    /// frontier. This is scheduler-private lifetime state; it never crosses a
    /// wire or renderer ABI and is not looked up by a resource identity.
    engine_ticket: crate::adapter::OrderedEngineTicket,
    /// Normal-wire producer boundary (possibly the KMD scanout-copy ring-1
    /// fence).
    watermark: u64,
    /// Whether `watermark` is a prefix bound or the one fence this packet's own
    /// work ends at. See [`WireBoundary`].
    wire_boundary: WireBoundary,
    domain: RetireDomain,
}

/// A WDDM submission popped from the pending FIFO whose `DMA_COMPLETED` has not
/// yet been delivered.
///
/// Deliberately **not** `Copy`, and consumed by exactly two operations
/// ([`WddmReady::delivered`] and [`VirtioGpu::requeue_wddm_front`]). While this
/// value is alive the entry is in no queue at all: the fence is out of
/// `wddm_pending` and the completed watermark still points below it. Dropping it
/// without doing either loses the fence permanently, VidSch never sees it retire,
/// and the only symptom is a TDR. The `Copy` derive it used to have was what made
/// the old `[WddmReady; 8]` batch array possible, which is why the one-at-a-time
/// taker is a precondition of this encoding rather than a stylistic choice.
#[must_use = "a popped WDDM fence must be delivered or requeued, never dropped"]
pub struct WddmReady {
    pending: WddmPending,
}

/// The outcome of one attempt to retire the head of the WDDM pending FIFO.
///
/// It used to be `Option<WddmReady>`, which collapsed "nothing queued", "the
/// app's own work is still running" and "the host has not read the frame we
/// published" into one `None`. The last of those is the only one the caller can
/// do something about — it can ask for the read it is waiting on — and it is
/// also the only one whose rate proves the 0ab-B gate is live rather than inert.
#[must_use = "a Ready outcome carries a fence that must be delivered or requeued"]
pub enum WddmTake {
    /// The FIFO is empty.
    Empty,
    /// The head's producer (Venus) watermark has not been reached.
    BlockedOnProducer,
    /// Deliver `DXGK_INTERRUPT_DMA_COMPLETED` for this submission.
    Ready(WddmReady),
}

/// Compatibility Venus admission beneath K9's authoritative engine frontier.
#[must_use = "a failed compatibility boundary must poison, never complete, its K9 ticket"]
pub enum WddmAdmission {
    /// No producer remains; the caller may mark the exact K9 ticket terminal.
    HostTerminal,
    /// The entry and its direct K9 ticket are both retained for the DPC.
    Pending,
    /// Transport failure or bounded FIFO exhaustion. The current K9 generation
    /// must fail closed; this outcome never authorizes DMA completion.
    Failed,
}

impl WddmReady {
    pub(crate) fn engine_ticket(&self) -> crate::adapter::OrderedEngineTicket {
        self.pending.engine_ticket
    }

    /// Consume the token after `DMA_COMPLETED` was delivered successfully.
    /// Exists so the success path *states* that it consumed the fence rather
    /// than letting it fall out of scope.
    pub fn delivered(self) {}
}

/// Result of [`VirtioGpu::fence_wait_prepare`].
pub enum FenceWaitPrep {
    /// The fence already completed (or the id predates the tracked window).
    Complete,
    /// Registered; wait on the block's event.
    Registered,
    /// The id was never assigned by this transport instance.
    Invalid,
    /// Waiter table full — retry after a short PASSIVE sleep.
    TableFull,
}

/// Result of [`VirtioGpu::blob_map_begin`].
pub enum BlobMapBegin {
    /// Already mapped — here is the existing mapping.
    Mapped(BlobMapPrep),
    /// Range reserved; the caller must run the RESOURCE_MAP_BLOB round-trip and
    /// then call [`VirtioGpu::blob_map_finish`].
    Start { offset: u64, len: u64 },
    /// Another mapper's round-trip is in flight — retry after a PASSIVE sleep.
    Busy,
    /// Unknown resource / no host-visible window / size out of range / window
    /// exhausted.
    Failed(VirtioError),
}

/// Result of [`VirtioGpu::blob_remap_begin`] (fixed-offset, VidMm-dictated maps
/// for the CPU-visible BAR memory segment — `build_paging_buffer.rs`).
pub enum BlobRemapBegin {
    /// Already mapped at exactly the requested offset — nothing to do.
    Mapped(BlobMapPrep),
    /// Reserved. The caller must (1) RESOURCE_UNMAP_BLOB if `old` is
    /// `Some((offset, len))` and return that range via
    /// [`VirtioGpu::free_window_range_pub`] (a no-op for VidMm-partition
    /// offsets), (2) RESOURCE_MAP_BLOB at the new offset, then (3) call
    /// [`VirtioGpu::blob_map_finish`] with the new offset.
    Start { old: Option<(u64, u64)>, len: u64 },
    /// Another mapper's round-trip is in flight — retry after a PASSIVE sleep.
    Busy,
    /// Unknown resource / out-of-partition target / bad size.
    Failed(VirtioError),
}

/// Result of [`VirtioGpu::blob_map_finish`].
pub enum BlobMapFinish {
    /// Mapping recorded.
    Done(BlobMapPrep),
    /// The host rejected the map (range returned to the allocator).
    HostRejected,
    /// The slot vanished mid-map (owner teardown raced the round-trip): the
    /// caller must issue RESOURCE_UNMAP_BLOB and return the range via
    /// [`VirtioGpu::free_window_range_pub`].
    SlotGone,
}

/// An initialized virtio-gpu transport.
pub struct VirtioGpu {
    /// Control transport + virtqueue under the device interrupt's single
    /// synchronization domain. Normal producers enter through dxgkrnl; the D4
    /// classic callback can reach only its narrow proof-gated fixed-slot method.
    // Separate allocation is load-bearing: the DIRQL publication points to this
    // object while lower-IRQL code may hold `&mut VirtioGpu` under `virtio_lock`.
    // Keeping the queue inline would let that outer unique borrow overlap the
    // raw interrupt-level queue reference even though `access` serialized every
    // actual queue-core mutation.
    queue: Box<InterruptQueue>,
    /// Next virtio-gpu 3D context id to hand out (guest-assigned; 0 is the
    /// reserved global context, so we start at 1). Phase 3.
    next_ctx_id: u32,
    /// Next virtio-gpu resource id to hand out (0 is reserved). Phase 3 (M3.5).
    next_resource_id: u32,
    /// Host-visible blob window (SHARED_MEMORY_CFG/HOST_VISIBLE BAR), discovered
    /// in `init`. `None` if the device exposes no host-visible window — the WDDM
    /// blob-map path is then unavailable (Stage 2 fails honestly). Gate 5a Stage 2.
    host_visible: Option<HostVisibleWindow>,
    /// Mapped kernel VA of the virtio ISR-status register (read-to-clear), or 0 if
    /// the device exposes no ISR cap. `DxgkDdiInterruptRoutine` reads this at DIRQL
    /// to acknowledge the line-based INTx (the device is `MSISupported=0`). See
    /// [`map_isr_status_register`].
    isr_status_va: usize,
    /// Tracked blobs (resource_id → size/mapping state). Heap-reserved to MAX_BLOBS
    /// at init so `push` under the spinlock never reallocates (the 0x7F lesson).
    blobs: Vec<BlobSlot>,
    /// Every host-live virtio resource id created through this transport.
    /// Removal is one-shot and gates CTX_DETACH_RESOURCE/RESOURCE_UNREF, avoiding
    /// qemu `RESOURCE_UNREF: resource does not exist` errors from duplicate DDI
    /// teardown paths.
    resources: Vec<u32>,
    /// Live-resource slots reserved by in-flight creates.
    resources_reserved: usize,
    /// Context tracking slots reserved by in-flight CTX_CREATEs. Tracking is
    /// MANDATORY (see `reserve_context_slot`), so a context is reserved before
    /// the wire round-trip and committed after it.
    contexts_reserved: usize,
    /// Live virtio-gpu contexts, tagged with the owning device handle, so
    /// `DxgkDdiDestroyDevice` can `CTX_DESTROY` any context an ICD created but did
    /// not tear down (crash / skipped CTX_DESTROY) — otherwise leaked contexts
    /// accumulate host-side state and eventually wedge the render server. Reserved
    /// to MAX_CONTEXTS at init (no realloc under the spinlock).
    contexts: Vec<ContextSlot>,
    /// Offsets inside the host-visible BAR window. See [`WindowAllocator`].
    window: WindowAllocator,
    /// In-flight control-queue entries (token-matched; capacity MAX_INFLIGHT,
    /// reserved at init — pushes never reallocate under the spinlock).
    inflight: Vec<InFlight>,
    /// Completed entries awaiting a PASSIVE reap (`swap_parked`). Capacity
    /// MAX_PARKED, reserved at init.
    parked: Vec<InFlight>,
    /// Empty pre-reserved vectors swapped through the PASSIVE reaper. This
    /// removes two kernel-heap allocations from every tiny Venus completion.
    parked_spare: Vec<InFlight>,
    reap_buffers_spare: Vec<DmaBuffer>,
    reap_in_progress: bool,
    /// Exact native completions extracted under `virtio_lock` and processed
    /// only after that lock is released. Both vectors reserve MAX_INFLIGHT at
    /// init, so used-ring and reset paths never allocate.
    native_terminals: Vec<crate::ddi::native_render::NativeHostTerminal>,
    native_terminals_spare: Vec<crate::ddi::native_render::NativeHostTerminal>,
    native_terminal_drain_in_progress: bool,
    /// PASSIVE-reaped DMA buffers ready for another command. Accessed under the
    /// existing virtio spinlock, but allocation/free never occurs there.
    dma_pool: Vec<DmaBuffer>,
    dma_pool_bytes: usize,
    /// Registered WAIT_FENCE waiters (capacity MAX_FENCE_WAITERS).
    fence_waiters: Vec<FenceWaiter>,
    /// Next wire fence id to assign (globally monotonic, starts at 1; 0 is
    /// never a valid wire fence).
    next_wire_fence: u64,
    /// FIRST wire fence id THIS transport generation may hand out — i.e.
    /// `next_wire_fence` as of `init`, before anything was assigned.
    ///
    /// ⛔ IT EXISTS BECAUSE AN UPPER BOUND IS NOT A GENERATION CHECK (A6,
    /// `docs/dx12/PENDING.md` §1). Every id space check in this transport was
    /// `id != 0 && id < next_wire_fence`, and [`NEXT_WIRE_FENCE_BASE`] strides the
    /// range by `WIRE_FENCE_INSTANCE_STRIDE` at every StartDevice — so an id
    /// sampled by a usermode client BEFORE a StopDevice/StartDevice cycle is
    /// billions below the new instance's range and satisfies that test trivially,
    /// while naming a fence this instance never issued. The in-flight scan then
    /// finds nothing at or below it and the dependency is satisfied INSTANTLY.
    /// For a wait that is merely a hint that is harmless; for a WDDM DMA fence it
    /// is a completion reported before the work exists.
    ///
    /// ⚠ `fence_wait_prepare` / `fence_event_register` (`:4390`, `:4433`) share the
    /// same one-sided predicate and are deliberately NOT changed here: their
    /// failure mode is a usermode wait that returns early to the process that
    /// forged the id, and the striding comment above records that arm as the
    /// intended behaviour for a client which survived a device restart. The WDDM
    /// arm is different in kind because dxgkrnl schedules the whole desktop on it.
    wire_fence_base: u64,
    /// Nonwrapping driver-global identity for synchronous persistent SET
    /// completions. Separate from the wire-fence range so neither namespace's
    /// exhaustion or reset semantics can authorize the other.
    scanout_transport_instance: u64,
    /// WDDM submissions pending on venus completion, FIFO (capacity
    /// MAX_WDDM_PENDING, reserved at init).
    wddm_pending: VecDeque<WddmPending>,
    /// Bounded completion-ordered DWM/primary dirty state.  Its oldest
    /// outstanding marker is retained for liveness while one later marker is
    /// coalesced with exact resource identity; boxed to keep StartDevice's
    /// by-value initialization frame below the kernel-stack headroom.
    /// `DmaGpuFence` (default 1). Retire ordinary (non-paging) WDDM DMA fences
    /// on host GPU COMPLETION instead of host DECODE.
    ///
    /// THE CONTRACT: dxgkrnl reads a DMA fence as "the GPU is finished with this
    /// work", and schedules everything downstream on that — including when a
    /// compositor may read a surface an app has just rendered. Retiring at
    /// decode reports completion while the host GPU is still executing, so
    /// dxgkrnl can advance a flip, or let dwm compose an app's window, over a
    /// buffer that is still being written. That is the black/torn frame, and it
    /// is confined to the region being drawn, which is why it presented as
    /// "only inside the app's window" and as Explorer's late top band.
    ///
    /// It was invisible while the UMD's present path blocked on GPU completion
    /// before publishing a present (`PresentOrder=0`): that made the fence's lie
    /// harmless by ensuring the work really was done before dxgkrnl saw it, at
    /// the cost of removing all CPU/GPU overlap (Fire Strike GT1 158 -> 136 fps,
    /// Combined 25.9 -> 18.8).
    ///
    /// The cost this trades against is the one `RetireDomain::DecodeOnly`'s doc
    /// names: ring >= 1 fences stay in flight for the whole GPU-work duration,
    /// so DMA fences now retire later. That is what the fences are supposed to
    /// mean. 0 restores the old behaviour as the A/B lever.
    ///
    /// This is the ONE reader. `AdapterKnobs` used to carry a second, unread
    /// copy of the same registry value; it was deleted 2026-08-05 so the two
    /// cannot disagree.
    dma_gpu_fence: bool,
    /// Ring-corruption latch: set when the used ring returns a token we do not
    /// track or `pop_used` fails structurally. The ring state is then
    /// untrustworthy and every subsequent command fails fast. NOTE: unlike the
    /// old model, a slow host does NOT set this — waiter timeouts abandon
    /// their entry and the transport keeps working.
    failed: bool,
    /// Scanout-0 preferred size `(width, height)` reported by the host in the
    /// `GET_DISPLAY_INFO` reply at `init` (`pmodes[0]`), or `None` if the host
    /// reported nothing usable. The display half uses this as the VidPn mode +
    /// generated-EDID native resolution so we present the size QEMU actually wants
    /// on scanout 0 (instead of a hardcoded guess). Read once by StartDevice.
    display_mode: Option<(u32, u32)>,
}

impl VirtioGpu {
    /// Bring the virtio-gpu device online and prove it with `GET_DISPLAY_INFO`.
    /// `passive` is threaded only to reach `DmaBuffer::new` for the
    /// GET_DISPLAY_INFO scratch page; the rest of bring-up is MMIO and PCI
    /// config access. It is a by-value ZST, so it costs neither a register nor a
    /// stack slot in this measured 3.0 KB frame — see `crate::irql`.
    pub fn init(
        passive: crate::irql::PassiveLevel,
        dxgkrnl: &DXGKRNL_INTERFACE,
    ) -> Result<Box<Self>, VirtioError> {
        // Queue mutation below depends on dxgkrnl's synchronous interrupt
        // serialization. Prove the callback before touching PCI or DRIVER_OK so
        // its absence cannot drop an already-live queue generation.
        let Some(synchronize) = dxgkrnl.DxgkCbSynchronizeExecution else {
            return Err(VirtioError::DeviceError);
        };
        // Reserve both nonwrapping transport namespaces before touching PCI or
        // DRIVER_OK. Later initialization failures deliberately burn them; a
        // late reservation refusal must never ordinary-drop a live device.
        let wire_fence_base = reserve_wire_fence_base().ok_or_else(|| {
            WIRE_FENCE_NAMESPACE_EXHAUSTED.fetch_add(1, Ordering::Relaxed);
            crate::diag::record_named_bytes(b"WfNsEx", 1);
            VirtioError::WireFenceNamespaceExhausted
        })?;
        let scanout_transport_instance = reserve_scanout_transport_instance().ok_or_else(|| {
            SCANOUT_TRANSPORT_INSTANCE_EXHAUSTED.fetch_add(1, Ordering::Relaxed);
            crate::diag::record_named_bytes(b"ScTiEx", 1);
            VirtioError::ScanoutTransportInstanceExhausted
        })?;
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
            core::hint::spin_loop();
        }
        // The bound was previously indistinguishable from success: the loop fell
        // through to ACKNOWLEDGE either way, so a device that never cleared its
        // status was driven through the whole init as if it had reset. Every
        // later assumption in this function rests on that reset. The sibling
        // GET_DISPLAY_INFO poll below already returns Err on its own bound.
        if !transport.get_status().is_empty() {
            crate::diag::fault(crate::diag::FaultCounter::StVioR, spins);
            return Err(VirtioError::DeviceError);
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
        // Spell out the error arm instead of `map_err(...)?`. In the measured
        // dev KMD, the combinator materialized and copied the queue-sized
        // `Result` through several init-frame slots. There is no error payload
        // to preserve here, and an early return still drops `transport` before
        // DRIVER_OK exactly as the combinator did.
        let mut control = match VirtQueue::<WdkHal, CTRL_QUEUE_SIZE>::new(
            &mut transport,
            CTRL_QUEUE,
            /* indirect */ false,
            /* event_idx */ false,
        ) {
            Ok(control) => control,
            Err(_) => return Err(VirtioError::DeviceError),
        };
        // Runtime ctrl completion is interrupt-driven. Be explicit instead of
        // relying on the freshly-zeroed avail.flags value: bit 0 clear asks the
        // device to interrupt after it adds a used element.
        control.set_dev_notify(true);
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
        // the device, response is written by it. The page is a local RAII
        // `DmaBuffer` — the runtime paths own per-command buffers instead
        // (C3/M3.4), so no shared scratch survives init.
        // Allocate the fixed direct-plane command owners at PASSIVE_LEVEL.
        // Their buffers are installed into `InterruptQueue` once and remain
        // there until transport teardown.
        let mut bind_cmd_pool = Vec::with_capacity(BIND_CMD_POOL);
        for _ in 0..BIND_CMD_POOL {
            let Some(buf) = DmaBuffer::new(passive, BIND_CMD_BYTES) else {
                break;
            };
            bind_cmd_pool.push(buf);
        }
        if crate::virtio::KMD_D2_OWNER_ENABLED && bind_cmd_pool.len() != BIND_CMD_POOL {
            return Err(VirtioError::OutOfMemory);
        }

        let mut scratch = DmaBuffer::new(passive, SCRATCH_BYTES).ok_or(VirtioError::OutOfMemory)?;
        let buf = scratch.as_mut_slice();
        let (req_buf, resp_buf) = buf.split_at_mut(SCRATCH_BYTES / 2);

        let hdr_len = core::mem::size_of::<VirtioGpuCtrlHdr>();
        let resp_len = core::mem::size_of::<VirtioGpuRespDisplayInfo>();
        let mut req = VirtioGpuCtrlHdr::zeroed();
        req.type_ = VIRTIO_GPU_CMD_GET_DISPLAY_INFO;
        req_buf[..hdr_len].copy_from_slice(bytemuck::bytes_of(&req));

        // Bounded inline round-trip (`Self` does not exist yet, so the
        // `ctrl_queue_bounded_roundtrip` helper is unavailable): a host that
        // never answers GET_DISPLAY_INFO must fail StartDevice cleanly, not
        // hang it forever. PASSIVE_LEVEL, no spinlock held.
        {
            let inputs: &[&[u8]] = &[&req_buf[..hdr_len]];
            let outputs: &mut [&mut [u8]] = &mut [&mut resp_buf[..resp_len]];
            // SAFETY: the scratch-page buffers stay valid for the whole block;
            // on timeout we bail out of init and never reuse this queue.
            let token =
                unsafe { control.add(inputs, outputs) }.map_err(|_| VirtioError::DeviceError)?;
            if control.should_notify() {
                transport.notify(CTRL_QUEUE);
            }
            let mut spins = 0u64;
            while !control.can_pop() {
                spins += 1;
                if spins >= CTRL_POLL_SPINS {
                    return Err(VirtioError::DeviceError);
                }
                core::hint::spin_loop();
            }
            // SAFETY: same buffers as `add`, still valid; `can_pop()` was true.
            let written_length = unsafe { control.pop_used(token, inputs, outputs) }
                .map_err(|_| VirtioError::DeviceError)? as usize;
            if written_length < hdr_len || written_length > resp_len {
                crate::diag::record_named_bytes(b"DpRsLen", written_length as u32);
                return Err(VirtioError::DeviceError);
            }
            let response_type =
                u32::from_le_bytes([resp_buf[0], resp_buf[1], resp_buf[2], resp_buf[3]]);
            if written_length == hdr_len && HostRejection::from_response_type(response_type).is_ok()
            {
                return Err(VirtioError::DeviceError);
            }
            if written_length != resp_len || response_type != VIRTIO_GPU_RESP_OK_DISPLAY_INFO {
                crate::diag::record_named_bytes(b"DpRsShp", response_type);
                return Err(VirtioError::DeviceError);
            }
        }

        let resp: &VirtioGpuRespDisplayInfo = bytemuck::from_bytes(&resp_buf[..resp_len]);
        crate::kmsg(c"Helios: virtio-gpu GET_DISPLAY_INFO OK\n");
        // Remember scanout 0's host-preferred size for the display half's VidPn
        // mode + generated EDID. QEMU reports it in `pmodes[0].r` even before a
        // scanout is bound; take it only when both dimensions look sane (a
        // 0×0 / not-yet-configured scanout falls back to the default in
        // StartDevice). Recorded so the host's report is visible live.
        let m0 = resp.pmodes[0].r;
        let display_mode =
            if m0.width >= 320 && m0.height >= 240 && m0.width <= 16384 && m0.height <= 16384 {
                Some((m0.width, m0.height))
            } else {
                None
            };
        crate::diag::record_named_bytes(
            b"DpInf",
            (m0.width.min(0xFFFF) << 16) | (m0.height & 0xFFFF),
        );

        // Discover the host-visible blob window (a fresh config accessor — the
        // original `access` was moved into `PciRoot` above; `DxgkConfigAccess` is
        // a cheap Copy of the device handle + callbacks). Gate 5a Stage 2.
        let host_visible = scan_host_visible_window(&DxgkConfigAccess::new(dxgkrnl));
        crate::diag::record(if host_visible.is_some() {
            0x0B00_0005
        } else {
            0x0B00_00E5
        });

        // Locate + map the ISR-status register so the (real) ISR can read-to-clear
        // the level-triggered INTx line and stop the unhandled-interrupt storm.
        let isr_status_va = map_isr_status_register(&DxgkConfigAccess::new(dxgkrnl));
        crate::diag::record(if isr_status_va != 0 {
            0x0B00_0006
        } else {
            0x0B00_00E6
        });
        // A map failure here is NOT a benign degrade on this INTx device: with no
        // ISR ack the level-triggered line stays asserted and Windows' interrupt
        // storm detector Code-43s the adapter. Report it ungated.
        let mmio_fails = crate::virtio::hal::MMIO_MAP_FAILS.load(Ordering::Relaxed);
        if mmio_fails != 0 {
            crate::diag::fault(crate::diag::FaultCounter::StIsr, mmio_fails);
        }

        // `VirtioGpu` contains the control virtqueue and many ownership tables.
        // Return it heap-owned so StartDevice never reserves a second by-value
        // copy of this large state while `init`'s own frame is live.
        let direct_buffers = if crate::virtio::KMD_D2_OWNER_ENABLED {
            core::mem::take(&mut bind_cmd_pool)
        } else {
            Vec::new()
        };
        let queue = Box::new(InterruptQueue::new(
            transport,
            control,
            Some(synchronize),
            dxgkrnl.DeviceHandle,
            scanout_transport_instance,
            direct_buffers,
            wire_fence_base,
        )?);
        let gpu = Box::new(Self {
            queue,
            next_ctx_id: 1,
            next_resource_id: 1,
            host_visible,
            isr_status_va,
            blobs: Vec::with_capacity(MAX_BLOBS),
            resources: Vec::with_capacity(MAX_RESOURCES),
            resources_reserved: 0,
            contexts_reserved: 0,
            contexts: Vec::with_capacity(MAX_CONTEXTS),
            window: WindowAllocator::new(host_visible.map_or(0, |w| w.len)),
            inflight: Vec::with_capacity(MAX_INFLIGHT),
            parked: Vec::with_capacity(MAX_PARKED),
            parked_spare: Vec::with_capacity(MAX_PARKED),
            reap_buffers_spare: Vec::with_capacity(2 * MAX_PARKED),
            reap_in_progress: false,
            native_terminals: Vec::with_capacity(MAX_INFLIGHT),
            native_terminals_spare: Vec::with_capacity(MAX_INFLIGHT),
            native_terminal_drain_in_progress: false,
            dma_pool: Vec::with_capacity(MAX_DMA_POOL),
            dma_pool_bytes: 0,
            fence_waiters: Vec::with_capacity(MAX_FENCE_WAITERS),
            // NOT 1. Wire fence ids arrive from an untrusted usermode buffer at
            // the bounded KMD wait path, and `fence_wait_prepare` plus the WDDM
            // boundary arm both decide against
            // the ordinal predicate `id < next_wire_fence && not in-flight`.
            // Restarting the id space at 1 on every transport init lets a stale id
            // from a PREVIOUS instance — an ICD that survived a `pnputil
            // /restart-device` still holding fences — ALIAS a live id of the new
            // instance, so a waiter could park against completely unrelated work
            // that happens to occupy the same number. Striding the base by
            // `WIRE_FENCE_INSTANCE_STRIDE` at each init makes the id ranges
            // disjoint, which removes the aliasing. Behaviour within one instance
            // is unchanged; the predicate is sound there, because next_wire_fence
            // is bumped only after `control.add` succeeds, in the same spinlock
            // section as the `inflight` push.
            //
            // ⛔ THIS COMMENT USED TO CLAIM the stride *"moves those ids into the
            // `>= next_wire_fence` Invalid arm"*. THAT IS BACKWARDS and it was
            // load-bearing: the stride moves the base UP, so a pre-restart id is
            // billions BELOW the live range and lands in the `< next_wire_fence`
            // arm — the one that reads "already complete". The stride buys
            // disjointness, never rejection. Rejection needs the two-sided bound
            // against `wire_fence_base`, which is why that field exists (A6).
            next_wire_fence: wire_fence_base,
            wire_fence_base,
            scanout_transport_instance,
            wddm_pending: VecDeque::with_capacity(MAX_WDDM_PENDING),
            // Snapshotted at transport init like every other knob, so
            // `reg add` + `pnputil /restart-device` flips it with no reboot.
            dma_gpu_fence: crate::diag::read_config_dword(crate::diag::knobs::DMA_GPU_FENCE, 1)
                != 0,
            failed: false,
            display_mode,
        });
        // (The old Gate-2 venus ctx self-test is gone: the StartDevice venus
        // client bring-up right after transport init exercises the full context
        // + blob lifecycle for real.)

        // Read-to-clear the ISR-status register once: the GET_DISPLAY_INFO
        // round-trip above completed via the polled path, which never touches
        // this register, so the device may still be asserting INTx from that
        // completion. Clear it now (PASSIVE) so the line starts deasserted
        // before the interrupt-driven runtime paths take over.
        if gpu.isr_status_va != 0 {
            // SAFETY: `isr_status_va` is the mapped MMIO VA of the 1-byte
            // read-to-clear ISR-status register; a volatile read clears it.
            let _ = unsafe { core::ptr::read_volatile(gpu.isr_status_va as *const u8) };
        }

        // `scratch` (the init round-trip page) drops here — the descriptor
        // chain it backed was popped above, so the device no longer references
        // it.
        drop(scratch);
        Ok(gpu)
    }

    // ── C3/M3.4 queue machinery ──────────────────────────────────────────────
    //
    // Every method here runs under the AdapterContext virtio spinlock at
    // DISPATCH_LEVEL and NEVER waits, allocates, or frees DMA memory. Waiting
    // happens in `virtio::ctrl` (PASSIVE, KEVENT); freeing happens when a
    // PASSIVE caller reaps the parked list.

    /// Ring-corruption latch (a wedged-slow host does NOT set this).
    pub fn transport_failed(&self) -> bool {
        self.failed || self.queue.is_failed()
    }

    /// Enqueue a synchronous control command. `meta` = `[in0 | in1? | resp]`
    /// (one contiguous DMA buffer); the entry owns it until completion. The
    /// waiter's block is signaled (resp copied in) by [`Self::drain_used`].
    /// On `QueueFull` the buffer is handed back for a PASSIVE retry.
    /// The device-facing half of every enqueue: the corruption latch, the
    /// capacity gate, the descriptor spans, and `control.add` with its two
    /// distinct failure arms.
    ///
    /// R1005. All three entry points repeated this verbatim -- the `failed`
    /// check, `inflight.len() >= MAX_INFLIGHT || parked.len() >=
    /// PARKED_ENQUEUE_GATE` with its QUEUE_FULL_RETRIES bump, the
    /// `chain.spans(..)` refusal, and the `QueueFull` vs corruption-latch split
    /// on `add`.
    ///
    /// The DmaBuffers are BORROWED, not consumed: the three entry points hand
    /// them back to their callers in two different tuple shapes
    /// (`(DmaBuffer, VirtioError)` and `(DmaBuffer, DmaBuffer, VirtioError)`),
    /// and threading that through a shared signature would need either a panic
    /// or a third shape nobody wants. Each caller keeps ownership and maps this
    /// bare `VirtioError` into its own tuple.
    ///
    /// # Safety of the spans
    /// The device-visible slices handed to `control.add` alias `meta`/`venus`,
    /// which the caller moves into the `InFlight` entry on the success path;
    /// the entry owns them until the matching `pop_used`. Moving a `DmaBuffer`
    /// moves the owning struct, not the DMA bytes. The borrows end at `add`.
    fn enqueue_core(
        &mut self,
        chain: Chain,
        meta: &DmaBuffer,
        venus: Option<&DmaBuffer>,
        resp_len: usize,
        sequence: Option<(&crate::adapter::AdapterContext, u32)>,
    ) -> Result<(u16, Option<u64>), VirtioError> {
        if self.failed || self.queue.is_failed() {
            return Err(VirtioError::DeviceError);
        }
        if self.inflight.len() >= MAX_INFLIGHT || self.parked.len() >= PARKED_ENQUEUE_GATE {
            QUEUE_FULL_RETRIES.fetch_add(1, Ordering::Relaxed);
            return Err(VirtioError::QueueFull);
        }
        let Some((reads, count, resp)) = chain.spans(meta, venus, resp_len) else {
            return Err(VirtioError::DeviceError);
        };
        match self.queue.add(reads, count, resp, sequence) {
            Ok(added) => Ok(added),
            Err(VirtioError::QueueFull) => {
                QUEUE_FULL_RETRIES.fetch_add(1, Ordering::Relaxed);
                Err(VirtioError::QueueFull)
            }
            Err(_) => {
                self.latch_failed_and_fail_inflight();
                Err(VirtioError::DeviceError)
            }
        }
    }

    /// Publish the in-flight entry, THEN ring the device doorbell.
    ///
    /// R1005, and k-gputransport-17. The order is the point, and it was not
    /// uniform: `enqueue_async_control` and `enqueue_submit_inner` pushed first
    /// and notified last, each carrying a comment about a fast host completing
    /// into the ISR/DPC on another CPU -- while `enqueue_sync` notified FIRST
    /// and pushed afterwards. A rule stated in prose, in two copies, and
    /// violated in the third.
    ///
    /// The comment was also misstated. The hazard it describes is not live:
    /// `drain_used` is the only used-ring consumer and every one of its call
    /// sites runs inside `adapter.with_virtio`, so it takes the same spinlock
    /// this call is already holding and cannot interleave between the doorbell
    /// and the push. THE REAL INVARIANT is that the token must be in `inflight`
    /// before this spinlock is released. Having one implementation makes that
    /// structural instead of repeated, so a future change that moves
    /// `drain_used` off `virtio_lock` cannot reintroduce the race in exactly
    /// one of three places.
    fn publish_then_notify(&mut self, entry: InFlight) {
        self.inflight.push(entry);
        bump_high_water(&INFLIGHT_HIGH_WATER, self.inflight.len());
        if self.queue.notify().is_err() {
            self.latch_failed_and_fail_inflight();
        }
    }

    pub fn enqueue_sync(
        &mut self,
        mut meta: DmaBuffer,
        in0_len: usize,
        in1_len: usize,
        resp_len: usize,
        waiter: NonNull<SyncWaitBlock>,
        scanout_bind: Option<(u32, bool)>,
        adapter: &crate::adapter::AdapterContext,
    ) -> Result<(SyncTicket, Option<(u64, u64, u64)>), (DmaBuffer, VirtioError)> {
        let chain = if in1_len > 0 {
            Chain::Meta2 { in0_len, in1_len }
        } else {
            Chain::Meta1 { in0_len }
        };
        if in0_len == 0 || resp_len == 0 || resp_len > SYNC_RESP_MAX {
            return Err((meta, VirtioError::DeviceError));
        }

        let fenced_scanout = scanout_bind.is_some_and(|(_, fenced)| fenced);
        let reserved_fence = if fenced_scanout {
            let Some(wire_fence_limit) = self.wire_fence_base.checked_add(D4_FENCE_OFFSET) else {
                WIRE_FENCE_NAMESPACE_EXHAUSTED.fetch_add(1, Ordering::Relaxed);
                return Err((meta, VirtioError::WireFenceNamespaceExhausted));
            };
            if self.next_wire_fence >= wire_fence_limit
                || in0_len < core::mem::size_of::<VirtioGpuCtrlHdr>()
            {
                WIRE_FENCE_NAMESPACE_EXHAUSTED.fetch_add(1, Ordering::Relaxed);
                return Err((meta, VirtioError::WireFenceNamespaceExhausted));
            }
            // SAFETY: the complete global command header is contained in in0.
            let mut header = unsafe {
                core::ptr::read_unaligned(meta.as_slice().as_ptr().cast::<VirtioGpuCtrlHdr>())
            };
            if header.type_ != VIRTIO_GPU_CMD_SET_SCANOUT_BLOB
                || header.flags != 0
                || header.fence_id != 0
                || header.ctx_id != 0
                || header.ring_idx != 0
                || header.padding != [0; 3]
            {
                return Err((meta, VirtioError::DeviceError));
            }
            header.flags = VIRTIO_GPU_FLAG_FENCE;
            header.fence_id = self.next_wire_fence;
            // SAFETY: same complete in-buffer header.
            unsafe {
                core::ptr::write_unaligned(
                    meta.as_mut_slice().as_mut_ptr().cast::<VirtioGpuCtrlHdr>(),
                    header,
                );
            }
            Some(self.next_wire_fence)
        } else {
            None
        };

        let sequence_request = scanout_bind.map(|(resource_id, _)| (adapter, resource_id));
        let (token, reserved_sequence) = match self.enqueue_core(
            chain,
            &meta,
            None,
            resp_len,
            sequence_request,
        ) {
            Ok(accepted) => accepted,
            Err(error) => {
                if reserved_fence.is_some() {
                    // SAFETY: restore the exact header patched above before
                    // returning the unaccepted buffer to its PASSIVE owner.
                    let mut header = unsafe {
                        core::ptr::read_unaligned(
                            meta.as_slice().as_ptr().cast::<VirtioGpuCtrlHdr>(),
                        )
                    };
                    header.flags = 0;
                    header.fence_id = 0;
                    unsafe {
                        core::ptr::write_unaligned(
                            meta.as_mut_slice().as_mut_ptr().cast::<VirtioGpuCtrlHdr>(),
                            header,
                        );
                    }
                }
                return Err((meta, error));
            }
        };
        if reserved_fence.is_some() {
            self.next_wire_fence += 1;
        }
        let identity = scanout_bind
            .zip(reserved_sequence)
            .map(|(_, sequence)| {
                (
                    self.scanout_transport_instance,
                    sequence,
                    reserved_fence.unwrap_or(0),
                )
            });
        self.publish_then_notify(InFlight {
            token,
            kind: InFlightKind::Sync {
                waiter: Some(waiter),
            },
            meta,
            chain,
            resp_len,
            venus: None,
        });
        Ok((SyncTicket { token }, identity))
    }

    pub(crate) const fn scanout_transport_instance(&self) -> u64 {
        self.scanout_transport_instance
    }

    pub(crate) fn interrupt_queue(&self) -> &InterruptQueue {
        &self.queue
    }

    pub(crate) fn enqueue_direct_scanout_dispatch(
        &self,
        adapter: &crate::adapter::AdapterContext,
        work: crate::ddi::direct_scanout::QueuedDirectScanoutBinding,
    ) -> Result<(), VirtioError> {
        self.queue.enqueue_direct_at_dispatch(adapter, work)
    }

    pub(crate) fn direct_queue_holds_allocation(
        &self,
        handle: usize,
        generation: u64,
        resource_id: u32,
    ) -> Result<bool, VirtioError> {
        self.queue
            .direct_holds_allocation(handle, generation, resource_id)
    }

    pub(crate) fn finish_physical_reset_and_abort(
        &mut self,
        expected_instance: u64,
        status: u32,
        spins: u32,
    ) -> Result<u32, VirtioError> {
        if expected_instance == 0 || expected_instance != self.scanout_transport_instance {
            return Err(VirtioError::DeviceError);
        }
        if !self.failed {
            self.latch_failed_and_fail_inflight();
        }
        if status == 0 {
            self.terminalize_native_after_physical_reset();
        }
        if status != 0 {
            crate::diag::fault(crate::diag::FaultCounter::StVioR, spins);
        }
        Ok(status)
    }

    /// A failed transport parks in-flight buffers because a ring latch alone
    /// does not prove the device stopped DMA. Only a successful physical reset
    /// may detach native custody from those parked entries and turn it into an
    /// explicit failed terminal.
    fn terminalize_native_after_physical_reset(&mut self) {
        for entry in &mut self.parked {
            let Some(completion) = take_native_completion(&mut entry.kind) else {
                continue;
            };
            if self.native_terminals.len() < MAX_INFLIGHT
                && self.native_terminals.len() < self.native_terminals.capacity()
            {
                self.native_terminals.push(completion.terminal(false));
            } else {
                PARKED_LEAKS.fetch_add(1, Ordering::Relaxed);
                core::mem::forget(completion);
            }
        }
    }

    /// Reset a fully initialized transport candidate that was never published.
    /// Any externally published ISR pointer into the candidate must already be
    /// cleared before this consumes it.
    /// A candidate whose raw status cannot be proven zero is deliberately leaked:
    /// its queue/storage allocations must remain valid for any device DMA that
    /// the failed reset did not stop.
    pub(crate) fn reset_unpublished_or_retain(
        passive: crate::irql::PassiveLevel,
        mut candidate: Box<Self>,
    ) -> Result<(), VirtioError> {
        let instance = candidate.scanout_transport_instance();
        let reset = candidate.queue.reset_status_and_poll(passive, instance);
        match reset.and_then(|(status, spins)| {
            candidate.finish_physical_reset_and_abort(instance, status, spins)
        }) {
            Ok(0) => Ok(()),
            Ok(_) | Err(_) => {
                core::mem::forget(candidate);
                Err(VirtioError::DeviceError)
            }
        }
    }

    /// Enqueue a control command without a blocking waiter.  Completion still
    /// consumes and validates the device response in [`Self::drain_used`], owns
    /// `meta` until then, clears the adapter-owned `completion` gate, and wakes
    /// `wake_event`.  The pointed-to objects must remain live until transport
    /// teardown; the scanout caller uses fields embedded in `AdapterContext`,
    /// whose lifetime encloses the virtio transport.
    pub fn enqueue_async_submit(
        &mut self,
        ctx_id: u32,
        ring_idx: u32,
        meta: DmaBuffer,
        venus: DmaBuffer,
        venus_len: usize,
    ) -> Result<u64, (DmaBuffer, DmaBuffer, VirtioError)> {
        self.enqueue_submit_inner(ctx_id, ring_idx, meta, venus, venus_len, None)
        .map_err(|(meta, venus, native, error)| {
            debug_assert!(native.is_none());
            (meta, venus, error)
        })
    }

    /// Enqueue one already-validated HNR2 batch on its exact nonzero session
    /// endpoint. The move-only completion token returns intact on every refusal
    /// and is published into the ordinary in-flight entry only after `add`
    /// accepts the descriptor chain.
    pub(crate) fn enqueue_native_submit(
        &mut self,
        ctx_id: u32,
        ring_idx: u32,
        meta: DmaBuffer,
        venus: DmaBuffer,
        venus_len: usize,
        completion: crate::ddi::native_render::NativeHostCompletion,
    ) -> Result<
        u64,
        (
            DmaBuffer,
            DmaBuffer,
            Option<crate::ddi::native_render::NativeHostCompletion>,
            VirtioError,
        ),
    > {
        if ctx_id == 0 || ring_idx == 0 {
            return Err((
                meta,
                venus,
                Some(completion),
                VirtioError::DeviceError,
            ));
        }
        self.enqueue_submit_inner(
            ctx_id,
            ring_idx,
            meta,
            venus,
            venus_len,
            Some(completion),
        )
    }

    /// Shared body for ordinary and native Venus submissions.
    fn enqueue_submit_inner(
        &mut self,
        ctx_id: u32,
        ring_idx: u32,
        mut meta: DmaBuffer,
        venus: DmaBuffer,
        venus_len: usize,
        mut native_completion: Option<crate::ddi::native_render::NativeHostCompletion>,
    ) -> Result<
        u64,
        (
            DmaBuffer,
            DmaBuffer,
            Option<crate::ddi::native_render::NativeHostCompletion>,
            VirtioError,
        ),
    > {
        let hdr_len = core::mem::size_of::<VirtioGpuCmdSubmit>();
        let resp_len = core::mem::size_of::<VirtioGpuCtrlHdr>();
        if self.failed {
            return Err((meta, venus, native_completion, VirtioError::DeviceError));
        }
        if native_completion.is_some()
            && (self.native_terminals.capacity() < MAX_INFLIGHT
                || self
                    .inflight
                    .len()
                    .saturating_add(self.native_terminals.len())
                    >= MAX_INFLIGHT)
        {
            return Err((meta, venus, native_completion, VirtioError::QueueFull));
        }
        let Some(wire_fence_limit) = self.wire_fence_base.checked_add(D4_FENCE_OFFSET)
        else {
            WIRE_FENCE_NAMESPACE_EXHAUSTED.fetch_add(1, Ordering::Relaxed);
            return Err((
                meta,
                venus,
                native_completion,
                VirtioError::WireFenceNamespaceExhausted,
            ));
        };
        if self.next_wire_fence >= wire_fence_limit {
            WIRE_FENCE_NAMESPACE_EXHAUSTED.fetch_add(1, Ordering::Relaxed);
            return Err((
                meta,
                venus,
                native_completion,
                VirtioError::WireFenceNamespaceExhausted,
            ));
        }
        if venus_len == 0
            || venus_len > venus.as_slice().len()
            || hdr_len + resp_len > meta.as_slice().len()
        {
            return Err((meta, venus, native_completion, VirtioError::DeviceError));
        }
        let fence_id = self.next_wire_fence;
        let mut cmd = VirtioGpuCmdSubmit::zeroed();
        cmd.hdr.type_ = VIRTIO_GPU_CMD_SUBMIT_3D;
        cmd.hdr.flags = VIRTIO_GPU_FLAG_FENCE;
        cmd.hdr.fence_id = fence_id;
        cmd.hdr.ctx_id = ctx_id;
        if ring_idx != 0 {
            cmd.hdr.flags |= VIRTIO_GPU_FLAG_INFO_RING_IDX;
            cmd.hdr.ring_idx = ring_idx.min(u8::MAX as u32) as u8;
        }
        cmd.size = venus_len as u32;
        meta.as_mut_slice()[..hdr_len].copy_from_slice(bytemuck::bytes_of(&cmd));

        let chain = Chain::MetaPlusVenus { hdr_len, venus_len };
        let token = match self.enqueue_core(chain, &meta, Some(&venus), resp_len, None) {
            Ok((token, None)) => token,
            Ok((_, Some(_))) => {
                return Err((meta, venus, native_completion, VirtioError::DeviceError))
            }
            Err(e) => return Err((meta, venus, native_completion, e)),
        };
        // Stays BETWEEN a successful `add` and the publish: the wire fence id
        // is only spent once the device has actually taken the descriptor.
        // Proven strictly below the checked transport limit above.
        self.next_wire_fence += 1;
        let ring = cmd.hdr.ring_idx;
        ASYNC_SUBMIT_COUNT.fetch_add(1, Ordering::Relaxed);
        if ring != 0 {
            RING_SUBMIT_COUNT.fetch_add(1, Ordering::Relaxed);
        }
        self.publish_then_notify(InFlight {
            token,
            kind: InFlightKind::AsyncVenus {
                fence_id,
                ring_idx: ring,
                native_completion: native_completion.take(),
            },
            meta,
            chain,
            resp_len,
            venus: Some(venus),
        });
        Ok(fence_id)
    }

    /// Set the ring-corruption latch and fail every in-flight entry exactly
    /// once. Call at the moment the latch is set, never later, and always under
    /// the device spinlock.
    ///
    /// This mirrors the success path's waiter and native-completion ownership:
    /// synchronous stack waiters are terminalized, while move-only native
    /// completions remain attached to the parked entry until PASSIVE teardown.
    fn latch_failed_and_fail_inflight(&mut self) {
        self.failed = true;
        self.queue.mark_failed();
        while let Some(entry) = self.inflight.pop() {
            match entry.kind {
                InFlightKind::Sync { waiter } => {
                    if let Some(block) = waiter {
                        // No response is copied: TransportAborted is the exact
                        // terminal state and readers never decode `resp` for it.
                        //
                        // SAFETY: identical to the success path's Sync arm in
                        // `drain_used`, and audited with it for the 22.22.218.0
                        // `0xA` (ROADMAP defect 0ab-C). The waiter has exactly
                        // two exits and both keep the block alive across these
                        // accesses: the SIGNAL, where the kernel's stack-event
                        // contract forbids resuming before `KeSetEvent` is done
                        // with the dispatcher object; and the TIMEOUT, whose
                        // `abandon_sync` runs under THIS lock and therefore
                        // either cleared `waiter` before us (it is still `Some`,
                        // so it did not) or runs after this arm completes. No
                        // lock-free disposition poll can authorize an exit any more —
                        // that fast path is what made this pattern unsound.
                        // TransportAborted is Release-published before KeSetEvent.
                        unsafe {
                            let b = block.as_ptr();
                            if (*b).publish_terminal(WaitDisposition::TransportAborted) {
                                KeSetEvent(&mut (*b).event, IO_NO_INCREMENT, 0);
                            }
                        }
                    }
                }
                InFlightKind::AsyncVenus { .. } => {}
            }
            // Park, never free: the host may still be DMAing into these buffers,
            // and DmaBuffer frees are PASSIVE-only. Same policy as the success
            // path.
            if self.parked.len() < MAX_PARKED {
                self.parked.push(entry);
                bump_high_water(&PARKED_HIGH_WATER, self.parked.len());
            } else {
                PARKED_LEAKS.fetch_add(1, Ordering::Relaxed);
                core::mem::forget(entry);
            }
        }

        // No wire fence can ever retire now, so release every bounded KMD
        // waiter rather than leaving it blocked on a dead transport.
        while let Some(w) = self.fence_waiters.pop() {
            // SAFETY: registered blocks stay valid until deregistration, which
            // happens under this same lock.
            unsafe {
                let b = w.block.as_ptr();
                if (*b).publish_terminal(WaitDisposition::TransportAborted) {
                    KeSetEvent(&mut (*b).event, IO_NO_INCREMENT, 0);
                }
            }
        }
    }

    /// Drain every completed entry off the used ring: pop the descriptor chain
    /// (token-matched), signal sync/fence waiters, and park the entry for a
    /// PASSIVE reap. The ONLY used-ring consumer (interrupt DPC + opportunistic
    /// callers under the same spinlock).
    /// Retire one assigned wire fence from the bounded KMD waiter table. The
    /// in-flight entry has already been removed, so the ordinal
    /// predicate used by WAIT_FENCE agrees with these explicit wakeups.
    fn retire_wire_fence_notifications(&mut self, fence_id: u64) {
        if fence_id == 0 {
            return;
        }
        let mut j = 0;
        while j < self.fence_waiters.len() {
            if self.fence_waiters[j].fence_id == fence_id {
                let w = self.fence_waiters.swap_remove(j);
                // SAFETY: registered blocks stay valid until deregistration
                // removes them under this same transport lock.
                unsafe {
                    let b = w.block.as_ptr();
                    if (*b).publish_terminal(WaitDisposition::FenceCompleted) {
                        KeSetEvent(&mut (*b).event, IO_NO_INCREMENT, 0);
                    }
                }
            } else {
                j += 1;
            }
        }
    }

    pub fn drain_used(&mut self, adapter: &crate::adapter::AdapterContext) {
        if self.failed {
            return;
        }
        if self.queue.is_failed() {
            self.latch_failed_and_fail_inflight();
            return;
        }
        loop {
            let token = match self.queue.peek_used() {
                Ok(Some(token)) => token,
                Ok(None) => return,
                Err(VirtioError::QueueFull) => return,
                Err(_) => {
                    self.latch_failed_and_fail_inflight();
                    return;
                }
            };
            match self.queue.take_direct_completion(token) {
                Ok(Some(completion)) => {
                    crate::ddi::direct_scanout::complete_queued(adapter, completion);
                    continue;
                }
                Ok(None) => {}
                Err(VirtioError::QueueFull) => return,
                Err(_) => {
                    self.latch_failed_and_fail_inflight();
                    return;
                }
            }
            let Some(idx) = self.inflight.iter().position(|entry| entry.token == token) else {
                DRAIN_BAD_TOKEN.fetch_add(1, Ordering::Relaxed);
                self.latch_failed_and_fail_inflight();
                return;
            };
            let (spans, resp_len) = {
                let entry = &self.inflight[idx];
                (
                    entry
                        .chain
                        .spans(&entry.meta, entry.venus.as_ref(), entry.resp_len),
                    entry.resp_len,
                )
            };
            let Some((reads, count, response)) = spans else {
                DRAIN_BAD_TOKEN.fetch_add(1, Ordering::Relaxed);
                self.latch_failed_and_fail_inflight();
                return;
            };
            let written_length = match self.queue.pop_used(token, reads, count, response) {
                Ok(length) => length,
                Err(VirtioError::QueueFull) => return,
                Err(_) => {
                    self.latch_failed_and_fail_inflight();
                    return;
                }
            };
            if written_length as usize > resp_len {
                DRAIN_BAD_USED_LENGTH.fetch_add(1, Ordering::Relaxed);
                self.latch_failed_and_fail_inflight();
                return;
            }

            let mut entry = self.inflight.swap_remove(idx);
            let native_completion = take_native_completion(&mut entry.kind);
            let response_base = {
                // SAFETY: the response span is within the entry-owned buffer.
                unsafe { response.as_slice() }.as_ptr()
            };
            let response_type =
                (written_length as usize >= size_of::<VirtioGpuCtrlHdr>()).then(|| {
                    // SAFETY: the used length covers the response type word.
                    unsafe { core::ptr::read_unaligned(response_base.cast::<u32>()) }
                });

            match entry.kind {
                InFlightKind::Sync { waiter } => {
                    if let Some(block) = waiter {
                        // SAFETY: the stack waiter remains registered until this
                        // lock publishes a terminal or abandonment removes it.
                        unsafe {
                            let block = block.as_ptr();
                            if (*block).disposition() == WaitDisposition::Pending {
                                (*block)
                                    .response_written
                                    .store(written_length, Ordering::Relaxed);
                                core::ptr::copy_nonoverlapping(
                                    response_base,
                                    (*block).resp.get().cast::<u8>(),
                                    written_length as usize,
                                );
                                if (*block)
                                    .publish_terminal(WaitDisposition::HostResponseAvailable)
                                {
                                    KeSetEvent(&mut (*block).event, IO_NO_INCREMENT, 0);
                                }
                            }
                        }
                    }
                }
                InFlightKind::AsyncVenus {
                    fence_id,
                    ring_idx,
                    native_completion: _,
                } => {
                    ASYNC_COMPLETE_COUNT.fetch_add(1, Ordering::Relaxed);
                    if ring_idx != 0 {
                        RING_COMPLETE_COUNT.fetch_add(1, Ordering::Relaxed);
                    }
                    let response_ok = written_length as usize
                        == size_of::<VirtioGpuCtrlHdr>()
                        && response_type == Some(VIRTIO_GPU_RESP_OK_NODATA);
                    if !response_ok {
                        ASYNC_RESP_ERRORS.fetch_add(1, Ordering::Relaxed);
                    }
                    self.retire_wire_fence_notifications(fence_id);
                    if let Some(completion) = native_completion {
                        if self.native_terminals.len() < MAX_INFLIGHT
                            && self.native_terminals.len() < self.native_terminals.capacity()
                        {
                            self.native_terminals.push(completion.terminal(response_ok));
                            crate::ddi::interrupt::request_wddm_completion_dpc(adapter);
                        } else {
                            PARKED_LEAKS.fetch_add(1, Ordering::Relaxed);
                            core::mem::forget(completion);
                        }
                    }
                }
            }

            if self.parked.len() < MAX_PARKED {
                self.parked.push(entry);
                bump_high_water(&PARKED_HIGH_WATER, self.parked.len());
            } else {
                PARKED_LEAKS.fetch_add(1, Ordering::Relaxed);
                core::mem::forget(entry);
            }
        }
    }
    /// Number of completed entries awaiting a PASSIVE reap.
    ///
    /// Unused today: `PARKED_LEAKS` is surfaced through the escape's
    /// QUERY_STATS instead, since it has no registry mirror. Kept as the typed
    /// accessor for that population. Pre-dates T6; surfaced when R906 removed
    /// the crate-wide `dead_code` allow over `mod virtio`.
    #[allow(dead_code)]
    pub fn parked_len(&self) -> usize {
        self.parked.len()
    }

    /// Swap out exact native terminals for processing after `virtio_lock` is
    /// released. A second drain leaves the first owner undisturbed.
    pub(crate) fn begin_native_terminal_drain(
        &mut self,
    ) -> Option<alloc::vec::Vec<crate::ddi::native_render::NativeHostTerminal>> {
        if self.native_terminal_drain_in_progress || self.native_terminals.is_empty() {
            return None;
        }
        self.native_terminal_drain_in_progress = true;
        let fresh = core::mem::take(&mut self.native_terminals_spare);
        debug_assert!(fresh.capacity() >= MAX_INFLIGHT);
        Some(core::mem::replace(&mut self.native_terminals, fresh))
    }

    /// Return the empty, pre-reserved terminal vector after every move-only
    /// completion token was discharged outside the transport lock.
    pub(crate) fn finish_native_terminal_drain(
        &mut self,
        mut terminals: alloc::vec::Vec<crate::ddi::native_render::NativeHostTerminal>,
    ) {
        terminals.clear();
        if terminals.capacity() < MAX_INFLIGHT {
            // Capacity never shrinks in any owner path. If that invariant is
            // lost, retain the short vector without allocating at DISPATCH;
            // later pushes take their explicit capacity refusal.
            PARKED_LEAKS.fetch_add(1, Ordering::Relaxed);
        }
        self.native_terminals_spare = terminals;
        self.native_terminal_drain_in_progress = false;
    }

    /// Begin one PASSIVE reap by swapping in the empty pre-reserved parked
    /// vector and lending the pre-reserved DMA-buffer scratch vector. A second
    /// caller returns None and leaves the active reaper to finish.
    pub fn begin_parked_reap(&mut self) -> Option<(Vec<InFlight>, Vec<DmaBuffer>)> {
        if self.reap_in_progress
            || self.parked.is_empty()
            || self
                .parked
                .iter()
                .any(|entry| has_native_completion(&entry.kind))
        {
            return None;
        }
        self.reap_in_progress = true;
        let fresh = core::mem::take(&mut self.parked_spare);
        debug_assert!(fresh.capacity() >= MAX_PARKED);
        let dead = core::mem::replace(&mut self.parked, fresh);
        let buffers = core::mem::take(&mut self.reap_buffers_spare);
        debug_assert!(buffers.capacity() >= 2 * MAX_PARKED);
        Some((dead, buffers))
    }

    /// Return the emptied pre-reserved reap vectors after excess DMA buffers
    /// were dropped at PASSIVE_LEVEL.
    pub fn finish_parked_reap(&mut self, mut entries: Vec<InFlight>, mut buffers: Vec<DmaBuffer>) {
        // These were debug_assert-only, i.e. absent from the release driver
        // (kmd_render sets no debug-assertions). They guard the capacity the
        // `parked.push` path relies on to never reallocate under the spinlock,
        // so enforce them for real: clear, and replace any vector whose capacity
        // fell below its reserve with a freshly reserved one. Both allocations
        // happen HERE, at PASSIVE, never under the lock.
        entries.clear();
        buffers.clear();
        if entries.capacity() < MAX_PARKED {
            entries = Vec::with_capacity(MAX_PARKED);
        }
        if buffers.capacity() < 2 * MAX_PARKED {
            buffers = Vec::with_capacity(2 * MAX_PARKED);
        }
        self.parked_spare = entries;
        self.reap_buffers_spare = buffers;
        self.reap_in_progress = false;
    }

    /// Undo [`Self::begin_parked_reap`] on a failure path, restoring both spares
    /// and clearing the in-progress flag.
    ///
    /// Without this, an early return between begin and finish stranded
    /// `reap_in_progress` at true AND left both pre-reserved spares taken, so
    /// reaping was permanently disabled and every later enqueue hit the
    /// `PARKED_ENQUEUE_GATE` refusal. Latent today only because the sole
    /// reachable early return is a concurrent StopDevice, after which the whole
    /// `VirtioGpu` is replaced - but that is a property of today's callers, not
    /// of the protocol.
    pub fn abort_parked_reap(&mut self, entries: Vec<InFlight>, buffers: Vec<DmaBuffer>) {
        REAP_ABANDONED.fetch_add(1, Ordering::Relaxed);
        self.finish_parked_reap(entries, buffers);
    }

    /// Take one already-allocated DMA buffer whose page capacity covers `len`.
    /// The caller allocates a new buffer at PASSIVE only when this returns None.
    pub fn take_dma_buffer(&mut self, len: usize) -> Option<DmaBuffer> {
        // `reset(0)` fails, and the only failure return below dropped the
        // DmaBuffer INSIDE the `with_virtio` closure — i.e. under
        // KeAcquireSpinLockRaiseToDpc — where `DmaBuffer::drop` calls
        // MmFreeContiguousMemory, which hal.rs states is PASSIVE-only. Every
        // sibling API hands buffers back through `Err((DmaBuffer, VirtioError))`
        // precisely so the free happens at PASSIVE; this was the one path that
        // broke the convention. Reject len == 0 up front, and swap-remove only
        // after the capacity filter has proved `reset` will succeed — which
        // also closes the accounting hole where dma_pool_bytes and
        // DMA_POOL_CACHED_BYTES were decremented before the failure return.
        if len == 0 {
            DMA_POOL_MISSES.fetch_add(1, Ordering::Relaxed);
            return None;
        }
        let Some((idx, _)) = self
            .dma_pool
            .iter()
            .enumerate()
            .filter(|(_, buf)| buf.capacity() >= len)
            .min_by_key(|(_, buf)| buf.capacity())
        else {
            DMA_POOL_MISSES.fetch_add(1, Ordering::Relaxed);
            return None;
        };
        let mut buf = self.dma_pool.swap_remove(idx);
        self.dma_pool_bytes = self.dma_pool_bytes.saturating_sub(buf.capacity());
        DMA_POOL_CACHED_BYTES.store(self.dma_pool_bytes as u32, Ordering::Relaxed);
        // Infallible here: the filter proved capacity >= len, and len > 0.
        if !buf.reset(len) {
            DMA_POOL_MISSES.fetch_add(1, Ordering::Relaxed);
            return None;
        }
        DMA_POOL_HITS.fetch_add(1, Ordering::Relaxed);
        Some(buf)
    }

    /// Move eligible completed buffers into the bounded pool without allocation.
    /// Any excess remains in `buffers` and is returned for PASSIVE-level drop.
    pub fn recycle_dma_buffers(&mut self, mut buffers: Vec<DmaBuffer>) -> Vec<DmaBuffer> {
        let mut i = 0;
        while i < buffers.len() {
            let capacity = buffers[i].capacity();
            let eligible = capacity <= MAX_DMA_POOL_BUFFER_BYTES
                && self.dma_pool.len() < MAX_DMA_POOL
                && self.dma_pool_bytes.saturating_add(capacity) <= MAX_DMA_POOL_BYTES;
            if !eligible {
                DMA_POOL_DROPS.fetch_add(1, Ordering::Relaxed);
                i += 1;
                continue;
            }
            let buf = buffers.swap_remove(i);
            self.dma_pool_bytes += capacity;
            self.dma_pool.push(buf);
        }
        DMA_POOL_CACHED_BYTES.store(self.dma_pool_bytes as u32, Ordering::Relaxed);
        buffers
    }

    /// Abandon a timed-out synchronous entry: detach its waiter so the eventual
    /// completion signals nobody (the entry itself is reaped when it completes).
    /// Returns `AlreadyCompleted` if the entry already reached either terminal;
    /// the wait block distinguishes a copied host response from transport abort.
    pub fn abandon_sync(
        &mut self,
        ticket: SyncTicket,
        block: NonNull<SyncWaitBlock>,
    ) -> SyncOutcome {
        for e in self.inflight.iter_mut() {
            if e.token != ticket.token {
                continue;
            }
            if let InFlightKind::Sync { waiter, .. } = &mut e.kind {
                if *waiter == Some(block) {
                    *waiter = None;
                    return SyncOutcome::Abandoned;
                }
                // The token is ours but the waiter is a DIFFERENT block, or the
                // entry is not a Sync at all. Either way this waiter's response
                // buffer was never written.
                return SyncOutcome::NotOurs;
            }
            return SyncOutcome::NotOurs;
        }
        SyncOutcome::AlreadyCompleted
    }

    // ── Wire-fence table (WAIT_FENCE) ────────────────────────────────────────

    /// Prepare a wait on wire fence `fence_id`, registering `block` if the
    /// fence is still in flight. Runs under the device spinlock — the
    /// in-flight check and the registration are atomic with respect to
    /// [`Self::drain_used`], so a completion can never fall between them.
    ///
    /// Completion predicate (System-class phase4e model): wire ids are
    /// assigned by this transport, monotonic and never reused, and every
    /// assigned id lives in `inflight` until its used-ring completion — so
    /// `id < next_wire_fence && not in-flight` ⇒ complete.
    ///
    /// ⚠ "ASSIGNED BY THIS TRANSPORT" IS THE LOAD-BEARING WORD, and until
    /// 2026-08-06 nothing tested it: the range check was one-sided, and StartDevice
    /// strides the id space up by 2^32, so an id from a PREVIOUS generation is
    /// below the range and reached the `Complete` arm. The ICD was then told a wire
    /// fence had retired when its whole transport generation was gone — exactly
    /// [`TRANSPORT_GONE_AT_WAIT`]'s failure, reported as success.
    pub fn fence_wait_prepare(
        &mut self,
        fence_id: u64,
        block: NonNull<SyncWaitBlock>,
    ) -> FenceWaitPrep {
        // A failed transport can never retire a fence, so parking a PASSIVE
        // waiter against one is a guaranteed timeout at best.
        if self.failed || fence_id == 0 || fence_id >= self.next_wire_fence {
            return FenceWaitPrep::Invalid;
        }
        if fence_id < self.wire_fence_base {
            FENCE_ID_FOREIGN_GENERATION.fetch_add(1, Ordering::Relaxed);
            return FenceWaitPrep::Invalid;
        }
        let in_flight = self.inflight.iter().any(|entry| {
            matches!(
                entry.kind,
                InFlightKind::AsyncVenus { fence_id: assigned, .. } if assigned == fence_id
            )
        });
        if !in_flight {
            // SAFETY: `block` is the initialized, frame-borrowed waiter supplied
            // by the caller, and this lock excludes every terminal publisher.
            unsafe {
                let _ = block
                    .as_ref()
                    .publish_terminal(WaitDisposition::FenceCompleted);
            }
            return FenceWaitPrep::Complete;
        }
        if self.fence_waiters.len() >= MAX_FENCE_WAITERS {
            return FenceWaitPrep::TableFull;
        }
        self.fence_waiters.push(FenceWaiter { fence_id, block });
        FENCE_WAIT_REGISTERED.fetch_add(1, Ordering::Relaxed);
        FenceWaitPrep::Registered
    }

    /// Deregister a timed-out fence waiter. Returns `true` if completion or
    /// transport abort signaled and removed it first; disposition distinguishes them.
    pub fn fence_wait_cancel(&mut self, block: NonNull<SyncWaitBlock>) -> bool {
        if let Some(i) = self.fence_waiters.iter().position(|w| w.block == block) {
            self.fence_waiters.swap_remove(i);
            false
        } else {
            true
        }
    }

    // ── WDDM pending-fence FIFO (SubmitCommand → DPC completion) ─────────────

    /// Whether every async wire fence `< watermark` in `domain` has retired.
    fn async_retired_up_to(&self, watermark: u64, domain: RetireDomain) -> bool {
        watermark == 0
            || !self.inflight.iter().any(|e| match e.kind {
                InFlightKind::AsyncVenus {
                    fence_id, ring_idx, ..
                } => {
                    fence_id < watermark
                        && match domain {
                            RetireDomain::IncludingGpu => true,
                            RetireDomain::DecodeOnly => ring_idx == 0,
                        }
                }
                _ => false,
            })
    }

    /// Whether the ONE async wire fence `fence_id` has retired.
    ///
    /// ⛔ THE FRAME'S OWN BOUNDARY, and the point of A4. Where
    /// [`Self::async_retired_up_to`] asks *"has everything below this retired"* —
    /// a prefix over every ring and every process — this asks only about the fence
    /// the submitting UMD actually named. Every id below `next_wire_fence` was
    /// genuinely assigned AND enqueued (the counter is bumped only after
    /// `control.add` succeeds, in the same spinlock section as the `inflight`
    /// push), so an id that is not in flight has necessarily retired: absence is a
    /// completion proof here, not an unknown.
    ///
    /// ⚠ NO `RetireDomain` FILTER, deliberately. A domain filter over a SINGLE
    /// a ring filter could only ever fake readiness — "ring 1, so ignore it" — never
    /// add safety, and the exact arm is constructed exclusively with
    /// `IncludingGpu` anyway (`gpu_completion_fence.is_some()` forces that domain
    /// one screen above the watermark selection). Taking the domain as a parameter
    /// and ignoring it would have been the trap.
    fn async_exact_retired(&self, fence_id: u64) -> bool {
        fence_id == 0
            || !self.inflight.iter().any(|e| match e.kind {
                InFlightKind::AsyncVenus {
                    fence_id: in_flight,
                    ..
                } => in_flight == fence_id,
                _ => false,
            })
    }

    /// Evaluate one WDDM entry's wire-fence dependency under its own
    /// interpretation. The ONLY caller shape for a `WddmPending`; the bare
    /// [`Self::async_retired_up_to`] keeps its three existing non-WDDM callers.
    fn wire_boundary_ready(
        &self,
        watermark: u64,
        domain: RetireDomain,
        boundary: WireBoundary,
    ) -> bool {
        match boundary {
            WireBoundary::Prefix => self.async_retired_up_to(watermark, domain),
            WireBoundary::Exact => self.async_exact_retired(watermark),
        }
    }

    fn overflow_wddm_pending(&mut self) {
        self.wddm_pending.clear();
    }

    /// Record the compatibility Venus boundary for K9's already-admitted exact
    /// WDDM submission ticket. The returned state never reconstructs or
    /// manufactures a scheduler completion.
    pub fn note_wddm_submission(
        &mut self,
        _order: &crate::adapter::NotifyOrdered<'_>,
        engine_ticket: crate::adapter::OrderedEngineTicket,
        paging: bool,
        gpu_completion_fence: Option<u64>,
        d3d12: bool,
    ) -> WddmAdmission {
        if self.failed {
            WDDM_SIGNAL_AFTER_FAILURE.fetch_add(1, Ordering::Relaxed);
            self.wddm_pending.clear();
            return WddmAdmission::Failed;
        }

        let domain = if gpu_completion_fence.is_some() {
            RetireDomain::IncludingGpu
        } else if paging || !self.dma_gpu_fence {
            RetireDomain::DecodeOnly
        } else {
            RetireDomain::IncludingGpu
        };
        let (watermark, wire_boundary) = if paging {
            (0, WireBoundary::Prefix)
        } else if let Some(gpu_fence_id) = gpu_completion_fence {
            use helios_kmd_logic::wddm_boundary as boundary;
            let selection = boundary::select(
                gpu_fence_id,
                self.wire_fence_base,
                self.next_wire_fence,
                d3d12,
            );
            match selection.rejection {
                boundary::Rejection::OutOfRange => {
                    GPU_FENCE_CLAMPED.fetch_add(1, Ordering::Relaxed);
                }
                boundary::Rejection::ForeignGeneration => {
                    GPU_FENCE_FOREIGN_GENERATION.fetch_add(1, Ordering::Relaxed);
                }
                boundary::Rejection::Accepted => {}
            }
            let wire_boundary = match selection.kind {
                boundary::Kind::Prefix => WireBoundary::Prefix,
                boundary::Kind::Exact => {
                    D3D12_EXACT_WATERMARK_USED.fetch_add(1, Ordering::Relaxed);
                    WireBoundary::Exact
                }
            };
            (selection.watermark, wire_boundary)
        } else {
            (self.next_wire_fence, WireBoundary::Prefix)
        };

        if self.wddm_pending.is_empty()
            && self.wire_boundary_ready(watermark, domain, wire_boundary)
        {
            return WddmAdmission::HostTerminal;
        }
        if self.wddm_pending.len() >= MAX_WDDM_PENDING {
            WDDM_PENDING_OVERFLOWS.fetch_add(1, Ordering::Relaxed);
            self.overflow_wddm_pending();
            return WddmAdmission::Failed;
        }
        self.wddm_pending.push_back(WddmPending {
            engine_ticket,
            watermark,
            wire_boundary,
            domain,
        });
        WddmAdmission::Pending
    }
    /// Pop the head-of-FIFO WDDM submission once its real producer boundary
    /// has retired. The FIFO remains strictly ordered; no completion is
    /// synthesized or allowed to bypass an older ticket.
    pub fn take_one_ready_wddm(
        &mut self,
        _order: &crate::adapter::NotifyOrdered<'_>,
    ) -> WddmTake {
        let Some(head) = self.wddm_pending.front() else {
            return WddmTake::Empty;
        };
        if !self.wire_boundary_ready(head.watermark, head.domain, head.wire_boundary) {
            WDDM_HEAD_BLOCKED_WIRE.fetch_add(1, Ordering::Relaxed);
            return WddmTake::BlockedOnProducer;
        }
        let Some(pending) = self.wddm_pending.pop_front() else {
            return WddmTake::Empty;
        };
        WDDM_FENCE_FROM_DPC.fetch_add(1, Ordering::Relaxed);
        WddmTake::Ready(WddmReady { pending })
    }


    /// Put a popped-but-undelivered submission back at the head of the FIFO.
    ///
    /// dxgkrnl requires monotonic SubmissionFenceId completion, so the entry
    /// must go back where it came from — `push_front` of the entry just popped
    /// is the only correct form. It also cannot exceed the
    /// `VecDeque::with_capacity(MAX_WDDM_PENDING)` reserve, so nothing
    /// reallocates under the device spinlock.
    pub fn requeue_wddm_front(
        &mut self,
        _order: &crate::adapter::NotifyOrdered<'_>,
        ready: WddmReady,
    ) {
        // Terminal membership remains in the FIFO until the notification
        // succeeds, so requeue only restores the WDDM entry. A failed callback
        // cannot lose a merged same-stream prefix.
        self.wddm_pending.push_front(ready.pending);
    }

    /// Preemption: drop every pending WDDM submission (dxgkrnl resubmits the
    /// unfinished DMA buffers with fresh fence ids after the preempt completes;
    /// the underlying venus work keeps executing host-side). Returns the count
    /// dropped.
    pub fn preempt_flush(&mut self, _order: &crate::adapter::NotifyOrdered<'_>) -> u32 {
        let n = self.wddm_pending.len() as u32;
        self.wddm_pending.clear();
        n
    }

    /// Terminally abandon the compatibility FIFO after K9 has already
    /// invalidated the authoritative ordered-engine generation.
    pub fn terminal_abandon_wddm_epoch(
        &mut self,
        _order: &crate::adapter::NotifyOrdered<'_>,
    ) -> u32 {
        let n = self.wddm_pending.len() as u32;
        self.wddm_pending.clear();
        n
    }

    /// Scanout-0 preferred `(width, height)` from the host's `GET_DISPLAY_INFO` at
    /// init, or `None` if the host reported nothing usable. The display half drives
    /// its VidPn mode + generated EDID from this so it presents the size QEMU wants.
    pub fn display_mode(&self) -> Option<(u32, u32)> {
        self.display_mode
    }

    /// Mapped kernel VA of the virtio ISR-status register (read-to-clear), or 0 if
    /// the device exposes no ISR cap. `DxgkDdiStartDevice` copies this into the
    /// `AdapterContext` so the DIRQL ISR can acknowledge the INTx line lock-free.
    pub fn isr_status_addr(&self) -> usize {
        self.isr_status_va
    }
}

impl Drop for VirtioGpu {
    fn drop(&mut self) {
        // SAFETY: every VirtioGpu owner drops at the documented PASSIVE
        // lifecycle/reap edge; DmaBuffer teardown below has the same contract.
        let passive = unsafe { crate::irql::PassiveLevel::assume() };
        let _ = self
            .queue
            .reset_status_and_poll(passive, self.scanout_transport_instance);
        // The reset above quiesced the device before the in-flight/parked entry
        // buffers free with this struct.
        //
        // The BAR MMIO mappings made inside `PciTransport` are intentionally NOT
        // freed here: `WdkHal` caches them by physical address and reuses them on
        // the next StartDevice (the BARs are stable across stop/start), so there
        // is no per-cycle leak. The cache is released wholesale in
        // `DxgkDdiUnload` via `WdkHal::unmap_all`.
    }
}
