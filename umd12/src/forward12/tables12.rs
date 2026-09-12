//! Typed D3D12 table construction and the subsystem install sequencer.
//!
//! Build a local table with signature-preserving counting fallbacks, run the
//! typed install chain, then copy only the runtime's validated byte count.
//! Unknown tables and oversized tails are refused before any write: no callable
//! x86 stub can be synthesized without knowing its stdcall argument list.
//!
//! # ⭐ Install order is structural, not textual
//!
//! `ARCHITECTURE.md` §12 rule 9: correctness of every ≥11.1 D3D11 device once
//! rested on `install()` running before `install_11_1()`, and the wrong order
//! produced *"wrong blending for DWM, no counter, no log, only pixels."*
//! [`Filling`] carries a stage marker, each lane's `install` names the previous
//! lane's marker as its input, and the whole chain is `#[must_use]`. So:
//!
//! * a lane **cannot** run before the stub fill — there is no other way to
//!   obtain a `Filling`, and its constructor is private to this module;
//! * a lane **cannot** be dropped from the chain — the next lane's signature
//!   names its marker, and the chain's terminus is consumed by [`seal`];
//! * a lane **cannot** be reordered — the same reason, in the other direction.
//!
//! It also carries the `&mut` to the table, so the table cannot be read or
//! copied while the chain is live.

use core::ffi::c_void;
use core::marker::PhantomData;

use helios_umd_common::hr::{Hresult, E_INVALIDARG, E_NOTIMPL, S_OK};
use helios_umd_common::refusals::RefusalCounter;

use super::noop12;
use super::{cmdlist, copy, descriptors, fence, misc, present12, pso, queue, resource12, rootargs};
use crate::{caps12, ddi12, log_error, note_refusal, UMD12_REFUSALS};

/// `D3D12DDI_DEVICE_FUNCS_CORE_0109` — 124 slots.
pub(crate) type DeviceCoreTable = ddi12::D3D12DDI_DEVICE_FUNCS_CORE_0109;
/// `D3D12DDI_COMMAND_LIST_FUNCS_3D_0108` — 75 slots.
pub(crate) type CommandListTable = ddi12::D3D12DDI_COMMAND_LIST_FUNCS_3D_0108;
/// `D3D12DDI_COMMAND_QUEUE_FUNCS_CORE_0001` — 7 slots.
pub(crate) type CommandQueueTable = ddi12::D3D12DDI_COMMAND_QUEUE_FUNCS_CORE_0001;

// ---------------------------------------------------------------------------
// The install-chain token
// ---------------------------------------------------------------------------

/// A DDI table under construction, tagged with how far the install chain has
/// got.
///
/// `T` is the table struct; `S` is an uninhabited stage marker. Neither is ever
/// a value: the whole type is one `&mut T` at runtime.
#[must_use = "the install chain must reach `seal`, or a lane was silently dropped"]
pub(crate) struct Filling<'a, T, S> {
    table: &'a mut T,
    _stage: PhantomData<fn() -> S>,
}

impl<'a, T, S> Filling<'a, T, S> {
    /// The table, for the lane that currently holds the token.
    pub(crate) fn table(&mut self) -> &mut T {
        self.table
    }

    /// Hand the table to the next stage.
    ///
    /// ⚠ Callable only by whoever already holds a `Filling`, i.e. by a lane the
    /// chain reached — which is what makes "cannot run before the stub fill"
    /// and "cannot be reordered" the same property.
    pub(crate) fn advance<S2>(self) -> Filling<'a, T, S2> {
        Filling {
            table: self.table,
            _stage: PhantomData,
        }
    }
}

/// Consume the last token of a chain.
///
/// Exists so the terminus is a named act rather than a `let _ =`, which would
/// silence the `#[must_use]` and with it the "a lane was dropped" signal.
fn seal<T, S>(_last: Filling<'_, T, S>) {}

/// Stage markers. Uninhabited: they are type-level names, never values.
///
/// ⚠ Deliberately suffixed `Slots` rather than named after the lane bare
/// (`Copy`, `Fence`, `Queue`): `Copy` would shadow the prelude trait in this
/// module, and one shadowed prelude name is enough to make a later
/// `#[derive(Copy)]` here fail with an error that points nowhere useful.
pub(crate) mod stage {
    /// Every slot carries its per-slot counting noop; no lane has run.
    pub(crate) enum Stubbed {}
    /// L1 — caps: format/MSAA queries.
    pub(crate) enum CapsSlots {}
    /// L2 — queue, pool, recorder, command-list lifetime.
    pub(crate) enum QueueSlots {}
    /// L4 — resources, heaps, residency, introspection.
    pub(crate) enum ResourceSlots {}
    /// L5 — descriptor heaps and views.
    pub(crate) enum DescriptorSlots {}
    /// L6a — PSO, root signatures, sub-state.
    pub(crate) enum PsoSlots {}
    /// L6b — shaders.
    pub(crate) enum ShaderSlots {}
    /// L7 — fences and query heaps.
    pub(crate) enum FenceSlots {}
    /// L8 — present.
    pub(crate) enum PresentSlots {}
    /// L9 — the tail: meta-commands, state objects, VRS, mesh, work graphs.
    pub(crate) enum MiscSlots {}
    /// L3a — recording: draw, fixed-function state, IA/SO/OM.
    pub(crate) enum RecordSlots {}
    /// L3b — root arguments, descriptor binding, clears.
    pub(crate) enum RootArgSlots {}
    /// L3c — copy, resolve, barriers, queries.
    pub(crate) enum CopySlots {}
}

// ---------------------------------------------------------------------------
// The command-list table handles
// ---------------------------------------------------------------------------

/// The runtime's per-index `D3D12DDI_HRTTABLE` for the command-list table.
///
/// ⭐ **Stashed here because there is no other way to obtain it.**
/// `DDI_REFERENCE.md` §2.2, measured by `D12-G5`: the runtime fills
/// `COMMAND_LIST_3D` **twice** during device creation, in immediate succession,
/// with the 5th `UINT` = 0 then 1 and two distinct `hRTTable` handles — and
/// those handles are exactly what the driver later hands to
/// `pfnSetCommandListDDITableCb` on every command-list create. Nothing else
/// carries them.
///
/// ⚠ Two entries because two is what was measured, and `pfnGetOptionalDDITables`
/// answers 0 (so the second table is the runtime's own doing, not something this
/// driver asked for). An index beyond this is counted and dropped rather than
/// growing the array on runtime data.
const COMMAND_LIST_TABLE_SLOTS: usize = 2;
static COMMAND_LIST_RT_TABLES: [core::sync::atomic::AtomicUsize; COMMAND_LIST_TABLE_SLOTS] =
    [const { core::sync::atomic::AtomicUsize::new(0) }; COMMAND_LIST_TABLE_SLOTS];

/// The runtime table handle for command-list table `index`, or 0.
///
/// L2 (`queue.rs`) is its real consumer, the moment `pfnCreateCommandList`
/// becomes a body: `pfnSetCommandListDDITableCb(hRTCommandList, hRTTable)` has
/// nowhere else to get its second argument (`DDI_REFERENCE.md` §2.2). Until
/// then it is read by [`log_command_list_tables`] — because a stash nothing
/// reads is the T5 anti-pattern, *an instrument nothing can read is not an
/// instrument*, and because the handles themselves are contract capture that
/// `D12-G5` needed a spy proxy in front of WARP to obtain.
pub(crate) fn command_list_rt_table(index: usize) -> usize {
    COMMAND_LIST_RT_TABLES
        .get(index)
        .map_or(0, |h| h.load(core::sync::atomic::Ordering::Relaxed))
}

/// Print the runtime table handles this adapter was handed, one line.
///
/// Called from `adapter12::close_adapter`, beside the refusal summary and the
/// noop hit counts.
pub(crate) fn log_command_list_tables() {
    let mut line = String::with_capacity(64);
    for index in 0..COMMAND_LIST_TABLE_SLOTS {
        line.push_str(&format!(" [{index}]={:#x}", command_list_rt_table(index)));
    }
    log_error!("D3D12 command-list hRTTable:{line}");
}

fn stash_command_list_rt_table(index: ddi12::UINT, handle: *mut c_void) {
    match COMMAND_LIST_RT_TABLES.get(index as usize) {
        Some(slot) => slot.store(handle as usize, core::sync::atomic::Ordering::Relaxed),
        None => note_refusal(&UMD12_REFUSALS.command_list_table_index_unbounded),
    }
}

// ---------------------------------------------------------------------------
// pfnFillDDITable
// ---------------------------------------------------------------------------

/// Fill one runtime-owned DDI table.
///
/// Called from `adapter12::fill_ddi_table`, which owns the DDI slot; this owns
/// the tables.
///
/// # Safety
/// `table` must point at `table_size` writable bytes the runtime owns, aligned
/// for function pointers. `table_size` is the runtime's own count and is the
/// only authority on how much of that buffer exists.
pub(crate) unsafe fn fill(
    table_type: ddi12::D3D12DDI_TABLE_TYPE,
    table: *mut c_void,
    table_size: ddi12::SIZE_T,
    index: ddi12::UINT,
    h_rt_table: ddi12::D3D12DDI_HRTTABLE,
) -> Hresult {
    let bytes = table_size as usize;
    if table.is_null()
        || bytes < core::mem::size_of::<usize>()
        || !bytes.is_multiple_of(core::mem::size_of::<usize>())
    {
        note_refusal(&UMD12_REFUSALS.fill_ddi_table_bad_arg);
        return E_INVALIDARG;
    }

    let header_bytes = header_table_size(table_type);
    if header_bytes == 0 {
        note_refusal(&UMD12_REFUSALS.fill_ddi_table_unknown_type);
        log_error!("FillDDITable: unsupported table type {table_type}, {bytes} bytes");
        return E_NOTIMPL;
    }
    if bytes > header_bytes {
        note_refusal(&UMD12_REFUSALS.fill_ddi_table_oversized);
        log_error!(
            "FillDDITable: type {table_type} asks for {bytes} bytes; header has {header_bytes}"
        );
        return E_INVALIDARG;
    }

    match table_type {
        ddi12::D3D12DDI_TABLE_TYPE_D3D12DDI_TABLE_TYPE_DEVICE_CORE => {
            // SAFETY: forwarded unchanged; the caller's guarantee is this
            // function's, and the table type has been checked.
            unsafe { fill_device_core(table, bytes) }
        }
        ddi12::D3D12DDI_TABLE_TYPE_D3D12DDI_TABLE_TYPE_COMMAND_LIST_3D => {
            stash_command_list_rt_table(index, h_rt_table.handle);
            // SAFETY: as above.
            unsafe { fill_command_list(table, bytes) }
        }
        ddi12::D3D12DDI_TABLE_TYPE_D3D12DDI_TABLE_TYPE_COMMAND_QUEUE_3D => {
            // SAFETY: as above.
            unsafe { fill_command_queue(table, bytes) }
        }
        ddi12::D3D12DDI_TABLE_TYPE_D3D12DDI_TABLE_TYPE_0096_EXTENDED_FEATURES => {
            let built = ddi12::D3D12DDI_EXTENDED_FEATURES_FUNCS_0096 {
                pfnGetSupportedExtendedFeatures: Some(get_extended_features_0096),
                pfnGetSupportedExtendedFeatureVersions: Some(get_extended_feature_versions),
                pfnEnableExtendedFeature: Some(enable_extended_feature),
                pfnSetExtendedFeatureCallbacks: Some(set_extended_feature_callbacks),
            };
            // SAFETY: validated runtime buffer; built has the negotiated table's type.
            unsafe { publish("EXTENDED_FEATURES_0096", table, bytes, &built) };
            S_OK
        }
        ddi12::D3D12DDI_TABLE_TYPE_D3D12DDI_TABLE_TYPE_0020_EXTENDED_FEATURES => {
            let built = ddi12::D3D12DDI_EXTENDED_FEATURES_FUNCS_0021 {
                pfnGetSupportedExtendedFeatures: Some(get_extended_features_0020),
                pfnGetSupportedExtendedFeatureVersions: Some(get_extended_feature_versions),
                pfnEnableExtendedFeature: Some(enable_extended_feature),
                pfnSetExtendedFeatureCallbacks: Some(set_extended_feature_callbacks),
            };
            // SAFETY: the _0020 table is the first three slots of this _0021 type.
            unsafe { publish("EXTENDED_FEATURES_0021", table, bytes, &built) };
            S_OK
        }
        _ => E_NOTIMPL,
    }
}

/// The size of the table struct **this build's header** describes, or 0 for a
/// table type this driver does not serve.
///
/// Exists for `probe12::helios_umd12_probe_ddi_table_size_v1`: a size test that
/// hard-codes 992 / 600 / 56 agrees with whichever SDK it was written against,
/// not the one being built, and that is the one failure mode a size test must
/// not have.
pub(crate) fn header_table_size(table_type: ddi12::D3D12DDI_TABLE_TYPE) -> usize {
    match table_type {
        ddi12::D3D12DDI_TABLE_TYPE_D3D12DDI_TABLE_TYPE_DEVICE_CORE => {
            core::mem::size_of::<DeviceCoreTable>()
        }
        ddi12::D3D12DDI_TABLE_TYPE_D3D12DDI_TABLE_TYPE_COMMAND_LIST_3D => {
            core::mem::size_of::<CommandListTable>()
        }
        ddi12::D3D12DDI_TABLE_TYPE_D3D12DDI_TABLE_TYPE_COMMAND_QUEUE_3D => {
            core::mem::size_of::<CommandQueueTable>()
        }
        ddi12::D3D12DDI_TABLE_TYPE_D3D12DDI_TABLE_TYPE_0096_EXTENDED_FEATURES => {
            core::mem::size_of::<ddi12::D3D12DDI_EXTENDED_FEATURES_FUNCS_0096>()
        }
        ddi12::D3D12DDI_TABLE_TYPE_D3D12DDI_TABLE_TYPE_0020_EXTENDED_FEATURES => {
            core::mem::size_of::<ddi12::D3D12DDI_EXTENDED_FEATURES_FUNCS_0021>()
        }
        _ => 0,
    }
}

/// Count shorter runtime tables and return the bounded copy length.
fn agreed_bytes(name: &str, runtime_bytes: usize, header_bytes: usize) -> usize {
    if runtime_bytes < header_bytes {
        // The R702 direction: the runtime's buffer is SHORTER than the struct
        // this build knows. Copying `header_bytes` here is the out-of-bounds
        // write. The tail slots simply do not exist for this runtime.
        note_refusal(&UMD12_REFUSALS.fill_ddi_table_truncated);
        log_error!(
            "FillDDITable {name}: runtime table is {runtime_bytes} B, this header's struct is \
             {header_bytes} B -- filling {runtime_bytes} B and leaving the rest alone"
        );
        runtime_bytes
    } else {
        header_bytes
    }
}

/// Copy a driver-built table into the runtime's buffer, bounded by the
/// runtime's own byte count.
///
/// # Safety
/// `dst` must point at `runtime_bytes` writable bytes the runtime owns.
unsafe fn publish<T>(name: &str, dst: *mut c_void, runtime_bytes: usize, built: &T) {
    let n = agreed_bytes(name, runtime_bytes, core::mem::size_of::<T>());
    // SAFETY: `n <= runtime_bytes` and `n <= size_of::<T>()` by construction in
    // `agreed_bytes`, `dst` is the runtime's buffer of at least `runtime_bytes`
    // writable bytes, and `built` is a live local — the two cannot overlap.
    unsafe {
        core::ptr::copy_nonoverlapping((built as *const T).cast::<u8>(), dst.cast::<u8>(), n);
    }
}

/// # Safety
/// As [`fill`].
unsafe fn fill_device_core(table: *mut c_void, bytes: usize) -> Hresult {
    let mut built = noop12::device_core::stubbed_table();

    // The install chain. Order is structural (see the module doc).
    let t = Filling::<DeviceCoreTable, stage::Stubbed> {
        table: &mut built,
        _stage: PhantomData,
    };
    let t = caps12::install(t);
    let t = queue::install_core(t);
    let t = resource12::install(t);
    let t = descriptors::install(t);
    let t = pso::install(t);
    let t = pso::install_shaders(t);
    let t = fence::install(t);
    let t = present12::install_core(t);
    let t = misc::install_core(t);
    seal(t);

    // Publish, bounded by the runtime's count.
    // SAFETY: as the caller's guarantee.
    unsafe { publish("DEVICE_CORE", table, bytes, &built) };
    S_OK
}

/// # Safety
/// As [`fill`].
unsafe fn fill_command_list(table: *mut c_void, bytes: usize) -> Hresult {
    let mut built = noop12::command_list::stubbed_table();

    let t = Filling::<CommandListTable, stage::Stubbed> {
        table: &mut built,
        _stage: PhantomData,
    };
    let t = cmdlist::install(t);
    let t = rootargs::install(t);
    let t = copy::install(t);
    let t = present12::install_cmdlist(t);
    let t = misc::install_cmdlist(t);
    seal(t);

    // SAFETY: as `fill_device_core`.
    unsafe { publish("COMMAND_LIST_3D", table, bytes, &built) };
    S_OK
}

/// # Safety
/// As [`fill`].
unsafe fn fill_command_queue(table: *mut c_void, bytes: usize) -> Hresult {
    let mut built = noop12::command_queue::stubbed_table();

    let t = Filling::<CommandQueueTable, stage::Stubbed> {
        table: &mut built,
        _stage: PhantomData,
    };
    let t = queue::install_queue(t);
    seal(t);

    // SAFETY: as `fill_device_core`.
    unsafe { publish("COMMAND_QUEUE_CORE", table, bytes, &built) };
    S_OK
}

// Extended-feature negotiation is part of ordinary device creation. The old
// signature-erased fallback returned S_OK without writing the feature count.
// Advertise an explicitly empty set and reject requests to enable absent features.
static EXTENDED_FEATURE_BAD_ARG: RefusalCounter = RefusalCounter::new("ExtendedFeatureBadArg");
static EXTENDED_FEATURE_UNSUPPORTED: RefusalCounter =
    RefusalCounter::new("ExtendedFeatureUnsupported");
pub(crate) static REFUSALS: &[&RefusalCounter] =
    &[&EXTENDED_FEATURE_BAD_ARG, &EXTENDED_FEATURE_UNSUPPORTED];

/// # Safety
/// A non-null `count` points to the runtime's writable feature-count word.
unsafe fn empty_extended_features(count: *mut ddi12::UINT32) -> ddi12::HRESULT {
    if count.is_null() {
        note_refusal(&EXTENDED_FEATURE_BAD_ARG);
        return E_INVALIDARG;
    }
    // SAFETY: writable count per the DDI contract; an empty set has no entries.
    unsafe { count.write(0) };
    S_OK
}

/// # Safety
/// `count` is writable when non-null; an empty feature set never reads `features`.
unsafe extern "system" fn get_extended_features_0020(
    _device: ddi12::D3D12DDI_HDEVICE,
    count: *mut ddi12::UINT32,
    _features: *mut ddi12::D3D12DDI_FEATURE_0020,
) -> ddi12::HRESULT {
    // SAFETY: forwarded unchanged from the runtime.
    unsafe { empty_extended_features(count) }
}

/// # Safety
/// As `get_extended_features_0020`; the highest feature index cannot add support.
unsafe extern "system" fn get_extended_features_0096(
    _device: ddi12::D3D12DDI_HDEVICE,
    _highest: ddi12::D3D12DDI_FEATURE_0020,
    count: *mut ddi12::UINT32,
    _features: *mut ddi12::D3D12DDI_FEATURE_0020,
) -> ddi12::HRESULT {
    // SAFETY: forwarded unchanged from the runtime.
    unsafe { empty_extended_features(count) }
}

/// # Safety
/// A non-null `count` is writable; no version entries exist.
unsafe extern "system" fn get_extended_feature_versions(
    _device: ddi12::D3D12DDI_HDEVICE,
    _feature: ddi12::D3D12DDI_FEATURE_0020,
    count: *mut ddi12::UINT32,
    _versions: *mut ddi12::UINT32,
) -> ddi12::HRESULT {
    note_refusal(&EXTENDED_FEATURE_UNSUPPORTED);
    // SAFETY: forwarded unchanged from the runtime.
    let hr = unsafe { empty_extended_features(count) };
    if hr == S_OK {
        E_INVALIDARG
    } else {
        hr
    }
}

/// STUB: no extended features are advertised, so none can be enabled.
/// # Safety
/// No handle or pointer is dereferenced.
unsafe extern "system" fn enable_extended_feature(
    _device: ddi12::D3D12DDI_HDEVICE,
    _feature: ddi12::D3D12DDI_FEATURE_0020,
    _version: ddi12::UINT32,
) -> ddi12::HRESULT {
    note_refusal(&EXTENDED_FEATURE_UNSUPPORTED);
    E_INVALIDARG
}

/// STUB: no enabled extended feature owns a callback table.
/// # Safety
/// No handle or pointer is dereferenced.
unsafe extern "system" fn set_extended_feature_callbacks(
    _device: ddi12::D3D12DDI_HDEVICE,
    _table: ddi12::D3D12DDI_TABLE_TYPE,
    _data: *const c_void,
    _bytes: ddi12::SIZE_T,
) -> ddi12::HRESULT {
    note_refusal(&EXTENDED_FEATURE_UNSUPPORTED);
    E_NOTIMPL
}
