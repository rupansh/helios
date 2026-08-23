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
use core::ptr::NonNull;
use core::sync::atomic::{AtomicU32, Ordering};

use alloc::vec::Vec;

use helios_kmd_logic::native_render::{
    admit_commit_tables, admit_hos1, apply_placement, apply_render_fragment, build_capability,
    commit_physical_hob1, plan_capability_table, plan_output_patch_slots, snapshot_placement,
    validate_render_fragment, CapabilityTablePlan, OuterSubmitContext, RenderContext, RenderEnv,
    RenderRefusal,
};
use helios_kmd_logic::session_transport::{HostSubmissionRefusal, HostSubmissionState};
use helios_kmd_logic::venus_executor::validate_venus_a7_outer_stream;
use helios_protocol::native_render::kernel_dma::{
    Hnr2DmaReject, Hnr2KmdDmaPrivateV1, Hnr2PhysicalCapability, Hob1KmdDmaPrivateV1,
    HELIOS_HNR2_KMD_DMA_FLAG_HOST_COMPLETED, HELIOS_HOB1_KMD_DMA_ABI_VERSION,
    HELIOS_HOB1_KMD_DMA_BYTES, HELIOS_HOB1_KMD_DMA_MAGIC,
};
use helios_protocol::native_render::{
    HeliosNativeRenderPatch, HeliosNativeRenderUse, HeliosNativeRenderV2, Hnr2Accept, Hnr2Reject,
    Hnr2TableReject, HELIOS_HNR2_HEADER_SIZE, HELIOS_HNR2_MAX_FRAGMENTS,
    HELIOS_HNR2_MAX_OUTSTANDING_SUBMISSIONS, HELIOS_HNR2_MAX_PATCH_RECORDS,
    HELIOS_HNR2_OPERAND_KIND_HOST_RESOURCE_ID32, HELIOS_HVC1_ALLOCATION_LIST_ENTRIES,
    HELIOS_HVM1_ROLE_REPLY_POOL, HELIOS_HVM1_ROLE_VULKAN_DEVICE_LOCAL, HELIOS_HVR1_FLAG_FINAL,
    HELIOS_HVR1_HEADER_SIZE, HELIOS_HVR1_MAGIC, HELIOS_HVR1_VERSION,
};
use helios_protocol::wddm::{
    hob1_header, hob1_operand_records, hob1_payload, hob1_use_records, validate_batch_record,
    HeliosOuterBatchExpectation, HeliosOuterSubmitV1, HELIOS_HOB1_ACCESS_WRITE,
    HELIOS_HOB1_FLAG_D3D11_PHYSICAL, HELIOS_HOB1_FLAG_D3D12_VIRTUAL,
    HELIOS_HOB1_IDENTITY_D3D11_ALLOCATION_INDEX, HELIOS_HOB1_IDENTITY_D3D12_GPUVA,
    HELIOS_HOB1_MAX_USE_RECORDS, HELIOS_HOB1_OPERAND_KIND_GENERATED_RESOURCE,
    HELIOS_HOB1_OPERAND_WIDTH_4,
};

use crate::dxgk::*;
use crate::irql::PassiveLevel;
use crate::sync::{FixedVec, SpinLock};
use crate::virtio::gpu::SUBMIT_META_BYTES;
use crate::virtio::hal::DmaBuffer;
use wdk_sys::ntddk::{
    IoAllocateWorkItem, IoFreeWorkItem, IoQueueWorkItem, KeClearEvent, KeInitializeEvent,
    KeSetEvent, KeWaitForSingleObject,
};
use wdk_sys::{_WORK_QUEUE_TYPE, PIO_WORKITEM};

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
/// Staging retirements at the exact custody terminal: synchronous control work
/// at Render return, and queued work at its asynchronous host terminal.
///
/// ⚠ GRADE IT AS A PAIR WITH `Nr2Slot`, NOT AS A LEVEL. A persistent shortfall
/// means a checked-out payload still has live custody or missed its terminal.
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
/// HOS1 submissions whose exact HOC1 extent and K9 ticket entered the owning
/// context's bounded PASSIVE staging queue.
pub static NR2_OUTER_QUEUED: AtomicU32 = AtomicU32::new(0);
/// Outer execution refusals, packed `(count << 16) | OuterExecutionRefusal`.
pub static NR2_OUTER_REJECT: AtomicU32 = AtomicU32::new(0);
/// Generated resource operands substituted with a real virtio resource id —
/// the KMD half of the venus memory import, counted globally so a per-allocation
/// zero (`D2BnImp`) can be told apart from a dead instrument.
pub static NR2_IMPORT_SUBSTITUTIONS: AtomicU32 = AtomicU32::new(0);
/// The last resource id substituted into such an operand.
pub static NR2_IMPORT_LAST_RESOURCE: AtomicU32 = AtomicU32::new(0);
/// Fully validated HOB1 payloads accepted by the existing stock-Venus
/// endpoint.  This moves only after private operand patching and descriptor
/// publication, never from HOS1 validation alone.
pub static NR2_OUTER_HOST: AtomicU32 = AtomicU32::new(0);

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
/// The counter names, as one list, so the collision proof and the writer cannot
/// drift apart.
const COUNTER_NAMES: [&[u8]; 43] = [
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
    b"Nr2OuterQ",
    b"Nr2OuterRej",
    b"Nr2OuterHost",
    b"Nr2ImpN",
    b"Nr2ImpRid",
];

/// The boundary counters that did not fit [`COUNTER_NAMES`]'s block, mirrored
/// alongside it. Split only because a `CounterBlock` writes one registry value
/// per entry and 27 is already the largest block in this driver.
const BOUNDARY_NAMES: [&[u8]; 3] = [
    b"Nr2NoStage",
    b"Nr2NoEpoch",
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
        e(COUNTER_NAMES[38], &NR2_OUTER_QUEUED),
        f(COUNTER_NAMES[39], &NR2_OUTER_REJECT),
        e(COUNTER_NAMES[40], &NR2_OUTER_HOST),
        e(COUNTER_NAMES[41], &NR2_IMPORT_SUBSTITUTIONS),
        e(COUNTER_NAMES[42], &NR2_IMPORT_LAST_RESOURCE),
        e(BOUNDARY_NAMES[0], &NR2_NO_STAGE),
        e(BOUNDARY_NAMES[1], &NR2_NO_EPOCH),
        f(BOUNDARY_NAMES[2], &crate::device::CONTEXT_HANDLE_REFUSED),
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
    /// Generated-schema operands, and allocation-list-index to use-ordinal
    /// mapping. Both are fixed at context creation; COMMIT never allocates
    /// parser scratch from attacker-controlled counts.
    expected_operands: Vec<helios_kmd_logic::venus_executor::VenusOperand>,
    /// Bounded scratch for count expressions nested inside generated A7
    /// command records (currently acceleration-structure geometry arrays).
    /// It is allocated once with the context and never grows during Render.
    schema_counts: Vec<u32>,
    patch_order: Vec<u32>,
    use_ordinals: Vec<u32>,
    /// Exact HWA2 generations resolved for one outer batch.  The worker sorts
    /// the populated prefix to enforce HOB1's unique-allocation closure without
    /// allocating parser scratch per submission.
    outer_generations: Vec<u64>,
    /// The one incomplete batch permitted by HNR2. Its payload is copied into
    /// KMD-owned DMA memory fragment by fragment and becomes immutable at
    /// COMMIT.
    building: Option<BuildingBatch>,
    /// HVC1's one incomplete pure-control stream. It owns no executor slot or
    /// separate completion identity; COMMIT executes it synchronously.
    control_building: Option<ControlBuilding>,
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
        let mut expected_operands = Vec::new();
        expected_operands
            .try_reserve_exact(HELIOS_HNR2_MAX_PATCH_RECORDS as usize)
            .ok()?;
        expected_operands.resize(
            HELIOS_HNR2_MAX_PATCH_RECORDS as usize,
            helios_kmd_logic::venus_executor::VenusOperand::default(),
        );
        let mut schema_counts = Vec::new();
        schema_counts
            .try_reserve_exact(HELIOS_HVC1_ALLOCATION_LIST_ENTRIES as usize)
            .ok()?;
        schema_counts.resize(HELIOS_HVC1_ALLOCATION_LIST_ENTRIES as usize, 0);
        let mut patch_order = Vec::new();
        patch_order
            .try_reserve_exact(HELIOS_HNR2_MAX_PATCH_RECORDS as usize)
            .ok()?;
        patch_order.resize(HELIOS_HNR2_MAX_PATCH_RECORDS as usize, 0);
        let mut use_ordinals = Vec::new();
        use_ordinals
            .try_reserve_exact(HELIOS_HVC1_ALLOCATION_LIST_ENTRIES as usize)
            .ok()?;
        use_ordinals.resize(HELIOS_HVC1_ALLOCATION_LIST_ENTRIES as usize, u32::MAX);
        let mut outer_generations = Vec::new();
        outer_generations
            .try_reserve_exact(HELIOS_HOB1_MAX_USE_RECORDS as usize)
            .ok()?;
        outer_generations.resize(HELIOS_HOB1_MAX_USE_RECORDS as usize, 0);
        Some(Self {
            uses,
            patches,
            write_bits,
            expected_operands,
            schema_counts,
            patch_order,
            use_ordinals,
            outer_generations,
            building: None,
            control_building: None,
        })
    }
}

const EXECUTOR_SLOTS: usize = HELIOS_HNR2_MAX_OUTSTANDING_SUBMISSIONS as usize;
const BATCH_TICKETS: usize = HELIOS_HNR2_MAX_FRAGMENTS as usize;

#[derive(Clone, Copy, PartialEq, Eq)]
struct BatchIdentity {
    batch_token: u64,
    slot_generation: u64,
    payload_bytes: u32,
    full_payload_crc64: u64,
    fragment_count: u16,
    session_generation: u64,
    context_generation: u64,
    ring_index: u32,
}

#[derive(Clone, Copy)]
struct BatchTickets {
    entries: [Option<crate::adapter::OrderedEngineTicket>; BATCH_TICKETS],
    count: u16,
    resubmit_count: u16,
    /// Exactly one DMA record carries the COMMIT payload length.  Host work
    /// may start only when VidSch submits that record: earlier fragment
    /// callbacks have no allocation list/residency authority for the batch.
    commit_seen: bool,
}

impl BatchTickets {
    const fn new() -> Self {
        Self {
            entries: [None; BATCH_TICKETS],
            count: 0,
            resubmit_count: 0,
            commit_seen: false,
        }
    }

    fn append(
        &mut self,
        ticket: crate::adapter::OrderedEngineTicket,
        resubmission: bool,
        fragment_count: u16,
        commit_record: bool,
        ticket_is_live: impl Fn(crate::adapter::OrderedEngineTicket) -> bool,
    ) -> bool {
        if resubmission {
            // Reset/preempt invalidates the entire prior K9 epoch.  Detect that
            // transition from the tickets themselves on EVERY resubmission
            // cycle: one batch may legitimately be preempted more than once.
            // Within one cycle later fragments share the replacement ticket
            // epoch and append even if an already-terminal batch retired the
            // preceding fragment immediately.
            let prior_epoch = self.entries[..self.count as usize]
                .iter()
                .flatten()
                .next()
                .map(|old| old.epoch());
            let new_epoch = prior_epoch.is_none_or(|epoch| epoch != ticket.epoch());
            let any_live = self.entries[..self.count as usize]
                .iter()
                .flatten()
                .any(|old| ticket_is_live(*old));
            if new_epoch {
                if any_live {
                    return false;
                }
                self.entries = [None; BATCH_TICKETS];
                self.count = 0;
                self.commit_seen = false;
            }
            self.resubmit_count = self.resubmit_count.saturating_add(1);
        }
        if commit_record && self.commit_seen {
            return false;
        }
        let Some(next_count) = self.count.checked_add(1) else {
            return false;
        };
        if next_count > fragment_count || commit_record != (next_count == fragment_count) {
            return false;
        }
        let index = self.count as usize;
        let Some(slot) = self.entries.get_mut(index) else {
            return false;
        };
        *slot = Some(ticket);
        self.count = next_count;
        self.commit_seen |= commit_record;
        true
    }
}

struct NativeContextOperation {
    owner: NonNull<NativeContext>,
}

unsafe impl Send for NativeContextOperation {}

impl Drop for NativeContextOperation {
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

#[derive(Clone, Copy)]
struct NativeContextRundown {
    open: bool,
    active: u32,
}

enum OuterCustody {
    Physical {
        _context: NativeContextOperation,
        _session: crate::ddi::session_transport::SessionExecutionOperation,
        _allocations: Vec<crate::ddi::create_allocation::OpenOuterUse>,
    },
    Virtual {
        _context: NativeContextOperation,
        _session: crate::ddi::session_transport::SessionExecutionOperation,
        _command_pool: crate::ddi::create_allocation::OpenOuterUse,
        _allocations: Vec<crate::ddi::create_allocation::OpenOuterUse>,
    },
}

impl OuterCustody {
    fn session(&self) -> &crate::ddi::session_transport::SessionExecutionOperation {
        match self {
            Self::Physical { _session, .. } | Self::Virtual { _session, .. } => _session,
        }
    }
}

enum NativeCustody {
    Hnr2 {
        _staging: StagingCustody,
        _session: crate::ddi::session_transport::SessionExecutionOperation,
        _allocations: Vec<crate::ddi::create_allocation::OpenExecutionUse>,
        reply: Option<ExecutionReply>,
    },
    Outer(OuterCustody),
}

impl NativeCustody {
    fn host_context_id(&self) -> u32 {
        match self {
            Self::Hnr2 { _session, .. } => _session.context_id,
            Self::Outer(custody) => custody.session().context_id,
        }
    }

    fn transport_instance(&self) -> u64 {
        match self {
            Self::Hnr2 { _session, .. } => _session.transport_instance,
            Self::Outer(custody) => custody.session().transport_instance,
        }
    }

    fn take_reply(&mut self) -> Option<ExecutionReply> {
        match self {
            Self::Hnr2 { reply, .. } => reply.take(),
            Self::Outer(_) => None,
        }
    }
}

/// Move-only ownership carried by the existing virtio-gpu in-flight entry.
/// The stock used-ring response is the only normal producer of a terminal;
/// transport reset may produce a failed terminal only after device status is
/// proven zero.
pub(crate) struct NativeHostCompletion {
    context: NonNull<NativeContext>,
    identity: BatchIdentity,
    slot_index: u32,
    custody: Option<NativeCustody>,
}

// SAFETY: the context/session/allocation operation guards touch only pinned
// nonpaged objects and their leaf locks/events. Their rundown joins before the
// owning objects are freed.
unsafe impl Send for NativeHostCompletion {}

pub(crate) struct NativeHostTerminal {
    completion: NativeHostCompletion,
    host_ok: bool,
}

impl NativeHostCompletion {
    pub(crate) fn terminal(self, host_ok: bool) -> NativeHostTerminal {
        NativeHostTerminal {
            completion: self,
            host_ok,
        }
    }
}

struct ExecutionReply {
    session: NonNull<crate::ddi::translation_session::SessionObject>,
    facts: crate::ddi::create_allocation::K11ReplyPoolFacts,
    slot_index: usize,
    slot_generation: u64,
    batch_token: u64,
    snapshot_generation: u64,
    session_generation: u64,
    reply_offset: u64,
    reply_capacity: u64,
    finished: bool,
}

unsafe impl Send for ExecutionReply {}

impl ExecutionReply {
    const RAW_ALLOCATE_REPLY_BYTES: u64 = 24;

    fn finish(mut self, host_ok: bool) -> bool {
        let mut published = false;
        let session_current = host_ok
            && crate::ddi::translation_session::execution_session_generation(self.session)
                == Some(self.session_generation);
        let raw_reply_offset = self
            .reply_offset
            .checked_add(HELIOS_HVR1_HEADER_SIZE as u64);
        let private_reply_copied = session_current
            && raw_reply_offset.is_some_and(|offset| {
                crate::ddi::translation_session::complete_generated_reply(
                    self.session,
                    self.facts,
                    offset,
                    Self::RAW_ALLOCATE_REPLY_BYTES,
                    helios_kmd_logic::venus_executor::OP_ALLOCATE_MEMORY,
                )
                .is_ok()
            });
        if private_reply_copied {
            let payload = unsafe {
                self.facts
                    .kernel_va
                    .as_ptr()
                    .add(self.reply_offset as usize + HELIOS_HVR1_HEADER_SIZE as usize)
            };
            let opcode = unsafe { core::ptr::read_unaligned(payload.cast::<u32>()) };
            let status = unsafe { core::ptr::read_unaligned(payload.add(4).cast::<i32>()) };
            let output_present = unsafe { core::ptr::read_unaligned(payload.add(8).cast::<u64>()) };
            if opcode == helios_kmd_logic::venus_executor::OP_ALLOCATE_MEMORY && output_present == 1
            {
                let header = helios_protocol::native_render::HeliosVenusReplyV1 {
                    magic: HELIOS_HVR1_MAGIC,
                    version: HELIOS_HVR1_VERSION,
                    header_size: HELIOS_HVR1_HEADER_SIZE as u16,
                    package_generation: helios_protocol::HELIOS_PACKAGE_GENERATION,
                    session_generation: self.session_generation,
                    slot_generation: self.slot_generation,
                    batch_token: self.batch_token,
                    snapshot_generation: self.snapshot_generation,
                    opcode,
                    status,
                    total_bytes: Self::RAW_ALLOCATE_REPLY_BYTES,
                    chunk_offset: 0,
                    chunk_bytes: Self::RAW_ALLOCATE_REPLY_BYTES as u32,
                    flags: HELIOS_HVR1_FLAG_FINAL,
                };
                published =
                    crate::ddi::session_transport::SessionTransport::publish_hvr1_existing_payload(
                        self.facts,
                        self.reply_offset,
                        self.reply_capacity,
                        &header,
                        Self::RAW_ALLOCATE_REPLY_BYTES,
                    )
                    .is_ok();
            }
        }
        if published {
            crate::ddi::translation_session::release_control_slot(
                self.session,
                self.slot_index,
                self.slot_generation,
            );
        } else {
            crate::ddi::translation_session::abort_control_slot(
                self.session,
                self.slot_index,
                self.slot_generation,
            );
        }
        self.finished = true;
        published
    }
}

impl Drop for ExecutionReply {
    fn drop(&mut self) {
        if !self.finished {
            crate::ddi::translation_session::abort_control_slot(
                self.session,
                self.slot_index,
                self.slot_generation,
            );
            self.finished = true;
        }
    }
}

struct ReadyBatch {
    payload: DmaBuffer,
    meta: DmaBuffer,
    custody: NativeCustody,
    /// A7 proved an allocation-only destroy/free terminal with no queue op.
    terminal_teardown: bool,
}

struct BuildingBatch {
    identity: BatchIdentity,
    slot_index: u32,
    payload: DmaBuffer,
    meta: DmaBuffer,
    staging: StagingCustody,
    session: crate::ddi::session_transport::SessionExecutionOperation,
}

struct ControlBuilding {
    batch_token: u64,
    total_payload_bytes: u64,
    full_payload_crc64: u64,
    fragment_count: u16,
    payload: DmaBuffer,
}

/// One exact checkout from the existing per-context HNR2 staging model.
/// Queue payload bytes remain charged from BEGIN until the real host terminal
/// (or an earlier explicit refusal) releases the immutable DMA copy.
struct StagingCustody {
    context: NativeContextOperation,
    bytes: u64,
}

impl Drop for StagingCustody {
    fn drop(&mut self) {
        let native = unsafe { self.context.owner.as_ref() };
        match native.state.lock().staging_mut().retire(self.bytes) {
            Ok(()) => {
                NR2_SLOT_RETIRED.fetch_add(1, Ordering::Relaxed);
            }
            Err(_) => {
                NR2_SLOT_UNDERFLOW.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

enum SubmissionSlot {
    Free,
    Collecting {
        identity: BatchIdentity,
        tickets: BatchTickets,
    },
    Ready {
        identity: BatchIdentity,
        tickets: BatchTickets,
        batch: Option<ReadyBatch>,
    },
    InFlight {
        identity: BatchIdentity,
        tickets: BatchTickets,
    },
    Terminal {
        identity: BatchIdentity,
        tickets: BatchTickets,
        success: bool,
        /// Only enqueue failure leaves DMA buffers here. Host-owned buffers
        /// park in VirtioGpu and are reaped at PASSIVE by the existing pool.
        cleanup: Option<(DmaBuffer, DmaBuffer)>,
    },
}

enum CloseSlotWork {
    None,
    Fail(BatchTickets),
    DropReady(BatchTickets, ReadyBatch),
}

struct ExecutorState {
    slots: Vec<SubmissionSlot>,
    next_slot_generation: u64,
}

impl ExecutorState {
    fn new() -> Option<Self> {
        let mut slots = Vec::new();
        slots.try_reserve_exact(EXECUTOR_SLOTS).ok()?;
        for _ in 0..EXECUTOR_SLOTS {
            slots.push(SubmissionSlot::Free);
        }
        Some(Self {
            slots,
            next_slot_generation: 1,
        })
    }

    fn mint_slot_generation(&mut self) -> Option<u64> {
        let generation = self.next_slot_generation;
        if generation == 0 {
            return None;
        }
        self.next_slot_generation = generation.checked_add(1)?;
        Some(generation)
    }
}

/// Stable reasons the outer path can refuse before or during exact HOB1
/// execution.  The registry counter stores the latest code alongside its
/// count; source gates pin these names so a missing/stale/cross-device token
/// can never collapse into an anonymous submit failure.
#[repr(u32)]
#[derive(Clone, Copy)]
enum OuterExecutionRefusal {
    PrivateData = 1,
    Descriptor = 2,
    FenceOrder = 3,
    ContextClosed = 4,
    SessionClosed = 5,
    SlotExhausted = 6,
    ResubmissionMismatch = 7,
    CommandPoolMissing = 8,
    WorkerClosed = 9,
    WorkerFull = 10,
    SnapshotFailed = 11,
    BatchRecord = 12,
    SubmitCrossCheck = 13,
    UseMissingOrForeign = 14,
    UseStaleGeneration = 15,
    DuplicateUse = 16,
    TransportMismatch = 17,
    Schema = 18,
    OperandClosure = 19,
    OperandWidth = 20,
    PatchBounds = 21,
    HostUnavailable = 22,
    HostEnqueue = 23,
    WorkerIrql = 24,
}

impl OuterExecutionRefusal {
    fn record(self) {
        bump_with_code(&NR2_OUTER_REJECT, self as u32);
    }
}

/// One HOS1 submission after all DISPATCH-safe identity checks have succeeded.
/// It carries direct object guards, never scalar rediscovery keys, into the
/// owning context's PASSIVE work item.
struct OuterPending {
    identity: BatchIdentity,
    slot_index: u32,
    prior_batch_id: u64,
    submit: HeliosOuterSubmitV1,
    device: NonNull<crate::device::DeviceContext>,
    session: NonNull<crate::ddi::translation_session::SessionObject>,
    command_pool: crate::ddi::create_allocation::OpenOuterUse,
    context_operation: NativeContextOperation,
    session_operation: crate::ddi::session_transport::SessionExecutionOperation,
}

// Every field is either immutable scalar state or a move-only rundown guard
// whose own type is Send. The pointed-to device/session/context objects are
// heap-pinned and cannot be freed until those guards are dropped.
unsafe impl Send for OuterPending {}

struct OuterWorkerState {
    open: bool,
    queued: bool,
    pending: FixedVec<OuterPending>,
}

struct OuterWorker {
    item: PIO_WORKITEM,
    state: SpinLock<OuterWorkerState>,
    drained: UnsafeCell<KEVENT>,
}

unsafe impl Send for OuterWorker {}
unsafe impl Sync for OuterWorker {}

impl OuterWorker {
    fn new(adapter: &crate::adapter::AdapterContext) -> Option<Self> {
        // PASSIVE_LEVEL: NativeContext::new is called only from CreateContext.
        let item = unsafe { IoAllocateWorkItem(adapter.physical_device_object()) };
        if item.is_null() {
            return None;
        }
        Some(Self {
            item,
            state: SpinLock::new(OuterWorkerState {
                open: true,
                queued: false,
                pending: FixedVec::with_max(EXECUTOR_SLOTS),
            }),
            drained: UnsafeCell::new(unsafe { core::mem::zeroed() }),
        })
    }

    /// Initialize the embedded dispatcher object only after NativeContext has
    /// reached its final boxed address.
    unsafe fn init_event(&self) {
        unsafe { KeInitializeEvent(self.drained.get(), 0, 1) };
    }

    fn enqueue(
        &self,
        native: NonNull<NativeContext>,
        pending: OuterPending,
    ) -> Result<(), (OuterExecutionRefusal, OuterPending)> {
        let schedule = {
            let mut state = self.state.lock();
            if !state.open {
                return Err((OuterExecutionRefusal::WorkerClosed, pending));
            }
            let schedule = !state.queued;
            state
                .pending
                .try_push(pending)
                .map_err(|pending| (OuterExecutionRefusal::WorkerFull, pending))?;
            if schedule {
                state.queued = true;
                unsafe { KeClearEvent(self.drained.get()) };
            }
            schedule
        };
        if schedule {
            // IoQueueWorkItem is legal through DISPATCH_LEVEL. The context
            // pointer is direct callback custody, never an identity lookup.
            unsafe {
                IoQueueWorkItem(
                    self.item,
                    Some(outer_work_item),
                    _WORK_QUEUE_TYPE::DelayedWorkQueue,
                    native.as_ptr().cast::<c_void>(),
                )
            };
        }
        Ok(())
    }

    fn take_next(&self) -> Option<(bool, OuterPending)> {
        let mut state = self.state.lock();
        if state.pending.len() != 0 {
            let open = state.open;
            return Some((open, state.pending.remove(0)));
        }
        state.queued = false;
        unsafe { KeSetEvent(self.drained.get(), 0, 0) };
        None
    }

    fn close_and_wait(&self) {
        let wait = {
            let mut state = self.state.lock();
            state.open = false;
            if !state.queued {
                unsafe { KeSetEvent(self.drained.get(), 0, 0) };
            }
            state.queued
        };
        if wait {
            let _ = unsafe {
                KeWaitForSingleObject(
                    self.drained.get() as wdk_sys::PVOID,
                    0,
                    0,
                    0,
                    core::ptr::null_mut(),
                )
            };
        }
    }
}

impl Drop for OuterWorker {
    fn drop(&mut self) {
        // Context destruction and every constructor failure run at PASSIVE.
        unsafe { IoFreeWorkItem(self.item) };
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
    /// HQA1 outer context.  It uses the same endpoint and K9/used-ring
    /// completion graph as `Queue`, but accepts only complete HOB1 records from
    /// the owning UMD path.
    Outer,
}

/// One HVC1 context's K6 state, allocated at `DxgkDdiCreateContext` and owned by
/// [`crate::device::ContextContext`]'s role.
pub(crate) struct NativeContext {
    adapter: NonNull<crate::adapter::AdapterContext>,
    class: NativeClass,
    /// The host ring this context's submissions execute on.
    ///
    /// Control is ring zero.  A queue context owns one direct, nonzero,
    /// non-recycled endpoint selected by its session at context creation.
    ring_index: u32,
    /// Exact HTS1 endpoint identity.  It is currently numerically equal to the
    /// ring index for real endpoints, but remains a separate immutable fact so
    /// HQA1/HOB1 validation never depends on that implementation coincidence.
    endpoint_id: u32,
    /// Exact session and context-local generations written into every private
    /// DMA record and rechecked by the context slot. For queue contexts the
    /// non-recycled endpoint ordinal is also the context generation.
    session_generation: u64,
    context_generation: u64,
    /// One exact WDDM SubmissionFenceId watermark for this HVC1 context.  It is
    /// neither an adapter-wide boundary queue nor a host/shared timeline.
    host_submissions: SpinLock<HostSubmissionState>,
    /// The HNR2 assembler and its staging accounting. The §10.7:2041 short lock:
    /// taken for the admission decision only, never held across the user copy,
    /// a host call, or a wait.
    state: SpinLock<RenderContext>,
    executor: SpinLock<ExecutorState>,
    /// Present only on HQA1 outer contexts. One context-owned work item moves
    /// full HOB1 snapshot/CRC/schema work out of SubmitCommandVirtual's
    /// DISPATCH-level window; it never forms an adapter/global queue.
    outer_worker: Option<OuterWorker>,
    rundown: SpinLock<NativeContextRundown>,
    drained: UnsafeCell<KEVENT>,
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
// `SpinLock`s; `class`/`ring_index`/`endpoint_id` are written once at
// construction and read-only afterwards; `scratch` is reachable only through
// [`NativeContext::claim`], which hands out at most one reference at a time via
// `busy`.
unsafe impl Send for NativeContext {}
// SAFETY: as above.
unsafe impl Sync for NativeContext {}

impl NativeContext {
    /// `None` when the COMMIT scratch cannot be allocated; the caller must then
    /// fail `DxgkDdiCreateContext`.
    pub(crate) fn new(
        adapter: NonNull<crate::adapter::AdapterContext>,
        class: NativeClass,
        ring_index: u32,
        endpoint_id: u32,
        session_generation: u64,
        context_generation: u64,
    ) -> Option<alloc::boxed::Box<Self>> {
        if match class {
            NativeClass::Control => ring_index != 0 || endpoint_id != 0,
            NativeClass::Queue | NativeClass::Outer => ring_index == 0 || endpoint_id == 0,
        } {
            return None;
        }
        if matches!(class, NativeClass::Queue | NativeClass::Outer)
            && (session_generation == 0 || context_generation == 0)
        {
            return None;
        }
        let Some(scratch) = Hnr2Scratch::new() else {
            NR2_SCRATCH_ALLOC_FAILED.fetch_add(1, Ordering::Relaxed);
            return None;
        };
        let executor = ExecutorState::new()?;
        let outer_worker = if class == NativeClass::Outer {
            Some(OuterWorker::new(unsafe { adapter.as_ref() })?)
        } else {
            None
        };
        let native = alloc::boxed::Box::new(Self {
            adapter,
            class,
            ring_index,
            endpoint_id,
            session_generation,
            context_generation,
            host_submissions: SpinLock::new(HostSubmissionState::new()),
            state: SpinLock::new(RenderContext::new()),
            executor: SpinLock::new(executor),
            outer_worker,
            rundown: SpinLock::new(NativeContextRundown {
                open: true,
                active: 0,
            }),
            drained: UnsafeCell::new(unsafe { core::mem::zeroed() }),
            busy: AtomicU32::new(0),
            scratch: UnsafeCell::new(scratch),
        });
        unsafe { KeInitializeEvent(native.drained.get(), 0, 1) };
        if let Some(worker) = native.outer_worker.as_ref() {
            unsafe { worker.init_event() };
        }
        Some(native)
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

    fn acquire_operation(&self) -> Option<NativeContextOperation> {
        let mut rundown = self.rundown.lock();
        if !rundown.open || rundown.active == u32::MAX {
            return None;
        }
        if rundown.active == 0 {
            unsafe { KeClearEvent(self.drained.get()) };
        }
        rundown.active += 1;
        Some(NativeContextOperation {
            owner: NonNull::from(self),
        })
    }

    /// Stop new Render/Submit admissions, fail every batch that never reached
    /// the stock transport, and join exact in-flight host custody. No transport
    /// polling or synthetic completion is involved: an InFlight slot can release
    /// its rundown only from a used-ring response or a proven physical reset.
    pub(crate) fn close(&self, passive: PassiveLevel) {
        {
            let mut rundown = self.rundown.lock();
            rundown.open = false;
            if rundown.active == 0 {
                unsafe { KeSetEvent(self.drained.get(), 0, 0) };
            }
        }

        // No new HOS1 can enter after the rundown closes. Join this context's
        // sole PASSIVE staging item before walking executor slots so no worker
        // can race a Ready/InFlight transition with teardown.
        if let Some(worker) = self.outer_worker.as_ref() {
            worker.close_and_wait();
        }

        // DestroyContext is serialized with Render for this handle. Give back a
        // partially assembled batch before walking its corresponding slot.
        let scratch = unsafe { &mut *self.scratch.get() };
        let building = scratch.building.take();
        abandon_control_building(scratch, self);

        let adapter = unsafe { self.adapter.as_ref() };
        for index in 0..EXECUTOR_SLOTS {
            let work = {
                let mut executor = self.executor.lock();
                let Some(slot) = executor.slots.get_mut(index) else {
                    continue;
                };
                let old = core::mem::replace(slot, SubmissionSlot::Free);
                match old {
                    SubmissionSlot::Collecting { identity, tickets } => {
                        *slot = SubmissionSlot::Terminal {
                            identity,
                            tickets,
                            success: false,
                            cleanup: None,
                        };
                        CloseSlotWork::Fail(tickets)
                    }
                    SubmissionSlot::Ready {
                        identity,
                        tickets,
                        batch,
                    } => {
                        *slot = SubmissionSlot::Terminal {
                            identity,
                            tickets,
                            success: false,
                            cleanup: None,
                        };
                        match batch {
                            Some(batch) => CloseSlotWork::DropReady(tickets, batch),
                            None => CloseSlotWork::Fail(tickets),
                        }
                    }
                    other => {
                        *slot = other;
                        CloseSlotWork::None
                    }
                }
            };
            match work {
                CloseSlotWork::None => {}
                CloseSlotWork::Fail(tickets) => {
                    settle_batch_tickets(adapter, tickets, false);
                }
                CloseSlotWork::DropReady(tickets, mut batch) => {
                    if let Some(reply) = batch.custody.take_reply() {
                        let _ = reply.finish(false);
                    }
                    settle_batch_tickets(adapter, tickets, false);
                    drop(batch);
                }
            }
        }
        // A partial batch can already own BEGIN/DATA K9 tickets. Keep its
        // session/context/staging custody until every such ticket has taken the
        // explicit failure transition above.
        drop(building);

        drain_host_terminals(adapter);
        let _ = unsafe {
            KeWaitForSingleObject(
                self.drained.get() as wdk_sys::PVOID,
                0,
                0,
                0,
                core::ptr::null_mut(),
            )
        };
        // A reset path may have queued failed terminals immediately before the
        // final rundown release. Discharge them and free only PASSIVE-owned
        // enqueue-failure buffers before the context box disappears.
        drain_host_terminals(adapter);
        reap_terminal_slots(self, passive);
    }
}

fn settle_batch_tickets(
    adapter: &crate::adapter::AdapterContext,
    tickets: BatchTickets,
    success: bool,
) {
    for ticket in tickets.entries[..tickets.count as usize].iter().flatten() {
        if success {
            let _ = crate::ddi::interrupt::complete_ordered_engine_submission(adapter, *ticket);
        } else {
            let _ = crate::ddi::interrupt::fail_ordered_engine_submission(adapter, *ticket);
        }
    }
}

fn fail_inflight_without_completion(
    native: &NativeContext,
    adapter: &crate::adapter::AdapterContext,
    identity: BatchIdentity,
    slot_index: u32,
) {
    let tickets = {
        let mut executor = native.executor.lock();
        let Some(slot) = executor.slots.get_mut(slot_index as usize) else {
            return;
        };
        let old = core::mem::replace(slot, SubmissionSlot::Free);
        match old {
            SubmissionSlot::InFlight {
                identity: found,
                tickets,
            } if found == identity => {
                *slot = SubmissionSlot::Terminal {
                    identity: found,
                    tickets,
                    success: false,
                    cleanup: None,
                };
                Some(tickets)
            }
            other => {
                *slot = other;
                None
            }
        }
    };
    if let Some(tickets) = tickets {
        settle_batch_tickets(adapter, tickets, false);
    }
}

impl NativeHostCompletion {
    fn finish_with_cleanup(
        mut self,
        adapter: &crate::adapter::AdapterContext,
        host_ok: bool,
        cleanup: Option<(DmaBuffer, DmaBuffer)>,
    ) {
        let native = unsafe { self.context.as_ref() };
        let exact_adapter = core::ptr::eq(unsafe { native.adapter.as_ref() }, adapter);
        // Prove the terminal still belongs to this exact parked slot before a
        // reply byte is published.  Context close leaves InFlight untouched
        // and joins this custody, so no legitimate owner can change it here.
        let exact_slot = {
            let executor = native.executor.lock();
            matches!(
                executor.slots.get(self.slot_index as usize),
                Some(SubmissionSlot::InFlight { identity, .. })
                    if *identity == self.identity
            )
        };
        let mut success = host_ok && exact_adapter && exact_slot;
        let mut custody = self.custody.take();
        if let Some(reply) = custody.as_mut().and_then(NativeCustody::take_reply) {
            success &= reply.finish(success);
        }

        let mut cleanup = cleanup;
        let tickets = {
            let mut executor = native.executor.lock();
            if let Some(slot) = executor.slots.get_mut(self.slot_index as usize) {
                let old = core::mem::replace(slot, SubmissionSlot::Free);
                match old {
                    SubmissionSlot::InFlight { identity, tickets } if identity == self.identity => {
                        *slot = SubmissionSlot::Terminal {
                            identity,
                            tickets,
                            success,
                            cleanup: cleanup.take(),
                        };
                        Some(tickets)
                    }
                    other => {
                        *slot = other;
                        None
                    }
                }
            } else {
                None
            }
        };

        // The operation guards may be released at DISPATCH, but contiguous DMA
        // buffers may not. An identity mismatch is an invariant failure; leak
        // those two buffers rather than freeing them at the wrong IRQL.
        if let Some((meta, payload)) = cleanup.take() {
            core::mem::forget(meta);
            core::mem::forget(payload);
        }
        if let Some(tickets) = tickets {
            settle_batch_tickets(adapter, tickets, success);
        }
        // Keep the exact context/session/allocation/staging custody through the
        // K9 terminal transition.  Only actual host completion may make the
        // WDDM tickets retire, and no referenced object may disappear between
        // that transition and its notification path.
        drop(custody);
    }
}

impl NativeHostTerminal {
    fn finish(self, adapter: &crate::adapter::AdapterContext) {
        self.completion
            .finish_with_cleanup(adapter, self.host_ok, None);
    }
}

/// Discharge host terminals after leaving `virtio_lock`. This is called by the
/// ordinary DPC and synchronously after a physical reset, so every K9 mutation
/// preserves the existing notification-lock ordering.
pub(crate) fn drain_host_terminals(adapter: &crate::adapter::AdapterContext) {
    // At most MAX_INFLIGHT native entries can exist.  Recheck after returning
    // the spare vector so a response appended while the prior batch was being
    // settled cannot depend on a later unrelated interrupt.  This is a finite
    // drain of already-terminal used-ring results, not host polling.
    for _ in 0..crate::virtio::gpu::MAX_INFLIGHT {
        let work = adapter.with_virtio(|gpu| gpu.begin_native_terminal_drain());
        let Ok(Some(mut terminals)) = work else {
            return;
        };
        while let Some(terminal) = terminals.pop() {
            terminal.finish(adapter);
        }
        let mut pending = Some(terminals);
        let returned = adapter.with_virtio(|gpu| {
            if let Some(terminals) = pending.take() {
                gpu.finish_native_terminal_drain(terminals);
            }
        });
        if returned.is_err() {
            // The vector is empty, but freeing its allocation at DISPATCH is
            // still outside the allocator contract. Transport removal normally
            // joins the DPC; retain it if that lifecycle invariant is violated.
            if let Some(terminals) = pending.take() {
                core::mem::forget(terminals);
            }
            return;
        }
    }
    // A continuously completing producer exhausted this bounded service pass.
    // Preserve forward progress without spinning here: schedule the same
    // ordinary completion DPC to resume the exact terminal queue.
    crate::ddi::interrupt::request_wddm_completion_dpc(adapter);
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

fn reap_terminal_slots(native: &NativeContext, _passive: PassiveLevel) {
    let adapter = unsafe { native.adapter.as_ref() };
    for index in 0..EXECUTOR_SLOTS {
        let snapshot = {
            let executor = native.executor.lock();
            match executor.slots.get(index) {
                Some(SubmissionSlot::Terminal {
                    identity, tickets, ..
                }) => Some((*identity, *tickets)),
                _ => None,
            }
        };
        let Some((identity, tickets)) = snapshot else {
            continue;
        };
        let retired = adapter.with_wddm_notify_lock(|guard| {
            tickets.entries[..tickets.count as usize]
                .iter()
                .flatten()
                .all(|ticket| guard.ordered_engine_ticket_was_retired(*ticket))
        });
        if !retired {
            continue;
        }
        let cleanup = {
            let mut executor = native.executor.lock();
            let Some(slot) = executor.slots.get_mut(index) else {
                continue;
            };
            match slot {
                SubmissionSlot::Terminal {
                    identity: found,
                    tickets: found_tickets,
                    cleanup,
                    ..
                } if *found == identity && found_tickets.count == tickets.count => {
                    let cleanup = cleanup.take();
                    *slot = SubmissionSlot::Free;
                    cleanup
                }
                _ => None,
            }
        };
        // PASSIVE_LEVEL: this is the only slot path that may destroy returned
        // contiguous DMA buffers.
        drop(cleanup);
    }
}

fn fail_outer_slot(
    native: &NativeContext,
    identity: BatchIdentity,
    slot_index: u32,
    refusal: OuterExecutionRefusal,
) {
    refusal.record();
    let adapter = unsafe { native.adapter.as_ref() };
    fail_inflight_without_completion(native, adapter, identity, slot_index);
}

/// Snapshot, validate, resolve, and privately patch one already-admitted HOB1
/// at PASSIVE_LEVEL. The only host enqueue is the same stock SUBMIT_3D path the
/// HNR2 executor uses; its used-ring terminal retains every exact object guard.
fn execute_outer_pending(
    native: &NativeContext,
    pending: OuterPending,
    passive: PassiveLevel,
) -> Result<(), OuterExecutionRefusal> {
    let OuterPending {
        identity,
        slot_index,
        prior_batch_id,
        submit,
        device,
        session,
        command_pool,
        context_operation,
        session_operation,
    } = pending;
    let adapter = unsafe { native.adapter.as_ref() };

    crate::virtio::ctrl::reap_parked(passive, adapter);
    reap_terminal_slots(native, passive);

    let record_len =
        usize::try_from(submit.hob1_bytes).map_err(|_| OuterExecutionRefusal::Descriptor)?;
    let mut record_buffer = adapter
        .with_virtio(|gpu| gpu.take_dma_buffer(record_len))
        .ok()
        .flatten()
        .or_else(|| DmaBuffer::new(passive, record_len))
        .ok_or(OuterExecutionRefusal::SnapshotFailed)?;
    let meta = adapter
        .with_virtio(|gpu| gpu.take_dma_buffer(SUBMIT_META_BYTES))
        .ok()
        .flatten()
        .or_else(|| DmaBuffer::new(passive, SUBMIT_META_BYTES))
        .ok_or(OuterExecutionRefusal::SnapshotFailed)?;
    if !command_pool.copy_hoc1(submit.hob1_bytes as u64, &mut record_buffer) {
        return Err(OuterExecutionRefusal::SnapshotFailed);
    }

    let expectation = HeliosOuterBatchExpectation {
        package_generation: helios_protocol::HELIOS_PACKAGE_GENERATION,
        session_generation: native.session_generation,
        context_generation: native.context_generation,
        endpoint_id: native.endpoint_id,
        flags: HELIOS_HOB1_FLAG_D3D12_VIRTUAL,
        max_command_bytes: submit.hob1_bytes as u64,
        last_batch_id: prior_batch_id,
        allocation_list_count: 0,
    };
    let record = record_buffer.as_slice();
    validate_batch_record(record, &expectation).map_err(|_| OuterExecutionRefusal::BatchRecord)?;
    let header = *hob1_header(record).map_err(|_| OuterExecutionRefusal::BatchRecord)?;
    submit
        .cross_check(&header)
        .map_err(|_| OuterExecutionRefusal::SubmitCrossCheck)?;
    let uses = hob1_use_records(record, &header).map_err(|_| OuterExecutionRefusal::BatchRecord)?;
    let operands =
        hob1_operand_records(record, &header).map_err(|_| OuterExecutionRefusal::BatchRecord)?;
    let payload = hob1_payload(record, &header).map_err(|_| OuterExecutionRefusal::BatchRecord)?;

    let mut allocations = Vec::new();
    allocations
        .try_reserve_exact(uses.len())
        .map_err(|_| OuterExecutionRefusal::SlotExhausted)?;
    let mut claim = native.claim().ok_or(OuterExecutionRefusal::ContextClosed)?;
    let scratch = claim.scratch();
    if uses.len() > scratch.outer_generations.len() || operands.len() > scratch.patch_order.len() {
        return Err(OuterExecutionRefusal::OperandClosure);
    }
    let exact_device = unsafe { device.as_ref() };
    for (index, record_use) in uses.iter().enumerate() {
        if record_use.identity_kind != HELIOS_HOB1_IDENTITY_D3D12_GPUVA {
            return Err(OuterExecutionRefusal::OperandClosure);
        }
        let Some(guard) = exact_device.acquire_outer_gpuva_use(
            session,
            record_use.address_or_index,
            record_use.byte_length,
            Some(record_use.expected_allocation_generation),
            false,
        ) else {
            return Err(
                if !crate::adapter::allocation_object::is_current(
                    record_use.expected_allocation_generation,
                ) {
                    OuterExecutionRefusal::UseStaleGeneration
                } else {
                    OuterExecutionRefusal::UseMissingOrForeign
                },
            );
        };
        if guard.generation() != record_use.expected_allocation_generation {
            return Err(OuterExecutionRefusal::UseStaleGeneration);
        }
        if guard.transport_instance() != Some(session_operation.transport_instance)
            || guard
                .resource_id()
                .is_none_or(|resource_id| resource_id == 0)
        {
            return Err(OuterExecutionRefusal::TransportMismatch);
        }
        scratch.outer_generations[index] = guard.generation();
        allocations.push(guard);
    }
    let generations = &mut scratch.outer_generations[..uses.len()];
    generations.sort_unstable();
    if generations.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(OuterExecutionRefusal::DuplicateUse);
    }

    let admission = validate_venus_a7_outer_stream(
        payload,
        &mut scratch.expected_operands,
        &mut scratch.schema_counts,
    )
    .map_err(|_| OuterExecutionRefusal::Schema)?;
    if admission.terminal_teardown && (uses.len() != 1 || !operands.is_empty()) {
        return Err(OuterExecutionRefusal::OperandClosure);
    }
    if admission.operand_count as usize != operands.len() {
        return Err(OuterExecutionRefusal::OperandClosure);
    }
    for (index, operand) in operands.iter().enumerate() {
        let expected = scratch.expected_operands[index];
        if operand.operand_kind != HELIOS_HOB1_OPERAND_KIND_GENERATED_RESOURCE
            || expected.operand_kind != HELIOS_HNR2_OPERAND_KIND_HOST_RESOURCE_ID32
            || operand.use_index as usize >= allocations.len()
        {
            return Err(OuterExecutionRefusal::OperandClosure);
        }
        if operand.encoded_width != HELIOS_HOB1_OPERAND_WIDTH_4 {
            return Err(OuterExecutionRefusal::OperandWidth);
        }
        let Some(expected_absolute) = header.payload_offset.checked_add(expected.payload_offset)
        else {
            return Err(OuterExecutionRefusal::PatchBounds);
        };
        if operand.payload_offset != expected_absolute {
            return Err(OuterExecutionRefusal::OperandClosure);
        }
        let Some(resource_id) = allocations[operand.use_index as usize].resource_id() else {
            return Err(OuterExecutionRefusal::UseMissingOrForeign);
        };
        allocations[operand.use_index as usize].note_import_operand_substitution();
        NR2_IMPORT_SUBSTITUTIONS.fetch_add(1, Ordering::Relaxed);
        NR2_IMPORT_LAST_RESOURCE.store(resource_id, Ordering::Relaxed);
        scratch.patch_order[index] = resource_id;
    }

    let payload_start =
        usize::try_from(header.payload_offset).map_err(|_| OuterExecutionRefusal::PatchBounds)?;
    let payload_len =
        usize::try_from(header.payload_bytes).map_err(|_| OuterExecutionRefusal::PatchBounds)?;
    let payload_end = payload_start
        .checked_add(payload_len)
        .ok_or(OuterExecutionRefusal::PatchBounds)?;
    // End every immutable HOB1 borrow before converting the same KMD-private
    // buffer into the host payload. The original HOC1 bytes remain untouched.
    let _ = (uses, operands, payload, record);
    record_buffer
        .as_mut_slice()
        .copy_within(payload_start..payload_end, 0);
    if !record_buffer.reset(payload_len) {
        return Err(OuterExecutionRefusal::PatchBounds);
    }
    for index in 0..admission.operand_count as usize {
        let start = scratch.expected_operands[index].payload_offset as usize;
        let end = start
            .checked_add(core::mem::size_of::<u32>())
            .ok_or(OuterExecutionRefusal::PatchBounds)?;
        let Some(dst) = record_buffer.as_mut_slice().get_mut(start..end) else {
            return Err(OuterExecutionRefusal::PatchBounds);
        };
        dst.copy_from_slice(&scratch.patch_order[index].to_le_bytes());
    }
    drop(claim);

    let custody = NativeCustody::Outer(OuterCustody::Virtual {
        _context: context_operation,
        _session: session_operation,
        _command_pool: command_pool,
        _allocations: allocations,
    });
    let host_context_id = custody.host_context_id();
    let transport_instance = custody.transport_instance();
    let completion = NativeHostCompletion {
        context: NonNull::from(native),
        identity,
        slot_index,
        custody: Some(custody),
    };
    // A nonzero INFO_RING_IDX is a Vulkan queue object id at the renderer.
    // The terminal A7 form contains no queue operation and Mesa joined every
    // exact allocation progress point before emitting it, so fence its actual
    // decoder execution on K11's existing session-local ring-zero timeline.
    // The used-ring response remains the sole normal K9 terminal.
    let submit_domain = if admission.terminal_teardown {
        crate::virtio::gpu::NativeSubmitDomain::Decoder
    } else {
        crate::virtio::gpu::NativeSubmitDomain::Queue(native.ring_index)
    };
    let mut pending_buffers = Some((meta, record_buffer, completion));
    let queued = adapter.with_virtio(|gpu| {
        if gpu.scanout_transport_instance() != transport_instance {
            return None;
        }
        pending_buffers.take().map(|(meta, payload, completion)| {
            gpu.enqueue_native_submit(
                host_context_id,
                submit_domain,
                meta,
                payload,
                payload_len,
                completion,
            )
        })
    });
    match queued {
        Ok(Some(Ok(_))) => {
            NR2_OUTER_HOST.fetch_add(1, Ordering::Relaxed);
        }
        Ok(Some(Err((meta, payload, Some(completion), _)))) => {
            OuterExecutionRefusal::HostEnqueue.record();
            completion.finish_with_cleanup(adapter, false, Some((meta, payload)));
        }
        Ok(Some(Err((meta, payload, None, _)))) => {
            OuterExecutionRefusal::HostEnqueue.record();
            drop(meta);
            drop(payload);
            fail_inflight_without_completion(native, adapter, identity, slot_index);
        }
        Ok(None) | Err(_) => {
            OuterExecutionRefusal::HostUnavailable.record();
            if let Some((meta, payload, completion)) = pending_buffers.take() {
                completion.finish_with_cleanup(adapter, false, Some((meta, payload)));
            } else {
                fail_inflight_without_completion(native, adapter, identity, slot_index);
            }
        }
    }
    Ok(())
}

/// System work-item callback for exactly one HQA1 context. No global queue,
/// lookup, or polling exists: the callback receives the owning context pointer
/// and drains only that context's fixed admission queue in FIFO order.
unsafe extern "C" fn outer_work_item(_device: PDEVICE_OBJECT, context: wdk_sys::PVOID) {
    let Some(native) = (context as *const NativeContext).as_ref() else {
        return;
    };
    let Some(worker) = native.outer_worker.as_ref() else {
        return;
    };
    loop {
        let Some((open, pending)) = worker.take_next() else {
            break;
        };
        let identity = pending.identity;
        let slot_index = pending.slot_index;
        if !open {
            fail_outer_slot(
                native,
                identity,
                slot_index,
                OuterExecutionRefusal::ContextClosed,
            );
            drop(pending);
            continue;
        }
        if unsafe { wdk_sys::ntddk::KeGetCurrentIrql() } != 0 {
            fail_outer_slot(
                native,
                identity,
                slot_index,
                OuterExecutionRefusal::WorkerIrql,
            );
            drop(pending);
            continue;
        }
        let passive = unsafe { PassiveLevel::assume() };
        if let Err(refusal) = execute_outer_pending(native, pending, passive) {
            fail_outer_slot(native, identity, slot_index, refusal);
        }
    }
}

/// D3D11 physical HOB1 Render. The UMD has already assembled one complete
/// record and the exact runtime allocation list; this function snapshots both,
/// resolves each list entry through the owning device, and parks a privately
/// patched Venus payload for the matching SubmitCommand callback.
pub(crate) unsafe fn render_outer_physical(
    native: &NativeContext,
    session: NonNull<crate::ddi::translation_session::SessionObject>,
    device: &crate::device::DeviceContext,
    outer: &SpinLock<OuterSubmitContext>,
    args: &mut DXGKARG_RENDER,
) -> NTSTATUS {
    let fail = |refusal: OuterExecutionRefusal, status: NTSTATUS| {
        refusal.record();
        status
    };
    if native.class != NativeClass::Outer
        || args.CommandLength == 0
        || args.pCommand.is_null()
        || args.pDmaBuffer.is_null()
        || args.CommandLength > args.DmaSize
        || args.PatchLocationListInSize != 0
        || args.AllocationListSize == 0
        || args.pAllocationList.is_null()
        || args.pDmaBufferPrivateData.is_null()
        || (args.DmaBufferPrivateDataSize as usize) < size_of::<Hob1KmdDmaPrivateV1>()
    {
        return fail(OuterExecutionRefusal::PrivateData, STATUS_INVALID_PARAMETER);
    }
    let Some(context_operation) = native.acquire_operation() else {
        return fail(
            OuterExecutionRefusal::ContextClosed,
            STATUS_INVALID_DEVICE_REQUEST,
        );
    };
    let Some(session_operation) =
        crate::ddi::translation_session::acquire_execution_operation(session)
    else {
        return fail(
            OuterExecutionRefusal::SessionClosed,
            STATUS_INVALID_DEVICE_REQUEST,
        );
    };
    if session_operation.transport_instance == 0
        || crate::ddi::translation_session::execution_session_generation(session)
            != Some(native.session_generation)
    {
        return fail(
            OuterExecutionRefusal::TransportMismatch,
            STATUS_INVALID_DEVICE_REQUEST,
        );
    }
    let Some(mut claim) = native.claim() else {
        return fail(
            OuterExecutionRefusal::ContextClosed,
            STATUS_DEVICE_NOT_READY,
        );
    };
    let passive = unsafe { PassiveLevel::assume() };
    let adapter = unsafe { native.adapter.as_ref() };
    crate::virtio::ctrl::reap_parked(passive, adapter);
    reap_terminal_slots(native, passive);

    let record_len = args.CommandLength as usize;
    let mut record_buffer = match adapter
        .with_virtio(|gpu| gpu.take_dma_buffer(record_len))
        .ok()
        .flatten()
        .or_else(|| DmaBuffer::new(passive, record_len))
    {
        Some(buffer) => buffer,
        None => return fail(OuterExecutionRefusal::SnapshotFailed, STATUS_NO_MEMORY),
    };
    if !unsafe {
        copy_from_command(
            record_buffer.as_mut_slice().as_mut_ptr(),
            args.pCommand.cast::<u8>(),
            record_len,
        )
    } {
        return fail(
            OuterExecutionRefusal::SnapshotFailed,
            STATUS_INVALID_PARAMETER,
        );
    }
    let meta = match adapter
        .with_virtio(|gpu| gpu.take_dma_buffer(SUBMIT_META_BYTES))
        .ok()
        .flatten()
        .or_else(|| DmaBuffer::new(passive, SUBMIT_META_BYTES))
    {
        Some(buffer) => buffer,
        None => return fail(OuterExecutionRefusal::SnapshotFailed, STATUS_NO_MEMORY),
    };

    let (expectation, expected_last) = {
        let context = outer.lock();
        if context.arm_flags != HELIOS_HOB1_FLAG_D3D11_PHYSICAL {
            return fail(
                OuterExecutionRefusal::Descriptor,
                STATUS_INVALID_DEVICE_REQUEST,
            );
        }
        (
            HeliosOuterBatchExpectation {
                package_generation: context.package_generation,
                session_generation: context.session_generation,
                context_generation: context.context_generation,
                endpoint_id: context.endpoint_id,
                flags: context.arm_flags,
                max_command_bytes: args.CommandLength as u64,
                last_batch_id: context.last_batch_id(),
                allocation_list_count: args.AllocationListSize,
            },
            context.last_batch_id(),
        )
    };
    let record = record_buffer.as_slice();
    if validate_batch_record(record, &expectation).is_err() {
        return fail(OuterExecutionRefusal::BatchRecord, STATUS_INVALID_PARAMETER);
    }
    let header = match hob1_header(record) {
        Ok(header) => *header,
        Err(_) => return fail(OuterExecutionRefusal::BatchRecord, STATUS_INVALID_PARAMETER),
    };
    let uses = match hob1_use_records(record, &header) {
        Ok(uses) => uses,
        Err(_) => return fail(OuterExecutionRefusal::BatchRecord, STATUS_INVALID_PARAMETER),
    };
    let operands = match hob1_operand_records(record, &header) {
        Ok(operands) => operands,
        Err(_) => return fail(OuterExecutionRefusal::BatchRecord, STATUS_INVALID_PARAMETER),
    };
    let payload = match hob1_payload(record, &header) {
        Ok(payload) => payload,
        Err(_) => return fail(OuterExecutionRefusal::BatchRecord, STATUS_INVALID_PARAMETER),
    };
    if uses.len() != args.AllocationListSize as usize {
        return fail(
            OuterExecutionRefusal::OperandClosure,
            STATUS_INVALID_PARAMETER,
        );
    }

    let scratch = claim.scratch();
    if uses.len() > scratch.outer_generations.len() || operands.len() > scratch.patch_order.len() {
        return fail(
            OuterExecutionRefusal::OperandClosure,
            STATUS_INVALID_PARAMETER,
        );
    }
    let mut allocations = Vec::new();
    if allocations.try_reserve_exact(uses.len()).is_err() {
        return fail(OuterExecutionRefusal::SlotExhausted, STATUS_NO_MEMORY);
    }
    for (index, record_use) in uses.iter().enumerate() {
        if record_use.identity_kind != HELIOS_HOB1_IDENTITY_D3D11_ALLOCATION_INDEX {
            return fail(
                OuterExecutionRefusal::OperandClosure,
                STATUS_INVALID_PARAMETER,
            );
        }
        let allocation_index = record_use.address_or_index as usize;
        let Some(entry) = (allocation_index < args.AllocationListSize as usize)
            .then(|| unsafe { &*args.pAllocationList.add(allocation_index) })
        else {
            return fail(
                OuterExecutionRefusal::UseMissingOrForeign,
                STATUS_INVALID_PARAMETER,
            );
        };
        let write = entry.__bindgen_anon_1.WriteOperation() != 0;
        if write != (record_use.access_flags & HELIOS_HOB1_ACCESS_WRITE != 0) {
            return fail(
                OuterExecutionRefusal::OperandClosure,
                STATUS_INVALID_PARAMETER,
            );
        }
        let Some(guard) = device.acquire_outer_physical_use(
            session,
            entry.hDeviceSpecificAllocation,
            record_use.expected_allocation_generation,
            record_use.byte_length,
        ) else {
            return fail(
                if !crate::adapter::allocation_object::is_current(
                    record_use.expected_allocation_generation,
                ) {
                    OuterExecutionRefusal::UseStaleGeneration
                } else {
                    OuterExecutionRefusal::UseMissingOrForeign
                },
                STATUS_INVALID_PARAMETER,
            );
        };
        if guard.generation() != record_use.expected_allocation_generation {
            return fail(
                OuterExecutionRefusal::UseStaleGeneration,
                STATUS_INVALID_PARAMETER,
            );
        }
        if guard.transport_instance() != Some(session_operation.transport_instance)
            || guard
                .resource_id()
                .is_none_or(|resource_id| resource_id == 0)
        {
            return fail(
                OuterExecutionRefusal::TransportMismatch,
                STATUS_INVALID_PARAMETER,
            );
        }
        scratch.outer_generations[index] = guard.generation();
        allocations.push(guard);
    }
    let generations = &mut scratch.outer_generations[..uses.len()];
    generations.sort_unstable();
    if generations.windows(2).any(|pair| pair[0] == pair[1]) {
        return fail(
            OuterExecutionRefusal::DuplicateUse,
            STATUS_INVALID_PARAMETER,
        );
    }

    let admission = match validate_venus_a7_outer_stream(
        payload,
        &mut scratch.expected_operands,
        &mut scratch.schema_counts,
    ) {
        Ok(admission) => admission,
        Err(_) => return fail(OuterExecutionRefusal::Schema, STATUS_INVALID_PARAMETER),
    };
    if admission.terminal_teardown && (uses.len() != 1 || !operands.is_empty()) {
        return fail(
            OuterExecutionRefusal::OperandClosure,
            STATUS_INVALID_PARAMETER,
        );
    }
    if admission.operand_count as usize != operands.len() {
        return fail(
            OuterExecutionRefusal::OperandClosure,
            STATUS_INVALID_PARAMETER,
        );
    }
    for (index, operand) in operands.iter().enumerate() {
        let expected = scratch.expected_operands[index];
        if operand.operand_kind != HELIOS_HOB1_OPERAND_KIND_GENERATED_RESOURCE
            || expected.operand_kind != HELIOS_HNR2_OPERAND_KIND_HOST_RESOURCE_ID32
            || operand.use_index as usize >= allocations.len()
        {
            return fail(
                OuterExecutionRefusal::OperandClosure,
                STATUS_INVALID_PARAMETER,
            );
        }
        if operand.encoded_width != HELIOS_HOB1_OPERAND_WIDTH_4 {
            return fail(
                OuterExecutionRefusal::OperandWidth,
                STATUS_INVALID_PARAMETER,
            );
        }
        let Some(expected_absolute) = header.payload_offset.checked_add(expected.payload_offset)
        else {
            return fail(OuterExecutionRefusal::PatchBounds, STATUS_INVALID_PARAMETER);
        };
        if operand.payload_offset != expected_absolute {
            return fail(
                OuterExecutionRefusal::OperandClosure,
                STATUS_INVALID_PARAMETER,
            );
        }
        let Some(resource_id) = allocations[operand.use_index as usize].resource_id() else {
            return fail(
                OuterExecutionRefusal::UseMissingOrForeign,
                STATUS_INVALID_PARAMETER,
            );
        };
        allocations[operand.use_index as usize].note_import_operand_substitution();
        NR2_IMPORT_SUBSTITUTIONS.fetch_add(1, Ordering::Relaxed);
        NR2_IMPORT_LAST_RESOURCE.store(resource_id, Ordering::Relaxed);
        scratch.patch_order[index] = resource_id;
    }

    // Preserve the complete zero-operand HOB1 in scheduler DMA. Only the
    // separately owned host buffer below receives renderer-private ids.
    unsafe {
        core::ptr::copy_nonoverlapping(
            record_buffer.as_slice().as_ptr(),
            args.pDmaBuffer.cast::<u8>(),
            record_len,
        )
    };
    let payload_start = header.payload_offset as usize;
    let payload_len = header.payload_bytes as usize;
    let Some(payload_end) = payload_start.checked_add(payload_len) else {
        return fail(OuterExecutionRefusal::PatchBounds, STATUS_INVALID_PARAMETER);
    };
    let _ = (record, uses, operands, payload);
    record_buffer
        .as_mut_slice()
        .copy_within(payload_start..payload_end, 0);
    if !record_buffer.reset(payload_len) {
        return fail(OuterExecutionRefusal::PatchBounds, STATUS_INVALID_PARAMETER);
    }
    for index in 0..admission.operand_count as usize {
        let start = scratch.expected_operands[index].payload_offset as usize;
        let Some(end) = start.checked_add(size_of::<u32>()) else {
            return fail(OuterExecutionRefusal::PatchBounds, STATUS_INVALID_PARAMETER);
        };
        let Some(dst) = record_buffer.as_mut_slice().get_mut(start..end) else {
            return fail(OuterExecutionRefusal::PatchBounds, STATUS_INVALID_PARAMETER);
        };
        dst.copy_from_slice(&scratch.patch_order[index].to_le_bytes());
    }
    drop(claim);

    let (slot_index, slot_generation) = {
        let mut executor = native.executor.lock();
        let Some(slot_index) = executor
            .slots
            .iter()
            .position(|slot| matches!(slot, SubmissionSlot::Free))
        else {
            return fail(OuterExecutionRefusal::SlotExhausted, STATUS_NO_MEMORY);
        };
        let Some(slot_generation) = executor.mint_slot_generation() else {
            return fail(
                OuterExecutionRefusal::SlotExhausted,
                STATUS_INVALID_DEVICE_REQUEST,
            );
        };
        (slot_index as u32, slot_generation)
    };
    let identity = BatchIdentity {
        batch_token: header.batch_id,
        slot_generation,
        payload_bytes: header.payload_bytes,
        full_payload_crc64: header.crc64,
        fragment_count: 1,
        session_generation: native.session_generation,
        context_generation: native.context_generation,
        ring_index: native.ring_index,
    };
    let batch = ReadyBatch {
        payload: record_buffer,
        meta,
        custody: NativeCustody::Outer(OuterCustody::Physical {
            _context: context_operation,
            _session: session_operation,
            _allocations: allocations,
        }),
        terminal_teardown: admission.terminal_teardown,
    };
    {
        let mut executor = native.executor.lock();
        let Some(slot) = executor.slots.get_mut(slot_index as usize) else {
            return fail(
                OuterExecutionRefusal::SlotExhausted,
                STATUS_INVALID_DEVICE_REQUEST,
            );
        };
        if !matches!(slot, SubmissionSlot::Free) {
            return fail(
                OuterExecutionRefusal::SlotExhausted,
                STATUS_DEVICE_NOT_READY,
            );
        }
        *slot = SubmissionSlot::Ready {
            identity,
            tickets: BatchTickets::new(),
            batch: Some(batch),
        };
    }
    let committed = {
        let mut context = outer.lock();
        context.last_batch_id() == expected_last
            && context.package_generation == expectation.package_generation
            && context.session_generation == expectation.session_generation
            && context.context_generation == expectation.context_generation
            && context.endpoint_id == expectation.endpoint_id
            && context.arm_flags == expectation.flags
            && commit_physical_hob1(&mut context, header.batch_id)
    };
    if !committed {
        // The runtime cannot submit this slot before Render returns, so an
        // exact zero-ticket Ready record is still ours to retract.  Drop its
        // buffers and allocation custody at PASSIVE after releasing the lock.
        let abandoned = {
            let mut executor = native.executor.lock();
            executor
                .slots
                .get_mut(slot_index as usize)
                .and_then(|slot| match slot {
                    SubmissionSlot::Ready {
                        identity: live_identity,
                        tickets,
                        ..
                    } if *live_identity == identity && tickets.count == 0 => {
                        match core::mem::replace(slot, SubmissionSlot::Free) {
                            SubmissionSlot::Ready {
                                batch: Some(batch), ..
                            } => Some(batch),
                            _ => None,
                        }
                    }
                    _ => None,
                })
        };
        debug_assert!(abandoned.is_some());
        drop(abandoned);
        return fail(
            OuterExecutionRefusal::Descriptor,
            STATUS_INVALID_DEVICE_REQUEST,
        );
    }

    let private = Hob1KmdDmaPrivateV1 {
        magic: HELIOS_HOB1_KMD_DMA_MAGIC,
        abi_version: HELIOS_HOB1_KMD_DMA_ABI_VERSION,
        struct_bytes: HELIOS_HOB1_KMD_DMA_BYTES,
        batch_id: header.batch_id,
        session_generation: native.session_generation,
        context_generation: native.context_generation,
        slot_generation,
        hob1_crc64: header.crc64,
        payload_bytes: header.payload_bytes,
        ring_index: native.ring_index,
        slot_index,
        flags: 0,
    };
    unsafe {
        core::ptr::copy_nonoverlapping(
            bytemuck::bytes_of(&private).as_ptr(),
            args.pDmaBufferPrivateData.cast::<u8>(),
            size_of::<Hob1KmdDmaPrivateV1>(),
        )
    };
    args.pDmaBuffer = unsafe {
        args.pDmaBuffer
            .cast::<u8>()
            .add(record_len)
            .cast::<c_void>()
    };
    args.PatchLocationListOutSize = 0;
    args.MultipassOffset = 0;
    NR2_OUTER_QUEUED.fetch_add(1, Ordering::Relaxed);
    NR2_COUNTERS.flush();
    STATUS_SUCCESS
}

fn abandon_unsubmitted_building(scratch: &mut Hnr2Scratch, native: &NativeContext) -> bool {
    let Some(building) = scratch.building.as_ref() else {
        return true;
    };
    let mut executor = native.executor.lock();
    let Some(slot) = executor.slots.get_mut(building.slot_index as usize) else {
        return false;
    };
    if matches!(
        slot,
        SubmissionSlot::Collecting { identity, tickets }
            if *identity == building.identity && tickets.count == 0
    ) {
        *slot = SubmissionSlot::Free;
    } else {
        return false;
    }
    drop(executor);
    // Render is PASSIVE; the two DMA buffers may be freed here.
    drop(scratch.building.take());
    true
}

fn abandon_control_building(scratch: &mut Hnr2Scratch, native: &NativeContext) {
    let Some(building) = scratch.control_building.take() else {
        return;
    };
    if native
        .state
        .lock()
        .staging_mut()
        .retire(building.total_payload_bytes)
        .is_ok()
    {
        NR2_SLOT_RETIRED.fetch_add(1, Ordering::Relaxed);
    } else {
        NR2_SLOT_UNDERFLOW.fetch_add(1, Ordering::Relaxed);
    }
    drop(building);
}

/// Finish the exact staging checkout for a control operation that has already
/// reached its synchronous host terminal in Render. The host-completed DMA
/// flag prevents SubmitCommand from retiring the same checkout a second time.
fn finish_synchronous_control_staging(native: &NativeContext, bytes: u64) {
    let finished = native.state.lock().staging_mut().finish_synchronous(bytes);
    match finished {
        Ok(()) => {
            NR2_SLOT_RETIRED.fetch_add(1, Ordering::Relaxed);
        }
        Err(_) => {
            NR2_SLOT_UNDERFLOW.fetch_add(1, Ordering::Relaxed);
        }
    }
}

fn begin_control_batch(
    scratch: &mut Hnr2Scratch,
    native: &NativeContext,
    header: &HeliosNativeRenderV2,
    passive: PassiveLevel,
) -> Result<(), NTSTATUS> {
    if let Some(building) = scratch.control_building.as_ref() {
        if building.batch_token == header.batch_token
            && building.total_payload_bytes == header.total_payload_bytes
            && building.full_payload_crc64 == header.full_payload_crc64
            && building.fragment_count == header.fragment_count
        {
            return Ok(());
        }
        abandon_control_building(scratch, native);
    }
    {
        let mut state = native.state.lock();
        state
            .staging_mut()
            .checkout(header.total_payload_bytes)
            .map_err(|refusal| refuse(refusal, STATUS_NO_MEMORY))?;
    }
    NR2_SLOT_TAKEN.fetch_add(1, Ordering::Relaxed);
    let payload_len =
        usize::try_from(header.total_payload_bytes).map_err(|_| STATUS_INVALID_PARAMETER)?;
    let adapter = unsafe { native.adapter.as_ref() };
    let payload = adapter
        .with_virtio(|gpu| gpu.take_dma_buffer(payload_len))
        .ok()
        .flatten()
        .or_else(|| DmaBuffer::new(passive, payload_len));
    let Some(payload) = payload else {
        let _ = native
            .state
            .lock()
            .staging_mut()
            .retire(header.total_payload_bytes);
        NR2_SLOT_RETIRED.fetch_add(1, Ordering::Relaxed);
        return Err(STATUS_NO_MEMORY);
    };
    scratch.control_building = Some(ControlBuilding {
        batch_token: header.batch_token,
        total_payload_bytes: header.total_payload_bytes,
        full_payload_crc64: header.full_payload_crc64,
        fragment_count: header.fragment_count,
        payload,
    });
    Ok(())
}

fn copy_control_fragment(
    scratch: &mut Hnr2Scratch,
    args: &DXGKARG_RENDER,
    header: &HeliosNativeRenderV2,
    accept: &Hnr2Accept,
) -> Result<(), NTSTATUS> {
    let Some(building) = scratch.control_building.as_mut() else {
        return Err(STATUS_INVALID_DEVICE_REQUEST);
    };
    if building.batch_token != header.batch_token
        || building.total_payload_bytes != header.total_payload_bytes
        || building.full_payload_crc64 != header.full_payload_crc64
        || building.fragment_count != header.fragment_count
    {
        return Err(STATUS_INVALID_DEVICE_REQUEST);
    }
    let start =
        usize::try_from(header.fragment_payload_offset).map_err(|_| STATUS_INVALID_PARAMETER)?;
    let end = start
        .checked_add(header.fragment_payload_bytes as usize)
        .ok_or(STATUS_INVALID_PARAMETER)?;
    let dst = building
        .payload
        .as_mut_slice()
        .get_mut(start..end)
        .ok_or(STATUS_INVALID_PARAMETER)?;
    let src = unsafe { (args.pCommand as *const u8).add(accept.layout.payload_offset as usize) };
    if !unsafe { copy_from_command(dst.as_mut_ptr(), src, dst.len()) } {
        return Err(STATUS_INVALID_PARAMETER);
    }
    Ok(())
}

fn begin_executor_batch(
    scratch: &mut Hnr2Scratch,
    native: &NativeContext,
    session: NonNull<crate::ddi::translation_session::SessionObject>,
    header: &HeliosNativeRenderV2,
    passive: PassiveLevel,
) -> Result<(), NTSTATUS> {
    if let Some(building) = scratch.building.as_ref() {
        if building.identity.batch_token == header.batch_token
            && building.identity.payload_bytes as u64 == header.total_payload_bytes
            && building.identity.fragment_count == header.fragment_count
            && building.identity.full_payload_crc64 == header.full_payload_crc64
        {
            return Ok(());
        }
        // A prior BEGIN whose Render never succeeded has no scheduler packet
        // and may be replaced. A published collecting slot with any ticket is
        // never silently recycled.
        if !abandon_unsubmitted_building(scratch, native) {
            return Err(STATUS_DEVICE_NOT_READY);
        }
    }

    reap_terminal_slots(native, passive);
    crate::virtio::ctrl::reap_parked(passive, unsafe { native.adapter.as_ref() });

    let context = native
        .acquire_operation()
        .ok_or(STATUS_INVALID_DEVICE_REQUEST)?;
    {
        let mut state = native.state.lock();
        if let Err(refusal) = state.staging_mut().checkout(header.total_payload_bytes) {
            drop(state);
            return Err(refuse(refusal, STATUS_NO_MEMORY));
        }
    }
    NR2_SLOT_TAKEN.fetch_add(1, Ordering::Relaxed);
    let staging = StagingCustody {
        context,
        bytes: header.total_payload_bytes,
    };
    let session_operation = crate::ddi::translation_session::acquire_execution_operation(session)
        .ok_or(STATUS_DEVICE_NOT_READY)?;
    if session_operation.transport_instance == 0 {
        return Err(STATUS_DEVICE_NOT_READY);
    }
    let payload_len =
        usize::try_from(header.total_payload_bytes).map_err(|_| STATUS_INVALID_PARAMETER)?;
    let adapter = unsafe { native.adapter.as_ref() };
    let payload = adapter
        .with_virtio(|v| v.take_dma_buffer(payload_len))
        .ok()
        .flatten()
        .or_else(|| DmaBuffer::new(passive, payload_len))
        .ok_or(STATUS_NO_MEMORY)?;
    let meta = adapter
        .with_virtio(|v| v.take_dma_buffer(SUBMIT_META_BYTES))
        .ok()
        .flatten()
        .or_else(|| DmaBuffer::new(passive, SUBMIT_META_BYTES))
        .ok_or(STATUS_NO_MEMORY)?;

    let (slot_index, slot_generation) = {
        let mut executor = native.executor.lock();
        let Some(slot_index) = executor
            .slots
            .iter()
            .position(|slot| matches!(slot, SubmissionSlot::Free))
        else {
            return Err(STATUS_NO_MEMORY);
        };
        let Some(slot_generation) = executor.mint_slot_generation() else {
            return Err(STATUS_INVALID_DEVICE_REQUEST);
        };
        (slot_index as u32, slot_generation)
    };
    let identity = BatchIdentity {
        batch_token: header.batch_token,
        slot_generation,
        payload_bytes: header.total_payload_bytes as u32,
        full_payload_crc64: header.full_payload_crc64,
        fragment_count: header.fragment_count,
        session_generation: native.session_generation,
        context_generation: native.context_generation,
        ring_index: native.ring_index,
    };
    {
        let mut executor = native.executor.lock();
        let Some(slot) = executor.slots.get_mut(slot_index as usize) else {
            return Err(STATUS_INVALID_DEVICE_REQUEST);
        };
        if !matches!(slot, SubmissionSlot::Free) {
            return Err(STATUS_INVALID_DEVICE_REQUEST);
        }
        *slot = SubmissionSlot::Collecting {
            identity,
            tickets: BatchTickets::new(),
        };
    }
    scratch.building = Some(BuildingBatch {
        identity,
        slot_index,
        payload,
        meta,
        staging,
        session: session_operation,
    });
    Ok(())
}

fn copy_executor_fragment(
    scratch: &mut Hnr2Scratch,
    args: &DXGKARG_RENDER,
    header: &HeliosNativeRenderV2,
    accept: &Hnr2Accept,
) -> Result<(u64, u32), NTSTATUS> {
    let Some(building) = scratch.building.as_mut() else {
        return Err(STATUS_INVALID_DEVICE_REQUEST);
    };
    if building.identity.batch_token != header.batch_token
        || building.identity.payload_bytes as u64 != header.total_payload_bytes
        || building.identity.fragment_count != header.fragment_count
        || building.identity.full_payload_crc64 != header.full_payload_crc64
    {
        return Err(STATUS_INVALID_DEVICE_REQUEST);
    }
    let dst_offset =
        usize::try_from(header.fragment_payload_offset).map_err(|_| STATUS_INVALID_PARAMETER)?;
    let bytes = header.fragment_payload_bytes as usize;
    let end = dst_offset
        .checked_add(bytes)
        .ok_or(STATUS_INVALID_PARAMETER)?;
    let dst = building
        .payload
        .as_mut_slice()
        .get_mut(dst_offset..end)
        .ok_or(STATUS_INVALID_PARAMETER)?;
    let source = unsafe { (args.pCommand as *const u8).add(accept.layout.payload_offset as usize) };
    if !unsafe { copy_from_command(dst.as_mut_ptr(), source, bytes) } {
        return Err(STATUS_INVALID_PARAMETER);
    }
    Ok((building.identity.slot_generation, building.slot_index))
}

struct PreparedExecutorCommit {
    identity: BatchIdentity,
    slot_index: u32,
    allocations: Vec<crate::ddi::create_allocation::OpenExecutionUse>,
    reply: Option<ExecutionReply>,
}

fn prepare_executor_commit(
    scratch: &mut Hnr2Scratch,
    native: &NativeContext,
    session: NonNull<crate::ddi::translation_session::SessionObject>,
    args: &DXGKARG_RENDER,
    header: &HeliosNativeRenderV2,
    accept: &Hnr2Accept,
    use_count: usize,
    patch_count: usize,
) -> Result<PreparedExecutorCommit, NTSTATUS> {
    let Hnr2Scratch {
        uses,
        patches,
        expected_operands,
        patch_order,
        use_ordinals,
        building,
        ..
    } = scratch;
    let uses = &uses[..use_count];
    let patches = &patches[..patch_count];
    let Some(building_ref) = building.as_ref() else {
        NR2_NO_STAGE.fetch_add(1, Ordering::Relaxed);
        return Err(STATUS_INVALID_DEVICE_REQUEST);
    };
    if building_ref.identity.batch_token != header.batch_token
        || building_ref.identity.slot_generation == 0
        || building_ref.identity.session_generation != native.session_generation
        || building_ref.identity.context_generation != native.context_generation
        || building_ref.identity.ring_index == 0
        || building_ref.identity.ring_index != native.ring_index
        || building_ref.session.transport_instance == 0
    {
        NR2_NO_STAGE.fetch_add(1, Ordering::Relaxed);
        return Err(STATUS_INVALID_DEVICE_REQUEST);
    }

    let actual_crc = helios_protocol::wddm::crc64_ecma(building_ref.payload.as_slice());
    if actual_crc != header.full_payload_crc64
        || building_ref.identity.full_payload_crc64 != header.full_payload_crc64
    {
        NR2_NO_STAGE.fetch_add(1, Ordering::Relaxed);
        return Err(STATUS_INVALID_PARAMETER);
    }
    let admission = match helios_kmd_logic::venus_executor::validate_venus_stream(
        building_ref.payload.as_slice(),
        expected_operands,
    ) {
        Ok(admission) => admission,
        Err(_) => {
            NR2_NO_SCHEMA.fetch_add(1, Ordering::Relaxed);
            return Err(STATUS_INVALID_PARAMETER);
        }
    };
    if admission.operand_count as usize != patches.len() {
        NR2_NO_SCHEMA.fetch_add(1, Ordering::Relaxed);
        return Err(STATUS_INVALID_PARAMETER);
    }

    let order = &mut patch_order[..patches.len()];
    for (i, ordinal) in order.iter_mut().enumerate() {
        *ordinal = i as u32;
    }
    order.sort_unstable_by_key(|ordinal| patches[*ordinal as usize].payload_offset);
    for (operand_index, patch_index) in order.iter().copied().enumerate() {
        let expected = scratch.expected_operands[operand_index];
        let patch = patches[patch_index as usize];
        if patch.payload_offset != expected.payload_offset
            || patch.operand_kind != expected.operand_kind
            || helios_protocol::native_render::hnr2_operand_width(patch.operand_kind)
                != Some(patch.encoded_width)
            || patch.operand_kind != HELIOS_HNR2_OPERAND_KIND_HOST_RESOURCE_ID32
        {
            NR2_NO_RESID.fetch_add(1, Ordering::Relaxed);
            return Err(STATUS_INVALID_PARAMETER);
        }
    }

    let list_count = args.AllocationListSize as usize;
    use_ordinals[..list_count].fill(u32::MAX);
    for (ordinal, use_record) in uses.iter().enumerate() {
        let index = use_record.allocation_list_index as usize;
        let Some(slot) = use_ordinals.get_mut(index) else {
            NR2_ALLOC_STALE.fetch_add(1, Ordering::Relaxed);
            return Err(STATUS_INVALID_PARAMETER);
        };
        if *slot != u32::MAX {
            NR2_ALLOC_STALE.fetch_add(1, Ordering::Relaxed);
            return Err(STATUS_INVALID_PARAMETER);
        }
        *slot = ordinal as u32;
    }

    let mut allocations = Vec::new();
    allocations
        .try_reserve_exact(uses.len())
        .map_err(|_| STATUS_NO_MEMORY)?;
    let passive = unsafe { PassiveLevel::assume() };
    for use_record in uses {
        let index = use_record.allocation_list_index as usize;
        if index >= list_count || args.pAllocationList.is_null() {
            NR2_ALLOC_STALE.fetch_add(1, Ordering::Relaxed);
            return Err(STATUS_INVALID_PARAMETER);
        }
        let handle = unsafe { (*args.pAllocationList.add(index)).hDeviceSpecificAllocation };
        let Some(guard) = (unsafe {
            crate::ddi::create_allocation::open_allocation_execution_use(
                handle,
                session,
                use_record.expected_allocation_generation,
                passive,
            )
        }) else {
            NR2_ALLOC_STALE.fetch_add(1, Ordering::Relaxed);
            return Err(STATUS_INVALID_PARAMETER);
        };
        if guard.transport_instance != building_ref.session.transport_instance
            || guard.allocation_generation != use_record.expected_allocation_generation
            || guard.resource_id == 0
            || guard.byte_size == 0
        {
            NR2_ALLOC_STALE.fetch_add(1, Ordering::Relaxed);
            return Err(STATUS_INVALID_PARAMETER);
        }
        allocations.push(guard);
    }

    let guard_for_patch = |patch: HeliosNativeRenderPatch| -> Option<&crate::ddi::create_allocation::OpenExecutionUse> {
        let use_ordinal = *use_ordinals.get(patch.allocation_list_index as usize)?;
        allocations.get(use_ordinal as usize)
    };

    let mut reply = None;
    let mut private_reply_patch = None;
    match admission.class {
        helios_kmd_logic::venus_executor::VenusCommandClass::AllocateMemory => {
            if !accept.has_reply
                || uses.len() != 2
                || patches.len() != 2
                || admission.reply_size != ExecutionReply::RAW_ALLOCATE_REPLY_BYTES
                || admission.reply_offset
                    != header
                        .reply_offset
                        .checked_add(HELIOS_HVR1_HEADER_SIZE as u64)
                        .ok_or(STATUS_INVALID_PARAMETER)?
            {
                NR2_NO_REPLY.fetch_add(1, Ordering::Relaxed);
                return Err(STATUS_INVALID_PARAMETER);
            }
            let set_reply_patch = patches[order[0] as usize];
            let import_patch = patches[order[1] as usize];
            let Some(reply_guard) = guard_for_patch(set_reply_patch) else {
                NR2_ALLOC_STALE.fetch_add(1, Ordering::Relaxed);
                return Err(STATUS_INVALID_PARAMETER);
            };
            let Some(import_guard) = guard_for_patch(import_patch) else {
                NR2_ALLOC_STALE.fetch_add(1, Ordering::Relaxed);
                return Err(STATUS_INVALID_PARAMETER);
            };
            // Roles 1-3 are OS-owned K2a pages: no host VkDeviceMemory existed
            // at allocation creation, so their canonical object truthfully has
            // no Vulkan memory-type identity to compare.  The submitted
            // memoryTypeIndex is consumed exactly by host vkAllocateMemory and
            // a host rejection remains the terminal result.  Role 4 and C57
            // already own a KMD-created host allocation, so re-importing those
            // resources must name its exact memory type.
            let exact_host_memory_type = matches!(
                import_guard.hvm1_role,
                0 | HELIOS_HVM1_ROLE_VULKAN_DEVICE_LOCAL
            );
            if set_reply_patch.allocation_list_index != header.reply_allocation_list_index
                || reply_guard.hvm1_role != HELIOS_HVM1_ROLE_REPLY_POOL
                || reply_guard.kernel_va.is_none()
                || import_guard.hvm1_role == HELIOS_HVM1_ROLE_REPLY_POOL
                || import_guard.resource_id == reply_guard.resource_id
                || import_guard.byte_size != admission.allocation_size
                || (exact_host_memory_type
                    && import_guard.memory_type_index != admission.memory_type_index)
            {
                NR2_ALLOC_STALE.fetch_add(1, Ordering::Relaxed);
                return Err(STATUS_INVALID_PARAMETER);
            }
            let reply_end = header
                .reply_offset
                .checked_add(HELIOS_HVR1_HEADER_SIZE as u64)
                .and_then(|offset| offset.checked_add(ExecutionReply::RAW_ALLOCATE_REPLY_BYTES))
                .ok_or(STATUS_INVALID_PARAMETER)?;
            if reply_end > reply_guard.byte_size
                || header.reply_capacity_bytes
                    < HELIOS_HVR1_HEADER_SIZE as u64 + ExecutionReply::RAW_ALLOCATE_REPLY_BYTES
            {
                NR2_NO_REPLY.fetch_add(1, Ordering::Relaxed);
                return Err(STATUS_INVALID_PARAMETER);
            }
            let snapshot_generation =
                crate::ddi::translation_session::mint_execution_snapshot_generation(session)
                    .ok_or(STATUS_INVALID_DEVICE_REQUEST)?;
            let reply_use_ordinal =
                use_ordinals[set_reply_patch.allocation_list_index as usize] as usize;
            let reply_use = uses[reply_use_ordinal];
            let request = helios_kmd_logic::translation_session::ExecutionReplyRequest {
                names_reply_pool: true,
                expected_allocation_generation: reply_use.expected_allocation_generation,
                access_flags: reply_use.access_flags,
                reply_offset: header.reply_offset,
                reply_capacity_bytes: header.reply_capacity_bytes,
                reply_slot_generation: header.reply_slot_generation,
                batch_token: header.batch_token,
                owner_context_generation: native.context_generation,
            };
            let slot = crate::ddi::translation_session::admit_execution_reply(session, &request)?;
            let slot_index = slot.slot_index.ok_or(STATUS_INVALID_DEVICE_REQUEST)?;
            let facts = crate::ddi::create_allocation::K11ReplyPoolFacts {
                allocation_generation: reply_guard.allocation_generation,
                resource_id: reply_guard.resource_id,
                transport_instance: reply_guard.transport_instance,
                kernel_va: reply_guard.kernel_va.ok_or(STATUS_INVALID_DEVICE_REQUEST)?,
                byte_size: reply_guard.byte_size,
            };
            unsafe {
                core::ptr::write_bytes(
                    facts.kernel_va.as_ptr().add(header.reply_offset as usize),
                    0,
                    (HELIOS_HVR1_HEADER_SIZE as u64 + ExecutionReply::RAW_ALLOCATE_REPLY_BYTES)
                        as usize,
                )
            };
            private_reply_patch = Some((
                facts,
                set_reply_patch.payload_offset,
                admission.reply_offset,
                admission.reply_size,
            ));
            reply = Some(ExecutionReply {
                session,
                facts,
                slot_index,
                slot_generation: slot.slot_generation,
                batch_token: slot.batch_token,
                snapshot_generation,
                session_generation: native.session_generation,
                reply_offset: slot.reply_offset,
                reply_capacity: slot.reply_capacity_bytes,
                finished: false,
            });
        }
        helios_kmd_logic::venus_executor::VenusCommandClass::FreeMemory => {
            if accept.has_reply
                || !patches.is_empty()
                || uses.len() != 1
                || allocations[0].hvm1_role == HELIOS_HVM1_ROLE_REPLY_POOL
            {
                NR2_NO_SCHEMA.fetch_add(1, Ordering::Relaxed);
                return Err(STATUS_INVALID_PARAMETER);
            }
        }
        helios_kmd_logic::venus_executor::VenusCommandClass::QueueSubmit
        | helios_kmd_logic::venus_executor::VenusCommandClass::QueueSubmit2
        | helios_kmd_logic::venus_executor::VenusCommandClass::QueueBindSparse => {
            if accept.has_reply
                || !patches.is_empty()
                || allocations
                    .iter()
                    .any(|guard| guard.hvm1_role == HELIOS_HVM1_ROLE_REPLY_POOL)
            {
                NR2_NO_SCHEMA.fetch_add(1, Ordering::Relaxed);
                return Err(STATUS_INVALID_PARAMETER);
            }
        }
    }

    // This is the only mutation of host-resource operands. `validate_venus_stream`
    // proved every destination is the exact generated zero placeholder in the
    // KMD-owned copy; user memory remains untouched.
    let Some(building) = building.as_mut() else {
        return Err(STATUS_INVALID_DEVICE_REQUEST);
    };
    if let Some((facts, operand_offset, raw_reply_offset, raw_reply_bytes)) = private_reply_patch {
        crate::ddi::translation_session::prepare_generated_reply(
            session,
            facts,
            building.payload.as_mut_slice(),
            operand_offset,
            raw_reply_offset,
            raw_reply_bytes,
        )?;
    }
    for patch in patches {
        if private_reply_patch
            .is_some_and(|(_, operand_offset, _, _)| operand_offset == patch.payload_offset)
        {
            continue;
        }
        let Some(guard) = guard_for_patch(*patch) else {
            return Err(STATUS_INVALID_PARAMETER);
        };
        let start = patch.payload_offset as usize;
        let end = start
            .checked_add(patch.encoded_width as usize)
            .ok_or(STATUS_INVALID_PARAMETER)?;
        let Some(dst) = building.payload.as_mut_slice().get_mut(start..end) else {
            return Err(STATUS_INVALID_PARAMETER);
        };
        dst.copy_from_slice(&guard.resource_id.to_le_bytes());
    }

    Ok(PreparedExecutorCommit {
        identity: building.identity,
        slot_index: building.slot_index,
        allocations,
        reply,
    })
}

fn finalize_executor_commit(
    scratch: &mut Hnr2Scratch,
    native: &NativeContext,
    prepared: PreparedExecutorCommit,
) -> Result<(), NTSTATUS> {
    let Some(building) = scratch.building.take() else {
        return Err(STATUS_INVALID_DEVICE_REQUEST);
    };
    if building.slot_index != prepared.slot_index
        || building.identity.batch_token != prepared.identity.batch_token
        || building.identity.slot_generation != prepared.identity.slot_generation
    {
        drop(building);
        return Err(STATUS_INVALID_DEVICE_REQUEST);
    }
    let batch = ReadyBatch {
        payload: building.payload,
        meta: building.meta,
        custody: NativeCustody::Hnr2 {
            _staging: building.staging,
            _session: building.session,
            _allocations: prepared.allocations,
            reply: prepared.reply,
        },
        terminal_teardown: false,
    };
    let mut executor = native.executor.lock();
    let Some(slot) = executor.slots.get_mut(prepared.slot_index as usize) else {
        drop(executor);
        drop(batch);
        return Err(STATUS_INVALID_DEVICE_REQUEST);
    };
    let old = core::mem::replace(slot, SubmissionSlot::Free);
    match old {
        SubmissionSlot::Collecting { identity, tickets }
            if identity.batch_token == prepared.identity.batch_token
                && identity.slot_generation == prepared.identity.slot_generation =>
        {
            *slot = SubmissionSlot::Ready {
                identity: prepared.identity,
                tickets,
                batch: Some(batch),
            };
            Ok(())
        }
        other => {
            *slot = other;
            drop(executor);
            drop(batch);
            Err(STATUS_INVALID_DEVICE_REQUEST)
        }
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
    match unsafe { helios_render_copy_user_seh(dst.cast(), src.cast(), bytes as u64, 1) } {
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
    session_generation: u64,
    context_generation: u64,
    slot_generation: u64,
    slot_index: u32,
    full_payload_crc64: u64,
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
        // The raw KMT device's exact session generation and the queue
        // context-local generation. Control INIT predates both and keeps zero.
        device_generation: session_generation,
        context_generation,
        slot_generation,
        full_payload_crc64,
        payload_bytes,
        capability_offset: plan.map_or(0, |p| p.offset),
        capability_count: plan.map_or(0, |p| p.count),
        ring_index,
        slot_index,
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

/// Read the distinct D3D11 outer private record from the exact scheduler
/// submission window. The one-record zero-window fallback mirrors HNR2 and is
/// bounded by the same exact 64-byte total-size proof.
unsafe fn read_hob1_dma_record(
    base: *const c_void,
    total: u32,
    start: u32,
    end: u32,
) -> Option<Hob1KmdDmaPrivateV1> {
    let bytes = size_of::<Hob1KmdDmaPrivateV1>();
    let (mut start, mut end, total) = (start as usize, end as usize, total as usize);
    if base.is_null() {
        return None;
    }
    if end <= start && total == bytes {
        NR2_WINDOW_FALLBACK.fetch_add(1, Ordering::Relaxed);
        start = 0;
        end = bytes;
    }
    if start > end || end > total || end - start < bytes {
        return None;
    }
    let mut raw = [0u8; size_of::<Hob1KmdDmaPrivateV1>()];
    unsafe {
        core::ptr::copy_nonoverlapping((base as *const u8).add(start), raw.as_mut_ptr(), bytes)
    };
    bytemuck::try_pod_read_unaligned::<Hob1KmdDmaPrivateV1>(&raw).ok()
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

    let control_init = if native.class == NativeClass::Control
        && header.fragment_count == 1
        && header.total_payload_bytes
            == size_of::<helios_protocol::translation_session::HeliosTranslationSessionInitV1>()
                as u64
    {
        let mut magic = [0u8; size_of::<u32>()];
        let src =
            unsafe { (args.pCommand as *const u8).add(accept.layout.payload_offset as usize) };
        (unsafe { copy_from_command(magic.as_mut_ptr(), src, magic.len()) })
            && u32::from_le_bytes(magic)
                == helios_protocol::translation_session::HELIOS_HTS1_INIT_MAGIC
    } else {
        false
    };

    let executor_slot = if native.class == NativeClass::Queue {
        let passive = unsafe { PassiveLevel::assume() };
        let scratch = claim.scratch();
        if accept.class.is_begin() {
            if let Err(status) = begin_executor_batch(scratch, native, session, &header, passive) {
                NR2_NO_STAGE.fetch_add(1, Ordering::Relaxed);
                return status;
            }
        }
        match copy_executor_fragment(scratch, args, &header, &accept) {
            Ok(slot) => Some(slot),
            Err(status) => {
                NR2_NO_STAGE.fetch_add(1, Ordering::Relaxed);
                return status;
            }
        }
    } else if native.class == NativeClass::Control && !control_init {
        let passive = unsafe { PassiveLevel::assume() };
        let scratch = claim.scratch();
        if accept.class.is_begin() {
            if let Err(status) = begin_control_batch(scratch, native, &header, passive) {
                NR2_NO_STAGE.fetch_add(1, Ordering::Relaxed);
                return status;
            }
        }
        if let Err(status) = copy_control_fragment(scratch, args, &header, &accept) {
            NR2_NO_STAGE.fetch_add(1, Ordering::Relaxed);
            return status;
        }
        Some((header.batch_token, 0))
    } else {
        None
    };

    if !accept.class.is_commit() {
        // An interior fragment carries no tables, no allocations and no reply.
        // SAFETY: `args` is dxgkrnl's live argument struct.
        let (slot_generation, slot_index) = executor_slot.unwrap_or((header.batch_token, 0));
        if !unsafe {
            publish_dma_record(
                args,
                &header,
                None,
                native.ring_index,
                0,
                native.session_generation,
                native.context_generation,
                slot_generation,
                slot_index,
                header.full_payload_crc64,
            )
        } {
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
        let mut capability = match build_capability(record, identity.generation, identity.byte_size)
        {
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
            let physical =
                unsafe { entry.__bindgen_anon_2.PhysicalAddress.as_ref().QuadPart } as u64;
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

    // INIT is the only pre-session control command.  Every later HVC1 payload
    // must have been copied through the bounded control assembler and will be
    // re-parsed by the generated schema below.
    let control_generated = native.class == NativeClass::Control
        && scratch.control_building.as_ref().is_some_and(|building| {
            building.batch_token == header.batch_token
                && building.total_payload_bytes == header.total_payload_bytes
                && building.full_payload_crc64 == header.full_payload_crc64
                && building.fragment_count == header.fragment_count
        });
    let k11_init = native.class == NativeClass::Control
        && !control_generated
        && accept.has_reply
        && header.fragment_count == 1
        && header.total_payload_bytes
            == size_of::<helios_protocol::translation_session::HeliosTranslationSessionInitV1>()
                as u64;
    if native.class == NativeClass::Control && !k11_init && !control_generated {
        NR2_NO_SCHEMA.fetch_add(1, Ordering::Relaxed);
        return STATUS_INVALID_PARAMETER;
    }

    // Queue bytes were charged at BEGIN and remain charged with their move-only
    // DMA custody until the actual host terminal.  The one-fragment control
    // INIT has no such building object, so it retains the original checkout at
    // COMMIT and retirement in SubmitCommand.
    if native.class == NativeClass::Control && k11_init {
        let mut state = native.state.lock();
        if let Err(refusal) = state.staging_mut().checkout(header.total_payload_bytes) {
            drop(state);
            return refuse(refusal, STATUS_NO_MEMORY);
        }
        NR2_SLOT_TAKEN.fetch_add(1, Ordering::Relaxed);
    }

    // Executor validation below may patch only its KMD-owned payload copy.
    // Prove the final DMA-private publication cannot refuse before that
    // mutation, so a BUFFER_TOO_SMALL replay always revalidates the original
    // CRC-zeroed wire bytes rather than a half-prepared host copy.
    if native.class == NativeClass::Queue
        && (args.pDmaBufferPrivateData.is_null()
            || (args.DmaBufferPrivateDataSize as usize) < size_of::<Hnr2KmdDmaPrivateV1>())
    {
        return STATUS_INVALID_PARAMETER;
    }

    let prepared_executor = if native.class == NativeClass::Queue {
        match prepare_executor_commit(
            scratch,
            native,
            session,
            args,
            header,
            accept,
            use_count,
            patch_count,
        ) {
            Ok(prepared) => Some(prepared),
            Err(status) => return status,
        }
    } else {
        None
    };

    let payload_bytes = header.total_payload_bytes.min(u32::MAX as u64) as u32;
    let (slot_generation, slot_index, full_payload_crc64) = prepared_executor
        .as_ref()
        .map(|prepared| {
            (
                prepared.identity.slot_generation,
                prepared.slot_index,
                prepared.identity.full_payload_crc64,
            )
        })
        .unwrap_or((header.batch_token, 0, header.full_payload_crc64));
    // SAFETY: `args` is dxgkrnl's live argument struct.
    if !unsafe {
        publish_dma_record(
            args,
            header,
            Some(&table),
            native.ring_index,
            payload_bytes,
            native.session_generation,
            native.context_generation,
            slot_generation,
            slot_index,
            full_payload_crc64,
        )
    } {
        if native.class == NativeClass::Control {
            if control_generated {
                let _ = scratch.control_building.take();
            }
            let _ = native
                .state
                .lock()
                .staging_mut()
                .retire(header.total_payload_bytes);
        }
        return STATUS_INVALID_PARAMETER;
    }

    // ── the control-context reply slot, and the finite host INIT ─────────────
    if native.class == NativeClass::Control {
        let mut init_payload = [0u8; size_of::<
            helios_protocol::translation_session::HeliosTranslationSessionInitV1,
        >()];
        let mut generated = if control_generated {
            scratch.control_building.take()
        } else {
            None
        };
        let payload = if let Some(building) = generated.as_mut() {
            if helios_protocol::wddm::crc64_ecma(building.payload.as_slice())
                != header.full_payload_crc64
            {
                let _ = native
                    .state
                    .lock()
                    .staging_mut()
                    .retire(header.total_payload_bytes);
                NR2_SLOT_RETIRED.fetch_add(1, Ordering::Relaxed);
                return STATUS_INVALID_PARAMETER;
            }
            building.payload.as_mut_slice()
        } else {
            let src =
                unsafe { (args.pCommand as *const u8).add(accept.layout.payload_offset as usize) };
            if !unsafe { copy_from_command(init_payload.as_mut_ptr(), src, init_payload.len()) } {
                let _ = native
                    .state
                    .lock()
                    .staging_mut()
                    .retire(header.total_payload_bytes);
                NR2_SLOT_RETIRED.fetch_add(1, Ordering::Relaxed);
                return STATUS_INVALID_PARAMETER;
            }
            &mut init_payload
        };
        let status = control_render(
            session,
            args,
            header,
            accept,
            &scratch.uses[..use_count],
            &scratch.patches[..patch_count],
            list_count,
            payload,
            &mut scratch.expected_operands,
            &mut scratch.schema_counts,
        );
        // Generated payload custody ends with the synchronous host operation,
        // not with the later scheduler fence callback.
        drop(generated);
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
        // SAFETY: `publish_dma_record` succeeded above and the private-data
        // pointer is not advanced until after this exact marker is written.
        unsafe { mark_dma_host_completed(args, header.batch_token) };
        finish_synchronous_control_staging(native, header.total_payload_bytes);
    } else if let Some(prepared) = prepared_executor {
        if let Err(status) = finalize_executor_commit(scratch, native, prepared) {
            return status;
        }
    }

    // Control INIT is already terminal here. Queue payloads are now immutable
    // in their context-local slot and remain owned until SubmitCommand attaches
    // the exact K9 ticket and the stock nonzero-ring response terminates.

    // SAFETY: `plan_capability_table` proved the whole table is inside `DmaSize`.
    args.pDmaBuffer = unsafe {
        (args.pDmaBuffer as *mut u8).add(CAPABILITY_TABLE_OFFSET as usize + table.bytes as usize)
            as *mut c_void
    };
    // SAFETY: `publish_dma_record` returned true, so the buffer held the record.
    unsafe { advance_private_data(args) };
    args.PatchLocationListOutSize = plan.count;
    if !args.pPatchLocationListOut.is_null() {
        args.pPatchLocationListOut = unsafe { args.pPatchLocationListOut.add(plan.count as usize) };
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
    /// A reply-less generated operation reached a real stock-Venus terminal.
    Completed,
    /// The payload was refused before any host reply existed.
    Refused,
    /// INIT failed after closing this session's admission; finish teardown only
    /// after the caller aborts its exact checked-out slot.
    InitFailed,
}

impl ControlPayloadOutcome {
    const fn status(self) -> NTSTATUS {
        match self {
            Self::Published | Self::Completed => STATUS_SUCCESS,
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
    patches: &[HeliosNativeRenderPatch],
    list_count: usize,
    payload: &mut [u8],
    expected_operands: &mut [helios_kmd_logic::venus_executor::VenusOperand],
    schema_counts: &mut [u32],
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
    // The finite operation and HVR1 publication are synchronous. Success
    // retires a genuinely-published reply; refusal cancels ownership without a
    // synthetic completion. Failed INIT has already closed admission, and only
    // after that exact cancellation may teardown destroy the host namespace.
    let outcome = run_control_payload(
        session,
        args,
        header,
        accept,
        &admission,
        uses,
        patches,
        payload,
        expected_operands,
        schema_counts,
    );
    match (outcome, admission.slot_index) {
        (ControlPayloadOutcome::Published, Some(slot_index)) => {
            hts1::release_control_slot(session, slot_index, admission.slot_generation)
        }
        (ControlPayloadOutcome::Completed, None) => {}
        (ControlPayloadOutcome::InitFailed, Some(slot_index)) => {
            hts1::abort_control_slot(session, slot_index, admission.slot_generation);
            hts1::finish_failed_session_init(session);
        }
        (_, Some(slot_index)) => {
            hts1::abort_control_slot(session, slot_index, admission.slot_generation)
        }
        _ => {}
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
    uses: &[HeliosNativeRenderUse],
    patches: &[HeliosNativeRenderPatch],
    payload: &mut [u8],
    expected_operands: &mut [helios_kmd_logic::venus_executor::VenusOperand],
    schema_counts: &mut [u32],
) -> ControlPayloadOutcome {
    use helios_protocol::translation_session::HeliosTranslationSessionInitV1;
    const INIT_BYTES: usize = size_of::<HeliosTranslationSessionInitV1>();

    if header.fragment_count == 1
        && header.total_payload_bytes == INIT_BYTES as u64
        && payload.len() == INIT_BYTES
        && u32::from_le_bytes(payload[..4].try_into().unwrap_or_default())
            == helios_protocol::translation_session::HELIOS_HTS1_INIT_MAGIC
    {
        if !accept.has_reply || !patches.is_empty() || uses.len() != 1 {
            return ControlPayloadOutcome::Refused;
        }
        return match crate::ddi::translation_session::session_init(
            session,
            payload,
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
        };
    }

    let generated = match helios_kmd_logic::venus_executor::validate_venus_control_stream(
        payload,
        accept.has_reply,
        expected_operands,
        schema_counts,
    ) {
        Ok(generated) => generated,
        Err(_) => {
            NR2_NO_SCHEMA.fetch_add(1, Ordering::Relaxed);
            return ControlPayloadOutcome::Refused;
        }
    };
    if generated.operand_count as usize != patches.len() {
        NR2_NO_SCHEMA.fetch_add(1, Ordering::Relaxed);
        return ControlPayloadOutcome::Refused;
    }

    if !accept.has_reply {
        if !uses.is_empty() || !patches.is_empty() || args.AllocationListSize != 0 {
            NR2_NO_SCHEMA.fetch_add(1, Ordering::Relaxed);
            return ControlPayloadOutcome::Refused;
        }
        return match crate::ddi::translation_session::execute_control_no_reply(session, payload) {
            Ok(()) => ControlPayloadOutcome::Completed,
            Err(_) => ControlPayloadOutcome::Refused,
        };
    }

    if uses.len() != 1 || patches.len() != 1 || admission.slot_index.is_none() {
        NR2_NO_REPLY.fetch_add(1, Ordering::Relaxed);
        return ControlPayloadOutcome::Refused;
    }
    let patch = patches[0];
    let expected = expected_operands[0];
    let reply_index = header.reply_allocation_list_index as usize;
    if patch.payload_offset != expected.payload_offset
        || patch.allocation_list_index as usize != reply_index
        || patch.operand_kind != HELIOS_HNR2_OPERAND_KIND_HOST_RESOURCE_ID32
        || patch.encoded_width != size_of::<u32>() as u16
        || generated.reply_offset
            != header
                .reply_offset
                .checked_add(HELIOS_HVR1_HEADER_SIZE as u64)
                .unwrap_or(u64::MAX)
        || generated.reply_size == 0
        || header.reply_capacity_bytes < HELIOS_HVR1_HEADER_SIZE as u64 + generated.reply_size
        || reply_index >= args.AllocationListSize as usize
        || args.pAllocationList.is_null()
    {
        NR2_NO_REPLY.fetch_add(1, Ordering::Relaxed);
        return ControlPayloadOutcome::Refused;
    }
    let use_record = uses[0];
    if use_record.allocation_list_index as usize != reply_index {
        return ControlPayloadOutcome::Refused;
    }
    let handle = unsafe { (*args.pAllocationList.add(reply_index)).hDeviceSpecificAllocation };
    let passive = unsafe { PassiveLevel::assume() };
    let Some(guard) = (unsafe {
        crate::ddi::create_allocation::open_allocation_execution_use(
            handle,
            session,
            use_record.expected_allocation_generation,
            passive,
        )
    }) else {
        NR2_ALLOC_STALE.fetch_add(1, Ordering::Relaxed);
        return ControlPayloadOutcome::Refused;
    };
    let Some(kernel_va) = guard.kernel_va else {
        return ControlPayloadOutcome::Refused;
    };
    if guard.hvm1_role != HELIOS_HVM1_ROLE_REPLY_POOL
        || guard.allocation_generation != use_record.expected_allocation_generation
        || guard.resource_id == 0
        || guard.transport_instance == 0
    {
        return ControlPayloadOutcome::Refused;
    }
    let facts = crate::ddi::create_allocation::K11ReplyPoolFacts {
        allocation_generation: guard.allocation_generation,
        resource_id: guard.resource_id,
        transport_instance: guard.transport_instance,
        kernel_va,
        byte_size: guard.byte_size,
    };
    match crate::ddi::translation_session::execute_generated_control(
        session,
        facts,
        payload,
        patch.payload_offset,
        header.reply_offset,
        header.reply_capacity_bytes,
        generated.reply_size,
        admission.slot_generation,
        admission.batch_token,
        generated.opcode,
    ) {
        Ok(()) => ControlPayloadOutcome::Published,
        Err(_) => ControlPayloadOutcome::Refused,
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
        let physical = unsafe {
            allocation
                .__bindgen_anon_2
                .PhysicalAddress
                .as_ref()
                .QuadPart
        } as u64;
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
    /// A queue packet's exact K9 ticket is now owned by the context-local
    /// executor. It is either waiting for the stock host response or was
    /// terminalized explicitly; the compatibility FIFO must not see it.
    Pending,
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

enum QueueSubmitAction {
    Pending,
    Enqueue {
        identity: BatchIdentity,
        slot_index: u32,
        batch: ReadyBatch,
    },
    AlreadyTerminal {
        success: bool,
    },
}

fn queue_record_matches(
    native: &NativeContext,
    record: &Hnr2KmdDmaPrivateV1,
    identity: BatchIdentity,
    slot_index: usize,
) -> bool {
    record.batch_token == identity.batch_token
        && record.device_generation == identity.session_generation
        && record.context_generation == identity.context_generation
        && record.slot_generation == identity.slot_generation
        && record.slot_index as usize == slot_index
        && record.ring_index == identity.ring_index
        && record.full_payload_crc64 == identity.full_payload_crc64
        && (record.payload_bytes == 0 || record.payload_bytes == identity.payload_bytes)
        && identity.session_generation == native.session_generation
        && identity.context_generation == native.context_generation
        && identity.ring_index == native.ring_index
        && identity.ring_index != 0
}

fn append_queue_ticket(
    adapter: &crate::adapter::AdapterContext,
    tickets: &mut BatchTickets,
    identity: BatchIdentity,
    ticket: crate::adapter::OrderedEngineTicket,
    resubmission: bool,
    commit_record: bool,
) -> bool {
    tickets.append(
        ticket,
        resubmission,
        identity.fragment_count,
        commit_record,
        |old| adapter.with_wddm_notify_lock(|guard| guard.ordered_engine_ticket_is_live(old)),
    )
}

fn outer_physical_record_matches(
    native: &NativeContext,
    record: &Hob1KmdDmaPrivateV1,
    identity: BatchIdentity,
    slot_index: usize,
) -> bool {
    record.magic == HELIOS_HOB1_KMD_DMA_MAGIC
        && record.abi_version == HELIOS_HOB1_KMD_DMA_ABI_VERSION
        && record.struct_bytes == HELIOS_HOB1_KMD_DMA_BYTES
        && record.flags == 0
        && record.batch_id == identity.batch_token
        && record.session_generation == identity.session_generation
        && record.context_generation == identity.context_generation
        && record.slot_generation == identity.slot_generation
        && record.hob1_crc64 == identity.full_payload_crc64
        && record.payload_bytes == identity.payload_bytes
        && record.ring_index == identity.ring_index
        && record.slot_index as usize == slot_index
        && identity.session_generation == native.session_generation
        && identity.context_generation == native.context_generation
        && identity.ring_index == native.ring_index
        && identity.fragment_count == 1
        && identity.ring_index != 0
}

/// Consume the D3D11 physical record parked by `render_outer_physical` and
/// transfer its exact K9 ticket plus direct custody into the existing host
/// completion path. No legacy scheduler FIFO observes an outer-context packet.
pub(crate) unsafe fn submit_outer_physical(
    native: &NativeContext,
    session: NonNull<crate::ddi::translation_session::SessionObject>,
    submit: &DXGKARG_SUBMITCOMMAND,
    ticket: crate::adapter::OrderedEngineTicket,
) -> NativeSubmitDisposition {
    NR2_SUBMITS.fetch_add(1, Ordering::Relaxed);
    let Some(_submit_operation) = native.acquire_operation() else {
        OuterExecutionRefusal::ContextClosed.record();
        return NativeSubmitDisposition::Revoked;
    };
    if native.class != NativeClass::Outer
        || crate::ddi::translation_session::execution_session_generation(session)
            != Some(native.session_generation)
    {
        OuterExecutionRefusal::SessionClosed.record();
        return NativeSubmitDisposition::Revoked;
    }
    let resubmission = (unsafe { submit.Flags.__bindgen_anon_1.Value } & (1 << 7)) != 0;
    let Some(record) = (unsafe {
        read_hob1_dma_record(
            submit.pDmaBufferPrivateData,
            submit.DmaBufferPrivateDataSize,
            submit.DmaBufferPrivateDataSubmissionStartOffset,
            submit.DmaBufferPrivateDataSubmissionEndOffset,
        )
    }) else {
        OuterExecutionRefusal::PrivateData.record();
        return NativeSubmitDisposition::Revoked;
    };
    if let Err(refusal) = native
        .host_submissions
        .lock()
        .admit_host_completion(submit.SubmissionFenceId, resubmission)
    {
        let code = match refusal {
            HostSubmissionRefusal::Duplicate { .. } => 2,
            HostSubmissionRefusal::WentBackward { .. } => 3,
        };
        bump_with_code(&NR2_HOST_SUBMIT_REJECT, code);
        OuterExecutionRefusal::FenceOrder.record();
        return NativeSubmitDisposition::Revoked;
    }
    let adapter = unsafe { native.adapter.as_ref() };
    let action = {
        let mut executor = native.executor.lock();
        let Some(slot) = executor.slots.get_mut(record.slot_index as usize) else {
            OuterExecutionRefusal::ResubmissionMismatch.record();
            return NativeSubmitDisposition::Revoked;
        };
        let old = core::mem::replace(slot, SubmissionSlot::Free);
        match old {
            SubmissionSlot::Ready {
                identity,
                mut tickets,
                mut batch,
            } => {
                if outer_physical_record_matches(
                    native,
                    &record,
                    identity,
                    record.slot_index as usize,
                ) && append_queue_ticket(
                    adapter,
                    &mut tickets,
                    identity,
                    ticket,
                    resubmission,
                    true,
                ) {
                    if let Some(batch) = batch.take() {
                        *slot = SubmissionSlot::InFlight { identity, tickets };
                        Some(QueueSubmitAction::Enqueue {
                            identity,
                            slot_index: record.slot_index,
                            batch,
                        })
                    } else {
                        *slot = SubmissionSlot::Ready {
                            identity,
                            tickets,
                            batch,
                        };
                        None
                    }
                } else {
                    *slot = SubmissionSlot::Ready {
                        identity,
                        tickets,
                        batch,
                    };
                    None
                }
            }
            SubmissionSlot::InFlight {
                identity,
                mut tickets,
            } => {
                if outer_physical_record_matches(
                    native,
                    &record,
                    identity,
                    record.slot_index as usize,
                ) && append_queue_ticket(
                    adapter,
                    &mut tickets,
                    identity,
                    ticket,
                    resubmission,
                    true,
                ) {
                    *slot = SubmissionSlot::InFlight { identity, tickets };
                    Some(QueueSubmitAction::Pending)
                } else {
                    *slot = SubmissionSlot::InFlight { identity, tickets };
                    None
                }
            }
            SubmissionSlot::Terminal {
                identity,
                mut tickets,
                success,
                cleanup,
            } => {
                if outer_physical_record_matches(
                    native,
                    &record,
                    identity,
                    record.slot_index as usize,
                ) && append_queue_ticket(
                    adapter,
                    &mut tickets,
                    identity,
                    ticket,
                    resubmission,
                    true,
                ) {
                    *slot = SubmissionSlot::Terminal {
                        identity,
                        tickets,
                        success,
                        cleanup,
                    };
                    Some(QueueSubmitAction::AlreadyTerminal { success })
                } else {
                    *slot = SubmissionSlot::Terminal {
                        identity,
                        tickets,
                        success,
                        cleanup,
                    };
                    None
                }
            }
            other => {
                *slot = other;
                None
            }
        }
    };
    let Some(action) = action else {
        OuterExecutionRefusal::ResubmissionMismatch.record();
        return NativeSubmitDisposition::Revoked;
    };
    NR2_HOST_SUBMIT_OK.fetch_add(1, Ordering::Relaxed);
    match action {
        QueueSubmitAction::Pending => NativeSubmitDisposition::Pending,
        QueueSubmitAction::AlreadyTerminal { success } => {
            if success {
                let _ = crate::ddi::interrupt::complete_ordered_engine_submission(adapter, ticket);
            } else {
                let _ = crate::ddi::interrupt::fail_ordered_engine_submission(adapter, ticket);
            }
            NativeSubmitDisposition::Pending
        }
        QueueSubmitAction::Enqueue {
            identity,
            slot_index,
            batch,
        } => {
            let submit_domain = if batch.terminal_teardown {
                crate::virtio::gpu::NativeSubmitDomain::Decoder
            } else {
                crate::virtio::gpu::NativeSubmitDomain::Queue(native.ring_index)
            };
            let host_context_id = batch.custody.host_context_id();
            let transport_instance = batch.custody.transport_instance();
            let completion = NativeHostCompletion {
                context: NonNull::from(native),
                identity,
                slot_index,
                custody: Some(batch.custody),
            };
            let mut pending = Some((batch.meta, batch.payload, completion));
            let queued = adapter.with_virtio(|gpu| {
                if gpu.scanout_transport_instance() != transport_instance {
                    return None;
                }
                pending.take().map(|(meta, payload, completion)| {
                    gpu.enqueue_native_submit(
                        host_context_id,
                        submit_domain,
                        meta,
                        payload,
                        identity.payload_bytes as usize,
                        completion,
                    )
                })
            });
            match queued {
                Ok(Some(Ok(_))) => {
                    NR2_OUTER_HOST.fetch_add(1, Ordering::Relaxed);
                    NativeSubmitDisposition::Pending
                }
                Ok(Some(Err((meta, payload, Some(completion), _)))) => {
                    OuterExecutionRefusal::HostEnqueue.record();
                    completion.finish_with_cleanup(adapter, false, Some((meta, payload)));
                    NativeSubmitDisposition::Pending
                }
                Ok(Some(Err((meta, payload, None, _)))) => {
                    OuterExecutionRefusal::HostEnqueue.record();
                    core::mem::forget(meta);
                    core::mem::forget(payload);
                    fail_inflight_without_completion(native, adapter, identity, slot_index);
                    NativeSubmitDisposition::Pending
                }
                Ok(None) | Err(_) => {
                    OuterExecutionRefusal::HostUnavailable.record();
                    if let Some((meta, payload, completion)) = pending.take() {
                        completion.finish_with_cleanup(adapter, false, Some((meta, payload)));
                        NativeSubmitDisposition::Pending
                    } else {
                        fail_inflight_without_completion(native, adapter, identity, slot_index);
                        NativeSubmitDisposition::Revoked
                    }
                }
            }
        }
    }
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
    ticket: crate::adapter::OrderedEngineTicket,
) -> NativeSubmitDisposition {
    NR2_SUBMITS.fetch_add(1, Ordering::Relaxed);
    let Some(_submit_operation) = native.acquire_operation() else {
        return NativeSubmitDisposition::Revoked;
    };
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
        return if native.class == NativeClass::Queue {
            NativeSubmitDisposition::Revoked
        } else {
            NativeSubmitDisposition::Refused
        };
    };
    if resubmission {
        NR2_SUBMIT_RESUBMISSION.fetch_add(1, Ordering::Relaxed);
    } else if native.class == NativeClass::Control
        && record.payload_bytes != 0
        && record.flags != HELIOS_HNR2_KMD_DMA_FLAG_HOST_COMPLETED
    {
        // Successful finite control work already released its staging at the
        // synchronous Render terminal. This arm is only defensive cleanup for
        // an unmarked record; the host-completed bit is the existing exact
        // per-submission discriminator and avoids a second retirement.
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
    if native.class == NativeClass::Queue {
        if record.flags != 0
            || record.device_generation != native.session_generation
            || record.context_generation != native.context_generation
            || record.ring_index != native.ring_index
            || record.ring_index == 0
            || crate::ddi::translation_session::execution_session_generation(session)
                != Some(native.session_generation)
        {
            bump_with_code(&NR2_HOST_SUBMIT_REJECT, 1);
            return NativeSubmitDisposition::Revoked;
        }

        let fence = submit.SubmissionFenceId;
        if let Err(refusal) = native
            .host_submissions
            .lock()
            .admit_host_completion(fence, resubmission)
        {
            let code = match refusal {
                HostSubmissionRefusal::Duplicate { .. } => 2,
                HostSubmissionRefusal::WentBackward { .. } => 3,
            };
            bump_with_code(&NR2_HOST_SUBMIT_REJECT, code);
            return NativeSubmitDisposition::Revoked;
        }

        let adapter = unsafe { native.adapter.as_ref() };
        let commit_record = record.payload_bytes != 0;
        let action = {
            let mut executor = native.executor.lock();
            let Some(slot) = executor.slots.get_mut(record.slot_index as usize) else {
                drop(executor);
                bump_with_code(&NR2_HOST_SUBMIT_REJECT, 4);
                return NativeSubmitDisposition::Revoked;
            };
            let old = core::mem::replace(slot, SubmissionSlot::Free);
            match old {
                SubmissionSlot::Collecting {
                    identity,
                    mut tickets,
                } => {
                    if queue_record_matches(native, &record, identity, record.slot_index as usize)
                        && append_queue_ticket(
                            adapter,
                            &mut tickets,
                            identity,
                            ticket,
                            resubmission,
                            commit_record,
                        )
                    {
                        *slot = SubmissionSlot::Collecting { identity, tickets };
                        Some(QueueSubmitAction::Pending)
                    } else {
                        *slot = SubmissionSlot::Collecting { identity, tickets };
                        None
                    }
                }
                SubmissionSlot::Ready {
                    identity,
                    mut tickets,
                    mut batch,
                } => {
                    if queue_record_matches(native, &record, identity, record.slot_index as usize)
                        && append_queue_ticket(
                            adapter,
                            &mut tickets,
                            identity,
                            ticket,
                            resubmission,
                            commit_record,
                        )
                    {
                        if commit_record {
                            if let Some(batch) = batch.take() {
                                *slot = SubmissionSlot::InFlight { identity, tickets };
                                Some(QueueSubmitAction::Enqueue {
                                    identity,
                                    slot_index: record.slot_index,
                                    batch,
                                })
                            } else {
                                *slot = SubmissionSlot::Ready {
                                    identity,
                                    tickets,
                                    batch,
                                };
                                None
                            }
                        } else {
                            *slot = SubmissionSlot::Ready {
                                identity,
                                tickets,
                                batch,
                            };
                            Some(QueueSubmitAction::Pending)
                        }
                    } else {
                        *slot = SubmissionSlot::Ready {
                            identity,
                            tickets,
                            batch,
                        };
                        None
                    }
                }
                SubmissionSlot::InFlight {
                    identity,
                    mut tickets,
                } => {
                    if queue_record_matches(native, &record, identity, record.slot_index as usize)
                        && append_queue_ticket(
                            adapter,
                            &mut tickets,
                            identity,
                            ticket,
                            resubmission,
                            commit_record,
                        )
                    {
                        *slot = SubmissionSlot::InFlight { identity, tickets };
                        Some(QueueSubmitAction::Pending)
                    } else {
                        *slot = SubmissionSlot::InFlight { identity, tickets };
                        None
                    }
                }
                SubmissionSlot::Terminal {
                    identity,
                    mut tickets,
                    success,
                    cleanup,
                } => {
                    if queue_record_matches(native, &record, identity, record.slot_index as usize)
                        && append_queue_ticket(
                            adapter,
                            &mut tickets,
                            identity,
                            ticket,
                            resubmission,
                            commit_record,
                        )
                    {
                        *slot = SubmissionSlot::Terminal {
                            identity,
                            tickets,
                            success,
                            cleanup,
                        };
                        Some(QueueSubmitAction::AlreadyTerminal { success })
                    } else {
                        *slot = SubmissionSlot::Terminal {
                            identity,
                            tickets,
                            success,
                            cleanup,
                        };
                        None
                    }
                }
                SubmissionSlot::Free => {
                    *slot = SubmissionSlot::Free;
                    None
                }
            }
        };
        let Some(action) = action else {
            bump_with_code(&NR2_HOST_SUBMIT_REJECT, 4);
            return NativeSubmitDisposition::Revoked;
        };
        NR2_HOST_SUBMIT_OK.fetch_add(1, Ordering::Relaxed);

        return match action {
            QueueSubmitAction::Pending => NativeSubmitDisposition::Pending,
            QueueSubmitAction::AlreadyTerminal { success } => {
                if success {
                    let _ =
                        crate::ddi::interrupt::complete_ordered_engine_submission(adapter, ticket);
                } else {
                    let _ = crate::ddi::interrupt::fail_ordered_engine_submission(adapter, ticket);
                }
                NativeSubmitDisposition::Pending
            }
            QueueSubmitAction::Enqueue {
                identity,
                slot_index,
                batch,
            } => {
                let host_context_id = batch.custody.host_context_id();
                let transport_instance = batch.custody.transport_instance();
                let completion = NativeHostCompletion {
                    context: NonNull::from(native),
                    identity,
                    slot_index,
                    custody: Some(batch.custody),
                };
                let mut pending = Some((batch.meta, batch.payload, completion));
                let queued = adapter.with_virtio(|gpu| {
                    if gpu.scanout_transport_instance() != transport_instance {
                        return None;
                    }
                    pending.take().map(|(meta, payload, completion)| {
                        gpu.enqueue_native_submit(
                            host_context_id,
                            crate::virtio::gpu::NativeSubmitDomain::Queue(native.ring_index),
                            meta,
                            payload,
                            identity.payload_bytes as usize,
                            completion,
                        )
                    })
                });
                match queued {
                    Ok(Some(Ok(_))) => NativeSubmitDisposition::Pending,
                    Ok(Some(Err((meta, payload, Some(completion), _)))) => {
                        completion.finish_with_cleanup(adapter, false, Some((meta, payload)));
                        NativeSubmitDisposition::Pending
                    }
                    Ok(Some(Err((meta, payload, None, _)))) => {
                        // The inner enqueue is required to return native
                        // custody on every refusal. Preserve the DMA buffers at
                        // DISPATCH and fail the exact K9 batch if that invariant
                        // is ever violated.
                        core::mem::forget(meta);
                        core::mem::forget(payload);
                        bump_with_code(&NR2_HOST_SUBMIT_REJECT, 5);
                        fail_inflight_without_completion(native, adapter, identity, slot_index);
                        NativeSubmitDisposition::Pending
                    }
                    Ok(None) | Err(_) => {
                        if let Some((meta, payload, completion)) = pending.take() {
                            completion.finish_with_cleanup(adapter, false, Some((meta, payload)));
                            NativeSubmitDisposition::Pending
                        } else {
                            bump_with_code(&NR2_HOST_SUBMIT_REJECT, 5);
                            NativeSubmitDisposition::Revoked
                        }
                    }
                }
            }
        };
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
    let disposition =
        crate::ddi::translation_session::with_current_host_submission(session, || {
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
        });
    disposition.unwrap_or_else(|| {
        bump_with_code(&NR2_HOST_SUBMIT_REJECT, 1);
        NativeSubmitDisposition::Revoked
    })
}

// ── `DxgkDdiSubmitCommandVirtual`, the HOS1 arm ──────────────────────────────

/// Admit one HOS1 descriptor on a D3D12-virtual HQA1 outer context.
///
/// This DISPATCH-level edge performs only bounded scalar/direct-object work:
/// exact 64-byte HOS1 validation, context/session/HOC1 rundown acquisition,
/// K9 ticket publication, and enqueue to the context-owned system work item.
/// The worker performs the potentially 15-MiB HOB1 snapshot/CRC/schema pass at
/// PASSIVE and enters the existing stock-Venus used-ring completion graph.
///
/// # Safety
/// `submit` is dxgkrnl's live argument struct for a submission on a context
/// whose role this driver resolved as an HQA1 attach.
pub(crate) unsafe fn submit_virtual(
    native: &NativeContext,
    session: NonNull<crate::ddi::translation_session::SessionObject>,
    device: &crate::device::DeviceContext,
    outer: &SpinLock<OuterSubmitContext>,
    submit: &DXGKARG_SUBMITCOMMANDVIRTUAL,
    ticket: crate::adapter::OrderedEngineTicket,
) -> NativeSubmitDisposition {
    // The measurement the two texts disagree about. Legal at any IRQL.
    // SAFETY: `KeGetCurrentIrql` reads the current processor's IRQL and has no
    // preconditions.
    NR2_SUBMIT_VIRTUAL_IRQL.store(
        unsafe { wdk_sys::ntddk::KeGetCurrentIrql() } as u32,
        Ordering::Relaxed,
    );

    let Some(context_operation) = native.acquire_operation() else {
        OuterExecutionRefusal::ContextClosed.record();
        return NativeSubmitDisposition::Revoked;
    };
    if native.class != NativeClass::Outer
        || crate::ddi::translation_session::execution_session_generation(session)
            != Some(native.session_generation)
    {
        OuterExecutionRefusal::SessionClosed.record();
        return NativeSubmitDisposition::Revoked;
    }

    let bytes = size_of::<HeliosOuterSubmitV1>();
    // §10.4: dxgkrnl copies the UMD's prefix into the front of KMD private data,
    // and `DmaBufferUmdPrivateDataSize` is how many bytes of it are the UMD's.
    // The selected UMD path contributes exactly one HOS1 and no compatibility
    // suffix. A shorter or larger UMD region cannot be reclassified.
    //
    // ⚠ `DXGKARG_SUBMITCOMMANDVIRTUAL` carries NO submission-offset pair, unlike
    // its physical sibling — the virtual DDI hands one submission's private data
    // directly — so offset 0 is the record here, and only here.
    if submit.pDmaBufferPrivateData.is_null()
        || (submit.DmaBufferPrivateDataSize as usize) != bytes
        || (submit.DmaBufferUmdPrivateDataSize as usize) != bytes
    {
        OuterExecutionRefusal::PrivateData.record();
        return NativeSubmitDisposition::Revoked;
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
            OuterExecutionRefusal::Descriptor.record();
            return NativeSubmitDisposition::Revoked;
        }
    };

    // SAFETY: `Value` is the UINT view of the WDK flags union. Bit 7 is the
    // same Resubmission discriminator used by physical SubmitCommand.
    let resubmission = (unsafe { submit.Flags.__bindgen_anon_1.Value } & (1 << 7)) != 0;
    let expectation = {
        let context = outer.lock();
        HeliosOuterBatchExpectation {
            package_generation: context.package_generation,
            session_generation: context.session_generation,
            context_generation: context.context_generation,
            endpoint_id: context.endpoint_id,
            flags: context.arm_flags,
            max_command_bytes: submit.DmaBufferSize as u64,
            last_batch_id: if resubmission {
                record.batch_id.saturating_sub(1)
            } else {
                context.last_batch_id()
            },
            allocation_list_count: 0,
        }
    };
    if let Err(reject) = record.validate(&expectation, submit.DmaBufferSize as u64) {
        bump_with_code(
            &NR2_HOS1_REJECT,
            helios_kmd_logic::native_render::outer_submit_code(reject),
        );
        OuterExecutionRefusal::Descriptor.record();
        return NativeSubmitDisposition::Revoked;
    }

    let fence = submit.SubmissionFenceId;
    if let Err(refusal) = native
        .host_submissions
        .lock()
        .admit_host_completion(fence, resubmission)
    {
        let code = match refusal {
            HostSubmissionRefusal::Duplicate { .. } => 2,
            HostSubmissionRefusal::WentBackward { .. } => 3,
        };
        bump_with_code(&NR2_HOST_SUBMIT_REJECT, code);
        OuterExecutionRefusal::FenceOrder.record();
        return NativeSubmitDisposition::Revoked;
    }

    let adapter = unsafe { native.adapter.as_ref() };
    if resubmission {
        let terminal = {
            let mut executor = native.executor.lock();
            let mut found = None;
            for slot in executor.slots.iter_mut() {
                let (identity, tickets, terminal) = match slot {
                    SubmissionSlot::InFlight { identity, tickets } => (identity, tickets, None),
                    SubmissionSlot::Terminal {
                        identity,
                        tickets,
                        success,
                        ..
                    } => (identity, tickets, Some(*success)),
                    _ => continue,
                };
                let exact = identity.batch_token == record.batch_id
                    && identity.payload_bytes == record.hob1_bytes
                    && identity.full_payload_crc64 == record.hob1_crc64
                    && identity.fragment_count == 1
                    && identity.session_generation == native.session_generation
                    && identity.context_generation == native.context_generation
                    && identity.ring_index == native.ring_index;
                if !exact {
                    continue;
                }
                if found.is_some()
                    || !append_queue_ticket(adapter, tickets, *identity, ticket, true, true)
                {
                    found = Some(Err(()));
                    break;
                }
                found = Some(Ok(terminal));
            }
            found
        };
        return match terminal {
            Some(Ok(Some(success))) => {
                if success {
                    let _ =
                        crate::ddi::interrupt::complete_ordered_engine_submission(adapter, ticket);
                } else {
                    let _ = crate::ddi::interrupt::fail_ordered_engine_submission(adapter, ticket);
                }
                NR2_HOST_SUBMIT_OK.fetch_add(1, Ordering::Relaxed);
                NativeSubmitDisposition::Pending
            }
            Some(Ok(None)) => {
                NR2_HOST_SUBMIT_OK.fetch_add(1, Ordering::Relaxed);
                NativeSubmitDisposition::Pending
            }
            Some(Err(())) | None => {
                OuterExecutionRefusal::ResubmissionMismatch.record();
                NativeSubmitDisposition::Revoked
            }
        };
    }

    let mut context = outer.lock();
    let prior_batch_id = context.last_batch_id();
    if let Err(refusal) = admit_hos1(&mut context, &record, submit.DmaBufferSize as u64) {
        drop(context);
        bump_with_code(&NR2_HOS1_REJECT, refusal.code());
        OuterExecutionRefusal::Descriptor.record();
        return NativeSubmitDisposition::Revoked;
    }
    drop(context);

    let Some(session_operation) =
        crate::ddi::translation_session::acquire_execution_operation(session)
    else {
        OuterExecutionRefusal::SessionClosed.record();
        return NativeSubmitDisposition::Revoked;
    };
    if session_operation.transport_instance == 0 {
        OuterExecutionRefusal::TransportMismatch.record();
        return NativeSubmitDisposition::Revoked;
    }
    let Some(command_pool) = device.acquire_outer_gpuva_use(
        session,
        submit.DmaBufferVirtualAddress,
        submit.DmaBufferSize as u64,
        None,
        true,
    ) else {
        OuterExecutionRefusal::CommandPoolMissing.record();
        return NativeSubmitDisposition::Revoked;
    };

    let (slot_index, identity) = {
        let mut executor = native.executor.lock();
        let Some(slot_index) = executor
            .slots
            .iter()
            .position(|slot| matches!(slot, SubmissionSlot::Free))
        else {
            OuterExecutionRefusal::SlotExhausted.record();
            return NativeSubmitDisposition::Revoked;
        };
        let Some(slot_generation) = executor.mint_slot_generation() else {
            OuterExecutionRefusal::SlotExhausted.record();
            return NativeSubmitDisposition::Revoked;
        };
        let identity = BatchIdentity {
            batch_token: record.batch_id,
            slot_generation,
            payload_bytes: record.hob1_bytes,
            full_payload_crc64: record.hob1_crc64,
            fragment_count: 1,
            session_generation: native.session_generation,
            context_generation: native.context_generation,
            ring_index: native.ring_index,
        };
        let mut tickets = BatchTickets::new();
        if !append_queue_ticket(adapter, &mut tickets, identity, ticket, false, true) {
            OuterExecutionRefusal::SlotExhausted.record();
            return NativeSubmitDisposition::Revoked;
        }
        executor.slots[slot_index] = SubmissionSlot::InFlight { identity, tickets };
        (slot_index as u32, identity)
    };

    let pending = OuterPending {
        identity,
        slot_index,
        prior_batch_id,
        submit: record,
        device: NonNull::from(device),
        session,
        command_pool,
        context_operation,
        session_operation,
    };
    let Some(worker) = native.outer_worker.as_ref() else {
        fail_outer_slot(
            native,
            identity,
            slot_index,
            OuterExecutionRefusal::WorkerClosed,
        );
        drop(pending);
        return NativeSubmitDisposition::Pending;
    };
    if let Err((refusal, pending)) = worker.enqueue(NonNull::from(native), pending) {
        fail_outer_slot(native, identity, slot_index, refusal);
        drop(pending);
        return NativeSubmitDisposition::Pending;
    }
    NR2_HOS1_OK.fetch_add(1, Ordering::Relaxed);
    NR2_OUTER_QUEUED.fetch_add(1, Ordering::Relaxed);
    NR2_HOST_SUBMIT_OK.fetch_add(1, Ordering::Relaxed);
    NativeSubmitDisposition::Pending
}
