//! Per-D3D-device, per-context, and per-process state, plus their DDIs.
//!
//! Phase 1 implements device alloc/free (so the runtime can open a device
//! without crashing). Context and GPU-VA process DDIs are stubbed until the
//! Venus path (Phase 4) and the memory model (Phase 3) land.
//!
//! NOTE: the exact argument struct/handle types below come from the generated
//! `dxgk` bindings and may need a binding-alignment pass at first compile.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::ffi::c_void;
use core::sync::atomic::{AtomicU32, Ordering};

use crate::adapter::AdapterContext;
use crate::ddi::native_render::{NativeClass, NativeContext};
use crate::ddi::translation_session::{
    self as hts1, ProcessSessionList, SessionEndpointObject,
    SessionObject as TranslationSessionObject,
};
use crate::dxgk::*;
use crate::irql::PassiveLevel;

/// State for one D3D device opened on the adapter.
pub struct DeviceContext {
    /// Back-pointer to the owning adapter (valid for the device's lifetime).
    ///
    /// PRIVATE. The only route from a device handle to an `&AdapterContext` is
    /// [`DeviceHandleRef`]'s checked traversal — see its docs for the cast this
    /// prevents.
    adapter: *mut AdapterContext,
    /// Documented WDDM KMD-process token (`DXGKARG_CREATEDEVICE.hKmdProcess`).
    /// This exact per-process object association is used by session and outer
    /// allocation ownership; it is never replaced by a PID or thread identity.
    creator_process: usize,
    /// K5: the one HTS1 session this raw KMT device's HVC1 control context
    /// created, if it is a raw device at all. `None` for every ordinary D3D
    /// device, which is most of them. §10.7:1720-1721 permits exactly one.
    session: crate::sync::SpinLock<Option<core::ptr::NonNull<TranslationSessionObject>>>,
    /// F21: the one direct HTS1 session used by this ordinary D3D device's
    /// HQA1 contexts, plus the bounded exact set of device-specific allocation
    /// objects opened on this device.  This is deliberately device-owned: it
    /// is neither a process allocation table nor a scalar-identity lookup.
    outer: crate::sync::SpinLock<DeviceOuterState>,
}

const MAX_DEVICE_OUTER_OPENS: usize = 4096;

struct DeviceOuterState {
    session: Option<core::ptr::NonNull<TranslationSessionObject>>,
    session_generation: u64,
    context_kind: u32,
    /// Transient one-shot host-attachment phase. OpenAllocation refuses while
    /// this is set, so the snapshot being attached cannot gain an unbound tail.
    binding: bool,
    /// Sticky after any partial binding failure. Some opens may then retain
    /// host attachment custody until their ordinary reverse teardown; no later
    /// context may reinterpret that partial set as complete.
    failed: bool,
    opens: crate::sync::FixedVec<usize>,
}

/// Tag proving a `HANDLE` really is a [`ContextContext`] — must be the FIRST
/// field, like `AllocationContext`'s.
const CONTEXT_CTX_MAGIC: u32 = 0x4843_5458; // "HCTX"

/// Non-null context handles that failed the magic check.
///
/// ⛔ MUST READ 0, and it exists because `DXGKARG_PATCH` and
/// `DXGKARG_SUBMITCOMMAND` deliver the context through a
/// `union { hDevice; hContext; }`. The header says the `hContext` arm is the one
/// a `SCHEDULINGCAPS_MULTI_ENGINE_AWARE` driver gets — this driver reports that
/// bit — but "the union is always the arm I expect" is exactly the assumption
/// [`ContextHandleRef`] was created to stop being an assumption, and reading a
/// `DeviceContext` as a `ContextContext` would find `helios` at the wrong offset
/// and dispatch on garbage. A nonzero value here is that hypothesis confirmed.
pub static CONTEXT_HANDLE_REFUSED: AtomicU32 = AtomicU32::new(0);

/// State for one scheduler context opened on a D3D device.
///
/// ⛔ `#[repr(C)]` IS LOAD-BEARING, not decoration. [`ContextHandleRef::from_raw`]
/// reads `magic` through `addr_of!` on a handle that may not be one of ours at
/// all — that is the whole point of the check — and Rust's default repr is free
/// to place a lone `u32` among these pointers at ANY offset. At a high offset,
/// the probe meant to reject a foreign object would read past the end of it
/// first. `repr(C)` is what puts `magic` at offset 0, where the comment below
/// has always claimed it was.
#[repr(C)]
pub struct ContextContext {
    /// [`CONTEXT_CTX_MAGIC`] — must be the FIRST field.
    magic: u32,
    /// Back-pointer to the owning device (valid for the context's lifetime).
    /// PRIVATE, for the same reason as [`DeviceContext::adapter`].
    device: *mut DeviceContext,
    /// K5: what this context is, and the strong session reference it holds.
    /// Written once at create and read once at destroy — the live context object
    /// is the identity, and nothing looks the numeric generation up again
    /// (§10.4:1250-1253).
    helios: HeliosContextRole,
}

/// What a context is to K5 and K6. `Legacy` is every D3D-runtime and CDD context
/// and holds nothing.
enum HeliosContextRole {
    Legacy,
    /// The one HVC1 control context of a raw KMT device: host ring 0, the HTS1
    /// session, and the only class that may carry an HNR2 reply.
    Control {
        session: core::ptr::NonNull<TranslationSessionObject>,
        native: alloc::boxed::Box<NativeContext>,
    },
    /// An HVC1 queue context on the same raw device. It holds its own strong
    /// session reference — the control context's belongs to the device — so the
    /// session cannot be freed under a queue context that outlives it.
    Queue {
        session: core::ptr::NonNull<TranslationSessionObject>,
        native: alloc::boxed::Box<NativeContext>,
    },
    /// An HQA1-attached outer context, with the generation it was admitted under.
    Attached {
        session: core::ptr::NonNull<TranslationSessionObject>,
        context_generation: u64,
        /// Direct fixed endpoint ownership established at HQA1 admission. K11
        /// retains this strong edge but executes only pure INIT; a later
        /// allocation/GPU unit may read its ring and dispatch serial without
        /// rediscovering the session at submit time (§10.4:1255-1258).
        #[allow(dead_code)]
        endpoint: core::ptr::NonNull<SessionEndpointObject>,
        /// Outer profile of the existing post-K9 executor.  The endpoint above
        /// remains the sole host context; this object owns only bounded guest
        /// validation, custody, and ordered-completion state.
        native: alloc::boxed::Box<NativeContext>,
        /// K6: the HOS1 gate `DxgkDdiSubmitCommandVirtual` validates against.
        /// Present on both arms because the arm itself is one of the things
        /// HOS1 checks — a D3D11-physical context must refuse a HOS1, and it can
        /// only do that if it carries its arm.
        outer: crate::sync::SpinLock<helios_kmd_logic::native_render::OuterSubmitContext>,
    },
}

impl DeviceContext {
    /// The bounded HTS1 session list of the `hKmdProcess` this device was
    /// created under, if it has one.
    ///
    /// # Safety of the cast
    /// `creator_process` is `DXGKARG_CREATEDEVICE::hKmdProcess`, which dxgkrnl
    /// obtained from `DxgkDdiCreateProcess`'s `Box::into_raw` and round-trips
    /// verbatim; dxgkrnl destroys a process's devices before the process. Zero
    /// means the device has no process object and every K5 arm refuses.
    fn process_sessions(&self) -> Option<&ProcessSessionList> {
        // SAFETY: the pointer is our own `ProcessContext`, live for at least as
        // long as this device.
        unsafe { (self.creator_process as *const ProcessContext).as_ref() }
            .map(|process| &process.sessions)
    }

    /// Publish the exact session carried by the first HQA1 context and bind all
    /// allocation opens that predate it.  Later opens observe the published
    /// direct edge and bind themselves before becoming visible in `opens`.
    fn bind_outer_session(
        &self,
        passive: PassiveLevel,
        session: core::ptr::NonNull<TranslationSessionObject>,
        session_generation: u64,
        context_kind: u32,
    ) -> bool {
        if session_generation == 0 || context_kind == 0 {
            return false;
        }

        {
            let mut state = self.outer.lock();
            if state.failed || state.binding {
                return false;
            }
            match state.session {
                Some(found) => {
                    return found == session
                        && state.session_generation == session_generation
                        && state.context_kind == context_kind;
                }
                None => state.binding = true,
            }
        }
        let Some(retained) = hts1::retain_direct_execution_session(session) else {
            let mut state = self.outer.lock();
            state.binding = false;
            state.failed = true;
            return false;
        };

        // Acquire every open's small rundown while the device table prevents
        // CloseAllocation from removing and freeing it.  Host CTX_ATTACH calls
        // happen only after the device spinlock is released.
        let count = self.outer.lock().opens.len();
        let mut guards = Vec::new();
        if guards.try_reserve_exact(count).is_err() {
            let mut state = self.outer.lock();
            state.binding = false;
            state.failed = true;
            drop(state);
            unsafe { hts1::release_execution_session(retained) };
            return false;
        }
        {
            let state = self.outer.lock();
            for &open in state.opens.as_slice() {
                let Some(guard) =
                    (unsafe { crate::ddi::create_allocation::acquire_outer_bind_guard(open) })
                else {
                    drop(state);
                    let mut state = self.outer.lock();
                    state.binding = false;
                    state.failed = true;
                    drop(state);
                    unsafe { hts1::release_execution_session(retained) };
                    return false;
                };
                guards.push(guard);
            }
        }
        let attached = guards
            .iter()
            .all(|guard| guard.bind(passive, session, session_generation));
        let published = {
            let mut state = self.outer.lock();
            let complete = attached
                && state.binding
                && !state.failed
                && state.session.is_none()
                && state.opens.len() == count;
            state.binding = false;
            if complete {
                state.session = Some(retained);
                state.session_generation = session_generation;
                state.context_kind = context_kind;
            } else {
                state.failed = true;
            }
            complete
        };
        if !published {
            unsafe { hts1::release_execution_session(retained) };
        }
        published
    }

    pub(crate) fn register_outer_open(&self, open: usize) -> bool {
        if open == 0 {
            return false;
        }
        let session = {
            let state = self.outer.lock();
            if state.failed || state.binding || state.opens.as_slice().contains(&open) {
                return false;
            }
            state
                .session
                .map(|session| (session, state.session_generation))
        };
        if let Some((session, generation)) = session {
            let Some(guard) =
                (unsafe { crate::ddi::create_allocation::acquire_outer_bind_guard(open) })
            else {
                return false;
            };
            if !guard.bind(unsafe { PassiveLevel::assume() }, session, generation) {
                return false;
            }
        }
        let mut state = self.outer.lock();
        if state.failed
            || state.binding
            || state.opens.as_slice().contains(&open)
            || session.is_some_and(|(session, generation)| {
                state.session != Some(session) || state.session_generation != generation
            })
        {
            return false;
        }
        state.opens.push(open)
    }

    pub(crate) fn unregister_outer_open(&self, open: usize) {
        let mut state = self.outer.lock();
        if let Some(index) = state
            .opens
            .as_slice()
            .iter()
            .position(|value| *value == open)
        {
            let _ = state.opens.swap_remove(index);
        }
    }

    pub(crate) fn acquire_outer_gpuva_use(
        &self,
        session: core::ptr::NonNull<TranslationSessionObject>,
        gpuva: u64,
        bytes: u64,
        expected_generation: Option<u64>,
        require_hoc1: bool,
    ) -> Option<crate::ddi::create_allocation::OpenOuterUse> {
        let state = self.outer.lock();
        if state.session != Some(session)
            || state.session_generation == 0
            || self.creator_process == 0
        {
            return None;
        }
        let mut found = None;
        for &open in state.opens.as_slice() {
            let candidate = unsafe {
                crate::ddi::create_allocation::acquire_outer_gpuva_use(
                    open,
                    session,
                    self.creator_process,
                    gpuva,
                    bytes,
                    expected_generation,
                    require_hoc1,
                )
            };
            if let Some(candidate) = candidate {
                if found.is_some() {
                    return None;
                }
                found = Some(candidate);
            }
        }
        found
    }

    /// Resolve one D3D11 HOB1 allocation-list entry through this device's exact
    /// bounded open set. A live device-specific handle is insufficient by
    /// itself: the open must belong to this device and this direct session.
    pub(crate) fn acquire_outer_physical_use(
        &self,
        session: core::ptr::NonNull<TranslationSessionObject>,
        open: HANDLE,
        expected_generation: u64,
        bytes: u64,
    ) -> Option<crate::ddi::create_allocation::OpenOuterUse> {
        if open.is_null() || expected_generation == 0 || bytes == 0 {
            return None;
        }
        let state = self.outer.lock();
        if state.session != Some(session)
            || state.session_generation == 0
            || self.creator_process == 0
            || !state.opens.as_slice().contains(&(open as usize))
        {
            return None;
        }
        unsafe {
            crate::ddi::create_allocation::acquire_outer_physical_use(
                open,
                session,
                expected_generation,
                bytes,
            )
        }
    }

    /// Name the refusal arm for a failed [`Self::acquire_outer_physical_use`].
    /// Diagnostic only; device-level arms are 1 (state/session) and 0
    /// (open not registered on this device), the rest come from
    /// [`crate::ddi::create_allocation::diagnose_outer_physical_use`].
    pub(crate) fn diagnose_outer_physical_use(
        &self,
        session: core::ptr::NonNull<TranslationSessionObject>,
        open: HANDLE,
        expected_generation: u64,
        bytes: u64,
    ) -> u32 {
        let state = self.outer.lock();
        if state.session != Some(session)
            || state.session_generation == 0
            || self.creator_process == 0
        {
            return 1;
        }
        if !state.opens.as_slice().contains(&(open as usize)) {
            return 0;
        }
        drop(state);
        unsafe {
            crate::ddi::create_allocation::diagnose_outer_physical_use(
                open,
                session,
                expected_generation,
                bytes,
            )
        }
    }
}

/// Typed borrowed view of a scheduler context handle.
///
/// DDI handle types are all C `HANDLE`s, so a direct cast can compile even when
/// the callback actually received an hContext rather than an hAdapter. Keeping
/// the traversal here makes the ownership chain explicit:
/// ContextContext -> DeviceContext -> AdapterContext.
pub struct ContextHandleRef<'a> {
    context: &'a ContextContext,
}

impl<'a> ContextHandleRef<'a> {
    /// # Safety
    /// `handle` is either null, a live hContext returned by
    /// [`dxgkddi_create_context`], or — from the DDIs whose argument struct
    /// carries the `hDevice`/`hContext` union — something else entirely. The
    /// magic check below is what makes the third case a counted refusal instead
    /// of a dispatch on a misread struct; that a magic-matching pointer really
    /// is live is dxgkrnl's contract and is not encodable.
    pub unsafe fn from_raw(handle: HANDLE) -> Option<Self> {
        if handle.is_null() {
            return None;
        }
        let p = handle as *const ContextContext;
        if !p.is_aligned() {
            CONTEXT_HANDLE_REFUSED.fetch_add(1, Ordering::Relaxed);
            return None;
        }
        // SAFETY: non-null and aligned. Reading ONLY the magic through
        // `addr_of!` + `read_unaligned` asserts nothing about the rest of the
        // referent, which is the property a `&*` cast would assert with no
        // evidence — the `open_allocation_context` shape, same reason.
        let magic = unsafe { core::ptr::read_unaligned(core::ptr::addr_of!((*p).magic)) };
        if magic != CONTEXT_CTX_MAGIC {
            CONTEXT_HANDLE_REFUSED.fetch_add(1, Ordering::Relaxed);
            return None;
        }
        // SAFETY: the magic matched, so this is one of our contexts.
        Some(Self {
            context: unsafe { &*p },
        })
    }

    pub fn adapter(&self) -> Option<&'a AdapterContext> {
        let device = unsafe { self.context.device.as_ref() }?;
        unsafe { device.adapter.as_ref() }
    }

    /// K6's native-render state and the session it belongs to, or `None` for
    /// every context that is not an HVC1 one.
    ///
    /// ⛔ THIS IS THE BRANCH `dxgkddi_render` MUST TAKE, and it must branch on
    /// the ROLE and not on a fourth command magic. The three existing arms in
    /// that function are magic-disjoint by construction; a magic-keyed HNR2 arm
    /// would let a legacy DWM Render take the HNR2 path and then still hit the
    /// tail memcpy.
    pub(crate) fn native(
        &self,
    ) -> Option<(
        &'a NativeContext,
        core::ptr::NonNull<TranslationSessionObject>,
    )> {
        match &self.context.helios {
            HeliosContextRole::Control { session, native }
            | HeliosContextRole::Queue { session, native } => Some((native, *session)),
            HeliosContextRole::Legacy | HeliosContextRole::Attached { .. } => None,
        }
    }

    /// Complete direct ownership required by the outer executor.  No value is
    /// rediscovered at submit: all four references live in this exact context
    /// and its owning device.
    pub(crate) fn outer_native(
        &self,
    ) -> Option<(
        &'a NativeContext,
        core::ptr::NonNull<TranslationSessionObject>,
        &'a DeviceContext,
        &'a crate::sync::SpinLock<helios_kmd_logic::native_render::OuterSubmitContext>,
    )> {
        let device = unsafe { self.context.device.as_ref() }?;
        match &self.context.helios {
            HeliosContextRole::Attached {
                session,
                native,
                outer,
                ..
            } => Some((native, *session, device, outer)),
            _ => None,
        }
    }
}

/// Typed borrowed view of a D3D **device** handle.
///
/// [`ContextHandleRef`] existed precisely because "DDI handle types are all C
/// `HANDLE`s, so a direct cast can compile even when the callback actually
/// received an hContext rather than an hAdapter" — and it was used at exactly ONE
/// site. Five other sites did the cast it was written to prevent, and both
/// back-pointer fields were `pub`, so nothing forced the checked traversal. The
/// sharpest was `create_allocation.rs`'s
/// `&*(*(h_device as *const DeviceContext)).adapter`, which dereferenced the
/// back-pointer in one expression with NO null check, unlike its two siblings.
///
/// With the fields private, the only route from a device handle to an
/// `&AdapterContext` outside this module is through here, so an adapter-scoped
/// DDI slot handed a device handle no longer compiles.
///
/// What this does NOT buy: newtyped handles (`struct HDevice(HANDLE)`) would go
/// further, but the DDI signatures are bindgen-fixed at `*mut c_void`, so an
/// `unsafe fn from_raw` is still needed at each entry. The win is that the cast
/// happens once per DDI in one module instead of inline in four subsystems.
pub struct DeviceHandleRef<'a> {
    device: &'a DeviceContext,
}

impl<'a> DeviceHandleRef<'a> {
    /// # Safety
    /// `handle` must be a live hDevice returned by [`dxgkddi_create_device`].
    /// That it really is one is dxgkrnl's contract and is not encodable; the null
    /// check below is the only part we can enforce.
    pub unsafe fn from_raw(handle: HANDLE) -> Option<Self> {
        let device = unsafe { (handle as *const DeviceContext).as_ref() }?;
        Some(Self { device })
    }

    pub fn adapter(&self) -> Option<&'a AdapterContext> {
        unsafe { self.device.adapter.as_ref() }
    }

    /// The device's HTS1 session cell, for `DxgkDdiOpenAllocation`'s role-1
    /// reply-pool binding. Exposed through the checked traversal rather than as
    /// a public field, for the same reason the back-pointers are private.
    pub fn session_cell(
        &self,
    ) -> &'a crate::sync::SpinLock<Option<core::ptr::NonNull<TranslationSessionObject>>> {
        &self.device.session
    }

    /// The raw back-pointer, for the one caller that stores it rather than
    /// borrowing through it (`scheduler.rs`'s `HwContext`).
    pub fn adapter_ptr(&self) -> *mut AdapterContext {
        self.device.adapter
    }

    pub(crate) fn register_outer_open(&self, open: usize) -> bool {
        self.device.register_outer_open(open)
    }

    pub(crate) fn unregister_outer_open(&self, open: usize) {
        self.device.unregister_outer_open(open);
    }
}

/// State for one GPU process object (WDDM 2.0 GPU-VA requirement). We keep no
/// per-process GPU virtual address space (host-owned VA), but dxgkrnl requires a
/// non-NULL driver handle it can round-trip through every per-process DDI and
/// hand back at DestroyProcess, so we allocate a real object to back the handle.
///
/// DELIBERATELY EMPTY. It used to carry an `adapter` back-pointer that was
/// written at CreateProcess and never read, which implied an ownership edge that
/// does not exist. The allocation stays — dxgkrnl needs the non-NULL handle — but
/// the object is an opaque token and nothing more.
pub struct ProcessContext {
    /// K5 reverses the "deliberately empty" note above for exactly one field.
    /// §17.6:4336 requires `DxgkDdiCreateProcess` to allocate one bounded HTS1
    /// session list, and invariant 10 permits exactly one edge into it: create-
    /// context admission. Submit, Present, allocation open and display never
    /// reach it.
    sessions: ProcessSessionList,
}

/// `DxgkDdiCreateDevice` — allocate per-device state.
pub unsafe extern "C" fn dxgkddi_create_device(
    miniport_device_context: *mut c_void,
    create_device: *mut DXGKARG_CREATEDEVICE,
) -> NTSTATUS {
    if miniport_device_context.is_null() || create_device.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    // SAFETY: Dxgkrnl passes our adapter context and a valid args struct.
    let args = unsafe { &mut *create_device };
    let ctx = Box::new(DeviceContext {
        adapter: miniport_device_context as *mut AdapterContext,
        creator_process: args.hKmdProcess as usize,
        session: crate::sync::SpinLock::new(None),
        outer: crate::sync::SpinLock::new(DeviceOuterState {
            session: None,
            session_generation: 0,
            context_kind: 0,
            binding: false,
            failed: false,
            opens: crate::sync::FixedVec::with_max(MAX_DEVICE_OUTER_OPENS),
        }),
    });
    // Hand the device handle back to Dxgkrnl; reclaimed in destroy_device.
    args.hDevice = Box::into_raw(ctx) as *mut c_void;
    STATUS_SUCCESS
}

/// `DxgkDdiDestroyDevice` — close admission, drain exact session/open custody,
/// reclaim this device's transport objects, and free its per-device state.
pub unsafe extern "C" fn dxgkddi_destroy_device(h_device: *mut c_void) -> NTSTATUS {
    if !h_device.is_null() {
        // SAFETY: h_device came from Box::into_raw in create_device; its `adapter`
        // back-pointer is valid for the device's lifetime.
        let Some(adapter) =
            (unsafe { DeviceHandleRef::from_raw(h_device) }).and_then(|d| d.adapter())
        else {
            // The Box still has to be reclaimed even if the back-pointer is
            // somehow null — leaking it would be the worse failure — and so does
            // any session reference it still holds, which the main path below
            // releases and this arm used to walk past.
            let device = unsafe { (h_device as *const DeviceContext).as_ref() };
            let stale = device.and_then(|d| d.session.lock().take());
            let outer = device.and_then(|d| d.outer.lock().session.take());
            if let Some(session) = stale {
                // SAFETY: the device's own reference. `take()` under the cell's
                // spinlock is what makes this single-release: whichever of this
                // and `dxgkddi_destroy_context` runs first gets the pointer and
                // the other sees `None`.
                unsafe { hts1::release_device_session(session, None) };
            }
            if let Some(session) = outer {
                unsafe { hts1::release_execution_session(session) };
            }
            // SAFETY: produced by Box::into_raw in create_device.
            drop(unsafe { Box::from_raw(h_device as *mut DeviceContext) });
            return STATUS_SUCCESS;
        };
        let owner = h_device as usize;
        // SAFETY: `DxgkDdiDestroyDevice` is documented PASSIVE_LEVEL. Session
        // transport teardown and the diagnostic dumps below both require it;
        // mint the proof once for this DDI.
        let passive = unsafe { crate::irql::PassiveLevel::assume() };
        // No allocation open may survive DestroyDevice.  Drop the device-owned
        // HQA1 session reference only after CloseAllocation removed every exact
        // open and joined its host custody.
        let outer_session = {
            let Some(device) = (unsafe { (h_device as *const DeviceContext).as_ref() }) else {
                return STATUS_INVALID_HANDLE;
            };
            let mut outer = device.outer.lock();
            if outer.opens.len() != 0 {
                crate::diag::record_named_bytes(b"OtOpenLeak", outer.opens.len() as u32);
            }
            outer.session.take()
        };
        if let Some(session) = outer_session {
            unsafe { hts1::release_execution_session(session) };
        }
        // Reclaim any virtio blobs / contexts this device allocated but did not
        // release (ICD crash, or a process that skipped RELEASE_BLOB/CTX_DESTROY —
        // e.g. a crash-looping client). Without this the bounded blob table fills
        // across device creations and later ALLOC_BLOBs fail STATUS_INSUFFICIENT_
        // RESOURCES, surfacing as spurious "venus wedge" / render corruption. If
        // the transport is already gone (StopDevice), there is nothing to reclaim.
        // DIAG: 0x0E00_0001 = DestroyDevice entry (unconditional). Low 16 bits of
        // the owning handle so we can correlate with ALLOC_BLOB's owner.
        crate::diag::record(0x0E00_0001);
        crate::diag::record(0x0E01_0000 | ((owner as u32) & 0xFFFF));
        // Mirror the GpuMmu page-table-DDI tracers into the PASSIVE ring so the
        // post-CreateContext Code-43 failure stage is visible over SSH without
        // ntoseye (Step-2 decorative-GpuMmu bring-up).
        crate::ddi::diag_dump_gpummu_atomics(passive);
        // Engine-path tracers (SubmitCommand/Render/Patch/ISR/DPC counts): show
        // whether VidSch exercised the submission engine at all before the
        // post-CreateContext VidSchTerminateAdapter Code-43 (Step-2 coherent fence).
        crate::ddi::diag_dump_engine_atomics();
        // Present-path tracers: the steady-state registry ring is too noisy for
        // per-call breadcrumbs, so mirror the latest cross-adapter present args
        // here at PASSIVE_LEVEL.
        crate::ddi::diag_dump_present_atomics();
        // Native-fence tracers. Every counter must read zero while the surface
        // is `Wddm2_1GpuMmu`: dxgkrnl invokes no WDDM 3.1/3.2 native-fence
        // callback on a 2.1 adapter, so a nonzero one means either the surface
        // flipped or a slot is being reached by a path nobody modelled. That is
        // the value of dumping them BEFORE the flip — the pre-flip run
        // establishes the zero baseline that makes the post-flip numbers mean
        // something.
        crate::ddi::diag_dump_native_fence_atomics(adapter.native_fence.as_ref());
        crate::ddi::diag_dump_translation_session_atomics();
        crate::ddi::diag_dump_native_render_atomics();
        // Sweep exactly this device's slots. A null hDevice would sweep the
        // KMD-owned ones, so the token is minted rather than cast.
        let device_owner = crate::virtio::gpu::DeviceOwner::new(owner);
        // Revoke/drain/destroy the session namespace before the generic owner
        // sweep can release the reply pool or context custody underneath it.
        let stale = { unsafe { (h_device as *const DeviceContext).as_ref() } }
            .and_then(|d| d.session.lock().take());
        if let Some(session) = stale {
            let list = unsafe { (h_device as *const DeviceContext).as_ref() }
                .and_then(|d| d.process_sessions());
            unsafe { crate::ddi::translation_session::release_device_session(session, list) };
        }
        // Unlike the older counter groups, K11 context teardown may be the
        // stale-device fallback immediately above. Publish its final census
        // only after that namespace is terminal so abrupt exit cannot leave
        // K11CtxNew visible without the matching K11CtxDel.
        crate::ddi::session_transport::diag_dump();
        let before = adapter.with_virtio(|v| v.blob_count() as u32).unwrap_or(0);
        let blobs = crate::virtio::ctrl::release_blobs_for_owner(passive, adapter, device_owner);
        let contexts =
            crate::virtio::ctrl::destroy_contexts_for_owner(passive, adapter, device_owner);
        // Opportunistic PASSIVE reap of completed transport entries.
        crate::virtio::ctrl::reap_parked(passive, adapter);
        // 0x0E02_BBBB = blob-table size BEFORE reclaim (saturated to 16 bits).
        crate::diag::record(0x0E02_0000 | before.min(0xFFFF));
        // 0x0E03_RRCC = reclaimed blobs (RR) + contexts (CC).
        crate::diag::record(0x0E03_0000 | ((blobs.min(0xFF) << 8) | contexts.min(0xFF)));
        // SAFETY: produced by Box::into_raw in create_device; destroyed exactly once.
        drop(unsafe { Box::from_raw(h_device as *mut DeviceContext) });
    }
    STATUS_SUCCESS
}

/// `DxgkDdiCreateContext` — GPU execution context.
///
// STUB: Phase 4 — create the Venus virtio-gpu context here.
// NOTE: a TEMP int3 debug breakpoint lived here during the Step-2 GpuMmu bring-up
// and was removed (ntoseye is a gdbstub, not a Windows KD — a bare int3 BSODs the
// guest instead of trapping). Use a spin-loop released via ntoseye write_memory if
// a live pause is needed. This comment also forces a clean recompile.
pub unsafe extern "C" fn dxgkddi_create_context(
    h_device: *mut c_void,
    create_context: *mut DXGKARG_CREATECONTEXT,
) -> NTSTATUS {
    crate::diag::record(0x0800_0001);
    if h_device.is_null() || create_context.is_null() {
        return STATUS_INVALID_PARAMETER;
    }

    let args = unsafe { &mut *create_context };

    // SAFETY: `Flags` is a C union whose `Value` member is its UINT view.
    let context_flags = unsafe { args.Flags.__bindgen_anon_1.Value };
    // SAFETY: h_device is the DeviceContext we returned from DxgkDdiCreateDevice.
    let Some(device) = (unsafe { (h_device as *const DeviceContext).as_ref() }) else {
        return STATUS_INVALID_PARAMETER;
    };
    let process_list = device.process_sessions();
    // SAFETY: dxgkrnl owns a valid create-context private buffer of the stated
    // size for the duration of the call; `classify_context` only reads it.
    let request = match unsafe {
        hts1::classify_context(
            args.pPrivateDriverData,
            args.PrivateDriverDataSize,
            context_flags,
            device.creator_process,
            device.adapter as *const AdapterContext,
            h_device as usize,
            &device.session,
            process_list,
        )
    } {
        Ok(request) => request,
        Err(status) => return status,
    };

    let (role, info) = match request {
        hts1::ContextRequest::Legacy => (HeliosContextRole::Legacy, ContextInfoProfile::Legacy),
        hts1::ContextRequest::HeliosControl => {
            // `classify_context` published the session into `device.session`.
            let Some(session) = *device.session.lock() else {
                return STATUS_INVALID_DEVICE_REQUEST;
            };
            // ⛔ The scratch is allocated HERE, at PASSIVE, and its failure fails
            // the context — never on the Render path, where 228 KiB of nonpaged
            // pool would be a per-batch allocation inside a DDI.
            let Some(adapter) = core::ptr::NonNull::new(device.adapter) else {
                return STATUS_INVALID_DEVICE_REQUEST;
            };
            let Some(native) = NativeContext::new(
                adapter,
                NativeClass::Control,
                helios_protocol::native_render::HELIOS_HVC1_CONTROL_RING_INDEX,
                0,
                0,
                0,
            ) else {
                return STATUS_NO_MEMORY;
            };
            (
                HeliosContextRole::Control { session, native },
                ContextInfoProfile::Hvc1,
            )
        }
        hts1::ContextRequest::HeliosQueue { session, endpoint } => {
            // SAFETY: classify_context returned the endpoint together with the
            // strong session reference that keeps the fixed array live.
            let ring_index = unsafe { endpoint.as_ref().ring_index() };
            let context_generation = unsafe { endpoint.as_ref().endpoint_id() } as u64;
            let Some(session_generation) = hts1::execution_session_generation(session) else {
                unsafe { hts1::release_queue_context(session) };
                return STATUS_INVALID_DEVICE_REQUEST;
            };
            let Some(adapter) = core::ptr::NonNull::new(device.adapter) else {
                unsafe { hts1::release_queue_context(session) };
                return STATUS_INVALID_DEVICE_REQUEST;
            };
            let Some(native) = NativeContext::new(
                adapter,
                NativeClass::Queue,
                ring_index,
                unsafe { endpoint.as_ref().endpoint_id() },
                session_generation,
                context_generation,
            ) else {
                // SAFETY: `classify_context` took a reference for this context
                // and nothing else owns it yet.
                unsafe { hts1::release_queue_context(session) };
                return STATUS_NO_MEMORY;
            };
            (
                HeliosContextRole::Queue { session, native },
                ContextInfoProfile::Hvc1,
            )
        }
        hts1::ContextRequest::HeliosAttach {
            session,
            session_generation,
            context_generation,
            endpoint,
            kind,
        } => {
            // SAFETY: `classify_context` returned the endpoint together with a
            // strong session reference, so the fixed endpoint array is live.
            let endpoint_id = unsafe { endpoint.as_ref().endpoint_id() };
            let ring_index = unsafe { endpoint.as_ref().ring_index() };
            let Some(adapter) = core::ptr::NonNull::new(device.adapter) else {
                unsafe { hts1::release_attached_context(session, context_generation) };
                return STATUS_INVALID_DEVICE_REQUEST;
            };
            if !device.bind_outer_session(
                unsafe { PassiveLevel::assume() },
                session,
                session_generation,
                kind.wire(),
            ) {
                unsafe { hts1::release_attached_context(session, context_generation) };
                return STATUS_INVALID_DEVICE_REQUEST;
            }
            let Some(native) = NativeContext::new(
                adapter,
                NativeClass::Outer,
                ring_index,
                endpoint_id,
                session_generation,
                context_generation,
            ) else {
                unsafe { hts1::release_attached_context(session, context_generation) };
                return STATUS_NO_MEMORY;
            };
            (
                HeliosContextRole::Attached {
                    session,
                    context_generation,
                    endpoint,
                    native,
                    outer: crate::sync::SpinLock::new(
                        helios_kmd_logic::native_render::OuterSubmitContext::new(
                            helios_protocol::HELIOS_PACKAGE_GENERATION,
                            session_generation,
                            context_generation,
                            endpoint_id,
                            kind.wire(),
                        ),
                    ),
                },
                // An HQA1 outer context is an ordinary D3D runtime context in every
                // respect but one: the D3D12 virtual arm's `pDmaBufferPrivateData`
                // must hold a 64-byte HOS1 prefix (§10.4:1332-1335).
                ContextInfoProfile::Hqa1Outer,
            )
        }
    };

    let ctx = Box::new(ContextContext {
        magic: CONTEXT_CTX_MAGIC,
        device: h_device as *mut DeviceContext,
        helios: role,
    });
    args.hContext = Box::into_raw(ctx) as HANDLE;
    write_context_info(&mut args.ContextInfo, info);
    STATUS_SUCCESS
}

/// Which `DXGK_CONTEXTINFO` a context gets.
///
/// ⛔ The two profiles differ in `DmaBufferSegmentSet`, and the difference is not
/// cosmetic: a zero segment set makes dxgmms2 skip the VIDMM allocation object
/// and then null-deref it in `VidMmInitDmaPool` for a runtime context. §10.7:1981
/// keeps segment 1 as "the only nonzero `DmaBufferSegmentSet` choice for existing
/// D3D runtime contexts, while HVC1 selects zero", so this is a per-arm branch
/// and never a global flip.
enum ContextInfoProfile {
    Legacy,
    Hvc1,
    /// An HQA1-attached outer context: Legacy in every field, declared
    /// separately so the 64-byte HOS1 requirement is stated at the site that
    /// satisfies it instead of being inherited by luck.
    Hqa1Outer,
}

/// `DXGKARG_SUBMITCOMMANDVIRTUAL` reads a `HeliosOuterSubmitV1` out of the front
/// of an outer context's DMA private data, so the advertised size must hold one.
/// It does — 88 >= 64 — and this assert is what keeps that true if either
/// number moves.
const _: () = assert!(
    crate::ddi::present_packet::PRESENT_DMA_PRIVATE_DATA_BYTES
        >= helios_protocol::wddm::HELIOS_HOS1_BYTES as u32
);
const _: () = assert!(helios_protocol::wddm::HELIOS_HOB1_MAX_BYTES <= u32::MAX as u64);

fn write_context_info(info: &mut DXGK_CONTEXTINFO, profile: ContextInfoProfile) {
    match profile {
        ContextInfoProfile::Legacy => {
            // Use the paging aperture for DMA buffers. With the decorative GpuMmu
            // model, dxgkrnl's CDD context creates a privileged DMA pool with
            // GPU-VA mapping enabled; if this is 0, dxgmms2 uses contiguous system
            // memory, skips creating a VIDMM allocation object, then later
            // dereferences that null allocation in VidMmInitDmaPool.
            info.DmaBufferSegmentSet = 1; // segment id 1 (aperture)
            info.DmaBufferSize = 256 * 1024;
            // ONE definition site: present_packet.rs, beside the two records that
            // live in the buffer and the compile-time proof they fit it.
            info.DmaBufferPrivateDataSize =
                crate::ddi::present_packet::PRESENT_DMA_PRIVATE_DATA_BYTES;
            info.AllocationListSize = DXGK_ALLOCATION_LIST_SIZE_GDICONTEXT;
            info.PatchLocationListSize = DXGK_ALLOCATION_LIST_SIZE_GDICONTEXT;
        }
        ContextInfoProfile::Hqa1Outer => {
            // HQA1 is still an ordinary D3D runtime context, so it must retain
            // the aperture-backed DMA allocation. A zero segment set null-derefs
            // dxgmms2 in VidMmInitDmaPool for this context class.
            info.DmaBufferSegmentSet = 1;

            // Unlike a legacy context, the first legal outer batch may be any
            // bounded HOB1. Advertise the complete package maximum up front so
            // the UMD never needs a smaller sacrificial batch before it can ask
            // the runtime to resize these three coupled windows.
            info.DmaBufferSize = helios_protocol::wddm::HELIOS_HOB1_MAX_BYTES as u32;
            info.DmaBufferPrivateDataSize =
                crate::ddi::present_packet::PRESENT_DMA_PRIVATE_DATA_BYTES;
            info.AllocationListSize =
                helios_protocol::native_render::HELIOS_HVC1_ALLOCATION_LIST_ENTRIES;
            info.PatchLocationListSize =
                helios_protocol::native_render::HELIOS_HVC1_PATCH_LOCATION_ENTRIES;
        }
        ContextInfoProfile::Hvc1 => {
            // §10.7:1734-1738, verbatim: a 256-KiB DMA buffer, a 4096-entry
            // allocation list, a 4096-entry patch-location capacity, 64 bytes of
            // KMD-only DMA private data, `DmaBufferSegmentSet=0`,
            // `Caps.NoPatchingRequired=0`, every other cap/reserved field zero.
            info.DmaBufferSize = helios_protocol::native_render::HELIOS_HVC1_DMA_BUFFER_BYTES;
            info.DmaBufferSegmentSet =
                helios_protocol::native_render::HELIOS_HVC1_DMA_BUFFER_SEGMENT_SET;
            info.DmaBufferPrivateDataSize =
                helios_protocol::native_render::HELIOS_HVC1_DMA_PRIVATE_DATA_BYTES;
            info.AllocationListSize =
                helios_protocol::native_render::HELIOS_HVC1_ALLOCATION_LIST_ENTRIES;
            info.PatchLocationListSize =
                helios_protocol::native_render::HELIOS_HVC1_PATCH_LOCATION_ENTRIES;
            info.Caps.__bindgen_anon_1.Value = 0;
            info.PagingCompanionNodeId = 0;
            info.Reserved = 0;
        }
    }
}

/// `DxgkDdiDestroyContext`.
// STUB: Phase 4 — tear down the Venus context.
pub unsafe extern "C" fn dxgkddi_destroy_context(h_context: *mut c_void) -> NTSTATUS {
    crate::diag::record(0x0800_0002);
    if !h_context.is_null() {
        // SAFETY: produced by Box::into_raw in create_context; destroyed once.
        let mut ctx = unsafe { Box::from_raw(h_context as *mut ContextContext) };
        // Poison the tag BEFORE the box dies: a stale handle arriving on the
        // Patch/SubmitCommand union then reads as a refusal rather than as a
        // live context, for whatever the allocator has since put there.
        ctx.magic = 0;
        let passive = unsafe { crate::irql::PassiveLevel::assume() };
        match ctx.helios {
            HeliosContextRole::Legacy => {}
            HeliosContextRole::Queue { session, native } => {
                // A queue context holds only its own reference: the session's
                // registration and the device's reference belong to the control
                // context, which `DxgkDdiDestroyDevice` settles.
                native.close(passive);
                drop(native);
                // SAFETY: the reference `classify_context` took for it.
                unsafe { hts1::release_queue_context(session) };
            }
            HeliosContextRole::Control { session, native } => {
                native.close(passive);
                drop(native);
                // ⛔ CLEAR THE CELL FIRST. `release_device_session` may drop the
                // last reference and free the object; a `DxgkDdiOpenAllocation`
                // on this device between the free and the clear would read a
                // dangling pointer out of `device.session`.
                let device = unsafe { ctx.device.as_ref() };
                if let Some(device) = device {
                    *device.session.lock() = None;
                }
                let list = device.and_then(|d| d.process_sessions());
                // SAFETY: the device's own reference, taken at control-context
                // creation and released exactly once here. The list entry is
                // removed under the list lock inside.
                unsafe { hts1::release_device_session(session, list) };
            }
            HeliosContextRole::Attached {
                session,
                context_generation,
                native,
                ..
            } => {
                native.close(passive);
                drop(native);
                // SAFETY: the reference this context took at HQA1 attach.
                unsafe { hts1::release_attached_context(session, context_generation) };
            }
        }
    }
    STATUS_SUCCESS
}

/// `DxgkDdiCreateProcess` — GPU-VA process object (WDDM 2.0 requirement).
///
/// dxgkrnl creates a process object during GPU-VA adapter bring-up and expects a
/// non-NULL driver handle back in `hKmdProcess`; leaving this a
/// `STATUS_NOT_IMPLEMENTED` stub fails post-StartDevice (one of the Code-43
/// triggers). We allocate a `ProcessContext`, hand its pointer back as the
/// handle, and reclaim it in DestroyProcess. No GPU virtual address space is
/// tracked (host-owned VA — see build_paging_buffer.rs).
pub unsafe extern "C" fn dxgkddi_create_process(
    miniport_device_context: *mut c_void,
    args: *mut DXGKARG_CREATEPROCESS,
) -> NTSTATUS {
    // DIAG: confirm dxgkrnl reaches CreateProcess during AddAdapter.
    crate::diag::record(0x0600_0000);
    if miniport_device_context.is_null() || args.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    // SAFETY: Dxgkrnl passes our adapter context and a valid args struct.
    let args = unsafe { &mut *args };
    // The handle is an opaque token: dxgkrnl only round-trips it. The old
    // `adapter` back-pointer here was written and never read.
    let ctx = Box::new(ProcessContext {
        sessions: ProcessSessionList::new(),
    });
    // Hand the process handle back to Dxgkrnl; reclaimed in destroy_process.
    args.hKmdProcess = Box::into_raw(ctx) as HANDLE;
    STATUS_SUCCESS
}

/// `DxgkDdiDestroyProcess` — free the per-process state from CreateProcess.
pub unsafe extern "C" fn dxgkddi_destroy_process(
    _miniport_device_context: *mut c_void,
    h_process: *mut c_void,
) -> NTSTATUS {
    if !h_process.is_null() {
        // SAFETY: h_process was produced by Box::into_raw in create_process and
        // is destroyed exactly once.
        let process = unsafe { Box::from_raw(h_process as *mut ProcessContext) };
        // §14: stop admission and invalidate every capability before anything
        // else. dxgkrnl destroys this process's devices — and therefore their
        // control contexts, each of which removes its own entry — before the
        // process, so the list is empty here on every ordinary path. A nonempty
        // one means a session outlived its owning device, which is an accounting
        // bug elsewhere: draining stops a later attach from reaching it, and the
        // `SessionObject` then LEAKS rather than dangling, because the only
        // pointers to it die with this box.
        process.sessions.drain_all();
        drop(process);
    }
    STATUS_SUCCESS
}
