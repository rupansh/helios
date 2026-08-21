//! PASSIVE-level control-command orchestration (C3/M3.4).
//!
//! Every virtio-gpu control verb (ctx/blob/map/attach/unref) is a multi-phase
//! flow here: table phase(s) under the device spinlock ([`VirtioGpu`] helpers)
//! interleaved with a host round-trip whose WAIT happens at PASSIVE_LEVEL on a
//! stack [`SyncWaitBlock`] KEVENT — never a DISPATCH spin under the spinlock.
//! The waits use adaptive slices and re-drain the used ring on each slice, so
//! they are interrupt-driven when interrupts flow and degrade to ~ms-latency
//! polling when they do not (bring-up, lost interrupts).
//!
//! Why this exists (2026-07-04 evidence): the host processes the virtio ctrl
//! queue serially, and a venus `RESOURCE_CREATE_BLOB(blob_id)` can legally
//! block host-side waiting for the vkr ring to execute the referenced
//! `vkAllocateMemory` — so ANY control command can take seconds under a
//! validate-slow host. The old model burned a ~1 s DISPATCH spin per waiter
//! under the device spinlock and then poisoned the transport; this model waits
//! properly and, on timeout, abandons only its own in-flight slot.
//!
//! # IRQL
//!
//! Every function in this module runs at PASSIVE_LEVEL, and since R614 that is a
//! signature rather than this comment: every entry point takes a
//! [`crate::irql::PassiveLevel`], which safe code cannot construct. What the
//! token proves, exactly:
//!
//! * **What it does prove.** A caller that holds no token cannot reach any of
//!   these functions at all. The concrete case: the DIRQL half of
//!   `DxgkDdiSetVidPnSourceAddress` (`ddi::display`) holds no token, so
//!   `set_scanout_blob` and `resource_flush` are unreachable from it, and adding
//!   such a call is a compile error instead of a shipped DISPATCH deadlock.
//! * **What it does NOT prove.** The live IRQL. Only `KeGetCurrentIrql` can, and
//!   this module deliberately does not call it per entry point — one check inside
//!   `PassiveLevel::assume` at the DDI boundary is the whole budget. So the
//!   guarantee is about *provenance*: every token in the driver traces to one of
//!   twelve audited mints (`grep -rn 'PassiveLevel::assume()' src/`), four of
//!   which sit below a runtime IRQL gate that already existed, plus one
//!   structural claim about the venus gateway
//!   (`AdapterContext::with_venus_client`).
//!
//! `crate::irql::IRQL_ASSUME_BAD` — the `IrqlBad` breadcrumb — is what turns a
//! wrong audit into evidence. It must read 0.
//!
//! One PASSIVE-only operation is still outside the type system:
//! `DmaBuffer`'s `Drop` (`MmFreeContiguousMemory`). `Drop::drop` has a fixed
//! signature, so the transport parks completed buffers and frees them from
//! [`reap_parked`] instead of letting the DISPATCH drain drop one.
//!
//! ONE function here is deliberately IRQL-free and takes no token:
//! [`fill_set_scanout_blob`], which only writes the fields of a wire command
//! someone else owns. It lives here so the DISPATCH-level fast bind
//! (`VirtioGpu::enqueue_scanout_bind_async`, ROADMAP defect 0ab-C) and the
//! PASSIVE round-trip below cannot encode the same command differently.

use alloc::vec::Vec;
use core::cell::Cell;
use core::mem::size_of;
use core::sync::atomic::AtomicU32;

use bytemuck::{bytes_of, cast_slice, Zeroable};
use wdk_sys::ntddk::{IoFreeMdl, KeDelayExecutionThread, KeWaitForSingleObject, MmUnlockPages};
use wdk_sys::{LARGE_INTEGER, PVOID, STATUS_SUCCESS};

use super::control_owner::ResourceBackingFinalizer;
use super::gpu::{
    BlobMapBegin, BlobMapFinish, BlobMapPrep, BlobRemapBegin, DeviceOwner, OwnerFilter,
    SyncOutcome, SyncTicket, SyncWaitBlock, WaitBlockRef, WaitDisposition, CTRL_TEARDOWN_ABANDONS,
    CTRL_TIMEOUT_COUNT,
};
use super::hal::DmaBuffer;
use super::VirtioError;
use crate::adapter::AdapterContext;
use crate::irql::PassiveLevel;
use core::sync::atomic::Ordering;
use helios_kmd_logic::context_attachment::AttachmentFinishEffect;
use helios_kmd_logic::context_lifecycle::ContextFinishEffect;
use helios_kmd_logic::control_owner_table::{DispatchWork, ObservedOwnerWork, PairKind};
use helios_kmd_logic::control_owner_tickets::RunnerOutcome;
use helios_kmd_logic::control_ownership::{
    AbandonReason, ControlVerb, HostRejection, ResourceFinishEffect, WindowFinishEffect,
};
use helios_protocol::{
    resp_is_ok, VirtioGpuCtrlHdr, VirtioGpuCtxCreate, VirtioGpuCtxDestroy, VirtioGpuCtxResource,
    VirtioGpuMemEntry, VirtioGpuRect, VirtioGpuResourceCreateBlob, VirtioGpuResourceMapBlob,
    VirtioGpuResourceUnmapBlob, VirtioGpuResourceUnref, VirtioGpuRespMapInfo,
    VirtioGpuSetScanoutBlob, VIRTIO_GPU_BLOB_FLAG_USE_MAPPABLE, VIRTIO_GPU_BLOB_MEM_GUEST,
    VIRTIO_GPU_BLOB_MEM_HOST3D, VIRTIO_GPU_CMD_CTX_ATTACH_RESOURCE, VIRTIO_GPU_CMD_CTX_CREATE,
    VIRTIO_GPU_CMD_CTX_DESTROY, VIRTIO_GPU_CMD_CTX_DETACH_RESOURCE,
    VIRTIO_GPU_CMD_RESOURCE_CREATE_BLOB, VIRTIO_GPU_CMD_RESOURCE_MAP_BLOB,
    VIRTIO_GPU_CMD_RESOURCE_UNMAP_BLOB, VIRTIO_GPU_CMD_RESOURCE_UNREF,
    VIRTIO_GPU_CMD_SET_SCANOUT_BLOB, VIRTIO_GPU_FLAG_FENCE, VIRTIO_GPU_FLAG_INFO_RING_IDX,
    VIRTIO_GPU_MAP_CACHE_MASK,
};

/// `KernelMode` (`KPROCESSOR_MODE`).
const KERNEL_MODE: i8 = 0;
/// `Executive` (`KWAIT_REASON`).
const EXECUTIVE: i32 = 0;
static CTRL_WAIT_PENDING: AtomicU32 = AtomicU32::new(0);
static CTRL_RESPONSE_MALFORMED: AtomicU32 = AtomicU32::new(0);
static FINALIZER_CUSTODY_INVARIANT: AtomicU32 = AtomicU32::new(0);

fn retain_resource_finalizer(
    finalizer: ResourceBackingFinalizer,
) -> Result<(), ResourceBackingFinalizer> {
    Err(finalizer)
}

/// Execute an orderly backing finalizer through an already-held Venus client.
/// Each successful stage is cleared before a later stage can fail, so a returned
/// token names only work that remains ambiguous.
pub(crate) fn finalize_resource_backing_with_client(
    client: &mut super::venus::VenusClient,
    adapter: &AdapterContext,
    mut finalizer: ResourceBackingFinalizer,
) -> Result<(), ResourceBackingFinalizer> {
    if finalizer.image_id != 0 {
        if client.destroy_image(adapter, finalizer.image_id).is_err() {
            return Err(finalizer);
        }
        finalizer.image_id = 0;
    }
    if finalizer.memory_id != 0 {
        if client
            .free_memory_blob(adapter, finalizer.memory_id)
            .is_err()
        {
            return Err(finalizer);
        }
        finalizer.memory_id = 0;
    }
    release_guest_pages(&mut finalizer);
    Ok(())
}

fn release_guest_pages(finalizer: &mut ResourceBackingFinalizer) {
    if finalizer.guest_mdl == 0 {
        return;
    }
    let mdl = finalizer.guest_mdl as wdk_sys::PMDL;
    finalizer.guest_mdl = 0;
    // SAFETY: the SetAllocationBackingStore path owns a successfully probed and
    // locked MDL. This finalizer is its sole release authority.
    unsafe {
        MmUnlockPages(mdl);
        IoFreeMdl(mdl);
    }
}

/// Physical reset has already made every host identity in the token inert.
/// Venus ids need no command; guest pages still require their local unlock.
pub(crate) fn finalize_resource_backing_after_reset(
    _passive: PassiveLevel,
    mut finalizer: ResourceBackingFinalizer,
) {
    finalizer.image_id = 0;
    finalizer.memory_id = 0;
    release_guest_pages(&mut finalizer);
}

fn finalize_resource_backing(
    passive: PassiveLevel,
    adapter: &AdapterContext,
    finalizer: ResourceBackingFinalizer,
) -> Result<(), ResourceBackingFinalizer> {
    if finalizer.is_empty() {
        return Ok(());
    }
    if finalizer.image_id == 0 && finalizer.memory_id == 0 {
        let mut finalizer = finalizer;
        release_guest_pages(&mut finalizer);
        return Ok(());
    }
    let mut pending = Some(finalizer);
    match adapter.with_venus_client(passive, |client| {
        let Some(finalizer) = pending.take() else {
            // `with_venus_client` accepts `FnOnce`, so this arm is
            // structurally unreachable. If that contract is ever weakened,
            // preserve safety by doing no second finalization and count the
            // invariant loss instead of bugchecking the machine.
            bump_wait_refusal(&FINALIZER_CUSTODY_INVARIANT, b"FnCust");
            return Ok(());
        };
        finalize_resource_backing_with_client(client, adapter, finalizer)
    }) {
        Ok(result) => result,
        Err(_) => match pending.take() {
            Some(finalizer) => Err(finalizer),
            None => {
                // Today `with_venus_client` returns `Err` only without calling
                // the closure, which leaves `pending` populated. If that
                // contract drifts, never synthesize or double-run custody.
                bump_wait_refusal(&FINALIZER_CUSTODY_INVARIANT, b"FnCust");
                Ok(())
            }
        },
    }
}

fn bump_wait_refusal(counter: &AtomicU32, name: &[u8]) {
    let n = counter.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
    if n == 1 || n % 64 == 0 {
        crate::diag::record_named_bytes(name, n);
    }
}

/// Default PASSIVE wait budget for one synchronous control round-trip. Sized
/// for a validate-slow host whose ctrl queue is momentarily blocked behind a
/// wait-for-mem-alloc blob create; beyond this the host is genuinely wedged
/// and the command fails loudly (`VirtioError::Timeout`).
const SYNC_ROUNDTRIP_TIMEOUT_MS: u64 = 30_000;
/// Backpressure retry budget when the control queue / in-flight tables are
/// full. MILLISECONDS, like its three siblings — it used to be a bare retry
/// count that only *happened* to equal 5 s because the sleep is hard-coded to
/// 1 ms.
const ENQUEUE_RETRY_MAX_MS: u64 = 5_000;
/// Bound on waiting out another mapper's in-flight RESOURCE_MAP_BLOB.
/// MILLISECONDS, as above.
const MAP_BUSY_MAX_MS: u64 = 30_000;

/// One PASSIVE retry slice. See [`sleep_ms`] for why this is not really 1 ms.
const RETRY_SLICE_MS: u64 = 1;

/// A retry budget in MILLISECONDS.
///
/// Two of the four sibling constants used to be millisecond budgets and two
/// were bare retry counts that only *happened* to equal 5 s and 30 s because
/// the sleep is hard-coded to 1 ms, and a fifth budget was an unnamed literal.
/// A units mismatch like that makes every reader over-estimate how fast the
/// driver gives up.
///
/// It counts NOMINAL slept milliseconds, not wall clock, which is exactly what
/// the retry counters it replaces did — same numbers, same sleeps, same failure
/// statuses. See [`sleep_ms`] for why nominal and actual differ by up to ~16x.
struct Budget {
    total_ms: u64,
    spent_ms: u64,
}

impl Budget {
    const fn new(total_ms: u64) -> Self {
        Self {
            total_ms,
            spent_ms: 0,
        }
    }

    /// Charge one slice. Returns true once the budget is exhausted, matching
    /// the old `attempts > MAX` test exactly (charge first, then test).
    fn charge_slice(&mut self) -> bool {
        self.spent_ms = self.spent_ms.saturating_add(RETRY_SLICE_MS);
        self.expired()
    }

    fn expired(&self) -> bool {
        self.spent_ms > self.total_ms
    }

    #[allow(dead_code)]
    fn elapsed_ms(&self) -> u64 {
        self.spent_ms
    }
}

/// PASSIVE sleep for ~`ms` milliseconds.
pub(crate) fn sleep_ms(_passive: PassiveLevel, ms: u64) {
    let mut interval: LARGE_INTEGER = unsafe { core::mem::zeroed() };
    interval.QuadPart = -((ms.max(1) as i64) * 10_000);
    // SAFETY: PASSIVE_LEVEL relative-timeout sleep.
    let _ = unsafe { KeDelayExecutionThread(KERNEL_MODE, 0, &mut interval) };
}
// ⚠ `KeDelayExecutionThread` with a small relative timeout rounds UP to the
// system timer granularity — ~15.6 ms by default. A `sleep_ms(1)` therefore
// costs up to ~16 ms of thread residency, so a [`Budget`] of N nominal
// milliseconds can be up to ~16N of real time. Every budget in this module is
// nominal for that reason; do not read one as wall clock.

/// Wait on `block` for up to `total_ms`, in adaptive slices (1 ms → 1 s),
/// opportunistically draining the used ring after each slice so a lost
/// interrupt costs only slice latency. Returns whether the block completed.
///
/// ⚠ THE SIGNAL IS THE ONLY COMPLETION-SIDE EXIT. THE INVARIANT: a waiter may
/// resume — and therefore pop the stack frame this block lives in — only once
/// the signaler has finished touching the block. This loop has exactly two
/// exits, and each one carries that guarantee:
///
///   * `KeWaitForSingleObject` returning STATUS_SUCCESS. The kernel's own
///     stack-event contract: the wait cannot be satisfied before `KeSetEvent`
///     has finished with the dispatcher object.
///   * The timeout, which goes to `abandon_sync` under `virtio_lock` — the same
///     lock the whole `InFlightKind::Sync` arm runs under, so abandon either
///     clears `waiter` before the drain runs (and the drain then signals
///     nothing) or observes `AlreadyCompleted` after the arm finished. The
///     timed-out waiter's frame is alive across its own abandon call, so the
///     block outlives every access on that side too.
///
/// A LOCK-FREE terminal-state POLL PROVIDES NEITHER, and this loop used to open
/// with one. The terminal state is published immediately before `KeSetEvent`
/// in the drain's Sync arm, so a waiter
/// polling it could return, pop its frame, and leave the drain to memcpy and
/// signal a dead stack frame — one ISR or KVM vm-exit inside that one-
/// instruction window is all it takes. That is the 22.22.218.0 `0xA` bugcheck,
/// root-caused from two dumps (ROADMAP defect 0ab-C): `KeSetEvent` walking the
/// waiter list of a "KEVENT" that was the HPD worker's own popped frame. It
/// only became reachable when the sync waits started outliving a wait slice.
///
/// Nothing is lost by removing it: a completion that lands before the first
/// wait leaves the KEVENT SIGNALED, and a KEVENT holds state, so the next
/// `KeWaitForSingleObject` returns immediately. The only cost is one
/// 15.6 ms-granularity slice on the rare poll-hit, and correctness owns that
/// trade.
fn wait_block(
    _passive: PassiveLevel,
    adapter: &AdapterContext,
    block: &WaitBlockRef<'_>,
    total_ms: u64,
) -> bool {
    let mut waited: u64 = 0;
    let mut slice: u64 = 1;
    loop {
        if waited >= total_ms {
            return false;
        }
        let this_slice = slice.min(total_ms - waited);
        let mut timeout: LARGE_INTEGER = unsafe { core::mem::zeroed() };
        timeout.QuadPart = -((this_slice.max(1) as i64) * 10_000);
        // SAFETY: the KEVENT was initialized by SyncWaitBlock::init at this
        // address and outlives the wait; PASSIVE_LEVEL.
        let status = unsafe {
            KeWaitForSingleObject(
                core::ptr::addr_of_mut!((*block.as_ptr().as_ptr()).event) as PVOID,
                EXECUTIVE,
                KERNEL_MODE,
                0,
                &mut timeout,
            )
        };
        if status == STATUS_SUCCESS {
            return true;
        }
        waited += this_slice;
        slice = (slice * 2).min(1_000);
        // Interrupt-loss tolerance: drain whatever completed.
        let _ = adapter.with_virtio(|v| v.drain_used(adapter));
    }
}

/// One finite event wait, with no periodic used-ring drain.
///
/// K11 has no interrupt-loss polling fallback and no queue-full sleep/retry:
/// either its one descriptor is accepted and the transport interrupt signals
/// this event, or the bounded wait is abandoned as one ambiguous operation.
fn wait_block_once(_passive: PassiveLevel, block: &WaitBlockRef<'_>, total_ms: u64) -> bool {
    let mut timeout: LARGE_INTEGER = unsafe { core::mem::zeroed() };
    timeout.QuadPart = -((total_ms.max(1) as i64) * 10_000);
    // SAFETY: the KEVENT was initialized by SyncWaitBlock::init at this address,
    // outlives this single PASSIVE_LEVEL wait, and no code samples its state.
    unsafe {
        KeWaitForSingleObject(
            core::ptr::addr_of_mut!((*block.as_ptr().as_ptr()).event) as PVOID,
            EXECUTIVE,
            KERNEL_MODE,
            0,
            &mut timeout,
        ) == STATUS_SUCCESS
    }
}

/// Reap completed entries at PASSIVE and retain their DMA buffers for reuse.
/// `MmAllocateContiguousMemory` per tiny Venus submission dominated DWM's
/// command rate; recycling page-backed buffers removes that steady-state cost.
pub fn reap_parked(_passive: PassiveLevel, adapter: &AdapterContext) {
    let work = adapter.with_virtio(|v| v.begin_parked_reap());
    let Ok(Some((mut dead, mut buffers))) = work else {
        return;
    };
    debug_assert!(buffers.capacity() >= dead.len().saturating_mul(2));
    for entry in dead.drain(..) {
        let (meta, venus) = entry.into_dma_buffers();
        buffers.push(meta);
        if let Some(venus) = venus {
            buffers.push(venus);
        }
    }
    // Moving buffers into the pre-reserved pool is allocation-free under the
    // spinlock. Excess buffers are returned and dropped here at PASSIVE.
    //
    // The `else` arm is the two-phase strand: returning here without
    // finish_parked_reap left reap_in_progress true and both pre-reserved
    // spares taken, permanently disabling reaping and then refusing every
    // enqueue at the PARKED_ENQUEUE_GATE. `dead` is already drained, so the
    // abort restores both vectors intact.
    let excess = adapter.with_virtio(move |v| v.recycle_dma_buffers(buffers));
    let Ok(mut excess) = excess else {
        let _ = adapter.with_virtio(move |v| v.abort_parked_reap(dead, alloc::vec::Vec::new()));
        return;
    };
    // Drop only the retained elements at PASSIVE while preserving the vector's
    // allocation for the next reap.
    excess.clear();
    let _ = adapter.with_virtio(move |v| v.finish_parked_reap(dead, excess));
}

/// A scan-out bind's mint, as passed down to the enqueue: where the minted wire
/// sequence goes, and which resource the command names.
///
/// The two travel together because they are published together, under the one
/// `virtio_lock` hold that enqueues the command — the sequence orders the
/// bookkeeping, the resource is the WIRE view of what is bound
/// (`AdapterContext::scanout_bind_wire_resource`), and neither is meaningful
/// against a different command's lock hold.
#[derive(Clone, Copy)]
struct BindMint<'a> {
    seq_out: &'a Cell<u64>,
    instance_out: &'a Cell<u64>,
    fence_out: &'a Cell<u64>,
    /// The `SET_SCANOUT_BLOB`'s own `resource_id`; 0 is the scan-out disable.
    resource_id: u32,
    /// Request one standard virtio-gpu fence. False is the legacy command;
    /// true is reachable only through the SURFACE-derived D2 owner.
    fenced: bool,
    /// D2's no-wait publication edge. Called after the descriptor and
    /// its exact mint are in the transport's in-flight table but before this
    /// `virtio_lock` hold can drain a completion. It may perform bounded plane
    /// state transitions only; no allocation, wait, cleanup, or ETW write.
    publish: Option<&'a dyn Fn(FencedScanoutPublish)>,
}

pub(crate) struct ScanoutBindIdentity {
    instance: u64,
    sequence: u64,
    fence_id: u64,
    resource_id: u32,
}

impl ScanoutBindIdentity {
    pub(crate) const fn instance(&self) -> u64 {
        self.instance
    }

    pub(crate) const fn sequence(&self) -> u64 {
        self.sequence
    }

    pub(crate) const fn fence_id(&self) -> u64 {
        self.fence_id
    }

    pub(crate) const fn resource_id(&self) -> u32 {
        self.resource_id
    }
}

/// Terminal classification for the D2 fenced SET path.
///
/// Every outcome after descriptor acceptance carries the exact mint. An
/// ambiguous response therefore preserves a completion key instead of losing
/// the identity needed to retain the plane candidate through physical reset.
#[must_use]
pub(crate) enum FencedScanoutSetOutcome {
    Accepted(ScanoutBindIdentity),
    Rejected(ScanoutBindIdentity),
    DefiniteNotEnqueued { error: VirtioError, instance: u64 },
    Ambiguous(Option<ScanoutBindIdentity>),
}

#[derive(Clone, Copy)]
pub(crate) struct FencedScanoutPublish {
    pub instance: u64,
    pub sequence: u64,
    pub fence_id: u64,
    pub resource_id: u32,
}

#[must_use]
pub(crate) enum CtrlRoundtripOutcome {
    HostResponseCopied { written_length: u32 },
    DefiniteNotEnqueued(VirtioError),
    Ambiguous(AbandonReason),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CtrlRoundtripMode {
    /// Existing control users retain their bounded queue-full retry and
    /// adaptive interrupt-loss drain behavior.
    LegacyRetry,
    /// K11: one enqueue attempt followed by one event wait.  No periodic drain,
    /// sleep, retry, or independent virtio fence timeline is permitted.
    FiniteEvent,
}

impl CtrlRoundtripOutcome {
    fn into_legacy_result(self) -> Result<usize, VirtioError> {
        match self {
            Self::HostResponseCopied { written_length } => Ok(written_length as usize),
            Self::DefiniteNotEnqueued(error) => Err(error),
            Self::Ambiguous(AbandonReason::Timeout) => Err(VirtioError::Timeout),
            Self::Ambiguous(
                AbandonReason::NotOurs
                | AbandonReason::TransportAborted
                | AbandonReason::MalformedResponse,
            ) => Err(VirtioError::DeviceError),
        }
    }
}

/// One synchronous control round-trip: `req` (+ optional second device-read
/// span `extra`) → device → `resp_out`. Blocks at PASSIVE until completion or
/// `timeout_ms`. On timeout the in-flight slot is abandoned (reaped when the
/// completion eventually arrives) — the transport is NOT poisoned.
/// `bind`, when supplied, is minted INSIDE the same `with_virtio` as the
/// successful enqueue (ROADMAP defect 0ab-C). Minting there and nowhere else is
/// what makes the sequence agree with the control queue's FIFO order, and
/// therefore with the order the host applies binds in. `None` for every command
/// that is not a `SET_SCANOUT_BLOB`.
fn ctrl_roundtrip_observed(
    passive: PassiveLevel,
    adapter: &AdapterContext,
    req: &[u8],
    extra: Option<&[u8]>,
    resp_out: &mut [u8],
    timeout_ms: u64,
    bind: Option<BindMint<'_>>,
    expected_instance: Option<u64>,
    mode: CtrlRoundtripMode,
) -> CtrlRoundtripOutcome {
    // Bind DNE reconciliation needs the exact producing transport even when
    // allocation or argument validation fails before enqueue. Snapshot only
    // this non-secret scalar; later cleanup still revalidates it under the live
    // transport lock and is inert after replacement.
    if let Some(bind) = bind {
        if let Ok(instance) = adapter.with_virtio(|v| v.scanout_transport_instance()) {
            bind.instance_out.set(instance);
        }
    }
    let in0_len = req.len();
    let in1_len = extra.map_or(0, |e| e.len());
    let resp_len = resp_out.len();
    if in0_len == 0 || resp_len == 0 {
        return CtrlRoundtripOutcome::DefiniteNotEnqueued(VirtioError::DeviceError);
    }
    reap_parked(passive, adapter);

    let total = in0_len + in1_len + resp_len;
    let Some(mut meta) = DmaBuffer::new(passive, total) else {
        return CtrlRoundtripOutcome::DefiniteNotEnqueued(VirtioError::OutOfMemory);
    };
    {
        let m = meta.as_mut_slice();
        m[..in0_len].copy_from_slice(req);
        if let Some(e) = extra {
            m[in0_len..in0_len + in1_len].copy_from_slice(e);
        }
    }

    // The wait block is created, initialised and dropped inside `with`, so it
    // is never nameable here: "registered before init" and "moved after init"
    // are not expressible. The abandon-on-timeout epilogue is the closure's
    // last statement, which is what keeps deregistration paired with the frame.
    SyncWaitBlock::with(|block| {
        // Enqueue, with PASSIVE backpressure while the queue is full.
        //
        // `meta` is carried as a loop value, not round-tripped through an
        // `Option`. The enqueue moves it into the closure and the QueueFull arm
        // reinitialises it before the back edge, which Rust's flow-sensitive move
        // checking accepts. A future retry arm that forgets to hand the buffer back
        // is then a *compile* error, where the take-then-expect this replaces was a
        // `KeBugCheck` inside a DDI on the next iteration.
        let mut budget = Budget::new(ENQUEUE_RETRY_MAX_MS);
        let token: SyncTicket = loop {
            let res = adapter.with_virtio(move |v| {
                // Existing users retain the opportunistic interrupt-loss
                // drain. K11's finite mode is event-driven only: if prior
                // completions have not freed a descriptor, its one enqueue
                // attempt reports QueueFull instead of sampling the used ring.
                if mode == CtrlRoundtripMode::LegacyRetry {
                    v.drain_used(adapter);
                }
                let queued = if expected_instance
                    .is_some_and(|expected| expected != v.scanout_transport_instance())
                {
                    Err((meta, VirtioError::DeviceError))
                } else {
                    v.enqueue_sync(
                        meta,
                        in0_len,
                        in1_len,
                        resp_len,
                        block.as_ptr(),
                        bind.map(|bind| (bind.resource_id, bind.fenced)),
                        adapter,
                    )
                };
                // The sequence is reserved before the queue add and committed
                // by `enqueue_sync` only after the descriptor is accepted. Keep
                // the caller's value in lockstep with the in-flight lifecycle
                // tag, so a late response after waiter abandonment can still
                // update the host-selection ledger.
                match queued {
                    Ok((ticket, identity)) => {
                        if let (Some(bind), Some((instance, seq, fence_id))) = (bind, identity) {
                            bind.instance_out.set(instance);
                            bind.seq_out.set(seq);
                            bind.fence_out.set(fence_id);
                            if let Some(publish) = bind.publish {
                                publish(FencedScanoutPublish {
                                    instance,
                                    sequence: seq,
                                    fence_id,
                                    resource_id: bind.resource_id,
                                });
                            }
                        }
                        Ok(ticket)
                    }
                    Err(error) => Err(error),
                }
            });
            match res {
                Err(_) => {
                    return CtrlRoundtripOutcome::DefiniteNotEnqueued(VirtioError::DeviceError)
                } // transport gone
                Ok(Ok(ticket)) => break ticket,
                Ok(Err((m_back, VirtioError::QueueFull))) => {
                    meta = m_back;
                    if mode == CtrlRoundtripMode::FiniteEvent {
                        return CtrlRoundtripOutcome::DefiniteNotEnqueued(VirtioError::QueueFull);
                    }
                    if budget.charge_slice() {
                        return CtrlRoundtripOutcome::DefiniteNotEnqueued(VirtioError::QueueFull);
                    }
                    reap_parked(passive, adapter);
                    sleep_ms(passive, RETRY_SLICE_MS);
                }
                Ok(Err((_m, e))) => return CtrlRoundtripOutcome::DefiniteNotEnqueued(e), // dropped here at PASSIVE
            }
        };

        // Kept for the refusal breadcrumb: SyncTicket is move-only, so it is
        // consumed by abandon_sync and cannot be read afterwards.
        let token_value = token.raw();
        let completed = match mode {
            CtrlRoundtripMode::LegacyRetry => wait_block(passive, adapter, block, timeout_ms),
            CtrlRoundtripMode::FiniteEvent => wait_block_once(passive, block, timeout_ms),
        };
        if !completed {
            // Final race check + abandonment under the lock.
            // Three outcomes, not two. `unwrap_or(true)` folded Err(DeviceNotFound)
            // - the transport was torn down under us - into "already completed
            // successfully", which skipped the timeout counter and picked the wrong
            // error class. Callers validate the exact returned response shape,
            // but the missing evidence was real.
            let reconciled = match mode {
                CtrlRoundtripMode::LegacyRetry => adapter.with_virtio(|v| {
                    v.drain_used(adapter);
                    v.abandon_sync(token, block.as_ptr())
                }),
                // No final poll for K11. `abandon_sync` is the exact locked
                // ownership transition: it still observes a completion that an
                // interrupt handler already published while the timeout raced,
                // but it never drives completion by sampling the used ring.
                CtrlRoundtripMode::FiniteEvent => {
                    adapter.with_virtio(|v| v.abandon_sync(token, block.as_ptr()))
                }
            };
            match reconciled {
                // The drain or failure latch already signalled us; disposition
                // below distinguishes copied response from transport abort.
                Ok(SyncOutcome::AlreadyCompleted) => {}
                Ok(SyncOutcome::Abandoned) => {
                    CTRL_TIMEOUT_COUNT.fetch_add(1, Ordering::Relaxed);
                    return CtrlRoundtripOutcome::Ambiguous(AbandonReason::Timeout);
                }
                // NEW population. The token names an entry that is not this
                // waiter's, so `resp` was never written — do NOT copy it out.
                // The old bool folded this into "already completed" and handed
                // the caller a zeroed buffer.
                Ok(SyncOutcome::NotOurs) => {
                    crate::diag::record_named_bytes(b"CtNotOurs", u32::from(token_value));
                    return CtrlRoundtripOutcome::Ambiguous(AbandonReason::NotOurs);
                }
                Err(_) => {
                    CTRL_TEARDOWN_ABANDONS.fetch_add(1, Ordering::Relaxed);
                    return CtrlRoundtripOutcome::Ambiguous(AbandonReason::TransportAborted);
                }
            }
        }
        // SAFETY: signal satisfaction or the locked AlreadyCompleted arm above
        // proves the terminal publisher finished touching the stack block.
        let observation = unsafe { block.copy_host_response_after_completion(resp_out) };
        match observation.disposition() {
            WaitDisposition::HostResponseAvailable => CtrlRoundtripOutcome::HostResponseCopied {
                written_length: observation.written_length(),
            },
            WaitDisposition::TransportAborted => {
                CTRL_TEARDOWN_ABANDONS.fetch_add(1, Ordering::Relaxed);
                CtrlRoundtripOutcome::Ambiguous(AbandonReason::TransportAborted)
            }
            WaitDisposition::MalformedResponse => {
                bump_wait_refusal(&CTRL_RESPONSE_MALFORMED, b"CtRsLen");
                CtrlRoundtripOutcome::Ambiguous(AbandonReason::MalformedResponse)
            }
            WaitDisposition::Pending => {
                bump_wait_refusal(&CTRL_WAIT_PENDING, b"CtDsPend");
                CtrlRoundtripOutcome::Ambiguous(AbandonReason::MalformedResponse)
            }
        }
    })
}

fn owner_response_word(response: &[u8], written_length: u32, offset: usize) -> u32 {
    if written_length as usize >= offset.saturating_add(size_of::<u32>())
        && response.len() >= offset.saturating_add(size_of::<u32>())
    {
        u32::from_le_bytes([
            response[offset],
            response[offset + 1],
            response[offset + 2],
            response[offset + 3],
        ])
    } else {
        0
    }
}

fn run_owner_work<K>(
    passive: PassiveLevel,
    adapter: &AdapterContext,
    work: DispatchWork<K>,
    verb: ControlVerb,
    request: &[u8],
    response: &mut [u8],
    mode: CtrlRoundtripMode,
) -> ObservedOwnerWork<VirtioError, K> {
    // SAFETY: this closure performs exactly one synchronous publish attempt,
    // returns the transport's exact copied length, and retains no wire key.
    unsafe {
        work.run_once(|key| {
            if key.verb() != verb {
                return RunnerOutcome::Ambiguous(AbandonReason::NotOurs);
            }
            match ctrl_roundtrip_observed(
                passive,
                adapter,
                request,
                None,
                response,
                SYNC_ROUNDTRIP_TIMEOUT_MS,
                None,
                None,
                mode,
            ) {
                CtrlRoundtripOutcome::HostResponseCopied { written_length } => {
                    RunnerOutcome::HostResponse {
                        response_type: owner_response_word(response, written_length, 0),
                        written_length: written_length as usize,
                        map_info: owner_response_word(
                            response,
                            written_length,
                            size_of::<VirtioGpuCtrlHdr>(),
                        ),
                    }
                }
                CtrlRoundtripOutcome::DefiniteNotEnqueued(error) => {
                    RunnerOutcome::DefiniteNotEnqueued(error)
                }
                CtrlRoundtripOutcome::Ambiguous(reason) => RunnerOutcome::Ambiguous(reason),
            }
        })
    }
}

fn owner_abandon_error(reason: AbandonReason) -> VirtioError {
    match reason {
        AbandonReason::Timeout => VirtioError::Timeout,
        AbandonReason::NotOurs
        | AbandonReason::TransportAborted
        | AbandonReason::MalformedResponse => VirtioError::DeviceError,
    }
}

fn ctrl_roundtrip(
    passive: PassiveLevel,
    adapter: &AdapterContext,
    req: &[u8],
    extra: Option<&[u8]>,
    resp_out: &mut [u8],
    timeout_ms: u64,
    bind: Option<BindMint<'_>>,
    expected_instance: Option<u64>,
) -> Result<usize, VirtioError> {
    ctrl_roundtrip_observed(
        passive,
        adapter,
        req,
        extra,
        resp_out,
        timeout_ms,
        bind,
        expected_instance,
        CtrlRoundtripMode::LegacyRetry,
    )
    .into_legacy_result()
}

/// Round-trip expecting a bare `VirtioGpuCtrlHdr` response; checks RESP_OK.
fn ctrl_roundtrip_ok(
    passive: PassiveLevel,
    adapter: &AdapterContext,
    req: &[u8],
    extra: Option<&[u8]>,
) -> Result<(), VirtioError> {
    ctrl_roundtrip_ok_seq(passive, adapter, req, extra, None)
}

fn ctrl_roundtrip_ok_seq(
    passive: PassiveLevel,
    adapter: &AdapterContext,
    req: &[u8],
    extra: Option<&[u8]>,
    bind: Option<BindMint<'_>>,
) -> Result<(), VirtioError> {
    ctrl_roundtrip_ok_mode(
        passive,
        adapter,
        req,
        extra,
        bind,
        CtrlRoundtripMode::LegacyRetry,
    )
}

fn ctrl_roundtrip_ok_finite(
    passive: PassiveLevel,
    adapter: &AdapterContext,
    req: &[u8],
    extra: Option<&[u8]>,
) -> Result<(), VirtioError> {
    ctrl_roundtrip_ok_mode(
        passive,
        adapter,
        req,
        extra,
        None,
        CtrlRoundtripMode::FiniteEvent,
    )
}

fn ctrl_roundtrip_ok_mode(
    passive: PassiveLevel,
    adapter: &AdapterContext,
    req: &[u8],
    extra: Option<&[u8]>,
    bind: Option<BindMint<'_>>,
    mode: CtrlRoundtripMode,
) -> Result<(), VirtioError> {
    let mut resp = [0u8; size_of::<VirtioGpuCtrlHdr>()];
    let outcome = ctrl_roundtrip_observed(
        passive,
        adapter,
        req,
        extra,
        &mut resp,
        SYNC_ROUNDTRIP_TIMEOUT_MS,
        bind,
        None,
        mode,
    );
    match outcome {
        CtrlRoundtripOutcome::HostResponseCopied { .. } => {}
        CtrlRoundtripOutcome::DefiniteNotEnqueued(error) => return Err(error),
        CtrlRoundtripOutcome::Ambiguous(AbandonReason::Timeout) => {
            return Err(VirtioError::Timeout)
        }
        CtrlRoundtripOutcome::Ambiguous(
            AbandonReason::NotOurs
            | AbandonReason::TransportAborted
            | AbandonReason::MalformedResponse,
        ) => return Err(VirtioError::DeviceError),
    }
    // `resp` starts zeroed and only the reported prefix was copied. Reading the
    // full word preserves legacy non-SET behavior for short responses while the
    // exact length remains available to strict SET/future OwnerTable callers.
    let resp_type = u32::from_le_bytes([resp[0], resp[1], resp[2], resp[3]]);
    debug_assert!(bind.is_none());
    // Keep the legacy response-type decision until OwnerTable activation can
    // retain request/backing custody on a malformed persistent response. The
    // exact written length is still propagated and is consumed by SET-specific
    // ambiguity handling and future table normalization.
    if resp_is_ok(resp_type) {
        Ok(())
    } else {
        Err(VirtioError::DeviceError)
    }
}

// ── Context lifecycle ────────────────────────────────────────────────────────

/// Create a virtio-gpu 3D context bound to `capset_id` (Venus = 4) and return
/// the guest-assigned context id. `owner` is the D3D device handle recorded for
/// `DxgkDdiDestroyDevice` reclamation (0 = KMD-internal).
pub fn ctx_create(
    passive: PassiveLevel,
    adapter: &AdapterContext,
    capset_id: u32,
    owner: Option<DeviceOwner>,
) -> Result<u32, VirtioError> {
    ctx_create_mode(
        passive,
        adapter,
        capset_id,
        owner,
        CtrlRoundtripMode::LegacyRetry,
    )
}

pub(crate) fn ctx_create_session(
    passive: PassiveLevel,
    adapter: &AdapterContext,
    capset_id: u32,
    owner: DeviceOwner,
) -> Result<u32, VirtioError> {
    ctx_create_mode(
        passive,
        adapter,
        capset_id,
        Some(owner),
        CtrlRoundtripMode::FiniteEvent,
    )
}

fn ctx_create_mode(
    passive: PassiveLevel,
    adapter: &AdapterContext,
    capset_id: u32,
    owner: Option<DeviceOwner>,
    mode: CtrlRoundtripMode,
) -> Result<u32, VirtioError> {
    if super::control_owner::KMD_D2_OWNER_ENABLED {
        let (ctx_id, work) = adapter.control_owner().begin_context_create(owner)?;
        let mut cmd = VirtioGpuCtxCreate::zeroed();
        cmd.hdr.type_ = VIRTIO_GPU_CMD_CTX_CREATE;
        cmd.hdr.ctx_id = ctx_id;
        cmd.context_init = capset_id;
        const NAME: &[u8] = b"helios";
        cmd.nlen = NAME.len() as u32;
        cmd.debug_name[..NAME.len()].copy_from_slice(NAME);
        crate::diag::record(0x0D20_0000 | (ctx_id & 0xFFFF));
        let mut response = [0u8; size_of::<VirtioGpuCtrlHdr>()];
        let observed = run_owner_work(
            passive,
            adapter,
            work,
            ControlVerb::ContextCreate,
            bytes_of(&cmd),
            &mut response,
            mode,
        );
        return match adapter.control_owner().finish_context(observed)? {
            ContextFinishEffect::CreateCompleted => {
                crate::diag::record(0x0D21_0000 | (ctx_id & 0xFFFF));
                Ok(ctx_id)
            }
            ContextFinishEffect::CreateDefiniteNotEnqueued(error) => Err(error),
            ContextFinishEffect::CreateAmbiguous(reason) => Err(owner_abandon_error(reason)),
            ContextFinishEffect::CreateHostRejected(_)
            | ContextFinishEffect::DestroyDefiniteNotEnqueued(_)
            | ContextFinishEffect::DestroyCompleted
            | ContextFinishEffect::DestroyHostRejected(_)
            | ContextFinishEffect::DestroyAmbiguous(_) => Err(VirtioError::DeviceError),
        };
    }
    let ctx_id = adapter
        .with_virtio(|v| v.alloc_ctx_id())
        .map_err(|_| VirtioError::DeviceError)?;
    // Reserve the tracking slot BEFORE the host round-trip: tracking is
    // mandatory, so a context this driver cannot track must not be created.
    let reserved = adapter
        .with_virtio(|v| v.reserve_context_slot())
        .map_err(|_| VirtioError::DeviceError)?;
    if !reserved {
        return Err(VirtioError::OutOfMemory);
    }
    let mut cmd = VirtioGpuCtxCreate::zeroed();
    cmd.hdr.type_ = VIRTIO_GPU_CMD_CTX_CREATE;
    cmd.hdr.ctx_id = ctx_id;
    // With VIRTIO_GPU_F_CONTEXT_INIT, context_init carries the capset id.
    cmd.context_init = capset_id;
    // A debug name helps host-side (virglrenderer) logs; purely cosmetic.
    const NAME: &[u8] = b"helios";
    cmd.nlen = NAME.len() as u32;
    cmd.debug_name[..NAME.len()].copy_from_slice(NAME);
    crate::diag::record(0x0D20_0000 | (ctx_id & 0xFFFF));
    if let Err(e) = ctrl_roundtrip_ok_mode(passive, adapter, bytes_of(&cmd), None, None, mode) {
        let _ = adapter.with_virtio(|v| v.cancel_context_reservation());
        return Err(e);
    }
    crate::diag::record(0x0D21_0000 | (ctx_id & 0xFFFF));
    let _ = adapter.with_virtio(|v| v.commit_context(owner, ctx_id));
    Ok(ctx_id)
}

/// Destroy a context and drop its tracking slot, scoped to its owner.
///
/// The untrack and the ownership test are ONE step under the device lock, so a
/// racing CTX_DESTROY for the same id cannot have both callers pass the check.
/// A guest-supplied id that this owner does not own never reaches the wire:
/// before this, CTX_DESTROY took the raw id straight to the host, so process B
/// (or A after a restart that recycled the id) could destroy process A's Venus
/// context and A's next submit referenced a destroyed host context — CS error,
/// fatal decoder state (k-capsescape-02).
pub fn ctx_destroy(
    passive: PassiveLevel,
    adapter: &AdapterContext,
    owner: Option<DeviceOwner>,
    ctx_id: u32,
) -> Result<(), VirtioError> {
    ctx_destroy_mode(
        passive,
        adapter,
        owner,
        ctx_id,
        CtrlRoundtripMode::LegacyRetry,
    )
}

pub(crate) fn ctx_destroy_session(
    passive: PassiveLevel,
    adapter: &AdapterContext,
    owner: DeviceOwner,
    ctx_id: u32,
) -> Result<(), VirtioError> {
    ctx_destroy_mode(
        passive,
        adapter,
        Some(owner),
        ctx_id,
        CtrlRoundtripMode::FiniteEvent,
    )
}

fn ctx_destroy_mode(
    passive: PassiveLevel,
    adapter: &AdapterContext,
    owner: Option<DeviceOwner>,
    ctx_id: u32,
    mode: CtrlRoundtripMode,
) -> Result<(), VirtioError> {
    if super::control_owner::KMD_D2_OWNER_ENABLED {
        let work = adapter
            .control_owner()
            .begin_context_destroy(owner, ctx_id)?;
        let mut cmd = VirtioGpuCtxDestroy::zeroed();
        cmd.hdr.type_ = VIRTIO_GPU_CMD_CTX_DESTROY;
        cmd.hdr.ctx_id = ctx_id;
        let mut response = [0u8; size_of::<VirtioGpuCtrlHdr>()];
        let observed = run_owner_work(
            passive,
            adapter,
            work,
            ControlVerb::ContextDestroy,
            bytes_of(&cmd),
            &mut response,
            mode,
        );
        return match adapter.control_owner().finish_context(observed)? {
            ContextFinishEffect::DestroyCompleted => Ok(()),
            ContextFinishEffect::DestroyDefiniteNotEnqueued(error) => Err(error),
            ContextFinishEffect::DestroyAmbiguous(reason) => Err(owner_abandon_error(reason)),
            ContextFinishEffect::DestroyHostRejected(_)
            | ContextFinishEffect::CreateDefiniteNotEnqueued(_)
            | ContextFinishEffect::CreateCompleted
            | ContextFinishEffect::CreateHostRejected(_)
            | ContextFinishEffect::CreateAmbiguous(_) => Err(VirtioError::DeviceError),
        };
    }
    let owned = adapter
        .with_virtio(|v| v.untrack_owned_context(owner, ctx_id))
        .map_err(|_| VirtioError::DeviceError)?;
    let Some(ctx_id) = owned else {
        return Err(VirtioError::NotOwned);
    };
    let mut cmd = VirtioGpuCtxDestroy::zeroed();
    cmd.hdr.type_ = VIRTIO_GPU_CMD_CTX_DESTROY;
    cmd.hdr.ctx_id = ctx_id;
    ctrl_roundtrip_ok_mode(passive, adapter, bytes_of(&cmd), None, None, mode)
}

/// Untracked teardown of a context this driver created for itself (the
/// persistent venus context, the virgl diagnostic contexts). Owner-scoped to
/// the KMD.
pub fn ctx_destroy_kmd(
    passive: PassiveLevel,
    adapter: &AdapterContext,
    ctx_id: u32,
) -> Result<(), VirtioError> {
    ctx_destroy(passive, adapter, None, ctx_id)
}

/// `CTX_DESTROY` every context still owned by `owner` (device teardown).
pub fn destroy_contexts_for_owner(
    passive: PassiveLevel,
    adapter: &AdapterContext,
    owner: Option<DeviceOwner>,
) -> u32 {
    if super::control_owner::KMD_D2_OWNER_ENABLED {
        let mut destroyed = 0u32;
        while let Some(ctx_id) = adapter.control_owner().first_context_for_owner(owner) {
            if ctx_destroy(passive, adapter, owner, ctx_id).is_err() {
                break;
            }
            destroyed = destroyed.saturating_add(1);
        }
        return destroyed;
    }
    let mut destroyed = 0u32;
    loop {
        let taken = adapter
            .with_virtio(|v| v.take_context_for_owner(owner))
            .unwrap_or(None);
        let Some(ctx_id) = taken else {
            break;
        };
        let mut cmd = VirtioGpuCtxDestroy::zeroed();
        cmd.hdr.type_ = VIRTIO_GPU_CMD_CTX_DESTROY;
        cmd.hdr.ctx_id = ctx_id;
        let _ = ctrl_roundtrip_ok(passive, adapter, bytes_of(&cmd), None);
        destroyed += 1;
    }
    destroyed
}

// ── Resource / blob lifecycle ────────────────────────────────────────────────

/// Attach a resource to a 3D context (`CTX_ATTACH_RESOURCE`).
pub fn ctx_attach_resource(
    passive: PassiveLevel,
    adapter: &AdapterContext,
    ctx_id: u32,
    resource_id: u32,
) -> Result<(), VirtioError> {
    ctx_attach_resource_mode(
        passive,
        adapter,
        ctx_id,
        resource_id,
        CtrlRoundtripMode::LegacyRetry,
    )
}

fn ctx_attach_resource_mode(
    passive: PassiveLevel,
    adapter: &AdapterContext,
    ctx_id: u32,
    resource_id: u32,
    mode: CtrlRoundtripMode,
) -> Result<(), VirtioError> {
    if super::control_owner::KMD_D2_OWNER_ENABLED {
        let work = adapter
            .control_owner()
            .begin_secondary_attach(resource_id, ctx_id)?;
        let mut cmd = VirtioGpuCtxResource::zeroed();
        cmd.hdr.type_ = VIRTIO_GPU_CMD_CTX_ATTACH_RESOURCE;
        cmd.hdr.ctx_id = ctx_id;
        cmd.resource_id = resource_id;
        let mut response = [0u8; size_of::<VirtioGpuCtrlHdr>()];
        let observed = run_owner_work(
            passive,
            adapter,
            work,
            ControlVerb::Attach,
            bytes_of(&cmd),
            &mut response,
            mode,
        );
        return match adapter.control_owner().finish_pair(observed)? {
            AttachmentFinishEffect::AttachCompleted => Ok(()),
            AttachmentFinishEffect::AttachDefiniteNotEnqueued(error) => Err(error),
            AttachmentFinishEffect::AttachAmbiguous(reason) => Err(owner_abandon_error(reason)),
            AttachmentFinishEffect::AttachHostRejected(_)
            | AttachmentFinishEffect::DetachDefiniteNotEnqueued(_)
            | AttachmentFinishEffect::DetachCompleted
            | AttachmentFinishEffect::DetachHostRejected(_)
            | AttachmentFinishEffect::DetachAmbiguous(_) => Err(VirtioError::DeviceError),
        };
    }
    let mut cmd = VirtioGpuCtxResource::zeroed();
    cmd.hdr.type_ = VIRTIO_GPU_CMD_CTX_ATTACH_RESOURCE;
    cmd.hdr.ctx_id = ctx_id;
    cmd.resource_id = resource_id;
    ctrl_roundtrip_ok_mode(passive, adapter, bytes_of(&cmd), None, None, mode)
}

/// Detach a resource from a 3D context.
pub fn ctx_detach_resource(
    passive: PassiveLevel,
    adapter: &AdapterContext,
    ctx_id: u32,
    resource_id: u32,
) -> Result<(), VirtioError> {
    ctx_detach_resource_mode(
        passive,
        adapter,
        ctx_id,
        resource_id,
        CtrlRoundtripMode::LegacyRetry,
    )
}

pub(crate) fn ctx_detach_session_resource(
    passive: PassiveLevel,
    adapter: &AdapterContext,
    ctx_id: u32,
    resource_id: u32,
) -> Result<(), VirtioError> {
    ctx_detach_resource_mode(
        passive,
        adapter,
        ctx_id,
        resource_id,
        CtrlRoundtripMode::FiniteEvent,
    )
}

fn ctx_detach_resource_mode(
    passive: PassiveLevel,
    adapter: &AdapterContext,
    ctx_id: u32,
    resource_id: u32,
    mode: CtrlRoundtripMode,
) -> Result<(), VirtioError> {
    if super::control_owner::KMD_D2_OWNER_ENABLED {
        let mut cmd = VirtioGpuCtxResource::zeroed();
        cmd.hdr.type_ = VIRTIO_GPU_CMD_CTX_DETACH_RESOURCE;
        cmd.hdr.ctx_id = ctx_id;
        cmd.resource_id = resource_id;
        let mut response = [0u8; size_of::<VirtioGpuCtrlHdr>()];
        return match adapter.control_owner().pair_kind(resource_id, ctx_id)? {
            PairKind::Creator => {
                let work = adapter.control_owner().begin_creator_detach(resource_id)?;
                let observed = run_owner_work(
                    passive,
                    adapter,
                    work,
                    ControlVerb::Detach,
                    bytes_of(&cmd),
                    &mut response,
                    mode,
                );
                match adapter
                    .control_owner()
                    .finish_resource(observed, retain_resource_finalizer)?
                {
                    ResourceFinishEffect::DetachCompleted => Ok(()),
                    ResourceFinishEffect::DetachDefiniteNotEnqueued(error) => Err(error),
                    ResourceFinishEffect::DetachAmbiguous(reason) => {
                        Err(owner_abandon_error(reason))
                    }
                    _ => Err(VirtioError::DeviceError),
                }
            }
            PairKind::Secondary => {
                let work = adapter
                    .control_owner()
                    .begin_secondary_detach(resource_id, ctx_id)?;
                let observed = run_owner_work(
                    passive,
                    adapter,
                    work,
                    ControlVerb::Detach,
                    bytes_of(&cmd),
                    &mut response,
                    mode,
                );
                match adapter.control_owner().finish_pair(observed)? {
                    AttachmentFinishEffect::DetachCompleted => Ok(()),
                    AttachmentFinishEffect::DetachDefiniteNotEnqueued(error) => Err(error),
                    AttachmentFinishEffect::DetachAmbiguous(reason) => {
                        Err(owner_abandon_error(reason))
                    }
                    _ => Err(VirtioError::DeviceError),
                }
            }
        };
    }
    let mut cmd = VirtioGpuCtxResource::zeroed();
    cmd.hdr.type_ = VIRTIO_GPU_CMD_CTX_DETACH_RESOURCE;
    cmd.hdr.ctx_id = ctx_id;
    cmd.resource_id = resource_id;
    ctrl_roundtrip_ok_mode(passive, adapter, bytes_of(&cmd), None, None, mode)
}

static FENCED_SCANOUT_RESPONSE_REFUSALS: AtomicU32 = AtomicU32::new(0);

fn record_fenced_scanout_response_refusal(code: u32) {
    let count = FENCED_SCANOUT_RESPONSE_REFUSALS.fetch_add(1, Ordering::Relaxed) + 1;
    if count == 1 || count % 64 == 0 {
        crate::diag::record_named_bytes(b"D2SetRef", (code << 24) | count.min(0x00ff_ffff));
    }
}

/// Issue one standard fenced `SET_SCANOUT_BLOB` for the D2 plane.
///
/// The transport mints both values only after accepting this command's exact
/// descriptor: a globally unique nonzero wire fence and the FIFO binding
/// sequence. A response is terminal only when the used-ring ticket selected
/// this entry, the exact header length was written, and the standard fence
/// flag/id plus global-command zero fields all echo this mint. The resource is
/// request-bound in the in-flight tag (virtio-gpu replies do not echo it).
pub(crate) fn set_scanout_blob_fenced(
    passive: PassiveLevel,
    adapter: &AdapterContext,
    resource_id: u32,
    width: u32,
    height: u32,
    format: u32,
    stride: u32,
    offset: u32,
    expected_instance: u64,
    on_publish: &dyn Fn(FencedScanoutPublish),
) -> FencedScanoutSetOutcome {
    if !super::control_owner::KMD_D2_OWNER_ENABLED {
        return FencedScanoutSetOutcome::DefiniteNotEnqueued {
            error: VirtioError::DeviceError,
            instance: 0,
        };
    }
    let mut cmd = VirtioGpuSetScanoutBlob::zeroed();
    fill_set_scanout_blob(&mut cmd, resource_id, width, height, format, stride, offset);
    let seq = Cell::new(0u64);
    let instance = Cell::new(0u64);
    let fence_id = Cell::new(0u64);
    let bind = BindMint {
        seq_out: &seq,
        instance_out: &instance,
        fence_out: &fence_id,
        resource_id,
        fenced: true,
        publish: Some(on_publish),
    };
    let mut response = [0u8; size_of::<VirtioGpuCtrlHdr>()];
    let observed = ctrl_roundtrip_observed(
        passive,
        adapter,
        bytes_of(&cmd),
        None,
        &mut response,
        SYNC_ROUNDTRIP_TIMEOUT_MS,
        Some(bind),
        Some(expected_instance),
        CtrlRoundtripMode::LegacyRetry,
    );
    let identity = || {
        (instance.get() != 0 && seq.get() != 0 && fence_id.get() != 0).then_some(
            ScanoutBindIdentity {
                instance: instance.get(),
                sequence: seq.get(),
                fence_id: fence_id.get(),
                resource_id,
            },
        )
    };
    match observed {
        CtrlRoundtripOutcome::DefiniteNotEnqueued(error) => {
            FencedScanoutSetOutcome::DefiniteNotEnqueued {
                error,
                instance: instance.get(),
            }
        }
        CtrlRoundtripOutcome::Ambiguous(_) => {
            record_fenced_scanout_response_refusal(1);
            FencedScanoutSetOutcome::Ambiguous(identity())
        }
        CtrlRoundtripOutcome::HostResponseCopied { written_length }
            if written_length as usize == response.len() =>
        {
            // SAFETY: the exact observed length covers the complete response
            // array; the byte array itself carries no alignment guarantee.
            let header =
                unsafe { core::ptr::read_unaligned(response.as_ptr().cast::<VirtioGpuCtrlHdr>()) };
            let Some(identity) = identity() else {
                record_fenced_scanout_response_refusal(2);
                return FencedScanoutSetOutcome::Ambiguous(None);
            };
            let exact = header.flags == VIRTIO_GPU_FLAG_FENCE
                && header.fence_id == identity.fence_id()
                && header.ctx_id == 0
                && header.ring_idx == 0
                && header.padding == [0; 3]
                && identity.instance() == expected_instance
                && identity.resource_id() == resource_id;
            if !exact {
                record_fenced_scanout_response_refusal(3);
                return FencedScanoutSetOutcome::Ambiguous(Some(identity));
            }
            if header.type_ == helios_protocol::VIRTIO_GPU_RESP_OK_NODATA {
                FencedScanoutSetOutcome::Accepted(identity)
            } else if HostRejection::from_response_type(header.type_).is_ok() {
                FencedScanoutSetOutcome::Rejected(identity)
            } else {
                record_fenced_scanout_response_refusal(4);
                FencedScanoutSetOutcome::Ambiguous(Some(identity))
            }
        }
        CtrlRoundtripOutcome::HostResponseCopied { .. } => {
            record_fenced_scanout_response_refusal(5);
            FencedScanoutSetOutcome::Ambiguous(identity())
        }
    }
}

/// Encode one `SET_SCANOUT_BLOB` into `cmd`, whoever owns the storage.
///
/// THE ONE ENCODER. Its two callers are the synchronous round-trip above, which
/// stages the command on its PASSIVE stack, and the DISPATCH-level fast bind,
/// which writes it straight into the transport's preallocated DMA buffer — so
/// the two commands are byte-identical by construction rather than by two
/// copies of the same twelve field assignments.
///
/// Every field is written, including the zeros: `cmd` may be a recycled buffer
/// whose previous contents are a different bind, and a stale `strides[1]` would
/// be read by QEMU as a real plane.
///
/// IRQL-free (plain field stores, no allocation, no round-trip), which is why it
/// takes no [`PassiveLevel`] unlike everything else in this module.
pub(crate) fn fill_set_scanout_blob(
    cmd: &mut VirtioGpuSetScanoutBlob,
    resource_id: u32,
    width: u32,
    height: u32,
    format: u32,
    stride: u32,
    offset: u32,
) {
    cmd.hdr = VirtioGpuCtrlHdr::zeroed();
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
    cmd.padding = 0;
    cmd.strides = [stride, 0, 0, 0];
    cmd.offsets = [offset, 0, 0, 0];
}

/// Drop the host's reference to a resource.
pub fn resource_unref(
    passive: PassiveLevel,
    adapter: &AdapterContext,
    resource_id: u32,
) -> Result<(), VirtioError> {
    let mut finalize = |finalizer| finalize_resource_backing(passive, adapter, finalizer);
    resource_unref_with_finalizer(passive, adapter, resource_id, &mut finalize)
}

fn resource_unref_with_finalizer<F>(
    passive: PassiveLevel,
    adapter: &AdapterContext,
    resource_id: u32,
    finalize: &mut F,
) -> Result<(), VirtioError>
where
    F: FnMut(ResourceBackingFinalizer) -> Result<(), ResourceBackingFinalizer>,
{
    resource_unref_with_finalizer_mode(
        passive,
        adapter,
        resource_id,
        finalize,
        CtrlRoundtripMode::LegacyRetry,
    )
}

fn resource_unref_with_finalizer_mode<F>(
    passive: PassiveLevel,
    adapter: &AdapterContext,
    resource_id: u32,
    finalize: &mut F,
    mode: CtrlRoundtripMode,
) -> Result<(), VirtioError>
where
    F: FnMut(ResourceBackingFinalizer) -> Result<(), ResourceBackingFinalizer>,
{
    if super::control_owner::KMD_D2_OWNER_ENABLED {
        let work = adapter.control_owner().begin_resource_unref(resource_id)?;
        let mut cmd = VirtioGpuResourceUnref::zeroed();
        cmd.hdr.type_ = VIRTIO_GPU_CMD_RESOURCE_UNREF;
        cmd.resource_id = resource_id;
        let mut response = [0u8; size_of::<VirtioGpuCtrlHdr>()];
        let observed = run_owner_work(
            passive,
            adapter,
            work,
            ControlVerb::Unref,
            bytes_of(&cmd),
            &mut response,
            mode,
        );
        return match adapter
            .control_owner()
            .finish_resource(observed, |finalizer| finalize(finalizer))?
        {
            ResourceFinishEffect::UnrefCompleted => Ok(()),
            ResourceFinishEffect::UnrefDefiniteNotEnqueued(error) => Err(error),
            ResourceFinishEffect::UnrefAmbiguous(reason) => Err(owner_abandon_error(reason)),
            _ => Err(VirtioError::DeviceError),
        };
    }
    let mut cmd = VirtioGpuResourceUnref::zeroed();
    cmd.hdr.type_ = VIRTIO_GPU_CMD_RESOURCE_UNREF;
    cmd.resource_id = resource_id;
    ctrl_roundtrip_ok_mode(passive, adapter, bytes_of(&cmd), None, None, mode)
}

/// Release K11's exact private reply resource with the finite-event lifecycle.
/// The owner/context tuple is checked in the canonical table before UNREF; a
/// scalar resource id alone is never authority for session teardown.
pub(crate) fn resource_unref_session_reply(
    passive: PassiveLevel,
    adapter: &AdapterContext,
    owner: DeviceOwner,
    context_id: u32,
    resource_id: u32,
) -> Result<(), VirtioError> {
    if !super::control_owner::KMD_D2_OWNER_ENABLED
        || !adapter
            .control_owner()
            .resource_owned_by(Some(owner), context_id, resource_id)
    {
        return Err(VirtioError::NotOwned);
    }
    let mut finalize = retain_resource_finalizer;
    resource_unref_with_finalizer_mode(
        passive,
        adapter,
        resource_id,
        &mut finalize,
        CtrlRoundtripMode::FiniteEvent,
    )
}

/// Create a HOST3D virtio-gpu blob resource in venus context `ctx_id`,
/// referencing venus device-memory `blob_id`, and attach it to the context.
/// Returns the guest-assigned resource id. Mirrors the proven System-class
/// `kmd::alloc_blob` sequence (create_blob → ctx_attach_resource); the
/// live-resource table slot is reserved up front so an untracked-but-live
/// resource can never exist.
pub fn resource_create_blob(
    passive: PassiveLevel,
    adapter: &AdapterContext,
    ctx_id: u32,
    blob_mem: u32,
    blob_flags: u32,
    blob_id: u64,
    size: u64,
) -> Result<u32, VirtioError> {
    let mut finalize = retain_resource_finalizer;
    resource_create_blob_owned(
        passive,
        adapter,
        ctx_id,
        blob_mem,
        blob_flags,
        blob_id,
        size,
        &[],
        None,
        ResourceBackingFinalizer::none(),
        &mut finalize,
    )
}

/// Transfer one KMD-created Venus backing into canonical resource custody
/// before CREATE reaches the wire. `finalize` runs outside the owner lock and
/// returns the exact unfinished tail on ambiguity.
pub(crate) fn resource_create_blob_with_finalizer<F>(
    passive: PassiveLevel,
    adapter: &AdapterContext,
    ctx_id: u32,
    blob_mem: u32,
    blob_flags: u32,
    blob_id: u64,
    size: u64,
    finalizer: ResourceBackingFinalizer,
    mut finalize: F,
) -> Result<u32, VirtioError>
where
    F: FnMut(ResourceBackingFinalizer) -> Result<(), ResourceBackingFinalizer>,
{
    resource_create_blob_owned(
        passive,
        adapter,
        ctx_id,
        blob_mem,
        blob_flags,
        blob_id,
        size,
        &[],
        None,
        finalizer,
        &mut finalize,
    )
}

/// Create and attach a blob whose exact backing is the supplied guest PFN
/// ranges. The locked MDL is transferred into canonical owner custody before
/// CREATE can reach the host.
pub(crate) fn resource_create_guest_blob(
    passive: PassiveLevel,
    adapter: &AdapterContext,
    ctx_id: u32,
    blob_flags: u32,
    size: u64,
    entries: &[VirtioGpuMemEntry],
    mdl: usize,
) -> Result<u32, VirtioError> {
    if mdl == 0 {
        return Err(VirtioError::DeviceError);
    }
    if !super::control_owner::KMD_D2_OWNER_ENABLED {
        let _ =
            finalize_resource_backing(passive, adapter, ResourceBackingFinalizer::guest_pages(mdl));
        return Err(VirtioError::DeviceError);
    }
    let mut finalize = |finalizer| finalize_resource_backing(passive, adapter, finalizer);
    resource_create_blob_owned(
        passive,
        adapter,
        ctx_id,
        VIRTIO_GPU_BLOB_MEM_GUEST,
        blob_flags,
        0,
        size,
        entries,
        None,
        ResourceBackingFinalizer::guest_pages(mdl),
        &mut finalize,
    )
}

fn create_blob_request(
    mut cmd: VirtioGpuResourceCreateBlob,
    entries: &[VirtioGpuMemEntry],
) -> Result<Vec<u8>, VirtioError> {
    if cmd.blob_mem == VIRTIO_GPU_BLOB_MEM_GUEST {
        if entries.is_empty() || entries.len() > u32::MAX as usize {
            return Err(VirtioError::DeviceError);
        }
        let mut total = 0u64;
        for entry in entries {
            if entry.addr & 0xFFF != 0
                || entry.length == 0
                || entry.length & 0xFFF != 0
                || entry.padding != 0
            {
                return Err(VirtioError::DeviceError);
            }
            total = total
                .checked_add(entry.length as u64)
                .ok_or(VirtioError::DeviceError)?;
        }
        if total != cmd.size {
            return Err(VirtioError::DeviceError);
        }
    } else if !entries.is_empty() {
        return Err(VirtioError::DeviceError);
    }
    cmd.nr_entries = entries.len() as u32;
    let entry_bytes = cast_slice(entries);
    let capacity = size_of::<VirtioGpuResourceCreateBlob>()
        .checked_add(entry_bytes.len())
        .ok_or(VirtioError::OutOfMemory)?;
    let mut request = Vec::new();
    request
        .try_reserve_exact(capacity)
        .map_err(|_| VirtioError::OutOfMemory)?;
    request.extend_from_slice(bytes_of(&cmd));
    request.extend_from_slice(entry_bytes);
    Ok(request)
}

fn resource_create_blob_owned<F>(
    passive: PassiveLevel,
    adapter: &AdapterContext,
    ctx_id: u32,
    blob_mem: u32,
    blob_flags: u32,
    blob_id: u64,
    size: u64,
    entries: &[VirtioGpuMemEntry],
    owner: Option<DeviceOwner>,
    finalizer: ResourceBackingFinalizer,
    finalize: &mut F,
) -> Result<u32, VirtioError>
where
    F: FnMut(ResourceBackingFinalizer) -> Result<(), ResourceBackingFinalizer>,
{
    resource_create_blob_owned_mode(
        passive,
        adapter,
        ctx_id,
        blob_mem,
        blob_flags,
        blob_id,
        size,
        entries,
        owner,
        finalizer,
        finalize,
        CtrlRoundtripMode::LegacyRetry,
    )
}

fn resource_create_blob_owned_mode<F>(
    passive: PassiveLevel,
    adapter: &AdapterContext,
    ctx_id: u32,
    blob_mem: u32,
    blob_flags: u32,
    blob_id: u64,
    size: u64,
    entries: &[VirtioGpuMemEntry],
    owner: Option<DeviceOwner>,
    finalizer: ResourceBackingFinalizer,
    finalize: &mut F,
    mode: CtrlRoundtripMode,
) -> Result<u32, VirtioError>
where
    F: FnMut(ResourceBackingFinalizer) -> Result<(), ResourceBackingFinalizer>,
{
    let mut cmd = VirtioGpuResourceCreateBlob::zeroed();
    cmd.hdr.type_ = VIRTIO_GPU_CMD_RESOURCE_CREATE_BLOB;
    cmd.hdr.ctx_id = ctx_id;
    cmd.blob_mem = blob_mem;
    cmd.blob_flags = blob_flags;
    cmd.blob_id = blob_id;
    cmd.size = size;
    let request = match create_blob_request(cmd, entries) {
        Ok(request) => request,
        Err(error) => {
            if let Err(remaining) = finalize(finalizer) {
                if super::control_owner::KMD_D2_OWNER_ENABLED {
                    adapter
                        .control_owner()
                        .quarantine_resource_finalizer(remaining);
                }
            }
            return Err(error);
        }
    };
    if super::control_owner::KMD_D2_OWNER_ENABLED {
        let (resource_id, work) = match adapter
            .control_owner()
            .begin_resource_create(size, owner, ctx_id, finalizer)
        {
            Ok(started) => started,
            Err(refused) => {
                let (error, returned) = refused.into_parts();
                if let Some(finalizer) = returned {
                    if let Err(remaining) = finalize(finalizer) {
                        adapter
                            .control_owner()
                            .quarantine_resource_finalizer(remaining);
                    }
                }
                return Err(error);
            }
        };
        let mut request = request;
        let resource_offset = core::mem::offset_of!(VirtioGpuResourceCreateBlob, resource_id);
        request[resource_offset..resource_offset + size_of::<u32>()]
            .copy_from_slice(&resource_id.to_le_bytes());
        let mut response = [0u8; size_of::<VirtioGpuCtrlHdr>()];
        let observed = run_owner_work(
            passive,
            adapter,
            work,
            ControlVerb::Create,
            &request,
            &mut response,
            mode,
        );
        match adapter
            .control_owner()
            .finish_resource(observed, |finalizer| finalize(finalizer))?
        {
            ResourceFinishEffect::CreateCompleted => {}
            ResourceFinishEffect::CreateDefiniteNotEnqueued(error) => return Err(error),
            ResourceFinishEffect::CreateAmbiguous(reason) => {
                return Err(owner_abandon_error(reason));
            }
            ResourceFinishEffect::CreateHostRejected(_) | _ => {
                return Err(VirtioError::DeviceError);
            }
        }

        let work = match adapter
            .control_owner()
            .begin_creator_attach(resource_id, ctx_id)
        {
            Ok(work) => work,
            Err(error) => {
                let _ = resource_unref_with_finalizer_mode(
                    passive,
                    adapter,
                    resource_id,
                    finalize,
                    mode,
                );
                return Err(error);
            }
        };
        let mut attach = VirtioGpuCtxResource::zeroed();
        attach.hdr.type_ = VIRTIO_GPU_CMD_CTX_ATTACH_RESOURCE;
        attach.hdr.ctx_id = ctx_id;
        attach.resource_id = resource_id;
        response.fill(0);
        let observed = run_owner_work(
            passive,
            adapter,
            work,
            ControlVerb::Attach,
            bytes_of(&attach),
            &mut response,
            mode,
        );
        return match adapter
            .control_owner()
            .finish_resource(observed, |finalizer| finalize(finalizer))?
        {
            ResourceFinishEffect::AttachCompleted => Ok(resource_id),
            ResourceFinishEffect::AttachDefiniteNotEnqueued(error) => {
                let _ = resource_unref_with_finalizer_mode(
                    passive,
                    adapter,
                    resource_id,
                    finalize,
                    mode,
                );
                Err(error)
            }
            ResourceFinishEffect::AttachHostRejected(_) => {
                let _ = resource_unref_with_finalizer_mode(
                    passive,
                    adapter,
                    resource_id,
                    finalize,
                    mode,
                );
                Err(VirtioError::DeviceError)
            }
            ResourceFinishEffect::AttachAmbiguous(reason) => Err(owner_abandon_error(reason)),
            _ => Err(VirtioError::DeviceError),
        };
    }
    let reserved = adapter
        .with_virtio(|v| v.reserve_resource_slot())
        .map_err(|_| VirtioError::DeviceError)?;
    if !reserved {
        return Err(VirtioError::OutOfMemory);
    }
    let resource_id = match adapter.with_virtio(|v| v.alloc_resource_id()) {
        Ok(id) => id,
        Err(_) => {
            let _ = adapter.with_virtio(|v| v.cancel_resource_reservation());
            return Err(VirtioError::DeviceError);
        }
    };
    let mut request = request;
    let resource_offset = core::mem::offset_of!(VirtioGpuResourceCreateBlob, resource_id);
    request[resource_offset..resource_offset + size_of::<u32>()]
        .copy_from_slice(&resource_id.to_le_bytes());
    if let Err(e) = ctrl_roundtrip_ok_mode(passive, adapter, &request, None, None, mode) {
        let _ = adapter.with_virtio(|v| v.cancel_resource_reservation());
        return Err(e);
    }
    if let Err(e) = ctx_attach_resource_mode(passive, adapter, ctx_id, resource_id, mode) {
        // The resource exists host-side but could not attach: drop it so it
        // does not leak untracked.
        let mut finalize = retain_resource_finalizer;
        let _ =
            resource_unref_with_finalizer_mode(passive, adapter, resource_id, &mut finalize, mode);
        let _ = adapter.with_virtio(|v| v.cancel_resource_reservation());
        return Err(e);
    }
    let _ = adapter.with_virtio(|v| v.commit_resource(resource_id));
    Ok(resource_id)
}

/// Create K11's sole private reply target in the exact session context.
/// HOST3D plus MAPPABLE is the stock virtio-gpu/Venus SHM resource shape; it is
/// never shared, exported, returned through an ABI, or used as a UMD carrier.
pub(crate) fn resource_create_session_reply_blob(
    passive: PassiveLevel,
    adapter: &AdapterContext,
    context_id: u32,
    owner: DeviceOwner,
    size: u64,
) -> Result<u32, VirtioError> {
    if !super::control_owner::KMD_D2_OWNER_ENABLED || size == 0 {
        return Err(VirtioError::DeviceError);
    }
    let mut finalize = retain_resource_finalizer;
    resource_create_blob_owned_mode(
        passive,
        adapter,
        context_id,
        VIRTIO_GPU_BLOB_MEM_HOST3D,
        VIRTIO_GPU_BLOB_FLAG_USE_MAPPABLE,
        0,
        size,
        &[],
        Some(owner),
        ResourceBackingFinalizer::none(),
        &mut finalize,
        CtrlRoundtripMode::FiniteEvent,
    )
}

fn resource_map_blob_owner_work(
    passive: PassiveLevel,
    adapter: &AdapterContext,
    resource_id: u32,
    offset: u64,
    work: DispatchWork<helios_kmd_logic::control_owner_slots::WindowSlotKind>,
    mode: CtrlRoundtripMode,
) -> Result<u32, VirtioError> {
    if !super::control_owner::KMD_D2_OWNER_ENABLED {
        return Err(VirtioError::DeviceError);
    }
    let mut cmd = VirtioGpuResourceMapBlob::zeroed();
    cmd.hdr.type_ = VIRTIO_GPU_CMD_RESOURCE_MAP_BLOB;
    cmd.resource_id = resource_id;
    cmd.offset = offset;
    let mut response = [0u8; size_of::<VirtioGpuRespMapInfo>()];
    let observed = run_owner_work(
        passive,
        adapter,
        work,
        ControlVerb::Map,
        bytes_of(&cmd),
        &mut response,
        mode,
    );
    match adapter.control_owner().finish_window(observed)? {
        WindowFinishEffect::MapCompleted => adapter
            .control_owner()
            .mapped_blob(resource_id)?
            .map(|mapped| mapped.map_cache & VIRTIO_GPU_MAP_CACHE_MASK)
            .ok_or(VirtioError::DeviceError),
        WindowFinishEffect::MapDefiniteNotEnqueued(error) => Err(error),
        WindowFinishEffect::MapAmbiguous(reason) => Err(owner_abandon_error(reason)),
        _ => Err(VirtioError::DeviceError),
    }
}

/// `RESOURCE_MAP_BLOB` round-trip; returns the host caching nibble.
fn resource_map_blob_roundtrip(
    passive: PassiveLevel,
    adapter: &AdapterContext,
    resource_id: u32,
    offset: u64,
) -> Result<u32, VirtioError> {
    if super::control_owner::KMD_D2_OWNER_ENABLED {
        let work = adapter
            .control_owner()
            .begin_window_map(resource_id, offset)?;
        return resource_map_blob_owner_work(
            passive,
            adapter,
            resource_id,
            offset,
            work,
            CtrlRoundtripMode::LegacyRetry,
        );
    }
    let mut cmd = VirtioGpuResourceMapBlob::zeroed();
    cmd.hdr.type_ = VIRTIO_GPU_CMD_RESOURCE_MAP_BLOB;
    cmd.resource_id = resource_id;
    cmd.offset = offset;
    let mut resp = [0u8; size_of::<VirtioGpuRespMapInfo>()];
    let _written_length = ctrl_roundtrip(
        passive,
        adapter,
        bytes_of(&cmd),
        None,
        &mut resp,
        SYNC_ROUNDTRIP_TIMEOUT_MS,
        None,
        None,
    )?;
    let resp_type = u32::from_le_bytes([resp[0], resp[1], resp[2], resp[3]]);
    // As above, MAP keeps its legacy broad-OK classification until its exact
    // WindowLifecycle can quarantine the reserved range on malformed success.
    if !resp_is_ok(resp_type) {
        return Err(VirtioError::DeviceError);
    }
    // VirtioGpuRespMapInfo = { hdr: VirtioGpuCtrlHdr, map_info: u32, .. }.
    let off = size_of::<VirtioGpuCtrlHdr>();
    let map_info = u32::from_le_bytes([resp[off], resp[off + 1], resp[off + 2], resp[off + 3]]);
    Ok(map_info & VIRTIO_GPU_MAP_CACHE_MASK)
}

/// Tear down a blob's host-visible mapping.
pub fn resource_unmap_blob(
    passive: PassiveLevel,
    adapter: &AdapterContext,
    resource_id: u32,
) -> Result<(), VirtioError> {
    resource_unmap_blob_mode(
        passive,
        adapter,
        resource_id,
        CtrlRoundtripMode::LegacyRetry,
    )
}

fn resource_unmap_blob_mode(
    passive: PassiveLevel,
    adapter: &AdapterContext,
    resource_id: u32,
    mode: CtrlRoundtripMode,
) -> Result<(), VirtioError> {
    if super::control_owner::KMD_D2_OWNER_ENABLED {
        let work = adapter.control_owner().begin_window_unmap(resource_id)?;
        let mut cmd = VirtioGpuResourceUnmapBlob::zeroed();
        cmd.hdr.type_ = VIRTIO_GPU_CMD_RESOURCE_UNMAP_BLOB;
        cmd.resource_id = resource_id;
        let mut response = [0u8; size_of::<VirtioGpuCtrlHdr>()];
        let observed = run_owner_work(
            passive,
            adapter,
            work,
            ControlVerb::Unmap,
            bytes_of(&cmd),
            &mut response,
            mode,
        );
        return match adapter.control_owner().finish_window(observed)? {
            WindowFinishEffect::UnmapCompleted => Ok(()),
            WindowFinishEffect::UnmapDefiniteNotEnqueued(error) => Err(error),
            WindowFinishEffect::UnmapAmbiguous(reason) => Err(owner_abandon_error(reason)),
            _ => Err(VirtioError::DeviceError),
        };
    }
    let mut cmd = VirtioGpuResourceUnmapBlob::zeroed();
    cmd.hdr.type_ = VIRTIO_GPU_CMD_RESOURCE_UNMAP_BLOB;
    cmd.resource_id = resource_id;
    ctrl_roundtrip_ok_mode(passive, adapter, bytes_of(&cmd), None, None, mode)
}

/// Map a blob into the host-visible window (idempotent — returns the existing
/// mapping if present). `owner = Some(o)` is the owner-scoped escape path;
/// `None` resolves by resource id alone (the GDI executor / kernel path).
pub fn map_blob_prepare(
    passive: PassiveLevel,
    adapter: &AdapterContext,
    owner: OwnerFilter,
    resource_id: u32,
) -> Result<BlobMapPrep, VirtioError> {
    if super::control_owner::KMD_D2_OWNER_ENABLED {
        if !adapter
            .control_owner()
            .resource_matches_filter(owner, resource_id)
        {
            return Err(VirtioError::NotOwned);
        }
        if let Some(mapped) = adapter.control_owner().mapped_blob(resource_id)? {
            return Ok(mapped);
        }
        let (offset, work) = adapter
            .control_owner()
            .begin_window_map_first_fit(resource_id)?;
        let _ = resource_map_blob_owner_work(
            passive,
            adapter,
            resource_id,
            offset,
            work,
            CtrlRoundtripMode::LegacyRetry,
        )?;
        return adapter
            .control_owner()
            .mapped_blob(resource_id)?
            .ok_or(VirtioError::DeviceError);
    }
    let mut busy = Budget::new(MAP_BUSY_MAX_MS);
    loop {
        let begin = adapter
            .with_virtio(|v| v.blob_map_begin(owner, resource_id))
            .map_err(|_| VirtioError::DeviceError)?;
        match begin {
            BlobMapBegin::Mapped(prep) => return Ok(prep),
            BlobMapBegin::Failed(e) => return Err(e),
            BlobMapBegin::Busy => {
                if busy.charge_slice() {
                    return Err(VirtioError::Timeout);
                }
                sleep_ms(passive, RETRY_SLICE_MS);
            }
            BlobMapBegin::Start { offset, len } => {
                let cache = resource_map_blob_roundtrip(passive, adapter, resource_id, offset);
                let cache_ok = cache.as_ref().ok().copied();
                let fin = adapter
                    .with_virtio(|v| v.blob_map_finish(resource_id, offset, len, cache_ok))
                    .map_err(|_| VirtioError::DeviceError)?;
                return match fin {
                    BlobMapFinish::Done(prep) => Ok(prep),
                    BlobMapFinish::HostRejected => {
                        Err(cache.err().unwrap_or(VirtioError::DeviceError))
                    }
                    BlobMapFinish::SlotGone => {
                        // Owner teardown raced the map: undo the host mapping
                        // and return the reserved range.
                        let _ = resource_unmap_blob(passive, adapter, resource_id);
                        let _ = adapter.with_virtio(|v| v.free_window_range_pub(offset, len));
                        Err(VirtioError::DeviceError)
                    }
                };
            }
        }
    }
}

/// Map the exact K11 private reply blob once with no retry, polling, or owner
/// discovery.  A pre-existing mapping is rejected because INIT must establish
/// one fresh, bounded resource/map pair before it can publish capacity.
pub(crate) fn map_session_reply_blob(
    passive: PassiveLevel,
    adapter: &AdapterContext,
    owner: DeviceOwner,
    context_id: u32,
    resource_id: u32,
) -> Result<BlobMapPrep, VirtioError> {
    if !super::control_owner::KMD_D2_OWNER_ENABLED
        || !adapter
            .control_owner()
            .resource_owned_by(Some(owner), context_id, resource_id)
    {
        return Err(VirtioError::NotOwned);
    }
    if adapter.control_owner().mapped_blob(resource_id)?.is_some() {
        return Err(VirtioError::DeviceError);
    }
    let (offset, work) = adapter
        .control_owner()
        .begin_window_map_first_fit(resource_id)?;
    let _ = resource_map_blob_owner_work(
        passive,
        adapter,
        resource_id,
        offset,
        work,
        CtrlRoundtripMode::FiniteEvent,
    )?;
    adapter
        .control_owner()
        .mapped_blob(resource_id)?
        .ok_or(VirtioError::DeviceError)
}

/// Unmap K11's exact private reply blob after session rundown has closed.
pub(crate) fn unmap_session_reply_blob(
    passive: PassiveLevel,
    adapter: &AdapterContext,
    owner: DeviceOwner,
    context_id: u32,
    resource_id: u32,
) -> Result<(), VirtioError> {
    if !super::control_owner::KMD_D2_OWNER_ENABLED
        || !adapter
            .control_owner()
            .resource_owned_by(Some(owner), context_id, resource_id)
    {
        return Err(VirtioError::NotOwned);
    }
    resource_unmap_blob_mode(
        passive,
        adapter,
        resource_id,
        CtrlRoundtripMode::FiniteEvent,
    )
}

/// Map a blob at the FIXED window offset VidMm assigned (the CPU-visible BAR
/// memory segment, `build_paging_buffer.rs`). Inverts the normal order: instead
/// of the KMD allocator picking the offset, the blob is placed where VidMm put
/// the allocation, so CPU raster (through the segment's CpuTranslatedAddress),
/// the GDI executor, and the host all address the same bytes.
///
/// A pre-existing mapping at another offset is torn down first (blob content is
/// intrinsic to the host memory object — a remap is content-preserving), and
/// any STALE other-blob mapping overlapping the target range (an eviction this
/// driver missed) is unmapped so host window subregions never overlap.
/// PASSIVE_LEVEL only (host round-trips).
pub fn map_blob_at(
    passive: PassiveLevel,
    adapter: &AdapterContext,
    resource_id: u32,
    window_offset: u64,
) -> Result<BlobMapPrep, VirtioError> {
    if super::control_owner::KMD_D2_OWNER_ENABLED {
        let size = adapter.control_owner().resource_size(resource_id)?;
        let map_len = helios_kmd_logic::round_up_page(size);
        if let Some(current) = adapter.control_owner().mapped_blob_offset(resource_id)? {
            if current == window_offset {
                return adapter
                    .control_owner()
                    .mapped_blob(resource_id)?
                    .ok_or(VirtioError::DeviceError);
            }
            resource_unmap_blob(passive, adapter, resource_id)?;
        }
        while let Some(stale) = adapter.control_owner().first_overlapping_window_resource(
            resource_id,
            window_offset,
            map_len,
        )? {
            resource_unmap_blob(passive, adapter, stale)?;
        }
        let _ = resource_map_blob_roundtrip(passive, adapter, resource_id, window_offset)?;
        return adapter
            .control_owner()
            .mapped_blob(resource_id)?
            .ok_or(VirtioError::DeviceError);
    }
    // Evict stale overlapping placements before reserving our own slot.
    let (_, blob_size, _) = adapter
        .with_virtio(|v| v.blob_lookup(resource_id))
        .map_err(|_| VirtioError::DeviceError)?
        .ok_or(VirtioError::DeviceError)?;
    let map_len = blob_size.saturating_add(4095) & !4095;
    let mut stale = [0u32; 8];
    // Two swallows used to live on this line. `.unwrap_or(0)` turned a torn-down
    // transport into "no stale placements", skipping the eviction pass entirely
    // instead of failing the map; and the scan itself silently stopped recording
    // once `stale` was full, so a ninth overlapping mapping was neither unmapped
    // nor reported and the RESOURCE_MAP_BLOB below created the overlapping host
    // window subregion this pass exists to prevent. Both are now hard failures:
    // refusing the aperture map / paging op is strictly better than two host
    // resources sharing one window subregion (k-gputransport-04).
    let n = adapter
        .with_virtio(|v| v.blobs_overlapping(window_offset, map_len, resource_id, &mut stale))
        .map_err(|_| VirtioError::DeviceError)?
        .map_err(|_truncated| VirtioError::DeviceError)?;
    for &res in stale[..n].iter() {
        let _ = resource_unmap_blob(passive, adapter, res);
        let _ = adapter.with_virtio(|v| v.blob_note_unmapped(res));
    }

    let mut busy = Budget::new(MAP_BUSY_MAX_MS);
    loop {
        let begin = adapter
            .with_virtio(|v| v.blob_remap_begin(resource_id, window_offset))
            .map_err(|_| VirtioError::DeviceError)?;
        match begin {
            BlobRemapBegin::Mapped(prep) => return Ok(prep),
            BlobRemapBegin::Failed(e) => return Err(e),
            BlobRemapBegin::Busy => {
                if busy.charge_slice() {
                    return Err(VirtioError::Timeout);
                }
                sleep_ms(passive, RETRY_SLICE_MS);
            }
            BlobRemapBegin::Start { old, len } => {
                if let Some((old_offset, old_len)) = old {
                    // Content-preserving move: unmap the previous placement and
                    // (for KMD-partition offsets only — the free guard ignores
                    // VidMm-partition ones) return its range.
                    let _ = resource_unmap_blob(passive, adapter, resource_id);
                    let _ = adapter.with_virtio(|v| v.free_window_range_pub(old_offset, old_len));
                }
                let cache =
                    resource_map_blob_roundtrip(passive, adapter, resource_id, window_offset);
                let cache_ok = cache.as_ref().ok().copied();
                let fin = adapter
                    .with_virtio(|v| v.blob_map_finish(resource_id, window_offset, len, cache_ok))
                    .map_err(|_| VirtioError::DeviceError)?;
                return match fin {
                    BlobMapFinish::Done(prep) => Ok(prep),
                    BlobMapFinish::HostRejected => {
                        Err(cache.err().unwrap_or(VirtioError::DeviceError))
                    }
                    BlobMapFinish::SlotGone => {
                        let _ = resource_unmap_blob(passive, adapter, resource_id);
                        Err(VirtioError::DeviceError)
                    }
                };
            }
        }
    }
}

fn release_owner_resource(
    passive: PassiveLevel,
    adapter: &AdapterContext,
    ctx_id: u32,
    resource_id: u32,
) -> Result<(), VirtioError> {
    if !super::control_owner::KMD_D2_OWNER_ENABLED {
        return Err(VirtioError::DeviceError);
    }
    if adapter
        .control_owner()
        .mapped_blob_offset(resource_id)?
        .is_some()
    {
        resource_unmap_blob(passive, adapter, resource_id)?;
    }
    ctx_detach_resource(passive, adapter, ctx_id, resource_id)?;
    resource_unref(passive, adapter, resource_id)?;
    Ok(())
}

/// Reclaim every blob still owned by `owner` (a destroyed D3D device handle):
/// unmap (if mapped), detach, unref, and return the window range. KMD-side
/// safety net for an ICD that crashes or skips RELEASE_BLOB. Returns the count.
pub fn release_blobs_for_owner(
    passive: PassiveLevel,
    adapter: &AdapterContext,
    owner: Option<DeviceOwner>,
) -> u32 {
    if super::control_owner::KMD_D2_OWNER_ENABLED {
        let mut reclaimed = 0u32;
        while let Some((ctx_id, resource_id)) = adapter.control_owner().resource_for_owner(owner) {
            if release_owner_resource(passive, adapter, ctx_id, resource_id).is_err() {
                break;
            }
            reclaimed = reclaimed.saturating_add(1);
        }
        return reclaimed;
    }
    let mut reclaimed = 0u32;
    loop {
        let taken = adapter
            .with_virtio(|v| v.take_blob_for_owner(owner))
            .unwrap_or(None);
        let Some((ctx_id, res, mapped, map_offset, map_len)) = taken else {
            return reclaimed;
        };
        if mapped {
            let _ = resource_unmap_blob(passive, adapter, res);
            let _ = adapter.with_virtio(|v| v.free_window_range_pub(map_offset, map_len));
        }
        let first_teardown = adapter
            .with_virtio(|v| v.take_live_resource(res))
            .unwrap_or(false);
        if first_teardown {
            let _ = ctx_detach_resource(passive, adapter, ctx_id, res);
            let _ = resource_unref(passive, adapter, res);
        }
        reclaimed += 1;
    }
}

/// Drop the KMD-internal (owner-0) blob slot for an allocation at
/// DestroyAllocation time, unmapping the window mapping the GDI executor may
/// have opened. Returns `true` if a live mapping was unmapped here (the caller
/// must not send a second host unmap for the same resource).
pub fn forget_allocation_blob(
    passive: PassiveLevel,
    adapter: &AdapterContext,
    resource_id: u32,
) -> bool {
    if super::control_owner::KMD_D2_OWNER_ENABLED {
        let Ok(mapped) = adapter.control_owner().mapped_blob_offset(resource_id) else {
            return false;
        };
        if mapped.is_some() {
            return resource_unmap_blob(passive, adapter, resource_id).is_ok();
        }
        return false;
    }
    let taken = adapter
        .with_virtio(|v| v.forget_allocation_blob(resource_id))
        .unwrap_or(None);
    let Some((mapped, map_offset, map_len)) = taken else {
        return false;
    };
    if mapped {
        let _ = resource_unmap_blob(passive, adapter, resource_id);
        let _ = adapter.with_virtio(|v| v.free_window_range_pub(map_offset, map_len));
        return true;
    }
    false
}

// ── Venus submission ─────────────────────────────────────────────────────────

/// SYNCHRONOUS venus SUBMIT_3D (in-kernel venus client's direct commands —
/// small ring-bootstrap/notify streams). Blocks at PASSIVE until the device
/// acks the command on the used ring (decode-level; the client's real waits
/// are its ring-head polls). `fence_id` stays 0 (parity with the proven
/// System-class `submit_direct` shape).
pub fn submit_3d_sync(
    passive: PassiveLevel,
    adapter: &AdapterContext,
    ctx_id: u32,
    stream: &[u8],
) -> Result<(), VirtioError> {
    if stream.is_empty() {
        return Err(VirtioError::DeviceError);
    }
    let mut cmd = helios_protocol::VirtioGpuCmdSubmit::zeroed();
    cmd.hdr.type_ = helios_protocol::VIRTIO_GPU_CMD_SUBMIT_3D;
    cmd.hdr.flags = helios_protocol::VIRTIO_GPU_FLAG_FENCE;
    cmd.hdr.ctx_id = ctx_id;
    cmd.size = stream.len() as u32;
    // The stream rides a second device-read descriptor (kept split so the host
    // never mis-parses the submit header as another control command).
    ctrl_roundtrip_ok(passive, adapter, bytes_of(&cmd), Some(stream))
}

pub fn submit_venus_sync(
    passive: PassiveLevel,
    adapter: &AdapterContext,
    ctx_id: u32,
    stream: &[u8],
) -> Result<(), VirtioError> {
    submit_3d_sync(passive, adapter, ctx_id, stream)
}

/// Exact canonical owner-table lease retained across K11's host response read
/// and HVR1 publication. The ids are private diagnostics for the submit helper;
/// callers cannot substitute another pair after admission.
pub(crate) struct VenusSessionGuard<'a> {
    _pair: super::control_owner::PairUseGuard<'a>,
    context_id: u32,
}

pub(crate) fn borrow_venus_session_pair<'a>(
    adapter: &'a AdapterContext,
    owner: super::gpu::DeviceOwner,
    context_id: u32,
    reply_resource_id: u32,
) -> Result<VenusSessionGuard<'a>, VirtioError> {
    let pair = adapter
        .control_owner()
        .borrow_session_pair(owner, reply_resource_id, context_id)?;
    Ok(VenusSessionGuard {
        _pair: pair,
        context_id,
    })
}

/// K11's finite session-local direct submit.
///
/// The ordinary direct helper predates canonical owner rundown and is suitable
/// only for the adapter Venus client, whose mutex/lifecycle is stopped by its
/// own owner.  A live HTS1 session instead borrows the exact role-1
/// resource/context association.  Stop/reset closes that admission and cannot
/// authorize physical reset until this guard returns, so a host decoder can
/// never retain K2a pages past their owner-table lifetime.
pub(crate) fn submit_venus_session_sync(
    passive: PassiveLevel,
    adapter: &AdapterContext,
    session: &VenusSessionGuard<'_>,
    control_fence_id: u64,
    stream: &[u8],
) -> Result<(), VirtioError> {
    if control_fence_id == 0 || stream.is_empty() {
        return Err(VirtioError::DeviceError);
    }
    let Ok(size) = u32::try_from(stream.len()) else {
        return Err(VirtioError::DeviceError);
    };
    let mut cmd = helios_protocol::VirtioGpuCmdSubmit::zeroed();
    cmd.hdr.type_ = helios_protocol::VIRTIO_GPU_CMD_SUBMIT_3D;
    // A bare SUBMIT_3D response proves only that the renderer accepted the
    // bytes for decode.  K11 needs the reply write itself to be terminal, so
    // each of its fixed CPU-control submissions carries a stock per-context
    // fence on ring zero.  This is neither the adapter-global wire timeline nor
    // a WDDM SubmissionFenceId: the newly created Venus context is the fence
    // namespace, ring zero is its decoder/pure-control timeline, and the
    // caller supplies one of that session's finite ordered constants.
    cmd.hdr.flags = VIRTIO_GPU_FLAG_FENCE | VIRTIO_GPU_FLAG_INFO_RING_IDX;
    cmd.hdr.ctx_id = session.context_id;
    cmd.hdr.fence_id = control_fence_id;
    cmd.hdr.ring_idx = 0;
    cmd.size = size;
    ctrl_roundtrip_ok_finite(passive, adapter, bytes_of(&cmd), Some(stream))
}
