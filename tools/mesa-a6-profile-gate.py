#!/usr/bin/env python3
"""Mesa A6 Windows lower-ICD profile source and in-memory mutation gate."""

from __future__ import annotations

import os
import re
import sys
from dataclasses import dataclass


REPO_DEFAULT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
PHYSICAL = "icd/mesa/src/virtio/vulkan/vn_physical_device.c"
PHYSICAL_H = "icd/mesa/src/virtio/vulkan/vn_physical_device.h"
INSTANCE = "icd/mesa/src/virtio/vulkan/vn_instance.c"
MEMORY = "icd/mesa/src/virtio/vulkan/vn_device_memory.c"
BUFFER = "icd/mesa/src/virtio/vulkan/vn_buffer.c"
IMAGE = "icd/mesa/src/virtio/vulkan/vn_image.c"
QUEUE = "icd/mesa/src/virtio/vulkan/vn_queue.c"
DEVICE = "icd/mesa/src/virtio/vulkan/vn_device.c"
SESSION = "icd/mesa/src/virtio/vulkan/vn_helios_translation_session.c"
SESSION_H = "icd/mesa/src/virtio/vulkan/vn_helios_translation_session.h"
RENDERER = "icd/mesa/src/virtio/vulkan/vn_renderer_helios_hvm.c"
RENDERER_H = "icd/mesa/src/virtio/vulkan/vn_renderer.h"
RETIREMENT = "tools/retirement-gates.sh"

SOURCES = (
    PHYSICAL,
    PHYSICAL_H,
    INSTANCE,
    MEMORY,
    BUFFER,
    IMAGE,
    QUEUE,
    DEVICE,
    SESSION,
    SESSION_H,
    RENDERER,
    RENDERER_H,
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
    match = re.search(
        rf"(?m)^\s*(?:static\s+)?[^\n;]*\b{re.escape(name)}\s*\([^;]*?\)\s*\{{",
        source,
    )
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
            errors.append(f"{path}:{name}: missing A6 fragment: {fragment}")


def require_order(path: str, name: str, source: str, tokens: tuple[str, ...], errors: list[str]) -> None:
    value = compact(source)
    cursor = 0
    for token in tokens:
        wanted = compact(token)
        found = value.find(wanted, cursor)
        if found < 0:
            errors.append(f"{path}:{name}: A6 order drifted at {token}")
            return
        cursor = found + len(wanted)


def check_sources(sources: dict[str, str]) -> list[str]:
    errors: list[str] = []
    physical = sources[PHYSICAL]
    physical_h = sources[PHYSICAL_H]

    if ".KHR_win32_surface = true" in sources[INSTANCE]:
        errors.append(f"{INSTANCE}: lower ICD re-advertises KHR_win32_surface")

    native_exts = function(physical, "vn_physical_device_get_native_extensions")
    require(
        PHYSICAL,
        "vn_physical_device_get_native_extensions",
        native_exts,
        ("#if defined(VN_USE_WSI_PLATFORM) && !DETECT_OS_WINDOWS",),
        errors,
    )
    physical_init = function(physical, "vn_physical_device_init")
    require(
        PHYSICAL,
        "vn_physical_device_init",
        physical_init,
        (
            "physical_dev->emulate_second_queue = -1",
            "physical_dev->sparse_binding_disabled = true",
            "#if DETECT_OS_WINDOWS\n   /* A6 removes the lower Windows present path.",
            "#else\n   result = vn_wsi_init(physical_dev)",
        ),
        errors,
    )
    physical_fini = function(physical, "vn_physical_device_fini")
    require(PHYSICAL, "vn_physical_device_fini", physical_fini, ("#if !DETECT_OS_WINDOWS\n   vn_wsi_fini",), errors)

    require(
        PHYSICAL_H,
        "two memory types",
        physical_h,
        (
            "VN_HELIOS_MEMORY_TYPE_DEVICE_LOCAL 0u",
            "VN_HELIOS_MEMORY_TYPE_HOST_VISIBLE 1u",
            "VN_HELIOS_MEMORY_TYPE_COUNT 2u",
            "helios_renderer_memory_type_indices[2]",
            "vn_physical_device_renderer_memory_type_index",
            "vn_physical_device_guest_memory_type_bits",
        ),
        errors,
    )
    memory_props = function(physical, "vn_physical_device_init_memory_properties")
    require_order(
        PHYSICAL,
        "vn_physical_device_init_memory_properties",
        memory_props,
        (
            "renderer_device_local = VK_MAX_MEMORY_TYPES",
            "renderer_host_visible = VK_MAX_MEMORY_TYPES",
            "HELIOS_A6_RENDERER_MEMORY_PROFILE_UNAVAILABLE",
            "vn_renderer_helios_local_heap_size",
            "HELIOS_A6_HLM1_HEAP_PROFILE_UNAVAILABLE",
            "props->memoryHeapCount = 1",
            ".size = MIN3(hlm1_heap_size",
            "props->memoryTypeCount = VN_HELIOS_MEMORY_TYPE_COUNT",
            "VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT",
            "VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT |\n                       VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT |\n                       VK_MEMORY_PROPERTY_HOST_COHERENT_BIT",
        ),
        errors,
    )
    guest_types = compact(memory_props).split(
        "props->memoryTypeCount=VN_HELIOS_MEMORY_TYPE_COUNT", 1
    )[-1].split("returnVK_SUCCESS;#else", 1)[0]
    if "VK_MEMORY_PROPERTY_HOST_CACHED_BIT" in guest_types:
        errors.append(f"{PHYSICAL}: A6 guest memory types gained HOST_CACHED")

    local_heap = function(sources[SESSION], "helios_session_local_heap_size")
    require(
        SESSION,
        "helios_session_local_heap_size",
        local_heap,
        (
            "s->adapter",
            "KMTQAITYPE_GETSEGMENTGROUPSIZE",
            "D3DKMTQueryAdapterInfo(&query)",
            "!sizes.LocalMemory",
            "*out_size = sizes.LocalMemory",
        ),
        errors,
    )
    require(RENDERER, "vn_renderer_helios_local_heap_size", function(sources[RENDERER], "vn_renderer_helios_local_heap_size"), ("helios_session_local_heap_size",), errors)

    if sources[BUFFER].count("vn_physical_device_sanitize_memory_requirements(") != 2:
        errors.append(f"{BUFFER}: buffer requirement masks must be remapped at both live query sites")
    if sources[IMAGE].count("vn_physical_device_sanitize_memory_requirements(") != 4:
        errors.append(f"{IMAGE}: image requirement masks must be remapped at all four live query sites")

    ordinary_alloc = function(sources[MEMORY], "vn_device_memory_alloc")
    require(
        MEMORY,
        "vn_device_memory_alloc",
        ordinary_alloc,
        (
            "if (mem->base.vk.export_handle_types)",
            "vn_physical_device_renderer_memory_type_index",
            "vn_renderer_helios_allocate_memory",
        ),
        errors,
    )
    import_memory = function(sources[MEMORY], "vn_device_memory_import_win32")
    require_order(
        MEMORY,
        "vn_device_memory_import_win32",
        import_memory,
        (
            "VK_EXTERNAL_MEMORY_HANDLE_TYPE_D3D12_RESOURCE_BIT",
            "!import_info->handle || import_info->name",
            "alloc_info->memoryTypeIndex != VN_HELIOS_MEMORY_TYPE_DEVICE_LOCAL",
            "!dedicated_image",
            "dedicated_image->base.vk.base.device != &dev->base.vk",
            "dedicated_image->base.vk.external_handle_types != VK_EXTERNAL_MEMORY_HANDLE_TYPE_D3D12_RESOURCE_BIT",
            "dedicated->buffer != VK_NULL_HANDLE",
            "mem->base.vk.export_handle_types",
            "vn_renderer_helios_external_memory_open",
            "payload_desc.allocation_kind != HELIOS_HWA2_KIND_IMAGE",
            "vn_physical_device_renderer_memory_type_index",
            "vn_renderer_helios_allocate_memory",
        ),
        errors,
    )
    handle_props = function(sources[MEMORY], "vn_GetMemoryWin32HandlePropertiesKHR")
    require(
        MEMORY,
        "vn_GetMemoryWin32HandlePropertiesKHR",
        handle_props,
        (
            "handleType != VK_EXTERNAL_MEMORY_HANDLE_TYPE_D3D12_RESOURCE_BIT",
            "vn_renderer_helios_external_memory_open",
            "payload_desc.allocation_kind == HELIOS_HWA2_KIND_IMAGE",
            "if (!exact_image) return vn_error(dev->instance, VK_ERROR_INVALID_EXTERNAL_HANDLE)",
            "UINT32_C(1) << VN_HELIOS_MEMORY_TYPE_DEVICE_LOCAL",
        ),
        errors,
    )

    image_formats = function(physical, "vn_sanitize_image_format_properties")
    require(
        PHYSICAL,
        "vn_sanitize_image_format_properties",
        image_formats,
        (
            "VK_EXTERNAL_MEMORY_FEATURE_IMPORTABLE_BIT | VK_EXTERNAL_MEMORY_FEATURE_DEDICATED_ONLY_BIT",
            "compatibleHandleTypes = VK_EXTERNAL_MEMORY_HANDLE_TYPE_D3D12_RESOURCE_BIT",
            "exportFromImportedHandleTypes = 0",
        ),
        errors,
    )
    buffer_props = function(physical, "vn_GetPhysicalDeviceExternalBufferProperties")
    require(
        PHYSICAL,
        "vn_GetPhysicalDeviceExternalBufferProperties",
        buffer_props,
        (
            "pExternalBufferInfo->handleType == VK_EXTERNAL_MEMORY_HANDLE_TYPE_D3D12_RESOURCE_BIT",
            "props->externalMemoryFeatures = 0",
        ),
        errors,
    )

    semaphore_props = function(physical, "vn_GetPhysicalDeviceExternalSemaphoreProperties")
    require(
        PHYSICAL,
        "vn_GetPhysicalDeviceExternalSemaphoreProperties",
        semaphore_props,
        (
            "sem_type == VK_SEMAPHORE_TYPE_TIMELINE",
            "VK_EXTERNAL_SEMAPHORE_HANDLE_TYPE_D3D12_FENCE_BIT",
            "exportFromImportedHandleTypes = 0",
            "VK_EXTERNAL_SEMAPHORE_FEATURE_EXPORTABLE_BIT",
            "VK_EXTERNAL_SEMAPHORE_FEATURE_IMPORTABLE_BIT",
        ),
        errors,
    )
    require(
        PHYSICAL,
        "D3D12_FENCE export-only block",
        semaphore_props,
        (
            "compatibleHandleTypes =\n         VK_EXTERNAL_SEMAPHORE_HANDLE_TYPE_D3D12_FENCE_BIT;\n      pExternalSemaphoreProperties->exportFromImportedHandleTypes = 0;\n      pExternalSemaphoreProperties->externalSemaphoreFeatures =\n         VK_EXTERNAL_SEMAPHORE_FEATURE_EXPORTABLE_BIT;\n      return;",
        ),
        errors,
    )
    semaphore_import = function(sources[QUEUE], "vn_ImportSemaphoreWin32HandleKHR")
    require(
        QUEUE,
        "vn_ImportSemaphoreWin32HandleKHR",
        semaphore_import,
        (
            "sem->base.vk.device != &dev->base.vk",
            "sem->type != VK_SEMAPHORE_TYPE_TIMELINE",
            "VK_EXTERNAL_SEMAPHORE_HANDLE_TYPE_D3D12_FENCE_BIT",
            "pImportSemaphoreWin32HandleInfo->flags != 0",
            "!pImportSemaphoreWin32HandleInfo->handle",
            "pImportSemaphoreWin32HandleInfo->name",
            "sem->permanent.win32_sync = sync",
            "sem->payload = &sem->permanent",
        ),
        errors,
    )
    semaphore_export = function(sources[QUEUE], "vn_GetSemaphoreWin32HandleKHR")
    require(
        QUEUE,
        "Vulkan-owned D3D12 fence export",
        semaphore_export,
        (
            "sem->base.vk.device != &dev->base.vk",
            "sem->type != VK_SEMAPHORE_TYPE_TIMELINE",
            "VK_EXTERNAL_SEMAPHORE_HANDLE_TYPE_D3D12_FENCE_BIT",
            "sem->external_handle_types & pGetWin32HandleInfo->handleType",
            "sem->payload != &sem->permanent",
            "!sem->permanent.win32_sync",
            "vn_renderer_helios_sync_export_win32",
            "*pHandle = (HANDLE)handle",
        ),
        errors,
    )

    external_memory_init = function(physical, "vn_physical_device_init_external_memory")
    require(
        PHYSICAL,
        "A7 memory activation boundary",
        external_memory_init,
        (
            "physical_dev->external_memory.win32_renderer_handle_type =\n"
            "      vn_physical_device_is_helios_normal_loader(physical_dev)\n"
            "         ? physical_dev->external_memory.renderer_handle_type\n"
            "         : 0",
            "physical_dev->external_memory.supported_handle_types =\n"
            "      vn_physical_device_is_helios_normal_loader(physical_dev)\n"
            "         ? VK_EXTERNAL_MEMORY_HANDLE_TYPE_D3D12_RESOURCE_BIT\n"
            "         : 0",
        ),
        errors,
    )
    external_semaphore_init = function(physical, "vn_physical_device_init_external_semaphore_handles")
    require_order(
        PHYSICAL,
        "A7 semaphore activation boundary",
        external_semaphore_init,
        (
            "physical_dev->external_timeline_semaphore_handles = 0",
            "if (vn_physical_device_is_helios_normal_loader(physical_dev))",
            "physical_dev->external_timeline_semaphore_handles =\n"
            "         VK_EXTERNAL_SEMAPHORE_HANDLE_TYPE_D3D12_FENCE_BIT",
        ),
        errors,
    )
    require(
        PHYSICAL,
        "normal-loader native capability publication",
        native_exts,
        (
            "exts->KHR_external_memory_win32 =\n"
            "      physical_dev->external_memory.supported_handle_types ==\n"
            "      VK_EXTERNAL_MEMORY_HANDLE_TYPE_D3D12_RESOURCE_BIT",
            "exts->KHR_external_semaphore_win32 =\n"
            "      physical_dev->external_timeline_semaphore_handles ==\n"
            "      VK_EXTERNAL_SEMAPHORE_HANDLE_TYPE_D3D12_FENCE_BIT",
        ),
        errors,
    )

    feature_profile = function(physical, "vn_physical_device_apply_helios_normal_feature_profile")
    require(
        PHYSICAL,
        "normal feature profile",
        feature_profile,
        (
            "if (!vn_physical_device_is_helios_normal_loader(physical_dev)) return",
            "feats->descriptorIndexing = false",
            "feats->descriptorBindingPartiallyBound = false",
            "feats->descriptorBindingVariableDescriptorCount = false",
            "feats->runtimeDescriptorArray = false",
            "feats->descriptorBuffer = false",
            "feats->descriptorHeap = false",
            "feats->shaderUniformBufferUnsizedArray = false",
        ),
        errors,
    )
    require(
        PHYSICAL,
        "normal closure bounds",
        physical,
        (
            "VN_HELIOS_NORMAL_MAX_GENERATED_USES <= HELIOS_HNR2_MAX_USE_RECORDS",
            "VN_HELIOS_NORMAL_MAX_GENERATED_OPERANDS <= HELIOS_HNR2_MAX_PATCH_RECORDS",
            "VN_HELIOS_CLAMP_DESCRIPTOR(maxDescriptorSetUniformBuffers)",
            "VN_HELIOS_CLAMP_DESCRIPTOR(maxDescriptorSetStorageBuffers)",
            "VN_HELIOS_CLAMP_DESCRIPTOR(maxDescriptorSetSampledImages)",
            "VN_HELIOS_CLAMP_DESCRIPTOR(maxDescriptorSetStorageImages)",
            "VN_HELIOS_CLAMP_DESCRIPTOR(maxDescriptorSetInputAttachments)",
        ),
        errors,
    )
    supported = function(physical, "vn_physical_device_init_supported_extensions")
    require(
        PHYSICAL,
        "normal extension profile",
        supported,
        (
            "EXT_memory_budget = false",
            "EXT_descriptor_heap = false",
            "EXT_descriptor_buffer = false",
            "EXT_mutable_descriptor_type = false",
            "VALVE_mutable_descriptor_type = false",
        ),
        errors,
    )
    queues = function(physical, "vn_physical_device_init_queue_family_properties")
    require(PHYSICAL, "queue profile", queues, ("~VK_QUEUE_SPARSE_BINDING_BIT", "physical_dev->emulate_second_queue = -1"), errors)

    gate_line = 'python3 "$REPO/tools/mesa-a6-profile-gate.py" "$REPO" --mutations'
    if sources[RETIREMENT].count(gate_line) != 1:
        errors.append(f"{RETIREMENT}: Mesa A6 mutation gate must be integrated exactly once")
    return errors


@dataclass(frozen=True)
class Mutation:
    name: str
    path: str
    old: str
    new: str


def mutation_cases() -> tuple[Mutation, ...]:
    return (
        Mutation("restore win32 surface", INSTANCE, ".EXT_headless_surface = true,", ".KHR_win32_surface = true,"),
        Mutation("restore Windows WSI init", PHYSICAL, "#if DETECT_OS_WINDOWS\n   /* A6 removes the lower Windows present path.", "#if 0\n   /* A6 removes the lower Windows present path."),
        Mutation("expose D3D memory to record-only", PHYSICAL, "vn_physical_device_is_helios_normal_loader(physical_dev)\n         ? VK_EXTERNAL_MEMORY_HANDLE_TYPE_D3D12_RESOURCE_BIT", "true\n         ? VK_EXTERNAL_MEMORY_HANDLE_TYPE_D3D12_RESOURCE_BIT"),
        Mutation("expose renderer handle to record-only", PHYSICAL, "vn_physical_device_is_helios_normal_loader(physical_dev)\n         ? physical_dev->external_memory.renderer_handle_type", "true\n         ? physical_dev->external_memory.renderer_handle_type"),
        Mutation("expose D3D fence to record-only", PHYSICAL, "if (vn_physical_device_is_helios_normal_loader(physical_dev))\n      physical_dev->external_timeline_semaphore_handles", "if (true)\n      physical_dev->external_timeline_semaphore_handles"),
        Mutation("weaken native memory publication", PHYSICAL, "supported_handle_types ==\n      VK_EXTERNAL_MEMORY_HANDLE_TYPE_D3D12_RESOURCE_BIT", "supported_handle_types != 0"),
        Mutation("add third memory type", PHYSICAL_H, "VN_HELIOS_MEMORY_TYPE_COUNT        2u", "VN_HELIOS_MEMORY_TYPE_COUNT        3u"),
        Mutation("cache guest host memory", PHYSICAL, "VK_MEMORY_PROPERTY_HOST_COHERENT_BIT,", "VK_MEMORY_PROPERTY_HOST_COHERENT_BIT | VK_MEMORY_PROPERTY_HOST_CACHED_BIT,"),
        Mutation("replace HLM1 size query", SESSION, "query.Type = KMTQAITYPE_GETSEGMENTGROUPSIZE;", "query.Type = KMTQAITYPE_GETSEGMENTSIZE;"),
        Mutation("accept zero HLM1 heap", SESSION, "D3DKMTQueryAdapterInfo(&query) != 0 || !sizes.LocalMemory", "D3DKMTQueryAdapterInfo(&query) != 0"),
        Mutation("drop buffer mask map", BUFFER, "vn_physical_device_sanitize_memory_requirements(\n      dev->physical_device, &pMemoryRequirements->memoryRequirements);", "(void)pMemoryRequirements;"),
        Mutation("drop image mask map", IMAGE, "vn_physical_device_sanitize_memory_requirements(\n         dev->physical_device,\n         &img->requirements[0].memory.memoryRequirements);", "(void)img;"),
        Mutation("pass guest type to renderer", MEMORY, "local_info.alloc.memoryTypeIndex =\n      vn_physical_device_renderer_memory_type_index(\n         dev->physical_device, mem->base.vk.memory_type_index);", "local_info.alloc.memoryTypeIndex = mem->base.vk.memory_type_index;"),
        Mutation("accept opaque memory", MEMORY, "import_info->handleType !=\n          VK_EXTERNAL_MEMORY_HANDLE_TYPE_D3D12_RESOURCE_BIT", "import_info->handleType !=\n          VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_WIN32_BIT"),
        Mutation("accept named memory", MEMORY, "!import_info->handle || import_info->name", "!import_info->handle"),
        Mutation("accept host-visible import type", MEMORY, "alloc_info->memoryTypeIndex != VN_HELIOS_MEMORY_TYPE_DEVICE_LOCAL", "alloc_info->memoryTypeIndex >= VN_HELIOS_MEMORY_TYPE_COUNT"),
        Mutation("accept nondedicated import", MEMORY, "!dedicated_image ||", "false ||"),
        Mutation("accept foreign image", MEMORY, "dedicated_image->base.vk.base.device != &dev->base.vk", "false"),
        Mutation("accept wrong image handle type", MEMORY, "dedicated_image->base.vk.external_handle_types !=\n          VK_EXTERNAL_MEMORY_HANDLE_TYPE_D3D12_RESOURCE_BIT", "false"),
        Mutation("accept buffer dedicated import", MEMORY, "dedicated->buffer != VK_NULL_HANDLE", "false"),
        Mutation("accept nonimage HWA2", MEMORY, "payload_desc.allocation_kind != HELIOS_HWA2_KIND_IMAGE", "false"),
        Mutation("accept nonimage handle query", MEMORY, "payload_desc.allocation_kind == HELIOS_HWA2_KIND_IMAGE", "true"),
        Mutation("report host-visible Win32 type", MEMORY, "UINT32_C(1) << VN_HELIOS_MEMORY_TYPE_DEVICE_LOCAL;", "UINT32_C(1) << VN_HELIOS_MEMORY_TYPE_HOST_VISIBLE;"),
        Mutation("drop dedicated-only", PHYSICAL, "VK_EXTERNAL_MEMORY_FEATURE_IMPORTABLE_BIT |\n            VK_EXTERNAL_MEMORY_FEATURE_DEDICATED_ONLY_BIT", "VK_EXTERNAL_MEMORY_FEATURE_IMPORTABLE_BIT"),
        Mutation("export imported fence", PHYSICAL, "compatibleHandleTypes =\n         VK_EXTERNAL_SEMAPHORE_HANDLE_TYPE_D3D12_FENCE_BIT;\n      pExternalSemaphoreProperties->exportFromImportedHandleTypes = 0;", "compatibleHandleTypes =\n         VK_EXTERNAL_SEMAPHORE_HANDLE_TYPE_D3D12_FENCE_BIT;\n      pExternalSemaphoreProperties->exportFromImportedHandleTypes = VK_EXTERNAL_SEMAPHORE_HANDLE_TYPE_D3D12_FENCE_BIT;"),
        Mutation("restore rejected fence import capability", PHYSICAL, "pExternalSemaphoreProperties->externalSemaphoreFeatures =\n         VK_EXTERNAL_SEMAPHORE_FEATURE_EXPORTABLE_BIT;", "pExternalSemaphoreProperties->externalSemaphoreFeatures =\n         VK_EXTERNAL_SEMAPHORE_FEATURE_IMPORTABLE_BIT;"),
        Mutation("accept binary fence", QUEUE, "sem->base.vk.device != &dev->base.vk ||\n       sem->type != VK_SEMAPHORE_TYPE_TIMELINE ||\n       pImportSemaphoreWin32HandleInfo->handleType", "sem->base.vk.device != &dev->base.vk ||\n       false ||\n       pImportSemaphoreWin32HandleInfo->handleType"),
        Mutation("accept temporary fence", QUEUE, "pImportSemaphoreWin32HandleInfo->flags != 0", "false"),
        Mutation("enable descriptor indexing", PHYSICAL, "feats->descriptorIndexing = false;", "feats->descriptorIndexing = true;"),
        Mutation("drop use closure assertion", PHYSICAL, "VN_HELIOS_NORMAL_MAX_GENERATED_USES <=\n                 HELIOS_HNR2_MAX_USE_RECORDS", "true"),
        Mutation("enable descriptor heap", PHYSICAL, "physical_dev->base.vk.supported_extensions.EXT_descriptor_heap = false;", "physical_dev->base.vk.supported_extensions.EXT_descriptor_heap = true;"),
        Mutation("advertise memory budget", PHYSICAL, "physical_dev->base.vk.supported_extensions.EXT_memory_budget = false;", "physical_dev->base.vk.supported_extensions.EXT_memory_budget = true;"),
        Mutation("restore sparse queue", PHYSICAL, "~VK_QUEUE_SPARSE_BINDING_BIT", "~0u"),
        Mutation("emulate second queue", PHYSICAL, "physical_dev->emulate_second_queue = -1;\n   physical_dev->sparse_binding_disabled = true;\n#endif", "physical_dev->emulate_second_queue = 0;\n   physical_dev->sparse_binding_disabled = true;\n#endif"),
    )


def run_mutations(sources: dict[str, str]) -> None:
    for case in mutation_cases():
        source = sources[case.path]
        count = source.count(case.old)
        if count != 1:
            raise SystemExit(f"A6 mutation setup failed for {case.name}: anchor count {count}")
        mutated = dict(sources)
        mutated[case.path] = source.replace(case.old, case.new, 1)
        if not check_sources(mutated):
            raise SystemExit(f"A6 mutation was accepted: {case.name}")
    print(f"OK: {len(mutation_cases())} in-memory Mesa A6 mutations rejected")


def main() -> None:
    repo = os.path.abspath(sys.argv[1]) if len(sys.argv) > 1 and not sys.argv[1].startswith("--") else REPO_DEFAULT
    sources = load_sources(repo)
    errors = check_sources(sources)
    if errors:
        raise SystemExit("Mesa A6 profile gate violated:\n" + "\n".join(errors))
    if "--mutations" in sys.argv[1:]:
        run_mutations(sources)
    print("OK: Mesa A6 is two-type, no-WSI, bounded, exact-import, and A7-activated only for the normal loader")


if __name__ == "__main__":
    main()
