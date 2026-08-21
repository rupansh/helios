//! Internal transport telemetry consumed by the OS-invoked bounded debug dump.
//!
//! Moved verbatim out of `virtio/gpu.rs` by T8/R1103, which re-exports this
//! module wholesale so the 53+ external `gpu::<COUNTER>` paths are unchanged.

use core::sync::atomic::{AtomicU32, Ordering};

/// Count of synchronous control-command timeouts (a passive waiter gave up and
/// abandoned its in-flight slot). Unlike the old model this does NOT poison
/// the transport — the slot is reaped when the completion eventually arrives —
/// but nonzero still means the host stopped answering in time. Read by
/// the OS-invoked bounded `DxgkDdiCollectDbgInfo` report (acceptance: stays 0).
pub static CTRL_TIMEOUT_COUNT: AtomicU32 = AtomicU32::new(0);

// ── C3/M3.4 async-transport telemetry (all DISPATCH-safe atomics) ────────────

/// Async SUBMIT_3D enqueues.
pub static ASYNC_SUBMIT_COUNT: AtomicU32 = AtomicU32::new(0);
/// Async SUBMIT_3D completions drained from the used ring.
pub static ASYNC_COMPLETE_COUNT: AtomicU32 = AtomicU32::new(0);
/// Async SUBMIT_3D completions whose ctrl response was not RESP_OK.
pub static ASYNC_RESP_ERRORS: AtomicU32 = AtomicU32::new(0);
/// WAIT_FENCE waiters registered (fence was still in flight at wait time).
pub static FENCE_WAIT_REGISTERED: AtomicU32 = AtomicU32::new(0);
/// WAIT_FENCE waits that timed out — the host did not complete this fence.
///
/// This is the counter the project reads as fence evidence and dumps into the
/// TDR report, so it must mean exactly one thing. It used to also count
/// [`FENCE_WAIT_TABLE_FULL`]'s condition, where the host is perfectly healthy.
pub static FENCE_WAIT_TIMEOUTS: AtomicU32 = AtomicU32::new(0);
/// WAIT_FENCE waits that gave up because all [`MAX_FENCE_WAITERS`] slots were
/// occupied for the whole retry budget — a *guest* table-size condition, not a
/// host one, and the fix is a bigger table rather than a host investigation.
///
/// `gpu.rs`'s own comment on `MAX_FENCE_WAITERS` says dwm plus several apps plus
/// WUDFHost are expected concurrently, so the 65th waiter is a reachable state.
/// Before the split it incremented [`FENCE_WAIT_TIMEOUTS`], so post-mortem
/// evidence could not distinguish a wedged host from a table that needs
/// resizing — and the TDR report blamed the host.
pub static FENCE_WAIT_TABLE_FULL: AtomicU32 = AtomicU32::new(0);
/// A `ctrl_roundtrip` whose waiter was abandoned because the transport went away
/// mid-wait (StopDevice), rather than because the host timed out. Previously
/// folded into "already completed successfully" by `unwrap_or(true)`.
pub static CTRL_TEARDOWN_ABANDONS: AtomicU32 = AtomicU32::new(0);
/// A `wait_fence` that found the transport gone. Previously reported as
/// `Complete`, which made `escape_wait_fence` tell the ICD that a wire fence had
/// retired when it never did.
pub static TRANSPORT_GONE_AT_WAIT: AtomicU32 = AtomicU32::new(0);
/// Used-ring completions whose token matched no in-flight entry (ring state
/// corrupt → transport latches failed).
pub static DRAIN_BAD_TOKEN: AtomicU32 = AtomicU32::new(0);
/// Used-ring completions whose reported output length exceeded the exact
/// response descriptor capacity (ring state corrupt -> transport latches).
pub static DRAIN_BAD_USED_LENGTH: AtomicU32 = AtomicU32::new(0);
/// `SET_SCANOUT_BLOB` completions whose shape cannot prove success or a
/// documented rejection; their publication ownership stays quarantined.
pub static SCANOUT_BIND_AMBIGUOUS_RESPONSES: AtomicU32 = AtomicU32::new(0);
/// `SET_SCANOUT_BLOB` commands refused before queue publication because their
/// transport-generation sequence high-water reached `u64::MAX`.
pub static SCANOUT_BIND_SEQUENCE_EXHAUSTED: AtomicU32 = AtomicU32::new(0);
/// `VirtioGpu::init` attempts refused before PCI/device mutation because the
/// driver-global scanout transport-instance namespace cannot advance.
pub static SCANOUT_TRANSPORT_INSTANCE_EXHAUSTED: AtomicU32 = AtomicU32::new(0);
/// `VirtioGpu::init` attempts refused before PCI/device mutation because no
/// disjoint wire-fence range remains.
pub static WIRE_FENCE_NAMESPACE_EXHAUSTED: AtomicU32 = AtomicU32::new(0);
/// Persistent SET descriptors accepted by the queue when the supposedly-free
/// publication transaction refused its exact claim. The request is retained
/// and the generation is ambiguity-sealed; nonzero is an internal invariant
/// failure, not host rejection.
pub static SCANOUT_PUBLICATION_CLAIM_LOST: AtomicU32 = AtomicU32::new(0);
/// Enqueue attempts refused because the queue/parked tables were full.
pub static QUEUE_FULL_RETRIES: AtomicU32 = AtomicU32::new(0);
/// WDDM pending-fence FIFO overflows (degraded to immediate completion).
pub static WDDM_PENDING_OVERFLOWS: AtomicU32 = AtomicU32::new(0);
/// High-water of concurrently in-flight control-queue entries.
pub static INFLIGHT_HIGH_WATER: AtomicU32 = AtomicU32::new(0);
/// High-water of parked (completed, awaiting PASSIVE free) entries.
pub static PARKED_HIGH_WATER: AtomicU32 = AtomicU32::new(0);
/// Parked entries force-forgotten because the parked table was full (leaked
/// DMA memory — must stay 0; the enqueue gate makes this unreachable).
pub static PARKED_LEAKS: AtomicU32 = AtomicU32::new(0);
/// WDDM submissions completed by the DPC (real venus-driven fences).
pub static WDDM_FENCE_FROM_DPC: AtomicU32 = AtomicU32::new(0);
/// WDDM heads still waiting on a real wire producer when the DPC looked.
pub static WDDM_HEAD_BLOCKED_WIRE: AtomicU32 = AtomicU32::new(0);

/// Fenced SUBMIT_3D enqueues carrying ring_idx >= 1 (GPU-completion fences —
/// WS1 #4 consumer-side ordering; these retire at host GPU completion, not
/// decode, so they legally stay in flight for the full GPU-work duration).
///
/// Adapter-wide: internal scanout and ordinary Present copies both contribute.
pub static RING_SUBMIT_COUNT: AtomicU32 = AtomicU32::new(0);
/// ring_idx >= 1 completions drained from the used ring. Same internal
/// producers as [`RING_SUBMIT_COUNT`]; `RngSub - RngCmp` is the in-flight
/// ring-1 depth, and a permanent gap means ring-1 work that never retired.
pub static RING_COMPLETE_COUNT: AtomicU32 = AtomicU32::new(0);

/// Guest-supplied completion boundaries REPLACED by `next_wire_fence` because
/// they were zero-or-beyond the fences this driver has actually assigned.
///
/// The clamp itself is old and correct — *"a malformed/stale private marker must
/// not manufacture an impossible future dependency"* — and until 2026-08-06 it
/// was silent, which made the one lossy step on the boundary path invisible: the
/// WDDM fence then reports a watermark the writer never named. Both writers reach
/// it, `PresentSubmissionPrivate`'s BLT fence and `HeliosD3D12SubmitCmd`'s
/// `gpu_wire_fence`, so it is not named after either.
///
/// ⛔ WHOSE ACTIVITY INCREMENTS IT: **BOTH WRITERS' — SO DWM'S D3D11 PRESENTS
/// MOVE IT WITH NO D3D12 CLIENT ON THE BOX.** The rejection is decided in
/// `wddm_boundary::select` BEFORE the `d3d12` bit is consulted, so a
/// `PresentSubmissionPrivate` BLT marker naming a stale id lands here exactly as a
/// `HeliosD3D12SubmitCmd` would. An earlier grading said a nonzero value means
/// *"the UMD named a fence id this KMD never issued"* — there is no "the UMD"
/// here, and reading it as a D3D12 finding attributes the present path's clamps
/// to `helios_umd12.dll`.
///
/// ⚠ NOT RESET AT StartDevice (a plain image-lifetime static), while the standard
/// deploy is `pnputil /restart-device`. A value therefore spans every device
/// generation since the image loaded, and CLAUDE.md rule 6 applies in full:
/// verify it MOVES within the window you are attributing before reading anything
/// into it.
///
/// GRADING, honestly: **a DELTA over a window in which only the D3D12 client
/// changed** is the only attributable form. A nonzero delta then means that client
/// sampled a fence id this KMD has not issued (a stale sample, or a value from
/// another transport generation), and the packet fell back to waiting on the whole
/// backlog instead of its own work — slower, never wedged. `RngSub`/`EscSubRing`
/// (as deltas, see their block above) say whether such a fence could exist at all.
/// ⛔ An absolute reading cannot separate the two writers and must not be quoted
/// as a D3D12 number.
pub static GPU_FENCE_CLAMPED: AtomicU32 = AtomicU32::new(0);
/// Guest-supplied completion boundaries REJECTED because they name a fence from a
/// FOREIGN transport generation — below this instance's `wire_fence_base`.
///
/// A6 (`docs/dx12/PENDING.md` §1). [`GPU_FENCE_CLAMPED`]'s condition is
/// `id == 0 || id >= next_wire_fence`, a one-sided bound, and every StartDevice
/// strides the id range up by 2^32 — so a fence sampled before a
/// StopDevice/StartDevice cycle sits BILLIONS below the new range, passes that
/// bound, matches no in-flight entry, and satisfies the dependency instantly. The
/// old code could not tell that apart from a boundary that had genuinely retired:
/// the WDDM fence completed early and `GpuFncClamp` stayed at 0.
///
/// ⚠ Exactly one of the two counters moves per rejected boundary; they are not
/// summable into "bad boundaries" without double-counting neither, but a nonzero
/// reading in either means the packet fell back to the conservative
/// `next_wire_fence` prefix rather than to the boundary the writer named.
///
/// ⛔ WHOSE ACTIVITY INCREMENTS IT: **BOTH WRITERS' — DWM'S D3D11 PRESENT BLT
/// MARKER REACHES THIS ARM TOO**, for the same reason as [`GPU_FENCE_CLAMPED`]:
/// the generation test runs inside `wddm_boundary::select` before the `d3d12` bit
/// is looked at. A survivor of a device restart is far more likely to be DWM (it
/// is the longest-lived D3D client on the box and it is not restarted by
/// `pnputil /restart-device`) than a freshly launched D3D12 probe.
///
/// ⛔⛔ GRADING, CORRECTED — **"0 on any session without a device restart" IS NOT
/// EVALUABLE FROM THIS COUNTER.** It is a plain image-lifetime static with NO
/// reset at StartDevice, and the standard deploy IS `pnputil /restart-device`. So
/// after the first restart in a boot the value is permanently nonzero and stays
/// nonzero for every later reading, whether or not the session being graded had a
/// restart of its own. The counter cannot tell you which generation its
/// increments came from.
///
/// ⇒ The evaluable forms:
///   * a DELTA across a window with NO device restart in it — must be 0. Nonzero
///     there means some client is sampling fence ids that did not come from this
///     transport, which is the different and worse finding;
///   * a DELTA across a window that DID contain a restart — a small number is the
///     surviving-client case the striding comment at `VirtioGpu::init` describes,
///     and it is expected;
///   * the absolute value — says only "at least one restart has happened since
///     this driver image loaded", which the boot log already says better.
pub static GPU_FENCE_FOREIGN_GENERATION: AtomicU32 = AtomicU32::new(0);
/// Bounded KMD wait refusals of a fence id from a foreign transport generation
/// — the same one-sided-bound defect as [`GPU_FENCE_FOREIGN_GENERATION`].
///
/// Before this counter existed those calls returned `Complete` / `AlreadyComplete`
/// for such an id: the ICD was told a wire fence had retired when its whole
/// transport generation was gone, which is [`TRANSPORT_GONE_AT_WAIT`]'s failure
/// dressed as success.
///
/// The sole increment site is `VirtioGpu::fence_wait_prepare`, used by bounded
/// in-kernel display and Venus waits. It is not a user-mode discovery surface.
///
/// ⛔⛔ GRADING: **the same correction as [`GPU_FENCE_FOREIGN_GENERATION`] —
/// "0 without a device restart" IS NOT EVALUABLE from the counter.** No reset at
/// StartDevice + `pnputil /restart-device` as the standard deploy ⇒ permanently
/// nonzero after the first restart in a boot. Only a delta over a restart-free
/// window can be graded, and only that delta must be 0.
///
/// ⚠ It is NOT expected to track [`GPU_FENCE_FOREIGN_GENERATION`]'s value: the two
/// paths have different callers (the WDDM boundary versus in-kernel waits).
pub static FENCE_ID_FOREIGN_GENERATION: AtomicU32 = AtomicU32::new(0);
/// Reused PASSIVE-allocated command buffers served from the bounded DMA pool.
pub static DMA_POOL_HITS: AtomicU32 = AtomicU32::new(0);
/// Command buffers that required a fresh PASSIVE allocation.
pub static DMA_POOL_MISSES: AtomicU32 = AtomicU32::new(0);
/// Completed buffers not cached because they exceeded a pool bound.
pub static DMA_POOL_DROPS: AtomicU32 = AtomicU32::new(0);
/// Current bytes retained in the DMA pool (page-rounded capacities).
pub static DMA_POOL_CACHED_BYTES: AtomicU32 = AtomicU32::new(0);

/// WDDM fences signalled immediately because the transport had already latched
/// its ring-corruption failure. Each one also cleared the pending FIFO, so a
/// nonzero value means the driver chose a TDR-visible completion over an
/// undrainable queue.
pub static WDDM_SIGNAL_AFTER_FAILURE: AtomicU32 = AtomicU32::new(0);
/// Parked reaps abandoned between `begin_parked_reap` and `finish_parked_reap`.
/// Exists so a future strand shows up as itself rather than as a generic
/// `QUEUE_FULL_RETRIES` climb.
pub static REAP_ABANDONED: AtomicU32 = AtomicU32::new(0);
// ── DISPATCH-safe resource-table telemetry ──────────────────────────────────
// All updated under the device spinlock (DISPATCH_LEVEL), so they must be
// atomics, never `diag::record` (RtlWriteRegistryValue is PASSIVE-only — the
// same latent-IRQL class the 2026-07-03 audit removed from the venus client).
// Read only by the OS-invoked `DxgkDdiCollectDbgInfo` callback.

/// High-water of `blobs.len()` since driver start.
pub static BLOB_HIGH_WATER: AtomicU32 = AtomicU32::new(0);
/// Internal blob tracking attempts rejected because the table was full.
pub static BLOB_FULL_REJECTS: AtomicU32 = AtomicU32::new(0);
/// High-water of `resources.len()` since driver start.
pub static RESOURCE_HIGH_WATER: AtomicU32 = AtomicU32::new(0);
/// resource_create_blob attempts rejected because the live-resource table was full.
pub static RESOURCE_FULL_REJECTS: AtomicU32 = AtomicU32::new(0);
/// Context tracking slots dropped because the context table was full.
pub static CONTEXT_FULL_DROPS: AtomicU32 = AtomicU32::new(0);
/// Freed window ranges dropped because the free list was full (leaked offsets).
pub static WINDOW_RANGE_DROPS: AtomicU32 = AtomicU32::new(0);
/// `configure_window_reserve` refusals — offsets had already been issued. Must
/// stay 0: a nonzero value means someone tried to move the VidMm partition out
/// from under live mappings.
pub static WINDOW_RECONFIG_REFUSED: AtomicU32 = AtomicU32::new(0);
/// `take_live_resource` misses (duplicate-teardown suppressions). Replaces the
/// in-lock `diag::record(0x0D20_00E0)` breadcrumb.
pub static TAKE_LIVE_MISSES: AtomicU32 = AtomicU32::new(0);
/// `alloc_window_range` refusals (host-visible window offset space exhausted /
/// fragmented past the request). Each one fails the bounded in-kernel map.
pub static WINDOW_ALLOC_REJECTS: AtomicU32 = AtomicU32::new(0);
/// Stale-overlap scans that found more overlapping window placements than the
/// caller's fixed buffer could hold. Nonzero means an eviction pass ran against
/// an incomplete list and the map that followed it was REFUSED rather than
/// allowed to create an overlapping host window subregion.
pub static WINDOW_OVERLAP_TRUNCATED: AtomicU32 = AtomicU32::new(0);

/// The stale-overlap scan could not report every overlapping placement.
///
/// A distinct type rather than a `usize` the caller may ignore: acting on a
/// truncated list is what creates two host resources through one window
/// subregion.
#[derive(Clone, Copy, Debug)]
pub struct WindowOverlapTruncated;

/// Raise `hw` to at least `n` (relaxed; approximate under concurrency is fine
/// for telemetry).
pub fn bump_high_water(hw: &AtomicU32, n: usize) {
    let n = n as u32;
    if hw.load(Ordering::Relaxed) < n {
        hw.store(n, Ordering::Relaxed);
    }
}
