//! ISR / DPC DDIs — the C3/M3.4 interrupt-driven used-ring drain.
//!
//! The virtio-gpu device is line-based INTx (`MSISupported=0`), i.e. *level*-
//! triggered: it asserts the shared INTx line when it pushes used-ring entries
//! and keeps it asserted until the driver reads the read-to-clear virtio
//! ISR-status register. The ISR reads that register (deasserting the line),
//! claims the interrupt, and queues the DPC via `DxgkCbQueueDpc`; the DPC
//! drains the used ring under the device spinlock (`VirtioGpu::drain_used` —
//! signaling sync/fence KEVENT waiters), marks exact K9 frontier tickets
//! host-terminal, and reports only the contiguous one-engine scheduler frontier
//! (`DXGK_INTERRUPT_DMA_COMPLETED` at DIRQL via `signal_dma_completed`).
//!
//! IRQL: the ISR runs at the device's DIRQL — no allocations, no spinlocks, no
//! pageable calls; it touches only the lock-free published ISR-status VA and
//! the saved dxgkrnl callback table. The DPC runs at DISPATCH_LEVEL.

use core::ffi::c_void;
use core::sync::atomic::{AtomicU32, Ordering};

use helios_kmd_logic::ordered_engine::{CompletionDisposition, FailureDisposition};

use crate::adapter::{AdapterContext, OrderedEngineTicket, WddmNotifyGuard};
use crate::dxgk::*;

// ── DIRQL/DISPATCH-safe instrumentation ──────────────────────────────────────
// `diag::record` is PASSIVE-only (RtlWriteRegistryValue), so the ISR/DPC cannot
// touch the registry ring. These atomics are incremented here at DIRQL/DISPATCH
// and dumped into the PASSIVE diag ring at DxgkDdiDestroyDevice.
pub static INT_ROUTINE_COUNT: AtomicU32 = AtomicU32::new(0);
pub static DPC_ROUTINE_COUNT: AtomicU32 = AtomicU32::new(0);
pub static CONTROL_INT_COUNT: AtomicU32 = AtomicU32::new(0);

/// Ask dxgkrnl to run the normal completion DPC after PASSIVE-side lifecycle
/// code changed a WDDM wait predicate.  The caller has already preserved the
/// producer watermark under `wddm_notify_lock`; queuing the DPC only provides
/// the prompt that lets it observe the now-ready head without waiting for an
/// unrelated virtio interrupt.
pub(crate) fn request_wddm_completion_dpc(adapter: &AdapterContext) {
    let Some(dxgkrnl) = adapter.dxgkrnl_opt() else {
        return;
    };
    let Some(queue_dpc) = dxgkrnl.DxgkCbQueueDpc else {
        return;
    };
    // SAFETY: the callback table belongs to this live adapter, and QueueDpc is
    // callable at PASSIVE/DISPATCH/DIRQL. It does not take wddm_notify_lock.
    unsafe { queue_dpc(dxgkrnl.DeviceHandle) };
}

#[derive(Clone, Copy, Default)]
struct OrderedDrain {
    delivered: u32,
}

/// Report every contiguous host-terminal K9 entry while the one notification
/// lock is held. An early completion remains in its exact slot; a failed
/// callback leaves the same head ready and queues one ordinary DPC retry.
fn drain_ordered_engine_locked(
    adapter: &AdapterContext,
    guard: &WddmNotifyGuard<'_>,
) -> OrderedDrain {
    let Some(dxgkrnl) = adapter.dxgkrnl_opt() else {
        return OrderedDrain::default();
    };
    let mut delivered = 0u32;
    loop {
        let Some(ready) = guard.ordered_engine_ready() else {
            break;
        };
        if !helios_kmd_logic::scanout_lease::fence_is_forward(
            guard.completed_fence(),
            ready.fence(),
        ) {
            // Admission already enforces the same half-range order. Reaching
            // this arm means the frontier and published watermark diverged;
            // poison the exact generation instead of treating an unreported
            // stale value as a successful DMA completion.
            super::submit_command::DMA_STALE_SKIP_COUNT.fetch_add(1, Ordering::Relaxed);
            let _ = guard.fail_ordered_engine_submission(ready.ticket());
            break;
        }
        // SAFETY: the WDDM notification lock is held; `ready.fence()` is the
        // exact OS fence admitted at SubmitCommand arrival and is currently the
        // ordered engine head.
        let status = unsafe {
            super::submit_command::signal_dma_completed(guard, dxgkrnl, ready.fence())
        };
        if status != STATUS_SUCCESS {
            super::submit_command::DMA_NOTIFY_FAILS.fetch_add(1, Ordering::Relaxed);
            guard.note_ordered_engine_notify_retry();
            if let Some(queue_dpc) = dxgkrnl.DxgkCbQueueDpc {
                // SAFETY: callable at <= DIRQL for this live adapter. QueueDpc
                // does not acquire `wddm_notify_lock`.
                unsafe { queue_dpc(dxgkrnl.DeviceHandle) };
            }
            break;
        }
        if !guard.retire_ordered_engine_ready(ready) {
            break;
        }
        delivered = delivered.saturating_add(1);
    }
    // Publish the delivered edge while the same notification guard still
    // excludes reset invalidation. Reset can therefore either clear this old
    // generation's edge or run wholly before it; an old DPC cannot republish a
    // native-fence rescan after the successor engine has opened.
    guard.note_ordered_engine_native_rescan(delivered);
    OrderedDrain { delivered }
}

/// Retry the K7 empty-array native-fence rescan only downstream of a
/// successfully delivered DMA frontier edge. The count remains published until
/// dxgkrnl accepts the callback; success subtracts only the observed prefix.
fn service_native_fence_rescans(
    adapter: &AdapterContext,
    guard: &WddmNotifyGuard<'_>,
) {
    // Keep pending nonzero until dxgkrnl accepts the edge. The guard serializes
    // DPCs and reset; exchange-before-callback would instead require restoring
    // credit after failure and risks carrying an old edge into a successor.
    let pending = guard.ordered_engine_native_rescans();
    if pending == 0 {
        return;
    }
    if !super::native_fence::has_possible_progress_edge(adapter) {
        return;
    }
    let Some(dxgkrnl) = adapter.dxgkrnl_opt() else {
        return;
    };
    let status = unsafe { super::native_fence::signal_native_fence_signaled(adapter, dxgkrnl) };
    if status != STATUS_SUCCESS {
        request_wddm_completion_dpc(adapter);
    } else {
        guard.retire_ordered_engine_native_rescans(pending);
    }
}

/// Complete one exact direct host callback ticket. This is the K9 seam used by
/// K11's already-terminal control submission and by the later nonzero-endpoint
/// executor: neither source can notify VidSch except through this frontier.
pub(crate) fn complete_ordered_engine_submission(
    adapter: &AdapterContext,
    ticket: OrderedEngineTicket,
) -> CompletionDisposition {
    let (disposition, delivered) = adapter.with_wddm_notify_lock(|guard| {
        let disposition = guard.mark_ordered_engine_host_completed(ticket);
        let delivered = match disposition {
            CompletionDisposition::Marked { .. }
            | CompletionDisposition::AlreadyCompleted => {
                drain_ordered_engine_locked(adapter, guard).delivered
            }
            CompletionDisposition::StaleEpoch
            | CompletionDisposition::StaleTicket
            | CompletionDisposition::Poisoned => 0,
        };
        service_native_fence_rescans(adapter, guard);
        (disposition, delivered)
    });
    if delivered != 0 {
        // A direct completion can retire a later compatibility ticket that an
        // earlier DPC already found host-terminal and requeued behind this
        // ticket.  Prompt the ordinary DPC to discharge that FIFO's separate
        // ownership token; no new virtio interrupt is guaranteed after the
        // direct head closes.
        request_wddm_completion_dpc(adapter);
    }
    disposition
}

/// Poison only the current ticket's engine generation. A callback carrying an
/// old reset generation is inert and cannot poison its successor.
pub(crate) fn fail_ordered_engine_submission(
    adapter: &AdapterContext,
    ticket: OrderedEngineTicket,
) -> FailureDisposition {
    adapter.with_wddm_notify_lock(|guard| guard.fail_ordered_engine_submission(ticket))
}

/// Drain used control-queue entries, apply any completed DISPATCH-level scan-out
/// bind, and retire any WDDM fences whose Venus watermark is now complete.
/// Normally called from the device DPC; the scanout worker may also call it at
/// PASSIVE_LEVEL as a bounded fallback while one fire-and-forget RESOURCE_FLUSH
/// is outstanding. Keeping fence retirement here prevents an opportunistic drain
/// from consuming a Venus completion without notifying VidSch.
///
/// The bind application lives HERE rather than in `drain_used` because of the
/// lock order — see the comment on it below.
pub(crate) fn drain_used_and_complete(adapter: &AdapterContext) {
    let _ = adapter.with_virtio(|transport| transport.drain_used(adapter));
    crate::ddi::native_render::drain_host_terminals(adapter);

    adapter.with_wddm_notify_lock(|guard| {
        // One compatibility Venus entry at a time. Its direct K9 ticket was
        // admitted in SubmitCommand order; readiness marks that exact ticket.
        loop {
            let taken = guard
                .with_virtio(|order, transport| transport.take_one_ready_wddm(order))
                .unwrap_or(crate::virtio::WddmTake::Empty);
            let ready = match taken {
                crate::virtio::WddmTake::Ready(ready) => ready,
                crate::virtio::WddmTake::Empty
                | crate::virtio::WddmTake::BlockedOnProducer => break,
            };
            let ticket = ready.engine_ticket();
            let disposition = guard.mark_ordered_engine_host_completed(ticket);
            let mark_owned = matches!(
                disposition,
                CompletionDisposition::Marked { .. }
                    | CompletionDisposition::AlreadyCompleted
            );
            if mark_owned {
                let _ = drain_ordered_engine_locked(adapter, guard);
            }
            if mark_owned && !guard.ordered_engine_ticket_is_live(ticket) {
                ready.delivered();
                continue;
            }
            if matches!(disposition, CompletionDisposition::StaleTicket)
                && guard.ordered_engine_ticket_was_retired(ticket)
            {
                ready.delivered();
                continue;
            }

            // Preserve the exact FIFO owner when the K9 frontier cannot yet
            // consume it. Bypassing this entry would violate engine order.
            let _ = guard.with_virtio(|order, transport| {
                transport.requeue_wddm_front(order, ready)
            });
            break;
        }

        let _ = drain_ordered_engine_locked(adapter, guard);
        service_native_fence_rescans(adapter, guard);
    });
}
/// `DxgkDdiInterruptRoutine` — runs at the device's DIRQL; returns TRUE if the
/// interrupt was ours.
//
// Read the ISR-status register (which DEASSERTS the level-triggered line), and
// if a bit was pending, claim the interrupt and queue the DPC. Without the
// read-to-clear, the line stays asserted and Windows' interrupt-storm detector
// disables the adapter (observed pre-fix: ~10000 unclaimed ISR calls → Code 43).
// The register VA is published lock-free by StartDevice (`isr_status`, Release)
// BEFORE the transport goes live, and `adapter.dxgkrnl` is written before that
// — so a nonzero `isr_status` implies a valid callback table.
pub unsafe extern "C" fn dxgkddi_interrupt_routine(
    miniport_device_context: *mut c_void,
    _message_number: u32,
) -> BOOLEAN {
    if miniport_device_context.is_null() {
        return 0;
    }
    // SAFETY: dxgkrnl passes our AdapterContext as the miniport device context;
    // it is valid for the device's lifetime and `isr_status` is an atomic.
    let adapter = unsafe { &*(miniport_device_context as *const AdapterContext) };
    let isr_va = adapter.isr_status.load(Ordering::Acquire);
    if isr_va == 0 {
        // Transport not up yet (or torn down): not in a position to claim it.
        return 0;
    }
    // SAFETY: `isr_va` is the mapped MMIO VA of the 1-byte read-to-clear
    // ISR-status register, published by StartDevice; the read clears + deasserts.
    let status = unsafe { core::ptr::read_volatile(isr_va as *const u8) };
    if status == 0 {
        // Shared line, but no virtio interrupt pending — not ours.
        return 0;
    }
    INT_ROUTINE_COUNT.fetch_add(1, Ordering::Relaxed);
    // Bit 1 = configuration change: the virtio-gpu raises it on a
    // VIRTIO_GPU_EVENT_DISPLAY (monitor connect / mode change). Latch it for the
    // DPC, which wakes the HPD worker to (re-)indicate the child connected — the
    // viogpu3d ISR_REASON_CHANGE path (`viogpu_adapter.cpp:1531`).
    if status & 0x2 != 0 {
        adapter.config_change_pending.store(1, Ordering::Release);
    }
    // Bit 0 = used-ring progress (drain), bit 1 = config change: either needs the
    // DPC. (The ISR-status read above already deasserted the line for both.)
    if status & 0x3 != 0 {
        if let Some(dxgkrnl) = adapter.dxgkrnl_opt() {
            if let Some(queue_dpc) = dxgkrnl.DxgkCbQueueDpc {
                // SAFETY: DxgkCbQueueDpc is callable from the ISR at DIRQL;
                // DeviceHandle is the live dxgkrnl device handle.
                unsafe { queue_dpc(dxgkrnl.DeviceHandle) };
            }
        }
    }
    1 // claimed + acknowledged (line now deasserted)
}

/// `DxgkDdiDpcRoutine` — runs at DISPATCH_LEVEL after the ISR (or a
/// `signal_dma_completed` notify pair) queues a DPC.
pub unsafe extern "C" fn dxgkddi_dpc_routine(miniport_device_context: *mut c_void) {
    DPC_ROUTINE_COUNT.fetch_add(1, Ordering::Relaxed);
    if miniport_device_context.is_null() {
        return;
    }
    // SAFETY: our AdapterContext, valid for the device's lifetime.
    let adapter = unsafe { &*(miniport_device_context as *const AdapterContext) };

    // A latched config-change (ISR bit 1): wake the HPD worker to re-indicate the
    // child connected. KeSetEvent (Wait=FALSE) is legal at DISPATCH_LEVEL.
    if adapter.config_change_pending.load(Ordering::Acquire) != 0 {
        adapter.signal_hpd();
    }

    // Let dxgkrnl process any interrupt data queued by DxgkCbNotifyInterrupt
    // (the WDDM fence completions signaled below re-queue this DPC, and this
    // call drains their packets — the viogpu3d NotifyDpcRoutine ordering).
    if let Some(dxgkrnl) = adapter.dxgkrnl_opt() {
        if let Some(notify_dpc) = dxgkrnl.DxgkCbNotifyDpc {
            // SAFETY: DISPATCH_LEVEL DPC context; live device handle.
            unsafe { notify_dpc(dxgkrnl.DeviceHandle) };
        }
    }

    // Drain the used ring, wake ctrl/fence waiters, and retire every WDDM
    // submission whose Venus watermark has been reached.
    drain_used_and_complete(adapter);
}

/// `DxgkDdiControlInterrupt` — enable/disable a class of GPU interrupts. Called at
/// up to DIRQL, so this path touches only atomics (no registry / pageable calls).
//
// The OS drives CRTC_VSYNC here. The display half (DisplayHalf on) services it: it
// toggles the free-running VSync heartbeat's delivery gate and returns SUCCESS.
// A render-only adapter (0 video-present sources) drives no VSYNC → NOT_IMPLEMENTED
// (MSDN requires that for any type the driver does not service); the virtio
// used-ring interrupt is not an OS-controlled class.
pub unsafe extern "C" fn dxgkddi_control_interrupt(
    h_adapter: IN_CONST_HANDLE,
    interrupt_type: IN_CONST_DXGK_INTERRUPT_TYPE,
    enable: IN_BOOLEAN,
) -> NTSTATUS {
    CONTROL_INT_COUNT.fetch_add(1, Ordering::Relaxed);
    let p = h_adapter as *const AdapterContext;
    if !p.is_null()
        && interrupt_type == _DXGK_INTERRUPT_TYPE::DXGK_INTERRUPT_CRTC_VSYNC
        // SAFETY: dxgkrnl hands our AdapterContext; `display_half` is a plain bool
        // set once at StartDevice.
        && unsafe { (*p).display_half() }
    {
        // SAFETY: valid for the device lifetime.
        let adapter = unsafe { &*p };
        adapter
            .vsync_enabled
            .store((enable != 0) as u32, Ordering::Release);
        return STATUS_SUCCESS;
    }
    STATUS_NOT_IMPLEMENTED
}
