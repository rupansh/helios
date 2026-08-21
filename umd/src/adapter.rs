//! Adapter open/close, DDI version negotiation, and device creation.
//!
//! The two `OpenAdapter*` exports the runtime resolves by name, the
//! interface-version handshake behind them, the adapter identity token, and
//! `create_device` with the `DeviceUnderConstruction` unwind guard.
//!
//! Moved verbatim out of `lib.rs` by T8/R1106.
//!
//! ⚠ **Two, not three, since S5.** `OpenAdapter12` left this DLL entirely when
//! `helios_umd12.dll` took `UserModeDriverName[3]` (`ARCHITECTURE.md` §6.2). It
//! is the only export S5 removes from the D3D11 driver, and it is deliberately
//! not replaced by a refusing stub — see the note above `AdapterToken`.

use std::sync::atomic::{AtomicUsize, Ordering};

use crate::get_caps;
use crate::hr::{Hresult, DXGI_STATUS_NO_REDIRECTION, E_FAIL, E_NOTIMPL, E_OUTOFMEMORY, S_OK};
use crate::{bridge, ddi, device_funcs};
use crate::{log_error, trace_line};
use crate::{log_knob_inventory, log_self_module_path, trace_enabled};
use helios_protocol::{HeliosUmdAdapterInfoV1, HELIOS_PACKAGE_GENERATION};
use helios_umd_common::direct_translator::DirectTranslator;

const fn ddi_supported(major: u64, minor: u64, build: u64) -> u64 {
    let interface = (major << 16) | minor;
    (interface << 32) | (build << 16)
}

// WDDM 2.1 is the selected package surface: it provides the modern callback
// table used by the exact-context HQC1 and resource sync-token path while the
// device retains its physical Render submission. Keep the older layouts as
// explicit runtime-selected fallbacks.
const SUPPORTED_DDI_VERSIONS: &[u64] = &[
    ddi_supported(11, 34, 1), // D3DWDDM2_1_DDI_SUPPORTED
    ddi_supported(11, 16, 1), // D3DWDDM1_3_DDI_SUPPORTED
    ddi_supported(11, 15, 0), // D3D11_1_DDI_SUPPORTED
    ddi_supported(11, 10, 2), // D3D11_0_DDI_SUPPORTED
];

/// The DDI interface versions `GetSupportedVersions` advertises, as a closed
/// set. `CreateDevice` dispatches on this and nothing else: the previous
/// `if/else-if/else` chain treated "unknown or older interface" as D3D11.0 and
/// bulk-filled `size_of::<D3D11DDI_DEVICEFUNCS>()` = 150 pointer slots into
/// whatever table the runtime had allocated — 101 slots for
/// `D3D10DDI_DEVICEFUNCS`, 103 for `D3D10_1DDI_DEVICEFUNCS`, i.e. a 376..392
/// byte out-of-bounds write into the runtime's heap. `OpenAdapter10` installs
/// `create_device` while installing no `pfnGetSupportedVersions`, so on that
/// path the negotiated interface is entirely the runtime's choice and every
/// D3D10 interface (`0x000a_0001..0x000a_000a`) landed in that `else`.
/// `pub(crate)`: stored on `HeliosDevice` since Phase C — a deferred context
/// fills its context-funcs table in the parent device's negotiated shape.
#[derive(Copy, Clone, PartialEq, Eq)]
pub(crate) enum NegotiatedInterface {
    D3D11_0,
    D3D11_1,
    Wddm1_3,
    Wddm2_1,
}

impl NegotiatedInterface {
    const D3D11_0_INTERFACE: u32 = 0x000b_000a;
    const D3D11_1_INTERFACE: u32 = 0x000b_000f;
    const WDDM1_3_INTERFACE: u32 = 0x000b_0010;
    const WDDM2_1_INTERFACE: u32 = 0x000b_0022;

    /// Panic-free: a closed four-value match, with no table indexing.
    fn from_interface(interface: u32) -> Option<Self> {
        match interface {
            Self::WDDM2_1_INTERFACE => Some(Self::Wddm2_1),
            Self::WDDM1_3_INTERFACE => Some(Self::Wddm1_3),
            Self::D3D11_1_INTERFACE => Some(Self::D3D11_1),
            Self::D3D11_0_INTERFACE => Some(Self::D3D11_0),
            _ => None,
        }
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Wddm2_1 => "WDDM2_1",
            Self::Wddm1_3 => "WDDM1_3",
            Self::D3D11_1 => "D3D11_1",
            Self::D3D11_0 => "D3D11_0",
        }
    }
}

// Keep the enum and the advertised table in lockstep at COMPILE time: adding a
// version to SUPPORTED_DDI_VERSIONS without adding an enum variant fails here,
// and adding a variant without a fill arm fails the exhaustive match in
// `create_device`. That is the property the `else`-as-default did not have.
//
const _: () = {
    assert!(SUPPORTED_DDI_VERSIONS.len() == 4);
    assert!((SUPPORTED_DDI_VERSIONS[0] >> 32) as u32 == NegotiatedInterface::WDDM2_1_INTERFACE);
    assert!((SUPPORTED_DDI_VERSIONS[1] >> 32) as u32 == NegotiatedInterface::WDDM1_3_INTERFACE);
    assert!((SUPPORTED_DDI_VERSIONS[2] >> 32) as u32 == NegotiatedInterface::D3D11_1_INTERFACE);
    assert!((SUPPORTED_DDI_VERSIONS[3] >> 32) as u32 == NegotiatedInterface::D3D11_0_INTERFACE);
};

// The seven d3d10umddi ABI structs that used to be hand-transcribed here
// (ddi::D3D10DDI_HADAPTER, D3d10DdiArgOpenAdapter, D3d10DdiAdapterFuncs,
// D3d10_2DdiAdapterFuncs, D3d10_2DdiArgGetCaps, DxgiDdiBaseArgs,
// D3d10DdiArgCreateDevice) are gone: every one of them is generated into
// `ddi::*` from the WDK header, with bindgen's size/alignment/offset
// assertions, and the code below uses the generated types directly. R802.
//
// The five hand-written `D3d12Ddi*` structs that used to sit here, the eight
// `d3d12_*` handlers and `D3D12_SUPPORTED_DDI_VERSIONS` went with T6's R908:
// the compiler already proved them unreachable behind `OpenAdapter12`'s
// unconditional early return, and `#[allow(unreachable_code)]` was silencing
// that proof. `D3d10_2DdiArgGetCaps` is NOT among them -- the live `get_caps`
// uses it.
//
// ⛔ AND THE `OpenAdapter12` EXPORT ITSELF IS GONE, at stage S5 (ARCHITECTURE.md
// §6.2, §11). It is the ONE deleted export in that commit, and it must not come
// back: `helios_umd12.dll` now serves `UserModeDriverName[3]` and owns the D3D12
// DDI. Two DLLs exporting the same name is not a link conflict -- the loader
// resolves each module's own -- but it makes "which one answered?" unanswerable
// from a log, and both DLLs can be loaded into one process (S4b's whole subject).
// A refusing duplicate here would also outrank nothing and explain nothing: the
// D3D12 kill switch lives in `umd12/src/knobs12.rs` as `UmdD3D12`, so "D3D12 is
// off" is already a countable, greppable fact in exactly one place.

const ADAPTER_STATE_MAGIC: u64 = 0x3141_3131_534F_494C;

/// Per-open adapter identity returned by the package KMD.
///
/// This is owned directly by `hAdapter`, never indexed through a process-global
/// table.  `CloseAdapter` invalidates the tag before reclaiming the allocation.
struct AdapterState {
    magic: u64,
    generation: u64,
    luid: i64,
}

/// Adapter handles that did not carry a live [`AdapterState`].
///
/// COUNT AND LOG ONLY -- deliberately not a refusal. The counter has to be
/// observed at zero on a real boot before any DDI starts rejecting on it.
/// The one known caller that tripped it by design -- `helios_umd_selftest`,
/// which passed a null `pDrvPrivate` -- was deleted in T6/R909, so a nonzero
/// reading is now unexplained and worth chasing.
static ADAPTER_UNRECOGNISED: AtomicUsize = AtomicUsize::new(0);

/// Resolve an adapter handle to the exact state we handed out. Reports only.
unsafe fn adapter_state(h: ddi::D3D10DDI_HADAPTER) -> Option<*mut AdapterState> {
    let state = h.pDrvPrivate.cast::<AdapterState>();
    if !state.is_null()
        && state.is_aligned()
        // SAFETY: WDDM only returns a driver handle supplied by this UMD.  The
        // null/alignment checks make malformed handles fail before the read.
        && unsafe { (*state).magic == ADAPTER_STATE_MAGIC }
    {
        return Some(state);
    }
    let n = ADAPTER_UNRECOGNISED.fetch_add(1, Ordering::Relaxed);
    if n < 8 {
        log_error!(
            "adapter handle not ours: pDrvPrivate={:p} (x{}) — counted only",
            h.pDrvPrivate,
            n + 1
        );
    }
    None
}

unsafe fn query_adapter_state(
    open: &ddi::D3D10DDIARG_OPENADAPTER,
) -> Result<Box<AdapterState>, Hresult> {
    if open.pAdapterCallbacks.is_null() {
        log_error!("OpenAdapter: null pAdapterCallbacks");
        return Err(E_FAIL);
    }
    // SAFETY: the runtime owns this callback table for the duration of
    // OpenAdapter and supplied its typed pointer in `open`.
    let Some(query) = (unsafe { &*open.pAdapterCallbacks }).pfnQueryAdapterInfoCb else {
        log_error!("OpenAdapter: null pfnQueryAdapterInfoCb");
        return Err(E_FAIL);
    };
    let mut info = HeliosUmdAdapterInfoV1::query(HELIOS_PACKAGE_GENERATION);
    let args = ddi::D3DDDICB_QUERYADAPTERINFO {
        pPrivateDriverData: core::ptr::addr_of_mut!(info).cast(),
        PrivateDriverDataSize: core::mem::size_of_val(&info) as u32,
    };
    // SAFETY: the runtime handle and callback are paired by OpenAdapter; the
    // fixed record lives across the synchronous call.
    let hr = unsafe { query(open.hRTAdapter.handle, &args) };
    if hr < 0 || info.validate_reply(HELIOS_PACKAGE_GENERATION).is_err() {
        log_error!(
            "OpenAdapter: private adapter identity query failed hr=0x{:08x}",
            hr as u32
        );
        return Err(E_FAIL);
    }
    let luid = info.adapter_luid as u64;
    log_error!(
        "OpenAdapter: adapter generation={} luid={:08x}:{:08x}",
        info.adapter_generation,
        (luid >> 32) as u32,
        luid as u32,
    );
    Ok(Box::new(AdapterState {
        magic: ADAPTER_STATE_MAGIC,
        generation: info.adapter_generation,
        luid: info.adapter_luid,
    }))
}

#[no_mangle]
pub unsafe extern "system" fn OpenAdapter10(
    open_data: *mut ddi::D3D10DDIARG_OPENADAPTER,
) -> Hresult {
    log_error!("OpenAdapter10");
    unsafe { open_adapter_common(open_data, false) }
}

#[no_mangle]
pub unsafe extern "system" fn OpenAdapter10_2(
    open_data: *mut ddi::D3D10DDIARG_OPENADAPTER,
) -> Hresult {
    log_error!("OpenAdapter10_2");
    unsafe { open_adapter_common(open_data, true) }
}

unsafe fn open_adapter_common(
    open_data: *mut ddi::D3D10DDIARG_OPENADAPTER,
    with_10_2: bool,
) -> Hresult {
    // ⚠ FIRST, above the first `log_error!` below and above anything that
    // consults the trace gate. `helios_umd_common::log` is shared with
    // `helios_umd12.dll` (stage S2), so each module must name its own log file
    // and resolve its own trace knob before it writes a line — two drivers
    // appending to one file would interleave unreadably and break every piece
    // of evidence that reads the log per module.
    //
    // ⛔ The basename is `"umd"`, which is also `umd_common`'s default, so this
    // call cannot change where D3D11 logs. That is deliberate: S2 must leave the
    // D3D11 path provably unchanged, and it makes the SECOND driver the one that
    // has to name itself. `LOG_INIT_LATE` counts the failure.
    //
    // `OpenAdapter` is the first entry point dxgkrnl calls on this module, and
    // as of S5 it is the ONLY one: the `OpenAdapter12` export that used to log
    // above this call is gone from this DLL, so there is no longer any path that
    // writes a line before `log::init` runs. ⚠ Before S5 that was harmless only
    // because the basename and the default coincided.
    crate::log::init("umd", crate::knobs::umd_trace_knob());

    if open_data.is_null() {
        log_error!("open_adapter_common null open_data");
        return E_NOTIMPL;
    }

    let open = unsafe { &mut *open_data };
    // `pAdapterFuncs` and `pAdapterFuncs_2` are the two members of one union;
    // which one the runtime means is decided by which OpenAdapter export it
    // called, i.e. by `with_10_2`. Read it generically for the null check --
    // both members alias at offset 0 -- and name the right member per arm
    // below, where the table shape actually matters.
    // SAFETY: reading either member of a union of same-offset pointers is
    // well-defined for any initialisation the runtime can have performed.
    if unsafe { open.__bindgen_anon_1.pAdapterFuncs }.is_null() {
        log_error!("open_adapter_common null p_adapter_funcs");
        return E_NOTIMPL;
    }
    log_error!(
        "open_adapter_common interface=0x{:08x} version=0x{:08x} with_10_2={}",
        open.Interface,
        open.Version,
        with_10_2
    );
    log_self_module_path();
    log_knob_inventory();

    let adapter = match unsafe { query_adapter_state(open) } {
        Ok(adapter) => adapter,
        Err(hr) => return hr,
    };

    open.hAdapter = ddi::D3D10DDI_HADAPTER {
        pDrvPrivate: Box::into_raw(adapter).cast(),
    };

    // The generated D3D10_2DDI_ADAPTERFUNCS is FLAT -- the WDK repeats the
    // three 10.0 entries rather than nesting them, where the hand copy modelled
    // them as a `base` sub-struct. Same layout (the hand copy's `base` sat at
    // offset 0), different spelling.
    unsafe {
        if with_10_2 {
            let funcs = &mut *open.__bindgen_anon_1.pAdapterFuncs_2;
            funcs.pfnCalcPrivateDeviceSize = Some(calc_private_device_size);
            funcs.pfnCreateDevice = Some(create_device);
            funcs.pfnCloseAdapter = Some(close_adapter);
            funcs.pfnGetSupportedVersions = Some(get_supported_versions);
            funcs.pfnGetCaps = Some(get_caps);
        } else {
            let funcs = &mut *open.__bindgen_anon_1.pAdapterFuncs;
            funcs.pfnCalcPrivateDeviceSize = Some(calc_private_device_size);
            funcs.pfnCreateDevice = Some(create_device);
            funcs.pfnCloseAdapter = Some(close_adapter);
        }
    }

    S_OK
}

// NOTE on the calling convention: the five functions below are `extern "C"`,
// not `extern "system"`, because they are stored into the generated
// `D3D10DDI_ADAPTERFUNCS` / `D3D10_2DDI_ADAPTERFUNCS` tables and bindgen types
// every `PFND3D10DDI_*` as `extern "C"`. On x86_64-pc-windows-msvc the two are
// the same calling convention, so this is a no-op in the emitted code -- but
// rustc treats them as distinct TYPES, so the tables will not accept a
// "system" fn. The `OpenAdapter*` exports above stay `extern "system"`: they
// are resolved by the loader against an exported name, not through a PFN type.
unsafe extern "C" fn calc_private_device_size(
    _h_adapter: ddi::D3D10DDI_HADAPTER,
    _args: *const ddi::D3D10DDIARG_CALCPRIVATEDEVICESIZE,
) -> ddi::SIZE_T {
    let size = device_funcs::device_private_size();
    log_error!("CalcPrivateDeviceSize -> {size}");
    // `SIZE_T` is the WDK's spelling and is a distinct type from `usize` even
    // though both are 64-bit here, so the PFN type needs the conversion.
    size as ddi::SIZE_T
}

unsafe extern "C" fn create_device(
    h_adapter: ddi::D3D10DDI_HADAPTER,
    args: *mut ddi::D3D10DDIARG_CREATEDEVICE,
) -> Hresult {
    let Some(adapter) = (unsafe { adapter_state(h_adapter) }) else {
        return E_FAIL;
    };
    // SAFETY: the runtime passes a valid `D3D10DDIARG_CREATEDEVICE*` per the
    // `PFND3D10DDI_CREATEDEVICE` contract; we only read scalar/pointer fields and
    // never write through it, so an E_NOTIMPL return leaves the runtime's state
    // untouched. We null-check defensively.
    if args.is_null() {
        log_error!("CreateDevice null args -> E_NOTIMPL");
        return E_NOTIMPL;
    }
    // The parameter is now typed by `PFND3D10DDI_CREATEDEVICE` itself rather
    // than being a `*mut c_void` this function reinterprets, so the cast that
    // used to sit here -- the one place a wrong type would have gone unnoticed
    // -- no longer exists.
    let create = unsafe { &*args };
    // The three union members this function reads generically: for logging, for
    // null-checking, and for handing to the runtime. Every member of each union
    // is a pointer at offset 0 -- machine-checked by the bindgen layout
    // assertions in the generated module -- so which member is named here does
    // not change the value. Where the CHOICE is load-bearing is the fill match
    // at step 3, which names the member matching the negotiated interface.
    // SAFETY: reading any member of a union of same-offset pointers is
    // well-defined for every initialisation the runtime can have performed;
    // `create` itself is validated by the caller contract above.
    let p_device_funcs = unsafe { create.__bindgen_anon_1.pDeviceFuncs };
    let p_um_callbacks = unsafe { create.__bindgen_anon_2.pUMCallbacks };
    let p_dxgi_base_functions =
        unsafe { create.DXGIBaseDDI.__bindgen_anon_1.pDXGIDDIBaseFunctions };
    // Ground-truth dump: the negotiated Interface decides which funcs-table
    // LAYOUT the runtime reads back, and a misread here silently wires typed
    // 11.1 handlers into slots an 11.0-negotiated device never calls (dwm's
    // singlethreaded devices were observed hitting the UNTYPED shader creates
    // → float32-typed SPIR-V inputs vs SINT vertex data, VUID-Input-08733).
    if trace_enabled() {
        // Bound the dump by the struct being interpreted, not by a literal. The
        // hand copy is 88 bytes (ppfnRetrieveSubObject@80) and that member only
        // exists from minor >= 3, so the runtime's object can be 80 bytes; the
        // old `0..12` read bytes 0..96, which is 8 past the largest possible
        // layout and 16 past the smallest — an access violation inside the
        // caller's D3D11CreateDevice if the arg sits at the end of a page, or a
        // garbage dump that reads as real ABI evidence.
        //
        // Words 0..9 cover every field this code actually interprets: hRTDevice,
        // interface/version, pKTCallbacks, pDeviceFuncs, hDrvDevice, the 16-byte
        // DXGIBaseDDI, hRTCoreLayer, pUMCallbacks, flags. Word 10 is read only
        // when the negotiated interface says it is there, keyed on the same
        // closed set R405 introduced; an unknown interface reads the short shape.
        let words = match NegotiatedInterface::from_interface(create.Interface) {
            Some(NegotiatedInterface::D3D11_1)
            | Some(NegotiatedInterface::Wddm1_3)
            | Some(NegotiatedInterface::Wddm2_1) => 11,
            Some(NegotiatedInterface::D3D11_0) | None => 10,
        };
        let q = args as *const u64;
        let mut raw = String::from("CreateDevice raw args:");
        for i in 0..words {
            raw.push_str(&format!(" [{}]=0x{:016x}", i, unsafe {
                q.add(i).read_unaligned()
            }));
        }
        trace_line!("{raw}");
    }
    log_error!(
        "CreateDevice interface=0x{:08x} version=0x{:08x} flags=0x{:08x} \
         pDeviceFuncs={:p} hDrvDevice={:p} pKTCallbacks={:p} pUMCallbacks={:p} pDXGIBaseFuncs={:p}",
        create.Interface,
        create.Version,
        create.Flags,
        p_device_funcs,
        create.hDrvDevice.pDrvPrivate,
        create.pKTCallbacks,
        p_um_callbacks,
        p_dxgi_base_functions,
    );

    // 0) Validate every runtime-supplied pointer BEFORE constructing anything.
    //    Both of these checks used to run after construction: the hDrvDevice one
    //    leaked the whole DXVK/Vulkan device, and the pDeviceFuncs one (which
    //    ran after the device, the in-place HeliosDevice, the runtime context
    //    and the paging queue all existed) leaked a kernel context and a paging
    //    queue per attempt, skipping both destroy_runtime_objects and
    //    drop_in_place. A crash-looping client exhausted them.
    if create.hDrvDevice.pDrvPrivate.is_null() {
        log_error!("  CreateDevice: null hDrvDevice -> E_FAIL");
        return E_FAIL;
    }
    if p_device_funcs.is_null() {
        log_error!("  CreateDevice: null pDeviceFuncs -> E_FAIL");
        return E_FAIL;
    }
    //    The negotiated interface is runtime-supplied too, and it selects the
    //    SHAPE of the table we write. Refuse anything outside the advertised
    //    set rather than defaulting to D3D11.0's 150-slot fill.
    let Some(negotiated) = NegotiatedInterface::from_interface(create.Interface) else {
        log_error!(
            "  CreateDevice: unsupported interface 0x{:08x} (advertised 0x{:08x}/0x{:08x}/0x{:08x}/0x{:08x}) -> E_NOTIMPL",
            create.Interface,
            NegotiatedInterface::WDDM2_1_INTERFACE,
            NegotiatedInterface::WDDM1_3_INTERFACE,
            NegotiatedInterface::D3D11_1_INTERFACE,
            NegotiatedInterface::D3D11_0_INTERFACE,
        );
        return E_NOTIMPL;
    };

    // 1) Bring up the DXVK device on the Helios venus adapter.
    // BridgeDevice::create folds the old is_null() test into construction, so a
    // stored BridgeDevice is always usable. R815.
    // SAFETY: `adapter_state` validated the exact live object and it remains
    // owned by hAdapter until CloseAdapter, after every device is destroyed.
    let adapter = unsafe { &*adapter };
    let luid = adapter.luid as u64;
    log_error!(
        "  CreateDevice: adapter generation={} luid={:08x}:{:08x}",
        adapter.generation,
        (luid >> 32) as u32,
        luid as u32,
    );
    let translator = match DirectTranslator::create(
        adapter.luid,
        helios_protocol::HELIOS_HTS1_MAX_ENDPOINTS_PER_SESSION,
        Some(device_funcs::translator_sync_progress_join),
        Some(device_funcs::translator_sync_progress_query),
    ) {
        Ok(translator) => translator,
        Err(error) => {
            log_error!(
                "  CreateDevice: A5 translator creation refused: {:?}",
                error
            );
            return E_FAIL;
        }
    };
    let mut outer = Box::new(device_funcs::OuterDevice {
        translator,
        context: None,
        allocations: std::sync::Mutex::new(device_funcs::OuterAllocationSet::new()),
        h_rt_device: create.hRTDevice.handle,
        kt_callbacks: create.pKTCallbacks,
        paging_queue: None,
        device_lost: std::sync::atomic::AtomicU32::new(0),
    });
    let outer_context = outer.as_mut() as *mut device_funcs::OuterDevice as usize;
    // Snapshot every A5 pointer before entering C++. The bridge synchronously
    // calls `dxvk_outer_device_admit` after vkCreateDevice has registered the
    // exact queues; no Rust borrow of `outer.translator` may remain live across
    // that callback's unique access to the boxed outer object.
    let vk_instance = outer.translator.vk_instance() as usize;
    let get_instance_proc_addr = outer.translator.get_instance_proc_addr() as usize;
    let icd_module_base = outer.translator.module_base() as usize;
    let Some(dxvk) = bridge::BridgeDevice::create(
        vk_instance,
        get_instance_proc_addr,
        icd_module_base,
        luid as u32,
        (luid >> 32) as u32 as i32,
        outer_context,
        device_funcs::dxvk_outer_device_admit as *const () as usize,
        device_funcs::dxvk_outer_submit_begin as *const () as usize,
        device_funcs::dxvk_outer_submit_finish as *const () as usize,
        device_funcs::dxvk_outer_submit_join as *const () as usize,
        device_funcs::dxvk_outer_allocation_create as *const () as usize,
        device_funcs::dxvk_outer_allocation_teardown_begin as *const () as usize,
        device_funcs::dxvk_outer_allocation_retire as *const () as usize,
    ) else {
        log_error!("  CreateDevice: DXVK device creation FAILED -> E_FAIL");
        unsafe { device_funcs::destroy_outer_runtime_context(&mut outer) };
        unsafe { device_funcs::destroy_runtime_paging_queue(&mut outer) };
        return E_FAIL;
    };
    if outer.context.is_none() || outer.paging_queue.is_none() {
        log_error!("  CreateDevice: DXVK returned without complete outer admission");
        let mut dxvk = dxvk;
        dxvk.shutdown();
        unsafe { device_funcs::destroy_outer_runtime_context(&mut outer) };
        unsafe { device_funcs::destroy_runtime_paging_queue(&mut outer) };
        return E_FAIL;
    }

    // 2) Construct our device object in the runtime-allocated private memory
    //    (size came from CalcPrivateDeviceSize). hDrvDevice IS that pointer.
    unsafe {
        core::ptr::write(
            create.hDrvDevice.pDrvPrivate as *mut device_funcs::HeliosDevice,
            device_funcs::HeliosDevice {
                // The discrimination word every HDEVICE resolver reads before
                // casting (a deferred context shares the handle namespace).
                tag: device_funcs::HELIOS_TAG_DEVICE,
                // One grouped field, constructed (and declared) before `dxvk`
                // so every bridge-derived COM object it holds outlives the
                // bridge device rather than three of the four dropping after
                // it. R807.
                owned: device_funcs::BridgeOwned::new(),
                dxvk,
                outer,
                h_rt_device: create.hRTDevice.handle,
                // Both of these arrive already typed from the bindgen struct;
                // the hand copy declared them as bare `c_void` pointers and had
                // to cast at this site.
                kt_callbacks: create.pKTCallbacks,
                dxgi_callbacks: create.DXGIBaseDDI.pDXGIBaseCallbacks,
                h_rt_core_layer: create.hRTCoreLayer.handle,
                um_callbacks: p_um_callbacks.cast(),
                negotiated,
            },
        );
    }

    // From here on every early return must tear down. The guard does it, so it
    // is not something each new failure arm has to remember.
    let guard = DeviceUnderConstruction {
        dev: create.hDrvDevice.pDrvPrivate as *mut device_funcs::HeliosDevice,
    };

    // 3) Fill the device-funcs table (Interface == D3D11_0 -> p11DeviceFuncs) and
    //    the DXGI base DDI table the runtime handed us.
    log_error!(
        "  CreateDevice: filling {} device-funcs table",
        negotiated.name()
    );
    // Each arm now names the union member the negotiated interface selects,
    // instead of casting one `*mut c_void` to a different table type per arm.
    // The pointer value is the same either way (all members alias at offset 0);
    // what changes is that the member name, the fill function and the interface
    // are readable as one triple, which is the R802 defect -- an editor could
    // previously pair `D3D11_1` with `fill_d3d11_device_funcs` and the cast
    // would still compile.
    unsafe {
        match negotiated {
            NegotiatedInterface::Wddm2_1 => {
                device_funcs::fill_wddm2_1_device_funcs(
                    create.__bindgen_anon_1.pWDDM2_1DeviceFuncs,
                );
                device_funcs::fill_dxgi_1_3_base_funcs(
                    create.DXGIBaseDDI.__bindgen_anon_1.pDXGIDDIBaseFunctions4,
                );
            }
            NegotiatedInterface::Wddm1_3 => {
                device_funcs::fill_wddm1_3_device_funcs(
                    create.__bindgen_anon_1.pWDDM1_3DeviceFuncs,
                );
                device_funcs::fill_dxgi_1_3_base_funcs(
                    create.DXGIBaseDDI.__bindgen_anon_1.pDXGIDDIBaseFunctions4,
                );
            }
            NegotiatedInterface::D3D11_1 => {
                device_funcs::fill_d3d11_1_device_funcs(create.__bindgen_anon_1.p11_1DeviceFuncs);
                device_funcs::fill_dxgi_1_1_base_funcs(
                    create.DXGIBaseDDI.__bindgen_anon_1.pDXGIDDIBaseFunctions2,
                );
            }
            NegotiatedInterface::D3D11_0 => {
                device_funcs::fill_d3d11_device_funcs(create.__bindgen_anon_1.p11DeviceFuncs);
                device_funcs::fill_dxgi_base_funcs(
                    create.DXGIBaseDDI.__bindgen_anon_1.pDXGIDDIBaseFunctions,
                );
            }
        }
    }

    // The device is handed to the runtime from here; it owns teardown through
    // DestroyDevice.
    guard.defuse();
    if std::env::var_os("HELIOS_DXGI_NO_REDIRECTION").is_some() {
        log_error!(
            "  CreateDevice -> DXGI_STATUS_NO_REDIRECTION (env-gated; DXGI desktop fallback)"
        );
        DXGI_STATUS_NO_REDIRECTION
    } else {
        log_error!("  CreateDevice -> S_OK (DXVK device + D3D11 funcs table installed)");
        S_OK
    }
}

/// Owns the in-place-constructed `HeliosDevice` for the rest of `CreateDevice`.
/// Any early return after construction tears down through `Drop`; the success
/// path calls [`Self::defuse`] immediately before returning to the runtime.
/// The compiler enforces it, rather than each failure arm remembering to —
/// which is exactly what the two hoisted null checks did not do.
///
/// Teardown order matches the paging-queue rollback it replaces:
/// `destroy_runtime_objects` (kernel context + paging queue, through the
/// runtime callbacks) first, then `drop_in_place` (the DXVK device and the
/// Rust-owned fields).
struct DeviceUnderConstruction {
    dev: *mut device_funcs::HeliosDevice,
}

impl DeviceUnderConstruction {
    fn defuse(mut self) {
        self.dev = core::ptr::null_mut();
    }
}

impl Drop for DeviceUnderConstruction {
    fn drop(&mut self) {
        if self.dev.is_null() {
            return;
        }
        // SAFETY: `dev` points at the runtime-owned private block this function
        // wrote a `HeliosDevice` into with `core::ptr::write`, and it has not
        // been dropped. The guard is the only owner while it is alive — the
        // runtime does not see the handle until `defuse()` runs — so no other
        // reference exists during teardown.
        unsafe {
            // The CreateDevice rollback path is the second place BridgeOwned
            // must be released explicitly (R807): reaching it means the bridge
            // device exists but the runtime never saw the handle, and letting
            // the refs go out with `drop_in_place` would put them back on the
            // drop order this type exists to stop depending on.
            let (variants, layouts) = (*self.dev).owned.release();
            log_error!(
                "CreateDevice rollback: released IA cache variants={} layouts={}",
                variants,
                layouts
            );
            (*self.dev).dxvk.shutdown();
            device_funcs::destroy_runtime_objects(&mut *self.dev);
            core::ptr::drop_in_place(self.dev);
        }
    }
}

unsafe extern "C" fn close_adapter(h_adapter: ddi::D3D10DDI_HADAPTER) -> Hresult {
    let Some(state) = (unsafe { adapter_state(h_adapter) }) else {
        return E_FAIL;
    };
    // SAFETY: CloseAdapter consumes the driver handle exactly once. Invalidate
    // before reclaiming so storage reuse cannot preserve a live association.
    unsafe {
        (*state).magic = 0;
        drop(Box::from_raw(state));
    }
    log_error!("CloseAdapter");
    S_OK
}

unsafe extern "C" fn get_supported_versions(
    _h_adapter: ddi::D3D10DDI_HADAPTER,
    entries: *mut u32,
    supported_versions: *mut u64,
) -> Hresult {
    if entries.is_null() {
        log_error!("GetSupportedVersions: null entries -> E_NOTIMPL");
        return E_NOTIMPL;
    }

    let requested_entries = unsafe { *entries };
    log_error!(
        "GetSupportedVersions requested={requested_entries} bufNull={} (advertising {:#018x?})",
        supported_versions.is_null(),
        SUPPORTED_DDI_VERSIONS,
    );
    unsafe { *entries = SUPPORTED_DDI_VERSIONS.len() as u32 };

    if supported_versions.is_null() {
        return S_OK;
    }

    if requested_entries < SUPPORTED_DDI_VERSIONS.len() as u32 {
        return E_OUTOFMEMORY;
    }

    for (index, version) in SUPPORTED_DDI_VERSIONS.iter().enumerate() {
        unsafe { *supported_versions.add(index) = *version };
    }
    S_OK
}
