//! K11 — one escape-free stock Venus transport per live HTS1 session.
//!
//! The transport owns no discovery namespace.  Its only backing identity is the
//! canonical allocation pointer retained by the raw device's ordinary role-1
//! `OpenAllocationContext`; its only host owner is that exact raw KMT device.
//! Each session owns one fixed private Venus SHM reply resource, and every
//! direct submit borrows that canonical resource/context pair so Stop/reset
//! rundown sees it. K2a remains only the Mesa-visible HVR1 carrier.

use core::cell::UnsafeCell;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicU32, Ordering};

use helios_protocol::native_render::{HELIOS_HVM1_REPLY_POOL_BYTES, HELIOS_HVR1_HEADER_SIZE};
use helios_protocol::{
    VIRTIO_GPU_MAP_CACHE_CACHED, VIRTIO_GPU_MAP_CACHE_UNCACHED, VIRTIO_GPU_MAP_CACHE_WC,
};
use wdk_sys::ntddk::{
    KeClearEvent, KeInitializeEvent, KeSetEvent, KeWaitForSingleObject, MmMapIoSpace,
    MmUnmapIoSpace,
};
use wdk_sys::{_MEMORY_CACHING_TYPE, KEVENT, PHYSICAL_ADDRESS, PVOID};

use crate::adapter::AdapterContext;
use crate::ddi::create_allocation::{k11_reply_pool_facts, K11ReplyPoolFacts};
use crate::dxgk::{NTSTATUS, STATUS_DEVICE_NOT_READY, STATUS_INVALID_DEVICE_REQUEST};
use crate::irql::PassiveLevel;
use crate::sync::SpinLock;
use crate::virtio::gpu::DeviceOwner;
use helios_kmd_logic::session_transport as pure;

const VENUS_CAPSET_ID: u32 = 4;
const SESSION_INSTANCE_HANDLE: u64 = 1;
const HOST_REPLY_POISON: u32 = u32::MAX;
// Mirror the four public HVR1 slot offsets in one renderer-private resource.
// Host reply bytes never target the K2a allocation itself; after a real host
// terminal the KMD copies only the validated finite range into that slot.
const SESSION_REPLY_BYTES: u64 = HELIOS_HVM1_REPLY_POOL_BYTES;
const SESSION_REPLY_OFFSET: u64 = 0;
// Every fenced ring-zero submit is serialized here, while the virtio enqueue
// mints its id from the same transport-global wire namespace used by native
// decoder teardown.  QEMU retires every matching context-fence command whose
// id is <= the callback id, so mixing small per-session ids with large decoder
// ids would let a delayed teardown callback satisfy a later control before its
// reply was written.  The replyless terminal vkDestroyInstance remains
// deliberately unfenced: QEMU asks the renderer to create an INFO_RING fence
// only after dispatching the stream, but that stream has already extinguished
// the host instance.  The following ordinary CTX_DESTROY is its teardown
// terminal.

pub(crate) static K11_CONTEXT_CREATED: AtomicU32 = AtomicU32::new(0);
pub(crate) static K11_CONTEXT_DESTROYED: AtomicU32 = AtomicU32::new(0);
pub(crate) static K11_HOST_INIT_OK: AtomicU32 = AtomicU32::new(0);
pub(crate) static K11_HOST_INIT_REJECT: AtomicU32 = AtomicU32::new(0);
pub(crate) static K11_HOST_REPLY_REJECT: AtomicU32 = AtomicU32::new(0);
pub(crate) static K11_SYNC_REPLY_REJECT: AtomicU32 = AtomicU32::new(0);
pub(crate) static K11_ASYNC_REPLY_REJECT: AtomicU32 = AtomicU32::new(0);
pub(crate) static K11_REPLY_REJECT_KIND: AtomicU32 = AtomicU32::new(0);
pub(crate) static K11_REPLY_EXPECTED_OPCODE: AtomicU32 = AtomicU32::new(0);
pub(crate) static K11_REPLY_FOUND_OPCODE: AtomicU32 = AtomicU32::new(0);
pub(crate) static K11_REPLY_REJECT_OFFSET: AtomicU32 = AtomicU32::new(0);
pub(crate) static K11_REPLY_REJECT_BYTES: AtomicU32 = AtomicU32::new(0);
pub(crate) static K11_REPLY_PUBLISHED: AtomicU32 = AtomicU32::new(0);
pub(crate) static K11_RUNDOWN_WAITED: AtomicU32 = AtomicU32::new(0);
pub(crate) static K11_COMPLETION_WAITED: AtomicU32 = AtomicU32::new(0);
pub(crate) static K11_COMPLETION_REOPEN_REJECT: AtomicU32 = AtomicU32::new(0);
pub(crate) static K11_STALE_TRANSPORT: AtomicU32 = AtomicU32::new(0);
pub(crate) static K11_CLEANUP_REJECT: AtomicU32 = AtomicU32::new(0);
pub(crate) static K11_REPLY_MAP_GONE: AtomicU32 = AtomicU32::new(0);

const COUNTERS: [(&[u8], &AtomicU32); 19] = [
    (b"K11MapGone", &K11_REPLY_MAP_GONE),
    (b"K11CtxNew", &K11_CONTEXT_CREATED),
    (b"K11CtxDel", &K11_CONTEXT_DESTROYED),
    (b"K11InitOk", &K11_HOST_INIT_OK),
    (b"K11InitRej", &K11_HOST_INIT_REJECT),
    (b"K11ReplyRej", &K11_HOST_REPLY_REJECT),
    (b"K11SyncRpRej", &K11_SYNC_REPLY_REJECT),
    (b"K11AsyncRpRej", &K11_ASYNC_REPLY_REJECT),
    (b"K11RpKind", &K11_REPLY_REJECT_KIND),
    (b"K11RpExpOp", &K11_REPLY_EXPECTED_OPCODE),
    (b"K11RpGotOp", &K11_REPLY_FOUND_OPCODE),
    (b"K11RpOff", &K11_REPLY_REJECT_OFFSET),
    (b"K11RpLen", &K11_REPLY_REJECT_BYTES),
    (b"K11ReplyOk", &K11_REPLY_PUBLISHED),
    (b"K11RdWait", &K11_RUNDOWN_WAITED),
    (b"K11CmpWait", &K11_COMPLETION_WAITED),
    (b"K11CmpOpenRej", &K11_COMPLETION_REOPEN_REJECT),
    (b"K11Stale", &K11_STALE_TRANSPORT),
    (b"K11CleanRej", &K11_CLEANUP_REJECT),
];

pub(crate) fn diag_dump() {
    for (name, counter) in COUNTERS {
        crate::diag::record_named_bytes(name, counter.load(Ordering::Relaxed));
    }
}

#[derive(Clone, Copy)]
struct CompletionRundownState {
    open: bool,
    active: u32,
}

/// Fixed per-adapter rundown for the short SubmitCommand interval from K11
/// revalidation through exact `DXGK_INTERRUPT_DMA_COMPLETED` delivery.
///
/// This is neither a queue nor a timeline and stores no session identity. Its
/// sole purpose is to let PASSIVE reset/stop close new completion admission and
/// wait for the exact callbacks that already crossed that boundary before the
/// scheduler epoch or transport is retired. Ordinary session teardown does not
/// wait here: the host operation is already terminal, and waiting from a
/// same-context Render would deadlock with `DxgkCbSynchronizeExecution`.
pub(crate) struct K11CompletionRundown {
    state: SpinLock<CompletionRundownState>,
    drained: UnsafeCell<KEVENT>,
}

// SAFETY: state is serialized by the spinlock. The event is initialized once
// after the enclosing AdapterContext reaches its final heap address.
unsafe impl Send for K11CompletionRundown {}
unsafe impl Sync for K11CompletionRundown {}

impl K11CompletionRundown {
    pub(crate) fn new() -> Self {
        Self {
            state: SpinLock::new(CompletionRundownState {
                open: false,
                active: 0,
            }),
            drained: UnsafeCell::new(unsafe { core::mem::zeroed() }),
        }
    }

    /// # Safety
    /// `self` must be at its final address and not yet published.
    pub(crate) unsafe fn init_event(&self) {
        // NotificationEvent, initially signaled because active == 0.
        unsafe { KeInitializeEvent(self.drained.get(), 0, 1) };
    }

    pub(crate) fn with_admitted<R>(&self, operation: impl FnOnce() -> R) -> Option<R> {
        let _operation = self.acquire_owned()?;
        Some(operation())
    }

    /// Hold reset/Stop completion rundown beyond the caller's stack frame.
    /// The adapter is heap-pinned and close waits for every returned token, so
    /// the raw pointer cannot outlive its owner.
    pub(crate) fn acquire_owned(&self) -> Option<K11CompletionOperation> {
        let mut state = self.state.lock();
        if !state.open || state.active == u32::MAX {
            return None;
        }
        if state.active == 0 {
            unsafe { KeClearEvent(self.drained.get()) };
        }
        state.active += 1;
        Some(K11CompletionOperation {
            owner: NonNull::from(self),
        })
    }

    pub(crate) fn close_completion_and_wait(&self, _passive: PassiveLevel) {
        let active = {
            let mut state = self.state.lock();
            state.open = false;
            state.active
        };
        if active == 0 {
            return;
        }
        K11_COMPLETION_WAITED.fetch_add(1, Ordering::Relaxed);
        // Exact rundown event, never polling or sleeping. Every admitted
        // SubmitCommand interval owns a Drop guard through notification.
        let _ = unsafe {
            KeWaitForSingleObject(self.drained.get() as PVOID, 0, 0, 0, core::ptr::null_mut())
        };
    }

    pub(crate) fn reopen(&self) {
        let mut state = self.state.lock();
        if state.active == 0 {
            state.open = true;
        } else {
            // A close-and-wait/open lifecycle is not permitted to carry an old
            // callback into the successor scheduler epoch. Stay closed.
            K11_COMPLETION_REOPEN_REJECT.fetch_add(1, Ordering::Relaxed);
        }
    }
}

pub(crate) struct K11CompletionOperation {
    owner: NonNull<K11CompletionRundown>,
}

// SAFETY: the token touches only the owner's nonpaged spinlock/event and the
// owner is joined before its adapter can be freed.
unsafe impl Send for K11CompletionOperation {}

impl Drop for K11CompletionOperation {
    fn drop(&mut self) {
        let owner = unsafe { self.owner.as_ref() };
        let signal = {
            let mut state = owner.state.lock();
            debug_assert!(state.active != 0);
            state.active = state.active.saturating_sub(1);
            state.active == 0
        };
        if signal {
            unsafe { KeSetEvent(owner.drained.get(), 0, 0) };
        }
    }
}

const MAX_SESSION_ATTACHMENTS: usize =
    helios_protocol::native_render::HELIOS_HNR2_MAX_USE_RECORDS as usize;

#[derive(Clone, Copy, PartialEq, Eq)]
enum AttachmentState {
    Attaching,
    Attached,
    Detaching,
}

#[derive(Clone, Copy)]
struct SessionAttachment {
    resource_id: u32,
    references: u32,
    state: AttachmentState,
}

struct LiveHost {
    allocation: usize,
    k2a_resource_id: u32,
    reply_resource_id: u32,
    transport_instance: u64,
    context_id: u32,
    instance_handle: u64,
    reply_map: SessionReplyMap,
}

#[derive(Clone, Copy)]
struct LiveHostIdentity {
    allocation: usize,
    k2a_resource_id: u32,
    reply_resource_id: u32,
    transport_instance: u64,
    context_id: u32,
}

impl LiveHost {
    fn identity(&self) -> LiveHostIdentity {
        LiveHostIdentity {
            allocation: self.allocation,
            k2a_resource_id: self.k2a_resource_id,
            reply_resource_id: self.reply_resource_id,
            transport_instance: self.transport_instance,
            context_id: self.context_id,
        }
    }
}

enum HostState {
    Provisional { allocation: usize },
    Initializing { allocation: usize },
    Live(LiveHost),
    Draining { allocation: usize },
    Dead { allocation: usize },
}

/// One fixed CPU mapping of K11's private HOST3D/MAPPABLE reply resource.
/// It is move-only and never crosses an ABI or becomes a discovery token.
struct SessionReplyMap {
    va: NonNull<u8>,
}

#[derive(Clone, Copy)]
enum ReplyCopyRefusal {
    Range,
    Opcode(u32),
}

#[derive(Clone, Copy)]
enum ReplyCopyPath {
    Async = 1,
    Sync = 2,
}

fn record_reply_copy_refusal(
    path: ReplyCopyPath,
    offset: u64,
    bytes: u64,
    expected_opcode: u32,
    refusal: ReplyCopyRefusal,
) {
    K11_HOST_REPLY_REJECT.fetch_add(1, Ordering::Relaxed);
    match path {
        ReplyCopyPath::Async => {
            K11_ASYNC_REPLY_REJECT.fetch_add(1, Ordering::Relaxed);
        }
        ReplyCopyPath::Sync => {
            K11_SYNC_REPLY_REJECT.fetch_add(1, Ordering::Relaxed);
        }
    }
    K11_REPLY_REJECT_KIND.store(path as u32, Ordering::Relaxed);
    K11_REPLY_EXPECTED_OPCODE.store(expected_opcode, Ordering::Relaxed);
    K11_REPLY_FOUND_OPCODE.store(
        match refusal {
            ReplyCopyRefusal::Range => u32::MAX - 1,
            ReplyCopyRefusal::Opcode(found) => found,
        },
        Ordering::Relaxed,
    );
    K11_REPLY_REJECT_OFFSET.store(u32::try_from(offset).unwrap_or(u32::MAX), Ordering::Relaxed);
    K11_REPLY_REJECT_BYTES.store(u32::try_from(bytes).unwrap_or(u32::MAX), Ordering::Relaxed);
}

// SAFETY: the mapping is accessed only while the enclosing session rundown is
// held. It is moved into the live host state and dropped once after rundown.
unsafe impl Send for SessionReplyMap {}

impl SessionReplyMap {
    fn new(prep: crate::virtio::gpu::BlobMapPrep) -> Option<Self> {
        if prep.size != SESSION_REPLY_BYTES || prep.gpa & 0xFFF != 0 {
            return None;
        }
        let caching = match prep.map_cache {
            VIRTIO_GPU_MAP_CACHE_CACHED => _MEMORY_CACHING_TYPE::MmCached,
            VIRTIO_GPU_MAP_CACHE_WC => _MEMORY_CACHING_TYPE::MmWriteCombined,
            VIRTIO_GPU_MAP_CACHE_UNCACHED => _MEMORY_CACHING_TYPE::MmNonCached,
            _ => return None,
        };
        let mut physical: PHYSICAL_ADDRESS = unsafe { core::mem::zeroed() };
        physical.QuadPart = prep.gpa as i64;
        let va = NonNull::new(
            unsafe { MmMapIoSpace(physical, SESSION_REPLY_BYTES, caching) }.cast::<u8>(),
        )?;
        Some(Self { va })
    }

    fn checked_range(offset: u64, bytes: u64) -> Option<core::ops::Range<usize>> {
        if bytes < core::mem::size_of::<u32>() as u64 {
            return None;
        }
        let end = offset.checked_add(bytes)?;
        if end > SESSION_REPLY_BYTES {
            return None;
        }
        Some(usize::try_from(offset).ok()?..usize::try_from(end).ok()?)
    }

    fn prepare_reply(&self, offset: u64, bytes: u64) -> bool {
        let Some(range) = Self::checked_range(offset, bytes) else {
            return false;
        };
        for index in range.clone() {
            unsafe { core::ptr::write_volatile(self.va.as_ptr().add(index), 0) };
        }
        let poison = HOST_REPLY_POISON.to_le_bytes();
        for (index, byte) in poison.iter().copied().enumerate() {
            unsafe { core::ptr::write_volatile(self.va.as_ptr().add(range.start + index), byte) };
        }
        core::sync::atomic::fence(Ordering::SeqCst);
        true
    }

    fn prepare_create_reply(&self) {
        let prepared =
            self.prepare_reply(SESSION_REPLY_OFFSET, pure::HOST_CREATE_INSTANCE_REPLY_BYTES);
        debug_assert!(prepared);
    }

    fn read_create_reply(&self) -> [u8; pure::HOST_CREATE_INSTANCE_REPLY_BYTES as usize] {
        core::sync::atomic::fence(Ordering::Acquire);
        let mut reply = [0u8; pure::HOST_CREATE_INSTANCE_REPLY_BYTES as usize];
        for (offset, byte) in reply.iter_mut().enumerate() {
            *byte = unsafe { core::ptr::read_volatile(self.va.as_ptr().add(offset)) };
        }
        reply
    }

    /// Copy one real host-written range from the context-local private pool to
    /// its exact public K2a slot. The caller proves the destination range from
    /// the retained canonical allocation; volatile reads preserve the mapped
    /// resource semantics after the host terminal.
    unsafe fn copy_reply_to(
        &self,
        offset: u64,
        bytes: u64,
        expected_opcode: u32,
        destination: NonNull<u8>,
    ) -> Result<(), ReplyCopyRefusal> {
        let Some(range) = Self::checked_range(offset, bytes) else {
            return Err(ReplyCopyRefusal::Range);
        };
        core::sync::atomic::fence(Ordering::Acquire);
        let mut opcode = [0u8; core::mem::size_of::<u32>()];
        for (index, byte) in opcode.iter_mut().enumerate() {
            *byte = unsafe { core::ptr::read_volatile(self.va.as_ptr().add(range.start + index)) };
        }
        let found_opcode = u32::from_le_bytes(opcode);
        if found_opcode != expected_opcode {
            return Err(ReplyCopyRefusal::Opcode(found_opcode));
        }
        for (destination_offset, source_offset) in range.enumerate() {
            let byte = unsafe { core::ptr::read_volatile(self.va.as_ptr().add(source_offset)) };
            unsafe { core::ptr::write(destination.as_ptr().add(destination_offset), byte) };
        }
        Ok(())
    }
}

impl Drop for SessionReplyMap {
    fn drop(&mut self) {
        unsafe { MmUnmapIoSpace(self.va.as_ptr().cast(), SESSION_REPLY_BYTES) };
    }
}

#[derive(Clone, Copy)]
struct RundownState {
    open: bool,
    active: u32,
}

/// The host's exact finite `vkCreateInstance` reply.  The context id and
/// renderer resource id deliberately do not leave this module.
pub(crate) struct HostInitEvidence {
    pub opcode: u32,
    pub status: i32,
}

/// Stable, fixed-size K11 state embedded in one heap-pinned `SessionObject`.
pub(crate) struct SessionTransport {
    state: SpinLock<HostState>,
    rundown: SpinLock<RundownState>,
    drained: UnsafeCell<KEVENT>,
    /// Keep the complete prepare/submit/reply interval single-owner. The
    /// transport-global fence is minted later under the virtio lock so it also
    /// orders against allocation teardown on this context's ring zero.
    ring_zero_control: SpinLock<pure::RingZeroControlState>,
    ring_zero_available: UnsafeCell<KEVENT>,
    /// Notification event for the finite attachment ledger.  A duplicate open
    /// that finds the same resource in `Attaching` waits on this event and then
    /// re-checks the ledger under its lock; no caller guesses whether the first
    /// host attach succeeded.
    attachments_changed: UnsafeCell<KEVENT>,
    /// Exact resources secondarily attached to this session's one Venus
    /// context.  The vector is fully reserved before publication, never grows
    /// past the HNR2 use bound, and is drained in reverse attachment order.
    attachments: SpinLock<alloc::vec::Vec<SessionAttachment>>,
}

// SAFETY: mutable state is reachable only through spinlocks.  Both events are
// initialized once after the enclosing SessionObject reaches its final heap
// address and thereafter used only through kernel dispatcher APIs.
unsafe impl Send for SessionTransport {}
unsafe impl Sync for SessionTransport {}

struct RingZeroControlOperation {
    owner: NonNull<SessionTransport>,
}

unsafe impl Send for RingZeroControlOperation {}

impl Drop for RingZeroControlOperation {
    fn drop(&mut self) {
        let owner = unsafe { self.owner.as_ref() };
        let mut state = owner.ring_zero_control.lock();
        let released = state.release();
        debug_assert!(released);
        if released {
            // Publish availability while holding the state lock, so a new
            // owner cannot clear the event before this release signals it.
            unsafe { KeSetEvent(owner.ring_zero_available.get(), 0, 0) };
        }
    }
}

impl SessionTransport {
    pub(crate) fn new() -> Option<Self> {
        let mut attachments = alloc::vec::Vec::new();
        attachments
            .try_reserve_exact(MAX_SESSION_ATTACHMENTS)
            .ok()?;
        Some(Self {
            state: SpinLock::new(HostState::Provisional { allocation: 0 }),
            rundown: SpinLock::new(RundownState {
                open: true,
                active: 0,
            }),
            // Initialized in place by `init_event` before publication.
            drained: UnsafeCell::new(unsafe { core::mem::zeroed() }),
            ring_zero_control: SpinLock::new(pure::RingZeroControlState::new()),
            ring_zero_available: UnsafeCell::new(unsafe { core::mem::zeroed() }),
            attachments_changed: UnsafeCell::new(unsafe { core::mem::zeroed() }),
            attachments: SpinLock::new(attachments),
        })
    }

    /// Initialize the embedded notification event after heap pinning.
    ///
    /// # Safety
    /// `self` must be at its final address and not yet published.
    pub(crate) unsafe fn init_event(&self) {
        // NotificationEvent, initially signaled because active == 0.
        unsafe { KeInitializeEvent(self.drained.get(), 0, 1) };
        // NotificationEvent, initially signaled because no synchronous
        // ring-zero control operation owns the session yet.
        unsafe { KeInitializeEvent(self.ring_zero_available.get(), 0, 1) };
        // NotificationEvent.  There is no transition to observe before the
        // first ledger entry is published, so the initial signaled state lets a
        // spurious waiter simply re-check.
        unsafe { KeInitializeEvent(self.attachments_changed.get(), 0, 1) };
    }

    /// Bind the exact role-1 canonical allocation once.  The caller retains a
    /// strong session ref in that allocation's device-specific open object.
    pub(crate) fn bind_reply_pool(&self, allocation: usize) -> bool {
        if allocation == 0 {
            return false;
        }
        let mut state = self.state.lock();
        if matches!(&*state, HostState::Provisional { allocation: 0 }) {
            *state = HostState::Provisional { allocation };
            true
        } else {
            false
        }
    }

    pub(crate) fn binding_matches(&self, allocation: usize) -> bool {
        match &*self.state.lock() {
            HostState::Provisional { allocation: bound }
            | HostState::Initializing { allocation: bound }
            | HostState::Live(LiveHost {
                allocation: bound, ..
            })
            | HostState::Draining { allocation: bound }
            | HostState::Dead { allocation: bound } => *bound != 0 && *bound == allocation,
        }
    }

    fn acquire(&self) -> Option<SessionOperation> {
        let mut rundown = self.rundown.lock();
        if !rundown.open || rundown.active == u32::MAX {
            return None;
        }
        if rundown.active == 0 {
            // The state lock makes clear-before-visible indivisible with the
            // first increment, so teardown can never observe active=1 while the
            // event is still signaled.
            unsafe { KeClearEvent(self.drained.get()) };
        }
        rundown.active += 1;
        Some(SessionOperation {
            owner: NonNull::from(self),
        })
    }

    /// Serialize the complete synchronous ring-zero interval. Every caller
    /// already owns session rundown, and every underlying control roundtrip is
    /// bounded, so teardown cannot free this event or strand a waiter. The
    /// exact fence is minted from the shared wire namespace during enqueue.
    /// This is an exact dispatcher-object wait, never polling or synthetic
    /// completion.
    fn acquire_ring_zero_control(
        &self,
        _passive: PassiveLevel,
    ) -> Result<RingZeroControlOperation, NTSTATUS> {
        loop {
            let mut state = self.ring_zero_control.lock();
            match state.try_acquire() {
                Ok(()) => {
                    unsafe { KeClearEvent(self.ring_zero_available.get()) };
                    return Ok(RingZeroControlOperation {
                        owner: NonNull::from(self),
                    });
                }
                Err(pure::RingZeroControlRefusal::Occupied) => {}
            }
            // Clear under the same lock used by Drop's set, preventing a lost
            // wake between observing the occupied state and beginning to wait.
            unsafe { KeClearEvent(self.ring_zero_available.get()) };
            drop(state);
            let _ = unsafe {
                KeWaitForSingleObject(
                    self.ring_zero_available.get() as PVOID,
                    0,
                    0,
                    0,
                    core::ptr::null_mut(),
                )
            };
        }
    }

    /// Acquire the session's exact live host namespace for an asynchronous
    /// executor submission.  The returned operation owns rundown until the
    /// terminal host result, and carries only the already-bound context id.
    pub(crate) fn acquire_execution(
        &self,
        adapter: &AdapterContext,
        owner: DeviceOwner,
    ) -> Option<SessionExecutionOperation> {
        let operation = self.acquire()?;
        let host = match &*self.state.lock() {
            HostState::Live(host) => host.identity(),
            _ => return None,
        };
        if Self::current_transport(adapter) != Some(host.transport_instance) {
            return None;
        }
        let pair = crate::virtio::ctrl::borrow_venus_session_pair(
            adapter,
            owner,
            host.context_id,
            host.reply_resource_id,
        )
        .ok()?;
        let facts = unsafe { k11_reply_pool_facts(host.allocation) }?;
        if facts.resource_id != host.k2a_resource_id
            || facts.transport_instance != host.transport_instance
        {
            return None;
        }
        drop(pair);
        Some(SessionExecutionOperation {
            _operation: operation,
            context_id: host.context_id,
            transport_instance: host.transport_instance,
        })
    }

    /// Attach one exact allocation resource to this session namespace.  A
    /// duplicate direct open increments a session-local reference rather than
    /// issuing a second host attach; no adapter-global reverse lookup exists.
    pub(crate) fn attach_execution_resource(
        &self,
        passive: PassiveLevel,
        adapter: &AdapterContext,
        owner: DeviceOwner,
        resource_id: u32,
        transport_instance: u64,
    ) -> Result<u32, NTSTATUS> {
        if resource_id == 0 {
            return Err(STATUS_INVALID_DEVICE_REQUEST);
        }
        let execution = self
            .acquire_execution(adapter, owner)
            .ok_or(STATUS_DEVICE_NOT_READY)?;
        if execution.transport_instance != transport_instance {
            return Err(STATUS_DEVICE_NOT_READY);
        }
        loop {
            let mut attachments = self.attachments.lock();
            if let Some(entry) = attachments
                .iter_mut()
                .find(|entry| entry.resource_id == resource_id)
            {
                match entry.state {
                    AttachmentState::Attached if entry.references != u32::MAX => {
                        entry.references += 1;
                        return Ok(execution.context_id);
                    }
                    AttachmentState::Attaching => {
                        // Clear while holding the ledger lock.  The publisher
                        // takes the same lock before signaling, so completion
                        // cannot be lost between this observation and the wait.
                        unsafe { KeClearEvent(self.attachments_changed.get()) };
                        drop(attachments);
                        let _ = unsafe {
                            KeWaitForSingleObject(
                                self.attachments_changed.get() as PVOID,
                                0,
                                0,
                                0,
                                core::ptr::null_mut(),
                            )
                        };
                        continue;
                    }
                    // A failed host detach is deliberately retained for final
                    // reverse teardown.  Re-attaching through that ambiguous
                    // state would create two host references with one ledger
                    // entry, so it remains a named refusal.
                    AttachmentState::Detaching | AttachmentState::Attached => {
                        return Err(STATUS_DEVICE_NOT_READY);
                    }
                }
            }
            if attachments.len() == attachments.capacity()
                || attachments.len() >= MAX_SESSION_ATTACHMENTS
            {
                return Err(crate::dxgk::STATUS_NO_MEMORY);
            }
            attachments.push(SessionAttachment {
                resource_id,
                references: 1,
                state: AttachmentState::Attaching,
            });
            break;
        }
        if crate::virtio::ctrl::ctx_attach_resource(
            passive,
            adapter,
            execution.context_id,
            resource_id,
        )
        .is_err()
        {
            self.attachments
                .lock()
                .retain(|entry| entry.resource_id != resource_id);
            unsafe { KeSetEvent(self.attachments_changed.get(), 0, 0) };
            return Err(STATUS_DEVICE_NOT_READY);
        }
        let mut attachments = self.attachments.lock();
        let Some(entry) = attachments
            .iter_mut()
            .find(|entry| entry.resource_id == resource_id)
        else {
            return Err(STATUS_DEVICE_NOT_READY);
        };
        entry.state = AttachmentState::Attached;
        drop(attachments);
        unsafe { KeSetEvent(self.attachments_changed.get(), 0, 0) };
        Ok(execution.context_id)
    }

    pub(crate) fn detach_execution_resource(
        &self,
        passive: PassiveLevel,
        adapter: &AdapterContext,
        resource_id: u32,
    ) {
        let context_id = match &*self.state.lock() {
            HostState::Live(host) => host.context_id,
            _ => return,
        };
        {
            let mut attachments = self.attachments.lock();
            let Some(entry) = attachments
                .iter_mut()
                .find(|entry| entry.resource_id == resource_id)
            else {
                return;
            };
            if entry.state != AttachmentState::Attached {
                return;
            }
            if entry.references > 1 {
                entry.references -= 1;
                return;
            }
            entry.state = AttachmentState::Detaching;
        }
        if crate::virtio::ctrl::ctx_detach_session_resource(
            passive,
            adapter,
            context_id,
            resource_id,
        )
        .is_ok()
        {
            self.attachments
                .lock()
                .retain(|entry| entry.resource_id != resource_id);
        }
    }

    /// Run `f` on the live reply map WHILE `state` is locked, so a concurrent
    /// Live->Dead replacement (same lock) cannot unmap the window mid-access.
    /// Two identical 0x50 dumps (2026-08-24, explorer, copy_reply_to+f5) prove
    /// the rundown alone does NOT order the unmap against a caller that
    /// captured the map pointer before a host round-trip. Bounded volatile
    /// copies only inside `f` -- the lock raises to DISPATCH.
    fn with_live_reply_map<R>(
        &self,
        expected_transport: u64,
        f: impl FnOnce(&SessionReplyMap) -> R,
    ) -> Option<R> {
        let state = self.state.lock();
        match &*state {
            HostState::Live(host) if host.transport_instance == expected_transport => {
                Some(f(&host.reply_map))
            }
            _ => {
                K11_REPLY_MAP_GONE.fetch_add(1, Ordering::Relaxed);
                None
            }
        }
    }

    fn close_and_wait(&self, _passive: PassiveLevel) {
        let active = {
            let mut rundown = self.rundown.lock();
            rundown.open = false;
            rundown.active
        };
        if active == 0 {
            return;
        }
        K11_RUNDOWN_WAITED.fetch_add(1, Ordering::Relaxed);
        // Exact event wait, never polling or a time slice.  Every admitted host
        // operation owns a Drop guard and the underlying control roundtrip is
        // itself bounded.
        let _ = unsafe {
            KeWaitForSingleObject(self.drained.get() as PVOID, 0, 0, 0, core::ptr::null_mut())
        };
    }

    fn current_transport(adapter: &AdapterContext) -> Option<u64> {
        adapter
            .with_virtio(|gpu| gpu.scanout_transport_instance())
            .ok()
            .filter(|instance| *instance != 0)
    }

    pub(crate) fn with_live_on_current_transport<R>(
        &self,
        adapter: &AdapterContext,
        owner: DeviceOwner,
        operation: impl FnOnce() -> R,
    ) -> Option<R> {
        let Some(_operation) = self.acquire() else {
            return None;
        };
        let host = match &*self.state.lock() {
            HostState::Live(host) => host.identity(),
            _ => return None,
        };
        if Self::current_transport(adapter) != Some(host.transport_instance) {
            return None;
        }
        let Ok(_pair) = crate::virtio::ctrl::borrow_venus_session_pair(
            adapter,
            owner,
            host.context_id,
            host.reply_resource_id,
        ) else {
            return None;
        };
        // Reset invalidates the canonical allocation generation before owner
        // admission is closed. Re-projecting this exact retained allocation
        // therefore closes the small create-context race too: no old
        // capability can attach merely because the retiring transport object
        // is still present for a few more instructions.
        let Some(facts) = (unsafe { k11_reply_pool_facts(host.allocation) }) else {
            return None;
        };
        if facts.resource_id != host.k2a_resource_id
            || facts.transport_instance != host.transport_instance
        {
            return None;
        }
        // Both rundown guards remain live through the model mutation. A reset
        // that already closed owner admission refuses before this point; one
        // that begins afterwards is ordered after this exact attach and drains
        // it as part of the old session.
        Some(operation())
    }

    /// Prepare one asynchronous generated reply in the exact private resource
    /// created with this session context. The caller's retained session and
    /// allocation custody spans the later host terminal; this short operation
    /// only patches the KMD-owned payload and poisons its unique slot range.
    pub(crate) fn prepare_generated_reply(
        &self,
        adapter: &AdapterContext,
        owner: DeviceOwner,
        facts: K11ReplyPoolFacts,
        payload: &mut [u8],
        resource_operand_offset: u32,
        raw_reply_offset: u64,
        raw_reply_bytes: u64,
    ) -> Result<(), NTSTATUS> {
        let operation = self
            .acquire_execution(adapter, owner)
            .ok_or(STATUS_DEVICE_NOT_READY)?;
        let host = match &*self.state.lock() {
            HostState::Live(host) => host.identity(),
            _ => return Err(STATUS_DEVICE_NOT_READY),
        };
        if operation.context_id != host.context_id
            || operation.transport_instance != host.transport_instance
            || facts.transport_instance != host.transport_instance
            || facts.resource_id != host.k2a_resource_id
            || facts.resource_id == 0
        {
            return Err(STATUS_INVALID_DEVICE_REQUEST);
        }
        let end = raw_reply_offset
            .checked_add(raw_reply_bytes)
            .ok_or(STATUS_INVALID_DEVICE_REQUEST)?;
        if end > facts.byte_size {
            return Err(STATUS_INVALID_DEVICE_REQUEST);
        }
        let prepared = self
            .with_live_reply_map(host.transport_instance, |map| {
                map.prepare_reply(raw_reply_offset, raw_reply_bytes)
            })
            .ok_or(STATUS_DEVICE_NOT_READY)?;
        if !prepared {
            return Err(STATUS_INVALID_DEVICE_REQUEST);
        }
        pure::patch_private_reply_resource(payload, resource_operand_offset, host.reply_resource_id)
            .map_err(|_| STATUS_INVALID_DEVICE_REQUEST)
    }

    /// Copy a terminal asynchronous reply into its canonical K2a/HVR1 slot.
    /// The renderer-private id and mapping stay inside K11; callers retain only
    /// their direct SessionObject and canonical allocation facts.
    pub(crate) fn complete_generated_reply(
        &self,
        adapter: &AdapterContext,
        owner: DeviceOwner,
        facts: K11ReplyPoolFacts,
        raw_reply_offset: u64,
        raw_reply_bytes: u64,
        expected_opcode: u32,
    ) -> Result<(), NTSTATUS> {
        let _operation = self.acquire().ok_or(STATUS_INVALID_DEVICE_REQUEST)?;
        let host = match &*self.state.lock() {
            HostState::Live(host) => host.identity(),
            _ => return Err(STATUS_DEVICE_NOT_READY),
        };
        if Self::current_transport(adapter) != Some(host.transport_instance)
            || facts.transport_instance != host.transport_instance
            || facts.resource_id != host.k2a_resource_id
            || facts.resource_id == 0
        {
            return Err(STATUS_INVALID_DEVICE_REQUEST);
        }
        let _pair = crate::virtio::ctrl::borrow_venus_session_pair(
            adapter,
            owner,
            host.context_id,
            host.reply_resource_id,
        )
        .map_err(|_| STATUS_DEVICE_NOT_READY)?;
        let end = raw_reply_offset
            .checked_add(raw_reply_bytes)
            .ok_or(STATUS_INVALID_DEVICE_REQUEST)?;
        if end > facts.byte_size {
            return Err(STATUS_INVALID_DEVICE_REQUEST);
        }
        let destination_offset =
            usize::try_from(raw_reply_offset).map_err(|_| STATUS_INVALID_DEVICE_REQUEST)?;
        let destination = NonNull::new(unsafe { facts.kernel_va.as_ptr().add(destination_offset) })
            .ok_or(STATUS_INVALID_DEVICE_REQUEST)?;
        // SAFETY: the map is accessed only under the state lock (no unmap can
        // interleave); the caller's OpenExecutionUse retains the K2a
        // allocation and the range was checked above against its byte size.
        if let Err(refusal) = self
            .with_live_reply_map(host.transport_instance, |map| unsafe {
                map.copy_reply_to(
                    raw_reply_offset,
                    raw_reply_bytes,
                    expected_opcode,
                    destination,
                )
            })
            .ok_or(STATUS_DEVICE_NOT_READY)?
        {
            record_reply_copy_refusal(
                ReplyCopyPath::Async,
                raw_reply_offset,
                raw_reply_bytes,
                expected_opcode,
                refusal,
            );
            return Err(STATUS_DEVICE_NOT_READY);
        }
        Ok(())
    }

    /// Execute one generated HVC1 payload on the already-owned stock Venus
    /// context. The host writes its context-local private target; only after
    /// the synchronous terminal does K11 copy validated bytes into K2a.
    pub(crate) fn execute_generated_control(
        &self,
        passive: PassiveLevel,
        adapter: &AdapterContext,
        owner: DeviceOwner,
        facts: K11ReplyPoolFacts,
        payload: &mut [u8],
        resource_operand_offset: u32,
        raw_reply_offset: u64,
        raw_reply_bytes: u64,
        expected_opcode: u32,
    ) -> Result<(), NTSTATUS> {
        let operation = self
            .acquire_execution(adapter, owner)
            .ok_or(STATUS_DEVICE_NOT_READY)?;
        let host = match &*self.state.lock() {
            HostState::Live(host) => host.identity(),
            _ => return Err(STATUS_DEVICE_NOT_READY),
        };
        if operation.context_id != host.context_id
            || operation.transport_instance != host.transport_instance
            || facts.transport_instance != host.transport_instance
            || facts.resource_id != host.k2a_resource_id
            || facts.resource_id == 0
        {
            return Err(STATUS_INVALID_DEVICE_REQUEST);
        }
        let end = raw_reply_offset
            .checked_add(raw_reply_bytes)
            .ok_or(STATUS_INVALID_DEVICE_REQUEST)?;
        if end > facts.byte_size {
            return Err(STATUS_INVALID_DEVICE_REQUEST);
        }
        let _ring_zero = self.acquire_ring_zero_control(passive)?;
        let prepared = self
            .with_live_reply_map(host.transport_instance, |map| {
                map.prepare_reply(raw_reply_offset, raw_reply_bytes)
            })
            .ok_or(STATUS_DEVICE_NOT_READY)?;
        if !prepared {
            return Err(STATUS_INVALID_DEVICE_REQUEST);
        }
        pure::patch_private_reply_resource(
            payload,
            resource_operand_offset,
            host.reply_resource_id,
        )
        .map_err(|_| STATUS_INVALID_DEVICE_REQUEST)?;
        let pair = crate::virtio::ctrl::borrow_venus_session_pair(
            adapter,
            owner,
            host.context_id,
            host.reply_resource_id,
        )
        .map_err(|_| STATUS_DEVICE_NOT_READY)?;
        crate::virtio::ctrl::submit_venus_session_sync(passive, adapter, &pair, payload)
            .map_err(|_| STATUS_DEVICE_NOT_READY)?;
        let destination_offset =
            usize::try_from(raw_reply_offset).map_err(|_| STATUS_INVALID_DEVICE_REQUEST)?;
        let destination = NonNull::new(unsafe { facts.kernel_va.as_ptr().add(destination_offset) })
            .ok_or(STATUS_INVALID_DEVICE_REQUEST)?;
        // SAFETY: the map is accessed only under the state lock -- the sync
        // submit above is a host round-trip, and this exact shape (capture the
        // map, wait, then dereference) is the 2026-08-24 0x50 pair. `pair`
        // retains the host side; the K2a destination range was checked above.
        if let Err(refusal) = self
            .with_live_reply_map(host.transport_instance, |map| unsafe {
                map.copy_reply_to(
                    raw_reply_offset,
                    raw_reply_bytes,
                    expected_opcode,
                    destination,
                )
            })
            .ok_or(STATUS_DEVICE_NOT_READY)?
        {
            record_reply_copy_refusal(
                ReplyCopyPath::Sync,
                raw_reply_offset,
                raw_reply_bytes,
                expected_opcode,
                refusal,
            );
            return Err(STATUS_DEVICE_NOT_READY);
        }
        Ok(())
    }

    pub(crate) fn execute_control_no_reply(
        &self,
        passive: PassiveLevel,
        adapter: &AdapterContext,
        owner: DeviceOwner,
        payload: &[u8],
    ) -> Result<(), NTSTATUS> {
        let _operation = self.acquire().ok_or(STATUS_INVALID_DEVICE_REQUEST)?;
        let host = match &*self.state.lock() {
            HostState::Live(host) => host.identity(),
            _ => return Err(STATUS_DEVICE_NOT_READY),
        };
        if Self::current_transport(adapter) != Some(host.transport_instance) {
            return Err(STATUS_DEVICE_NOT_READY);
        }
        let _ring_zero = self.acquire_ring_zero_control(passive)?;
        let pair = crate::virtio::ctrl::borrow_venus_session_pair(
            adapter,
            owner,
            host.context_id,
            host.reply_resource_id,
        )
        .map_err(|_| STATUS_DEVICE_NOT_READY)?;
        crate::virtio::ctrl::submit_venus_session_sync(passive, adapter, &pair, payload)
            .map_err(|_| STATUS_DEVICE_NOT_READY)
    }

    /// Create one distinct stock Venus context and one `VkInstance`, validate
    /// the real host reply, and let `publish` atomically finish the HTS1/HVR1
    /// side before the session becomes externally live.
    pub(crate) fn initialize<R>(
        &self,
        passive: PassiveLevel,
        adapter: &AdapterContext,
        owner: DeviceOwner,
        reply_offset: u64,
        reply_capacity: u64,
        expected_allocation_generation: u64,
        final_payload_bytes: u64,
        publish: impl FnOnce(K11ReplyPoolFacts, &HostInitEvidence) -> Result<R, NTSTATUS>,
    ) -> Result<R, NTSTATUS> {
        let _operation = self.acquire().ok_or(STATUS_INVALID_DEVICE_REQUEST)?;
        let allocation = {
            let mut state = self.state.lock();
            let HostState::Provisional { allocation } = &*state else {
                K11_HOST_INIT_REJECT.fetch_add(1, Ordering::Relaxed);
                return Err(STATUS_INVALID_DEVICE_REQUEST);
            };
            let allocation = *allocation;
            if allocation == 0 {
                K11_HOST_INIT_REJECT.fetch_add(1, Ordering::Relaxed);
                return Err(STATUS_DEVICE_NOT_READY);
            }
            *state = HostState::Initializing { allocation };
            allocation
        };

        // SAFETY: the retained ordinary open owns the canonical allocation for
        // this entire operation; CloseAllocation first closes this rundown.
        let Some(facts) = (unsafe { k11_reply_pool_facts(allocation) }) else {
            self.fail_initialization(allocation);
            return Err(STATUS_DEVICE_NOT_READY);
        };
        if facts.allocation_generation != expected_allocation_generation {
            self.fail_initialization(allocation);
            return Err(STATUS_DEVICE_NOT_READY);
        }
        if Self::current_transport(adapter) != Some(facts.transport_instance) {
            K11_STALE_TRANSPORT.fetch_add(1, Ordering::Relaxed);
            self.fail_initialization(allocation);
            return Err(STATUS_DEVICE_NOT_READY);
        }
        // K2a holds only the final HVR1 record. Its payload bound remains exact,
        // while the renderer's raw reply is confined to the private SHM below.
        let payload_offset = match reply_offset.checked_add(HELIOS_HVR1_HEADER_SIZE as u64) {
            Some(offset) => offset,
            None => {
                self.fail_initialization(allocation);
                return Err(STATUS_INVALID_DEVICE_REQUEST);
            }
        };
        let range = match pure::admit_reply_range(
            facts.byte_size,
            helios_protocol::native_render::HELIOS_HVM1_REPLY_SLOT_BYTES,
            reply_offset,
            reply_capacity,
            HELIOS_HVR1_HEADER_SIZE as u64,
            final_payload_bytes,
        ) {
            Ok(range) => range,
            Err(_) => {
                self.fail_initialization(allocation);
                return Err(STATUS_INVALID_DEVICE_REQUEST);
            }
        };
        if range.payload_offset != payload_offset {
            self.fail_initialization(allocation);
            return Err(STATUS_INVALID_DEVICE_REQUEST);
        }

        let context_id =
            match crate::virtio::ctrl::ctx_create_session(passive, adapter, VENUS_CAPSET_ID, owner)
            {
                Ok(id) => id,
                Err(_) => {
                    self.fail_initialization(allocation);
                    return Err(STATUS_DEVICE_NOT_READY);
                }
            };
        K11_CONTEXT_CREATED.fetch_add(1, Ordering::Relaxed);

        let reply_resource_id = match crate::virtio::ctrl::resource_create_session_reply_blob(
            passive,
            adapter,
            context_id,
            owner,
            SESSION_REPLY_BYTES,
        ) {
            Ok(resource_id) => resource_id,
            Err(_) => {
                self.cleanup_empty_context(passive, adapter, owner, context_id);
                self.fail_initialization(allocation);
                return Err(STATUS_DEVICE_NOT_READY);
            }
        };

        let prep = match crate::virtio::ctrl::map_session_reply_blob(
            passive,
            adapter,
            owner,
            context_id,
            reply_resource_id,
        ) {
            Ok(prep) => prep,
            Err(_) => {
                self.cleanup_session_resource(
                    passive,
                    adapter,
                    owner,
                    context_id,
                    reply_resource_id,
                    false,
                    None,
                );
                self.fail_initialization(allocation);
                return Err(STATUS_DEVICE_NOT_READY);
            }
        };
        let reply_map = match SessionReplyMap::new(prep) {
            Some(map) => map,
            None => {
                self.cleanup_session_resource(
                    passive,
                    adapter,
                    owner,
                    context_id,
                    reply_resource_id,
                    true,
                    None,
                );
                self.fail_initialization(allocation);
                return Err(STATUS_DEVICE_NOT_READY);
            }
        };

        // The canonical pair lease covers both direct submissions and the raw
        // private-SHM read used to establish the instance. K2a is never a host
        // reply target; it receives only validated bytes copied after a real
        // context-local terminal.
        let pair = match crate::virtio::ctrl::borrow_venus_session_pair(
            adapter,
            owner,
            context_id,
            reply_resource_id,
        ) {
            Ok(pair) => pair,
            Err(_) => {
                self.cleanup_session_resource(
                    passive,
                    adapter,
                    owner,
                    context_id,
                    reply_resource_id,
                    true,
                    Some(reply_map),
                );
                self.fail_initialization(allocation);
                return Err(STATUS_DEVICE_NOT_READY);
            }
        };

        let evidence =
            match self.create_instance(passive, adapter, &pair, reply_resource_id, &reply_map) {
                Ok(evidence) => evidence,
                Err(status) => {
                    drop(pair);
                    // INIT was never admitted, so do not issue vkDestroyInstance
                    // against an unproven handle. Destroying the private Venus
                    // context is the exact fail-closed cleanup whether CREATE was
                    // rejected, completed with a malformed reply, or ambiguous.
                    self.cleanup_session_resource(
                        passive,
                        adapter,
                        owner,
                        context_id,
                        reply_resource_id,
                        true,
                        Some(reply_map),
                    );
                    self.fail_initialization(allocation);
                    return Err(status);
                }
            };

        let live = LiveHost {
            allocation,
            k2a_resource_id: facts.resource_id,
            reply_resource_id,
            transport_instance: facts.transport_instance,
            context_id,
            instance_handle: SESSION_INSTANCE_HANDLE,
            reply_map,
        };
        // Keep the transport non-live while the callback makes the model Live
        // and release-publishes HVR1.  HQA1 admission requires both states, so
        // it cannot enter the narrow model-publication/HVR1-publication window.
        let result = match publish(facts, &evidence) {
            Ok(result) => result,
            Err(status) => {
                drop(pair);
                self.cleanup_live_host(passive, adapter, owner, live);
                self.fail_initialization(allocation);
                return Err(status);
            }
        };
        // Host creation, exact raw reply validation, model publication, and
        // HVR1 publication are all terminal. Only now may outer contexts see a
        // live host namespace.
        *self.state.lock() = HostState::Live(live);
        drop(pair);
        K11_HOST_INIT_OK.fetch_add(1, Ordering::Relaxed);
        Ok(result)
    }

    fn create_instance(
        &self,
        passive: PassiveLevel,
        adapter: &AdapterContext,
        pair: &crate::virtio::ctrl::VenusSessionGuard<'_>,
        reply_resource_id: u32,
        reply_map: &SessionReplyMap,
    ) -> Result<HostInitEvidence, NTSTATUS> {
        reply_map.prepare_create_reply();

        let target = pure::encode_set_reply_command_stream(
            reply_resource_id,
            SESSION_REPLY_OFFSET,
            pure::HOST_CREATE_INSTANCE_REPLY_BYTES,
        );
        let bytes = target.finished().ok_or(STATUS_DEVICE_NOT_READY)?;
        if crate::virtio::ctrl::submit_venus_session_sync(passive, adapter, pair, bytes).is_err() {
            return Err(STATUS_DEVICE_NOT_READY);
        }

        let create = pure::encode_create_instance(SESSION_INSTANCE_HANDLE);
        let bytes = create.finished().ok_or(STATUS_DEVICE_NOT_READY)?;
        if crate::virtio::ctrl::submit_venus_session_sync(passive, adapter, pair, bytes).is_err() {
            return Err(STATUS_DEVICE_NOT_READY);
        }

        // The second finite used-ring response carries the stock context-local
        // ring-zero fence, so it is terminal for CREATE decode and the reply
        // write. Read only the fixed private SHM bytes; K2a is written later,
        // after this evidence is fully validated.
        let raw_reply = reply_map.read_create_reply();
        let evidence =
            match pure::validate_create_instance_reply(&raw_reply, SESSION_INSTANCE_HANDLE) {
                Ok(evidence) => evidence,
                Err(_) => {
                    K11_HOST_REPLY_REJECT.fetch_add(1, Ordering::Relaxed);
                    return Err(STATUS_DEVICE_NOT_READY);
                }
            };
        if evidence.opcode != pure::CMD_CREATE_INSTANCE || evidence.status != 0 {
            K11_HOST_REPLY_REJECT.fetch_add(1, Ordering::Relaxed);
            return Err(STATUS_DEVICE_NOT_READY);
        }
        Ok(HostInitEvidence {
            opcode: evidence.opcode,
            status: evidence.status,
        })
    }

    fn fail_initialization(&self, allocation: usize) {
        K11_HOST_INIT_REJECT.fetch_add(1, Ordering::Relaxed);
        *self.state.lock() = HostState::Draining { allocation };
        self.rundown.lock().open = false;
    }

    fn cleanup_live_host(
        &self,
        passive: PassiveLevel,
        adapter: &AdapterContext,
        owner: DeviceOwner,
        host: LiveHost,
    ) {
        let LiveHost {
            reply_resource_id,
            transport_instance,
            context_id,
            instance_handle,
            reply_map,
            ..
        } = host;
        if Self::current_transport(adapter) != Some(transport_instance) {
            K11_STALE_TRANSPORT.fetch_add(1, Ordering::Relaxed);
            drop(reply_map);
            return;
        }
        // Allocation-backed resources were attached after the reply target and
        // are revoked first, in reverse attachment order. Rundown is already
        // closed, so no executor can acquire or retain one while this drains.
        loop {
            let resource_id = self.attachments.lock().pop().map(|entry| entry.resource_id);
            let Some(resource_id) = resource_id else {
                break;
            };
            if crate::virtio::ctrl::ctx_detach_session_resource(
                passive,
                adapter,
                context_id,
                resource_id,
            )
            .is_err()
            {
                self.quarantine_failed_cleanup(adapter, owner, context_id);
                drop(reply_map);
                return;
            }
        }
        if let Ok(pair) = crate::virtio::ctrl::borrow_venus_session_pair(
            adapter,
            owner,
            context_id,
            reply_resource_id,
        ) {
            let destroy = pure::encode_destroy_instance(instance_handle);
            if let Some(bytes) = destroy.finished() {
                let _ = crate::virtio::ctrl::submit_venus_session_destroy(
                    passive, adapter, &pair, bytes,
                );
            }
            drop(pair);
        }
        self.cleanup_session_resource(
            passive,
            adapter,
            owner,
            context_id,
            reply_resource_id,
            true,
            Some(reply_map),
        );
    }

    fn cleanup_session_resource(
        &self,
        passive: PassiveLevel,
        adapter: &AdapterContext,
        owner: DeviceOwner,
        context_id: u32,
        resource_id: u32,
        host_mapped: bool,
        reply_map: Option<SessionReplyMap>,
    ) {
        // A terminal UNMAP is the FIFO drain behind any earlier direct submit.
        // If it cannot be proved, stop: owner-table quarantine/reset retains the
        // host objects and no later step releases their backing out of order.
        let unmap_terminal = !host_mapped
            || crate::virtio::ctrl::unmap_session_reply_blob(
                passive,
                adapter,
                owner,
                context_id,
                resource_id,
            )
            .is_ok();
        drop(reply_map);
        if !unmap_terminal {
            self.quarantine_failed_cleanup(adapter, owner, context_id);
            return;
        }
        if crate::virtio::ctrl::ctx_detach_session_resource(
            passive,
            adapter,
            context_id,
            resource_id,
        )
        .is_err()
        {
            self.quarantine_failed_cleanup(adapter, owner, context_id);
            return;
        }
        if crate::virtio::ctrl::resource_unref_session_reply(
            passive,
            adapter,
            owner,
            context_id,
            resource_id,
        )
        .is_err()
        {
            self.quarantine_failed_cleanup(adapter, owner, context_id);
            return;
        }
        self.cleanup_empty_context(passive, adapter, owner, context_id);
    }

    fn cleanup_empty_context(
        &self,
        passive: PassiveLevel,
        adapter: &AdapterContext,
        owner: DeviceOwner,
        context_id: u32,
    ) {
        if crate::virtio::ctrl::ctx_destroy_session(passive, adapter, owner, context_id).is_ok() {
            K11_CONTEXT_DESTROYED.fetch_add(1, Ordering::Relaxed);
        } else {
            self.quarantine_failed_cleanup(adapter, owner, context_id);
        }
    }

    fn quarantine_failed_cleanup(
        &self,
        adapter: &AdapterContext,
        owner: DeviceOwner,
        context_id: u32,
    ) {
        K11_CLEANUP_REJECT.fetch_add(1, Ordering::Relaxed);
        let _ = adapter
            .control_owner()
            .quarantine_session_context(owner, context_id);
    }

    /// Revoke, drain, and destroy this session's host namespace.  Idempotent.
    pub(crate) fn teardown(
        &self,
        passive: PassiveLevel,
        adapter: &AdapterContext,
        owner: DeviceOwner,
    ) {
        self.close_and_wait(passive);
        let old = {
            let mut state = self.state.lock();
            let allocation = match &*state {
                HostState::Provisional { allocation }
                | HostState::Initializing { allocation }
                | HostState::Live(LiveHost { allocation, .. })
                | HostState::Draining { allocation }
                | HostState::Dead { allocation } => *allocation,
            };
            core::mem::replace(&mut *state, HostState::Dead { allocation })
        };
        if let HostState::Live(host) = old {
            self.cleanup_live_host(passive, adapter, owner, host);
        }
    }

    /// K11 publishes only within the exact checked-out role-1 slot.  Payload is
    /// copied first, then the header with magic zero, and magic is the release
    /// publication word; Mesa reads it only after the ordinary C51 wait.
    pub(crate) fn publish_hvr1(
        facts: K11ReplyPoolFacts,
        reply_offset: u64,
        reply_capacity: u64,
        header: &helios_protocol::native_render::HeliosVenusReplyV1,
        payload: &[u8],
    ) -> Result<(), NTSTATUS> {
        use helios_protocol::native_render::{HELIOS_HVM1_REPLY_SLOT_BYTES, HELIOS_HVR1_MAGIC};
        let header_bytes = core::mem::size_of_val(header) as u64;
        let needed = header_bytes
            .checked_add(payload.len() as u64)
            .ok_or(STATUS_INVALID_DEVICE_REQUEST)?;
        let end = reply_offset
            .checked_add(needed)
            .ok_or(STATUS_INVALID_DEVICE_REQUEST)?;
        if reply_offset % HELIOS_HVM1_REPLY_SLOT_BYTES != 0
            || reply_capacity < needed
            || reply_capacity > HELIOS_HVM1_REPLY_SLOT_BYTES
            || end > facts.byte_size
            || header.magic != HELIOS_HVR1_MAGIC
        {
            return Err(STATUS_INVALID_DEVICE_REQUEST);
        }
        let dst = unsafe { facts.kernel_va.as_ptr().add(reply_offset as usize) };
        unsafe {
            core::ptr::write_unaligned(dst.cast::<u32>(), 0);
            core::ptr::copy_nonoverlapping(
                payload.as_ptr(),
                dst.add(header_bytes as usize),
                payload.len(),
            );
            let mut unpublished = *header;
            unpublished.magic = 0;
            core::ptr::copy_nonoverlapping(
                bytemuck::bytes_of(&unpublished).as_ptr(),
                dst,
                header_bytes as usize,
            );
        }
        // K2a mappings and 1-MiB slot offsets are page aligned, so the magic
        // word satisfies AtomicU32's alignment. This release store is the exact
        // publication edge for every header/payload byte copied above.
        let magic = unsafe { &*dst.cast::<AtomicU32>() };
        magic.store(HELIOS_HVR1_MAGIC.to_le(), Ordering::Release);
        K11_REPLY_PUBLISHED.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    /// Publish an HVR1 header over reply bytes K11 has already copied from its
    /// context-local private resource into the exact role-1 K2a range. This is
    /// the nonzero-ring executor seam: magic remains zero until both the real
    /// host terminal and the finite private-to-K2a copy have completed.
    pub(crate) fn publish_hvr1_existing_payload(
        facts: K11ReplyPoolFacts,
        reply_offset: u64,
        reply_capacity: u64,
        header: &helios_protocol::native_render::HeliosVenusReplyV1,
        payload_bytes: u64,
    ) -> Result<(), NTSTATUS> {
        use helios_protocol::native_render::{HELIOS_HVM1_REPLY_SLOT_BYTES, HELIOS_HVR1_MAGIC};
        let header_bytes = core::mem::size_of_val(header) as u64;
        let needed = header_bytes
            .checked_add(payload_bytes)
            .ok_or(STATUS_INVALID_DEVICE_REQUEST)?;
        let end = reply_offset
            .checked_add(needed)
            .ok_or(STATUS_INVALID_DEVICE_REQUEST)?;
        if reply_offset % HELIOS_HVM1_REPLY_SLOT_BYTES != 0
            || reply_capacity < needed
            || reply_capacity > HELIOS_HVM1_REPLY_SLOT_BYTES
            || end > facts.byte_size
            || header.magic != HELIOS_HVR1_MAGIC
        {
            return Err(STATUS_INVALID_DEVICE_REQUEST);
        }
        let dst = unsafe { facts.kernel_va.as_ptr().add(reply_offset as usize) };
        unsafe {
            core::ptr::write_unaligned(dst.cast::<u32>(), 0);
            let mut unpublished = *header;
            unpublished.magic = 0;
            core::ptr::copy_nonoverlapping(
                bytemuck::bytes_of(&unpublished).as_ptr(),
                dst,
                header_bytes as usize,
            );
        }
        let magic = unsafe { &*dst.cast::<AtomicU32>() };
        magic.store(HELIOS_HVR1_MAGIC.to_le(), Ordering::Release);
        K11_REPLY_PUBLISHED.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

pub(crate) struct SessionExecutionOperation {
    _operation: SessionOperation,
    pub(crate) context_id: u32,
    pub(crate) transport_instance: u64,
}

struct SessionOperation {
    owner: NonNull<SessionTransport>,
}

// SAFETY: SessionTransport is heap-pinned inside SessionObject and teardown
// joins its rundown before that object can be released.
unsafe impl Send for SessionOperation {}
unsafe impl Send for SessionExecutionOperation {}

impl Drop for SessionOperation {
    fn drop(&mut self) {
        let owner = unsafe { self.owner.as_ref() };
        let signal = {
            let mut rundown = owner.rundown.lock();
            debug_assert!(rundown.active != 0);
            rundown.active = rundown.active.saturating_sub(1);
            rundown.active == 0
        };
        if signal {
            unsafe { KeSetEvent(owner.drained.get(), 0, 0) };
        }
    }
}

const _: () = {
    assert!(pure::HOST_CREATE_INSTANCE_REPLY_BYTES == 24);
    assert!(pure::HOST_CREATE_INSTANCE_REPLY_BYTES <= SESSION_REPLY_BYTES);
    assert!(SESSION_REPLY_BYTES == HELIOS_HVM1_REPLY_POOL_BYTES);
    assert!(HELIOS_HVR1_HEADER_SIZE == 80);
    assert!(SESSION_INSTANCE_HANDLE != 0);
};
