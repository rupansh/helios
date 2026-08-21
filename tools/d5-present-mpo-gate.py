#!/usr/bin/env python3
"""Static and mutation gate for the active ordinary D5 MPO Present arm."""

from __future__ import annotations

import os
import re
import runpy
import sys
from typing import Callable


REPO_DEFAULT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
D4_GATE = os.path.join(os.path.dirname(__file__), "d4-dirql-gate.py")

# Reuse D4's Rust lexer, brace parser, complete source loader, and static proof.
# D5 shares present_packet.rs with D4, so accepting two subtly different ideas
# of live Rust or weakening the already-landed DIRQL proof is not an option.
D4 = runpy.run_path(D4_GATE, run_name="d5_imported_d4_gate")
live_rust: Callable[[str], str] = D4["live_rust"]
functions = D4["functions"]
call_names: Callable[[str], frozenset[str]] = D4["call_names"]
qualified_calls: Callable[[str], frozenset[str]] = D4["qualified_calls"]
macro_calls: Callable[[str], frozenset[str]] = D4["macro_calls"]
load_sources: Callable[[str], dict[str, str]] = D4["load_sources"]
d4_check_sources: Callable[[dict[str, str]], list[str]] = D4["check_sources"]

DISPLAY = "kmd_render/src/ddi/display.rs"
PACKET = "kmd_render/src/ddi/present_packet.rs"
SCHEDULER = "kmd_render/src/ddi/scheduler.rs"
BOUNDARY_PATH = "kmd_render/src/virtio/control_owner.rs"
SURFACE_PATH = "kmd_render/src/ddi/wddm_surface.rs"
BOUNDARY = "KMD_D2_OWNER_ENABLED"


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


def unique_decode(
    sources: dict[str, str], errors: list[str]
) -> tuple[str, str] | None:
    raw = sources.get(PACKET, "")
    live = live_rust(raw)
    found = []
    for item in functions(live):
        if item.name == "decode" and "pPresentMultiPlaneOverlayInfo" in live[item.brace : item.end]:
            found.append(item)
    if len(found) != 1:
        errors.append(f"{PACKET}: expected one MPO union decoder, found {len(found)}")
        return None
    item = found[0]
    return raw[item.brace : item.end], live[item.brace : item.end]


def compact(source: str) -> str:
    return re.sub(r"\s+", "", source)


CALL_MANIFEST: dict[tuple[str, str], frozenset[str]] = {
    (DISPLAY, "present_mpo_d5"): frozenset(
        {"emit_mpo_present", "prepare_mpo_present", "refuse_mpo_present", "store"}
    ),
    (DISPLAY, "refuse_mpo_present"): frozenset(
        {"as_slice", "fetch_add", "record_named_bytes", "store", "wrapping_add"}
    ),
    (PACKET, "prepare_mpo_present"): frozenset(
        {
            "Reserved",
            "SegmentId",
            "checked_add",
            "exact_mpo_primary_profile",
            "is_aligned",
            "is_current",
            "is_null",
            "new",
            "open_allocation_identity",
            "open_direct_scanout_allocation_facts",
            "output_capacity",
        }
    ),
    (PACKET, "emit_mpo_present"): frozenset({"as_ptr", "cast", "write_unaligned"}),
    (PACKET, "exact_mpo_primary_profile"): frozenset({"is_err", "validate_create_output"}),
    (PACKET, "output_capacity"): frozenset({"is_null"}),
}

QUALIFIED_MANIFEST: dict[tuple[str, str], frozenset[str]] = {
    (DISPLAY, "present_mpo_d5"): frozenset(),
    (DISPLAY, "refuse_mpo_present"): frozenset({"crate::diag::record_named_bytes"}),
    (PACKET, "prepare_mpo_present"): frozenset(
        {
            "NonNull::new",
            "crate::adapter::allocation_object::is_current",
            "crate::ddi::create_allocation::open_allocation_identity",
            "crate::ddi::create_allocation::open_direct_scanout_allocation_facts",
        }
    ),
    (PACKET, "emit_mpo_present"): frozenset({"core::ptr::write_unaligned"}),
    (PACKET, "exact_mpo_primary_profile"): frozenset(),
    (PACKET, "output_capacity"): frozenset(),
}


def check_call_surface(
    key: tuple[str, str], body: str, errors: list[str]
) -> None:
    observed = call_names(body)
    expected = CALL_MANIFEST[key]
    if observed != expected:
        errors.append(
            f"{key[0]}:{key[1]} D5 call surface drifted: "
            f"missing={sorted(expected - observed)} extra={sorted(observed - expected)}"
        )
    observed_qualified = qualified_calls(body)
    expected_qualified = QUALIFIED_MANIFEST[key]
    if observed_qualified != expected_qualified:
        errors.append(
            f"{key[0]}:{key[1]} D5 qualified-call surface drifted: "
            f"missing={sorted(expected_qualified - observed_qualified)} "
            f"extra={sorted(observed_qualified - expected_qualified)}"
        )
    macros = macro_calls(body)
    if macros:
        errors.append(f"{key[0]}:{key[1]} D5 macro call surface is not audited: {sorted(macros)}")


def check_sources(sources: dict[str, str]) -> list[str]:
    errors: list[str] = []
    live = {path: live_rust(source) for path, source in sources.items()}
    joined_live = "\n".join(live.values())

    owner = compact(live.get(BOUNDARY_PATH, ""))
    expected_owner = (
        "pub(crate)constKMD_D2_OWNER_ENABLED:bool="
        "matches!(SURFACE,WddmSurface::Wddm3_2GpuMmu);"
    )
    if owner.count(expected_owner) != 1:
        errors.append(
            f"{BOUNDARY_PATH}: D5 authority must be the sole SURFACE-derived 3.2 predicate"
        )
    if not re.search(
        r"\bconst\s+SURFACE\s*:\s*WddmSurface\s*=\s*WddmSurface::Wddm3_2GpuMmu\s*;",
        live.get(SURFACE_PATH, ""),
    ):
        errors.append(f"{SURFACE_PATH}: active D5 requires the exact WDDM 3.2 surface")

    bodies: dict[tuple[str, str], tuple[str, str]] = {}
    for key in CALL_MANIFEST:
        body = unique_function(sources, key[0], key[1], errors)
        if body is not None:
            bodies[key] = body
            check_call_surface(key, body[1], errors)

    handler = bodies.get((DISPLAY, "present_mpo_d5"))
    if handler is not None:
        handler_compact = compact(handler[1])
        guard = (
            "if!crate::virtio::KMD_D2_OWNER_ENABLED{"
            "returnrefuse_mpo_present(MpoPresentRefusal::DisabledOwnerBoundary);}"
        )
        guard_at = handler_compact.find(guard)
        prepare_at = handler_compact.find("payload.prepare_mpo_present(args)")
        emit_at = handler_compact.find("plan.emit_mpo_present(args)")
        if guard_at < 0 or prepare_at < 0 or not (guard_at < prepare_at):
            errors.append(f"{DISPLAY}: D5 entry lost its dominating activation-coherence guard")
        if not (0 <= prepare_at < emit_at):
            errors.append(f"{DISPLAY}: D5 packet emission is not dominated by complete prepare")
        prefix = handler_compact[: max(guard_at, 0)]
        if any(
            token in prefix
            for token in (
                "prepare_mpo_present(",
                "open_allocation",
                "open_direct_scanout",
                "emit_mpo_present(",
            )
        ):
            errors.append(f"{DISPLAY}: D5 dereference/allocation/packet work precedes owner guard")

    all_compact = compact(joined_live)
    if all_compact.count("present_mpo_d5(") != 2:
        errors.append(f"{DISPLAY}: D5 entry must have exactly one caller")
    if (
        all_compact.count(".prepare_mpo_present(") != 1
        or all_compact.count("fnprepare_mpo_present(") != 1
        or "PresentMpoPayload::prepare_mpo_present(" in all_compact
    ):
        errors.append(f"{PACKET}: MPO prepare must have exactly one guarded call site")
    if (
        all_compact.count(".emit_mpo_present(") != 1
        or all_compact.count("fnemit_mpo_present(") != 1
    ):
        errors.append(f"{PACKET}: MPO emit must have exactly one prepared call site")

    inner = unique_function(sources, DISPLAY, "dxgkddi_present_inner", errors)
    if inner is not None:
        inner_compact = compact(inner[1])
        branch = (
            "PresentPayload::MultiPlaneOverlay(mpo)=>{"
            "returnunsafe{present_mpo_d5(args,mpo)};}"
        )
        branch_at = inner_compact.find(branch)
        legacy_at = inner_compact.find("PresentAllocations::from_allocation_list")
        if branch_at < 0 or legacy_at < 0 or branch_at >= legacy_at:
            errors.append(
                f"{DISPLAY}: MPO union arm must return through D5 before allocation-list decode"
            )

    decoder = unique_decode(sources, errors)
    if decoder is not None:
        decode_compact = compact(decoder[1])
        if decode_compact.count("args.Flags.__bindgen_anon_1.Value") != 1:
            errors.append(f"{PACKET}: active Present union arm is not selected exactly once")
        if decode_compact.count("pPresentMultiPlaneOverlayInfo") != 1:
            errors.append(f"{PACKET}: MPO union pointer must be read exactly once")
        if "args.Reserved" in decoder[1]:
            errors.append(f"{PACKET}: D5 must ignore DXGKARG_PRESENT system-reserved storage")
        if "Self::MultiPlaneOverlay(PresentMpoPayload{" not in decode_compact:
            errors.append(f"{PACKET}: MPO variant lost its provenance-carrying payload")
        if decode_compact.find("FLAG_FLIP_WITH_MPO") > decode_compact.find(
            "pPresentMultiPlaneOverlayInfo"
        ):
            errors.append(f"{PACKET}: MPO union pointer is read before its selecting flag")

    prepare = bodies.get((PACKET, "prepare_mpo_present"))
    prepare_compact = compact(prepare[1]) if prepare is not None else ""
    if prepare is not None:
        required_fragments = (
            "self.info.is_null()||!self.info.is_aligned()",
            "info.PlaneListCount!=MPO_MAX_PLANES",
            "info.VidPnSourceId!=0",
            "self.present_flags&MPO_PRESENT_RESERVED_FLAGS!=0",
            "self.present_flags!=PresentPayload::FLAG_FLIP_WITH_MPO",
            "plane_pointer.is_null()||!plane_pointer.is_aligned()",
            "plane.LayerIndex!=0",
            "plane.Enabled!=1",
            "plane.__bindgen_anon_1.Reserved()!=0",
            "open_direct_scanout_allocation_facts(open_handle)",
            "open_allocation_identity(open_handle)",
            "allocation_object::is_current(identity.generation)",
            "exact_mpo_primary_profile(&facts,identity,info.VidPnSourceId)",
            "segment_id:plane.__bindgen_anon_1.SegmentId()",
            "physical_address:unsafe{plane.PhysicalAddress.QuadPartasu64}",
        )
        for fragment in required_fragments:
            if fragment not in prepare_compact:
                errors.append(f"{PACKET}: D5 validation/provenance fragment missing: {fragment}")
        info_check = prepare_compact.find("self.info.is_null()||!self.info.is_aligned()")
        info_ref = prepare_compact.find("&*self.info")
        count_check = prepare_compact.find("info.PlaneListCount!=MPO_MAX_PLANES")
        plane_ref = prepare_compact.find("&*plane_pointer")
        if not (0 <= info_check < info_ref):
            errors.append(f"{PACKET}: MPO info reference precedes null/alignment validation")
        if not (0 <= count_check < plane_ref):
            errors.append(f"{PACKET}: PlaneListCount is not bounded before the plane reference")
        if "core::slice" in prepare_compact or ".add(" in prepare_compact:
            errors.append(f"{PACKET}: one-plane D5 must not form a slice or do plane pointer arithmetic")
        profile_at = prepare_compact.find("exact_mpo_primary_profile")
        placement_at = prepare_compact.find("physical_address:")
        capacity_at = prepare_compact.find("letdma_bytes=")
        if not (0 <= profile_at < placement_at < capacity_at):
            errors.append(
                f"{PACKET}: canonical identity/profile, exact placement, and capacity order drifted"
            )
        if any(
            token in prepare_compact
            for token in (
                "core::ptr::write",
                "args.pDmaBuffer=",
                "args.MultipassOffset=",
                "args.pPatchLocationListOut=",
            )
        ):
            errors.append(f"{PACKET}: D5 prepare mutates output before validation completes")
        capacity_fragments = (
            "output_capacity(args.pDmaBuffer,args.DmaSizeasusize,dma_bytes)",
            "output_capacity(args.pDmaBufferPrivateData,args.DmaBufferPrivateDataSizeasusize,MPO_PRIVATE_BYTES,)",
            "output_capacity(args.pPatchLocationListOut,args.PatchLocationListOutSizeasusize,MPO_PATCH_REFERENCES,)",
        )
        for fragment in capacity_fragments:
            if fragment not in prepare_compact:
                errors.append(f"{PACKET}: complete D5 output capacity proof missing: {fragment}")
        command_fragments = (
            "letSome(next_dma_address)=(args.pDmaBufferasusize).checked_add(dma_bytes)else{",
            "dma:args.pDmaBuffer",
            "next_dma:next_dma_addressas*mutc_void",
        )
        for fragment in command_fragments:
            if fragment not in prepare_compact:
                errors.append(f"{PACKET}: ordinary MPO packet contract drifted: {fragment}")

    packet_live = live.get(PACKET, "")
    if not re.search(r"\bconst\s+MPO_MAX_PLANES\s*:\s*u32\s*=\s*1\s*;", packet_live):
        errors.append(f"{PACKET}: D5 MaxPlanes must remain exactly one")
    if not re.search(r"\bconst\s+MPO_PRIVATE_BYTES\s*:\s*usize\s*=\s*0\s*;", packet_live):
        errors.append(f"{PACKET}: ordinary MPO packet must not claim private-data bytes")
    if not re.search(r"\bconst\s+MPO_PATCH_REFERENCES\s*:\s*usize\s*=\s*0\s*;", packet_live):
        errors.append(f"{PACKET}: WDK 28000 MPO packet must not invent patch indices")
    if not re.search(
        r"\bconst\s+MPO_PRESENT_RESERVED_FLAGS\s*:\s*u32\s*=\s*0xFFFF_C000\s*;",
        packet_live,
    ):
        errors.append(f"{PACKET}: WDK 28000 Present reserved-bit mask drifted")

    profile = bodies.get((PACKET, "exact_mpo_primary_profile"))
    if profile is not None:
        profile_compact = compact(profile[1])
        required_profile = (
            "validate_create_output(HELIOS_PACKAGE_GENERATION)",
            "identity.generation!=facts.allocation_generation",
            "identity.generation!=allocation.allocation_generation",
            "identity.kind!=allocation.allocation_kind",
            "identity.byte_size!=allocation.byte_size",
            "facts.backing_size<allocation.byte_size",
            "allocation.plane_count==MPO_MAX_PLANES",
            "allocation.vidpn_source==source_id",
        )
        for fragment in required_profile:
            if fragment not in profile_compact:
                errors.append(f"{PACKET}: exact allocation/profile check missing: {fragment}")

    emit = bodies.get((PACKET, "emit_mpo_present"))
    if emit is not None:
        emit_compact = compact(emit[1])
        for fragment in (
            "self.plane.open_handle.as_ptr()",
            "self.plane.segment_id",
            "self.plane.physical_address",
            "core::ptr::write_unaligned",
            "args.pDmaBuffer=self.next_dma",
            "args.MultipassOffset=0",
        ):
            if fragment not in emit_compact:
                errors.append(f"{PACKET}: exact planned MPO emission drifted: {fragment}")

    d5_function_live = "\n".join(body[1] for body in bodies.values())
    forbidden = (
        "pAllocationList",
        "PresentAllocations::from_allocation_list",
        "present_alloc_info",
        "resource_id",
        ".width",
        ".height",
        "current_scanout",
        "scanout_allocation_for_resource",
        "validate_direct_scanout_binding",
        "retain_candidate",
        "PlaneState",
        "SET_SCANOUT_BLOB",
        "set_scanout_blob",
        "with_virtio",
        "control_owner",
        "wait_fence",
        "KeWait",
        "cleanup",
        "display_lease",
        "PresentFlipPrivate::write",
        "PresentSubmissionPrivate",
        "DXGK_PRESENT_SOURCE_INDEX",
    )
    for spelling in forbidden:
        if spelling in d5_function_live:
            errors.append(f"D5 Present closure contains forbidden identity/lease/work route {spelling}")

    d5_start = packet_live.find("enum PresentPayload")
    d5_end = packet_live.find("struct PresentAllocationList", d5_start)
    d5_region = packet_live[d5_start:d5_end] if 0 <= d5_start < d5_end else ""
    if not d5_region:
        errors.append(f"{PACKET}: cannot isolate D5 type/implementation region")
    else:
        if re.search(r"\b(?:Hps|HPS|Ticket|Lookup|Registry)\w*\b", d5_region):
            errors.append(f"{PACKET}: D5 introduced a private HPS/ticket/lookup packet")
        if d5_region.count("physical_address") != 3 or d5_region.count("segment_id") != 3:
            errors.append(f"{PACKET}: exact MPO handle/segment/address pairing drifted")

    refusal_variants = (
        ("DisabledOwnerBoundary", "D5_DISABLED_OWNER_REFUSALS", b"D5Disabled"),
        ("MpoInfoPointer", "D5_MPO_INFO_POINTER_REFUSALS", b"D5InfoPtr"),
        ("PlaneListCount", "D5_PLANE_LIST_COUNT_REFUSALS", b"D5PlaneCnt"),
        ("PlaneListPointer", "D5_PLANE_LIST_POINTER_REFUSALS", b"D5PlanePtr"),
        ("SourceOrLayer", "D5_SOURCE_OR_LAYER_REFUSALS", b"D5SrcLayer"),
        (
            "DisabledOrMalformedPlane",
            "D5_DISABLED_OR_MALFORMED_PLANE_REFUSALS",
            b"D5PlaneBad",
        ),
        ("ReservedBits", "D5_RESERVED_BITS_REFUSALS", b"D5Reserved"),
        ("OpenAllocation", "D5_OPEN_ALLOCATION_REFUSALS", b"D5OpenBad"),
        (
            "AllocationIdentityOrProfile",
            "D5_ALLOCATION_PROFILE_REFUSALS",
            b"D5Profile",
        ),
        ("OutputCapacity", "D5_OUTPUT_CAPACITY_REFUSALS", b"D5Capacity"),
        (
            "PacketConstruction",
            "D5_PACKET_CONSTRUCTION_REFUSALS",
            b"D5Packet",
        ),
    )
    display_raw = sources.get(DISPLAY, "")
    for variant, counter, name in refusal_variants:
        if not re.search(rf"\b{re.escape(variant)}\b", d5_region):
            errors.append(f"{PACKET}: typed D5 refusal missing {variant}")
        if display_raw.count(counter) < 2 or name not in display_raw.encode():
            errors.append(f"{DISPLAY}: named D5 refusal counter missing for {variant}")

    hwq = unique_function(sources, SCHEDULER, "dxgkddi_present_to_hw_queue", errors)
    if hwq is not None:
        hwq_full = sources[SCHEDULER][
            sources[SCHEDULER].find("pub unsafe extern \"C\" fn dxgkddi_present_to_hw_queue") :
        ]
        hwq_body = hwq[1]
        if re.search(r"\b_?args\b", hwq_body) or any(
            token in hwq_body
            for token in ("pAllocationList", "PresentPayload", "from_allocation_list")
        ):
            errors.append(f"{SCHEDULER}: PresentToHwQueue reads a refused Present union arm")
        if "STATUS_NOT_SUPPORTED" not in hwq_body or "PRESENT_HWQ_COUNT" not in hwq_body:
            errors.append(f"{SCHEDULER}: PresentToHwQueue lost its counted refusal")
        if "_args: INOUT_PDXGKARG_PRESENT" not in hwq_full[: hwq_full.find("}") + 1]:
            errors.append(f"{SCHEDULER}: PresentToHwQueue argument is no longer explicitly unread")

    flip = re.search(r"struct\s+PresentFlipPrivate\s*\{([^}]*)\}", packet_live, re.S)
    expected_fields = [
        ("magic", "u32"),
        ("version", "u32"),
        ("allocation", "u64"),
        ("physical_address", "u64"),
        ("primary_segment", "u32"),
        ("operation_flags", "u32"),
    ]
    if flip is None:
        errors.append(f"{PACKET}: D4 PresentFlipPrivate disappeared")
    else:
        fields = re.findall(r"(?m)^\s*(\w+)\s*:\s*([^,\s]+)\s*,", flip.group(1))
        if fields != expected_fields:
            errors.append(f"{PACKET}: D4 PresentFlipPrivate 32-byte v2 field layout drifted")
    for pattern, message in (
        (r"PRESENT_FLIP_PRIVATE_OFFSET\s*:\s*usize\s*=\s*32\s*;", "offset 32"),
        (r"PRESENT_DMA_PRIVATE_DATA_BYTES\s*:\s*u32\s*=\s*64\s*;", "total 64 bytes"),
        (r"PRESENT_FLIP_VERSION\s*:\s*u32\s*=\s*2\s*;", "version 2"),
    ):
        if not re.search(pattern, packet_live):
            errors.append(f"{PACKET}: D4 PresentFlipPrivate lost {message}")

    return errors


def combined_check(sources: dict[str, str]) -> list[str]:
    errors = check_sources(sources)
    errors.extend(f"D4: {error}" for error in d4_check_sources(sources))
    return errors


def replace_once(
    sources: dict[str, str], path: str, old: str, new: str
) -> dict[str, str]:
    mutated = dict(sources)
    if mutated[path].count(old) != 1:
        raise RuntimeError(f"cannot uniquely replace {old!r} in {path}")
    mutated[path] = mutated[path].replace(old, new, 1)
    return mutated


def replace_in_function(
    sources: dict[str, str], path: str, name: str, old: str, new: str
) -> dict[str, str]:
    mutated = dict(sources)
    live = live_rust(mutated[path])
    found = [item for item in functions(live) if item.name == name]
    if len(found) != 1:
        raise RuntimeError(f"cannot find unique function {path}:{name}")
    item = found[0]
    body = mutated[path][item.brace : item.end]
    if body.count(old) != 1:
        raise RuntimeError(f"cannot uniquely replace {old!r} in {path}:{name}")
    body = body.replace(old, new, 1)
    mutated[path] = mutated[path][: item.brace] + body + mutated[path][item.end :]
    return mutated


def inject_function(
    sources: dict[str, str], path: str, name: str, injection: str
) -> dict[str, str]:
    mutated = dict(sources)
    live = live_rust(mutated[path])
    found = [item for item in functions(live) if item.name == name]
    if len(found) != 1:
        raise RuntimeError(f"cannot find unique function {path}:{name}")
    at = found[0].brace + 1
    mutated[path] = mutated[path][:at] + injection + mutated[path][at:]
    return mutated


def require_rejected(
    label: str, sources: dict[str, str], needle: str | None = None
) -> None:
    errors = combined_check(sources)
    if not errors:
        raise SystemExit(f"D5 MPO mutation self-test failed: {label} was accepted")
    if needle is not None and not any(needle in error for error in errors):
        raise SystemExit(
            f"D5 MPO mutation self-test failed: {label} missed {needle!r}; got {errors!r}"
        )


def main() -> None:
    repo = os.path.abspath(sys.argv[1] if len(sys.argv) > 1 else REPO_DEFAULT)
    sources = load_sources(repo)
    errors = combined_check(sources)
    if errors:
        raise SystemExit("D5 MPO Present boundary violated:\n" + "\n".join(errors))

    decoupled = dict(sources)
    owner_anchor = (
        "pub(crate) const KMD_D2_OWNER_ENABLED: bool = "
        "matches!(SURFACE, WddmSurface::Wddm3_2GpuMmu);"
    )
    if decoupled[BOUNDARY_PATH].count(owner_anchor) != 1:
        raise SystemExit("D5 MPO mutation setup failed: owner derivation anchor drifted")
    decoupled[BOUNDARY_PATH] = decoupled[BOUNDARY_PATH].replace(
        owner_anchor,
        "pub(crate) const KMD_D2_OWNER_ENABLED: bool = true;",
        1,
    )
    require_rejected("decoupled D2 activation", decoupled, "sole SURFACE-derived")

    guard = (
        "if !crate::virtio::KMD_D2_OWNER_ENABLED {\n"
        "        return refuse_mpo_present(MpoPresentRefusal::DisabledOwnerBoundary);\n"
        "    }"
    )
    require_rejected(
        "entry guard removal",
        replace_in_function(sources, DISPLAY, "present_mpo_d5", guard, "if false { return STATUS_NOT_SUPPORTED; }"),
        "dominating activation-coherence guard",
    )
    require_rejected(
        "non-dominating decoy guard",
        replace_in_function(
            sources,
            DISPLAY,
            "present_mpo_d5",
            guard,
            "if !crate::virtio::KMD_D2_OWNER_ENABLED { let _ = false; }",
        ),
        "dominating activation-coherence guard",
    )
    require_rejected(
        "MPO pAllocationList direct read",
        inject_function(
            sources,
            DISPLAY,
            "present_mpo_d5",
            " let _ = unsafe { args.__bindgen_anon_1.pAllocationList }; ",
        ),
        "pAllocationList",
    )
    require_rejected(
        "MPO pAllocationList helper read",
        inject_function(
            sources,
            PACKET,
            "prepare_mpo_present",
            " let _ = unsafe { args.__bindgen_anon_1.pAllocationList }; ",
        ),
        "pAllocationList",
    )
    require_rejected(
        "MPO fixed allocation-list decoder",
        inject_function(
            sources,
            DISPLAY,
            "present_mpo_d5",
            " let _ = PresentAllocations::from_allocation_list(decoy); ",
        ),
        "from_allocation_list",
    )
    require_rejected(
        "DXGKARG_PRESENT system-reserved read",
        replace_once(
            sources,
            PACKET,
            "let flags = unsafe { args.Flags.__bindgen_anon_1.Value };",
            "let _ = args.Reserved;\n        let flags = unsafe { args.Flags.__bindgen_anon_1.Value };",
        ),
        "system-reserved storage",
    )
    require_rejected(
        "misaligned MPO info accepted",
        replace_in_function(
            sources,
            PACKET,
            "prepare_mpo_present",
            "if self.info.is_null() || !self.info.is_aligned() {",
            "if self.info.is_null() {",
        ),
        "validation/provenance fragment missing",
    )
    require_rejected(
        "misaligned plane list accepted",
        replace_in_function(
            sources,
            PACKET,
            "prepare_mpo_present",
            "if plane_pointer.is_null() || !plane_pointer.is_aligned() {",
            "if plane_pointer.is_null() {",
        ),
        "validation/provenance fragment missing",
    )
    require_rejected(
        "zero PlaneListCount accepted",
        replace_in_function(
            sources,
            PACKET,
            "prepare_mpo_present",
            "if info.PlaneListCount != MPO_MAX_PLANES {",
            "if info.PlaneListCount > MPO_MAX_PLANES {",
        ),
        "validation/provenance fragment missing",
    )
    require_rejected(
        "MaxPlanes exceeds one",
        replace_once(sources, PACKET, "const MPO_MAX_PLANES: u32 = 1;", "const MPO_MAX_PLANES: u32 = 2;"),
        "MaxPlanes must remain exactly one",
    )
    require_rejected(
        "disabled plane accepted",
        replace_in_function(
            sources,
            PACKET,
            "prepare_mpo_present",
            "if plane.Enabled != 1 {",
            "if plane.Enabled > 1 {",
        ),
        "validation/provenance fragment missing",
    )
    require_rejected(
        "non-primary layer accepted",
        replace_in_function(
            sources, PACKET, "prepare_mpo_present", "if plane.LayerIndex != 0 {", "if false {"
        ),
        "validation/provenance fragment missing",
    )
    require_rejected(
        "foreign VidPn source accepted",
        replace_in_function(
            sources,
            PACKET,
            "prepare_mpo_present",
            "if info.VidPnSourceId != 0 {",
            "if false {",
        ),
        "validation/provenance fragment missing",
    )
    require_rejected(
        "Present reserved flag bits accepted",
        replace_in_function(
            sources,
            PACKET,
            "prepare_mpo_present",
            "if self.present_flags & MPO_PRESENT_RESERVED_FLAGS != 0 {",
            "if false {",
        ),
        "validation/provenance fragment missing",
    )
    require_rejected(
        "reserved plane bits accepted",
        replace_in_function(
            sources,
            PACKET,
            "prepare_mpo_present",
            "if plane.__bindgen_anon_1.Reserved() != 0 {",
            "if false {",
        ),
        "validation/provenance fragment missing",
    )
    require_rejected(
        "canonical open validation removed",
        replace_once(
            sources,
            PACKET,
            "crate::ddi::create_allocation::open_direct_scanout_allocation_facts(open_handle)",
            "crate::ddi::create_allocation::open_allocation_identity(open_handle)",
        ),
        "open_direct_scanout_allocation_facts",
    )
    require_rejected(
        "published open identity validation removed",
        replace_once(
            sources,
            PACKET,
            "crate::ddi::create_allocation::open_allocation_identity(open_handle)",
            "crate::ddi::create_allocation::open_direct_scanout_allocation_facts(open_handle)",
        ),
        "open_allocation_identity",
    )
    require_rejected(
        "physical address used as allocation identity",
        replace_once(
            sources,
            PACKET,
            "open_direct_scanout_allocation_facts(open_handle)",
            "open_direct_scanout_allocation_facts(plane.PhysicalAddress.QuadPart as HANDLE)",
        ),
        "validation/provenance fragment missing",
    )
    require_rejected(
        "legacy resource-id lookup",
        inject_function(
            sources,
            PACKET,
            "prepare_mpo_present",
            " let _ = present_alloc_info(open_handle); ",
        ),
        "present_alloc_info",
    )
    require_rejected(
        "dimension used as allocation identity",
        inject_function(
            sources,
            PACKET,
            "exact_mpo_primary_profile",
            " let _ = facts.final_hwa2.width == identity.byte_size as u32; ",
        ),
        ".width",
    )
    require_rejected(
        "candidate retention from Present",
        inject_function(sources, DISPLAY, "present_mpo_d5", " retain_candidate(adapter, candidate); "),
        "retain_candidate",
    )
    require_rejected(
        "PlaneState mutation from Present",
        inject_function(sources, DISPLAY, "present_mpo_d5", " PlaneState::latch(plane); "),
        "PlaneState",
    )
    require_rejected(
        "SET_SCANOUT_BLOB from Present",
        inject_function(sources, DISPLAY, "present_mpo_d5", " set_scanout_blob(adapter, plane); "),
        "set_scanout_blob",
    )
    require_rejected(
        "PresentToHwQueue union read",
        inject_function(
            sources,
            SCHEDULER,
            "dxgkddi_present_to_hw_queue",
            " let _ = unsafe { (*_args).__bindgen_anon_1.pAllocationList }; ",
        ),
        "PresentToHwQueue reads",
    )
    private_packet = dict(sources)
    private_packet[PACKET] = private_packet[PACKET].replace(
        "pub(crate) enum MpoPresentRefusal {",
        "struct HpsPresentTicket { key: u64 }\npub(crate) enum MpoPresentRefusal {",
        1,
    )
    require_rejected("private HPS ticket packet", private_packet, "private HPS/ticket/lookup")
    require_rejected(
        "classic allocation index forced onto MPO",
        inject_function(
            sources,
            PACKET,
            "prepare_mpo_present",
            " let _ = DXGK_PRESENT_SOURCE_INDEX; ",
        ),
        "DXGK_PRESENT_SOURCE_INDEX",
    )
    require_rejected(
        "D4 flip record minted by MPO Present",
        inject_function(
            sources,
            DISPLAY,
            "present_mpo_d5",
            " let _ = PresentFlipPrivate::write(a, b, c, d, e, f); ",
        ),
        "PresentFlipPrivate::write",
    )
    require_rejected(
        "partial output cursor mutation during prepare",
        inject_function(
            sources, PACKET, "prepare_mpo_present", " args.MultipassOffset = 1; "
        ),
        "mutates output before validation completes",
    )
    require_rejected(
        "private-data capacity invented",
        replace_once(sources, PACKET, "const MPO_PRIVATE_BYTES: usize = 0;", "const MPO_PRIVATE_BYTES: usize = 64;"),
        "must not claim private-data bytes",
    )
    require_rejected(
        "patch-list mapping invented",
        replace_once(
            sources,
            PACKET,
            "const MPO_PATCH_REFERENCES: usize = 0;",
            "const MPO_PATCH_REFERENCES: usize = 1;",
        ),
        "must not invent patch indices",
    )
    require_rejected(
        "DMA capacity proof removed",
        replace_in_function(
            sources,
            PACKET,
            "prepare_mpo_present",
            "if !output_capacity(args.pDmaBuffer, args.DmaSize as usize, dma_bytes)",
            "if false",
        ),
        "complete D5 output capacity proof missing",
    )
    require_rejected(
        "packet construction overflow check removed",
        replace_in_function(
            sources,
            PACKET,
            "prepare_mpo_present",
            "let Some(next_dma_address) = (args.pDmaBuffer as usize).checked_add(dma_bytes) else {",
            "let next_dma_address = (args.pDmaBuffer as usize).wrapping_add(dma_bytes); if false {",
        ),
        "ordinary MPO packet contract drifted",
    )
    require_rejected(
        "stale allocation epoch accepted",
        replace_once(
            sources,
            PACKET,
            "if !crate::adapter::allocation_object::is_current(identity.generation) {",
            "if false {",
        ),
        "validation/provenance fragment missing",
    )
    require_rejected(
        "exact segment provenance replaced",
        replace_once(
            sources,
            PACKET,
            "segment_id: plane.__bindgen_anon_1.SegmentId(),",
            "segment_id: 0,",
        ),
        "validation/provenance fragment missing",
    )
    require_rejected(
        "exact physical-address provenance replaced",
        replace_once(
            sources,
            PACKET,
            "physical_address: unsafe { plane.PhysicalAddress.QuadPart as u64 },",
            "physical_address: 0,",
        ),
        "validation/provenance fragment missing",
    )
    require_rejected(
        "typed packet-construction counter removed",
        replace_once(sources, DISPLAY, 'b"D5Packet".as_slice()', 'b"D5Other".as_slice()'),
        "named D5 refusal counter missing",
    )
    require_rejected(
        "D4 private offset weakened",
        replace_once(
            sources,
            PACKET,
            "pub(crate) const PRESENT_FLIP_PRIVATE_OFFSET: usize = 32;",
            "pub(crate) const PRESENT_FLIP_PRIVATE_OFFSET: usize = 16;",
        ),
        "D4 PresentFlipPrivate lost offset 32",
    )
    require_rejected(
        "D4 DIRQL allocation route",
        D4["mutate_function"](
            sources,
            "kmd_render/src/virtio/gpu/mod.rs",
            "enqueue_direct_locked",
            " let _ = DmaBuffer::new(passive, 1); ",
        ),
        "D4:",
    )

    print(
        "OK: active D5 MPO Present guard, exact one-plane allocation provenance, "
        "ordinary zero-marker packet, no-lease closure, D4 proof, and mutations are enforced"
    )


if __name__ == "__main__":
    main()
