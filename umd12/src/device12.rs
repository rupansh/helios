//! `HeliosD3D12Device` — the driver's per-device private block, and the
//! `pfnCalcPrivateDeviceSize` / `pfnCreateDevice` / `pfnDestroyDevice` bodies
//! behind it.
//!
//! # Where this block lives, and why the size site and the write site are one
//! function
//!
//! WDDM's object model: the driver states a size, the **runtime** allocates that
//! many bytes, and the driver constructs its object *in place* inside them.
//! `hDrvDevice.pDrvPrivate` is that pointer. Identical to the D3D11 shape
//! (`umd/src/device_funcs.rs`, R2 §1.1).
//!
//! ⚠ `D3D12DDI_CREATE_DEVICE_FLAG_DEBUGGABLE` arrives on **both**
//! `D3D12DDIARG_CALCPRIVATEDEVICESIZE` and `D3D12DDIARG_CREATEDEVICE_0109`
//! (`DDI_REFERENCE.md` §1.4), so the private size may legitimately differ
//! between debug and retail — and the two sites must compute it from the *same*
//! function of `Flags`, never `size_of::<Device>()` at one and a constant at the
//! other. [`device_private_size`] is that one function, and both call it.
//!
//! # ⛔ `DECISIONS.md` D13 and what does NOT belong here
//!
//! This block is **per-object, per-process, and crosses no module boundary** —
//! nothing outside this DLL ever reads it — so it lives here, typed against
//! `ddi12`, which is what D13's refined form says. What must never be
//! re-declared here is the private data that *does* cross: since the HPS2
//! retirement that is the single create-time allocation descriptor
//! `HeliosWddmAllocationDescV2` (`'HWA2'`, 168 bytes), which L4 builds, sends
//! through `pfnAllocateCb` and validates the kernel's write-back of. It is
//! `helios_protocol`'s, read by `kmd_render` and by the D3D11 driver, and L4
//! reuses it verbatim.
//!
//! ⚠ The three records this block used to name — `HeliosWddmAllocPrivate`, the
//! KMD-stamped `HeliosWddmOpenIdentity` and the `HeliosPresentRenderCmd` present
//! identity channel — are **retired**, and the reversal is recorded rather than
//! quietly edited. The first two are replaced by HWA2 in one direction only (the
//! KMD writes at create, every opener treats it as `const`); the third has no
//! successor in this driver yet, because its `resource_id` is a host resource id
//! §10.3 forbids any UMD supplying — see `PresentIdentityNoResourceId` and mesa
//! lane unit A3.
//!
//! # The unwind guard
//!
//! `create_device` builds three things that must be released if a later step
//! fails: the engine device, the in-place `HeliosD3D12Device`, and (from L2) the
//! per-queue WDDM contexts. `umd/src/adapter.rs`'s `DeviceUnderConstruction`
//! exists because two checks used to run *after* construction and each leaked —
//! *"the `hDrvDevice` one leaked the whole DXVK/Vulkan device, and the
//! `pDeviceFuncs` one … leaked a kernel context and a paging queue per attempt.
//! A crash-looping client exhausted them."* [`DeviceUnderConstruction`] is the
//! same guard for D3D12, and the ordering rule it enforces is the cheaper half:
//! **validate every runtime-supplied pointer BEFORE constructing anything.**

use helios_umd_common::hr::{
    Hresult, D3DDDIERR_APPLICATIONERROR, D3DDDIERR_DEVICEREMOVED, DXGI_ERROR_DEVICE_REMOVED,
    DXGI_ERROR_UNSUPPORTED, E_FAIL, E_INVALIDARG, E_OUTOFMEMORY, S_OK,
};

use crate::adapter12::{self, Ddi12Interface};
use crate::bridge12::BridgeDevice12;
use crate::{ddi12, forward12, log_error, note_refusal, UMD12_REFUSALS};
use core::ffi::c_void;
use helios_protocol::{
    HeliosSyncProgressJoinV1, HeliosSyncProgressResultV1, HeliosTranslatorStatusCode,
};
use helios_umd_common::direct_translator::DirectTranslator;

pub(crate) extern "C" fn translator_sync_progress_join(
    host_context_cookie: *mut c_void,
    request: *const HeliosSyncProgressJoinV1,
    out_result: *mut HeliosSyncProgressResultV1,
) -> HeliosTranslatorStatusCode {
    forward12::queue::translator_sync_progress_join(host_context_cookie, request, out_result)
}

pub(crate) extern "C" fn translator_sync_progress_query(
    host_context_cookie: *mut c_void,
    context_generation: u64,
    out_result: *mut HeliosSyncProgressResultV1,
) -> HeliosTranslatorStatusCode {
    forward12::queue::translator_sync_progress_query(
        host_context_cookie,
        context_generation,
        out_result,
    )
}

/// The Helios D3D12 device: what this driver keeps for the lifetime of one
/// `ID3D12Device`.
///
/// ⚠ **New fields are appended at the END** (`PARALLEL.md` §5). Eleven lanes add
/// to this struct; reordering or repurposing an existing field is how two lanes'
/// state silently becomes one.
pub(crate) struct HeliosD3D12Device {
    /// The runtime's handle for this device. Every corelayer callback that
    /// reports an error takes it — `pfnSetErrorCb(hRTDevice, hr)` is how a
    /// `VOID`-returning DDI says it failed, and most D3D12 DDIs return `VOID`
    /// (`DECISIONS.md` §7.6).
    pub(crate) h_rt_device: ddi12::D3D12DDI_HRTDEVICE,

    /// The negotiated interface.
    ///
    /// ⚠ **The arm `Ddi12Interface::tables_implemented()` admits, and nothing
    /// else** — today that is `R8_0110` on every device that exists, but it is
    /// enforced rather than assumed: `Umd12CoreDdi` can make the *advertised*
    /// token `_0116`, and [`create_device`]'s table-shape gate refuses any arm
    /// whose shapes `forward12::tables12` does not fill, before this struct is
    /// written. A lane that needs to know which revision it is serving reads it
    /// from here rather than assuming, and the day the 0116 tables land this is
    /// the field that stops being a constant.
    pub(crate) negotiated: Ddi12Interface,

    /// `D3D12DDI_CREATE_DEVICE_FLAGS` as the runtime gave them, including
    /// `DEBUGGABLE`. Kept because [`device_private_size`] is a function of them
    /// and a later reader must be able to check that this block was sized by the
    /// same input it was built with.
    pub(crate) flags: ddi12::D3D12DDI_CREATE_DEVICE_FLAGS,

    /// Exact adapter LUID captured from the package-owned OpenAdapter record.
    /// Native-fence HNF1 create/open validation compares this value; zero is
    /// never used as a wildcard.
    pub(crate) adapter_luid: i64,

    /// The **usermode** D3D12 corelayer callbacks, at the `_0062` revision.
    ///
    /// ⛔ This pointer is the `ARCHITECTURE.md` §12 trap 2 landmine in its
    /// D3D12 form. `p12UMCallbacks` is a **union of four arms** — `_0003`,
    /// `_0022`, `_0050`, `_0062` — of 12 / 14 / 17 / 18 pointer-wide members
    /// (`DDI_REFERENCE.md` §1.4, §6.2), and reading the wrong arm reads past the
    /// end of a shorter one. Which arm is live is decided by the negotiated
    /// version, and D12's one-token set fixes it at `_0062`: there is no other
    /// revision this driver can be asked for. That is why the field has a
    /// concrete type instead of a `*const c_void` a call site reinterprets.
    pub(crate) um_callbacks: *const ddi12::D3D12DDI_CORELAYER_DEVICECALLBACKS_0062,

    /// The **kernel** callbacks — the same 65-entry `D3DDDI_DEVICECALLBACKS`
    /// table the D3D11 UMD drives (`DECISIONS.md` §4.1, R2 §1.2), carrying
    /// `pfnRenderCb`, `pfnPresentCb`, `pfnAllocateCb`, `pfnEscapeCb`.
    ///
    /// ⭐ It is the reason a D3D12 present can share the D3D11 identity channel
    /// at all: `DDI_REFERENCE.md` §8.2 records that there is **no DMA buffer in
    /// the D3D12 DDI and no `pfnRenderCb` in `d3d12umddi.h`** — it arrives here,
    /// through `pKTCallbacks`, which is `d3dumddi.h`'s table.
    pub(crate) kt_callbacks: *const ddi12::D3DDDI_DEVICECALLBACKS,

    /// The vkd3d engine device. Owns the `ID3D12Device` and, through it, the
    /// whole Vulkan device, its memory and its queues.
    ///
    /// ⚠ Dropping this drops all of that, which is why `destroy_device` is
    /// `drop_in_place` and not a bare "null the handle".
    pub(crate) engine: BridgeDevice12,

    // ── Appended for the Core-0116 arm (`PARALLEL.md` §5: new fields go at the
    // END). ──────────────────────────────────────────────────────────────────
    /// The corelayer callbacks at the **`_0116`** revision, or NULL.
    ///
    /// ⭐ **The whole reason the retirement wants Core 0116.** `_0116` is
    /// `_0062` plus exactly two appended slots — `pfnCreateNativeFenceCb` at
    /// offset 144 and `pfnOpenNativeFenceCb` at 152 — and those two are the only
    /// documented route from a D3D12 `HFENCE` to a kernel native fence
    /// (`HELIOS_PRESENT_SYNC_RETIREMENT.md` §12.1 steps 1-2). Nothing else in
    /// the D3D12 DDI can create one.
    ///
    /// ⛔ **NULL on the `_0110` arm, and that is a fact about the runtime, not a
    /// convenience.** The union arm the runtime filled is chosen by the
    /// negotiated version; reading `p12UMCallbacks_0116` out of a `_0062`
    /// negotiation would be reading 16 bytes past the table the runtime
    /// allocated. [`create_device`] selects the arm with an exhaustive match and
    /// stores NULL here for every arm that is not `_0116`.
    ///
    /// ⚠ [`Self::um_callbacks`] stays the `_0062` view on **both** arms and is
    /// what `set_error` / `set_command_list_error` read. That is a prefix read,
    /// not a reinterpretation: the compile-time block below asserts all 18
    /// shared fields are at identical offsets in the two shapes.
    pub(crate) um_callbacks_0116: *const ddi12::D3D12DDI_CORELAYER_DEVICECALLBACKS_0116,

    /// Owns the sole A5 VkInstance and drops after the vkd3d wrapper.
    pub(crate) translator: DirectTranslator,

    /// Bounded token/resource/WDDM ownership for this exact A5 device
    /// generation. Never process-global; distinct devices in one process have
    /// disjoint registries and reject each other's tokens.
    pub(crate) outer_allocations:
        std::sync::Arc<std::sync::Mutex<forward12::identity12::IdentityRegistry>>,

    /// The sole device-owned C65/HOC1 allocation.  It drops after every queue
    /// has been destroyed and before the runtime callback pointers disappear.
    pub(crate) outer_command_pool: std::sync::Arc<forward12::command_pool12::OuterCommandPool>,

    /// Queue context generations are monotonically assigned and never reused
    /// within this A5 device generation.
    pub(crate) next_outer_context_generation: std::sync::atomic::AtomicU64,

    /// Bounded weak lifetime set for exact terminal allocation scopes. Queue
    /// pointers are never allocation identities or lookup keys.
    pub(crate) outer_queues: forward12::queue::OuterQueueRegistry12,

    /// Stable construction-time callback context. vkd3d may allocate internal
    /// Vulkan memory before this runtime-owned device block is initialized, so
    /// the callbacks must never dereference `hDrvDevice` on that forward edge.
    /// The shared registry and paging pool are the same objects later consumed
    /// by submission; this is not a second namespace or lookup service.
    pub(crate) vkd3d_outer_context: Box<Vkd3dOuterContext12>,
}

pub(crate) struct Vkd3dOuterContext12 {
    pub(crate) h_device: ddi12::D3D12DDI_HDEVICE,
    pub(crate) h_rt_device: ddi12::D3D12DDI_HRTDEVICE,
    pub(crate) um_callbacks: *const ddi12::D3D12DDI_CORELAYER_DEVICECALLBACKS_0062,
    pub(crate) device_generation: u64,
    pub(crate) outer_allocations:
        std::sync::Arc<std::sync::Mutex<forward12::identity12::IdentityRegistry>>,
    pub(crate) outer_command_pool: std::sync::Arc<forward12::command_pool12::OuterCommandPool>,
}

/// `D3D12DDI_CORELAYER_DEVICECALLBACKS_0062` is a byte-exact **prefix** of
/// `_0116`, field for field.
///
/// ⛔ This is the premise of [`HeliosD3D12Device::um_callbacks`] keeping its
/// `_0062` type on the 0116 arm, and it is the `ARCHITECTURE.md` §12 trap 2
/// surface in its D3D12 form: `p12UMCallbacks` is a union of six arms of
/// 12/14/17/18/19/20 pointer-wide members, and reading the wrong one reads past
/// the end of a shorter table. Here the direction is safe — a shorter *view* of
/// a longer table — but "safe" is a claim about offsets, so the offsets are
/// asserted rather than described.
macro_rules! assert_corelayer_prefix {
    ($($field:ident),* $(,)?) => {
        const _: () = {
            $(assert!(
                core::mem::offset_of!(ddi12::D3D12DDI_CORELAYER_DEVICECALLBACKS_0062, $field)
                    == core::mem::offset_of!(ddi12::D3D12DDI_CORELAYER_DEVICECALLBACKS_0116, $field)
            );)*
            // ⭐ And the list is COMPLETE: naming 17 of the 18 fields would
            // assert a prefix relation that holds on the fields someone
            // remembered. The count comes from the macro's own argument list.
            const NAMED: usize = [$(stringify!($field)),*].len();
            assert!(
                NAMED * core::mem::size_of::<usize>()
                    == core::mem::size_of::<ddi12::D3D12DDI_CORELAYER_DEVICECALLBACKS_0062>()
            );
        };
    };
}

assert_corelayer_prefix!(
    pfnSetErrorCb,
    pfnSetCommandListErrorCb,
    pfnSetCommandListDDITableCb,
    pfnCreateContextCb,
    pfnCreateContextVirtualCb,
    pfnDestroyContextCb,
    pfnCreatePagingQueueCb,
    pfnDestroyPagingQueueCb,
    pfnMakeResidentCb,
    pfnEvictCb,
    pfnReclaimAllocations2Cb,
    pfnOfferAllocationsCb,
    pfnAllocateCb,
    pfnDeallocateCb,
    pfnCreateSchedulingGroupContextCb,
    pfnCreateSchedulingGroupContextVirtualCb,
    pfnCreateHwQueueCb,
    pfnQueueBackgroundProcessingWorkCb,
);

/// The size of the private block the runtime must allocate for one device.
///
/// ⛔ **One function, called by both `pfnCalcPrivateDeviceSize` and
/// `pfnCreateDevice`**, because `DEBUGGABLE` reaches both and a size that is a
/// function of `Flags` at one site and a constant at the other is a buffer
/// overrun waiting for a debug-layer client (`DDI_REFERENCE.md` §1.4).
///
/// Today the answer does not depend on `flags` — nothing in
/// [`HeliosD3D12Device`] is debug-only. The parameter is present so that when a
/// lane adds a debug-only field it is *forced* to change one function rather
/// than remember two.
pub(crate) fn device_private_size(_flags: ddi12::D3D12DDI_CREATE_DEVICE_FLAGS) -> usize {
    core::mem::size_of::<HeliosD3D12Device>()
}

/// A device that has been written into the runtime's private block but whose
/// handle the runtime has not accepted yet.
///
/// Dropping it tears the device down; [`DeviceUnderConstruction::defuse`] hands
/// ownership to the runtime. `umd/src/adapter.rs`'s guard of the same name is
/// the precedent, and its docstring records what its absence cost: a leaked
/// Vulkan device per failed create, and a leaked kernel context and paging queue
/// per attempt, which *"a crash-looping client exhausted"*.
struct DeviceUnderConstruction {
    device: *mut HeliosD3D12Device,
}

impl DeviceUnderConstruction {
    /// The runtime now owns the device; do not tear it down.
    fn defuse(mut self) {
        self.device = core::ptr::null_mut();
    }
}

impl Drop for DeviceUnderConstruction {
    fn drop(&mut self) {
        if self.device.is_null() {
            return;
        }
        log_error!("CreateDevice rollback: dropping the partially-built D3D12 device");
        // SAFETY: `device` points at the runtime-allocated private block this
        // guard was constructed around, `core::ptr::write` has run over it, and
        // the runtime has never seen the handle — so this guard is the only
        // owner and no other reference can exist during teardown.
        unsafe { core::ptr::drop_in_place(self.device) };
    }
}

/// `pfnCalcPrivateDeviceSize`.
///
/// # Safety
/// `arg`, when non-null, must point at a live `D3D12DDIARG_CALCPRIVATEDEVICESIZE`
/// for the duration of the call.
pub(crate) unsafe fn calc_private_device_size(
    arg: *const ddi12::D3D12DDIARG_CALCPRIVATEDEVICESIZE,
) -> usize {
    if arg.is_null() {
        // ⚠ There is no HRESULT to refuse with — the DDI returns `SIZE_T`. A 0
        // here is paired with a `create_device` that will refuse the same null
        // world, so the runtime allocates nothing and the create fails cleanly.
        note_refusal(&UMD12_REFUSALS.calc_private_device_size_bad_arg);
        return 0;
    }
    // SAFETY: non-null per the check above; the DDI declares it `_In_ CONST`, so
    // the runtime guarantees a live, aligned struct for the call.
    let a = unsafe { &*arg };

    // Refuse-shaped, but a size cannot refuse: report 0 for a version this
    // driver never advertised, so that if the runtime somehow proceeds it
    // proceeds into `create_device`'s explicit refusal rather than into a block
    // sized for a shape nobody agreed on.
    if Ddi12Interface::from_pair(a.Interface, a.Version).is_none() {
        note_refusal(&UMD12_REFUSALS.ddi12_version_mismatch);
        let (major, minor, build) = adapter12::decode_pair(a.Interface, a.Version);
        log_error!(
            "CalcPrivateDeviceSize: UNADVERTISED Interface={:#010x} (major={major} minor={minor}) \
             Version={:#010x} (build={build}) -> 0",
            a.Interface,
            a.Version,
        );
        return 0;
    }

    // ⚠ A negotiated-but-unimplemented Core build (`Umd12CoreDdi=116`) does
    // **not** refuse here, and that is deliberate rather than an oversight. This
    // DDI only states how many bytes the runtime should allocate for a block
    // this driver owns and never writes on that arm; refusing with a 0 could
    // stop the runtime before `pfnCreateDevice`, which is the exact call whose
    // arrival — with which `(Interface, Version)` — is the experiment's whole
    // reading. The refusal lives one call later, where nothing is constructed
    // either way.

    let size = device_private_size(a.Flags);
    log_error!("CalcPrivateDeviceSize: Flags={:#x} -> {size}", a.Flags,);
    size
}

/// `pfnCreateDevice`.
///
/// The eight steps, in the order `umd/src/adapter.rs` learned they must run:
///
/// 0. validate **every** runtime-supplied pointer, before constructing anything;
/// 1. dispatch the interface through the closed set — never an `else`;
/// 2. refuse a negotiated build whose **table shapes** are not implemented,
///    still before constructing anything (`Umd12CoreDdi=116`);
/// 3. bring up the engine device;
/// 4. write `HeliosD3D12Device` into the runtime's private block, under the
///    unwind guard;
/// 5. defuse the guard and return.
///
/// ⚠ There is no table fill here. D3D12 fills its tables at **adapter** scope
/// through `pfnFillDDITable`, before any device exists (`ARCHITECTURE.md` §1.2,
/// and measured at S5) — the opposite of D3D11, where `CreateDevice` writes the
/// device-funcs table itself.
///
/// # Safety
/// `arg` must point at a live `D3D12DDIARG_CREATEDEVICE_0109` for the duration
/// of the call, and its `hDrvDevice.pDrvPrivate` at
/// [`device_private_size`]-many writable bytes the runtime allocated for this
/// device.
pub(crate) unsafe fn create_device(
    arg: *const ddi12::D3D12DDIARG_CREATEDEVICE_0109,
    adapter_identity: adapter12::AdapterIdentity,
) -> Hresult {
    if arg.is_null() {
        note_refusal(&UMD12_REFUSALS.create_device_bad_arg);
        return E_INVALIDARG;
    }
    // SAFETY: non-null per the check above. ⛔ It is the `_0109` shape and not
    // `_0003` only because D12 advertises a single token — a `_0003`-generation
    // negotiation would make the two trailing fields (`pReserveRanges`,
    // `NumReserveRanges`) a read past the end of the runtime's struct.
    let a = unsafe { &*arg };

    // ── 0. Validate BEFORE constructing ─────────────────────────────────────
    if a.hDrvDevice.pDrvPrivate.is_null() {
        log_error!("CreateDevice: null hDrvDevice -> E_INVALIDARG");
        note_refusal(&UMD12_REFUSALS.create_device_bad_arg);
        return E_INVALIDARG;
    }
    if a.pKTCallbacks.is_null() {
        log_error!("CreateDevice: null pKTCallbacks -> E_INVALIDARG");
        note_refusal(&UMD12_REFUSALS.create_device_bad_arg);
        return E_INVALIDARG;
    }
    // SAFETY: every arm of this union is a pointer at offset 0 — machine-checked
    // by the bindgen layout assertions — so reading one member to null-check it
    // is well-defined for any initialisation the runtime can have performed.
    // ⛔ Which member is *named* is load-bearing where the pointee is read; that
    // choice is made once, below, against the negotiated version.
    let um_callbacks_raw = unsafe { a.__bindgen_anon_1.p12UMCallbacks_0062 };
    if um_callbacks_raw.is_null() {
        log_error!("CreateDevice: null p12UMCallbacks -> E_INVALIDARG");
        note_refusal(&UMD12_REFUSALS.create_device_bad_arg);
        return E_INVALIDARG;
    }

    // ── 1. The closed interface dispatch ────────────────────────────────────
    let Some(negotiated) = Ddi12Interface::from_pair(a.Interface, a.Version) else {
        // ⛔ `ARCHITECTURE.md` §12 trap 2, refused instead of guessed: treating
        // an unknown interface as the newest known one is what bulk-filled 150
        // slots into a 101-slot table. Here there is no arm that builds anything
        // for an unrecognised pair, so the mistake is unrepresentable rather
        // than merely avoided.
        let (major, minor, build) = adapter12::decode_pair(a.Interface, a.Version);
        log_error!(
            "CreateDevice: UNADVERTISED Interface={:#010x} (major={major} minor={minor}) \
             Version={:#010x} (build={build}) token={:#018x} advertised={} -> \
             DXGI_ERROR_UNSUPPORTED",
            a.Interface,
            a.Version,
            ((a.Interface as u64) << 32) | (a.Version as u64),
            adapter12::selected_core_ddi_build(),
        );
        note_refusal(&UMD12_REFUSALS.ddi12_version_mismatch);
        return DXGI_ERROR_UNSUPPORTED;
    };

    // ⭐ The received token, decoded, on every create — not only on the
    // mismatch path. It is the one line that says which Core DDI build the
    // runtime actually negotiated, and "the runtime accepted the token we
    // advertised" is not a fact a driver may infer from having advertised it.
    let (major, minor, build) = adapter12::decode_pair(a.Interface, a.Version);
    log_error!(
        "CreateDevice: {} Interface={:#010x} (major={major} minor={minor}) Version={:#010x} \
         (build={build}) token={:#018x} hRTDevice={:p} hDrvDevice={:p} pKTCallbacks={:p} \
         p12UMCallbacks={:p} Flags={:#x} NumReserveRanges={}",
        negotiated.name(),
        a.Interface,
        a.Version,
        ((a.Interface as u64) << 32) | (a.Version as u64),
        a.hRTDevice.handle,
        a.hDrvDevice.pDrvPrivate,
        a.pKTCallbacks,
        um_callbacks_raw,
        a.Flags,
        a.NumReserveRanges,
    );

    // ── 2. ⛔⛔ THE TABLE-SHAPE GATE ─────────────────────────────────────────
    //
    // An exhaustive match, not an `if`: the day a third arm is added the
    // compiler makes someone decide which side of this gate it is on, which is
    // the only structural defence against a version being advertised without
    // its tables.
    //
    // ⛔ **A negotiated version selects a TABLE SHAPE**, and `forward12::tables12`
    // fills exactly one set of them — whichever its three `pub(crate) type`
    // aliases name. Every handler in `forward12` is typed against those aliases,
    // and so is this device block's `p12UMCallbacks` union arm. Constructing a
    // device for a build whose shapes are not the filled ones is
    // `ARCHITECTURE.md` §12 trap 2 with the version *negotiated* instead of
    // guessed: the D3D11 driver's `else` arm filled 150 pointer slots into a
    // 101-slot table, *"a 376..392 byte out-of-bounds write into the runtime's
    // heap"*.
    //
    // ⚠ The shapes are deliberately NOT restated here as `_0109`/`_0108`/`_0001`.
    // A second copy of that fact in a file that does not own it is the thing
    // that goes stale; the log line below reads it off `tables12` instead.
    //
    // ⚠ Refusing HERE — before `BridgeDevice12::create` and before the in-place
    // `core::ptr::write` — is the whole point: nothing is constructed, nothing
    // is leaked, and the runtime gets a clean decline.
    //
    // ⭐ **And it is early enough that no table is ever filled for an
    // unimplemented shape.** ⛔ MEASURED, 2026-08-09, and it CORRECTS this
    // crate's own module docs: `adapter12`'s header and `ARCHITECTURE.md` §1.2
    // both say `pfnFillDDITable` runs *before* `pfnCreateDevice`. On
    // 26100.8737 it does not — the observed order is `OpenAdapter12` →
    // `pfnGetCaps`(1074) → `pfnGetSupportedVersions` ×2 →
    // `pfnCalcPrivateDeviceSize` → **`pfnCreateDevice`** → `pfnGetCaps` ×24 →
    // `pfnGetOptionalDDITables` → `pfnFillDDITable` ×5
    // (`tmp/dx12/core-ddi-0116/final-A-110-default/umd12.log:11-47`). So this
    // refusal is upstream of the fill, and the 0116 arm never presents a table
    // of a shape this driver has not written.
    // ⚠ It would be safe either way: `forward12::tables12::fill` takes its byte
    // count from the runtime's own `SIZE_T` in both directions and stub-fills
    // the whole buffer first. The ordering makes the question moot rather than
    // merely survivable.
    // ⭐ **DERIVED, as of the retirement's U0.** The gate used to be a literal
    // `R8_0116 => refuse` arm, which was correct and unmaintainable in the same
    // breath: it said "0116 is not implemented" in a file that would not notice
    // when it became implemented. It now asks
    // `Ddi12Interface::tables_implemented()`, which reads
    // `forward12::tables12`'s own three type aliases through `TableShape` — so
    // the day `tables12` fills `…_CORE_0116` + `…_FUNCS_3D_0114` this gate opens
    // by construction, and if `tables12` ever moves while the knob default does
    // not, `adapter12`'s compile-time block fails the build first.
    if !negotiated.tables_implemented() {
        let (dev, list, queue) = negotiated.table_builds();
        note_refusal(&UMD12_REFUSALS.create_device_core_ddi_unimplemented);
        log_error!(
            "CreateDevice: the runtime NEGOTIATED Core DDI build {build} (token {:#018x}), which \
             selects the DEVICE_CORE_{dev:04} + COMMAND_LIST_3D_{list:04} + \
             COMMAND_QUEUE_CORE_{queue:04} table shapes -- this build's forward12::tables12 fills \
             DEVICE_CORE_{:04} + COMMAND_LIST_3D_{:04} + COMMAND_QUEUE_CORE_{:04}, so no device is \
             constructed. ⛔ The command-list tables are the SAME 600 bytes at the SAME 75 \
             offsets, so this is the one mismatch no size check can catch: 38 of the 0114 slots \
             take D3D12DDI_API_HCOMMANDLIST (a runtime-bypass header) where the 0108 bodies \
             expect D3D12DDI_HCOMMANDLIST. -> DXGI_ERROR_UNSUPPORTED",
            ((a.Interface as u64) << 32) | (a.Version as u64),
            <forward12::tables12::DeviceCoreTable as adapter12::TableShape>::BUILD,
            <forward12::tables12::CommandListTable as adapter12::TableShape>::BUILD,
            <forward12::tables12::CommandQueueTable as adapter12::TableShape>::BUILD,
        );
        // ⛔ `DXGI_ERROR_UNSUPPORTED` (0x887A_0004), NEVER
        // `DXGI_ERROR_DRIVER_INTERNAL_ERROR` (0x887A_0020): the latter is
        // recorded by the runtime and by ETW as a *driver fault*, and this is a
        // declined negotiation. R801 is the scar on the D3D11 side.
        return DXGI_ERROR_UNSUPPORTED;
    }

    // ── 2b. The corelayer union arm ─────────────────────────────────────────
    //
    // ⛔ **Still an exhaustive match, and this is now where that property
    // earns its keep.** The gate above is a boolean; picking which of the six
    // `p12UMCallbacks*` union arms the runtime actually filled is the
    // `ARCHITECTURE.md` §12 trap 2 landmine itself, and it must be decided by
    // the negotiated version and nothing else. A third arm added to
    // `Ddi12Interface` fails to compile here until someone says which callback
    // table it brings.
    //
    // ⚠ `um_callbacks_raw` (the `_0062` view, null-checked above) is kept on
    // BOTH arms — a prefix read of the longer table, asserted field-by-field at
    // the top of this file — so `set_error` and `set_command_list_error` need
    // no arm of their own.
    let um_callbacks_0116 = match negotiated {
        Ddi12Interface::R8_0110 => core::ptr::null(),
        // SAFETY: every arm of this union is a pointer at offset 0 — machine
        // checked by the bindgen layout assertions — and the negotiated version
        // is `_0116`, which is what makes `p12UMCallbacks_0116` the arm the
        // runtime filled. The pointer is not dereferenced here.
        Ddi12Interface::R8_0116 => unsafe { a.__bindgen_anon_1.p12UMCallbacks_0116 },
    };

    // ⚠ `pReserveRanges` / `NumReserveRanges` are read and reported, not acted
    // on. They are the GPU-virtual-address ranges the runtime asks the driver to
    // reserve at device creation, and honouring them is L4's (resources, heaps,
    // GPU VA). Counting a non-empty request makes "we ignored it" a number
    // rather than a silence.
    if a.NumReserveRanges != 0 {
        note_refusal(&UMD12_REFUSALS.reserve_ranges_ignored);
    }

    // ── 3. Bring the engine up on the exact adapter identity queried during
    // OpenAdapter. A zero or stale identity cannot reach this point because the
    // package-versioned record was validated before hAdapter was published.
    let luid = adapter_identity.luid as u64;
    log_error!(
        "CreateDevice: adapter generation={} luid={:08x}:{:08x}",
        adapter_identity.generation,
        (luid >> 32) as u32,
        luid as u32,
    );
    let translator = match DirectTranslator::create(
        adapter_identity.luid,
        helios_protocol::HELIOS_HTS1_MAX_ENDPOINTS_PER_SESSION,
        Some(translator_sync_progress_join),
        Some(translator_sync_progress_query),
    ) {
        Ok(translator) => translator,
        Err(error) => {
            log_error!("CreateDevice: A5 translator creation refused: {:?}", error);
            note_refusal(&UMD12_REFUSALS.create_device_engine_failed);
            return E_FAIL;
        }
    };
    let outer_command_pool = match unsafe {
        forward12::command_pool12::OuterCommandPool::create(a.hRTDevice.handle, a.pKTCallbacks)
    } {
        Ok(pool) => std::sync::Arc::new(pool),
        Err(hr) => {
            log_error!(
                "CreateDevice: HOC1 pool construction refused hr={:#010x}",
                hr as u32
            );
            return if hr < 0 { hr } else { E_FAIL };
        }
    };
    let outer_allocations = std::sync::Arc::new(std::sync::Mutex::new(
        forward12::identity12::IdentityRegistry::new(),
    ));
    let mut vkd3d_outer_context = Box::new(Vkd3dOuterContext12 {
        h_device: a.hDrvDevice,
        h_rt_device: a.hRTDevice,
        um_callbacks: um_callbacks_raw,
        device_generation: translator.session_generation(),
        outer_allocations: std::sync::Arc::clone(&outer_allocations),
        outer_command_pool: std::sync::Arc::clone(&outer_command_pool),
    });
    let outer_context = vkd3d_outer_context.as_mut() as *mut Vkd3dOuterContext12 as usize;
    let Some(engine) = BridgeDevice12::create(
        translator.vk_instance() as usize,
        translator.get_instance_proc_addr() as usize,
        translator.module_base() as usize,
        luid as u32,
        (luid >> 32) as u32 as i32,
        outer_context,
        forward12::queue::outer_allocation_create as *const () as usize,
        forward12::queue::outer_allocation_teardown_begin as *const () as usize,
        forward12::queue::outer_allocation_begin as *const () as usize,
        forward12::queue::outer_allocation_finish as *const () as usize,
        forward12::queue::outer_allocation_retire as *const () as usize,
    ) else {
        // The C++ side has already logged the engine's HRESULT.
        log_error!("CreateDevice: vkd3d device creation FAILED -> E_FAIL");
        note_refusal(&UMD12_REFUSALS.create_device_engine_failed);
        return E_FAIL;
    };
    // ── 4. Construct in place, under the guard ──────────────────────────────
    let device = a.hDrvDevice.pDrvPrivate.cast::<HeliosD3D12Device>();
    // SAFETY: `pDrvPrivate` is non-null (checked above) and points at the block
    // the runtime allocated using the size THIS driver returned from
    // `pfnCalcPrivateDeviceSize`, which is `device_private_size(Flags)` — the
    // same function, so the block is exactly large enough and correctly aligned
    // for `HeliosD3D12Device`. `write` initialises it without reading whatever
    // the runtime left there.
    unsafe {
        core::ptr::write(
            device,
            HeliosD3D12Device {
                h_rt_device: a.hRTDevice,
                negotiated,
                flags: a.Flags,
                adapter_luid: adapter_identity.luid,
                um_callbacks: um_callbacks_raw,
                kt_callbacks: a.pKTCallbacks,
                engine,
                translator,
                um_callbacks_0116,
                outer_allocations,
                outer_command_pool,
                next_outer_context_generation: std::sync::atomic::AtomicU64::new(1),
                outer_queues: forward12::queue::OuterQueueRegistry12::new(),
                vkd3d_outer_context,
            },
        );
    }
    let guard = DeviceUnderConstruction { device };

    // ⚠ Every fallible step from here on must return WITHOUT defusing, so the
    // guard tears the engine device down. There are none today; there will be
    // once L2 mints WDDM contexts and L4 honours `pReserveRanges`, and this is
    // where they go.

    // ── 5. Hand it to the runtime ───────────────────────────────────────────
    guard.defuse();
    S_OK
}

/// `pfnDestroyDevice`.
///
/// ⚠ Lives on the **adapter** table (`d3d12umddi.h:13649`), not the device
/// table — a shape difference from D3D11 and, per `DDI_REFERENCE.md` §1.3, a
/// classic place to leave a NULL.
///
/// Returns `VOID`: there is no way to report a failure, which is the general
/// D3D12 shape (`DECISIONS.md` §7.6) and the reason the counters matter.
///
/// # Safety
/// `h_device` must be a handle this driver's [`create_device`] returned `S_OK`
/// for and which has not already been destroyed. Passing it twice is a double
/// free.
pub(crate) unsafe fn destroy_device(h_device: ddi12::D3D12DDI_HDEVICE) {
    // SAFETY: the caller guarantees a live, not-yet-destroyed handle from
    // `create_device`. Borrowed only to produce the line below, and dropped
    // before the `drop_in_place` that follows.
    let Some(dev) = (unsafe { device(h_device) }) else {
        note_refusal(&UMD12_REFUSALS.destroy_device_bad_arg);
        return;
    };
    log_device_teardown(dev);

    let device = h_device.pDrvPrivate.cast::<HeliosD3D12Device>();
    // SAFETY: the same live handle, which `core::ptr::write` initialised at
    // exactly this address. Dropping in place drops `BridgeDevice12`, which runs
    // the out-of-line C++ destructor and releases the engine's `ID3D12Device` —
    // and with it the Vulkan device, its memory and its queues. ⛔ The runtime
    // owns the memory itself and frees it; this must not.
    unsafe { core::ptr::drop_in_place(device) };
}

/// One line per device teardown: what the runtime gave this device, and what it
/// did with it.
///
/// ⭐ Not decoration — it is the readout for three things that have no other
/// channel, and dwm owns **several** devices in one process (20th session), so
/// per-device is the granularity that matters:
///
/// * **the two callback-table pointers.** The corelayer arm is the
///   `ARCHITECTURE.md` §12 trap 2 landmine in its D3D12 form (four union arms of
///   12/14/17/18 members). *"Did every device get the same `_0062` table?"* is a
///   real question with a real failure mode, and this is what answers it.
/// * **the counters and the noop hits at device scope**, which says what *this*
///   device touched rather than what the whole adapter did.
fn log_device_teardown(dev: &HeliosD3D12Device) {
    log_error!(
        "DestroyDevice: {} hRTDevice={:p} Flags={:#x} p12UMCallbacks={:p} p12UMCallbacks_0116={:p} \
         pKTCallbacks={:p} vkd3dOuter={:p} generation={}",
        dev.negotiated.name(),
        dev.h_rt_device.handle,
        dev.flags,
        dev.um_callbacks,
        dev.um_callbacks_0116,
        dev.kt_callbacks,
        dev.vkd3d_outer_context.as_ref(),
        dev.vkd3d_outer_context.device_generation,
    );
    crate::log_refusal_summary();
    crate::forward12::noop12::log_noop_hits();
}

/// Borrow the device behind a DDI handle.
///
/// ⛔ **The D3D12 soundness argument is re-derived here and NOT inherited from
/// D3D11**, which is what `umd_common::slot` demands in as many words
/// (`slot.rs:304-322`) and `PARALLEL.md` §9.4 restates: the D3D11 `&'static S`
/// argument rests on the runtime's `CUseCountedObject` first-created /
/// last-destroyed ordering, and *"`d3d12umddi` has no THREADING cap and no
/// `CUseCountedObject` statement has been located for it."*
///
/// The argument this function stands on is narrower and does not depend on that:
///
/// * the returned reference is **not** `'static` — it borrows for as long as the
///   caller's own binding lives, and every caller is one DDI invocation;
/// * `pfnDestroyDevice` is the only path that drops the block, and it is the
///   **device** object — the coarsest one there is. A runtime that could destroy
///   a device while another DDI held the same `hDrvDevice` could not implement
///   D3D12 at all;
/// * ⚠ concurrent **reads** across FREETHREADED worker threads are permitted by
///   `&` and are the expected case. A lane that needs interior mutation puts an
///   atomic or a lock in [`HeliosD3D12Device`] — it does not get a `&mut` from
///   here, and there is deliberately no `device_mut`.
///
/// ⚠ Even inside `destroy_device` the borrow is dropped before the
/// `drop_in_place`, so no reference is live across the teardown.
///
/// # Safety
/// `h_device` must be a live handle from [`create_device`] that
/// [`destroy_device`] has not been called on, and the returned reference must
/// not outlive the DDI call that obtained it.
pub(crate) unsafe fn device<'a>(
    h_device: ddi12::D3D12DDI_HDEVICE,
) -> Option<&'a HeliosD3D12Device> {
    let device = h_device.pDrvPrivate.cast::<HeliosD3D12Device>();
    if device.is_null() {
        return None;
    }
    // SAFETY: non-null per the check, and the caller guarantees it is a live
    // block `create_device` wrote and `destroy_device` has not dropped.
    Some(unsafe { &*device })
}

/// Report a device-scope failure to the runtime. Returns whether it was
/// delivered.
///
/// ⭐ **This is how a `VOID`-returning D3D12 DDI says it failed**, and most of
/// them are `VOID` (`DECISIONS.md` §7.6). It did not exist at S6-0b, and its
/// absence was the rule rather than an oversight — `PARALLEL.md` §10 forbids
/// `#[allow(dead_code)]` on a hand-written line (R908) and no handler could yet
/// fail. The S6 Round-1 lanes are what made it reachable: L2's queue-table fence
/// ops, L5's ten `VOID` view/heap slots and L6's thirteen `VOID` shader and
/// sub-state creates all fail with no other channel.
///
/// ⚠ **Three of the four lanes wrote this function independently, in isolated
/// worktrees, and converged on the same contract** — including the non-obvious
/// half, that it returns a `bool` instead of counting. That agreement is the
/// reason the shape below is stated as a rule rather than as one lane's taste.
///
/// Two things the S6-0b comment this replaced flagged as *looking* like
/// mistakes. Both are still true, both are load-bearing, and all three lanes
/// re-derived them:
///
/// * `PFND3D12DDI_SETERROR_CB` takes a `D3D10DDI_HRTDEVICE`, **not** the
///   `D3D12DDI_HRTDEVICE` the create arg carries. They are the same one-pointer
///   struct — `D3D12DDI_HRTDEVICE` is a bindgen *alias* — so `h_rt_device`
///   passes with no cast;
/// * a null `pfnSetErrorCb` must be **counted**. It is the only channel a `VOID`
///   slot has, so losing it turns an error into corrupt output instead of a
///   removed device.
///
/// ⛔ **Which is why the counter belongs to the CALLER, and why this is
/// `#[must_use]`.** Eleven lanes reach this function and each keeps its refusal
/// set in its own file (`PARALLEL.md` §9.1, and `lib.rs`'s `UMD12_REFUSAL_SETS`).
/// A counter named *here* would either have to live in `lib.rs` — the merge point
/// that whole scheme exists to remove — or bind this shared function to one
/// lane's set. `RefusalCounter::note` is `#[must_use]` for the same reason.
///
/// ⚠ Safe, not `unsafe`, and that is a deliberate choice between the two shapes
/// the lanes submitted. The only raw operation is reading `um_callbacks`, whose
/// validity is a **type invariant of [`HeliosD3D12Device`]** rather than a caller
/// obligation: [`create_device`] is the sole constructor, it null-checks the
/// pointer before writing the struct, the field is never reassigned, and there is
/// deliberately no `device_mut` through which it could be. An `unsafe fn` whose
/// precondition no caller can violate teaches callers to stop reading
/// `# Safety` sections.
#[must_use = "a device-scope error that could not be reported must be counted by the caller"]
pub(crate) fn set_error(dev: &HeliosD3D12Device, hr: Hresult) -> bool {
    if dev.um_callbacks.is_null() {
        return false;
    }
    // SAFETY: non-null per the check above, and `create_device` stored the arm
    // the negotiated version selects — D12 advertises exactly one token, so
    // `_0062` is the only reachable shape — for a table the runtime keeps alive
    // at least as long as the device. The borrow does not outlive this call.
    let Some(set_error_cb) = (unsafe { (*dev.um_callbacks).pfnSetErrorCb }) else {
        return false;
    };
    // SAFETY: the runtime supplied this callback for this device, and
    // `h_rt_device` is the handle it supplied alongside it and owns until
    // `pfnDestroyDevice`. The call transfers no ownership.
    unsafe { set_error_cb(dev.h_rt_device, hr) };
    true
}

/// Narrow an arbitrary HRESULT to one `pfnSetCommandListErrorCb` accepts.
///
/// ⛔ **The callback takes exactly three codes**, and the spec says so in a list
/// (`tmp/dx12/specs/d3d/CPUEfficiency.md:2143-2158`):
///
/// > There are only 3 errors which drivers should pass to this function:
/// > `E_OUTOFMEMORY`, `D3DDDIERR_DEVICEREMOVED`, `D3DDDIERROR_APPLICATIONERROR`.
///
/// So a recording slot that wants to report `E_INVALIDARG`, `E_NOTIMPL` or a raw
/// engine HRESULT cannot pass it through: everything that is not one of the two
/// specific conditions is *"the application recorded something this driver
/// cannot honour"*, which is what `D3DDDIERR_APPLICATIONERROR` means.
///
/// ⭐ **One declaration, three callers.** `cmdlist.rs`, `rootargs.rs` and
/// `copy.rs` all need it and the rule is subtle enough that three copies would
/// be three chances to drop an arm. It lives here rather than in a lane because
/// it is a property of the DDI, not of any lane — the same reason
/// [`set_error`] does.
pub(crate) fn command_list_error_code(hr: Hresult) -> Hresult {
    match hr {
        // Passed through: the runtime can act on an allocation failure, and it
        // is one of the three.
        E_OUTOFMEMORY => E_OUTOFMEMORY,
        // The engine reporting a lost device is the second, and it must not be
        // flattened into an application error: they call for different recovery.
        DXGI_ERROR_DEVICE_REMOVED | D3DDDIERR_DEVICEREMOVED => D3DDDIERR_DEVICEREMOVED,
        // Everything else. ⚠ Including `E_FAIL` and raw engine HRESULTs: this
        // driver cannot tell the runtime *which* engine failure occurred through
        // this channel, and the log line at the call site is where that detail
        // lives.
        _ => D3DDDIERR_APPLICATIONERROR,
    }
}

/// Report a **command-list**-scope failure to the runtime. Returns whether it was
/// delivered.
///
/// # ⭐⭐ This is the command-list table's error channel, and it is NOT
/// [`set_error`]
///
/// ⛔ The S6 Round 2 spine asserted the opposite — *"All 75 of its slots take
/// `D3D12DDI_HCOMMANDLIST` and nothing else, and 74 of the 75 return `VOID`, so
/// a recording failure can only be reported through the device-scoped
/// `pfnSetErrorCb`"* — and three lanes copied that sentence into 49 call sites
/// before the `PARALLEL.md` §10 review opened the header. The first half is
/// true; the conclusion is false.
///
/// `pfnSetCommandListErrorCb` sits **one field below `pfnSetErrorCb`** in
/// `D3D12DDI_CORELAYER_DEVICECALLBACKS_0062` and one field *above*
/// `pfnSetCommandListDDITableCb`, which this driver already reads out of that
/// same struct (`forward12::queue::set_command_list_ddi_table`). So the callback
/// was reachable the whole time. It takes the **runtime's** command-list handle,
/// `D3D12DDI_HRTCOMMANDLIST` — which is why `queue::CommandListState` now carries
/// one.
///
/// The difference is the whole point:
///
/// | callback | the runtime's response |
/// |---|---|
/// | `pfnSetErrorCb` | *"Removing device due to bad UMD error"* — the whole `ID3D12Device` dies, with every list, queue, PSO, heap and resource on it. If it is DWM's device, the compositor goes with it. |
/// | `pfnSetCommandListErrorCb` | *"the runtime will drop all calls into the driver which record commands on the specified command list"* — one list is quarantined and the application learns at `Close()`. |
///
/// ⚠ **Which is exactly the contract D3D12 recording already has**: a recording
/// error is not observable until `Close()`, and the runtime is what implements
/// that. Reporting a bad viewport count by removing the device is not a stricter
/// reading of the same rule, it is a different and much worse behaviour.
///
/// ⛔ The HRESULT must come from [`command_list_error_code`]; the callback takes
/// only three values.
///
/// ⚠ Safe, not `unsafe`, for the identical reason [`set_error`] is: the only raw
/// operation is reading `um_callbacks`, whose validity is a type invariant of
/// [`HeliosD3D12Device`] rather than a caller obligation.
#[must_use = "a command-list error that could not be reported must be counted by the caller"]
pub(crate) fn set_command_list_error(
    dev: &HeliosD3D12Device,
    h_rt_list: ddi12::D3D12DDI_HRTCOMMANDLIST,
    hr: Hresult,
) -> bool {
    if dev.um_callbacks.is_null() {
        return false;
    }
    // SAFETY: non-null per the check above, and `create_device` stored the arm
    // the negotiated version selects — D12 advertises exactly one token, so
    // `_0062` is the only reachable shape — for a table the runtime keeps alive
    // at least as long as the device. The borrow does not outlive this call.
    let Some(cb) = (unsafe { (*dev.um_callbacks).pfnSetCommandListErrorCb }) else {
        return false;
    };
    // SAFETY: the runtime supplied this callback for this device, and
    // `h_rt_list` is the handle it supplied to `pfnCreateCommandList` for the
    // list being reported on. The call transfers no ownership.
    unsafe { cb(h_rt_list, command_list_error_code(hr)) };
    true
}
