//! K6 — the HVC1 native render path.
//!
//! Every decision with a right and a wrong answer is
//! [`helios_kmd_logic::native_render`], host-tested against A2's real encoder
//! corpus (`tools/hnr2-decode-gate.sh`); this file does the pointer work, the
//! SEH-guarded user copy, the locking and the counters. `helios_protocol` owns
//! the wire records — nothing here re-declares one.
//!
//! # The seam with K5 and K11
//!
//! K5 owns `DxgkDdiCreateContext`'s private-data dispatch and the HTS1 session
//! state; K6 owns the HVC1 **queue** context, the per-context HNR2 assembler,
//! and the Render/Patch/SubmitCommand arms. K11 advances exactly one boundary:
//! the fixed pure-control HTS1 INIT gets a private stock-Venus context and a
//! host-derived HVR1 reply. There is still no general Venus opcode schema,
//! allocation resid substitution, or GPU queue executor, so every other host
//! handoff remains a named refusal rather than a silent gap.

use core::cell::UnsafeCell;
use core::ffi::c_void;
use core::mem::size_of;
use core::sync::atomic::{AtomicU32, Ordering};

use alloc::vec::Vec;

use helios_kmd_logic::native_render::{
    admit_commit_tables, admit_hos1, apply_placement, apply_render_fragment, build_capability,
    plan_capability_table, plan_output_patch_slots, snapshot_placement, validate_render_fragment,
    CapabilityTablePlan, OuterSubmitContext, RenderContext, RenderEnv, RenderRefusal,
};
use helios_kmd_logic::session_transport::{HostSubmissionRefusal, HostSubmissionState};
use helios_protocol::native_render::kernel_dma::{
    Hnr2DmaReject, Hnr2KmdDmaPrivateV1, Hnr2PhysicalCapability,
    HELIOS_HNR2_KMD_DMA_FLAG_HOST_COMPLETED,
};
use helios_protocol::native_render::{
    HeliosNativeRenderPatch, HeliosNativeRenderUse, HeliosNativeRenderV2, Hnr2Accept, Hnr2Reject,
    Hnr2TableReject, HELIOS_HNR2_HEADER_SIZE, HELIOS_HNR2_MAX_PATCH_RECORDS,
    HELIOS_HVC1_ALLOCATION_LIST_ENTRIES,
};
use helios_protocol::wddm::HeliosOuterSubmitV1;

use crate::dxgk::*;
use crate::sync::SpinLock;

// ── Named counters ───────────────────────────────────────────────────────────
//
// Every refusal below increments exactly one of these (CLAUDE.md rule 2).
// `DxgkDdiSubmitCommand` runs at DISPATCH_LEVEL, so these are atomics and the
// registry mirror happens on the PASSIVE cadences only:
// [`diag_dump_native_render_atomics`] from `dxgkddi_destroy_device`, and the
// block's own throttled flush from `DxgkDdiRender` (documented PASSIVE_LEVEL,
// `d3dkmddi.h:160`).

/// HVC1 **queue** contexts admitted. K6's first proof of life: it can only move
/// once `ContextRequest::HeliosQueue` exists, and nothing in the shipping ICD
/// asks for one until mesa A3 — so before that, only `tools/hnr2_native_probe.c`
/// moves it.
pub static NR2_QUEUE_CTX: AtomicU32 = AtomicU32::new(0);
/// Queue contexts refused. Today the only cause is a raw device with no HVC1
/// control context, i.e. no session to hold a reference to.
pub static NR2_QUEUE_CTX_REJECT: AtomicU32 = AtomicU32::new(0);
/// Native contexts refused at create because their COMMIT scratch could not be
/// allocated. **Must read 0** — a nonzero value is 228 KiB of nonpaged pool
/// unavailable at `DxgkDdiCreateContext`, which is a machine under memory
/// pressure rather than a driver defect, but it fails context creation.
pub static NR2_SCRATCH_ALLOC_FAILED: AtomicU32 = AtomicU32::new(0);

/// HNR2 fragments admitted, all classes.
pub static NR2_FRAGMENTS: AtomicU32 = AtomicU32::new(0);
/// COMMITs admitted — batches that closed with legal tables.
pub static NR2_COMMITS: AtomicU32 = AtomicU32::new(0);
/// Zero-length Renders taken as the §10.7 capacity renegotiation
/// (`vn_helios_native_kmt.c:764`): a counted no-op, never a batch.
pub static NR2_RESIZE: AtomicU32 = AtomicU32::new(0);
/// HNR2 Renders refused, packed `(count << 16) | RenderRefusal::code()`.
pub static NR2_REJECT: AtomicU32 = AtomicU32::new(0);
/// A second `DxgkDdiRender` arrived on a context whose first had not returned.
/// §10.7 permits one incomplete batch per context, so this is a producer bug;
/// it is refused rather than serialized because the alternative is a ~228 KiB
/// copy under a DISPATCH-level spinlock.
pub static NR2_REENTRANT: AtomicU32 = AtomicU32::new(0);
/// Output `D3DDDI_PATCHLOCATIONLIST` entries emitted — one per COMMIT use
/// record, never one per typed operand.
pub static NR2_PATCH_SLOTS: AtomicU32 = AtomicU32::new(0);
/// `DxgkDdiPatch` calls that took the HNR2 arm.
pub static NR2_PATCH_CALLS: AtomicU32 = AtomicU32::new(0);
/// Capability records whose placement Patch snapshotted in.
pub static NR2_PATCH_SNAPS: AtomicU32 = AtomicU32::new(0);
/// A patch-location entry that does not name a capability slot, or a submission
/// whose header witness does not match its private record. **Must read 0**:
/// every entry Patch sees was written by this driver's own Render.
///
/// ⚠ IT DOES NOT COUNT A RELOCATION. An allocation that moved between Render and
/// Patch is the single event `DxgkDdiPatch` exists for; it lands in
/// [`NR2_PATCH_RELOCATED`] and is expected to move.
pub static NR2_PATCH_DIFF: AtomicU32 = AtomicU32::new(0);
/// Capability records whose placement CHANGED at Patch — VidMm evicted and
/// re-paged the allocation between Render and Patch. Expected nonzero under
/// memory pressure; it is information, not a fault.
pub static NR2_PATCH_RELOCATED: AtomicU32 = AtomicU32::new(0);
/// `DxgkDdiSubmitCommand` calls carrying `DXGK_SUBMITCOMMANDFLAGS::Resubmission`
/// — dxgkrnl re-running a packet it preempted.
///
/// ⛔ THE STAGING RETIREMENT IS SKIPPED FOR THESE, and that is the whole reason
/// the bit is read. §10.7:1886 is "a COMMIT transitions once from staged to
/// submitted": the private record is per-DMA-buffer state dxgkrnl does not
/// change, so a resubmission re-reads the same `payload_bytes` and a second
/// `retire()` would consume a DIFFERENT live submission's accounting — leaking
/// the pool upward until legitimate COMMITs are refused, with `Nr2Slot` and
/// `Nr2SlotRet` staying equal the whole time and the documented pair-grading
/// reporting health.
pub static NR2_SUBMIT_RESUBMISSION: AtomicU32 = AtomicU32::new(0);
/// Private-record reads that took the offset-0 fallback because dxgkrnl
/// reported an EMPTY submission window over a buffer holding exactly one record.
///
/// ⚠ GRADED AS A PAIR WITH `Nr2SubWin`, and it is an experiment as much as a
/// counter. This build also advances `pDmaBufferPrivateData` at the end of
/// Render, on the hypothesis that the field is `in/out` like `pDmaBuffer` and
/// that not advancing it is WHY the window is empty. So: if `Nr2SubWin` now
/// reads `start=0 end=64` and this stays 0, the pointer was the answer and the
/// window is real. If this keeps climbing, the window is simply not populated on
/// this path and the fallback is what carries it — which is the same conclusion
/// `decode_present_fence` reached before K6 existed.
pub static NR2_WINDOW_FALLBACK: AtomicU32 = AtomicU32::new(0);

/// Submissions whose private record could not be read at all — the window did
/// not describe 64 readable bytes.
///
/// ⛔ IT EXISTS BECAUSE THE FIRST PROBE RUN COULD ONLY INFER IT. `submit()`
/// returned silently on a refused read, so the only evidence was `Nr2Slot`
/// moving while `Nr2SlotRet` did not — a difference the counter's own doc says
/// means the OPPOSITE thing (that dxgkrnl never called SubmitCommand). A refusal
/// with no counter is the rule this driver does not get to break.
pub static NR2_SUBMIT_NO_RECORD: AtomicU32 = AtomicU32::new(0);

/// The private-data SUBMISSION WINDOW `DxgkDdiSubmitCommand` reported, packed
/// `(start << 16) | end`, last-value.
///
/// ⛔ MEASURED BECAUSE THE FIRST PROBE RUN SAID SOMETHING WAS WRONG AND NOT WHAT.
/// On KMD 22.22.269.0 the K6 probe took 3 staging checkouts (`Nr2Slot=3`) and
/// **0** retirements, with `Nr2SlotUnd=0` — so `retire()` was never called at
/// all, which can only mean `read_dma_record` refused the window on every one of
/// the 5 submissions. `publish_dma_record` had already SUCCEEDED at Render (a
/// failure there returns before `Nr2Commit`, which read 3), so the 64-byte
/// buffer exists; it is the window that does not describe it. These two report
/// the numbers instead of guessing which of "the window is empty", "the total is
/// short" or "the offsets are elsewhere" it is.
pub static NR2_SUBMIT_WINDOW: AtomicU32 = AtomicU32::new(0);
/// `DXGKARG_SUBMITCOMMAND::DmaBufferPrivateDataSize` as reported, last-value.
pub static NR2_SUBMIT_WINDOW_TOTAL: AtomicU32 = AtomicU32::new(0);
/// The same pair for `DxgkDdiPatch`, whose one call this boot also refused the
/// record (`Nr2PatchDiff=1`, `Nr2PatchCall=1`).
pub static NR2_PATCH_WINDOW: AtomicU32 = AtomicU32::new(0);
/// `DXGKARG_PATCH::DmaBufferPrivateDataSize` as reported, last-value.
pub static NR2_PATCH_WINDOW_TOTAL: AtomicU32 = AtomicU32::new(0);

/// Staging retirements refused — the pure half's `StagingUnderflow`.
///
/// **Must read 0.** It was discarded with `.is_ok()` and no counter, which is
/// the one thing that could make the pool drift invisibly; `Nr2Slot`/`Nr2SlotRet`
/// cannot show it, because a failed retire bumps neither.
pub static NR2_SLOT_UNDERFLOW: AtomicU32 = AtomicU32::new(0);
/// Staging admissions (COMMITs whose reassembled size the §10.7 pool accepted).
pub static NR2_SLOT_TAKEN: AtomicU32 = AtomicU32::new(0);
/// Staging retirements at SubmitCommand.
///
/// ⚠ GRADE IT AS A PAIR WITH `Nr2Slot`, NOT AS A LEVEL. A persistent shortfall
/// means dxgkrnl is not calling `DxgkDdiSubmitCommand` for these submissions,
/// which is the one thing about this path that is asserted rather than measured
/// — hence the 112-byte header copy that keeps every HNR2 DMA buffer non-empty.
pub static NR2_SLOT_RETIRED: AtomicU32 = AtomicU32::new(0);
/// HNR2 `DxgkDdiSubmitCommand` calls.
pub static NR2_SUBMITS: AtomicU32 = AtomicU32::new(0);
/// Exact K11 host-completed DMA records admitted onto their context-local WDDM
/// submission watermark.  Each increment is downstream of the validated host
/// reply and HVR1 publication, never merely a decoded/enqueued command.
pub static NR2_HOST_SUBMIT_OK: AtomicU32 = AtomicU32::new(0);
/// Host-completed records refused at SubmitCommand, packed as
/// `(count << 16) | reason`: 1 = session/transport generation is no longer
/// current, 2 = an unmarked duplicate fence, 3 = a backward fence.
pub static NR2_HOST_SUBMIT_REJECT: AtomicU32 = AtomicU32::new(0);
/// A user-buffer read whose `ProbeForRead` raised — the range was not user
/// space. **Must read 0**; a nonzero value means `pCommand` is not the
/// user-mapped command buffer this arm assumes.
pub static NR2_PROBE_FAULTS: AtomicU32 = AtomicU32::new(0);
/// A user-buffer read whose copy raised after a clean probe: the producer
/// unmapped the range under us. Not a driver defect — it is exactly the
/// bugcheck this arm's SEH shim exists to convert into a refusal.
pub static NR2_COPY_FAULTS: AtomicU32 = AtomicU32::new(0);
/// A Render/Patch/Submit whose `hContext` was null where a role was needed.
/// Paging submissions legitimately carry a null context and are NOT counted
/// here — this counts only the arms that had already resolved a role.
pub static NR2_NULL_CONTEXT: AtomicU32 = AtomicU32::new(0);
/// A use record naming an allocation-list entry this driver could not resolve,
/// or whose published generation the record did not match.
pub static NR2_ALLOC_STALE: AtomicU32 = AtomicU32::new(0);
/// Control Renders admitted onto a reply slot.
pub static NR2_CONTROL_RENDERS: AtomicU32 = AtomicU32::new(0);
/// The measured `KeGetCurrentIrql()` at `DxgkDdiSubmitCommandVirtual`,
/// last-value.
///
/// ⛔ IT EXISTS BECAUSE TWO TEXTS DISAGREE AND NEITHER WAS MEASURED. WDK 28000
/// annotates the DDI `_IRQL_requires_(PASSIVE_LEVEL)` (`d3dkmddi.h:5365`);
/// `submit_command.rs`'s own comment says DISPATCH_LEVEL. Everything K6 does on
/// that path is legal at DISPATCH either way, so this reports the answer rather
/// than assuming one. `0` = PASSIVE_LEVEL, `2` = DISPATCH_LEVEL.
pub static NR2_SUBMIT_VIRTUAL_IRQL: AtomicU32 = AtomicU32::new(0);
/// HOS1 descriptors validated against their outer context.
pub static NR2_HOS1_OK: AtomicU32 = AtomicU32::new(0);
/// HOS1 refusals, packed `(count << 16) | RenderRefusal::code()`.
pub static NR2_HOS1_REJECT: AtomicU32 = AtomicU32::new(0);

// ── The boundary counters. Each names something K6 deliberately does NOT do,
// at the site where a later unit will do it. None of them is a failure; all of
// them are expected to move, and a ZERO on any of them during a probe run means
// that path was never reached.

/// COMMITs whose Venus payload was not re-parsed, because no generated opcode
/// schema exists on this side (`protocol/src/translation_session.rs:1354`
/// records that the table is generated in Mesa and the KMD and is not
/// duplicated; `virtio/venus/protocol.rs` is ~30 hand-picked constants with no
/// parser). The COMMIT is refused rather than admitted unclassified.
pub static NR2_NO_SCHEMA: AtomicU32 = AtomicU32::new(0);
/// COMMITs whose typed operands were left as the encoder wrote them (zero).
/// The capability ordinal and the output patch entry ARE written; the resid
/// rewrite belongs to the later allocation/GPU execution units.
pub static NR2_NO_RESID: AtomicU32 = AtomicU32::new(0);
/// Control Renders refused because K11's finite allowlist requires an actual
/// role-1 HVR1 reply carrier.
pub static NR2_NO_REPLY: AtomicU32 = AtomicU32::new(0);
/// Submissions not dispatched to a host ring. K11 completes only the exact
/// pure-control INIT; allocation-backed and queue work remains refused.
pub static NR2_NO_HOST: AtomicU32 = AtomicU32::new(0);
/// COMMITs whose reassembled payload was accounted against the §10.7 pool but
/// not retained. This applies to operations outside K11's synchronous finite
/// INIT; retaining up to 15 MiB per context would provide no executor.
pub static NR2_NO_STAGE: AtomicU32 = AtomicU32::new(0);
/// Capability records left with `hpm_epoch == 0` because the KMD placement
/// epoch is K2/K3's and has no producer — which is why `validate_at_submit` is
/// not called.
pub static NR2_NO_EPOCH: AtomicU32 = AtomicU32::new(0);
/// HOS1 descriptors validated and then NOT enqueued at a GPUVA.
pub static NR2_HOS1_NOT_EXECUTED: AtomicU32 = AtomicU32::new(0);

/// The counter names, as one list, so the collision proof and the writer cannot
/// drift apart.
const COUNTER_NAMES: [&[u8]; 38] = [
    b"Nr2QCtx",
    b"Nr2QCtxRej",
    b"Nr2Scratch",
    b"Nr2Frag",
    b"Nr2Commit",
    b"Nr2Resize",
    b"Nr2Rej",
    b"Nr2Reent",
    b"Nr2Patch",
    b"Nr2PatchCall",
    b"Nr2PatchSnap",
    b"Nr2PatchDiff",
    b"Nr2Slot",
    b"Nr2SlotRet",
    b"Nr2Sub",
    b"Nr2ProbeFlt",
    b"Nr2CopyFlt",
    b"Nr2CtxNull",
    b"Nr2Stale",
    b"Nr2Ctl",
    b"Nr2SvIrql",
    b"Nr2Hos1Ok",
    b"Nr2Hos1Rej",
    b"Nr2NoSchema",
    b"Nr2NoResid",
    b"Nr2NoReply",
    b"Nr2NoHost",
    b"Nr2Reloc",
    b"Nr2SubDup",
    b"Nr2SlotUnd",
    b"Nr2SubWin",
    b"Nr2SubTot",
    b"Nr2PchWin",
    b"Nr2PchTot",
    b"Nr2SubNoRec",
    b"Nr2WinFB",
    b"Nr2HostOk",
    b"Nr2HostRej",
];

/// The boundary counters that did not fit [`COUNTER_NAMES`]'s block, mirrored
/// alongside it. Split only because a `CounterBlock` writes one registry value
/// per entry and 27 is already the largest block in this driver.
const BOUNDARY_NAMES: [&[u8]; 4] = [
    b"Nr2NoStage",
    b"Nr2NoEpoch",
    b"Nr2Hos1NoX",
    // Not a K6 counter by subject, but K6 is what made the hazard reachable:
    // `DxgkDdiPatch`/`DxgkDdiSubmitCommand` deliver the context through a
    // `hDevice`/`hContext` union. It is mirrored here because this block already
    // has both a PASSIVE flush site and a teardown one.
    b"CtxBadH",
];

/// Compile-time proof that no counter name can be truncated into another's.
/// `diag::record_named_bytes` clamps silently at `MAX_CONFIG_NAME`, so two names
/// sharing that prefix would merge into one registry value. Measured 2026-08-11:
/// no existing name in this driver starts with `Nr2`, so the only collisions
/// possible are among these.
const _: () = {
    let mut i = 0;
    while i < COUNTER_NAMES.len() {
        assert!(
            COUNTER_NAMES[i].len() <= crate::diag::MAX_CONFIG_NAME,
            "native-render counter name exceeds MAX_CONFIG_NAME and would merge with another"
        );
        i += 1;
    }
    let mut j = 0;
    while j < BOUNDARY_NAMES.len() {
        assert!(
            BOUNDARY_NAMES[j].len() <= crate::diag::MAX_CONFIG_NAME,
            "native-render boundary counter name exceeds MAX_CONFIG_NAME"
        );
        j += 1;
    }
};

static NR2_FLUSH_TICKS: AtomicU32 = AtomicU32::new(0);
static NR2_FLUSH_FAILURES: AtomicU32 = AtomicU32::new(0);

const fn e(name: &'static [u8], value: &'static AtomicU32) -> crate::diag::CounterEntry {
    crate::diag::CounterEntry {
        name,
        value: crate::diag::CounterRef::U32(value),
        failure: false,
    }
}
const fn f(name: &'static [u8], value: &'static AtomicU32) -> crate::diag::CounterEntry {
    crate::diag::CounterEntry {
        name,
        value: crate::diag::CounterRef::U32(value),
        failure: true,
    }
}

static NR2_COUNTERS: crate::diag::CounterBlock = crate::diag::CounterBlock {
    entries: &[
        e(COUNTER_NAMES[0], &NR2_QUEUE_CTX),
        f(COUNTER_NAMES[1], &NR2_QUEUE_CTX_REJECT),
        f(COUNTER_NAMES[2], &NR2_SCRATCH_ALLOC_FAILED),
        e(COUNTER_NAMES[3], &NR2_FRAGMENTS),
        e(COUNTER_NAMES[4], &NR2_COMMITS),
        e(COUNTER_NAMES[5], &NR2_RESIZE),
        f(COUNTER_NAMES[6], &NR2_REJECT),
        f(COUNTER_NAMES[7], &NR2_REENTRANT),
        e(COUNTER_NAMES[8], &NR2_PATCH_SLOTS),
        e(COUNTER_NAMES[9], &NR2_PATCH_CALLS),
        e(COUNTER_NAMES[10], &NR2_PATCH_SNAPS),
        f(COUNTER_NAMES[11], &NR2_PATCH_DIFF),
        e(COUNTER_NAMES[12], &NR2_SLOT_TAKEN),
        e(COUNTER_NAMES[13], &NR2_SLOT_RETIRED),
        e(COUNTER_NAMES[14], &NR2_SUBMITS),
        f(COUNTER_NAMES[15], &NR2_PROBE_FAULTS),
        f(COUNTER_NAMES[16], &NR2_COPY_FAULTS),
        f(COUNTER_NAMES[17], &NR2_NULL_CONTEXT),
        f(COUNTER_NAMES[18], &NR2_ALLOC_STALE),
        e(COUNTER_NAMES[19], &NR2_CONTROL_RENDERS),
        e(COUNTER_NAMES[20], &NR2_SUBMIT_VIRTUAL_IRQL),
        e(COUNTER_NAMES[21], &NR2_HOS1_OK),
        f(COUNTER_NAMES[22], &NR2_HOS1_REJECT),
        e(COUNTER_NAMES[23], &NR2_NO_SCHEMA),
        e(COUNTER_NAMES[24], &NR2_NO_RESID),
        e(COUNTER_NAMES[25], &NR2_NO_REPLY),
        e(COUNTER_NAMES[26], &NR2_NO_HOST),
        e(COUNTER_NAMES[27], &NR2_PATCH_RELOCATED),
        e(COUNTER_NAMES[28], &NR2_SUBMIT_RESUBMISSION),
        f(COUNTER_NAMES[29], &NR2_SLOT_UNDERFLOW),
        e(COUNTER_NAMES[30], &NR2_SUBMIT_WINDOW),
        e(COUNTER_NAMES[31], &NR2_SUBMIT_WINDOW_TOTAL),
        e(COUNTER_NAMES[32], &NR2_PATCH_WINDOW),
        e(COUNTER_NAMES[33], &NR2_PATCH_WINDOW_TOTAL),
        f(COUNTER_NAMES[34], &NR2_SUBMIT_NO_RECORD),
        e(COUNTER_NAMES[35], &NR2_WINDOW_FALLBACK),
        e(COUNTER_NAMES[36], &NR2_HOST_SUBMIT_OK),
        f(COUNTER_NAMES[37], &NR2_HOST_SUBMIT_REJECT),
        e(BOUNDARY_NAMES[0], &NR2_NO_STAGE),
        e(BOUNDARY_NAMES[1], &NR2_NO_EPOCH),
        e(BOUNDARY_NAMES[2], &NR2_HOS1_NOT_EXECUTED),
        f(BOUNDARY_NAMES[3], &crate::device::CONTEXT_HANDLE_REFUSED),
    ],
    ticks: &NR2_FLUSH_TICKS,
    failures: &NR2_FLUSH_FAILURES,
    policy: crate::diag::FlushPolicy::EveryNth(64),
};

/// Mirror the counters into the service key. PASSIVE only.
pub fn diag_dump_native_render_atomics() {
    NR2_COUNTERS.flush();
}

/// Pack a count with a `kmd_logic` reason code into one registry value, so the
/// refusal names both how often and which rule. The same encoding K5 uses.
fn bump_with_code(counter: &AtomicU32, code: u32) {
    let prior = counter.load(Ordering::Relaxed) >> 16;
    let next = prior.saturating_add(1).min(0xFFFF);
    counter.store((next << 16) | (code & 0xFFFF), Ordering::Relaxed);
}

// ── The COMMIT table scratch ─────────────────────────────────────────────────

/// Room for one COMMIT's tables, copied out of the user command buffer before
/// anything reads them.
///
/// ⛔ The copy is not optional and the size is not negotiable.
/// `DXGKARG_RENDER::pCommand` is the context's command buffer, which stays
/// mapped and WRITABLE in the submitting process for the whole call, so
/// validating in place is a TOCTOU against the producer. `validate_commit_tables`
/// needs random access to both whole tables (a uniqueness bitmap over allocation
/// indices, and tiled patch runs), so streaming would mean re-deriving
/// `protocol`'s rules here — which is the split this unit exists to avoid.
///
/// 4096·24 + 8192·16 + 4096 = 233,472 bytes, allocated ONCE per HVC1 context at
/// `DxgkDdiCreateContext` (PASSIVE) rather than per Render. The bound is the
/// context's own advertised `AllocationListSize`/`PatchLocationListSize`, so a
/// COMMIT that fits the advertised capacity always fits this.
struct Hnr2Scratch {
    uses: Vec<HeliosNativeRenderUse>,
    patches: Vec<HeliosNativeRenderPatch>,
    /// `DXGK_ALLOCATIONLIST::WriteOperation`, one per allocation-list entry —
    /// the single Windows fact `admit_commit_tables` needs, reduced to a
    /// primitive at the boundary.
    write_bits: Vec<bool>,
}

impl Hnr2Scratch {
    /// `None` when the pool cannot supply the tables.
    ///
    /// `#[inline(never)]` and three separate `Vec`s, never one boxed struct: the
    /// T3 lesson (`adapter/mod.rs:283`) is that a multi-KiB value built inline on
    /// a DDI frame is how this driver overflowed a 24-KiB kernel stack twice, and
    /// 228 KiB would not merely overflow it, it would double-fault.
    #[inline(never)]
    fn new() -> Option<Self> {
        let mut uses = Vec::new();
        uses.try_reserve_exact(HELIOS_HVC1_ALLOCATION_LIST_ENTRIES as usize)
            .ok()?;
        uses.resize(
            HELIOS_HVC1_ALLOCATION_LIST_ENTRIES as usize,
            HeliosNativeRenderUse::default(),
        );
        let mut patches = Vec::new();
        patches
            .try_reserve_exact(HELIOS_HNR2_MAX_PATCH_RECORDS as usize)
            .ok()?;
        patches.resize(
            HELIOS_HNR2_MAX_PATCH_RECORDS as usize,
            HeliosNativeRenderPatch::default(),
        );
        let mut write_bits = Vec::new();
        write_bits
            .try_reserve_exact(HELIOS_HVC1_ALLOCATION_LIST_ENTRIES as usize)
            .ok()?;
        write_bits.resize(HELIOS_HVC1_ALLOCATION_LIST_ENTRIES as usize, false);
        Some(Self {
            uses,
            patches,
            write_bits,
        })
    }
}

// ── The per-context native state ─────────────────────────────────────────────

/// Which HVC1 class a native context is.
///
/// ⛔ Both classes take the HNR2 Render path, and the difference is not
/// cosmetic: only the control context may carry a reply, and only it owns the
/// HTS1 session's INIT.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum NativeClass {
    Control,
    Queue,
}

/// One HVC1 context's K6 state, allocated at `DxgkDdiCreateContext` and owned by
/// [`crate::device::ContextContext`]'s role.
pub(crate) struct NativeContext {
    class: NativeClass,
    /// The host ring this context's submissions execute on.
    ///
    /// ⛔ Always 0 today, for both classes. K11 reserves fixed nonzero endpoint
    /// rings before publishing INIT capacity, but no queue operation executes
    /// in this tranche; a queue context therefore retains ring zero only as a
    /// refusal value and never submits through it.
    ring_index: u32,
    /// One exact WDDM SubmissionFenceId watermark for this HVC1 context.  It is
    /// neither an adapter-wide boundary queue nor a host/shared timeline.
    host_submissions: SpinLock<HostSubmissionState>,
    /// The HNR2 assembler and its staging accounting. The §10.7:2041 short lock:
    /// taken for the admission decision only, never held across the user copy,
    /// a host call, or a wait.
    state: SpinLock<RenderContext>,
    /// Claimed for the whole of one `DxgkDdiRender`, so [`Self::scratch`] has
    /// exactly one writer.
    ///
    /// A spinlock would be the obvious shape and is the wrong one: the section it
    /// would guard contains a ~228 KiB copy out of user memory, and `SpinLock`
    /// raises to DISPATCH_LEVEL. Two concurrent Renders on one context are a
    /// producer bug anyway — §10.7 permits exactly one incomplete batch per
    /// context — so the second is refused and counted rather than serialized.
    busy: AtomicU32,
    /// Touched only while [`Self::busy`] is claimed.
    scratch: UnsafeCell<Hnr2Scratch>,
}

// SAFETY: `state` and `host_submissions` are reachable only through their
// `SpinLock`s; `class`/`ring_index` are written once at construction and
// read-only afterwards; `scratch` is reachable only through
// [`NativeContext::claim`], which hands out at most one reference at a time via
// `busy`.
unsafe impl Send for NativeContext {}
// SAFETY: as above.
unsafe impl Sync for NativeContext {}

impl NativeContext {
    /// `None` when the COMMIT scratch cannot be allocated; the caller must then
    /// fail `DxgkDdiCreateContext`.
    pub(crate) fn new(class: NativeClass) -> Option<Self> {
        let Some(scratch) = Hnr2Scratch::new() else {
            NR2_SCRATCH_ALLOC_FAILED.fetch_add(1, Ordering::Relaxed);
            return None;
        };
        Some(Self {
            class,
            ring_index: helios_protocol::native_render::HELIOS_HVC1_CONTROL_RING_INDEX,
            host_submissions: SpinLock::new(HostSubmissionState::new()),
            state: SpinLock::new(RenderContext::new()),
            busy: AtomicU32::new(0),
            scratch: UnsafeCell::new(scratch),
        })
    }

    /// Take exclusive use of [`Self::scratch`] for one `DxgkDdiRender`, or
    /// `None` when another Render on this context has not returned.
    fn claim(&self) -> Option<NativeClaim<'_>> {
        if self
            .busy
            .compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            NR2_REENTRANT.fetch_add(1, Ordering::Relaxed);
            return None;
        }
        Some(NativeClaim { owner: self })
    }
}

/// Release is a `Drop` obligation, for the reason `crate::sync`'s guard exists:
/// this function has a dozen refusal returns and a leaked claim would make every
/// later Render on the context refuse `Nr2Reent` forever.
struct NativeClaim<'a> {
    owner: &'a NativeContext,
}

impl Drop for NativeClaim<'_> {
    fn drop(&mut self) {
        self.owner.busy.store(0, Ordering::Release);
    }
}

impl NativeClaim<'_> {
    fn scratch(&mut self) -> &mut Hnr2Scratch {
        // SAFETY: the claim holds `busy`, and `busy` is the only route to this
        // cell, so no other reference to the scratch exists.
        unsafe { &mut *self.owner.scratch.get() }
    }
}

// ── The SEH-guarded user copy ────────────────────────────────────────────────

extern "C" {
    /// C SEH shim (`src/render_user_copy.c`, compiled by build.rs):
    /// `ProbeForRead` then `memcpy`, each in its own `__try/__except`.
    /// `DXGKARG_RENDER::pCommand` is a user-mode mapping the submitting process
    /// can unmap at any moment, and a raise unwinding out of a no_std DDI
    /// bugchecks. Returns 0 on success, 1 if the probe raised, 2 if the copy did.
    fn helios_render_copy_user_seh(
        destination: *mut core::ffi::c_void,
        source: *const core::ffi::c_void,
        length: u64,
        alignment: u32,
    ) -> u32;
}

/// Copy `bytes` out of the command buffer, or count which half faulted.
///
/// # Safety
/// `dst` must be writable for `bytes`. `src` is untrusted: it is only ever
/// dereferenced inside the shim's exception frame.
unsafe fn copy_from_command(dst: *mut u8, src: *const u8, bytes: usize) -> bool {
    // SAFETY: `dst` is a kernel buffer of at least `bytes`; `src` is guarded.
    match unsafe {
        helios_render_copy_user_seh(dst.cast(), src.cast(), bytes as u64, 1)
    } {
        0 => true,
        1 => {
            NR2_PROBE_FAULTS.fetch_add(1, Ordering::Relaxed);
            false
        }
        _ => {
            NR2_COPY_FAULTS.fetch_add(1, Ordering::Relaxed);
            false
        }
    }
}

// ── The DMA-buffer layout one HNR2 submission produces ───────────────────────
//
//   +0    HeliosNativeRenderV2      the validated header, copied verbatim
//   +112  Hnr2PhysicalCapability[]  one per COMMIT use record
//
// ⛔ The Venus payload and the COMMIT tables are NOT in here, and that is what
// makes the arithmetic work: 112 + 4096·48 = 192 KiB fits the 256-KiB buffer,
// while the command itself (up to 229 KiB) plus the table would not. The header
// copy is also why every HNR2 submission is non-empty — including an interior
// fragment, which would otherwise advance `pDmaBuffer` by zero and might never
// reach `DxgkDdiSubmitCommand` to retire its accounting.

/// Byte offset of the capability table inside an HNR2 DMA buffer.
const CAPABILITY_TABLE_OFFSET: u32 = HELIOS_HNR2_HEADER_SIZE as u32;

/// Write the 112-byte header copy and the KMD-private record that describes
/// this submission to `DxgkDdiPatch` and `DxgkDdiSubmitCommand`.
///
/// ⚠ UNCONDITIONAL, including on an interior fragment with nothing to describe.
/// dxgkrnl RECYCLES DMA private-data buffers between submissions — the lesson
/// the D3D12 ECL arm records in `submit_command.rs` — so declining to write
/// would leave the PREVIOUS submission's record in place and retire its staging
/// twice.
///
/// # Safety
/// `args` is dxgkrnl's live Render argument; both buffers are checked for size
/// before a byte is written.
unsafe fn publish_dma_record(
    args: &mut DXGKARG_RENDER,
    header: &HeliosNativeRenderV2,
    plan: Option<&CapabilityTablePlan>,
    ring_index: u32,
    payload_bytes: u32,
) -> bool {
    let header_bytes = size_of::<HeliosNativeRenderV2>();
    if (args.DmaSize as usize) < header_bytes || args.pDmaBuffer.is_null() {
        return false;
    }
    // SAFETY: `pDmaBuffer` is the kernel mapping of this Render's DMA buffer and
    // `DmaSize >= header_bytes` was just checked.
    unsafe {
        core::ptr::copy_nonoverlapping(
            bytemuck::bytes_of(header).as_ptr(),
            args.pDmaBuffer as *mut u8,
            header_bytes,
        );
    }
    let record = Hnr2KmdDmaPrivateV1 {
        batch_token: header.batch_token,
        // K6 mints neither: the KMD device object carries no generation, and an
        // HVC1 context is identified by its own object rather than a number
        // (§10.4:1250 — "the live context object is the identity").
        device_generation: 0,
        context_generation: 0,
        // ⛔ NOT a pool slot index: K6's staging pool is accounting only (see
        // `Nr2NoStage`), so the batch token is the submission's identity and the
        // slot index is always 0.
        slot_generation: header.batch_token,
        full_payload_crc64: header.full_payload_crc64,
        payload_bytes,
        capability_offset: plan.map_or(0, |p| p.offset),
        capability_count: plan.map_or(0, |p| p.count),
        ring_index,
        slot_index: 0,
        flags: 0,
    };
    let private_bytes = size_of::<Hnr2KmdDmaPrivateV1>();
    if args.pDmaBufferPrivateData.is_null()
        || (args.DmaBufferPrivateDataSize as usize) < private_bytes
    {
        return false;
    }
    // SAFETY: non-null and at least the record's size, checked above. The buffer
    // is KMD-private for this context — no UMD prefix is copied into a Render's
    // private data.
    unsafe {
        core::ptr::copy_nonoverlapping(
            bytemuck::bytes_of(&record).as_ptr(),
            args.pDmaBufferPrivateData as *mut u8,
            private_bytes,
        );
    }
    true
}

/// Mark the exact DMA-private record whose finite host operation has already
/// completed. `publish_dma_record` proved this pointer and extent immediately
/// before the host call; this fixed offset write is therefore infallible local
/// publication, not a context-global token that can be overwritten by a later
/// pipelined Render.
///
/// # Safety
/// The caller must have successfully published this submission's record into
/// `args` and must not yet have advanced `pDmaBufferPrivateData`.
unsafe fn mark_dma_host_completed(args: &DXGKARG_RENDER, batch_token: u64) {
    let base = args.pDmaBufferPrivateData as *mut u8;
    debug_assert!(!base.is_null());
    debug_assert!((args.DmaBufferPrivateDataSize as usize) >= size_of::<Hnr2KmdDmaPrivateV1>());
    let found = unsafe { core::ptr::read_unaligned(base.cast::<u64>()) };
    debug_assert_eq!(found, batch_token);
    let flags = unsafe {
        base.add(core::mem::offset_of!(Hnr2KmdDmaPrivateV1, flags))
            .cast::<u32>()
    };
    unsafe { core::ptr::write_unaligned(flags, HELIOS_HNR2_KMD_DMA_FLAG_HOST_COMPLETED) };
}

/// Advance `pDmaBufferPrivateData` past the record Render just wrote.
///
/// ⚠ AN EXPERIMENT WITH A COUNTER, not a known contract. The WDK header carries
/// no annotation on this field, and nothing in this driver has ever advanced it
/// — which is exactly consistent with the measured `start = end = 0` window, if
/// the field is `in/out` like `pDmaBuffer` and the advance is how a driver tells
/// dxgkrnl how much private data it produced. If it is NOT in/out, dxgkrnl
/// ignores the write and nothing changes; `Nr2WinFB` distinguishes the two on
/// the next run.
///
/// # Safety
/// The caller must already have written the record, i.e. `publish_dma_record`
/// returned true, which proves the buffer is at least that long.
unsafe fn advance_private_data(args: &mut DXGKARG_RENDER) {
    let bytes = size_of::<Hnr2KmdDmaPrivateV1>();
    if args.pDmaBufferPrivateData.is_null() || (args.DmaBufferPrivateDataSize as usize) < bytes {
        return;
    }
    // SAFETY: the buffer holds at least `bytes`, checked here and proven by the
    // write that preceded this call.
    args.pDmaBufferPrivateData =
        unsafe { (args.pDmaBufferPrivateData as *mut u8).add(bytes) as *mut c_void };
}

/// Read back the record [`publish_dma_record`] wrote.
///
/// ⛔ `DxgkDdiRender`'s `pDmaBufferPrivateData` POINTS AT THIS SUBMISSION'S
/// SLICE, while `DxgkDdiPatch`'s and `DxgkDdiSubmitCommand`'s point at the
/// BUFFER BASE with `…SubmissionStartOffset` locating the slice. Reading at
/// offset 0 on the consuming side therefore reads a *different* submission's
/// record — and on the Patch path that record's `capability_offset` would then
/// aim a write at an arbitrary place in the DMA buffer.
/// `decode_legacy_present_fence` already applies the offset; this is the same
/// rule, and it is the reason this helper takes a window rather than a pointer.
///
/// # Safety
/// `base`/`total` are dxgkrnl's private-data pair; `start`/`end` are its
/// submission window into them.
unsafe fn read_dma_record(
    base: *const c_void,
    total: u32,
    start: u32,
    end: u32,
) -> Option<Hnr2KmdDmaPrivateV1> {
    let bytes = size_of::<Hnr2KmdDmaPrivateV1>();
    let (mut start, mut end, total) = (start as usize, end as usize, total as usize);
    if base.is_null() {
        return None;
    }
    // ⛔ MEASURED, NOT ASSUMED: on 22.22.270.0 `DxgkDdiSubmitCommand` reports
    // `DmaBufferPrivateDataSize = 64` — exactly one record — with the submission
    // window `start = end = 0`. So the buffer is there and the WINDOW is empty,
    // which is why the first deployment took 3 staging checkouts and 0
    // retirements. The shipping present path already copes with the same thing:
    // `decode_present_fence` tries the window and then falls back to offset 0.
    //
    // The fallback is gated on `total == bytes` and that gate is the whole
    // safety argument: a buffer holding exactly ONE record cannot be two
    // batched Renders, so there is no other submission for offset 0 to belong
    // to. A bigger buffer with an empty window stays refused.
    if end <= start && total == bytes {
        NR2_WINDOW_FALLBACK.fetch_add(1, Ordering::Relaxed);
        start = 0;
        end = bytes;
    }
    if start > end || end > total || end - start < bytes {
        return None;
    }
    let mut raw = [0u8; size_of::<Hnr2KmdDmaPrivateV1>()];
    // SAFETY: `start + bytes <= end <= total`, and the buffer is kernel memory
    // dxgkrnl owns for the duration of the call.
    unsafe {
        core::ptr::copy_nonoverlapping((base as *const u8).add(start), raw.as_mut_ptr(), bytes)
    };
    bytemuck::try_pod_read_unaligned::<Hnr2KmdDmaPrivateV1>(&raw).ok()
}

/// Refuse this Render, naming the rule.
///
/// ⛔ ONE registry write, not a block flush. `Nr2Rej` is a FAILURE entry, and
/// `CounterBlock::flush` writes EVERY entry whenever the failure sum changed —
/// which it does on every refusal — so flushing here cost 31 synchronous
/// `RtlWriteRegistryValue` calls per refused Render, inside the DDI, reachable
/// by any process that can call `D3DKMTRender`. That is exactly the regression
/// `CounterBlock` was written to remove (`diag.rs`: the paging block's 24 writes
/// per op). A refusal only needs its own value published; the rest of the block
/// rides the existing PASSIVE cadences.
///
/// PASSIVE-only, which every caller is: they are all on the Render path.
fn refuse(refusal: RenderRefusal, status: NTSTATUS) -> NTSTATUS {
    bump_with_code(&NR2_REJECT, refusal.code());
    crate::diag::record_named_bytes(COUNTER_NAMES[6], NR2_REJECT.load(Ordering::Relaxed));
    status
}

// ── `DxgkDdiRender`, the HNR2 arm ────────────────────────────────────────────

/// One HNR2 fragment on an HVC1 context.
///
/// Runs at PASSIVE_LEVEL (`d3dkmddi.h:160`), which is what licenses the
/// `ProbeForRead` in the copy shim and the throttled registry flush at the end.
///
/// # Safety
/// `args` is dxgkrnl's live `DXGKARG_RENDER`; `native` and `session` belong to
/// the context handle dxgkrnl passed, resolved through
/// [`crate::device::ContextHandleRef`].
pub(crate) unsafe fn render(
    native: &NativeContext,
    session: core::ptr::NonNull<crate::ddi::translation_session::SessionObject>,
    args: &mut DXGKARG_RENDER,
) -> NTSTATUS {
    // §10.7's capacity renegotiation, which A2 issues before its first batch
    // (`vn_helios_native_kmt.c:764`): a zero-length Render with no lists. It is
    // not a fragment, and treating it as one would refuse it as a malformed
    // header and lose the context on the ICD side.
    if args.CommandLength == 0 && args.AllocationListSize == 0 && args.PatchLocationListInSize == 0
    {
        NR2_RESIZE.fetch_add(1, Ordering::Relaxed);
        args.PatchLocationListOutSize = 0;
        args.MultipassOffset = 0;
        NR2_COUNTERS.flush();
        return STATUS_SUCCESS;
    }
    if args.pCommand.is_null() || args.pDmaBuffer.is_null() {
        return STATUS_INVALID_PARAMETER;
    }

    let Some(mut claim) = native.claim() else {
        // Counted as `Nr2Reent`; the status is the generic refusal because
        // `DxgkDdiRender`'s documented return set is narrow and every reason
        // this arm has lives in a counter, not in the NTSTATUS.
        return STATUS_INVALID_PARAMETER;
    };

    // ── the header ──────────────────────────────────────────────────────────
    let header_bytes = size_of::<HeliosNativeRenderV2>();
    if (args.CommandLength as usize) < header_bytes {
        return refuse(
            RenderRefusal::Header(Hnr2Reject::CommandLengthMismatch),
            STATUS_INVALID_PARAMETER,
        );
    }
    let mut raw = [0u8; size_of::<HeliosNativeRenderV2>()];
    // SAFETY: `raw` is exactly the header size; `pCommand` is untrusted and is
    // read only inside the shim's exception frame.
    if !unsafe { copy_from_command(raw.as_mut_ptr(), args.pCommand as *const u8, header_bytes) } {
        return STATUS_INVALID_PARAMETER;
    }
    let Ok(header) = bytemuck::try_pod_read_unaligned::<HeliosNativeRenderV2>(&raw) else {
        return refuse(
            RenderRefusal::Header(Hnr2Reject::HeaderSizeMismatch),
            STATUS_INVALID_PARAMETER,
        );
    };

    let env = RenderEnv {
        package_generation: helios_protocol::HELIOS_PACKAGE_GENERATION,
        allocation_list_count: args.AllocationListSize,
        command_length: args.CommandLength,
        command_buffer_bytes: args.DmaSize,
        patch_location_list_in_size: args.PatchLocationListInSize,
    };

    // ⛔ VALIDATE, DO EVERY FALLIBLE THING, THEN APPLY — never admit-then-work.
    // This DDI may answer `STATUS_BUFFER_TOO_SMALL`, and dxgkrnl's documented
    // response is to grow the buffer and CALL IT AGAIN with the same command.
    // An assembler advanced before that discovery would refuse its own replay
    // with `BatchTokenNotIncreasing` and lose the context.
    //
    // The §10.7:2041 short lock covers the decision only; everything after it —
    // the user copy, the capability table, the reply-slot checkout — runs
    // outside it, and `claim` is what makes that safe: only a Render touches the
    // assembler, and `busy` admits one Render per context.
    let accept = {
        let state = native.state.lock();
        match validate_render_fragment(&state, &header, &env) {
            Ok(accept) => accept,
            Err(refusal) => {
                drop(state);
                return refuse(refusal, STATUS_INVALID_PARAMETER);
            }
        }
    };

    if !accept.class.is_commit() {
        // An interior fragment carries no tables, no allocations and no reply.
        // SAFETY: `args` is dxgkrnl's live argument struct.
        if !unsafe { publish_dma_record(args, &header, None, native.ring_index, 0) } {
            return STATUS_INVALID_PARAMETER;
        }
        args.pDmaBuffer = unsafe { (args.pDmaBuffer as *mut u8).add(header_bytes) as *mut c_void };
        unsafe { advance_private_data(args) };
        args.PatchLocationListOutSize = 0;
        args.MultipassOffset = 0;
        apply_render_fragment(&mut native.state.lock(), &header, &accept);
        NR2_FRAGMENTS.fetch_add(1, Ordering::Relaxed);
        NR2_COUNTERS.flush();
        return STATUS_SUCCESS;
    }

    commit(&mut claim, native, session, args, &header, &accept)
}

/// The COMMIT arm: the tables, the capability records, the output patch list,
/// the staging admission, and — on a control context — the reply slot.
fn commit(
    claim: &mut NativeClaim<'_>,
    native: &NativeContext,
    session: core::ptr::NonNull<crate::ddi::translation_session::SessionObject>,
    args: &mut DXGKARG_RENDER,
    header: &HeliosNativeRenderV2,
    accept: &Hnr2Accept,
) -> NTSTATUS {
    let use_count = header.use_record_count as usize;
    let patch_count = header.patch_record_count as usize;
    let list_count = args.AllocationListSize as usize;
    let scratch = claim.scratch();
    if use_count > scratch.uses.len()
        || patch_count > scratch.patches.len()
        || list_count > scratch.write_bits.len()
    {
        // The context advertised these capacities, so a COMMIT above them is a
        // header `protocol` should already have refused. Belt and braces: the
        // scratch is what bounds the copies below.
        return refuse(
            RenderRefusal::Table(Hnr2TableReject::AllocationListTooLarge),
            STATUS_INVALID_PARAMETER,
        );
    }
    if list_count != 0 && args.pAllocationList.is_null() {
        return STATUS_INVALID_PARAMETER;
    }

    // ── the two tables, copied out of the command buffer before anything reads
    // them. Validating in place would be a TOCTOU against the producer, which
    // keeps `pCommand` mapped and writable for the whole call.
    if use_count != 0 {
        let bytes = use_count * size_of::<HeliosNativeRenderUse>();
        // SAFETY: `use_record_offset` was validated against `CommandLength` by
        // `HeliosNativeRenderV2::validate`; the destination holds `use_count`
        // records; the source is read only inside the shim's exception frame.
        let ok = unsafe {
            copy_from_command(
                scratch.uses.as_mut_ptr() as *mut u8,
                (args.pCommand as *const u8).add(accept.layout.use_record_offset as usize),
                bytes,
            )
        };
        if !ok {
            return STATUS_INVALID_PARAMETER;
        }
    }
    if patch_count != 0 {
        let bytes = patch_count * size_of::<HeliosNativeRenderPatch>();
        // SAFETY: as above, for the patch table.
        let ok = unsafe {
            copy_from_command(
                scratch.patches.as_mut_ptr() as *mut u8,
                (args.pCommand as *const u8).add(accept.layout.patch_record_offset as usize),
                bytes,
            )
        };
        if !ok {
            return STATUS_INVALID_PARAMETER;
        }
    }

    // The single Windows fact `protocol` cannot see, reduced to a primitive.
    for i in 0..list_count {
        // SAFETY: `pAllocationList` is non-null with `AllocationListSize`
        // entries (dxgkrnl's contract for this Render), and `i < list_count`.
        let entry = unsafe { &*args.pAllocationList.add(i) };
        scratch.write_bits[i] = entry.__bindgen_anon_1.WriteOperation() != 0;
    }

    let uses = &scratch.uses[..use_count];
    let patches = &scratch.patches[..patch_count];
    let write_bits = &scratch.write_bits[..list_count];
    if let Err(refusal) =
        admit_commit_tables(header, uses, patches, args.AllocationListSize, write_bits)
    {
        return refuse(refusal, STATUS_INVALID_PARAMETER);
    }

    // ── the output patch list and the capability table ──────────────────────
    let plan = match plan_output_patch_slots(uses, args.PatchLocationListOutSize) {
        Ok(plan) => plan,
        Err(refusal) => {
            // The one refusal that asks the runtime to grow rather than fail:
            // a short patch-location list is exactly what STATUS_BUFFER_TOO_SMALL
            // means on this DDI.
            return refuse(refusal, STATUS_BUFFER_TOO_SMALL);
        }
    };
    let table = match plan_capability_table(CAPABILITY_TABLE_OFFSET, plan.count, args.DmaSize) {
        Ok(table) => table,
        Err(refusal) => return refuse(refusal, STATUS_BUFFER_TOO_SMALL),
    };
    if plan.count != 0 && args.pPatchLocationListOut.is_null() {
        return STATUS_INVALID_PARAMETER;
    }

    for i in 0..plan.count {
        let Some(record) = uses.get(i as usize) else {
            return refuse(
                RenderRefusal::Table(Hnr2TableReject::UseCountMismatch),
                STATUS_INVALID_PARAMETER,
            );
        };
        let index = record.allocation_list_index as usize;
        if index >= list_count {
            return refuse(
                RenderRefusal::Table(Hnr2TableReject::AllocationIndexOutOfRange),
                STATUS_INVALID_PARAMETER,
            );
        }
        // SAFETY: `index < list_count` and the list has that many entries.
        let entry = unsafe { &*args.pAllocationList.add(index) };
        // ⛔ `open_allocation_identity`, NEVER `allocation_identity`. Render and
        // Patch only ever see `hDeviceSpecificAllocation` — the OPEN handle —
        // and the create-keyed accessor answers about a different object with a
        // different generation, so a check written against it would refuse every
        // use record (`FINDINGS.md` F11).
        let Some(identity) = (unsafe {
            crate::ddi::create_allocation::open_allocation_identity(entry.hDeviceSpecificAllocation)
        }) else {
            NR2_ALLOC_STALE.fetch_add(1, Ordering::Relaxed);
            return refuse(
                RenderRefusal::Table(Hnr2TableReject::AllocationIndexOutOfRange),
                STATUS_INVALID_PARAMETER,
            );
        };
        if record.expected_allocation_generation != identity.generation {
            NR2_ALLOC_STALE.fetch_add(1, Ordering::Relaxed);
            return refuse(
                RenderRefusal::Dma(Hnr2DmaReject::AllocationGenerationStale),
                STATUS_INVALID_PARAMETER,
            );
        }
        let mut capability =
            match build_capability(record, identity.generation, identity.byte_size) {
                Ok(capability) => capability,
                Err(refusal) => return refuse(refusal, STATUS_INVALID_PARAMETER),
            };
        // §10.7:1856 — pre-patch whenever the validated allocation list ALREADY
        // reports a segment. It normally does not at Render (residency has not
        // run), which is what `DxgkDdiPatch` is for.
        let segment = entry.__bindgen_anon_1.SegmentId();
        if segment != 0 {
            // SAFETY: the union's `PhysicalAddress` arm is the one a context
            // created with `VirtualAddressing = 0` gets, and HVC1 admits only
            // that (`hvc1_admit_context_flags`).
            let physical = unsafe { entry.__bindgen_anon_2.PhysicalAddress.as_ref().QuadPart } as u64;
            match apply_placement(&mut capability, segment, physical, identity.byte_size) {
                // Placed, but with no epoch behind it: the KMD placement epoch
                // is K2/K3's and has no producer, which is why the record can
                // never be run through `validate_at_submit`.
                Ok(_) => NR2_NO_EPOCH.fetch_add(1, Ordering::Relaxed),
                Err(refusal) => return refuse(refusal, STATUS_INVALID_PARAMETER),
            };
        }

        let Some(offset) = table.entry_offset(i) else {
            return refuse(
                RenderRefusal::Dma(Hnr2DmaReject::OutputPatchCountNotUseCount),
                STATUS_INVALID_PARAMETER,
            );
        };
        // SAFETY: `plan_capability_table` proved `offset + 48 <= DmaSize`.
        unsafe {
            core::ptr::copy_nonoverlapping(
                bytemuck::bytes_of(&capability).as_ptr(),
                (args.pDmaBuffer as *mut u8).add(offset as usize),
                size_of::<Hnr2PhysicalCapability>(),
            );
        }

        // SAFETY: `i < plan.count <= PatchLocationListOutSize` and the pointer
        // is non-null whenever the count is nonzero.
        let out = unsafe { &mut *args.pPatchLocationListOut.add(i as usize) };
        // SAFETY: one `D3DDDI_PATCHLOCATIONLIST` this Render owns; zeroing first
        // is what keeps a recycled entry's `SlotId` union out of the record.
        unsafe { core::ptr::write_bytes(out as *mut _, 0, 1) };
        out.AllocationIndex = record.allocation_list_index;
        // ⛔ The capability ORDINAL, so the table and the patch list are indexed
        // by the same number and neither can be re-derived from the other.
        out.DriverId = i;
        out.PatchOffset = offset;
        out.AllocationOffset = 0;
        out.SplitOffset = 0;
    }

    // Only the control context owns the session's reply pool (§10.4:1205).
    if native.class != NativeClass::Control && accept.has_reply {
        return refuse(RenderRefusal::ReplyOnQueueContext, STATUS_INVALID_PARAMETER);
    }

    // K11 executes only this finite, copied one-fragment INIT allowlist.  The
    // reply carrier is mandatory; reply-less control streams and every queue
    // payload remain at the K6 refusal boundary.
    let k11_init = native.class == NativeClass::Control
        && accept.has_reply
        && header.fragment_count == 1
        && header.total_payload_bytes
            == size_of::<helios_protocol::translation_session::HeliosTranslationSessionInitV1>()
                as u64;
    if native.class == NativeClass::Control && !accept.has_reply {
        NR2_NO_REPLY.fetch_add(1, Ordering::Relaxed);
        return STATUS_INVALID_PARAMETER;
    }
    if native.class == NativeClass::Control && !k11_init {
        NR2_NO_SCHEMA.fetch_add(1, Ordering::Relaxed);
        return STATUS_INVALID_PARAMETER;
    }

    // ── the §10.7 staging admission ─────────────────────────────────────────
    // This and the private-record publication are deliberately before the host
    // operation. Once the real host INIT succeeds, every remaining action in
    // this Render is infallible local publication/accounting.
    {
        let mut state = native.state.lock();
        if let Err(refusal) = state.staging_mut().checkout(header.total_payload_bytes) {
            drop(state);
            return refuse(refusal, STATUS_NO_MEMORY);
        }
    }
    NR2_SLOT_TAKEN.fetch_add(1, Ordering::Relaxed);

    let payload_bytes = header.total_payload_bytes.min(u32::MAX as u64) as u32;
    // SAFETY: `args` is dxgkrnl's live argument struct.
    if !unsafe { publish_dma_record(args, header, Some(&table), native.ring_index, payload_bytes) }
    {
        let _ = native
            .state
            .lock()
            .staging_mut()
            .retire(header.total_payload_bytes);
        return STATUS_INVALID_PARAMETER;
    }

    // ── the control-context reply slot, and the finite host INIT ─────────────
    if native.class == NativeClass::Control {
        let status = control_render(session, args, header, accept, uses, list_count);
        if status != STATUS_SUCCESS {
            // No SubmitCommand follows a failed Render, so give back the exact
            // staging admission here. `control_render` likewise gives back its
            // exact reply slot on every outcome.
            let _ = native
                .state
                .lock()
                .staging_mut()
                .retire(header.total_payload_bytes);
            return status;
        }
        debug_assert!(k11_init);
        // SAFETY: `publish_dma_record` succeeded above and the private-data
        // pointer is not advanced until after this exact marker is written.
        unsafe { mark_dma_host_completed(args, header.batch_token) };
    }

    // The finite INIT needs no retained host staging: by this point the host
    // operation and HVR1 publication are terminal. Every other payload remains
    // deliberately unimplemented and retains K6's boundary counters.
    if !k11_init {
        NR2_NO_STAGE.fetch_add(1, Ordering::Relaxed);
    }
    // Outside pure INIT, typed operands stay as the encoder wrote them: zero.
    // K11 resolves INIT's reply pool through the retained session allocation,
    // so its reply-target patch is not a missing-resid event. General resid
    // substitution belongs to the later allocation/GPU execution units.
    if patch_count != 0 && !k11_init {
        NR2_NO_RESID.fetch_add(1, Ordering::Relaxed);
    }
    // The payload was never classified against a generated opcode schema,
    // because none exists on this side. K6 does not execute it either.
    if !k11_init {
        NR2_NO_SCHEMA.fetch_add(1, Ordering::Relaxed);
    }

    // SAFETY: `plan_capability_table` proved the whole table is inside `DmaSize`.
    args.pDmaBuffer = unsafe {
        (args.pDmaBuffer as *mut u8).add(CAPABILITY_TABLE_OFFSET as usize + table.bytes as usize)
            as *mut c_void
    };
    // SAFETY: `publish_dma_record` returned true, so the buffer held the record.
    unsafe { advance_private_data(args) };
    args.PatchLocationListOutSize = plan.count;
    if !args.pPatchLocationListOut.is_null() {
        args.pPatchLocationListOut =
            unsafe { args.pPatchLocationListOut.add(plan.count as usize) };
    }
    args.MultipassOffset = 0;
    // The assembler moves HERE, after the last thing that could have refused.
    apply_render_fragment(&mut native.state.lock(), header, accept);
    NR2_FRAGMENTS.fetch_add(1, Ordering::Relaxed);
    NR2_COMMITS.fetch_add(1, Ordering::Relaxed);
    NR2_PATCH_SLOTS.fetch_add(plan.count, Ordering::Relaxed);
    NR2_COUNTERS.flush();
    STATUS_SUCCESS
}

/// Whether one classified control payload produced real reply bytes or only an
/// exact ownership cancellation.
#[derive(Clone, Copy)]
enum ControlPayloadOutcome {
    /// A real host reply and HVR1 payload were published.
    Published,
    /// The payload was refused before any host reply existed.
    Refused,
    /// INIT failed after closing this session's admission; finish teardown only
    /// after the caller aborts its exact checked-out slot.
    InitFailed,
}

impl ControlPayloadOutcome {
    const fn status(self) -> NTSTATUS {
        match self {
            Self::Published => STATUS_SUCCESS,
            Self::Refused | Self::InitFailed => STATUS_INVALID_PARAMETER,
        }
    }
}

/// The control context's half of a COMMIT: check out the reply slot, run the
/// finite HTS1 INIT if that is what the payload is, and give the slot back.
///
/// ⛔ THE SLOT IS ALWAYS GIVEN BACK ON A REFUSAL. `admit_control_render` takes
/// the first idle slot and A1 reuses slot 0 for every serial transaction
/// (`vn_helios_translation_session.c:145`), so a refusal that left the slot
/// `InFlight` would make every later control Render on this session fail
/// `ControlRenderSlotBusy` — a one-off refusal turned into a dead session.
fn control_render(
    session: core::ptr::NonNull<crate::ddi::translation_session::SessionObject>,
    args: &DXGKARG_RENDER,
    header: &HeliosNativeRenderV2,
    accept: &Hnr2Accept,
    uses: &[HeliosNativeRenderUse],
    list_count: usize,
) -> NTSTATUS {
    use crate::ddi::translation_session as hts1;

    // Resolved from the kernel allocation object, never from the wire: does the
    // one listed allocation belong to THIS session's reply pool?
    let mut names_reply_pool = false;
    let mut expected_allocation_generation = 0u64;
    let mut access_flags = 0u32;
    if accept.has_reply {
        let index = header.reply_allocation_list_index as usize;
        if index >= list_count || args.pAllocationList.is_null() {
            return refuse(
                RenderRefusal::Header(Hnr2Reject::ReplyIndexOutOfRange),
                STATUS_INVALID_PARAMETER,
            );
        }
        // SAFETY: `index < list_count` and the list has that many entries.
        let entry = unsafe { &*args.pAllocationList.add(index) };
        // SAFETY: `hDeviceSpecificAllocation` is an open handle this driver
        // minted, or something the accessor's magic check refuses.
        if let Some(identity) = unsafe {
            crate::ddi::create_allocation::open_allocation_identity(entry.hDeviceSpecificAllocation)
        } {
            names_reply_pool = identity.kind == crate::ddi::create_allocation::ALLOC_KIND_HVM1
                && hts1::reply_pool_generation(session) == Some(identity.generation);
        }
        let Some(record) = uses
            .iter()
            .find(|record| record.allocation_list_index as usize == index)
        else {
            return refuse(
                RenderRefusal::Table(Hnr2TableReject::ReplyIndexHasNoUseRecord),
                STATUS_INVALID_PARAMETER,
            );
        };
        expected_allocation_generation = record.expected_allocation_generation;
        access_flags = record.access_flags;
    }

    let request = helios_kmd_logic::translation_session::ControlRenderRequest {
        on_control_context: true,
        has_reply: accept.has_reply,
        allocation_count: args.AllocationListSize,
        names_reply_pool,
        expected_allocation_generation,
        access_flags,
        reply_offset: header.reply_offset,
        reply_capacity_bytes: header.reply_capacity_bytes,
        reply_slot_generation: header.reply_slot_generation,
        batch_token: header.batch_token,
        // The raw HVC1 control context predates every attach and has no HQA1
        // generation of its own.
        owner_context_generation: 0,
    };
    let admission = match hts1::admit_control_render(session, &request) {
        Ok(admission) => admission,
        Err(status) => return status,
    };
    NR2_CONTROL_RENDERS.fetch_add(1, Ordering::Relaxed);
    let Some(slot_index) = admission.slot_index else {
        // K11's finite allowlist contains only INIT, and INIT has an actual
        // host reply.  A reply-less control stream has no classified operation
        // behind it and is refused before SubmitCommand.
        NR2_NO_REPLY.fetch_add(1, Ordering::Relaxed);
        return STATUS_INVALID_PARAMETER;
    };

    // The finite operation and HVR1 publication are synchronous. Success
    // retires a genuinely-published reply; refusal cancels ownership without a
    // synthetic completion. Failed INIT has already closed admission, and only
    // after that exact cancellation may teardown destroy the host namespace.
    let outcome = run_control_payload(session, args, header, accept, &admission);
    match outcome {
        ControlPayloadOutcome::Published => {
            hts1::release_control_slot(session, slot_index, admission.slot_generation)
        }
        ControlPayloadOutcome::Refused => {
            hts1::abort_control_slot(session, slot_index, admission.slot_generation)
        }
        ControlPayloadOutcome::InitFailed => {
            hts1::abort_control_slot(session, slot_index, admission.slot_generation);
            hts1::finish_failed_session_init(session);
        }
    }
    outcome.status()
}

/// What the control payload is, and what K6 can do about it.
fn run_control_payload(
    session: core::ptr::NonNull<crate::ddi::translation_session::SessionObject>,
    args: &DXGKARG_RENDER,
    header: &HeliosNativeRenderV2,
    accept: &Hnr2Accept,
    admission: &helios_kmd_logic::translation_session::ControlRenderAdmission,
) -> ControlPayloadOutcome {
    use helios_protocol::translation_session::HeliosTranslationSessionInitV1;
    const INIT_BYTES: usize = size_of::<HeliosTranslationSessionInitV1>();

    // The finite HTS1 INIT is the one control payload this package understands,
    // and it always arrives whole: A1 sends it as one fragment.
    if header.fragment_count != 1 || header.total_payload_bytes != INIT_BYTES as u64 {
        // Any other payload is an opaque Venus control stream, and there is no
        // generated opcode schema on this side to classify it against — so it is
        // refused rather than admitted unclassified onto ring 0.
        NR2_NO_SCHEMA.fetch_add(1, Ordering::Relaxed);
        return ControlPayloadOutcome::Refused;
    }
    let mut raw = [0u8; INIT_BYTES];
    // SAFETY: `payload_offset`/`payload_bytes` were validated against
    // `CommandLength` by `HeliosNativeRenderV2::validate`, and the length equals
    // the destination exactly. `pCommand` is read only inside the shim's frame.
    let ok = unsafe {
        copy_from_command(
            raw.as_mut_ptr(),
            (args.pCommand as *const u8).add(accept.layout.payload_offset as usize),
            INIT_BYTES,
        )
    };
    if !ok {
        return ControlPayloadOutcome::Refused;
    }
    match crate::ddi::translation_session::session_init(
        session,
        &raw,
        admission.reply_offset,
        admission.reply_capacity_bytes,
        admission.slot_generation,
        admission.batch_token,
    ) {
        // K11 synchronously validated the actual host reply and release-
        // published HVR1/HTS1 into the admitted role-1 slot. The ordinary C51
        // signal submitted immediately after Render remains the user-visible
        // completion edge; no synthetic SubmissionFenceId is created here.
        Ok(_reply) => ControlPayloadOutcome::Published,
        // ⛔ NORMALISED. `session_init` answers STATUS_DEVICE_NOT_READY /
        // STATUS_INSUFFICIENT_RESOURCES, and neither is in `DxgkDdiRender`'s
        // documented return set — an illegal NTSTATUS out of a DDI is itself
        // logged by dxgkrnl as a driver bug. The real reason is in `TsInitRej`.
        Err(_status) => ControlPayloadOutcome::InitFailed,
    }
}

// ── `DxgkDdiPatch`, the HNR2 arm ─────────────────────────────────────────────

/// Snapshot the allocation-list placement into the capability records this
/// submission's Render emitted.
///
/// ⛔ INFALLIBLE. Returning an error from `DxgkDdiPatch` bugchecks Windows
/// (§10.7:1872), so every disagreement here counts and continues; the fallible
/// work — the tables, the plan, the generations — was all done at Render, where
/// a refusal is legal. Runs at PASSIVE_LEVEL (`d3dkmddi.h:4403`).
///
/// # Safety
/// `args` is dxgkrnl's live `DXGKARG_PATCH` for a submission on an HVC1 context.
pub(crate) unsafe fn patch(args: &DXGKARG_PATCH) {
    NR2_PATCH_CALLS.fetch_add(1, Ordering::Relaxed);
    NR2_PATCH_WINDOW.store(
        (args.DmaBufferPrivateDataSubmissionStartOffset << 16)
            | (args.DmaBufferPrivateDataSubmissionEndOffset & 0xFFFF),
        Ordering::Relaxed,
    );
    NR2_PATCH_WINDOW_TOTAL.store(args.DmaBufferPrivateDataSize, Ordering::Relaxed);
    // SAFETY: the private-data pair and window dxgkrnl supplied for this
    // submission.
    let Some(record) = (unsafe {
        read_dma_record(
            args.pDmaBufferPrivateData,
            args.DmaBufferPrivateDataSize,
            args.DmaBufferPrivateDataSubmissionStartOffset,
            args.DmaBufferPrivateDataSubmissionEndOffset,
        )
    }) else {
        NR2_PATCH_DIFF.fetch_add(1, Ordering::Relaxed);
        return;
    };
    if record.capability_count == 0 {
        return;
    }
    if args.pDmaBuffer.is_null() || args.pAllocationList.is_null() {
        NR2_PATCH_DIFF.fetch_add(1, Ordering::Relaxed);
        return;
    }
    // The offsets Render wrote are relative to ITS `pDmaBuffer`, which dxgkrnl
    // had already advanced to this submission's start. Here `pDmaBuffer` is the
    // buffer base, so the window has to be re-established before any of them
    // means anything.
    let submission_start = args.DmaBufferSubmissionStartOffset as usize;
    let submission_end = args.DmaBufferSubmissionEndOffset as usize;
    if submission_start > submission_end || submission_end > args.DmaBufferSize as usize {
        NR2_PATCH_DIFF.fetch_add(1, Ordering::Relaxed);
        return;
    }
    let submission_bytes = (submission_end - submission_start).min(u32::MAX as usize) as u32;
    let Ok(table) = plan_capability_table(
        record.capability_offset,
        record.capability_count,
        submission_bytes,
    ) else {
        NR2_PATCH_DIFF.fetch_add(1, Ordering::Relaxed);
        return;
    };

    // ⛔ THE ASSUMPTION THIS TURNS INTO A CHECK. Render wrote every offset
    // relative to ITS `pDmaBuffer`, which dxgkrnl had advanced to that Render's
    // start; here they are re-based on `DmaBufferSubmissionStartOffset`. The two
    // agree only if one Render is one submission — and dxgkrnl is free to batch
    // several into one, which is precisely why the offset pair exists. If it
    // ever does, `submission_start` is a DIFFERENT Render's start and every
    // write below would land at an arbitrary place in the DMA buffer.
    //
    // The 112-byte header copy Render puts at its own offset 0 is the witness:
    // if the batch token there matches the private record's, this submission
    // starts where that Render did. Anything else counts and patches nothing.
    if (submission_end - submission_start) < size_of::<HeliosNativeRenderV2>() {
        NR2_PATCH_DIFF.fetch_add(1, Ordering::Relaxed);
        return;
    }
    let mut witness = [0u8; size_of::<HeliosNativeRenderV2>()];
    // SAFETY: the window was bounded against `DmaBufferSize` above, and it holds
    // at least the header.
    unsafe {
        core::ptr::copy_nonoverlapping(
            (args.pDmaBuffer as *const u8).add(submission_start),
            witness.as_mut_ptr(),
            size_of::<HeliosNativeRenderV2>(),
        )
    };
    match bytemuck::try_pod_read_unaligned::<HeliosNativeRenderV2>(&witness) {
        Ok(header)
            if header.magic == helios_protocol::native_render::HELIOS_HNR2_MAGIC
                && header.batch_token == record.batch_token => {}
        _ => {
            NR2_PATCH_DIFF.fetch_add(1, Ordering::Relaxed);
            return;
        }
    }
    if args.pPatchLocationList.is_null() {
        return;
    }

    // Only this submission's slice of the patch list. dxgkrnl may hand the whole
    // context list with a window into it.
    let start = args.PatchLocationListSubmissionStart as usize;
    let len = args.PatchLocationListSubmissionLength as usize;
    let total = args.PatchLocationListSize as usize;
    let end = match start.checked_add(len) {
        Some(end) if end <= total => end,
        _ => {
            NR2_PATCH_DIFF.fetch_add(1, Ordering::Relaxed);
            return;
        }
    };
    let list_count = args.AllocationListSize as usize;

    for i in start..end {
        // SAFETY: `i < PatchLocationListSize` and the pointer is non-null.
        let entry = unsafe { &*args.pPatchLocationList.add(i) };
        // ⛔ UNTRUSTED EVEN THOUGH THIS DRIVER WROTE IT: dxgkrnl owns the buffer
        // between Render and here, so the offset is re-derived rather than
        // assumed.
        let Some(ordinal) = table.entry_at_offset(entry.PatchOffset) else {
            NR2_PATCH_DIFF.fetch_add(1, Ordering::Relaxed);
            continue;
        };
        if ordinal != entry.DriverId {
            NR2_PATCH_DIFF.fetch_add(1, Ordering::Relaxed);
            continue;
        }
        let index = entry.AllocationIndex as usize;
        if index >= list_count {
            NR2_PATCH_DIFF.fetch_add(1, Ordering::Relaxed);
            continue;
        }
        // SAFETY: `index < AllocationListSize` and the list is non-null.
        let allocation = unsafe { &*args.pAllocationList.add(index) };
        // ⚠ NOT SKIPPED WHEN ZERO. A snapshot of "not resident" is a real
        // snapshot: if Render placed the record and the allocation has since
        // been evicted, keeping the old physical address would be the stale
        // value this whole path exists to avoid.
        let segment = allocation.__bindgen_anon_1.SegmentId();
        // SAFETY: `entry_at_offset` proved `PatchOffset` names a whole slot
        // inside the submission window, and the window is inside `DmaBufferSize`.
        let slot = unsafe {
            (args.pDmaBuffer as *mut u8).add(submission_start + entry.PatchOffset as usize)
                as *mut Hnr2PhysicalCapability
        };
        // SAFETY: as above; the record was written by this driver's Render and
        // is 8-aligned because both the DMA buffer and the 112-byte header are.
        let mut capability = unsafe { core::ptr::read_unaligned(slot) };
        // SAFETY: the `PhysicalAddress` arm — HVC1 admits only
        // `VirtualAddressing = 0`.
        let physical =
            unsafe { allocation.__bindgen_anon_2.PhysicalAddress.as_ref().QuadPart } as u64;
        // The record's own span, which Render set from the allocation's size:
        // `DXGK_ALLOCATIONLIST` carries no length, so this is the only bound
        // available here and re-deriving it would be a second source.
        let allocation_bytes = capability.byte_length;
        // ⛔ SNAPSHOT, NOT `apply_placement`. §10.7 has Patch take "the exact
        // allocation-list placement" on EVERY call; VidMm evicting and re-paging
        // between Render and Patch is the single event this DDI exists for, and
        // `apply_placement` — correct for Render, which never re-places — calls
        // a moved allocation a disagreement and would keep the stale address.
        let placed_before = capability.is_placed();
        match snapshot_placement(&mut capability, segment, physical, allocation_bytes) {
            Ok(true) => {
                // SAFETY: as the read above.
                unsafe { core::ptr::write_unaligned(slot, capability) };
                NR2_PATCH_SNAPS.fetch_add(1, Ordering::Relaxed);
                NR2_NO_EPOCH.fetch_add(1, Ordering::Relaxed);
                if placed_before {
                    NR2_PATCH_RELOCATED.fetch_add(1, Ordering::Relaxed);
                }
            }
            // §18.2 invokes Patch twice: the second call finding the record
            // already correct is the contract, not an anomaly.
            Ok(false) => {}
            Err(_) => {
                NR2_PATCH_DIFF.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

// ── `DxgkDdiSubmitCommand`, the HNR2 arm ─────────────────────────────────────

/// What the scheduler-facing DDI may do after retiring one HNR2 record.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum NativeSubmitDisposition {
    /// This exact WDDM fence belongs to a still-current session whose finite
    /// host operation and HVR1 reply were already terminal before SubmitCommand.
    HostCompleted(u32),
    /// This packet never crossed K11's finite host boundary. Preserve K6's
    /// existing scheduler retirement behavior without claiming host execution.
    Refused,
    /// Render crossed K11's finite host boundary, but teardown/reset or an
    /// invalid context-local fence transition revoked completion authority
    /// before SubmitCommand. The DDI must accept the callback without routing
    /// this packet through any legacy/global completion path; reset owns the
    /// abandoned scheduler epoch.
    Revoked,
}

/// Retire one HNR2 submission's staging admission. K11's exact INIT token has
/// already completed its finite host operation synchronously in Render; every
/// other packet remains a counted K6 host-handoff refusal.
///
/// ⛔ RETURNS A LOCAL DISPOSITION, NOT AN NTSTATUS. A non-SUCCESS return from
/// `DxgkDdiSubmitCommand`
/// bugchecks `dxgmms2!VidSchiSendToExecutionQueue` 0x119 Arg1=2, so the refusal
/// is a counter and the packet is still accepted. Runs at DISPATCH_LEVEL
/// (`d3dkmddi.h:4491`): atomics and the leaf spinlock only, no registry write.
///
/// # Safety
/// `submit` is dxgkrnl's live argument struct for a submission on an HVC1
/// context.
pub(crate) unsafe fn submit(
    native: &NativeContext,
    session: core::ptr::NonNull<crate::ddi::translation_session::SessionObject>,
    submit: &DXGKARG_SUBMITCOMMAND,
) -> NativeSubmitDisposition {
    NR2_SUBMITS.fetch_add(1, Ordering::Relaxed);
    // ⛔ A RESUBMITTED PACKET MUST NOT RETIRE AGAIN. This driver advertises
    // DMA-buffer-boundary preemption and acks `DMA_PREEMPTED`
    // (`query_adapter_info.rs:395`, `dxgkddi_preempt_command`), and this file's
    // own `abandon_pending_submissions` records that "dxgkrnl resubmits the same
    // private record after it re-establishes residency". The record is state
    // dxgkrnl does not change, so the second arrival reads the same
    // `payload_bytes`; `StagingPool::retire` keys on nothing but that number, so
    // it would consume a DIFFERENT live submission's accounting and leak the
    // pool upward until legitimate COMMITs are refused — invisibly, because
    // `Nr2Slot` and `Nr2SlotRet` stay equal throughout.
    //
    // The WDK supplies the discriminator rather than us inventing one:
    // `DXGK_SUBMITCOMMANDFLAGS::Resubmission` (`d3dkmddi.h:4426`, bit 7).
    // SAFETY: `Value` is a plain UINT view of the (valid) flags union.
    let resubmission = (unsafe { submit.Flags.__bindgen_anon_1.Value } & (1 << 7)) != 0;
    // Recorded BEFORE the read, so a refused window still reports its shape.
    NR2_SUBMIT_WINDOW.store(
        (submit.DmaBufferPrivateDataSubmissionStartOffset << 16)
            | (submit.DmaBufferPrivateDataSubmissionEndOffset & 0xFFFF),
        Ordering::Relaxed,
    );
    NR2_SUBMIT_WINDOW_TOTAL.store(submit.DmaBufferPrivateDataSize, Ordering::Relaxed);
    // SAFETY: per this function's contract.
    let Some(record) = (unsafe {
        read_dma_record(
            submit.pDmaBufferPrivateData,
            submit.DmaBufferPrivateDataSize,
            submit.DmaBufferPrivateDataSubmissionStartOffset,
            submit.DmaBufferPrivateDataSubmissionEndOffset,
        )
    }) else {
        NR2_SUBMIT_NO_RECORD.fetch_add(1, Ordering::Relaxed);
        return NativeSubmitDisposition::Refused;
    };
    if resubmission {
        NR2_SUBMIT_RESUBMISSION.fetch_add(1, Ordering::Relaxed);
    } else if record.payload_bytes != 0 {
        let mut state = native.state.lock();
        let retired = state.staging_mut().retire(record.payload_bytes as u64);
        drop(state);
        match retired {
            Ok(()) => {
                NR2_SLOT_RETIRED.fetch_add(1, Ordering::Relaxed);
            }
            // The pure half declared `StagingUnderflow` precisely so the pool
            // cannot drift silently; discarding it was the drift.
            Err(_) => {
                NR2_SLOT_UNDERFLOW.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
    if record.flags != HELIOS_HNR2_KMD_DMA_FLAG_HOST_COMPLETED {
        // The host handoff, and the end of K6 for every operation outside K11's
        // finite INIT allowlist. There is still no allocation-backed or GPU
        // queue executor in this tranche.
        NR2_NO_HOST.fetch_add(1, Ordering::Relaxed);
        return NativeSubmitDisposition::Refused;
    }

    // A reset/Stop or ordinary session teardown can occur after Render's host
    // reply and before SubmitCommand. The caller already owns the adapter's
    // fixed K11 completion-rundown guard. Revalidate through the context's
    // direct strong session edge, keep both exact host-resource guards through
    // context-local fence admission, and never rediscover by a scalar identity.
    // Those session guards end on return; the caller retains only the adapter
    // completion guard through notification. Reset joins it, while ordinary
    // session teardown never waits on an OS callback that may itself be waiting
    // for a same-context Render to return.
    let disposition = crate::ddi::translation_session::with_current_host_submission(
        session,
        || {
            let fence = submit.SubmissionFenceId;
            let admission = native
                .host_submissions
                .lock()
                .admit_host_completion(fence, resubmission);
            if let Err(refusal) = admission {
                let code = match refusal {
                    HostSubmissionRefusal::Duplicate { .. } => 2,
                    HostSubmissionRefusal::WentBackward { .. } => 3,
                };
                bump_with_code(&NR2_HOST_SUBMIT_REJECT, code);
                return NativeSubmitDisposition::Revoked;
            }
            NR2_HOST_SUBMIT_OK.fetch_add(1, Ordering::Relaxed);
            NativeSubmitDisposition::HostCompleted(fence)
        },
    );
    disposition.unwrap_or_else(|| {
        bump_with_code(&NR2_HOST_SUBMIT_REJECT, 1);
        NativeSubmitDisposition::Revoked
    })
}

// ── `DxgkDdiSubmitCommandVirtual`, the HOS1 arm ──────────────────────────────

/// Validate one HOS1 descriptor on a D3D12-virtual HQA1 outer context.
///
/// ⛔ VALIDATED, NEVER ENQUEUED. The HOB1 lives at the submitted GPUVA and
/// §10.4 forbids the KMD from dereferencing it, so `cross_check` is not called
/// and nothing is executed. Like its physical sibling this returns nothing: a
/// non-SUCCESS return bugchecks the scheduler.
///
/// # Safety
/// `submit` is dxgkrnl's live argument struct for a submission on a context
/// whose role this driver resolved as an HQA1 attach.
pub(crate) unsafe fn submit_virtual(
    outer: &SpinLock<OuterSubmitContext>,
    submit: &DXGKARG_SUBMITCOMMANDVIRTUAL,
) {
    // The measurement the two texts disagree about. Legal at any IRQL.
    // SAFETY: `KeGetCurrentIrql` reads the current processor's IRQL and has no
    // preconditions.
    NR2_SUBMIT_VIRTUAL_IRQL.store(
        unsafe { wdk_sys::ntddk::KeGetCurrentIrql() } as u32,
        Ordering::Relaxed,
    );

    let bytes = size_of::<HeliosOuterSubmitV1>();
    // §10.4: dxgkrnl copies the UMD's prefix into the front of KMD private data,
    // and `DmaBufferUmdPrivateDataSize` is how many bytes of it are the UMD's.
    // A shorter prefix is not a HOS1.
    //
    // ⚠ `DXGKARG_SUBMITCOMMANDVIRTUAL` carries NO submission-offset pair, unlike
    // its physical sibling — the virtual DDI hands one submission's private data
    // directly — so offset 0 is the record here, and only here.
    if submit.pDmaBufferPrivateData.is_null()
        || (submit.DmaBufferPrivateDataSize as usize) < bytes
        || (submit.DmaBufferUmdPrivateDataSize as usize) < bytes
    {
        return;
    }
    let mut raw = [0u8; size_of::<HeliosOuterSubmitV1>()];
    // SAFETY: non-null with at least `bytes`, checked above.
    unsafe {
        core::ptr::copy_nonoverlapping(
            submit.pDmaBufferPrivateData as *const u8,
            raw.as_mut_ptr(),
            bytes,
        )
    };
    let record = match HeliosOuterSubmitV1::from_private_data(&raw) {
        Ok(record) => record,
        Err(reject) => {
            bump_with_code(
                &NR2_HOS1_REJECT,
                helios_kmd_logic::native_render::outer_submit_code(reject),
            );
            return;
        }
    };
    let mut context = outer.lock();
    match admit_hos1(&mut context, &record, submit.DmaBufferSize as u64) {
        Ok(()) => {
            drop(context);
            NR2_HOS1_OK.fetch_add(1, Ordering::Relaxed);
            // The boundary: a validated descriptor that names a GPUVA nobody
            // reads. K7/K8 own the HOB1; allocation/GPU dispatch remains a
            // later execution unit and is not part of K11's pure INIT.
            NR2_HOS1_NOT_EXECUTED.fetch_add(1, Ordering::Relaxed);
        }
        Err(refusal) => {
            drop(context);
            bump_with_code(&NR2_HOS1_REJECT, refusal.code());
        }
    }
}
