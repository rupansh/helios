#!/usr/bin/env python3
"""Static and mutation gate for the active HPS2 D4 DIRQL enqueue seam."""

from __future__ import annotations

import os
import re
import sys
from dataclasses import dataclass


def rust_kinds(src: str) -> bytearray:
    n = len(src)
    kind = bytearray(b"c" * n)
    ident = set("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_")
    i = 0
    while i < n:
        c = src[i]
        if c == "/" and i + 1 < n and src[i + 1] == "/":
            j = src.find("\n", i)
            j = n if j < 0 else j
            kind[i:j] = b"#" * (j - i)
            i = j
            continue
        if c == "/" and i + 1 < n and src[i + 1] == "*":
            depth, j = 1, i + 2
            while j < n and depth:
                if src[j : j + 2] == "/*":
                    depth += 1
                    j += 2
                elif src[j : j + 2] == "*/":
                    depth -= 1
                    j += 2
                else:
                    j += 1
            kind[i:j] = b"#" * (j - i)
            i = j
            continue
        if c == "r" and (i == 0 or src[i - 1] not in ident or src[i - 1] == "b"):
            j = i + 1
            while j < n and src[j] == "#":
                j += 1
            if j < n and src[j] == '"':
                close = '"' + "#" * (j - i - 1)
                e = src.find(close, j + 1)
                e = n if e < 0 else e + len(close)
                kind[i:e] = b"s" * (e - i)
                i = e
                continue
        if c == '"':
            j = i + 1
            while j < n:
                if src[j] == "\\":
                    j += 2
                elif src[j] == '"':
                    j += 1
                    break
                else:
                    j += 1
            kind[i:j] = b"s" * (j - i)
            i = j
            continue
        if c == "'":
            if i + 1 < n and src[i + 1] == "\\":
                j = i + 2
                while j < n and src[j] != "'":
                    j += 2 if src[j] == "\\" else 1
                j = min(j + 1, n)
            elif i + 2 < n and src[i + 2] == "'":
                j = i + 3
            else:
                i += 1
                continue
            kind[i:j] = b"s" * (j - i)
            i = j
            continue
        i += 1
    return kind


def live_rust(src: str) -> str:
    kinds = rust_kinds(src)
    return "".join(ch if kinds[i] == ord("c") else " " for i, ch in enumerate(src))


@dataclass(frozen=True)
class Function:
    name: str
    start: int
    brace: int
    end: int


def braced_end(live: str, brace: int) -> int | None:
    depth = 0
    for index in range(brace, len(live)):
        if live[index] == "{":
            depth += 1
        elif live[index] == "}":
            depth -= 1
            if depth == 0:
                return index + 1
    return None


def functions(live: str) -> list[Function]:
    pattern = re.compile(
        r"(?m)^\s*(?:pub(?:\([^)]*\))?\s+)?(?:const\s+)?(?:unsafe\s+)?"
        r"(?:extern\s+)?fn\s+(\w+)(?:\s*<[^>{}]*>)?\s*\("
    )
    out: list[Function] = []
    for match in pattern.finditer(live):
        brace = live.find("{", match.end())
        if brace < 0:
            continue
        end = braced_end(live, brace)
        if end is not None:
            out.append(Function(match.group(1), match.start(), brace, end))
    return out


def methods(live: str, items: list[Function], type_name: str, method_name: str) -> list[Function]:
    pattern = re.compile(
        rf"(?m)^\s*impl(?:\s*<[^>{{}}]*>)?\s+{re.escape(type_name)}"
        rf"(?:\s*<[^>{{}}]*>)?\s*\{{"
    )
    impl_ranges: list[tuple[int, int]] = []
    for match in pattern.finditer(live):
        brace = live.find("{", match.start(), match.end())
        end = braced_end(live, brace) if brace >= 0 else None
        if end is not None:
            impl_ranges.append((brace, end))
    return [
        item
        for item in items
        if item.name == method_name
        and any(start < item.start < end for start, end in impl_ranges)
    ]


def enclosing(items: list[Function], offset: int) -> Function | None:
    found = [item for item in items if item.brace <= offset < item.end]
    return min(found, key=lambda item: item.end - item.brace) if found else None


def load_sources(repo: str) -> dict[str, str]:
    sources: dict[str, str] = {}
    for relative_root in ("kmd_render/src", "kmd_logic/src", "protocol/src"):
        root = os.path.join(repo, relative_root)
        for base, _, names in os.walk(root):
            for name in names:
                if name.endswith(".rs"):
                    path = os.path.join(base, name)
                    rel = os.path.relpath(path, repo).replace(os.sep, "/")
                    with open(path, encoding="utf-8") as stream:
                        sources[rel] = stream.read()
    for rel in ("kmd_render/Cargo.toml", "kmd_render/Cargo.lock"):
        with open(os.path.join(repo, rel), encoding="utf-8") as stream:
            sources[rel] = stream.read()
    return sources


DISPLAY = "kmd_render/src/ddi/display.rs"
DIRECT = "kmd_render/src/ddi/direct_scanout.rs"
ALLOC = "kmd_render/src/ddi/create_allocation.rs"
ADAPTER = "kmd_render/src/adapter/mod.rs"
ADAPTER_SCANOUT = "kmd_render/src/adapter/scanout.rs"
GPU = "kmd_render/src/virtio/gpu/mod.rs"
HAL = "kmd_render/src/virtio/hal.rs"
CTRL = "kmd_render/src/virtio/ctrl.rs"
PACKET = "kmd_render/src/ddi/present_packet.rs"
COMMITTED = "kmd_render/src/ddi/committed_mode.rs"
SUBMIT = "kmd_render/src/ddi/submit_command.rs"
LOGIC_ADMISSION = "kmd_logic/src/direct_scanout_admission.rs"
LOGIC_LIFETIME = "kmd_logic/src/direct_scanout_lifetime.rs"
LOGIC_COMMITTED = "kmd_logic/src/committed_mode.rs"
LOGIC_COMMITTED_LIFECYCLE = "kmd_logic/src/committed_mode_lifecycle.rs"
LOGIC_LIB = "kmd_logic/src/lib.rs"
PROTOCOL_WDDM = "protocol/src/wddm.rs"
KMD_CARGO = "kmd_render/Cargo.toml"
KMD_LOCK = "kmd_render/Cargo.lock"
BOUNDARY_PATH = "kmd_render/src/virtio/control_owner.rs"
SURFACE_PATH = "kmd_render/src/ddi/wddm_surface.rs"
BOUNDARY = "KMD_D2_OWNER_ENABLED"


TURBOFISH = r"(?:\s*::\s*<[^(){};]*>)?"
CALL_NAME = re.compile(
    rf"(?:(?:::|\.)\s*|\b)([A-Za-z_]\w*){TURBOFISH}\s*\("
)
QUALIFIED_CALL = re.compile(
    rf"\b((?:[A-Za-z_]\w*\s*::\s*)+[A-Za-z_]\w*){TURBOFISH}\s*\("
)
MACRO_CALL = re.compile(r"\b([A-Za-z_]\w*)\s*!\s*[({\[]")
CALL_KEYWORDS = frozenset(
    {"if", "for", "while", "match", "loop", "return", "let", "Some", "None", "Ok", "Err"}
)


def call_names(body: str) -> frozenset[str]:
    return frozenset(
        match.group(1)
        for match in CALL_NAME.finditer(body)
        if match.group(1) not in CALL_KEYWORDS
    )


def qualified_calls(body: str) -> frozenset[str]:
    return frozenset(re.sub(r"\s+", "", match.group(1)) for match in QUALIFIED_CALL.finditer(body))


def macro_calls(body: str) -> frozenset[str]:
    return frozenset(MACRO_CALL.findall(body))


# This is an effect allowlist, not a hash freeze. Every call or macro reachable
# in the device-DIRQL half must be named here, and stale permissions fail too.
# A helper therefore cannot hide an allocation/wait behind a new call without
# changing this reviewed boundary and its mutation tests.
DIRQL_CALL_MANIFEST: dict[tuple[str, str], frozenset[str]] = {
    (DISPLAY, "set_vidpn_source_address_d4"): frozenset(
        {
            "enqueue_d4_scanout_dirql",
            "enqueue_d4_scanout_dispatch",
            "d4_queue_status",
            "fetch_add",
            "is_aligned",
            "is_null",
            "mint",
            "from_exact_os_transition",
            "validate_direct_scanout_binding",
        }
    ),
    (DISPLAY, "mint"): frozenset({"KeGetCurrentIrql", "then_some"}),
    (DISPLAY, "authorizes"): frozenset({"KeGetCurrentIrql", "eq"}),
    (DISPLAY, "d4_queue_status"): frozenset(),
    (SUBMIT, "signal_dma_completed"): frozenset(
        {
            "as_mut",
            "completed_fence",
            "fence_is_forward",
            "fetch_add",
            "notify_at_dirql",
            "set_completed_fence",
            "zeroed",
        }
    ),
    (SUBMIT, "notify_at_dirql"): frozenset({"is_none", "store", "sync"}),
    (SUBMIT, "notify_at_dirql_routine"): frozenset(
        {"fetch_add", "is_null", "notify_interrupt", "queue_dpc"}
    ),
    (DIRECT, "validate_direct_scanout_binding"): frozenset(
        {
            "Present",
            "active_transport_instance",
            "direct_scanout_allocation_facts",
            "is_err",
            "mode",
            "new",
            "read",
            "record_refusal",
            "validate_binding_model",
        }
    ),
    (DIRECT, "record_refusal"): frozenset({"fetch_add", "store"}),
    (DIRECT, "active_transport_instance"): frozenset({"load"}),
    (DIRECT, "from_exact_os_transition"): frozenset(),
    (DIRECT, "resource_id"): frozenset({"token"}),
    (DIRECT, "width"): frozenset({"token"}),
    (DIRECT, "height"): frozenset({"token"}),
    (DIRECT, "format"): frozenset({"token"}),
    (DIRECT, "stride"): frozenset({"token"}),
    (DIRECT, "offset"): frozenset({"token"}),
    (DIRECT, "transport_instance"): frozenset(),
    (DIRECT, "matches_exact_allocation"): frozenset({"matches_allocation", "token"}),
    (DIRECT, "matches_allocation"): frozenset(),
    (ALLOC, "direct_scanout_allocation_facts"): frozenset({"load", "resolve_alloc"}),
    (ALLOC, "resolve_alloc"): frozenset({"is_null", "then_some"}),
    (ALLOC, "open_direct_scanout_allocation_facts"): frozenset(
        {"direct_scanout_allocation_facts", "open_allocation_context"}
    ),
    (ALLOC, "open_allocation_context"): frozenset(
        {"is_aligned", "is_null", "read_unaligned", "refuse_open_allocation_handle"}
    ),
    (ALLOC, "refuse_open_allocation_handle"): frozenset({"fetch_add"}),
    (COMMITTED, "read"): frozenset(
        {
            "Present",
            "fence",
            "fetch_add",
            "is_err",
            "load_lifecycle",
            "load_raw",
            "mark_poisoned",
            "read_state",
            "record_read_refusal",
            "record_read_stored_validation_error",
            "restore_snapshot",
            "validate_removed_terminal",
        }
    ),
    (COMMITTED, "load_lifecycle"): frozenset({"from_atomic", "load"}),
    (COMMITTED, "load_raw"): frozenset({"is_present", "load"}),
    (COMMITTED, "validate_removed_terminal"): frozenset(
        {
            "Lifecycle",
            "Logic",
            "StoredState",
            "fence",
            "load_lifecycle",
            "load_raw",
            "map_err",
            "record_removed_terminal_error",
            "restore_snapshot",
            "validate_removed_tombstone",
        }
    ),
    (COMMITTED, "mark_poisoned"): frozenset({"apply_atomic_fallback", "poison_atomic_fallback"}),
    (COMMITTED, "restore_snapshot"): frozenset(
        {"decode_reason", "from_stored", "map_err", "ok_or", "restore", "then_some", "validate_reason_shape"}
    ),
    (COMMITTED, "record_read_refusal"): frozenset({"fetch_add"}),
    (COMMITTED, "record_read_stored_validation_error"): frozenset(
        {"Logic", "bump_logic_refusal", "fetch_add"}
    ),
    (COMMITTED, "apply_atomic_fallback"): frozenset(
        {"clear_operand", "fetch_and", "fetch_or", "load_lifecycle", "set_operand"}
    ),
    (COMMITTED, "record_removed_terminal_error"): frozenset(
        {
            "Lifecycle",
            "StoredState",
            "bump_logic_refusal",
            "fetch_add",
            "removed_tombstone_refusal_index",
        }
    ),
    (COMMITTED, "bump_logic_refusal"): frozenset({"fetch_add"}),
    (COMMITTED, "removed_tombstone_refusal_index"): frozenset(),
    (COMMITTED, "decode_reason"): frozenset(),
    (COMMITTED, "validate_reason_shape"): frozenset(),
    (ADAPTER, "enqueue_d4_scanout_dirql"): frozenset(
        {"as_ref", "enqueue_direct_at_dirql", "fetch_add", "fetch_sub", "load", "new"}
    ),
    (ADAPTER_SCANOUT, "reserve_scanout_bind_seq"): frozenset(
        {"load", "next_bind_sequence", "store"}
    ),
    (ADAPTER_SCANOUT, "commit_scanout_bind_seq"): frozenset({"store"}),
    (GPU, "enqueue_direct_at_dirql"): frozenset(
        {"authorizes", "enqueue_direct_locked", "is_failed", "release", "try_access"}
    ),
    (GPU, "interrupt_queue_operation"): frozenset(
        {
            "Direct",
            "Holds",
            "Peek",
            "Popped",
            "add",
            "any",
            "as_mut",
            "as_mut_slice",
            "as_ref",
            "as_slice",
            "commit_scanout_bind_seq",
            "copy_from_slice",
            "enqueue_direct_locked",
            "is_null",
            "is_some_and",
            "iter",
            "matches_exact_allocation",
            "peek_used",
            "pop_used",
            "position",
            "reserve_scanout_bind_seq",
            "release",
            "size_of",
            "span",
            "take",
            "try_access",
        }
    ),
    (GPU, "enqueue_direct_locked"): frozenset(
        {
            "add",
            "as_mut_ptr",
            "as_mut_slice",
            "as_slice",
            "cast",
            "commit_scanout_bind_seq",
            "fill_set_scanout_blob",
            "find",
            "format",
            "get",
            "height",
            "is_none",
            "iter_mut",
            "mark_failed",
            "offset",
            "ok_or",
            "reserve_scanout_bind_seq",
            "reset",
            "resource_id",
            "size_of",
            "span",
            "store",
            "stride",
            "take",
            "transport_instance",
            "width",
        }
    ),
    (GPU, "try_access"): frozenset({"compare_exchange", "map", "ok"}),
    (GPU, "release"): frozenset({"release_access"}),
    (GPU, "release_access"): frozenset({"get", "notify", "should_notify", "store", "swap"}),
    (GPU, "is_failed"): frozenset({"load"}),
    (GPU, "mark_failed"): frozenset({"store"}),
    (HAL, "capacity"): frozenset(),
    (HAL, "reset"): frozenset({"capacity"}),
    (HAL, "span"): frozenset({"add", "as_ptr", "checked_add"}),
    (HAL, "share"): frozenset({"MmGetPhysicalAddress", "as_ptr"}),
    (CTRL, "fill_set_scanout_blob"): frozenset({"zeroed"}),
    (LOGIC_ADMISSION, "validate_direct_scanout_binding"): frozenset(
        {
            "MpoCheck",
            "MpoSet",
            "checked_add",
            "checked_mul",
            "from",
            "helios_hwa2_swizzle_is_direct_flip_capable",
            "map_err",
            "ok_or",
            "validate_create_output",
            "validate_mpo_plane",
            "validate_mpo_set",
        }
    ),
    (LOGIC_ADMISSION, "validate_mpo_set"): frozenset({"validate_mpo_plane"}),
    (LOGIC_ADMISSION, "validate_mpo_plane"): frozenset({"full_output"}),
    (LOGIC_LIFETIME, "Binding::new"): frozenset(),
    (LOGIC_LIFETIME, "Binding::token"): frozenset(),
    (LOGIC_COMMITTED, "ModePolicySnapshot::from_stored"): frozenset(
        {"validate_bound_identity"}
    ),
    (LOGIC_COMMITTED, "ModePolicySnapshot::effective_powered"): frozenset(),
    (LOGIC_COMMITTED, "CommittedModeState::restore"): frozenset(
        {"is_some", "validate_bound_identity", "validate_restored_mode"}
    ),
    (LOGIC_COMMITTED, "validate_bound_identity"): frozenset(),
    (LOGIC_COMMITTED, "validate_source_identity"): frozenset(),
    (LOGIC_COMMITTED, "validate_target_identity"): frozenset(),
    (LOGIC_COMMITTED, "validate_extents"): frozenset(),
    (LOGIC_COMMITTED, "validate_restored_mode"): frozenset(
        {
            "effective_powered",
            "validate_extents",
            "validate_source_identity",
            "validate_target_identity",
        }
    ),
    (LOGIC_COMMITTED_LIFECYCLE, "LifecycleWord::from_atomic"): frozenset({"Self"}),
    (LOGIC_COMMITTED_LIFECYCLE, "LifecycleWord::revision"): frozenset(),
    (LOGIC_COMMITTED_LIFECYCLE, "LifecycleWord::has_writer"): frozenset(),
    (LOGIC_COMMITTED_LIFECYCLE, "LifecycleWord::is_present"): frozenset(),
    (LOGIC_COMMITTED_LIFECYCLE, "LifecycleWord::is_reset_closed"): frozenset(),
    (LOGIC_COMMITTED_LIFECYCLE, "LifecycleWord::is_removed"): frozenset(),
    (LOGIC_COMMITTED_LIFECYCLE, "LifecycleWord::is_poisoned"): frozenset(),
    (LOGIC_COMMITTED_LIFECYCLE, "AtomicFallbackPlan::set_operand"): frozenset(),
    (LOGIC_COMMITTED_LIFECYCLE, "AtomicFallbackPlan::clear_operand"): frozenset(),
    (LOGIC_COMMITTED_LIFECYCLE, "poison_atomic_fallback"): frozenset(),
    (LOGIC_COMMITTED_LIFECYCLE, "read_state"): frozenset(
        {"has_writer", "is_poisoned", "is_present", "is_removed", "is_reset_closed"}
    ),
    (LOGIC_COMMITTED_LIFECYCLE, "validate_removed_tombstone"): frozenset(
        {
            "has_writer",
            "is_poisoned",
            "is_present",
            "is_removed",
            "is_reset_closed",
            "revision",
        }
    ),
    (LOGIC_LIB, "next_bind_sequence"): frozenset(),
    (PROTOCOL_WDDM, "HeliosWddmAllocationDescV2::validate_create_output"): frozenset(
        {"validate_stage"}
    ),
    (PROTOCOL_WDDM, "HeliosWddmAllocationDescV2::validate_stage"): frozenset(
        {
            "checked_add",
            "has_flag",
            "helios_hwa2_kind_is_standard",
            "helios_hwa2_swizzle_is_direct_flip_capable",
            "is_image",
            "len",
        }
    ),
    (PROTOCOL_WDDM, "HeliosWddmAllocationDescV2::has_flag"): frozenset(),
    (PROTOCOL_WDDM, "HeliosWddmAllocationDescV2::is_image"): frozenset(
        {"helios_hwa2_kind_is_image"}
    ),
    (PROTOCOL_WDDM, "helios_hwa2_kind_is_image"): frozenset(),
    (PROTOCOL_WDDM, "helios_hwa2_kind_is_standard"): frozenset(),
    (PROTOCOL_WDDM, "helios_hwa2_swizzle_is_direct_flip_capable"): frozenset(),
}

DIRQL_QUALIFIED_MANIFEST: dict[tuple[str, str], frozenset[str]] = {
    (DISPLAY, "set_vidpn_source_address_d4"): frozenset(
        {
            "SetVidPnDirql::mint",
            "crate::ddi::direct_scanout::QueuedDirectScanoutBinding::from_exact_os_transition",
            "crate::ddi::direct_scanout::validate_direct_scanout_binding",
        }
    ),
    (DIRECT, "validate_direct_scanout_binding"): frozenset(
        {"Binding::new", "CommittedModeRead::Present", "super::create_allocation::direct_scanout_allocation_facts"}
    ),
    (ALLOC, "open_allocation_context"): frozenset({"core::ptr::read_unaligned"}),
    (COMMITTED, "read"): frozenset({"CommittedModeRead::Present"}),
    (COMMITTED, "load_lifecycle"): frozenset({"LifecycleWord::from_atomic"}),
    (COMMITTED, "validate_removed_terminal"): frozenset(
        {"RemovedTerminalError::Lifecycle", "RemovedTerminalError::StoredState", "StoredValidationError::Logic"}
    ),
    (COMMITTED, "restore_snapshot"): frozenset(
        {"CommittedModeState::restore", "ModePolicySnapshot::from_stored"}
    ),
    (COMMITTED, "record_read_stored_validation_error"): frozenset({"StoredValidationError::Logic"}),
    (COMMITTED, "record_removed_terminal_error"): frozenset(
        {"RemovedTerminalError::Lifecycle", "RemovedTerminalError::StoredState"}
    ),
    (DISPLAY, "authorizes"): frozenset({"core::ptr::eq"}),
    (SUBMIT, "signal_dma_completed"): frozenset(
        {"core::mem::zeroed", "helios_kmd_logic::scanout_lease::fence_is_forward"}
    ),
    (ADAPTER, "enqueue_d4_scanout_dirql"): frozenset({"NonNull::new"}),
    (ADAPTER_SCANOUT, "reserve_scanout_bind_seq"): frozenset(
        {"helios_kmd_logic::scanout_retire::next_bind_sequence"}
    ),
    (GPU, "interrupt_queue_operation"): frozenset(
        {
            "QueueCallResult::Direct",
            "QueueCallResult::Holds",
            "QueueCallResult::Peek",
            "QueueCallResult::Popped",
            "core::mem::size_of",
        }
    ),
    (GPU, "enqueue_direct_locked"): frozenset(
        {"core::mem::size_of", "super::ctrl::fill_set_scanout_blob"}
    ),
    (HAL, "share"): frozenset(),
    (CTRL, "fill_set_scanout_blob"): frozenset({"VirtioGpuCtrlHdr::zeroed"}),
    (LOGIC_ADMISSION, "validate_direct_scanout_binding"): frozenset(
        {"PlaneFacts::MpoCheck", "PlaneFacts::MpoSet", "u64::from"}
    ),
    (LOGIC_ADMISSION, "validate_mpo_plane"): frozenset({"PlaneRect::full_output"}),
}

DIRQL_MACRO_MANIFEST: dict[tuple[str, str], frozenset[str]] = {
    (ALLOC, "open_allocation_context"): frozenset({"addr_of"}),
    (PROTOCOL_WDDM, "HeliosWddmAllocationDescV2::validate_stage"): frozenset({"matches"}),
    (PROTOCOL_WDDM, "helios_hwa2_kind_is_image"): frozenset({"matches"}),
    (PROTOCOL_WDDM, "helios_hwa2_kind_is_standard"): frozenset({"matches"}),
}

# The shared notifier is the reviewed implementation of these two kernel
# callbacks, so their field names are expected only inside that narrow helper.
# Every other DIRQL-audited body retains the blanket callback prohibition.
DIRQL_FORBIDDEN_ALLOWLIST: dict[tuple[str, str], frozenset[str]] = {
    (SUBMIT, "notify_at_dirql"): frozenset(
        {"DxgkCbQueueDpc", "DxgkCbSynchronizeExecution"}
    ),
    (SUBMIT, "notify_at_dirql_routine"): frozenset({"DxgkCbQueueDpc"}),
}


def check_sources(sources: dict[str, str]) -> list[str]:
    errors: list[str] = []
    live = {
        path: live_rust(src) if path.endswith(".rs") else src
        for path, src in sources.items()
    }
    ranges = {
        path: functions(text) if path.endswith(".rs") else []
        for path, text in live.items()
    }
    joined_live = "\n".join(live.values())

    def one(path: str, name: str) -> tuple[Function, str] | None:
        if "::" in name:
            type_name, method_name = name.split("::", 1)
            matches = methods(live.get(path, ""), ranges.get(path, []), type_name, method_name)
        else:
            matches = [item for item in ranges.get(path, []) if item.name == name]
        if len(matches) != 1:
            errors.append(f"{path}: expected one {name}, found {len(matches)}")
            return None
        item = matches[0]
        return item, live[path][item.brace : item.end]

    owner = re.sub(r"\s+", "", live.get(BOUNDARY_PATH, ""))
    expected_owner = (
        "pub(crate)constKMD_D2_OWNER_ENABLED:bool="
        "matches!(SURFACE,WddmSurface::Wddm3_2GpuMmu);"
    )
    if owner.count(expected_owner) != 1:
        errors.append(
            f"{BOUNDARY_PATH}: D4 authority must be the sole SURFACE-derived 3.2 predicate"
        )
    if not re.search(
        r"\bconst\s+SURFACE\s*:\s*WddmSurface\s*=\s*WddmSurface::Wddm3_2GpuMmu\s*;",
        live.get(SURFACE_PATH, ""),
    ):
        errors.append(f"{SURFACE_PATH}: active D4 requires the exact WDDM 3.2 surface")

    # D4's exact allocation association is one scoped WDDM-2 reference, not
    # the WDDM-1.x non-retaining query and not a reference retained across the
    # device-specific open. Target dxgkrnl rejects DxgkCbGetHandleData from a
    # WDDM-2+ driver. Microsoft's WDDM-2 compute sample acquires, copies the KMD
    # pointer, and releases inside OpenAllocation; CloseAllocation-before-
    # DestroyAllocation is the separate lifetime guarantee for the copied
    # pointer. Escaping the release token pins/re-enters dxgkrnl teardown.
    alloc_live = live.get(ALLOC, "")
    alloc_src = sources.get(ALLOC, "")
    if re.search(r"\bDxgkCbGetHandleData\b", alloc_live):
        errors.append(f"{ALLOC}: WDDM-1.x DxgkCbGetHandleData returned to active D4")
    if re.search(r"\bAcquiredAllocation\b", alloc_live):
        errors.append(f"{ALLOC}: allocation acquire reference escaped scoped OpenAllocation resolution")
    canonical = one(ALLOC, "canonical_open_allocation")
    if canonical is not None:
        item, body = canonical
        source_body = alloc_src[item.brace : item.end]
        acquire_cb_at = source_body.find("let acquire = dxgkrnl.DxgkCbAcquireHandleData?;")
        release_cb_at = source_body.find("let release = dxgkrnl.DxgkCbReleaseHandleData?;")
        acquire_at = source_body.find("acquire(&args, &mut release_handle)")
        validate_at = source_body.find(
            "let valid = unsafe { resolve_alloc(allocation) }.is_some();"
        )
        release_args_at = source_body.find("let release_args = DXGKARGCB_RELEASEHANDLEDATA {")
        release_type_at = source_body.find(
            "Type: _DXGK_HANDLE_TYPE::DXGK_HANDLE_ALLOCATION,"
        )
        release_at = source_body.find("unsafe { release(release_args) };")
        publish_at = source_body.find("valid.then_some(allocation as usize)")
        if not (
            0
            <= acquire_cb_at
            < release_cb_at
            < acquire_at
            < validate_at
            < release_args_at
            < release_type_at
            < release_at
            < publish_at
        ):
            errors.append(
                f"{ALLOC}: canonical open must acquire, validate, release, then publish one scoped WDDM-2 association"
            )
    open_context = re.search(r"struct\s+OpenAllocationContext\s*\{([^}]*)\}", alloc_live, re.S)
    if open_context is None or not re.search(
        r"\ballocation\s*:\s*usize\s*,", open_context.group(1)
    ):
        errors.append(f"{ALLOC}: device-specific open lost its exact copied allocation pointer")
    elif re.search(r"release_handle|DXGKARG_RELEASE_HANDLE", open_context.group(1)):
        errors.append(f"{ALLOC}: allocation acquire token escaped into the device-specific open")
    close = one(ALLOC, "dxgkddi_close_allocation")
    if close is not None and "let _ = unsafe { take_open_ctx(handle) };" not in alloc_src[close[0].brace : close[0].end]:
        errors.append(f"{ALLOC}: CloseAllocation no longer drops the device-specific open object")

    for name, value in (
        ("CLASSIC_MODE_CHANGE", "0x0000_0001"),
        ("CLASSIC_FLIP_IMMEDIATE", "0x0000_0002"),
        ("CLASSIC_FLIP_ON_NEXT_VSYNC", "0x0000_0004"),
        ("CLASSIC_STEREO_MASK", "0x0000_0038"),
        ("CLASSIC_SHARED_PRIMARY_TRANSITION", "0x0000_0040"),
        ("CLASSIC_INDEPENDENT_FLIP_EXCLUSIVE", "0x0000_0080"),
    ):
        if not re.search(rf"\bconst\s+{name}\s*:\s*u32\s*=\s*{value}\s*;", live.get(DISPLAY, "")):
            errors.append(f"{DISPLAY}: exact WDK 28000 SetVidPn flag {name} drifted")
    if "const _: () = assert!(CLASSIC_SUPPORTED_FLAG_MASK == 0x0000_00ff);" not in sources.get(DISPLAY, ""):
        errors.append(f"{DISPLAY}: classic SetVidPn operation mask lost its exact low-byte assertion")

    cargo = sources.get(KMD_CARGO, "")
    if cargo.count('virtio-drivers = { version = "=0.13.0", default-features = false }') != 1:
        errors.append(
            f"{KMD_CARGO}: DIRQL-audited virtio-drivers must remain exactly 0.13.0 with alloc disabled"
        )
    lock = sources.get(KMD_LOCK, "")
    locked_virtio = re.findall(
        r'\[\[package\]\]\s*name = "virtio-drivers"\s*version = "([^"]+)"\s*'
        r'source = "registry\+https://github\.com/rust-lang/crates\.io-index"\s*'
        r'checksum = "([^"]+)"',
        lock,
        re.S,
    )
    if locked_virtio != [
        (
            "0.13.0",
            "cfdc1c628cdd8ce7c3b9e65a8ed550d0338e9ef9f911e729666f1cce097de2f7",
        )
    ]:
        errors.append(f"{KMD_LOCK}: DIRQL-audited virtio-drivers source drifted: {locked_virtio!r}")

    display = live.get(DISPLAY, "")
    token = re.search(r"pub\s*\(\s*crate\s*\)\s+struct\s+SetVidPnDirql\s*<", display)
    if token is None:
        errors.append(f"{DISPLAY}: missing private-scope SetVidPnDirql capability")
    elif re.search(r"#\s*\[\s*derive\s*\([^]]*\b(?:Clone|Copy)\b", display[max(0, token.start() - 160) : token.start()]):
        errors.append(f"{DISPLAY}: SetVidPnDirql must remain non-Clone and non-Copy")
    if "_not_send: PhantomData<*mut ()>" not in sources.get(DISPLAY, ""):
        errors.append(f"{DISPLAY}: SetVidPnDirql lost its !Send/!Sync marker")

    mint_calls = []
    for path, text in live.items():
        for match in re.finditer(r"\bSetVidPnDirql\s*::\s*mint\s*\(", text):
            mint_calls.append((path, enclosing(ranges[path], match.start())))
    if len(mint_calls) != 1 or mint_calls[0][0] != DISPLAY or mint_calls[0][1] is None or mint_calls[0][1].name != "set_vidpn_source_address_d4":
        errors.append(f"SetVidPnDirql must be minted exactly once in set_vidpn_source_address_d4, found {mint_calls!r}")
    minted = one(DISPLAY, "mint")
    if minted is not None:
        item, body = minted
        signature = live[DISPLAY][item.start : item.brace]
        if re.search(r"\bpub\b", signature):
            errors.append(f"{DISPLAY}: SetVidPnDirql::mint must remain module-private")
        if not re.search(
            r"KeGetCurrentIrql\s*\(\s*\)\s*(?:\}\s*)?>\s*Self\s*::\s*DISPATCH_LEVEL_IRQL",
            body,
        ):
            errors.append(f"{DISPLAY}: SetVidPnDirql::mint lost the above-DISPATCH runtime proof")

    passive_reset = one(GPU, "reset_status_and_poll")
    if passive_reset is not None:
        item, body = passive_reset
        signature = live[GPU][item.start : item.brace]
        if "crate::irql::PassiveLevel" not in signature:
            errors.append(f"{GPU}: PCI transport status access lost its PassiveLevel proof")
        if "transport.set_status" not in body or "transport.get_status" not in body:
            errors.append(f"{GPU}: typed PASSIVE transport reset/poll body drifted")
        if re.search(r"\bsynchronize\s*\(", body):
            errors.append(f"{GPU}: PCI transport status access was raised into interrupt synchronization")

    adapter_reset = one(ADAPTER, "reset_virtio_physical")
    if adapter_reset is not None:
        item, _ = adapter_reset
        signature = live[ADAPTER][item.start : item.brace]
        body = sources[ADAPTER][item.brace : item.end]
        if "crate::irql::PassiveLevel" not in signature:
            errors.append(f"{ADAPTER}: physical reset lost its caller PASSIVE proof")
        increment_at = body.find("d4_dirql_readers.fetch_add(1, Ordering::SeqCst)")
        recheck_at = body.find("d4_dirql_queue.load(Ordering::SeqCst) != raw")
        reset_at = body.find("reset_status_and_poll(passive, expected_instance)")
        decrement_at = body.rfind("d4_dirql_readers.fetch_sub(1, Ordering::SeqCst)")
        lock_at = body.find("with_virtio")
        if not (0 <= increment_at < recheck_at < reset_at < decrement_at < lock_at):
            errors.append(
                f"{ADAPTER}: PASSIVE PCI reset must be lifetime-pinned and complete before virtio lock"
            )

    init = one(GPU, "VirtioGpu::init")
    if init is not None:
        body = init[1]
        synchronize_at = body.find(
            "let Some(synchronize) = dxgkrnl.DxgkCbSynchronizeExecution"
        )
        pci_at = body.find("PciTransport::new")
        if synchronize_at < 0 or pci_at < 0 or synchronize_at >= pci_at:
            errors.append(
                f"{GPU}: interrupt synchronization callback must be proven before PCI/DRIVER_OK"
            )

    fail_closed = re.compile(
        rf"\bif\s*!\s*crate::virtio::{BOUNDARY}\s*\{{[^{{}}]*\breturn\b[^{{}}]*;\s*\}}",
        re.S,
    )
    d4_functions = (
        (DISPLAY, "set_vidpn_source_address_d4"),
        (DISPLAY, "arm_dma_flip_d4"),
        (ADAPTER, "enqueue_d4_scanout_dirql"),
        (ADAPTER, "enqueue_d4_scanout_dispatch"),
    )
    for path, name in d4_functions:
        found = one(path, name)
        if found is not None and fail_closed.search(found[1]) is None:
            errors.append(f"{path}: {name} lacks a local fail-closed D4 boundary")

    # A check elsewhere in a wrapper is not an entry guard. The active branch
    # must be the first authority arm selected by the actual production DDI.
    for wrapper, target in (
        ("dxgkddi_set_vidpn_source_address", "set_vidpn_source_address_d4"),
        ("arm_dma_flip_programming", "arm_dma_flip_d4"),
    ):
        found = one(DISPLAY, wrapper)
        if found is None:
            continue
        if not re.search(
            rf"\bif\s+crate::virtio::{BOUNDARY}\s*\{{\s*return\s+(?:unsafe\s*\{{\s*)?{target}\s*\(",
            found[1],
            re.S,
        ):
            errors.append(f"{DISPLAY}: {wrapper} lost its dominating activation-coherence guard")

    d4_legacy_forbidden = re.compile(
        r"\b(?:production_linear_scanout|fast_bind_from_flip|pending_vidpn_allocation|"
        r"SCANOUT_RETRY_BUDGET|note_retry_attempt|set_vidpn_primary_address|"
        r"set_vidpn_source_address_dirql|process_deferred_vidpn_source_address|"
        r"apply_deferred_vidpn_source_address_locked|apply_vidpn_source_address|"
        r"program_vidpn_source|SnapshotDescriptor|from_snapshot_descriptor|"
        r"RowPitch\s*::\s*linear|mode_w|mode_h)\b"
    )
    for path, name in (
        (DISPLAY, "set_vidpn_source_address_d4"),
        (DISPLAY, "arm_dma_flip_d4"),
        (ADAPTER, "enqueue_d4_scanout_dirql"),
        (ADAPTER, "enqueue_d4_scanout_dispatch"),
        (GPU, "enqueue_direct_at_dirql"),
        (GPU, "enqueue_direct_at_dispatch"),
        (GPU, "enqueue_direct_locked"),
    ):
        found = one(path, name)
        if found is not None:
            legacy = d4_legacy_forbidden.search(found[1])
            if legacy:
                errors.append(f"{path}: {name} re-enters retired D4 mechanism {legacy.group(0)!r}")

    authority_calls = {
        "enqueue_d4_scanout_dirql": {"set_vidpn_source_address_d4"},
        "enqueue_d4_scanout_dispatch": {"set_vidpn_source_address_d4", "arm_dma_flip_d4"},
        "enqueue_direct_at_dirql": {"enqueue_d4_scanout_dirql"},
        "enqueue_direct_scanout_dispatch": {"enqueue_d4_scanout_dispatch"},
    }
    for called, allowed_owners in authority_calls.items():
        pattern = re.compile(rf"\.\s*{called}\s*\(")
        for path, text in live.items():
            for match in pattern.finditer(text):
                owner = enclosing(ranges[path], match.start())
                if owner is None or owner.name not in allowed_owners:
                    errors.append(
                        f"{path}: {called} authority called from unexpected "
                        f"{owner.name if owner else '<outside function>'}"
                    )
                    continue
                body = text[owner.brace : owner.end]
                if owner.name not in {"enqueue_direct_at_dirql", "enqueue_direct_scanout_dispatch"} and fail_closed.search(body) is None:
                    errors.append(f"{path}: {owner.name} reaches {called} without a real local boundary")

    common_forbidden = re.compile(
        r"\b(?:Box\s*::\s*new|Vec\s*::|DmaBuffer\s*::\s*new|try_reserve\w*|"
        r"KeWait\w*|KeDelay\w*|KeAcquire\w*|KeSetEvent|KeQuery\w*|"
        r"ObDereference\w*|MmAllocate\w*|ExAllocate\w*|RtlWrite\w*|Etw\w*|"
        r"PassiveLevel\s*::|(?:diag|diag_etw|scanout_trace|scanout_timeline)\s*::|"
        r"with_virtio|with_scanout_lifecycle|with_venus_client|"
        r"signal_hpd|KeSetEvent|DxgkCbQueueDpc|DxgkCbSynchronizeExecution|"
        r"spin_loop|ctrl\s*::)\b|\.\s*(?:get_status|set_status|lock)\s*\("
    )
    set_d4 = one(DISPLAY, "set_vidpn_source_address_d4")
    if set_d4 is not None:
        _, body = set_d4
        split = re.search(r"\bif\s+let\s+Some\s*\(\s*proof\s*\)\s*=\s*SetVidPnDirql\s*::\s*mint", body)
        if split is None:
            errors.append(f"{DISPLAY}: D4 classic path lost its explicit DIRQL/DISPATCH split")
        else:
            common = body[: split.start()]
            match = common_forbidden.search(common)
            if match:
                errors.append(f"{DISPLAY}: DIRQL common path reaches forbidden {match.group(0)!r}")
        if "enqueue_d4_scanout_dirql(&proof, work)" not in sources[DISPLAY][set_d4[0].brace : set_d4[0].end]:
            errors.append(f"{DISPLAY}: proof arm no longer moves exact work into the narrow DIRQL gateway")
        if not re.search(
            r"let\s+queued\s*=\s*if\s+let\s+Some\s*\(\s*proof\s*\)\s*=\s*"
            r"SetVidPnDirql\s*::\s*mint\s*\(\s*adapter\s*\)\s*\{\s*"
            r"unsafe\s*\{\s*adapter\s*\.\s*enqueue_d4_scanout_dirql\s*"
            r"\(\s*&proof\s*,\s*work\s*\)\s*\}\s*\}\s*else\s*\{\s*"
            r"adapter\s*\.\s*enqueue_d4_scanout_dispatch\s*\(\s*work\s*\)\s*\}\s*;",
            body,
            re.S,
        ):
            errors.append(
                f"{DISPLAY}: D4 classic splitter no longer confines dispatch authority "
                "to the lower-IRQL else arm"
            )

    audited_bodies = (
        (DISPLAY, "set_vidpn_source_address_d4"),
        (DISPLAY, "mint"),
        (DISPLAY, "d4_queue_status"),
        (SUBMIT, "signal_dma_completed"),
        (SUBMIT, "notify_at_dirql"),
        (SUBMIT, "notify_at_dirql_routine"),
        (DIRECT, "validate_direct_scanout_binding"),
        (DIRECT, "record_refusal"),
        (DIRECT, "active_transport_instance"),
        (DIRECT, "from_exact_os_transition"),
        (DIRECT, "resource_id"),
        (DIRECT, "width"),
        (DIRECT, "height"),
        (DIRECT, "format"),
        (DIRECT, "stride"),
        (DIRECT, "offset"),
        (DIRECT, "transport_instance"),
        (DIRECT, "matches_exact_allocation"),
        (DIRECT, "matches_allocation"),
        (ALLOC, "direct_scanout_allocation_facts"),
        (ALLOC, "resolve_alloc"),
        (ALLOC, "open_direct_scanout_allocation_facts"),
        (ALLOC, "open_allocation_context"),
        (ALLOC, "refuse_open_allocation_handle"),
        (COMMITTED, "read"),
        (COMMITTED, "load_lifecycle"),
        (COMMITTED, "load_raw"),
        (COMMITTED, "validate_removed_terminal"),
        (COMMITTED, "mark_poisoned"),
        (COMMITTED, "restore_snapshot"),
        (COMMITTED, "record_read_refusal"),
        (COMMITTED, "record_read_stored_validation_error"),
        (COMMITTED, "apply_atomic_fallback"),
        (COMMITTED, "record_removed_terminal_error"),
        (COMMITTED, "bump_logic_refusal"),
        (COMMITTED, "removed_tombstone_refusal_index"),
        (COMMITTED, "decode_reason"),
        (COMMITTED, "validate_reason_shape"),
        (DISPLAY, "authorizes"),
        (ADAPTER, "enqueue_d4_scanout_dirql"),
        (ADAPTER_SCANOUT, "reserve_scanout_bind_seq"),
        (ADAPTER_SCANOUT, "commit_scanout_bind_seq"),
        (GPU, "interrupt_queue_operation"),
        (GPU, "enqueue_direct_at_dirql"),
        (GPU, "enqueue_direct_locked"),
        (GPU, "try_access"),
        (GPU, "release"),
        (GPU, "release_access"),
        (GPU, "is_failed"),
        (GPU, "mark_failed"),
        (HAL, "capacity"),
        (HAL, "reset"),
        (HAL, "span"),
        (HAL, "share"),
        (CTRL, "fill_set_scanout_blob"),
        (LOGIC_ADMISSION, "validate_direct_scanout_binding"),
        (LOGIC_ADMISSION, "validate_mpo_set"),
        (LOGIC_ADMISSION, "validate_mpo_plane"),
        (LOGIC_LIFETIME, "Binding::new"),
        (LOGIC_LIFETIME, "Binding::token"),
        (LOGIC_COMMITTED, "ModePolicySnapshot::from_stored"),
        (LOGIC_COMMITTED, "ModePolicySnapshot::effective_powered"),
        (LOGIC_COMMITTED, "CommittedModeState::restore"),
        (LOGIC_COMMITTED, "validate_bound_identity"),
        (LOGIC_COMMITTED, "validate_source_identity"),
        (LOGIC_COMMITTED, "validate_target_identity"),
        (LOGIC_COMMITTED, "validate_extents"),
        (LOGIC_COMMITTED, "validate_restored_mode"),
        (LOGIC_COMMITTED_LIFECYCLE, "LifecycleWord::from_atomic"),
        (LOGIC_COMMITTED_LIFECYCLE, "LifecycleWord::revision"),
        (LOGIC_COMMITTED_LIFECYCLE, "LifecycleWord::has_writer"),
        (LOGIC_COMMITTED_LIFECYCLE, "LifecycleWord::is_present"),
        (LOGIC_COMMITTED_LIFECYCLE, "LifecycleWord::is_reset_closed"),
        (LOGIC_COMMITTED_LIFECYCLE, "LifecycleWord::is_removed"),
        (LOGIC_COMMITTED_LIFECYCLE, "LifecycleWord::is_poisoned"),
        (LOGIC_COMMITTED_LIFECYCLE, "AtomicFallbackPlan::set_operand"),
        (LOGIC_COMMITTED_LIFECYCLE, "AtomicFallbackPlan::clear_operand"),
        (LOGIC_COMMITTED_LIFECYCLE, "poison_atomic_fallback"),
        (LOGIC_COMMITTED_LIFECYCLE, "read_state"),
        (LOGIC_COMMITTED_LIFECYCLE, "validate_removed_tombstone"),
        (LOGIC_LIB, "next_bind_sequence"),
        (PROTOCOL_WDDM, "HeliosWddmAllocationDescV2::validate_create_output"),
        (PROTOCOL_WDDM, "HeliosWddmAllocationDescV2::validate_stage"),
        (PROTOCOL_WDDM, "HeliosWddmAllocationDescV2::has_flag"),
        (PROTOCOL_WDDM, "HeliosWddmAllocationDescV2::is_image"),
        (PROTOCOL_WDDM, "helios_hwa2_kind_is_image"),
        (PROTOCOL_WDDM, "helios_hwa2_kind_is_standard"),
        (PROTOCOL_WDDM, "helios_hwa2_swizzle_is_direct_flip_capable"),
    )
    if set(audited_bodies) != set(DIRQL_CALL_MANIFEST):
        errors.append("internal D4 gate error: audited body set and call manifest differ")
    for path, name in audited_bodies:
        found = one(path, name)
        if found is None:
            continue
        body = found[1]
        audited = body.replace("super::ctrl::fill_set_scanout_blob", "fill_set_scanout_blob")
        key = (path, name)
        allowed_forbidden = DIRQL_FORBIDDEN_ALLOWLIST.get(key, frozenset())
        for match in common_forbidden.finditer(audited):
            if match.group(0) not in allowed_forbidden:
                errors.append(f"{path}: {name} reaches DIRQL-forbidden {match.group(0)!r}")
        found_calls = call_names(body)
        expected_calls = DIRQL_CALL_MANIFEST[key]
        if found_calls != expected_calls:
            errors.append(
                f"{path}: {name} static DIRQL call surface changed; "
                f"added={sorted(found_calls - expected_calls)!r}, "
                f"removed={sorted(expected_calls - found_calls)!r}"
            )
        found_qualified = qualified_calls(body)
        expected_qualified = DIRQL_QUALIFIED_MANIFEST.get(key, frozenset())
        if found_qualified != expected_qualified:
            errors.append(
                f"{path}: {name} static DIRQL qualified-call surface changed; "
                f"added={sorted(found_qualified - expected_qualified)!r}, "
                f"removed={sorted(expected_qualified - found_qualified)!r}"
            )
        found_macros = macro_calls(body)
        expected_macros = DIRQL_MACRO_MANIFEST.get(key, frozenset())
        if found_macros != expected_macros:
            errors.append(
                f"{path}: {name} static DIRQL macro surface changed; "
                f"added={sorted(found_macros - expected_macros)!r}, "
                f"removed={sorted(expected_macros - found_macros)!r}"
            )
        indirect_calls = len(re.findall(r"\)\s*\(", body))
        expected_indirect_calls = 1 if key == (GPU, "interrupt_queue_operation") else 0
        if indirect_calls != expected_indirect_calls:
            errors.append(
                f"{path}: {name} static DIRQL indirect-call surface changed; "
                f"found={indirect_calls}, expected={expected_indirect_calls}"
            )

    # Both slice views reached by the fixed queue are raw bounds-preserving
    # projections. They share method names, so audit every implementation body
    # instead of silently selecting one overload.
    for name in ("as_slice", "as_mut_slice"):
        matches = [item for item in ranges.get(HAL, []) if item.name == name]
        if len(matches) != 2:
            errors.append(f"{HAL}: expected two audited {name} projections, found {len(matches)}")
            continue
        for item in matches:
            body = live[HAL][item.brace : item.end]
            if common_forbidden.search(body):
                errors.append(f"{HAL}: {name} projection gained a DIRQL-forbidden operation")
            calls = call_names(body)
            allowed = frozenset({"as_ptr", "from_raw_parts", "from_raw_parts_mut"})
            if not calls <= allowed:
                errors.append(f"{HAL}: {name} projection call surface changed: {sorted(calls)!r}")

    # The DMA-flip arm begins at DISPATCH_LEVEL. It may enter the existing
    # transport spinlock/synchronization callback, but it still may not allocate,
    # wait, mint a PASSIVE capability, signal a worker, or perform diagnostics.
    dispatch_forbidden = re.compile(
        r"\b(?:Box\s*::\s*new|Vec\s*::|DmaBuffer\s*::\s*new|try_reserve\w*|"
        r"KeWait\w*|KeDelay\w*|KeSetEvent|ObDereference\w*|MmAllocate\w*|"
        r"ExAllocate\w*|RtlWrite\w*|Etw\w*|PassiveLevel\s*::|"
        r"(?:diag|diag_etw|scanout_trace|scanout_timeline)\s*::|"
        r"with_scanout_lifecycle|with_venus_client|signal_hpd|DxgkCbQueueDpc|"
        r"spin_loop|ctrl\s*::)\b|\.\s*lock\s*\("
    )
    for path, name in (
        (DISPLAY, "arm_dma_flip_d4"),
        (ADAPTER, "enqueue_d4_scanout_dispatch"),
        (GPU, "enqueue_direct_at_dispatch"),
    ):
        found = one(path, name)
        if found is not None:
            match = dispatch_forbidden.search(found[1])
            if match:
                errors.append(f"{path}: {name} reaches DISPATCH-forbidden {match.group(0)!r}")

    queue_dirql = one(GPU, "enqueue_direct_at_dirql")
    if queue_dirql is not None:
        signature = live[GPU][queue_dirql[0].start : queue_dirql[0].brace]
        source_body = sources[GPU][queue_dirql[0].brace : queue_dirql[0].end]
        if "SetVidPnDirql" not in signature or "QueuedDirectScanoutBinding" not in signature:
            errors.append(f"{GPU}: DIRQL queue entry lost capability or move-only candidate")
        if re.search(r"\bsynchronize\s*\(", queue_dirql[1]):
            errors.append(f"{GPU}: DIRQL queue entry recursively synchronizes")
        access_at = source_body.find("let Some(access) = self.try_access()")
        mutation_at = source_body.find("self.enqueue_direct_locked(adapter, work)")
        release_at = source_body.find("access.release()")
        if access_at < 0:
            errors.append(f"{GPU}: DIRQL queue entry bypasses the nonblocking exclusion gate")
        elif mutation_at < 0 or release_at < 0 or not (access_at < mutation_at < release_at):
            errors.append(f"{GPU}: DIRQL queue entry must acquire-mutate-release exactly once")

    callback = one(GPU, "interrupt_queue_operation")
    if callback is not None:
        source_body = sources[GPU][callback[0].brace : callback[0].end]
        access_at = source_body.find("let Some(access) = queue.try_access()")
        operation_at = source_body.find("match operation")
        release_at = source_body.find("access.release()")
        if access_at < 0:
            errors.append(f"{GPU}: synchronized queue callback bypasses the common exclusion gate")
        elif operation_at < 0 or release_at < 0 or not (access_at < operation_at < release_at):
            errors.append(f"{GPU}: synchronized queue callback must acquire-dispatch-release exactly once")

    locked = one(GPU, "enqueue_direct_locked")
    if locked is not None:
        body = locked[1]
        required = (
            "direct_slots",
            ".find(",
            "slot.work = Some(work)",
            ".control",
            ".add(",
            "notify_pending.store(1, Ordering::Release)",
        )
        for spelling in required:
            if spelling not in sources[GPU][locked[0].brace : locked[0].end]:
                errors.append(f"{GPU}: fixed DIRQL enqueue lost required step {spelling!r}")
        if re.search(
            r"\.\s*push\s*\(|\.\s*insert\s*\(|\.\s*(?:try_)?reserve(?:_exact)?\s*\(",
            body,
        ):
            errors.append(f"{GPU}: DIRQL enqueue may not grow storage")

    gpu_src = sources.get(GPU, "")
    if not re.search(
        rf"if\s+crate::virtio::{BOUNDARY}\s*&&\s*bind_cmd_pool\.len\(\)\s*!=\s*BIND_CMD_POOL\s*\{{\s*return\s+Err\(VirtioError::OutOfMemory\)",
        live.get(GPU, ""),
        re.S,
    ):
        errors.append(f"{GPU}: owner-enabled init no longer requires every fixed DIRQL slot")
    if "direct_slots: Box<[DirectQueueSlot]>" not in gpu_src:
        errors.append(f"{GPU}: fixed DIRQL slots are no longer boxed preallocated storage")
    if "queue: Box<InterruptQueue>" not in gpu_src or "Box::new(InterruptQueue::new(" not in gpu_src:
        errors.append(f"{GPU}: published DIRQL queue must remain separately heap-owned")
    for required in ("access: AtomicU32", "notify_pending: AtomicU32"):
        if required not in gpu_src:
            errors.append(f"{GPU}: queue lost static DIRQL exclusion field {required!r}")
    notify = one(GPU, "notify")
    if notify is not None:
        notify_body = sources[GPU][notify[0].brace : notify[0].end]
        if not re.search(
            r"QueueCallResult::QueueFull\s*=>\s*Err\s*\(\s*VirtioError::QueueFull\s*\)",
            notify_body,
        ):
            errors.append(f"{GPU}: contended notification must fail closed")

    # These values can be dropped on a refused add at device DIRQL. Keep them
    # scalar/move-only and forbid a future cleanup callback from entering Drop.
    for type_name in (
        "DisplayBacking",
        "ValidatedDirectScanoutBinding",
        "QueuedDirectScanoutBinding",
        "InterruptQueueAccess",
    ):
        if re.search(rf"impl\s+(?:<[^>]*>\s+)?Drop\s+for\s+{type_name}\b", joined_live):
            errors.append(f"{DIRECT}: {type_name} may not run cleanup from DIRQL Drop")
    queued_struct = re.search(
        r"struct\s+QueuedDirectScanoutBinding\s*\{([^}]*)\}",
        live.get(DIRECT, ""),
        re.S,
    )
    if queued_struct is None or not all(
        re.search(rf"\b{name}\s*:\s*{kind}\b", queued_struct.group(1))
        for name, kind in (
            ("candidate", "ValidatedDirectScanoutBinding"),
            ("source_id", "u32"),
            ("primary_segment", "u32"),
            ("primary_address", "u64"),
            ("operation_flags", "u32"),
        )
    ):
        errors.append(f"{DIRECT}: fixed-queue work lost its scalar move-only shape")

    allowed_queue_mutators = {
        found[0]
        for found in (
            callback,
            locked,
            one(GPU, "release_access"),
            init,
        )
        if found is not None
    }
    for match in re.finditer(
        r"(?:\.|\b)(?:control|transport)\s*\.\s*(?:add|pop_used|peek_used|should_notify|notify)\s*\(",
        live.get(GPU, ""),
    ):
        owner = enclosing(ranges.get(GPU, []), match.start())
        if owner is None or owner not in allowed_queue_mutators:
            errors.append(f"{GPU}: queue mutation escapes interrupt synchronization in {owner}")

    allowed_transport_status = {
        found[0] for found in (init, passive_reset) if found is not None
    }
    for match in re.finditer(
        r"(?:\.|\b)transport\s*\.\s*(?:get_status|set_status)\s*\(",
        live.get(GPU, ""),
    ):
        owner = enclosing(ranges.get(GPU, []), match.start())
        if owner is None or owner not in allowed_transport_status:
            errors.append(f"{GPU}: PCI transport status access escaped typed PASSIVE/init sites")

    adapter_src = sources.get(ADAPTER, "")
    hazard_required = (
        "d4_dirql_readers: AtomicU32",
        "d4_dirql_transport_retained: AtomicU32",
        "d4_dirql_queue.load(Ordering::SeqCst)",
        "d4_dirql_readers.fetch_add(1, Ordering::SeqCst)",
        "d4_dirql_readers.fetch_sub(1, Ordering::SeqCst)",
        "d4_dirql_queue.store(0, Ordering::SeqCst)",
        "d4_dirql_readers.load(Ordering::SeqCst) == 0",
    )
    for required in hazard_required:
        if required not in adapter_src:
            errors.append(f"{ADAPTER}: D4 queue lifetime hazard lost {required!r}")
    enqueue_hazard = one(ADAPTER, "enqueue_d4_scanout_dirql")
    if enqueue_hazard is not None:
        hazard_body = sources[ADAPTER][enqueue_hazard[0].brace : enqueue_hazard[0].end]
        add_at = hazard_body.find("d4_dirql_readers.fetch_add(1, Ordering::SeqCst)")
        recheck_at = hazard_body.find("d4_dirql_queue.load(Ordering::SeqCst) != raw")
        deref_at = hazard_body.find("queue.as_ref().enqueue_direct_at_dirql")
        subtract_at = hazard_body.rfind("d4_dirql_readers.fetch_sub(1, Ordering::SeqCst)")
        if not (0 <= add_at < recheck_at < deref_at < subtract_at):
            errors.append(f"{ADAPTER}: D4 queue pointer hazard order is not increment-recheck-use-decrement")
    install_hazard = one(ADAPTER, "install_virtio")
    if install_hazard is not None:
        install_body = sources[ADAPTER][install_hazard[0].brace : install_hazard[0].end]
        own_at = install_body.find("*slot = Some(new)")
        publish_at = install_body.find("d4_dirql_queue.store(d4_queue, Ordering::SeqCst)")
        unlock_at = install_body.find(
            "KeReleaseSpinLock(self.virtio_lock.get(), irql)",
            publish_at,
        )
        if not (0 <= own_at < publish_at < unlock_at):
            errors.append(
                f"{ADAPTER}: D4 queue publication must occur after Box ownership and before virtio unlock"
            )
    start = one(DIRECT, "start")
    if start is not None:
        start_body = sources[DIRECT][start[0].brace : start[0].end]
        close_at = start_body.find("active_transport_instance\n            .store(0, Ordering::Release)")
        expected_at = start_body.find("let expected_instance")
        open_at = start_body.find("active_transport_instance\n            .store(expected_instance, Ordering::Release)")
        if not (0 <= close_at < expected_at < open_at):
            errors.append(
                f"{DIRECT}: D4 admission must close before fallible start work and open only after plane construction"
            )
    remove_hazard = one(ADAPTER, "remove_virtio_and_reset_scanout_bind_generation")
    if remove_hazard is not None:
        hazard_body = sources[ADAPTER][remove_hazard[0].brace : remove_hazard[0].end]
        acquire_at = hazard_body.find("KeAcquireSpinLockRaiseToDpc(self.virtio_lock.get())")
        withdraw_at = hazard_body.find(
            "d4_dirql_queue.store(0, Ordering::SeqCst)",
            acquire_at,
        )
        readers_at = hazard_body.find("d4_dirql_readers.load(Ordering::SeqCst)")
        take_at = hazard_body.find("core::mem::replace(unsafe { &mut *self.virtio.get() }, None)")
        if not (0 <= acquire_at < withdraw_at < readers_at < take_at):
            errors.append(
                f"{ADAPTER}: D4 withdrawal must repeat under owner lock before reader decision and Box removal"
            )
        retain_at = hazard_body.find("d4_dirql_transport_retained.store(1, Ordering::Release)")
        unlock_at = hazard_body.find("KeReleaseSpinLock(self.virtio_lock.get(), irql)")
        if retain_at < 0 or unlock_at < 0 or retain_at >= unlock_at:
            errors.append(f"{ADAPTER}: retained transport latch must publish before virtio unlock")
    holds = one(ADAPTER, "d4_queue_holds_allocation")
    if holds is not None:
        holds_body = sources[ADAPTER][holds[0].brace : holds[0].end]
        if holds_body.count("d4_dirql_transport_retained.load(Ordering::Acquire) != 0") < 2:
            errors.append(f"{ADAPTER}: allocation retirement can miss a retained DIRQL transport")

    for retired in ("SCANOUT_ALLOCS", "scanout_allocation_for_resource"):
        if re.search(rf"\b{retired}\b", joined_live):
            errors.append(f"retired heuristic allocation identity {retired} is live")

    packet = live.get(PACKET, "")
    flip_struct = re.search(r"struct\s+PresentFlipPrivate\s*\{([^}]*)\}", packet, re.S)
    if flip_struct is None:
        errors.append(f"{PACKET}: missing exact DMA flip record")
    else:
        fields = flip_struct.group(1)
        for required in ("allocation", "physical_address", "primary_segment", "operation_flags"):
            if not re.search(rf"\b{required}\s*:", fields):
                errors.append(f"{PACKET}: DMA flip record lost exact {required}")
        if re.search(r"\bsnap\w*\s*:", fields):
            errors.append(f"{PACKET}: snapshot substitution returned to the D4 flip record")

    return errors


def mutate_function(sources: dict[str, str], path: str, name: str, injection: str) -> dict[str, str]:
    mutated = dict(sources)
    live = live_rust(mutated[path])
    found = [item for item in functions(live) if item.name == name]
    if len(found) != 1:
        raise RuntimeError(f"cannot mutate {path}:{name}")
    at = found[0].brace + 1
    mutated[path] = mutated[path][:at] + injection + mutated[path][at:]
    return mutated


def replace_in_function(
    sources: dict[str, str],
    path: str,
    name: str,
    old: str,
    new: str,
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


def replace_once(sources: dict[str, str], path: str, old: str, new: str) -> dict[str, str]:
    mutated = dict(sources)
    if mutated[path].count(old) != 1:
        raise RuntimeError(f"cannot uniquely replace {old!r} in {path}")
    mutated[path] = mutated[path].replace(old, new, 1)
    return mutated


def require_rejected(label: str, mutated: dict[str, str], needle: str | None = None) -> None:
    errors = check_sources(mutated)
    if not errors:
        raise SystemExit(f"D4 DIRQL mutation self-test failed: {label} was accepted")
    if needle is not None and not any(needle in error for error in errors):
        raise SystemExit(
            f"D4 DIRQL mutation self-test failed: {label} missed {needle!r}; got {errors!r}"
        )


def main() -> None:
    repo = os.path.abspath(sys.argv[1] if len(sys.argv) > 1 else os.path.join(os.path.dirname(__file__), ".."))
    sources = load_sources(repo)
    errors = check_sources(sources)
    if errors:
        raise SystemExit("D4 DIRQL static boundary violated:\n" + "\n".join(errors))

    require_rejected(
        "DIRQL allocation",
        mutate_function(sources, GPU, "enqueue_direct_locked", " let _ = DmaBuffer::new(passive, 1); "),
        "DIRQL-forbidden",
    )
    require_rejected(
        "DIRQL wait",
        mutate_function(sources, GPU, "enqueue_direct_locked", " KeWaitForSingleObject(a,b,c,d,e); "),
        "DIRQL-forbidden",
    )
    require_rejected(
        "DIRQL adapter lock",
        mutate_function(sources, ADAPTER, "enqueue_d4_scanout_dirql", " let _ = self.with_virtio(|_| ()); "),
        "DIRQL-forbidden",
    )
    require_rejected(
        "DIRQL PASSIVE proof",
        mutate_function(sources, DISPLAY, "set_vidpn_source_address_d4", " let _ = PassiveLevel::assume(); "),
        "DIRQL common path",
    )
    require_rejected(
        "DIRQL unsafe signal",
        mutate_function(sources, GPU, "enqueue_direct_locked", " KeSetEvent(event, 0, 0); "),
        "DIRQL-forbidden",
    )
    require_rejected(
        "synchronized callback wait",
        mutate_function(sources, GPU, "interrupt_queue_operation", " KeWaitForSingleObject(a,b,c,d,e); "),
        "DIRQL-forbidden",
    )
    require_rejected(
        "DIRQL PCI transport status access",
        mutate_function(
            sources,
            GPU,
            "interrupt_queue_operation",
            " let _ = transport.get_status(); ",
        ),
        "DIRQL-forbidden",
    )
    require_rejected(
        "untyped PCI transport reset",
        replace_once(
            sources,
            GPU,
            "    pub(crate) fn reset_status_and_poll(\n        &self,\n        _passive: crate::irql::PassiveLevel,\n",
            "    pub(crate) fn reset_status_and_poll(\n        &self,\n",
        ),
        "lost its PassiveLevel proof",
    )
    require_rejected(
        "PCI reset under raised virtio lock",
        mutate_function(
            sources,
            ADAPTER,
            "reset_virtio_physical",
            " let _ = self.with_virtio(|_| ()); ",
        ),
        "complete before virtio lock",
    )
    hidden_helper = mutate_function(
        sources,
        GPU,
        "enqueue_direct_locked",
        " hidden_dirql_wait(); ",
    )
    hidden_helper[GPU] += "\nfn hidden_dirql_wait() { KeWaitForSingleObject(a,b,c,d,e); }\n"
    require_rejected(
        "DIRQL wait hidden behind helper",
        hidden_helper,
        "static DIRQL call surface changed",
    )
    hidden_turbofish_helper = mutate_function(
        sources,
        GPU,
        "enqueue_direct_locked",
        " hidden_dirql_wait::<u32>(); ",
    )
    hidden_turbofish_helper[GPU] += (
        "\nfn hidden_dirql_wait<T>() { KeWaitForSingleObject(a,b,c,d,e); }\n"
    )
    require_rejected(
        "DIRQL wait hidden behind turbofish helper",
        hidden_turbofish_helper,
        "static DIRQL call surface changed",
    )
    hidden_parenthesized_helper = mutate_function(
        sources,
        GPU,
        "enqueue_direct_locked",
        " (hidden_dirql_wait)(); ",
    )
    hidden_parenthesized_helper[GPU] += (
        "\nfn hidden_dirql_wait() { KeWaitForSingleObject(a,b,c,d,e); }\n"
    )
    require_rejected(
        "DIRQL wait hidden behind parenthesized helper",
        hidden_parenthesized_helper,
        "static DIRQL indirect-call surface changed",
    )
    hidden_macro = mutate_function(
        sources,
        GPU,
        "enqueue_direct_locked",
        " hidden_dirql_wait!(); ",
    )
    hidden_macro[GPU] += (
        "\nmacro_rules! hidden_dirql_wait { () => { KeWaitForSingleObject(a,b,c,d,e); } }\n"
    )
    require_rejected(
        "DIRQL wait hidden behind macro",
        hidden_macro,
        "static DIRQL macro surface changed",
    )
    require_rejected(
        "virtqueue alloc feature enabled",
        replace_once(
            sources,
            KMD_CARGO,
            'virtio-drivers = { version = "=0.13.0", default-features = false }',
            'virtio-drivers = { version = "=0.13.0", default-features = true }',
        ),
        "alloc disabled",
    )
    require_rejected(
        "unreviewed virtqueue version",
        replace_once(
            sources,
            KMD_CARGO,
            'virtio-drivers = { version = "=0.13.0", default-features = false }',
            'virtio-drivers = { version = "0.13", default-features = false }',
        ),
        "exactly 0.13.0",
    )
    require_rejected(
        "DISPATCH DMA allocation",
        mutate_function(sources, DISPLAY, "arm_dma_flip_d4", " let _ = Vec::new(); "),
        "DISPATCH-forbidden",
    )
    require_rejected(
        "DISPATCH DMA worker signal",
        mutate_function(sources, ADAPTER, "enqueue_d4_scanout_dispatch", " self.signal_hpd(); "),
        "DISPATCH-forbidden",
    )
    require_rejected(
        "growable DIRQL queue",
        mutate_function(sources, GPU, "enqueue_direct_locked", " core.direct_slots.push(slot); "),
        "grow storage",
    )
    require_rejected(
        "recursive interrupt synchronization",
        mutate_function(sources, GPU, "enqueue_direct_at_dirql", " self.synchronize(operation); "),
        "recursively synchronizes",
    )
    require_rejected(
        "DIRQL queue exclusion bypass",
        replace_once(
            sources,
            GPU,
            "if !proof.authorizes(adapter) || self.is_failed() {\n            return Err(VirtioError::DeviceError);\n        }\n        let Some(access) = self.try_access() else {",
            "if !proof.authorizes(adapter) || self.is_failed() {\n            return Err(VirtioError::DeviceError);\n        }\n        let Some(_access) = Some(InterruptQueueAccess { queue: self }) else {",
        ),
        "bypasses the nonblocking exclusion gate",
    )
    require_rejected(
        "normal queue exclusion bypass",
        replace_once(
            sources,
            GPU,
            "let Some(access) = queue.try_access() else {",
            "let Some(_access) = Some(InterruptQueueAccess { queue }) else {",
        ),
        "callback bypasses",
    )
    require_rejected(
        "DIRQL queue release omission",
        replace_once(
            sources,
            GPU,
            "let result = unsafe { self.enqueue_direct_locked(adapter, work) };\n        access.release();\n        result",
            "unsafe { self.enqueue_direct_locked(adapter, work) }",
        ),
        "acquire-mutate-release",
    )
    require_rejected(
        "synchronized queue release omission",
        replace_once(
            sources,
            GPU,
            "    access.release();\n    handled\n}\n\nimpl InterruptQueue {",
            "    handled\n}\n\nimpl InterruptQueue {",
        ),
        "acquire-dispatch-release",
    )
    implicit_cleanup = dict(sources)
    implicit_cleanup[GPU] += (
        "\nimpl Drop for InterruptQueueAccess<'_> { "
        "fn drop(&mut self) { self.queue.release_access(); } }\n"
    )
    require_rejected(
        "implicit DIRQL access cleanup",
        implicit_cleanup,
        "may not run cleanup from DIRQL Drop",
    )
    require_rejected(
        "inline published queue alias",
        replace_once(
            sources,
            GPU,
            "queue: Box<InterruptQueue>,",
            "queue: InterruptQueue,",
        ),
        "separately heap-owned",
    )
    require_rejected(
        "contended notify accepted",
        replace_once(
            sources,
            GPU,
            "QueueCallResult::Notified => Ok(()),\n            QueueCallResult::QueueFull => Err(VirtioError::QueueFull),",
            "QueueCallResult::Notified | QueueCallResult::QueueFull => Ok(()),",
        ),
        "notification must fail closed",
    )
    require_rejected(
        "queue pointer hazard recheck removal",
        replace_in_function(
            sources,
            ADAPTER,
            "enqueue_d4_scanout_dirql",
            "if self.d4_dirql_queue.load(Ordering::SeqCst) != raw {",
            "if false {",
        ),
        "hazard order",
    )
    require_rejected(
        "late queue pointer publication",
        replace_once(
            sources,
            ADAPTER,
            "self.d4_dirql_queue.store(d4_queue, Ordering::SeqCst);\n        unsafe { KeReleaseSpinLock(self.virtio_lock.get(), irql) };",
            "unsafe { KeReleaseSpinLock(self.virtio_lock.get(), irql) };\n        self.d4_dirql_queue.store(d4_queue, Ordering::SeqCst);",
        ),
        "before virtio unlock",
    )
    require_rejected(
        "weakened queue pointer hazard ordering",
        replace_in_function(
            sources,
            ADAPTER,
            "enqueue_d4_scanout_dirql",
            "self.d4_dirql_readers.fetch_add(1, Ordering::SeqCst);",
            "self.d4_dirql_readers.fetch_add(1, Ordering::Acquire);",
        ),
        "hazard order",
    )
    require_rejected(
        "late retained transport publication",
        replace_once(
            sources,
            ADAPTER,
            "self.d4_dirql_transport_retained.store(1, Ordering::Release);\n        }\n        unsafe { KeReleaseSpinLock(self.virtio_lock.get(), irql) };",
            "unsafe { KeReleaseSpinLock(self.virtio_lock.get(), irql) };\n            self.d4_dirql_transport_retained.store(1, Ordering::Release);\n        }",
        ),
        "latch must publish",
    )
    require_rejected(
        "missing under-lock queue withdrawal",
        replace_once(
            sources,
            ADAPTER,
            "let irql = unsafe { KeAcquireSpinLockRaiseToDpc(self.virtio_lock.get()) };\n        // Repeat withdrawal under the owner lock. The first store blocks new\n        // readers promptly; this one also defeats any concurrent install that\n        // published while teardown was waiting to acquire the lock.\n        self.d4_dirql_queue.store(0, Ordering::SeqCst);",
            "let irql = unsafe { KeAcquireSpinLockRaiseToDpc(self.virtio_lock.get()) };",
        ),
        "withdrawal must repeat",
    )
    require_rejected(
        "shifted shared-primary flag",
        replace_once(
            sources,
            DISPLAY,
            "const CLASSIC_SHARED_PRIMARY_TRANSITION: u32 = 0x0000_0040;",
            "const CLASSIC_SHARED_PRIMARY_TRANSITION: u32 = 0x0000_0020;",
        ),
        "flag CLASSIC_SHARED_PRIMARY_TRANSITION drifted",
    )
    require_rejected(
        "stale D4 admission on failed start",
        replace_once(
            sources,
            DIRECT,
            ".active_transport_instance\n            .store(0, Ordering::Release);",
            ".active_transport_instance\n            .store(1, Ordering::Release);",
        ),
        "admission must close",
    )
    forged = dict(sources)
    marker = "pub(crate) struct SetVidPnDirql<'a>"
    forged[DISPLAY] = forged[DISPLAY].replace(marker, "#[derive(Clone)]\n" + marker, 1)
    require_rejected("cloneable DIRQL capability", forged, "non-Clone")

    second_mint = dict(sources)
    second_mint[DISPLAY] += "\nfn decoy(adapter: &AdapterContext) { let _ = SetVidPnDirql::mint(adapter); }\n"
    require_rejected("second capability mint", second_mint, "minted exactly once")

    unguarded = dict(sources)
    unguarded[DISPLAY] += "\nfn unguarded(adapter: &AdapterContext, work: Work) { let _ = adapter.enqueue_d4_scanout_dispatch(work); }\n"
    require_rejected("unguarded D4 authority", unguarded, "unexpected")

    decoy = dict(sources)
    decoy[DISPLAY] += (
        "\nfn decoy_guard(adapter: &AdapterContext, work: Work) { "
        "if !crate::virtio::KMD_D2_OWNER_ENABLED { let _ = false; } "
        "let _ = adapter.enqueue_d4_scanout_dispatch(work); }\n"
    )
    require_rejected("non-dominating D4 decoy guard", decoy, "unexpected")

    require_rejected(
        "classic wrapper entry guard removal",
        replace_once(
            sources,
            DISPLAY,
            "if crate::virtio::KMD_D2_OWNER_ENABLED {\n        return unsafe { set_vidpn_source_address_d4(adapter, address) };\n    }",
            "if true {\n        return unsafe { set_vidpn_source_address_d4(adapter, address) };\n    }",
        ),
        "dominating activation-coherence guard",
    )
    require_rejected(
        "legacy fast bind in active D4",
        mutate_function(
            sources,
            DISPLAY,
            "arm_dma_flip_d4",
            " fast_bind_from_flip(adapter, h_open_allocation, 0, 0, 0, None); ",
        ),
        "retired D4 mechanism",
    )

    require_rejected(
        "WDDM-1.x allocation query",
        replace_in_function(
            sources,
            ALLOC,
            "canonical_open_allocation",
            "let acquire = dxgkrnl.DxgkCbAcquireHandleData?;",
            "let acquire = dxgkrnl.DxgkCbGetHandleData?;",
        ),
        "WDDM-1.x DxgkCbGetHandleData",
    )
    require_rejected(
        "missing paired allocation release",
        replace_in_function(
            sources,
            ALLOC,
            "canonical_open_allocation",
            "unsafe { release(release_args) };",
            "let _ = release_args;",
        ),
        "acquire, validate, release, then publish",
    )
    require_rejected(
        "wrong paired allocation release type",
        replace_in_function(
            sources,
            ALLOC,
            "canonical_open_allocation",
            "Type: _DXGK_HANDLE_TYPE::DXGK_HANDLE_ALLOCATION,",
            "Type: _DXGK_HANDLE_TYPE::DXGK_HANDLE_RESOURCE,",
        ),
        "acquire, validate, release, then publish",
    )
    require_rejected(
        "allocation pointer published before paired release",
        replace_in_function(
            sources,
            ALLOC,
            "canonical_open_allocation",
            "unsafe { release(release_args) };\n    valid.then_some(allocation as usize)",
            "let published = valid.then_some(allocation as usize);\n    unsafe { release(release_args) };\n    published",
        ),
        "acquire, validate, release, then publish",
    )
    require_rejected(
        "allocation pointer accepted without magic validation",
        replace_in_function(
            sources,
            ALLOC,
            "canonical_open_allocation",
            "let valid = unsafe { resolve_alloc(allocation) }.is_some();",
            "let valid = true;",
        ),
        "acquire, validate, release, then publish",
    )
    require_rejected(
        "CloseAllocation open-object leak",
        replace_in_function(
            sources,
            ALLOC,
            "dxgkddi_close_allocation",
            "let _ = unsafe { take_open_ctx(handle) };",
            "let _ = handle;",
        ),
        "CloseAllocation no longer drops the device-specific open object",
    )
    require_rejected(
        "long-lived allocation acquire token",
        replace_once(
            sources,
            ALLOC,
            "    allocation: usize,",
            "    allocation: usize,\n    release_handle: DXGKARG_RELEASE_HANDLE,",
        ),
        "allocation acquire token escaped",
    )

    heuristic = dict(sources)
    heuristic[ALLOC] += "\nfn scanout_allocation_for_resource(_: u32) -> HANDLE { core::ptr::null_mut() }\n"
    require_rejected("heuristic resource reverse lookup", heuristic, "retired heuristic")

    snapshot = dict(sources)
    snapshot[PACKET] = snapshot[PACKET].replace(
        "operation_flags: u32,", "operation_flags: u32,\n    snap_resid: u32,", 1
    )
    require_rejected("snapshot substitution", snapshot, "snapshot substitution")

    decoupled = dict(sources)
    owner_anchor = (
        "pub(crate) const KMD_D2_OWNER_ENABLED: bool = "
        "matches!(SURFACE, WddmSurface::Wddm3_2GpuMmu);"
    )
    if decoupled[BOUNDARY_PATH].count(owner_anchor) != 1:
        raise SystemExit("D4 DIRQL mutation setup failed: owner derivation anchor drifted")
    decoupled[BOUNDARY_PATH] = decoupled[BOUNDARY_PATH].replace(
        owner_anchor,
        "pub(crate) const KMD_D2_OWNER_ENABLED: bool = true;",
        1,
    )
    require_rejected("decoupled D2 activation", decoupled, "sole SURFACE-derived")

    print("OK: D4 DIRQL proof, fixed queue, exact identity, and forbidden call graph are statically enforced; mutations rejected")


if __name__ == "__main__":
    main()
