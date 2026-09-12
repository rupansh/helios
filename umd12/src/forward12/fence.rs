//! Runtime fence lifetime and engine query heaps.
//!
//! On this software-scheduled surface dxgkrnl owns fence values, CPU signals,
//! shared opens and context waits. Exact ECL admission/completion orders the
//! real engine work against those operations. CreateFence supplies no kernel
//! handle or initial value, so a private engine fence cannot represent it.
//! Nonzero GPU fence placements require a real GPU-VA fence implementation and
//! are explicitly refused. Queue fence DDIs are likewise refused if entered;
//! their absence in a workload is not proof of native-fence support.

use helios_umd_common::hr::{Hresult, E_FAIL, E_INVALIDARG, E_NOTIMPL, S_OK};
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
// Query heaps own engine COM objects. Runtime fences own only a device
// association: CREATE_FENCE supplies no KMT handle, CPU mapping or initial value.
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

/// Exact runtime object lifetime; no shadow timeline or guessed fence identity.
pub struct FenceState {
    device: ddi12::D3D12DDI_HDEVICE,
}

impl FenceState {
    pub(crate) fn belongs_to(&self, device: ddi12::D3D12DDI_HDEVICE) -> bool {
        self.device.pDrvPrivate == device.pDrvPrivate
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
/// The returned state authenticates the runtime device association. It carries
/// no private engine fence or invented signal watermark.
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
unsafe extern "system" fn calc_private_fence_size(
    _h_device: ddi12::D3D12DDI_HDEVICE,
    arg: *const ddi12::D3D12DDIARG_CREATE_FENCE,
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
unsafe extern "system" fn create_fence(
    h_device: ddi12::D3D12DDI_HDEVICE,
    h_fence: ddi12::D3D12DDI_HFENCE,
    arg: *const ddi12::D3D12DDIARG_CREATE_FENCE,
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

    // ⚠ Per-arm validation, not a max-union: `Fences` is only readable for
    // `FenceCount` entries and only meaningful when that count is 1 here.
    if a.FenceCount != 1 || a.Fences.is_null() {
        note_refusal(&L7_REFUSALS.fence_multi_adapter_refused);
        if let Some(n) = budget(&FENCE_LOG) {
            log_error!(
                "CreateFence: FenceCount={} Fences={:p} -- single-adapter Helios backs exactly one \
                 placement -> E_INVALIDARG (x{})",
                a.FenceCount,
                a.Fences,
                n + 1,
            );
        }
        return E_INVALIDARG;
    }
    // SAFETY: `Fences` is non-null and `FenceCount == 1` per the check above, so
    // element 0 is inside the array the DDI declares `_Field_size_(FenceCount)`.
    let placement = unsafe { &*a.Fences };

    let flags = placement.Flags;
    let known = ddi12::D3D12DDI_FENCE_FLAGS_D3D12DDI_FENCE_FLAG_BOTTOM_OF_PIPE;
    if flags & !known != 0 {
        note_refusal(&L7_REFUSALS.fence_flags_unknown);
        return E_INVALIDARG;
    }
    if placement.FenceValue.BaseAddress != 0 || placement.FenceMonitoredValue.BaseAddress != 0 {
        note_refusal(&L7_REFUSALS.fence_gpu_va_refused);
        return E_NOTIMPL;
    }
    // SAFETY: this device-scope DDI supplies its live creating device.
    if unsafe { device12::device(h_device) }.is_none() {
        note_refusal(&L7_REFUSALS.fence_no_device);
        return E_FAIL;
    }
    note_refusal(&L7_REFUSALS.fence_runtime_owned);

    // ⚠ On the SUCCESS path, and budgeted: this is the only capture anywhere of
    // the GPU virtual addresses the runtime picks for a D3D12 monitored fence on
    // this adapter, which is contract data no document in `docs/dx12/` holds and
    // which the G-fence probe (`DDI_REFERENCE.md` §10.5) will want to compare
    // against.
    if let Some(n) = budget(&FENCE_LOG) {
        log_error!(
            "CreateFence: valueVA={:#x} monitoredVA={:#x} flags={:#x} -> runtime-owned fence (x{})",
            placement.FenceValue.BaseAddress,
            placement.FenceMonitoredValue.BaseAddress,
            flags,
            n + 1,
        );
    }

    // SAFETY: validated empty runtime-private slot; it owns this object until DestroyFence.
    unsafe {
        slot.store(FenceState { device: h_device });
    }
    S_OK
}

/// `pfnDestroyFence`.
///
/// Returns `VOID`; the counter is the only channel, which is why a destroy on an
/// empty slot is counted rather than ignored.
///
/// # Safety
/// `h_fence` must be a handle [`create_fence`] returned `S_OK` for and which has
/// not already been destroyed.
unsafe extern "system" fn destroy_fence(
    _h_device: ddi12::D3D12DDI_HDEVICE,
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
    drop(unsafe { slot.take() });
}

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
unsafe extern "system" fn calc_private_query_heap_size(
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
unsafe extern "system" fn create_query_heap(
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
unsafe extern "system" fn destroy_query_heap(
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
    table.pfnCreateFence = Some(create_fence);
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
    /// `D3D12DDIARG_CREATE_FENCE` asked for `FenceCount != 1` (or a null array),
    /// and the create was refused.
    ///
    /// ⛔ **Expected 0 on this guest.** `FenceCount > 1` is the multi-adapter
    /// (LDA) case, one placement per physical adapter (`DDI_REFERENCE.md`
    /// §10.1), and Helios reports one implicit physical adapter. A hit means the
    /// multi-adapter assumption behind `ARCHITECTURE.md` §13 UNVERIFIED-11 has
    /// been reached for real, and a single engine fence cannot honour it.
    fence_multi_adapter_refused: RefusalCounter,
    /// Retired counter, preserved at its diagnostic index; always zero.
    fence_bottom_of_pipe_unproven: RefusalCounter,
    /// A `D3D12DDI_FENCE::Flags` carried a bit outside the two enumerators
    /// `d3d12umddi.h` defines. **Expected 0**; a hit means the header this build
    /// was generated from is older than the runtime asking.
    fence_flags_unknown: RefusalCounter,
    /// Retired counter, preserved at its diagnostic index; always zero.
    fence_engine_failed: RefusalCounter,
    fence_runtime_owned: RefusalCounter,
    fence_gpu_va_refused: RefusalCounter,
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
    fence_multi_adapter_refused: RefusalCounter::new("FenceMultiAdapterRefused"),
    fence_bottom_of_pipe_unproven: RefusalCounter::new("FenceBottomOfPipeUnproven"),
    fence_flags_unknown: RefusalCounter::new("FenceFlagsUnknown"),
    fence_engine_failed: RefusalCounter::new("FenceEngineFailed"),
    fence_runtime_owned: RefusalCounter::new("FenceRuntimeOwned"),
    fence_gpu_va_refused: RefusalCounter::new("FenceGpuVaRefused"),
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
    &L7_REFUSALS.fence_multi_adapter_refused,
    &L7_REFUSALS.fence_bottom_of_pipe_unproven,
    &L7_REFUSALS.fence_flags_unknown,
    &L7_REFUSALS.fence_engine_failed,
    &L7_REFUSALS.query_heap_bad_arg,
    &L7_REFUSALS.query_heap_no_device,
    &L7_REFUSALS.query_heap_type_unsupported,
    &L7_REFUSALS.query_heap_engine_failed,
    &L7_REFUSALS.fence_runtime_owned,
    &L7_REFUSALS.fence_gpu_va_refused,
];

// ⚠ `Hresult` is imported for the `E_*`/`S_OK` constants this file returns; the
// DDI's own `HRESULT` (bindgen's `c_long`) is the declared return type and the
// two are the same `i32`. Naming both keeps `umd_common::hr`'s constants usable
// without a cast at forty return sites (`umd_common/src/hr.rs:31-34`).
const _: () = assert!(core::mem::size_of::<Hresult>() == core::mem::size_of::<ddi12::HRESULT>());
