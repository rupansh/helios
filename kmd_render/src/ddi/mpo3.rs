//! D3 one-primary MPO3 table for the active D9 source package.
//! WDK 10.0.28000 ABI; every authority-bearing path is fenced by the D2
//! SURFACE-derived predicate and reaches the exact allocation-object interface.

#![allow(
    dead_code,
    reason = "D3 keeps bounded diagnostic counters outside the callback surface"
)]

use core::mem::{align_of, offset_of, size_of};
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use helios_kmd_logic::direct_scanout_admission::{
    MpoPlaneFacts, MpoSetPlaneFacts, PlaneFacts, PlaneRect,
};
use wdk_sys::ntddk::KeGetCurrentIrql;
use wdk_sys::{
    NTSTATUS, STATUS_INVALID_PARAMETER, STATUS_NOT_SUPPORTED, STATUS_RETRY, STATUS_SUCCESS,
};

use crate::adapter::AdapterContext;
use crate::device::ContextHandleRef;
use crate::dxgk::*;

use super::direct_scanout::{
    explicit_unbind, retain_candidate, validate_direct_scanout_binding, DirectScanoutOperation,
};

const SOURCE_ID: u32 = 0;
const PLANE_INDEX: u32 = 0;
const INPUT_STEREO_MASK: u32 = 0x7;
const INPUT_RETRY_AT_LOWER_IRQL: u32 = 0x8;
const INPUT_KNOWN_MASK: u32 = INPUT_STEREO_MASK | INPUT_RETRY_AT_LOWER_IRQL;
const PLANE_ENABLED: u32 = 0x1;
const PLANE_FLIP_IMMEDIATE: u32 = 0x2;
const PLANE_FLIP_ON_NEXT_VSYNC: u32 = 0x4;
const PLANE_SHARED_PRIMARY_TRANSITION: u32 = 0x8;
const PLANE_INDEPENDENT_FLIP_EXCLUSIVE: u32 = 0x10;
const PLANE_FLIP_IMMEDIATE_NO_TEARING: u32 = 0x20;
const PLANE_KNOWN_MASK: u32 = 0x3f;
const ATTR_VERTICAL_FLIP: u32 = 0x1;
const ATTR_HORIZONTAL_FLIP: u32 = 0x2;
const ATTR_STATIC_CHECK: u32 = 0x4;
const DEFAULT_SDR_WHITE_NITS: u32 = 80;

static CHECK_REFUSALS: AtomicU32 = AtomicU32::new(0);
static SET_REFUSALS: AtomicU32 = AtomicU32::new(0);
static SET_RETRIES: AtomicU32 = AtomicU32::new(0);
static SET_RETRIES_MIRRORED: AtomicU32 = AtomicU32::new(0);
static SET_ACCEPTS: AtomicU32 = AtomicU32::new(0);
static LAST_PRESENT_ID: AtomicU64 = AtomicU64::new(0);
static POST_PRESENT_HITS: AtomicU32 = AtomicU32::new(0);
static UPDATE_REFUSALS: AtomicU32 = AtomicU32::new(0);
static MODE_REQUESTS: AtomicU32 = AtomicU32::new(0);
static CAPS_REFUSALS: AtomicU32 = AtomicU32::new(0);

const _: () = {
    assert!(size_of::<DXGK_SETVIDPNSOURCEADDRESS_INPUT_FLAGS>() == 4);
    assert!(size_of::<DXGK_PLANE_SPECIFIC_INPUT_FLAGS>() == 4);
    assert!(size_of::<DXGK_MULTIPLANE_OVERLAY_FLAGS>() == 4);
    assert!(INPUT_KNOWN_MASK == 0x0f);
    assert!(
        PLANE_KNOWN_MASK
            == PLANE_ENABLED
                | PLANE_FLIP_IMMEDIATE
                | PLANE_FLIP_ON_NEXT_VSYNC
                | PLANE_SHARED_PRIMARY_TRANSITION
                | PLANE_INDEPENDENT_FLIP_EXCLUSIVE
                | PLANE_FLIP_IMMEDIATE_NO_TEARING
    );
    assert!(PLANE_KNOWN_MASK == 0x3f);
    assert!(ATTR_VERTICAL_FLIP | ATTR_HORIZONTAL_FLIP | ATTR_STATIC_CHECK == 0x07);
    assert!(size_of::<DXGK_MULTIPLANE_OVERLAY_PLANE_WITH_SOURCE2>() == 104);
    assert!(offset_of!(DXGK_MULTIPLANE_OVERLAY_PLANE_WITH_SOURCE2, hAllocation) == 0);
    assert!(offset_of!(DXGK_MULTIPLANE_OVERLAY_PLANE_WITH_SOURCE2, PlaneAttributes) == 16);
    assert!(size_of::<DXGKARG_CHECKMULTIPLANEOVERLAYSUPPORT3>() == 40);
    assert!(offset_of!(DXGKARG_CHECKMULTIPLANEOVERLAYSUPPORT3, ppPlanes) == 8);
    assert!(offset_of!(DXGKARG_CHECKMULTIPLANEOVERLAYSUPPORT3, Supported) == 32);
    assert!(size_of::<DXGK_PRIMARYCONTEXTDATA>() == 32);
    assert!(offset_of!(DXGK_PRIMARYCONTEXTDATA, hContext) == 0);
    assert!(offset_of!(DXGK_PRIMARYCONTEXTDATA, hAllocation) == 8);
    assert!(size_of::<DXGK_MULTIPLANE_OVERLAY_PLANE3>() == 144);
    assert!(offset_of!(DXGK_MULTIPLANE_OVERLAY_PLANE3, ppContextData) == 32);
    assert!(offset_of!(DXGK_MULTIPLANE_OVERLAY_PLANE3, PlaneAttributes) == 56);
    assert!(size_of::<DXGKARG_SETVIDPNSOURCEADDRESSWITHMULTIPLANEOVERLAY3>() == 56);
    assert!(
        offset_of!(
            DXGKARG_SETVIDPNSOURCEADDRESSWITHMULTIPLANEOVERLAY3,
            ppPlanes
        ) == 16
    );
    assert!(
        offset_of!(
            DXGKARG_SETVIDPNSOURCEADDRESSWITHMULTIPLANEOVERLAY3,
            TargetFlipTime
        ) == 48
    );
    assert!(size_of::<DXGKARG_POSTMULTIPLANEOVERLAYPRESENT>() == 24);
    assert!(size_of::<DXGKARG_VALIDATEUPDATEALLOCPROPERTY>() == 24);
    assert!(size_of::<DXGKARG_GETMULTIPLANEOVERLAYCAPS>() == 28);
    assert!(size_of::<DXGKARG_GETPOSTCOMPOSITIONCAPS>() == 12);
    assert!(size_of::<DXGKARG_CONTROLMODEBEHAVIOR>() == 12);
};

fn record_passive(counter: &AtomicU32, name: &'static [u8], code: u32) -> u32 {
    let count = counter.fetch_add(1, Ordering::Relaxed) + 1;
    if count == 1 || count % 64 == 0 {
        crate::diag::record_named_bytes(name, (code << 24) | count.min(0x00ff_ffff));
    }
    count
}

fn mirror_retries_passive() {
    let retries = SET_RETRIES.load(Ordering::Relaxed);
    let old = SET_RETRIES_MIRRORED.swap(retries, Ordering::Relaxed);
    if retries != old {
        crate::diag::record_named_bytes(b"MpoRetry", retries);
    }
}

fn adapter_from_handle<'a>(h_adapter: IN_CONST_HANDLE) -> Option<&'a AdapterContext> {
    if h_adapter.is_null() || !(h_adapter as *const AdapterContext).is_aligned() {
        return None;
    }
    Some(unsafe { &*(h_adapter as *const AdapterContext) })
}

unsafe fn first_mut_ptr<T>(table: *mut *mut T) -> Option<*mut T> {
    if table.is_null() || (table as usize) % align_of::<*mut T>() != 0 {
        return None;
    }
    let value = unsafe { table.read() };
    (!value.is_null() && (value as usize) % align_of::<T>() == 0).then_some(value)
}

fn rect(rect: &RECT) -> PlaneRect {
    PlaneRect {
        left: i64::from(rect.left),
        top: i64::from(rect.top),
        right: i64::from(rect.right),
        bottom: i64::from(rect.bottom),
    }
}

fn plane_facts(
    attributes: &DXGK_MULTIPLANE_OVERLAY_ATTRIBUTES3,
    plane_count: u32,
    layer_index: u32,
    post_composition: bool,
    hdr_metadata: bool,
    set_path: bool,
    extra_unsupported: u64,
) -> MpoPlaneFacts {
    let flags = unsafe { attributes.Flags.__bindgen_anon_1.Value };
    let blend = unsafe { attributes.Blend.__bindgen_anon_1.Value };
    let allowed_flags = if set_path {
        ATTR_VERTICAL_FLIP | ATTR_HORIZONTAL_FLIP
    } else {
        ATTR_VERTICAL_FLIP | ATTR_HORIZONTAL_FLIP | ATTR_STATIC_CHECK
    };
    let unsupported = extra_unsupported
        | u64::from(flags & !allowed_flags)
        | (u64::from(blend & !1) << 32)
        | (u64::from(attributes.StretchQuality != 0) << 33)
        | (u64::from(
            attributes.SDRWhiteLevel != 0 && attributes.SDRWhiteLevel != DEFAULT_SDR_WHITE_NITS,
        ) << 34)
        | (u64::from(attributes.DirtyRectCnt != 0) << 35);
    let source = rect(&attributes.SrcRect);
    let destination = rect(&attributes.DstRect);
    MpoPlaneFacts {
        plane_count,
        layer_index,
        source_rect: source,
        destination_rect: destination,
        clip_rect: rect(&attributes.ClipRect),
        identity_rotation: attributes.Rotation == _D3DDDI_ROTATION::D3DDDI_ROTATION_IDENTITY,
        vertical_flip: flags & ATTR_VERTICAL_FLIP != 0,
        horizontal_flip: flags & ATTR_HORIZONTAL_FLIP != 0,
        alpha_blend: blend & 1 != 0,
        sdr_rgb: attributes.ColorSpaceType
            == D3DDDI_COLOR_SPACE_TYPE::D3DDDI_COLOR_SPACE_RGB_FULL_G22_NONE_P709,
        scaling: source != destination,
        post_composition,
        hdr_metadata,
        unsupported_or_reserved_attributes_or_features: unsupported,
    }
}

fn operation_facts(global_flags: u32, plane_flags: u32, extra: u32) -> DirectScanoutOperation {
    DirectScanoutOperation {
        immediate_flip: plane_flags & (PLANE_FLIP_IMMEDIATE | PLANE_FLIP_IMMEDIATE_NO_TEARING) != 0,
        stereo: global_flags & INPUT_STEREO_MASK != 0,
        shared_primary_transition: plane_flags & PLANE_SHARED_PRIMARY_TRANSITION != 0,
        independent_flip_exclusive: plane_flags & PLANE_INDEPENDENT_FLIP_EXCLUSIVE != 0,
        // MPO3 flips only arrive on an already-visible source; the classic
        // MODE_CHANGE pre-visibility programming window does not exist here.
        mode_change: false,
        unsupported_or_reserved_flags: (global_flags & !INPUT_KNOWN_MASK)
            | (plane_flags & !PLANE_KNOWN_MASK)
            | extra,
    }
}

fn refuse_check(args: &mut DXGKARG_CHECKMULTIPLANEOVERLAYSUPPORT3, code: u32) -> NTSTATUS {
    args.Supported = 0;
    args.ReturnInfo = Default::default();
    record_passive(&CHECK_REFUSALS, b"MpoChkRef", code);
    STATUS_SUCCESS
}

/// PASSIVE_LEVEL. Unsupported shapes are ordinary composition fallbacks and
/// therefore return success with a reserved-clean false answer.
pub unsafe extern "C" fn dxgkddi_check_multi_plane_overlay_support3(
    h_adapter: IN_CONST_HANDLE,
    p_check: IN_OUT_PDXGKARG_CHECKMULTIPLANEOVERLAYSUPPORT3,
) -> NTSTATUS {
    if p_check.is_null() || !(p_check as *const DXGKARG_CHECKMULTIPLANEOVERLAYSUPPORT3).is_aligned()
    {
        return STATUS_INVALID_PARAMETER;
    }
    let args = unsafe { &mut *p_check };
    args.Supported = 0;
    args.ReturnInfo = Default::default();
    let Some(adapter) = adapter_from_handle(h_adapter) else {
        return refuse_check(args, 1);
    };
    if !crate::virtio::KMD_D2_OWNER_ENABLED {
        return STATUS_SUCCESS;
    }
    if args.PlaneCount != 1 || args.PostCompositionCount != 0 {
        return refuse_check(args, 2);
    }
    let Some(plane_ptr) = (unsafe { first_mut_ptr(args.ppPlanes) }) else {
        return refuse_check(args, 3);
    };
    let plane = unsafe { &*plane_ptr };
    let facts = plane_facts(
        &plane.PlaneAttributes,
        args.PlaneCount,
        plane.LayerIndex,
        args.PostCompositionCount != 0,
        false,
        false,
        0,
    );
    let operation = operation_facts(0, 0, 0);
    if unsafe {
        validate_direct_scanout_binding(
            adapter,
            plane.hAllocation,
            plane.VidPnSourceId,
            operation,
            PlaneFacts::MpoCheck(facts),
        )
    }
    .is_err()
    {
        return refuse_check(args, 4);
    }
    args.Supported = 1;
    STATUS_SUCCESS
}

/// DIRQL-to-PASSIVE split. Above PASSIVE this touches only the writable output
/// flag and atomics; the retry owns all pointer walks and plane transitions.
pub unsafe extern "C" fn dxgkddi_set_vidpn_source_address_with_multi_plane_overlay3(
    h_adapter: IN_CONST_HANDLE,
    p_set: IN_OUT_PDXGKARG_SETVIDPNSOURCEADDRESSWITHMULTIPLANEOVERLAY3,
) -> NTSTATUS {
    if p_set.is_null()
        || !(p_set as *const DXGKARG_SETVIDPNSOURCEADDRESSWITHMULTIPLANEOVERLAY3).is_aligned()
    {
        return STATUS_INVALID_PARAMETER;
    }
    let args = unsafe { &mut *p_set };
    args.OutputFlags = Default::default();
    if !crate::virtio::KMD_D2_OWNER_ENABLED {
        return STATUS_NOT_SUPPORTED;
    }
    if unsafe { KeGetCurrentIrql() } != 0 {
        args.OutputFlags.__bindgen_anon_1.Value = 1;
        SET_RETRIES.fetch_add(1, Ordering::Relaxed);
        return STATUS_RETRY;
    }

    mirror_retries_passive();
    let Some(adapter) = adapter_from_handle(h_adapter) else {
        record_passive(&SET_REFUSALS, b"MpoSetRef", 1);
        return STATUS_INVALID_PARAMETER;
    };
    let global_flags = unsafe { args.InputFlags.__bindgen_anon_1.Value };
    if args.VidPnSourceId != SOURCE_ID || global_flags & !INPUT_KNOWN_MASK != 0 {
        record_passive(&SET_REFUSALS, b"MpoSetRef", 2);
        return STATUS_INVALID_PARAMETER;
    }
    // SAFETY: this is the OS-documented PASSIVE retry, verified above with
    // KeGetCurrentIrql before minting the proof token.
    let passive = unsafe { crate::irql::PassiveLevel::assume() };
    if args.PlaneCount == 0 {
        if global_flags & INPUT_STEREO_MASK != 0
            || !args.pPostComposition.is_null()
            || args.Duration != 0
            || !args.pHDRMetaData.is_null()
            || args.TargetFlipTime != 0
        {
            record_passive(&SET_REFUSALS, b"MpoSetRef", 3);
            return STATUS_INVALID_PARAMETER;
        }
        return explicit_unbind(passive, adapter);
    }
    if args.PlaneCount != 1
        || !args.pPostComposition.is_null()
        || args.Duration != 0
        || !args.pHDRMetaData.is_null()
        || args.TargetFlipTime != 0
    {
        record_passive(&SET_REFUSALS, b"MpoSetRef", 4);
        return STATUS_INVALID_PARAMETER;
    }
    let Some(plane_ptr) = (unsafe { first_mut_ptr(args.ppPlanes) }) else {
        record_passive(&SET_REFUSALS, b"MpoSetRef", 5);
        return STATUS_INVALID_PARAMETER;
    };
    let plane = unsafe { &mut *plane_ptr };
    plane.OutputFlags = Default::default();
    let plane_flags = unsafe { plane.InputFlags.__bindgen_anon_1.Value };
    if plane.ContextCount != 1
        || plane.DriverPrivateDataSize != 0
        || !plane.pDriverPrivateData.is_null()
        || plane.MaxImmediateFlipLine != 0
    {
        record_passive(&SET_REFUSALS, b"MpoSetRef", 6);
        return STATUS_INVALID_PARAMETER;
    }
    let Some(context_ptr) = (unsafe { first_mut_ptr(plane.ppContextData) }) else {
        record_passive(&SET_REFUSALS, b"MpoSetRef", 7);
        return STATUS_INVALID_PARAMETER;
    };
    let context = unsafe { &*context_ptr };
    let context_adapter =
        unsafe { ContextHandleRef::from_raw(context.hContext) }.and_then(|handle| handle.adapter());
    let gpuva = unsafe { *context.__bindgen_anon_2.VirtualAddress.as_ref() };
    if context.hAllocation.is_null()
        || gpuva == 0
        || context_adapter.is_none_or(|owner| !core::ptr::eq(owner, adapter))
    {
        record_passive(&SET_REFUSALS, b"MpoSetRef", 8);
        return STATUS_INVALID_PARAMETER;
    }
    let extra = u32::from(plane_flags & PLANE_ENABLED == 0)
        | (u32::from(plane_flags & PLANE_FLIP_ON_NEXT_VSYNC == 0) << 1);
    let operation = operation_facts(global_flags, plane_flags, extra);
    let facts = plane_facts(
        &plane.PlaneAttributes,
        args.PlaneCount,
        plane.LayerIndex,
        false,
        false,
        true,
        0,
    );
    let candidate = match unsafe {
        validate_direct_scanout_binding(
            adapter,
            context.hAllocation,
            args.VidPnSourceId,
            operation,
            PlaneFacts::MpoSet(MpoSetPlaneFacts {
                plane: facts,
                enabled: plane_flags & PLANE_ENABLED != 0,
                context_count: plane.ContextCount,
                context_record_present: true,
            }),
        )
    } {
        Ok(candidate) => candidate,
        Err(status) => {
            record_passive(&SET_REFUSALS, b"MpoSetRef", 9);
            return status;
        }
    };
    let status = retain_candidate(adapter, candidate);
    if status != STATUS_SUCCESS {
        record_passive(&SET_REFUSALS, b"MpoSetRef", 10);
        return status;
    }
    LAST_PRESENT_ID.store(plane.PresentId, Ordering::Release);
    let accepted = SET_ACCEPTS.fetch_add(1, Ordering::Relaxed) + 1;
    if accepted == 1 || accepted % 64 == 0 {
        crate::diag::record_named_bytes(b"MpoSetOk", accepted);
        crate::diag::record_named_bytes(b"MpoPrLo", plane.PresentId as u32);
        crate::diag::record_named_bytes(b"MpoPrHi", (plane.PresentId >> 32) as u32);
    }
    STATUS_SUCCESS
}

pub unsafe extern "C" fn dxgkddi_get_multi_plane_overlay_caps(
    h_adapter: IN_CONST_HANDLE,
    p_caps: IN_OUT_PDXGKARG_GETMULTIPLANEOVERLAYCAPS,
) -> NTSTATUS {
    if p_caps.is_null() || !(p_caps as *const DXGKARG_GETMULTIPLANEOVERLAYCAPS).is_aligned() {
        return STATUS_INVALID_PARAMETER;
    }
    let caps = unsafe { &mut *p_caps };
    caps.MaxPlanes = 0;
    caps.MaxRGBPlanes = 0;
    caps.MaxYUVPlanes = 0;
    caps.OverlayCaps = Default::default();
    caps.MaxStretchFactor = 0.0;
    caps.MaxShrinkFactor = 0.0;
    if !crate::virtio::KMD_D2_OWNER_ENABLED {
        return STATUS_NOT_SUPPORTED;
    }
    if adapter_from_handle(h_adapter).is_none() || caps.VidPnSourceId != SOURCE_ID {
        record_passive(&CAPS_REFUSALS, b"MpoCapRef", 1);
        return STATUS_INVALID_PARAMETER;
    }
    caps.MaxPlanes = 1;
    caps.MaxRGBPlanes = 1;
    caps.MaxStretchFactor = 1.0;
    caps.MaxShrinkFactor = 1.0;
    STATUS_SUCCESS
}

pub unsafe extern "C" fn dxgkddi_get_post_composition_caps(
    h_adapter: IN_CONST_HANDLE,
    p_caps: IN_OUT_PDXGKARG_GETPOSTCOMPOSITIONCAPS,
) -> NTSTATUS {
    if p_caps.is_null() || !(p_caps as *const DXGKARG_GETPOSTCOMPOSITIONCAPS).is_aligned() {
        return STATUS_INVALID_PARAMETER;
    }
    let caps = unsafe { &mut *p_caps };
    caps.MaxStretchFactor = 0.0;
    caps.MaxShrinkFactor = 0.0;
    if !crate::virtio::KMD_D2_OWNER_ENABLED {
        return STATUS_NOT_SUPPORTED;
    }
    if adapter_from_handle(h_adapter).is_none() || caps.VidPnSourceId != SOURCE_ID {
        record_passive(&CAPS_REFUSALS, b"MpoCapRef", 2);
        return STATUS_INVALID_PARAMETER;
    }
    caps.MaxStretchFactor = 1.0;
    caps.MaxShrinkFactor = 1.0;
    STATUS_SUCCESS
}

/// The profile never requests `PostPresentNeeded`, so this notification owns no
/// mutation. It remains an always-success, counted tripwire if the OS invokes it.
pub unsafe extern "C" fn dxgkddi_post_multi_plane_overlay_present(
    _h_adapter: IN_CONST_HANDLE,
    p_post: IN_CONST_PDXGKARG_POSTMULTIPLANEOVERLAYPRESENT,
) -> NTSTATUS {
    let code = if p_post.is_null() { 1 } else { 0 };
    record_passive(&POST_PRESENT_HITS, b"MpoPost", code);
    STATUS_SUCCESS
}

pub unsafe extern "C" fn dxgkddi_validate_update_allocation_property(
    h_adapter: IN_CONST_HANDLE,
    p_update: IN_CONST_PDXGKARG_VALIDATEUPDATEALLOCPROPERTY,
) -> NTSTATUS {
    let Some(_adapter) = adapter_from_handle(h_adapter) else {
        return STATUS_INVALID_PARAMETER;
    };
    if p_update.is_null() || !(p_update as *const DXGKARG_VALIDATEUPDATEALLOCPROPERTY).is_aligned()
    {
        return STATUS_INVALID_PARAMETER;
    }
    if !crate::virtio::KMD_D2_OWNER_ENABLED {
        return STATUS_NOT_SUPPORTED;
    }
    let update = unsafe { &*p_update };
    let identity =
        unsafe { super::create_allocation::open_allocation_identity(update.hAllocation) };
    if identity.is_none_or(|identity| {
        identity.kind == helios_protocol::HELIOS_HWA2_KIND_INVALID
            || identity.kind > helios_protocol::HELIOS_HWA2_KIND_MAX
            || !crate::adapter::allocation_object::is_current(identity.generation)
    }) {
        record_passive(&UPDATE_REFUSALS, b"MpoUpdRef", 1);
        return STATUS_INVALID_PARAMETER;
    }
    let mask = unsafe { update.__bindgen_anon_1.PropertyMaskValue };
    if mask != 0 {
        record_passive(&UPDATE_REFUSALS, b"MpoUpdRef", 2);
        return STATUS_NOT_SUPPORTED;
    }
    STATUS_SUCCESS
}

pub unsafe extern "C" fn dxgkddi_control_mode_behavior(
    h_adapter: IN_CONST_HANDLE,
    p_control: INOUT_PDXGKARG_CONTROLMODEBEHAVIOR,
) -> NTSTATUS {
    if h_adapter.is_null()
        || p_control.is_null()
        || !(p_control as *const DXGKARG_CONTROLMODEBEHAVIOR).is_aligned()
    {
        return STATUS_INVALID_PARAMETER;
    }
    let control = unsafe { &mut *p_control };
    let request = unsafe { control.Request.Value };
    control.Satisfied = Default::default();
    control.NotSatisfied = Default::default();
    // WDK contract: NotSatisfied is only for a behavior this adapter SUPPORTS
    // but failed to apply. Helios supports neither PrioritizeHDR nor
    // ColorimetricControl, so every requested unsupported bit stays clear in
    // both result fields. Echoing Request into NotSatisfied falsely tells
    // dxgkrnl that a supported mode-control operation failed.
    if request != 0 {
        record_passive(&MODE_REQUESTS, b"MpoModeRq", request & 0xff);
    }
    STATUS_SUCCESS
}
