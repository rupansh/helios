//! Per-D3D-device, per-context, and per-process state, plus their DDIs.
//!
//! Phase 1 implements device alloc/free (so the runtime can open a device
//! without crashing). Context and GPU-VA process DDIs are stubbed until the
//! Venus path (Phase 4) and the memory model (Phase 3) land.
//!
//! NOTE: the exact argument struct/handle types below come from the generated
//! `dxgk` bindings and may need a binding-alignment pass at first compile.

use alloc::boxed::Box;
use core::ffi::c_void;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use helios_kmd_logic::snapshot_bind::SnapshotDescriptor;

use crate::adapter::AdapterContext;
use crate::ddi::native_render::{NativeClass, NativeContext};
use crate::ddi::translation_session::{
    self as hts1, ProcessSessionList, SessionEndpointObject,
    SessionObject as TranslationSessionObject,
};
use crate::dxgk::*;

/// State for one D3D device opened on the adapter.
pub struct DeviceContext {
    /// Back-pointer to the owning adapter (valid for the device's lifetime).
    ///
    /// PRIVATE. The only route from a device handle to an `&AdapterContext` is
    /// [`DeviceHandleRef`]'s checked traversal — see its docs for the cast this
    /// prevents.
    adapter: *mut AdapterContext,
    /// Documented WDDM KMD-process token (`DXGKARG_CREATEDEVICE.hKmdProcess`).
    /// Present markers arrive through the UMD runtime's D3DKMT device rather
    /// than the ICD device that registered a stream, so this exact per-process
    /// object association — never a PID or current-thread inference — is the
    /// authorization edge between the two devices.  Zero means that async
    /// stream registration/marker use is refused while ordinary rendering stays
    /// available.
    creator_process: usize,
    /// K5: the one HTS1 session this raw KMT device's HVC1 control context
    /// created, if it is a raw device at all. `None` for every ordinary D3D
    /// device, which is most of them. §10.7:1720-1721 permits exactly one.
    session: crate::sync::SpinLock<Option<core::ptr::NonNull<TranslationSessionObject>>>,
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
    /// D4b snapshot descriptor STASH: the render→present handoff
    /// (FIX-DESIGN-d4b-snapshot.md §4, corrected delivery route).
    ///
    /// WHY IT EXISTS. dxgkrnl does NOT forward the UMD's PresentCb
    /// `pPrivateDriverData` to `DxgkDdiPresent` on flip presents (measured:
    /// `PBIdOk` reads "no payload" across three driver generations), so the
    /// descriptor's only working route into the KMD is the inline
    /// `HeliosPresentRenderCmd` that `DxgkDdiRender` already decodes — the
    /// same route the present-marker resid has always used. The UMD issues
    /// pfnRenderCb then pfnPresentCb back-to-back on one thread for the same
    /// hContext, so Render stashes HERE and the immediately following Present
    /// takes it.
    ///
    /// DISCIPLINE. `snap_resid == 0` = empty. The writer stores the payload
    /// fields Relaxed and the resid LAST with Release; the taker swaps the
    /// resid to 0 with Acquire and only then reads the payload — read+clear on
    /// EVERY present that resolves its context (flip, MMIO, BLT alike), so an
    /// orphaned stash (a Render whose Present failed) cannot outlive the next
    /// present on this context: staleness is bounded to one present, and the
    /// Present-time validation (extent vs the allocation list, layout,
    /// liveness at the executor) re-checks everything the stash claims.
    /// Atomics rather than plain fields because context state is reached
    /// through a shared reference; the same-thread Render→Present pairing is
    /// the expected regime, and under any cross-thread interleaving a torn
    /// stash degrades to a mix of two VALIDATED descriptors, which that same
    /// validation absorbs. The stash is plain data inside this Box — it dies
    /// with the context in `dxgkddi_destroy_context`, and dxgkrnl serializes
    /// context destruction against in-flight Render/Present on the handle, so
    /// nothing can dangle.
    snap_resid: AtomicU32,
    snap_width: AtomicU32,
    snap_height: AtomicU32,
    snap_pitch: AtomicU32,
    snap_dxgi_format: AtomicU32,
    /// Kept full-width: narrowing to u32 happens only AFTER Present-time
    /// validation proved `plane_offset <= u32::MAX` (a pre-narrow truncation
    /// could alias an invalid offset onto a valid-looking one).
    snap_plane_offset: AtomicU64,
    snap_alloc_size: AtomicU64,
    snap_memory_type: AtomicU32,
    snap_purpose: AtomicU32,
    /// Registered-stream marker STASH. Exactly like the snapshot handoff, the
    /// Render→Present pair is the only documented route from UMD command bytes
    /// into the KMD-private DMA record consumed by SubmitCommand. The three
    /// fields are one lock-protected value: publishing/claiming them as separate
    /// atomics lets a later Render overwrite ctx/value after Present claims the
    /// prior cookie, cross-pairing two frames. The fixed Option neither allocates
    /// nor blocks below DISPATCH; `SpinLock` raises/restores IRQL around the
    /// handful of scalar accesses.
    present_stream_marker: crate::sync::SpinLock<Option<(u32, u32, u64)>>,
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

    /// The exact documented KMD-process object token that created this
    /// context's device.
    pub fn creator_process(&self) -> Option<usize> {
        let device = unsafe { self.context.device.as_ref() }?;
        Some(device.creator_process)
    }

    /// Stash a VALIDATED-shape D4b snapshot descriptor from `DxgkDdiRender`'s
    /// `HeliosPresentRenderCmd` for the present that follows on this context.
    /// See [`ContextContext::snap_resid`] for the whole contract; the payload
    /// stores precede the resid's Release publication so the taker's Acquire
    /// swap observes a complete descriptor.
    pub fn stash_snapshot(&self, snap: &SnapshotDescriptor) {
        let ctx = self.context;
        ctx.snap_width.store(snap.width, Ordering::Relaxed);
        ctx.snap_height.store(snap.height, Ordering::Relaxed);
        ctx.snap_pitch.store(snap.pitch, Ordering::Relaxed);
        ctx.snap_dxgi_format
            .store(snap.dxgi_format, Ordering::Relaxed);
        ctx.snap_plane_offset
            .store(snap.plane_offset, Ordering::Relaxed);
        ctx.snap_alloc_size
            .store(snap.venus_alloc_size, Ordering::Relaxed);
        ctx.snap_memory_type
            .store(snap.memory_type_index, Ordering::Relaxed);
        ctx.snap_purpose.store(snap.purpose, Ordering::Relaxed);
        ctx.snap_resid.store(snap.resource_id, Ordering::Release);
    }

    /// Take (read + CLEAR) the stashed snapshot descriptor, or `None`.
    ///
    /// Called on EVERY present that resolves this context — including the
    /// MMIO/desktop and BLT arms, which never substitute — because the clear
    /// is the orphan bound: a stash whose present failed must not leak past
    /// the next present. The swap claims the descriptor exactly once.
    pub fn take_snapshot_stash(&self) -> Option<SnapshotDescriptor> {
        let ctx = self.context;
        let resid = ctx.snap_resid.swap(0, Ordering::Acquire);
        if resid == 0 {
            return None;
        }
        Some(SnapshotDescriptor {
            resource_id: resid,
            width: ctx.snap_width.load(Ordering::Relaxed),
            height: ctx.snap_height.load(Ordering::Relaxed),
            pitch: ctx.snap_pitch.load(Ordering::Relaxed),
            dxgi_format: ctx.snap_dxgi_format.load(Ordering::Relaxed),
            plane_offset: ctx.snap_plane_offset.load(Ordering::Relaxed),
            venus_alloc_size: ctx.snap_alloc_size.load(Ordering::Relaxed),
            memory_type_index: ctx.snap_memory_type.load(Ordering::Relaxed),
            purpose: ctx.snap_purpose.load(Ordering::Relaxed),
        })
    }

    /// Stash one complete nonzero stream marker for the immediately following
    /// Present on this context.  The KMD validates registry/process ownership
    /// while consuming it; this handoff only preserves the exact UMD boundary
    /// until the WDDM private-data buffer exists.
    pub fn stash_present_stream_marker(&self, ctx_id: u32, value: u32, cookie: u64) {
        if ctx_id == 0 || value == 0 || cookie == 0 {
            return;
        }
        *self.context.present_stream_marker.lock() = Some((ctx_id, value, cookie));
    }

    /// Take and clear the stream marker stash, bounding an orphaned Render to
    /// one following Present just like the snapshot descriptor.
    pub fn take_present_stream_marker_stash(&self) -> Option<(u32, u32, u64)> {
        self.context.present_stream_marker.lock().take()
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

    /// The HOS1 gate of an HQA1-attached outer context.
    pub(crate) fn outer_submit(
        &self,
    ) -> Option<&'a crate::sync::SpinLock<helios_kmd_logic::native_render::OuterSubmitContext>>
    {
        match &self.context.helios {
            HeliosContextRole::Attached { outer, .. } => Some(outer),
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

    /// The exact documented KMD-process object token that created this device.
    pub fn creator_process(&self) -> usize {
        self.device.creator_process
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
    });
    // Hand the device handle back to Dxgkrnl; reclaimed in destroy_device.
    args.hDevice = Box::into_raw(ctx) as *mut c_void;
    STATUS_SUCCESS
}

/// `DxgkDdiDestroyDevice` — free per-device state, after unmapping any host-visible
/// blob views this device opened (Gate 5a Stage 2b). The user VAs were mapped by
/// `HELIOS_ESCAPE_MAP_BLOB` (tagged with this `h_device` as owner) and MUST be
/// unmapped here — in the creating process, at PASSIVE_LEVEL — or the kernel
/// bugchecks `0x76 PROCESS_HAS_LOCKED_PAGES` at process exit. This DDI runs in the
/// context of the thread destroying the device (the ICD's process), so the unmap is
/// in-process. The mapping table is on the AdapterContext (independent spinlock), so
/// teardown is correct even if the virtio transport is already gone.
/// Mappings harvested per spinlock acquisition in DestroyDevice.
///
/// 64 pairs = 1 KiB of stack, which is affordable on the PASSIVE DestroyDevice
/// frame and turns an 8192-mapping teardown from 8192 acquisitions into 128.
const MAPPING_DRAIN_BATCH: usize = 64;

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
            let stale = (unsafe { (h_device as *const DeviceContext).as_ref() })
                .and_then(|d| d.session.lock().take());
            if let Some(session) = stale {
                // SAFETY: the device's own reference. `take()` under the cell's
                // spinlock is what makes this single-release: whichever of this
                // and `dxgkddi_destroy_context` runs first gets the pointer and
                // the other sees `None`.
                unsafe { hts1::release_device_session(session, None) };
            }
            // SAFETY: produced by Box::into_raw in create_device.
            drop(unsafe { Box::from_raw(h_device as *mut DeviceContext) });
            return STATUS_SUCCESS;
        };
        let owner = h_device as usize;
        // SAFETY: `DxgkDdiDestroyDevice` is documented "IRQL: PASSIVE_LEVEL" (WDK
        // DXGKDDI_DESTROYDEVICE), and the unmap loop below already depends on it
        // — `MmUnmapLockedPages` is PASSIVE-only, which is exactly why the table
        // lock is dropped between batches. Minted HERE rather than further down
        // because the diag dumps between also require it; still ONE mint for
        // this DDI.
        let passive = unsafe { crate::irql::PassiveLevel::assume() };
        // Drain THIS device's mappings in batches, unmapping outside the table
        // lock (MmUnmapLockedPages needs PASSIVE; the table lock raises to
        // DISPATCH). One acquisition per entry was O(n) acquisitions and O(n^2)
        // comparisons, and MAX_MAPPINGS is 8192 because a DOOM level load really
        // does hold thousands.
        let mut batch = [(0u64, 0usize); MAPPING_DRAIN_BATCH];
        loop {
            let n = adapter.mappings.drain_for(owner, &mut batch);
            if n == 0 {
                break;
            }
            for &(user_va, mdl) in &batch[..n] {
                // SAFETY: PASSIVE_LEVEL in the creating process; pair from a
                // prior MAP_BLOB on this device handle.
                unsafe { crate::ddi::unmap_io_pages_from_user(user_va, mdl as *mut wdk_sys::MDL) };
            }
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
        // D4a: drop this device's scanout retirement-event registrations —
        // dereference ONLY, no signal (the process is exiting; a wake would
        // land nowhere). Its read-ledger page mapping needs nothing here: it
        // rides the MappingTable and was unmapped by the drain above.
        adapter.read_ledger.reclaim_events_for_owner(owner);
        // Sweep exactly this device's slots. A null hDevice would sweep the
        // KMD-owned ones, so the token is minted rather than cast.
        let device_owner = crate::virtio::gpu::DeviceOwner::new(owner);
        // Present-stream slots borrow this DeviceContext's KMD-process token,
        // so purge them before the device handle can disappear.
        let purged_streams = adapter.with_wddm_notify_lock(|guard| {
            guard
                .with_virtio(|order, v| v.purge_present_streams_for_owner(order, device_owner))
                .unwrap_or(0)
        });
        if purged_streams != 0 {
            crate::ddi::interrupt::request_wddm_completion_dpc(adapter);
        }
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
            ) else {
                return STATUS_NO_MEMORY;
            };
            (
                HeliosContextRole::Control {
                    session,
                    native,
                },
                ContextInfoProfile::Hvc1,
            )
        }
        hts1::ContextRequest::HeliosQueue { session, endpoint } => {
            // SAFETY: classify_context returned the endpoint together with the
            // strong session reference that keeps the fixed array live.
            let ring_index = unsafe { endpoint.as_ref().ring_index() };
            let context_generation = unsafe { endpoint.as_ref().endpoint_id() } as u64;
            let Some(session_generation) =
                hts1::execution_session_generation(session)
            else {
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
                session_generation,
                context_generation,
            ) else {
                // SAFETY: `classify_context` took a reference for this context
                // and nothing else owns it yet.
                unsafe { hts1::release_queue_context(session) };
                return STATUS_NO_MEMORY;
            };
            (
                HeliosContextRole::Queue {
                    session,
                    native,
                },
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
            (
                HeliosContextRole::Attached {
                    session,
                    context_generation,
                    endpoint,
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
        snap_resid: AtomicU32::new(0),
        snap_width: AtomicU32::new(0),
        snap_height: AtomicU32::new(0),
        snap_pitch: AtomicU32::new(0),
        snap_dxgi_format: AtomicU32::new(0),
        snap_plane_offset: AtomicU64::new(0),
        snap_alloc_size: AtomicU64::new(0),
        snap_memory_type: AtomicU32::new(0),
        snap_purpose: AtomicU32::new(0),
        present_stream_marker: crate::sync::SpinLock::new(None),
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

fn write_context_info(info: &mut DXGK_CONTEXTINFO, profile: ContextInfoProfile) {
    match profile {
        // ⛔ IDENTICAL TO `Legacy`, INCLUDING `DmaBufferSegmentSet = 1`. A zero
        // segment set null-derefs dxgmms2 in `VidMmInitDmaPool` for a runtime
        // context (measured, see the profile enum), and an HQA1 outer context IS
        // a runtime context — only HVC1 selects zero.
        ContextInfoProfile::Hqa1Outer | ContextInfoProfile::Legacy => {
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
                ..
            } => {
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
