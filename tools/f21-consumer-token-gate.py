#!/usr/bin/env python3
"""F21 direct-consumer and exact outer-allocation-token mutation gate."""

from __future__ import annotations

import os
import re
import sys
from dataclasses import dataclass


REPO_DEFAULT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))

PROTOCOL = "protocol/src/resource_association.rs"
PROTOCOL_H = "protocol/include/helios_resource_association.h"
DISPATCH_H = "protocol/include/helios_translator_dispatch.h"
COMMON = "umd_common/src/direct_translator.rs"
UMD_STATE = "umd/src/forward/state.rs"
UMD_DEVICE = "umd/src/device_funcs.rs"
UMD_BRIDGE = "umd/bridge/dxvk_bridge.cpp"
UMD_BUILD = "umd/build.rs"
UMD12_IDENTITY = "umd12/src/forward12/identity12.rs"
UMD12_RESOURCE = "umd12/src/forward12/resource12.rs"
UMD12_QUEUE = "umd12/src/forward12/queue.rs"
UMD12_BRIDGE = "umd12/bridge/vkd3d_bridge.cpp"
UMD12_BUILD = "umd12/build.rs"
DXVK_INSTANCE = "dxvk-helios/src/dxvk/dxvk_instance.cpp"
DXVK_LOADER = "dxvk-helios/src/vulkan/vulkan_loader.cpp"
DXVK_DEVICE = "dxvk-helios/src/dxvk/dxvk_device.cpp"
DXVK_MEMORY = "dxvk-helios/src/dxvk/dxvk_memory.cpp"
VKD_ENTRY = "vkd3d-proton-helios/libs/d3d12core/helios_entry.c"
VKD_DEVICE = "vkd3d-proton-helios/libs/vkd3d/device.c"
VKD_MEMORY = "vkd3d-proton-helios/libs/vkd3d/memory.c"
VKD_HEAP = "vkd3d-proton-helios/libs/vkd3d/heap.c"
RETIREMENT = "tools/retirement-gates.sh"

SOURCES = (
    PROTOCOL,
    PROTOCOL_H,
    DISPATCH_H,
    COMMON,
    UMD_STATE,
    UMD_DEVICE,
    UMD_BRIDGE,
    UMD_BUILD,
    UMD12_IDENTITY,
    UMD12_RESOURCE,
    UMD12_QUEUE,
    UMD12_BRIDGE,
    UMD12_BUILD,
    DXVK_INSTANCE,
    DXVK_LOADER,
    DXVK_DEVICE,
    DXVK_MEMORY,
    VKD_ENTRY,
    VKD_DEVICE,
    VKD_MEMORY,
    VKD_HEAP,
    RETIREMENT,
)


def compact(value: str) -> str:
    return re.sub(r"\s+", "", value)


def load_sources(repo: str) -> dict[str, str]:
    out: dict[str, str] = {}
    for path in SOURCES:
        with open(os.path.join(repo, path), encoding="utf-8", errors="replace") as stream:
            out[path] = stream.read()
    return out


def function(source: str, name: str) -> str:
    rust = re.search(rf"\bfn\s+{re.escape(name)}(?:\s*<[^{{}};]*>)?\s*\(", source)
    candidates = [rust] if rust else []
    candidates.extend(re.finditer(rf"\b{re.escape(name)}\s*\(", source))
    match = None
    start = -1
    for candidate in candidates:
        if candidate is None:
            continue
        paren = source.find("(", candidate.start())
        depth = 0
        end = paren
        while end < len(source):
            if source[end] == "(":
                depth += 1
            elif source[end] == ")":
                depth -= 1
                if depth == 0:
                    break
            end += 1
        brace = source.find("{", end + 1)
        semi = source.find(";", end + 1)
        if brace >= 0 and (semi < 0 or brace < semi):
            match = candidate
            start = brace
            break
    if match is None:
        return ""
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
        elif ch == "/" and nxt == "/":
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


def require(path: str, label: str, source: str, parts: tuple[str, ...], errors: list[str]) -> None:
    value = compact(source)
    for part in parts:
        if compact(part) not in value:
            errors.append(f"{path}:{label}: missing F21 invariant: {part}")


def require_order(path: str, label: str, source: str, parts: tuple[str, ...], errors: list[str]) -> None:
    value = compact(source)
    cursor = 0
    for part in parts:
        wanted = compact(part)
        at = value.find(wanted, cursor)
        if at < 0:
            errors.append(f"{path}:{label}: order drifted at {part}")
            return
        cursor = at + len(wanted)


def check_sources(s: dict[str, str]) -> list[str]:
    errors: list[str] = []

    require(
        PROTOCOL,
        "HRA1 declaration",
        s[PROTOCOL],
        (
            "pub const HELIOS_RESOURCE_ASSOCIATION_BYTES: u32 = 72",
            "pub device_generation: u64",
            "pub outer_allocation_token: u64",
            "pub outer_allocation_bytes: u64",
            "pub cpu_mapping: *mut c_void",
            "self.device_generation != expected_device_generation",
            "self.outer_allocation_token == 0",
            "self.outer_allocation_bytes == 0",
            "!= !self.cpu_mapping.is_null()",
        ),
        errors,
    )
    require(
        PROTOCOL_H,
        "C mirror",
        s[PROTOCOL_H],
        (
            "sizeof(HeliosResourceAssociationV1) == 72",
            "HELIOS_RESOURCE_ASSOCIATION_ALIGNOF(HeliosResourceAssociationV1) == 8",
            "offsetof(HeliosResourceAssociationV1, outer_allocation_token) == 40",
            "offsetof(HeliosResourceAssociationV1, cpu_mapping) == 56",
            "offsetof(HeliosResourceAssociationV1, reserved1) == 68",
        ),
        errors,
    )
    hra = s[PROTOCOL].split("pub struct HeliosResourceAssociationV1", 1)[-1].split("}", 1)[0]
    for forbidden in (
        "host_resid",
        "h_allocation",
        "wddm_handle",
        "gpu_virtual_address",
        "allocation_generation",
        "resource_id",
        "process_id",
        "name:",
    ):
        if forbidden in hra.lower():
            errors.append(f"{PROTOCOL}: HRA1 gained forbidden identity field {forbidden}")
    require(
        DISPATCH_H,
        "fixed A5 ABI",
        s[DISPATCH_H],
        ("sizeof(HeliosTranslatorDispatchV1) == 112", "24-byte header + 11 slots"),
        errors,
    )

    require(
        COMMON,
        "direct package import",
        s[COMMON],
        (
            "fn helios_icd_create_translator_v1(",
            "let status = unsafe { helios_icd_create_translator_v1(&create_info, &mut instance) }",
            "entry_module != dispatch.icd_module_base.cast_mut()",
            "dispatch_provenance_is_exact(dispatch, entry_module)",
            "HELIOS_TRANSLATOR_SUBMISSION_MODE_RECORD_ONLY",
        ),
        errors,
    )
    for forbidden in ("LoadLibrary", "GetProcAddress", "K32EnumProcessModules", "EnumProcessModules"):
        if re.search(rf"\b{forbidden}\s*\(", s[COMMON]):
            errors.append(f"{COMMON}: direct construction calls forbidden {forbidden}")

    for path in (UMD_BUILD, UMD12_BUILD):
        if s[path].count("vulkan_virtio.dll.a") != 1:
            errors.append(f"{path}: lower-ICD import library must be linked exactly once")

    require(
        UMD_BRIDGE,
        "DXVK injection",
        function(s[UMD_BRIDGE], "helios_dxvk_create_device"),
        (
            "import_info.loaderProc",
            "import_info.instance",
            "import_info.recordOnlyDirect = true",
            "import_info.expectedModule",
            "createDevice(outer_ops)",
        ),
        errors,
    )
    require(
        DXVK_INSTANCE,
        "direct loader selection",
        function(s[DXVK_INSTANCE], "DxvkInstance::initVulkanLoader"),
        (
            "args.loaderProc",
            "args.recordOnlyDirect ? args.instance : VK_NULL_HANDLE",
            "args.expectedModule",
            ": new vk::LibraryFn()",
        ),
        errors,
    )
    init_instance = function(s[DXVK_INSTANCE], "DxvkInstance::initVulkanInstance")
    require(
        DXVK_INSTANCE,
        "single instance",
        init_instance,
        (
            "if (!args.recordOnlyDirect)",
            "vkEnumerateInstanceLayerProperties",
            "vkEnumerateInstanceExtensionProperties",
            "VkInstance instance = args.instance",
            "if (!instance)",
            "!args.instance, instance",
        ),
        errors,
    )
    direct_prefix = init_instance.split("// When importing an instance", 1)[0]
    for call in (
        "vkEnumerateInstanceLayerProperties",
        "vkEnumerateInstanceExtensionProperties",
    ):
        call_at = direct_prefix.find(call)
        guard_at = direct_prefix.rfind("if (!args.recordOnlyDirect)", 0, call_at)
        if call_at < 0 or guard_at < 0:
            errors.append(
                f"{DXVK_INSTANCE}: direct A5 path does not guard loader-global {call}"
            )
    require(
        DXVK_LOADER,
        "procedure provenance",
        function(s[DXVK_LOADER], "LibraryLoader::ownsProc"),
        (
            "GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS",
            "GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT",
            "owner != m_expectedModule",
        ),
        errors,
    )

    require(
        VKD_ENTRY,
        "vkd3d direct entry",
        function(s[VKD_ENTRY], "helios_vkd3d_create_device"),
        (
            "instance_create_info.pfn_vkGetInstanceProcAddr = gipa",
            "instance_create_info.vk_instance = vk_instance",
            "instance_create_info.expected_vk_module = expected_vk_module",
            "instance_create_info.helios_record_only = true",
            "device_create_info.helios_outer_allocation_create",
        ),
        errors,
    )
    if re.search(r"\b(?:LoadLibrary|GetProcAddress)\w*\s*\(", s[VKD_ENTRY]):
        errors.append(f"{VKD_ENTRY}: direct entry gained loader/module search")
    imported = s[VKD_DEVICE].split("if (create_info->vk_instance)", 1)[-1].split("if (FAILED(hr = vkd3d_init_vk_global_procs", 1)[0]
    require(
        VKD_DEVICE,
        "imported instance branch",
        imported,
        (
            "vkd3d_load_vk_instance_procs",
            "instance->vk_instance = create_info->vk_instance",
            "instance->owns_vk_instance = false",
            "instance->helios_record_only = create_info->helios_record_only",
            "return S_OK",
        ),
        errors,
    )

    assign11 = function(s[UMD_STATE], "assign_outer_allocation")
    require_order(
        UMD_STATE,
        "D3D11 token assignment",
        assign11,
        (
            "set.entries.len() >= crate::device_funcs::HELIOS_MAX_LIVE_OUTER_ALLOCATIONS",
            "DuplicateAllocation",
            "let token = set.next_token",
            "token == 0 || token == u64::MAX",
            "set.next_token = token + 1",
            "set.entries.push",
            "outer_allocation_token: token",
            ".validate(HELIOS_PACKAGE_GENERATION, device_generation)",
        ),
        errors,
    )
    retire11 = function(s[UMD_STATE], "retire_outer_allocation")
    require_order(
        UMD_STATE,
        "D3D11 reverse teardown",
        retire11,
        (
            ".position(|pending|",
            ".position(|entry|",
            "set.pending_teardown.swap_remove",
            "set.entries.swap_remove",
            "complete_pending_outer_allocation",
        ),
        errors,
    )
    complete11 = function(s[UMD_STATE], "complete_pending_outer_allocation")
    require(
        UMD_STATE,
        "D3D11 failed deallocation ownership",
        complete11,
        (
            "let needs_deallocate = allocation != 0 || !pending.rt_resource.is_null()",
            "outer.kt_callbacks.is_null()",
            "(*outer.kt_callbacks).pfnDeallocateCb.is_none()",
            "backing.leak()",
            "return Err(R::TeardownNotPending)",
        ),
        errors,
    )
    finish11 = function(s[UMD_DEVICE], "dxvk_outer_submit_finish")
    require_order(
        UMD_DEVICE,
        "D3D11 scope provenance cleanup",
        finish11,
        (
            "let Some(scope) = active.take()",
            "if cookie != context",
            "close_outer_scope(scope, None)",
            'mark_outer_lost(outer, "outer submit cookie mismatch")',
        ),
        errors,
    )
    submit11 = function(s[UMD_DEVICE], "submit_outer_scope")
    require(
        UMD_DEVICE,
        "D3D11 HOB submission",
        submit11,
        (
            "use_record.byte_offset != 0",
            "seen_tokens.contains(&use_record.outer_allocation_token)",
            ".checked_add(use_record.byte_length)",
            ".find(|state| state.token == use_record.outer_allocation_token)",
            "state.allocation_generation == 0",
            "end > state.bytes",
            "address_or_index: u64::from(allocation_index)",
            "entry.hAllocation = state.allocation",
            "render.NumAllocations = resolved.len() as u32",
            "render_cb(outer.h_rt_device, &mut render)",
            "signal_hqc1_locked",
            "close_outer_scope(scope, Some(progress))",
        ),
        errors,
    )

    encode = function(s[COMMON], "encode_hob1")
    require(
        COMMON,
        "field-by-field sealed conversion",
        encode,
        (
            "address_or_index: identity.address_or_index",
            "byte_length: sealed.byte_length",
            "expected_allocation_generation: identity.allocation_generation",
            "access_flags: sealed.access_flags",
            "identity_kind",
            "operand_count: sealed.operand_count",
            "first_operand: sealed.first_operand",
            "reserved: 0",
            "payload_offset: absolute",
            "operand_kind: sealed.operand_kind",
            "encoded_width: sealed.encoded_width",
        ),
        errors,
    )
    if "memcpy" in encode or "copy_from_slice" in encode:
        errors.append(f"{COMMON}: sealed-use conversion became a bulk copy")

    identity12 = s[UMD12_IDENTITY]
    require(
        UMD12_IDENTITY,
        "device-owned token set",
        identity12,
        (
            "pub(crate) const MAX_LIVE_ALLOCATIONS: usize = 4096",
            "pub(crate) struct IdentityRegistry",
            "next_token: u64",
            "entries: Vec<AllocationEntry>",
            "let token = self.next_token",
            "self.next_token = token + 1",
            "DuplicateToken",
            "ForeignDeviceGeneration",
            "UseAfterDestroy",
            "self.entries.swap_remove(index)",
        ),
        errors,
    )
    if re.search(r"(?m)^\s*static\s+\w+\s*:\s*.*IdentityRegistry", identity12):
        errors.append(f"{UMD12_IDENTITY}: identity registry became process-global")

    submit12 = function(s[UMD12_QUEUE], "submit_outer_scope12")
    require(
        UMD12_QUEUE,
        "D3D12 HOB/HOS submission",
        submit12,
        (
            ".resolve_token(",
            ".gpu_virtual_address.checked_add(use_record.byte_offset)",
            "seen_tokens.contains(&use_record.outer_allocation_token)",
            "address_or_index: address",
            "let mut hos1 = hob.outer_submit()",
            "core::mem::size_of_val(&hos1) != helios_protocol::HELIOS_HOS1_BYTES as usize",
            "(*dev.kt_callbacks).pfnSubmitCommandCb",
            "arg.Commands = reservation.gpuva",
            "arg.PrivateDriverDataSize = u32::from(helios_protocol::HELIOS_HOS1_BYTES)",
            "signal_outer_hqc1",
            "close_outer_scope(scope, Some(progress))",
        ),
        errors,
    )
    finish12 = function(s[UMD12_QUEUE], "outer_allocation_finish")
    require_order(
        UMD12_QUEUE,
        "D3D12 scope provenance cleanup",
        finish12,
        (
            "let context_matches = context == scope.device_context",
            "device_from_outer_context(scope.device_context)",
            "let translator_scope = active.take()",
            "if !context_matches",
            "close_outer_scope(translator_scope, None)",
            'mark_outer_lost(outer.as_ref(), "terminal allocation scope provenance")',
        ),
        errors,
    )

    require(
        DXVK_MEMORY,
        "DXVK immutable allocation edge",
        function(s[DXVK_MEMORY], "DxvkMemoryAllocator::allocateDeviceMemory"),
        (
            "createHeliosOuterAllocation",
            "internalAssociation.p_next = next",
            "memoryInfo = { VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO, next }",
            "vkAllocateMemory",
            "result.heliosAssociation = *exactAssociation",
            "result.heliosAssociation.p_next = nullptr",
        ),
        errors,
    )
    require_order(
        DXVK_MEMORY,
        "DXVK reverse teardown",
        function(s[DXVK_MEMORY], "DxvkMemoryAllocator::freeDeviceMemory"),
        (
            "beginHeliosOuterAllocationTeardown",
            "vkFreeMemory",
            "finishHeliosOuterSubmit",
            "retireHeliosOuterAllocation",
        ),
        errors,
    )

    require_order(
        VKD_MEMORY,
        "vkd3d allocation ingress",
        s[VKD_MEMORY],
        (
            "device->helios_outer_allocation_create(",
            "helios_association.p_next = pNext",
            "allocate_info.pNext = &helios_association",
            "VK_CALL(vkAllocateMemory",
            "allocation->helios_device_generation = helios_association.device_generation",
            "allocation->helios_outer_allocation_token",
        ),
        errors,
    )
    require_order(
        VKD_MEMORY,
        "vkd3d reverse teardown",
        function(s[VKD_MEMORY], "vkd3d_free_memory"),
        (
            "helios_outer_allocation_begin",
            "vkd3d_memory_allocation_free",
            "helios_outer_allocation_finish",
            "helios_outer_allocation_retire",
        ),
        errors,
    )

    global_identity = re.sub(
        r"//.*?$|/\*.*?\*/|//!.*?$", "", s[UMD_STATE] + s[UMD12_IDENTITY],
        flags=re.MULTILINE | re.DOTALL,
    )
    for pattern in (r"static\s+.*OuterAllocationSet", r"OnceLock\s*<\s*HashMap"):
        if re.search(pattern, global_identity):
            errors.append("UMD exact allocation ownership became process-global")

    gate_line = 'python3 "$REPO/tools/f21-consumer-token-gate.py" "$REPO" --mutations'
    if s[RETIREMENT].count(gate_line) != 1:
        errors.append(f"{RETIREMENT}: F21 consumer/token gate must be integrated exactly once")
    return errors


@dataclass(frozen=True)
class Mutation:
    name: str
    path: str
    old: str
    new: str


def mutation_cases() -> tuple[Mutation, ...]:
    return (
        Mutation("zero token admitted", PROTOCOL, "if self.outer_allocation_token == 0", "if false"),
        Mutation("change HRA size", PROTOCOL, "HELIOS_RESOURCE_ASSOCIATION_BYTES: u32 = 72", "HELIOS_RESOURCE_ASSOCIATION_BYTES: u32 = 64"),
        Mutation("drop entry provenance", COMMON, "entry_module != dispatch.icd_module_base.cast_mut()", "false"),
        Mutation("DXVK loader fallback", UMD_BRIDGE, "import_info.recordOnlyDirect = true", "import_info.recordOnlyDirect = false"),
        Mutation("DXVK direct loader enumeration", DXVK_INSTANCE, "if (!args.recordOnlyDirect) {\n      uint32_t layerCount", "if (true) {\n      uint32_t layerCount"),
        Mutation("DXVK second instance", DXVK_INSTANCE, "VkInstance instance = args.instance;", "VkInstance instance = VK_NULL_HANDLE;"),
        Mutation("vkd3d owns imported instance", VKD_DEVICE, "instance->owns_vk_instance = false;", "instance->owns_vk_instance = true;"),
        Mutation("D3D11 reuse token", UMD_STATE, "set.next_token = token + 1;", "set.next_token = token;"),
        Mutation("D3D11 ignore missing deallocate callback", UMD_STATE, "|| (*outer.kt_callbacks).pfnDeallocateCb.is_none())", "|| false)"),
        Mutation("D3D11 accept foreign scope cookie", UMD_DEVICE, "if cookie != context {", "if false {"),
        Mutation("D3D11 accept offset", UMD_DEVICE, "if use_record.byte_offset != 0", "if false"),
        Mutation("D3D11 use token as index", UMD_DEVICE, "address_or_index: u64::from(allocation_index)", "address_or_index: use_record.outer_allocation_token"),
        Mutation("bulk-copy sealed use", COMMON, "uses.push(HeliosOuterBatchUseV1 {", "copy_from_slice(); uses.push(HeliosOuterBatchUseV1 {"),
        Mutation("D3D12 reuse token", UMD12_IDENTITY, "self.next_token = token + 1;", "self.next_token = token;"),
        Mutation("D3D12 accept foreign scope context", UMD12_QUEUE, "let context_matches = context == scope.device_context;", "let context_matches = true;"),
        Mutation("D3D12 omit byte offset", UMD12_QUEUE, ".gpu_virtual_address\n                .checked_add(use_record.byte_offset)", ".gpu_virtual_address\n                .checked_add(0)"),
        Mutation("DXVK omit association pNext", DXVK_MEMORY, "internalAssociation.p_next = next;", "internalAssociation.p_next = nullptr;"),
        Mutation("vkd3d omit association pNext", VKD_MEMORY, "helios_association.p_next = pNext;", "helios_association.p_next = NULL;"),
        Mutation(
            "vkd3d omit reverse retirement",
            VKD_MEMORY,
            "        retire_hr = device->helios_outer_allocation_retire(\n"
            "                device->helios_outer_context,\n"
            "                allocation->helios_device_generation,\n"
            "                allocation->helios_outer_allocation_token,\n"
            "                teardown_result);",
            "        retire_hr = E_FAIL;",
        ),
    )


def run_mutations(sources: dict[str, str]) -> None:
    for case in mutation_cases():
        source = sources[case.path]
        count = source.count(case.old)
        if count != 1:
            raise SystemExit(f"F21 mutation setup failed for {case.name}: anchor count {count}")
        mutated = dict(sources)
        mutated[case.path] = source.replace(case.old, case.new, 1)
        if not check_sources(mutated):
            raise SystemExit(f"F21 mutation was accepted: {case.name}")
    print(f"OK: {len(mutation_cases())} in-memory F21 consumer/token mutations rejected")


def main() -> None:
    repo = os.path.abspath(sys.argv[1]) if len(sys.argv) > 1 and not sys.argv[1].startswith("--") else REPO_DEFAULT
    sources = load_sources(repo)
    errors = check_sources(sources)
    if errors:
        raise SystemExit("F21 consumer/token gate violated:\n" + "\n".join(errors))
    if "--mutations" in sys.argv[1:]:
        run_mutations(sources)
    print("OK: F21 direct consumers and exact outer-allocation ownership are closed")


if __name__ == "__main__":
    main()
