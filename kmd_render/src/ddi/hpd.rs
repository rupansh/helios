//! Hot-plug-detect (HPD) worker for the display half.
//!
//! `DxgkCbIndicateChildStatus` tells the OS the child video-output is *connected*
//! — the transition that makes the VidPn target *available* so dxgkrnl will build a
//! source→target present path (without it the OS negotiates cleanly but commits
//! 0-path VidPns → 0 display paths, 36th-session symptom). It is PASSIVE-only and
//! MUST NOT be called during `DxgkDdiStartDevice`, so a dedicated system thread does
//! it — the viogpu3d `ThreadWorkRoutine`/`ConfigChanged` analog
//! (`viogpu_adapter.cpp:1580,1599`). The thread indicates once shortly after start
//! (an initial connect), then re-indicates on each virtio config-change interrupt
//! (`VIRTIO_GPU_EVENT_DISPLAY`, ISR status bit 1 → DPC → `signal_hpd`).

use core::ffi::c_void;
use core::sync::atomic::Ordering;

use crate::adapter::AdapterContext;
use crate::dxgk::*;
use wdk_sys::ntddk::{KeWaitForSingleObject, PsTerminateSystemThread};

const STATUS_TIMEOUT: NTSTATUS = 0x0000_0102;

/// Bounded fallback for the prologue's wait on "StartDevice has returned".
///
/// This is a SAFETY BOUND, not the mechanism. The mechanism is
/// `AdapterContext::signal_start_complete`, which StartDevice calls as its last
/// action; this only caps how long the worker sits idle if that edge is somehow
/// missed, so the first `Connected=1` still reaches the OS. Keeping the bound is
/// deliberate — the defect was a delay standing in for an event, not the
/// existence of a timeout.
const START_COMPLETE_FALLBACK_100NS: i64 = -5_000_000; // 500 ms, relative

/// Indicate the single child video-output's connection state to the OS. PASSIVE.
fn indicate_child_status(adapter: &AdapterContext, connected: bool) {
    let Some(dxgkrnl) = adapter.dxgkrnl_opt() else {
        return;
    };
    let Some(indicate) = dxgkrnl.DxgkCbIndicateChildStatus else {
        return;
    };
    // SAFETY: an all-zero DXGK_CHILD_STATUS is a valid starting point.
    let mut status: DXGK_CHILD_STATUS = unsafe { core::mem::zeroed() };
    status.Type = _DXGK_CHILD_STATUS_TYPE::StatusConnection;
    status.ChildUid = crate::ddi::vidpn::CHILD_UID;
    // Plain union write (safe): the HotPlug arm is the one StatusConnection uses.
    status.__bindgen_anon_1.HotPlug.Connected = connected as u8;
    // SAFETY: live callback interface; `status` is a fully-initialized child-status
    // packet valid for the synchronous call. PASSIVE_LEVEL (worker thread).
    let st = unsafe { indicate(dxgkrnl.DeviceHandle, &mut status) };
    crate::diag::record_named_bytes(b"HpdI", ((connected as u32) << 16) | (st as u32 & 0xFFFF));
    HPD_INDICATE_COUNT.fetch_add(1, Ordering::Relaxed);
    crate::diag::record_named_bytes(b"HpdN", HPD_INDICATE_COUNT.load(Ordering::Relaxed));
    // Emitted alongside HpdN so the two are always read together: a nonzero
    // HpdStTo means the first indication was driven by the fallback, not the
    // start edge.
    crate::diag::record_named_bytes(b"HpdStTo", HPD_START_EDGE_TIMEOUTS.load(Ordering::Relaxed));
}

/// Count of child-status indications this boot (diag `HpdN`).
static HPD_INDICATE_COUNT: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// Times the prologue's bounded fallback fired instead of the real start edge
/// (diag `HpdStTo`). Must read 0 on a healthy boot: a nonzero value means
/// StartDevice's `signal_start_complete` did not reach the worker, and the first
/// `Connected=1` was as late as it used to be.
static HPD_START_EDGE_TIMEOUTS: core::sync::atomic::AtomicU32 =
    core::sync::atomic::AtomicU32::new(0);

/// The HPD worker thread body (`PsCreateSystemThread`). Runs at PASSIVE_LEVEL for
/// the device's lifetime; `AdapterContext::stop_hpd` signals `hpd_stop` + the wake
/// event and joins it before teardown.
///
/// # Safety
/// `context` is the `AdapterContext` pointer passed to `PsCreateSystemThread`,
/// valid until `stop_hpd` joins this thread.
pub unsafe extern "C" fn hpd_thread_routine(context: *mut c_void) {
    if context.is_null() {
        return;
    }
    // SAFETY: the adapter context is alive until StopDevice joins this thread.
    let adapter = unsafe { &*(context as *const AdapterContext) };
    // SAFETY: this is a `PsCreateSystemThread` body. A system worker thread runs
    // at PASSIVE_LEVEL for its whole life — it is not entered from a DDI, an ISR
    // or a DPC — and every wait below is a `KeWaitForSingleObject` that would
    // already be illegal otherwise. This is the one mint in the driver whose
    // justification is structural rather than a documented DDI annotation, which
    // is why it is minted ONCE here and passed down rather than re-asserted per
    // callee.
    let passive = unsafe { crate::irql::PassiveLevel::assume() };

    // ── Phase 1: wait for StartDevice to return ─────────────────────────────
    //
    // `DxgkCbIndicateChildStatus` is forbidden DURING StartDevice, so the first
    // indication must wait for it to return. That used to be a bare 500 ms
    // relative wait — a delay standing in for an event, with nothing observing
    // the return at all. StartDevice now sets `start_complete` and signals
    // `hpd_event` as its last action, so this wait has a REAL wake source and
    // `START_COMPLETE_FALLBACK_100NS` is only the bound that keeps a missed edge
    // from parking the worker forever.
    //
    // Looped because `hpd_event` is a SynchronizationEvent shared with the
    // scanout/config-change paths: a wake that is not the start edge must not be
    // mistaken for one. The loop is bounded by the same fallback per iteration
    // and by `hpd_stop`.
    while adapter.start_complete.load(Ordering::Acquire) == 0 {
        if adapter.hpd_stop.load(Ordering::Acquire) != 0 {
            break;
        }
        let mut timeout: LARGE_INTEGER = unsafe { core::mem::zeroed() };
        timeout.QuadPart = START_COMPLETE_FALLBACK_100NS;
        // SAFETY: waiting on the initialized hpd_event with a relative timeout.
        let st = unsafe {
            KeWaitForSingleObject(
                adapter.hpd_event.get() as PVOID,
                0, // Executive
                0, // KernelMode
                0, // non-alertable
                &mut timeout,
            )
        };
        if st == STATUS_TIMEOUT {
            // The bound fired. Proceed anyway — a late indication is far better
            // than none — and count it, because it means the edge was missed.
            HPD_START_EDGE_TIMEOUTS.fetch_add(1, Ordering::Relaxed);
            break;
        }
    }
    if adapter.hpd_stop.load(Ordering::Acquire) != 0 {
        // Publish "worker exited" BEFORE terminating, so stop_hpd's join does
        // not depend on ObReferenceObjectByHandle succeeding.
        // SAFETY: initialized NotificationEvent on the adapter, which outlives
        // this thread by the leak rule in stop_hpd.
        unsafe { wdk_sys::ntddk::KeSetEvent(adapter.hpd_exited.get(), 0, 0) };
        // SAFETY: terminates the current system thread; does not return.
        let _ = unsafe { PsTerminateSystemThread(STATUS_SUCCESS) };
        return;
    }
    // SWAP BEFORE indicating, not store(0) after.
    //
    // The old order was: indicate, then unconditionally store 0. An ISR that set
    // `config_change_pending` while `DxgkCbIndicateChildStatus` was in flight had
    // its bit DISCARDED — and because `hpd_event` is a SynchronizationEvent, the
    // DPC's signal then satisfied the loop's first wait, whose own swap returned
    // 0, so nothing was indicated. That host mode change was never re-reported to
    // the OS.
    //
    // Swapping first means a bit set DURING the indication survives to the next
    // iteration and is acted on there. The steady-state loop already had this
    // right; only this one-shot prologue did not.
    adapter.config_change_pending.swap(0, Ordering::AcqRel);
    indicate_child_status(adapter, true);

    // Steady state is edge-driven. Interrupt/DPC completion owns control-ring
    // draining; this thread only handles the OS child-status edge and the one
    // PASSIVE continuation for D4 plane work.
    loop {
        // SAFETY: wait on the initialized synchronization event. No timeout:
        // there is no deferred retry continuation.
        let _ = unsafe {
            KeWaitForSingleObject(
                adapter.hpd_event.get() as PVOID,
                0,
                0,
                0,
                core::ptr::null_mut(),
            )
        };
        if adapter.hpd_stop.load(Ordering::Acquire) != 0 {
            // SAFETY: initialized NotificationEvent on the adapter, which
            // outlives this thread by the leak rule in stop_hpd.
            unsafe { wdk_sys::ntddk::KeSetEvent(adapter.hpd_exited.get(), 0, 0) };
            // SAFETY: terminates the current system thread; does not return.
            let _ = unsafe { PsTerminateSystemThread(STATUS_SUCCESS) };
            return;
        }

        if adapter.config_change_pending.swap(0, Ordering::AcqRel) != 0 {
            indicate_child_status(adapter, true);
        }
        crate::ddi::direct_scanout::service_pending(passive, adapter);
    }
}
