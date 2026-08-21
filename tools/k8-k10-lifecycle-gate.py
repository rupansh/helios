#!/usr/bin/env python3
"""Focused source and mutation gate for the non-HPM1 K8/K10 closure."""

from __future__ import annotations

import os
import re
import sys
from dataclasses import dataclass


REPO_DEFAULT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
LOCAL = "kmd_render/src/ddi/local_segment.rs"
SEGMENTS = "kmd_render/src/ddi/segment_table.rs"
QUERY = "kmd_render/src/ddi/query_adapter_info.rs"
LIFECYCLE = "kmd_render/src/ddi/lifecycle.rs"
SUBMIT = "kmd_render/src/ddi/submit_command.rs"
LIB = "kmd_render/src/lib.rs"
DELETED = (
    "kmd_render/src/ddi/bar_segment.rs",
    "protocol/src/physical_memory.rs",
)


def live_rust(source: str) -> str:
    """Blank comments and literals without changing offsets or brace layout."""
    out = list(source)
    i = 0
    while i < len(source):
        if source.startswith("//", i):
            end = source.find("\n", i)
            end = len(source) if end < 0 else end
            out[i:end] = " " * (end - i)
            i = end
        elif source.startswith("/*", i):
            depth, end = 1, i + 2
            while end < len(source) and depth:
                if source.startswith("/*", end):
                    depth += 1
                    end += 2
                elif source.startswith("*/", end):
                    depth -= 1
                    end += 2
                else:
                    end += 1
            out[i:end] = " " * (end - i)
            i = end
        elif source[i] == '"':
            end = i + 1
            while end < len(source):
                if source[end] == "\\":
                    end += 2
                elif source[end] == '"':
                    end += 1
                    break
                else:
                    end += 1
            out[i:end] = " " * (end - i)
            i = end
        else:
            i += 1
    return "".join(out)


def function_body(source: str, name: str) -> str:
    live = live_rust(source)
    match = re.search(rf"\bfn\s+{re.escape(name)}\s*\(", live)
    if not match:
        return ""
    brace = live.find("{", match.end())
    if brace < 0:
        return ""
    depth = 0
    for index in range(brace, len(live)):
        if live[index] == "{":
            depth += 1
        elif live[index] == "}":
            depth -= 1
            if depth == 0:
                return live[brace + 1 : index]
    return ""


def require_order(errors: list[str], label: str, body: str, needles: tuple[str, ...]) -> None:
    cursor = -1
    for needle in needles:
        found = body.find(needle, cursor + 1)
        if found < 0:
            errors.append(f"{label}: missing ordered lifecycle edge: {needle}")
            return
        cursor = found


def check(sources: dict[str, str], existing: set[str]) -> list[str]:
    errors: list[str] = []
    for path in DELETED:
        if path in existing:
            errors.append(f"{path}: retired non-HPM1 carrier returned")

    local = live_rust(sources[LOCAL])
    for required in (
        "let size = gpu.host_visible()?.len;",
        "VIDMM_LOCAL_MIN_BYTES: u64 = 256 << 20",
        "VIDMM_LOCAL_MAX_BYTES: u64 = 64 << 30",
        "size & 4095 != 0",
        "seg_id: crate::ddi::gpummu::MEMORY_SEGMENT_ID",
    ):
        if required not in local:
            errors.append(f"{LOCAL}: deterministic local-capacity contract lost: {required}")
    if re.search(r"Registry|CpuHostAperture|MapCpuHost|HPM1|topology", local, re.I):
        errors.append(f"{LOCAL}: retired policy or CPU-host-aperture authority returned")

    segments = live_rust(sources[SEGMENTS])
    for required in (
        "entries: [Some(SegmentSpec::Aperture), None]",
        "Some(SegmentSpec::Aperture)",
        "Some(SegmentSpec::Local { gpu_base, size })",
        ".map(|(idx, spec)| (idx as u32 + 1, *spec))",
    ):
        if required not in segments:
            errors.append(f"{SEGMENTS}: immutable aperture/local ordering lost: {required}")

    query = live_rust(sources[QUERY])
    local_spec = function_body(sources[QUERY], "local")
    local_writer = function_body(sources[QUERY], "write_local_memory_descriptor")
    for required in (
        "kind: SegmentKind::Memory",
        "cache_coherent: false",
        "application_target: true",
        "local_budget_group: true",
    ):
        if required not in local_spec:
            errors.append(f"{QUERY}: non-CPU-visible local descriptor drifted: {required}")
    if "set_CpuVisible" in query or "set_Aperture(1)" in local_writer:
        errors.append(f"{QUERY}: local memory became a CPU-visible/aperture mapping")
    if "crate::virtio::KMD_D2_OWNER_ENABLED" not in local_writer:
        errors.append(f"{QUERY}: local DirectFlip is no longer surface-derived")

    lib = live_rust(sources[LIB])
    if "core::mem::zeroed()" not in lib:
        errors.append(f"{LIB}: DDI table is no longer zero-initialized")
    for slot in ("DxgkDdiEscape", "DxgkDdiMapCpuHostAperture", "DxgkDdiUnmapCpuHostAperture"):
        assignments = re.findall(rf"\bdata\.{slot}\s*=\s*([^;]+);", lib)
        if len(assignments) > 1 or any(value.strip() != "None" for value in assignments):
            errors.append(f"{LIB}: disabled slot was registered: {slot}")
    if "data.DxgkDdiSetAllocationBackingStore = Some(ddi::dxgkddi_set_allocation_backing_store);" not in lib:
        errors.append(f"{LIB}: K2a ShareBackingStoreWithKmd registration lost")

    restart = function_body(sources[SUBMIT], "dxgkddi_restart_from_timeout")
    for required in (
        "let local_capacity = adapter.local_segment().map(|segment| segment.size);",
        ".host_visible()",
        ".is_some_and(|window| window.len == expected)",
    ):
        if required not in restart:
            errors.append(f"{SUBMIT}: restart local-capacity identity check drifted: {required}")
    if re.search(r"window\.len\s*(?:>=|<=|>|<)\s*expected", restart):
        errors.append(f"{SUBMIT}: restart accepts an inexact local-capacity generation")

    lifecycle = sources[LIFECYCLE]
    live_lifecycle = live_rust(lifecycle)
    if len(re.findall(r"\bfn\s+bring_up_venus\s*\(", live_lifecycle)) != 1:
        errors.append(f"{LIFECYCLE}: bring_up_venus owner definition drifted")
    if len(re.findall(r"(?<!fn )\bbring_up_venus\s*\(", live_lifecycle)) != 1:
        errors.append(f"{LIFECYCLE}: StartDevice Venus-owner call drifted")
    if len(re.findall(r"\bbring_up_venus\s*\(", live_rust(sources[SUBMIT]))) != 1:
        errors.append(f"{SUBMIT}: restart Venus-owner call drifted")

    common_prefix = (
        "native_fence::invalidate_all",
        "allocation_object::invalidate_all",
        "close_k11_completions_and_wait",
    )
    require_order(
        errors,
        "retire_skipped_stop_transport",
        function_body(lifecycle, "retire_skipped_stop_transport"),
        common_prefix
        + (
            "stop_vsync",
            "stop_hpd",
            "prepare_reset",
            "reset_display_publication_state",
            "set_venus_client(None)",
            "close_control_owner_transport",
            "retire_control_owner_transport",
            "complete_verified_reset",
            "remove_virtio_and_reset_scanout_bind_generation",
            "set_transport_generation(None)",
        ),
    )
    require_order(
        errors,
        "dxgkddi_stop_device",
        function_body(lifecycle, "dxgkddi_stop_device"),
        common_prefix
        + (
            "reset_display_publication_state",
            "close_control_owner_transport",
            "set_venus_client(None)",
            "retire_control_owner_transport",
            "complete_verified_reset",
            "remove_virtio_and_reset_scanout_bind_generation",
            "set_transport_generation(None)",
        ),
    )
    require_order(
        errors,
        "dxgkddi_remove_device",
        function_body(lifecycle, "dxgkddi_remove_device"),
        common_prefix
        + (
            "stop_vsync",
            "stop_hpd",
            "reset_display_publication_state",
            "prepare_reset",
            "set_venus_client(None)",
            "close_control_owner_transport",
            "retire_control_owner_transport",
            "complete_verified_reset",
            "remove_virtio_and_reset_scanout_bind_generation",
            "set_transport_generation(None)",
            "complete_removal",
        ),
    )
    require_order(
        errors,
        "dxgkddi_reset_from_timeout",
        function_body(sources[SUBMIT], "dxgkddi_reset_from_timeout"),
        common_prefix + ("abandon_pending_submissions", "stop_vsync", "stop_hpd"),
    )
    return errors


@dataclass(frozen=True)
class Mutation:
    name: str
    path: str
    old: str
    new: str


def run_mutations(sources: dict[str, str], existing: set[str]) -> None:
    cases = (
        Mutation("hard-code local capacity", LOCAL, "gpu.host_visible()?.len", "512 << 20"),
        Mutation("make local memory CPU visible", QUERY, "cache_coherent: false", "cache_coherent: true\n            // set_CpuVisible"),
        Mutation("register CPU host aperture", LIB, "data.DxgkDdiMapCpuHostAperture = None;", "data.DxgkDdiMapCpuHostAperture = Some(ddi::map_cpu_host_aperture);"),
        Mutation("lose skipped-stop allocation invalidation", LIFECYCLE, "crate::adapter::allocation_object::invalidate_all();", ""),
        Mutation("accept a larger replacement BAR", SUBMIT, "window.len == expected", "window.len >= expected"),
        Mutation("drop K2a registration", LIB, "data.DxgkDdiSetAllocationBackingStore = Some(ddi::dxgkddi_set_allocation_backing_store);", "data.DxgkDdiSetAllocationBackingStore = None;"),
        Mutation("reorder K11 close after teardown", LIFECYCLE, "adapter.close_k11_completions_and_wait(passive);", ""),
        Mutation("drop restart Venus owner", SUBMIT, "super::lifecycle::bring_up_venus(passive, adapter)", "0"),
    )
    for case in cases:
        before = sources[case.path]
        if case.old not in before:
            raise SystemExit(f"K8/K10 mutation anchor missing: {case.name}")
        mutated = dict(sources)
        mutated[case.path] = before.replace(case.old, case.new, 1)
        if not check(mutated, existing):
            raise SystemExit(f"K8/K10 mutation accepted: {case.name}")
    print(f"OK: {len(cases)} in-memory K8/K10 mutations rejected")


def main() -> None:
    repo = os.path.abspath(sys.argv[1] if len(sys.argv) > 1 else REPO_DEFAULT)
    paths = (LOCAL, SEGMENTS, QUERY, LIFECYCLE, SUBMIT, LIB)
    sources: dict[str, str] = {}
    for path in paths:
        with open(os.path.join(repo, path), encoding="utf-8") as stream:
            sources[path] = stream.read()
    existing = {
        path for path in DELETED if os.path.exists(os.path.join(repo, path))
    }
    errors = check(sources, existing)
    if errors:
        raise SystemExit("K8/K10 lifecycle gate violated:\n" + "\n".join(errors))
    run_mutations(sources, existing)
    print("OK: deterministic K8 caps and K10 reverse teardown preserve K2a/K7/K9/K11")


if __name__ == "__main__":
    main()
