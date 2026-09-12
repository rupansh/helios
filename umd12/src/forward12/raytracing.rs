//! Native DXR translation. The runtime owns validation/admission and resolves
//! subobject associations; vkd3d owns compilation, acceleration structures and
//! GPU recording. Neither boundary may be replaced by a successful noop.
//!
//! DDI sources: d3d12umddi.h _0054 state objects and RT commands, _0072
//! additions, and _0075 pipeline config. Native RT1.0 consumes the shorter
//! _0054 config payload even when the negotiated interface is _0110.
//! https://microsoft.github.io/DirectX-Specs/d3d/Raytracing.html
//!
//! DDI GPUVAs are UINT64 values originally returned by this driver's
//! CheckResourceVirtualAddress -> engine GetGPUVirtualAddress. Instance records,
//! root descriptors and shader tables consequently use the same engine address
//! domain. Only DDI object handles are decoded; none is cast to an API GPUVA.
//!
//! Native caps remain a separate admission decision. Tools visualization is
//! explicitly refused because the engine has no inverse-AS implementation.
//! Serialization uses the stock Vulkan serialization format, including its
//! compatibility check and GPU-side pointer relocation. A forwarded DXR call
//! is not proof of full DXR conformance or Port Royal's visual correctness.

use core::ffi::c_void;
use core::mem::{align_of, size_of, ManuallyDrop};
use std::collections::{HashMap, HashSet};

use helios_umd_common::hr::{Hresult, E_FAIL, E_INVALIDARG, E_NOTIMPL, E_OUTOFMEMORY, S_OK};
use helios_umd_common::refusals::RefusalCounter;
use windows::core::{Interface, PCWSTR};
use windows::Win32::Graphics::Direct3D::ID3DBlob;
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::*;

use super::{budget, L9_REFUSALS, RT_LOG};
use crate::forward12::{pso, queue};
use crate::{bridge12, ddi12, device12, log_error, note_refusal};

#[derive(Clone, Copy)]
struct RtError {
    hr: Hresult,
    reason: &'static str,
}
type RtResult<T> = Result<T, RtError>;

fn error(hr: Hresult, reason: &'static str) -> RtError {
    RtError { hr, reason }
}

fn engine_error(e: windows::core::Error) -> RtError {
    error(e.code().0, "engine rejected DXR operation")
}

fn record_error(counter: &RefusalCounter, e: RtError) {
    if e.hr == E_OUTOFMEMORY {
        // Preserve the HRESULT/callback path under persistent allocation
        // failure. The existing summary readouts report this counter later.
        counter.bump();
        return;
    }
    note_refusal(counter);
    if let Some(n) = budget(&RT_LOG) {
        log_error!("DXR: {} hr={:#010x} (x{})", e.reason, e.hr as u32, n + 1);
    }
}

fn reserve<T>(v: &mut Vec<T>, count: usize) -> RtResult<()> {
    v.try_reserve(count)
        .map_err(|_| error(E_OUTOFMEMORY, "DXR temporary allocation failed"))
}

fn boxed<T>(value: T) -> RtResult<Box<T>> {
    let layout = std::alloc::Layout::new::<T>();
    // All callers box non-zero-sized state/descriptor types.
    // SAFETY: the layout is for T; allocation failure is tested before write.
    let ptr = unsafe { std::alloc::alloc(layout).cast::<T>() };
    if ptr.is_null() {
        return Err(error(E_OUTOFMEMORY, "DXR object allocation failed"));
    }
    // SAFETY: allocation has T's size/alignment and is uniquely owned. It is
    // initialized once and transferred to Box with the identical allocator.
    unsafe {
        ptr.write(value);
        Ok(Box::from_raw(ptr))
    }
}

/// Borrow only the declared DDI arm. The runtime guarantees live backing for
/// the indicated count; count arithmetic and null/alignment are checked here.
/// SAFETY: non-null ptr has count readable T values in one allocation, and that
/// allocation remains live and unmodified for the returned borrow's lifetime.
unsafe fn array<'a, T>(ptr: *const T, count: u32) -> RtResult<&'a [T]> {
    if count == 0 {
        return Ok(&[]);
    }
    let bytes = (count as usize).checked_mul(size_of::<T>());
    if ptr.is_null()
        || !(ptr as usize).is_multiple_of(align_of::<T>())
        || bytes.is_none_or(|n| n > isize::MAX as usize)
    {
        return Err(error(E_INVALIDARG, "invalid DXR pointer/count"));
    }
    // SAFETY: checked arithmetic/alignment; caller guarantees the SAL extent.
    Ok(unsafe { core::slice::from_raw_parts(ptr, count as usize) })
}

/// SAFETY: non-null ptr addresses a readable T that outlives the returned borrow.
unsafe fn input<'a, T>(ptr: *const T) -> RtResult<&'a T> {
    // SAFETY: the caller guarantees one readable T if the pointer is non-null.
    unsafe { array(ptr, 1) }?
        .first()
        .ok_or(error(E_INVALIDARG, "missing DXR descriptor"))
}

/// The runtime supplies terminated UTF-16 strings. A finite scan also prevents
/// an unterminated malformed descriptor from being forwarded to the compiler.
/// SAFETY: non-null ptr is readable through its terminating UTF-16 zero; the
/// string outlives every use of the returned borrowed PCWSTR.
unsafe fn name(ptr: *const u16, optional: bool) -> RtResult<PCWSTR> {
    if ptr.is_null() {
        return if optional {
            Ok(PCWSTR::null())
        } else {
            Err(error(E_INVALIDARG, "missing DXR export name"))
        };
    }
    for i in 0..32768 {
        // SAFETY: caller guarantees a terminated runtime-owned string. The
        // explicit bound limits malformed input work without changing its bytes.
        if unsafe { ptr.add(i).read_unaligned() } == 0 {
            return if i != 0 {
                Ok(PCWSTR(ptr))
            } else {
                Err(error(E_INVALIDARG, "empty DXR export name"))
            };
        }
    }
    Err(error(
        E_INVALIDARG,
        "DXR export name exceeds supported string extent",
    ))
}

struct StateObject {
    engine: ID3D12StateObject,
    properties: ID3D12StateObjectProperties,
    h_device: ddi12::D3D12DDI_HDEVICE,
    kind: D3D12_STATE_OBJECT_TYPE,
    // Explicit API exports replace internal/mangled names in vkd3d. Retain
    // their public namespace for later implicit collection imports and Add.
    explicit_exports: HashSet<Vec<u16>>,
}

fn valid_slot(h: ddi12::D3D12DDI_HSTATEOBJECT_0054) -> bool {
    !h.pDrvPrivate.is_null()
        && (h.pDrvPrivate as usize).is_multiple_of(align_of::<*mut StateObject>())
}

/// Typed, one-word runtime private storage. Accessors borrow for one DDI call;
/// Destroy is the only owner release. The runtime retains collection ancestors
/// and AddToStateObject families as specified under "State object lifetimes as
/// seen by driver"; vkd3d also retains the corresponding internal collections.
/// SAFETY: h names runtime private storage initialized by this lane; its live
/// StateObject cannot be destroyed until the returned borrow has expired.
unsafe fn state<'a>(h: ddi12::D3D12DDI_HSTATEOBJECT_0054) -> RtResult<&'a StateObject> {
    if !valid_slot(h) {
        return Err(error(E_INVALIDARG, "null DXR state-object handle"));
    }
    // SAFETY: the runtime supplies the private block sized by CalcPrivate; it
    // contains only our StateObject pointer, initialized even on create failure.
    let ptr = unsafe { h.pDrvPrivate.cast::<*mut StateObject>().read() };
    // SAFETY: non-null stored pointers own StateObject until Destroy; the
    // returned borrow is restricted by the caller to this live DDI invocation.
    unsafe { ptr.as_ref() }.ok_or(error(E_INVALIDARG, "uninitialized DXR state object"))
}

/// SAFETY: h addresses the writable private block sized by CalcPrivate; it does
/// not contain a previously published object whose ownership would be lost.
unsafe fn clear_slot(h: ddi12::D3D12DDI_HSTATEOBJECT_0054) -> RtResult<()> {
    if !valid_slot(h) {
        return Err(error(E_INVALIDARG, "null DXR state-object private slot"));
    }
    // SAFETY: runtime sized the block for a pointer; create owns initialization.
    unsafe {
        h.pDrvPrivate
            .cast::<*mut StateObject>()
            .write(core::ptr::null_mut())
    };
    Ok(())
}

/// SAFETY: h is the writable pointer slot successfully validated and cleared by
/// clear_slot in the current create call; no concurrent access can publish it.
unsafe fn publish(h: ddi12::D3D12DDI_HSTATEOBJECT_0054, value: StateObject) -> RtResult<()> {
    let value = boxed(value)?;
    // SAFETY: create validated and cleared this runtime-owned pointer slot.
    unsafe {
        h.pDrvPrivate
            .cast::<*mut StateObject>()
            .write(Box::into_raw(value))
    };
    Ok(())
}

/// SAFETY: non-null h names initialized Device12 storage held live by the DDI
/// caller while this function acquires its independently retained engine COM ref.
unsafe fn device5(h: ddi12::D3D12DDI_HDEVICE) -> RtResult<ID3D12Device5> {
    // SAFETY: the DDI supplies a device that outlives this call and its objects.
    let dev = unsafe { device12::device(h) }.ok_or(error(E_INVALIDARG, "missing DXR device"))?;
    let engine = dev
        .engine
        .d3d12_device()
        .ok_or(error(E_FAIL, "missing engine device"))?;
    engine.cast().map_err(engine_error)
}

fn require_engine_dxr(engine: &ID3D12Device5) -> RtResult<()> {
    let mut options = D3D12_FEATURE_DATA_D3D12_OPTIONS5::default();
    // SAFETY: exact API feature-data struct and matching size, writable for call.
    unsafe {
        engine.CheckFeatureSupport(
            D3D12_FEATURE_D3D12_OPTIONS5,
            core::ptr::from_mut(&mut options).cast(),
            size_of::<D3D12_FEATURE_DATA_D3D12_OPTIONS5>() as u32,
        )
    }
    .map_err(engine_error)?;
    if options.RaytracingTier.0 < D3D12_RAYTRACING_TIER_1_0.0 {
        return Err(error(
            E_NOTIMPL,
            "engine has no DXR tier; command must not be dropped",
        ));
    }
    Ok(())
}

/// The DDI library starts with DXIL ProgramHeader, not the API DXBC container.
/// Reconstruct a one-part container; shader exports/associations are supplied
/// separately by the native runtime and do not require a fabricated RDAT part.
/// SAFETY: code addresses a runtime-validated DXIL ProgramHeader followed by its
/// declared SizeInUint32 words, all readable during the current create call.
unsafe fn library_container(code: *const u32) -> RtResult<Vec<u8>> {
    // SAFETY: DDI library bytecode has at least the two-word length header.
    let header = unsafe { array(code, 2) }?;
    let words = header[1];
    if header[0] >> 16 != 6 || words < 6 {
        return Err(error(
            E_INVALIDARG,
            "DXR library is not a raw DXIL library program",
        ));
    }
    let bytes = (words as usize)
        .checked_mul(4)
        .filter(|n| *n <= u32::MAX as usize - 44)
        .ok_or(error(
            E_INVALIDARG,
            "DXR library byte count overflows container",
        ))?;
    // SAFETY: the self-describing DDI library is words long; arithmetic checked.
    let words = unsafe { array(code, words) }?;
    if words[2] != u32::from_le_bytes(*b"DXIL") || words[4] < 16 {
        return Err(error(E_INVALIDARG, "invalid DXIL library bitcode header"));
    }
    let end = 8usize
        .checked_add(words[4] as usize)
        .and_then(|offset| offset.checked_add(words[5] as usize));
    if words[5] < 4 || end.is_none_or(|n| n > bytes) {
        return Err(error(
            E_INVALIDARG,
            "DXIL library bitcode exceeds declared program",
        ));
    }
    let mut blob = Vec::new();
    reserve(&mut blob, bytes + 44)?;
    blob.extend_from_slice(b"DXBC");
    // Engine parser ignores the DXBC checksum. DXIL validation was performed by
    // the native runtime before this DDI and the bitcode is preserved verbatim.
    blob.extend_from_slice(&[0; 16]);
    for word in [1, (bytes + 44) as u32, 1, 36] {
        blob.extend_from_slice(&word.to_le_bytes());
    }
    blob.extend_from_slice(b"DXIL");
    blob.extend_from_slice(&(bytes as u32).to_le_bytes());
    for word in words {
        blob.extend_from_slice(&word.to_le_bytes());
    }
    Ok(blob)
}

/// SAFETY: ptr addresses count readable export descriptors, whose UTF-16 strings
/// remain readable and terminated until the receiving CreateStateObject returns.
unsafe fn exports(
    ptr: *const ddi12::D3D12DDI_EXPORT_DESC_0054,
    count: u32,
) -> RtResult<Vec<D3D12_EXPORT_DESC>> {
    // SAFETY: DDI count/pointer pair; each string remains live for this create.
    let src = unsafe { array(ptr, count) }?;
    let mut out = Vec::new();
    reserve(&mut out, src.len())?;
    for e in src {
        if e.Flags != 0 {
            return Err(error(E_INVALIDARG, "unknown DXR export flags"));
        }
        // SAFETY: strings are owned by the current runtime create descriptor.
        out.push(unsafe {
            D3D12_EXPORT_DESC {
                Name: name(e.Name, false)?,
                ExportToRename: name(e.ExportToRename, true)?,
                Flags: D3D12_EXPORT_FLAG_NONE,
            }
        });
    }
    Ok(out)
}

/// Pointer-bearing API payloads are exposed only after the arena is complete,
/// so their addresses remain stable until Create returns.
/// vkd3d's deferred collections deep-copy DXIL/hit groups/export strings and
/// retain root signatures; no pointer to this arena escapes the engine call.
enum Payload {
    Config(D3D12_STATE_OBJECT_CONFIG),
    Global(D3D12_GLOBAL_ROOT_SIGNATURE),
    Local(D3D12_LOCAL_ROOT_SIGNATURE),
    Node(D3D12_NODE_MASK),
    Library(D3D12_DXIL_LIBRARY_DESC, Vec<u8>, Vec<D3D12_EXPORT_DESC>),
    Collection(D3D12_EXISTING_COLLECTION_DESC, Vec<D3D12_EXPORT_DESC>),
    Shader(D3D12_RAYTRACING_SHADER_CONFIG),
    Pipeline(D3D12_RAYTRACING_PIPELINE_CONFIG),
    Hit(D3D12_HIT_GROUP_DESC),
    Association(D3D12_SUBOBJECT_TO_EXPORTS_ASSOCIATION, usize, [PCWSTR; 1]),
}

impl Payload {
    fn api(&self) -> D3D12_STATE_SUBOBJECT {
        let (kind, ptr) = match self {
            Self::Config(p) => (
                D3D12_STATE_SUBOBJECT_TYPE_STATE_OBJECT_CONFIG,
                core::ptr::from_ref(p).cast(),
            ),
            Self::Global(p) => (
                D3D12_STATE_SUBOBJECT_TYPE_GLOBAL_ROOT_SIGNATURE,
                core::ptr::from_ref(p).cast(),
            ),
            Self::Local(p) => (
                D3D12_STATE_SUBOBJECT_TYPE_LOCAL_ROOT_SIGNATURE,
                core::ptr::from_ref(p).cast(),
            ),
            Self::Node(p) => (
                D3D12_STATE_SUBOBJECT_TYPE_NODE_MASK,
                core::ptr::from_ref(p).cast(),
            ),
            Self::Library(p, _, _) => (
                D3D12_STATE_SUBOBJECT_TYPE_DXIL_LIBRARY,
                core::ptr::from_ref(p).cast(),
            ),
            Self::Collection(p, _) => (
                D3D12_STATE_SUBOBJECT_TYPE_EXISTING_COLLECTION,
                core::ptr::from_ref(p).cast(),
            ),
            Self::Shader(p) => (
                D3D12_STATE_SUBOBJECT_TYPE_RAYTRACING_SHADER_CONFIG,
                core::ptr::from_ref(p).cast(),
            ),
            Self::Pipeline(p) => (
                D3D12_STATE_SUBOBJECT_TYPE_RAYTRACING_PIPELINE_CONFIG,
                core::ptr::from_ref(p).cast(),
            ),
            Self::Hit(p) => (
                D3D12_STATE_SUBOBJECT_TYPE_HIT_GROUP,
                core::ptr::from_ref(p).cast(),
            ),
            Self::Association(p, _, _) => (
                D3D12_STATE_SUBOBJECT_TYPE_SUBOBJECT_TO_EXPORTS_ASSOCIATION,
                core::ptr::from_ref(p).cast(),
            ),
        };
        D3D12_STATE_SUBOBJECT {
            Type: kind,
            pDesc: ptr,
        }
    }
}

impl Drop for Payload {
    fn drop(&mut self) {
        // SAFETY: these ManuallyDrop fields own exactly one cloned COM reference
        // each. No Payload clones exist and this is their single release site.
        unsafe {
            match self {
                Self::Global(p) => ManuallyDrop::drop(&mut p.pGlobalRootSignature),
                Self::Local(p) => ManuallyDrop::drop(&mut p.pLocalRootSignature),
                Self::Collection(p, _) => ManuallyDrop::drop(&mut p.pExistingCollection),
                _ => {}
            }
        }
    }
}

struct Translation {
    payloads: Vec<Payload>,
    index: HashMap<usize, usize>,
    api: Vec<D3D12_STATE_SUBOBJECT>,
    empty_global: Option<usize>,
    empty_local: Option<usize>,
    explicit_exports: HashSet<Vec<u16>>,
}

impl Translation {
    fn new() -> Self {
        Self {
            payloads: Vec::new(),
            index: HashMap::new(),
            api: Vec::new(),
            empty_global: None,
            empty_local: None,
            explicit_exports: HashSet::new(),
        }
    }

    fn retain_export(&mut self, units: &[u16]) -> RtResult<()> {
        if self.explicit_exports.contains(units) {
            return Ok(());
        }
        self.explicit_exports
            .try_reserve(1)
            .map_err(|_| error(E_OUTOFMEMORY, "DXR export namespace allocation failed"))?;
        let mut copy = Vec::new();
        reserve(&mut copy, units.len())?;
        copy.extend_from_slice(units);
        self.explicit_exports.insert(copy);
        Ok(())
    }

    fn inherit_exports(&mut self, object: &StateObject) -> RtResult<()> {
        for units in &object.explicit_exports {
            self.retain_export(units)?;
        }
        Ok(())
    }

    /// SAFETY: exports() has checked these runtime-owned terminated UTF-16
    /// names; each remains readable until this Create/Add call returns.
    unsafe fn retain_api_exports(&mut self, exports: &[D3D12_EXPORT_DESC]) -> RtResult<()> {
        for export in exports {
            // SAFETY: validated, non-null LPCWSTR supplied by the runtime.
            self.retain_export(unsafe { export.Name.as_wide() })?;
        }
        Ok(())
    }

    /// SAFETY: non-null names are validated terminated strings supplied by the
    /// current runtime summary; the returned pointer is borrowed for this call.
    unsafe fn summary_export(&self, mangled: PCWSTR, plain: PCWSTR) -> PCWSTR {
        // Explicit exports in vkd3d carry only their declared public Name:
        // libs/vkd3d-shader/dxil.c sets mangled_entry_point=NULL for that arm.
        // Prefer an exact declared name, preserving explicitly mangled exports
        // and renamed aliases. An unfiltered library retains both spellings,
        // so its fallback remains mangled to distinguish overloaded functions.
        for candidate in [mangled, plain] {
            if !candidate.is_null()
                // SAFETY: the current DDI summary owns this validated string.
                && self.explicit_exports.contains(unsafe { candidate.as_wide() })
            {
                return candidate;
            }
        }
        if mangled.is_null() {
            plain
        } else {
            mangled
        }
    }

    fn push(&mut self, payload: Payload) -> RtResult<usize> {
        reserve(&mut self.payloads, 1)?;
        let index = self.payloads.len();
        self.payloads.push(payload);
        Ok(index)
    }

    /// SAFETY: ptr and its type-selected descriptor arm belong to the current
    /// runtime create call; referenced roots/collections and h_device are live.
    unsafe fn subobject(
        &mut self,
        ptr: *const ddi12::D3D12DDI_STATE_SUBOBJECT_0054,
        h_device: ddi12::D3D12DDI_HDEVICE,
    ) -> RtResult<usize> {
        if let Some(index) = self.index.get(&(ptr as usize)) {
            return Ok(*index);
        }
        // SAFETY: runtime supplies either a top-level or summary-referenced
        // subobject, with exactly the descriptor arm selected by Type.
        let src = unsafe { input(ptr) }?;
        // All reads below are from the descriptor arm declared by that tag.
        // SAFETY: the runtime owns each arm, live until this create returns.
        let payload = unsafe {
            match src.Type {
                0 => {
                    let p = input(src.pDesc.cast::<ddi12::D3D12DDI_STATE_OBJECT_CONFIG_0054>())?;
                    if p.Flags & !7 != 0 {
                        return Err(error(E_INVALIDARG, "unknown state-object flags"));
                    }
                    Payload::Config(D3D12_STATE_OBJECT_CONFIG {
                        Flags: D3D12_STATE_OBJECT_FLAGS(p.Flags as i32),
                    })
                }
                1 => {
                    let p = input(
                        src.pDesc
                            .cast::<ddi12::D3D12DDI_GLOBAL_ROOT_SIGNATURE_0054>(),
                    )?;
                    let root = pso::root_signature(p.hGlobalRootSignature).ok_or(error(
                        E_INVALIDARG,
                        "unresolved global root-signature handle",
                    ))?;
                    Payload::Global(D3D12_GLOBAL_ROOT_SIGNATURE {
                        pGlobalRootSignature: ManuallyDrop::new(Some((*root).clone())),
                    })
                }
                2 => {
                    let p = input(
                        src.pDesc
                            .cast::<ddi12::D3D12DDI_LOCAL_ROOT_SIGNATURE_0054>(),
                    )?;
                    let root = pso::root_signature(p.hLocalRootSignature).ok_or(error(
                        E_INVALIDARG,
                        "unresolved local root-signature handle",
                    ))?;
                    Payload::Local(D3D12_LOCAL_ROOT_SIGNATURE {
                        pLocalRootSignature: ManuallyDrop::new(Some((*root).clone())),
                    })
                }
                3 => {
                    let p = input(src.pDesc.cast::<ddi12::D3D12DDI_NODE_MASK_0054>())?;
                    if p.NodeMask > 1 {
                        return Err(error(E_INVALIDARG, "DXR node mask names an absent adapter"));
                    }
                    Payload::Node(D3D12_NODE_MASK {
                        NodeMask: p.NodeMask,
                    })
                }
                5 => {
                    let p = input(src.pDesc.cast::<ddi12::D3D12DDI_DXIL_LIBRARY_DESC_0054>())?;
                    let blob = library_container(p.pDXILLibrary)?;
                    let exports = exports(p.pExports, p.NumExports)?;
                    self.retain_api_exports(&exports)?;
                    Payload::Library(
                        D3D12_DXIL_LIBRARY_DESC {
                            DXILLibrary: D3D12_SHADER_BYTECODE {
                                pShaderBytecode: blob.as_ptr().cast(),
                                BytecodeLength: blob.len(),
                            },
                            NumExports: p.NumExports,
                            pExports: exports.as_ptr(),
                        },
                        blob,
                        exports,
                    )
                }
                6 => {
                    let p = input(
                        src.pDesc
                            .cast::<ddi12::D3D12DDI_EXISTING_COLLECTION_DESC_0054>(),
                    )?;
                    let collection = state(p.hExistingCollection)?;
                    if collection.kind != D3D12_STATE_OBJECT_TYPE_COLLECTION
                        || collection.h_device.pDrvPrivate != h_device.pDrvPrivate
                    {
                        return Err(error(
                            E_INVALIDARG,
                            "existing collection has wrong type or device",
                        ));
                    }
                    let exports = exports(p.pExports, p.NumExports)?;
                    if p.NumExports == 0 {
                        self.inherit_exports(collection)?;
                    } else {
                        self.retain_api_exports(&exports)?;
                    }
                    Payload::Collection(
                        D3D12_EXISTING_COLLECTION_DESC {
                            pExistingCollection: ManuallyDrop::new(Some(collection.engine.clone())),
                            NumExports: p.NumExports,
                            pExports: exports.as_ptr(),
                        },
                        exports,
                    )
                }
                9 => {
                    let p = input(
                        src.pDesc
                            .cast::<ddi12::D3D12DDI_RAYTRACING_SHADER_CONFIG_0054>(),
                    )?;
                    if p.MaxAttributeSizeInBytes > D3D12_RAYTRACING_MAX_ATTRIBUTE_SIZE_IN_BYTES {
                        return Err(error(E_INVALIDARG, "DXR attributes exceed API limit"));
                    }
                    Payload::Shader(D3D12_RAYTRACING_SHADER_CONFIG {
                        MaxPayloadSizeInBytes: p.MaxPayloadSizeInBytes,
                        MaxAttributeSizeInBytes: p.MaxAttributeSizeInBytes,
                    })
                }
                10 => {
                    // Native caps expose RT1.0. DDI _0110 negotiation alone
                    // does not extend this payload to the RT1.1 _0075 form:
                    // on runtime 26100.9278 Port Royal's valid depth=1 has
                    // unrelated bytes (e.g. 0x6c617645) after the _0054 UINT.
                    // Consume exactly the RT1.0 payload and forward the API's
                    // depth-only CONFIG. RT1.1/CONFIG1 needs separate admission
                    // and runtime payload validation before reading Flags.
                    let p = input(
                        src.pDesc
                            .cast::<ddi12::D3D12DDI_RAYTRACING_PIPELINE_CONFIG_0054>(),
                    )?;
                    if p.MaxTraceRecursionDepth > 31 {
                        return Err(error(E_INVALIDARG, "invalid DXR recursion depth"));
                    }
                    Payload::Pipeline(D3D12_RAYTRACING_PIPELINE_CONFIG {
                        MaxTraceRecursionDepth: p.MaxTraceRecursionDepth,
                    })
                }
                11 => {
                    let p = input(src.pDesc.cast::<ddi12::D3D12DDI_HIT_GROUP_DESC_0054>())?;
                    if p.Type > 1 || p.SummaryFlags & !7 != 0 {
                        return Err(error(E_INVALIDARG, "invalid DXR hit-group type/summary"));
                    }
                    Payload::Hit(D3D12_HIT_GROUP_DESC {
                        HitGroupExport: name(p.HitGroupExport, false)?,
                        Type: D3D12_HIT_GROUP_TYPE(p.Type as i32),
                        AnyHitShaderImport: name(p.AnyHitShaderImport, true)?,
                        ClosestHitShaderImport: name(p.ClosestHitShaderImport, true)?,
                        IntersectionShaderImport: name(p.IntersectionShaderImport, true)?,
                    })
                }
                _ => {
                    return Err(error(
                        E_NOTIMPL,
                        "state subobject is outside implemented raytracing contract",
                    ));
                }
            }
        };
        self.index
            .try_reserve(1)
            .map_err(|_| error(E_OUTOFMEMORY, "DXR subobject index allocation failed"))?;
        let index = self.push(payload)?;
        self.index.insert(ptr as usize, index);
        Ok(index)
    }

    fn association(&mut self, target: usize, export: PCWSTR) -> RtResult<()> {
        self.push(Payload::Association(
            D3D12_SUBOBJECT_TO_EXPORTS_ASSOCIATION::default(),
            target,
            [export],
        ))?;
        Ok(())
    }

    fn empty_root(&mut self, engine: &ID3D12Device5, local: bool) -> RtResult<usize> {
        if let Some(index) = if local {
            self.empty_local
        } else {
            self.empty_global
        } {
            return Ok(index);
        }
        let root = empty_root_signature(engine, local)?;
        let payload = if local {
            Payload::Local(D3D12_LOCAL_ROOT_SIGNATURE {
                pLocalRootSignature: ManuallyDrop::new(Some(root)),
            })
        } else {
            Payload::Global(D3D12_GLOBAL_ROOT_SIGNATURE {
                pGlobalRootSignature: ManuallyDrop::new(Some(root)),
            })
        };
        let index = self.push(payload)?;
        if local {
            self.empty_local = Some(index);
        } else {
            self.empty_global = Some(index);
        }
        Ok(index)
    }

    /// SAFETY: ptr, export summaries, names and all referenced subobjects remain
    /// live through this create call; h_device owns the supplied engine device.
    unsafe fn summaries(
        &mut self,
        ptr: *const ddi12::D3D12DDI_FUNCTION_SUMMARY_0054,
        h_device: ddi12::D3D12DDI_HDEVICE,
        engine: &ID3D12Device5,
    ) -> RtResult<()> {
        // SAFETY: the SHADER_EXPORT_SUMMARY tag selects this live DDI payload.
        let summary = unsafe { input(ptr) }?;
        if summary.OverallFlags & !7 != 0 {
            return Err(error(E_INVALIDARG, "unknown DXR summary flags"));
        }
        // SAFETY: exact SAL array supplied by the runtime.
        for function in unsafe { array(summary.pSummaries, summary.NumExportedFunctions) }? {
            if function.Flags & !7 != 0 {
                return Err(error(E_INVALIDARG, "unknown DXR export summary flags"));
            }
            let mut has_global = false;
            let mut has_local = false;
            // Resolve against the public namespace actually given to vkd3d;
            // runtime summaries may still name the internal DXIL symbol.
            // SAFETY: summary names are runtime-owned terminated UTF-16 and
            // remain readable until the engine has consumed this descriptor.
            let export = unsafe {
                let mangled = name(function.ExportNameMangled, true)?;
                let plain = name(function.ExportNameUnmangled, !mangled.is_null())?;
                self.summary_export(mangled, plain)
            };
            // SAFETY: runtime's associated-subobject pointer array is live.
            for ptr in unsafe {
                array(
                    function.ppAssociatedSubobjects,
                    function.NumAssociatedSubobjects,
                )
            }? {
                // SAFETY: each entry points at the resolved subobject (possibly
                // imported from a collection), valid for the whole create call.
                let associated = unsafe { input(*ptr) }?;
                // SAFETY: the same runtime-owned tagged descriptor.
                let index = unsafe { self.subobject(*ptr, h_device) }?;
                match associated.Type {
                    1 => {
                        has_global = true;
                        self.association(index, export)?;
                    }
                    2 => {
                        has_local = true;
                        self.association(index, export)?;
                    }
                    9 | 10 => self.association(index, export)?,
                    // State config is local-object-wide; node mask is full-
                    // object-wide. They are already applied as top-level data.
                    0 | 3 => {}
                    _ => return Err(error(E_INVALIDARG, "non-associable DXR summary subobject")),
                }
            }
            // The native runtime already resolved defaults. Prevent the engine
            // from applying some other shader's local/global root as a fallback
            // to an export with no root. Unresolved collection bindings remain
            // unresolved so a later enclosing object can provide them.
            if function.Flags & 5 == 0 {
                if !has_global {
                    let index = self.empty_root(engine, false)?;
                    self.association(index, export)?;
                }
                if !has_local {
                    let index = self.empty_root(engine, true)?;
                    self.association(index, export)?;
                }
            }
        }
        Ok(())
    }

    fn finish(&mut self, kind: D3D12_STATE_OBJECT_TYPE) -> RtResult<D3D12_STATE_OBJECT_DESC> {
        let count = u32::try_from(self.payloads.len())
            .map_err(|_| error(E_INVALIDARG, "DXR subobject count overflow"))?;
        reserve(&mut self.api, self.payloads.len())?;
        for payload in &self.payloads {
            self.api.push(payload.api());
        }
        // No more pushes after this point: all API subobject pointers stay fixed.
        for payload in &mut self.payloads {
            match payload {
                Payload::Association(desc, index, exports) => {
                    desc.pSubobjectToAssociate = self
                        .api
                        .get(*index)
                        .map(core::ptr::from_ref)
                        .ok_or(error(E_FAIL, "DXR association index missing"))?;
                    desc.NumExports = 1;
                    desc.pExports = exports.as_ptr();
                }
                Payload::Library(desc, blob, exports) => {
                    desc.DXILLibrary.pShaderBytecode = blob.as_ptr().cast();
                    desc.pExports = exports.as_ptr();
                }
                Payload::Collection(desc, exports) => desc.pExports = exports.as_ptr(),
                _ => {}
            }
        }
        Ok(D3D12_STATE_OBJECT_DESC {
            Type: kind,
            NumSubobjects: count,
            pSubobjects: self.api.as_ptr(),
        })
    }
}

fn empty_root_signature(engine: &ID3D12Device5, local: bool) -> RtResult<ID3D12RootSignature> {
    let desc = D3D12_ROOT_SIGNATURE_DESC {
        Flags: if local {
            D3D12_ROOT_SIGNATURE_FLAG_LOCAL_ROOT_SIGNATURE
        } else {
            D3D12_ROOT_SIGNATURE_FLAG_NONE
        },
        ..Default::default()
    };
    let mut blob = 0;
    let mut errors = 0;
    // SAFETY: exact API descriptor and writable out-pointers. The bridge returns
    // owned blob references, both released on every result path below.
    let hr = unsafe {
        bridge12::serialize_root_signature(
            core::ptr::from_ref(&desc) as usize,
            D3D_ROOT_SIGNATURE_VERSION_1_0.0 as u32,
            &mut blob,
            &mut errors,
        )
    };
    if errors != 0 {
        // SAFETY: nonzero owned ID3DBlob reference returned by the bridge.
        drop(unsafe { ID3DBlob::from_raw(errors as *mut c_void) });
    }
    if blob == 0 {
        return Err(error(
            if hr < 0 { hr } else { E_FAIL },
            "empty DXR root serialization failed",
        ));
    }
    // SAFETY: owned bridge blob reference; this scope releases it exactly once.
    let blob = unsafe { ID3DBlob::from_raw(blob as *mut c_void) };
    if hr < 0 {
        return Err(error(hr, "empty DXR root serialization failed"));
    }
    // SAFETY: the blob owns its readable extent for this scope.
    let (ptr, len) = unsafe { (blob.GetBufferPointer(), blob.GetBufferSize()) };
    if ptr.is_null() || len == 0 || len > u32::MAX as usize {
        return Err(error(E_FAIL, "invalid serialized empty DXR root"));
    }
    // SAFETY: extent checked and owned by live blob; API consumes it synchronously.
    unsafe { engine.CreateRootSignature(0, core::slice::from_raw_parts(ptr.cast(), len)) }
        .map_err(engine_error)
}

unsafe fn create_inner(
    h_device: ddi12::D3D12DDI_HDEVICE,
    kind: ddi12::D3D12DDI_STATE_OBJECT_TYPE,
    ptr: *const ddi12::D3D12DDI_STATE_SUBOBJECT_0054,
    count: u32,
    parent: Option<ddi12::D3D12DDI_HSTATEOBJECT_0054>,
) -> RtResult<StateObject> {
    let kind = match kind {
        0 => D3D12_STATE_OBJECT_TYPE_COLLECTION,
        3 => D3D12_STATE_OBJECT_TYPE_RAYTRACING_PIPELINE,
        _ => {
            return Err(error(
                E_NOTIMPL,
                "only collection and raytracing state objects are implemented",
            ));
        }
    };
    // SAFETY: device and array are runtime-owned live DDI inputs.
    let engine = unsafe { device5(h_device) }?;
    require_engine_dxr(&engine)?;
    // SAFETY: runtime guarantees count live subobjects; arrays are checked.
    let src = unsafe { array(ptr, count) }?;
    let mut translated = Translation::new();
    if let Some(parent) = parent {
        // SAFETY: the runtime keeps AddToStateObject's parent alive for this
        // call; metadata is copied so no borrowed name escapes its lifetime.
        translated.inherit_exports(unsafe { state(parent) }?)?;
    }
    for object in src {
        if object.Type != 0x100000 {
            // SAFETY: object is inside the checked live runtime array.
            unsafe { translated.subobject(core::ptr::from_ref(object), h_device) }?;
        }
    }
    for object in src {
        if object.Type == 0x100000 {
            // SAFETY: this tag selects a runtime-owned summary descriptor.
            unsafe { translated.summaries(object.pDesc.cast(), h_device, &engine) }?;
        }
    }
    let desc = translated.finish(kind)?;
    let state: ID3D12StateObject = if let Some(parent) = parent {
        // SAFETY: parent DDI handle is live throughout AddToStateObject.
        let parent = unsafe { state(parent) }?;
        if kind != D3D12_STATE_OBJECT_TYPE_RAYTRACING_PIPELINE
            || parent.kind != kind
            || parent.h_device.pDrvPrivate != h_device.pDrvPrivate
        {
            return Err(error(
                E_INVALIDARG,
                "state-object addition has wrong parent type or device",
            ));
        }
        let engine7: ID3D12Device7 = engine.cast().map_err(engine_error)?;
        // SAFETY: all translated descriptors and parent remain alive for call.
        unsafe { engine7.AddToStateObject(&desc, &parent.engine) }.map_err(engine_error)?
    } else {
        // SAFETY: API descriptor graph owns stable pointers until call returns.
        unsafe { engine.CreateStateObject(&desc) }.map_err(engine_error)?
    };
    let properties = state.cast().map_err(engine_error)?;
    Ok(StateObject {
        engine: state,
        properties,
        h_device,
        kind,
        explicit_exports: translated.explicit_exports,
    })
}

/// SAFETY: h_device is live; arg and its type-selected descriptor graph are
/// readable, and h_state is exclusive writable CalcPrivate-sized runtime storage.
pub(super) unsafe extern "system" fn create_state_object(
    h_device: ddi12::D3D12DDI_HDEVICE,
    arg: *const ddi12::D3D12DDIARG_CREATE_STATE_OBJECT_0054,
    h_state: ddi12::D3D12DDI_HSTATEOBJECT_0054,
    _h_rt: ddi12::D3D12DDI_HRTSTATEOBJECT_0054,
) -> ddi12::HRESULT {
    if !valid_slot(h_state) {
        note_refusal(&L9_REFUSALS.state_object_bad_slot);
    }
    // SAFETY: the runtime supplied the sized private slot and complete create
    // descriptor. Clear occurs before any fallible work or parent dereference.
    let result = unsafe {
        (|| {
            clear_slot(h_state)?;
            let arg = input(arg)?;
            let state = create_inner(h_device, arg.Type, arg.pSubobjects, arg.NumSubobjects, None)?;
            publish(h_state, state)
        })()
    };
    match result {
        Ok(()) => {
            L9_REFUSALS.rt_state_objects_created.bump();
            S_OK
        }
        Err(e) => {
            record_error(&L9_REFUSALS.state_object_refused, e);
            e.hr
        }
    }
}

/// SAFETY: h_device, the parent and every descriptor dependency remain live for
/// the call; h_state is exclusive writable CalcPrivateAdd-sized runtime storage.
pub(super) unsafe extern "system" fn add_to_state_object(
    h_device: ddi12::D3D12DDI_HDEVICE,
    arg: *const ddi12::D3D12DDIARG_ADD_TO_STATE_OBJECT_0072,
    h_state: ddi12::D3D12DDI_HSTATEOBJECT_0054,
    _h_rt: ddi12::D3D12DDI_HRTSTATEOBJECT_0054,
) -> ddi12::HRESULT {
    if !valid_slot(h_state) {
        note_refusal(&L9_REFUSALS.add_to_state_object_bad_slot);
    }
    // SAFETY: same initialization/lifetime contract as CreateStateObject.
    let result = unsafe {
        (|| {
            clear_slot(h_state)?;
            let arg = input(arg)?;
            let state = create_inner(
                h_device,
                arg.Type,
                arg.pSubobjects,
                arg.NumSubobjects,
                Some(arg.StateObjectToGrowFrom),
            )?;
            publish(h_state, state)
        })()
    };
    match result {
        Ok(()) => {
            L9_REFUSALS.rt_state_objects_added.bump();
            S_OK
        }
        Err(e) => {
            record_error(&L9_REFUSALS.add_to_state_object_refused, e);
            e.hr
        }
    }
}

/// SAFETY: h_state is a slot initialized by this lane; the runtime has retired
/// its uses and serializes destruction against every access to the stored object.
pub(super) unsafe extern "system" fn destroy_state_object(
    _h_device: ddi12::D3D12DDI_HDEVICE,
    h_state: ddi12::D3D12DDI_HSTATEOBJECT_0054,
) {
    if !valid_slot(h_state) {
        note_refusal(&L9_REFUSALS.state_object_destroy_unexpected);
        return;
    }
    // SAFETY: this slot was initialized before create attempted any work. The
    // runtime serializes destruction with live calls on this object.
    let ptr = unsafe { h_state.pDrvPrivate.cast::<*mut StateObject>().read() };
    // SAFETY: valid sized runtime slot, now taking its one owned box reference.
    unsafe {
        h_state
            .pDrvPrivate
            .cast::<*mut StateObject>()
            .write(core::ptr::null_mut())
    };
    if !ptr.is_null() {
        // SAFETY: publish created this unique Box; clearing first prevents a
        // second destroy from releasing it twice. Engine releases its dependencies.
        drop(unsafe { Box::from_raw(ptr) });
    }
}

/// SAFETY: h owns a live state and export is a terminated runtime-owned string.
/// The caller cannot retain the returned engine pointer after state destruction.
pub(super) unsafe extern "system" fn get_shader_identifier(
    h: ddi12::D3D12DDI_HSTATEOBJECT_0054,
    export: ddi12::LPCWSTR,
) -> *mut c_void {
    // SAFETY: live state and runtime-owned name; returned identifier remains
    // engine-owned for the state object's lifetime, as required by the DDI.
    let result = unsafe {
        (|| {
            let state = state(h)?;
            Ok(state.properties.GetShaderIdentifier(name(export, false)?))
        })()
    };
    match result {
        Ok(ptr) if !ptr.is_null() => ptr,
        Ok(_) => {
            note_refusal(&L9_REFUSALS.shader_identifier_absent);
            core::ptr::null_mut()
        }
        Err(e) => {
            record_error(&L9_REFUSALS.shader_identifier_absent, e);
            core::ptr::null_mut()
        }
    }
}

/// SAFETY: h owns a live state and export remains a readable terminated string
/// throughout the query; neither object is concurrently destroyed or mutated.
pub(super) unsafe extern "system" fn get_shader_stack_size(
    h: ddi12::D3D12DDI_HSTATEOBJECT_0054,
    export: ddi12::LPCWSTR,
) -> ddi12::UINT {
    // SAFETY: live state/name borrowed only for this query.
    let result = unsafe {
        (|| {
            let state = state(h)?;
            let size = state.properties.GetShaderStackSize(name(export, false)?);
            u32::try_from(size)
                .map_err(|_| error(E_FAIL, "engine shader stack exceeds DDI UINT range"))
        })()
    };
    match result {
        Ok(size) if size != u32::MAX => size,
        Ok(_) => {
            note_refusal(&L9_REFUSALS.shader_stack_size_absent);
            u32::MAX
        }
        Err(e) => {
            record_error(&L9_REFUSALS.shader_stack_size_absent, e);
            u32::MAX
        }
    }
}

/// SAFETY: h owns an initialized state kept live throughout this query.
pub(super) unsafe extern "system" fn get_pipeline_stack_size(
    h: ddi12::D3D12DDI_HSTATEOBJECT_0054,
) -> ddi12::UINT {
    // SAFETY: the runtime holds the state live for this query.
    let result = unsafe {
        (|| {
            let state = state(h)?;
            u32::try_from(state.properties.GetPipelineStackSize())
                .map_err(|_| error(E_FAIL, "engine pipeline stack exceeds DDI UINT range"))
        })()
    };
    match result {
        Ok(size) => size,
        Err(e) => {
            record_error(&L9_REFUSALS.pipeline_stack_size_absent, e);
            u32::MAX
        }
    }
}

/// SAFETY: h owns a live state; runtime/application synchronization excludes
/// concurrent incompatible stack mutation or destruction during this call.
pub(super) unsafe extern "system" fn set_pipeline_stack_size(
    h: ddi12::D3D12DDI_HSTATEOBJECT_0054,
    size: ddi12::UINT,
) {
    // SAFETY: native runtime synchronizes stack mutation with other operations;
    // the engine object is held live by the DDI private slot.
    match unsafe { state(h) } {
        Ok(state) => {
            // SAFETY: live engine state; widening UINT to UINT64 is exact.
            unsafe { state.properties.SetPipelineStackSize(u64::from(size)) };
        }
        Err(e) => record_error(&L9_REFUSALS.set_pipeline_stack_size_dropped, e),
    }
}

/// SAFETY: h_device is live and non-null identifier addresses one readable
/// matching-identifier struct supplied by the runtime for this query.
pub(super) unsafe extern "system" fn check_driver_matching_identifier(
    h_device: ddi12::D3D12DDI_HDEVICE,
    data_type: ddi12::D3D12DDI_SERIALIZED_DATA_TYPE,
    identifier: *const ddi12::D3D12DDI_SERIALIZED_DATA_DRIVER_MATCHING_IDENTIFIER_0054,
) -> ddi12::D3D12DDI_DRIVER_MATCHING_IDENTIFIER_STATUS {
    // SAFETY: typed runtime identifier and device are readable during the query.
    let result = unsafe {
        (|| {
            let engine = device5(h_device)?;
            let src = input(identifier)?;
            let id = D3D12_SERIALIZED_DATA_DRIVER_MATCHING_IDENTIFIER {
                DriverOpaqueGUID: windows::core::GUID::from_values(
                    src.DriverOpaqueGUID.Data1,
                    src.DriverOpaqueGUID.Data2,
                    src.DriverOpaqueGUID.Data3,
                    src.DriverOpaqueGUID.Data4,
                ),
                DriverOpaqueVersioningData: src.DriverOpaqueVersioningData,
            };
            let result =
                engine.CheckDriverMatchingIdentifier(D3D12_SERIALIZED_DATA_TYPE(data_type), &id);
            if !(0..=4).contains(&result.0) {
                return Err(error(E_FAIL, "unknown engine serialized-identifier result"));
            }
            Ok(result.0)
        })()
    };
    match result {
        Ok(0) => 0,
        Ok(status) => {
            note_refusal(&L9_REFUSALS.driver_matching_identifier_refused);
            status
        }
        Err(e) => {
            record_error(&L9_REFUSALS.driver_matching_identifier_refused, e);
            ddi12::D3D12DDI_DRIVER_MATCHING_IDENTIFIER_STATUS_D3D12DDI_DRIVER_MATCHING_IDENTIFIER_UNRECOGNIZED
        }
    }
}

fn gpu_range(address: u64, bytes: u64, alignment: u64, optional: bool) -> RtResult<()> {
    if address == 0 && optional {
        return Ok(());
    }
    if address == 0 || !address.is_multiple_of(alignment) || address.checked_add(bytes).is_none() {
        return Err(error(
            E_INVALIDARG,
            "DXR GPU address has invalid alignment or extent",
        ));
    }
    Ok(())
}

fn strided_extent(count: u64, stride: u64, element: u64) -> RtResult<u64> {
    if count == 0 {
        return Ok(0);
    }
    (count - 1)
        .checked_mul(stride)
        .and_then(|n| n.checked_add(element))
        .ok_or(error(E_INVALIDARG, "DXR strided GPU extent overflows"))
}

/// Owns the CPU geometry array only. The GPU buffers are never mapped, copied,
/// or released here: they retain the application's address/ordering/lifetime
/// contract. The engine consumes this CPU description during the DDI call.
struct BuildInputs {
    api: D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS,
    geometry: Vec<D3D12_RAYTRACING_GEOMETRY_DESC>,
}

impl BuildInputs {
    /// SAFETY: src's type/layout-selected CPU geometry arrays and any secondary
    /// pointers address NumDescs readable entries for this DDI call. GPU addresses
    /// are opaque values; this function never dereferences them on the CPU.
    unsafe fn translate(
        src: &ddi12::D3D12DDI_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS_0054,
        gpu: bool,
    ) -> RtResult<Self> {
        if src.Type > 1
            || src.Type < 0
            || src.DescsLayout > 1
            || src.DescsLayout < 0
            || src.Flags & !0x3f != 0
            || src.Flags & 0xc == 0xc
        {
            return Err(error(
                E_INVALIDARG,
                "invalid DXR build type, layout or flags",
            ));
        }
        let mut translated = Self {
            api: D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS {
                Type: D3D12_RAYTRACING_ACCELERATION_STRUCTURE_TYPE(src.Type),
                Flags: D3D12_RAYTRACING_ACCELERATION_STRUCTURE_BUILD_FLAGS(src.Flags),
                NumDescs: src.NumDescs,
                DescsLayout: D3D12_ELEMENTS_LAYOUT(src.DescsLayout),
                ..Default::default()
            },
            geometry: Vec::new(),
        };
        if src.Type == 0 {
            if src.NumDescs > D3D12_RAYTRACING_MAX_INSTANCES_PER_TOP_LEVEL_ACCELERATION_STRUCTURE {
                return Err(error(E_INVALIDARG, "DXR instance count exceeds API limit"));
            }
            // SAFETY: TOP_LEVEL selects InstanceDescs, an address (not a CPU pointer).
            let instances = unsafe { src.__bindgen_anon_1.InstanceDescs };
            if gpu && src.NumDescs != 0 {
                let (stride, alignment) = if src.DescsLayout == 0 {
                    (64, 16)
                } else {
                    (8, 8)
                };
                gpu_range(
                    instances,
                    u64::from(src.NumDescs) * stride,
                    alignment,
                    false,
                )?;
            }
            translated.api.Anonymous.InstanceDescs = instances;
        } else {
            if src.NumDescs
                > D3D12_RAYTRACING_MAX_GEOMETRIES_PER_BOTTOM_LEVEL_ACCELERATION_STRUCTURE
            {
                return Err(error(E_INVALIDARG, "DXR geometry count exceeds API limit"));
            }
            reserve(&mut translated.geometry, src.NumDescs as usize)?;
            // SAFETY: BOTTOM_LEVEL and DescsLayout select exactly one CPU array.
            let (contiguous, pointers) = unsafe {
                if src.DescsLayout == 0 {
                    (
                        array(src.__bindgen_anon_1.pGeometryDescs, src.NumDescs)?,
                        &[][..],
                    )
                } else {
                    (
                        &[][..],
                        array(src.__bindgen_anon_1.ppGeometryDescs, src.NumDescs)?,
                    )
                }
            };
            let mut primitives = 0u64;
            let mut geometry_type = None;
            for i in 0..src.NumDescs as usize {
                let geometry = if src.DescsLayout == 0 {
                    &contiguous[i]
                } else {
                    // SAFETY: each pointer is one runtime-owned geometry descriptor.
                    unsafe { input(pointers[i]) }?
                };
                if geometry_type.is_some_and(|kind| kind != geometry.Type) {
                    return Err(error(E_INVALIDARG, "DXR BLAS mixes triangles and AABBs"));
                }
                geometry_type = Some(geometry.Type);
                // SAFETY: translation reads only the active geometry union arm.
                let (desc, count) = unsafe { translate_geometry(geometry, gpu) }?;
                primitives = primitives
                    .checked_add(count)
                    .ok_or(error(E_INVALIDARG, "DXR primitive count overflows"))?;
                if primitives
                    > u64::from(
                        D3D12_RAYTRACING_MAX_PRIMITIVES_PER_BOTTOM_LEVEL_ACCELERATION_STRUCTURE,
                    )
                {
                    return Err(error(E_INVALIDARG, "DXR primitive count exceeds API limit"));
                }
                translated.geometry.push(desc);
            }
            // Both native layouts have been consumed; the API owns a flat
            // equivalent array. Vec's backing remains stable across this move.
            translated.api.DescsLayout = D3D12_ELEMENTS_LAYOUT_ARRAY;
            translated.api.Anonymous.pGeometryDescs = translated.geometry.as_ptr();
        }
        Ok(translated)
    }
}

/// SAFETY: src's Type-selected union arm is initialized and readable for the
/// current DDI call. Embedded GPU addresses remain opaque and are not CPU reads.
unsafe fn translate_geometry(
    src: &ddi12::D3D12DDI_RAYTRACING_GEOMETRY_DESC_0054,
    gpu: bool,
) -> RtResult<(D3D12_RAYTRACING_GEOMETRY_DESC, u64)> {
    if src.Flags & !3 != 0 {
        return Err(error(E_INVALIDARG, "unknown DXR geometry flags"));
    }
    let mut out = D3D12_RAYTRACING_GEOMETRY_DESC {
        Type: D3D12_RAYTRACING_GEOMETRY_TYPE(src.Type),
        Flags: D3D12_RAYTRACING_GEOMETRY_FLAGS(src.Flags),
        ..Default::default()
    };
    let count;
    match src.Type {
        0 => {
            // SAFETY: TRIANGLES selects only this arm of the runtime union.
            let t = unsafe { &src.__bindgen_anon_1.Triangles };
            let (vertex_bytes, alignment) = match DXGI_FORMAT(t.VertexFormat) {
                DXGI_FORMAT_R32G32B32_FLOAT => (12, 4),
                DXGI_FORMAT_R32G32_FLOAT => (8, 4),
                DXGI_FORMAT_R16G16B16A16_FLOAT | DXGI_FORMAT_R16G16B16A16_SNORM => (8, 2),
                DXGI_FORMAT_R16G16_FLOAT | DXGI_FORMAT_R16G16_SNORM => (4, 2),
                // Tier 1.1 extends the accepted vertex encodings. The engine
                // applies its physical format support when building the BLAS.
                DXGI_FORMAT_R16G16B16A16_UNORM => (8, 2),
                DXGI_FORMAT_R16G16_UNORM => (4, 2),
                DXGI_FORMAT_R10G10B10A2_UNORM => (4, 4),
                DXGI_FORMAT_R8G8B8A8_UNORM | DXGI_FORMAT_R8G8B8A8_SNORM => (4, 1),
                DXGI_FORMAT_R8G8_UNORM | DXGI_FORMAT_R8G8_SNORM => (2, 1),
                _ => return Err(error(E_INVALIDARG, "invalid DXR vertex format")),
            };
            // The DDI uses only the low 32 stride bits; its UINT64 field is
            // for structure alignment. Normalize before validation or COM
            // translation, whose public descriptor is a separate contract.
            let vertex_stride = u64::from(t.VertexBuffer.StrideInBytes as u32);
            if !vertex_stride.is_multiple_of(alignment) {
                return Err(error(E_INVALIDARG, "unaligned DXR vertex stride"));
            }
            let index_bytes = match DXGI_FORMAT(t.IndexFormat) {
                DXGI_FORMAT_UNKNOWN if t.IndexCount == 0 => 0,
                DXGI_FORMAT_R16_UINT => 2,
                DXGI_FORMAT_R32_UINT => 4,
                _ => return Err(error(E_INVALIDARG, "invalid DXR index format/count")),
            };
            let primitive_vertices = if index_bytes == 0 {
                t.VertexCount
            } else {
                t.IndexCount
            };
            if !primitive_vertices.is_multiple_of(3) {
                return Err(error(
                    E_INVALIDARG,
                    "DXR triangle element count is not divisible by three",
                ));
            }
            count = u64::from(primitive_vertices / 3);
            if gpu {
                if t.VertexCount != 0 {
                    gpu_range(
                        t.VertexBuffer.StartAddress,
                        strided_extent(u64::from(t.VertexCount), vertex_stride, vertex_bytes)?,
                        alignment,
                        false,
                    )?;
                }
                if index_bytes != 0 && t.IndexCount != 0 {
                    gpu_range(
                        t.IndexBuffer,
                        u64::from(t.IndexCount) * index_bytes,
                        index_bytes,
                        false,
                    )?;
                }
                gpu_range(t.ColumnMajorTransform3x4, 48, 16, true)?;
            }
            out.Anonymous.Triangles = D3D12_RAYTRACING_GEOMETRY_TRIANGLES_DESC {
                // This is the runtime's GPU pointer to the same API transform;
                // the DDI spelling does not authorize a CPU transpose or rebase.
                Transform3x4: t.ColumnMajorTransform3x4,
                IndexFormat: DXGI_FORMAT(t.IndexFormat),
                VertexFormat: DXGI_FORMAT(t.VertexFormat),
                IndexCount: t.IndexCount,
                VertexCount: t.VertexCount,
                IndexBuffer: t.IndexBuffer,
                VertexBuffer: D3D12_GPU_VIRTUAL_ADDRESS_AND_STRIDE {
                    StartAddress: t.VertexBuffer.StartAddress,
                    StrideInBytes: vertex_stride,
                },
            };
        }
        1 => {
            // SAFETY: PROCEDURAL_PRIMITIVE_AABBS selects only this union arm.
            let a = unsafe { &src.__bindgen_anon_1.AABBs };
            // D3D12DDI_GPU_VIRTUAL_ADDRESS_AND_STRIDE ignores the upper word.
            let aabb_stride = u64::from(a.AABBs.StrideInBytes as u32);
            if !aabb_stride.is_multiple_of(8) {
                return Err(error(E_INVALIDARG, "unaligned DXR AABB stride"));
            }
            count = a.AABBCount;
            if gpu && count != 0 {
                gpu_range(
                    a.AABBs.StartAddress,
                    strided_extent(count, aabb_stride, 24)?,
                    8,
                    false,
                )?;
            }
            out.Anonymous.AABBs = D3D12_RAYTRACING_GEOMETRY_AABBS_DESC {
                AABBCount: count,
                AABBs: D3D12_GPU_VIRTUAL_ADDRESS_AND_STRIDE {
                    StartAddress: a.AABBs.StartAddress,
                    StrideInBytes: aabb_stride,
                },
            };
        }
        _ => {
            return Err(error(
                E_NOTIMPL,
                "DXR geometry type is outside the implemented contract",
            ));
        }
    }
    Ok((out, count))
}

fn postbuild(
    src: &ddi12::D3D12DDI_RAYTRACING_ACCELERATION_STRUCTURE_POSTBUILD_INFO_DESC_0054,
    count: u32,
) -> RtResult<D3D12_RAYTRACING_ACCELERATION_STRUCTURE_POSTBUILD_INFO_DESC> {
    let (kind, size) = match src.InfoType {
        0 => (
            D3D12_RAYTRACING_ACCELERATION_STRUCTURE_POSTBUILD_INFO_COMPACTED_SIZE,
            8,
        ),
        2 => (
            D3D12_RAYTRACING_ACCELERATION_STRUCTURE_POSTBUILD_INFO_SERIALIZATION,
            16,
        ),
        3 => (
            D3D12_RAYTRACING_ACCELERATION_STRUCTURE_POSTBUILD_INFO_CURRENT_SIZE,
            8,
        ),
        _ => {
            return Err(error(
                E_NOTIMPL,
                "engine has no tools-visualization postbuild format",
            ));
        }
    };
    if count != 0 {
        gpu_range(src.DestBuffer, u64::from(count) * size, 8, false)?;
    }
    Ok(
        D3D12_RAYTRACING_ACCELERATION_STRUCTURE_POSTBUILD_INFO_DESC {
            DestBuffer: src.DestBuffer,
            InfoType: kind,
        },
    )
}

/// SAFETY: a non-null h refers to this lane's device private storage, held live
/// by the active DDI call while the runtime's error callback is invoked.
unsafe fn device_failure(h: ddi12::D3D12DDI_HDEVICE, e: RtError) {
    // SAFETY: the owning device is kept live during every query on it.
    if let Some(device) = unsafe { device12::device(h) } {
        if !device12::set_error(device, e.hr) {
            record_error(&L9_REFUSALS.rt_error_callback_unavailable, e);
        }
    } else {
        record_error(&L9_REFUSALS.rt_error_callback_unavailable, e);
    }
}

enum RtCommand {
    Build,
    Postbuild,
    Copy,
    SetPipelineState,
    Dispatch,
}

/// Recording errors quarantine the one list, preserving unrelated device
/// work. A missing runtime callback is itself counted; never report success.
/// SAFETY: non-null h names live CommandListState private storage, and the caller
/// obeys the runtime's recording-thread exclusivity for this list and callback.
unsafe fn list_call(
    h: ddi12::D3D12DDI_HCOMMANDLIST,
    command: RtCommand,
    refused: &RefusalCounter,
    forwarded: &RefusalCounter,
    op: impl FnOnce(&queue::CommandListState, &ID3D12GraphicsCommandList4) -> RtResult<()>,
) {
    // SAFETY: the runtime holds the command-list private state live for this DDI.
    let Some(state) = (unsafe { queue::command_list_state(h) }) else {
        record_error(
            refused,
            error(E_INVALIDARG, "unresolved DXR command-list handle"),
        );
        note_refusal(&L9_REFUSALS.command_list_missing);
        return;
    };
    let result = (|| {
        // DXR permits pipeline binding and ray dispatch in bundles. AS build,
        // copy and postbuild emission remain direct/compute-only operations.
        let bundle_allowed = matches!(command, RtCommand::SetPipelineState | RtCommand::Dispatch);
        if state.list_type() != D3D12_COMMAND_LIST_TYPE_DIRECT
            && state.list_type() != D3D12_COMMAND_LIST_TYPE_COMPUTE
            && !(bundle_allowed && state.list_type() == D3D12_COMMAND_LIST_TYPE_BUNDLE)
        {
            return Err(error(
                E_INVALIDARG,
                "DXR operation is invalid on this command-list type",
            ));
        }
        // SAFETY: the list retains the device throughout this synchronous DDI.
        let device = unsafe { device5(state.h_device()) }?;
        require_engine_dxr(&device)?;
        let engine = state.engine().cast().map_err(engine_error)?;
        op(state, &engine)
    })();
    match result {
        Ok(()) => {
            forwarded.bump();
        }
        Err(e) => {
            record_error(refused, e);
            // SAFETY: the parent device outlives its runtime command list.
            if let Some(device) = unsafe { device12::device(state.h_device()) } {
                if !device12::set_command_list_error(device, state.h_rt_list(), e.hr) {
                    record_error(&L9_REFUSALS.rt_error_callback_unavailable, e);
                }
            } else {
                record_error(&L9_REFUSALS.rt_error_callback_unavailable, e);
            }
        }
    }
}

/// SAFETY: h_device is live; desc and its selected CPU arrays are readable for
/// this query, and non-null info is one exclusive writable runtime output struct.
pub(super) unsafe extern "system" fn get_raytracing_acceleration_structure_prebuild_info(
    h_device: ddi12::D3D12DDI_HDEVICE,
    desc: *const ddi12::D3D12DDI_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS_0054,
    info: *mut ddi12::D3D12DDI_RAYTRACING_ACCELERATION_STRUCTURE_PREBUILD_INFO_0054,
) {
    if info.is_null()
        || !(info as usize).is_multiple_of(align_of::<
            ddi12::D3D12DDI_RAYTRACING_ACCELERATION_STRUCTURE_PREBUILD_INFO_0054,
        >())
    {
        record_error(
            &L9_REFUSALS.rt_prebuild_info_bad_arg,
            error(E_INVALIDARG, "missing DXR prebuild output"),
        );
        // SAFETY: runtime owns this device throughout the query.
        unsafe { device_failure(h_device, error(E_INVALIDARG, "missing DXR prebuild output")) };
        return;
    }
    // SAFETY: exact non-null aligned runtime output struct; always initialize it.
    unsafe { info.write(Default::default()) };
    // SAFETY: the runtime's typed input graph remains live through this query.
    let result = unsafe {
        (|| {
            let engine = device5(h_device)?;
            require_engine_dxr(&engine)?;
            let inputs = BuildInputs::translate(input(desc)?, false)?;
            let mut out = D3D12_RAYTRACING_ACCELERATION_STRUCTURE_PREBUILD_INFO::default();
            engine.GetRaytracingAccelerationStructurePrebuildInfo(&inputs.api, &mut out);
            if out.ResultDataMaxSizeInBytes == 0 {
                return Err(error(E_FAIL, "engine returned no DXR prebuild size"));
            }
            Ok(out)
        })()
    };
    match result {
        Ok(out) => {
            // SAFETY: same checked output; field translation avoids ABI casting.
            unsafe {
                info.write(
                    ddi12::D3D12DDI_RAYTRACING_ACCELERATION_STRUCTURE_PREBUILD_INFO_0054 {
                        ResultDataMaxSizeInBytes: out.ResultDataMaxSizeInBytes,
                        ScratchDataSizeInBytes: out.ScratchDataSizeInBytes,
                        UpdateScratchDataSizeInBytes: out.UpdateScratchDataSizeInBytes,
                    },
                )
            };
            L9_REFUSALS.rt_prebuild_info_forwarded.bump();
        }
        Err(e) => {
            record_error(&L9_REFUSALS.rt_prebuild_info_empty, e);
            // SAFETY: same live query device; zero sizes must not imply success.
            unsafe { device_failure(h_device, e) };
        }
    }
}

pub(super) unsafe extern "system" fn build_raytracing_acceleration_structure(
    h: ddi12::D3D12DDI_HCOMMANDLIST,
    arg: *const ddi12::D3D12DDIARG_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_0054,
) {
    // SAFETY: runtime holds list, descriptor and both CPU arrays live for call.
    unsafe {
        list_call(
            h,
            RtCommand::Build,
            &L9_REFUSALS.rt_build_refused,
            &L9_REFUSALS.rt_build_forwarded,
            |_, engine| {
                let arg = input(arg)?;
                let inputs = BuildInputs::translate(&arg.Inputs, true)?;
                gpu_range(arg.DestAccelerationStructureData, 0, 256, false)?;
                gpu_range(arg.ScratchAccelerationStructureData, 0, 256, false)?;
                if arg.Inputs.Flags & 0x20 != 0 {
                    gpu_range(arg.SourceAccelerationStructureData, 0, 256, false)?;
                }
                let src_postbuild = array(arg.pPostbuildInfoDescs, arg.NumPostbuildInfoDescs)?;
                let mut postbuilds = Vec::new();
                reserve(&mut postbuilds, src_postbuild.len())?;
                for src in src_postbuild {
                    postbuilds.push(postbuild(src, 1)?);
                }
                let desc = D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_DESC {
                    DestAccelerationStructureData: arg.DestAccelerationStructureData,
                    Inputs: inputs.api,
                    SourceAccelerationStructureData: arg.SourceAccelerationStructureData,
                    ScratchAccelerationStructureData: arg.ScratchAccelerationStructureData,
                };
                engine.BuildRaytracingAccelerationStructure(&desc, Some(&postbuilds));
                Ok(())
            },
        )
    };
}

/// SAFETY: h is a live recording list; arg and its declared GPU-address array
/// are runtime-owned readable memory for the duration of this call.
pub(super) unsafe extern "system" fn emit_raytracing_acceleration_structure_postbuild_info(
    h: ddi12::D3D12DDI_HCOMMANDLIST,
    arg: *const ddi12::D3D12DDIARG_EMIT_RAYTRACING_ACCELERATION_STRUCTURE_POSTBUILD_INFO_0054,
) {
    // SAFETY: runtime-owned list/descriptor and source-address array, live for call.
    unsafe {
        list_call(
            h,
            RtCommand::Postbuild,
            &L9_REFUSALS.rt_postbuild_info_refused,
            &L9_REFUSALS.rt_postbuild_info_forwarded,
            |_, engine| {
                let arg = input(arg)?;
                let desc = postbuild(&arg.Desc, arg.NumSourceAccelerationStructures)?;
                let sources = array(
                    arg.pSourceAccelerationStructureData,
                    arg.NumSourceAccelerationStructures,
                )?;
                for address in sources {
                    gpu_range(*address, 0, 256, false)?;
                }
                engine.EmitRaytracingAccelerationStructurePostbuildInfo(&desc, sources);
                Ok(())
            },
        )
    };
}

/// SAFETY: h is a live recording list and non-null arg is one readable runtime
/// copy descriptor; the application retains its GPU buffers through completion.
pub(super) unsafe extern "system" fn copy_raytracing_acceleration_structure(
    h: ddi12::D3D12DDI_HCOMMANDLIST,
    arg: *const ddi12::D3D12DDIARG_COPY_RAYTRACING_ACCELERATION_STRUCTURE_0054,
) {
    // SAFETY: list and exact copy descriptor are live for this recording call.
    unsafe {
        list_call(
            h,
            RtCommand::Copy,
            &L9_REFUSALS.rt_copy_refused,
            &L9_REFUSALS.rt_copy_forwarded,
            |_, engine| {
                let arg = input(arg)?;
                let mode = match arg.Mode {
                    0 => D3D12_RAYTRACING_ACCELERATION_STRUCTURE_COPY_MODE_CLONE,
                    1 => D3D12_RAYTRACING_ACCELERATION_STRUCTURE_COPY_MODE_COMPACT,
                    3 => D3D12_RAYTRACING_ACCELERATION_STRUCTURE_COPY_MODE_SERIALIZE,
                    4 => D3D12_RAYTRACING_ACCELERATION_STRUCTURE_COPY_MODE_DESERIALIZE,
                    _ => {
                        return Err(error(
                            E_NOTIMPL,
                            "engine has no DXR tools-visualization copy",
                        ));
                    }
                };
                gpu_range(arg.SourceAccelerationStructureData, 0, 256, false)?;
                gpu_range(arg.DestAccelerationStructureData, 0, 256, false)?;
                if arg.SourceAccelerationStructureData == arg.DestAccelerationStructureData {
                    return Err(error(
                        E_INVALIDARG,
                        "DXR copy source aliases its destination",
                    ));
                }
                engine.CopyRaytracingAccelerationStructure(
                    arg.DestAccelerationStructureData,
                    arg.SourceAccelerationStructureData,
                    mode,
                );
                Ok(())
            },
        )
    };
}

/// SAFETY: h and the non-null h_state refer to live runtime private objects;
/// runtime/application lifetime rules keep the recorded pipeline alive on GPU.
pub(super) unsafe extern "system" fn set_pipeline_state1(
    h: ddi12::D3D12DDI_HCOMMANDLIST,
    h_state: ddi12::D3D12DDI_HSTATEOBJECT_0054,
) {
    // SAFETY: the runtime keeps state object and list alive through recording;
    // its recorded-object lifetime rules and app's GPU lifetime obligations
    // prevent Destroy while command execution still references this pipeline.
    unsafe {
        list_call(
            h,
            RtCommand::SetPipelineState,
            &L9_REFUSALS.set_pipeline_state1_refused,
            &L9_REFUSALS.rt_pipeline_bound,
            |list, engine| {
                if h_state.pDrvPrivate.is_null() {
                    engine.SetPipelineState1(None::<&ID3D12StateObject>);
                } else {
                    let object = state(h_state)?;
                    if object.kind != D3D12_STATE_OBJECT_TYPE_RAYTRACING_PIPELINE
                        || object.h_device.pDrvPrivate != list.h_device().pDrvPrivate
                    {
                        return Err(error(
                            E_INVALIDARG,
                            "DXR pipeline binding has wrong type or device",
                        ));
                    }
                    engine.SetPipelineState1(&object.engine);
                }
                Ok(())
            },
        )
    };
}

fn shader_table(
    src: &ddi12::D3D12DDI_GPU_VIRTUAL_ADDRESS_RANGE_AND_STRIDE,
) -> RtResult<D3D12_GPU_VIRTUAL_ADDRESS_RANGE_AND_STRIDE> {
    // Shader-table RANGE_AND_STRIDE uses all 64 bits. Only the separate
    // geometry ADDRESS_AND_STRIDE type specifies ignoring its upper 32 bits.
    let stride = src.StrideInBytes;
    if src.SizeInBytes != 0 {
        gpu_range(src.StartAddress, src.SizeInBytes, 64, false)?;
        if !stride.is_multiple_of(32) || stride > 4096 {
            return Err(error(E_INVALIDARG, "invalid DXR shader-table stride"));
        }
        // Zero stride deliberately means broadcast one record. The API allows
        // it; do not turn it into a divide-by-zero or a fabricated stride.
        if stride != 0 && !src.SizeInBytes.is_multiple_of(stride) {
            return Err(error(
                E_INVALIDARG,
                "DXR shader-table size is not a multiple of its stride",
            ));
        }
    }
    Ok(D3D12_GPU_VIRTUAL_ADDRESS_RANGE_AND_STRIDE {
        StartAddress: src.StartAddress,
        SizeInBytes: src.SizeInBytes,
        StrideInBytes: stride,
    })
}

/// SAFETY: h is a live recording list and arg is readable runtime memory for
/// this call. Shader-table GPU addresses are validated values, never CPU reads.
pub(super) unsafe extern "system" fn dispatch_rays(
    h: ddi12::D3D12DDI_HCOMMANDLIST,
    arg: *const ddi12::D3D12DDIARG_DISPATCH_RAYS_0054,
) {
    // SAFETY: runtime owns the checked list and descriptor for this invocation.
    unsafe {
        list_call(
            h,
            RtCommand::Dispatch,
            &L9_REFUSALS.dispatch_rays_refused,
            &L9_REFUSALS.rt_dispatch_forwarded,
            |_, engine| {
                let arg = input(arg)?;
                let threads = u64::from(arg.Width)
                    .checked_mul(u64::from(arg.Height))
                    .and_then(|n| n.checked_mul(u64::from(arg.Depth)));
                if threads.is_none_or(|n| {
                    n > u64::from(D3D12_RAYTRACING_MAX_RAY_GENERATION_SHADER_THREADS)
                }) {
                    return Err(error(
                        E_INVALIDARG,
                        "DXR dispatch dimension product exceeds API limit",
                    ));
                }
                let raygen = &arg.RayGenerationShaderRecord;
                gpu_range(raygen.StartAddress, raygen.SizeInBytes, 64, false)?;
                if raygen.SizeInBytes == 0
                    || raygen.SizeInBytes > 4096
                    || !raygen.SizeInBytes.is_multiple_of(32)
                {
                    return Err(error(
                        E_INVALIDARG,
                        "invalid DXR ray-generation record size",
                    ));
                }
                let desc = D3D12_DISPATCH_RAYS_DESC {
                    RayGenerationShaderRecord: D3D12_GPU_VIRTUAL_ADDRESS_RANGE {
                        StartAddress: raygen.StartAddress,
                        SizeInBytes: raygen.SizeInBytes,
                    },
                    MissShaderTable: shader_table(&arg.MissShaderTable)?,
                    HitGroupTable: shader_table(&arg.HitGroupTable)?,
                    CallableShaderTable: shader_table(&arg.CallableShaderTable)?,
                    Width: arg.Width,
                    Height: arg.Height,
                    Depth: arg.Depth,
                };
                engine.DispatchRays(&desc);
                Ok(())
            },
        )
    };
}
