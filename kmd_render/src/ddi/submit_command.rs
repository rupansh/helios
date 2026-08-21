//! Command-submission and TDR DDIs.
//!
//! Render work is still disabled, but the scheduler-facing submission path must
//! be able to retire early paging/null-engine DMA buffers without timing out.

use core::ffi::c_void;
use core::mem::size_of;
use core::sync::atomic::{AtomicU32, Ordering};

use crate::adapter::{AdapterContext, WddmNotifyGuard};
use crate::ddi::present_packet::{PresentSubmissionBoundary, PresentSubmissionPrivate};
use crate::dxgk::_DXGK_INTERRUPT_TYPE::DXGK_INTERRUPT_DMA_COMPLETED;
use crate::dxgk::*;

// ── DISPATCH-safe instrumentation (Step-2 coherent-fence bring-up) ───────────
// `dxgkddi_submit_command` runs at DISPATCH_LEVEL (and the render path may too),
// where `diag::record` (PASSIVE-only) is illegal. We trace via these atomics and
// mirror them into the registry ring at DxgkDdiDestroyDevice (see
// `diag_dump_engine_atomics`). The decisive question this answers: does VidSch
// drive SubmitCommand / Render at all before `VidSchTerminateAdapter` fires right
// after CreateContext? (If none of these advance, the engine path is downstream
// of the Code-43 blocker and the real cause is in the caps/context config.)
pub static SUBMIT_COUNT: AtomicU32 = AtomicU32::new(0);
pub static SUBMIT_PAGING_COUNT: AtomicU32 = AtomicU32::new(0);
pub static SUBMIT_LAST_FENCE: AtomicU32 = AtomicU32::new(0);
pub static RENDER_COUNT: AtomicU32 = AtomicU32::new(0);
pub static PATCH_COUNT: AtomicU32 = AtomicU32::new(0);
pub static PREEMPT_COUNT: AtomicU32 = AtomicU32::new(0);
pub static DMA_NOTIFY_COUNT: AtomicU32 = AtomicU32::new(0);
pub static DMA_QUEUE_DPC_COUNT: AtomicU32 = AtomicU32::new(0);
pub static DMA_SYNC_STATUS_LOW: AtomicU32 = AtomicU32::new(0);
pub static DMA_SYNC_RET: AtomicU32 = AtomicU32::new(0);
/// `DXGK_INTERRUPT_DMA_COMPLETED` deliveries that failed and whose fence was
/// therefore put back at the head of the pending FIFO for a later DPC.
///
/// This is the counter that did not exist: `DMA_SYNC_STATUS_LOW`/`DMA_SYNC_RET`
/// are last-value-wins, so a later successful notify erased the only trace that
/// a fence had been lost. Nonzero means the retry path ran; a *rising* value on
/// an otherwise healthy boot means dxgkrnl is repeatedly refusing the
/// synchronized callback.
pub static DMA_NOTIFY_FAILS: AtomicU32 = AtomicU32::new(0);
/// Older DMA_COMPLETED packets suppressed after a newer watermark won the
/// cross-CPU notification race. The newer watermark implicitly retires them.
pub static DMA_STALE_SKIP_COUNT: AtomicU32 = AtomicU32::new(0);

// ── D3D12 ECL submission record (`HeliosD3D12SubmitCmd`) ─────────────────────
// All three are bumped inside `dxgkddi_render`, which the WDK header declares
// `_IRQL_requires_(PASSIVE_LEVEL)`; they are atomics anyway because the registry
// mirror and any future DISPATCH reader must see them without a lock.

/// Valid D3D12 ECL records seen by `dxgkddi_render`.
///
/// The first nonzero value is the proof that `pfnRenderCb` from
/// `pfnExecuteCommandLists` reaches this driver at all — the thing rung 0 of
/// `D12-G8` could not establish, because `EclNoWddmSubmission = 1` meant no
/// packet was ever built.
pub static D3D12_SUBMIT_RECORDS: AtomicU32 = AtomicU32::new(0);
/// Records carrying `gpu_wire_fence == 0` — the documented plumbing arm
/// ("submit the packet, order it against nothing") and the A/B disable for the
/// boundary itself.
///
/// ⚠ GRADED AS EXPECTED-NONZERO DURING BRING-UP. `D12Zero == D12Rec` means the
/// UMD is submitting packets and naming no boundary yet, which is the intended
/// first step, not a defect. It becomes a finding only once the UMD is supposed
/// to be sampling a real ring-1 fence: `D12Zero` climbing then means the ICD
/// handed it nothing.
///
/// ⚠ `EscSubRing` IS THE CROSS-CHECK BUT ONLY AS A DELTA. It is adapter-global and
/// DWM's own present path makes it nonzero, so "`EscSubRing == 0` ⇒ no such fence
/// exists to name" is a test that can never pass. Compare an idle-desktop delta
/// with a probe delta over the same wall-clock window; the procedure is beside the
/// static in `virtio/counters.rs`.
pub static D3D12_SUBMIT_ZERO_FENCE: AtomicU32 = AtomicU32::new(0);
/// Records whose boundary could not be written into this Render's private data
/// (`merge_fence` refused: null pointer, or a private-data buffer shorter than
/// the 32-byte record).
///
/// Must stay 0: `DxgkDdiCreateContext` reports
/// `PRESENT_DMA_PRIVATE_DATA_BYTES = 64` for EVERY context, so
/// a nonzero value means a context this driver did not size, i.e. the packet was
/// submitted with no boundary and the D3D12 fence is reporting decode.
pub static D3D12_SUBMIT_MERGE_FAILS: AtomicU32 = AtomicU32::new(0);
/// D3D12 records that found ANOTHER D3D12 record already in the same private-data
/// buffer, and merged their fences (largest wins).
///
/// This is the counter for the recycled-buffer hazard, and it is the reason the
/// hazard is closed rather than merely counted: `mark_d3d12` writes on every
/// record and `decode` consumes the D3D12 arm, so a predecessor can now only be
/// an ECL batched into the SAME DMA buffer, or a buffer whose earlier submission
/// never reached SubmitCommand (preempted / TDR-abandoned). Both merge
/// conservatively — the larger wire fence names work that really was enqueued.
///
/// ⚠ It is therefore NOT a failure counter, and it is not expected to be 0. What
/// would have been the defect is `D12Rec` moving while a stale boundary survives
/// into a later submission — which is now unrepresentable, and this counter is
/// how you would see the batching that used to make it look benign.
pub static D3D12_SUBMIT_MERGED: AtomicU32 = AtomicU32::new(0);
/// `'HD12'` records cleared by a `DxgkDdiRender` that did **not** write one —
/// the present-identity Render, or any other Render on a D3D12 WDDM context.
///
/// ⛔ **This closes the half of the recycled-buffer hazard that `D12Merged`'s doc
/// above assumed away.** That doc argues a predecessor "can now only be an ECL
/// batched into the SAME DMA buffer, or a buffer whose earlier submission never
/// reached SubmitCommand" — true, and both merge conservatively *for another ECL*.
/// It does not cover a **non-ECL** Render inheriting the record, which UP-9 made
/// possible by adding a second `pfnRenderCb` on the same context that writes
/// nothing at offset 0. `invalidate_d3d12` removes that inheritance.
///
/// ⚠ **NOT a failure counter, and not expected to be 0.** Nonzero means either
/// the hazard was live (a stale record really was inherited before this landed),
/// or — the benign and probably dominant cause — dxgkrnl batched an ECL and a
/// present identity into one DMA buffer, in which case the cleared boundary was
/// correct for that packet and the packet falls back to the conservative
/// `next_wire_fence` prefix. ⛔ The two are not separable by this counter alone;
/// `D12Clr` climbing in lockstep with present frequency is the batching, while
/// `D12Clr` moving on a run with no D3D12 presents at all is the hazard.
///
/// ⚠ It was one of the terms missing from the `D12Exact` "identity" this file's
/// mirror comment used to assert. **That identity is retired** — it could not hold
/// for four other reasons besides this one, and its failure was documented as
/// re-opening a closed defect. `D12Exact` now carries a sound bound and a
/// qualitative A4 signal instead; see `D3D12_EXACT_WATERMARK_USED`.
pub static D3D12_STALE_RECORD_CLEARED: AtomicU32 = AtomicU32::new(0);

/// Mirror the DISPATCH-safe engine tracers into the PASSIVE diag ring. Call ONLY
/// from a PASSIVE DDI (DxgkDdiDestroyDevice). Codes (continuing the 0x0F.. space
/// used by `build_paging_buffer::diag_dump_gpummu_atomics`):
///   0x0F06_NNNN = SubmitCommand call count
///   0x0F07_FFFF = last SubmissionFenceId (low 16)
///   0x0F08_NNNN = paging-submit count (Flags.Paging == 1)
///   0x0F09_NNNN = Render call count
///   0x0F0A_NNNN = Patch call count
///   0x0F0B_NNNN = PreemptCommand call count
///   0x0F0C_NNNN = InterruptRoutine delivery count
///   0x0F0D_NNNN = DpcRoutine count
///   0x0F0E_NNNN = ControlInterrupt count
///   0x0F0F_NNNN = DMA-complete NotifyInterrupt count
///   0x0F10_NNNN = DMA-complete QueueDpc count
///   0x0F11_NNNN = DxgkCbSynchronizeExecution status low 16
///   0x0F12_NNNN = DxgkCbSynchronizeExecution return BOOLEAN
pub fn diag_dump_engine_atomics() {
    crate::diag::record(0x0F06_0000 | (SUBMIT_COUNT.load(Ordering::Relaxed) & 0xFFFF));
    crate::diag::record(0x0F07_0000 | (SUBMIT_LAST_FENCE.load(Ordering::Relaxed) & 0xFFFF));
    crate::diag::record(0x0F08_0000 | (SUBMIT_PAGING_COUNT.load(Ordering::Relaxed) & 0xFFFF));
    crate::diag::record(0x0F09_0000 | (RENDER_COUNT.load(Ordering::Relaxed) & 0xFFFF));
    crate::diag::record(0x0F0A_0000 | (PATCH_COUNT.load(Ordering::Relaxed) & 0xFFFF));
    crate::diag::record(0x0F0B_0000 | (PREEMPT_COUNT.load(Ordering::Relaxed) & 0xFFFF));
    crate::diag::record(
        0x0F0C_0000 | (super::interrupt::INT_ROUTINE_COUNT.load(Ordering::Relaxed) & 0xFFFF),
    );
    crate::diag::record(
        0x0F0D_0000 | (super::interrupt::DPC_ROUTINE_COUNT.load(Ordering::Relaxed) & 0xFFFF),
    );
    crate::diag::record(
        0x0F0E_0000 | (super::interrupt::CONTROL_INT_COUNT.load(Ordering::Relaxed) & 0xFFFF),
    );
    crate::diag::record(0x0F0F_0000 | (DMA_NOTIFY_COUNT.load(Ordering::Relaxed) & 0xFFFF));
    crate::diag::record(0x0F10_0000 | (DMA_QUEUE_DPC_COUNT.load(Ordering::Relaxed) & 0xFFFF));
    crate::diag::record(0x0F11_0000 | (DMA_SYNC_STATUS_LOW.load(Ordering::Relaxed) & 0xFFFF));
    crate::diag::record(0x0F12_0000 | (DMA_SYNC_RET.load(Ordering::Relaxed) & 0xFFFF));
    crate::diag::record(0x0F18_0000 | (DMA_STALE_SKIP_COUNT.load(Ordering::Relaxed) & 0xFFFF));
    // C3/M3.4 async-transport atoms:
    //   0x0F13_NNNN = async SUBMIT_3D enqueues   0x0F14_NNNN = completions
    //   0x0F15_NNNN = WDDM fences completed from the DPC
    //   0x0F16_NNNN = WAIT_FENCE timeouts        0x0F17_NNNN = sync cmd timeouts
    crate::diag::record(
        0x0F13_0000 | (crate::virtio::gpu::ASYNC_SUBMIT_COUNT.load(Ordering::Relaxed) & 0xFFFF),
    );
    crate::diag::record(
        0x0F14_0000 | (crate::virtio::gpu::ASYNC_COMPLETE_COUNT.load(Ordering::Relaxed) & 0xFFFF),
    );
    crate::diag::record(
        0x0F15_0000 | (crate::virtio::gpu::WDDM_FENCE_FROM_DPC.load(Ordering::Relaxed) & 0xFFFF),
    );
    crate::diag::record(
        0x0F16_0000 | (crate::virtio::gpu::FENCE_WAIT_TIMEOUTS.load(Ordering::Relaxed) & 0xFFFF),
    );
    crate::diag::record(
        0x0F17_0000 | (crate::virtio::gpu::CTRL_TIMEOUT_COUNT.load(Ordering::Relaxed) & 0xFFFF),
    );
    crate::adapter::dump_ordered_engine_atomics();
}

/// Context handed to [`notify_at_dirql_routine`] across the
/// `DxgkCbSynchronizeExecution` boundary (it runs at the device's DIRQL).
struct NotifyAtDirqlCtx {
    dxgkrnl: *const DXGKRNL_INTERFACE,
    interrupt: *mut DXGKARGCB_NOTIFY_INTERRUPT_DATA,
    account_legacy: bool,
}

/// Runs at the device's interrupt IRQL (DIRQL), synchronized with the ISR — the
/// only level at which `DxgkCbNotifyInterrupt` may be called. Mirrors viogpu3d's
/// `NotifyRoutine` (`viogpu_adapter.cpp:50-72`).
unsafe extern "C" fn notify_at_dirql_routine(context: *mut c_void) -> BOOLEAN {
    if context.is_null() {
        return 0;
    }
    // SAFETY: `context` is the `NotifyAtDirqlCtx` we passed to
    // DxgkCbSynchronizeExecution; valid for the duration of that synchronous call.
    let ctx = unsafe { &*(context as *const NotifyAtDirqlCtx) };
    let dxgkrnl = unsafe { &*ctx.dxgkrnl };
    if let Some(notify_interrupt) = dxgkrnl.DxgkCbNotifyInterrupt {
        // SAFETY: at DIRQL (raised by DxgkCbSynchronizeExecution); `interrupt`
        // points to a fully-initialized DMA_COMPLETED packet, live for this call.
        unsafe { notify_interrupt(dxgkrnl.DeviceHandle, ctx.interrupt) };
        if ctx.account_legacy {
            DMA_NOTIFY_COUNT.fetch_add(1, Ordering::Relaxed);
        }
    }
    if let Some(queue_dpc) = dxgkrnl.DxgkCbQueueDpc {
        // viogpu3d queues the DPC from the synchronized interrupt routine, while
        // still at the device DIRQL. Keep that ordering so dxgkrnl sees the
        // notify+DPC pair as one interrupt-completion event.
        unsafe { queue_dpc(dxgkrnl.DeviceHandle) };
        if ctx.account_legacy {
            DMA_QUEUE_DPC_COUNT.fetch_add(1, Ordering::Relaxed);
        }
    }
    1 // TRUE
}

/// Deliver a prepared `DXGKARGCB_NOTIFY_INTERRUPT_DATA` packet at the correct
/// IRQL: hand it to `DxgkCbNotifyInterrupt` from inside a
/// `DxgkCbSynchronizeExecution` callback (which raises to the device's DIRQL),
/// then `DxgkCbQueueDpc` so dxgkrnl drains the packet. Callable at <= DIRQL.
pub(crate) unsafe fn notify_at_dirql(
    dxgkrnl: &DXGKRNL_INTERFACE,
    interrupt: &mut DXGKARGCB_NOTIFY_INTERRUPT_DATA,
    account_legacy: bool,
) -> NTSTATUS {
    if dxgkrnl.DxgkCbNotifyInterrupt.is_none() || dxgkrnl.DxgkCbQueueDpc.is_none() {
        return STATUS_DEVICE_NOT_READY;
    }
    let ctx = NotifyAtDirqlCtx {
        dxgkrnl: dxgkrnl as *const DXGKRNL_INTERFACE,
        interrupt: interrupt as *mut DXGKARGCB_NOTIFY_INTERRUPT_DATA,
        account_legacy,
    };

    if let Some(sync) = dxgkrnl.DxgkCbSynchronizeExecution {
        let mut ret: BOOLEAN = 0;
        // SAFETY: valid DeviceHandle; the routine + context live for the call.
        let status = unsafe {
            sync(
                dxgkrnl.DeviceHandle,
                Some(notify_at_dirql_routine),
                &ctx as *const _ as *mut c_void,
                0,
                &mut ret,
            )
        };
        DMA_SYNC_STATUS_LOW.store(status as u32, Ordering::Relaxed);
        DMA_SYNC_RET.store(ret as u32, Ordering::Relaxed);
        if status != STATUS_SUCCESS {
            return status;
        }
        if ret == 0 {
            return STATUS_DEVICE_NOT_READY;
        }
    } else {
        return STATUS_DEVICE_NOT_READY;
    }
    STATUS_SUCCESS
}

/// Signal `DXGK_INTERRUPT_DMA_COMPLETED` for `fence` (see [`notify_at_dirql`]),
/// with the adapter's WDDM notification lock ALREADY HELD -- the only door.
///
/// Queue arbitration and the callback must be one critical section; otherwise a
/// DPC can pop fence N, a concurrent submit can observe an empty FIFO and report
/// N+1 first, and VidSch bugchecks 0x119/1.
///
/// T6/R914 deleted the sibling wrapper that took `&AdapterContext` and acquired
/// the lock itself. It had zero callers and was not re-exported, and the invalid
/// sequence it invited is real: `with_wddm_notify_lock` uses
/// `KeAcquireSpinLockRaiseToDpc` and a `KSPIN_LOCK` is not recursive, so the
/// first caller to reach for it from INSIDE the guard hard-hangs a CPU at
/// DISPATCH. Requiring a `&WddmNotifyGuard` removes the footgun; it does not
/// remove the class, since a hand-written nested `with_wddm_notify_lock` is
/// still writable.
pub(crate) unsafe fn signal_dma_completed(
    guard: &WddmNotifyGuard<'_>,
    dxgkrnl: &DXGKRNL_INTERFACE,
    fence: u32,
) -> NTSTATUS {
    let last = guard.completed_fence();
    // Sequence comparison remains correct across u32 wrap: a forward id is
    // within the next half of the sequence space; equal/backward is stale. The
    // predicate lives in `helios_kmd_logic` so the wrap arithmetic has a host
    // test instead of only this comment.
    let forward = helios_kmd_logic::scanout_lease::fence_is_forward(last, fence);
    if !forward {
        DMA_STALE_SKIP_COUNT.fetch_add(1, Ordering::Relaxed);
        return STATUS_INVALID_PARAMETER;
    }

    let mut interrupt = unsafe { core::mem::zeroed::<DXGKARGCB_NOTIFY_INTERRUPT_DATA>() };
    interrupt.InterruptType = DXGK_INTERRUPT_DMA_COMPLETED;
    // SAFETY: bindgen lowered the per-type union to __BindgenUnionField accessors;
    // DmaCompleted is the correct arm for DXGK_INTERRUPT_DMA_COMPLETED.
    let completed = unsafe { interrupt.__bindgen_anon_1.DmaCompleted.as_mut() };
    completed.SubmissionFenceId = fence;
    completed.NodeOrdinal = 0;
    completed.EngineOrdinal = 0;
    // SAFETY: fully-initialized packet, live for the call.
    let status = unsafe { notify_at_dirql(dxgkrnl, &mut interrupt, true) };
    if status == STATUS_SUCCESS {
        guard.set_completed_fence(fence);
    }
    status
}

/// Synthesize a `DXGK_INTERRUPT_CRTC_VSYNC` for the display half's single target
/// (viogpu3d FlipThread analog, `viogpu_vidpn.cpp:1977-1983`). `physical_address`
/// is the primary currently bound via `SetVidPnSourceAddress` (0 before the first
/// bind); dxgkrnl retires the queued flip whose address matches. `target_id` is the
/// video-present target the VSync belongs to. Callable at <= DIRQL (the DPC path).
pub(crate) unsafe fn signal_crtc_vsync(
    dxgkrnl: &DXGKRNL_INTERFACE,
    physical_address: i64,
    target_id: u32,
) -> NTSTATUS {
    let mut interrupt = unsafe { core::mem::zeroed::<DXGKARGCB_NOTIFY_INTERRUPT_DATA>() };
    interrupt.InterruptType = _DXGK_INTERRUPT_TYPE::DXGK_INTERRUPT_CRTC_VSYNC;
    // SAFETY: CrtcVsync is the correct union arm for DXGK_INTERRUPT_CRTC_VSYNC.
    let vsync = unsafe { interrupt.__bindgen_anon_1.CrtcVsync.as_mut() };
    vsync.VidPnTargetId = target_id;
    vsync.PhysicalAddress.QuadPart = physical_address;
    // SAFETY: fully-initialized packet, live for the call.
    unsafe { notify_at_dirql(dxgkrnl, &mut interrupt, true) }
}

/// Signal `DXGK_INTERRUPT_DMA_PREEMPTED` (see [`notify_at_dirql`]): the node's
/// pending submissions are released back to the scheduler, which resubmits the
/// incomplete ones later.
unsafe fn signal_dma_preempted_locked(
    guard: &WddmNotifyGuard<'_>,
    dxgkrnl: &DXGKRNL_INTERFACE,
    preempt_fence: u32,
) -> NTSTATUS {
    let mut interrupt = unsafe { core::mem::zeroed::<DXGKARGCB_NOTIFY_INTERRUPT_DATA>() };
    interrupt.InterruptType = _DXGK_INTERRUPT_TYPE::DXGK_INTERRUPT_DMA_PREEMPTED;
    // SAFETY: DmaPreempted is the correct arm for DXGK_INTERRUPT_DMA_PREEMPTED.
    let preempted = unsafe { interrupt.__bindgen_anon_1.DmaPreempted.as_mut() };
    preempted.PreemptionFenceId = preempt_fence;
    preempted.LastCompletedFenceId = guard.completed_fence();
    preempted.NodeOrdinal = 0;
    preempted.EngineOrdinal = 0;
    // SAFETY: fully-initialized packet, live for the call.
    unsafe { notify_at_dirql(dxgkrnl, &mut interrupt, true) }
}

/// Common submission handling (C3/M3.4): admit the exact WDDM fence into K9,
/// then record its private ticket behind the Venus work outstanding at submit
/// time. An already-terminal boundary is marked through the same frontier;
/// otherwise the interrupt DPC marks it after the exact producer retires.
/// Transport loss or bounded-owner exhaustion fails the generation closed and
/// never authorizes `DMA_COMPLETED`.
/// The only thing `note_and_maybe_signal` can tell a SubmitCommand DDI.
///
/// This type exists so a transport or notification status physically cannot
/// become the DDI's return value. Both DDIs used to `return
/// note_and_maybe_signal(..)` verbatim, so a failed
/// `DxgkCbSynchronizeExecution` in a stop/rebalance window - the exact failure
/// R209 turns into a retry on the DPC path - was returned to VidSch as
/// STATUS_DEVICE_NOT_READY, and this file's own record says a non-SUCCESS return
/// here bugchecks dxgmms2!VidSchiSendToExecutionQueue with 0x119
/// VIDEO_SCHEDULER_INTERNAL_ERROR Arg1=2. CLAUDE.md's DDI rule says the same
/// thing in general: an illegal NTSTATUS is itself logged by dxgkrnl as a driver
/// bug. A failed notify has to be handled where it can be retried, not escalated.
enum SubmitAck {
    Accepted,
}

/// Retire one K11 DMA whose exact finite host operation and HVR1 publication
/// were already terminal before the scheduler delivered SubmitCommand.
///
/// This is deliberately not routed through `note_wddm_submission`: K11 owns no
/// compatibility Venus boundary and creates no independent timeline. K9 owns
/// the one adapter/engine scheduler frontier; K11 merely marks its exact direct
/// ticket terminal after the HVC1 context-local watermark and actual host reply
/// both validate.
fn complete_k11_host_submission(
    adapter: &AdapterContext,
    ticket: crate::adapter::OrderedEngineTicket,
) {
    let _ = super::interrupt::complete_ordered_engine_submission(adapter, ticket);
}

fn note_and_maybe_signal(
    adapter: &AdapterContext,
    fence: u32,
    is_paging: bool,
    present_submission: Option<PresentSubmissionBoundary>,
    preadmitted: Option<crate::adapter::OrderedEngineTicket>,
) -> SubmitAck {
    let complete_now = adapter.with_wddm_notify_lock(|guard| {
        let ticket = match preadmitted {
            Some(ticket) if guard.ordered_engine_ticket_is_live(ticket) => ticket,
            Some(_) => return None,
            None => guard.admit_ordered_engine_submission(fence)?,
        };
        let admission = guard
            .with_virtio(|o, v| {
                let gpu_completion_fence = present_submission.and_then(|present| {
                    (present.gpu_fence_id != 0).then_some(present.gpu_fence_id)
                });
                // D3D12 selects the exact F21/A4 boundary; ordinary submissions
                // retain their conservative prefix semantics.
                let d3d12 = present_submission.is_some_and(|present| present.d3d12);
                v.note_wddm_submission(
                    o,
                    ticket,
                    is_paging,
                    gpu_completion_fence,
                    d3d12,
                )
            })
            // Transport down is a failed boundary, never proof of completion.
            .unwrap_or(crate::virtio::gpu::WddmAdmission::Failed);
        match admission {
            crate::virtio::gpu::WddmAdmission::HostTerminal => Some(ticket),
            crate::virtio::gpu::WddmAdmission::Pending => None,
            crate::virtio::gpu::WddmAdmission::Failed => {
                let _ = guard.fail_ordered_engine_submission(ticket);
                None
            }
        }
    });
    if let Some(ticket) = complete_now {
        let _ = super::interrupt::complete_ordered_engine_submission(adapter, ticket);
    }
    SubmitAck::Accepted
}

/// The ONE present-marker decoder, shared by both SubmitCommand entry points.
///
/// `kmd_range` is the half of the private data the KMD owns: `umd..total` on the
/// virtual path (dxgkrnl reports a UMD prefix size) and `start..end` on the
/// legacy one (it reports an explicit submission window). Both then fall back to
/// decoding the whole buffer, and finally to the bounded evidence-only scan.
///
/// # Safety
/// `base` must be readable and writable for `total` bytes for the duration of
/// the call.
///
/// THE PRESENT RECORD IS NOT MUTATED, and that rule stands: replay is resolved by
/// exact pending/terminal membership in `VirtioGpu`, because preemption may
/// resubmit that same documented record after the host copy has already
/// terminalized.
///
/// ⚠ THE D3D12 RECORD IS THE EXCEPTION — `PresentSubmissionPrivate::decode`
/// consumes that one arm, which is why `base` is now `*mut`. The reason is the
/// hazard in the other direction: dxgkrnl recycles these buffers, and a D3D12
/// boundary left behind is read again by the next submission that reuses the
/// buffer, gating it on an already-retired fence. The replay cost is acceptable
/// where the Present path's would not have been: a replayed D3D12 packet finds no
/// record and falls back to `next_wire_fence`, i.e. every transport entry
/// enqueued before it — conservative, always eventually satisfied, never a lie.
unsafe fn decode_present_fence(
    base: *mut u8,
    total: usize,
    kmd_range: core::ops::Range<usize>,
) -> Option<PresentSubmissionBoundary> {
    if !base.is_null() && kmd_range.start <= kmd_range.end && kmd_range.end <= total {
        let size = kmd_range.end - kmd_range.start;
        if let Some(fence) = unsafe {
            PresentSubmissionPrivate::decode(base.add(kmd_range.start).cast(), size as u32)
        } {
            return Some(fence);
        }
    }
    if let Some(fence) = unsafe {
        PresentSubmissionPrivate::decode(base.cast(), total.min(u32::MAX as usize) as u32)
    } {
        return Some(fence);
    }
    None
}

/// Pick up a DMA-BUFFER FLIP record from a submission's private data and arm
/// the scan-out programming for it.
///
/// This is the DMA-flip contract's equivalent of `SetVidPnSourceAddress`: for
/// an IMMEDIATE flip dxgkrnl never calls that DDI, so unless the driver
/// programs the display from HERE the scan-out never follows the flip at all
/// (ROADMAP defect 0aa). Runs at DISPATCH. The D4 arm publishes its fixed
/// descriptor synchronously through interrupt serialization; the legacy arm is
/// unreachable while the SURFACE-derived D2 owner is active.
///
/// # Safety
/// `base`/`total` describe the kernel-only DMA private-data buffer dxgkrnl
/// supplied for this submission.
unsafe fn arm_dma_flip(adapter: &AdapterContext, base: *mut c_void, total: u32) {
    let Some((h_alloc, primary_address, primary_segment, operation_flags)) =
        (unsafe { crate::ddi::present_packet::PresentFlipPrivate::take(base, total) })
    else {
        return;
    };
    let _ = unsafe {
        crate::ddi::display::arm_dma_flip_programming(
            adapter,
            h_alloc,
            primary_segment,
            primary_address,
            operation_flags,
        )
    };
}

unsafe fn decode_virtual_present_fence(
    submit: &DXGKARG_SUBMITCOMMANDVIRTUAL,
) -> Option<PresentSubmissionBoundary> {
    let base = submit.pDmaBufferPrivateData.cast::<u8>();
    let total = submit.DmaBufferPrivateDataSize as usize;
    let umd = submit.DmaBufferUmdPrivateDataSize as usize;
    unsafe { decode_present_fence(base, total, umd..total) }
}

unsafe fn decode_legacy_present_fence(
    submit: &DXGKARG_SUBMITCOMMAND,
) -> Option<PresentSubmissionBoundary> {
    let base = submit.pDmaBufferPrivateData.cast::<u8>();
    let total = submit.DmaBufferPrivateDataSize as usize;
    let start = submit.DmaBufferPrivateDataSubmissionStartOffset as usize;
    let end = submit.DmaBufferPrivateDataSubmissionEndOffset as usize;
    unsafe { decode_present_fence(base, total, start..end) }
}

/// `DxgkDdiSubmitCommandVirtual` — submit a DMA buffer addressed by GPU virtual
/// address. Because Helios declares the GpuMmu model (`VirtualAddressingSupported`
/// + `GpuMmuSupported`), VidSch routes a GpuMmu context's command buffers HERE, not
/// to `DxgkDdiSubmitCommand`. Leaving it `STATUS_NOT_SUPPORTED` was fine only while
/// no render work was ever submitted; once `DxgkDdiRenderGdi` produces a real render
/// DMA buffer, `dxgmms2!VidSchiSendToExecutionQueue` submits it here, gets
/// NOT_SUPPORTED (0xC00000BB), and bugchecks **0x119 (VIDEO_SCHEDULER_INTERNAL_ERROR)
/// Arg1=2** ("driver failed upon submission of a command") — observed live.
///
/// There is no guest GPU to program (the host owns the real MMU; venus addresses
/// by resource id — the actual work rides the direct Venus/K9 channel), but since
/// C3/M3.4 the fence is NOT lied about: it queues behind the venus work
/// outstanding at submit time and completes from the interrupt DPC. Runs at
/// DISPATCH_LEVEL.
pub unsafe extern "C" fn dxgkddi_submit_command_virtual(
    h_adapter: *mut c_void,
    submit_command: *const DXGKARG_SUBMITCOMMANDVIRTUAL,
) -> NTSTATUS {
    if h_adapter.is_null() || submit_command.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    let adapter = unsafe { &*(h_adapter as *const AdapterContext) };
    let submit = unsafe { &*submit_command };
    let fence = submit.SubmissionFenceId;

    SUBMIT_COUNT.fetch_add(1, Ordering::Relaxed);
    SUBMIT_LAST_FENCE.store(fence, Ordering::Relaxed);
    // SAFETY: `Value` is a plain UINT view of the (valid) flags union.
    let is_paging = (unsafe { submit.Flags.__bindgen_anon_1.Value } & 1) != 0;
    if is_paging {
        SUBMIT_PAGING_COUNT.fetch_add(1, Ordering::Relaxed);
    }

    // F21: an HQA1 D3D12 context is an exclusive HOS1 submission lane. Admit
    // its exact K9 ticket before the context-owned PASSIVE worker takes HOC1
    // custody; neither a refusal nor an async terminal may fall through to the
    // compatibility FIFO or present-marker decoders below.
    if !submit.hContext.is_null() {
        let context = unsafe { crate::device::ContextHandleRef::from_raw(submit.hContext) };
        if let Some((native, session, device, outer)) =
            context.as_ref().and_then(|c| c.outer_native())
        {
            let _ = adapter.with_k11_completion(|| {
                let Some(ticket) = adapter
                    .with_wddm_notify_lock(|guard| guard.admit_ordered_engine_submission(fence))
                else {
                    return;
                };
                // SAFETY: the role resolution above proves all four direct
                // owners and this submission's private-data/GPUVA pair.
                let disposition = unsafe {
                    crate::ddi::native_render::submit_virtual(
                        native, session, device, outer, submit, ticket,
                    )
                };
                if !matches!(
                    disposition,
                    crate::ddi::native_render::NativeSubmitDisposition::Pending
                ) {
                    let _ = super::interrupt::fail_ordered_engine_submission(adapter, ticket);
                }
            });
            return STATUS_SUCCESS;
        }
    }

    let present_fence = unsafe { decode_virtual_present_fence(submit) };
    // Before the completion bookkeeping: a flip carried in this buffer must be
    // armed while its fence is still outstanding, which is the whole point of
    // the DMA-flip contract. It also mints the presentation epoch and takes the
    // frame boundary this flip carries to its bind.
    unsafe {
        arm_dma_flip(
            adapter,
            submit.pDmaBufferPrivateData,
            submit.DmaBufferPrivateDataSize,
        )
    };
    // The submission is accepted regardless of how the notification went; a
    // non-SUCCESS return here bugchecks dxgmms2 with 0x119 Arg1=2.
    let SubmitAck::Accepted = note_and_maybe_signal(adapter, fence, is_paging, present_fence, None);
    STATUS_SUCCESS
}

/// `DxgkDdiSubmitCommand` — submit a DMA buffer to the GPU. Critically, this is
/// also how Dxgkrnl queues *paging* buffers (built by DxgkDdiBuildPagingBuffer,
/// with `hDevice == NULL`); since we register paging, this slot must be present.
// Runs at DISPATCH_LEVEL. Same C3/M3.4 completion model as SubmitCommandVirtual.
pub unsafe extern "C" fn dxgkddi_submit_command(
    h_adapter: IN_CONST_HANDLE,
    submit_command: IN_CONST_PDXGKARG_SUBMITCOMMAND,
) -> NTSTATUS {
    if h_adapter.is_null() || submit_command.is_null() {
        return STATUS_INVALID_PARAMETER;
    }

    let adapter = unsafe { &*(h_adapter as *const AdapterContext) };
    let submit = unsafe { &*submit_command };
    let fence = submit.SubmissionFenceId;

    SUBMIT_COUNT.fetch_add(1, Ordering::Relaxed);
    SUBMIT_LAST_FENCE.store(fence, Ordering::Relaxed);
    // Flags.Paging is bit 0 of the flags word; read it via the union's `Value`
    // arm (the bitfield accessor lives behind the same union).
    // SAFETY: `Value` is a plain UINT view of the (valid) flags union.
    let is_paging = (unsafe { submit.Flags.__bindgen_anon_1.Value } & 1) != 0;
    if is_paging {
        SUBMIT_PAGING_COUNT.fetch_add(1, Ordering::Relaxed);
    }

    // ── K6: the HNR2 arm, and it must come BEFORE the two legacy decodes ────
    //
    // ⛔ THIS IS WHAT MAKES THE 64-BYTE HVC1 PRIVATE BUFFER SAFE. Both decodes
    // below are keyed only on a magic at offset 0 of `pDmaBufferPrivateData`,
    // and `Hnr2KmdDmaPrivateV1`'s first u32 is the low half of a guest-supplied
    // `batch_token` — so a token whose low word happened to be 'HPBL' or 'HD12'
    // would be read as a present record. The role gate makes that unreachable
    // rather than unlikely.
    let h_context = unsafe { submit.__bindgen_anon_1.hContext };
    if !h_context.is_null() {
        let context = unsafe { crate::device::ContextHandleRef::from_raw(h_context) };
        if let Some((native, session, _device, _outer)) =
            context.as_ref().and_then(|c| c.outer_native())
        {
            let _ = adapter.with_k11_completion(|| {
                let Some(ticket) = adapter
                    .with_wddm_notify_lock(|guard| guard.admit_ordered_engine_submission(fence))
                else {
                    return;
                };
                // SAFETY: the attached context is the direct owner of this
                // scheduler private-data window.
                let disposition = unsafe {
                    crate::ddi::native_render::submit_outer_physical(
                        native, session, submit, ticket,
                    )
                };
                if !matches!(
                    disposition,
                    crate::ddi::native_render::NativeSubmitDisposition::Pending
                ) {
                    let _ = super::interrupt::fail_ordered_engine_submission(adapter, ticket);
                }
            });
            return STATUS_SUCCESS;
        }
        if let Some((native, session)) = context.as_ref().and_then(|c| c.native()) {
            // One fixed adapter rundown guard spans scheduler admission and the
            // synchronous K11-control completion path.  Queue work moves its
            // own session/context/allocation custody into the async host entry;
            // retaining this adapter guard there would deadlock TDR, whose
            // physical reset is what makes an unresponsive host entry terminal.
            let disposition = adapter
                .with_k11_completion(|| {
                    // K9 admission happens before any native submit transition.
                    // Preemption/reset therefore either sees and invalidates this
                    // exact ticket or happens wholly before the new generation's
                    // admission; a late host result cannot discover a successor by
                    // fence value.
                    let ticket = adapter.with_wddm_notify_lock(|guard| {
                        guard.admit_ordered_engine_submission(fence)
                    })?;
                    // SAFETY: the private-data pair for this submission.
                    let disposition = unsafe {
                        crate::ddi::native_render::submit(native, session, submit, ticket)
                    };
                    if let crate::ddi::native_render::NativeSubmitDisposition::HostCompleted(
                        exact_fence,
                    ) = disposition
                    {
                        if exact_fence == fence {
                            // Host-resource rundown has ended. K9 retains an early
                            // cross-context completion and notifies only when this
                            // ticket reaches the one-engine head.
                            complete_k11_host_submission(adapter, ticket);
                        } else {
                            // The direct context admitted a different scheduler
                            // fence than the callback supplied. Fail this generation
                            // closed; never substitute either scalar.
                            let _ =
                                super::interrupt::fail_ordered_engine_submission(adapter, ticket);
                        }
                    } else if matches!(
                        disposition,
                        crate::ddi::native_render::NativeSubmitDisposition::Revoked
                    ) {
                        let _ = super::interrupt::fail_ordered_engine_submission(adapter, ticket);
                    }
                    Some((disposition, ticket))
                })
                .flatten();
            let SubmitAck::Accepted = match disposition {
                Some((crate::ddi::native_render::NativeSubmitDisposition::HostCompleted(_), _))
                | Some((crate::ddi::native_render::NativeSubmitDisposition::Pending, _)) => {
                    SubmitAck::Accepted
                }
                Some((crate::ddi::native_render::NativeSubmitDisposition::Revoked, _)) | None => {
                    // The host-completed marker belonged to a session whose
                    // exact transport/fence authority was revoked before this
                    // callback, or reset already closed the adapter completion
                    // epoch. Do not forge completion through the legacy queue.
                    SubmitAck::Accepted
                }
                Some((crate::ddi::native_render::NativeSubmitDisposition::Refused, ticket)) => {
                    note_and_maybe_signal(adapter, fence, is_paging, None, Some(ticket))
                }
            };
            return STATUS_SUCCESS;
        }
    }

    let present_fence = unsafe { decode_legacy_present_fence(submit) };
    unsafe {
        arm_dma_flip(
            adapter,
            submit.pDmaBufferPrivateData,
            submit.DmaBufferPrivateDataSize,
        )
    };
    // As above: accepted regardless of the notification outcome.
    let SubmitAck::Accepted = note_and_maybe_signal(adapter, fence, is_paging, present_fence, None);
    STATUS_SUCCESS
}

/// Cumulative count of pending WDDM fences discarded by a scheduler epoch —
/// engine reset, preemption, or TDR recovery.
///
/// Exactly the number a TDR post-mortem wants, and before R615 nothing recorded
/// it: all three sites discarded `preempt_flush`'s return with `let _`.
pub static ABANDONED_FENCES: AtomicU32 = AtomicU32::new(0);

/// What the caller owes VidSch after the pending fences are dropped.
///
/// The three TDR-adjacent DDIs perform the SAME "take the notify lock, drop
/// every pending WDDM fence" step and then do three different things
/// afterwards, with the shared step named nowhere. Making the difference an
/// exhaustive value forces any future TDR-adjacent DDI to declare which
/// notification it owes; today the choice is invisible.
pub(crate) enum AbandonOutcome<'a> {
    /// `DxgkDdiResetFromTimeout`: dxgkrnl owns the post-reset fence state and
    /// wants no packet.
    Silent,
    /// `DxgkDdiPreemptCommand`: acknowledge with a `DMA_PREEMPTED` packet.
    Preempted {
        dxgkrnl: &'a DXGKRNL_INTERFACE,
        fence: u32,
    },
    /// `DxgkDdiResetEngine`: report the completed watermark.
    ReportLastAborted { out: &'a mut UINT },
}

/// Drop every pending WDDM fence and settle what is owed to VidSch, in ONE
/// notification critical section.
///
/// The one-critical-section rule is the load-bearing part and it used to be
/// documented only inside `DxgkDdiPreemptCommand`, where a reader of
/// `DxgkDdiResetEngine` would never see it: preemption participates in the same
/// VidSch fence stream as DMA_COMPLETED, so if the FIFO is cleared and the
/// watermark sampled in one section but the packet is built in another, a
/// completion DPC can advance `last_completed_fence` in between and make the
/// preemption packet claim the preemption fence itself as already completed.
/// Dxgkrnl rejects that one-fence leap with bugcheck 0x119/1 (observed:
/// expected 0x17a, received 0x17b).
///
/// Returns the number of fences dropped and the status to report.
///
/// ⚠ The count goes to an ATOMIC ONLY, never to `record_named_bytes` as the
/// review proposed: all three callers run at DISPATCH_LEVEL, and a registry
/// write above PASSIVE is one of the project's never-violate rules. The
/// `AbnDrop` mirror is written from the PASSIVE telemetry flush in `adapter.rs`,
/// the same way `WtOut` and `WtTbl` are.
pub(crate) fn abandon_pending_submissions(
    adapter: &AdapterContext,
    outcome: AbandonOutcome<'_>,
) -> (u32, NTSTATUS) {
    // Preemption is the sole replayable scheduler outcome: dxgkrnl resubmits
    // the same private record after it re-establishes residency.
    let retain_for_resubmit = matches!(&outcome, AbandonOutcome::Preempted { .. });
    adapter.with_wddm_notify_lock(|guard| {
        // K9 invalidates the exact frontier generation in the same critical
        // section that abandons the compatibility FIFO. A host callback that
        // arrives later carries only its old direct ticket and is inert.
        guard.invalidate_ordered_engine();
        let dropped = guard
            .with_virtio(|o, v| {
                if retain_for_resubmit {
                    v.preempt_flush(o)
                } else {
                    v.terminal_abandon_wddm_epoch(o)
                }
            })
            .unwrap_or(0);
        if dropped != 0 {
            ABANDONED_FENCES.fetch_add(dropped, Ordering::Relaxed);
        }
        let status = match outcome {
            AbandonOutcome::Silent => STATUS_SUCCESS,
            AbandonOutcome::Preempted { dxgkrnl, fence } => {
                // SAFETY: the WDDM notification lock serializes this packet with
                // every DMA_COMPLETED packet; the callback interface is live and
                // delivery is raised to DIRQL by notify_at_dirql.
                unsafe { signal_dma_preempted_locked(guard, dxgkrnl, fence) }
            }
            AbandonOutcome::ReportLastAborted { out } => {
                // Written INSIDE the guard, exactly as before. Do NOT change the
                // value: whether DXGKARG_RESETENGINE wants the completed
                // watermark or the first aborted fence is an OPEN QUESTION
                // against the WDK header, deliberately not resolved here.
                *out = guard.completed_fence() as UINT;
                STATUS_SUCCESS
            }
        };
        // Successful preemption is the one abandonment that permits replay in
        // the same physical transport. Reopen only after the PREEMPTED packet
        // was accepted; failure leaves the engine closed for removal/TDR.
        if retain_for_resubmit && status == STATUS_SUCCESS {
            let _ = guard.reopen_ordered_engine();
        }
        (dropped, status)
    })
}

/// `DxgkDdiPreemptCommand` — VidSch wants the node's pending submissions back
/// (TDR probe or priority scheduling). We cannot abort host venus work, but we
/// CAN release the pending WDDM fences: drop them (the scheduler resubmits the
/// incomplete DMA buffers later; the venus work keeps executing and the fresh
/// submissions re-queue behind whatever is still outstanding) and acknowledge
/// with `DMA_PREEMPTED`. Without this ack, a validate-slow venus fence
/// (> TdrDelay) escalates straight to ResetFromTimeout. Runs at DISPATCH_LEVEL.
pub unsafe extern "C" fn dxgkddi_preempt_command(
    h_adapter: *mut c_void,
    preempt_command: *const DXGKARG_PREEMPTCOMMAND,
) -> NTSTATUS {
    PREEMPT_COUNT.fetch_add(1, Ordering::Relaxed);
    if h_adapter.is_null() || preempt_command.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    let adapter = unsafe { &*(h_adapter as *const AdapterContext) };
    let preempt = unsafe { &*preempt_command };

    let dxgkrnl = match adapter.dxgkrnl() {
        Ok(interface) => interface,
        Err(_) => return STATUS_DEVICE_NOT_READY,
    };
    // The one-critical-section rationale now lives on
    // `abandon_pending_submissions`, where DxgkDdiResetEngine's reader can see
    // it too.
    abandon_pending_submissions(
        adapter,
        AbandonOutcome::Preempted {
            dxgkrnl,
            fence: preempt.PreemptionFenceId,
        },
    )
    .1
}

/// `DxgkDdiResetFromTimeout` — TDR recovery. The legacy path preserves its
/// scheduler-only reset. Active KMD D2 additionally closes canonical control
/// admission, proves runner/finalizer rundown, performs an exact virtio device
/// reset, and drains ambiguous custody before the old transport can be dropped.
pub unsafe extern "C" fn dxgkddi_reset_from_timeout(h_adapter: *mut c_void) -> NTSTATUS {
    if h_adapter.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    let adapter = unsafe { &*(h_adapter as *const AdapterContext) };
    let passive = unsafe { crate::irql::PassiveLevel::assume() };
    // Close capability/object generations before any device-lost wakeup or
    // transport producer can observe the reset boundary. Both invalidations
    // are lock-free and define one indivisible adapter epoch transition.
    crate::ddi::native_fence::invalidate_all(
        adapter,
        crate::ddi::native_fence::NativeFenceInvalidation::Reset,
    );
    crate::adapter::allocation_object::invalidate_all();
    // Close new K11 completions and join every callback that already owns the
    // old scheduler epoch before its fences are abandoned. The guard spans no
    // host work and waits by one event, never by polling.
    adapter.close_k11_completions_and_wait(passive);
    // Prevent a DPC from taking a fence out of the pending FIFO while reset is
    // discarding that same scheduler epoch.  Dxgkrnl owns the post-reset fence
    // state; no completion from the abandoned epoch may escape concurrently.
    let _ = abandon_pending_submissions(adapter, AbandonOutcome::Silent);
    // Consume transport_failed(), which had zero callers repo-wide: a TDR
    // against a latched ring is the loop this tranche exists to break, and
    // without this the only evidence was a DiagLevel-gated breadcrumb. Reported
    // here and mirrored on change only, so a TDR storm cannot become a registry
    // write storm.
    let failed = adapter
        .with_virtio(|v| v.transport_failed())
        .unwrap_or(false);
    if failed {
        let bad_token = crate::virtio::gpu::DRAIN_BAD_TOKEN.load(Ordering::Relaxed);
        let bad_length = crate::virtio::gpu::DRAIN_BAD_USED_LENGTH.load(Ordering::Relaxed);
        let bad = bad_token.min(u16::MAX as u32) | (bad_length.min(u16::MAX as u32) << 16);
        if RING_FAIL_REPORTED.swap(bad, Ordering::Relaxed) != bad {
            crate::diag::fault(crate::diag::FaultCounter::StRing, bad);
        }
    }
    if crate::virtio::KMD_D2_OWNER_ENABLED {
        // ResetFromTimeout is a PASSIVE_LEVEL DDI. First cancel/join display
        // producers, then removing the Venus client joins its mutex-protected
        // producers and makes later callers fail closed before canonical owner
        // admission is sealed.
        let had_transport = adapter.with_virtio(|_| ()).is_ok();
        adapter.stop_vsync();
        adapter.stop_hpd();
        if adapter.hpd_worker_may_be_running() {
            return STATUS_DEVICE_NOT_READY;
        }
        if had_transport {
            crate::ddi::direct_scanout::prepare_reset(
                passive,
                adapter,
                helios_kmd_logic::direct_scanout_lifetime::DrainReason::DwmRestart,
            );
        }
        adapter.set_venus_client(None);
        adapter
            .isr_status
            .store(0, core::sync::atomic::Ordering::Release);
        if let Err(error) = adapter
            .close_control_owner_transport()
            .and_then(|()| adapter.retire_control_owner_transport(passive))
        {
            let status: NTSTATUS = error.into();
            crate::diag::fault(crate::diag::FaultCounter::StVioR, status as u32);
            return status;
        }
        if had_transport {
            crate::ddi::direct_scanout::complete_verified_reset(passive, adapter);
        }
        let _ = adapter.remove_virtio_and_reset_scanout_bind_generation(passive);
        adapter.reset_display_publication_state();
        // SAFETY: the reset DDI owns the transport transition; the replacement
        // context is not published until RestartFromTimeout initializes it.
        let _ = unsafe { adapter.set_reset_venus_context(0) };
    }
    STATUS_SUCCESS
}

/// Last packed `(overlength count, unmatched-token count)` reported through
/// `StRing`.
static RING_FAIL_REPORTED: AtomicU32 = AtomicU32::new(u32::MAX);

/// `DxgkDdiRestartFromTimeout` — resume after TDR.
pub unsafe extern "C" fn dxgkddi_restart_from_timeout(h_adapter: *mut c_void) -> NTSTATUS {
    if h_adapter.is_null() {
        return STATUS_INVALID_PARAMETER;
    }

    let adapter = unsafe { &*(h_adapter as *const AdapterContext) };
    if crate::virtio::KMD_D2_OWNER_ENABLED {
        let passive = unsafe { crate::irql::PassiveLevel::assume() };
        adapter
            .start_complete
            .store(0, core::sync::atomic::Ordering::Release);
        let dxgkrnl = match adapter.dxgkrnl() {
            Ok(interface) => interface,
            Err(_) => return STATUS_DEVICE_NOT_READY,
        };
        let reserve = adapter.bar_segment().map_or(0, |bar| bar.size);
        let absent = adapter.remove_virtio_and_reset_scanout_bind_generation(passive);
        if !absent.installable() {
            return STATUS_DEVICE_NOT_READY;
        }
        let mut gpu = match crate::virtio::VirtioGpu::init(passive, dxgkrnl) {
            Ok(gpu) => gpu,
            Err(error) => {
                let status: NTSTATUS = error.into();
                crate::diag::fault(crate::diag::FaultCounter::StVioR, status as u32);
                return status;
            }
        };
        if reserve != 0 {
            let exact_window = gpu.host_visible().is_some_and(|window| {
                adapter
                    .bar_segment()
                    .is_some_and(|bar| window.base == bar.gpa)
            });
            if !exact_window || !gpu.configure_window_reserve(reserve) {
                crate::diag::fault(crate::diag::FaultCounter::StVioR, u32::MAX);
                return match crate::virtio::VirtioGpu::reset_unpublished_or_retain(passive, gpu) {
                    Ok(()) => STATUS_DEVICE_NOT_READY,
                    Err(error) => error.into(),
                };
            }
        }
        adapter
            .isr_status
            .store(gpu.isr_status_addr(), core::sync::atomic::Ordering::Release);
        if let Err(error) = unsafe { adapter.install_virtio(passive, absent, gpu) } {
            adapter
                .isr_status
                .store(0, core::sync::atomic::Ordering::Release);
            let status: NTSTATUS = error.into();
            crate::diag::fault(crate::diag::FaultCounter::StVioR, status as u32);
            return status;
        }
        let venus_context = super::lifecycle::bring_up_venus(passive, adapter);
        if venus_context == 0 {
            adapter
                .isr_status
                .store(0, core::sync::atomic::Ordering::Release);
            let cleanup = adapter
                .close_control_owner_transport()
                .and_then(|()| adapter.retire_control_owner_transport(passive));
            if cleanup.is_ok() {
                crate::ddi::direct_scanout::complete_verified_reset(passive, adapter);
                let _ = adapter.remove_virtio_and_reset_scanout_bind_generation(passive);
                adapter.reset_display_publication_state();
            }
            let status = cleanup
                .err()
                .map_or(STATUS_DEVICE_NOT_READY, |error| error.into());
            crate::diag::fault(crate::diag::FaultCounter::StVioR, status as u32);
            return status;
        }
        if !unsafe { adapter.set_reset_venus_context(venus_context) } {
            crate::diag::fault(crate::diag::FaultCounter::StVioR, u32::MAX - 1);
            adapter
                .isr_status
                .store(0, core::sync::atomic::Ordering::Release);
            let cleanup = adapter
                .close_control_owner_transport()
                .and_then(|()| adapter.retire_control_owner_transport(passive));
            if cleanup.is_ok() {
                crate::ddi::direct_scanout::complete_verified_reset(passive, adapter);
                let _ = adapter.remove_virtio_and_reset_scanout_bind_generation(passive);
                adapter.reset_display_publication_state();
            }
            return cleanup
                .err()
                .map_or(STATUS_DEVICE_NOT_READY, |error| error.into());
        }
        if adapter.display_half() {
            let (width, height) = adapter.display_mode();
            if let Err(start_error) =
                crate::ddi::direct_scanout::start(passive, adapter, width, height)
            {
                crate::ddi::direct_scanout::prepare_reset(
                    passive,
                    adapter,
                    helios_kmd_logic::direct_scanout_lifetime::DrainReason::DwmRestart,
                );
                adapter
                    .isr_status
                    .store(0, core::sync::atomic::Ordering::Release);
                adapter.set_venus_client(None);
                let cleanup = adapter
                    .close_control_owner_transport()
                    .and_then(|()| adapter.retire_control_owner_transport(passive));
                let status = match cleanup {
                    Ok(()) => {
                        crate::ddi::direct_scanout::complete_verified_reset(passive, adapter);
                        let _ = adapter.remove_virtio_and_reset_scanout_bind_generation(passive);
                        adapter.reset_display_publication_state();
                        let _ = unsafe { adapter.set_reset_venus_context(0) };
                        start_error.into()
                    }
                    Err(error) => error.into(),
                };
                crate::diag::fault(crate::diag::FaultCounter::StVioR, status as u32);
                return status;
            }
            unsafe { adapter.start_vsync() };
            unsafe { adapter.init_hpd() };
            adapter.signal_start_complete();
        }
    }

    adapter.reopen_k11_completions();
    crate::ddi::native_fence::resume_after_reset(adapter);
    STATUS_SUCCESS
}

// ── Render-path DDIs. ───────────────────────────────────────────────────────

/// `DxgkDdiRender` — record a DMA buffer from a UMD command buffer.
///
/// Our UMD command buffer already begins with a `HeliosWddmCmdBuf` followed by the
/// opaque venus stream (`protocol/src/wddm.rs`), so "recording" is a straight copy
/// of `pCommand` into `pDmaBuffer`; there are no guest GPU-VAs to translate
/// (decorative GpuMmu — the host owns the real MMU), so the patch-location list is
/// passed through untouched and the matching `DxgkDdiPatch` is a no-op. The venus
/// forwarding itself happens at submit/complete time (see `dxgkddi_submit_command`).
///
/// NOTE: not exercised during VidSch adapter bring-up (no UMD/app is rendering
/// yet); present so the render-capable DDI contract is real rather than a
/// NOT_IMPLEMENTED stub.
pub unsafe extern "C" fn dxgkddi_render(
    h_context: IN_CONST_HANDLE,
    render: INOUT_PDXGKARG_RENDER,
) -> NTSTATUS {
    RENDER_COUNT.fetch_add(1, Ordering::Relaxed);
    if render.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    let args = unsafe { &mut *render };
    if args.pDmaBuffer.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    let cmd_len = args.CommandLength as usize;
    let dma_cap = args.DmaSize as usize;
    if cmd_len > 0 && args.pCommand.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    if cmd_len > dma_cap {
        // Buffer too small for the recorded command: ask the runtime to grow it.
        return STATUS_BUFFER_TOO_SMALL;
    }

    // ── K6: the HVC1 native-render arm ──────────────────────────────────────
    //
    // ⛔ BRANCH ON THE CONTEXT ROLE, NEVER ON A FOURTH COMMAND MAGIC. The three
    // arms below are magic-disjoint by construction; a magic-keyed HNR2 arm
    // would let a legacy DWM/CDD Render take the HNR2 path, and an HNR2 Render
    // would still fall through `invalidate_d3d12` and the tail memcpy. This
    // returns before all of them.
    //
    // ⚠ `h_context` IS NULL-CHECKED HERE and was not checked anywhere in this
    // function before: the HERF/HEPR arms below reach it only through
    // `ContextHandleRef::from_raw`, which checks. A role lookup added without
    // one dereferences null in a DDI, which is a silent graphics deadlock.
    if !h_context.is_null() {
        let context = unsafe { crate::device::ContextHandleRef::from_raw(h_context) };
        if let Some((native, session, device, outer)) =
            context.as_ref().and_then(|c| c.outer_native())
        {
            // SAFETY: the attached role supplies the exact context/session/
            // device graph and Render's live command/allocation windows.
            return unsafe {
                crate::ddi::native_render::render_outer_physical(
                    native, session, device, outer, args,
                )
            };
        }
        if let Some((native, session)) = context.as_ref().and_then(|c| c.native()) {
            // SAFETY: `args` is dxgkrnl's live argument struct, and `native` /
            // `session` belong to the context handle it just passed.
            return unsafe { crate::ddi::native_render::render(native, session, args) };
        }
    }

    // ── The D3D12 ECL arm (`HeliosD3D12SubmitCmd`, 16 B) ────────────────────
    //
    // `helios_umd12.dll` submits this through `pfnRenderCb` during
    // `pfnExecuteCommandLists` so the D3D12 queue's WDDM context carries a DMA
    // packet at all. The record's `gpu_wire_fence` is the completion boundary,
    // and writing it into THIS Render's `pDmaBufferPrivateData` is the whole
    // commit: `dxgkddi_submit_command{,_virtual}` already decode a
    // `PresentSubmissionPrivate` from that same buffer unconditionally
    // (`:871` / `:827`, keyed only on the record's magic — nothing there is
    // Present-specific), and `note_wddm_submission` already turns a nonzero
    // `gpu_fence_id` into `RetireDomain::IncludingGpu` with the exact watermark.
    //
    // ⚠ FIRST WRITE TO `pDmaBufferPrivateData` ON THIS DDI. Verified against the
    // WDK header rather than a doc: `_DXGKARG_RENDER` carries
    // `pDmaBufferPrivateData` + `DmaBufferPrivateDataSize`
    // (`tmp/dx12/sdk/d3dkmddi.h:130-147`). Offset 0, and the pointer is NOT
    // advanced — the shape the Present path already proves, and the shape
    // SubmitCommand's offset-0 fallback (`decode_present_fence`) reads.
    //
    // ⛔ THE WRITE IS UNCONDITIONAL, INCLUDING THE ZERO-FENCE ARM, and that is a
    // correction of this arm's first form (which declined to write when the fence
    // was 0). dxgkrnl RECYCLES DMA private-data buffers between submissions — the
    // reason `PresentFlipPrivate::take` consumes its record — so declining to
    // write leaves the PREVIOUS ECL's boundary at offset 0, and SubmitCommand then
    // gates this packet on a fence that has already retired: the application's
    // `ID3D12Fence` signals early while `D12Rec` moves, `D12MrgF` is 0 and
    // `GpuFncClamp` is 0. The original defect wearing the instrumentation of the
    // fix. Writing every time makes "no boundary" explicit for THIS submission
    // instead of inherited from another one, `mark_d3d12`'s own magic keeps an
    // all-zero payload identifiable, and `decode` consumes the D3D12 arm so a
    // missing write can only fail safe. It does not touch the
    // `D12Rec` and `D12Merged` count this arm.
    //
    // The three arms in this function are magic-disjoint by construction, and
    // `protocol/src/wddm.rs`'s const asserts pin the two size relationships that
    // make this one reachable without shadowing the others.
    let mut stamped_d3d12 = false;
    if cmd_len >= size_of::<helios_protocol::HeliosD3D12SubmitCmd>() {
        let mut raw = [0u8; size_of::<helios_protocol::HeliosD3D12SubmitCmd>()];
        // SAFETY: the runtime guarantees `CommandLength` readable bytes at
        // `pCommand` (non-null, checked above) and `cmd_len` covers the record.
        unsafe {
            core::ptr::copy_nonoverlapping(
                args.pCommand as *const u8,
                raw.as_mut_ptr(),
                size_of::<helios_protocol::HeliosD3D12SubmitCmd>(),
            );
        }
        // SAFETY: `raw` is a fully initialized local of exactly the record size;
        // the read is unaligned-safe and cannot leave the local.
        let command = unsafe {
            core::ptr::read_unaligned(raw.as_ptr().cast::<helios_protocol::HeliosD3D12SubmitCmd>())
        };
        if command.is_valid() {
            D3D12_SUBMIT_RECORDS.fetch_add(1, Ordering::Relaxed);
            if command.gpu_wire_fence == 0 {
                D3D12_SUBMIT_ZERO_FENCE.fetch_add(1, Ordering::Relaxed);
            }
            match unsafe {
                PresentSubmissionPrivate::mark_d3d12(
                    args.pDmaBufferPrivateData,
                    args.DmaBufferPrivateDataSize,
                    command.gpu_wire_fence,
                )
            } {
                Ok(crate::ddi::present_packet::D3d12Mark::MergedWithPredecessor) => {
                    D3D12_SUBMIT_MERGED.fetch_add(1, Ordering::Relaxed);
                }
                Ok(crate::ddi::present_packet::D3d12Mark::First) => {}
                Err(_status) => {
                    // COUNT AND IGNORE, never a failing return: a non-SUCCESS
                    // status out of a submit DDI bugchecks
                    // `dxgmms2!VidSchiSendToExecutionQueue` with 0x119/1, which
                    // this file's own `SubmitAck` doc records.
                    // `STATUS_BUFFER_TOO_SMALL` is legal here for the DMA buffer,
                    // but it asks the runtime to GROW and retry — which is not an
                    // answer to a private-data buffer the driver itself sized.
                    D3D12_SUBMIT_MERGE_FAILS.fetch_add(1, Ordering::Relaxed);
                }
            }
            stamped_d3d12 = true;
        }
    }

    // ⛔ THE OTHER HALF OF THE UNCONDITIONAL WRITE ABOVE. A Render that did not
    // stamp a `'HD12'` record must not leave one it found, or it inherits another
    // submission's boundary. `mark_d3d12`'s write covers the ECL arm; this covers
    // every other `pfnRenderCb` on a D3D12 WDDM context — above all UP-9's
    // present-identity Render, which writes nothing at offset 0.
    //
    // Placed here rather than at the end of the function on purpose: in
    // `DxgkDdiRender` the ECL arm is the ONLY writer of offset 0 (the HEPR/HERF
    // arms below merge through `display.rs`'s Present DDI, never through this
    // one), so once that arm has been decided nothing later can re-stamp it.
    //
    // ⚠ A HOLE WAS SUSPECTED HERE AND IS **REFUTED BY GUARD SYMMETRY** — recorded
    // so it is not re-raised. `stamped_d3d12` is set on the `mark_d3d12` ERROR arm
    // too, which looks like a Render that neither writes nor clears and could
    // therefore inherit a predecessor's boundary. It cannot: `mark_d3d12`,
    // `invalidate_d3d12` and `peek` carry the **identical** guard
    // (`private_data.is_null() || private_size < size_of::<PresentSubmissionPrivate>()`).
    // So `mark_d3d12` returning `Err` implies `invalidate_d3d12` would have
    // returned `false` anyway AND `peek` returns `None`, i.e. the buffer is too
    // small (or absent) to hold a record at all — there is nothing to inherit and
    // no boundary is decoded. The three guards must stay identical for this
    // argument to hold; that is the invariant to preserve, not the branch.
    //
    // SAFETY: `pDmaBufferPrivateData` / `DmaBufferPrivateDataSize` are the pair
    // dxgkrnl supplied for THIS Render; the helper re-checks null and size before
    // touching a byte, and writes only the 4-byte magic at offset 0.
    if !stamped_d3d12
        && unsafe {
            PresentSubmissionPrivate::invalidate_d3d12(
                args.pDmaBufferPrivateData,
                args.DmaBufferPrivateDataSize,
            )
        }
    {
        D3D12_STALE_RECORD_CLEARED.fetch_add(1, Ordering::Relaxed);
    }

    if args.PatchLocationListInSize > args.PatchLocationListOutSize {
        return STATUS_BUFFER_TOO_SMALL;
    }
    if args.PatchLocationListInSize != 0
        && (args.pPatchLocationListIn.is_null() || args.pPatchLocationListOut.is_null())
    {
        return STATUS_INVALID_PARAMETER;
    }

    for i in 0..args.PatchLocationListInSize {
        let input = unsafe { &*args.pPatchLocationListIn.add(i as usize) };
        let output = unsafe { &mut *args.pPatchLocationListOut.add(i as usize) };
        unsafe { core::ptr::write_bytes(output as *mut _, 0, 1) };
        output.AllocationIndex = input.AllocationIndex;
        output.AllocationOffset = 0;
        output.PatchOffset = 0;
        output.SplitOffset = 0;
        output.__bindgen_anon_1.Value = i & 0x00ff_ffff;
    }
    args.PatchLocationListOutSize = args.PatchLocationListInSize;
    if !args.pPatchLocationListOut.is_null() {
        args.pPatchLocationListOut = unsafe {
            args.pPatchLocationListOut
                .add(args.PatchLocationListInSize as usize)
        };
    }

    if cmd_len > 0 {
        // SAFETY: the runtime guarantees `pCommand` has `CommandLength` readable
        // bytes and `pDmaBuffer` has `DmaSize` writable bytes; we copy at most
        // `cmd_len` (<= DmaSize) and the ranges do not overlap.
        unsafe {
            core::ptr::copy_nonoverlapping(
                args.pCommand as *const u8,
                args.pDmaBuffer as *mut u8,
                cmd_len,
            );
        }
    }
    args.pDmaBuffer = unsafe { (args.pDmaBuffer as *mut u8).add(cmd_len) as *mut c_void };
    args.MultipassOffset = 0;
    STATUS_SUCCESS
}

/// `DxgkDdiRenderKm` — kernel-mode (GDI hardware-acceleration) render path.
///
/// dxgkrnl drives this when GDI renders to a cross-adapter / GDI-accelerated surface
/// — gated by `DXGK_PRESENTATIONCAPS::SupportKernelModeCommandBuffer` (which we
/// advertise, mandatory for Code-0 load) together with `CrossAdapterResource`
/// (`gdi-hardware-acceleration.md`: GDI-HW-accel KMDs MUST implement
/// CreateAllocation + GetStandardAllocationDriverData + RenderKm). The OS passes an
/// array of `DXGK_RENDERKM_COMMAND` ops in `pCommand`; the driver must translate
/// them into a DMA buffer + patch-location list, **advance the in/out pointers**,
/// and return SUCCESS. Returning `STATUS_NOT_IMPLEMENTED` leaves `pDmaBuffer`
/// unadvanced and the submission output unfilled, after which
/// `dxgkrnl!ADAPTER_RENDER::DdiRenderGdi` calls a null function pointer
/// (observed live: `DdiRenderGdi+0x140` `call rax`, rax=0 → 0xC0000005).
///
/// Decorative-GpuMmu model: the host GPU (venus) owns real rendering by resource id,
/// so we do not lower GDI ops to GPU instructions here. We record the opaque command
/// bytes into the DMA buffer (so `DxgkDdiSubmitCommand` has a non-empty buffer to
/// retire) and advance the DMA write pointer; there are no guest GPU-VAs to patch
/// (matching `DxgkDdiPatch`'s no-op), so the out patch list stays at its base (0
/// entries). `SubmitCommand` drives the fence. NOTE: this makes the path structurally
/// complete (no crash); pixel-correct GDI lowering is a later step — DWM's own
/// composition is D3D (the UMD `DxgkDdiRender` path), not this GDI path.
pub unsafe extern "C" fn dxgkddi_render_km(
    _h_context: IN_CONST_HANDLE,
    render: INOUT_PDXGKARG_RENDER,
) -> NTSTATUS {
    RENDER_COUNT.fetch_add(1, Ordering::Relaxed);
    if render.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    let args = unsafe { &mut *render };
    if args.pDmaBuffer.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    let cmd_len = args.CommandLength as usize;
    let dma_cap = args.DmaSize as usize;
    // Ask the runtime to grow the DMA buffer if the command does not fit, rather
    // than truncating (mirrors `dxgkddi_render`).
    if cmd_len > dma_cap {
        return STATUS_BUFFER_TOO_SMALL;
    }
    if cmd_len > 0 && !args.pCommand.is_null() {
        // SAFETY: runtime guarantees `CommandLength` readable bytes at `pCommand`
        // and `DmaSize` (>= cmd_len) writable bytes at `pDmaBuffer`; distinct buffers.
        unsafe {
            core::ptr::copy_nonoverlapping(
                args.pCommand as *const u8,
                args.pDmaBuffer as *mut u8,
                cmd_len,
            );
        }
    }
    // Advance the DMA write pointer past the recorded bytes so the runtime sees a
    // non-empty buffer to submit. No GPU-VA patches → leave pPatchLocationListOut at
    // its base. Single pass → MultipassOffset 0.
    // SAFETY: advancing within the `DmaSize`-byte buffer (cmd_len <= dma_cap).
    args.pDmaBuffer = unsafe { (args.pDmaBuffer as *mut u8).add(cmd_len) as *mut c_void };
    args.MultipassOffset = 0;
    STATUS_SUCCESS
}

/// `DxgkDdiRenderGdi` — GDI hardware-acceleration render path
/// (`PDXGKDDI_RENDERGDI`, args `DXGKARG_RENDERGDI`). This is a SEPARATE DDI from
/// `DxgkDdiRender` and `DxgkDdiRenderKm` — and the one dxgkrnl's
/// `ADAPTER_RENDER::DdiRenderGdi` actually invokes (through a CFG-guarded indirect
/// call). Leaving the `DxgkDdiRenderGdi` field null (we previously registered only
/// Render + RenderKm) made that call land on a null pointer and bugcheck
/// (kernel `0xC0000005`, `DdiRenderGdi+0x140`, observed live), which is why this
/// entry point stays registered and answers SUCCESS even though the driver no
/// longer opts into GDI hardware acceleration at all: as of 22.22.180.0
/// `DXGK_PRESENTATIONCAPS::SupportKernelModeCommandBuffer` is hard-coded 0
/// (`query_adapter_info`), so dxgkrnl routes GDI through win32k's CPU redirection
/// path and never drives this DDI. The KMD CPU raster executor that used to run
/// here (`gdi_blit.rs`) was deleted with it.
///
/// Body (identical in shape to `dxgkddi_render_km`, which is why T7 dedups them):
/// record the opaque command bytes into the DMA buffer so `DxgkDdiSubmitCommand`
/// has a non-empty buffer to retire, advance the DMA write pointer, single pass,
/// no GPU-VA patches → SUCCESS.
pub unsafe extern "C" fn dxgkddi_render_gdi(
    _h_context: IN_CONST_HANDLE,
    render_gdi: INOUT_PDXGKARG_RENDERGDI,
) -> NTSTATUS {
    RENDER_COUNT.fetch_add(1, Ordering::Relaxed);
    if render_gdi.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    let args = unsafe { &mut *render_gdi };
    if args.pDmaBuffer.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    let cmd_len = args.CommandLength as usize;
    let dma_cap = args.DmaSize as usize;
    // Ask the runtime to grow the DMA buffer rather than truncating the stream.
    if cmd_len > dma_cap {
        return STATUS_BUFFER_TOO_SMALL;
    }
    if cmd_len > 0 && !args.pCommand.is_null() {
        // SAFETY: runtime guarantees CommandLength readable bytes at pCommand and
        // DmaSize (>= cmd_len) writable bytes at pDmaBuffer; distinct buffers.
        unsafe {
            core::ptr::copy_nonoverlapping(
                args.pCommand as *const u8,
                args.pDmaBuffer as *mut u8,
                cmd_len,
            );
        }
    }
    // SAFETY: advancing within the DmaSize-byte buffer (cmd_len <= dma_cap).
    args.pDmaBuffer = unsafe { (args.pDmaBuffer as *mut u8).add(cmd_len) as *mut c_void };
    args.MultipassOffset = 0;
    STATUS_SUCCESS
}

/// `DxgkDdiPatch` — patch allocation references in a DMA buffer.
///
/// No-op success, like viogpu3d (`viogpu_command.cpp:289-298`): the decorative
/// GpuMmu has no guest GPU-VAs to patch (venus addresses resources by opaque id,
/// the host owns the real MMU), so there is nothing to fix up. Must return SUCCESS
/// (not NOT_IMPLEMENTED) for a render-capable adapter.
pub unsafe extern "C" fn dxgkddi_patch(
    _h_adapter: IN_CONST_HANDLE,
    patch: IN_CONST_PDXGKARG_PATCH,
) -> NTSTATUS {
    PATCH_COUNT.fetch_add(1, Ordering::Relaxed);
    if patch.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    // K6: an HNR2 submission's capability records take their placement snapshot
    // here. Reading the `hDevice`/`hContext` union as a context is legal because
    // this driver reports `SCHEDULINGCAPS_MULTI_ENGINE_AWARE`
    // (`query_adapter_info.rs:395`); a paging patch carries a null context.
    //
    // ⛔ NOTHING BELOW CAN FAIL THIS DDI. An error return from `DxgkDdiPatch`
    // bugchecks Windows outright (§10.7:1872), which is why every fallible step
    // — the tables, the plan, the generations — was done at Render.
    let args = unsafe { &*patch };
    let h_context = unsafe { args.__bindgen_anon_1.hContext };
    if !h_context.is_null() {
        let context = unsafe { crate::device::ContextHandleRef::from_raw(h_context) };
        if context.as_ref().and_then(|c| c.native()).is_some() {
            // SAFETY: `args` is dxgkrnl's live argument struct for a submission
            // on a context this driver resolved as HVC1.
            unsafe { crate::ddi::native_render::patch(args) };
        }
    }
    STATUS_SUCCESS
}

/// `DxgkDdiQueryCurrentFence` — report the last fence the GPU completed.
pub unsafe extern "C" fn dxgkddi_query_current_fence(
    h_adapter: IN_CONST_HANDLE,
    query_current_fence: INOUT_PDXGKARG_QUERYCURRENTFENCE,
) -> NTSTATUS {
    if h_adapter.is_null() || query_current_fence.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    let adapter = unsafe { &*(h_adapter as *const AdapterContext) };
    let query = unsafe { &mut *query_current_fence };
    unsafe {
        core::ptr::write_bytes(
            query as *mut _ as *mut u8,
            0,
            size_of::<DXGKARG_QUERYCURRENTFENCE>(),
        );
    }
    query.CurrentFence = adapter.completed_fence();
    query.NodeOrdinal = 0;
    query.EngineOrdinal = 0;
    STATUS_SUCCESS
}

/// `DxgkDdiCollectDbgInfo` — dump driver debug state on a TDR/bugcheck.
///
/// Contract notes (this fires DURING TDR dump collection, possibly at
/// HIGH_LEVEL during a bugcheck, so returning STATUS_NOT_IMPLEMENTED here
/// marked the driver as misbehaving in the 2026-07-02 ETW capture):
/// - May be called at any IRQL; must not block, allocate, take locks, or touch
///   pageable code/data. Only the DISPATCH-safe atomics are read.
/// - The OS-provided buffer must be written in full (unused tail zeroed).
pub unsafe extern "C" fn dxgkddi_collect_dbg_info(
    h_adapter: IN_CONST_HANDLE,
    collect_dbg_info: IN_CONST_PDXGKARG_COLLECTDBGINFO,
) -> NTSTATUS {
    if collect_dbg_info.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    // SAFETY: dxgkrnl guarantees the argument struct is valid for the call.
    let args = unsafe { &*collect_dbg_info };
    if args.pBuffer.is_null() || args.BufferSize == 0 {
        return STATUS_INVALID_PARAMETER;
    }
    let buf_len = args.BufferSize as usize;

    // SAFETY: dxgkrnl guarantees BufferSize writable non-paged bytes at
    // pBuffer for the duration of the call. Zero the whole buffer first so
    // the report is fully written regardless of how much we fill.
    unsafe {
        core::ptr::write_bytes(args.pBuffer as *mut u8, 0, buf_len);
    }

    // Fixed-shape DWORD report: magic + version + reason + engine counters.
    // Decoded offline from the TDR minidump's driver-private section.
    let last_fence = if h_adapter.is_null() {
        0
    } else {
        // SAFETY: dxgkrnl passes the adapter context handle it got from
        // DxgkDdiAddDevice; valid for the adapter's lifetime. Atomic load only.
        let adapter = unsafe { &*(h_adapter as *const AdapterContext) };
        adapter.completed_fence()
    };
    let etw_registration = crate::ddi::diag_etw::registration_snapshot();
    let report: [u32; 47] = [
        0x4844_4247, // 'HDBG'
        8,           // report version (8: + persistent SET namespaces at 42..46)
        args.Reason,
        SUBMIT_COUNT.load(Ordering::Relaxed),
        SUBMIT_LAST_FENCE.load(Ordering::Relaxed),
        SUBMIT_PAGING_COUNT.load(Ordering::Relaxed),
        RENDER_COUNT.load(Ordering::Relaxed),
        PATCH_COUNT.load(Ordering::Relaxed),
        PREEMPT_COUNT.load(Ordering::Relaxed),
        DMA_NOTIFY_COUNT.load(Ordering::Relaxed),
        DMA_QUEUE_DPC_COUNT.load(Ordering::Relaxed),
        last_fence,
        // Nonzero = synchronous control commands timed out their PASSIVE wait
        // budget (host stopped answering in time) — no longer a transport
        // poison, but still the likely reason for the TDR this dump belongs to.
        crate::virtio::gpu::CTRL_TIMEOUT_COUNT.load(Ordering::Relaxed),
        // v2: bounded-table telemetry (the 2026-07-03 MAX_BLOBS exhaustion class).
        crate::virtio::gpu::BLOB_HIGH_WATER.load(Ordering::Relaxed),
        crate::virtio::gpu::BLOB_FULL_REJECTS.load(Ordering::Relaxed),
        crate::virtio::gpu::RESOURCE_HIGH_WATER.load(Ordering::Relaxed),
        crate::virtio::gpu::RESOURCE_FULL_REJECTS.load(Ordering::Relaxed),
        crate::virtio::gpu::CONTEXT_FULL_DROPS.load(Ordering::Relaxed),
        crate::virtio::gpu::WINDOW_RANGE_DROPS.load(Ordering::Relaxed),
        crate::virtio::gpu::TAKE_LIVE_MISSES.load(Ordering::Relaxed),
        // Retired word retained as zero so later minidump indices do not move.
        0,
        // v3: C3/M3.4 async-transport telemetry.
        crate::virtio::gpu::ASYNC_SUBMIT_COUNT.load(Ordering::Relaxed),
        crate::virtio::gpu::ASYNC_COMPLETE_COUNT.load(Ordering::Relaxed),
        crate::virtio::gpu::ASYNC_RESP_ERRORS.load(Ordering::Relaxed),
        crate::virtio::gpu::FENCE_WAIT_REGISTERED.load(Ordering::Relaxed),
        crate::virtio::gpu::FENCE_WAIT_TIMEOUTS.load(Ordering::Relaxed),
        crate::virtio::gpu::DRAIN_BAD_TOKEN.load(Ordering::Relaxed),
        crate::virtio::gpu::QUEUE_FULL_RETRIES.load(Ordering::Relaxed),
        crate::virtio::gpu::WDDM_PENDING_OVERFLOWS.load(Ordering::Relaxed),
        crate::virtio::gpu::INFLIGHT_HIGH_WATER.load(Ordering::Relaxed),
        crate::virtio::gpu::PARKED_HIGH_WATER.load(Ordering::Relaxed),
        crate::virtio::gpu::PARKED_LEAKS.load(Ordering::Relaxed),
        crate::virtio::gpu::WDDM_FENCE_FROM_DPC.load(Ordering::Relaxed),
        // v4: ring_idx >= 1 GPU-completion fences (WS1 #4).
        crate::virtio::gpu::RING_SUBMIT_COUNT.load(Ordering::Relaxed),
        crate::virtio::gpu::RING_COMPLETE_COUNT.load(Ordering::Relaxed),
        // Retired v5 control-path word retained as zero so every following
        // diagnostic index is stable for existing minidump decoders.
        0,
        // Second retired v5 control-path word retained for the same fixed-index
        // compatibility; neither word is backed by a live outbound carrier.
        0,
        // v6 (R604): split out of FENCE_WAIT_TIMEOUTS (word 25), which now means
        // only "the host did not complete this fence". This word means "all 64
        // waiter slots were taken", a guest table-size condition. The report is
        // decoded offline BY INDEX, so word 25 keeps its meaning and the new
        // word is appended — never renumbered — and the version word above moves
        // with the array in the same commit.
        crate::virtio::gpu::FENCE_WAIT_TABLE_FULL.load(Ordering::Relaxed),
        // v7: D0 provider registration, appended so every older word retains its index.
        etw_registration.attempts,
        etw_registration.failures,
        etw_registration.last_status as u32,
        etw_registration.registered,
        // v8: nonwrapping persistent-SET namespaces and conservative response
        // quarantine. These are occurrence counters, never raw identities.
        crate::virtio::gpu::SCANOUT_BIND_SEQUENCE_EXHAUSTED.load(Ordering::Relaxed),
        crate::virtio::gpu::SCANOUT_TRANSPORT_INSTANCE_EXHAUSTED.load(Ordering::Relaxed),
        crate::virtio::gpu::WIRE_FENCE_NAMESPACE_EXHAUSTED.load(Ordering::Relaxed),
        crate::virtio::gpu::SCANOUT_BIND_AMBIGUOUS_RESPONSES.load(Ordering::Relaxed),
        crate::virtio::gpu::SCANOUT_PUBLICATION_CLAIM_LOST.load(Ordering::Relaxed),
    ];
    let report_bytes = size_of::<[u32; 47]>();
    let copy_len = core::cmp::min(report_bytes, buf_len);
    // SAFETY: copy_len <= BufferSize (writable, checked above) and
    // copy_len <= size_of report (readable local array).
    unsafe {
        core::ptr::copy_nonoverlapping(
            report.as_ptr() as *const u8,
            args.pBuffer as *mut u8,
            copy_len,
        );
    }
    STATUS_SUCCESS
}
