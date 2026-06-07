//! VidPN (Video Present Network) mode-management for the DOD.
//!
//! The display-config DDIs dxgkrnl drives to negotiate + commit a display mode:
//! `IsSupportedVidPn`, `EnumVidPnCofuncModality`, `CommitVidPn`,
//! `RecommendMonitorModes`. Ported from the Microsoft KMDOD sample
//! (`video/KMDOD/bdd_dmm.cxx`) — the canonical reference for the VidPN calls a
//! display-only driver must make so a cofunctional mode commits and
//! `DxgkDdiPresentDisplayOnly` starts being called. Without these (the prior
//! SUCCESS-returning stubs) no mode commits and the desktop never paints.
//!
//! The dod.rs DDI thunks are thin wrappers over the `*_impl` fns here. First
//! light offers a single 1024x768 BGRA mode (a guaranteed-pairable
//! source/target/monitor triple); expand `MODE_TABLE` once the desktop appears.

use core::ffi::c_void;

use crate::adapter::AdapterContext;
use crate::dxgk::*;

/// Modes offered (width, height), all 32bpp BGRA (`D3DDDIFMT_A8R8G8B8`). The SAME set
/// is published on source/target/monitor so dxgkrnl can always pair them by resolution.
/// Index 0 is the PREFERRED mode (must match the StartDevice scanout default so the
/// first commit lands on it); the rest are alternatives dxgkrnl can switch to —
/// offering only one mode left the post-present re-negotiation unable to converge.
/// All resolutions use `video_signal_info`'s self-consistent 60 Hz timing.
const MODE_TABLE: &[(u32, u32)] = &[
    (1024, 768),  // preferred (== StartDevice DEFAULT_WIDTH/HEIGHT)
    (800, 600),
    (640, 480),
    (1280, 720),
    (1280, 1024),
    (1920, 1080),
];

/// Preference for the target/monitor mode at `idx`: index 0 is the single PREFERRED
/// mode, all others NOTPREFERRED. (Marking EVERY mode PREFERRED is invalid once more
/// than one mode is offered and confuses dxgkrnl's mode pinning.)
#[inline]
fn mode_preference(idx: usize) -> _D3DKMDT_MODE_PREFERENCE::Type {
    if idx == 0 {
        _D3DKMDT_MODE_PREFERENCE::D3DKMDT_MP_PREFERRED
    } else {
        _D3DKMDT_MODE_PREFERENCE::D3DKMDT_MP_NOTPREFERRED
    }
}

// ── Graphics status codes bindgen dropped (C #defines). NTSTATUS = i32. ──────
const STATUS_GRAPHICS_NO_MORE_ELEMENTS_IN_DATASET: NTSTATUS = 0x401E_034Cu32 as NTSTATUS;
const STATUS_GRAPHICS_VIDPN_MODALITY_NOT_SUPPORTED: NTSTATUS = 0xC01E_0306u32 as NTSTATUS;

#[inline]
fn ok(st: NTSTATUS) -> bool {
    st >= 0
}

fn invalid_source_mode_reason(mode: &_D3DKMDT_VIDPN_SOURCE_MODE) -> u32 {
    if mode.Type != _D3DKMDT_VIDPN_SOURCE_MODE_TYPE::D3DKMDT_RMT_GRAPHICS {
        return 0x01;
    }
    let g = unsafe { &mode.Format.Graphics };
    if g.ColorBasis != _D3DKMDT_COLOR_BASIS::D3DKMDT_CB_SCRGB
        && g.ColorBasis != _D3DKMDT_COLOR_BASIS::D3DKMDT_CB_UNINITIALIZED
    {
        return 0x02;
    }
    if g.PixelValueAccessMode != _D3DKMDT_PIXEL_VALUE_ACCESS_MODE::D3DKMDT_PVAM_DIRECT {
        return 0x03;
    }
    if g.PixelFormat != _D3DDDIFORMAT::D3DDDIFMT_A8R8G8B8 {
        return 0x04;
    }
    0
}

fn valid_source_mode(mode: &_D3DKMDT_VIDPN_SOURCE_MODE) -> bool {
    invalid_source_mode_reason(mode) == 0
}

fn invalid_present_path_reason(
    path: &_D3DKMDT_VIDPN_PRESENT_PATH,
    allow_unpinned: bool,
) -> u32 {
    invalid_present_path_reason_ex(path, allow_unpinned, allow_unpinned)
}

fn invalid_present_path_reason_ex(
    path: &_D3DKMDT_VIDPN_PRESENT_PATH,
    allow_unpinned_scaling: bool,
    allow_unpinned_rotation: bool,
) -> u32 {
    if path.VidPnSourceId >= crate::dod::MAX_VIEWS || path.VidPnTargetId >= crate::dod::MAX_CHILDREN {
        return 0x10 | ((path.VidPnSourceId & 0xF) << 4) | (path.VidPnTargetId & 0xF);
    }
    let scaling = path.ContentTransformation.Scaling;
    if scaling != _D3DKMDT_VIDPN_PRESENT_PATH_SCALING::D3DKMDT_VPPS_IDENTITY
        && scaling != _D3DKMDT_VIDPN_PRESENT_PATH_SCALING::D3DKMDT_VPPS_CENTERED
        && scaling != _D3DKMDT_VIDPN_PRESENT_PATH_SCALING::D3DKMDT_VPPS_NOTSPECIFIED
        && scaling != _D3DKMDT_VIDPN_PRESENT_PATH_SCALING::D3DKMDT_VPPS_UNINITIALIZED
        && !(allow_unpinned_scaling
            && scaling == _D3DKMDT_VIDPN_PRESENT_PATH_SCALING::D3DKMDT_VPPS_UNPINNED)
    {
        return 0x200 | (scaling as u32 & 0xFF);
    }
    let rotation = path.ContentTransformation.Rotation;
    if rotation != _D3DKMDT_VIDPN_PRESENT_PATH_ROTATION::D3DKMDT_VPPR_IDENTITY
        && rotation != _D3DKMDT_VIDPN_PRESENT_PATH_ROTATION::D3DKMDT_VPPR_ROTATE90
        && rotation != _D3DKMDT_VIDPN_PRESENT_PATH_ROTATION::D3DKMDT_VPPR_NOTSPECIFIED
        && rotation != _D3DKMDT_VIDPN_PRESENT_PATH_ROTATION::D3DKMDT_VPPR_UNINITIALIZED
        && !(allow_unpinned_rotation
            && rotation == _D3DKMDT_VIDPN_PRESENT_PATH_ROTATION::D3DKMDT_VPPR_UNPINNED)
    {
        return 0x300 | (rotation as u32 & 0xFF);
    }
    if path.VidPnTargetColorBasis != _D3DKMDT_COLOR_BASIS::D3DKMDT_CB_SCRGB
        && path.VidPnTargetColorBasis != _D3DKMDT_COLOR_BASIS::D3DKMDT_CB_UNINITIALIZED
    {
        return 0x400 | (path.VidPnTargetColorBasis as u32 & 0xFF);
    }
    0
}

fn valid_present_path(path: &_D3DKMDT_VIDPN_PRESENT_PATH, allow_unpinned: bool) -> bool {
    invalid_present_path_reason(path, allow_unpinned) == 0
}

pub(crate) fn active_present_path_reason(path: &_D3DKMDT_VIDPN_PRESENT_PATH) -> u32 {
    invalid_present_path_reason(path, false)
}

/// Fetch the `DXGK_VIDPN_INTERFACE` for a VidPN handle via the saved Dxgkrnl
/// callback.
unsafe fn query_vidpn_interface(
    adapter: &AdapterContext,
    h_vidpn: D3DKMDT_HVIDPN,
) -> Result<*const DXGK_VIDPN_INTERFACE, NTSTATUS> {
    let dxgkrnl = adapter.dxgkrnl().map_err(|e| e.into_ntstatus())?;
    let cb = dxgkrnl.DxgkCbQueryVidPnInterface.ok_or(STATUS_INVALID_PARAMETER)?;
    let mut iface: *const DXGK_VIDPN_INTERFACE = core::ptr::null();
    // SAFETY: callback valid for the device lifetime; h_vidpn is dxgkrnl's; iface
    // is a valid out-pointer.
    let st = unsafe {
        cb(
            h_vidpn,
            _DXGK_VIDPN_INTERFACE_VERSION::DXGK_VIDPN_INTERFACE_VERSION_V1,
            &mut iface,
        )
    };
    if !ok(st) {
        return Err(st);
    }
    if iface.is_null() {
        return Err(STATUS_INVALID_PARAMETER);
    }
    Ok(iface)
}

/// Build a `D3DKMDT_VIDEO_SIGNAL_INFO` with CONCRETE, self-consistent 60 Hz timing.
///
/// VioGpuDod's `BuildVideoSignalInfo` uses `D3DKMDT_FREQUENCY_NOTSPECIFIED` (0) freqs,
/// but that is its VGA path (modes from the POST framebuffer). On our NON-VGA
/// virtio-gpu-pci adapter on win11 build 26100, `pfnAddMode(target)` REJECTS
/// NOTSPECIFIED with `STATUS_GRAPHICS_INVALID_FREQUENCY (0xC01E030A)` — reproduced
/// with ActiveSize both 0 and (w,h). Concrete timing is accepted. The blanking
/// fractions reproduce the DMT 1024x768@60 raster; the three derived fields are
/// mutually consistent (PixelRate = HTotal·VTotal·VSync, HSync = VTotal·VSync). Target
/// and monitor modes use this same builder so their VideoSignalInfo are identical.
unsafe fn video_signal_info(w: u32, h: u32) -> D3DKMDT_VIDEO_SIGNAL_INFO {
    const REFRESH_HZ: u32 = 60;
    let h_total = w + (w * 5 / 16); // ~31% horizontal blanking
    let v_total = h + (h / 20); //     ~5% vertical blanking
    let mut sig: D3DKMDT_VIDEO_SIGNAL_INFO = unsafe { core::mem::zeroed() };
    sig.VideoStandard = _D3DKMDT_VIDEO_SIGNAL_STANDARD::D3DKMDT_VSS_OTHER;
    sig.TotalSize.cx = h_total;
    sig.TotalSize.cy = v_total;
    sig.ActiveSize.cx = w;
    sig.ActiveSize.cy = h;
    sig.VSyncFreq.Numerator = REFRESH_HZ;
    sig.VSyncFreq.Denominator = 1;
    sig.HSyncFreq.Numerator = v_total.saturating_mul(REFRESH_HZ);
    sig.HSyncFreq.Denominator = 1;
    sig.PixelRate = (h_total as SIZE_T) * (v_total as SIZE_T) * (REFRESH_HZ as SIZE_T);
    sig.__bindgen_anon_1.ScanLineOrdering =
        _D3DDDI_VIDEO_SIGNAL_SCANLINE_ORDERING::D3DDDI_VSSLO_PROGRESSIVE;
    sig
}

// ── IsSupportedVidPn ────────────────────────────────────────────────────────

/// Validate a desired VidPN (ported from KMDOD `bdd_dmm.cxx` IsSupportedVidPn).
/// Rejection is by setting the bool FALSE + returning SUCCESS, never a failure status.
///
/// We do NOT blanket-accept: dxgkrnl re-checks the committed VidPN via this DDI right
/// after the first present, and falsely reporting "supported" for a VidPN we cannot
/// realize is a contract violation that makes dxgkrnl give up and tear the adapter
/// down (post-present Code 43). KMDOD's check: a null desired VidPN is supported; any
/// source must not have more paths than we have children (MAX_CHILDREN == 1).
pub unsafe fn is_supported_vidpn(
    adapter: &AdapterContext,
    arg: *mut _DXGKARG_ISSUPPORTEDVIDPN,
) -> NTSTATUS {
    if arg.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    let arg = unsafe { &mut *arg };
    // A null desired VidPN is supported.
    if arg.hDesiredVidPn.is_null() {
        arg.IsVidPnSupported = 1;
        return STATUS_SUCCESS;
    }
    // Default to NOT supported until shown otherwise.
    arg.IsVidPnSupported = 0;
    let iface = match unsafe { query_vidpn_interface(adapter, arg.hDesiredVidPn) } {
        Ok(i) => i,
        Err(e) => return e,
    };
    let iface = unsafe { &*iface };
    let Some(get_topology) = iface.pfnGetTopology else {
        return STATUS_INVALID_PARAMETER;
    };
    let mut h_topo: D3DKMDT_HVIDPNTOPOLOGY = unsafe { core::mem::zeroed() };
    let mut p_topo: *const DXGK_VIDPNTOPOLOGY_INTERFACE = core::ptr::null();
    let st = unsafe { get_topology(arg.hDesiredVidPn, &mut h_topo, &mut p_topo) };
    if !ok(st) {
        return st;
    }
    if p_topo.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    let topo = unsafe { &*p_topo };
    if let Some(get_num) = topo.pfnGetNumPaths {
        let mut num_paths: SIZE_T = 0;
        let s = unsafe { get_num(h_topo, &mut num_paths) };
        if ok(s) && num_paths == 0 && adapter.framebuffer_active() {
            crate::diag::record_supp(0x0700_0700);
            return STATUS_SUCCESS; // leave IsVidPnSupported = 0
        }
    }

    if let (Some(first), Some(next), Some(release_path)) = (
        topo.pfnAcquireFirstPathInfo,
        topo.pfnAcquireNextPathInfo,
        topo.pfnReleasePathInfo,
    ) {
        // DMM probes can contain unpinned transform fields even after a framebuffer
        // is active. VioGpuDod does not reject those in IsSupportedVidPn; it reports
        // support in EnumVidPnCofuncModality and validates the realized pinned path
        // in CommitVidPn. Rejecting the probe here makes dxgkrnl blank the source.
        let mut p_path: *const _D3DKMDT_VIDPN_PRESENT_PATH = core::ptr::null();
        let mut st = unsafe { first(h_topo, &mut p_path) };
        while ok(st) && st != STATUS_GRAPHICS_NO_MORE_ELEMENTS_IN_DATASET && !p_path.is_null() {
            let path = unsafe { &*p_path };
            let path_reason = invalid_present_path_reason_ex(path, true, true);
            if path_reason != 0 {
                crate::diag::record_supp(0x0700_0000 | path_reason);
                unsafe { release_path(h_topo, p_path) };
                return STATUS_SUCCESS; // leave IsVidPnSupported = 0
            }

            // Our single source must not drive more paths than we have children.
            if let Some(get_num_from_src) = topo.pfnGetNumPathsFromSource {
                let mut num: SIZE_T = 0;
                let s = unsafe { get_num_from_src(h_topo, path.VidPnSourceId, &mut num) };
                if ok(s) && num > crate::dod::MAX_CHILDREN as SIZE_T {
                    crate::diag::record_supp(0x0700_0500 | (num as u32 & 0xFF));
                    unsafe { release_path(h_topo, p_path) };
                    return STATUS_SUCCESS; // leave IsVidPnSupported = 0
                }
            }

            if let (Some(acq_s), Some(rel_s)) =
                (iface.pfnAcquireSourceModeSet, iface.pfnReleaseSourceModeSet)
            {
                let mut h_ss: D3DKMDT_HVIDPNSOURCEMODESET = unsafe { core::mem::zeroed() };
                let mut p_ss: *const DXGK_VIDPNSOURCEMODESET_INTERFACE = core::ptr::null();
                if ok(unsafe { acq_s(arg.hDesiredVidPn, path.VidPnSourceId, &mut h_ss, &mut p_ss) })
                    && !p_ss.is_null()
                {
                    let ssi = unsafe { &*p_ss };
                    let mut pinned: *const _D3DKMDT_VIDPN_SOURCE_MODE = core::ptr::null();
                    if let Some(acq_pin) = ssi.pfnAcquirePinnedModeInfo {
                        unsafe { acq_pin(h_ss, &mut pinned) };
                    }
                    if !pinned.is_null() {
                        let src_reason = invalid_source_mode_reason(unsafe { &*pinned });
                        if let Some(rm) = ssi.pfnReleaseModeInfo {
                            unsafe { rm(h_ss, pinned) };
                        }
                        unsafe { rel_s(arg.hDesiredVidPn, h_ss) };
                        if src_reason != 0 {
                            crate::diag::record_supp(0x0700_0600 | src_reason);
                            unsafe { release_path(h_topo, p_path) };
                            return STATUS_SUCCESS; // leave IsVidPnSupported = 0
                        }
                    } else {
                        unsafe { rel_s(arg.hDesiredVidPn, h_ss) };
                    }
                }
            }

            let mut p_next: *const _D3DKMDT_VIDPN_PRESENT_PATH = core::ptr::null();
            st = unsafe { next(h_topo, p_path, &mut p_next) };
            unsafe { release_path(h_topo, p_path) };
            p_path = p_next;
        }
    } else {
        return STATUS_INVALID_PARAMETER;
    }

    crate::diag::record_supp(0x0700_00FF);
    arg.IsVidPnSupported = 1;
    STATUS_SUCCESS
}

// ── EnumVidPnCofuncModality ─────────────────────────────────────────────────

/// Add our source mode(s) to a (newly created) source mode set.
unsafe fn add_single_source_mode(
    iface: &DXGK_VIDPNSOURCEMODESET_INTERFACE,
    h_set: D3DKMDT_HVIDPNSOURCEMODESET,
) {
    let (Some(create), Some(add), Some(release)) =
        (iface.pfnCreateNewModeInfo, iface.pfnAddMode, iface.pfnReleaseModeInfo)
    else {
        return;
    };
    for &(w, h) in MODE_TABLE {
        let mut p_mode: *mut _D3DKMDT_VIDPN_SOURCE_MODE = core::ptr::null_mut();
        // SAFETY: dxgkrnl allocates the mode; out-pointer valid.
        if !ok(unsafe { create(h_set, &mut p_mode) }) || p_mode.is_null() {
            continue;
        }
        // SAFETY: p_mode points to a fresh source-mode (Id pre-set by dxgkrnl).
        let m = unsafe { &mut *p_mode };
        m.Type = _D3DKMDT_VIDPN_SOURCE_MODE_TYPE::D3DKMDT_RMT_GRAPHICS;
        let g = unsafe { &mut m.Format.Graphics };
        g.PrimSurfSize.cx = w;
        g.PrimSurfSize.cy = h;
        g.VisibleRegionSize = g.PrimSurfSize;
        g.Stride = w * 4;
        g.PixelFormat = _D3DDDIFORMAT::D3DDDIFMT_A8R8G8B8;
        g.ColorBasis = _D3DKMDT_COLOR_BASIS::D3DKMDT_CB_SCRGB;
        g.PixelValueAccessMode = _D3DKMDT_PIXEL_VALUE_ACCESS_MODE::D3DKMDT_PVAM_DIRECT;
        // SAFETY: add takes ownership-by-const-ptr of the populated mode.
        let st = unsafe { add(h_set, p_mode) };
        if !ok(st) {
            unsafe { release(h_set, p_mode as *const _) };
        }
    }
}

/// Add our target mode(s) to a (newly created) target mode set. `is_nopivot` selects
/// which breadcrumb records the `pfnAddMode` status (HeliosTAnp vs HeliosTAdd) so the
/// no-pivot enum's result is visible separately from the source/target-pivot enums.
unsafe fn add_single_target_mode(
    iface: &DXGK_VIDPNTARGETMODESET_INTERFACE,
    h_set: D3DKMDT_HVIDPNTARGETMODESET,
    is_nopivot: bool,
) {
    let (Some(create), Some(add), Some(release)) =
        (iface.pfnCreateNewModeInfo, iface.pfnAddMode, iface.pfnReleaseModeInfo)
    else {
        return;
    };
    // VioGpuDod effectively publishes the current/preferred target timing for the
    // single target. Keep source/monitor modes broad, but avoid making the target
    // timing set another dimension for DMM to keep pivoting through post-present.
    for (idx, &(w, h)) in MODE_TABLE.iter().take(1).enumerate() {
        let mut p_mode: *mut _D3DKMDT_VIDPN_TARGET_MODE = core::ptr::null_mut();
        if !ok(unsafe { create(h_set, &mut p_mode) }) || p_mode.is_null() {
            continue;
        }
        let m = unsafe { &mut *p_mode };
        m.VideoSignalInfo = unsafe { video_signal_info(w, h) };
        // Preference MUST be set (a fresh mode has MP_UNINITIALIZED(0), which pfnAddMode
        // rejects → empty modeset → non-cofunctional). Exactly one mode (idx 0) is
        // PREFERRED, the rest NOTPREFERRED. (Bitfield in the nested anon union of
        // _D3DKMDT_VIDPN_TARGET_MODE.)
        unsafe {
            m.__bindgen_anon_1
                .__bindgen_anon_1
                .set_Preference(mode_preference(idx))
        };
        let st = unsafe { add(h_set, p_mode) };
        if is_nopivot {
            crate::diag::record_tadd_np(st); // no-pivot enum target add
        } else {
            crate::diag::record_tadd(st); // 0 = pfnAddMode(target) succeeded
        }
        if !ok(st) {
            unsafe { release(h_set, p_mode as *const _) };
        }
    }
}

/// `DxgkDdiEnumVidPnCofuncModality` — for every present path with an unpinned
/// source/target mode set, create + assign our mode set, and advertise
/// identity/centered scaling + identity rotation. Honors the source/target pivot.
pub unsafe fn enum_vidpn_cofunc_modality(
    adapter: &AdapterContext,
    arg: *const _DXGKARG_ENUMVIDPNCOFUNCMODALITY,
) -> NTSTATUS {
    if arg.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    let arg = unsafe { &*arg };
    let h_vidpn = arg.hConstrainingVidPn;
    let iface = match unsafe { query_vidpn_interface(adapter, h_vidpn) } {
        Ok(i) => i,
        Err(e) => return e,
    };
    let iface = unsafe { &*iface };
    let Some(get_topology) = iface.pfnGetTopology else {
        return STATUS_INVALID_PARAMETER;
    };
    let mut h_topo: D3DKMDT_HVIDPNTOPOLOGY = unsafe { core::mem::zeroed() };
    let mut p_topo: *const DXGK_VIDPNTOPOLOGY_INTERFACE = core::ptr::null();
    if !ok(unsafe { get_topology(h_vidpn, &mut h_topo, &mut p_topo) }) || p_topo.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    let topo = unsafe { &*p_topo };
    let (Some(first), Some(next), Some(release_path)) =
        (topo.pfnAcquireFirstPathInfo, topo.pfnAcquireNextPathInfo, topo.pfnReleasePathInfo)
    else {
        return STATUS_INVALID_PARAMETER;
    };

    // Diagnostics → HeliosEnum = 0x0800_0000 | (pivotType<<20) | flags | paths:
    //   paths bits[0..8]; 0x100 src-assigned; 0x200 tgt-assigned;
    //   0x400 src-already-pinned; 0x800 tgt-already-pinned;
    //   0x4000 src-pivot; 0x8000 tgt-pivot. (record_enum is single-write/no-atomic.)
    let mut paths: u32 = 0;
    let mut flags: u32 = 0;
    let mut any_modified = false; // any path's support info updated this enum
    let pivot_type = arg.EnumPivotType as u32 & 0xF;
    // A "no-pivot" enum is the cofunctional enumeration where dxgkrnl has pinned
    // nothing (UNINITIALIZED=0) or explicitly NOPIVOT=5 — the pass that should build
    // the full source+target mode sets. 1=SOURCE 2=TARGET 3=SCALING 4=ROTATION pivots.
    let is_nopivot = pivot_type == 0 || pivot_type == 5;
    let mut p_path: *const _D3DKMDT_VIDPN_PRESENT_PATH = core::ptr::null();
    let mut st = unsafe { first(h_topo, &mut p_path) };
    while ok(st) && st != STATUS_GRAPHICS_NO_MORE_ELEMENTS_IN_DATASET && !p_path.is_null() {
        paths = paths.saturating_add(1);
        let path = unsafe { &*p_path };
        let source_id = path.VidPnSourceId;
        let target_id = path.VidPnTargetId;

        // ── Diagnostic: pinned source resolution (if any) → HeliosSrcRes ─────
        // KMDOD sizes the target mode from the pinned source; if dxgkrnl pinned a
        // source at a resolution != our fixed 1024x768, our target assign fails.
        if let (Some(acq_s), Some(rel_s)) =
            (iface.pfnAcquireSourceModeSet, iface.pfnReleaseSourceModeSet)
        {
            let mut h_ss: D3DKMDT_HVIDPNSOURCEMODESET = unsafe { core::mem::zeroed() };
            let mut p_ss: *const DXGK_VIDPNSOURCEMODESET_INTERFACE = core::ptr::null();
            if ok(unsafe { acq_s(h_vidpn, source_id, &mut h_ss, &mut p_ss) }) && !p_ss.is_null() {
                let ssi = unsafe { &*p_ss };
                let mut pin: *const _D3DKMDT_VIDPN_SOURCE_MODE = core::ptr::null();
                if let Some(ap) = ssi.pfnAcquirePinnedModeInfo {
                    unsafe { ap(h_ss, &mut pin) };
                }
                if !pin.is_null() {
                    let g = unsafe { &(*pin).Format.Graphics };
                    crate::diag::record_sres((g.PrimSurfSize.cx << 16) | (g.PrimSurfSize.cy & 0xFFFF));
                    if let Some(rm) = ssi.pfnReleaseModeInfo {
                        unsafe { rm(h_ss, pin) };
                    }
                }
                unsafe { rel_s(h_vidpn, h_ss) };
            }
        }

        // ── Source side (skip if pivoting on this source) ───────────────────
        let src_pivot = arg.EnumPivotType
            == _D3DKMDT_ENUMCOFUNCMODALITY_PIVOT_TYPE::D3DKMDT_EPT_VIDPNSOURCE
            && arg.EnumPivot.VidPnSourceId == source_id;
        if src_pivot {
            flags |= 0x4000;
        }
        if !src_pivot {
            if let (Some(acq), Some(create_set), Some(release_set), Some(assign)) = (
                iface.pfnAcquireSourceModeSet,
                iface.pfnCreateNewSourceModeSet,
                iface.pfnReleaseSourceModeSet,
                iface.pfnAssignSourceModeSet,
            ) {
                let mut h_set: D3DKMDT_HVIDPNSOURCEMODESET = unsafe { core::mem::zeroed() };
                let mut p_iface: *const DXGK_VIDPNSOURCEMODESET_INTERFACE = core::ptr::null();
                let acq_st = unsafe { acq(h_vidpn, source_id, &mut h_set, &mut p_iface) };
                if !ok(acq_st) || p_iface.is_null() {
                    crate::diag::record_err(acq_st);
                    return if ok(acq_st) { STATUS_INVALID_PARAMETER } else { acq_st };
                } else {
                    let set_iface = unsafe { &*p_iface };
                    let mut pinned: *const _D3DKMDT_VIDPN_SOURCE_MODE = core::ptr::null();
                    if let Some(acq_pin) = set_iface.pfnAcquirePinnedModeInfo {
                        let pin_st = unsafe { acq_pin(h_set, &mut pinned) };
                        if !ok(pin_st) {
                            crate::diag::record_err(pin_st);
                            unsafe { release_set(h_vidpn, h_set) };
                            return pin_st;
                        }
                    }
                    if pinned.is_null() {
                        // No pinned mode → replace with our mode set.
                        unsafe { release_set(h_vidpn, h_set) };
                        let mut h_new: D3DKMDT_HVIDPNSOURCEMODESET = unsafe { core::mem::zeroed() };
                        let mut p_new: *const DXGK_VIDPNSOURCEMODESET_INTERFACE = core::ptr::null();
                        let cs = unsafe { create_set(h_vidpn, source_id, &mut h_new, &mut p_new) };
                        if ok(cs) && !p_new.is_null() {
                            unsafe { add_single_source_mode(&*p_new, h_new) };
                            let a = unsafe { assign(h_vidpn, source_id, h_new) };
                            if ok(a) {
                                flags |= 0x100;
                            } else {
                                // Release the created-but-unassigned modeset (avoid
                                // UNASSIGNED_MODESET_ALREADY_EXISTS on the next create).
                                unsafe { release_set(h_vidpn, h_new) };
                                crate::diag::record_err(a);
                                return a;
                            }
                        } else if !ok(cs) {
                            crate::diag::record_err(cs);
                            return cs;
                        } else {
                            return STATUS_INVALID_PARAMETER;
                        }
                    } else {
                        flags |= 0x400; // source already pinned (dxgkrnl pivoting)
                        if let Some(rel) = set_iface.pfnReleaseModeInfo {
                            unsafe { rel(h_set, pinned) };
                        }
                        unsafe { release_set(h_vidpn, h_set) };
                    }
                }
            }
        }

        // ── Target side (skip if pivoting on this target) ───────────────────
        let tgt_pivot = arg.EnumPivotType
            == _D3DKMDT_ENUMCOFUNCMODALITY_PIVOT_TYPE::D3DKMDT_EPT_VIDPNTARGET
            && arg.EnumPivot.VidPnTargetId == target_id;
        if tgt_pivot {
            flags |= 0x8000;
        }
        if !tgt_pivot {
            if let (Some(acq), Some(create_set), Some(release_set), Some(assign)) = (
                iface.pfnAcquireTargetModeSet,
                iface.pfnCreateNewTargetModeSet,
                iface.pfnReleaseTargetModeSet,
                iface.pfnAssignTargetModeSet,
            ) {
                let mut h_set: D3DKMDT_HVIDPNTARGETMODESET = unsafe { core::mem::zeroed() };
                let mut p_iface: *const DXGK_VIDPNTARGETMODESET_INTERFACE = core::ptr::null();
                let acq_st = unsafe { acq(h_vidpn, target_id, &mut h_set, &mut p_iface) };
                if !ok(acq_st) || p_iface.is_null() {
                    crate::diag::record_err(acq_st);
                    return if ok(acq_st) { STATUS_INVALID_PARAMETER } else { acq_st };
                } else {
                    let set_iface = unsafe { &*p_iface };
                    let mut pinned: *const _D3DKMDT_VIDPN_TARGET_MODE = core::ptr::null();
                    if let Some(acq_pin) = set_iface.pfnAcquirePinnedModeInfo {
                        let pin_st = unsafe { acq_pin(h_set, &mut pinned) };
                        if !ok(pin_st) {
                            crate::diag::record_err(pin_st);
                            unsafe { release_set(h_vidpn, h_set) };
                            return pin_st;
                        }
                    }
                    if pinned.is_null() {
                        unsafe { release_set(h_vidpn, h_set) };
                        let mut h_new: D3DKMDT_HVIDPNTARGETMODESET = unsafe { core::mem::zeroed() };
                        let mut p_new: *const DXGK_VIDPNTARGETMODESET_INTERFACE = core::ptr::null();
                        let cs = unsafe { create_set(h_vidpn, target_id, &mut h_new, &mut p_new) };
                        if ok(cs) && !p_new.is_null() {
                            unsafe { add_single_target_mode(&*p_new, h_new, is_nopivot) };
                            let a = unsafe { assign(h_vidpn, target_id, h_new) };
                            crate::diag::record_aerr(a);
                            if ok(a) {
                                flags |= 0x200;
                            } else {
                                // Assign failed → release the created-but-unassigned
                                // modeset, else the next create returns
                                // UNASSIGNED_MODESET_ALREADY_EXISTS (0xC01E0350).
                                unsafe { release_set(h_vidpn, h_new) };
                                crate::diag::record_err(a);
                                return a;
                            }
                        } else if !ok(cs) {
                            crate::diag::record_err(cs);
                            return cs;
                        } else {
                            return STATUS_INVALID_PARAMETER;
                        }
                    } else {
                        flags |= 0x800; // target already pinned (dxgkrnl pivoting)
                        if let Some(rel) = set_iface.pfnReleaseModeInfo {
                            unsafe { rel(h_set, pinned) };
                        }
                        unsafe { release_set(h_vidpn, h_set) };
                    }
                }
            }
        }

        // ── Path scaling/rotation support ───────────────────────────────────
        // Scaling follows the normal pivot rule: when dxgkrnl pivots on scaling for
        // this path, it owns that field. Rotation intentionally follows VioGpuDod's
        // actual guard, which *does* publish rotation support on the ROTATION pivot.
        // Without that, DMM can ask for rotation modality, receive no support bits,
        // then leave Rotation UNPINNED; the post-active IsSupportedVidPn reject hides
        // the source. Keep the support identity-only because this blit path does not
        // implement rotated presents.
        let scaling_pivot = arg.EnumPivotType
            == _D3DKMDT_ENUMCOFUNCMODALITY_PIVOT_TYPE::D3DKMDT_EPT_SCALING
            && arg.EnumPivot.VidPnSourceId == source_id
            && arg.EnumPivot.VidPnTargetId == target_id;
        let rotation_pivot = arg.EnumPivotType
            == _D3DKMDT_ENUMCOFUNCMODALITY_PIVOT_TYPE::D3DKMDT_EPT_ROTATION
            && arg.EnumPivot.VidPnSourceId == source_id
            && arg.EnumPivot.VidPnTargetId == target_id;
        // Owned copy (the struct may not be Copy due to unions/arrays). Only call
        // pfnUpdatePathSupportInfo when the support bits actually change; repeated
        // no-op updates keep the post-present cofunctional enumeration dirty.
        let mut local: _D3DKMDT_VIDPN_PRESENT_PATH = unsafe { core::ptr::read(p_path) };
        let mut modified = false;
        if !scaling_pivot
            && local.ContentTransformation.Scaling
                == _D3DKMDT_VIDPN_PRESENT_PATH_SCALING::D3DKMDT_VPPS_UNPINNED
            && (local.ContentTransformation.ScalingSupport.Identity() == 0
                || local.ContentTransformation.ScalingSupport.Centered() == 0)
        {
            local.ContentTransformation.ScalingSupport = unsafe { core::mem::zeroed() };
            local.ContentTransformation.ScalingSupport.set_Identity(1);
            local.ContentTransformation.ScalingSupport.set_Centered(1);
            modified = true;
        }
        if (rotation_pivot
            || arg.EnumPivot.VidPnSourceId != source_id
            || arg.EnumPivot.VidPnTargetId != target_id)
            && local.ContentTransformation.Rotation
                == _D3DKMDT_VIDPN_PRESENT_PATH_ROTATION::D3DKMDT_VPPR_UNPINNED
            && (local.ContentTransformation.RotationSupport.Identity() == 0
                || local.ContentTransformation.RotationSupport.Rotate90() == 0
                || local.ContentTransformation.RotationSupport.Rotate180() != 0
                || local.ContentTransformation.RotationSupport.Rotate270() != 0)
        {
            local.ContentTransformation.RotationSupport = unsafe { core::mem::zeroed() };
            local.ContentTransformation.RotationSupport.set_Identity(1);
            local.ContentTransformation.RotationSupport.set_Rotate90(1);
            modified = true;
        }
        if modified {
            any_modified = true;
            if let Some(update) = topo.pfnUpdatePathSupportInfo {
                unsafe { update(h_topo, &local) };
            }
        }

        // ── Advance ─────────────────────────────────────────────────────────
        let mut p_next: *const _D3DKMDT_VIDPN_PRESENT_PATH = core::ptr::null();
        st = unsafe { next(h_topo, p_path, &mut p_next) };
        unsafe { release_path(h_topo, p_path) };
        p_path = p_next;
    }
    // Rolling pivot log: low 3 bits = pivot type, bit3 = modified-any-path this enum.
    crate::diag::record_piv((pivot_type & 0x7) | ((any_modified as u32) << 3));
    let result = 0x0800_0000 | ((pivot_type & 0xF) << 20) | (flags & 0xFFFF) | (paths & 0xFF);
    if is_nopivot {
        crate::diag::record_enum(result); // no-pivot (UNINIT/NOPIVOT) call → HeliosEnum
    } else {
        crate::diag::record_enum_pivot(result); // source/target/scaling/rotation pivot → HeliosEnumP
    }
    STATUS_SUCCESS
}

// ── CommitVidPn ─────────────────────────────────────────────────────────────

/// `DxgkDdiCommitVidPn` — read the pinned source mode and program the scanout to
/// that resolution (`set_desktop_mode`). This is what makes the committed mode
/// take effect on the device.
pub unsafe fn commit_vidpn(
    adapter: &AdapterContext,
    arg: *const _DXGKARG_COMMITVIDPN,
) -> NTSTATUS {
    if arg.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    let arg = unsafe { &*arg };
    let h_vidpn = arg.hFunctionalVidPn;
    let iface = match unsafe { query_vidpn_interface(adapter, h_vidpn) } {
        Ok(i) => i,
        Err(e) => return e,
    };
    let iface = unsafe { &*iface };
    let (Some(get_topology), Some(acq_src), Some(release_src)) = (
        iface.pfnGetTopology,
        iface.pfnAcquireSourceModeSet,
        iface.pfnReleaseSourceModeSet,
    ) else {
        return STATUS_INVALID_PARAMETER;
    };
    let mut h_topo: D3DKMDT_HVIDPNTOPOLOGY = unsafe { core::mem::zeroed() };
    let mut p_topo: *const DXGK_VIDPNTOPOLOGY_INTERFACE = core::ptr::null();
    if !ok(unsafe { get_topology(h_vidpn, &mut h_topo, &mut p_topo) }) || p_topo.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    let topo = unsafe { &*p_topo };
    let mut num_paths: SIZE_T = 0;
    if let Some(get_num) = topo.pfnGetNumPaths {
        unsafe { get_num(h_topo, &mut num_paths) };
    }
    crate::diag::record_commit(
        0x0900_0000
            | (((num_paths as u32) & 0xFF) << 16)
            | (arg.AffectedVidPnSourceId & 0xFF),
    );
    if num_paths == 0 {
        if adapter.framebuffer_active() {
            return STATUS_GRAPHICS_VIDPN_MODALITY_NOT_SUPPORTED;
        }
        adapter.mark_framebuffer_active(false);
        return STATUS_SUCCESS; // nothing pinned
    }

    let mut committed_rotation =
        _D3DKMDT_VIDPN_PRESENT_PATH_ROTATION::D3DKMDT_VPPR_IDENTITY;
    if let (Some(first), Some(next), Some(release_path)) = (
        topo.pfnAcquireFirstPathInfo,
        topo.pfnAcquireNextPathInfo,
        topo.pfnReleasePathInfo,
    ) {
        let mut p_path: *const _D3DKMDT_VIDPN_PRESENT_PATH = core::ptr::null();
        let mut st = unsafe { first(h_topo, &mut p_path) };
        while ok(st) && st != STATUS_GRAPHICS_NO_MORE_ELEMENTS_IN_DATASET && !p_path.is_null() {
            let path = unsafe { &*p_path };
            if !valid_present_path(path, false) {
                unsafe { release_path(h_topo, p_path) };
                adapter.mark_framebuffer_active(false);
                return STATUS_GRAPHICS_VIDPN_MODALITY_NOT_SUPPORTED;
            }
            committed_rotation = path.ContentTransformation.Rotation;
            let mut p_next: *const _D3DKMDT_VIDPN_PRESENT_PATH = core::ptr::null();
            st = unsafe { next(h_topo, p_path, &mut p_next) };
            unsafe { release_path(h_topo, p_path) };
            p_path = p_next;
        }
    } else {
        adapter.mark_framebuffer_active(false);
        return STATUS_INVALID_PARAMETER;
    }

    let mut h_set: D3DKMDT_HVIDPNSOURCEMODESET = unsafe { core::mem::zeroed() };
    let mut p_iface: *const DXGK_VIDPNSOURCEMODESET_INTERFACE = core::ptr::null();
    if !ok(unsafe { acq_src(h_vidpn, arg.AffectedVidPnSourceId, &mut h_set, &mut p_iface) })
        || p_iface.is_null()
    {
        return STATUS_SUCCESS;
    }
    let set_iface = unsafe { &*p_iface };
    let mut pinned: *const _D3DKMDT_VIDPN_SOURCE_MODE = core::ptr::null();
    if let Some(acq_pin) = set_iface.pfnAcquirePinnedModeInfo {
        unsafe { acq_pin(h_set, &mut pinned) };
    }
    let mut dims: Option<(u32, u32)> = None;
    if !pinned.is_null() {
        if !valid_source_mode(unsafe { &*pinned }) {
            if let Some(rel) = set_iface.pfnReleaseModeInfo {
                unsafe { rel(h_set, pinned) };
            }
            unsafe { release_src(h_vidpn, h_set) };
            adapter.mark_framebuffer_active(false);
            return STATUS_GRAPHICS_VIDPN_MODALITY_NOT_SUPPORTED;
        }
        let g = unsafe { &(*pinned).Format.Graphics };
        dims = Some((g.PrimSurfSize.cx, g.PrimSurfSize.cy));
        if let Some(rel) = set_iface.pfnReleaseModeInfo {
            unsafe { rel(h_set, pinned) };
        }
    }
    unsafe { release_src(h_vidpn, h_set) };

    if let Some((w, h)) = dims {
        // Program the scanout to the committed resolution (PASSIVE_LEVEL).
        // force=TRUE: re-realize the scanout for the committed VidPN on every commit
        // (dxgkrnl tears the DOD down after one present otherwise — see set_desktop_mode).
        let programmed = crate::dod::set_desktop_mode(adapter, w, h, true).is_ok();
        adapter.mark_framebuffer_active(programmed);
        adapter.set_rotation(committed_rotation);
        crate::diag::record_commit(
            0x0900_0000
                | ((programmed as u32) << 23)
                | (((w / 8) & 0x7FF) << 11)
                | ((h / 8) & 0x7FF),
        );
    } else {
        adapter.mark_framebuffer_active(false);
        crate::diag::record_commit(0x090F_0000 | (arg.AffectedVidPnSourceId & 0xFF));
    }
    STATUS_SUCCESS
}

// ── RecommendMonitorModes ───────────────────────────────────────────────────

/// `DxgkDdiRecommendMonitorModes` — publish the monitor's supported modes (the
/// first marked preferred).
pub unsafe fn recommend_monitor_modes(arg: *const _DXGKARG_RECOMMENDMONITORMODES) -> NTSTATUS {
    if arg.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    let arg = unsafe { &*arg };
    if arg.pMonitorSourceModeSetInterface.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    let iface = unsafe { &*arg.pMonitorSourceModeSetInterface };
    let h_set = arg.hMonitorSourceModeSet;
    let (Some(create), Some(add), Some(release)) =
        (iface.pfnCreateNewModeInfo, iface.pfnAddMode, iface.pfnReleaseModeInfo)
    else {
        return STATUS_INVALID_PARAMETER;
    };
    for (idx, &(w, h)) in MODE_TABLE.iter().enumerate() {
        let mut p_mode: *mut _D3DKMDT_MONITOR_SOURCE_MODE = core::ptr::null_mut();
        if !ok(unsafe { create(h_set, &mut p_mode) }) || p_mode.is_null() {
            continue;
        }
        let m = unsafe { &mut *p_mode };
        m.VideoSignalInfo = unsafe { video_signal_info(w, h) };
        m.Origin = _D3DKMDT_MONITOR_CAPABILITIES_ORIGIN::D3DKMDT_MCO_DRIVER;
        m.Preference = mode_preference(idx);
        m.ColorBasis = _D3DKMDT_COLOR_BASIS::D3DKMDT_CB_SRGB;
        // 8 bits per channel — VioGpuDod's AddSingleMonitorMode sets exactly this.
        // A zeroed color range is inconsistent with the 8bpc A8R8G8B8 source/target
        // and is a candidate for dxgkrnl's post-commit monitor/mode consistency check.
        m.ColorCoeffDynamicRanges.FirstChannel = 8;
        m.ColorCoeffDynamicRanges.SecondChannel = 8;
        m.ColorCoeffDynamicRanges.ThirdChannel = 8;
        m.ColorCoeffDynamicRanges.FourthChannel = 8;
        let st = unsafe { add(h_set, p_mode) };
        // Skip (don't abort) a mode dxgkrnl won't accept — with a multi-mode table a
        // single bad resolution must not wipe the whole monitor mode set.
        if !ok(st) {
            unsafe { release(h_set, p_mode as *const _) };
        }
    }
    STATUS_SUCCESS
}

// Keep `c_void` referenced (used via raw-pointer casts in the DDI thunks).
const _: () = {
    let _ = core::mem::size_of::<*const c_void>();
};
