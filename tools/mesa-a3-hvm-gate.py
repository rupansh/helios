#!/usr/bin/env python3
"""Mesa A3 escape-free HVM1 renderer source and in-memory mutation gate."""

from __future__ import annotations

import os
import re
import sys
from dataclasses import dataclass


REPO_DEFAULT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
MESA = "icd/mesa/src/virtio/vulkan/vn_renderer_helios_hvm.c"
OLD_MESA = "icd/mesa/src/virtio/vulkan/vn_renderer_helios.c"
RENDERER = "icd/mesa/src/virtio/vulkan/vn_renderer.h"
MEMORY = "icd/mesa/src/virtio/vulkan/vn_device_memory.c"
PHYSICAL = "icd/mesa/src/virtio/vulkan/vn_physical_device.c"
INSTANCE = "icd/mesa/src/virtio/vulkan/vn_instance.c"
RING = "icd/mesa/src/virtio/vulkan/vn_ring.c"
QUEUE = "icd/mesa/src/virtio/vulkan/vn_queue.c"
MESON = "icd/mesa/src/virtio/vulkan/meson.build"
RETIREMENT = "tools/retirement-gates.sh"

SOURCES = (MESA, RENDERER, MEMORY, PHYSICAL, INSTANCE, RING, QUEUE, MESON, RETIREMENT)


def compact(value: str) -> str:
    return re.sub(r"\s+", "", value)


def load_sources(repo: str) -> dict[str, str]:
    out: dict[str, str] = {}
    for path in SOURCES:
        with open(os.path.join(repo, path), encoding="utf-8", errors="replace") as stream:
            out[path] = stream.read()
    return out


def function(source: str, name: str) -> str:
    match = re.search(rf"(?m)^\s*(?:static\s+)?[^\n;]*\b{re.escape(name)}\s*\([^;]*?\)\s*\{{", source)
    if not match:
        return ""
    start = source.find("{", match.start())
    depth = 0
    state = "code"
    quote = ""
    i = start
    while i < len(source):
        ch = source[i]
        nxt = source[i + 1] if i + 1 < len(source) else ""
        if state == "line":
            if ch == "\n":
                state = "code"
        elif state == "block":
            if ch == "*" and nxt == "/":
                state = "code"
                i += 1
        elif state == "string":
            if ch == "\\":
                i += 1
            elif ch == quote:
                state = "code"
        else:
            if ch == "/" and nxt == "/":
                state = "line"
                i += 1
            elif ch == "/" and nxt == "*":
                state = "block"
                i += 1
            elif ch in ('"', "'"):
                state = "string"
                quote = ch
            elif ch == "{":
                depth += 1
            elif ch == "}":
                depth -= 1
                if depth == 0:
                    return source[match.start() : i + 1]
        i += 1
    return ""


def require(path: str, name: str, source: str, fragments: tuple[str, ...], errors: list[str]) -> None:
    value = compact(source)
    for fragment in fragments:
        if compact(fragment) not in value:
            errors.append(f"{path}:{name}: missing A3 fragment: {fragment}")


def require_order(path: str, name: str, source: str, tokens: tuple[str, ...], errors: list[str]) -> None:
    value = compact(source)
    cursor = 0
    for token in tokens:
        wanted = compact(token)
        found = value.find(wanted, cursor)
        if found < 0:
            errors.append(f"{path}:{name}: A3 order drifted at {token}")
            return
        cursor = found + len(wanted)


def check_sources(sources: dict[str, str], repo: str) -> list[str]:
    errors: list[str] = []
    mesa = sources[MESA]
    meson = sources[MESON]

    if os.path.exists(os.path.join(repo, OLD_MESA)):
        errors.append(f"{OLD_MESA}: retired selected Windows backend still exists")
    if meson.count("vn_renderer_helios_hvm.c") != 1:
        errors.append(f"{MESON}: A3 backend must be wired exactly once")
    if "vn_renderer_helios.c" in meson:
        errors.append(f"{MESON}: retired Escape renderer is still wired")

    forbidden = (
        r"D3DKMTEscape\s*\(",
        r"DeviceIoControl\s*\(",
        r"D3DKMTOpenNtHandleFromName\s*\(",
        r"D3DKMTOpenResource\s*\(",
        r"LoadLibrary(?:A|W)?\s*\(",
        r"GetProcAddress\s*\(",
        r"VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_WIN32(?:_KMT)?_BIT",
        r"HELIOS_ESCAPE_",
        r"present_stream",
    )
    for pattern in forbidden:
        if re.search(pattern, mesa, re.I):
            errors.append(f"{MESA}: selected A3 backend contains forbidden {pattern}")

    allocate = function(mesa, "helios_allocation_create")
    if compact("#define HELIOS_CPU_VISIBLE_MAX_BYTES (UINT64_C(1024) * UINT64_C(4096))") not in compact(mesa):
        errors.append(f"{MESA}: the proven CPU-visible bound is not exactly 1024 pages")
    require(
        MESA,
        "helios_allocation_create",
        allocate,
        (
            "const bool cpu_visible = role != HELIOS_HVM1_ROLE_VULKAN_DEVICE_LOCAL",
            "cpu_visible && size > HELIOS_CPU_VISIBLE_MAX_BYTES",
            "HELIOS_HVM1_CACHE_NOT_CPU_VISIBLE",
            "info.pSystemMem = NULL",
            "create.Flags.CreateResource = 1",
            "create.Flags.CreateShared = 1",
            "create.Flags.NtSecuritySharing = 1",
            "D3DKMTCreateAllocation2(&create)",
            "!hvm1.object_generation",
            "D3DKMTMakeResident(&resident)",
            "if (cpu_visible)",
            "D3DKMTLock2(&lock)",
        ),
        errors,
    )
    require_order(
        MESA,
        "helios_allocation_create",
        allocate,
        (
            "D3DKMTCreateAllocation2(&create)",
            "D3DKMTMakeResident(&resident)",
            "if (cpu_visible)",
            "D3DKMTLock2(&lock)",
            "*out = allocation",
        ),
        errors,
    )
    if "role == HELIOS_HVM1_ROLE_VULKAN_DEVICE_LOCAL" in compact(allocate) and "D3DKMTLock2" in compact(allocate).split("role==HELIOS_HVM1_ROLE_VULKAN_DEVICE_LOCAL")[-1][:300]:
        errors.append(f"{MESA}: role 4 gained a Lock2 path")

    destroy_alloc = function(mesa, "helios_allocation_destroy_locked")
    require_order(
        MESA,
        "helios_allocation_destroy_locked",
        destroy_alloc,
        ("if (allocation->cpu)", "D3DKMTUnlock2", "D3DKMTDestroyAllocation2", "memset(allocation, 0"),
        errors,
    )

    open_memory = function(mesa, "vn_renderer_helios_external_memory_open")
    require(
        MESA,
        "vn_renderer_helios_external_memory_open",
        open_memory,
        (
            "VK_EXTERNAL_MEMORY_HANDLE_TYPE_D3D12_RESOURCE_BIT",
            "!import_info->handle || import_info->name",
            "D3DKMTQueryResourceInfoFromNtHandle(&query)",
            "query.NumAllocations != 1",
            "calloc(1, query.PrivateRuntimeDataSize)",
            "D3DDDI_OPENALLOCATIONINFO2 allocation_info[1]",
            "memset(allocation_info, 0, sizeof(allocation_info))",
            "D3DKMTOpenResourceFromNtHandle(&open)",
            "allocation_info[0].hAllocation",
            "helios_hwa2_from_private_data",
            "helios_hwa2_validate_create_output",
            "HELIOS_HWA2_FLAG_RESOURCE_ASSOCIATED",
            "!open.hKeyedMutex && !open.hSyncObject",
            "D3DKMTDestroyAllocation2(&destroy)",
        ),
        errors,
    )
    require_order(
        MESA,
        "vn_renderer_helios_external_memory_open",
        open_memory,
        (
            "D3DKMTQueryResourceInfoFromNtHandle(&query)",
            "D3DKMTOpenResourceFromNtHandle(&open)",
            "helios_hwa2_from_private_data",
            "helios_hwa2_validate_create_output",
            "*out_bo = &bo->base",
        ),
        errors,
    )
    if "true||helios_hwa2_from_private_data" in compact(open_memory):
        errors.append(f"{MESA}: C57 HWA2 parse became optional")
    if "true||helios_hwa2_validate_create_output" in compact(open_memory):
        errors.append(f"{MESA}: C57 HWA2 validation became optional")
    if compact("st != 0 || query.NumAllocations != 1 || query.PrivateRuntimeDataSize > HELIOS_PRIVATE_DATA_LIMIT") not in compact(open_memory):
        errors.append(f"{MESA}: C57 sizing query no longer requires exactly one allocation")

    open_fence = function(mesa, "vn_renderer_helios_sync_create_from_win32")
    require(
        MESA,
        "vn_renderer_helios_sync_create_from_win32",
        open_fence,
        (
            "VK_EXTERNAL_SEMAPHORE_HANDLE_TYPE_D3D12_FENCE_BIT",
            "open.hDevice = helios->device",
            "open.EngineAffinity = 1u",
            "open.Flags.Shared = 1",
            "open.Flags.NtSecuritySharing = 1",
            "D3DKMTOpenNativeFenceFromNtHandle(&open)",
            "open.NativeFenceMapping.CurrentValueCpuVa",
            "open.NativeFenceMapping.CurrentValueGpuVa",
            "open.NativeFenceMapping.MonitoredValueGpuVa",
            "sizeof(open.NativeFenceMapping.Reserved)",
            "sizeof(open.Reserved)",
            "current_cpu & (sizeof(uint64_t) - 1)",
            "open.NativeFenceMapping.CurrentValueGpuVa & (sizeof(uint64_t) - 1)",
            "!generation",
            "D3DKMTDestroySynchronizationObject",
        ),
        errors,
    )

    import_fence = function(sources[QUEUE], "vn_ImportSemaphoreWin32HandleKHR")
    require(
        QUEUE,
        "vn_ImportSemaphoreWin32HandleKHR",
        import_fence,
        (
            "sem->base.vk.device != &dev->base.vk",
            "sem->type != VK_SEMAPHORE_TYPE_TIMELINE",
            "pImportSemaphoreWin32HandleInfo->flags != 0",
            "pImportSemaphoreWin32HandleInfo->name",
            "vn_renderer_helios_sync_create_from_win32",
            "vn_sync_payload_release(dev, &sem->permanent)",
            "sem->permanent.type = VN_SYNC_TYPE_IMPORTED_WIN32_SYNC",
            "sem->payload = &sem->permanent",
        ),
        errors,
    )
    if "sem->payload = &sem->temporary" in import_fence:
        errors.append(f"{QUEUE}: permanent D3D12 fence import was stored as temporary")

    allocate_memory = function(mesa, "vn_renderer_helios_allocate_memory")
    require(
        MESA,
        "vn_renderer_helios_allocate_memory",
        allocate_memory,
        (
            ".pNext = alloc_info->pNext, .resourceId = 0",
            "helios_session_execute_allocate",
            "bo->allocation.allocation",
            "bo->allocation.generation",
            "helios_native_context_destroy(helios->bootstrap)",
        ),
        errors,
    )

    create_renderer = function(mesa, "vn_renderer_create_helios")
    require_order(
        MESA,
        "vn_renderer_create_helios",
        create_renderer,
        (
            "helios_find_adapter(helios)",
            "helios_translation_session_create",
            "helios_session_device_handle",
            "helios_native_context_create",
            "D3DKMTCreatePagingQueue",
            "*out_renderer = &helios->base",
        ),
        errors,
    )
    destroy_renderer = function(mesa, "helios_destroy")
    require_order(
        MESA,
        "helios_destroy",
        destroy_renderer,
        (
            "helios_native_context_destroy(helios->bootstrap)",
            "D3DKMTDestroyPagingQueue",
            "helios_translation_session_destroy",
            "DeleteCriticalSection(&helios->bootstrap_lock)",
            "DeleteCriticalSection(&helios->allocation_lock)",
        ),
        errors,
    )
    destroy_compact = compact(destroy_renderer)
    if destroy_compact.find("helios_translation_session_destroy") < destroy_compact.find("helios_native_context_destroy"):
        errors.append(f"{MESA}: session teardown precedes bootstrap context drain")

    require(
        INSTANCE,
        "vn_CreateInstance",
        function(sources[INSTANCE], "vn_CreateInstance"),
        ("vn_object_set_id(instance, 1, VK_OBJECT_TYPE_INSTANCE)", "result = VK_SUCCESS"),
        errors,
    )
    ring_create = function(sources[RING], "vn_ring_create")
    require(RING, "vn_ring_create", ring_create, (".resourceId = 0", "(void)info"), errors)
    if "vn_renderer_submit_simple(instance->renderer" in compact(ring_create).split("#else")[-1]:
        errors.append(f"{RING}: Windows local ring seam submits a second CreateRing")

    physical = compact(sources[PHYSICAL])
    if "external_memory.win32_renderer_handle_type=0" not in physical:
        errors.append(f"{PHYSICAL}: A3 must not advertise A6 external-memory support")
    if "external_binary_semaphore_handles=0" not in physical or "external_timeline_semaphore_handles=0" not in physical:
        errors.append(f"{PHYSICAL}: A3 must not advertise A6 semaphore support")

    gate_line = 'python3 "$REPO/tools/mesa-a3-hvm-gate.py" "$REPO" --mutations'
    if sources[RETIREMENT].count(gate_line) != 1:
        errors.append(f"{RETIREMENT}: Mesa A3 mutation gate must be integrated exactly once")
    return errors


@dataclass(frozen=True)
class Mutation:
    name: str
    path: str
    old: str
    new: str


def mutation_cases() -> tuple[Mutation, ...]:
    return (
        Mutation("map role 4", MESA, "const bool cpu_visible = role != HELIOS_HVM1_ROLE_VULKAN_DEVICE_LOCAL;", "const bool cpu_visible = true;"),
        Mutation("remove 1024-page bound", MESA, "(cpu_visible && size > HELIOS_CPU_VISIBLE_MAX_BYTES)", "false"),
        Mutation("supply CPU pages", MESA, "info.pSystemMem = NULL;", "info.pSystemMem = out;"),
        Mutation("drop shared allocation", MESA, "create.Flags.CreateShared = 1;", "create.Flags.CreateShared = 0;"),
        Mutation("drop NT sharing", MESA, "create.Flags.NtSecuritySharing = 1;", "create.Flags.NtSecuritySharing = 0;"),
        Mutation("skip residency", MESA, "st = D3DKMTMakeResident(&resident);", "st = 0;"),
        Mutation("drop allocation generation", MESA, "!hvm1.object_generation", "false"),
        Mutation("drop Unlock2", MESA, "HELIOS_IGNORE_STATUS(D3DKMTUnlock2(&unlock));", "(void)unlock;"),
        Mutation("accept opaque memory", MESA, "VK_EXTERNAL_MEMORY_HANDLE_TYPE_D3D12_RESOURCE_BIT", "VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_WIN32_BIT"),
        Mutation("accept import name", MESA, "!import_info->handle || import_info->name", "!import_info->handle"),
        Mutation("accept multiple allocations", MESA, "st != 0 || query.NumAllocations != 1 ||\n       query.PrivateRuntimeDataSize > HELIOS_PRIVATE_DATA_LIMIT", "st != 0 || query.NumAllocations == 0 ||\n       query.PrivateRuntimeDataSize > HELIOS_PRIVATE_DATA_LIMIT"),
        Mutation("unzero open array", MESA, "memset(allocation_info, 0, sizeof(allocation_info));", "allocation_info[0].hAllocation = 1;"),
        Mutation("skip HWA2 parse", MESA, "helios_hwa2_from_private_data(", "true || helios_hwa2_from_private_data("),
        Mutation("skip HWA2 validate", MESA, "helios_hwa2_validate_create_output(", "true || helios_hwa2_validate_create_output("),
        Mutation("allow keyed mutex", MESA, "!open.hKeyedMutex && !open.hSyncObject", "true"),
        Mutation("accept opaque fence", MESA, "VK_EXTERNAL_SEMAPHORE_HANDLE_TYPE_D3D12_FENCE_BIT", "VK_EXTERNAL_SEMAPHORE_HANDLE_TYPE_OPAQUE_WIN32_BIT"),
        Mutation("widen fence affinity", MESA, "open.EngineAffinity = 1u;", "open.EngineAffinity = 3u;"),
        Mutation("drop fence shared bit", MESA, "open.Flags.Shared = 1;", "open.Flags.Shared = 0;"),
        Mutation("drop fence NT sharing", MESA, "open.Flags.NtSecuritySharing = 1;", "open.Flags.NtSecuritySharing = 0;"),
        Mutation("skip native mapping reserved", MESA, "i < sizeof(open.NativeFenceMapping.Reserved)", "i < 0"),
        Mutation("skip native mapping alignment", MESA, "(current_cpu & (sizeof(uint64_t) - 1))", "false"),
        Mutation("skip fence generation", MESA, "!generation || helios_load_u32", "false || helios_load_u32"),
        Mutation("accept foreign D3D12 fence", QUEUE, "sem->base.vk.device != &dev->base.vk", "false"),
        Mutation("accept binary D3D12 fence", QUEUE, "sem->base.vk.device != &dev->base.vk ||\n       sem->type != VK_SEMAPHORE_TYPE_TIMELINE", "sem->base.vk.device != &dev->base.vk ||\n       false"),
        Mutation("accept temporary D3D12 fence", QUEUE, "pImportSemaphoreWin32HandleInfo->flags != 0", "false"),
        Mutation("store D3D12 fence temporarily", QUEUE, "sem->permanent.win32_sync = sync;\n   sem->payload = &sem->permanent;", "sem->permanent.win32_sync = sync;\n   sem->payload = &sem->temporary;"),
        Mutation("wire raw resource id", MESA, ".pNext = alloc_info->pNext,\n      .resourceId = 0,", ".pNext = alloc_info->pNext,\n      .resourceId = 7,"),
        Mutation("create second host instance", INSTANCE, "result = VK_SUCCESS;\n#else", "result = vn_call_vkCreateInstance(instance->ring.ring, pCreateInfo, NULL, &instance_handle);\n#else"),
        Mutation("create Windows generic ring", RING, "/* K11 already owns the session namespace.", "vn_renderer_submit_simple(instance->renderer, NULL, 0);\n   /* K11 already owns the session namespace."),
        Mutation("destroy session before bootstrap", MESA, "if (helios->bootstrap) {\n         helios_native_context_destroy(helios->bootstrap);", "if (helios->bootstrap) {\n         helios_translation_session_destroy(helios->session);\n         helios_native_context_destroy(helios->bootstrap);"),
        Mutation("advertise A6 memory", PHYSICAL, "physical_dev->external_memory.win32_renderer_handle_type = 0;", "physical_dev->external_memory.win32_renderer_handle_type = VK_EXTERNAL_MEMORY_HANDLE_TYPE_D3D12_RESOURCE_BIT;"),
    )


def run_mutations(sources: dict[str, str], repo: str) -> None:
    for case in mutation_cases():
        source = sources[case.path]
        if source.count(case.old) != 1:
            raise SystemExit(f"A3 mutation setup failed for {case.name}: anchor count {source.count(case.old)}")
        mutated = dict(sources)
        mutated[case.path] = source.replace(case.old, case.new, 1)
        if not check_sources(mutated, repo):
            raise SystemExit(f"A3 mutation was accepted: {case.name}")
    print(f"OK: {len(mutation_cases())} in-memory Mesa A3 mutations rejected")


def main() -> None:
    repo = os.path.abspath(sys.argv[1]) if len(sys.argv) > 1 and not sys.argv[1].startswith("--") else REPO_DEFAULT
    sources = load_sources(repo)
    errors = check_sources(sources, repo)
    if errors:
        raise SystemExit("Mesa A3 HVM1 gate violated:\n" + "\n".join(errors))
    if "--mutations" in sys.argv[1:]:
        run_mutations(sources, repo)
    print("OK: Mesa A3 is escape-free, role-4-unmapped, exact-import, and reverse-teardown checked")


if __name__ == "__main__":
    main()
