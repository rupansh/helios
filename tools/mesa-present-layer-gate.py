#!/usr/bin/env python3
"""VK_LAYER_HELIOS_present B0-B9 source and in-memory mutation gate."""

from __future__ import annotations

import os
import re
import sys
from dataclasses import dataclass


REPO_DEFAULT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
LAYER = "icd/mesa/src/vulkan/helios-present-layer/helios_present_layer.cpp"
LAYER_H = "icd/mesa/src/vulkan/helios-present-layer/helios_present_layer.h"
LAYER_MESON = "icd/mesa/src/vulkan/helios-present-layer/meson.build"
LAYER_DEF = "icd/mesa/src/vulkan/helios-present-layer/helios_present_layer.def"
MANIFEST = "icd/mesa/src/vulkan/helios-present-layer/VkLayer_HELIOS_present.json.in"
PRIVATE_WSI = "icd/mesa/src/vulkan/helios_private_wsi.h"
VULKAN_MESON = "icd/mesa/src/vulkan/meson.build"
WSI_MESON = "icd/mesa/src/vulkan/wsi/meson.build"
VIRTIO_MESON = "icd/mesa/src/virtio/vulkan/meson.build"
LOWER_DEF = "icd/mesa/src/virtio/vulkan/vn_helios_exports.def"
VN_IMAGE = "icd/mesa/src/virtio/vulkan/vn_image.c"
VN_IMAGE_H = "icd/mesa/src/virtio/vulkan/vn_image.h"
RETIREMENT = "tools/retirement-gates.sh"

GENERIC_WSI = (
    "icd/mesa/src/vulkan/wsi/wsi_common.c",
    "icd/mesa/src/vulkan/wsi/wsi_common.h",
    "icd/mesa/src/vulkan/wsi/wsi_common_private.h",
    "icd/mesa/src/vulkan/wsi/wsi_common_win32.cpp",
)
REMOVED_WSI = (
    "icd/mesa/src/vulkan/wsi/wsi_helios_present_sync.c",
    "icd/mesa/src/vulkan/wsi/wsi_helios_present_sync.h",
)
SOURCES = (
    LAYER,
    LAYER_H,
    LAYER_MESON,
    LAYER_DEF,
    MANIFEST,
    PRIVATE_WSI,
    VULKAN_MESON,
    WSI_MESON,
    VIRTIO_MESON,
    LOWER_DEF,
    VN_IMAGE,
    VN_IMAGE_H,
    RETIREMENT,
    *GENERIC_WSI,
)


def compact(value: str) -> str:
    return re.sub(r"\s+", "", value)


def load_sources(repo: str) -> dict[str, str]:
    result: dict[str, str] = {}
    for path in SOURCES:
        with open(os.path.join(repo, path), encoding="utf-8", errors="replace") as stream:
            result[path] = stream.read()
    return result


def function(source: str, name: str) -> str:
    for candidate in re.finditer(rf"\b{re.escape(name)}\s*\(", source):
        paren = source.find("(", candidate.start())
        depth = 0
        end = paren
        state = "code"
        while end < len(source):
            ch = source[end]
            nxt = source[end + 1] if end + 1 < len(source) else ""
            if state == "line":
                if ch == "\n":
                    state = "code"
            elif state == "block":
                if ch == "*" and nxt == "/":
                    state = "code"
                    end += 1
            elif ch == "/" and nxt == "/":
                state = "line"
                end += 1
            elif ch == "/" and nxt == "*":
                state = "block"
                end += 1
            elif ch == "(":
                depth += 1
            elif ch == ")":
                depth -= 1
                if depth == 0:
                    break
            end += 1
        brace = source.find("{", end + 1)
        semi = source.find(";", end + 1)
        if brace < 0 or (semi >= 0 and semi < brace):
            continue
        depth = 0
        state = "code"
        quote = ""
        i = brace
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
                    return source[candidate.start() : i + 1]
            i += 1
    return ""


def require(path: str, label: str, source: str, fragments: tuple[str, ...], errors: list[str]) -> None:
    value = compact(source)
    for fragment in fragments:
        if compact(fragment) not in value:
            errors.append(f"{path}:{label}: missing B-lane fragment: {fragment}")


def require_order(path: str, label: str, source: str, tokens: tuple[str, ...], errors: list[str]) -> None:
    value = compact(source)
    cursor = 0
    for token in tokens:
        wanted = compact(token)
        found = value.find(wanted, cursor)
        if found < 0:
            errors.append(f"{path}:{label}: B-lane order drifted at {token}")
            return
        cursor = found + len(wanted)


def forbid(path: str, label: str, source: str, tokens: tuple[str, ...], errors: list[str]) -> None:
    for token in tokens:
        if token.lower() in source.lower():
            errors.append(f"{path}:{label}: forbidden B-lane token remains: {token}")


def def_exports(source: str) -> tuple[str, ...]:
    lines = []
    after_exports = False
    for raw in source.splitlines():
        line = raw.strip()
        if not line or line.startswith(";"):
            continue
        if line.upper() == "EXPORTS":
            after_exports = True
            continue
        if after_exports:
            lines.append(line.split()[0])
    return tuple(lines)


def check_sources(repo: str, s: dict[str, str]) -> list[str]:
    errors: list[str] = []
    layer = s[LAYER]

    # B0: remove only the retired Helios writer additions; keep the ordinary
    # platform selection and the separate Windows fail-closed seam.
    for path in REMOVED_WSI:
        if os.path.exists(os.path.join(repo, path)):
            errors.append(f"{path}: retired Mesa HPS2 source still exists")
    forbid(WSI_MESON, "B0 source list", s[WSI_MESON], ("wsi_helios_present_sync", "helios_present_sync"), errors)
    require(
        WSI_MESON,
        "generic WSI preservation",
        s[WSI_MESON],
        (
            "files_vulkan_wsi = files('wsi_common.c')",
            "if with_platform_x11",
            "if with_platform_wayland",
            "if with_platform_windows",
            "files_vulkan_wsi += files('wsi_common_win32_failclosed.c')",
            "else\n  files_vulkan_wsi += files('wsi_common_headless.c')",
            "if with_platform_macos",
            "if system_has_kms_drm and not with_platform_android",
        ),
        errors,
    )
    for path in GENERIC_WSI:
        forbid(path, "retired HPS2 unreachability", s[path], ("wsi_helios_present_sync", "HPS2"), errors)
    require(
        VIRTIO_MESON,
        "Windows header-only WSI boundary",
        s[VIRTIO_MESON],
        (
            "if with_platform_windows\n  vn_deps += idep_vulkan_wsi_headers",
            "else\n  vn_deps += idep_vulkan_wsi",
        ),
        errors,
    )

    # B1-B3: exact loader chain and owner-rooted virtual object graph.
    negotiate = function(layer, "helios_layer_NegotiateLoaderLayerInterfaceVersion")
    require_order(
        LAYER,
        "loader negotiation",
        negotiate,
        (
            "pVersionStruct->sType != LAYER_NEGOTIATE_INTERFACE_STRUCT",
            "!helios_verify_entry_manifest()",
            "CURRENT_LOADER_LAYER_INTERFACE_VERSION",
            "MIN_SUPPORTED_LOADER_LAYER_INTERFACE_VERSION",
            "pfnGetInstanceProcAddr = helios_layer_GetInstanceProcAddr",
            "pfnGetDeviceProcAddr = helios_layer_GetDeviceProcAddr",
            "pfnGetPhysicalDeviceProcAddr = helios_layer_GetPhysicalDeviceProcAddr",
        ),
        errors,
    )
    create_instance = function(layer, "helios_CreateInstance")
    require_order(
        LAYER,
        "captured next instance dispatch",
        create_instance,
        (
            "pfnNextGetInstanceProcAddr",
            "pfnNextGetPhysicalDeviceProcAddr",
            "chain->u.pLayerInfo = chain->u.pLayerInfo->pNext",
            "next_gipa(VK_NULL_HANDLE, \"vkCreateInstance\")",
            "VkInstanceCreateInfo lower = *pCreateInfo",
            "lower.ppEnabledExtensionNames",
            "next_create(&lower",
            "inst->disp.GetInstanceProcAddr = next_gipa",
        ),
        errors,
    )
    create_device = function(layer, "helios_CreateDevice")
    require_order(
        LAYER,
        "captured next device dispatch",
        create_device,
        (
            "pfnNextGetDeviceProcAddr",
            "chain->u.pLayerInfo = chain->u.pLayerInfo->pNext",
            "VkDeviceCreateInfo lower = *pCreateInfo",
            "VK_KHR_EXTERNAL_MEMORY_WIN32_EXTENSION_NAME",
            "VK_KHR_EXTERNAL_SEMAPHORE_WIN32_EXTENSION_NAME",
            "next_create(physicalDevice, &lower",
            "dev->disp.GetDeviceProcAddr = next_gdpa",
            "next_gdpa(*pDevice, HELIOS_SET_PRESENTABLE_IMAGE_NAME)",
        ),
        errors,
    )
    require(
        LAYER,
        "loader device-create chain copy",
        function(layer, "helios_pnext_size"),
        (
            "case VK_STRUCTURE_TYPE_LOADER_DEVICE_CREATE_INFO:",
            "return sizeof(VkLayerDeviceCreateInfo)",
        ),
        errors,
    )
    forbid(
        LAYER,
        "acyclic next-layer provenance",
        layer,
        (
            "LoadLibrary",
            "GetModuleHandle",
            "GetProcAddress",
            "EnumProcessModules",
            "vulkan-1.dll",
            "helios_icd_create_translator_v1",
            "D3DKMTEscape",
            "DeviceIoControl",
            "CreateFile(",
            "thread_local",
        ),
        errors,
    )
    require(
        LAYER,
        "owner-scoped non-dispatchable handles",
        layer,
        (
            "std::unordered_map<VkSurfaceKHR, HeliosSurface *> surfaces",
            "std::unordered_map<VkSwapchainKHR, HeliosSwapchain *> swapchains",
            "helios_surface_of(HeliosInstance *inst",
            "helios_swapchain_of(HeliosDevice *dev",
            "helios_allocate_generation(helios_next_surface_id)",
            "helios_allocate_generation(helios_next_swapchain_id)",
            "dev->retained_backings.emplace(sc->id, sc)",
        ),
        errors,
    )
    forbid(
        LAYER,
        "association fallbacks",
        layer,
        ("helios_surfaces", "helios_swapchains", "GetCurrentProcessId", "TLS", "(uintptr_t)s;"),
        errors,
    )
    admit = function(layer, "helios_admit_compute")
    require(
        LAYER,
        "exact LUID admission",
        admit,
        ("id.deviceLUIDValid", "memcpy(&info.luid, id.deviceLUID", "helios_adapter_by_luid", "info.admitted = true"),
        errors,
    )
    require(
        LAYER,
        "exact adapter construction",
        function(layer, "helios_d3d12_device"),
        ("helios_adapter_by_luid(dev->inst, info.luid)", "D3D12CreateDevice(adapter"),
        errors,
    )

    # B4-B5: resource import and the corrected Vulkan-to-D3D12 fence direction.
    canonical = function(layer, "helios_import_image")
    require_order(
        LAYER,
        "canonical image before exposure",
        canonical,
        (
            "VK_EXTERNAL_MEMORY_HANDLE_TYPE_D3D12_RESOURCE_BIT",
            "GetMemoryWin32HandlePropertiesKHR",
            "VkMemoryDedicatedAllocateInfo dedicated",
            "VkImportMemoryWin32HandleInfoKHR import",
            "AllocateMemory",
            "bind.memoryOffset = 0",
            "BindImageMemory2",
            "SetHeliosPresentableImage",
        ),
        errors,
    )
    fence = function(layer, "helios_export_fence")
    require_order(
        LAYER,
        "Vulkan-owned exported fences",
        fence,
        (
            "VkExportSemaphoreCreateInfo export_info",
            "VK_EXTERNAL_SEMAPHORE_HANDLE_TYPE_D3D12_FENCE_BIT",
            "VK_SEMAPHORE_TYPE_TIMELINE",
            "dev->disp.CreateSemaphore",
            "dev->disp.GetSemaphoreWin32HandleKHR",
            "d3d->OpenSharedHandle",
            "CloseHandle(h)",
        ),
        errors,
    )
    require(
        LAYER,
        "transient fence handle refusal closure",
        fence,
        ("if (h)\n         CloseHandle(h)", "DestroySemaphore", "helios_com_release(fence)"),
        errors,
    )
    forbid(
        LAYER,
        "rejected D3D-to-Vulkan fence direction",
        layer,
        ("vkImportSemaphoreWin32HandleKHR", "ImportSemaphoreWin32Handle", "D3D12_FENCE_FLAG_SHARED"),
        errors,
    )

    # B6: the complete state set, event waits, overflow refusal and causal order.
    states = re.search(r"enum HeliosSlotState\s*\{(.*?)\};", layer, re.S)
    expected_states = (
        "HELIOS_SLOT_NEVER_USED",
        "HELIOS_SLOT_AVAILABLE",
        "HELIOS_SLOT_ACQUIRED",
        "HELIOS_SLOT_PRESENT_QUEUED",
        "HELIOS_SLOT_D3D_COPY",
        "HELIOS_SLOT_DXGI_OWNED",
        "HELIOS_SLOT_RELEASE_QUEUED",
        "HELIOS_SLOT_ALIAS_ONLY",
        "HELIOS_SLOT_LOST",
    )
    if not states or tuple(re.findall(r"HELIOS_SLOT_[A-Z0-9_]+", states.group(1))) != expected_states:
        errors.append(f"{LAYER}: nine-state slot enum drifted")
    acquire = function(layer, "helios_acquire")
    require(
        LAYER,
        "event-based acquire",
        acquire,
        (
            "if (slot.epoch == UINT64_MAX)",
            "VK_NOT_READY",
            "VK_TIMEOUT",
            "SetEventOnCompletion(wait_epoch",
            "WaitForMultipleObjects",
            "ResetCommandPool",
            "QueueSubmit2(dev->helper_queue",
        ),
        errors,
    )
    forbid(LAYER, "no acquire polling/watchdog", acquire, ("Sleep(", "sleep_for", "watchdog", "poll("), errors)
    present = function(layer, "helios_QueuePresentKHR")
    require_order(
        LAYER,
        "Ready-copy-Release-Present causality",
        present,
        (
            "dev->disp.QueueSubmit2(queue",
            "sc->queue->Wait(slot.ready, epoch)",
            "GetCurrentBackBufferIndex()",
            "CopyResource(back, slot.s)",
            "sc->queue->Signal(slot.release, epoch)",
            "slot.state = HELIOS_SLOT_RELEASE_QUEUED",
            "sc->dxgi->Present(1, 0)",
        ),
        errors,
    )
    require(
        LAYER,
        "per-swapchain results",
        present,
        (
            "pPresentInfo->pResults[k] = results[k]",
            "fail_all_presentable(\"record release barrier\")",
            "fail_all_presentable(\"common release submit\")",
            "if (release_state_error)\n      return fail_all_presentable(release_state_error)",
        ),
        errors,
    )
    if present.count("write_present_results();") < 3:
        errors.append(f"{LAYER}: Present does not write pResults on every post-validation exit")

    # B7: exact candidate association, validation, mixed batch and independent lifetime.
    require(
        PRIVATE_WSI,
        "fixed candidate sentinel",
        s[PRIVATE_WSI],
        ("HELIOS_PRESENTABLE_IMAGE_ALIAS_CANDIDATE UINT32_MAX", "PFN_vkSetHeliosPresentableImageHELIOS"),
        errors,
    )
    lower_tag = function(s[VN_IMAGE], "vn_SetHeliosPresentableImageHELIOS")
    require_order(
        VN_IMAGE,
        "candidate-to-slot association",
        lower_tag,
        (
            "imageIndex == HELIOS_PRESENTABLE_IMAGE_ALIAS_CANDIDATE",
            "img->helios_presentable.alias_candidate = true",
            "if (img->helios_presentable.alias_candidate)",
            "img->helios_presentable.swapchain_id != swapchainId",
            "img->helios_presentable.alias_candidate = false",
            "img->helios_presentable.tagged = true",
            "img->helios_presentable.image_index = imageIndex",
        ),
        errors,
    )
    alias_create = function(layer, "helios_CreateImage")
    require(
        LAYER,
        "alias create VUID and null form",
        alias_create,
        (
            "sci->swapchain == VK_NULL_HANDLE",
            "helios_strip_pnext",
            "VUID-VkImageSwapchainCreateInfoKHR-swapchain-00995",
            "VK_EXTERNAL_MEMORY_HANDLE_TYPE_D3D12_RESOURCE_BIT",
            "HELIOS_PRESENTABLE_IMAGE_ALIAS_CANDIDATE",
            "dev->aliases.emplace(image, alias)",
            "sc->aliases.insert(alias)",
        ),
        errors,
    )
    alias_bind = function(layer, "helios_BindImageMemory2")
    require_order(
        LAYER,
        "mixed alias bind",
        alias_bind,
        (
            "VUID-VkBindImageMemoryInfo-memory-01630",
            "VUID-VkBindImageMemoryInfo-memoryOffset-01631",
            "VUID-VkBindImageMemorySwapchainInfoKHR-imageIndex-01644",
            "helios_allocate_alias_memory",
            "SetHeliosPresentableImage",
            "dev->disp.BindImageMemory2(device, bindInfoCount, copies.data())",
            "entry.alias->memory = entry.memory",
            "HELIOS_ALIAS_INDETERMINATE",
            "alias_refs++",
        ),
        errors,
    )
    require(
        LAYER,
        "bind status and null-form preservation",
        alias_bind,
        ("VkBindMemoryStatus", "VK_ERROR_UNKNOWN", "entries[i].null_form", "copies[i].pNext = chains[i].head"),
        errors,
    )
    alias_destroy = function(layer, "helios_DestroyImage")
    require_order(
        LAYER,
        "alias reverse lifetime",
        alias_destroy,
        (
            "dev->aliases.erase(it)",
            "dev->disp.DestroyImage(device, image, pAllocator)",
            "dev->disp.FreeMemory(device, alias->memory, nullptr)",
            "sc->aliases.erase(alias)",
            "helios_release_retained_swapchain(sc)",
        ),
        errors,
    )

    # B8: new-before-old retirement, true D3D/DXGI drain, reciprocal ownership,
    # retained backing and reverse teardown.
    create_swapchain = function(layer, "helios_CreateSwapchainKHR")
    require_order(
        LAYER,
        "oldSwapchain atomic retirement",
        create_swapchain,
        (
            "dev->swapchains.emplace(sc->handle, sc)",
            "dev->retained_backings.emplace(sc->id, sc)",
            "dev->canonical_images.insert(slot.image)",
            "old_sc->retired = true",
            "*pSwapchain = helios_swapchain_handle(sc)",
        ),
        errors,
    )
    drain_d3d = function(layer, "helios_drain_d3d_queue")
    require_order(
        LAYER,
        "DXGI queue drain",
        drain_d3d,
        (
            "CreateFence(0, D3D12_FENCE_FLAG_NONE",
            "sc->queue->Signal(drain, 1)",
            "drain->SetEventOnCompletion(1, event)",
            "WaitForSingleObject(event, INFINITE)",
        ),
        errors,
    )
    drain = function(layer, "helios_drain_swapchain")
    require_order(
        LAYER,
        "final exact ownership return",
        drain,
        (
            "helios_wait_release(slot)",
            "helios_drain_d3d_queue(sc)",
            "slot.epoch == 0",
            "VK_QUEUE_FAMILY_EXTERNAL",
            "dev->canonical_family",
            "waits[count].semaphore = slot.release_sem",
            "waits[count].value = slot.epoch",
            "dev->disp.QueueSubmit2(dev->helper_queue",
            "dev->disp.WaitForFences",
            "restored[i]->externally_owned = false",
        ),
        errors,
    )
    forbid(LAYER, "no synthetic Release during teardown", drain, ("Signal(slot.release", "slot.epoch = target"), errors)
    destroy_swapchain = function(layer, "helios_destroy_swapchain_locked")
    require_order(
        LAYER,
        "presentation-before-backing teardown",
        destroy_swapchain,
        (
            "helios_drain_swapchain(sc)",
            "helios_slot_teardown_presentation(dev, slot)",
            "HELIOS_ALIAS_ONLY",
            "retained = !sc->aliases.empty()",
            "if (!retained)",
            "helios_slot_teardown_backing(slot)",
        ),
        errors,
    )
    require(
        LAYER,
        "device-loss refusal",
        layer,
        ("completed == UINT64_MAX", "VK_ERROR_DEVICE_LOST", "HELIOS_ALIAS_LOST"),
        errors,
    )

    # B9: one separate artifact, exact exports/manifest, no lower-ICD linkage.
    expected_layer_exports = (
        "vkNegotiateLoaderLayerInterfaceVersion",
        "vkGetInstanceProcAddr",
        "vkGetDeviceProcAddr",
        "vk_layerGetPhysicalDeviceProcAddr",
        "vkEnumerateInstanceLayerProperties",
        "vkEnumerateInstanceExtensionProperties",
        "vkEnumerateDeviceLayerProperties",
        "vkEnumerateDeviceExtensionProperties",
    )
    if def_exports(s[LAYER_DEF]) != expected_layer_exports:
        errors.append(f"{LAYER_DEF}: layer export list is not the exact eight-name loader ABI")
    expected_lower_exports = (
        "vk_icdNegotiateLoaderICDInterfaceVersion",
        "vk_icdGetInstanceProcAddr",
        "vk_icdGetPhysicalDeviceProcAddr",
        "helios_icd_create_translator_v1",
    )
    if def_exports("EXPORTS\n" + s[LOWER_DEF]) != expected_lower_exports:
        errors.append(f"{LOWER_DEF}: lower ICD export list is not the exact four-name ABI")
    require(
        LAYER_MESON,
        "separate layer target",
        s[LAYER_MESON],
        (
            "shared_library(\n  'VkLayer_HELIOS_present'",
            "vs_module_defs : 'helios_present_layer.def'",
            "cpp.find_library('d3d12')",
            "cpp.find_library('dxgi')",
            "cpp.find_library('dxguid')",
            "configure_file(",
            "VkLayer_HELIOS_present.dll",
        ),
        errors,
    )
    require(
        VULKAN_MESON,
        "Windows-only layer selection",
        s[VULKAN_MESON],
        ("if with_vulkan_helios_present_layer", "error('vulkan-layers=helios-present requires platforms=windows')", "subdir('helios-present-layer')"),
        errors,
    )
    forbid(WSI_MESON, "layer absent from libvulkan_wsi", s[WSI_MESON], ("helios-present-layer", "helios_present_layer.cpp"), errors)
    forbid(VIRTIO_MESON, "layer absent from lower ICD", s[VIRTIO_MESON], ("helios-present-layer", "helios_present_layer.cpp", "d3d12", "dxgi", "vulkan-1"), errors)
    require(
        MANIFEST,
        "implicit layer manifest",
        s[MANIFEST],
        ('"name": "VK_LAYER_HELIOS_present"', '"library_path": "@library_path@"', '"api_version": "1.3.0"', '"disable_environment"', '"DISABLE_LAYER_HELIOS_PRESENT": "1"'),
        errors,
    )

    gate_line = 'python3 "$REPO/tools/mesa-present-layer-gate.py" "$REPO" --mutations'
    if s[RETIREMENT].count(gate_line) != 1:
        errors.append(f"{RETIREMENT}: present-layer gate must be integrated exactly once")
    return errors


@dataclass(frozen=True)
class Mutation:
    name: str
    path: str
    old: str
    new: str


def mutation_cases() -> tuple[Mutation, ...]:
    return (
        Mutation("restore HPS2 WSI source", WSI_MESON, "files_vulkan_wsi += files('wsi_common_win32_failclosed.c')", "files_vulkan_wsi += files('wsi_helios_present_sync.c')"),
        Mutation("link full Windows WSI into lower ICD", VIRTIO_MESON, "vn_deps += idep_vulkan_wsi_headers", "vn_deps += idep_vulkan_wsi"),
        Mutation("skip entry manifest check", LAYER, "if (!helios_verify_entry_manifest())", "if (false)"),
        Mutation("drop loader create-chain copy", LAYER, "case VK_STRUCTURE_TYPE_LOADER_DEVICE_CREATE_INFO:", "case VK_STRUCTURE_TYPE_MAX_ENUM:"),
        Mutation("add loader fallback", LAYER, "static inline bool\nstreq", "/* GetProcAddress fallback */\nstatic inline bool\nstreq"),
        Mutation("accept missing LUID", LAYER, "if (!id.deviceLUIDValid)", "if (false)"),
        Mutation("reverse fence direction", LAYER, "dev->disp.GetSemaphoreWin32HandleKHR", "dev->disp.ImportSemaphoreWin32HandleKHR"),
        Mutation("leak exported fence handle", LAYER, "CloseHandle(h);\n   h = nullptr;", "h = nullptr;"),
        Mutation("omit canonical tag", LAYER, "dev->disp.SetHeliosPresentableImage(dev->device, slot.image, sc->id, index)", "VK_SUCCESS"),
        Mutation("accept epoch wrap", LAYER, "if (slot.epoch == UINT64_MAX)", "if (false)"),
        Mutation("add acquire sleep loop", LAYER, "for (;;) {\n      uint32_t chosen", "for (;;) {\n      Sleep(1);\n      uint32_t chosen"),
        Mutation("drop FIFO Present contract", LAYER, "sc->dxgi->Present(1, 0)", "sc->dxgi->Present(0, 0)"),
        Mutation("skip failure pResults", LAYER, "write_present_results();\n      return helios_refuse", "return helios_refuse"),
        Mutation("restore process-global swapchains", LAYER, "static std::unordered_map<void *, HeliosDevice *> helios_devices;", "static std::unordered_map<void *, HeliosDevice *> helios_devices;\nstatic std::unordered_set<HeliosSwapchain *> helios_swapchains;"),
        Mutation("use object address as surface handle", LAYER, "return (VkSurfaceKHR)(uintptr_t)s->id;", "return (VkSurfaceKHR)(uintptr_t)s;"),
        Mutation("drop alias VUID 01631", LAYER, "VUID-VkBindImageMemoryInfo-memoryOffset-01631", "VUID-removed"),
        Mutation("skip dedicated alias import", LAYER, "VkResult result = helios_allocate_alias_memory(dev, &entries[i]);", "VkResult result = VK_SUCCESS;"),
        Mutation("free possibly bound alias memory", LAYER, "entry.alias->memory = entry.memory;", "dev->disp.FreeMemory(device, entry.memory, nullptr);"),
        Mutation("do not retire old swapchain", LAYER, "old_sc->retired = true;", "old_sc->retired = false;"),
        Mutation("share teardown drain fence", LAYER, "CreateFence(0, D3D12_FENCE_FLAG_NONE", "CreateFence(0, D3D12_FENCE_FLAG_SHARED"),
        Mutation("restore to foreign owner", LAYER, "VK_QUEUE_FAMILY_EXTERNAL,\n                                dev->canonical_family", "VK_QUEUE_FAMILY_EXTERNAL,\n                                VK_QUEUE_FAMILY_FOREIGN_EXT"),
        Mutation("release backing with live aliases", LAYER, "if (!retained) {", "if (true) {"),
        Mutation("link layer into generic WSI", WSI_MESON, "files_vulkan_wsi = files('wsi_common.c')", "files_vulkan_wsi = files('wsi_common.c', '../helios-present-layer/helios_present_layer.cpp')"),
        Mutation("widen layer export ABI", LAYER_DEF, "    vkEnumerateDeviceExtensionProperties", "    vkEnumerateDeviceExtensionProperties\n    vkCreateSwapchainKHR"),
        Mutation("widen lower ICD export ABI", LOWER_DEF, "helios_icd_create_translator_v1", "helios_icd_create_translator_v1\nvkCreateSwapchainKHR"),
        Mutation("drop dxguid linkage", LAYER_MESON, "    cpp.find_library('dxguid'),", ""),
        Mutation("rename manifest layer", MANIFEST, '"name": "VK_LAYER_HELIOS_present"', '"name": "VK_LAYER_HELIOS_fallback"'),
    )


def run_mutations(repo: str, sources: dict[str, str]) -> None:
    failures: list[str] = []
    for mutation in mutation_cases():
        if mutation.old not in sources[mutation.path]:
            failures.append(f"{mutation.name}: mutation anchor missing in {mutation.path}")
            continue
        changed = dict(sources)
        changed[mutation.path] = changed[mutation.path].replace(mutation.old, mutation.new, 1)
        if not check_sources(repo, changed):
            failures.append(f"{mutation.name}: mutation was not rejected")
    if failures:
        raise SystemExit("Present-layer mutation gate failed:\n" + "\n".join(failures))
    print(f"OK: {len(mutation_cases())} in-memory present-layer mutations rejected")


def main() -> None:
    repo = os.path.abspath(sys.argv[1]) if len(sys.argv) > 1 and not sys.argv[1].startswith("--") else REPO_DEFAULT
    sources = load_sources(repo)
    errors = check_sources(repo, sources)
    if errors:
        raise SystemExit("Mesa present-layer gate violated:\n" + "\n".join(errors))
    if "--mutations" in sys.argv[1:]:
        run_mutations(repo, sources)
    print("OK: Mesa present layer B0-B9 is source-closed and artifact-separated")


if __name__ == "__main__":
    main()
