#!/usr/bin/env python3
"""D9 WDDM 3.2 activation-coherence and executable-mutation gate."""

from __future__ import annotations

import collections
import os
import re
import runpy
import subprocess
import sys
import tempfile
from dataclasses import dataclass
from typing import Callable


REPO_DEFAULT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
K7_PATH = os.path.join(os.path.dirname(__file__), "k7-native-fence-gate.py")
K7 = runpy.run_path(K7_PATH, run_name="d9_imported_k7_gate")
live_rust: Callable[[str], str] = K7["live_rust"]
load_rust_sources: Callable[[str], dict[str, str]] = K7["load_sources"]
k7_check_sources: Callable[[dict[str, str]], list[str]] = K7["check_sources"]
unique_function = K7["unique_function"]

SURFACE = "kmd_render/src/ddi/wddm_surface.rs"
OWNER = "kmd_render/src/virtio/control_owner.rs"
NATIVE = "kmd_render/src/ddi/native_fence.rs"
LIB = "kmd_render/src/lib.rs"
MOD = "kmd_render/src/ddi/mod.rs"
QUERY = "kmd_render/src/ddi/query_adapter_info.rs"
MPO = "kmd_render/src/ddi/mpo3.rs"
DIAG = "kmd_render/src/ddi/diag_etw.rs"
ADAPTER = "kmd_render/src/adapter/mod.rs"
CORE_DIAG = "kmd_render/src/diag.rs"
DISPLAY = "kmd_render/src/ddi/display.rs"
ADAPTER_SCANOUT = "kmd_render/src/adapter/scanout.rs"
SUBMIT = "kmd_render/src/ddi/submit_command.rs"
LOGIC_ADMISSION = "kmd_logic/src/direct_scanout_admission.rs"
CLASSES = "kmd_render/tools/wddm32_slot_classes.tsv"
AUDIT_RS = "kmd_render/src/ddi/wddm32_slot_audit.rs"
AUDIT_MD = "docs/retirement/d9-wddm32-slot-audit.md"
HEADER = "kmd_render/tools/wdk-28000/km/dispmprt.h"
COLD_GATE = "tools/d9-cold-dwm-admission.ps1"
RETIREMENT_GATES = "tools/retirement-gates.sh"
GENERATOR = "kmd_render/tools/gen_wddm32_slot_audit.py"
UMD_PRESENT = "umd/src/forward/present.rs"

EXTRA_SOURCES = (
    CLASSES,
    AUDIT_MD,
    HEADER,
    COLD_GATE,
    RETIREMENT_GATES,
    UMD_PRESENT,
)

DISABLED_D9_SLOTS = (
    "DxgkDdiEscape",
    "DxgkDdiCreateHwContext",
    "DxgkDdiDestroyHwContext",
    "DxgkDdiCreateHwQueue",
    "DxgkDdiDestroyHwQueue",
    "DxgkDdiSubmitCommandToHwQueue",
    "DxgkDdiSwitchToHwContextList",
    "DxgkDdiPresentToHwQueue",
    "DxgkDdiQueryDiagnosticTypesSupport",
    "DxgkDdiControlDiagnosticReporting",
)

DIAGNOSTIC_SLOTS = {
    "DxgkDdiCollectDiagnosticInfo": "dxgkddi_collect_diagnostic_info",
    "DxgkDdiCollectDbgInfo2": "dxgkddi_collect_dbg_info2",
}

NATIVE_SLOTS = {
    "DxgkDdiCreateNativeFence": "dxgkddi_create_native_fence",
    "DxgkDdiDestroyNativeFence": "dxgkddi_destroy_native_fence",
    "DxgkDdiOpenNativeFence": "dxgkddi_open_native_fence",
    "DxgkDdiCloseNativeFence": "dxgkddi_close_native_fence",
    "DxgkDdiUpdateMonitoredValues": "dxgkddi_update_monitored_values",
    "DxgkDdiUpdateCurrentValuesFromCpu": "dxgkddi_update_current_values_from_cpu",
}


def compact(source: str) -> str:
    return re.sub(r"\s+", "", source)


def load_sources(repo: str) -> dict[str, str]:
    sources = load_rust_sources(repo)
    for relative in EXTRA_SOURCES:
        path = os.path.join(repo, relative)
        with open(path, encoding="utf-8", errors="replace") as stream:
            sources[relative] = stream.read()
    return sources


def require_fragments(
    path: str,
    name: str,
    body: str,
    fragments: tuple[str, ...],
    errors: list[str],
) -> None:
    body_compact = compact(body)
    for fragment in fragments:
        if compact(fragment) not in body_compact:
            errors.append(f"{path}:{name}: required D9 fragment missing: {fragment}")


def require_order(
    path: str,
    name: str,
    body: str,
    tokens: tuple[str, ...],
    errors: list[str],
) -> None:
    body_compact = compact(body)
    positions = [body_compact.find(compact(token)) for token in tokens]
    if any(position < 0 for position in positions) or positions != sorted(positions):
        errors.append(f"{path}:{name}: required D9 order drifted: {' -> '.join(tokens)}")


def parse_classes(source: str, errors: list[str]) -> list[tuple[str, str, str]]:
    rows: list[tuple[str, str, str]] = []
    for line_number, raw in enumerate(source.splitlines(), 1):
        if not raw.strip() or raw.lstrip().startswith("#"):
            continue
        fields = raw.split("\t")
        if len(fields) != 3:
            errors.append(f"{CLASSES}:{line_number}: expected three TSV fields")
            continue
        rows.append(tuple(field.strip() for field in fields))
    return rows


def check_activation(sources: dict[str, str], live: dict[str, str], errors: list[str]) -> None:
    surface_defs = re.findall(
        r"\bpub\s*\(\s*crate\s*\)\s+const\s+SURFACE\s*:\s*WddmSurface\s*=\s*"
        r"WddmSurface::(\w+)\s*;",
        live.get(SURFACE, ""),
    )
    if surface_defs != ["Wddm3_2GpuMmu"]:
        errors.append(f"{SURFACE}: D9 requires exactly one Wddm3_2GpuMmu SURFACE")

    owner = compact(live.get(OWNER, ""))
    owner_definition = (
        "pub(crate)constKMD_D2_OWNER_ENABLED:bool="
        "matches!(SURFACE,WddmSurface::Wddm3_2GpuMmu);"
    )
    if owner.count(owner_definition) != 1:
        errors.append(f"{OWNER}: D2 authority must derive solely from SURFACE")

    native = compact(live.get(NATIVE, ""))
    native_definition = (
        "pub(crate)constNATIVE_FENCE_ADVERTISED:bool="
        "matches!(SURFACE,WddmSurface::Wddm3_2GpuMmu);"
    )
    if native.count(native_definition) != 1:
        errors.append(f"{NATIVE}: native-fence advertisement must derive solely from SURFACE")

    activation_switches: list[tuple[str, str]] = []
    switch_pattern = re.compile(
        r"\b(?:pub\s*\(\s*crate\s*\)\s+)?const\s+"
        r"([A-Z0-9_]*(?:D2|WDDM|SURFACE|ACTIV|NATIVE_FENCE)[A-Z0-9_]*)"
        r"\s*:\s*bool\s*="
    )
    for path, text in live.items():
        for match in switch_pattern.finditer(text):
            activation_switches.append((path, match.group(1)))
    expected_switches = sorted(
        ((OWNER, "KMD_D2_OWNER_ENABLED"), (NATIVE, "NATIVE_FENCE_ADVERTISED"))
    )
    if sorted(activation_switches) != expected_switches:
        errors.append(
            "D9 has an independent or decoy activation switch: "
            f"{sorted(activation_switches)!r}"
        )

    switch_scope = (SURFACE, OWNER, NATIVE, QUERY, LIB)
    primitive_switches: list[tuple[str, str]] = []
    primitive_pattern = re.compile(
        r"\b(?:pub\s*\(\s*crate\s*\)\s+)?(?:const|static)\s+"
        r"([A-Za-z0-9_]*(?:ENABLE|ACTIV|ADVERT|RAISE|WDDM32|D2_OWNER)[A-Za-z0-9_]*)"
        r"\s*:\s*(?:bool|[ui](?:8|16|32|64|128|size))\s*="
    )
    for path in switch_scope:
        for match in primitive_pattern.finditer(live.get(path, "")):
            primitive_switches.append((path, match.group(1)))
        if re.search(r"#\s*\[\s*cfg[^]]*(?:wddm|d2|native.?fence|activ)", live.get(path, ""), re.I):
            errors.append(f"{path}: cfg-selected D9 activation authority is forbidden")
    expected_primitives = sorted(
        (
            (OWNER, "KMD_D2_OWNER_ENABLED"),
            (NATIVE, "NATIVE_FENCE_ADVERTISED"),
            (NATIVE, "FEATURE_ENABLED"),
            (NATIVE, "LIFECYCLE_ACTIVE"),
        )
    )
    if sorted(primitive_switches) != expected_primitives:
        errors.append(
            "D9 has an alternate primitive activation selector: "
            f"{sorted(primitive_switches)!r}"
        )


def check_audit(sources: dict[str, str], errors: list[str]) -> None:
    rows = parse_classes(sources.get(CLASSES, ""), errors)
    if len(rows) != 192:
        errors.append(f"{CLASSES}: expected 192 callback rows, found {len(rows)}")
    counts = collections.Counter(row[1] for row in rows)
    transitional = [row[0] for row in rows if row[1] in ("Pending", "Retiring")]
    unknown = [row[0] for row in rows if row[1] not in ("Implemented", "Disabled")]
    if transitional or unknown:
        errors.append(
            f"{CLASSES}: every D9 slot must be terminal; non-terminal rows={transitional + unknown!r}"
        )
    if counts != collections.Counter({"Disabled": 100, "Implemented": 92}):
        errors.append(f"{CLASSES}: terminal count drifted: {dict(counts)!r}")

    rs = sources.get(AUDIT_RS, "")
    md = sources.get(AUDIT_MD, "")
    for fragment in (
        "pub(crate) const SLOT_COUNT: usize = 192;",
        "const EXPECTED_STRUCT_SIZE: usize = 8 + SLOT_COUNT * 8;",
        "size_of::<DRIVER_INITIALIZATION_DATA>() == EXPECTED_STRUCT_SIZE",
    ):
        if fragment not in rs:
            errors.append(f"{AUDIT_RS}: generated layout proof missing: {fragment}")
    for fragment in (
        "Slots: **192** (plus `Version`), struct size **1544** bytes.",
        "* `Implemented` — 92",
        "* `Disabled` — 100",
        "* `Pending` — 0",
        "* `Retiring` — 0",
    ):
        if fragment not in md:
            errors.append(f"{AUDIT_MD}: generated count/layout marker missing: {fragment}")


def check_table(live: dict[str, str], errors: list[str]) -> None:
    table = compact(live.get(LIB, ""))
    for slot in DISABLED_D9_SLOTS:
        if re.search(rf"\bdata\.{re.escape(slot)}=", table):
            errors.append(f"{LIB}: disabled D9 slot is registered: {slot}")

    for slots in (DIAGNOSTIC_SLOTS, NATIVE_SLOTS):
        for slot, function in slots.items():
            assignment = f"data.{slot}=Some(ddi::{function});"
            if table.count(assignment) != 1:
                errors.append(f"{LIB}: expected exactly one assignment {assignment}")

    native_log_slots = (
        "DxgkDdiSetNativeFenceLogBuffer",
        "DxgkDdiUpdateNativeFenceLogs",
    )
    for slot in native_log_slots:
        if re.search(rf"\bdata\.{slot}=", table):
            errors.append(f"{LIB}: unsupported native-fence log slot is registered: {slot}")


def check_diagnostics(sources: dict[str, str], errors: list[str]) -> None:
    diagnostic = unique_function(sources, DIAG, "dxgkddi_collect_diagnostic_info", errors)
    dbg_info2 = unique_function(sources, DIAG, "dxgkddi_collect_dbg_info2", errors)
    if diagnostic is not None:
        body = diagnostic[1]
        require_order(
            DIAG,
            "dxgkddi_collect_diagnostic_info",
            body,
            (
                "KeGetCurrentIrql()",
                "physical_device_object.is_null()",
                "ptr::read(collect_diagnostic_info)",
                "valid_diagnostic_info_type(input.Type)",
                "checked_range(input.pBuffer.cast_const(), copy_len)",
                "let adapter = if input.hAdapter.is_null()",
                "let report = os_diagnostic_report",
                "let mut output = input",
                "ptr::copy_nonoverlapping",
                "ptr::write(collect_diagnostic_info, output)",
            ),
            errors,
        )
        require_fragments(
            DIAG,
            "dxgkddi_collect_diagnostic_info",
            body,
            (
                "collect_diagnostic_info.is_aligned()",
                "input.BufferSizeIn != 0 && input.pBuffer.is_null()",
                "ranges_overlap(input.pBuffer.cast_const(), copy_len, collect_diagnostic_info.cast()",
                "adapter_ptr.is_aligned()",
                "ranges_overlap(collect_diagnostic_info.cast(), size_of::<DXGKARG_COLLECTDIAGNOSTICINFO>(), adapter_ptr.cast()",
                "ranges_overlap(input.pBuffer.cast_const(), copy_len, adapter_ptr.cast()",
                "output.BucketingString = [0; 64]",
                "output.DescriptionString = [0; 128]",
                "output.__bindgen_anon_1.pReserved = ptr::null_mut()",
                "output.BufferSizeOut = copy_len as u32",
            ),
            errors,
        )

    if dbg_info2 is not None:
        body = dbg_info2[1]
        require_order(
            DIAG,
            "dxgkddi_collect_dbg_info2",
            body,
            (
                "KeGetCurrentIrql()",
                "adapter_ptr.is_null()",
                "ptr::read(collect_dbg_info2)",
                "valid_tdr_type(input.TdrType)",
                "usize::try_from(input.BufferSize)",
                "checked_range(input.pBuffer.cast_const(), copy_len)",
                "input.pExtension.is_aligned()",
                "input.TdrPayload.is_null()",
                "let adapter = unsafe { &*adapter_ptr }",
                "let report = os_diagnostic_report",
                "let extension = DXGKARG_COLLECTDBGINFO_EXT::default()",
                "ptr::copy_nonoverlapping",
                "ptr::write(input.pExtension, extension)",
            ),
            errors,
        )
        require_fragments(
            DIAG,
            "dxgkddi_collect_dbg_info2",
            body,
            (
                "collect_dbg_info2.is_aligned()",
                "ranges_overlap(collect_dbg_info2.cast(), size_of::<DXGKARG_COLLECTDBGINFO2>(), adapter_ptr.cast()",
                "VIDEO_TDR_TIMEOUT_DETECTED | VIDEO_ENGINE_TIMEOUT_DETECTED",
                "input.BufferSize != 0 && input.pBuffer.is_null()",
                "checked_range(input.pExtension.cast(), size_of::<DXGKARG_COLLECTDBGINFO_EXT>()",
                "input.TdrPayloadSize < size_of::<DXGK_TDR_PAYLOAD_ENGINE_TIMEOUT>() as u32",
                "input.TdrPayloadSize < size_of::<DXGK_TDR_PAYLOAD_VSYNC_TIMEOUT>() as u32",
                "checked_range(input.TdrPayload.cast_const(), size_of::<DXGK_TDR_PAYLOAD_ENGINE_TIMEOUT>()",
                "checked_range(input.TdrPayload.cast_const(), size_of::<DXGK_TDR_PAYLOAD_VSYNC_TIMEOUT>()",
                "payload.NumberOfPendingSuspendRequests != 0",
                "payload.NumberOfReadyInteractiveHwQueues != 0",
                "payload.hContext = ptr::null_mut()",
            ),
            errors,
        )

    diag_live = live_rust(sources.get(DIAG, ""))
    diagnostic_types = unique_function(
        {DIAG: sources.get(DIAG, "")}, DIAG, "valid_diagnostic_info_type", errors
    )
    tdr_types = unique_function({DIAG: sources.get(DIAG, "")}, DIAG, "valid_tdr_type", errors)
    if diagnostic_types is not None:
        require_fragments(
            DIAG,
            "valid_diagnostic_info_type",
            diagnostic_types[1],
            ("DXGK_DI_ADDDEVICE", "DXGK_DI_STARTDEVICE", "DXGK_DI_BLACKSCREEN"),
            errors,
        )
    if tdr_types is not None:
        require_fragments(
            DIAG,
            "valid_tdr_type",
            tdr_types[1],
            (
                "DXGK_TDR_TYPE_UNKNOWN",
                "DXGK_TDR_TYPE_FORCED",
                "DXGK_TDR_TYPE_PREEMPT_TIMEOUT",
                "DXGK_TDR_TYPE_VSYNC_TIMEOUT",
                "DXGK_TDR_TYPE_DOD_PRESENT_FORCED",
                "DXGK_TDR_TYPE_DOD_PRESENT_TIMEOUT",
                "DXGK_TDR_TYPE_ENGINE_TIMEOUT",
                "DXGK_TDR_TYPE_DOD_VSYNC_FORCED",
                "DXGK_TDR_TYPE_DOD_VSYNC_TIMEOUT",
                "DXGK_TDR_TYPE_ENGINE_TIMEOUT_PROMOTED",
                "DXGK_TDR_TYPE_PAGE_FAULT",
                "DXGK_TDR_TYPE_INVALID_FENCE",
                "DXGK_TDR_TYPE_ENGINE_PAGE_FAULT",
                "DXGK_TDR_TYPE_DISPLAY_ENGINE_FAULT",
            ),
            errors,
        )
    for forbidden in (
        "ZwQuery",
        "IoCreate",
        "MmMap",
        "D3DKMTEscape",
        "lookup_ticket",
        "read_config_dword",
    ):
        if forbidden in diag_live:
            errors.append(f"{DIAG}: diagnostic callback gained forbidden channel {forbidden}")


def check_caps_and_mpo(sources: dict[str, str], errors: list[str]) -> None:
    query = unique_function(sources, QUERY, "query_driver_caps", errors)
    if query is not None:
        body = query[1]
        body_compact = compact(body)
        minimum = (
            "constREQUIRED_DRIVER_CAPS_SIZE:usize="
            "offset_of!(DXGK_DRIVERCAPS,MaxOverlayPlanes)+size_of::<UINT>();"
        )
        if body_compact.count(minimum) != 1:
            errors.append(f"{QUERY}: caps minimum must include the full MPO plane count")
        direct_flip = (
            "letsupport_direct_flip:BOOLEAN="
            "crate::virtio::KMD_D2_OWNER_ENABLEDasBOOLEAN;"
        )
        if body_compact.count(direct_flip) != 1:
            errors.append(f"{QUERY}: Direct Flip capability is not derived from D2 authority")
        support = (
            "constSUPPORT_MULTI_PLANE_OVERLAY:BOOLEAN="
            "crate::virtio::KMD_D2_OWNER_ENABLEDasBOOLEAN;"
        )
        if body_compact.count(support) != 1:
            errors.append(f"{QUERY}: MPO capability is not derived from D2 authority")
        if body_compact.count(
            "out.set(caps_offset!(SupportMultiPlaneOverlay),SUPPORT_MULTI_PLANE_OVERLAY,);"
        ) != 1:
            errors.append(f"{QUERY}: active package does not publish SupportMultiPlaneOverlay once")
        if body_compact.count(
            "letmax_overlay_planes:UINT=SUPPORT_MULTI_PLANE_OVERLAYasUINT;"
            "out.set(caps_offset!(MaxOverlayPlanes),max_overlay_planes);"
        ) != 1:
            errors.append(
                f"{QUERY}: active package does not publish the exact one-primary MPO plane count"
            )

        writes = re.findall(
            r"\bout\s*\.\s*set(?:\s*::\s*<[^>]+>)?\s*\(\s*"
            r"caps_offset!\s*\(\s*(\w+)",
            body,
        )
        expected_writes = collections.Counter(
            {
                "HighestAcceptableAddress": 1,
                "MaxAllocationListSlotId": 1,
                "ApertureSegmentCommitLimit": 1,
                "SupportNonVGA": 1,
                "WDDMVersion": 1,
                "PreemptionCaps": 2,
                "SupportPerEngineTDR": 1,
                "PresentationCaps": 1,
                "FlipCaps": 1,
                "SchedulingCaps": 1,
                "MemoryManagementCaps": 1,
                "MaxQueuedFlipOnVSync": 1,
                "SupportDirectFlip": 1,
                "SupportMultiPlaneOverlay": 1,
                "MaxOverlayPlanes": 1,
                "GpuEngineTopology": 1,
            }
        )
        if collections.Counter(writes) != expected_writes:
            errors.append(
                f"{QUERY}: versioned caps write set drifted beyond the reviewed bound: {writes!r}"
            )

    # Direct Flip is a required part of the admitted MPO package, not a legacy
    # registry A/B. Its DRIVERCAPS bit and every segment bit must move with the
    # same SURFACE-derived D2 predicate or dxgkrnl rejects AddAdapter.
    for path in (ADAPTER, CORE_DIAG, QUERY):
        if "DirectFlipCaps" in live_rust(sources.get(path, "")):
            errors.append(f"{path}: retired DirectFlipCaps activation knob restored")

    query_live = compact(live_rust(sources.get(QUERY, "")))
    aperture_authority = (
        "SegmentDescriptorSpec::aperture("
        "crate::virtio::KMD_D2_OWNER_ENABLED)"
    )
    if query_live.count(aperture_authority) != 3:
        errors.append(
            f"{QUERY}: all three aperture generations must derive DirectFlip from D2 authority"
        )
    bar_authority = (
        "constDIRECT_FLIP_FLAG:u32=0x20;"
        "letbar_flags=(knobs.bar_seg_flags&!DIRECT_FLIP_FLAG)|"
        "ifcrate::virtio::KMD_D2_OWNER_ENABLED{DIRECT_FLIP_FLAG}else{0};"
        "letspec=SegmentDescriptorSpec::from_bar_flags(bar_flags,"
    )
    if query_live.count(bar_authority) != 1:
        errors.append(
            f"{QUERY}: BAR DirectFlip must mask the knob bit and derive from D2 authority"
        )

    mpo = compact(live_rust(sources.get(MPO, "")))
    for fragment in (
        "caps.MaxPlanes=0;caps.MaxRGBPlanes=0;caps.MaxYUVPlanes=0;"
        "caps.OverlayCaps=Default::default();",
        "caps.MaxPlanes=1;caps.MaxRGBPlanes=1;caps.MaxStretchFactor=1.0;"
        "caps.MaxShrinkFactor=1.0;",
        "args.Supported=0;args.ReturnInfo=Default::default();",
        "args.OutputFlags=Default::default();",
        "plane.OutputFlags=Default::default();",
        "args.PostCompositionCount!=0",
        "!args.pPostComposition.is_null()",
        "!args.pHDRMetaData.is_null()",
        "plane.MaxImmediateFlipLine!=0",
    ):
        if fragment not in mpo:
            errors.append(f"{MPO}: exact one-primary RGB/unity output proof missing: {fragment}")
    for forbidden in (
        "set_PostPresentNeeded(",
        "set_HsyncFlipCompletion(",
        "set_HsyncInterruptCompletion(",
        "set_Shared(",
        "set_Immediate(",
        "set_Rotation(",
        "set_StretchYUV(",
        "MaxYUVPlanes=1",
    ):
        if forbidden in mpo:
            errors.append(f"{MPO}: unsupported MPO output authority enabled: {forbidden}")

    mode_behavior = unique_function(
        sources, MPO, "dxgkddi_control_mode_behavior", errors
    )
    if mode_behavior is not None:
        body = compact(mode_behavior[1])
        for fragment in (
            "letrequest=unsafe{control.Request.Value};",
            "control.Satisfied=Default::default();",
            "control.NotSatisfied=Default::default();",
        ):
            if body.count(compact(fragment)) != 1:
                errors.append(
                    f"{MPO}: ControlModeBehavior must publish exact unsupported outputs: "
                    f"{fragment}"
                )
        for forbidden in (
            "control.Satisfied.Value=request",
            "control.NotSatisfied.Value=request",
        ):
            if compact(forbidden) in body:
                errors.append(
                    f"{MPO}: ControlModeBehavior falsely claims support for an "
                    f"unsupported request: {forbidden}"
                )

    validator = unique_function(sources, LOGIC_ADMISSION, "validate_mpo_plane", errors)
    if validator is not None:
        require_fragments(
            LOGIC_ADMISSION,
            "validate_mpo_plane",
            validator[1],
            (
                "if !plane.identity_rotation",
                "if plane.vertical_flip",
                "if plane.horizontal_flip",
                "if plane.alpha_blend",
                "if !plane.sdr_rgb",
                "if plane.scaling",
                "if plane.post_composition",
                "if plane.hdr_metadata",
            ),
            errors,
        )


def check_physical_adapter_caps(sources: dict[str, str], errors: list[str]) -> None:
    source = live_rust(sources.get(QUERY, ""))
    dispatcher = (
        "DXGKQAITYPE_PHYSICALADAPTERCAPS=>unsafe{"
        "query_physical_adapter_caps(adapter,args)}"
    )
    if compact(source).count(dispatcher) != 1:
        errors.append(
            f"{QUERY}: WDDM 3.2 physical-adapter caps must be dispatched exactly once"
        )

    query = unique_function(sources, QUERY, "query_physical_adapter_caps", errors)
    if query is None:
        return
    body = query[1]
    require_order(
        QUERY,
        "query_physical_adapter_caps",
        body,
        (
            "let input_ptr = args.pInputData.cast::<DXGK_QUERYPHYSICALADAPTERCAPSIN>()",
            "args.InputDataSize as usize != size_of::<DXGK_QUERYPHYSICALADAPTERCAPSIN>()",
            "input_ptr.is_null()",
            "!input_ptr.is_aligned()",
            "let output_ptr = args.pOutputData.cast::<DXGK_PHYSICALADAPTERCAPS>()",
            "(args.OutputDataSize as usize) < size_of::<DXGK_PHYSICALADAPTERCAPS>()",
            "output_ptr.is_null()",
            "!output_ptr.is_aligned()",
            "core::ptr::read(input_ptr)",
            "input.PhysicalAdapterIndex != 0",
            "adapter.dxgkrnl()",
            "let mut caps = DXGK_PHYSICALADAPTERCAPS::default()",
            "caps.NumExecutionNodes = 1",
            "caps.PagingNodeIndex = 0",
            "caps.DxgkPhysicalAdapterHandle = dxgkrnl.DeviceHandle",
            "caps.Flags.__bindgen_anon_1.Value = physical_adapter_flags",
            "core::ptr::write(output_ptr, caps)",
        ),
        errors,
    )
    require_fragments(
        QUERY,
        "query_physical_adapter_caps",
        body,
        (
            "Err(_) => return STATUS_DEVICE_NOT_READY",
            "const PHYSICAL_ADAPTER_GPU_MMU_SUPPORTED: UINT = 1 << 1",
            "let physical_adapter_flags = if SURFACE.gpu_mmu()",
            "PHYSICAL_ADAPTER_GPU_MMU_SUPPORTED",
            "else { 0 }",
        ),
        errors,
    )
    assignments = collections.Counter(
        re.findall(r"\bcaps\.(\w+)(?:\.[A-Za-z0-9_]+)*\s*=", body)
    )
    expected_assignments = collections.Counter(
        {
            "NumExecutionNodes": 1,
            "PagingNodeIndex": 1,
            "DxgkPhysicalAdapterHandle": 1,
            "Flags": 1,
        }
    )
    if assignments != expected_assignments:
        errors.append(
            f"{QUERY}: physical-adapter caps output authority drifted: "
            f"{dict(assignments)!r}"
        )
    for forbidden in (
        "write_bytes",
        "NumExecutionNodes = 0",
        "DxgkPhysicalAdapterHandle = core::ptr::null_mut()",
        "VPRPagingNode =",
        "VirtualCopyNodeIndex =",
    ):
        if forbidden in body:
            errors.append(
                f"{QUERY}:query_physical_adapter_caps: unsafe/unsupported output restored: "
                f"{forbidden}"
            )


def check_umd_mpo(sources: dict[str, str], errors: list[str]) -> None:
    source = live_rust(sources.get(UMD_PRESENT, ""))
    source_compact = compact(source)
    for fragment in (
        "pub(crate) const DXGI_MPO_MAX_PLANES: u32 = 1;",
        "pub(crate) const RGB: u32 = ddi::DXGI_DDI_MULTIPLANE_OVERLAY_FEATURE_CAPS_DXGI_DDI_MULTIPLANE_OVERLAY_FEATURE_CAPS_RGB as u32;",
        "pub(crate) const HELIOS_MPO_MAX_STRETCH: f32 = 1.0;",
        "pub(crate) const HELIOS_MPO_MAX_SHRINK: f32 = 1.0;",
        "pub(crate) const HELIOS_MPO_GROUPS: u32 = 1;",
        "pub(crate) const HELIOS_MPO_OVERLAY_CAPS: u32 = RGB;",
        "const MPO_PRESENT_FLIP_FLAG: u32 = 0x2;",
    ):
        if compact(fragment) not in source_compact:
            errors.append(f"{UMD_PRESENT}: exact one-primary UMD MPO cap missing: {fragment}")
    for forbidden in (
        "pub(crate) const BILINEAR:",
        "pub(crate) const SHARED:",
        "pub(crate) const IMMEDIATE:",
    ):
        if compact(forbidden) in source_compact:
            errors.append(f"{UMD_PRESENT}: unsupported UMD MPO capability restored: {forbidden}")

    same_rect = unique_function(sources, UMD_PRESENT, "same_mpo_rect", errors)
    if same_rect is not None:
        require_fragments(
            UMD_PRESENT,
            "same_mpo_rect",
            same_rect[1],
            (
                "left.left == right.left",
                "left.top == right.top",
                "left.right == right.right",
                "left.bottom == right.bottom",
            ),
            errors,
        )
    full_rect = unique_function(sources, UMD_PRESENT, "full_resource_mpo_rect", errors)
    if full_rect is not None:
        require_fragments(
            UMD_PRESENT,
            "full_resource_mpo_rect",
            full_rect[1],
            (
                "i32::try_from(width)",
                "i32::try_from(height)",
                "rect.left == 0",
                "rect.top == 0",
                "rect.right == width",
                "rect.bottom == height",
                "width != 0",
                "height != 0",
            ),
            errors,
        )

    caps = unique_function(sources, UMD_PRESENT, "dxgi_get_mpo_caps", errors)
    if caps is not None:
        require_order(
            UMD_PRESENT,
            "dxgi_get_mpo_caps",
            caps[1],
            (
                "arg.is_null() || !arg.is_aligned()",
                "let vidpn_source_id = core::ptr::addr_of!((*arg).VidPnSourceId).read()",
                "vidpn_source_id != 0",
                "let caps = ddi::DXGI_DDI_MULTIPLANE_OVERLAY_CAPS",
                "core::ptr::addr_of_mut!((*arg).MultiplaneOverlayCaps).write(caps)",
            ),
            errors,
        )

    group = unique_function(sources, UMD_PRESENT, "dxgi_get_mpo_group_caps", errors)
    if group is not None:
        require_order(
            UMD_PRESENT,
            "dxgi_get_mpo_group_caps",
            group[1],
            (
                "arg.is_null() || !arg.is_aligned()",
                "let vidpn_source_id = core::ptr::addr_of!((*arg).VidPnSourceId).read()",
                "let group_index = core::ptr::addr_of!((*arg).GroupIndex).read()",
                "vidpn_source_id != 0 || group_index != 0",
                "let caps = ddi::DXGI_DDI_MULTIPLANE_OVERLAY_GROUP_CAPS",
                "core::ptr::addr_of_mut!((*arg).MultiplaneOverlayGroupCaps).write(caps)",
            ),
            errors,
        )
        require_fragments(
            UMD_PRESENT,
            "dxgi_get_mpo_group_caps",
            group[1],
            (
                "NumPlanes: DXGI_MPO_MAX_PLANES",
                "MaxStretchFactor: HELIOS_MPO_MAX_STRETCH",
                "MaxShrinkFactor: HELIOS_MPO_MAX_SHRINK",
                "OverlayCaps: HELIOS_MPO_OVERLAY_CAPS",
                "StereoCaps: 0",
            ),
            errors,
        )

    present = unique_function(sources, UMD_PRESENT, "dxgi_present_mpo", errors)
    if present is not None:
        body = present[1]
        require_order(
            UMD_PRESENT,
            "dxgi_present_mpo",
            body,
            (
                "arg.is_null() || !arg.is_aligned()",
                "let a = core::ptr::read(arg)",
                "a.PresentPlaneCount != DXGI_MPO_MAX_PLANES",
                "a.pPresentPlanes.is_null() || !a.pPresentPlanes.is_aligned()",
                "a.Reserved != 0",
                "a.VidPnSourceId != 0",
                "present_flags != MPO_PRESENT_FLIP_FLAG",
                "DXGI_DDI_FLIP_INTERVAL_TYPE_DXGI_DDI_FLIP_INTERVAL_ONE",
                "DXGI_DDI_FLIP_INTERVAL_TYPE_DXGI_DDI_FLIP_INTERVAL_FOUR",
                "let plane = core::ptr::read(a.pPresentPlanes)",
                "plane.LayerIndex != 0 || plane.Enabled != 1 || plane.hResource == 0",
                "plane.SubResourceIndex != 0",
                "attrs.Flags != 0",
                "DXGI_DDI_MODE_ROTATION_DXGI_DDI_MODE_ROTATION_IDENTITY",
                "DXGI_DDI_MULTIPLANE_OVERLAY_BLEND_DXGI_DDI_MULTIPLANE_OVERLAY_BLEND_OPAQUE",
                "DXGI_DDI_MULTIPLANE_OVERLAY_VIDEO_FRAME_FORMAT_DXGI_DDI_MULIIPLANE_OVERLAY_VIDEO_FRAME_FORMAT_PROGRESSIVE",
                "attrs.YCbCrFlags != 0",
                "DXGI_DDI_MULTIPLANE_OVERLAY_STEREO_FORMAT_DXGI_DDI_MULTIPLANE_OVERLAY_STEREO_FORMAT_MONO",
                "attrs.StereoLeftViewFrame0 != 0",
                "attrs.StereoBaseViewFrame0 != 0",
                "DXGI_DDI_MULTIPLANE_OVERLAY_STEREO_FLIP_MODE_DXGI_DDI_MULTIPLANE_OVERLAY_STEREO_FLIP_NONE",
                "attrs.StretchQuality != 0",
                "same_mpo_rect(&attrs.SrcRect, &attrs.DstRect)",
                "same_mpo_rect(&attrs.SrcRect, &attrs.ClipRect)",
                "let resource = dxgi_resource_handle(plane.hResource)",
                "load_resource(resource)",
                "cast::<ID3D11Texture2D>()",
                "desc.MipLevels != 1",
                "desc.ArraySize != 1",
                "desc.SampleDesc.Count != 1",
                "desc.SampleDesc.Quality != 0",
                "DXGI_FORMAT_B8G8R8A8_UNORM",
                "full_resource_mpo_rect(&attrs.SrcRect, desc.Width, desc.Height)",
                "let alloc = resource_allocation(resource)",
                "cb.AllocationInfo[0].PresentAllocation = alloc",
                "cb.AllocationInfo[0].SubResourceIndex = 0",
                "cb.AllocationInfoCount = 1",
            ),
            errors,
        )
        if re.search(r"\bfor\s+\w+\s+in\s+0\s*\.\.", body):
            errors.append(f"{UMD_PRESENT}:dxgi_present_mpo: multi-plane forwarding loop restored")


def check_legacy_authority_closure(sources: dict[str, str], errors: list[str]) -> None:
    guarded = (
        (
            DISPLAY,
            "service_windowed_blt",
            "if crate::virtio::KMD_D2_OWNER_ENABLED { return; }",
        ),
        (
            DISPLAY,
            "process_deferred_vidpn_source_address",
            "if crate::virtio::KMD_D2_OWNER_ENABLED { return; }",
        ),
        (
            ADAPTER_SCANOUT,
            "queue_active_scanout_refresh",
            "if crate::virtio::KMD_D2_OWNER_ENABLED { return ScanoutRefreshQueue::Dropped; }",
        ),
        (
            SUBMIT,
            "arm_scanout_refresh_after_current_venus",
            "if crate::virtio::KMD_D2_OWNER_ENABLED { return; }",
        ),
    )
    for path, name, guard in guarded:
        function = unique_function(sources, path, name, errors)
        if function is None:
            continue
        body = compact(function[1])
        guard_at = body.find(compact(guard))
        if guard_at < 0:
            errors.append(f"{path}:{name}: active D9 does not locally close legacy authority")
            continue
        first_effect = min(
            (position for position in (body.find("with_scanout_lifecycle("), body.find("MARKER_HISTOGRAM.note(")) if position >= 0),
            default=len(body),
        )
        if guard_at > first_effect:
            errors.append(f"{path}:{name}: legacy authority guard is not the first effect")
    direct = unique_function(sources, LOGIC_ADMISSION, "validate_direct_scanout_binding", errors)
    if direct is not None:
        require_fragments(
            LOGIC_ADMISSION,
            "validate_direct_scanout_binding",
            direct[1],
            ("if operation.immediate_flip", "if operation.stereo"),
            errors,
        )


def check_cold_gate(sources: dict[str, str], errors: list[str]) -> None:
    script = sources.get(COLD_GATE, "")
    for fragment in (
        "d9-cold-dwm-admission-v1",
        "$InstalledKmdPath",
        "$ExpectedKmdSha256",
        "$InstalledInfPath",
        "$ExpectedInfSha256",
        "$ExpectedDriverVersion",
        "$ExpectedOsBuild",
        "$MinimumBootUtc",
        "COLD_BOOT_POWER_CYCLE",
        "VISIBLE_DWM_DESKTOP",
        "$OwnerVisibleDescription",
        "LastBootUpTime.ToUniversalTime()",
        "[string]$os.BuildNumber -ne $ExpectedOsBuild",
        "DEVPKEY_Device_ProblemCode",
        "DEVPKEY_Device_DriverVersion",
        "Driver Model:\\s*(?<model>",
        "$driverModel -ne 'WDDM 3.2'",
        "CDDisplaySwapChain|E_NOTIMPL|0x80004001",
        "$passed = $failures.Count -eq 0",
        "WDDM 3.2 KMD surface admitted on this cold boot with owner-observed visible DWM startup.",
        "does not establish full HPS2 retirement or production correctness",
    ):
        if fragment not in script:
            errors.append(f"{COLD_GATE}: cold-DWM admission marker missing: {fragment}")
    if "BuildNumber -ne '28000'" in script or "exact build 28000" in script:
        errors.append(
            f"{COLD_GATE}: superseded build-28000 guest minimum was restored; "
            "WDK 28000 is the binding authority and FINDINGS.md F1 governs the target OS"
        )
    for forbidden in (
        "Restart-Computer",
        "pnputil.exe",
        "devcon",
        "Disable-PnpDevice",
        "Enable-PnpDevice",
        "Win32_Shutdown",
        "VNC",
        "screenshot",
    ):
        if forbidden.lower() in script.lower():
            errors.append(f"{COLD_GATE}: admission harness must not perform/use {forbidden}")

    retirement = sources.get(RETIREMENT_GATES, "")
    if "DORMANT_OWNER_GATE_PY" in retirement:
        errors.append(f"{RETIREMENT_GATES}: stale dormant-only owner gate remains")
    static_invocations = re.findall(
        r'(?m)^\s*python3\s+"\$REPO/tools/d9-wddm32-activation-gate\.py"\s+"\$REPO"\s*$',
        retirement,
    )
    if len(static_invocations) != 1:
        errors.append(f"{RETIREMENT_GATES}: D9 static gate is not integrated exactly once")
    mutation_invocations = re.findall(
        r'(?m)^\s*python3\s+"\$REPO/tools/d9-wddm32-activation-gate\.py"\s+'
        r'"\$REPO"\s+--mutations\s*$',
        retirement,
    )
    if len(mutation_invocations) != 1:
        errors.append(f"{RETIREMENT_GATES}: D9 mutation gate is not integrated exactly once")


def check_local_sources(sources: dict[str, str]) -> list[str]:
    errors: list[str] = []
    live = {
        path: live_rust(source) if path.endswith(".rs") else source
        for path, source in sources.items()
    }
    check_activation(sources, live, errors)
    check_audit(sources, errors)
    check_table(live, errors)
    check_diagnostics(sources, errors)
    check_caps_and_mpo(sources, errors)
    check_physical_adapter_caps(sources, errors)
    check_umd_mpo(sources, errors)
    check_legacy_authority_closure(sources, errors)
    check_cold_gate(sources, errors)
    return errors


def check_sources(sources: dict[str, str]) -> list[str]:
    errors = [f"K7: {error}" for error in k7_check_sources(sources)]
    errors.extend(check_local_sources(sources))
    return errors


def check_generated(repo: str) -> list[str]:
    generator = os.path.join(REPO_DEFAULT, GENERATOR)
    command = [
        sys.executable,
        generator,
        "--check",
        "--header",
        os.path.join(repo, HEADER),
        "--classes",
        os.path.join(repo, CLASSES),
        "--rs",
        os.path.join(repo, AUDIT_RS),
        "--md",
        os.path.join(repo, AUDIT_MD),
    ]
    result = subprocess.run(
        command,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        check=False,
    )
    if result.returncode == 0:
        return []
    return ["generated WDDM 3.2 audit is stale:\n" + result.stdout.strip()]


@dataclass(frozen=True)
class Mutation:
    name: str
    path: str
    old: str
    new: str


def mutation_cases() -> tuple[Mutation, ...]:
    registration_anchor = (
        "    data.DxgkDdiCollectDiagnosticInfo = Some(ddi::dxgkddi_collect_diagnostic_info);"
    )
    cases: list[Mutation] = [
        Mutation(
            "lower SURFACE without package rollback",
            SURFACE,
            "pub(crate) const SURFACE: WddmSurface = WddmSurface::Wddm3_2GpuMmu;",
            "pub(crate) const SURFACE: WddmSurface = WddmSurface::Wddm2_1GpuMmu;",
        ),
        Mutation(
            "hard-code D2 owner",
            OWNER,
            "pub(crate) const KMD_D2_OWNER_ENABLED: bool = matches!(SURFACE, WddmSurface::Wddm3_2GpuMmu);",
            "pub(crate) const KMD_D2_OWNER_ENABLED: bool = true;",
        ),
        Mutation(
            "add decoy activation switch",
            SURFACE,
            "pub(crate) const SURFACE: WddmSurface = WddmSurface::Wddm3_2GpuMmu;",
            "const WDDM32_ACTIVATED: bool = true;\n"
            "pub(crate) const SURFACE: WddmSurface = WddmSurface::Wddm3_2GpuMmu;",
        ),
        Mutation(
            "add integer decoy activation selector",
            SURFACE,
            "pub(crate) const SURFACE: WddmSurface = WddmSurface::Wddm3_2GpuMmu;",
            "const ENABLE_WDDM32: u32 = 1;\n"
            "pub(crate) const SURFACE: WddmSurface = WddmSurface::Wddm3_2GpuMmu;",
        ),
        Mutation(
            "decouple native-fence advertisement",
            NATIVE,
            "pub(crate) const NATIVE_FENCE_ADVERTISED: bool = matches!(SURFACE, WddmSurface::Wddm3_2GpuMmu);",
            "pub(crate) const NATIVE_FENCE_ADVERTISED: bool = true;",
        ),
        Mutation(
            "restore Pending classification",
            CLASSES,
            "DxgkDdiCollectDiagnosticInfo\tImplemented\t",
            "DxgkDdiCollectDiagnosticInfo\tPending\t",
        ),
        Mutation(
            "stale generated audit",
            AUDIT_RS,
            "pub(crate) const SLOT_COUNT: usize = 192;",
            "pub(crate) const SLOT_COUNT: usize = 191;",
        ),
        Mutation(
            "register Escape",
            LIB,
            registration_anchor,
            registration_anchor + "\n    data.DxgkDdiEscape = Some(ddi::dxgkddi_escape);",
        ),
        Mutation(
            "leave CollectDiagnosticInfo NULL",
            LIB,
            registration_anchor + "\n",
            "",
        ),
        Mutation(
            "unsafe diagnostic IRQL stub",
            DIAG,
            "if unsafe { KeGetCurrentIrql() } != crate::ddi::PASSIVE_LEVEL_IRQL {\n"
            "        return refuse_os_diagnostic(&COLLECT_DIAGNOSTIC_REFUSALS, 1);\n"
            "    }",
            "if false { return STATUS_SUCCESS; }",
        ),
        Mutation(
            "allow diagnostic argument to alias adapter",
            DIAG,
            "            || ranges_overlap(\n"
            "                collect_diagnostic_info.cast(),\n"
            "                size_of::<DXGKARG_COLLECTDIAGNOSTICINFO>(),\n"
            "                adapter_ptr.cast(),\n"
            "                size_of::<AdapterContext>(),\n"
            "            )\n",
            "",
        ),
        Mutation(
            "allow dbginfo2 argument to alias adapter",
            DIAG,
            "    if ranges_overlap(\n"
            "        collect_dbg_info2.cast(),\n"
            "        size_of::<DXGKARG_COLLECTDBGINFO2>(),\n"
            "        adapter_ptr.cast(),\n"
            "        size_of::<AdapterContext>(),\n"
            "    ) {\n"
            "        return refuse_os_diagnostic(&COLLECT_DBG_INFO2_REFUSALS, 0x102);\n"
            "    }\n",
            "",
        ),
        Mutation(
            "omit MPO capability",
            QUERY,
            "    out.set(\n"
            "        caps_offset!(SupportMultiPlaneOverlay),\n"
            "        SUPPORT_MULTI_PLANE_OVERLAY,\n"
            "    );\n",
            "",
        ),
        Mutation(
            "decouple Direct Flip capability from D2 authority",
            QUERY,
            "    let support_direct_flip: BOOLEAN = crate::virtio::KMD_D2_OWNER_ENABLED as BOOLEAN;",
            "    let support_direct_flip: BOOLEAN = 0;",
        ),
        Mutation(
            "decouple MPO capability from D2 authority",
            QUERY,
            "    const SUPPORT_MULTI_PLANE_OVERLAY: BOOLEAN =\n"
            "        crate::virtio::KMD_D2_OWNER_ENABLED as BOOLEAN;",
            "    const SUPPORT_MULTI_PLANE_OVERLAY: BOOLEAN = 1;",
        ),
        Mutation(
            "decouple aperture Direct Flip from D2 authority",
            QUERY,
            "unsafe fn write_aperture_descriptor(seg: *mut DXGK_SEGMENTDESCRIPTOR4) {\n"
            "    unsafe {\n"
            "        SegmentDescriptorSpec::aperture(crate::virtio::KMD_D2_OWNER_ENABLED).write_into_v4(seg)\n"
            "    };\n"
            "}",
            "unsafe fn write_aperture_descriptor(seg: *mut DXGK_SEGMENTDESCRIPTOR4) {\n"
            "    unsafe { SegmentDescriptorSpec::aperture(false).write_into_v4(seg) };\n"
            "}",
        ),
        Mutation(
            "restore BAR Direct Flip knob authority",
            QUERY,
            "    let spec = SegmentDescriptorSpec::from_bar_flags(\n"
            "        bar_flags,",
            "    let spec = SegmentDescriptorSpec::from_bar_flags(\n"
            "        knobs.bar_seg_flags,",
        ),
        Mutation(
            "restore DirectFlipCaps activation knob",
            CORE_DIAG,
            "    pub const CROSS_ADAPT_CAPS: KnobName = KnobName::new(b\"CrossAdaptCaps\");",
            "    pub const DIRECT_FLIP_CAPS: KnobName = KnobName::new(b\"DirectFlipCaps\");\n"
            "    pub const CROSS_ADAPT_CAPS: KnobName = KnobName::new(b\"CrossAdaptCaps\");",
        ),
        Mutation(
            "shorten caps bound",
            QUERY,
            "offset_of!(DXGK_DRIVERCAPS, MaxOverlayPlanes) + size_of::<UINT>();",
            "offset_of!(DXGK_DRIVERCAPS, SupportMultiPlaneOverlay) + size_of::<BOOLEAN>();",
        ),
        Mutation(
            "omit active MPO plane count",
            QUERY,
            "    out.set(caps_offset!(MaxOverlayPlanes), max_overlay_planes);\n",
            "",
        ),
        Mutation(
            "zero active MPO plane count",
            QUERY,
            "    let max_overlay_planes: UINT = SUPPORT_MULTI_PLANE_OVERLAY as UINT;",
            "    let max_overlay_planes: UINT = 0;",
        ),
        Mutation(
            "widen active MPO plane count",
            QUERY,
            "    let max_overlay_planes: UINT = SUPPORT_MULTI_PLANE_OVERLAY as UINT;",
            "    let max_overlay_planes: UINT = 2;",
        ),
        Mutation(
            "remove physical-adapter caps dispatch",
            QUERY,
            "        DXGKQAITYPE_PHYSICALADAPTERCAPS => unsafe { query_physical_adapter_caps(adapter, args) },\n",
            "",
        ),
        Mutation(
            "accept nonzero physical-adapter index",
            QUERY,
            "    if input.PhysicalAdapterIndex != 0 {",
            "    if false {",
        ),
        Mutation(
            "restore zero execution-node placeholder",
            QUERY,
            "    caps.NumExecutionNodes = 1;",
            "    caps.NumExecutionNodes = 0;",
        ),
        Mutation(
            "misidentify physical-adapter paging node",
            QUERY,
            "    caps.PagingNodeIndex = 0;",
            "    caps.PagingNodeIndex = 1;",
        ),
        Mutation(
            "drop dxgkrnl physical-adapter handle",
            QUERY,
            "    caps.DxgkPhysicalAdapterHandle = dxgkrnl.DeviceHandle;",
            "    caps.DxgkPhysicalAdapterHandle = core::ptr::null_mut();",
        ),
        Mutation(
            "advertise physical-adapter virtual copy engine",
            QUERY,
            "    const PHYSICAL_ADAPTER_GPU_MMU_SUPPORTED: UINT = 1 << 1;",
            "    const PHYSICAL_ADAPTER_GPU_MMU_SUPPORTED: UINT = (1 << 1) | (1 << 5);",
        ),
        Mutation(
            "skip physical-adapter input alignment validation",
            QUERY,
            "        || !input_ptr.is_aligned()\n",
            "",
        ),
        Mutation(
            "weaken physical-adapter output size validation",
            QUERY,
            "    if (args.OutputDataSize as usize) < size_of::<DXGK_PHYSICALADAPTERCAPS>() {",
            "    if args.OutputDataSize == 0 {",
        ),
        Mutation(
            "advertise YUV plane",
            MPO,
            "    caps.MaxYUVPlanes = 0;",
            "    caps.MaxYUVPlanes = 1;",
        ),
        Mutation(
            "set PostPresentNeeded",
            MPO,
            "    args.ReturnInfo = Default::default();\n"
            "    record_passive(&CHECK_REFUSALS, b\"MpoChkRef\", code);",
            "    args.ReturnInfo = Default::default();\n"
            "    args.ReturnInfo.__bindgen_anon_1.set_PostPresentNeeded(1);\n"
            "    record_passive(&CHECK_REFUSALS, b\"MpoChkRef\", code);",
        ),
        Mutation(
            "misreport unsupported mode behavior as failed",
            MPO,
            "    control.NotSatisfied = Default::default();\n",
            "    control.NotSatisfied = Default::default();\n"
            "    control.NotSatisfied.Value = request;\n",
        ),
        Mutation(
            "widen UMD MPO plane count",
            UMD_PRESENT,
            "pub(crate) const DXGI_MPO_MAX_PLANES: u32 = 1;",
            "pub(crate) const DXGI_MPO_MAX_PLANES: u32 = 16;",
        ),
        Mutation(
            "advertise UMD MPO stretch",
            UMD_PRESENT,
            "pub(crate) const HELIOS_MPO_MAX_STRETCH: f32 = 1.0;",
            "pub(crate) const HELIOS_MPO_MAX_STRETCH: f32 = 16.0;",
        ),
        Mutation(
            "advertise UMD MPO shrink",
            UMD_PRESENT,
            "pub(crate) const HELIOS_MPO_MAX_SHRINK: f32 = 1.0;",
            "pub(crate) const HELIOS_MPO_MAX_SHRINK: f32 = 16.0;",
        ),
        Mutation(
            "advertise unsupported UMD MPO feature",
            UMD_PRESENT,
            "pub(crate) const HELIOS_MPO_OVERLAY_CAPS: u32 = RGB;",
            "pub(crate) const HELIOS_MPO_OVERLAY_CAPS: u32 = RGB | 0x80;",
        ),
        Mutation(
            "mislabel unsupported UMD MPO bit as RGB",
            UMD_PRESENT,
            "DXGI_DDI_MULTIPLANE_OVERLAY_FEATURE_CAPS_DXGI_DDI_MULTIPLANE_OVERLAY_FEATURE_CAPS_RGB",
            "DXGI_DDI_MULTIPLANE_OVERLAY_FEATURE_CAPS_DXGI_DDI_MULTIPLANE_OVERLAY_FEATURE_CAPS_BILINEAR_FILTER",
        ),
        Mutation(
            "widen UMD MPO present flag mask",
            UMD_PRESENT,
            "const MPO_PRESENT_FLIP_FLAG: u32 = 0x2;",
            "const MPO_PRESENT_FLIP_FLAG: u32 = 0xffff_ffff;",
        ),
        Mutation(
            "publish UMD MPO caps for another source",
            UMD_PRESENT,
            "    if vidpn_source_id != 0 {\n"
            "        return E_INVALIDARG;\n"
            "    }\n"
            "    let caps = ddi::DXGI_DDI_MULTIPLANE_OVERLAY_CAPS {",
            "    if false {\n"
            "        return E_INVALIDARG;\n"
            "    }\n"
            "    let caps = ddi::DXGI_DDI_MULTIPLANE_OVERLAY_CAPS {",
        ),
        Mutation(
            "admit multiple UMD MPO present planes",
            UMD_PRESENT,
            "    if a.PresentPlaneCount != DXGI_MPO_MAX_PLANES {",
            "    if a.PresentPlaneCount > DXGI_MPO_MAX_PLANES {",
        ),
        Mutation(
            "admit unsupported UMD MPO present flags",
            UMD_PRESENT,
            "    if present_flags != MPO_PRESENT_FLIP_FLAG {",
            "    if false {",
        ),
        Mutation(
            "admit immediate UMD MPO present",
            UMD_PRESENT,
            "    if !(ddi::DXGI_DDI_FLIP_INTERVAL_TYPE_DXGI_DDI_FLIP_INTERVAL_ONE\n"
            "        ..=ddi::DXGI_DDI_FLIP_INTERVAL_TYPE_DXGI_DDI_FLIP_INTERVAL_FOUR)\n"
            "        .contains(&a.FlipInterval)\n"
            "    {",
            "    if false {",
        ),
        Mutation(
            "admit UMD MPO transforms",
            UMD_PRESENT,
            "        || attrs.Flags != 0\n",
            "        || false\n",
        ),
        Mutation(
            "admit UMD MPO stretch quality",
            UMD_PRESENT,
            "        || attrs.StretchQuality != 0\n",
            "        || false\n",
        ),
        Mutation(
            "weaken UMD MPO rectangle equality",
            UMD_PRESENT,
            "        && left.bottom == right.bottom\n",
            "        && true\n",
        ),
        Mutation(
            "forward multiple UMD MPO allocations",
            UMD_PRESENT,
            "    cb.AllocationInfoCount = 1;",
            "    cb.AllocationInfoCount = 2;",
        ),
        Mutation(
            "admit immediate MPO",
            LOGIC_ADMISSION,
            "    if operation.immediate_flip {",
            "    if false {",
        ),
        Mutation(
            "admit MPO scaling",
            LOGIC_ADMISSION,
            "    if plane.scaling {",
            "    if false {",
        ),
        Mutation(
            "enable hardware queues",
            QUERY,
            "        | SCHEDULINGCAPS_PREEMPTION_AWARE\n",
            "        | SCHEDULINGCAPS_PREEMPTION_AWARE | (1 << 7)\n",
        ),
        Mutation(
            "enable No64BitAtomics",
            NATIVE,
            "        VIDSCHCAPS_NATIVE_GPU_FENCE\n",
            "        VIDSCHCAPS_NATIVE_GPU_FENCE | VIDSCHCAPS_NO_64BIT_ATOMICS\n",
        ),
        Mutation(
            "enable optimized native interrupt",
            NATIVE,
            "        VIDSCHCAPS_NATIVE_GPU_FENCE\n",
            "        VIDSCHCAPS_NATIVE_GPU_FENCE | VIDSCHCAPS_OPTIMIZED_NATIVE_FENCE_INTERRUPT\n",
        ),
        Mutation(
            "register native-fence logs",
            LIB,
            "    data.DxgkDdiUpdateCurrentValuesFromCpu = Some(ddi::dxgkddi_update_current_values_from_cpu);",
            "    data.DxgkDdiUpdateCurrentValuesFromCpu = Some(ddi::dxgkddi_update_current_values_from_cpu);\n"
            "    data.DxgkDdiSetNativeFenceLogBuffer = Some(ddi::dxgkddi_set_native_fence_log_buffer);",
        ),
        Mutation(
            "weaken HNF1",
            "kmd_logic/src/lib.rs",
            "pub const HNF1_SIZE: usize = 64;",
            "pub const HNF1_SIZE: usize = 63;",
        ),
        Mutation(
            "weaken D4",
            "kmd_render/src/ddi/present_packet.rs",
            "pub(crate) const PRESENT_FLIP_PRIVATE_OFFSET: usize = 32;",
            "pub(crate) const PRESENT_FLIP_PRIVATE_OFFSET: usize = 16;",
        ),
        Mutation(
            "weaken D5",
            "kmd_render/src/ddi/present_packet.rs",
            "const MPO_MAX_PLANES: u32 = 1;",
            "const MPO_MAX_PLANES: u32 = 2;",
        ),
        Mutation(
            "reopen legacy WindowedBlt authority",
            DISPLAY,
            "    if crate::virtio::KMD_D2_OWNER_ENABLED {\n"
            "        return;\n"
            "    }\n"
            "    adapter.with_scanout_lifecycle(passive, |lock| {",
            "    adapter.with_scanout_lifecycle(passive, |lock| {",
        ),
        Mutation(
            "reopen legacy deferred VidPn authority",
            DISPLAY,
            "    if crate::virtio::KMD_D2_OWNER_ENABLED {\n"
            "        return;\n"
            "    }\n"
            "    let status = adapter.with_scanout_lifecycle(passive, |lock| {",
            "    let status = adapter.with_scanout_lifecycle(passive, |lock| {",
        ),
        Mutation(
            "reopen legacy refresh queue",
            ADAPTER_SCANOUT,
            "        if crate::virtio::KMD_D2_OWNER_ENABLED {\n"
            "            return ScanoutRefreshQueue::Dropped;\n"
            "        }\n"
            "        let outcome = self.with_scanout_lifecycle(passive, |lock| {",
            "        let outcome = self.with_scanout_lifecycle(passive, |lock| {",
        ),
        Mutation(
            "reopen legacy marker refresh",
            SUBMIT,
            "    if crate::virtio::KMD_D2_OWNER_ENABLED {\n"
            "        return;\n"
            "    }\n"
            "    // Unsampled: what the app PRESENTED",
            "    // Unsampled: what the app PRESENTED",
        ),
        Mutation(
            "accept Code 0 without visible desktop",
            COLD_GATE,
            "$passed = $failures.Count -eq 0",
            "$passed = $checks.device_problem_code -eq 0",
        ),
        Mutation(
            "remove exact WDDM 3.2 runtime check",
            COLD_GATE,
            "if ($driverModel -ne 'WDDM 3.2') {",
            "if ($false) {",
        ),
        Mutation(
            "ignore exact target-OS build check",
            COLD_GATE,
            "if ([string]$os.BuildNumber -ne $ExpectedOsBuild) {",
            "if ($false) {",
        ),
        Mutation(
            "restore superseded build-28000 guest minimum",
            COLD_GATE,
            "if ([string]$os.BuildNumber -ne $ExpectedOsBuild) {",
            "if ([string]$os.BuildNumber -ne '28000') {",
        ),
        Mutation(
            "claim warm boot as cold",
            COLD_GATE,
            "if ($OwnerColdBootConfirmation -ne 'COLD_BOOT_POWER_CYCLE') {",
            "if ($false) {",
        ),
        Mutation(
            "restore dormant-only gate",
            RETIREMENT_GATES,
            "run_gate \"D9 WDDM 3.2 activation package is coherent and mutation-closed\"",
            "DORMANT_OWNER_GATE_PY=stale\n"
            "run_gate \"D9 WDDM 3.2 activation package is coherent and mutation-closed\"",
        ),
    ]
    for slot in (
        "DxgkDdiCreateHwContext",
        "DxgkDdiDestroyHwContext",
        "DxgkDdiCreateHwQueue",
        "DxgkDdiDestroyHwQueue",
        "DxgkDdiSubmitCommandToHwQueue",
        "DxgkDdiSwitchToHwContextList",
        "DxgkDdiPresentToHwQueue",
    ):
        function = re.sub(r"(?<!^)(?=[A-Z])", "_", slot.removeprefix("DxgkDdi")).lower()
        cases.append(
            Mutation(
                f"register {slot}",
                LIB,
                registration_anchor,
                registration_anchor + f"\n    data.{slot} = Some(ddi::dxgkddi_{function});",
            )
        )
    return tuple(cases)


def write_source_tree(root: str, sources: dict[str, str]) -> None:
    for relative, source in sources.items():
        path = os.path.join(root, relative)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w", encoding="utf-8") as stream:
            stream.write(source)


def run_mutations(sources: dict[str, str]) -> None:
    for case in mutation_cases():
        count = sources.get(case.path, "").count(case.old)
        if count != 1:
            raise SystemExit(
                f"D9 mutation setup failed for {case.name}: expected one anchor "
                f"in {case.path}, found {count}"
            )
        mutated = dict(sources)
        mutated[case.path] = mutated[case.path].replace(case.old, case.new, 1)
        # Every D9 mutation is intended to be rejected by the D9-owned portion
        # of the real checker.  Run that first; only consult the unchanged K7
        # ancestry when a mutation escapes it.  The last-resort generated-file
        # check retains exact main-program semantics without creating a source
        # tree and Python process for every case.
        errors = check_local_sources(mutated)
        if not errors:
            errors.extend(f"K7: {error}" for error in k7_check_sources(mutated))
        if not errors:
            with tempfile.TemporaryDirectory(prefix="helios-d9-gate-") as temp:
                write_source_tree(temp, mutated)
                errors.extend(check_generated(temp))
        if not errors:
            raise SystemExit(f"D9 mutation was accepted by the real gate: {case.name}")
    print(f"OK: {len(mutation_cases())} in-memory D9 mutations rejected by the real gate")


def main() -> None:
    args = sys.argv[1:]
    mutations = False
    if "--mutations" in args:
        mutations = True
        args.remove("--mutations")
    if len(args) > 1:
        raise SystemExit("usage: d9-wddm32-activation-gate.py [repo] [--mutations]")
    repo = os.path.abspath(args[0]) if args else REPO_DEFAULT
    sources = load_sources(repo)
    errors = check_sources(sources)
    errors.extend(check_generated(repo))
    if errors:
        raise SystemExit("D9 WDDM 3.2 activation gate violated:\n" + "\n".join(errors))
    if mutations:
        run_mutations(sources)
    else:
        print(
            "OK: D9 surface/D2/caps/table/diagnostics/native-fence package is atomic; "
            "cold-DWM admission remains runtime-unadmitted"
        )


if __name__ == "__main__":
    main()
