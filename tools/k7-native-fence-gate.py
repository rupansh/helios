#!/usr/bin/env python3
"""Static and executable-mutation gate for the dormant K7 native-fence surface."""

from __future__ import annotations

import os
import re
import runpy
import subprocess
import sys
import tempfile
from dataclasses import dataclass
from typing import Callable


REPO_DEFAULT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
D4_PATH = os.path.join(os.path.dirname(__file__), "d4-dirql-gate.py")
D5_PATH = os.path.join(os.path.dirname(__file__), "d5-present-mpo-gate.py")
D4 = runpy.run_path(D4_PATH, run_name="k7_imported_d4_gate")
D5 = runpy.run_path(D5_PATH, run_name="k7_imported_d5_gate")
live_rust: Callable[[str], str] = D4["live_rust"]
functions = D4["functions"]
braced_end = D4["braced_end"]
load_sources: Callable[[str], dict[str, str]] = D4["load_sources"]
d4_check_sources: Callable[[dict[str, str]], list[str]] = D4["check_sources"]
d5_check_sources: Callable[[dict[str, str]], list[str]] = D5["check_sources"]

NATIVE = "kmd_render/src/ddi/native_fence.rs"
ADAPTER = "kmd_render/src/adapter/mod.rs"
QUERY = "kmd_render/src/ddi/query_adapter_info.rs"
LIFECYCLE = "kmd_render/src/ddi/lifecycle.rs"
INTERRUPT = "kmd_render/src/ddi/interrupt.rs"
SUBMIT = "kmd_render/src/ddi/submit_command.rs"
DEVICE = "kmd_render/src/device.rs"
LIB = "kmd_render/src/lib.rs"
SLOT_AUDIT = "kmd_render/src/ddi/wddm32_slot_audit.rs"
LOGIC = "kmd_logic/src/lib.rs"
SURFACE = "kmd_render/src/ddi/wddm_surface.rs"
OWNER = "kmd_render/src/virtio/control_owner.rs"
PRESENT_PACKET = "kmd_render/src/ddi/present_packet.rs"

DIAGNOSTIC_COUNTERS = frozenset(
    {
        "NF_CREATE_OK",
        "NF_CREATE_REJ",
        "NF_OPEN_OK",
        "NF_OPEN_REJ",
        "NF_CLOSE_OK",
        "NF_DESTROY_OK",
        "NF_TEARDOWN_REJ",
        "NF_PDD_REJ",
        "NF_STALE_EPOCH",
        "NF_NOT_ADMITTED",
        "NF_CAPS_OK",
        "NF_CAPS_REJ",
        "NF_MON_UPD",
        "NF_CUR_UPD",
        "NF_UPD_REJ",
        "NF_UPD_BACKWARD",
        "NF_INT_SIGNALED",
        "NF_INT_FAILED",
        "NF_EPOCH_BUMPS",
        "NF_FEATURE_REJ",
        "NF_BUFFER_REJ",
        "NF_CAPS_SIZE_REJ",
        "NF_MISSING_LUID",
        "NF_FOREIGN_ADAPTER",
        "NF_BAD_HANDLE",
        "NF_STALE_GENERATION",
        "NF_FLAGS_REJ",
        "NF_COUNT_OVERFLOW",
        "NF_PREFLIGHT_REJ",
        "NF_LIFECYCLE_REJ",
        "NF_EPOCH_EXHAUSTED",
        "NF_OBJECT_GENERATION_EXHAUSTED",
        "NF_INT_NO_EDGE",
    }
)


def compact(source: str) -> str:
    return re.sub(r"\s+", "", source)


def unique_function(
    sources: dict[str, str], path: str, name: str, errors: list[str]
) -> tuple[str, str] | None:
    raw = sources.get(path, "")
    live = live_rust(raw)
    found = [item for item in functions(live) if item.name == name]
    if len(found) != 1:
        errors.append(f"{path}: expected one {name}, found {len(found)}")
        return None
    item = found[0]
    return raw[item.brace : item.end], live[item.brace : item.end]


def require_fragments(
    path: str, name: str, body: str, fragments: tuple[str, ...], errors: list[str]
) -> None:
    body = compact(body)
    for fragment in fragments:
        if compact(fragment) not in body:
            errors.append(f"{path}:{name}: required K7 fragment missing: {fragment}")


def require_order(
    path: str, name: str, body: str, tokens: tuple[str, ...], errors: list[str]
) -> None:
    body = compact(body)
    positions = [body.find(compact(token)) for token in tokens]
    if any(position < 0 for position in positions) or positions != sorted(positions):
        errors.append(f"{path}:{name}: required K7 order drifted: {' -> '.join(tokens)}")


def function_bodies(
    sources: dict[str, str], requests: tuple[tuple[str, str], ...], errors: list[str]
) -> dict[tuple[str, str], tuple[str, str]]:
    out: dict[tuple[str, str], tuple[str, str]] = {}
    for path, name in requests:
        body = unique_function(sources, path, name, errors)
        if body is not None:
            out[(path, name)] = body
    return out


def check_sources(sources: dict[str, str]) -> list[str]:
    errors: list[str] = []
    live = {path: live_rust(source) for path, source in sources.items()}
    compact_live = {path: compact(source) for path, source in live.items()}

    # K7 is additive to D4/D5. Import their real source checkers so a K7 change
    # cannot make either earlier proof weaker and still pass this gate.
    errors.extend(f"D4: {error}" for error in d4_check_sources(sources))
    errors.extend(f"D5: {error}" for error in d5_check_sources(sources))

    owner_defs = re.findall(
        r"\bconst\s+KMD_D2_OWNER_ENABLED\s*:\s*bool\s*=\s*(true|false)\s*;",
        live.get(OWNER, ""),
    )
    if owner_defs != ["false"]:
        errors.append(f"{OWNER}: K7 requires exactly one false KMD_D2_OWNER_ENABLED")
    if not re.search(
        r"\bconst\s+SURFACE\s*:\s*WddmSurface\s*=\s*WddmSurface::Wddm2_1GpuMmu\s*;",
        live.get(SURFACE, ""),
    ):
        errors.append(f"{SURFACE}: K7 must retain Wddm2_1GpuMmu")

    native = compact_live.get(NATIVE, "")
    advertised = (
        "pub(crate)constNATIVE_FENCE_ADVERTISED:bool="
        "matches!(SURFACE,WddmSurface::Wddm3_2GpuMmu);"
    )
    if native.count(advertised) != 1:
        errors.append(f"{NATIVE}: advertisement must derive solely from SURFACE")
    bool_switches = re.findall(
        r"\bconst\s+([A-Z0-9_]*NATIVE_FENCE[A-Z0-9_]*)\s*:\s*bool\s*=",
        live.get(NATIVE, ""),
    )
    if bool_switches != ["NATIVE_FENCE_ADVERTISED"]:
        errors.append(f"{NATIVE}: independent/decoy native-fence switch found: {bool_switches}")

    # All persistent authority is one ref-counted state owned by the adapter.
    state_match = re.search(
        r"struct\s+NativeFenceAdapterState\s*\{", live.get(NATIVE, "")
    )
    if state_match is None:
        errors.append(f"{NATIVE}: missing NativeFenceAdapterState")
    else:
        brace = live[NATIVE].find("{", state_match.start())
        end = braced_end(live[NATIVE], brace)
        state_body = compact(live[NATIVE][brace:end]) if end is not None else ""
        for fragment in (
            "feature:AtomicU64",
            "luid:AtomicI64",
            "lifecycle:AtomicU32",
            "epoch:AtomicU64",
            "object_generation:AtomicU64",
            "live_global:AtomicU32",
            "live_local:AtomicU32",
            "active_monitored:AtomicU32",
        ):
            if fragment not in state_body:
                errors.append(f"{NATIVE}: per-adapter authority field missing: {fragment}")
    adapter = compact_live.get(ADAPTER, "")
    if (
        "native_fence:Arc<crate::ddi::native_fence::NativeFenceAdapterState>" not in adapter
        or "native_fence:Arc::new(crate::ddi::native_fence::NativeFenceAdapterState::new())"
        not in adapter
    ):
        errors.append(f"{ADAPTER}: adapter does not exclusively own ref-counted K7 authority")
    static_declarations = re.findall(
        r"(?m)^\s*(?:pub(?:\([^)]*\))?\s+)?static\s+([A-Za-z_]\w*)\s*:\s*([^=;]+)",
        live.get(NATIVE, ""),
    )
    static_names = {name for name, _ in static_declarations}
    if static_names != DIAGNOSTIC_COUNTERS:
        errors.append(
            f"{NATIVE}: module globals must be exactly the diagnostic counters; "
            f"added={sorted(static_names - DIAGNOSTIC_COUNTERS)!r}, "
            f"removed={sorted(DIAGNOSTIC_COUNTERS - static_names)!r}"
        )
    for static_name, static_type in static_declarations:
        if static_name in DIAGNOSTIC_COUNTERS and compact(static_type) != "AtomicU32":
            errors.append(f"{NATIVE}: diagnostic static has authority-shaped type: {static_name}")

    forbidden = re.compile(
        r"\b(?:HashMap|BTreeMap|Vec|registry|lookup|scan|ticket|Escape|raw_id|resource_id)\b",
        re.IGNORECASE,
    )
    for match in forbidden.finditer(live.get(NATIVE, "")):
        line = live[NATIVE][: match.start()].count("\n") + 1
        errors.append(f"{NATIVE}:{line}: forbidden discovery/packet mechanism {match.group(0)}")

    requests = (
        (NATIVE, "publish_adapter_luid"),
        (NATIVE, "invalidate_all"),
        (NATIVE, "ensure_feature_admitted"),
        (NATIVE, "query_feature_support"),
        (NATIVE, "native_fence_admitted"),
        (NATIVE, "vidschcaps_native_fence_bits"),
        (NATIVE, "validate_global_for_use"),
        (NATIVE, "dxgkddi_create_native_fence"),
        (NATIVE, "dxgkddi_open_native_fence"),
        (NATIVE, "dxgkddi_close_native_fence"),
        (NATIVE, "dxgkddi_destroy_native_fence"),
        (NATIVE, "dxgkddi_update_monitored_values"),
        (NATIVE, "dxgkddi_update_current_values_from_cpu"),
        (NATIVE, "update_values"),
        (NATIVE, "fill_native_fence_caps"),
        (NATIVE, "signal_native_fence_signaled"),
        (NATIVE, "has_possible_progress_edge"),
        (QUERY, "dxgkddi_query_adapter_info"),
        (QUERY, "query_driver_caps"),
        (LIFECYCLE, "dxgkddi_start_device"),
        (LIFECYCLE, "retire_skipped_stop_transport"),
        (LIFECYCLE, "dxgkddi_stop_device"),
        (LIFECYCLE, "dxgkddi_remove_device"),
        (INTERRUPT, "drain_used_and_complete"),
        (SUBMIT, "notify_at_dirql"),
        (SUBMIT, "dxgkddi_reset_from_timeout"),
        (SUBMIT, "dxgkddi_restart_from_timeout"),
    )
    bodies = function_bodies(sources, requests, errors)

    def body(path: str, name: str) -> str:
        pair = bodies.get((path, name))
        return pair[1] if pair is not None else ""

    publish = body(NATIVE, "publish_adapter_luid")
    require_fragments(
        NATIVE,
        "publish_adapter_luid",
        publish,
        (
            "let state = adapter.native_fence.as_ref();",
            "nf::next_epoch(generation, MAX_ADAPTER_GENERATION)",
            "let exact = ((luid.HighPart as i64) << 32) | luid.LowPart as i64;",
            "state.luid.store(exact, Ordering::Release);",
            "feature_word(next, FEATURE_UNKNOWN)",
            "state.lifecycle.store(LIFECYCLE_ACTIVE, Ordering::Release);",
        ),
        errors,
    )
    start = body(LIFECYCLE, "dxgkddi_start_device")
    start_compact = compact(start)
    exact_luid_call = (
        "crate::ddi::native_fence::publish_adapter_luid(adapter,unsafe{"
        "(*dxgk_start_info).AdapterLuid});"
    )
    if start_compact.count(exact_luid_call) != 1:
        errors.append(f"{LIFECYCLE}: StartDevice must publish its exact AdapterLuid once")
    if sum(source.count("publish_adapter_luid(") for source in compact_live.values()) != 2:
        errors.append(f"{NATIVE}: AdapterLuid must have one definition and one StartDevice caller")

    ensure = body(NATIVE, "ensure_feature_admitted")
    require_fragments(
        NATIVE,
        "ensure_feature_admitted",
        ensure,
        (
            "!NATIVE_FENCE_ADVERTISED",
            "!crate::virtio::KMD_D2_OWNER_ENABLED",
            "!adapter.native_fence.lifecycle_active()",
            "FEATURE_QUERYING",
            "FEATURE_DECLINED",
            "compare_exchange(observed, querying, Ordering::AcqRel, Ordering::Acquire)",
            "let enabled = unsafe { query_feature_support(adapter) };",
            "enabled && published",
        ),
        errors,
    )
    feature = body(NATIVE, "query_feature_support")
    require_fragments(
        NATIVE,
        "query_feature_support",
        feature,
        (
            "dxgkrnl.DxgkCbQueryFeatureSupport",
            "args.DeviceHandle = dxgkrnl.DeviceHandle;",
            "args.FeatureId = _DXGK_FEATURE_ID::DXGK_FEATURE_NATIVE_FENCE;",
            "args.DriverSupportState = DXGK_FEATURE_SUPPORT_STABLE_VALUE;",
            "args.Enabled = 0;",
            "let status = unsafe { query(&mut args) };",
            "status == STATUS_SUCCESS",
            "args.DeviceHandle == dxgkrnl.DeviceHandle",
            "args.DriverSupportState == DXGK_FEATURE_SUPPORT_STABLE_VALUE",
            "args.Enabled == 1",
        ),
        errors,
    )
    admitted = compact(body(NATIVE, "native_fence_admitted"))
    admitted_call = (
        "nf::surface_is_admitted(NATIVE_FENCE_ADVERTISED,"
        "crate::virtio::KMD_D2_OWNER_ENABLED,state.feature_enabled(),"
        "state.current_generation()!=0,state.lifecycle_active(),)"
    )
    if admitted_call not in admitted:
        errors.append(f"{NATIVE}: native-fence admission is not the exact five-way conjunction")
    bits = compact(body(NATIVE, "vidschcaps_native_fence_bits"))
    if (
        "ifunsafe{ensure_feature_admitted(adapter)}"
        "&&native_fence_admitted(adapter.native_fence.as_ref())"
        "{VIDSCHCAPS_NATIVE_GPU_FENCE}else{0}" not in bits
        or "VIDSCHCAPS_NO_64BIT_ATOMICS" in bits
        or "VIDSCHCAPS_OPTIMIZED_NATIVE_FENCE_INTERRUPT" in bits
    ):
        errors.append(f"{NATIVE}: NativeGpuFence bits are not exact admitted-only publication")

    query_entry = compact(body(QUERY, "dxgkddi_query_adapter_info"))
    if (
        "DXGKQAITYPE_NATIVE_FENCE_CAPS=>unsafe{"
        "crate::ddi::native_fence::fill_native_fence_caps(adapter,args)}" not in query_entry
    ):
        errors.append(f"{QUERY}: missing exact native-fence caps query arm")
    driver_caps = compact(body(QUERY, "query_driver_caps"))
    assignment = re.search(r"letscheduling_caps:UINT=([^;]+);", driver_caps)
    expected_caps = (
        "SCHEDULINGCAPS_MULTI_ENGINE_AWARE|SCHEDULINGCAPS_PREEMPTION_AWARE|"
        "unsafe{crate::ddi::native_fence::vidschcaps_native_fence_bits(adapter)}"
    )
    if assignment is None or assignment.group(1) != expected_caps:
        errors.append(f"{QUERY}: SchedulingCaps has a partial or unsupported publication")

    caps = body(NATIVE, "fill_native_fence_caps")
    require_fragments(
        NATIVE,
        "fill_native_fence_caps",
        caps,
        (
            "args.pOutputData.is_null()",
            "!(args.pOutputData as *mut DXGK_NATIVE_FENCE_CAPS).is_aligned()",
            "args.OutputDataSize as usize != size_of::<DXGK_NATIVE_FENCE_CAPS>()",
            "ensure_feature_admitted(adapter)",
            "native_fence_admitted(adapter.native_fence.as_ref())",
            "core::mem::zeroed::<DXGK_NATIVE_FENCE_CAPS>()",
            "caps.MonitoredValuePadding = 0;",
            "caps.MapToGpuSystemProcess = 0;",
            "caps.MinimumAddress = NATIVE_FENCE_MINIMUM_ADDRESS;",
            "caps.MaximumAddress = NATIVE_FENCE_MAXIMUM_ADDRESS;",
            "(args.pOutputData as *mut DXGK_NATIVE_FENCE_CAPS).write(caps)",
        ),
        errors,
    )
    require_order(
        NATIVE,
        "fill_native_fence_caps",
        caps,
        (
            "args.pOutputData.is_null()",
            "args.OutputDataSize as usize != size_of::<DXGK_NATIVE_FENCE_CAPS>()",
            "ensure_feature_admitted(adapter)",
            "core::mem::zeroed::<DXGK_NATIVE_FENCE_CAPS>()",
            ".write(caps)",
        ),
        errors,
    )
    if "caps.Reserved" in caps:
        errors.append(f"{NATIVE}: native caps reserved bytes must remain zero-filled")
    for fragment in (
        "pub(crate)constNATIVE_FENCE_MINIMUM_ADDRESS:u64=0;",
        "pub(crate)constNATIVE_FENCE_MAXIMUM_ADDRESS:u64=(1u64<<crate::ddi::gpummu::VIRTUAL_ADDRESS_BIT_COUNT)-1;",
        "VIDSCHCAPS_NATIVE_GPU_FENCE&(VIDSCHCAPS_NO_64BIT_ATOMICS|VIDSCHCAPS_OPTIMIZED_NATIVE_FENCE_INTERRUPT)==0",
    ):
        if fragment not in native:
            errors.append(f"{NATIVE}: native caps constant/proof drifted: {fragment}")

    # Six callbacks are the package; native logs remain NULL and HWQueue bits
    # remain absent from SchedulingCaps.
    lib = compact_live.get(LIB, "")
    callbacks = (
        "CreateNativeFence",
        "DestroyNativeFence",
        "OpenNativeFence",
        "CloseNativeFence",
        "UpdateMonitoredValues",
        "UpdateCurrentValuesFromCpu",
    )
    for callback in callbacks:
        assignment = f"data.DxgkDdi{callback}=Some(ddi::dxgkddi_{re.sub(r'(?<!^)(?=[A-Z])', '_', callback).lower()});"
        if lib.count(assignment) != 1:
            errors.append(f"{LIB}: native-fence callback package drifted at {callback}")
    for callback in ("SetNativeFenceLogBuffer", "UpdateNativeFenceLogs"):
        if re.search(rf"data\.DxgkDdi{callback}=Some\(", lib):
            errors.append(f"{LIB}: {callback} must remain NULL/Disabled")
    slot_audit = compact(sources.get(SLOT_AUDIT, ""))
    for callback in ("DxgkDdiSetNativeFenceLogBuffer", "DxgkDdiUpdateNativeFenceLogs"):
        pattern = rf'name:"{callback}".*?class:SlotClass::Disabled'
        if not re.search(pattern, slot_audit):
            errors.append(f"{SLOT_AUDIT}: {callback} must remain classified Disabled")

    logic = compact_live.get(LOGIC, "")
    hnf1_fragments = (
        "pubconstHNF1_MAGIC:u32=0x3146_4e48",
        "pubconstHNF1_ABI_VERSION:u16=1",
        "pubconstHNF1_SIZE:usize=64",
        "pubconstOFF_MAGIC:usize=0",
        "pubconstOFF_ABI_VERSION:usize=4",
        "pubconstOFF_STRUCT_SIZE:usize=6",
        "pubconstOFF_PACKAGE_GENERATION:usize=8",
        "pubconstOFF_OBJECT_GENERATION:usize=16",
        "pubconstOFF_NATIVE_TYPE:usize=24",
        "pubconstOFF_FLAGS:usize=28",
        "pubconstOFF_ADAPTER_LUID:usize=32",
        "pubconstOFF_RESERVED:usize=40",
        "pubconstRESERVED_LEN:usize=24",
        "rd_u32(bytes,OFF_MAGIC)!=HNF1_MAGIC",
        "rd_u16(bytes,OFF_ABI_VERSION)!=HNF1_ABI_VERSION",
        "rd_u16(bytes,OFF_STRUCT_SIZE)asusize!=HNF1_SIZE",
        "parsed.package_generation!=package_generation",
        "parsed.flags&HNF1_FLAGS_RESERVED_MASK!=0",
        "parsed.object_generation!=0",
        "bytes[OFF_RESERVED+i]!=0",
        "parsed.native_type!=native_type",
        "parsed.flags!=flags",
        "parsed.adapter_luid!=adapter_luid",
        "letmutout=[0u8;HNF1_SIZE]",
        "native_type==NATIVE_FENCE_TYPE_DEFAULT",
        "previous.checked_add(1).filter(|next|*next!=0)",
        "previous.checked_add(1).filter(|next|*next<=maximum)",
    )
    for fragment in hnf1_fragments:
        if fragment not in logic:
            errors.append(f"{LOGIC}: HNF1/generation invariant missing: {fragment}")
    type_body = unique_function(sources, LOGIC, "native_type_is_documented", errors)
    if type_body is not None and "NATIVE_FENCE_TYPE_INTRA_GPU" in type_body[1]:
        errors.append(f"{LOGIC}: INTRA_GPU accepted without a fence-storage allocation path")
    for fragment in (
        "offset_of!(DXGKARG_CREATENATIVEFENCE,Flags)-offset_of!(DXGKARG_CREATENATIVEFENCE,pPrivateDriverData)==nf::HNF1_SIZE",
        "offset_of!(DXGKARG_OPENNATIVEFENCE,Reserved)-offset_of!(DXGKARG_OPENNATIVEFENCE,pPrivateDriverData)==nf::HNF1_SIZE",
        "nf::NATIVE_FENCE_TYPE_DEFAULT==crate::dxgk::_D3DDDI_NATIVEFENCE_TYPE::D3DDDI_NATIVEFENCE_TYPE_DEFAULTasu32",
        "nf::NATIVE_FENCE_TYPE_INTRA_GPU==crate::dxgk::_D3DDDI_NATIVEFENCE_TYPE::D3DDDI_NATIVEFENCE_TYPE_INTRA_GPUasu32",
    ):
        if fragment not in native:
            errors.append(f"{NATIVE}: exact 64-byte WDK PDD proof missing")

    global_match = re.search(r"struct\s+GlobalFenceObject\s*\{", live.get(NATIVE, ""))
    global_body = ""
    if global_match is not None:
        brace = live[NATIVE].find("{", global_match.start())
        end = braced_end(live[NATIVE], brace)
        global_body = compact(live[NATIVE][brace:end]) if end is not None else ""
    if (
        "authority:Arc<NativeFenceAdapterState>" not in global_body
        or "adapter:*constAdapterContext" in global_body
        or "Arc::clone(&adapter.native_fence)" not in native
    ):
        errors.append(f"{NATIVE}: driver handle lacks direct strong per-adapter authority")

    create = body(NATIVE, "dxgkddi_create_native_fence")
    require_fragments(
        NATIVE,
        "dxgkddi_create_native_fence",
        create,
        (
            "h_adapter.is_null()",
            "!(h_adapter as *const AdapterContext).is_aligned()",
            "p_create.is_null()",
            "!p_create.is_aligned()",
            "nf::validate_create(",
            "args.Flags.__bindgen_anon_1.Value",
            "args.hGlobalNativeFence.is_null()",
            "!all_zero(&args.Reserved)",
            "args.CurrentValueSystemProcessGpuVa != 0",
            "args.MonitoredValueSystemProcessGpuVa != 0",
            "reserve_object_generation()",
            "admit(&adapter.native_fence.live_global, nf::MAX_LIVE_GLOBAL)",
            "validate_global_for_use(&object, Some(adapter.native_fence.as_ref()))",
            "args.pPrivateDriverData = nf::encode(",
        ),
        errors,
    )
    opened = body(NATIVE, "dxgkddi_open_native_fence")
    require_fragments(
        NATIVE,
        "dxgkddi_open_native_fence",
        opened,
        (
            "validate_global_for_use(global, Some(adapter.native_fence.as_ref()))",
            "nf::validate_open(",
            "global.adapter_luid",
            "global.native_type",
            "global.flags",
            "DeviceHandleRef::from_raw(args.hDevice)",
            "core::ptr::eq(device_adapter, adapter)",
            "args.hLocalNativeFence.is_null()",
            "!all_zero(&args.Reserved)",
            "fence_gpuva_is_supported(args.CurrentValueGpuVa)",
            "fence_gpuva_is_supported(args.MonitoredValueGpuVa)",
            "args.CurrentValueGpuVa == args.MonitoredValueGpuVa",
            "global.flags & nf::HNF1_FLAG_SHARED == 0",
            "admit(&adapter.native_fence.live_local, nf::MAX_LIVE_LOCAL)",
            "global.local_refs.fetch_add(1, Ordering::AcqRel)",
        ),
        errors,
    )
    close = body(NATIVE, "dxgkddi_close_native_fence")
    require_fragments(
        NATIVE,
        "dxgkddi_close_native_fence",
        close,
        (
            "args.Flags.__bindgen_anon_1.Value",
            "all_zero(&args.Reserved)",
            "core::ptr::eq(global_ref.authority.as_ref(), adapter.native_fence.as_ref(),",
            "global_ref.adapter_generation != local_ref.adapter_generation",
            "global_ref.object_generation != local_ref.object_generation",
            "global_ref.epoch != local_ref.epoch",
        ),
        errors,
    )
    destroy = body(NATIVE, "dxgkddi_destroy_native_fence")
    if "STATE_DRAINING" in destroy or "free_global(" not in destroy:
        errors.append(f"{NATIVE}: destroy must rely on OS lifetime and fail closed on outstanding locals")

    update = body(NATIVE, "update_values")
    update_compact = compact(update)
    require_order(
        NATIVE,
        "update_values",
        update,
        (
            "if !nf::update_count_is_bounded(count)",
            "if count == 0",
            "let count = count as usize",
            "checked_mul(size_of::<HANDLE>())",
            "handles.is_null()",
            "while i < count",
        ),
        errors,
    )
    if "core::slice" in update_compact or "from_raw_parts" in update_compact:
        errors.append(f"{NATIVE}: update path must not construct an unbounded slice")
    loop_matches = list(re.finditer(r"while\s+i\s*<\s*count\s*\{", update))
    if len(loop_matches) != 2:
        errors.append(f"{NATIVE}: update path must have one validation and one mutation pass")
    else:
        first_brace = update.find("{", loop_matches[0].start())
        first_end = braced_end(update, first_brace)
        second_brace = update.find("{", loop_matches[1].start())
        second_end = braced_end(update, second_brace)
        first = compact(update[first_brace:first_end]) if first_end is not None else ""
        second = compact(update[second_brace:second_end]) if second_end is not None else ""
        for fragment in (
            "values.add(i).read()",
            "global_from_handle(handle)",
            "validate_global_for_use(global,None)",
            "slot.is_null()",
            "slot.cast::<u64>().is_aligned()",
        ):
            if fragment not in first:
                errors.append(f"{NATIVE}: complete update preflight missing: {fragment}")
        if any(token in first for token in (".swap(", "write_volatile", "active_monitored")):
            errors.append(f"{NATIVE}: update mutates before complete preflight")
        for fragment in (
            ".swap(value,Ordering::AcqRel)",
            "slot.cast::<u64>().write_volatile(value)",
        ):
            if fragment not in second:
                errors.append(f"{NATIVE}: second-pass update publication missing: {fragment}")
        if second.find("slot.cast::<u64>().write_volatile(value)") > second.find(
            ".swap(value,Ordering::AcqRel)"
        ):
            errors.append(f"{NATIVE}: storage publication must precede diagnostic mirrors")
    for ddi in ("dxgkddi_update_monitored_values", "dxgkddi_update_current_values_from_cpu"):
        ddi_body = body(NATIVE, ddi)
        if "p_args.is_null()" not in ddi_body or "all_zero(&args.Reserved)" not in ddi_body:
            errors.append(f"{NATIVE}:{ddi}: malformed/reserved input is not closed")

    invalidation_sites = (
        (LIFECYCLE, "retire_skipped_stop_transport", "NativeFenceInvalidation::StopOrRemove"),
        (LIFECYCLE, "dxgkddi_stop_device", "NativeFenceInvalidation::StopOrRemove"),
        (LIFECYCLE, "dxgkddi_remove_device", "NativeFenceInvalidation::StopOrRemove"),
        (SUBMIT, "dxgkddi_reset_from_timeout", "NativeFenceInvalidation::Reset"),
    )
    for path, name, boundary in invalidation_sites:
        site = body(path, name)
        require_order(
            path,
            name,
            site,
            (
                "crate::ddi::native_fence::invalidate_all(",
                boundary,
                "crate::adapter::allocation_object::invalidate_all();",
            ),
            errors,
        )
    calls = sum(
        source.count("crate::ddi::native_fence::invalidate_all(")
        for source in compact_live.values()
    )
    if calls != 4:
        errors.append(f"{NATIVE}: expected exactly four paired invalidation sites, found {calls}")
    invalidate = body(NATIVE, "invalidate_all")
    require_order(
        NATIVE,
        "invalidate_all",
        invalidate,
        (
            "state.lifecycle.store(",
            "state.epoch.fetch_update(",
            "state.active_monitored.store(0, Ordering::Release)",
        ),
        errors,
    )
    if "nf::next_epoch(previous,u64::MAX)" not in compact(invalidate):
        errors.append(f"{NATIVE}: invalidation epoch can wrap or reuse stale objects")
    if "crate::ddi::native_fence::resume_after_reset(adapter);" not in compact(
        body(SUBMIT, "dxgkddi_restart_from_timeout")
    ):
        errors.append(f"{SUBMIT}: successful restart does not reopen the reset lifecycle")

    drain = body(INTERRUPT, "drain_used_and_complete")
    require_fragments(
        INTERRUPT,
        "drain_used_and_complete",
        drain,
        (
            "let mut completed_wddm_submission = false;",
            "if status == STATUS_SUCCESS {",
            "completed_wddm_submission = true;",
            "if completed_wddm_submission && super::native_fence::has_possible_progress_edge(adapter)",
            "super::native_fence::signal_native_fence_signaled(adapter, dxgkrnl)",
        ),
        errors,
    )
    signal = body(NATIVE, "signal_native_fence_signaled")
    require_fragments(
        NATIVE,
        "signal_native_fence_signaled",
        signal,
        (
            "if !has_possible_progress_edge(adapter)",
            "DXGK_INTERRUPT_NATIVE_FENCE_SIGNALED",
            "arm.SignaledNativeFenceCount = 0;",
            "arm.pSignaledNativeFenceArray = core::ptr::null_mut();",
            "arm.hHWQueue = core::ptr::null_mut();",
            "super::submit_command::notify_at_dirql(dxgkrnl, &mut interrupt, false)",
        ),
        errors,
    )
    possible = compact(body(NATIVE, "has_possible_progress_edge"))
    if (
        "native_fence_admitted(state)" not in possible
        or "state.live_global.load(Ordering::Acquire)!=0" not in possible
        or "state.active_monitored.load(Ordering::Acquire)!=0" not in possible
    ):
        errors.append(f"{NATIVE}: interrupt edge is not correlated to current monitored population")
    if "DxgkCbNotifyInterrupt" in live.get(NATIVE, ""):
        errors.append(f"{NATIVE}: native interrupt bypasses the shared DIRQL helper")
    notify = compact(body(SUBMIT, "notify_at_dirql"))
    if (
        "DxgkCbSynchronizeExecution" not in notify
        or "DxgkCbNotifyInterrupt.is_none()" not in notify
        or "DxgkCbQueueDpc.is_none()" not in notify
    ):
        errors.append(f"{SUBMIT}: shared native/DMA notify helper lost its DIRQL contract")
    native_signal_calls = sum(
        source.count("signal_native_fence_signaled(") for source in compact_live.values()
    )
    if native_signal_calls != 2:
        errors.append(f"{NATIVE}: native interrupt must have one definition and one correlated caller")

    counter_symbols = (
        "NF_FEATURE_REJ",
        "NF_BUFFER_REJ",
        "NF_CAPS_SIZE_REJ",
        "NF_MISSING_LUID",
        "NF_FOREIGN_ADAPTER",
        "NF_BAD_HANDLE",
        "NF_STALE_GENERATION",
        "NF_FLAGS_REJ",
        "NF_COUNT_OVERFLOW",
        "NF_PREFLIGHT_REJ",
        "NF_LIFECYCLE_REJ",
        "NF_EPOCH_EXHAUSTED",
        "NF_OBJECT_GENERATION_EXHAUSTED",
        "NF_INT_NO_EDGE",
    )
    for counter in counter_symbols:
        if native.count(f"pubstatic{counter}:AtomicU32=AtomicU32::new(0);") != 1:
            errors.append(f"{NATIVE}: typed K7 refusal counter missing: {counter}")

    return errors


@dataclass(frozen=True)
class Mutation:
    name: str
    path: str
    old: str
    new: str


def mutation_cases() -> tuple[Mutation, ...]:
    return (
        Mutation("raise SURFACE", SURFACE, "WddmSurface::Wddm2_1GpuMmu", "WddmSurface::Wddm3_2GpuMmu"),
        Mutation("enable D2 owner", OWNER, "KMD_D2_OWNER_ENABLED: bool = false", "KMD_D2_OWNER_ENABLED: bool = true"),
        Mutation("decoy activation switch", NATIVE, "pub(crate) const NATIVE_FENCE_ADVERTISED", "const NATIVE_FENCE_ENABLED: bool = true;\npub(crate) const NATIVE_FENCE_ADVERTISED"),
        Mutation("bypass caps admission", NATIVE, "    let admitted = unsafe { ensure_feature_admitted(adapter) }\n        && native_fence_admitted(adapter.native_fence.as_ref());", "    let admitted = true;"),
        Mutation("unaligned caps output", NATIVE, "        || !(args.pOutputData as *mut DXGK_NATIVE_FENCE_CAPS).is_aligned()\n", ""),
        Mutation("nonzero caps reserved", NATIVE, "    caps.MonitoredValuePadding = 0;", "    caps.Reserved[0] = 1;\n    caps.MonitoredValuePadding = 0;"),
        Mutation("unsupported caps mapping", NATIVE, "    caps.MapToGpuSystemProcess = 0;", "    caps.MapToGpuSystemProcess = 1;"),
        Mutation("unsupported caps range", NATIVE, "pub(crate) const NATIVE_FENCE_MINIMUM_ADDRESS: u64 = 0;", "pub(crate) const NATIVE_FENCE_MINIMUM_ADDRESS: u64 = 8;"),
        Mutation("remove NativeGpuFence publication", QUERY, "        | unsafe { crate::ddi::native_fence::vidschcaps_native_fence_bits(adapter) };", ";"),
        Mutation("enable No64BitAtomics", NATIVE, "        VIDSCHCAPS_NATIVE_GPU_FENCE\n", "        VIDSCHCAPS_NATIVE_GPU_FENCE | VIDSCHCAPS_NO_64BIT_ATOMICS\n"),
        Mutation("enable optimized interrupt", NATIVE, "        VIDSCHCAPS_NATIVE_GPU_FENCE\n", "        VIDSCHCAPS_NATIVE_GPU_FENCE | VIDSCHCAPS_OPTIMIZED_NATIVE_FENCE_INTERRUPT\n"),
        Mutation("enable hardware queues", QUERY, "        | SCHEDULINGCAPS_PREEMPTION_AWARE\n", "        | SCHEDULINGCAPS_PREEMPTION_AWARE | (1 << 7)\n"),
        Mutation("register native log callback", LIB, "    data.DxgkDdiUpdateCurrentValuesFromCpu = Some(ddi::dxgkddi_update_current_values_from_cpu);", "    data.DxgkDdiUpdateCurrentValuesFromCpu = Some(ddi::dxgkddi_update_current_values_from_cpu);\n    data.DxgkDdiSetNativeFenceLogBuffer = Some(ddi::dxgkddi_set_native_fence_log_buffer);"),
        Mutation("fabricate AdapterLuid", LIFECYCLE, "        (*dxgk_start_info).AdapterLuid", "        LUID { LowPart: 1, HighPart: 0 }"),
        Mutation("module-global authority", NATIVE, "pub static NF_CREATE_OK", "static NF_EPOCH_AUTHORITY: AtomicU64 = AtomicU64::new(1);\npub static NF_CREATE_OK"),
        Mutation("weaken HNF1 layout", LOGIC, "pub const HNF1_SIZE: usize = 64;", "pub const HNF1_SIZE: usize = 63;"),
        Mutation("weaken HNF1 package generation", LOGIC, "if parsed.package_generation != package_generation {", "if false {"),
        Mutation("weaken HNF1 object generation", LOGIC, "if parsed.object_generation != 0 {", "if false {"),
        Mutation("weaken HNF1 type", LOGIC, "native_type == NATIVE_FENCE_TYPE_DEFAULT", "true"),
        Mutation("weaken HNF1 flags", LOGIC, "if parsed.flags & HNF1_FLAGS_RESERVED_MASK != 0 {", "if false {"),
        Mutation("weaken HNF1 reserved tail", LOGIC, "if bytes[OFF_RESERVED + i] != 0 {", "if false {"),
        Mutation("add handle registry", NATIVE, "const GLOBAL_MAGIC", "static FENCE_TABLE: AtomicU64 = AtomicU64::new(0);\nfn lookup(handle: u64) -> u64 { handle }\nconst GLOBAL_MAGIC"),
        Mutation("remove update bound", NATIVE, "    if !nf::update_count_is_bounded(count) {", "    if false {"),
        Mutation("mutate during update preflight", NATIVE, "        // SAFETY: dxgkrnl returns only driver handles this module assigned.\n        let Some(global)", "        unsafe { slot.cast::<u64>().write_volatile(0) };\n        // SAFETY: dxgkrnl returns only driver handles this module assigned.\n        let Some(global)"),
        Mutation("wrap object generation", LOGIC, "previous.checked_add(1).filter(|next| *next != 0)", "Some(previous.wrapping_add(1))"),
        Mutation("interrupt every completion", INTERRUPT, "if completed_wddm_submission && super::native_fence::has_possible_progress_edge(adapter)", "if super::native_fence::has_possible_progress_edge(adapter)"),
        Mutation("name unsafe interrupt subset", NATIVE, "    arm.SignaledNativeFenceCount = 0;", "    arm.SignaledNativeFenceCount = 1;"),
        Mutation("set hardware queue on interrupt", NATIVE, "    arm.hHWQueue = core::ptr::null_mut();", "    arm.hHWQueue = 1usize as HANDLE;"),
        Mutation("bypass shared DIRQL helper", NATIVE, "        super::submit_command::notify_at_dirql(dxgkrnl, &mut interrupt, false)", "        dxgkrnl.DxgkCbNotifyInterrupt.unwrap()(dxgkrnl.DeviceHandle, &mut interrupt); STATUS_SUCCESS"),
        Mutation("reorder reset invalidation", SUBMIT, "    crate::ddi::native_fence::invalidate_all(\n        adapter,\n        crate::ddi::native_fence::NativeFenceInvalidation::Reset,\n    );\n    crate::adapter::allocation_object::invalidate_all();", "    crate::adapter::allocation_object::invalidate_all();\n    crate::ddi::native_fence::invalidate_all(\n        adapter,\n        crate::ddi::native_fence::NativeFenceInvalidation::Reset,\n    );"),
        Mutation("remove skipped-stop invalidation", LIFECYCLE, "    crate::ddi::native_fence::invalidate_all(\n        adapter,\n        crate::ddi::native_fence::NativeFenceInvalidation::StopOrRemove,\n    );\n    crate::adapter::allocation_object::invalidate_all();\n    adapter\n        .isr_status", "    crate::adapter::allocation_object::invalidate_all();\n    adapter\n        .isr_status"),
        Mutation("remove StopDevice invalidation", LIFECYCLE, "        crate::ddi::native_fence::invalidate_all(\n            adapter,\n            crate::ddi::native_fence::NativeFenceInvalidation::StopOrRemove,\n        );\n        crate::adapter::allocation_object::invalidate_all();\n        // Stop the ISR", "        crate::adapter::allocation_object::invalidate_all();\n        // Stop the ISR"),
        Mutation("remove RemoveDevice invalidation", LIFECYCLE, "        crate::ddi::native_fence::invalidate_all(\n            adapter,\n            crate::ddi::native_fence::NativeFenceInvalidation::StopOrRemove,\n        );\n        crate::adapter::allocation_object::invalidate_all();\n        if crate::virtio::KMD_D2_OWNER_ENABLED", "        crate::adapter::allocation_object::invalidate_all();\n        if crate::virtio::KMD_D2_OWNER_ENABLED"),
        Mutation("weaken D4 layout", PRESENT_PACKET, "pub(crate) const PRESENT_FLIP_PRIVATE_OFFSET: usize = 32;", "pub(crate) const PRESENT_FLIP_PRIVATE_OFFSET: usize = 16;"),
        Mutation("weaken D5 plane bound", PRESENT_PACKET, "const MPO_MAX_PLANES: u32 = 1;", "const MPO_MAX_PLANES: u32 = 2;"),
    )


def write_source_tree(root: str, sources: dict[str, str]) -> None:
    for relative, source in sources.items():
        path = os.path.join(root, relative)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w", encoding="utf-8") as stream:
            stream.write(source)


def run_mutations(sources: dict[str, str]) -> None:
    for case in mutation_cases():
        if sources[case.path].count(case.old) != 1:
            raise SystemExit(
                f"K7 mutation setup failed for {case.name}: expected one anchor in {case.path}"
            )
        mutated = dict(sources)
        mutated[case.path] = mutated[case.path].replace(case.old, case.new, 1)
        with tempfile.TemporaryDirectory(prefix="helios-k7-gate-") as temp:
            write_source_tree(temp, mutated)
            result = subprocess.run(
                [sys.executable, os.path.abspath(__file__), temp],
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                check=False,
            )
            if result.returncode == 0:
                raise SystemExit(f"K7 mutation was accepted by the real gate: {case.name}")
    print(f"OK: {len(mutation_cases())} K7 temporary-tree mutations rejected by the real gate")


def main() -> None:
    args = sys.argv[1:]
    mutations = False
    if "--mutations" in args:
        mutations = True
        args.remove("--mutations")
    if len(args) > 1:
        raise SystemExit("usage: k7-native-fence-gate.py [repo] [--mutations]")
    repo = os.path.abspath(args[0]) if args else REPO_DEFAULT
    sources = load_sources(repo)
    errors = check_sources(sources)
    if errors:
        raise SystemExit("dormant K7 native-fence gate violated:\n" + "\n".join(errors))
    if mutations:
        run_mutations(sources)
    else:
        print(
            "OK: dormant K7 admission, caps, handle lifetime, updates, interrupts, "
            "lifecycle, and D4/D5 closure are statically enforced"
        )


if __name__ == "__main__":
    main()
