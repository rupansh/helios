//! Signature-preserving fallback handlers for the three D3D12 DDI tables.
//!
//! The typed field drives the ABI, argument list and return convention of each
//! fallback, including x86 stdcall cleanup and aggregate returns. Slot names and
//! ordinals are checked against bindgen field offsets on each architecture.
//! Real handlers overwrite these fallbacks before a table is published; any
//! remaining fallback hit is named, counted and included in the adapter summary.

use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use helios_umd_common::noop::{log_backtrace, StubReport};

use crate::log_error;

/// `D3D12DDI_TABLE_TYPE_DEVICE_CORE`, as a flat-array table index.
///
/// ⚠ Deliberately **not** the `D3D12DDI_TABLE_TYPE` enumerator value: those are
/// 0, 1, 2 today, but they are the runtime's numbering of 25 table types
/// (`DDI_REFERENCE.md` §2.1) and only three of them are ours. Using our own
/// dense index keeps [`HITS`] at 206 entries instead of a sparse 28.
pub(crate) const TABLE_DEVICE_CORE: usize = 0;
/// `D3D12DDI_TABLE_TYPE_COMMAND_LIST_3D`, as a flat-array table index.
pub(crate) const TABLE_COMMAND_LIST: usize = 1;
/// `D3D12DDI_TABLE_TYPE_COMMAND_QUEUE_3D`, as a flat-array table index.
pub(crate) const TABLE_COMMAND_QUEUE: usize = 2;
const TABLE_COUNT: usize = 3;

/// One table's identity for the readout: its name and where its slots start in
/// the flat [`HITS`] array.
struct TableInfo {
    name: &'static str,
    base: usize,
    slots: &'static [&'static str],
}

// The queue's two reserved void* fields have no callable signature. The
// generated table's Default already initializes them to null; only
// queue::install_queue assigns their counted non-returning traps.
macro_rules! install_counting_slot {
    ($table:ident, pfnUnused, $report:ty) => {};
    ($table:ident, pfnUnused2, $report:ty) => {};
    ($table:ident, $slot:ident, $report:ty) => {
        helios_umd_common::noop::install_stub::<$report, _>(&mut $table.$slot)
    };
}

/// Emit one table's typed constructor and compile-time field-order proof.
macro_rules! ddi_noop_table {
    (
        $(#[$meta:meta])*
        $name:ident, $table:ty, $table_id:ident, [ $($slot:ident),* $(,)? ]
    ) => {
        $(#[$meta])*
        pub(crate) mod $name {
            use super::SlotReport;
            use crate::ddi12;

            /// The slot names, in header order — the readout's labels.
            pub(crate) const NAMES: &[&str] = &[ $( stringify!($slot) ),* ];

            /// Build every fallback through its actual bindgen function type.
            pub(crate) fn stubbed_table() -> $table {
                let mut table = <$table>::default();
                $(install_counting_slot!(table, $slot, SlotReport<
                    { super::$table_id },
                    { ::core::mem::offset_of!($table, $slot)
                        / ::core::mem::size_of::<usize>() },
                >);)*
                table
            }

            /// ⭐ THE ABI-ORDER PROOF. See the module doc.
            const OFFSETS: &[usize] = &[ $( ::core::mem::offset_of!($table, $slot) ),* ];
            const _: () = {
                assert!(
                    OFFSETS.len()
                        == ::core::mem::size_of::<$table>() / ::core::mem::size_of::<usize>(),
                    "slot list length does not match the bindgen struct's slot count"
                );
                let mut i = 0;
                while i < OFFSETS.len() {
                    assert!(
                        OFFSETS[i] == i * ::core::mem::size_of::<usize>(),
                        "slot list is not in the bindgen struct's field order"
                    );
                    i += 1;
                }
            };
        }
    };
}

// ── The three driver-side DDI tables, 206 slots ────────────────────────────
//
// ⭐ The slot lists below were EXTRACTED from `umd12/bindgen/cached/d3d12umddi.rs`
// by parsing the struct bodies, and the `const _` assertions inside the macro
// re-check that extraction on every build: a misspelled name does not compile,
// and a duplicated, reordered, missing or extra name fails the order proof.
//
// 124 + 75 + 7 = 206, plus the 8 adapter slots `adapter12` fills = **214**
// (`DECISIONS.md` §4.1, the canonical count table).

ddi_noop_table! {
    /// `D3D12DDI_DEVICE_FUNCS_CORE_0109` — **124** slots.
    device_core, ddi12::D3D12DDI_DEVICE_FUNCS_CORE_0109, TABLE_DEVICE_CORE, [
        pfnCheckFormatSupport, pfnCheckMultisampleQualityLevels, pfnGetMipPacking,
        pfnCalcPrivateElementLayoutSize, pfnCreateElementLayout, pfnDestroyElementLayout,
        pfnCalcPrivateBlendStateSize, pfnCreateBlendState, pfnDestroyBlendState,
        pfnCalcPrivateDepthStencilStateSize, pfnCreateDepthStencilState,
        pfnDestroyDepthStencilState, pfnCalcPrivateRasterizerStateSize,
        pfnCreateRasterizerState, pfnDestroyRasterizerState, pfnCalcPrivateShaderSize,
        pfnCreateVertexShader, pfnCreatePixelShader, pfnCreateGeometryShader,
        pfnCreateComputeShader, pfnCalcPrivateGeometryShaderWithStreamOutput,
        pfnCreateGeometryShaderWithStreamOutput, pfnCalcPrivateTessellationShaderSize,
        pfnCreateHullShader, pfnCreateDomainShader, pfnDestroyShader,
        pfnCalcPrivateCommandQueueSize, pfnCreateCommandQueue, pfnDestroyCommandQueue,
        pfnCalcPrivateCommandPoolSize, pfnCreateCommandPool, pfnDestroyCommandPool,
        pfnResetCommandPool, pfnCalcPrivatePipelineStateSize, pfnCreatePipelineState,
        pfnDestroyPipelineState, pfnCalcPrivateCommandListSize, pfnCreateCommandList,
        pfnDestroyCommandList, pfnCalcPrivateFenceSize, pfnCreateFence, pfnDestroyFence,
        pfnCalcPrivateDescriptorHeapSize, pfnCreateDescriptorHeap, pfnDestroyDescriptorHeap,
        pfnGetDescriptorSizeInBytes, pfnGetCPUDescriptorHandleForHeapStart,
        pfnGetGPUDescriptorHandleForHeapStart, pfnCreateShaderResourceView,
        pfnCreateConstantBufferView, pfnCreateSampler, pfnCreateUnorderedAccessView,
        pfnCreateRenderTargetView, pfnCreateDepthStencilView, pfnCalcPrivateRootSignatureSize,
        pfnCreateRootSignature, pfnDestroyRootSignature, pfnMapHeap, pfnUnmapHeap,
        pfnCalcPrivateHeapAndResourceSizes, pfnCreateHeapAndResource, pfnDestroyHeapAndResource,
        pfnMakeResident, pfnEvict, pfnCalcPrivateOpenedHeapAndResourceSizes,
        pfnOpenHeapAndResource, pfnCopyDescriptors, pfnCopyDescriptorsSimple,
        pfnCalcPrivateQueryHeapSize, pfnCreateQueryHeap, pfnDestroyQueryHeap,
        pfnCalcPrivateCommandSignatureSize, pfnCreateCommandSignature,
        pfnDestroyCommandSignature, pfnCheckResourceVirtualAddress,
        pfnCheckResourceAllocationInfo, pfnCheckSubresourceInfo,
        pfnCheckExistingResourceAllocationInfo, pfnOfferResources, pfnReclaimResources,
        pfnGetImplicitPhysicalAdapterMask, pfnGetPresentPrivateDriverDataSize, pfnQueryNodeMap,
        pfnRetrieveShaderComment, pfnCheckResourceAllocationHandle,
        pfnCalcPrivatePipelineLibrarySize, pfnCreatePipelineLibrary, pfnDestroyPipelineLibrary,
        pfnAddPipelineStateToLibrary, pfnCalcSerializedLibrarySize, pfnSerializeLibrary,
        pfnGetDebugAllocationInfo, pfnCalcPrivateCommandRecorderSize, pfnCreateCommandRecorder,
        pfnDestroyCommandRecorder, pfnCommandRecorderSetCommandPoolAsTarget,
        pfnCalcPrivateSchedulingGroupSize, pfnCreateSchedulingGroup, pfnDestroySchedulingGroup,
        pfnEnumerateMetaCommands, pfnEnumerateMetaCommandParameters,
        pfnCalcPrivateMetaCommandSize, pfnCreateMetaCommand, pfnDestroyMetaCommand,
        pfnGetMetaCommandRequiredParameterInfo, pfnCalcPrivateStateObjectSize,
        pfnCreateStateObject, pfnDestroyStateObject,
        pfnGetRaytracingAccelerationStructurePrebuildInfo, pfnCheckDriverMatchingIdentifier,
        pfnGetShaderIdentifier, pfnGetShaderStackSize, pfnGetPipelineStackSize,
        pfnSetPipelineStackSize, pfnSetBackgroundProcessingMode,
        pfnCalcPrivateAddToStateObjectSize, pfnAddToStateObject,
        pfnCreateSamplerFeedbackUnorderedAccessView, pfnCreateAmplificationShader,
        pfnCreateMeshShader, pfnCalcPrivateMeshShaderSize, pfnImplicitShaderCacheControl,
        pfnGetProgramIdentifier, pfnGetWorkGraphMemoryRequirements,
    ]
}

ddi_noop_table! {
    /// `D3D12DDI_COMMAND_LIST_FUNCS_3D_0108` — **75** slots.
    command_list, ddi12::D3D12DDI_COMMAND_LIST_FUNCS_3D_0108, TABLE_COMMAND_LIST, [
        pfnCloseCommandList, pfnResetCommandList, pfnDrawInstanced, pfnDrawIndexedInstanced,
        pfnDispatch, pfnClearUnorderedAccessViewUint, pfnClearUnorderedAccessViewFloat,
        pfnClearRenderTargetView, pfnClearDepthStencilView, pfnDiscardResource,
        pfnCopyTextureRegion, pfnResourceCopy, pfnCopyTiles, pfnCopyBufferRegion,
        pfnResourceResolveSubresource, pfnExecuteBundle, pfnExecuteIndirect, pfnResourceBarrier,
        pfnBlt, pfnPresent, pfnBeginQuery, pfnEndQuery, pfnResolveQueryData, pfnSetPredication,
        pfnIaSetTopology, pfnRsSetViewports, pfnRsSetScissorRects, pfnOmSetBlendFactor,
        pfnOmSetStencilRef, pfnSetPipelineState, pfnSetDescriptorHeaps,
        pfnSetComputeRootSignature, pfnSetGraphicsRootSignature,
        pfnSetComputeRootDescriptorTable, pfnSetGraphicsRootDescriptorTable,
        pfnSetComputeRoot32BitConstant, pfnSetGraphicsRoot32BitConstant,
        pfnSetComputeRoot32BitConstants, pfnSetGraphicsRoot32BitConstants,
        pfnSetComputeRootConstantBufferView, pfnSetGraphicsRootConstantBufferView,
        pfnSetComputeRootShaderResourceView, pfnSetGraphicsRootShaderResourceView,
        pfnSetComputeRootUnorderedAccessView, pfnSetGraphicsRootUnorderedAccessView,
        pfnIASetIndexBuffer, pfnIASetVertexBuffers, pfnSOSetTargets, pfnOMSetRenderTargets,
        pfnSetMarker, pfnClearRootArguments, pfnAtomicCopyBufferRegion, pfnOMSetDepthBounds,
        pfnSetSamplePositions, pfnResourceResolveSubresourceRegion,
        pfnSetProtectedResourceSession, pfnWriteBufferImmediate, pfnSetViewInstanceMask,
        pfnInitializeMetaCommand, pfnExecuteMetaCommand,
        pfnBuildRaytracingAccelerationStructure,
        pfnEmitRaytracingAccelerationStructurePostbuildInfo,
        pfnCopyRaytracingAccelerationStructure, pfnSetPipelineState1, pfnDispatchRays,
        pfnRSSetShadingRate, pfnRSSetShadingRateImage, pfnDispatchMesh, pfnBarrier,
        pfnOmSetAlphaBlendFactor, pfnOmSetFrontAndBackStencilRef, pfnRSSetDepthBias,
        pfnIASetIndexBufferStripCutValue, pfnSetProgram, pfnDispatchGraph,
    ]
}

ddi_noop_table! {
    /// `D3D12DDI_COMMAND_QUEUE_FUNCS_CORE_0001` — **7** slots, two of which the
    /// header itself names `pfnUnused` / `pfnUnused2`. They are stubbed like
    /// every other slot: "unused" is the header's word for them, and a NULL
    /// there is still a NULL the runtime could call through.
    command_queue, ddi12::D3D12DDI_COMMAND_QUEUE_FUNCS_CORE_0001, TABLE_COMMAND_QUEUE, [
        pfnExecuteCommandLists, pfnUnused, pfnUnused2, pfnUpdateTileMappings,
        pfnCopyTileMappings, pfnSignalFence, pfnWaitForFence,
    ]
}

// ── The flat hit array ─────────────────────────────────────────────────────

const DEVICE_CORE_BASE: usize = 0;
const COMMAND_LIST_BASE: usize = DEVICE_CORE_BASE + device_core::NAMES.len();
const COMMAND_QUEUE_BASE: usize = COMMAND_LIST_BASE + command_list::NAMES.len();
/// 124 + 75 + 7. Derived, then checked against the canonical count table.
pub(crate) const TOTAL_SLOTS: usize = COMMAND_QUEUE_BASE + command_queue::NAMES.len();

// `DECISIONS.md` §4.1 is the only trustworthy count table in this directory, and
// it says 8 + 124 + 75 + 7 = 214. The 8 are `adapter12`'s. If this ever fails,
// the SDK header moved and §4.1 is what has to be re-derived — not this line.
const _: () = assert!(TOTAL_SLOTS == 206);

static HITS: [AtomicU32; TOTAL_SLOTS] = [const { AtomicU32::new(0) }; TOTAL_SLOTS];

static TABLES: [TableInfo; TABLE_COUNT] = [
    TableInfo {
        name: "DEVICE_FUNCS_CORE_0109",
        base: DEVICE_CORE_BASE,
        slots: device_core::NAMES,
    },
    TableInfo {
        name: "COMMAND_LIST_FUNCS_3D_0108",
        base: COMMAND_LIST_BASE,
        slots: command_list::NAMES,
    },
    TableInfo {
        name: "COMMAND_QUEUE_FUNCS_CORE_0001",
        base: COMMAND_QUEUE_BASE,
        slots: command_queue::NAMES,
    },
];

/// Invalid internal table/slot identities. Expected zero: every identity comes
/// from the compiler-checked tables below, never from a runtime byte count.
static UNKNOWN_SLOT_HITS: AtomicUsize = AtomicUsize::new(0);

/// How many first-hit backtraces the whole process may spend.
///
/// A backtrace names the *runtime call* that reached an unimplemented slot,
/// which is the difference between "this DDI is missing" and "this DDI is
/// missing **and something actually calls it**". Capturing and formatting 32
/// frames is itself a measured cost (`umd_common::noop`), and with 206 slots an
/// uncapped budget would be a log flood on the first real workload — so the
/// budget buys the first few, which are the informative ones.
const BACKTRACE_BUDGET: usize = 8;
static BACKTRACES_SPENT: AtomicUsize = AtomicUsize::new(0);

/// One reporter per ABI-derived slot; the common helper supplies its exact
/// function signature. Default returns retain the existing counted-fallback
/// behavior for slots not replaced by a subsystem's implementation.
pub(crate) struct SlotReport<const TABLE: usize, const SLOT: usize>;
impl<R: Default, const TABLE: usize, const SLOT: usize> StubReport<R> for SlotReport<TABLE, SLOT> {
    fn hit() -> R {
        note_slot_hit(TABLE, SLOT);
        R::default()
    }
}

/// Count one slot hit and, on its first, say which slot it was.
fn note_slot_hit(table: usize, slot: usize) {
    let (Some(info), _) = (TABLES.get(table), ()) else {
        UNKNOWN_SLOT_HITS.fetch_add(1, Ordering::Relaxed);
        return;
    };
    let (Some(name), Some(hits)) = (info.slots.get(slot), HITS.get(info.base + slot)) else {
        UNKNOWN_SLOT_HITS.fetch_add(1, Ordering::Relaxed);
        return;
    };
    if hits.fetch_add(1, Ordering::Relaxed) != 0 {
        return;
    }
    log_error!("noop DDI first hit: {}::{name}", info.name);
    if BACKTRACES_SPENT.fetch_add(1, Ordering::Relaxed) < BACKTRACE_BUDGET {
        // SAFETY: `log_backtrace` calls `RtlCaptureStackBackTrace`, which walks
        // the caller's stack. This runs on an ordinary user-mode DDI thread with
        // no unwind in progress — the crate is `panic = "abort"`, so there is no
        // unwinding anywhere in this module.
        unsafe { log_backtrace(&format!("noop DDI {}::{name}", info.name)) };
    }
}

/// Log every slot that was hit, and nothing about the ones that were not.
///
/// ⭐ This is the instrument `PARALLEL.md` §9.2 and `CONFORMANCE.md` are written
/// against, and T5's lesson is why it exists as a *readout* and not merely as an
/// array of atomics: *an instrument nothing can read is not an instrument* —
/// three of the four R806/R809 scan-out counters were process-global atomics
/// nothing ever loaded, so ROADMAP's own instruction to read them after a gate
/// run was not executable.
///
/// Called from `adapter12::close_adapter`, beside the refusal summary.
pub(crate) fn log_noop_hits() {
    let mut line = String::with_capacity(512);
    let mut hit_slots = 0usize;
    for info in TABLES.iter() {
        for (slot, name) in info.slots.iter().enumerate() {
            let Some(hits) = HITS.get(info.base + slot) else {
                continue;
            };
            let n = hits.load(Ordering::Relaxed);
            if n == 0 {
                continue;
            }
            hit_slots += 1;
            line.push(' ');
            line.push_str(name);
            line.push('=');
            line.push_str(&n.to_string());
        }
    }
    log_error!(
        "D3D12 noop DDI hits: slots={hit_slots}/{TOTAL_SLOTS} unknown={}{line}",
        UNKNOWN_SLOT_HITS.load(Ordering::Relaxed),
    );
}
