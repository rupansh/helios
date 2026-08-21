#!/usr/bin/env python3
"""Protocol-only HRA1 shape, generation, and mutation gate."""

from __future__ import annotations

import os
import re
import sys
from dataclasses import dataclass


REPO_DEFAULT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
RUST = "protocol/src/resource_association.rs"
HEADER = "protocol/include/helios_resource_association.h"
CONFIG = "protocol/cbindgen-resource-association.toml"
LIB = "protocol/src/lib.rs"
DISPATCH = "protocol/include/helios_translator_dispatch.h"
RETIREMENT = "tools/retirement-gates.sh"
GENERATION_HEADERS = (
    "protocol/include/helios_diagnostics.h",
    "protocol/include/helios_native_fence.h",
    "protocol/include/helios_native_render.h",
    "protocol/include/helios_translation_session.h",
    "protocol/include/helios_wddm.h",
)
SOURCES = (RUST, HEADER, CONFIG, LIB, DISPATCH, RETIREMENT, *GENERATION_HEADERS)


def load_sources(repo: str) -> dict[str, str]:
    out: dict[str, str] = {}
    for path in SOURCES:
        with open(os.path.join(repo, path), encoding="utf-8", errors="replace") as stream:
            out[path] = stream.read()
    return out


def compact(value: str) -> str:
    return re.sub(r"\s+", "", value)


def require(path: str, source: str, fragments: tuple[str, ...], errors: list[str]) -> None:
    value = compact(source)
    for fragment in fragments:
        if compact(fragment) not in value:
            errors.append(f"{path}: missing HRA1 invariant: {fragment}")


def struct_body(source: str, declaration: str) -> str:
    match = re.search(rf"\b{re.escape(declaration)}\b\s*\{{", source)
    if not match:
        return ""
    start = source.find("{", match.start())
    end = source.find("}", start + 1)
    return source[start + 1 : end] if end >= 0 else ""


def check_sources(s: dict[str, str]) -> list[str]:
    errors: list[str] = []

    require(
        RUST,
        s[RUST],
        (
            "pub const HELIOS_RESOURCE_ASSOCIATION_ABI_VERSION: u32 = 1",
            "pub const HELIOS_RESOURCE_ASSOCIATION_BYTES: u32 = 72",
            "pub p_next: *const c_void",
            "pub package_generation: u64",
            "pub device_generation: u64",
            "pub outer_allocation_token: u64",
            "pub outer_allocation_bytes: u64",
            "pub cpu_mapping: *mut c_void",
            "pub association_flags: u32",
            "self.package_generation == 0",
            "expected_device_generation == 0",
            "self.device_generation != expected_device_generation",
            "self.outer_allocation_token == 0",
            "self.outer_allocation_bytes == 0",
            "!= !self.cpu_mapping.is_null()",
            "core::mem::size_of::<HeliosResourceAssociationV1>() == 72",
        ),
        errors,
    )
    require(
        HEADER,
        s[HEADER],
        (
            "typedef struct HeliosResourceAssociationV1",
            "sizeof(HeliosResourceAssociationV1) == 72",
            "HELIOS_RESOURCE_ASSOCIATION_ALIGNOF(HeliosResourceAssociationV1) == 8",
            "offsetof(HeliosResourceAssociationV1, outer_allocation_token) == 40",
            "offsetof(HeliosResourceAssociationV1, cpu_mapping) == 56",
            "offsetof(HeliosResourceAssociationV1, reserved1) == 68",
        ),
        errors,
    )

    rust_fields = re.findall(
        r"^\s*pub\s+(\w+)\s*:",
        struct_body(s[RUST], "pub struct HeliosResourceAssociationV1"),
        flags=re.MULTILINE,
    )
    c_fields = re.findall(
        r"^\s*(?:const\s+)?(?:void|uint32_t|uint64_t)\s*\*?\s*(\w+)\s*;",
        struct_body(s[HEADER], "typedef struct HeliosResourceAssociationV1"),
        flags=re.MULTILINE,
    )
    expected_fields = [
        "s_type",
        "struct_bytes",
        "p_next",
        "abi_version",
        "reserved",
        "package_generation",
        "device_generation",
        "outer_allocation_token",
        "outer_allocation_bytes",
        "cpu_mapping",
        "association_flags",
        "reserved1",
    ]
    if rust_fields != expected_fields:
        errors.append(f"{RUST}: HRA1 field list drifted: {rust_fields!r}")
    if c_fields != expected_fields:
        errors.append(f"{HEADER}: HRA1 C field list drifted: {c_fields!r}")

    body = compact(struct_body(s[RUST], "pub struct HeliosResourceAssociationV1")).lower()
    for forbidden in (
        "host_resid",
        "wddm_handle",
        "allocation_list_index",
        "gpuva",
        "gpu_virtual_address",
        "allocation_generation",
        "session_capability",
        "resource_id",
        "process_id",
        "pid",
        "name:",
    ):
        if forbidden in body:
            errors.append(f"{RUST}: HRA1 gained forbidden identity field {forbidden}")
    if re.search(r"\b(?:extern\s+\"C\"\s+fn|Pfn|callback|register)\b", body, re.IGNORECASE):
        errors.append(f"{RUST}: HRA1 became a callable registration surface")

    require(
        LIB,
        s[LIB],
        (
            "pub mod resource_association;",
            "pub use resource_association::*;",
            "pub const HELIOS_PACKAGE_GENERATION_ORDINAL: u32 = 4",
            "0x4845_4C49_0000_0004",
        ),
        errors,
    )
    for path in GENERATION_HEADERS:
        require(
            path,
            s[path],
            (
                "HELIOS_PACKAGE_GENERATION_ORDINAL 4u",
                "0x48454C4900000004",
            ),
            errors,
        )

    require(
        DISPATCH,
        s[DISPATCH],
        (
            "sizeof(HeliosTranslatorDispatchV1) == 112",
            "24-byte header + 11 slots",
            "(sizeof(HeliosTranslatorDispatchV1) - 24) / 8 == 11",
        ),
        errors,
    )
    dispatch_body = struct_body(s[DISPATCH], "typedef struct HeliosTranslatorDispatchV1")
    dispatch_slots = re.findall(
        r"^\s*PFN_helios_translator_\w+\s+(\w+)\s*;",
        dispatch_body,
        flags=re.MULTILINE,
    )
    expected_slots = [
        "get_instance_proc_addr",
        "enumerate_endpoints",
        "build_queue_attach",
        "attach_outer_context",
        "detach_outer_context",
        "open_outer_scope",
        "seal_outer_scope",
        "copy_sealed_batch",
        "close_outer_scope",
        "query_refusal_counters",
        "destroy_instance",
    ]
    if dispatch_slots != expected_slots:
        errors.append(f"{DISPATCH}: fixed 11-slot A5 table drifted: {dispatch_slots!r}")

    require(
        CONFIG,
        s[CONFIG],
        (
            'include_guard = "HELIOS_RESOURCE_ASSOCIATION_H"',
            'includes = ["helios_wddm.h"]',
            '"HeliosResourceAssociationV1"',
        ),
        errors,
    )
    gate_line = 'python3 "$REPO/tools/resource-association-gate.py" "$REPO" --mutations'
    if s[RETIREMENT].count(gate_line) != 1:
        errors.append(f"{RETIREMENT}: HRA1 gate must be integrated exactly once")
    return errors


@dataclass(frozen=True)
class Mutation:
    name: str
    path: str
    old: str
    new: str


def mutation_cases() -> tuple[Mutation, ...]:
    return (
        Mutation(
            "zero token admitted",
            RUST,
            "if self.outer_allocation_token == 0",
            "if false",
        ),
        Mutation(
            "foreign device admitted",
            RUST,
            "|| self.device_generation != expected_device_generation",
            "|| false",
        ),
        Mutation(
            "forbidden GPUVA identity",
            RUST,
            "pub reserved1: u32,",
            "pub gpu_virtual_address: u64,\n    pub reserved1: u32,",
        ),
        Mutation(
            "C mirror size drift",
            HEADER,
            "sizeof(HeliosResourceAssociationV1) == 72",
            "sizeof(HeliosResourceAssociationV1) == 64",
        ),
        Mutation(
            "package generation drift",
            GENERATION_HEADERS[0],
            "HELIOS_PACKAGE_GENERATION_ORDINAL 4u",
            "HELIOS_PACKAGE_GENERATION_ORDINAL 3u",
        ),
        Mutation(
            "generic dispatch registration",
            DISPATCH,
            "PFN_helios_translator_query_refusal_counters query_refusal_counters;",
            "PFN_helios_translator_query_refusal_counters query_refusal_counters;\n"
            "    PFN_helios_translator_query_refusal_counters register_allocation;",
        ),
    )


def run_mutations(sources: dict[str, str]) -> None:
    for case in mutation_cases():
        source = sources[case.path]
        count = source.count(case.old)
        if count != 1:
            raise SystemExit(f"HRA1 mutation setup failed for {case.name}: anchor count {count}")
        mutated = dict(sources)
        mutated[case.path] = source.replace(case.old, case.new, 1)
        if not check_sources(mutated):
            raise SystemExit(f"HRA1 mutation was accepted: {case.name}")
    print(f"OK: {len(mutation_cases())} in-memory HRA1 mutations rejected")


def main() -> None:
    repo = (
        os.path.abspath(sys.argv[1])
        if len(sys.argv) > 1 and not sys.argv[1].startswith("--")
        else REPO_DEFAULT
    )
    sources = load_sources(repo)
    errors = check_sources(sources)
    if errors:
        raise SystemExit("HRA1 protocol gate violated:\n" + "\n".join(errors))
    if "--mutations" in sys.argv[1:]:
        run_mutations(sources)
    print("OK: HRA1 is immutable, package-versioned, and outside the fixed A5 table")


if __name__ == "__main__":
    main()
