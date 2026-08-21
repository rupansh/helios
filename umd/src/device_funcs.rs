//! D3D11 device object + device-funcs table fill (Gate 5b, Milestone 1).
//!
//! The OS D3D11 runtime drives `D3D11CreateDevice` into our adapter `CreateDevice`
//! DDI, which must hand back a fully-populated `D3D11DDI_DEVICEFUNCS` table (152
//! entries) and return S_OK. A null entry the runtime calls = crash, so we fill
//! **all** entries with a safe stub and specialise the few whose return value
//! matters. This is the minimal honest device that lets the runtime accept the
//! device (Milestone 1 = D3D11CreateDevice S_OK → DWM stops fail-fasting); real
//! rendering DDIs come later, backed by the DXVK device this object holds.
//!
//! ABI note: every device DDI takes `D3D10DDI_HDEVICE` (one pointer) as its first
//! arg and the x64 calling convention is caller-clean, so a uniform
//! `extern "C" fn(usize) -> usize` stub transmuted into each slot reads only the
//! first arg, ignores the rest, and returns in RAX — valid for the void / HRESULT
//! / SIZE_T return shapes alike.

use crate::bridge;
use crate::ddi;
use crate::log_error;
use core::ffi::c_void;
use helios_protocol::{
    HeliosSyncProgressJoinV1, HeliosSyncProgressResultV1, HeliosTranslatorScope,
    HeliosTranslatorStatus, HeliosTranslatorStatusCode, HELIOS_ENGINE_CLASS_GRAPHICS,
    HELIOS_HOB1_ACCESS_WRITE, HELIOS_HOB1_MAX_BYTES, HELIOS_HQA1_FLAG_D3D11_PHYSICAL,
    HELIOS_HVC1_ALLOCATION_LIST_ENTRIES, HELIOS_HVC1_PATCH_LOCATION_ENTRIES,
    HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION, HELIOS_TRANSLATOR_PROGRESS_FLAG_DEVICE_LOST,
    HELIOS_TRANSLATOR_SYNC_PROGRESS_RESULT_BYTES,
};
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use windows::core::{IUnknown, Interface};

/// WDDM 2.x paging queue used to order explicit residency operations.
///
/// The non-zero handles and non-null monitored-fence mapping are validated at
/// construction, so any allocation carrying a residency reference can rely on
/// this queue being usable without repeating raw-handle checks.
#[derive(Clone, Copy)]
pub struct RuntimePagingQueue {
    pub handle: core::num::NonZeroU32,
    pub sync_object: core::num::NonZeroU32,
    pub fence_value_cpu: core::ptr::NonNull<u64>,
}

// `Window<T>` moved to `helios_umd_common::window` (`DECISIONS.md` D3b,
// stage S1). Re-exported so `Window<c_void>` / `Window<ddi::…>` in this
// module resolve unchanged.
pub use helios_umd_common::window::Window;

/// The kernel context every present path submits through, plus the three
/// runtime-owned buffer windows that arrive with it.
///
/// Seven fields used to become meaningful together or not at all, depending on
/// one `hr` the caller never saw — `create_runtime_context` returned unit — and
/// every consumer re-derived the invariant by hand with its own
/// `h_context.is_null()` test. `RuntimePagingQueue`, twenty lines above, already
/// demonstrated the fix and even documented it; the context group was never
/// converted. R808.
///
/// Stored as `Option<RuntimeContext>` exactly like `paging_queue`, so "context
/// exists" is one check that yields a value in which a pointer and its capacity
/// can never disagree.
pub struct RuntimeContext {
    pub handle: core::ptr::NonNull<c_void>,
    /// Legacy command buffer recycled by `pfnRenderCb`.
    pub command: core::cell::Cell<Option<Window<c_void>>>,
    pub allocations: core::cell::Cell<Option<Window<ddi::D3DDDI_ALLOCATIONLIST>>>,
    pub patches: core::cell::Cell<Option<Window<ddi::D3DDDI_PATCHLOCATIONLIST>>>,
    /// A5/HQA1 identity of this exact runtime context.
    pub context_generation: u64,
    pub endpoint_id: u32,
    pub context_flags: u32,
    /// The context-private HQC1 monitored fence and its read-only CPU mapping.
    pub hqc1: core::num::NonZeroU32,
    pub hqc1_cpu: core::ptr::NonNull<u64>,
    /// Strictly increasing values issued only after an accepted outer batch.
    pub next_progress: AtomicU64,
    pub last_submitted_progress: AtomicU64,
    pub last_batch_id: AtomicU64,
    /// Serializes the runtime-owned command/allocation windows and Render.
    pub render_lock: std::sync::Mutex<()>,
    /// One scope owned by the current DXVK outer operation. A join takes,
    /// seals, closes, and replaces it without TLS or a global registry.
    pub active_scope: std::sync::Mutex<Option<HeliosTranslatorScope>>,
}

/// The complete outer submission object. It is boxed before HQA1 creation so
/// the address attached to A5 and published to DXVK remains stable until
/// direct rundown. No process-global lookup participates in either direction.
pub struct OuterDevice {
    pub translator: helios_umd_common::direct_translator::DirectTranslator,
    pub context: Option<RuntimeContext>,
    pub allocations: std::sync::Mutex<OuterAllocationSet>,
    pub h_rt_device: ddi::HANDLE,
    pub kt_callbacks: *const ddi::D3DDDI_DEVICECALLBACKS,
    pub paging_queue: Option<RuntimePagingQueue>,
    pub device_lost: AtomicU32,
}

const VK_SUCCESS: i32 = 0;
const VK_ERROR_DEVICE_LOST: i32 = -4;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateEventW(
        security_attributes: *mut c_void,
        manual_reset: i32,
        initial_state: i32,
        name: *const u16,
    ) -> *mut c_void;
    fn WaitForSingleObject(handle: *mut c_void, milliseconds: u32) -> u32;
    fn CloseHandle(handle: *mut c_void) -> i32;
}

const WAIT_OBJECT_0: u32 = 0;
const INFINITE: u32 = u32::MAX;
const E_PENDING: i32 = 0x8000_000Au32 as i32;

/// A5 host callbacks dereference only the boxed object that was attached to
/// this exact context. The context generation is checked before any wait or
/// down-call; there is no lookup, TLS identity, or process-global state.
pub(crate) extern "C" fn translator_sync_progress_join(
    host_context_cookie: *mut c_void,
    request: *const HeliosSyncProgressJoinV1,
    out_result: *mut HeliosSyncProgressResultV1,
) -> HeliosTranslatorStatusCode {
    if host_context_cookie.is_null() || request.is_null() || out_result.is_null() {
        return HeliosTranslatorStatus::NullArgument as HeliosTranslatorStatusCode;
    }
    // SAFETY: A5 owns an attachment reference until detach returns. The cookie
    // is the stable Box<OuterDevice> address supplied by create_runtime_context.
    let outer = unsafe { &*(host_context_cookie as *const OuterDevice) };
    let Some(context) = outer.context.as_ref() else {
        return HeliosTranslatorStatus::UnknownContext as HeliosTranslatorStatusCode;
    };
    let request = unsafe { &*request };
    if let Err(status) = request.validate(context.context_generation) {
        return status as HeliosTranslatorStatusCode;
    }
    match unsafe { join_outer_progress(outer, request.required_progress_value) } {
        Ok(result) => {
            unsafe { out_result.write(result) };
            HeliosTranslatorStatus::Ok as HeliosTranslatorStatusCode
        }
        Err(status) => status as HeliosTranslatorStatusCode,
    }
}

pub(crate) extern "C" fn translator_sync_progress_query(
    host_context_cookie: *mut c_void,
    context_generation: u64,
    out_result: *mut HeliosSyncProgressResultV1,
) -> HeliosTranslatorStatusCode {
    if host_context_cookie.is_null() || out_result.is_null() {
        return HeliosTranslatorStatus::NullArgument as HeliosTranslatorStatusCode;
    }
    let outer = unsafe { &*(host_context_cookie as *const OuterDevice) };
    let Some(context) = outer.context.as_ref() else {
        return HeliosTranslatorStatus::UnknownContext as HeliosTranslatorStatusCode;
    };
    if context_generation == 0 || context_generation != context.context_generation {
        return HeliosTranslatorStatus::UnknownContext as HeliosTranslatorStatusCode;
    }
    unsafe { out_result.write(progress_result(outer, context)) };
    HeliosTranslatorStatus::Ok as HeliosTranslatorStatusCode
}

/// Every COM object this device holds that came OUT of the bridge.
///
/// `HeliosDevice` used to state an ownership rule — "declared before `dxvk` so
/// entries release their D3D11 textures before the bridge device drops" — and
/// enforce it with field declaration order for exactly ONE of its four
/// COM-owning fields. `scanout_import`, `composition_source` and `ia` all sat
/// *after* `dxvk`, so all three dropped after the bridge `UniquePtr` had run
/// `~HeliosDxvkDeviceImpl`. `ia` was patched around explicitly in
/// `ddi_destroy_device`; the other two were not.
///
/// Nothing has crashed because DXVK's `D3D11DeviceChild` holds a strong parent
/// reference, so a surviving child keeps the device alive. The concrete defect
/// is that the stated invariant was false, and the next COM-owning field added
/// below `dxvk` would inherit an unreviewed assumption — a bridge-derived
/// object that does NOT take a parent reference (a raw Vulkan-side handle, or a
/// DXGI object owned by the impl) would touch freed state in its `Drop`.
///
/// Grouping them into one field placed before `dxvk` encodes the rule for all
/// present and future members at once. [`BridgeOwned::release`] then makes
/// correctness not depend on drop order at all; `Drop` remains as the
/// rollback/panic path. R807.
pub struct BridgeOwned {
    /// Device-global shader/layout caches for lazy `ID3D11InputLayout`
    /// creation. The d3d10umddi `CreateElementLayout` DDI does NOT pass the
    /// vertex-shader input-signature bytecode that
    /// `ID3D11Device::CreateInputLayout` requires, so we stash the element
    /// descs + the bound VS bytecode and create the layout lazily at draw.
    ///
    /// Mutex, not RefCell: create/destroy DDIs mutate these from any thread
    /// once FREETHREADED caps are reported, concurrently with draw-path
    /// lookups. Lock via [`BridgeOwned::caches_lock`]; never hold the guard
    /// across a bridge/COM call (COM releases and creates run arbitrary DXVK
    /// code).
    pub caches: std::sync::Mutex<ShaderCaches>,
    /// Immediate-context pipeline binding shadow. Per-context state — each
    /// deferred context gets its own copy when command lists land.
    pub bindings: CtxBindings,
}

impl BridgeOwned {
    pub fn new() -> Self {
        Self {
            caches: std::sync::Mutex::new(ShaderCaches::default()),
            bindings: CtxBindings::default(),
        }
    }

    /// Lock the shader/layout caches, ignoring poison: with `panic = "abort"`
    /// in both profiles no unwind can ever poison the mutex, and a DDI must
    /// never panic on lock regardless.
    pub fn caches_lock(&self) -> std::sync::MutexGuard<'_, ShaderCaches> {
        self.caches
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Release every bridge-derived COM object, explicitly and in order, while
    /// the bridge device is still alive.
    ///
    /// Returns the shader caches' `(variants, layouts)` so the existing
    /// `DDI DestroyDevice: released IA cache variants=N layouts=M` line keeps
    /// its counts verbatim.
    ///
    /// Must be called on EVERY teardown path — `ddi_destroy_device` and the
    /// `CreateDevice` rollback. If it is missed, the refs do not leak forever,
    /// but they move from "released here" to "released whenever the field
    /// drops", which is the ordering this type exists to stop depending on.
    pub fn release(&mut self) -> (usize, usize) {
        self.bindings.bound_vs_com.store(0, Ordering::Relaxed);
        match self.caches.get_mut() {
            Ok(caches) => caches.release_owned_com(),
            Err(poisoned) => poisoned.into_inner().release_owned_com(),
        }
    }
}

/// `D3D11DDICAPS_FREETHREADED` (d3d10umddi.h; NOT in the bindgen output —
/// the cap *bit* macros are preprocessor defines bindgen's allowlist misses).
pub const D3D11DDICAPS_FREETHREADED: u32 = 0x1;
/// `D3D11DDICAPS_COMMANDLISTS` (deprecated at BUILD_VERSION >= 2) and
/// `D3D11DDICAPS_COMMANDLISTS_BUILD_2`. Declared ONLY for the compile-time
/// guarantee below; nothing may report them until the R812 slots are real.
pub const D3D11DDICAPS_COMMANDLISTS: u32 = 0x2;
pub const D3D11DDICAPS_COMMANDLISTS_BUILD_2: u32 = 0x4;

/// Every THREADING cap this driver can EVER report with the current slot
/// implementations. Phase C widened this together with the real deferred-
/// context/command-list slots — the pairing the R811/R812 assert existed to
/// force. The deprecated COMMANDLISTS (0x2) bit stays impossible; BUILD_2
/// devices report only 0x1|0x4.
pub const THREADING_CAPS_POSSIBLE: u32 =
    D3D11DDICAPS_FREETHREADED | D3D11DDICAPS_COMMANDLISTS_BUILD_2;

/// `D3D11DDI_THREADING_CAPS::Caps`, the value `get_caps` reports.
///
/// Phase B (`tmp/handoff-perf-structural/PLAN-commandlists.md`): FREETHREADED
/// when the `UmdFreeThreaded` knob is on (absent = ON; explicit 0 is the kill
/// switch), else 0. The runtime then calls create/destroy/calc DDIs from any
/// thread, concurrent with the immediate context — the state that exposes
/// went thread-safe in Phase A ([`ShaderCaches`] mutex and [`CtxBindings`]
/// atomics). The [`RuntimeContext`] window `Cell`s remain sound: present and
/// the other immediate-context DDIs stay runtime-serialized under FREETHREADED.
///
/// Phase C: |= COMMANDLISTS_BUILD_2 when `UmdCommandLists` is also on (the
/// knob accessor itself forces it off without FREETHREADED — COMMANDLISTS
/// requires FREETHREADED per the WDK). The deferred-context/command-list
/// slots this invites the runtime to call are REAL and installed
/// unconditionally by `install_calc_and_lifecycle`/`forward::install`, so the
/// R812 hazard (a 256-byte calc stub paired with a live Create) is gone
/// structurally: caps only decide whether the runtime USES the slots, never
/// whether they are safe to call.
pub fn threading_caps() -> u32 {
    if !crate::umd_free_threaded() {
        return 0;
    }
    let mut caps = D3D11DDICAPS_FREETHREADED;
    if crate::umd_command_lists() {
        caps |= D3D11DDICAPS_COMMANDLISTS_BUILD_2;
    }
    caps
}

/// First word of every object this driver constructs behind a
/// `D3D10DDI_HDEVICE`'s `pDrvPrivate`. A deferred context IS an HDEVICE at
/// the DDI level (there is no pfnDestroyContext — DCs are destroyed through
/// pfnDestroyDevice), so once command lists exist two different types share
/// one handle namespace and every resolver must discriminate BEFORE casting.
/// Full-word magic values, not small enums: a stray private block cannot
/// alias a valid tag by accident.
pub const HELIOS_TAG_DEVICE: usize = 0x4845_4C49_4F44_4556; // "HELIODEV"
pub const HELIOS_TAG_DEFERRED: usize = 0x4845_4C49_4F44_4643; // "HELIODFC"

/// `D3D10DDI_HDEVICE` handles whose private block carried neither tag.
/// Count + refuse, never cast: a wild cast here is the worst-risk failure of
/// the whole deferred-context feature (`ddi_destroy_device` tearing a device
/// down through a DC pointer, or vice versa).
pub static DEVICE_TAG_MISMATCH: AtomicUsize = AtomicUsize::new(0);

pub(crate) const HELIOS_MAX_LIVE_OUTER_ALLOCATIONS: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct OuterAllocationState {
    pub token: u64,
    pub allocation: u32,
    pub allocation_generation: u64,
    pub bytes: u64,
}

/// Bounded ownership of the exact live WDDM allocations for one A5 device
/// generation. Tokens only move forward; removing an entry never makes its
/// token available again.
pub struct OuterAllocationSet {
    pub(crate) next_token: u64,
    pub(crate) entries: Vec<OuterAllocationState>,
    pub(crate) internal_owned: Vec<crate::forward::InternalOuterAllocation>,
    pub(crate) pending_teardown: Vec<crate::forward::PendingOuterAllocationTeardown>,
}

impl OuterAllocationSet {
    pub(crate) fn new() -> Self {
        Self {
            next_token: 1,
            entries: Vec::new(),
            internal_owned: Vec::new(),
            pending_teardown: Vec::new(),
        }
    }
}

/// Bump the mismatch counter and log its first hits.
pub(crate) fn note_device_tag_mismatch(site: &str, tag: usize) {
    let n = DEVICE_TAG_MISMATCH.fetch_add(1, Ordering::Relaxed);
    if n < 16 {
        log_error!(
            "{site}: HDEVICE tag mismatch tag=0x{tag:016x} (x{}) — refused",
            n + 1
        );
    }
}

/// Per-device UMD state, constructed in-place in the runtime-allocated private
/// device memory (size = [`device_private_size`]). Owns the DXVK device the cxx
/// bridge created on the Helios venus adapter.
///
/// `#[repr(C)]` so `tag` is guaranteed to sit at offset 0 — the resolvers and
/// `ddi_destroy_device` read that word through the raw handle before deciding
/// which type the block is. Field DROP order is still declaration order
/// (`owned` before `dxvk`, see [`BridgeOwned`]); repr(C) fixes offsets, not
/// drop semantics.
#[repr(C)]
pub struct HeliosDevice {
    /// Always [`HELIOS_TAG_DEVICE`]; must stay the first field.
    pub tag: usize,
    /// Everything this device owns that came OUT of the bridge, in one field
    /// declared before `dxvk`. See [`BridgeOwned`] for why the position and
    /// the explicit `release()` both exist.
    pub owned: BridgeOwned,
    pub dxvk: bridge::BridgeDevice,
    /// Owns the sole A5 VkInstance, its attached runtime context/HQC1, and the
    /// exact allocation-token set. It follows DXVK in declaration/drop order
    /// so no queue callback can outlive this stable boxed cookie.
    pub outer: Box<OuterDevice>,
    pub h_rt_device: ddi::HANDLE,
    pub kt_callbacks: *const ddi::D3DDDI_DEVICECALLBACKS,
    pub dxgi_callbacks: *mut ddi::DXGI_DDI_BASE_CALLBACKS,
    /// Runtime corelayer handle + callbacks (pfnSetErrorCb) so VOID-returning
    /// DDIs can report failures to the runtime instead of leaving null handles.
    pub h_rt_core_layer: *mut core::ffi::c_void,
    pub um_callbacks: *const core::ffi::c_void,
    /// The interface level this device negotiated at CreateDevice. A deferred
    /// context created on this device fills its context-funcs table in the
    /// SAME shape (`D3D11DDIARG_CREATEDEFERREDCONTEXT`'s funcs union member is
    /// selected by the device's negotiated level).
    pub negotiated: crate::adapter::NegotiatedInterface,
}

/// Per-deferred-context UMD state, constructed in-place in the runtime-
/// allocated `hDrvContext` private memory (size =
/// [`deferred_context_private_size`]). At the DDI level a deferred context IS
/// an HDEVICE — same handle type, destroyed through `pfnDestroyDevice` — so
/// this starts with the same tag header as [`HeliosDevice`] and every
/// resolver discriminates before casting.
#[repr(C)]
pub struct HeliosDeferredContext {
    /// Always [`HELIOS_TAG_DEFERRED`]; must stay the first field.
    pub tag: usize,
    /// The device this DC records against. Valid for the DC's whole life:
    /// the runtime guarantees the device (an IC handle is first-created/
    /// last-destroyed) outlives every DC created on it.
    pub parent: *const HeliosDevice,
    /// Owned DXVK deferred COM context from `ID3D11Device::CreateDeferredContext`.
    /// Never crosses the cxx bridge — the five immediate-context static_casts
    /// in dxvk_bridge.cpp must never see a deferred pointer.
    pub dc: Option<windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext>,
    /// This DC's own pipeline binding shadow (the immediate context's copy is
    /// [`BridgeOwned::bindings`]).
    pub bindings: CtxBindings,
    /// The DC's OWN corelayer handle + callbacks: per the WDK, a DC create/
    /// record error is reported through the DC's pfnSetErrorCb, not the
    /// device's.
    pub dc_core_layer: *mut core::ffi::c_void,
    pub dc_um_callbacks: *const core::ffi::c_void,
}

pub fn deferred_context_private_size() -> usize {
    core::mem::size_of::<HeliosDeferredContext>()
}

/// Device-global shader/layout caches (see [`BridgeOwned::caches`]).
///
/// Keys are context-invariant identities — VS COM pointers and `LayoutData`
/// box pointers — never DDI handle-region addresses, so the same entries will
/// serve deferred contexts once context-local handles exist (a DC-local handle
/// region carries a copy of the same identity word).
#[derive(Default)]
pub struct ShaderCaches {
    /// VS COM pointer (as `usize`) → its DXBC input-signature bytecode.
    pub vs_bytecode: std::collections::HashMap<usize, Vec<u8>>,
    /// VS COM pointer → the flattened DDI signature words it was created with
    /// ([n_in, n_out, (sysval, reg, mask, comptype, stream) × …]); used to
    /// recompile input-class variants (see `resolve_vs_input_variant`).
    pub vs_sig_words: std::collections::HashMap<usize, Vec<u32>>,
    /// (VS COM pointer, layout input-class key) → variant VS COM pointer,
    /// recompiled with the layout's per-register numeric classes. Variants
    /// live until device teardown (bounded: shaders × distinct class sets).
    pub vs_variants: std::collections::HashMap<(usize, u64), usize>,
    /// Cache of created input layouts keyed by (LayoutData box ptr, VS COM
    /// ptr) → owned `ID3D11InputLayout` raw pointer.
    pub layout_cache: std::collections::HashMap<(usize, usize), usize>,
}

impl ShaderCaches {
    /// Release the owned COM references held by the lazy IA caches.
    ///
    /// Cache keys are non-owning runtime/DXVK identities. Only the cache
    /// values own references transferred by `CreateInputLayout` and
    /// `create_shader_sig`.
    pub fn release_owned_com(&mut self) -> (usize, usize) {
        let variant_count = self.vs_variants.values().filter(|&&raw| raw != 0).count();
        let layout_count = self.layout_cache.values().filter(|&&raw| raw != 0).count();
        let mut owned = std::collections::HashSet::new();
        owned.extend(self.vs_variants.drain().filter_map(
            |(_, raw)| {
                if raw == 0 {
                    None
                } else {
                    Some(raw)
                }
            },
        ));
        owned.extend(self.layout_cache.drain().filter_map(
            |(_, raw)| {
                if raw == 0 {
                    None
                } else {
                    Some(raw)
                }
            },
        ));
        for raw in owned {
            // SAFETY: cache values are owned COM references whose ownership
            // was transferred into the cache with `into_raw` or returned by
            // the bridge's Create* call. `from_raw` adopts exactly that ref.
            unsafe {
                drop(IUnknown::from_raw(raw as *mut c_void));
            }
        }
        (variant_count, layout_count)
    }
}

impl Drop for ShaderCaches {
    fn drop(&mut self) {
        // Normal DestroyDevice calls this explicitly before the DXVK bridge
        // drops. Keep Drop as rollback/panic-path ownership protection.
        self.release_owned_com();
    }
}

/// Per-context pipeline binding shadow (draw diagnostics + the lazy
/// input-layout keys). The immediate context's copy is
/// [`BridgeOwned::bindings`]; each deferred context will own one.
///
/// Every field is an independent relaxed atomic scalar rather than a
/// `RefCell`: a free-threaded `DestroyShader`/`DestroyElementLayout` clears a
/// matching binding concurrently with draw-path reads, and a `RefCell`
/// double-borrow there is a panic — with `panic = "abort"` an immediate dwm
/// kill. No compound invariant spans two fields: the (layout, VS) pair read in
/// `bind_input_layout` tolerates tearing because the runtime's use-counting
/// never destroys an object that is still bound, so a torn read only ever
/// sees values that were both current a moment ago.
#[derive(Default)]
pub struct CtxBindings {
    /// The VS COM pointer most recently handed to DXVK's VSSetShader (may be
    /// a variant; `current_vs` stays the runtime's own binding).
    pub bound_vs_com: AtomicUsize,
    /// Currently-bound per-stage shader COM pointers.
    pub current_vs: AtomicUsize,
    pub current_ps: AtomicUsize,
    pub current_gs: AtomicUsize,
    pub current_hs: AtomicUsize,
    pub current_ds: AtomicUsize,
    pub current_cs: AtomicUsize,
    /// Currently-bound primitive topology and first IA buffer state, for draw
    /// diagnostics on complex D3D11 content.
    pub current_topology: AtomicU32,
    pub current_vb0: AtomicUsize,
    pub current_vb0_stride: AtomicU32,
    pub current_vb0_offset: AtomicU32,
    pub current_ib: AtomicUsize,
    pub current_ib_format: AtomicU32,
    pub current_ib_offset: AtomicU32,
    /// Allocation behind RTV slot 0, for live composition diagnostics.
    pub current_rt0_alloc: AtomicU32,
    /// Dimensions/format behind RTV slot 0, for live composition diagnostics.
    pub current_rt0_width: AtomicU32,
    pub current_rt0_height: AtomicU32,
    pub current_rt0_format: AtomicU32,
    /// Currently-bound element layout's `LayoutData` raw pointer (0 = none).
    pub current_layout: AtomicUsize,
}

impl CtxBindings {
    /// Zero every shadow slot for clear-state semantics: post-
    /// `pfnCommandListExecute` the runtime treats everything as unbound and
    /// rebinds lazily.
    pub fn reset(&self) {
        self.bound_vs_com.store(0, Ordering::Relaxed);
        self.current_vs.store(0, Ordering::Relaxed);
        self.current_ps.store(0, Ordering::Relaxed);
        self.current_gs.store(0, Ordering::Relaxed);
        self.current_hs.store(0, Ordering::Relaxed);
        self.current_ds.store(0, Ordering::Relaxed);
        self.current_cs.store(0, Ordering::Relaxed);
        self.current_topology.store(0, Ordering::Relaxed);
        self.current_vb0.store(0, Ordering::Relaxed);
        self.current_vb0_stride.store(0, Ordering::Relaxed);
        self.current_vb0_offset.store(0, Ordering::Relaxed);
        self.current_ib.store(0, Ordering::Relaxed);
        self.current_ib_format.store(0, Ordering::Relaxed);
        self.current_ib_offset.store(0, Ordering::Relaxed);
        self.current_rt0_alloc.store(0, Ordering::Relaxed);
        self.current_rt0_width.store(0, Ordering::Relaxed);
        self.current_rt0_height.store(0, Ordering::Relaxed);
        self.current_rt0_format.store(0, Ordering::Relaxed);
        self.current_layout.store(0, Ordering::Relaxed);
    }

    /// Reset the pipeline-semantic shadows required when a deferred context
    /// is reborn. With tracing enabled, retain the full diagnostic reset.
    pub fn reset_for_deferred_context_rebirth(&self) {
        self.reset_for_deferred_clear_state(crate::trace_enabled());
    }

    /// Reset after ExecuteCommandList(..., FALSE).  The three semantic shadows
    /// drive the input-layout/vertex-shader variant path after the runtime
    /// clears state; the remaining slots exist only for trace diagnostics.
    /// Keep their full reset whenever either diagnostic surface is enabled.
    pub fn reset_after_command_list_execute(&self) {
        self.reset_for_deferred_clear_state(
            crate::trace_enabled() || crate::umd_deferred_diagnostics(),
        );
    }

    fn reset_for_deferred_clear_state(&self, full_diagnostic_reset: bool) {
        if full_diagnostic_reset {
            self.reset();
            return;
        }

        self.bound_vs_com.store(0, Ordering::Relaxed);
        self.current_vs.store(0, Ordering::Relaxed);
        self.current_layout.store(0, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deferred_clear_fast_path_preserves_diagnostic_shadows() {
        let bindings = CtxBindings::default();
        bindings.bound_vs_com.store(1, Ordering::Relaxed);
        bindings.current_vs.store(2, Ordering::Relaxed);
        bindings.current_layout.store(3, Ordering::Relaxed);
        bindings.current_ps.store(4, Ordering::Relaxed);
        bindings.current_topology.store(5, Ordering::Relaxed);
        bindings.current_rt0_alloc.store(6, Ordering::Relaxed);

        bindings.reset_for_deferred_clear_state(false);

        assert_eq!(bindings.bound_vs_com.load(Ordering::Relaxed), 0);
        assert_eq!(bindings.current_vs.load(Ordering::Relaxed), 0);
        assert_eq!(bindings.current_layout.load(Ordering::Relaxed), 0);
        assert_eq!(bindings.current_ps.load(Ordering::Relaxed), 4);
        assert_eq!(bindings.current_topology.load(Ordering::Relaxed), 5);
        assert_eq!(bindings.current_rt0_alloc.load(Ordering::Relaxed), 6);
    }

    #[test]
    fn deferred_clear_diagnostics_reset_every_shadow() {
        let bindings = CtxBindings::default();
        bindings.bound_vs_com.store(1, Ordering::Relaxed);
        bindings.current_vs.store(2, Ordering::Relaxed);
        bindings.current_ps.store(3, Ordering::Relaxed);
        bindings.current_gs.store(4, Ordering::Relaxed);
        bindings.current_hs.store(5, Ordering::Relaxed);
        bindings.current_ds.store(6, Ordering::Relaxed);
        bindings.current_cs.store(7, Ordering::Relaxed);
        bindings.current_topology.store(8, Ordering::Relaxed);
        bindings.current_vb0.store(9, Ordering::Relaxed);
        bindings.current_vb0_stride.store(10, Ordering::Relaxed);
        bindings.current_vb0_offset.store(11, Ordering::Relaxed);
        bindings.current_ib.store(12, Ordering::Relaxed);
        bindings.current_ib_format.store(13, Ordering::Relaxed);
        bindings.current_ib_offset.store(14, Ordering::Relaxed);
        bindings.current_rt0_alloc.store(15, Ordering::Relaxed);
        bindings.current_rt0_width.store(16, Ordering::Relaxed);
        bindings.current_rt0_height.store(17, Ordering::Relaxed);
        bindings.current_rt0_format.store(18, Ordering::Relaxed);
        bindings.current_layout.store(19, Ordering::Relaxed);

        bindings.reset_for_deferred_clear_state(true);

        assert_eq!(bindings.bound_vs_com.load(Ordering::Relaxed), 0);
        assert_eq!(bindings.current_vs.load(Ordering::Relaxed), 0);
        assert_eq!(bindings.current_ps.load(Ordering::Relaxed), 0);
        assert_eq!(bindings.current_gs.load(Ordering::Relaxed), 0);
        assert_eq!(bindings.current_hs.load(Ordering::Relaxed), 0);
        assert_eq!(bindings.current_ds.load(Ordering::Relaxed), 0);
        assert_eq!(bindings.current_cs.load(Ordering::Relaxed), 0);
        assert_eq!(bindings.current_topology.load(Ordering::Relaxed), 0);
        assert_eq!(bindings.current_vb0.load(Ordering::Relaxed), 0);
        assert_eq!(bindings.current_vb0_stride.load(Ordering::Relaxed), 0);
        assert_eq!(bindings.current_vb0_offset.load(Ordering::Relaxed), 0);
        assert_eq!(bindings.current_ib.load(Ordering::Relaxed), 0);
        assert_eq!(bindings.current_ib_format.load(Ordering::Relaxed), 0);
        assert_eq!(bindings.current_ib_offset.load(Ordering::Relaxed), 0);
        assert_eq!(bindings.current_rt0_alloc.load(Ordering::Relaxed), 0);
        assert_eq!(bindings.current_rt0_width.load(Ordering::Relaxed), 0);
        assert_eq!(bindings.current_rt0_height.load(Ordering::Relaxed), 0);
        assert_eq!(bindings.current_rt0_format.load(Ordering::Relaxed), 0);
        assert_eq!(bindings.current_layout.load(Ordering::Relaxed), 0);
    }
}

pub fn device_private_size() -> usize {
    core::mem::size_of::<HeliosDevice>()
}

// `UniformFn` and `log_backtrace` moved to `helios_umd_common::noop`
// (`DECISIONS.md` D3b, stage S2) — a WDDM UMD of either D3D version must fill
// every slot of a table it is handed, so the stub signature, the counting
// idiom and the one-shot backtrace are engine- and version-agnostic.
// Re-exported at their original paths so no call site in this crate moved.
use helios_umd_common::noop::{stub_fill_sized_table, UniformFn};
// `pub(crate)`, matching what this module exported before the move:
// `forward/deferred.rs` reaches for `crate::device_funcs::log_backtrace`.
pub(crate) use helios_umd_common::noop::log_backtrace;

static DEVICE_NOOP_LOG_COUNT: AtomicUsize = AtomicUsize::new(0);
static DXGI_NOOP_LOG_COUNT: AtomicUsize = AtomicUsize::new(0);
static WDDM13_TABLE_AUDIT_COUNT: AtomicUsize = AtomicUsize::new(0);
static WDDM21_TABLE_AUDIT_COUNT: AtomicUsize = AtomicUsize::new(0);
static DXGI13_TABLE_AUDIT_COUNT: AtomicUsize = AtomicUsize::new(0);

/// No-op DDI stub: returns 0 (S_OK for HRESULT funcs; ignored for void funcs).
///
/// The counter is the WS3 "drive noop-DDI hit counts to zero" metric and stays
/// unconditional. Only the I/O is gated: this used to do a heap-allocating
/// `format!` plus an unbuffered write under the process-global log mutex from
/// inside the runtime's call — and the very first hit additionally captured and
/// formatted 32 stack frames through `RtlCaptureStackBackTrace` — none of it
/// behind `trace_enabled()`, unlike the rest of the repeat traffic.
unsafe extern "C" fn ddi_noop_device(_a: usize) -> usize {
    let n = DEVICE_NOOP_LOG_COUNT.fetch_add(1, Ordering::Relaxed);
    if n < 512 && crate::trace_enabled() {
        if n == 0 {
            log_backtrace("DDI noop(device)");
        } else {
            log_error!("DDI noop(device) hit={n}");
        }
    }
    0
}

/// DXGI base no-op DDI stub. Kept separate so Present-adjacent missing funcs are
/// distinguishable from D3D11 device-func misses.
///
/// PROVEN DEAD (T2 R419, name-diffed against the generated structs):
/// `install_dxgi`, `install_dxgi_1_1` and `install_dxgi_1_3` overwrite all
/// 7 / 8 / 18 slots of every DXGI table, so no slot is left pointing here. The
/// deletion belongs to T6, which owns deletions; this comment records the proof
/// so the next reader does not re-derive it.
unsafe extern "C" fn ddi_noop_dxgi(_a: usize) -> usize {
    let n = DXGI_NOOP_LOG_COUNT.fetch_add(1, Ordering::Relaxed);
    if n < 256 && crate::trace_enabled() {
        if n == 0 {
            log_backtrace("DDI noop(dxgi)");
        } else {
            log_error!("DDI noop(dxgi) hit={n}");
        }
    }
    0
}

/// CalcPrivate*Size stub: return a small nonzero, pointer-aligned size so the
/// runtime's driver-private object allocation is valid. Our Create* stubs never
/// write into it and no other stub reads it, so the exact size is immaterial.
unsafe extern "C" fn ddi_calc_size(_a: usize) -> usize {
    256
}

/// `pfnRelocateDeviceFuncs` is a NOTIFICATION: the runtime has already
/// copied the (driver-filled) table to the new location and tells the driver
/// so it can update any cached table pointer. This driver caches none, so
/// the correct implementation is a counted no-op.
///
/// ⚠ It must NOT refill the table. Under command lists the runtime relocates
/// TWICE PER pfnCommandListExecute (measured 1,585,160 calls = 2 × 792k
/// executes in one Fire Strike run) on the render thread, while FREETHREADED
/// create/calc DDIs read the same table from worker threads. The old
/// refill-on-relocate (stub-sweep every slot to `ddi_noop_device`, then
/// reinstall) made a concurrent `CalcPrivate*Size` transiently return 0 —
/// the runtime then allocates a zero-byte private region, the paired Create
/// writes through it, and the heap corruption surfaces as a wild
/// call (3DMarkICFWorkload c0000005 at a data address, faulting module
/// "unknown", 2026-08-03). The per-call log line was its own T2-class cost
/// (~9k mutex-serialized writes/s); first 8 + every 65536th now.
static RELOCATE_LOG_COUNT: AtomicUsize = AtomicUsize::new(0);

fn relocate_log(tag: &str) {
    let n = RELOCATE_LOG_COUNT.fetch_add(1, Ordering::Relaxed);
    if n < 8 || n % 65536 == 0 {
        log_error!(
            "DDI RelocateDeviceFuncs({tag}) (x{}) — noted, table untouched",
            n + 1
        );
    }
}

unsafe extern "C" fn ddi_relocate_device_funcs(
    _h_device: ddi::D3D10DDI_HDEVICE,
    _funcs: *mut ddi::D3D11DDI_DEVICEFUNCS,
) {
    relocate_log("D3D11");
}

unsafe extern "C" fn ddi_relocate_device_funcs_11_1(
    _h_device: ddi::D3D10DDI_HDEVICE,
    _funcs: *mut ddi::D3D11_1DDI_DEVICEFUNCS,
) {
    relocate_log("D3D11.1");
}

unsafe extern "C" fn ddi_relocate_device_funcs_wddm1_3(
    _h_device: ddi::D3D10DDI_HDEVICE,
    _funcs: *mut ddi::D3DWDDM1_3DDI_DEVICEFUNCS,
) {
    relocate_log("WDDM1.3");
}

unsafe extern "C" fn ddi_relocate_device_funcs_wddm2_1(
    _h_device: ddi::D3D10DDI_HDEVICE,
    _funcs: *mut ddi::D3DWDDM2_1DDI_DEVICEFUNCS,
) {
    relocate_log("WDDM2.1");
}

unsafe fn audit_wddm1_3_device_funcs(tag: &str, funcs: *mut ddi::D3DWDDM1_3DDI_DEVICEFUNCS) {
    let hit = WDDM13_TABLE_AUDIT_COUNT.fetch_add(1, Ordering::Relaxed);
    if hit >= 32 || !crate::trace_enabled() {
        return;
    }

    let n = core::mem::size_of::<ddi::D3DWDDM1_3DDI_DEVICEFUNCS>() / core::mem::size_of::<usize>();
    let slots = funcs as *const usize;
    log_error!(
        "{tag}: WDDM1.3 funcs table={funcs:p} slots={n} audit={}",
        hit + 1
    );

    const EXT_NAMES: [&str; 9] = [
        "UpdateTileMappings",
        "CopyTileMappings",
        "CopyTiles",
        "UpdateTiles",
        "TiledResourceBarrier",
        "GetMipPacking",
        "ResizeTilePool",
        "SetMarker",
        "SetMarkerMode",
    ];
    for (offset, name) in EXT_NAMES.iter().enumerate() {
        let index = 155 + offset;
        if index < n {
            log_error!(
                "{tag}: WDDM1.3 slot[{index:03}] {name}=0x{:016x}",
                *slots.add(index)
            );
        }
    }

    // Exact stub identities, not an ASLR assumption. `value < 0x1_0000_0000` was
    // a guess about where modules load: it flagged every legitimately
    // low-loaded pointer and said nothing about WHICH stub a slot held. Both
    // addresses are already in scope here, so the classification is exact and
    // per-slot — that is what answers "which DDI is still a stub", at fill
    // time and by index, instead of a 32-frame backtrace at hit time.
    let noop = ddi_noop_device as UniformFn as usize;
    let calc = ddi_calc_size as UniformFn as usize;
    let mut null_slots = 0usize;
    let mut noop_slots = 0usize;
    let mut calc_slots = 0usize;
    for i in 0..n {
        let value = *slots.add(i);
        if value == 0 {
            null_slots += 1;
            log_error!("{tag}: WDDM1.3 NULL slot[{i:03}]");
        } else if value == noop {
            noop_slots += 1;
            if noop_slots <= 32 {
                log_error!("{tag}: WDDM1.3 noop slot[{i:03}]");
            }
        } else if value == calc {
            calc_slots += 1;
        }
    }
    log_error!(
        "{tag}: WDDM1.3 slots real={} noop={} calc={} null={}",
        n.saturating_sub(noop_slots + calc_slots + null_slots),
        noop_slots,
        calc_slots,
        null_slots
    );
}

unsafe fn audit_wddm2_1_device_funcs(tag: &str, funcs: *mut ddi::D3DWDDM2_1DDI_DEVICEFUNCS) {
    const EXPECTED_SLOTS: usize = 170;
    const _: () = assert!(
        core::mem::size_of::<ddi::D3DWDDM2_1DDI_DEVICEFUNCS>()
            == EXPECTED_SLOTS * core::mem::size_of::<usize>()
    );
    let hit = WDDM21_TABLE_AUDIT_COUNT.fetch_add(1, Ordering::Relaxed);
    if hit >= 32 || !crate::trace_enabled() {
        return;
    }
    let slots = funcs as *const usize;
    log_error!(
        "{tag}: WDDM2.1 funcs table={funcs:p} slots={EXPECTED_SLOTS} audit={}",
        hit + 1
    );
    const TAIL_NAMES: [&str; 6] = [
        "SetHardwareProtection",
        "GetResourceLayout",
        "RetrieveShaderComment",
        "SetHardwareProtectionState",
        "AcquireResource",
        "ReleaseResource",
    ];
    let noop = ddi_noop_device as UniformFn as usize;
    for (offset, name) in TAIL_NAMES.iter().enumerate() {
        let index = 164 + offset;
        let value = *slots.add(index);
        log_error!("{tag}: WDDM2.1 slot[{index:03}] {name}=0x{value:016x}");
        if value == 0 || value == noop {
            log_error!("{tag}: WDDM2.1 tail refusal: {name} is not typed");
        }
    }
}

unsafe fn audit_dxgi_1_3_base_funcs(tag: &str, funcs: *mut ddi::DXGI1_3_DDI_BASE_FUNCTIONS) {
    let hit = DXGI13_TABLE_AUDIT_COUNT.fetch_add(1, Ordering::Relaxed);
    if hit >= 32 || !crate::trace_enabled() {
        return;
    }

    let n = core::mem::size_of::<ddi::DXGI1_3_DDI_BASE_FUNCTIONS>() / core::mem::size_of::<usize>();
    let slots = funcs as *const usize;
    log_error!(
        "{tag}: DXGI1.3 funcs table={funcs:p} slots={n} audit={}",
        hit + 1
    );

    const NAMES: [&str; 18] = [
        "Present",
        "GetGammaCaps",
        "SetDisplayMode",
        "SetResourcePriority",
        "QueryResourceResidency",
        "RotateResourceIdentities",
        "Blt",
        "ResolveSharedResource",
        "Blt1",
        "OfferResources",
        "ReclaimResources",
        "GetMultiplaneOverlayCaps",
        "GetMultiplaneOverlayGroupCaps",
        "Reserved1",
        "PresentMultiplaneOverlay",
        "Reserved2",
        "Present1",
        "CheckPresentDurationSupport",
    ];
    for (i, name) in NAMES.iter().enumerate() {
        if i < n {
            log_error!(
                "{tag}: DXGI1.3 slot[{i:02}] {name}=0x{:016x}",
                *slots.add(i)
            );
        }
    }

    // Same exact classification as the device-funcs auditor. `noop` here should
    // never be found: install_dxgi/_1_1/_1_3 overwrite every slot of every DXGI
    // table (the proof recorded on ddi_noop_dxgi), and this line is what would
    // contradict that if it ever stopped holding.
    let noop = ddi_noop_dxgi as UniformFn as usize;
    let mut null_slots = 0usize;
    let mut noop_slots = 0usize;
    for i in 0..n {
        let value = *slots.add(i);
        if value == 0 {
            null_slots += 1;
            log_error!("{tag}: DXGI1.3 NULL slot[{i:02}]");
        } else if value == noop {
            noop_slots += 1;
            log_error!("{tag}: DXGI1.3 noop slot[{i:02}]");
        }
    }
    log_error!(
        "{tag}: DXGI1.3 slots real={} noop={} null={}",
        n.saturating_sub(noop_slots + null_slots),
        noop_slots,
        null_slots
    );
}

/// Real DestroyDevice: drop the in-place object behind the handle.
/// The runtime owns the backing memory, so we only run the destructor.
///
/// ⚠ TAG DISCRIMINATION IS LOAD-BEARING: a deferred context is destroyed
/// through THIS entry point too (no pfnDestroyContext exists). Running the
/// device teardown below on a DC handle would unregister/release/drop state
/// the parent device still owns — the single highest-risk confusion of the
/// command-list feature, which is why the tag is checked before any cast.
pub(crate) unsafe extern "C" fn ddi_destroy_device(h_device: ddi::D3D10DDI_HDEVICE) {
    if h_device.pDrvPrivate.is_null() {
        log_error!("DDI: DestroyDevice on null handle — refused");
        return;
    }
    match *(h_device.pDrvPrivate as *const usize) {
        HELIOS_TAG_DEVICE => {}
        HELIOS_TAG_DEFERRED => {
            // A deferred context dying through the shared DestroyDevice entry
            // point. Its teardown is ONLY its own state: drop the owned DXVK
            // deferred COM context and run the in-place destructor. None of
            // the device teardown below may run — the parent device still
            // owns all of it.
            crate::forward::note_deferred_context_destroyed();
            core::ptr::drop_in_place(h_device.pDrvPrivate as *mut HeliosDeferredContext);
            return;
        }
        tag => {
            note_device_tag_mismatch("DDI DestroyDevice", tag);
            return;
        }
    }
    log_error!("DDI: DestroyDevice");
    // R911: the nine refusal counters, once per device teardown. Process-global
    // rather than per-device, so this is a running total; the point is that
    // they are READ at all, which the T5 scan-out counters were not.
    log_error!("{}", crate::forward::ddi_refusal_summary());
    // The deferred-context surface, same readout discipline (Phase C).
    log_error!("{}", crate::forward::deferred_summary());
    // Bounded, process-global Present/Present1/MPO entry and callback evidence.
    // This is emitted before teardown while the UMD log remains available.
    log_error!("{}", crate::forward::present_boundary_summary());
    {
        let dev = &mut *(h_device.pDrvPrivate as *mut HeliosDevice);
        // One explicit release of everything bridge-derived, while the bridge
        // device is still alive. Pre-R807 this released `ia` only; the other
        // three COM owners relied on a drop order that did not hold.
        let (variants, layouts) = dev.owned.release();
        log_error!(
            "DDI DestroyDevice: released IA cache variants={} layouts={}",
            variants,
            layouts
        );
        dev.dxvk.shutdown();
        destroy_runtime_objects(dev);
        core::ptr::drop_in_place(h_device.pDrvPrivate as *mut HeliosDevice);
    }
}

/// Destroy runtime-owned kernel objects while the runtime device handle and
/// callback table are still valid. This is shared by normal DestroyDevice and
/// the CreateDevice rollback path.
pub unsafe fn destroy_runtime_objects(dev: &mut HeliosDevice) {
    destroy_outer_runtime_context(&mut dev.outer);
    destroy_runtime_paging_queue(&mut dev.outer);
}

pub unsafe fn destroy_runtime_paging_queue(outer: &mut OuterDevice) {
    if !outer.kt_callbacks.is_null() {
        if let Some(queue) = outer.paging_queue.take() {
            if let Some(destroy_queue_cb) = (*outer.kt_callbacks).pfnDestroyPagingQueueCb {
                let arg = ddi::D3DDDI_DESTROYPAGINGQUEUE {
                    hPagingQueue: queue.handle.get(),
                };
                let hr = destroy_queue_cb(outer.h_rt_device, &arg);
                log_error!(
                    "DDI DestroyDevice: DestroyPagingQueue hQueue=0x{:x} hr=0x{:08x}",
                    queue.handle.get(),
                    hr as u32
                );
            }
        }
    }
}

/// Detach A5 before destroying HQC1 and the exact runtime context. This helper
/// also services construction rollback, where no HeliosDevice exists yet.
pub unsafe fn destroy_outer_runtime_context(outer: &mut OuterDevice) {
    let Some(context) = outer.context.take() else {
        return;
    };

    if let Some(scope) = crate::forward::lock_ignore_poison(&context.active_scope).take() {
        let result = outer.translator.close_outer_scope(scope, None);
        log_error!("DDI outer teardown: abandoned live scope result={result:?}");
    }
    let (pending_teardown, live_allocations) =
        crate::forward::drain_outer_allocation_teardown(outer);
    if pending_teardown != 0 || live_allocations != 0 {
        outer.device_lost.store(1, Ordering::Release);
        log_error!(
            "DDI outer teardown: forced allocation rundown pending={} live={}",
            pending_teardown,
            live_allocations
        );
    }
    let detach = outer
        .translator
        .detach_outer_context(context.context_generation);
    log_error!(
        "DDI outer teardown: detach generation={} result={detach:?}",
        context.context_generation
    );

    if !outer.kt_callbacks.is_null() {
        if let Some(destroy_sync_cb) = (*outer.kt_callbacks).pfnDestroySynchronizationObjectCb {
            let arg = ddi::D3DDDICB_DESTROYSYNCHRONIZATIONOBJECT {
                hSyncObject: context.hqc1.get(),
            };
            let hr = destroy_sync_cb(outer.h_rt_device, &arg);
            log_error!(
                "DDI outer teardown: DestroySynchronizationObject hSync=0x{:x} hr=0x{:08x}",
                context.hqc1.get(),
                hr as u32
            );
        }
        if let Some(destroy_context_cb) = (*outer.kt_callbacks).pfnDestroyContextCb {
            let arg = ddi::D3DDDICB_DESTROYCONTEXT {
                hContext: context.handle.as_ptr(),
            };
            let hr = destroy_context_cb(outer.h_rt_device, &arg);
            log_error!(
                "DDI outer teardown: DestroyContext hContext={:p} hr=0x{:08x}",
                context.handle.as_ptr(),
                hr as u32
            );
        }
    }
}

/// Create the kernel context every present path submits through. Returns an
/// HRESULT: a context-less device is not a usable device — its presents run the
/// GPU copy and the flush, report S_OK to DXGI and never call `pfnPresentCb`,
/// so the swapchain token is never minted and DXGI never falls back.
pub unsafe fn create_runtime_context(outer: &mut OuterDevice) -> i32 {
    use crate::hr::E_FAIL;

    if outer.kt_callbacks.is_null() {
        log_error!("CreateDevice: no KT callbacks for CreateContext");
        return E_FAIL;
    }
    let callbacks = &*outer.kt_callbacks;
    let Some(create_context_cb) = callbacks.pfnCreateContextCb else {
        log_error!("CreateDevice: pfnCreateContextCb missing");
        return E_FAIL;
    };
    let Some(create_sync_cb) = callbacks.pfnCreateSynchronizationObject2Cb else {
        log_error!("CreateDevice: pfnCreateSynchronizationObject2Cb missing");
        return E_FAIL;
    };
    if callbacks.pfnDestroyContextCb.is_none()
        || callbacks.pfnDestroySynchronizationObjectCb.is_none()
        || callbacks.pfnRenderCb.is_none()
        || callbacks.pfnSignalSynchronizationObjectFromGpuCb.is_none()
        || callbacks.pfnWaitForSynchronizationObjectFromCpuCb.is_none()
    {
        log_error!("CreateDevice: complete Render/HQC1 callback set missing");
        return E_FAIL;
    }

    let endpoints = match outer.translator.endpoints() {
        Ok(endpoints) => endpoints,
        Err(error) => {
            log_error!("CreateDevice: A5 endpoint enumeration refused: {error:?}");
            return E_FAIL;
        }
    };
    let Some(endpoint) = endpoints.into_iter().find(|endpoint| {
        endpoint
            .validate(outer.translator.endpoint_capacity())
            .is_ok()
            && endpoint.engine_class == HELIOS_ENGINE_CLASS_GRAPHICS
    }) else {
        log_error!("CreateDevice: A5 has no validated graphics endpoint");
        return E_FAIL;
    };
    let context_generation = 1u64;
    let attach = match outer.translator.build_queue_attach(
        context_generation,
        endpoint.endpoint_id,
        HELIOS_ENGINE_CLASS_GRAPHICS,
        HELIOS_HQA1_FLAG_D3D11_PHYSICAL,
    ) {
        Ok(attach) => attach,
        Err(error) => {
            log_error!("CreateDevice: A5 HQA1 build refused: {error:?}");
            return E_FAIL;
        }
    };

    let mut arg = ddi::D3DDDICB_CREATECONTEXT::default();
    arg.NodeOrdinal = 0;
    arg.EngineAffinity = 0;
    arg.pPrivateDriverData = (&attach as *const _ as *mut c_void).cast();
    arg.PrivateDriverDataSize = core::mem::size_of_val(&attach) as u32;
    let hr = create_context_cb(outer.h_rt_device, &mut arg);
    log_error!(
        "CreateDevice: CreateContext hr=0x{:08x} hContext={:p} cmd={:p}/{} allocList={:p}/{} patchList={:p}/{}",
        hr as u32,
        arg.hContext,
        arg.pCommandBuffer,
        arg.CommandBufferSize,
        arg.pAllocationList,
        arg.AllocationListSize,
        arg.pPatchLocationList,
        arg.PatchLocationListSize
    );
    if hr != 0 {
        return hr;
    }
    // The whole group becomes meaningful at once, or the call fails. A null
    // hContext with hr == 0 would previously have left six companion fields set
    // and every consumer to discover it five checks deep.
    let Some(handle) = core::ptr::NonNull::new(arg.hContext) else {
        log_error!("CreateDevice: CreateContext returned S_OK with a null hContext");
        return E_FAIL;
    };
    let command = Window::new(arg.pCommandBuffer, arg.CommandBufferSize);
    let allocations = Window::new(arg.pAllocationList, arg.AllocationListSize);
    let patches = Window::new(arg.pPatchLocationList, arg.PatchLocationListSize);
    if command.is_none()
        || allocations.is_none()
        || patches.is_none()
        || arg.CommandBufferSize < HELIOS_HOB1_MAX_BYTES as u32
        || arg.AllocationListSize < HELIOS_HVC1_ALLOCATION_LIST_ENTRIES
        || arg.PatchLocationListSize < HELIOS_HVC1_PATCH_LOCATION_ENTRIES
    {
        log_error!(
            "CreateDevice: HOB1 windows below package minimum cmd={} alloc={} patch={}",
            arg.CommandBufferSize,
            arg.AllocationListSize,
            arg.PatchLocationListSize
        );
        if let Some(destroy_context_cb) = callbacks.pfnDestroyContextCb {
            let destroy = ddi::D3DDDICB_DESTROYCONTEXT {
                hContext: handle.as_ptr(),
            };
            let _ = destroy_context_cb(outer.h_rt_device, &destroy);
        }
        return E_FAIL;
    }

    let mut sync = ddi::D3DDDICB_CREATESYNCHRONIZATIONOBJECT2::default();
    sync.Info.Type = ddi::_D3DDDI_SYNCHRONIZATIONOBJECT_TYPE_D3DDDI_MONITORED_FENCE;
    sync.Info.Flags.__bindgen_anon_1.Value = (1 << 6) | (1 << 7);
    sync.Info.__bindgen_anon_1.MonitoredFence = Default::default();
    sync.Info.__bindgen_anon_1.MonitoredFence.InitialFenceValue = 0;
    sync.Info.__bindgen_anon_1.MonitoredFence.EngineAffinity = 1;
    let sync_hr = create_sync_cb(outer.h_rt_device, &mut sync);
    let hqc1 = core::num::NonZeroU32::new(sync.hSyncObject);
    let hqc1_cpu = core::ptr::NonNull::new(
        sync.Info
            .__bindgen_anon_1
            .MonitoredFence
            .FenceValueCPUVirtualAddress
            .cast::<u64>(),
    );
    let (Some(hqc1), Some(hqc1_cpu)) = (hqc1, hqc1_cpu) else {
        log_error!(
            "CreateDevice: HQC1 creation refused hr=0x{:08x} hSync=0x{:x} cpu={:p}",
            sync_hr as u32,
            sync.hSyncObject,
            sync.Info
                .__bindgen_anon_1
                .MonitoredFence
                .FenceValueCPUVirtualAddress
        );
        if sync.hSyncObject != 0 {
            if let Some(destroy_sync_cb) = callbacks.pfnDestroySynchronizationObjectCb {
                let destroy = ddi::D3DDDICB_DESTROYSYNCHRONIZATIONOBJECT {
                    hSyncObject: sync.hSyncObject,
                };
                let _ = destroy_sync_cb(outer.h_rt_device, &destroy);
            }
        }
        if let Some(destroy_context_cb) = callbacks.pfnDestroyContextCb {
            let destroy = ddi::D3DDDICB_DESTROYCONTEXT {
                hContext: handle.as_ptr(),
            };
            let _ = destroy_context_cb(outer.h_rt_device, &destroy);
        }
        return if sync_hr != 0 { sync_hr } else { E_FAIL };
    };
    if sync_hr != 0 {
        log_error!(
            "CreateDevice: HQC1 creation returned failure with handles hr=0x{:08x} hSync=0x{:x}",
            sync_hr as u32,
            sync.hSyncObject
        );
        if let Some(destroy_sync_cb) = callbacks.pfnDestroySynchronizationObjectCb {
            let destroy = ddi::D3DDDICB_DESTROYSYNCHRONIZATIONOBJECT {
                hSyncObject: sync.hSyncObject,
            };
            let _ = destroy_sync_cb(outer.h_rt_device, &destroy);
        }
        if let Some(destroy_context_cb) = callbacks.pfnDestroyContextCb {
            let destroy = ddi::D3DDDICB_DESTROYCONTEXT {
                hContext: handle.as_ptr(),
            };
            let _ = destroy_context_cb(outer.h_rt_device, &destroy);
        }
        return sync_hr;
    }

    outer.context = Some(RuntimeContext {
        handle,
        command: core::cell::Cell::new(command),
        allocations: core::cell::Cell::new(allocations),
        patches: core::cell::Cell::new(patches),
        context_generation,
        endpoint_id: endpoint.endpoint_id,
        context_flags: HELIOS_HQA1_FLAG_D3D11_PHYSICAL,
        hqc1,
        hqc1_cpu,
        next_progress: AtomicU64::new(1),
        last_submitted_progress: AtomicU64::new(0),
        last_batch_id: AtomicU64::new(0),
        render_lock: std::sync::Mutex::new(()),
        active_scope: std::sync::Mutex::new(None),
    });
    let cookie = outer as *mut OuterDevice as *mut c_void;
    if let Err(error) = outer.translator.attach_outer_context(
        context_generation,
        endpoint.endpoint_id,
        HELIOS_HQA1_FLAG_D3D11_PHYSICAL,
        cookie,
    ) {
        log_error!("CreateDevice: A5 outer-context attach refused: {error:?}");
        destroy_outer_runtime_context(outer);
        return E_FAIL;
    }
    log_error!(
        "CreateDevice: A5 HQA1 attached generation={} endpoint={} HQC1=0x{:x}",
        context_generation,
        endpoint.endpoint_id,
        hqc1.get()
    );
    0
}

fn direct_error_status(
    error: helios_umd_common::direct_translator::DirectTranslatorError,
) -> HeliosTranslatorStatus {
    match error {
        helios_umd_common::direct_translator::DirectTranslatorError::Refused(status) => status,
        _ => HeliosTranslatorStatus::HostCallbackFailed,
    }
}

fn progress_result(outer: &OuterDevice, context: &RuntimeContext) -> HeliosSyncProgressResultV1 {
    let last = context.last_submitted_progress.load(Ordering::Acquire);
    // SAFETY: HQC1 construction proved the runtime mapping non-null and detach
    // cannot run until A5 has released the callback reference.
    let completed = unsafe { context.hqc1_cpu.as_ptr().read_volatile() }.min(last);
    HeliosSyncProgressResultV1 {
        struct_bytes: HELIOS_TRANSLATOR_SYNC_PROGRESS_RESULT_BYTES,
        abi_version: HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION,
        completed_progress_value: completed,
        last_submitted_progress_value: last,
        flags: if outer.device_lost.load(Ordering::Acquire) != 0 {
            HELIOS_TRANSLATOR_PROGRESS_FLAG_DEVICE_LOST
        } else {
            0
        },
        reserved: 0,
    }
}

fn mark_outer_lost(outer: &OuterDevice, site: &str) {
    if outer.device_lost.swap(1, Ordering::AcqRel) == 0 {
        log_error!("A7 D3D11 outer device lost at {site}");
    }
}

/// Publish one FromGpu signal after every accepted HOB1, or an explicit join
/// against an empty context. The render mutex is the context's single writer
/// serialization; callers release it before any event-backed CPU wait.
unsafe fn signal_hqc1_locked(
    outer: &OuterDevice,
    context: &RuntimeContext,
) -> Result<u64, HeliosTranslatorStatus> {
    if outer.device_lost.load(Ordering::Acquire) != 0 || outer.kt_callbacks.is_null() {
        return Err(HeliosTranslatorStatus::DeviceLost);
    }
    let progress = context.next_progress.load(Ordering::Acquire);
    if progress == 0 || progress == u64::MAX {
        mark_outer_lost(outer, "HQC1 progress overflow");
        return Err(HeliosTranslatorStatus::DeviceLost);
    }
    let Some(signal_cb) = (*outer.kt_callbacks).pfnSignalSynchronizationObjectFromGpuCb else {
        mark_outer_lost(outer, "missing FromGpu signal callback");
        return Err(HeliosTranslatorStatus::DeviceLost);
    };
    let sync = context.hqc1.get();
    let mut signal = ddi::D3DDDICB_SIGNALSYNCHRONIZATIONOBJECTFROMGPU::default();
    signal.hContext = context.handle.as_ptr();
    signal.ObjectCount = 1;
    signal.ObjectHandleArray = &sync;
    signal.__bindgen_anon_1.MonitoredFenceValueArray = &progress;
    let hr = signal_cb(outer.h_rt_device, &signal);
    if hr != 0 {
        log_error!(
            "A7 D3D11 HQC1 FromGpu signal failed value={} hr=0x{:08x}",
            progress,
            hr as u32
        );
        mark_outer_lost(outer, "HQC1 FromGpu signal");
        return Err(HeliosTranslatorStatus::DeviceLost);
    }
    context
        .last_submitted_progress
        .store(progress, Ordering::Release);
    context.next_progress.store(progress + 1, Ordering::Release);
    Ok(progress)
}

/// Convert, copy and submit one immutable seal through the exact D3D11 Render
/// command/allocation windows. Returns None only for a named empty scope, which
/// is always closed ABANDONED and never emitted as an empty HOB1.
unsafe fn submit_outer_scope(
    outer: &OuterDevice,
    context: &RuntimeContext,
    scope: HeliosTranslatorScope,
) -> Result<Option<u64>, HeliosTranslatorStatus> {
    use helios_umd_common::direct_translator::{
        DirectHobContext, DirectIdentityRefusal, DirectResolvedUse,
    };

    let batch = match outer.translator.seal_and_copy(scope) {
        Ok(batch) => batch,
        Err(helios_umd_common::direct_translator::DirectTranslatorError::Refused(
            HeliosTranslatorStatus::ScopeEmpty,
        )) => {
            outer
                .translator
                .close_outer_scope(scope, None)
                .map_err(direct_error_status)?;
            return Ok(None);
        }
        Err(error) => {
            let status = direct_error_status(error);
            let _ = outer.translator.close_outer_scope(scope, None);
            return Err(status);
        }
    };

    let command_capacity = context.command.get().map_or(0, |window| window.capacity);
    let allocation_capacity = context
        .allocations
        .get()
        .map_or(0, |window| window.capacity);
    let hob_context = DirectHobContext {
        context_generation: context.context_generation,
        endpoint_id: context.endpoint_id,
        context_flags: context.context_flags,
        max_command_bytes: u64::from(command_capacity),
        last_batch_id: context.last_batch_id.load(Ordering::Acquire),
        allocation_list_count: allocation_capacity,
    };

    let mut resolved = Vec::with_capacity(batch.uses.len());
    let mut seen_tokens = Vec::with_capacity(batch.uses.len());
    let allocations = crate::forward::lock_ignore_poison(&outer.allocations);
    let hob = outer
        .translator
        .encode_hob1(&batch, hob_context, |index, use_record| {
            if use_record.byte_offset != 0 {
                return Err(DirectIdentityRefusal::D3D11ByteOffset);
            }
            if seen_tokens.contains(&use_record.outer_allocation_token) {
                return Err(DirectIdentityRefusal::DuplicateAssociation);
            }
            let end = use_record
                .byte_offset
                .checked_add(use_record.byte_length)
                .ok_or(DirectIdentityRefusal::RangeOverflow)?;
            let state = allocations
                .entries
                .iter()
                .find(|state| state.token == use_record.outer_allocation_token)
                .copied()
                .ok_or(DirectIdentityRefusal::MissingToken)?;
            if state.allocation == 0 || state.allocation_generation == 0 {
                return Err(DirectIdentityRefusal::StaleGeneration);
            }
            if use_record.byte_length == 0 || end > state.bytes {
                return Err(DirectIdentityRefusal::RangeOutOfBounds);
            }
            let allocation_index =
                u32::try_from(index).map_err(|_| DirectIdentityRefusal::AllocationIndexOverflow)?;
            seen_tokens.push(use_record.outer_allocation_token);
            resolved.push((state, use_record.access_flags));
            Ok(DirectResolvedUse {
                address_or_index: u64::from(allocation_index),
                allocation_generation: state.allocation_generation,
            })
        });
    drop(allocations);
    let hob = match hob {
        Ok(hob) => hob,
        Err(error) => {
            log_error!("A7 D3D11 sealed-use/HOB1 refusal: {error:?}");
            let _ = outer.translator.close_outer_scope(scope, None);
            return Err(HeliosTranslatorStatus::HostCallbackFailed);
        }
    };

    let _render_guard = crate::forward::lock_ignore_poison(&context.render_lock);
    let command_window = context.command.get();
    let allocation_window = context.allocations.get();
    let patch_window = context.patches.get();
    let Some(command_window) = command_window else {
        let _ = outer.translator.close_outer_scope(scope, None);
        return Err(HeliosTranslatorStatus::HostCallbackFailed);
    };
    let Some(allocation_window) = allocation_window else {
        let _ = outer.translator.close_outer_scope(scope, None);
        return Err(HeliosTranslatorStatus::HostCallbackFailed);
    };
    let Some(patch_window) = patch_window else {
        let _ = outer.translator.close_outer_scope(scope, None);
        return Err(HeliosTranslatorStatus::HostCallbackFailed);
    };
    if hob.as_bytes().len() > command_window.capacity as usize
        || resolved.len() > allocation_window.capacity as usize
    {
        log_error!(
            "A7 D3D11 complete batch exceeds current windows hob={}/{} uses={}/{}",
            hob.as_bytes().len(),
            command_window.capacity,
            resolved.len(),
            allocation_window.capacity
        );
        let _ = outer.translator.close_outer_scope(scope, None);
        return Err(HeliosTranslatorStatus::BatchBoundExceeded);
    }

    core::ptr::copy_nonoverlapping(
        hob.as_bytes().as_ptr(),
        command_window.ptr.as_ptr().cast::<u8>(),
        hob.as_bytes().len(),
    );
    for (index, (state, access_flags)) in resolved.iter().enumerate() {
        let mut entry = ddi::D3DDDI_ALLOCATIONLIST::default();
        entry.hAllocation = state.allocation;
        entry.__bindgen_anon_1.Value = u32::from(access_flags & HELIOS_HOB1_ACCESS_WRITE != 0);
        allocation_window.ptr.as_ptr().add(index).write(entry);
    }

    let Some(render_cb) = (*outer.kt_callbacks).pfnRenderCb else {
        let _ = outer.translator.close_outer_scope(scope, None);
        return Err(HeliosTranslatorStatus::DeviceLost);
    };
    let mut render = ddi::D3DDDICB_RENDER::default();
    render.CommandLength = hob.as_bytes().len() as u32;
    render.CommandOffset = 0;
    render.NumAllocations = resolved.len() as u32;
    render.NumPatchLocations = 0;
    render.hContext = context.handle.as_ptr();
    if command_window.capacity < HELIOS_HOB1_MAX_BYTES as u32 {
        render.Flags.__bindgen_anon_1.Value |= 1;
        render.NewCommandBufferSize = HELIOS_HOB1_MAX_BYTES as u32;
    }
    if allocation_window.capacity < HELIOS_HVC1_ALLOCATION_LIST_ENTRIES {
        render.Flags.__bindgen_anon_1.Value |= 1 << 1;
        render.NewAllocationListSize = HELIOS_HVC1_ALLOCATION_LIST_ENTRIES;
    }
    if patch_window.capacity < HELIOS_HVC1_PATCH_LOCATION_ENTRIES {
        render.Flags.__bindgen_anon_1.Value |= 1 << 2;
        render.NewPatchLocationListSize = HELIOS_HVC1_PATCH_LOCATION_ENTRIES;
    }
    let hr = render_cb(outer.h_rt_device, &mut render);
    if hr < 0 {
        log_error!(
            "A7 D3D11 HOB1 Render refused batch={} hr=0x{:08x}",
            hob.header().batch_id,
            hr as u32
        );
        let _ = outer.translator.close_outer_scope(scope, None);
        mark_outer_lost(outer, "HOB1 Render");
        return Err(HeliosTranslatorStatus::DeviceLost);
    }

    if render.NewCommandBufferSize != 0 {
        if let Some(window) = Window::new(render.pNewCommandBuffer, render.NewCommandBufferSize) {
            context.command.set(Some(window));
        }
    }
    if render.NewAllocationListSize != 0 {
        if let Some(window) = Window::new(render.pNewAllocationList, render.NewAllocationListSize) {
            context.allocations.set(Some(window));
        }
    }
    if render.NewPatchLocationListSize != 0 {
        if let Some(window) = Window::new(
            render.pNewPatchLocationList,
            render.NewPatchLocationListSize,
        ) {
            context.patches.set(Some(window));
        }
    }
    context
        .last_batch_id
        .store(hob.header().batch_id, Ordering::Release);

    let progress = match signal_hqc1_locked(outer, context) {
        Ok(progress) => progress,
        Err(status) => {
            let _ = outer.translator.close_outer_scope(scope, None);
            return Err(status);
        }
    };
    if let Err(error) = outer.translator.close_outer_scope(scope, Some(progress)) {
        log_error!("A7 D3D11 committed scope close failed: {error:?}");
        mark_outer_lost(outer, "committed scope close");
        return Err(HeliosTranslatorStatus::DeviceLost);
    }
    Ok(Some(progress))
}

unsafe fn wait_hqc1(
    outer: &OuterDevice,
    context: &RuntimeContext,
    required: u64,
) -> Result<(), HeliosTranslatorStatus> {
    if required == 0 || required > context.last_submitted_progress.load(Ordering::Acquire) {
        return Err(HeliosTranslatorStatus::HostCallbackFailed);
    }
    let event = CreateEventW(core::ptr::null_mut(), 0, 0, core::ptr::null());
    if event.is_null() {
        mark_outer_lost(outer, "HQC1 event creation");
        return Err(HeliosTranslatorStatus::DeviceLost);
    }
    let Some(wait_cb) = (*outer.kt_callbacks).pfnWaitForSynchronizationObjectFromCpuCb else {
        CloseHandle(event);
        mark_outer_lost(outer, "missing FromCpu wait callback");
        return Err(HeliosTranslatorStatus::DeviceLost);
    };
    let sync = context.hqc1.get();
    let mut wait = ddi::D3DDDICB_WAITFORSYNCHRONIZATIONOBJECTFROMCPU::default();
    wait.ObjectCount = 1;
    wait.ObjectHandleArray = &sync;
    wait.FenceValueArray = &required;
    wait.hAsyncEvent = event;
    let hr = wait_cb(outer.h_rt_device, &wait);
    if hr != 0 && hr != E_PENDING {
        CloseHandle(event);
        log_error!(
            "A7 D3D11 HQC1 FromCpu wait refused value={} hr=0x{:08x}",
            required,
            hr as u32
        );
        mark_outer_lost(outer, "HQC1 FromCpu callback");
        return Err(HeliosTranslatorStatus::DeviceLost);
    }
    let wait_result = WaitForSingleObject(event, INFINITE);
    CloseHandle(event);
    if wait_result != WAIT_OBJECT_0 || context.hqc1_cpu.as_ptr().read_volatile() < required {
        mark_outer_lost(outer, "HQC1 event completion");
        return Err(HeliosTranslatorStatus::DeviceLost);
    }
    Ok(())
}

unsafe fn join_outer_progress(
    outer: &OuterDevice,
    required_progress: u64,
) -> Result<HeliosSyncProgressResultV1, HeliosTranslatorStatus> {
    if outer.device_lost.load(Ordering::Acquire) != 0 {
        return Err(HeliosTranslatorStatus::DeviceLost);
    }
    let context = outer
        .context
        .as_ref()
        .ok_or(HeliosTranslatorStatus::UnknownContext)?;

    let mut active = crate::forward::lock_ignore_poison(&context.active_scope);
    let cut_progress = if let Some(scope) = active.take() {
        let submitted = submit_outer_scope(outer, context, scope)?;
        let progress = match submitted {
            Some(progress) => progress,
            None => {
                let _render_guard = crate::forward::lock_ignore_poison(&context.render_lock);
                signal_hqc1_locked(outer, context)?
            }
        };
        let reopened = outer
            .translator
            .open_outer_scope(context.context_generation, context.endpoint_id)
            .map_err(direct_error_status)?;
        *active = Some(reopened);
        Some(progress)
    } else {
        None
    };

    let target = if required_progress != 0 {
        if required_progress > context.last_submitted_progress.load(Ordering::Acquire) {
            return Err(HeliosTranslatorStatus::HostCallbackFailed);
        }
        required_progress
    } else if let Some(progress) = cut_progress {
        progress
    } else {
        let _render_guard = crate::forward::lock_ignore_poison(&context.render_lock);
        signal_hqc1_locked(outer, context)?
    };
    drop(active);
    wait_hqc1(outer, context, target)?;
    let result = progress_result(outer, context);
    result
        .validate_join(required_progress)
        .map_err(|_| HeliosTranslatorStatus::HostCallbackFailed)?;
    Ok(result)
}

/// DXVK opens one scope immediately around each actual lower queue submit.
/// The returned cookie is the stable outer object itself, not the A5 scope
/// pointer; a synchronous join may replace that scope before finish returns.
pub(crate) extern "C" fn dxvk_outer_submit_begin(context: *mut c_void) -> *mut c_void {
    if context.is_null() {
        return core::ptr::null_mut();
    }
    let outer = unsafe { &*(context as *const OuterDevice) };
    let Some(runtime) = outer.context.as_ref() else {
        return core::ptr::null_mut();
    };
    if outer.device_lost.load(Ordering::Acquire) != 0 {
        return core::ptr::null_mut();
    }
    let mut active = crate::forward::lock_ignore_poison(&runtime.active_scope);
    if active.is_some() {
        return core::ptr::null_mut();
    }
    match outer
        .translator
        .open_outer_scope(runtime.context_generation, runtime.endpoint_id)
    {
        Ok(scope) => {
            *active = Some(scope);
            context
        }
        Err(error) => {
            log_error!("A7 D3D11 outer scope open refused: {error:?}");
            core::ptr::null_mut()
        }
    }
}

pub(crate) extern "C" fn dxvk_outer_submit_finish(
    context: *mut c_void,
    cookie: *mut c_void,
    lower_result: i32,
) -> i32 {
    if context.is_null() {
        return VK_ERROR_DEVICE_LOST;
    }
    let outer = unsafe { &*(context as *const OuterDevice) };
    let Some(runtime) = outer.context.as_ref() else {
        return VK_ERROR_DEVICE_LOST;
    };
    let mut active = crate::forward::lock_ignore_poison(&runtime.active_scope);
    let Some(scope) = active.take() else {
        return VK_ERROR_DEVICE_LOST;
    };
    if cookie != context {
        let _ = outer.translator.close_outer_scope(scope, None);
        mark_outer_lost(outer, "outer submit cookie mismatch");
        return VK_ERROR_DEVICE_LOST;
    }
    if lower_result != VK_SUCCESS {
        let _ = outer.translator.close_outer_scope(scope, None);
        return lower_result;
    }
    match unsafe { submit_outer_scope(outer, runtime, scope) } {
        Ok(_) => VK_SUCCESS,
        Err(status) => {
            log_error!("A7 D3D11 outer submit refused: {status:?}");
            mark_outer_lost(outer, "outer submit");
            VK_ERROR_DEVICE_LOST
        }
    }
}

/// Complete the C60 GPU-dependent path on the same physical D3D11 context.
/// No lower Vulkan queue wait or independent timeline is permitted here.
pub(crate) extern "C" fn dxvk_outer_submit_join(context: *mut c_void) -> i32 {
    if context.is_null() {
        return VK_ERROR_DEVICE_LOST;
    }
    let outer = unsafe { &*(context as *const OuterDevice) };
    match unsafe { join_outer_progress(outer, 0) } {
        Ok(_) => VK_SUCCESS,
        Err(status) => {
            log_error!("A7 D3D11 exact outer join refused: {status:?}");
            mark_outer_lost(outer, "outer join");
            VK_ERROR_DEVICE_LOST
        }
    }
}

/// Direct forward half of one DXVK-internal allocation construction. The UMD
/// creates and retains the exact WDDM allocation before returning an immutable
/// HRA1 value for the one synchronous vkAllocateMemory call.
pub(crate) extern "C" fn dxvk_outer_allocation_create(
    context: *mut c_void,
    bytes: u64,
    cpu_visible: u32,
    device_local: u32,
    association_out: *mut helios_protocol::HeliosResourceAssociationV1,
) -> i32 {
    if context.is_null()
        || association_out.is_null()
        || bytes == 0
        || cpu_visible > 1
        || device_local > 1
    {
        return VK_ERROR_DEVICE_LOST;
    }
    unsafe { association_out.write(core::mem::zeroed()) };
    let outer = unsafe { &*(context as *const OuterDevice) };
    match unsafe {
        crate::forward::allocate_dxvk_internal_wddm_memory(
            outer,
            bytes,
            cpu_visible != 0,
            device_local != 0,
        )
    } {
        Ok(association) => {
            unsafe { association_out.write(association) };
            VK_SUCCESS
        }
        Err(hr) => {
            log_error!(
                "A7 D3D11 internal WDDM allocation REFUSED: bytes={} cpu_visible={} device_local={} hr=0x{:08x}",
                bytes,
                cpu_visible,
                device_local,
                hr as u32
            );
            VK_ERROR_DEVICE_LOST
        }
    }
}

/// Reverse construction edge. Internal allocations are armed here; ordinary
/// DDI resources must already have been armed by DestroyResource. Only after
/// that exact token transition succeeds may DXVK record the terminal batch.
pub(crate) extern "C" fn dxvk_outer_allocation_teardown_begin(
    context: *mut c_void,
    device_generation: u64,
    outer_allocation_token: u64,
) -> *mut c_void {
    if context.is_null() || device_generation == 0 || outer_allocation_token == 0 {
        return core::ptr::null_mut();
    }
    let outer = unsafe { &*(context as *const OuterDevice) };
    if let Err(refusal) = crate::forward::arm_dxvk_outer_allocation_teardown(
        outer,
        device_generation,
        outer_allocation_token,
    ) {
        log_error!(
            "A7 D3D11 outer teardown begin REFUSED: {:?} generation={} token={}",
            refusal,
            device_generation,
            outer_allocation_token
        );
        mark_outer_lost(outer, "outer allocation teardown begin");
        return core::ptr::null_mut();
    }
    dxvk_outer_submit_begin(context)
}

/// Reverse half of the immutable HRA1 construction edge. DXVK calls this
/// only from the exact associated allocation destructor, after Mesa has
/// accepted or refused its terminal outer batch. The callback cookie is
/// lifetime only; generation plus token remain the sole identity.
pub(crate) extern "C" fn dxvk_outer_allocation_retire(
    context: *mut c_void,
    device_generation: u64,
    outer_allocation_token: u64,
    teardown_result: i32,
) -> i32 {
    if context.is_null() || device_generation == 0 || outer_allocation_token == 0 {
        return VK_ERROR_DEVICE_LOST;
    }
    let outer = unsafe { &*(context as *const OuterDevice) };
    let retired = unsafe {
        crate::forward::retire_outer_allocation(outer, device_generation, outer_allocation_token)
    };
    if let Err(refusal) = retired {
        log_error!(
            "A7 D3D11 outer allocation retire REFUSED: {:?} generation={} token={}",
            refusal,
            device_generation,
            outer_allocation_token
        );
        mark_outer_lost(outer, "outer allocation retire");
        return VK_ERROR_DEVICE_LOST;
    }
    if teardown_result != VK_SUCCESS {
        log_error!(
            "A7 D3D11 outer allocation retired after failed terminal batch: generation={} token={} result={}",
            device_generation,
            outer_allocation_token,
            teardown_result
        );
        mark_outer_lost(outer, "outer allocation terminal batch");
        return teardown_result;
    }
    VK_SUCCESS
}

/// Create the monitored-fence paging queue required by WDDM 2.x
/// pfnMakeResidentCb. Returns an HRESULT and leaves `paging_queue` empty on
/// failure.
pub unsafe fn create_runtime_paging_queue(outer: &mut OuterDevice) -> i32 {
    use crate::hr::E_FAIL;

    if outer.kt_callbacks.is_null() {
        log_error!("CreateDevice: no KT callbacks for CreatePagingQueue");
        return E_FAIL;
    }
    let Some(create_queue_cb) = (*outer.kt_callbacks).pfnCreatePagingQueueCb else {
        log_error!("CreateDevice: pfnCreatePagingQueueCb missing");
        return E_FAIL;
    };

    let mut arg = ddi::D3DDDICB_CREATEPAGINGQUEUE::default();
    // D3DDDI_PAGINGQUEUE_PRIORITY_NORMAL == 0.
    arg.Priority = 0;
    arg.PhysicalAdapterIndex = 0;
    let hr = create_queue_cb(outer.h_rt_device, &mut arg);
    log_error!(
        "CreateDevice: CreatePagingQueue hr=0x{:08x} hQueue=0x{:x} hSync=0x{:x} fence={:p}",
        hr as u32,
        arg.hPagingQueue,
        arg.hSyncObject,
        arg.FenceValueCPUVirtualAddress
    );
    if hr != 0 {
        return hr;
    }

    let queue = core::num::NonZeroU32::new(arg.hPagingQueue);
    let sync_object = core::num::NonZeroU32::new(arg.hSyncObject);
    let fence_value_cpu = core::ptr::NonNull::new(arg.FenceValueCPUVirtualAddress.cast::<u64>());
    let (Some(handle), Some(sync_object), Some(fence_value_cpu)) =
        (queue, sync_object, fence_value_cpu)
    else {
        log_error!("CreateDevice: CreatePagingQueue returned invalid outputs");
        if let Some(destroy_queue_cb) = (*outer.kt_callbacks).pfnDestroyPagingQueueCb {
            if arg.hPagingQueue != 0 {
                let destroy = ddi::D3DDDI_DESTROYPAGINGQUEUE {
                    hPagingQueue: arg.hPagingQueue,
                };
                let _ = destroy_queue_cb(outer.h_rt_device, &destroy);
            }
        }
        return E_FAIL;
    };

    outer.paging_queue = Some(RuntimePagingQueue {
        handle,
        sync_object,
        fence_value_cpu,
    });
    0
}

/// Bulk-fill every pointer slot of a device-funcs table with `ddi_noop_device`
/// and return the D3D11.0-typed view of it.
///
/// The slot count comes from `size_of::<T>()`, so it CANNOT disagree with the
/// table actually being filled. That matters: the failure mode this replaces is
/// a wrong length under-stubbing a table and leaving uninitialised slots past
/// the prefix, which is precisely what `fill_dxgi_1_3_base_funcs`'s comment
/// exists to warn about. The three fills each spelled the length out by hand.
///
/// # Safety
/// `funcs` must point to a writable `T` whose every field is a pointer-sized
/// `Option<fn>`, and `T` must be a layout-compatible extension of
/// `D3D11DDI_DEVICEFUNCS` (a WDK header property no Rust type can assert).
unsafe fn stub_fill_device_table<T>(funcs: *mut T) -> *mut ddi::D3D11DDI_DEVICEFUNCS {
    // ⚠ Deriving the slot count from `size_of::<T>()` is correct HERE and only
    // here: the d3d10umddi device-funcs tables carry no size argument, so the
    // type IS the contract. `d3d12umddi`'s `pfnFillDDITable` passes a `SIZE_T`
    // (`:2527-2528`) and the D3D12 filler must use it — `umd_common`'s
    // `stub_fill_bytes` is that primitive, and this is the convenience on top.
    // ARCHITECTURE §12 rule 16 / R702: 24H2 passed 576 bytes for a 592-byte
    // DRIVERCAPS.
    unsafe { stub_fill_sized_table(funcs, ddi_noop_device) };
    funcs as *mut ddi::D3D11DDI_DEVICEFUNCS
}

/// The `CalcPrivate*Size` entries and the two real lifecycle entries every
/// device-funcs table gets, applied identically at every interface level.
///
/// Was three verbatim copies of one 18-name `calc!` list plus the same two
/// assignments and the same 10-line rationale comment.
///
/// # Safety
/// `f` must be the D3D11.0-typed view of a stub-filled table.
unsafe fn install_calc_and_lifecycle(f: &mut ddi::D3D11DDI_DEVICEFUNCS) {
    // CalcPrivate*Size funcs must return a valid nonzero size.
    macro_rules! calc {
        ($($field:ident),* $(,)?) => {$(
            f.$field = core::mem::transmute::<UniformFn, _>(ddi_calc_size as UniformFn);
        )*};
    }
    calc!(
        pfnCalcPrivateResourceSize,
        pfnCalcPrivateOpenedResourceSize,
        pfnCalcPrivateShaderResourceViewSize,
        pfnCalcPrivateRenderTargetViewSize,
        pfnCalcPrivateDepthStencilViewSize,
        pfnCalcPrivateElementLayoutSize,
        pfnCalcPrivateBlendStateSize,
        pfnCalcPrivateDepthStencilStateSize,
        pfnCalcPrivateRasterizerStateSize,
        pfnCalcPrivateShaderSize,
        pfnCalcPrivateGeometryShaderWithStreamOutput,
        pfnCalcPrivateSamplerSize,
        pfnCalcPrivateQuerySize,
        pfnCalcPrivateTessellationShaderSize,
        pfnCalcPrivateUnorderedAccessViewSize,
    );
    // The deferred-context/command-list size family is REAL (Phase C), no
    // longer the 256-byte stub: the paired Create slots are live in
    // `forward::install`, so a stub size here would be exactly the R812 heap
    // corruption the old compile-time assert guarded against. The deprecated
    // COMMANDLISTS (0x2) bit remains impossible — BUILD_2 devices report
    // 0x1|0x4 only.
    const _: () = assert!(THREADING_CAPS_POSSIBLE & D3D11DDICAPS_COMMANDLISTS == 0);
    f.pfnCalcDeferredContextHandleSize = Some(crate::forward::calc_deferred_context_handle_size);
    f.pfnCalcPrivateDeferredContextSize = Some(crate::forward::calc_private_deferred_context_size);
    f.pfnCalcPrivateCommandListSize = Some(crate::forward::calc_private_command_list_size);
    // Not a size getter -- a void writer. Installed with its real signature, no
    // transmute, identically in every table. R812; real array since Phase C.
    f.pfnCheckDeferredContextHandleSizes =
        Some(crate::forward::check_deferred_context_handle_sizes);

    // Real cleanup on device teardown (matching signature, no transmute).
    f.pfnDestroyDevice = Some(ddi_destroy_device);
}

/// Fill every entry of a `D3D11DDI_DEVICEFUNCS` table with safe stubs, then
/// specialise the entries whose behaviour matters for device creation.
///
/// # Safety
/// `funcs` must point to a writable `D3D11DDI_DEVICEFUNCS` (the runtime's table,
/// selected when Interface == D3D11_0_DDI_INTERFACE_VERSION).
pub unsafe fn fill_d3d11_device_funcs(funcs: *mut ddi::D3D11DDI_DEVICEFUNCS) {
    let f = &mut *stub_fill_device_table(funcs);
    install_calc_and_lifecycle(f);
    f.pfnRelocateDeviceFuncs = Some(ddi_relocate_device_funcs);

    // Override stubs with the real D3D11 COM forwarders. The 11.0 table stops
    // here: the returned proof is what the higher levels below consume.
    let _base = crate::forward::install(funcs);
}

/// Fill a D3D11.1 device-funcs table. The D3D11.1 layout is an extension of the
/// D3D11.0 prefix, so the implemented forwarders can be installed through the
/// D3D11.0 view after the whole larger table has been stub-filled.
pub unsafe fn fill_d3d11_1_device_funcs(funcs: *mut ddi::D3D11_1DDI_DEVICEFUNCS) {
    let f = &mut *stub_fill_device_table(funcs);
    install_calc_and_lifecycle(f);
    (*funcs).pfnRelocateDeviceFuncs = Some(ddi_relocate_device_funcs_11_1);

    // The ordering is now structural, not textual: `install_11_1` cannot be
    // called without the `Filled11_0` token `install` returns.
    let base = crate::forward::install(f);
    let _l1 = crate::forward::install_11_1(base, funcs);
}

pub unsafe fn fill_wddm1_3_device_funcs(funcs: *mut ddi::D3DWDDM1_3DDI_DEVICEFUNCS) {
    let f = &mut *stub_fill_device_table(funcs);
    install_calc_and_lifecycle(f);
    (*funcs).pfnRelocateDeviceFuncs = Some(ddi_relocate_device_funcs_wddm1_3);

    let base = crate::forward::install(f);
    let l1 = crate::forward::install_11_1(base, funcs as *mut ddi::D3D11_1DDI_DEVICEFUNCS);
    let _l13 = crate::forward::install_wddm1_3(l1, funcs);
    audit_wddm1_3_device_funcs("FillDeviceFuncs", funcs);
}

pub unsafe fn fill_wddm2_1_device_funcs(funcs: *mut ddi::D3DWDDM2_1DDI_DEVICEFUNCS) {
    let f = &mut *stub_fill_device_table(funcs);
    install_calc_and_lifecycle(f);
    (*funcs).pfnRelocateDeviceFuncs = Some(ddi_relocate_device_funcs_wddm2_1);

    let base = crate::forward::install(f);
    let l1 = crate::forward::install_11_1(base, funcs as *mut ddi::D3D11_1DDI_DEVICEFUNCS);
    let l13 = crate::forward::install_wddm1_3(l1, funcs as *mut ddi::D3DWDDM1_3DDI_DEVICEFUNCS);
    let _l21 = crate::forward::install_wddm2_1(l13, funcs);
    audit_wddm2_1_device_funcs("FillDeviceFuncs", funcs);
}

/// Fill the DXGI base DDI table (presentation/resource base funcs) the runtime
/// hands us in the CREATEDEVICE args. All stubbed for Milestone 1 (no present).
///
/// # Safety
/// `funcs` must point to a writable `DXGI_DDI_BASE_FUNCTIONS`, or be null.
pub unsafe fn fill_dxgi_base_funcs(funcs: *mut ddi::DXGI_DDI_BASE_FUNCTIONS) {
    if funcs.is_null() {
        return;
    }
    let n = core::mem::size_of::<ddi::DXGI_DDI_BASE_FUNCTIONS>() / core::mem::size_of::<usize>();
    let slots = funcs as *mut Option<UniformFn>;
    for i in 0..n {
        *slots.add(i) = Some(ddi_noop_dxgi);
    }
    // Real (benign) present so LogonUI/DWM don't fail-fast on present.
    crate::forward::install_dxgi(funcs);
}

/// Fill the DXGI 1.1 base table. This is required for D3D11.1 device creation
/// because the table adds pfnResolveSharedResource after the D3D10-era prefix.
pub unsafe fn fill_dxgi_1_1_base_funcs(funcs: *mut ddi::DXGI1_1_DDI_BASE_FUNCTIONS) {
    if funcs.is_null() {
        return;
    }
    let n = core::mem::size_of::<ddi::DXGI1_1_DDI_BASE_FUNCTIONS>() / core::mem::size_of::<usize>();
    let slots = funcs as *mut Option<UniformFn>;
    for i in 0..n {
        *slots.add(i) = Some(ddi_noop_dxgi);
    }
    crate::forward::install_dxgi(funcs as *mut ddi::DXGI_DDI_BASE_FUNCTIONS);
    crate::forward::install_dxgi_1_1(funcs);
}

/// Fill the DXGI 1.3 base table required by WDDM1.3 devices. DWM can call the
/// later Present1/MPO/residency slots immediately after CreateDevice; handing it
/// only the DXGI 1.1 prefix leaves uninitialized callback pointers past slot 7.
pub unsafe fn fill_dxgi_1_3_base_funcs(funcs: *mut ddi::DXGI1_3_DDI_BASE_FUNCTIONS) {
    if funcs.is_null() {
        return;
    }
    let n = core::mem::size_of::<ddi::DXGI1_3_DDI_BASE_FUNCTIONS>() / core::mem::size_of::<usize>();
    let slots = funcs as *mut Option<UniformFn>;
    for i in 0..n {
        *slots.add(i) = Some(ddi_noop_dxgi);
    }
    crate::forward::install_dxgi(funcs as *mut ddi::DXGI_DDI_BASE_FUNCTIONS);
    crate::forward::install_dxgi_1_1(funcs as *mut ddi::DXGI1_1_DDI_BASE_FUNCTIONS);
    crate::forward::install_dxgi_1_3(funcs);
    audit_dxgi_1_3_base_funcs("FillDXGIBaseFuncs", funcs);
}
