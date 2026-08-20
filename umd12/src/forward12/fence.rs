//! L7 — fences and query heaps.
//!
//! Owns 6 of `DEVICE_FUNCS_CORE_0109` (groups (i) 3, (j) 3).
//!
//! ⭐ **A D3D12 fence object IS a pair of GPU virtual addresses**
//! (`DDI_REFERENCE.md` §10.1), and there are exactly **two** fence operations,
//! both queue-level: `pfnSignalFence` and `pfnWaitForFence` on the command-queue
//! table (L2's). ⛔ There is **no** CPU-signal DDI and **no** CPU-wait DDI
//! (§10.3) — a reading that looks like a missing slot and is not.
//!
//! `DECISIONS.md` §6 downgraded the monitored-fence risk to MEDIUM; the residual
//! probe is G-fence.
//!
//! # ⭐ What the driver gets, and what it therefore builds
//!
//! `D3D12DDIARG_CREATE_FENCE` carries **only** `{FenceCount, Fences}`, and each
//! `D3D12DDI_FENCE` is `{FenceValue.BaseAddress, FenceMonitoredValue.BaseAddress,
//! Flags}` — three values the *runtime* chose. The driver never receives a
//! `D3DKMT_HANDLE` for the fence and never gets the CPU mapping; the runtime
//! created the monitored fence with `D3DKMTCreateSynchronizationObject2` and kept
//! the CPU half and the kernel handle for itself (§10.1). So there is nothing
//! here for the driver to *own* on the WDDM side.
//!
//! What the driver must own is the other half: **an engine fence to order its own
//! pipeline with.** §10.3 states the shape — `pfnSignalFence` / `pfnWaitForFence`
//! are *ordering instructions to the driver's own pipeline* plus an
//! adapter-mask report, and the kernel-side signal/wait is the runtime's job.
//! Helios' pipeline is vkd3d, so this lane creates one `ID3D12Fence` on the
//! engine device per DDI fence, and L2's two queue slots forward
//! `ID3D12CommandQueue::Signal` / `::Wait` onto it. That is the whole object.
//!
//! ⛔ **The runtime's GPU VAs are therefore deliberately NOT stored, and there is
//! no future lane that will want them.** They are logged at create — which is the
//! only thing this driver can honestly do with them — and a field nothing reads
//! would be exactly the dead state `PARALLEL.md` §10 forbids.
//!
//! ⛔⛔ **The old text here named a consumer that CANNOT EXIST, and it is quoted so
//! nobody re-derives it:** *"The lane that gains a real consumer (a
//! `pfnSignalSynchronizationObjectFromGpuCb` queued software signal packet on the
//! queue's WDDM context, §10.4's table) adds them then."* `DDI_REFERENCE.md`
//! §10.4's correction block (`:2306-2331`) **struck those rows** and states why
//! from the bindings: every `pfnSignal*Cb` / `pfnWait*Cb` names its target by
//! `D3DKMT_HANDLE`, and `D3D12DDIARG_CREATE_FENCE` carries no `D3DKMT_HANDLE`, no
//! `hRTFence` and no CPU pointer — so **the driver can never name a D3D12 fence to
//! the kernel.** Measured on Helios, both `BaseAddress`es arrive **0**
//! (`CreateFence: valueVA=0x0 monitoredVA=0x0`,
//! `tmp/dx12/gates/G8-r0/umd12-trace-pid10836.log`), so there is not even fence
//! memory to write. `KMD_IMPACT.md` §14a.5 then **forbids** the design outright:
//! *"No `pfnSignal*Cb` for the application's fence."*
//!
//! ⭐ What is true instead: the runtime owns the fence and its signal, and this
//! driver's only lever is **what dxgkrnl orders that signal behind** — the DMA
//! packets already submitted on the queue's WDDM context. That is `EclWddmSubmitted`
//! and nothing in this file.
//!
//! # ⛔⛔ The engine fence is a SHADOW, and it CAN diverge from the runtime's
//!
//! ⚠ **Read this before touching either fence slot: the naive forward deadlocks
//! the stack, silently.** The engine `ID3D12Fence` is created at **0** and the
//! only thing in this driver that ever advances it is L2's `pfnSignalFence`. Two
//! ordinary application behaviours move the *runtime's* fence without moving this
//! one:
//!
//! * `ID3D12Device::CreateFence(InitialValue = N)` — `D3D12DDIARG_CREATE_FENCE`
//!   carries only `{FenceCount, Fences}`
//!   (`umd12/bindgen/cached/d3d12umddi.rs:51121-51124`), so the initial value
//!   never reaches the driver. `CreateFence(1)` + `queue->Wait(f, 1)` is a common
//!   idiom precisely because the runtime considers that wait already satisfied;
//! * `ID3D12Fence::Signal(N)` from the CPU — §10.3: the CPU signal, the CPU wait
//!   and `GetCompletedValue` are all executed by the *runtime* against the
//!   monitored fence's own mapping and **never reach the driver**.
//!
//! ⛔ An `ID3D12CommandQueue::Wait` issued on the shadow for a value the shadow
//! can never reach does not fail — it **blocks that engine queue forever**. Every
//! later submit on that queue then never executes, and the readout is a TDR or a
//! frozen compositor with `FenceOpBadArg = FenceOpFenceMissing =
//! FenceOpEngineFailed = 0` and not one log line. ⚠ §14.0's measurement that WARP
//! never entered these two slots across 20 frames is **not** a defence: a zero
//! reading is not evidence a path works, and WARP is one software-scheduled
//! implementation rather than the contract.
//!
//! ⇒ [`FenceState`] therefore carries a **watermark of every `Value` this driver
//! has itself issued a signal for**, plus a **count of the signals it has issued at
//! all**, and L2's `pfnWaitForFence` forwards only the waits that watermark can
//! satisfy. Everything else is counted and left to the kernel-side ordering §10.3
//! says the runtime performs itself.
//!
//! # ⛔⛔ Two arms, because a dropped wait has two very different meanings
//!
//! ⚠ **The cost of the watermark choice, named rather than hidden:** a legal
//! wait-before-signal — an application enqueuing `queueB->Wait(f, N)` *before*
//! `queueA->Signal(f, N)`, which D3D12 permits — is above the watermark at the
//! moment it arrives and is dropped rather than carried. **Dropping it produces
//! wrong pixels**, silently, and the async-compute subtest of a real benchmark is
//! exactly the shape that issues it.
//!
//! ⛔ **The old text said this "closes when §10.4's
//! `pfnWaitForSynchronizationObjectFromGpuCb` half lands". That is REFUTED and
//! FORBIDDEN** — see the correction quoted above: no such callback can ever name a
//! `D3D12DDI_HFENCE`, and `KMD_IMPACT.md` §14a.5 rules the design out. There is no
//! pending work that closes this gap; the **only** channel that orders anything is
//! the engine forward `queue::fence_operation` already makes, which vkd3d resolves
//! into the signalling queue's `submission_timeline`.
//!
//! ⇒ so the dropped waits are **split by whether this driver participates in the
//! fence's timeline at all**, and only one of the two arms can be honest about it:
//!
//! | condition | counter | treatment | why |
//! |---|---|---|---|
//! | [`FenceState::driver_signals_issued`] is **false** | `FenceWaitRuntimeOwned` | dropped, counted, **not** reported | the value's provenance is entirely outside this DDI: a `CreateFence(InitialValue = N)` the DDI never delivers, or a CPU `ID3D12Fence::Signal` §10.3 says never reaches the driver. Both are waits the runtime **already considers satisfied**, so dropping is exactly right, and `CreateFence(1)` + `queue->Wait(f, 1)` is a common idiom. Reporting it would answer a legal call with *"Removing device due to bad UMD error"* — `descriptors.rs`'s scar |
//! | it is **> 0** and `Value` is above the watermark | `FenceWaitNotForwarded` | dropped, counted, logged — ⛔ **NOT** reported through `pfnSetErrorCb` | this driver *is* on that fence's timeline and is being asked for ordering beyond what it has issued, so the drop is a real gap (`PENDING.md` §S-2). ⛔ **But it is NOT a driver fault, and until 2026-08-07 this row said it was**: the legal wait-before-signal named four paragraphs above lands here **deterministically**, so removing the `ID3D12Device` would answer an ordinary async-compute frame with *"Removing device due to bad UMD error"*. The *"empty scene with a score"* shape is what the **counter and the log** exist to prevent; device removal adds no attribution and costs the whole run. Argument at the site, `queue::fence_operation` |
//!
//! ⛔ **And the first arm is NOT clean, which is stated here rather than hidden in a
//! counter's grading.** A CPU `Signal` that has *not happened yet* — `queueB->Wait(f,
//! N)` followed later by `fence->Signal(N)` from the CPU — is **indistinguishable at
//! this DDI** from the already-satisfied case, because the driver is never told the
//! initial value and never sees a CPU signal. It lands in `FenceWaitRuntimeOwned`
//! and it is a real ordering gap. Forwarding it instead is not an option: the engine
//! fence can never reach `N`, so an engine wait for it blocks that vkd3d queue
//! **forever**. ⇒ the counter's grading says exactly this, and the only real fix is
//! the runtime's monitored fence, which §10.4 proves this driver cannot reach.
//!
//! ⚠ **Shared fences are the third source and they are not separable either.**
//! `D3D12DDI_FENCE_FLAGS` has no shared bit at all (see below), so a fence shared
//! through `D3DKMTShareObjects` on the runtime's own kernel handle arrives here
//! looking exactly like any other. It belongs with the shared-handle work, not with
//! a counter here.
//!
//! # ⚠ The two fence shapes this driver cannot honour, and what it does instead
//!
//! * **`FenceCount > 1`** is the multi-adapter (LDA) case — one placement per
//!   physical adapter (§10.1). Helios is single-adapter and
//!   `pfnGetImplicitPhysicalAdapterMask` says so, so a multi-placement fence is
//!   refused rather than silently backed by one engine fence.
//! * **`D3D12DDI_FENCE_FLAG_BOTTOM_OF_PIPE`** is the driver being told the fence
//!   must be signalled after *all* preceding GPU work retires, not at
//!   command-processor front-end time. ⛔ §10.4: *"Do not claim
//!   `BOTTOM_OF_PIPE` semantics the stack cannot deliver."* Forwarding to
//!   `ID3D12CommandQueue::Signal` does give bottom-of-pipe ordering **within the
//!   engine** — vkd3d signals the timeline semaphore after the submission — and
//!   the WDDM half is `pfnExecuteCommandLists`' `pfnRenderCb` packet carrying the
//!   frame's own completion boundary (`EclFenceSampled`, CLAUDE.md's fence
//!   invariant). ⛔ Its old text said that boundary is *"knob-gated and off by
//!   default (A1, `knobs12::UMD12_ECL_DRAIN`)"* — **FALSE since `f71fef4`**:
//!   `Umd12EclFence` defaults **ON** and samples on **both** drain arms, so
//!   `EclFenceSampled` is nonzero on a default build. `Umd12EclDrain` (default
//!   OFF) decides only EXACT vs a **prefix that may under-wait**, and
//!   `EclFenceNoDrain` — not `EclFenceSampled` — is what says which you got.
//!   ⛔ It is **not** a queued software signal packet on this fence; that design
//!   is struck and forbidden, see above. The flag is accepted and **counted**.
//!
//! ⚠ **There is no shared / cross-adapter fence flag at this DDI.**
//! `D3D12DDI_FENCE_FLAGS` has exactly two enumerators, `NONE = 0x0` and
//! `BOTTOM_OF_PIPE = 0x1` (`d3d12umddi.h:1156-1161`); `D3D12_FENCE_FLAG_SHARED`
//! and `..._CROSS_ADAPTER` are **API** flags that never reach the driver, because
//! sharing a fence is `D3DKMTShareObjects` on the runtime's own kernel handle —
//! the one the driver never sees. A bit outside the two the header defines is
//! still counted, because the header is the only authority here and an unknown
//! bit is a contract this driver has not read.
//!
//! # Query heaps
//!
//! Straight forward to `ID3D12Device::CreateQueryHeap`. ⛔ The type is
//! **translated**, never passed through: `DDI_REFERENCE.md` §9.6.1 is the scar —
//! the descriptor-heap flag enums collide on value `0x1` with different meanings
//! and forwarding the DDI value produced the wrong heap with no error. The DDI
//! and API query-heap enumerators happen to agree numerically today
//! (0,1,2,3,4,5,7); the match below makes that a fact the compiler re-checks
//! rather than an assumption, and a value in neither list is refused.
//!
//! ⚠ Their consumer is L3c (`copy.rs` — `pfnBeginQuery`/`pfnEndQuery`/
//! `pfnResolveQueryData`), which is not in this round. Creating them correctly
//! now is still this lane's job: the runtime creates a query heap when the
//! application does, not when the first query is recorded.

use helios_umd_common::hr::{Hresult, E_FAIL, E_INVALIDARG, S_OK};
use helios_umd_common::refusals::RefusalCounter;
use helios_umd_common::slot::{Boxed, Com, DdiHandle, Slot};
use helios_umd_common::throttle::LogThrottle;
use windows::Win32::Graphics::Direct3D12::{
    ID3D12QueryHeap, D3D12_QUERY_HEAP_DESC, D3D12_QUERY_HEAP_TYPE,
    D3D12_QUERY_HEAP_TYPE_COPY_QUEUE_TIMESTAMP, D3D12_QUERY_HEAP_TYPE_OCCLUSION,
    D3D12_QUERY_HEAP_TYPE_PIPELINE_STATISTICS, D3D12_QUERY_HEAP_TYPE_PIPELINE_STATISTICS1,
    D3D12_QUERY_HEAP_TYPE_SO_STATISTICS, D3D12_QUERY_HEAP_TYPE_TIMESTAMP,
    D3D12_QUERY_HEAP_TYPE_VIDEO_DECODE_STATISTICS,
};

use super::tables12::{stage, DeviceCoreTable, Filling};
use crate::{ddi12, device12, log_error, note_refusal};

// ---------------------------------------------------------------------------
// Handle payloads
// ---------------------------------------------------------------------------

// ⛔ **The payload is a property of the HANDLE TYPE, declared once, here.**
// `ARCHITECTURE.md` §12 rule 7 / R803 is the scar: choosing the payload at the
// call site compiled and produced a `ManuallyDrop` whose vtable pointer was a
// struct field — a wild call on first use.
//
// ⚠ A query heap is a bare owning COM pointer and nothing else — it needs no
// shadow state, so `com_handles!`. ⛔ A **fence** is not: it carries the
// signalled watermark the module doc's shadow-divergence section exists for, and
// a watermark that lived anywhere but on the fence object could not answer
// "can this driver's timeline reach `Value`?" for the fence actually named by
// `D3D12DDIARG_FENCE_OPERATION::Fence`. So it is `boxed_handles!`.
helios_umd_common::com_handles!(crate::ddi12::D3D12DDI_HQUERYHEAP,);

helios_umd_common::boxed_handles!(crate::ddi12::D3D12DDI_HFENCE => FenceState);

// ---------------------------------------------------------------------------
// Log budgets
// ---------------------------------------------------------------------------

// ⛔ **`log_error!` is unbounded by construction** — `umd_common/src/log.rs:279`
// formats and writes on every call — and an application that creates a fence per
// frame-in-flight, or a query heap per pass, would turn a one-line create record
// into the ~9k writes/s T2 measured (`umd/src/device_funcs.rs:713-723`). Both
// families below carry a budget: the first 8, then every 4096th.

/// Budget for the fence-lifetime lines.
static FENCE_LOG: LogThrottle = LogThrottle::new();
/// Budget for the query-heap lines.
static QUERY_HEAP_LOG: LogThrottle = LogThrottle::new();

/// The one budget shape this lane uses: the first 8, then every 4096th.
///
/// Returns the occurrence ordinal (0-based) when the line should be emitted.
fn budget(t: &LogThrottle) -> Option<usize> {
    t.first_n_then_every(8, 4096)
}

/// The private-block size every `CalcPrivate*` in this lane returns.
///
/// ⛔ **One machine word, and never 0.** `umd_common::slot`'s whole encoding is
/// *"a machine word inside private memory the driver sized"*, and the shipping
/// D3D11 driver returns the same 8 for every real `CalcPrivate*Size`
/// (`umd/src/forward/queries.rs:9-14`, `umd/src/forward/shaders.rs:63-69`).
///
/// ⚠ The "never 0" half is load-bearing and is a different rule from
/// `device12::calc_private_device_size`, which *does* answer 0 for a null arg.
/// That pair is coherent because its `pfnCreateDevice` refuses on exactly the
/// same condition. Here it would not be: a 0 makes the runtime hand the paired
/// `Create` a **zero-byte** private region that the slot write then runs past —
/// which is `umd/src/device_funcs.rs:708-723` verbatim, where a transient 0 from
/// a concurrent `CalcPrivate*Size` produced heap corruption surfacing as a wild
/// call in a 3DMark worker. So a bad argument here is counted and the size is
/// still answered.
const PRIVATE_SLOT_SIZE: usize = core::mem::size_of::<*mut core::ffi::c_void>();

/// Per-`D3D12DDI_HFENCE` Core-0116 native object state.
///
/// ⚠ **`pub`, not `pub(crate)`, and that is forced rather than chosen.**
/// `BoxedHandle` is a `pub` trait in `helios_umd_common`, so an associated type
/// less visible than the trait is E0446. It escapes nowhere: `forward12` and
/// `fence` are both `pub(crate) mod` inside a `cdylib` that exports no Rust API,
/// every field below is private, and the three methods are the whole surface L2
/// can reach. Same shape as `queue::QueueState`.
/// No lower-vkd3d fence or independent timeline exists. Queue Signal/Wait names
/// `h_sync_object` through the exact owning WDDM context callbacks.
pub struct FenceState {
    h_device: ddi12::D3D12DDI_HDEVICE,
    h_rt_fence: ddi12::D3D12DDI_HRTFENCE,
    h_sync_object: ddi12::D3DKMT_HANDLE,
    mapping: ddi12::D3DDDI_NATIVEFENCEMAPPING,
    pdd: helios_protocol::HeliosNativeFencePddV1,
    opened: bool,
}

impl FenceState {
    pub(crate) fn belongs_to(&self, h_device: ddi12::D3D12DDI_HDEVICE) -> bool {
        !self.h_device.pDrvPrivate.is_null()
            && self.h_device.pDrvPrivate == h_device.pDrvPrivate
            && self.h_sync_object != 0
            && self.pdd.object_generation != 0
    }

    pub(crate) fn h_sync_object(&self) -> ddi12::D3DKMT_HANDLE {
        self.h_sync_object
    }
}
/// The slot behind a `D3D12DDI_HFENCE`.
///
/// ⭐ Named rather than generic **on purpose**: a `slot_of::<T>(h)` helper would
/// put the payload type back at the call site, which is the R803 shape the
/// `BoxedHandle` / `ComHandle` markers exist to remove. One function per handle
/// type names its payload exactly once in this file.
///
/// # Safety
/// `h`'s slot, when non-null, must lie inside the private memory
/// [`calc_private_fence_size`] sized for this handle.
unsafe fn fence_slot(h: ddi12::D3D12DDI_HFENCE) -> Option<Slot<Boxed<FenceState>>> {
    // SAFETY: forwarded unchanged; the caller's guarantee is `Slot::from_priv`'s.
    unsafe { Slot::from_priv(h.drv_private()) }
}

/// The slot behind a `D3D12DDI_HQUERYHEAP`. Named for the same reason as
/// [`fence_slot`].
///
/// # Safety
/// `h`'s slot, when non-null, must lie inside the private memory
/// [`calc_private_query_heap_size`] sized for this handle.
unsafe fn query_heap_slot(h: ddi12::D3D12DDI_HQUERYHEAP) -> Option<Slot<Com<ID3D12QueryHeap>>> {
    // SAFETY: forwarded unchanged; the caller's guarantee is `Slot::from_priv`'s.
    unsafe { Slot::from_priv(h.drv_private()) }
}

/// The fence state behind a DDI fence handle, borrowed for the caller's DDI
/// call.
///
/// ⭐ **This is L2's door into L7**, and it exists so the `D3D12DDI_HFENCE`
/// payload has exactly one declaration. `PARALLEL.md` §4 folded L2 and L7 into
/// one agent for precisely this edge: the queue table's `pfnSignalFence` /
/// `pfnWaitForFence` take a `D3D12DDI_HFENCE` whose payload type this file
/// declares, and splitting them would have made L2 refuse two slots on a
/// dependency that costs six to remove.
///
/// ⛔ It hands back the **state**, not the bare `ID3D12Fence`, because the
/// watermark and the engine fence must be read as one object: a caller holding
/// only the fence has no way to ask whether a wait on it is satisfiable, which is
/// exactly the mistake the module doc's shadow-divergence section exists to
/// prevent.
///
/// ⚠ `Slot::ptr()`, never `Slot::<Boxed<_>>::get()` — the same second door
/// `queue.rs`'s accessors take, with the D3D12 argument re-derived there: the
/// returned reference is bound to the caller's own binding (one DDI call) rather
/// than `'static`, and the only path that drops the box is this fence's
/// `pfnDestroyFence`.
///
/// # Safety
/// `h` must be a handle [`create_fence`] returned `S_OK` for and
/// [`destroy_fence`] has not been called on, and the returned reference must not
/// outlive the DDI call that obtained it.
pub(crate) unsafe fn fence_state<'a>(h: ddi12::D3D12DDI_HFENCE) -> Option<&'a FenceState> {
    // SAFETY: the caller guarantees a live fence handle, so its slot lies inside
    // the private block `calc_private_fence_size` sized.
    let slot = unsafe { fence_slot(h) }?;
    // SAFETY: same precondition; `ptr` reads the word and reports an empty slot
    // as null rather than fabricating a reference.
    let p = unsafe { slot.ptr() };
    if p.is_null() {
        return None;
    }
    // SAFETY: non-null per the check, and the box it points at was written by
    // `create_fence` and is dropped only by `destroy_fence`.
    Some(unsafe { &*p })
}

// ---------------------------------------------------------------------------
// ⭐ The query-heap seam — L3c's door into L7 (the ONE accessor L3c may add)
// ---------------------------------------------------------------------------

/// The engine `ID3D12QueryHeap` behind a DDI query-heap handle, borrowed for
/// the caller's DDI call.
///
/// ⭐ **`pub(crate)` and named, because the payload of `D3D12DDI_HQUERYHEAP` is
/// declared exactly once — by the `com_handles!` invocation at the top of this
/// file.** L3c (`copy.rs`) owns `pfnBeginQuery`, `pfnEndQuery` and
/// `pfnResolveQueryData`, all three of which need the engine heap behind this
/// handle, and `ARCHITECTURE.md` §12 rule 7 / R803 is the scar that says the
/// payload must be derived from the handle **type** in one place rather than
/// decoded at each call site. L7 owns the handle, so this is that one place —
/// the same shape [`fence_state`] takes for `D3D12DDI_HFENCE` and
/// `resource12::engine_resource` takes for `D3D12DDI_HRESOURCE`.
///
/// ⚠ **A `ManuallyDrop`, not a shared reference, and that is the slot's shape
/// rather than a weaker choice.** `resource12`'s accessor can hand back `&T`
/// because a resource's owning reference lives in a `Box<ResourceState>` field
/// there is something to borrow *from*; a query heap is a bare `Slot<Com<_>>`
/// whose whole content is one raw COM word, so the only way to name it as an
/// interface is to rebuild the wrapper. `ManuallyDrop` is what stops that
/// rebuilt wrapper from issuing a `Release` for the reference the slot still
/// owns — `Slot::<Com<_>>::load`'s own documented contract, and the same
/// pairing `pso.rs` uses for a borrowed root signature.
///
/// # Safety
/// `h` must be a handle [`create_query_heap`] returned `S_OK` for and
/// [`destroy_query_heap`] has not been called on, and the returned value must
/// not outlive the DDI call that obtained it.
pub(crate) unsafe fn engine_query_heap(
    h: ddi12::D3D12DDI_HQUERYHEAP,
) -> Option<core::mem::ManuallyDrop<ID3D12QueryHeap>> {
    // SAFETY: the caller guarantees a live query-heap handle, so its slot lies
    // inside the private block `calc_private_query_heap_size` sized.
    let slot = unsafe { query_heap_slot(h) }?;
    // SAFETY: same precondition; `load` reads the slot word and reports an empty
    // slot as `None` rather than fabricating an interface.
    unsafe { slot.load() }
}

// ---------------------------------------------------------------------------
// (i) Fences — 3 slots
// ---------------------------------------------------------------------------

/// `pfnCalcPrivateFenceSize`.
///
/// # Safety
/// `arg`, when non-null, must point at a live `D3D12DDIARG_CREATE_FENCE` for the
/// duration of the call.
unsafe extern "C" fn calc_private_fence_size(
    _h_device: ddi12::D3D12DDI_HDEVICE,
    arg: *const ddi12::D3D12DDIARG_CREATE_FENCE_0116,
) -> ddi12::SIZE_T {
    if arg.is_null() {
        note_refusal(&L7_REFUSALS.fence_bad_arg);
    }
    // ⛔ Answered unconditionally — see `PRIVATE_SLOT_SIZE`. The size does not
    // depend on the argument, so there is nothing a null could change except to
    // make this driver hand back a zero-byte region.
    PRIVATE_SLOT_SIZE as ddi12::SIZE_T
}

/// `pfnCreateFence`.
///
/// Returns `HRESULT` directly — one of the nine `Create*` slots that do
/// (`DDI_REFERENCE.md` §7.3(2)), so no `pfnSetErrorCb` is involved.
///
/// # Safety
/// `h_device` must be a live handle from `device12::create_device`; `h_fence`'s
/// `pDrvPrivate` must address the private block [`calc_private_fence_size`]
/// sized; `arg` must point at a live `D3D12DDIARG_CREATE_FENCE` whose `Fences`
/// addresses `FenceCount` readable `D3D12DDI_FENCE`s for the call.
fn all_zero(bytes: &[u8]) -> bool {
    bytes.iter().all(|byte| *byte == 0)
}

fn mapping_is_valid(mapping: &ddi12::D3DDDI_NATIVEFENCEMAPPING) -> bool {
    !mapping.CurrentValueCpuVa.is_null()
        && mapping.CurrentValueGpuVa != 0
        && mapping.MonitoredValueGpuVa != 0
        && all_zero(&mapping.Reserved)
}

fn hnf1_flags_from_sync_flags(flags: &ddi12::D3DDDI_SYNCHRONIZATIONOBJECT_FLAGS) -> Option<u32> {
    // SAFETY: `Value` is the documented UINT view of this flags union.
    let value = unsafe { flags.__bindgen_anon_1.Value };
    const SHARED: u32 = 1 << 0;
    const NT_SECURITY_SHARING: u32 = 1 << 1;
    if value & !(SHARED | NT_SECURITY_SHARING) != 0
        || value & NT_SECURITY_SHARING != 0 && value & SHARED == 0
    {
        return None;
    }
    Some(if value & SHARED != 0 {
        helios_protocol::HELIOS_HNF1_FLAG_SHARED
    } else {
        0
    })
}

unsafe fn create_native_fence(
    dev: &device12::HeliosD3D12Device,
    h_device: ddi12::D3D12DDI_HDEVICE,
    h_rt_fence: ddi12::D3D12DDI_HRTFENCE,
    native_args: *mut ddi12::D3DKMT_CREATENATIVEFENCE,
) -> Result<FenceState, ddi12::HRESULT> {
    let Some(callbacks) = (unsafe { dev.um_callbacks_0116.as_ref() }) else {
        return Err(E_FAIL);
    };
    let Some(create) = callbacks.pfnCreateNativeFenceCb else {
        return Err(E_FAIL);
    };
    let Some(args) = (unsafe { native_args.as_mut() }) else {
        return Err(E_INVALIDARG);
    };
    let sync_flags = hnf1_flags_from_sync_flags(&args.Info.Flags).ok_or(E_INVALIDARG)?;
    if args.Info.Type as u32 != helios_protocol::HELIOS_NATIVE_FENCE_TYPE_DEFAULT
        || args.Info.EngineAffinity != 1
        || args.Info.PhysicalAdapterIndex != 0
        || !all_zero(&args.Info.Reserved)
        || !all_zero(&args.Reserved)
        // SAFETY: `Value` is the UINT view; every create-flags bit is reserved.
        || unsafe { args.Flags.__bindgen_anon_1.Value } != 0
    {
        return Err(E_INVALIDARG);
    }
    args.PrivateDriverData = helios_protocol::HeliosNativeFencePddV1::create_input(
        helios_protocol::HELIOS_PACKAGE_GENERATION,
        helios_protocol::HELIOS_NATIVE_FENCE_TYPE_DEFAULT,
        sync_flags,
    )
    .to_bytes();

    let hr = unsafe { create(dev.h_rt_device, h_rt_fence, args) };
    if hr < 0 {
        return Err(hr);
    }
    let pdd = helios_protocol::HeliosNativeFencePddV1::from_bytes(&args.PrivateDriverData);
    pdd.validate_returned(
        helios_protocol::HELIOS_PACKAGE_GENERATION,
        dev.adapter_luid,
        helios_protocol::HELIOS_NATIVE_FENCE_TYPE_DEFAULT,
        sync_flags,
    )
    .map_err(|_| E_FAIL)?;
    if args.hSyncObject == 0
        || args.Info.Type as u32 != helios_protocol::HELIOS_NATIVE_FENCE_TYPE_DEFAULT
        || args.Info.EngineAffinity != 1
        || args.Info.PhysicalAdapterIndex != 0
        || !mapping_is_valid(&args.Info.NativeFenceMapping)
        || !all_zero(&args.Info.Reserved)
        || !all_zero(&args.Reserved)
    {
        return Err(E_FAIL);
    }
    Ok(FenceState {
        h_device,
        h_rt_fence,
        h_sync_object: args.hSyncObject,
        mapping: args.Info.NativeFenceMapping,
        pdd,
        opened: false,
    })
}

unsafe fn open_native_fence(
    dev: &device12::HeliosD3D12Device,
    h_device: ddi12::D3D12DDI_HDEVICE,
    h_rt_fence: ddi12::D3D12DDI_HRTFENCE,
    open_args: *mut ddi12::D3DKMT_OPENNATIVEFENCEFROMNTHANDLE,
) -> Result<FenceState, ddi12::HRESULT> {
    let Some(callbacks) = (unsafe { dev.um_callbacks_0116.as_ref() }) else {
        return Err(E_FAIL);
    };
    let Some(open) = callbacks.pfnOpenNativeFenceCb else {
        return Err(E_FAIL);
    };
    let Some(args) = (unsafe { open_args.as_mut() }) else {
        return Err(E_INVALIDARG);
    };
    // Core-0116 shared open is one-node, shared NT-security only. Cross-adapter
    // and every reserved byte are a named refusal, never a fallback.
    // SAFETY: `Value` is the UINT view of the flags union.
    let flags = unsafe { args.Flags.__bindgen_anon_1.Value };
    if args.hNtHandle.is_null()
        || args.EngineAffinity != 1
        || flags != 0b11
        || !all_zero(&args.Reserved)
    {
        return Err(E_INVALIDARG);
    }
    let mut input = helios_protocol::HeliosNativeFencePddV1::create_input(
        helios_protocol::HELIOS_PACKAGE_GENERATION,
        helios_protocol::HELIOS_NATIVE_FENCE_TYPE_DEFAULT,
        helios_protocol::HELIOS_HNF1_FLAG_SHARED,
    );
    input.adapter_luid = dev.adapter_luid;
    args.PrivateDriverData = input.to_bytes();

    let hr = unsafe { open(dev.h_rt_device, h_rt_fence, args) };
    if hr < 0 {
        return Err(hr);
    }
    let pdd = helios_protocol::HeliosNativeFencePddV1::from_bytes(&args.PrivateDriverData);
    pdd.validate_returned(
        helios_protocol::HELIOS_PACKAGE_GENERATION,
        dev.adapter_luid,
        helios_protocol::HELIOS_NATIVE_FENCE_TYPE_DEFAULT,
        helios_protocol::HELIOS_HNF1_FLAG_SHARED,
    )
    .map_err(|_| E_FAIL)?;
    if args.hSyncObject == 0
        || args.EngineAffinity != 1
        || !mapping_is_valid(&args.NativeFenceMapping)
        || !all_zero(&args.Reserved)
    {
        return Err(E_FAIL);
    }
    Ok(FenceState {
        h_device,
        h_rt_fence,
        h_sync_object: args.hSyncObject,
        mapping: args.NativeFenceMapping,
        pdd,
        opened: true,
    })
}

unsafe extern "C" fn create_fence(
    h_device: ddi12::D3D12DDI_HDEVICE,
    h_fence: ddi12::D3D12DDI_HFENCE,
    h_rt_fence: ddi12::D3D12DDI_HRTFENCE,
    arg: *const ddi12::D3D12DDIARG_CREATE_FENCE_0116,
) -> ddi12::HRESULT {
    // SAFETY: the caller guarantees the slot lies in the sized private block.
    let Some(slot) = (unsafe { fence_slot(h_fence) }) else {
        note_refusal(&L7_REFUSALS.fence_bad_arg);
        return E_INVALIDARG;
    };
    // ⛔ Clear first, so every refusal below leaves a null slot rather than
    // whatever the runtime's allocator left there. `destroy_fence` then finds
    // `None` and reports nothing to release.
    // SAFETY: as above.
    unsafe { slot.clear() };

    if arg.is_null() {
        note_refusal(&L7_REFUSALS.fence_bad_arg);
        return E_INVALIDARG;
    }
    // SAFETY: non-null per the check; the DDI declares it `_In_ CONST`.
    let a = unsafe { &*arg };
    let Some(dev) = (unsafe { device12::device(h_device) }) else {
        note_refusal(&L7_REFUSALS.fence_no_device);
        return E_FAIL;
    };
    if h_rt_fence.handle.is_null() {
        note_refusal(&L7_REFUSALS.fence_bad_arg);
        return E_INVALIDARG;
    }

    let created = match a.FenceType {
        ddi12::D3D12DDI_FENCE_TYPE_0112_D3D12DDI_FENCE_TYPE_NATIVE => unsafe {
            create_native_fence(
                dev,
                h_device,
                h_rt_fence,
                a.__bindgen_anon_1.pNativeFenceArgs,
            )
        },
        ddi12::D3D12DDI_FENCE_TYPE_0112_D3D12DDI_FENCE_TYPE_OPENED_NATIVE => unsafe {
            open_native_fence(
                dev,
                h_device,
                h_rt_fence,
                a.__bindgen_anon_1.pNativeFenceOpenArgs,
            )
        },
        ddi12::D3D12DDI_FENCE_TYPE_0112_D3D12DDI_FENCE_TYPE_MONITORED => {
            note_refusal(&L7_REFUSALS.fence_monitored_refused);
            Err(E_INVALIDARG)
        }
        _ => {
            note_refusal(&L7_REFUSALS.fence_type_unknown);
            Err(E_INVALIDARG)
        }
    };
    match created {
        Ok(state) => {
            if let Some(n) = budget(&FENCE_LOG) {
                log_error!(
                    "CreateFence0116: type={} hRTFence={:p} hSyncObject={} generation={} opened={} (x{})",
                    a.FenceType,
                    state.h_rt_fence.handle,
                    state.h_sync_object,
                    state.pdd.object_generation,
                    state.opened,
                    n + 1,
                );
            }
            unsafe { slot.store(state) };
            S_OK
        }
        Err(hr) => {
            note_refusal(&L7_REFUSALS.fence_native_callback_failed);
            hr
        }
    }
}

/// `pfnDestroyFence`.
///
/// Returns `VOID`; the counter is the only channel, which is why a destroy on an
/// empty slot is counted rather than ignored.
///
/// # Safety
/// `h_fence` must be a handle [`create_fence`] returned `S_OK` for and which has
/// not already been destroyed.
unsafe extern "C" fn destroy_fence(
    h_device: ddi12::D3D12DDI_HDEVICE,
    h_fence: ddi12::D3D12DDI_HFENCE,
) {
    // SAFETY: the caller guarantees a live handle from `create_fence`.
    let Some(slot) = (unsafe { fence_slot(h_fence) }) else {
        note_refusal(&L7_REFUSALS.fence_bad_arg);
        return;
    };
    // SAFETY: the slot holds either null or the one box `create_fence` moved in;
    // `take` empties it, so a destroy after a refused create — or a second
    // destroy — is a no-op rather than a double free. Dropping the box releases
    // the engine fence's single reference.
    let Some(state) = (unsafe { slot.take() }) else {
        note_refusal(&L7_REFUSALS.fence_bad_arg);
        return;
    };
    if !state.belongs_to(h_device)
        || !mapping_is_valid(&state.mapping)
        || unsafe { device12::device(h_device) }.is_none_or(|dev| {
            state
                .pdd
                .validate_returned(
                    helios_protocol::HELIOS_PACKAGE_GENERATION,
                    dev.adapter_luid,
                    helios_protocol::HELIOS_NATIVE_FENCE_TYPE_DEFAULT,
                    state.pdd.flags,
                )
                .is_err()
        })
    {
        note_refusal(&L7_REFUSALS.fence_destroy_invalid);
        if let Some(dev) = unsafe { device12::device(h_device) } {
            let _ = device12::set_error(dev, E_FAIL);
        }
    }
    drop(state);
}

/// The exact Core-0116 create slot installed below. The native-fence cap reads
/// this same typed value, so capability publication and table wiring cannot
/// drift into separate hand-maintained booleans.
pub(crate) const NATIVE_FENCE_CREATE_HANDLER: ddi12::PFND3D12DDI_CREATEFENCE_0116 =
    Some(create_fence);

// ---------------------------------------------------------------------------
// (j) Query heaps — 3 slots
// ---------------------------------------------------------------------------

/// Translate a `D3D12DDI_QUERY_HEAP_TYPE` into the API enum vkd3d takes.
///
/// ⛔ **Translated, never forwarded.** `DDI_REFERENCE.md` §9.6.1 records what
/// passing a DDI enum straight into an API call cost on the descriptor-heap
/// path: the two flag enums collide on `0x1` with different meanings, so the
/// compiler cannot catch it and the result is the wrong heap with no error. The
/// two query-heap enums *do* agree numerically at this SDK — every arm below is
/// an identity — and writing the match out is what turns that into something the
/// compiler re-checks when either header moves, instead of an assumption nobody
/// can see.
fn engine_query_heap_type(t: ddi12::D3D12DDI_QUERY_HEAP_TYPE) -> Option<D3D12_QUERY_HEAP_TYPE> {
    use ddi12::{
        D3D12DDI_QUERY_HEAP_TYPE_D3D12DDI_QUERY_HEAP_TYPE_0020_VIDEO_DECODE_STATISTICS as DDI_VIDEO_DECODE,
        D3D12DDI_QUERY_HEAP_TYPE_D3D12DDI_QUERY_HEAP_TYPE_0032_COPY_QUEUE_TIMESTAMP as DDI_COPY_TIMESTAMP,
        D3D12DDI_QUERY_HEAP_TYPE_D3D12DDI_QUERY_HEAP_TYPE_OCCLUSION as DDI_OCCLUSION,
        D3D12DDI_QUERY_HEAP_TYPE_D3D12DDI_QUERY_HEAP_TYPE_PIPELINE_STATISTICS as DDI_PIPELINE_STATS,
        D3D12DDI_QUERY_HEAP_TYPE_D3D12DDI_QUERY_HEAP_TYPE_PIPELINE_STATISTICS1 as DDI_PIPELINE_STATS1,
        D3D12DDI_QUERY_HEAP_TYPE_D3D12DDI_QUERY_HEAP_TYPE_SO_STATISTICS as DDI_SO_STATS,
        D3D12DDI_QUERY_HEAP_TYPE_D3D12DDI_QUERY_HEAP_TYPE_TIMESTAMP as DDI_TIMESTAMP,
    };
    match t {
        DDI_OCCLUSION => Some(D3D12_QUERY_HEAP_TYPE_OCCLUSION),
        DDI_TIMESTAMP => Some(D3D12_QUERY_HEAP_TYPE_TIMESTAMP),
        DDI_PIPELINE_STATS => Some(D3D12_QUERY_HEAP_TYPE_PIPELINE_STATISTICS),
        DDI_SO_STATS => Some(D3D12_QUERY_HEAP_TYPE_SO_STATISTICS),
        DDI_VIDEO_DECODE => Some(D3D12_QUERY_HEAP_TYPE_VIDEO_DECODE_STATISTICS),
        DDI_COPY_TIMESTAMP => Some(D3D12_QUERY_HEAP_TYPE_COPY_QUEUE_TIMESTAMP),
        DDI_PIPELINE_STATS1 => Some(D3D12_QUERY_HEAP_TYPE_PIPELINE_STATISTICS1),
        // ⚠ Not an `else that fills the largest arm` (`DECISIONS.md` §7.4): a
        // type this header does not name is refused, never guessed at.
        _ => None,
    }
}

/// `pfnCalcPrivateQueryHeapSize`.
///
/// # Safety
/// `arg`, when non-null, must point at a live
/// `D3D12DDIARG_CREATE_QUERY_HEAP_0001` for the duration of the call.
unsafe extern "C" fn calc_private_query_heap_size(
    _h_device: ddi12::D3D12DDI_HDEVICE,
    arg: *const ddi12::D3D12DDIARG_CREATE_QUERY_HEAP_0001,
) -> ddi12::SIZE_T {
    if arg.is_null() {
        note_refusal(&L7_REFUSALS.query_heap_bad_arg);
    }
    // ⛔ Answered unconditionally — see `PRIVATE_SLOT_SIZE`.
    PRIVATE_SLOT_SIZE as ddi12::SIZE_T
}

/// `pfnCreateQueryHeap`.
///
/// # Safety
/// `h_device` must be a live handle from `device12::create_device`; `arg` must
/// point at a live `D3D12DDIARG_CREATE_QUERY_HEAP_0001`; `h_heap`'s
/// `pDrvPrivate` must address the private block
/// [`calc_private_query_heap_size`] sized.
unsafe extern "C" fn create_query_heap(
    h_device: ddi12::D3D12DDI_HDEVICE,
    arg: *const ddi12::D3D12DDIARG_CREATE_QUERY_HEAP_0001,
    h_heap: ddi12::D3D12DDI_HQUERYHEAP,
) -> ddi12::HRESULT {
    // SAFETY: the caller guarantees the slot lies in the sized private block.
    let Some(slot) = (unsafe { query_heap_slot(h_heap) }) else {
        note_refusal(&L7_REFUSALS.query_heap_bad_arg);
        return E_INVALIDARG;
    };
    // ⛔ Clear first, for the same reason as `create_fence`.
    // SAFETY: as above.
    unsafe { slot.clear() };

    if arg.is_null() {
        note_refusal(&L7_REFUSALS.query_heap_bad_arg);
        return E_INVALIDARG;
    }
    // SAFETY: non-null per the check; the DDI declares it `_In_ CONST`.
    let a = unsafe { &*arg };

    let Some(heap_type) = engine_query_heap_type(a.Type) else {
        note_refusal(&L7_REFUSALS.query_heap_type_unsupported);
        if let Some(n) = budget(&QUERY_HEAP_LOG) {
            log_error!(
                "CreateQueryHeap: D3D12DDI_QUERY_HEAP_TYPE {} is not named by this header -> \
                 E_INVALIDARG (x{})",
                a.Type,
                n + 1,
            );
        }
        return E_INVALIDARG;
    };

    // SAFETY: device-scope DDI; the borrow lives only until the end of the call.
    let Some(dev) = (unsafe { device12::device(h_device) }) else {
        note_refusal(&L7_REFUSALS.query_heap_no_device);
        return E_FAIL;
    };
    let Some(engine) = dev.engine.d3d12_device() else {
        note_refusal(&L7_REFUSALS.query_heap_no_device);
        return E_FAIL;
    };

    // ⚠ `NodeMask` is passed through rather than forced to 0. Helios advertises
    // one node, so the only legal values the runtime can send are 0 and 1, and
    // both mean "the single node" to vkd3d — narrowing it here would hide a
    // multi-node request instead of letting the engine reject it.
    let desc = D3D12_QUERY_HEAP_DESC {
        Type: heap_type,
        Count: a.Count,
        NodeMask: a.NodeMask,
    };
    let mut heap: Option<ID3D12QueryHeap> = None;
    // SAFETY: `desc` is a live local for the call and `heap` is a writable
    // `Option<ID3D12QueryHeap>` the wrapper initialises on success.
    if let Err(e) = unsafe { engine.CreateQueryHeap(&desc, &mut heap) } {
        note_refusal(&L7_REFUSALS.query_heap_engine_failed);
        if let Some(n) = budget(&QUERY_HEAP_LOG) {
            log_error!(
                "CreateQueryHeap: type={} count={} engine failed hr={:#010x} (x{})",
                a.Type,
                a.Count,
                e.code().0 as u32,
                n + 1,
            );
        }
        return E_FAIL;
    }
    let Some(heap) = heap else {
        // ⚠ `S_OK` with no object out. Counted rather than assumed impossible:
        // the wrapper's out-param is only written on success, so this would mean
        // the engine broke its own COM contract.
        note_refusal(&L7_REFUSALS.query_heap_engine_failed);
        return E_FAIL;
    };

    // SAFETY: the slot lies in the sized private block and is currently null;
    // `store` moves the single reference the engine returned into it.
    unsafe { slot.store(heap) };
    S_OK
}

/// `pfnDestroyQueryHeap`.
///
/// # Safety
/// `h_heap` must be a handle [`create_query_heap`] returned `S_OK` for and which
/// has not already been destroyed.
unsafe extern "C" fn destroy_query_heap(
    _h_device: ddi12::D3D12DDI_HDEVICE,
    h_heap: ddi12::D3D12DDI_HQUERYHEAP,
) {
    // SAFETY: the caller guarantees a live handle from `create_query_heap`.
    let Some(slot) = (unsafe { query_heap_slot(h_heap) }) else {
        note_refusal(&L7_REFUSALS.query_heap_bad_arg);
        return;
    };
    // SAFETY: the slot holds either null or the one `ID3D12QueryHeap` reference
    // `create_query_heap` moved in; `release` is idempotent on null.
    unsafe { slot.release() };
}

/// Install L7's 6 device-core slots.
///
/// Chain position: `ShaderSlots` -> `FenceSlots` on the device-core table.
pub(crate) fn install(
    mut filling: Filling<'_, DeviceCoreTable, stage::ShaderSlots>,
) -> Filling<'_, DeviceCoreTable, stage::FenceSlots> {
    let table = filling.table();
    // (i) fences — 3
    table.pfnCalcPrivateFenceSize = Some(calc_private_fence_size);
    table.pfnCreateFence = NATIVE_FENCE_CREATE_HANDLER;
    table.pfnDestroyFence = Some(destroy_fence);
    // (j) query heaps — 3
    table.pfnCalcPrivateQueryHeapSize = Some(calc_private_query_heap_size);
    table.pfnCreateQueryHeap = Some(create_query_heap);
    table.pfnDestroyQueryHeap = Some(destroy_query_heap);
    filling.advance()
}

// ---------------------------------------------------------------------------
// Refusal counters
// ---------------------------------------------------------------------------

/// L7's refusal counters. One instance, [`L7_REFUSALS`]; the set that prints
/// them is [`REFUSALS`].
pub(crate) struct L7Refusals {
    /// `pfnCalcPrivateFenceSize` / `pfnCreateFence` / `pfnDestroyFence` with a
    /// null arg or a null `pDrvPrivate`. **Expected 0** — the DDI declares every
    /// one of them non-optional.
    fence_bad_arg: RefusalCounter,
    /// A fence slot could not reach the engine: the `hDevice` did not resolve, or
    /// the bridge carries no `ID3D12Device`. **Expected 0** — these are
    /// device-scope DDIs and a device exists by construction.
    fence_no_device: RefusalCounter,
    fence_monitored_refused: RefusalCounter,
    fence_type_unknown: RefusalCounter,
    fence_native_callback_failed: RefusalCounter,
    fence_destroy_invalid: RefusalCounter,
    /// `D3D12DDIARG_CREATE_FENCE` asked for `FenceCount != 1` (or a null array),
    /// and the create was refused.
    ///
    /// ⛔ **Expected 0 on this guest.** `FenceCount > 1` is the multi-adapter
    /// (LDA) case, one placement per physical adapter (`DDI_REFERENCE.md`
    /// §10.1), and Helios reports one implicit physical adapter. A hit means the
    /// multi-adapter assumption behind `ARCHITECTURE.md` §13 UNVERIFIED-11 has
    /// been reached for real, and a single engine fence cannot honour it.
    fence_multi_adapter_refused: RefusalCounter,
    /// A fence carried `D3D12DDI_FENCE_FLAG_BOTTOM_OF_PIPE`, which this driver
    /// backs **only inside the engine**.
    ///
    /// ⚠ **Expected non-zero, and it is a coupling rather than a fault.**
    /// `ID3D12CommandQueue::Signal` on the vkd3d queue does retire behind that
    /// queue's submitted work, so the engine half is honoured.
    ///
    /// ⛔ **The WDDM half this doc used to name — *"the queued software signal
    /// packet"* on this fence — CANNOT EXIST.** `DDI_REFERENCE.md` §10.4's
    /// correction block (`:2306-2331`) struck it, because every `pfnSignal*Cb`
    /// names its target by `D3DKMT_HANDLE` and `D3D12DDIARG_CREATE_FENCE` carries
    /// none; `KMD_IMPACT.md` §14a.5 forbids the design by name. The real WDDM half
    /// is the `pfnRenderCb` packet `pfnExecuteCommandLists` submits with the
    /// frame's own GPU-completion boundary (`EclFenceSampled`).
    ///
    /// ⛔ **GRADING CORRECTED 2026-08-07.** This said the boundary is *"off by
    /// default under A1 (`knobs12::UMD12_ECL_DRAIN`)"*, which has been false since
    /// `f71fef4`. Reading it that way inverts the conclusion on a default build:
    /// `Umd12EclFence` is **ON**, so `EclFenceSampled` is nonzero on every ECL,
    /// and someone told to expect 0 would read a flat fence measurement as
    /// unattributable when in fact a boundary was carried.
    ///
    /// ⇒ read this counter beside **`EclFenceNoDrain`**, not beside
    /// `EclFenceSampled`. `EclFenceSampled` only says a boundary was sampled;
    /// `EclFenceNoDrain` is what distinguishes an EXACT boundary from a **prefix**
    /// that may name less work than the frame contains — which is the distinction
    /// a bottom-of-pipe claim actually turns on.
    fence_bottom_of_pipe_unproven: RefusalCounter,
    /// A `D3D12DDI_FENCE::Flags` carried a bit outside the two enumerators
    /// `d3d12umddi.h` defines. **Expected 0**; a hit means the header this build
    /// was generated from is older than the runtime asking.
    fence_flags_unknown: RefusalCounter,
    /// `ID3D12Device::CreateFence` on the engine failed. **Expected 0** — it
    /// allocates a Vulkan timeline semaphore and nothing else.
    fence_engine_failed: RefusalCounter,
    /// A query-heap slot was called with a null arg or a null `pDrvPrivate`.
    /// **Expected 0.**
    query_heap_bad_arg: RefusalCounter,
    /// A query-heap slot could not reach the engine. **Expected 0**, same
    /// reasoning as `FenceNoDevice`.
    query_heap_no_device: RefusalCounter,
    /// `D3D12DDIARG_CREATE_QUERY_HEAP_0001::Type` was a value this build's
    /// `d3d12umddi.h` does not name, so it was refused rather than guessed at.
    /// **Expected 0.**
    query_heap_type_unsupported: RefusalCounter,
    /// `ID3D12Device::CreateQueryHeap` on the engine failed, or returned `S_OK`
    /// with no object.
    ///
    /// ⚠ **May legitimately be non-zero**: vkd3d refuses the query-heap types it
    /// has no Vulkan query pool for (the video-decode-statistics family), and
    /// this driver forwards every type the header names rather than pre-filtering
    /// on a capability it has not measured.
    query_heap_engine_failed: RefusalCounter,
}

pub(crate) static L7_REFUSALS: L7Refusals = L7Refusals {
    fence_bad_arg: RefusalCounter::new("FenceBadArg"),
    fence_no_device: RefusalCounter::new("FenceNoDevice"),
    fence_monitored_refused: RefusalCounter::new("FenceMonitoredRefused"),
    fence_type_unknown: RefusalCounter::new("FenceTypeUnknown"),
    fence_native_callback_failed: RefusalCounter::new("FenceNativeCallbackFailed"),
    fence_destroy_invalid: RefusalCounter::new("FenceDestroyInvalid"),
    fence_multi_adapter_refused: RefusalCounter::new("FenceMultiAdapterRefused"),
    fence_bottom_of_pipe_unproven: RefusalCounter::new("FenceBottomOfPipeUnproven"),
    fence_flags_unknown: RefusalCounter::new("FenceFlagsUnknown"),
    fence_engine_failed: RefusalCounter::new("FenceEngineFailed"),
    query_heap_bad_arg: RefusalCounter::new("QueryHeapBadArg"),
    query_heap_no_device: RefusalCounter::new("QueryHeapNoDevice"),
    query_heap_type_unsupported: RefusalCounter::new("QueryHeapTypeUnsupported"),
    query_heap_engine_failed: RefusalCounter::new("QueryHeapEngineFailed"),
};

/// L7's refusal counters, printed by `crate::log_refusal_summary` at this
/// lane's position in `lib.rs`'s `UMD12_REFUSAL_SETS`.
///
/// ⭐ **Declared here rather than in `lib.rs` so this lane's diff against the
/// crate root is empty.** Every one of the eleven S6 lanes needs counters
/// (`PARALLEL.md` §9.1: *every skipped or refused path gets a named counter*),
/// and one flat array in `lib.rs` would have been the split's hottest merge
/// point — §5's shared-file table does not even list `lib.rs`. Same move
/// `forward12::tables12` makes for the 206 slots: name all eleven up front and
/// the lanes become substitutive instead of additive.
///
/// ⛔ **Append only.** Counter order inside a set, and set order in
/// `UMD12_REFUSAL_SETS`, are both the evidence contract: `D3D12 DDI refusals:`
/// lines get diffed across builds.
pub(crate) static REFUSALS: &[&RefusalCounter] = &[
    &L7_REFUSALS.fence_bad_arg,
    &L7_REFUSALS.fence_no_device,
    &L7_REFUSALS.fence_monitored_refused,
    &L7_REFUSALS.fence_type_unknown,
    &L7_REFUSALS.fence_native_callback_failed,
    &L7_REFUSALS.fence_destroy_invalid,
    &L7_REFUSALS.fence_multi_adapter_refused,
    &L7_REFUSALS.fence_bottom_of_pipe_unproven,
    &L7_REFUSALS.fence_flags_unknown,
    &L7_REFUSALS.fence_engine_failed,
    &L7_REFUSALS.query_heap_bad_arg,
    &L7_REFUSALS.query_heap_no_device,
    &L7_REFUSALS.query_heap_type_unsupported,
    &L7_REFUSALS.query_heap_engine_failed,
];

// ⚠ `Hresult` is imported for the `E_*`/`S_OK` constants this file returns; the
// DDI's own `HRESULT` (bindgen's `c_long`) is the declared return type and the
// two are the same `i32`. Naming both keeps `umd_common::hr`'s constants usable
// without a cast at forty return sites (`umd_common/src/hr.rs:31-34`).
const _: () = assert!(core::mem::size_of::<Hresult>() == core::mem::size_of::<ddi12::HRESULT>());
