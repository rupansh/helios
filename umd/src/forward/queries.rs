//! Queries, predication, multisample quality levels and performance counters.
//!
//! Moved verbatim out of `forward.rs` by T8/R1107.

use super::*;

// Query DDI: WDK 26100 d3d10umddi.h and the Microsoft QueryGetData/CreateQuery
// references. DDI enums are NOT API enums, and the two pipeline result layouts
// differ. Keep the publication rules independently executable on the host.
#[path = "query_contract.rs"]
mod contract;
use contract::{query_spec, QueryError};

/// Immediate handles own a COM word plus their original DDI kind. Deferred
/// aliases copy ONLY the first word (deferred.rs); Begin/End/SetPredication use
/// only that COM identity. GetData is absent from DC tables, and its explicit
/// immediate-context check precedes every read of this metadata.
#[repr(C)]
struct QueryPrivate {
    com_raw: usize,
    ddi_query: ddi::D3D10DDI_QUERY,
}

const _: () = {
    assert!(core::mem::offset_of!(QueryPrivate, com_raw) == 0);
    assert!(core::mem::offset_of!(QueryPrivate, ddi_query) == core::mem::size_of::<usize>());
    assert!(core::mem::size_of::<QueryPrivate>() == 2 * core::mem::size_of::<usize>());
    assert!(core::mem::size_of::<ddi::BOOL>() == 4);
    assert!(core::mem::size_of::<ddi::UINT64>() == 8);
    assert!(core::mem::size_of::<ddi::D3D10_DDI_QUERY_DATA_TIMESTAMP_DISJOINT>() == 16);
    assert!(core::mem::size_of::<D3D11_QUERY_DATA_TIMESTAMP_DISJOINT>() == 16);
    assert!(core::mem::size_of::<ddi::D3D10_DDI_QUERY_DATA_SO_STATISTICS>() == 16);
    assert!(core::mem::size_of::<D3D11_QUERY_DATA_SO_STATISTICS>() == 16);
    assert!(core::mem::size_of::<ddi::D3D10_DDI_QUERY_DATA_PIPELINE_STATISTICS>() == 64);
    assert!(core::mem::size_of::<ddi::D3D11_DDI_QUERY_DATA_PIPELINE_STATISTICS>() == 88);
    assert!(core::mem::size_of::<D3D11_QUERY_DATA_PIPELINE_STATISTICS>() == 88);
    assert!(
        ddi::D3D10_DDI_GET_DATA_FLAG_D3D10_DDI_GET_DATA_DO_NOT_FLUSH
            == D3D11_ASYNC_GETDATA_DONOTFLUSH.0
    );
};

// Prove the platform-neutral enum table against both independently generated
// SDK/WDK constant sets. No cast or arithmetic infers a query's API enum.
macro_rules! check_query_mapping {
    ($($ddi_kind:ident => $api_kind:ident),+ $(,)?) => {
        const _: () = {$(
            match query_spec(ddi::$ddi_kind) {
                Some(spec) => assert!(spec.api_query == $api_kind.0),
                None => panic!("missing WDK query mapping"),
            }
        )+};
    };
}
check_query_mapping!(
    D3D10DDI_QUERY_D3D10DDI_QUERY_EVENT => D3D11_QUERY_EVENT,
    D3D10DDI_QUERY_D3D10DDI_QUERY_OCCLUSION => D3D11_QUERY_OCCLUSION,
    D3D10DDI_QUERY_D3D10DDI_QUERY_TIMESTAMP => D3D11_QUERY_TIMESTAMP,
    D3D10DDI_QUERY_D3D10DDI_QUERY_TIMESTAMPDISJOINT => D3D11_QUERY_TIMESTAMP_DISJOINT,
    D3D10DDI_QUERY_D3D10DDI_QUERY_PIPELINESTATS => D3D11_QUERY_PIPELINE_STATISTICS,
    D3D10DDI_QUERY_D3D10DDI_QUERY_OCCLUSIONPREDICATE => D3D11_QUERY_OCCLUSION_PREDICATE,
    D3D10DDI_QUERY_D3D10DDI_QUERY_STREAMOUTPUTSTATS => D3D11_QUERY_SO_STATISTICS,
    D3D10DDI_QUERY_D3D10DDI_QUERY_STREAMOVERFLOWPREDICATE => D3D11_QUERY_SO_OVERFLOW_PREDICATE,
    D3D10DDI_QUERY_D3D11DDI_QUERY_PIPELINESTATS => D3D11_QUERY_PIPELINE_STATISTICS,
    D3D10DDI_QUERY_D3D11DDI_QUERY_STREAMOUTPUTSTATS_STREAM0 => D3D11_QUERY_SO_STATISTICS_STREAM0,
    D3D10DDI_QUERY_D3D11DDI_QUERY_STREAMOUTPUTSTATS_STREAM1 => D3D11_QUERY_SO_STATISTICS_STREAM1,
    D3D10DDI_QUERY_D3D11DDI_QUERY_STREAMOUTPUTSTATS_STREAM2 => D3D11_QUERY_SO_STATISTICS_STREAM2,
    D3D10DDI_QUERY_D3D11DDI_QUERY_STREAMOUTPUTSTATS_STREAM3 => D3D11_QUERY_SO_STATISTICS_STREAM3,
    D3D10DDI_QUERY_D3D11DDI_QUERY_STREAMOVERFLOWPREDICATE_STREAM0 => D3D11_QUERY_SO_OVERFLOW_PREDICATE_STREAM0,
    D3D10DDI_QUERY_D3D11DDI_QUERY_STREAMOVERFLOWPREDICATE_STREAM1 => D3D11_QUERY_SO_OVERFLOW_PREDICATE_STREAM1,
    D3D10DDI_QUERY_D3D11DDI_QUERY_STREAMOVERFLOWPREDICATE_STREAM2 => D3D11_QUERY_SO_OVERFLOW_PREDICATE_STREAM2,
    D3D10DDI_QUERY_D3D11DDI_QUERY_STREAMOVERFLOWPREDICATE_STREAM3 => D3D11_QUERY_SO_OVERFLOW_PREDICATE_STREAM3,
);

macro_rules! check_query_fields {
    ($ddi_type:ty, $api_type:ty, $($field:ident),+ $(,)?) => {
        const _: () = {$(
            assert!(core::mem::offset_of!($ddi_type, $field) == core::mem::offset_of!($api_type, $field));
        )+};
    };
}
check_query_fields!(
    ddi::D3D10_DDI_QUERY_DATA_PIPELINE_STATISTICS,
    D3D11_QUERY_DATA_PIPELINE_STATISTICS,
    IAVertices,
    IAPrimitives,
    VSInvocations,
    GSInvocations,
    GSPrimitives,
    CInvocations,
    CPrimitives,
    PSInvocations
);
check_query_fields!(
    ddi::D3D11_DDI_QUERY_DATA_PIPELINE_STATISTICS,
    D3D11_QUERY_DATA_PIPELINE_STATISTICS,
    IAVertices,
    IAPrimitives,
    VSInvocations,
    GSInvocations,
    GSPrimitives,
    CInvocations,
    CPrimitives,
    PSInvocations,
    HSInvocations,
    DSInvocations,
    CSInvocations
);
check_query_fields!(
    ddi::D3D10_DDI_QUERY_DATA_TIMESTAMP_DISJOINT,
    D3D11_QUERY_DATA_TIMESTAMP_DISJOINT,
    Frequency,
    Disjoint
);
check_query_fields!(
    ddi::D3D10_DDI_QUERY_DATA_SO_STATISTICS,
    D3D11_QUERY_DATA_SO_STATISTICS,
    NumPrimitivesWritten,
    PrimitivesStorageNeeded
);

static QUERY_CREATE_FAILED: RefusalCounter = RefusalCounter::new("query_create_failed");
static QUERY_INVALID_REQUEST: RefusalCounter = RefusalCounter::new("query_invalid_request");
static QUERY_BACKEND_FAILED: RefusalCounter = RefusalCounter::new("query_backend_failed");

unsafe fn query_failure(
    h: Hdevice,
    counter: &RefusalCounter,
    operation: &str,
    cause: i32,
    report: i32,
) {
    counter.bump();
    // Every failure publishes the named counter and the backend cause, including
    // a process that never reaches DestroyDevice. Pending is normal and uncounted.
    log_error!(
        "DDI queries: {}={} op={} cause=0x{:08x} report=0x{:08x}",
        counter.name(),
        counter.get(),
        operation,
        cause as u32,
        report as u32
    );
    // SAFETY: callback dispatch validates the device tag and uses its core layer.
    unsafe { set_runtime_error(h, report) };
}

pub(crate) unsafe extern "system" fn calc_size_query(
    _h: Hdevice,
    _a: *const ddi::D3D10DDIARG_CREATEQUERY,
) -> ddi::SIZE_T {
    core::mem::size_of::<QueryPrivate>() as ddi::SIZE_T
}

pub(crate) unsafe extern "system" fn create_query(
    h: Hdevice,
    arg: *const ddi::D3D10DDIARG_CREATEQUERY,
    h_query: ddi::D3D10DDI_HQUERY,
    _hrt: ddi::D3D10DDI_HRTQUERY,
) {
    // SAFETY: runtime private slots satisfy their paired CalcPrivate size. Only
    // the first word is touched until the immediate-context tag is established.
    unsafe {
        clear_handle(h_query);
        if arg.is_null()
            || h_query.pDrvPrivate.is_null()
            || !matches!(drv_handle(h), Some(DrvHandle::Device(_)))
        {
            query_failure(
                h,
                &QUERY_INVALID_REQUEST,
                "CreateQuery arguments/context",
                E_INVALIDARG,
                crate::hr::D3DDDIERR_DEVICEREMOVED,
            );
            return;
        }
        let a = &*arg;
        let Some(spec) = query_spec(a.Query) else {
            // STUB: device-dependent/exclusive counters are not advertised or implemented.
            query_failure(
                h,
                &QUERY_INVALID_REQUEST,
                "CreateQuery unsupported DDI kind",
                a.Query,
                crate::hr::D3DDDIERR_DEVICEREMOVED,
            );
            return;
        };
        let Some(device) = d3d11_device(h) else {
            query_failure(
                h,
                &QUERY_BACKEND_FAILED,
                "CreateQuery device",
                E_FAIL,
                crate::hr::D3DDDIERR_DEVICEREMOVED,
            );
            return;
        };
        let desc = D3D11_QUERY_DESC {
            Query: D3D11_QUERY(spec.api_query),
            MiscFlags: a.MiscFlags,
        };
        let mut query = None;
        match device.CreateQuery(&desc, Some(&mut query)) {
            Ok(()) => match query {
                Some(query) if query.GetDataSize() == spec.api_size => {
                    h_query
                        .pDrvPrivate
                        .cast::<QueryPrivate>()
                        .write(QueryPrivate {
                            com_raw: query.into_raw() as usize,
                            ddi_query: a.Query,
                        });
                }
                _ => query_failure(
                    h,
                    &QUERY_CREATE_FAILED,
                    "CreateQuery missing/mismatched result",
                    E_FAIL,
                    crate::hr::D3DDDIERR_DEVICEREMOVED,
                ),
            },
            Err(e) => {
                // CreateQuery allows OOM, NONEXCLUSIVE, or DEVICEREMOVED. In
                // particular E_INVALIDARG must not escape through this DDI.
                let report = if e.code().0 == E_OUTOFMEMORY {
                    E_OUTOFMEMORY
                } else {
                    crate::hr::D3DDDIERR_DEVICEREMOVED
                };
                query_failure(
                    h,
                    &QUERY_CREATE_FAILED,
                    "CreateQuery backend",
                    e.code().0,
                    report,
                );
            }
        }
    }
}

pub(crate) unsafe extern "system" fn destroy_query(_h: Hdevice, h_query: ddi::D3D10DDI_HQUERY) {
    // SAFETY: the immediate slot owns its first COM word. DC aliases use the
    // separate close shim and never release this borrowed identity.
    unsafe { release_com(h_query) };
}

unsafe fn query_async(h: Hdevice, h_query: ddi::D3D10DDI_HQUERY) -> Option<ID3D11Asynchronous> {
    // SAFETY: both immediate and DC query slots contain the same live COM word;
    // runtime ordering keeps the immediate owner alive while a DC borrows it.
    unsafe {
        let Some(query) = load_com::<ID3D11Query>(h_query) else {
            query_failure(
                h,
                &QUERY_INVALID_REQUEST,
                "query handle",
                E_INVALIDARG,
                crate::hr::D3DDDIERR_DEVICEREMOVED,
            );
            return None;
        };
        match (*query).cast::<ID3D11Asynchronous>() {
            Ok(query) => Some(query),
            Err(e) => {
                query_failure(
                    h,
                    &QUERY_BACKEND_FAILED,
                    "query asynchronous interface",
                    e.code().0,
                    crate::hr::D3DDDIERR_DEVICEREMOVED,
                );
                None
            }
        }
    }
}

pub(crate) unsafe extern "system" fn query_begin(h: Hdevice, h_query: ddi::D3D10DDI_HQUERY) {
    // SAFETY: device/context and query resolvers validate handles; the acquired
    // asynchronous reference spans the context call and works on IC and DC.
    unsafe {
        let Some(context) = d3d11_context(h) else {
            query_failure(
                h,
                &QUERY_BACKEND_FAILED,
                "QueryBegin context",
                E_FAIL,
                crate::hr::D3DDDIERR_DEVICEREMOVED,
            );
            return;
        };
        if let Some(query) = query_async(h, h_query) {
            context.Begin(&query);
        }
    }
}

pub(crate) unsafe extern "system" fn query_end(h: Hdevice, h_query: ddi::D3D10DDI_HQUERY) {
    // SAFETY: as query_begin; only the COM word is read for a deferred alias.
    unsafe {
        let Some(context) = d3d11_context(h) else {
            query_failure(
                h,
                &QUERY_BACKEND_FAILED,
                "QueryEnd context",
                E_FAIL,
                crate::hr::D3DDDIERR_DEVICEREMOVED,
            );
            return;
        };
        if let Some(query) = query_async(h, h_query) {
            context.End(&query);
        }
    }
}

pub(crate) unsafe extern "system" fn query_get_data(
    h: Hdevice,
    h_query: ddi::D3D10DDI_HQUERY,
    data: *mut c_void,
    data_size: u32,
    flags: u32,
) {
    // SAFETY: metadata belongs only to immediate query slots. The tag check
    // precedes the read; output slices are formed only for the exact validated
    // DDI size, inside the caller's potentially uninitialized runtime buffer.
    // MaybeUninit accepts that storage without reading it. Backend writes target
    // a separate aligned local buffer and reach the caller only after S_OK.
    unsafe {
        if !matches!(drv_handle(h), Some(DrvHandle::Device(_))) {
            query_failure(
                h,
                &QUERY_INVALID_REQUEST,
                "QueryGetData deferred/invalid context",
                E_INVALIDARG,
                crate::hr::D3DDDIERR_DEVICEREMOVED,
            );
            return;
        }
        let Some(context) = d3d11_context(h) else {
            query_failure(
                h,
                &QUERY_BACKEND_FAILED,
                "QueryGetData context",
                E_FAIL,
                crate::hr::D3DDDIERR_DEVICEREMOVED,
            );
            return;
        };
        let Some(query) = query_async(h, h_query) else {
            return;
        };
        let kind = (*h_query.pDrvPrivate.cast::<QueryPrivate>()).ddi_query;
        let Some(spec) = query_spec(kind) else {
            query_failure(
                h,
                &QUERY_INVALID_REQUEST,
                "QueryGetData query metadata",
                kind,
                crate::hr::D3DDDIERR_DEVICEREMOVED,
            );
            return;
        };
        if data_size != 0 && (data.is_null() || data_size != spec.ddi_size) {
            query_failure(
                h,
                &QUERY_INVALID_REQUEST,
                "QueryGetData output size/pointer",
                E_INVALIDARG,
                crate::hr::D3DDDIERR_DEVICEREMOVED,
            );
            return;
        }
        let output = if data_size == 0 {
            None
        } else {
            Some(core::slice::from_raw_parts_mut(
                data.cast::<core::mem::MaybeUninit<u8>>(),
                data_size as usize,
            ))
        };
        let result = contract::get_data(spec, output, flags, |local, size, flags| {
            let pointer = local.map_or(core::ptr::null_mut(), |local| local.as_mut_ptr());
            // Keep raw HRESULT: windows::Result<()> collapses S_FALSE into Ok.
            (Interface::vtable(&*context).GetData)(
                Interface::as_raw(&*context),
                Interface::as_raw(&query),
                pointer,
                size,
                flags,
            )
            .0
        });
        match result {
            Ok(()) => {} // S_OK is represented by no SetError callback.
            Err(QueryError::Pending) => {
                set_runtime_error(h, crate::hr::DXGI_DDI_ERR_WASSTILLDRAWING)
            }
            Err(QueryError::InvalidRequest) => query_failure(
                h,
                &QUERY_INVALID_REQUEST,
                "QueryGetData flags/size",
                E_INVALIDARG,
                crate::hr::D3DDDIERR_DEVICEREMOVED,
            ),
            Err(QueryError::Backend(hr)) => query_failure(
                h,
                &QUERY_BACKEND_FAILED,
                "QueryGetData backend",
                hr,
                crate::hr::D3DDDIERR_DEVICEREMOVED,
            ),
        }
    }
}

pub(crate) unsafe extern "system" fn set_predication(
    h: Hdevice,
    h_query: ddi::D3D10DDI_HQUERY,
    predicate_value: i32,
) {
    // SAFETY: the first query word is a borrowed COM identity for either
    // context kind. A null handle is the documented predication unbind.
    unsafe {
        let Some(context) = d3d11_context(h) else {
            query_failure(
                h,
                &QUERY_BACKEND_FAILED,
                "SetPredication context",
                E_FAIL,
                crate::hr::D3DDDIERR_DEVICEREMOVED,
            );
            return;
        };
        if h_query.pDrvPrivate.is_null() {
            context.SetPredication(None, predicate_value != 0);
            return;
        }
        let Some(query) = load_com::<ID3D11Query>(h_query) else {
            query_failure(
                h,
                &QUERY_INVALID_REQUEST,
                "SetPredication query handle",
                E_INVALIDARG,
                crate::hr::D3DDDIERR_DEVICEREMOVED,
            );
            return;
        };
        match (*query).cast::<ID3D11Predicate>() {
            Ok(predicate) => context.SetPredication(&predicate, predicate_value != 0),
            Err(e) => query_failure(
                h,
                &QUERY_BACKEND_FAILED,
                "SetPredication unsupported interface",
                e.code().0,
                crate::hr::D3DDDIERR_DEVICEREMOVED,
            ),
        }
    }
}

/// Shared rate cap for the three MSAA log sites (R829).
pub(crate) static MSAA_LOG_COUNT: LogThrottle = LogThrottle::new();

/// D3D11 multisample-quality caps, keyed off the active feature-level profile.
///
/// The Microsoft runtime validates `CheckFormatSupport` and
/// `CheckMultisampleQualityLevels` as a coherent feature-level contract during
/// `CDevice::LLOCompleteLayerConstruction`. The FL10.0 profile expresses a
/// no-multisample device (1x only) coherently with `check_format_support`
/// stripping the multisample bits. The FL11_0 profile advertises 1x, 4x, 8x and
/// the optional standard patterns (2x/16x) for EVERY output-capable format. The
/// runtime rejects arbitrary non-power-of-two sample counts.
///
/// R829 (OWNER DECISION): this doc previously claimed the D3D11.3 §19.2.5
/// exception -- 8x only for output formats *below* 128 bits/sample -- which the
/// code has never implemented. The decision was to correct the DOC, not the
/// code. §19.2.5 is a FLOOR, not a ceiling: a driver may advertise above it,
/// and the caps/quality pair stays internally coherent either way because
/// `check_format_support` uses the SAME
/// `dxgi_msaa_bits_per_sample(fmt, caps).is_some()` predicate.
///
/// What made this a decision rather than a cleanup, and worth knowing before
/// revisiting it: `dxgi_msaa_bits_per_sample` resolves to a static format table
/// plus the DXVK caps word. It never asks whether that SAMPLE COUNT is
/// supported, so today's "8x on a 128-bit format" is a table assertion, not a
/// capability probe. Implementing the floor would narrow the claim; probing
/// DXVK would make it true. Neither is done here -- both are behaviour changes
/// on the default-live FL11 caps path, which this tranche freezes.
pub(crate) unsafe fn helios_multisample_quality_levels(
    h: Hdevice,
    fmt: ddi::DXGI_FORMAT,
    sample_count: u32,
) -> u32 {
    if crate::caps::feature_profile().msaa == crate::caps::MsaaPolicy::SingleSampleOnly {
        return if sample_count == 1 { 1 } else { 0 };
    }
    if sample_count == 0 {
        return 0;
    }
    let Some(device) = d3d11_device(h) else {
        // DXVK unreachable: fall back to the conservative single-sample answer.
        return if sample_count == 1 { 1 } else { 0 };
    };
    let caps = device
        .CheckFormatSupport(DXGI_FORMAT(fmt as i32))
        .unwrap_or(0);
    let output_bits = dxgi_msaa_bits_per_sample(fmt as u32, caps);
    // The two arms were identical -- (1|2|4|16, Some(_)) and (8, Some(_)) both
    // yielding true -- which is what made the doc's 128-bit exception look
    // implemented. Collapsed; `output_bits` is still bound for the log, which
    // is the only thing that ever consumed it.
    let required = matches!((sample_count, output_bits), (1 | 2 | 4 | 8 | 16, Some(_)));
    let val = if required { 1 } else { 0 };
    // DECLARED diagnostic-volume change (R829): this site fired whenever
    // `required || sample_count <= 8`, i.e. on essentially every query, and the
    // two public wrappers below logged unconditionally with no cap at all. All
    // three now share one throttle.
    if (required || sample_count <= 8) && MSAA_LOG_COUNT.first_n_then_every(256, 4096).is_some() {
        trace_line!(
            "MSAA q fmt={fmt} c={sample_count} output_bits={output_bits:?} required={required} -> {val}"
        );
    }
    val
}

pub(crate) unsafe extern "system" fn check_multisample_quality_levels(
    h: Hdevice,
    fmt: ddi::DXGI_FORMAT,
    sample_count: u32,
    out: *mut u32,
) {
    if !out.is_null() {
        let val = helios_multisample_quality_levels(h, fmt, sample_count);
        *out = val;
        if MSAA_LOG_COUNT.first_n_then_every(256, 4096).is_some() {
            trace_line!("MSAA out fmt={fmt} c={sample_count} flags=legacy out={out:p} val={val}");
        }
    }
}

pub(crate) unsafe extern "system" fn check_multisample_quality_levels_wddm1_3(
    h: Hdevice,
    fmt: ddi::DXGI_FORMAT,
    sample_count: u32,
    _flags: u32,
    out: *mut u32,
) {
    if !out.is_null() {
        let val = helios_multisample_quality_levels(h, fmt, sample_count);
        *out = val;
        if MSAA_LOG_COUNT.first_n_then_every(256, 4096).is_some() {
            trace_line!(
                "MSAA out fmt={fmt} c={sample_count} flags=0x{_flags:x} out={out:p} val={val}"
            );
        }
    }
}

/// `pfnCheckCounterInfo` — report the device's performance-counter capabilities.
/// Previously an unimplemented noop that left the out struct unwritten, so the
/// D3D11 runtime read whatever it had pre-set for `NumSimultaneousCounters` /
/// `NumDetectableParallelUnits` (potentially garbage → over-allocation/validation
/// failure during `LLOCompleteLayerConstruction`). We expose no device-dependent
/// counters: zero the struct (LastDeviceDependentCounter = 0, 0 simultaneous
/// counters) and report a single detectable parallel unit. PATH-A (2026-06-22).
pub(crate) unsafe extern "system" fn check_counter_info(
    _h: Hdevice,
    info: *mut ddi::D3D10DDI_COUNTER_INFO,
) {
    if !info.is_null() {
        core::ptr::write_bytes(
            info as *mut u8,
            0,
            core::mem::size_of::<ddi::D3D10DDI_COUNTER_INFO>(),
        );
        (*info).NumDetectableParallelUnits = 1;
    }
}

pub(crate) unsafe extern "system" fn check_counter(
    _h: Hdevice,
    _query: ddi::D3D10DDI_QUERY,
    counter_type: *mut ddi::D3D10DDI_COUNTER_TYPE,
    active_counters: *mut u32,
    _name: ddi::LPSTR,
    name_len: *mut u32,
    _units: ddi::LPSTR,
    units_len: *mut u32,
    _description: ddi::LPSTR,
    description_len: *mut u32,
) {
    if !counter_type.is_null() {
        *counter_type = ddi::D3D10DDI_COUNTER_TYPE_D3D10DDI_COUNTER_TYPE_UINT64;
    }
    if !active_counters.is_null() {
        *active_counters = 0;
    }
    if !name_len.is_null() {
        *name_len = 0;
    }
    if !units_len.is_null() {
        *units_len = 0;
    }
    if !description_len.is_null() {
        *description_len = 0;
    }
}
