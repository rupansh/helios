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
/// A `ctrl_roundtrip` whose waiter was abandoned because the transport went away
/// mid-wait (StopDevice), rather than because the host timed out. Previously
/// folded into "already completed successfully" by `unwrap_or(true)`.
pub static CTRL_TEARDOWN_ABANDONS: AtomicU32 = AtomicU32::new(0);
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
/// `take_live_resource` misses (duplicate-teardown suppressions). Replaces the
/// in-lock `diag::record(0x0D20_00E0)` breadcrumb.
pub static TAKE_LIVE_MISSES: AtomicU32 = AtomicU32::new(0);
/// `alloc_window_range` refusals (host-visible window offset space exhausted /
/// fragmented past the request). Each one fails the bounded in-kernel map.
pub static WINDOW_ALLOC_REJECTS: AtomicU32 = AtomicU32::new(0);
/// Raise `hw` to at least `n` (relaxed; approximate under concurrency is fine
/// for telemetry).
pub fn bump_high_water(hw: &AtomicU32, n: usize) {
    let n = n as u32;
    if hw.load(Ordering::Relaxed) < n {
        hw.store(n, Ordering::Relaxed);
    }
}
