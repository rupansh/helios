#![allow(
    dead_code,
    reason = "D0 precedes the D1-D3 event sites and D9 diagnostic snapshot consumer"
)]

use core::ffi::c_void;
use core::hint::spin_loop;
use core::mem::{align_of, size_of};
use core::ptr;
use core::sync::atomic::{AtomicI32, AtomicU32, AtomicU64, Ordering};

use helios_protocol::diagnostics::{
    gate_event, validate_control_etw_logging_flags, validate_descriptor_v1,
    validate_payload_v1_for_generation, HeliosEtwEventDescriptor, HeliosEtwEventId, HeliosEtwGate,
    HeliosEtwGuid, HeliosEtwReject, HeliosGraphicsEtwPayloadV1, HELIOS_ETW_DATA_DESCRIPTOR_COUNT,
    HELIOS_ETW_EVENT_DESCRIPTORS, HELIOS_ETW_PAYLOAD_V1_SIZE, HELIOS_ETW_PROVIDER_GUID,
    HELIOS_ETW_REJECT_CODE_MAX,
};
use wdk_sys::ntddk::{EtwProviderEnabled, EtwRegister, EtwUnregister, EtwWrite, KeGetCurrentIrql};
use wdk_sys::{BOOLEAN, EVENT_DATA_DESCRIPTOR, EVENT_DESCRIPTOR, GUID, REGHANDLE, UCHAR, ULONG};

use crate::adapter::AdapterContext;
use crate::dxgk::_DXGK_DIAGNOSTICINFO_TYPE::{
    DXGK_DI_ADDDEVICE, DXGK_DI_BLACKSCREEN, DXGK_DI_STARTDEVICE,
};
use crate::dxgk::_DXGK_TDR_TYPE::{
    DXGK_TDR_TYPE_DISPLAY_ENGINE_FAULT, DXGK_TDR_TYPE_DOD_PRESENT_FORCED,
    DXGK_TDR_TYPE_DOD_PRESENT_TIMEOUT, DXGK_TDR_TYPE_DOD_VSYNC_FORCED,
    DXGK_TDR_TYPE_DOD_VSYNC_TIMEOUT, DXGK_TDR_TYPE_ENGINE_PAGE_FAULT, DXGK_TDR_TYPE_ENGINE_TIMEOUT,
    DXGK_TDR_TYPE_ENGINE_TIMEOUT_PROMOTED, DXGK_TDR_TYPE_FORCED, DXGK_TDR_TYPE_INVALID_FENCE,
    DXGK_TDR_TYPE_PAGE_FAULT, DXGK_TDR_TYPE_PREEMPT_TIMEOUT, DXGK_TDR_TYPE_UNKNOWN,
    DXGK_TDR_TYPE_VSYNC_TIMEOUT,
};
use crate::dxgk::{
    CHAR, DXGKARG_COLLECTDBGINFO2, DXGKARG_COLLECTDBGINFO_EXT, DXGKARG_COLLECTDIAGNOSTICINFO,
    DXGK_TDR_PAYLOAD_ENGINE_TIMEOUT, DXGK_TDR_PAYLOAD_VSYNC_TIMEOUT,
    INOUT_PDXGKARG_COLLECTDBGINFO2, INOUT_PDXGKARG_COLLECTDIAGNOSTICINFO, IN_CONST_HANDLE,
    IN_CONST_PDEVICE_OBJECT, NTSTATUS, STATUS_INVALID_PARAMETER, STATUS_SUCCESS,
};

const CONTROL_LEVEL_MASK: u32 = 0xff;
const CONTROL_ENABLED: u32 = 1 << 8;
const PROVIDER_RUNDOWN_CLOSED: u32 = 1 << 31;
const PROVIDER_RUNDOWN_ACTIVE_MASK: u32 = 0x0000_ffff;
const PROVIDER_RUNDOWN_ACTIVE_MAX: u32 = PROVIDER_RUNDOWN_ACTIVE_MASK;
const ADAPTER_RUNDOWN_CLOSED: u64 = 1 << 63;
const ADAPTER_RUNDOWN_ACTIVE_MASK: u64 = 0x0000_ffff;
const ADAPTER_RUNDOWN_ACTIVE_MAX: u64 = ADAPTER_RUNDOWN_ACTIVE_MASK;
const ADAPTER_RUNDOWN_EPOCH_SHIFT: u32 = 16;
const ADAPTER_RUNDOWN_EPOCH_ONE: u64 = 1 << ADAPTER_RUNDOWN_EPOCH_SHIFT;
const ADAPTER_RUNDOWN_EPOCH_MASK: u64 = !(ADAPTER_RUNDOWN_CLOSED | ADAPTER_RUNDOWN_ACTIVE_MASK);
pub const HELIOS_ETW_REJECTION_COUNTER_COUNT: usize = HELIOS_ETW_REJECT_CODE_MAX as usize + 1;

static CONTROL_STATE: AtomicU32 = AtomicU32::new(0);
static PROVIDER_RUNDOWN_STATE: AtomicU32 = AtomicU32::new(PROVIDER_RUNDOWN_CLOSED);
static REG_HANDLE: AtomicU64 = AtomicU64::new(0);

static REGISTER_ATTEMPTS: AtomicU32 = AtomicU32::new(0);
static REGISTER_FAILURES: AtomicU32 = AtomicU32::new(0);
static REGISTER_DUPLICATES: AtomicU32 = AtomicU32::new(0);
static REGISTER_ZERO_HANDLES: AtomicU32 = AtomicU32::new(0);
static LAST_REGISTER_STATUS: AtomicI32 = AtomicI32::new(0);
static ADAPTER_STARTS: AtomicU32 = AtomicU32::new(0);
static ADAPTER_STOPS: AtomicU32 = AtomicU32::new(0);
static ADAPTER_START_UNREGISTERED: AtomicU32 = AtomicU32::new(0);
static ADAPTER_OPEN_REFUSED: AtomicU32 = AtomicU32::new(0);
static ADAPTER_EPOCH_EXHAUSTED: AtomicU32 = AtomicU32::new(0);
static UNREGISTER_CALLS: AtomicU32 = AtomicU32::new(0);
static UNREGISTER_FAILURES: AtomicU32 = AtomicU32::new(0);
static LAST_UNREGISTER_STATUS: AtomicI32 = AtomicI32::new(0);
static CONTROL_FLAGS_REFUSED: AtomicU32 = AtomicU32::new(0);
static PROVIDER_RUNDOWN_CLOSED_REFUSED: AtomicU32 = AtomicU32::new(0);
static PROVIDER_RUNDOWN_FULL_REFUSED: AtomicU32 = AtomicU32::new(0);
static PROVIDER_RUNDOWN_CONTENDED_REFUSED: AtomicU32 = AtomicU32::new(0);
static ADAPTER_RUNDOWN_CLOSED_REFUSED: AtomicU32 = AtomicU32::new(0);
static ADAPTER_RUNDOWN_FULL_REFUSED: AtomicU32 = AtomicU32::new(0);
static ADAPTER_RUNDOWN_CONTENDED_REFUSED: AtomicU32 = AtomicU32::new(0);
static ADAPTER_RUNDOWN_STALE_EPOCH_REFUSED: AtomicU32 = AtomicU32::new(0);
static ADAPTER_RUNDOWN_REVALIDATE_REFUSED: AtomicU32 = AtomicU32::new(0);
static UNREGISTERED_EMIT_REFUSED: AtomicU32 = AtomicU32::new(0);
static REJECT_INDEX_REFUSED: AtomicU32 = AtomicU32::new(0);
static WRITE_SUCCESSES: AtomicU32 = AtomicU32::new(0);
static WRITE_FAILURES: AtomicU32 = AtomicU32::new(0);
static LAST_WRITE_STATUS: AtomicI32 = AtomicI32::new(0);
static COLLECT_DIAGNOSTIC_CALLS: AtomicU32 = AtomicU32::new(0);
static COLLECT_DIAGNOSTIC_REFUSALS: AtomicU32 = AtomicU32::new(0);
static COLLECT_DBG_INFO2_CALLS: AtomicU32 = AtomicU32::new(0);
static COLLECT_DBG_INFO2_REFUSALS: AtomicU32 = AtomicU32::new(0);
static REJECTION_COUNTS: [AtomicU32; HELIOS_ETW_REJECTION_COUNTER_COUNT] =
    [const { AtomicU32::new(0) }; HELIOS_ETW_REJECTION_COUNTER_COUNT];

#[derive(Clone, Copy)]
pub struct HeliosEtwDiagnosticSnapshot {
    pub register_attempts: u32,
    pub register_failures: u32,
    pub register_duplicates: u32,
    pub register_zero_handles: u32,
    pub last_register_status: i32,
    pub registered: u32,
    pub adapter_starts: u32,
    pub adapter_stops: u32,
    pub adapter_start_unregistered: u32,
    pub adapter_open_refused: u32,
    pub adapter_epoch_exhausted: u32,
    pub control_enabled: u32,
    pub control_level: u32,
    pub unregister_calls: u32,
    pub unregister_failures: u32,
    pub last_unregister_status: i32,
    pub control_flags_refused: u32,
    pub provider_rundown_closed_refused: u32,
    pub provider_rundown_full_refused: u32,
    pub provider_rundown_contended_refused: u32,
    pub adapter_rundown_closed_refused: u32,
    pub adapter_rundown_full_refused: u32,
    pub adapter_rundown_contended_refused: u32,
    pub adapter_rundown_stale_epoch_refused: u32,
    pub adapter_rundown_revalidate_refused: u32,
    pub unregistered_emit_refused: u32,
    pub reject_index_refused: u32,
    pub write_successes: u32,
    pub write_failures: u32,
    pub last_write_status: i32,
    pub rejection_counts: [u32; HELIOS_ETW_REJECTION_COUNTER_COUNT],
}

#[derive(Clone, Copy)]
pub struct HeliosEtwRegistrationSnapshot {
    pub attempts: u32,
    pub failures: u32,
    pub last_status: i32,
    pub registered: u32,
}

/// Pointer-free, bounded payload shared by the OS-owned C42 diagnostic DDIs.
/// It contains only package/adapter state and counters; never a handle, pointer,
/// raw resource identity, process identity, or discovery token.
#[repr(C)]
#[derive(Clone, Copy)]
struct HeliosOsDiagnosticReportV1 {
    magic: u32,
    version: u16,
    size: u16,
    request_kind: u32,
    reason: u32,
    package_generation: u64,
    surface_interface_version: u32,
    completed_fence: u32,
    etw_registered: u32,
    etw_last_register_status: i32,
    etw_control_enabled: u32,
    etw_control_level: u32,
    adapter_epoch: u64,
    adapter_rundown_active: u32,
    adapter_rundown_closed: u32,
}

const HELIOS_OS_DIAGNOSTIC_MAGIC: u32 = 0x3144_4F48; // HOD1
const VIDEO_TDR_TIMEOUT_DETECTED: u32 = 0x117;
const VIDEO_ENGINE_TIMEOUT_DETECTED: u32 = 0x141;
const _: () = {
    assert!(size_of::<HeliosOsDiagnosticReportV1>() == 64);
    assert!(align_of::<HeliosOsDiagnosticReportV1>() == 8);
    assert!(size_of::<DXGKARG_COLLECTDIAGNOSTICINFO>() == 232);
    assert!(align_of::<DXGKARG_COLLECTDIAGNOSTICINFO>() == 8);
    assert!(size_of::<DXGKARG_COLLECTDBGINFO2>() == 48);
    assert!(align_of::<DXGKARG_COLLECTDBGINFO2>() == 8);
    assert!(size_of::<DXGKARG_COLLECTDBGINFO_EXT>() == 32);
    assert!(align_of::<DXGKARG_COLLECTDBGINFO_EXT>() == 4);
    assert!(size_of::<DXGK_TDR_PAYLOAD_ENGINE_TIMEOUT>() == 40);
    assert!(align_of::<DXGK_TDR_PAYLOAD_ENGINE_TIMEOUT>() == 8);
    assert!(size_of::<DXGK_TDR_PAYLOAD_VSYNC_TIMEOUT>() == 16);
    assert!(align_of::<DXGK_TDR_PAYLOAD_VSYNC_TIMEOUT>() == 8);
};

fn refuse_os_diagnostic(counter: &AtomicU32, code: u32) -> NTSTATUS {
    counter.fetch_add(1, Ordering::Relaxed);
    crate::diag::record(0x0D90_0000 | (code & 0xffff));
    STATUS_INVALID_PARAMETER
}

fn checked_range(raw: *const c_void, len: usize) -> Option<(usize, usize)> {
    if len == 0 {
        let at = raw as usize;
        return Some((at, at));
    }
    if raw.is_null() {
        return None;
    }
    let start = raw as usize;
    Some((start, start.checked_add(len)?))
}

fn ranges_overlap(
    left: *const c_void,
    left_len: usize,
    right: *const c_void,
    right_len: usize,
) -> bool {
    let Some((left_start, left_end)) = checked_range(left, left_len) else {
        return true;
    };
    let Some((right_start, right_end)) = checked_range(right, right_len) else {
        return true;
    };
    left_start < right_end && right_start < left_end
}

fn copy_ascii<const N: usize>(destination: &mut [CHAR; N], source: &[u8]) {
    let count = core::cmp::min(source.len(), N.saturating_sub(1));
    for (out, byte) in destination.iter_mut().zip(source.iter()).take(count) {
        *out = *byte as CHAR;
    }
}

fn os_diagnostic_report(
    adapter: Option<&AdapterContext>,
    request_kind: u32,
    reason: u32,
) -> HeliosOsDiagnosticReportV1 {
    let registration = registration_snapshot();
    let control = CONTROL_STATE.load(Ordering::Acquire);
    let (completed_fence, rundown) = match adapter {
        Some(adapter) => (adapter.completed_fence(), adapter_rundown_snapshot(adapter)),
        None => (
            0,
            HeliosEtwAdapterRundownSnapshot {
                epoch: 0,
                active: 0,
                closed: 1,
            },
        ),
    };
    HeliosOsDiagnosticReportV1 {
        magic: HELIOS_OS_DIAGNOSTIC_MAGIC,
        version: 1,
        size: size_of::<HeliosOsDiagnosticReportV1>() as u16,
        request_kind,
        reason,
        package_generation: helios_protocol::HELIOS_PACKAGE_GENERATION,
        surface_interface_version: crate::ddi::wddm_surface::SURFACE.ddi_interface_version(),
        completed_fence,
        etw_registered: registration.registered,
        etw_last_register_status: registration.last_status,
        etw_control_enabled: u32::from(control & CONTROL_ENABLED != 0),
        etw_control_level: control & CONTROL_LEVEL_MASK,
        adapter_epoch: rundown.epoch,
        adapter_rundown_active: rundown.active,
        adapter_rundown_closed: rundown.closed,
    }
}

fn valid_diagnostic_info_type(kind: i32) -> bool {
    matches!(
        kind,
        DXGK_DI_ADDDEVICE | DXGK_DI_STARTDEVICE | DXGK_DI_BLACKSCREEN
    )
}

fn valid_tdr_type(kind: i32) -> bool {
    matches!(
        kind,
        DXGK_TDR_TYPE_UNKNOWN
            | DXGK_TDR_TYPE_FORCED
            | DXGK_TDR_TYPE_PREEMPT_TIMEOUT
            | DXGK_TDR_TYPE_VSYNC_TIMEOUT
            | DXGK_TDR_TYPE_DOD_PRESENT_FORCED
            | DXGK_TDR_TYPE_DOD_PRESENT_TIMEOUT
            | DXGK_TDR_TYPE_ENGINE_TIMEOUT
            | DXGK_TDR_TYPE_DOD_VSYNC_FORCED
            | DXGK_TDR_TYPE_DOD_VSYNC_TIMEOUT
            | DXGK_TDR_TYPE_ENGINE_TIMEOUT_PROMOTED
            | DXGK_TDR_TYPE_PAGE_FAULT
            | DXGK_TDR_TYPE_INVALID_FENCE
            | DXGK_TDR_TYPE_ENGINE_PAGE_FAULT
            | DXGK_TDR_TYPE_DISPLAY_ENGINE_FAULT
    )
}

/// `DxgkDdiCollectDiagnosticInfo` — bounded WDDM 2.6+ black-box collection.
///
/// WDK 28000 makes `hAdapter` optional because AddDevice can fail before the
/// miniport publishes one. WDDM 2.7+ nevertheless requires BLACKSCREEN support,
/// so all three defined request types receive the same pointer-free snapshot.
pub unsafe extern "C" fn dxgkddi_collect_diagnostic_info(
    physical_device_object: IN_CONST_PDEVICE_OBJECT,
    collect_diagnostic_info: INOUT_PDXGKARG_COLLECTDIAGNOSTICINFO,
) -> NTSTATUS {
    COLLECT_DIAGNOSTIC_CALLS.fetch_add(1, Ordering::Relaxed);
    // WDK 28000 declares PASSIVE_LEVEL. Check it before reading caller memory.
    if unsafe { KeGetCurrentIrql() } != crate::ddi::PASSIVE_LEVEL_IRQL {
        return refuse_os_diagnostic(&COLLECT_DIAGNOSTIC_REFUSALS, 1);
    }
    if physical_device_object.is_null()
        || !physical_device_object.is_aligned()
        || collect_diagnostic_info.is_null()
        || !collect_diagnostic_info.is_aligned()
    {
        return refuse_os_diagnostic(&COLLECT_DIAGNOSTIC_REFUSALS, 2);
    }

    // SAFETY: null/alignment were checked above; dxgkrnl owns this 232-byte
    // argument for the call. Copying it creates no reference and performs no
    // output mutation.
    let input = unsafe { ptr::read(collect_diagnostic_info) };
    if !valid_diagnostic_info_type(input.Type) {
        return refuse_os_diagnostic(&COLLECT_DIAGNOSTIC_REFUSALS, 3);
    }
    if input.BufferSizeIn != 0 && input.pBuffer.is_null() {
        return refuse_os_diagnostic(&COLLECT_DIAGNOSTIC_REFUSALS, 4);
    }
    let copy_len = core::cmp::min(
        input.BufferSizeIn as usize,
        size_of::<HeliosOsDiagnosticReportV1>(),
    );
    if checked_range(input.pBuffer.cast_const(), copy_len).is_none()
        || ranges_overlap(
            input.pBuffer.cast_const(),
            copy_len,
            collect_diagnostic_info.cast(),
            size_of::<DXGKARG_COLLECTDIAGNOSTICINFO>(),
        )
    {
        return refuse_os_diagnostic(&COLLECT_DIAGNOSTIC_REFUSALS, 5);
    }

    let adapter = if input.hAdapter.is_null() {
        None
    } else {
        let adapter_ptr = input.hAdapter as *const AdapterContext;
        if !adapter_ptr.is_aligned()
            || ranges_overlap(
                collect_diagnostic_info.cast(),
                size_of::<DXGKARG_COLLECTDIAGNOSTICINFO>(),
                adapter_ptr.cast(),
                size_of::<AdapterContext>(),
            )
            || ranges_overlap(
                input.pBuffer.cast_const(),
                copy_len,
                adapter_ptr.cast(),
                size_of::<AdapterContext>(),
            )
        {
            return refuse_os_diagnostic(&COLLECT_DIAGNOSTIC_REFUSALS, 6);
        }
        // SAFETY: this is the optional adapter handle dxgkrnl received from
        // AddDevice; alignment was checked before reference formation.
        Some(unsafe { &*adapter_ptr })
    };

    let report = os_diagnostic_report(adapter, input.Type as u32, 0);
    let mut output = input;
    output.BucketingString = [0; 64];
    output.DescriptionString = [0; 128];
    let (bucket, description): (&[u8], &[u8]) = if input.Type == DXGK_DI_ADDDEVICE {
        (b"Helios_AddDevice", b"Helios_WDDM32_AddDevice_diagnostic")
    } else if input.Type == DXGK_DI_STARTDEVICE {
        (
            b"Helios_StartDevice",
            b"Helios_WDDM32_StartDevice_diagnostic",
        )
    } else if input.Type == DXGK_DI_BLACKSCREEN {
        (
            b"Helios_BlackScreen",
            b"Helios_WDDM32_BlackScreen_diagnostic",
        )
    } else {
        return refuse_os_diagnostic(&COLLECT_DIAGNOSTIC_REFUSALS, 7);
    };
    copy_ascii(&mut output.BucketingString, bucket);
    copy_ascii(&mut output.DescriptionString, description);
    output.__bindgen_anon_1.pReserved = ptr::null_mut();
    output.BufferSizeOut = copy_len as u32;

    // All validation and local construction are complete. Publish the bounded
    // bytes first, then the one argument result as a single struct write.
    if copy_len != 0 {
        unsafe {
            ptr::copy_nonoverlapping(
                (&report as *const HeliosOsDiagnosticReportV1).cast::<u8>(),
                input.pBuffer.cast::<u8>(),
                copy_len,
            )
        };
    }
    unsafe { ptr::write(collect_diagnostic_info, output) };
    STATUS_SUCCESS
}

/// `DxgkDdiCollectDbgInfo2` — WDDM 3.2 TDR-aware bounded snapshot.
pub unsafe extern "C" fn dxgkddi_collect_dbg_info2(
    h_adapter: IN_CONST_HANDLE,
    collect_dbg_info2: INOUT_PDXGKARG_COLLECTDBGINFO2,
) -> NTSTATUS {
    COLLECT_DBG_INFO2_CALLS.fetch_add(1, Ordering::Relaxed);
    if unsafe { KeGetCurrentIrql() } != crate::ddi::PASSIVE_LEVEL_IRQL {
        return refuse_os_diagnostic(&COLLECT_DBG_INFO2_REFUSALS, 0x101);
    }
    let adapter_ptr = h_adapter as *const AdapterContext;
    if adapter_ptr.is_null()
        || !adapter_ptr.is_aligned()
        || collect_dbg_info2.is_null()
        || !collect_dbg_info2.is_aligned()
    {
        return refuse_os_diagnostic(&COLLECT_DBG_INFO2_REFUSALS, 0x102);
    }
    if ranges_overlap(
        collect_dbg_info2.cast(),
        size_of::<DXGKARG_COLLECTDBGINFO2>(),
        adapter_ptr.cast(),
        size_of::<AdapterContext>(),
    ) {
        return refuse_os_diagnostic(&COLLECT_DBG_INFO2_REFUSALS, 0x102);
    }

    // SAFETY: validated 48-byte WDK argument; copy before forming references or
    // touching any output.
    let input = unsafe { ptr::read(collect_dbg_info2) };
    if !matches!(
        input.Reason,
        VIDEO_TDR_TIMEOUT_DETECTED | VIDEO_ENGINE_TIMEOUT_DETECTED
    ) || !valid_tdr_type(input.TdrType)
    {
        return refuse_os_diagnostic(&COLLECT_DBG_INFO2_REFUSALS, 0x103);
    }
    if input.BufferSize != 0 && input.pBuffer.is_null() {
        return refuse_os_diagnostic(&COLLECT_DBG_INFO2_REFUSALS, 0x104);
    }
    let Ok(buffer_size) = usize::try_from(input.BufferSize) else {
        return refuse_os_diagnostic(&COLLECT_DBG_INFO2_REFUSALS, 0x105);
    };
    let copy_len = core::cmp::min(buffer_size, size_of::<HeliosOsDiagnosticReportV1>());
    if checked_range(input.pBuffer.cast_const(), copy_len).is_none()
        || ranges_overlap(
            input.pBuffer.cast_const(),
            copy_len,
            collect_dbg_info2.cast(),
            size_of::<DXGKARG_COLLECTDBGINFO2>(),
        )
        || ranges_overlap(
            input.pBuffer.cast_const(),
            copy_len,
            adapter_ptr.cast(),
            size_of::<AdapterContext>(),
        )
    {
        return refuse_os_diagnostic(&COLLECT_DBG_INFO2_REFUSALS, 0x105);
    }

    if !input.pExtension.is_null()
        && (!input.pExtension.is_aligned()
            || checked_range(
                input.pExtension.cast(),
                size_of::<DXGKARG_COLLECTDBGINFO_EXT>(),
            )
            .is_none()
            || ranges_overlap(
                input.pExtension.cast(),
                size_of::<DXGKARG_COLLECTDBGINFO_EXT>(),
                collect_dbg_info2.cast(),
                size_of::<DXGKARG_COLLECTDBGINFO2>(),
            )
            || ranges_overlap(
                input.pExtension.cast(),
                size_of::<DXGKARG_COLLECTDBGINFO_EXT>(),
                input.pBuffer.cast_const(),
                copy_len,
            )
            || ranges_overlap(
                input.pExtension.cast(),
                size_of::<DXGKARG_COLLECTDBGINFO_EXT>(),
                adapter_ptr.cast(),
                size_of::<AdapterContext>(),
            ))
    {
        return refuse_os_diagnostic(&COLLECT_DBG_INFO2_REFUSALS, 0x106);
    }

    let mut engine_payload = None;
    if input.TdrPayload.is_null() {
        if input.TdrPayloadSize != 0 {
            return refuse_os_diagnostic(&COLLECT_DBG_INFO2_REFUSALS, 0x107);
        }
    } else {
        match input.TdrType {
            DXGK_TDR_TYPE_ENGINE_TIMEOUT => {
                let payload_ptr = input.TdrPayload.cast::<DXGK_TDR_PAYLOAD_ENGINE_TIMEOUT>();
                if input.TdrPayloadSize < size_of::<DXGK_TDR_PAYLOAD_ENGINE_TIMEOUT>() as u32
                    || !payload_ptr.is_aligned()
                    || checked_range(
                        input.TdrPayload.cast_const(),
                        size_of::<DXGK_TDR_PAYLOAD_ENGINE_TIMEOUT>(),
                    )
                    .is_none()
                    || ranges_overlap(
                        input.TdrPayload.cast_const(),
                        size_of::<DXGK_TDR_PAYLOAD_ENGINE_TIMEOUT>(),
                        collect_dbg_info2.cast(),
                        size_of::<DXGKARG_COLLECTDBGINFO2>(),
                    )
                    || ranges_overlap(
                        input.TdrPayload.cast_const(),
                        size_of::<DXGK_TDR_PAYLOAD_ENGINE_TIMEOUT>(),
                        input.pBuffer.cast_const(),
                        copy_len,
                    )
                    || (!input.pExtension.is_null()
                        && ranges_overlap(
                            input.TdrPayload.cast_const(),
                            size_of::<DXGK_TDR_PAYLOAD_ENGINE_TIMEOUT>(),
                            input.pExtension.cast(),
                            size_of::<DXGKARG_COLLECTDBGINFO_EXT>(),
                        ))
                    || ranges_overlap(
                        input.TdrPayload.cast_const(),
                        size_of::<DXGK_TDR_PAYLOAD_ENGINE_TIMEOUT>(),
                        adapter_ptr.cast(),
                        size_of::<AdapterContext>(),
                    )
                {
                    return refuse_os_diagnostic(&COLLECT_DBG_INFO2_REFUSALS, 0x108);
                }
                // WDK permits later structures to append fields; read only the
                // validated 28000 prefix and preserve the input values locally.
                let mut payload = unsafe { ptr::read(payload_ptr) };
                if payload.NodeOrdinal != 0
                    || payload.EngineOrdinal != 0
                    || payload.NumberOfPendingSuspendRequests != 0
                    || payload.NumberOfReadyInteractiveHwQueues != 0
                {
                    return refuse_os_diagnostic(&COLLECT_DBG_INFO2_REFUSALS, 0x109);
                }
                payload.hContext = ptr::null_mut();
                engine_payload = Some((payload_ptr, payload));
            }
            DXGK_TDR_TYPE_VSYNC_TIMEOUT => {
                let payload_ptr = input.TdrPayload.cast::<DXGK_TDR_PAYLOAD_VSYNC_TIMEOUT>();
                if input.TdrPayloadSize < size_of::<DXGK_TDR_PAYLOAD_VSYNC_TIMEOUT>() as u32
                    || !payload_ptr.is_aligned()
                    || checked_range(
                        input.TdrPayload.cast_const(),
                        size_of::<DXGK_TDR_PAYLOAD_VSYNC_TIMEOUT>(),
                    )
                    .is_none()
                    || ranges_overlap(
                        input.TdrPayload.cast_const(),
                        size_of::<DXGK_TDR_PAYLOAD_VSYNC_TIMEOUT>(),
                        collect_dbg_info2.cast(),
                        size_of::<DXGKARG_COLLECTDBGINFO2>(),
                    )
                    || ranges_overlap(
                        input.TdrPayload.cast_const(),
                        size_of::<DXGK_TDR_PAYLOAD_VSYNC_TIMEOUT>(),
                        input.pBuffer.cast_const(),
                        copy_len,
                    )
                    || (!input.pExtension.is_null()
                        && ranges_overlap(
                            input.TdrPayload.cast_const(),
                            size_of::<DXGK_TDR_PAYLOAD_VSYNC_TIMEOUT>(),
                            input.pExtension.cast(),
                            size_of::<DXGKARG_COLLECTDBGINFO_EXT>(),
                        ))
                    || ranges_overlap(
                        input.TdrPayload.cast_const(),
                        size_of::<DXGK_TDR_PAYLOAD_VSYNC_TIMEOUT>(),
                        adapter_ptr.cast(),
                        size_of::<AdapterContext>(),
                    )
                {
                    return refuse_os_diagnostic(&COLLECT_DBG_INFO2_REFUSALS, 0x10a);
                }
                let payload = unsafe { ptr::read(payload_ptr) };
                if payload.VidPnSourceId != 0 || payload.LayerIndex != 0 {
                    return refuse_os_diagnostic(&COLLECT_DBG_INFO2_REFUSALS, 0x10b);
                }
            }
            _ => return refuse_os_diagnostic(&COLLECT_DBG_INFO2_REFUSALS, 0x10c),
        }
    }

    // SAFETY: dxgkrnl's adapter handle was checked before forming the reference.
    let adapter = unsafe { &*adapter_ptr };
    let report = os_diagnostic_report(Some(adapter), input.TdrType as u32, input.Reason);
    let extension = DXGKARG_COLLECTDBGINFO_EXT::default();

    // There is no fallible work below this point. Publish only the fully built
    // local values, leaving every reserved extension field zero.
    if copy_len != 0 {
        unsafe {
            ptr::copy_nonoverlapping(
                (&report as *const HeliosOsDiagnosticReportV1).cast::<u8>(),
                input.pBuffer.cast::<u8>(),
                copy_len,
            )
        };
    }
    if !input.pExtension.is_null() {
        unsafe { ptr::write(input.pExtension, extension) };
    }
    if let Some((destination, payload)) = engine_payload {
        unsafe { ptr::write(destination, payload) };
    }
    STATUS_SUCCESS
}

pub(crate) struct EtwAdapterRundown {
    state: AtomicU64,
}

impl EtwAdapterRundown {
    pub(crate) const fn new() -> Self {
        Self {
            state: AtomicU64::new(ADAPTER_RUNDOWN_CLOSED),
        }
    }

    fn open_next_epoch(&self) -> bool {
        let state = self.state.load(Ordering::Acquire);
        if state & ADAPTER_RUNDOWN_CLOSED == 0 || state & ADAPTER_RUNDOWN_ACTIVE_MASK != 0 {
            ADAPTER_OPEN_REFUSED.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        let epoch = state & ADAPTER_RUNDOWN_EPOCH_MASK;
        if epoch == ADAPTER_RUNDOWN_EPOCH_MASK {
            ADAPTER_EPOCH_EXHAUSTED.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        if self
            .state
            .compare_exchange(
                state,
                epoch + ADAPTER_RUNDOWN_EPOCH_ONE,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            ADAPTER_OPEN_REFUSED.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        true
    }

    fn close_and_drain(&self) {
        self.state
            .fetch_or(ADAPTER_RUNDOWN_CLOSED, Ordering::AcqRel);
        while self.state.load(Ordering::Acquire) & ADAPTER_RUNDOWN_ACTIVE_MASK != 0 {
            spin_loop();
        }
    }

    fn acquire(&self) -> Option<EventSiteGuard<'_>> {
        let state = self.state.load(Ordering::Acquire);
        if state & ADAPTER_RUNDOWN_CLOSED != 0 {
            ADAPTER_RUNDOWN_CLOSED_REFUSED.fetch_add(1, Ordering::Relaxed);
            return None;
        }
        if state & ADAPTER_RUNDOWN_ACTIVE_MASK == ADAPTER_RUNDOWN_ACTIVE_MAX {
            ADAPTER_RUNDOWN_FULL_REFUSED.fetch_add(1, Ordering::Relaxed);
            return None;
        }
        if let Err(current) = self.state.compare_exchange(
            state,
            state.wrapping_add(1),
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            if current & ADAPTER_RUNDOWN_EPOCH_MASK != state & ADAPTER_RUNDOWN_EPOCH_MASK {
                ADAPTER_RUNDOWN_STALE_EPOCH_REFUSED.fetch_add(1, Ordering::Relaxed);
            } else if current & ADAPTER_RUNDOWN_CLOSED != 0 {
                ADAPTER_RUNDOWN_CLOSED_REFUSED.fetch_add(1, Ordering::Relaxed);
            } else if current & ADAPTER_RUNDOWN_ACTIVE_MASK == ADAPTER_RUNDOWN_ACTIVE_MAX {
                ADAPTER_RUNDOWN_FULL_REFUSED.fetch_add(1, Ordering::Relaxed);
            } else {
                ADAPTER_RUNDOWN_CONTENDED_REFUSED.fetch_add(1, Ordering::Relaxed);
            }
            return None;
        }
        Some(EventSiteGuard {
            rundown: self,
            epoch: state & ADAPTER_RUNDOWN_EPOCH_MASK,
        })
    }

    fn snapshot(&self) -> HeliosEtwAdapterRundownSnapshot {
        let state = self.state.load(Ordering::Acquire);
        HeliosEtwAdapterRundownSnapshot {
            epoch: (state & ADAPTER_RUNDOWN_EPOCH_MASK) >> ADAPTER_RUNDOWN_EPOCH_SHIFT,
            active: (state & ADAPTER_RUNDOWN_ACTIVE_MASK) as u32,
            closed: u32::from(state & ADAPTER_RUNDOWN_CLOSED != 0),
        }
    }
}

#[derive(Clone, Copy)]
pub struct HeliosEtwAdapterRundownSnapshot {
    pub epoch: u64,
    pub active: u32,
    pub closed: u32,
}

struct EventSiteGuard<'a> {
    rundown: &'a EtwAdapterRundown,
    epoch: u64,
}

impl EventSiteGuard<'_> {
    fn revalidate(&self) -> bool {
        let state = self.rundown.state.load(Ordering::Acquire);
        state & ADAPTER_RUNDOWN_CLOSED == 0 && state & ADAPTER_RUNDOWN_EPOCH_MASK == self.epoch
    }
}

impl Drop for EventSiteGuard<'_> {
    fn drop(&mut self) {
        self.rundown.state.fetch_sub(1, Ordering::Release);
    }
}

const _: () = {
    assert!(core::mem::size_of::<HeliosEtwGuid>() == core::mem::size_of::<GUID>());
    assert!(core::mem::align_of::<HeliosEtwGuid>() == core::mem::align_of::<GUID>());
    assert!(
        core::mem::size_of::<HeliosEtwEventDescriptor>()
            == core::mem::size_of::<EVENT_DESCRIPTOR>()
    );
    assert!(
        core::mem::align_of::<HeliosEtwEventDescriptor>()
            == core::mem::align_of::<EVENT_DESCRIPTOR>()
    );
    assert!(core::mem::size_of::<EVENT_DATA_DESCRIPTOR>() == 16);
    assert!(core::mem::align_of::<EVENT_DATA_DESCRIPTOR>() == 8);
    assert!(core::mem::size_of::<HeliosGraphicsEtwPayloadV1>() == 72);
    assert!(core::mem::align_of::<HeliosGraphicsEtwPayloadV1>() == 8);
    assert!(HELIOS_ETW_PAYLOAD_V1_SIZE == 72);
    assert!(HELIOS_ETW_DATA_DESCRIPTOR_COUNT == 1);
};

fn bump_rejection(reason: HeliosEtwReject) {
    if let Some(counter) = REJECTION_COUNTS.get(reason.code() as usize) {
        counter.fetch_add(1, Ordering::Relaxed);
    } else {
        REJECT_INDEX_REFUSED.fetch_add(1, Ordering::Relaxed);
    }
}

struct ProviderEventGuard;

impl Drop for ProviderEventGuard {
    fn drop(&mut self) {
        PROVIDER_RUNDOWN_STATE.fetch_sub(1, Ordering::Release);
    }
}

fn acquire_provider_site() -> Option<ProviderEventGuard> {
    let state = PROVIDER_RUNDOWN_STATE.load(Ordering::Acquire);
    if state & PROVIDER_RUNDOWN_CLOSED != 0 {
        PROVIDER_RUNDOWN_CLOSED_REFUSED.fetch_add(1, Ordering::Relaxed);
        return None;
    }
    if state & PROVIDER_RUNDOWN_ACTIVE_MASK == PROVIDER_RUNDOWN_ACTIVE_MAX {
        PROVIDER_RUNDOWN_FULL_REFUSED.fetch_add(1, Ordering::Relaxed);
        return None;
    }
    if let Err(current) = PROVIDER_RUNDOWN_STATE.compare_exchange(
        state,
        state.wrapping_add(1),
        Ordering::AcqRel,
        Ordering::Acquire,
    ) {
        if current & PROVIDER_RUNDOWN_CLOSED != 0 {
            PROVIDER_RUNDOWN_CLOSED_REFUSED.fetch_add(1, Ordering::Relaxed);
        } else if current & PROVIDER_RUNDOWN_ACTIVE_MASK == PROVIDER_RUNDOWN_ACTIVE_MAX {
            PROVIDER_RUNDOWN_FULL_REFUSED.fetch_add(1, Ordering::Relaxed);
        } else {
            PROVIDER_RUNDOWN_CONTENDED_REFUSED.fetch_add(1, Ordering::Relaxed);
        }
        return None;
    }
    Some(ProviderEventGuard)
}

pub unsafe fn register() {
    if REGISTER_ATTEMPTS
        .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        REGISTER_DUPLICATES.fetch_add(1, Ordering::Relaxed);
        return;
    }

    let mut handle: REGHANDLE = 0;
    // SAFETY: DriverEntry calls this once at PASSIVE; the GUID layout is asserted above and
    // both output storage and provider identity remain valid for the complete call.
    let status = unsafe {
        EtwRegister(
            (&HELIOS_ETW_PROVIDER_GUID as *const HeliosEtwGuid).cast::<GUID>(),
            None,
            core::ptr::null_mut(),
            &mut handle,
        )
    };
    LAST_REGISTER_STATUS.store(status as i32, Ordering::Release);
    if status < 0 || handle == 0 {
        REGISTER_FAILURES.fetch_add(1, Ordering::Relaxed);
        if handle == 0 {
            REGISTER_ZERO_HANDLES.fetch_add(1, Ordering::Relaxed);
        }
        return;
    }

    REG_HANDLE.store(handle, Ordering::Release);
    PROVIDER_RUNDOWN_STATE.store(0, Ordering::Release);
}

fn close_provider_sites() {
    PROVIDER_RUNDOWN_STATE.fetch_or(PROVIDER_RUNDOWN_CLOSED, Ordering::AcqRel);
    while PROVIDER_RUNDOWN_STATE.load(Ordering::Acquire) & PROVIDER_RUNDOWN_ACTIVE_MASK != 0 {
        spin_loop();
    }
}

pub fn adapter_start(adapter: &AdapterContext) {
    if REG_HANDLE.load(Ordering::Acquire) == 0 {
        ADAPTER_START_UNREGISTERED.fetch_add(1, Ordering::Relaxed);
        return;
    }
    if adapter.etw_rundown.open_next_epoch() {
        ADAPTER_STARTS.fetch_add(1, Ordering::Relaxed);
    }
}

pub fn adapter_stop(adapter: &AdapterContext) {
    adapter.etw_rundown.close_and_drain();
    ADAPTER_STOPS.fetch_add(1, Ordering::Relaxed);
}

pub fn adapter_rundown_snapshot(adapter: &AdapterContext) -> HeliosEtwAdapterRundownSnapshot {
    adapter.etw_rundown.snapshot()
}

pub fn unregister() {
    CONTROL_STATE.store(0, Ordering::Release);
    close_provider_sites();

    let handle = REG_HANDLE.swap(0, Ordering::AcqRel);
    if handle == 0 {
        return;
    }
    UNREGISTER_CALLS.fetch_add(1, Ordering::Relaxed);
    // SAFETY: the packed rundown is closed and empty, so no EtwProviderEnabled or EtwWrite
    // can still use this retained handle; driver teardown invokes this at PASSIVE.
    let status = unsafe { EtwUnregister(handle) };
    LAST_UNREGISTER_STATUS.store(status as i32, Ordering::Release);
    if status < 0 {
        UNREGISTER_FAILURES.fetch_add(1, Ordering::Relaxed);
    }
}

pub fn control_etw_logging(enable: BOOLEAN, flags: ULONG, level: UCHAR) {
    if let Err(reason) = validate_control_etw_logging_flags(flags as u64) {
        CONTROL_STATE.store(0, Ordering::Release);
        CONTROL_FLAGS_REFUSED.fetch_add(1, Ordering::Relaxed);
        bump_rejection(reason);
        return;
    }
    let state = (level as u32) | if enable != 0 { CONTROL_ENABLED } else { 0 };
    CONTROL_STATE.store(state, Ordering::Release);
}

pub fn emit(
    adapter: &AdapterContext,
    event: HeliosEtwEventId,
    build_payload: impl FnOnce() -> HeliosGraphicsEtwPayloadV1,
) {
    let control = CONTROL_STATE.load(Ordering::Acquire);
    if gate_event(
        event,
        control & CONTROL_ENABLED != 0,
        (control & CONTROL_LEVEL_MASK) as u8,
    ) != HeliosEtwGate::Emit
    {
        return;
    }

    let Some(adapter_guard) = adapter.etw_rundown.acquire() else {
        return;
    };
    let Some(_provider_guard) = acquire_provider_site() else {
        return;
    };
    let handle = REG_HANDLE.load(Ordering::Acquire);
    if handle == 0 {
        UNREGISTERED_EMIT_REFUSED.fetch_add(1, Ordering::Relaxed);
        return;
    }
    let descriptor_index = usize::from(event.id().saturating_sub(1));
    let Some(descriptor) = HELIOS_ETW_EVENT_DESCRIPTORS.get(descriptor_index) else {
        bump_rejection(HeliosEtwReject::UnknownEventId);
        return;
    };
    if let Err(reason) = validate_descriptor_v1(descriptor) {
        bump_rejection(reason);
        return;
    }

    // SAFETY: the retained handle is protected by the event-site guard; the descriptor is a
    // nonpaged immutable driver-image object with the WDK layout asserted above.
    if unsafe { EtwProviderEnabled(handle, event.level(), event.keyword()) } == 0 {
        return;
    }
    if !adapter_guard.revalidate() {
        ADAPTER_RUNDOWN_REVALIDATE_REFUSED.fetch_add(1, Ordering::Relaxed);
        return;
    }

    let payload = build_payload();
    if let Err(reason) = validate_payload_v1_for_generation(
        event,
        &payload,
        helios_protocol::HELIOS_PACKAGE_GENERATION,
    ) {
        bump_rejection(reason);
        return;
    }

    // SAFETY: zero is the documented reserved value for the descriptor union; Ptr and Size
    // are filled below before the object crosses the FFI boundary.
    let mut data: EVENT_DATA_DESCRIPTOR = unsafe { core::mem::zeroed() };
    data.Ptr = (&payload as *const HeliosGraphicsEtwPayloadV1 as usize) as u64;
    data.Size = HELIOS_ETW_PAYLOAD_V1_SIZE as ULONG;
    // SAFETY: payload and data live on the nonpaged kernel stack through the synchronous call;
    // the descriptor cast is covered by the size/alignment and protocol field-layout proofs.
    let status = unsafe {
        EtwWrite(
            handle,
            (descriptor as *const HeliosEtwEventDescriptor).cast::<EVENT_DESCRIPTOR>(),
            core::ptr::null(),
            HELIOS_ETW_DATA_DESCRIPTOR_COUNT,
            &mut data,
        )
    };
    LAST_WRITE_STATUS.store(status as i32, Ordering::Release);
    if status < 0 {
        WRITE_FAILURES.fetch_add(1, Ordering::Relaxed);
    } else {
        WRITE_SUCCESSES.fetch_add(1, Ordering::Relaxed);
    }
}

pub fn diagnostic_snapshot() -> HeliosEtwDiagnosticSnapshot {
    let control = CONTROL_STATE.load(Ordering::Acquire);
    let mut rejection_counts = [0; HELIOS_ETW_REJECTION_COUNTER_COUNT];
    for (destination, source) in rejection_counts.iter_mut().zip(REJECTION_COUNTS.iter()) {
        *destination = source.load(Ordering::Relaxed);
    }
    HeliosEtwDiagnosticSnapshot {
        register_attempts: REGISTER_ATTEMPTS.load(Ordering::Relaxed),
        register_failures: REGISTER_FAILURES.load(Ordering::Relaxed),
        register_duplicates: REGISTER_DUPLICATES.load(Ordering::Relaxed),
        register_zero_handles: REGISTER_ZERO_HANDLES.load(Ordering::Relaxed),
        last_register_status: LAST_REGISTER_STATUS.load(Ordering::Acquire),
        registered: u32::from(REG_HANDLE.load(Ordering::Acquire) != 0),
        adapter_starts: ADAPTER_STARTS.load(Ordering::Relaxed),
        adapter_stops: ADAPTER_STOPS.load(Ordering::Relaxed),
        adapter_start_unregistered: ADAPTER_START_UNREGISTERED.load(Ordering::Relaxed),
        adapter_open_refused: ADAPTER_OPEN_REFUSED.load(Ordering::Relaxed),
        adapter_epoch_exhausted: ADAPTER_EPOCH_EXHAUSTED.load(Ordering::Relaxed),
        control_enabled: u32::from(control & CONTROL_ENABLED != 0),
        control_level: control & CONTROL_LEVEL_MASK,
        unregister_calls: UNREGISTER_CALLS.load(Ordering::Relaxed),
        unregister_failures: UNREGISTER_FAILURES.load(Ordering::Relaxed),
        last_unregister_status: LAST_UNREGISTER_STATUS.load(Ordering::Acquire),
        control_flags_refused: CONTROL_FLAGS_REFUSED.load(Ordering::Relaxed),
        provider_rundown_closed_refused: PROVIDER_RUNDOWN_CLOSED_REFUSED.load(Ordering::Relaxed),
        provider_rundown_full_refused: PROVIDER_RUNDOWN_FULL_REFUSED.load(Ordering::Relaxed),
        provider_rundown_contended_refused: PROVIDER_RUNDOWN_CONTENDED_REFUSED
            .load(Ordering::Relaxed),
        adapter_rundown_closed_refused: ADAPTER_RUNDOWN_CLOSED_REFUSED.load(Ordering::Relaxed),
        adapter_rundown_full_refused: ADAPTER_RUNDOWN_FULL_REFUSED.load(Ordering::Relaxed),
        adapter_rundown_contended_refused: ADAPTER_RUNDOWN_CONTENDED_REFUSED
            .load(Ordering::Relaxed),
        adapter_rundown_stale_epoch_refused: ADAPTER_RUNDOWN_STALE_EPOCH_REFUSED
            .load(Ordering::Relaxed),
        adapter_rundown_revalidate_refused: ADAPTER_RUNDOWN_REVALIDATE_REFUSED
            .load(Ordering::Relaxed),
        unregistered_emit_refused: UNREGISTERED_EMIT_REFUSED.load(Ordering::Relaxed),
        reject_index_refused: REJECT_INDEX_REFUSED.load(Ordering::Relaxed),
        write_successes: WRITE_SUCCESSES.load(Ordering::Relaxed),
        write_failures: WRITE_FAILURES.load(Ordering::Relaxed),
        last_write_status: LAST_WRITE_STATUS.load(Ordering::Acquire),
        rejection_counts,
    }
}

pub fn registration_snapshot() -> HeliosEtwRegistrationSnapshot {
    HeliosEtwRegistrationSnapshot {
        attempts: REGISTER_ATTEMPTS.load(Ordering::Relaxed),
        failures: REGISTER_FAILURES.load(Ordering::Relaxed),
        last_status: LAST_REGISTER_STATUS.load(Ordering::Acquire),
        registered: u32::from(REG_HANDLE.load(Ordering::Acquire) != 0),
    }
}

pub unsafe fn driver_unload() {
    crate::kmsg(c"Helios: Unload\n");
    unregister();
    crate::virtio::hal::WdkHal::unmap_all();
}
