//! L8 — present.
//!
//! Owns 3 slots: `pfnGetPresentPrivateDriverDataSize` on the device-core table
//! and `pfnBlt` / `pfnPresent` on the command-list table.
//!
//! ⛔ **Not parallelisable, and it lands after L3a/L3b, not beside them**
//! (`PARALLEL.md` §8): it used to touch the `HeliosPresentRenderCmd` identity
//! channel shared with the KMD **and** with the D3D11 driver.
//!
//! ⚠⚠ **That channel is RETIRED here, and the reversal is recorded rather than
//! quietly edited.** `DECISIONS.md` D13 made the record `helios_protocol`'s
//! (`HeliosPresentRenderCmd`, `HeliosPresentPrivateData`), reused verbatim so the
//! KMD had one spelling to decode. Its load-bearing field is
//! `HeliosPresentPrivateData::resource_id` — the back buffer's **host** venus
//! resource id — and `HELIOS_PRESENT_SYNC_RETIREMENT.md` §10.3 forbids any UMD,
//! ICD, batch or private descriptor naming or supplying one
//! (`docs/retirement/K4-CONTRACT.md` §5). ⇒ **this driver builds and submits no
//! identity record at all**; it counts `PresentIdentityNoResourceId` naming mesa
//! lane unit **A3**, and the present proceeds. See [`present`] for the severity
//! argument.
//!
//! ⚠ Present private data **never reaches `DxgkDdiPresent` on DMA flips** — it
//! rides the Render command (64th session, permanent). And ⛔ never reintroduce
//! a producer-side CPU present gate: owner directive, 2026-07-29
//! (`ARCHITECTURE.md` §12 rule 13, `DECISIONS.md` §7.9).
//!
//! ⚠ There is **no DXGI table** to fill: `D3D12DDI_TABLE_TYPE_DXGI` was never
//! requested across 20 flip-model presents, and present arrives on the
//! command-list table (`D12-G5`).
//!
//! # ⭐ What `pfnPresent` actually is, and why it needs no KMD work
//!
//! `PFND3D12DDI_PRESENT_0051` **outputs** handles. Its primary job is to name the
//! kernel objects the runtime should then hand `D3DKMTPresent` — the back buffer's
//! `D3DKMT_HANDLE`, the WDDM context to submit on, and a few scalars — and nothing
//! here touches the scanout.
//!
//! ⚠ **This block has now been right, wrong and right again, and all three states
//! are recorded.** It first said *"it is not a submission … nothing here builds a
//! packet"*; UP-9 made that false by adding the identity `pfnRenderCb`; the HPS2
//! retirement makes it true once more, because the record that submission carried
//! may no longer be built. ⇒ `pfnPresent` is once again an out-parameter DDI and
//! nothing else, and the kernel learns no identity for a D3D12 frame until mesa lane
//! unit A3 lands.
//!
//! ⭐ **A windowed DWM-composited present never reaches `DxgkDdiPresent` at all**,
//! which is measured rather than assumed: `PRESENT_FLAGS_HISTOGRAM`
//! (`kmd_render/src/ddi/scanout_trace.rs`) is unsampled and non-overflowing, only
//! `0x1` and `0xC` have ever arrived, and `RedirectedFlip` has zero occurrences in
//! `kmd_render`. So the windowed path this lane targets needs **no**
//! `DIRECT_SCANOUT` bit, no host stride agreement and none of the 0ab machinery.
//! Fullscreen flip is a later, separate piece of work.
//!
//! # ⭐ The two seams into `forward12::queue.rs`, and why they are two
//!
//! This lane needs two things from the queue and neither of them is a
//! `&QueueState`:
//!
//! 1. [`queue::present_context`] — the WDDM context
//!    `D3D12DDI_PRESENT_CONTEXTS_0051::hContext` must name, i.e. the one
//!    `pfnCreateCommandQueue` minted (UP-7).
//! 2. ⛔ **RETIRED** — [`queue::submit_present_identity`], the `pfnRenderCb`
//!    submission carrying this frame's `HeliosPresentRenderCmd` (UP-9). That record's
//!    load-bearing field is a **host** venus resource id, which §10.3 forbids any UMD
//!    supplying; see the refusal in [`present`] and `PresentIdentityNoResourceId`.
//!    ⚠ The accessor itself is left standing in `queue.rs` for the present-ticket
//!    retirement unit to remove together with the record it takes — it is not K4's.
//!
//! ⛔ **Both are handle-taking free functions rather than accessors on the
//! state**, because `QueueState` carries invariants that are `queue.rs`'s to
//! keep: the window guard spans write → `pfnRenderCb` → re-latch, the three
//! runtime windows rotate under it, and the submission must happen on the thread
//! inside the owning DDI. A borrow handed across the module boundary would export
//! all three. Each accessor's own doc has the argument.
//!
//! ⚠ **The order below is load-bearing and it is not the order the fields are
//! declared in.** The context is resolved first and the out-structs are filled
//! last — so a refusal at any step leaves the all-zero descriptor written at the
//! top of [`present`] standing, which is unmistakably *"the driver answered
//! nothing"* rather than a half-filled answer.

use helios_umd_common::refusals::RefusalCounter;

use windows::core::Interface;

use super::tables12::{stage, Filling};
use super::tables12::{CommandListTable, DeviceCoreTable};
use super::{queue, resource12};
use crate::{ddi12, log_error, note_refusal};

/// How many times any one bounded evidence line may repeat, per site.
///
/// Same idiom and same reason as `resource12`'s: the counter is unbounded, the log
/// line is not. Present is a per-frame path, so an unbounded logger here would be
/// the T2 measurement all over again at 200 Hz.
const LOG_BUDGET: usize = 32;

/// `pfnGetPresentPrivateDriverDataSize` — **0 bytes, and that is the answer rather
/// than a placeholder.**
///
/// The runtime asks how many bytes of driver-private data to carry alongside a
/// present. This driver needs none, and the reason is a measurement this project
/// paid for twice:
///
/// ⛔ **Present private data never reaches `DxgkDdiPresent` on a DMA flip** (64th
/// session, recorded as PERMANENT in `DECISIONS.md`'s D4b chain). So a private-data
/// trailer was never the identity channel, and asking the runtime to allocate one
/// nothing reads would be bytes per frame in exchange for nothing. ⚠ The channel
/// that *was* used instead — `HeliosPresentRenderCmd` on `pfnRenderCb` — is retired
/// with the host resource id it carried; see the module doc. Neither answer changes:
/// this is still 0.
///
/// ⚠ `KMD_IMPACT.md` §14a.3 UP-8 describes *"0, with a 72-byte arm behind a knob for
/// U6's arrival half"* — U6 being the open question of whether the runtime ever
/// delivers that trailer to the kernel. The knob arm is **not** implemented here:
/// the knobs live in `knobs12.rs`, which this lane does not own, and a knob whose ON
/// arm nothing consumes would be a configuration with no stated meaning. The census
/// counter below is what says how often the runtime asks.
///
/// # Safety
/// `_p_present` is not dereferenced. Declared `unsafe` because the DDI's PFN typedef
/// is.
unsafe extern "C" fn get_present_private_driver_data_size(
    _h_device: ddi12::D3D12DDI_HDEVICE,
    _p_present: *const ddi12::D3D12DDIARG_PRESENT_0001,
) -> ddi12::UINT {
    L8_REFUSALS.present_private_data_size_queries.bump();
    0
}

/// `pfnPresent` — name the kernel objects the runtime should present with.
///
/// # ⭐ It is an OUT-parameter DDI, and that is the whole shape of it
///
/// `D12-G5` established the argument list; the four out-structs are
/// `D3D12DDI_PRESENT_0051` (the allocations and the scalars),
/// `D3D12DDI_PRESENT_CONTEXTS_0051` (the WDDM context) and
/// `D3D12DDI_PRESENT_HWQUEUES_0051` (hardware queues, which this driver has none of
/// — `pfnCreateHwQueue` is refused with `HwQRef`). ⚠ Nothing is submitted from here
/// any more: UP-9's identity `pfnRenderCb` is retired with the record it carried
/// (module doc), so this DDI is its out-parameters and nothing else.
///
/// The values this driver answers with, each with its reason:
///
/// | field | value | why |
/// |---|---|---|
/// | `BroadcastSrcAllocation[0]` | the back buffer's `D3DKMT_HANDLE` | the create-time HWA2 allocation, out of the `identity12` table |
/// | `BroadcastDstAllocation[0]` | 0 | there is no destination allocation: a windowed present's destination is DWM's, named by the runtime and not by this driver |
/// | `AddedGpuWork` | `FALSE` | ⛔ nothing is recorded into `hCommandList`. The UP-9 `pfnRenderCb` below is a **kernel** submission already in dxgkrnl's FIFO for the same context, not pending command-list work |
/// | `BackBufferMultiplicity` | 1 | ⛔ an **output**, not a request: the D3D12 in-struct has no such field, so there is nothing to honour, refuse or count. See the field |
/// | `SyncIntervalOverrideValid` | `FALSE` | the driver does not override the application's sync interval; `SyncIntervalOverride` is therefore left alone |
/// | `_CONTEXTS.hContext` | the queue's WDDM context | [`queue::present_context`], the first of the module doc's two seams |
/// | `_CONTEXTS.BroadcastContextCount` | 0 | one adapter, one context; broadcast is a multi-GPU shape |
/// | `_HWQUEUES.BroadcastQueueCount` | 0 | this driver refuses hardware queues |
///
/// # ⛔ Two runtime-shape hazards, both measured rather than reasoned
///
/// 1. **`pContexts` and `pHwQueues` are `_Out_opt_`.** `D12-G5` saw `pHwQ`
///    **non-NULL at `_0040` and NULL at `_0110`**, so a driver that writes through
///    them unconditionally faults on one generation of the runtime and not the
///    other. Every write below is behind its own null check.
/// 2. **`hDstResource` was measured NULL only on the WARP control arm**, which has
///    no display adapter — so its NULL-ness is *not* established for this driver,
///    and a non-NULL value would mean the runtime is asking for a
///    resource-to-resource present this lane does not implement. It is refused
///    loudly with its own counter rather than ignored, because ignoring it would
///    present the back buffer to the wrong destination.
///
/// # Safety
/// `p_present`, when non-null, must be a live `D3D12DDIARG_PRESENT_0001` with
/// `SurfacesToPresent` valid entries at `phSurfacesToPresent`. The three out-structs
/// must be live for the call when non-null. `h_queue`'s private block, when
/// non-null, must be one `pfnCreateCommandQueue` wrote.
unsafe extern "C" fn present(
    _h_command_list: ddi12::D3D12DDI_HCOMMANDLIST,
    h_queue: ddi12::D3D12DDI_HCOMMANDQUEUE,
    p_present: *const ddi12::D3D12DDIARG_PRESENT_0001,
    p_out: *mut ddi12::D3D12DDI_PRESENT_0051,
    p_contexts: *mut ddi12::D3D12DDI_PRESENT_CONTEXTS_0051,
    p_hw_queues: *mut ddi12::D3D12DDI_PRESENT_HWQUEUES_0051,
) {
    // ⭐ The DENOMINATOR, counted as the first statement so it counts *entries* and
    // not successes — the same construction and the same reason as
    // `FenceSignalEntered`. Without it `PresentIdentitySubmitted` and the four L8
    // refusals below are a partition with no left-hand side, and a run could not be
    // checked as arithmetic. ⚠ `bump`, not `note_refusal`: this is a per-frame census
    // rather than a refusal, and R911 forbids an already-loud-or-benign arm emitting
    // the whole ~300-counter summary. `log_refusal_summary` prints the set at adapter
    // close and device teardown, so it is still readable.
    L8_REFUSALS.present_entered.bump();

    // ⛔ The out-structs are cleared FIRST, every one that exists, before anything
    // that can refuse. The runtime allocated them and this driver does not know what
    // it left in them; a refusal that returned early without zeroing would hand back
    // whatever was there — and `BroadcastSrcAllocation` is 65 `D3DKMT_HANDLE`s, so
    // "whatever was there" is 65 chances to name a kernel object at random.
    // SAFETY: each pointer is either null — `as_mut` then yields `None` and nothing
    // is written — or a live out-struct the runtime allocated for this call, per the
    // caller's guarantee. `Default` for each of the three is bindgen's `write_bytes`
    // zero fill, so the whole struct is initialised by the assignment and no field is
    // left holding what the runtime had there.
    unsafe {
        if let Some(out) = p_out.as_mut() {
            *out = ddi12::D3D12DDI_PRESENT_0051::default();
        }
        if let Some(contexts) = p_contexts.as_mut() {
            *contexts = ddi12::D3D12DDI_PRESENT_CONTEXTS_0051::default();
        }
        if let Some(hw_queues) = p_hw_queues.as_mut() {
            *hw_queues = ddi12::D3D12DDI_PRESENT_HWQUEUES_0051::default();
        }
    }

    // SAFETY: `_In_ CONST` and live for the call when non-null, per the caller.
    let Some(arg) = (unsafe { p_present.as_ref() }) else {
        note_refusal(&L8_REFUSALS.present_bad_arg);
        return;
    };
    if p_out.is_null() {
        // The allocations out-struct is the one output the runtime cannot proceed
        // without, so its absence is a distinct fault from a null argument struct.
        // ⚠ Tested for nullness here and bound again at the tail rather than held
        // as a `&mut` across the whole body: everything between the two is a
        // refusal path or a call into another module, and a live `&mut` into the
        // runtime's out-struct held across those would be an aliasing claim this
        // function cannot make.
        note_refusal(&L8_REFUSALS.present_no_out_struct);
        return;
    }

    // ⛔ A destination resource means a present this lane does not implement. Loud,
    // with its own counter: the measured NULL came from WARP, which has no display
    // adapter, so nothing establishes that this driver will only ever see NULL.
    if !arg.hDstResource.pDrvPrivate.is_null() {
        note_refusal(&L8_REFUSALS.present_dst_resource_refused);
        let n = L8_REFUSALS.present_dst_resource_refused.get();
        if n <= LOG_BUDGET {
            log_error!(
                "L8: pfnPresent with hDstResource={:p} subresource={} -- a resource-to-resource \
                 present is not implemented; refusing rather than presenting the back buffer to \
                 the wrong destination (flags={:#x} vidpn={} surfaces={}) (x{n})",
                arg.hDstResource.pDrvPrivate,
                arg.DstSubResourceIndex,
                // SAFETY: the union's `Value` arm is a plain `UINT` overlaying the
                // bitfield; reading it is defined for any initialised bit pattern.
                unsafe { arg.Flags.__bindgen_anon_1.Value },
                arg.VidPnSourceID,
                arg.SurfacesToPresent,
            );
        }
        return;
    }

    // The source surface. ⚠ Exactly one is read: `BroadcastSrcAllocation` is a
    // multi-GPU broadcast array and this adapter is single-node, so entries above 0
    // have no meaning here. More than one arriving is counted rather than silently
    // truncated.
    if arg.SurfacesToPresent == 0 || arg.phSurfacesToPresent.is_null() {
        note_refusal(&L8_REFUSALS.present_bad_arg);
        let n = L8_REFUSALS.present_bad_arg.get();
        if n <= LOG_BUDGET {
            log_error!(
                "L8: pfnPresent with SurfacesToPresent={} phSurfacesToPresent={:p} (x{n})",
                arg.SurfacesToPresent,
                arg.phSurfacesToPresent,
            );
        }
        return;
    }
    if arg.SurfacesToPresent > 1 {
        note_refusal(&L8_REFUSALS.present_extra_surfaces_ignored);
    }
    // SAFETY: `phSurfacesToPresent` is non-null and `SurfacesToPresent >= 1` per the
    // checks above, so element 0 is within the array the runtime supplied.
    let surface = unsafe { &*arg.phSurfacesToPresent };

    // SAFETY: the runtime names a resource handle it obtained from this driver's own
    // create, so the block is ours; the borrow ends inside this function.
    let Some(engine) = (unsafe { resource12::engine_resource(surface.hSurface) }) else {
        note_refusal(&L8_REFUSALS.present_source_unresolved);
        return;
    };
    let Some(identity) = (unsafe { resource12::allocation_identity(surface.hSurface) }) else {
        // ⛔ The back buffer has no WDDM allocation, so there is nothing to present.
        // Distinct from `PresentSourceUnresolved`: the resource IS this driver's, and
        // its create did not reach `identity12::record` — which means
        // `create_committed_allocation` refused it. Read this against
        // `CommittedVenusExport` and the `Hwa2*` refusals, which say which one.
        note_refusal(&L8_REFUSALS.present_source_not_adopted);
        // ⛔ BUDGETED, and this is the site where it matters most: if the admission
        // predicate is wrong then EVERY present takes this arm, at frame rate, on the
        // exact run designed to answer that question -- and an unbounded logger there
        // is the T2 measurement again (module doc's `LOG_BUDGET`). The counter is
        // unbounded; the line is not.
        let n = L8_REFUSALS.present_source_not_adopted.get();
        if n <= LOG_BUDGET {
            log_error!(
                "L8: pfnPresent source resource {:#x} has no kernel allocation -- its create \
                 was refused, so the runtime has nothing to present. Check \
                 CommittedVenusExport against IdentityRecorded and the Hwa2* refusals \
                 (subresource={}) (x{n})",
                engine.as_raw() as usize,
                surface.SubResourceIndex,
            );
        }
        return;
    };

    // ⚠ The queue handle is validated for non-nullness on its own so that "the
    // runtime passed no queue" and "this driver cannot read the queue's context" stay
    // different counters.
    if h_queue.pDrvPrivate.is_null() {
        note_refusal(&L8_REFUSALS.present_bad_arg);
        return;
    }
    // The WDDM context this present must be submitted on — the first of the two
    // seams into `queue.rs` (module doc). Resolved BEFORE anything is written,
    // because a present descriptor naming a back buffer with no context to submit it
    // on is a request dxgkrnl cannot serve, and filling it halfway would be a silent
    // wrong answer rather than a loud missing one.
    //
    // SAFETY: the runtime names a queue handle `create_command_queue` returned
    // `S_OK` for and has not destroyed — the accessor's stated precondition. The
    // returned handle is dxgkrnl's, read for this call and never stored.
    let Some(h_context) = (unsafe { queue::present_context(h_queue) }) else {
        note_refusal(&L8_REFUSALS.present_queue_context_unavailable);
        let n = L8_REFUSALS.present_queue_context_unavailable.get();
        if n <= LOG_BUDGET {
            log_error!(
                "L8: pfnPresent REFUSED -- queue {:p} resolved to no live WDDM context, so no \
                 present descriptor can be completed. Everything else was ready: alloc={:#x} \
                 gen={} {}x{} fmt={} (x{n})",
                h_queue.pDrvPrivate,
                identity.h_allocation,
                identity.allocation_generation,
                identity.geometry.width,
                identity.geometry.height,
                identity.geometry.dxgi_format,
            );
        }
        return;
    };

    // The source allocation travels in the D3D12 present descriptor below. Keep the
    // table's non-zero invariant explicit before that channel is used.
    if identity.h_allocation == 0 {
        // ⛔ Unreachable by the table's invariant — `identity12`'s module doc: an
        // entry exists **iff** this driver owns a WDDM allocation, so
        // `h_allocation != 0` on every recorded entry. Counted because "unreachable
        // by construction" is a claim, and this is where it would be observed
        // breaking.
        note_refusal(&L8_REFUSALS.present_source_allocation_zero);
        return;
    }

    // ── ⛔ THE FRAME'S IDENTITY CANNOT BE BUILT, and this is the §5 gap ─────
    //
    // ⛔⛔ **ORDER IS LOAD-BEARING: this block must stay BELOW every arm that
    // returns.** It is `PresentEntered`'s terminal term — it fires on exactly the
    // presents that complete — so it has to be the last thing counted before the
    // descriptor write, and the `h_allocation == 0` refusal above was moved up
    // past it for that reason. While it sat above that arm, a
    // `PresentSourceAllocationZero` event was counted in BOTH terms and
    // `PresentEntered`'s documented identity over-counted by exactly the number of
    // hits on the one arm the identity exists to catch. That is the
    // instrument-attribution failure class this project has already paid for with
    // `WfBWire`, `RING_SUBMIT_COUNT` and `RENDER_COUNT`: an arithmetic check that
    // breaks hardest precisely when the thing it watches for happens.
    //
    // ⚠ **This block used to submit `HeliosPresentRenderCmd` on the queue's WDDM
    // context (UP-9) and the reversal is recorded rather than quietly edited.** The
    // record's load-bearing field is `HeliosPresentPrivateData::resource_id`, the
    // **host** venus resource id of the back buffer, and its `is_valid()` gates on
    // `resource_id != 0`. `docs/retirement/K4-CONTRACT.md` §5, from §10.3: *"no host
    // resource token, resid, PID, process handle, synchronization object, mutable
    // value, or independently usable identity … no UMD, ICD, batch, or private
    // descriptor can name or supply one"*. ⇒ this driver may not put one in a private
    // descriptor and hand it to the kernel, and the identity table no longer holds one
    // to put there. The record is not built, not zero-filled and not faked.
    //
    // ⛔ **The mechanism that replaces it is mesa lane unit A3**, not a different
    // field: the ICD stops naming host resources at all and the KMD patches the resid
    // in from `HeliosNativeRenderPatch`. Until A3 lands, the kernel learns no identity
    // for a D3D12 frame — which is the retirement's intended intermediate state and is
    // why this is a *counted refusal*, not a fallback.
    //
    // ⚠ **Counted, NOT raised, and the present PROCEEDS**, which is the severity this
    // path already chose for a missing identity — see `PresentIdentityUnavailable`'s
    // own argument: a windowed D3D12 frame reaches the screen through DWM's own D3D11
    // composition, which carries its own identity for the primary. This record is the
    // KMD's per-frame watermark and identity channel, not a precondition for a pixel,
    // and refusing the whole present over it would turn a lost diagnostic into a black
    // window. ⛔ It is deliberately not knob-gated (`METHOD.md` §2 Phase 4
    // consequence 1).
    note_refusal(&L8_REFUSALS.present_identity_no_resource_id);
    {
        let n = L8_REFUSALS.present_identity_no_resource_id.get();
        if n <= LOG_BUDGET {
            log_error!(
                "L8: pfnPresent carries NO identity record -- HWA2 has no host resource id and \
                 §10.3 forbids this UMD supplying one, so HeliosPresentPrivateData::resource_id \
                 has no legal source. alloc={:#x} gen={} {}x{} fmt={} on ctx={h_context:p}. The \
                 kernel will not learn this frame's identity until mesa unit A3 lands (the ICD \
                 stops naming host resources and the KMD patches the resid in from \
                 HeliosNativeRenderPatch) (x{n})",
                identity.h_allocation,
                identity.allocation_generation,
                identity.geometry.width,
                identity.geometry.height,
                identity.geometry.dxgi_format,
            );
        }
    }

    // ── the descriptor ──────────────────────────────────────────────────────
    //
    // ⛔ Written LAST and only once nothing above refused, so that every refusal
    // path leaves the all-zero descriptor from the top of this function standing.
    // An all-zero descriptor is unmistakably "the driver answered nothing", which is
    // what `METHOD.md` §5's *"trusting a zero"* asks a reader to be able to tell
    // apart from a half-filled one.
    //
    // SAFETY: `p_out` is non-null per the check above and `p_contexts`/`p_hw_queues`
    // are each either null — `as_mut` then yields `None` — or the live out-struct the
    // runtime allocated for this call. Every one of them was fully initialised by the
    // zero-fill at the top of this function, so each field assignment below writes
    // over a defined value.
    unsafe {
        if let Some(out) = p_out.as_mut() {
            // Entry 0 only: `BroadcastSrcAllocation` is a 65-entry multi-GPU
            // broadcast array and this adapter is single-node.
            out.BroadcastSrcAllocation[0] = identity.h_allocation;
            // ⛔ `BroadcastDstAllocation[0]` stays 0. A windowed present's
            // destination is DWM's surface, named by the runtime and not by this
            // driver; naming one here would be a claim about a resource this driver
            // does not own.
            //
            // ⛔ `AddedGpuWork = FALSE`, and the reason is narrower than it looks.
            // The flag is about GPU work **recorded into `hCommandList`**
            // (`PRESENT.md` §3.4's *"optionally record GPU work into the given
            // command list and set `AddedGpuWork`"*), and this body records nothing
            // into it. ⚠ It does make a kernel submission — the UP-9 `pfnRenderCb`
            // below — but that packet is already in dxgkrnl's FIFO for this very
            // context by the time the runtime's own present is issued on it, so
            // there is nothing left for the runtime to flush. TRUE would ask it to.
            out.AddedGpuWork = 0;
            // One back buffer per present. 1 is the value that *means* "no
            // multiplicity" rather than a guess at one.
            //
            // ⛔ **It is an OUT field on an OUT-only struct, so there is no request
            // here to honour or refuse.** `D3D12DDI_PRESENT_0051` is `_Out_` — the
            // `PFND3D12DDI_PRESENT_0051` typedef, `PRESENT.md` §3.2 — and the entire
            // IN struct `D3D12DDIARG_PRESENT_0001` has no multiplicity field: its
            // twelve members are `phSurfacesToPresent` / `SurfacesToPresent`,
            // `hDstResource` / `DstSubResourceIndex`, `Flags`, `FlipInterval`,
            // `VidPnSourceID`, `pDirtyRects` / `DirtyRects`, the private-data pair
            // and `OptimizeForComposition` (`bindgen/cached/d3d12umddi.rs`,
            // `D3D12DDIARG_PRESENT_0001`). The runtime states no multiplicity to
            // this DDI; the driver answers one.
            //
            // ⚠ **It is the D3D11 shape that misleads, and it is one struct away.**
            // On the DXGI DDI the field is an *input*:
            // `DXGI_DDI_ARG_PRESENT1::BackBufferMultiplicity` is a member of the arg
            // struct and the shipping D3D11 driver reads it — `umd/src/forward/
            // present.rs`'s `dxgi_present1`, forwarded into
            // `probe_present1_multi_entry`. D3D12 moved the field to the out side,
            // so the D3D11 intuition *"the runtime asks, we honour it or count the
            // refusal"* does not transfer.
            //
            // ⛔⛔ **CORRECTED 2026-08-07: this comment and the table in the doc
            // above both said the value was "unpriced, and instrumented for exactly
            // that reason", and cited a counter that has never existed.**
            // `L8Refusals` has no multiplicity field and `REFUSALS` no such entry.
            // Nor could one be added honestly: a counter for *"the runtime asked for
            // a multiplicity this driver does not honour"* can never move, because
            // the DDI gives the runtime no way to ask. Its permanent zero would then
            // be read as *"nobody ever wanted anything else"* — `METHOD.md` §5's
            // *"trusting a zero"*, pre-installed into a comment instead of arrived
            // at by accident. ⇒ The instrument claim is deleted rather than
            // implemented, and this paragraph is what stands in its place.
            //
            // ⚠ So what WOULD price it is not a counter: a swapchain whose presents
            // each look correct while its buffer rotation does not, on a runtime
            // that expected the driver to claim more than one physical buffer per
            // logical back buffer. That is a measurement against the screen
            // (CLAUDE.md rule 6), and no instrument at this slot can stand in for
            // it.
            out.BackBufferMultiplicity = 1;
            // The driver does not override the application's sync interval, so
            // `SyncIntervalOverride` is left at the zero-fill's value.
            out.SyncIntervalOverrideValid = 0;
        }
        if let Some(contexts) = p_contexts.as_mut() {
            contexts.hContext = h_context;
            // One adapter, one context. Broadcast is a multi-GPU shape and
            // `BroadcastContext` stays zeroed with the count.
            contexts.BroadcastContextCount = 0;
        }
        // ⛔ `p_hw_queues` is deliberately left exactly as the zero-fill wrote it:
        // `BroadcastQueueCount = 0`, no handles. This driver refuses hardware queues
        // at creation (`HwQRef`), so there is no handle it could legally name — and
        // `D12-G5` measured this pointer non-NULL at `_0040` and NULL at `_0110`,
        // which is why nothing here dereferences it.
    }
}

/// Install L8's one device-core slot, `pfnGetPresentPrivateDriverDataSize`.
///
/// Chain position: `FenceSlots` -> `PresentSlots` on the device-core table.
pub(crate) fn install_core(
    mut filling: Filling<'_, DeviceCoreTable, stage::FenceSlots>,
) -> Filling<'_, DeviceCoreTable, stage::PresentSlots> {
    let table = filling.table();
    table.pfnGetPresentPrivateDriverDataSize = Some(get_present_private_driver_data_size);
    filling.advance()
}

/// Install L8's command-list slots.
///
/// Chain position: `CopySlots` -> `PresentSlots` on the command-list table.
///
/// ⚠ **`pfnBlt` is deliberately left on its counting noop.** It is L8's third slot
/// and it is a *different* present model — the legacy DXGI blt path, which
/// `docs/archive/WINDOWED_BLT_DESIGN` and the 34th session's
/// `DXGI_STATUS_OCCLUDED` finding are about. Nothing in `PENDING.md` §S-3 asks for
/// it, no measured D3D12 present has taken it, and installing an untested handler
/// beside the one that matters would put two present models in review at once. Its
/// noop counter is what says whether it is ever entered.
pub(crate) fn install_cmdlist(
    mut filling: Filling<'_, CommandListTable, stage::CopySlots>,
) -> Filling<'_, CommandListTable, stage::PresentSlots> {
    let table = filling.table();
    table.pfnPresent = Some(present);
    filling.advance()
}

/// L8's counters.
struct L8Refusals {
    /// `pfnGetPresentPrivateDriverDataSize` calls served, all answering 0. ⚠ **Not a
    /// refusal — the census**, and the only instrument that says whether the runtime
    /// asks at all. Non-zero with `PresentBadArg` at 0 and no present hits would say
    /// the runtime sizes a trailer for presents it never issues to this driver.
    present_private_data_size_queries: RefusalCounter,
    /// `pfnPresent` with a null argument struct, a null/empty surface list, or a null
    /// queue handle. ⛔ Expected 0: the DDI declares its argument non-optional.
    present_bad_arg: RefusalCounter,
    /// `pfnPresent` with a null `D3D12DDI_PRESENT_0051*`. ⛔ Expected 0 — it is the
    /// one output the runtime cannot proceed without, so its own counter rather than
    /// `PresentBadArg`'s aggregate.
    present_no_out_struct: RefusalCounter,
    /// `pfnPresent` arrived with a **non-NULL `hDstResource`**. ⛔ Expected 0, and
    /// refused: a resource-to-resource present is not implemented. ⚠ Its NULL-ness
    /// is *not* established for this driver — the measurement came from the WARP
    /// control arm, which has no display adapter — so a hit is new information about
    /// the runtime rather than a driver bug.
    present_dst_resource_refused: RefusalCounter,
    /// More than one surface arrived and only element 0 was considered. ⚠ Expected 0
    /// on a single-node adapter; `BroadcastSrcAllocation`'s other 64 entries are a
    /// multi-GPU shape.
    present_extra_surfaces_ignored: RefusalCounter,
    /// The presented surface's handle did not resolve to an engine resource. ⛔
    /// Expected 0: the runtime presents a resource it created through this driver.
    present_source_unresolved: RefusalCounter,
    /// The presented surface resolved but has **no kernel allocation**, i.e. its
    /// create was refused. ⛔ Expected 0 once a real swapchain runs; read it against
    /// `CommittedVenusExport`, `IdentityRecorded` and L4's `Hwa2*` refusals, which say
    /// which stage of the HWA2 create contract failed, not against this counter
    /// alone.
    ///
    /// ⚠ **The name says "NotAdopted" and there is no adoption any more.** It is kept
    /// verbatim on purpose: `D3D12 DDI refusals:` lines are diffed across builds, so
    /// a renamed counter reads as a new counter and its history stops. The *meaning*
    /// is the one above — no kernel allocation — which is what the name always
    /// measured; only the mechanism behind it changed (K4-CONTRACT §5).
    present_source_not_adopted: RefusalCounter,
    /// The queue handle did not resolve to a live WDDM context.
    ///
    /// ⚠⚠ **RE-GRADED, and the old grading is written out so the change is not
    /// silent.** It used to mean *"`QueueState::h_context` is private to another
    /// lane's file and there is no accessor"* — a structural blocker, expected
    /// non-zero on **every** present until [`queue::present_context`] landed. That
    /// accessor exists now, so the counter no longer says anything about this
    /// driver's own structure and instead means one of two runtime-facing things:
    /// the handle did not resolve to a live `QueueState`, or its `h_context` was
    /// null.
    ///
    /// ⛔ **Expected 0**, and both causes are unreachable by construction —
    /// `create_wddm_context` fails the queue create on a null context, so a live
    /// queue always has one. ⇒ a hit is a lifetime finding (a present on a
    /// destroyed queue), not a missing feature.
    present_queue_context_unavailable: RefusalCounter,
    /// ⭐ **`pfnPresent` entries — the census, not a refusal.** Counted as the
    /// function's first statement, above the out-struct zero-fill, so it counts
    /// entries and not outcomes.
    ///
    /// ⛔ **It is the left-hand side of this lane's arithmetic**, and nothing else in
    /// the driver can be: `PresentEntered == PresentIdentityNoResourceId +
    /// PresentBadArg + PresentNoOutStruct + PresentDstResourceRefused +
    /// PresentSourceUnresolved + PresentSourceNotAdopted +
    /// PresentQueueContextUnavailable + PresentSourceAllocationZero`.
    ///
    /// ⛔⛔ **What makes that hold is that every term is a path that RETURNS, plus
    /// exactly one terminal term.** `PresentIdentityNoResourceId` is the terminal
    /// one — it is counted after the last refusal arm, so it equals the presents
    /// that complete. It did NOT hold when it was first written: the
    /// `PresentIdentityNoResourceId` bump sat *above* the `h_allocation == 0` arm,
    /// so every `PresentSourceAllocationZero` was counted in both terms and the
    /// right-hand side over-counted by exactly the hits on the arm this identity
    /// exists to catch. The site now carries a comment forbidding the move back.
    ///
    /// ⚠ **`PresentExtraSurfacesIgnored` is deliberately NOT a term** — it does not
    /// return, so a present that hits it goes on to land in one of the terms above.
    /// Summing "all of L8's counters" therefore does not reproduce this identity,
    /// and neither does adding `PresentPrivateDataSizeQueries`, which counts a
    /// different DDI. ⚠ The four identity-submission slots
    /// (`PresentIdentitySubmitted` in L2's set, `PresentIdentityRefused`,
    /// `PresentIdentityUnavailable`, `PresentIdentityInvalid`) are no longer terms
    /// either: nothing reaches them. Same construction and
    /// the same reason as `FenceSignalEntered`: a zero here and a zero everywhere else
    /// is *"the runtime never presented through this driver"*, which is a completely
    /// different finding from *"every present refused"*, and before this counter the
    /// two were indistinguishable.
    present_entered: RefusalCounter,
    /// ⛔ **Retired append-only telemetry slot: PERMANENTLY 0 as of the HPS2
    /// retirement, and that is what it now means.** It counted a
    /// `HeliosPresentRenderCmd` that failed its own `is_valid()`. No such record is
    /// built any more — `resource_id` is a host resource id and §10.3 forbids this
    /// UMD supplying one — so nothing can reach the check. ⚠ Read
    /// `PresentIdentityNoResourceId` instead; this slot keeps its position because
    /// `D3D12 DDI refusals:` field order is the evidence contract and re-numbering it
    /// would invalidate every cross-build diff.
    present_identity_invalid: RefusalCounter,
    /// A present resolved to a recorded identity whose `h_allocation` was **0**, so
    /// no allocation list could be built.
    ///
    /// ⛔ **Expected 0 and unreachable by the table's invariant** — `identity12`'s
    /// module doc: an entry exists *iff* this driver owns a WDDM allocation. Counted
    /// because "unreachable by construction" is a claim about another module's
    /// invariant, and this is the site that would observe it breaking.
    present_source_allocation_zero: RefusalCounter,
    /// ⛔ **Retired append-only telemetry slot: PERMANENTLY 0**, same reason as
    /// `PresentIdentityInvalid` above. It counted an identity submission that could
    /// not be built for a *local* reason (no `pfnRenderCb`, no context, no command
    /// window); no submission is attempted at all now.
    present_identity_unavailable: RefusalCounter,
    /// ⛔ **Retired append-only telemetry slot: PERMANENTLY 0**, same reason. It
    /// counted dxgkrnl refusing a present identity packet this driver had built.
    present_identity_refused: RefusalCounter,
    /// ⛔⛔ **The frame's identity record has no legal source, so none was built or
    /// submitted — and this is the §5 gap, named.**
    ///
    /// `HeliosPresentPrivateData::resource_id` is the back buffer's **host** venus
    /// resource id, and §10.3 forbids any UMD, ICD, batch or private descriptor
    /// naming or supplying one. HWA2 carries no such field, `identity12` no longer
    /// holds one, and nothing here fabricates one.
    ///
    /// ⛔ **Counted after the LAST arm that returns**, which makes it exactly the
    /// presents that complete and makes `PresentEntered`'s documented identity an
    /// equality rather than an over-count. Moving this bump back above the
    /// `h_allocation == 0` refusal would double-count that arm; the site says so.
    ///
    /// ⚠ **Expected to equal `PresentEntered` minus the other refusals, i.e. to fire
    /// on EVERY present**, until mesa lane unit **A3** lands: the ICD stops naming
    /// host resources at all and the KMD patches the resid in from
    /// `HeliosNativeRenderPatch`. That is the retirement's intended intermediate state
    /// and not a regression — an ICD in this state cannot import, which is recorded
    /// rather than worked around.
    ///
    /// ⛔ The present still PROCEEDS; see the site for why a lost diagnostic must not
    /// become a black window.
    present_identity_no_resource_id: RefusalCounter,
}

static L8_REFUSALS: L8Refusals = L8Refusals {
    present_private_data_size_queries: RefusalCounter::new("PresentPrivateDataSizeQueries"),
    present_bad_arg: RefusalCounter::new("PresentBadArg"),
    present_no_out_struct: RefusalCounter::new("PresentNoOutStruct"),
    present_dst_resource_refused: RefusalCounter::new("PresentDstResourceRefused"),
    present_extra_surfaces_ignored: RefusalCounter::new("PresentExtraSurfacesIgnored"),
    present_source_unresolved: RefusalCounter::new("PresentSourceUnresolved"),
    present_source_not_adopted: RefusalCounter::new("PresentSourceNotAdopted"),
    present_queue_context_unavailable: RefusalCounter::new("PresentQueueContextUnavailable"),
    present_entered: RefusalCounter::new("PresentEntered"),
    present_identity_invalid: RefusalCounter::new("PresentIdentityInvalid"),
    present_source_allocation_zero: RefusalCounter::new("PresentSourceAllocationZero"),
    present_identity_unavailable: RefusalCounter::new("PresentIdentityUnavailable"),
    present_identity_refused: RefusalCounter::new("PresentIdentityRefused"),
    present_identity_no_resource_id: RefusalCounter::new("PresentIdentityNoResourceId"),
};

/// L8's refusal counters, printed by `crate::log_refusal_summary` at this lane's
/// position in `lib.rs`'s `UMD12_REFUSAL_SETS`.
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
    &L8_REFUSALS.present_private_data_size_queries,
    &L8_REFUSALS.present_bad_arg,
    &L8_REFUSALS.present_no_out_struct,
    &L8_REFUSALS.present_dst_resource_refused,
    &L8_REFUSALS.present_extra_surfaces_ignored,
    &L8_REFUSALS.present_source_unresolved,
    &L8_REFUSALS.present_source_not_adopted,
    &L8_REFUSALS.present_queue_context_unavailable,
    // ⛔ APPENDED, UP-9 (the identity `pfnRenderCb`). Four, at the END for the reason
    // the block comment above states: `D3D12 DDI refusals:` lines are diffed across
    // builds and inserting shifts every counter after the insertion point. ⚠ The
    // submission's SUCCESS counter is `PresentIdentitySubmitted` and it lives in L2's
    // set, beside the code that submits -- see `queue::submit_present_identity`.
    &L8_REFUSALS.present_entered,
    &L8_REFUSALS.present_identity_invalid,
    &L8_REFUSALS.present_source_allocation_zero,
    &L8_REFUSALS.present_identity_unavailable,
    &L8_REFUSALS.present_identity_refused,
    // ⛔ APPENDED, the HPS2 retirement (K4): the §5 host-resource-id gap, naming mesa
    // unit A3. At the END for the reason above -- field order is the evidence contract.
    &L8_REFUSALS.present_identity_no_resource_id,
];
