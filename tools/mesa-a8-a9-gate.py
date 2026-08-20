#!/usr/bin/env python3
"""Mesa A8 Windows ring retirement and A9 lower-ICD wiring mutation gate."""

from __future__ import annotations

import os
import re
import sys
from dataclasses import dataclass


REPO_DEFAULT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
RING = "icd/mesa/src/virtio/vulkan/vn_ring.c"
COMMON = "icd/mesa/src/virtio/vulkan/vn_common.c"
INSTANCE = "icd/mesa/src/virtio/vulkan/vn_instance.c"
QUEUE = "icd/mesa/src/virtio/vulkan/vn_queue.c"
MEMORY = "icd/mesa/src/virtio/vulkan/vn_device_memory.c"
SESSION = "icd/mesa/src/virtio/vulkan/vn_helios_translation_session.c"
MESON = "icd/mesa/src/virtio/vulkan/meson.build"
EXPORTS = "icd/mesa/src/virtio/vulkan/vn_helios_exports.def"
DEVICE = "icd/mesa/src/virtio/vulkan/vn_device.c"
PHYSICAL = "icd/mesa/src/virtio/vulkan/vn_physical_device.c"
IMAGE = "icd/mesa/src/virtio/vulkan/vn_image.c"
RETIREMENT = "tools/retirement-gates.sh"

SOURCES = (
    RING,
    COMMON,
    INSTANCE,
    QUEUE,
    MEMORY,
    SESSION,
    MESON,
    EXPORTS,
    DEVICE,
    PHYSICAL,
    IMAGE,
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
    candidates = re.finditer(rf"\b{re.escape(name)}\s*\(", source)
    start = -1
    match_start = -1
    for candidate in candidates:
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
            start = brace
            match_start = candidate.start()
            break
    if start < 0:
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
                    return source[match_start : i + 1]
        i += 1
    return ""


def require(path: str, label: str, source: str, fragments: tuple[str, ...], errors: list[str]) -> None:
    value = compact(source)
    for fragment in fragments:
        if compact(fragment) not in value:
            errors.append(f"{path}:{label}: missing A8/A9 fragment: {fragment}")


def require_order(path: str, label: str, source: str, tokens: tuple[str, ...], errors: list[str]) -> None:
    value = compact(source)
    cursor = 0
    for token in tokens:
        wanted = compact(token)
        found = value.find(wanted, cursor)
        if found < 0:
            errors.append(f"{path}:{label}: A8/A9 order drifted at {token}")
            return
        cursor = found + len(wanted)


def eval_windows_condition(expr: str) -> bool | None:
    value = re.sub(r"\s+", "", expr)
    if value in ("DETECT_OS_WINDOWS", "defined(_WIN32)", "defined(_WIN64)"):
        return True
    if value in (
        "!DETECT_OS_WINDOWS",
        "!defined(_WIN32)",
        "!defined(_WIN64)",
        "defined(VN_USE_WSI_PLATFORM)",
        "defined(VK_USE_PLATFORM_ANDROID_KHR)",
    ):
        return False
    return None


def windows_reachable(source: str, offset: int) -> bool:
    active = True
    stack: list[tuple[bool, bool | None]] = []
    for line in source[:offset].splitlines():
        stripped = line.strip()
        match = re.match(r"#\s*if\s+(.+)$", stripped)
        if match:
            condition = eval_windows_condition(match.group(1))
            stack.append((active, condition))
            active = active and (condition is not False)
            continue
        match = re.match(r"#\s*ifdef\s+(.+)$", stripped)
        if match:
            name = match.group(1).strip()
            if name in ("_WIN32", "_WIN64"):
                condition = True
            elif name in ("VN_USE_WSI_PLATFORM", "VK_USE_PLATFORM_ANDROID_KHR"):
                condition = False
            else:
                condition = None
            stack.append((active, condition))
            active = active and (condition is not False)
            continue
        match = re.match(r"#\s*ifndef\s+(.+)$", stripped)
        if match:
            name = match.group(1).strip()
            if name in ("_WIN32", "_WIN64"):
                condition = False
            elif name in ("VN_USE_WSI_PLATFORM", "VK_USE_PLATFORM_ANDROID_KHR"):
                condition = True
            else:
                condition = None
            stack.append((active, condition))
            active = active and (condition is not False)
            continue
        if re.match(r"#\s*else\b", stripped) and stack:
            parent, condition = stack[-1]
            active = parent and (condition is not True)
            continue
        if re.match(r"#\s*elif\s+", stripped) and stack:
            parent, old_condition = stack[-1]
            expression = re.sub(r"^#\s*elif\s+", "", stripped)
            condition = eval_windows_condition(expression)
            stack[-1] = (parent, condition)
            active = parent and old_condition is not True and condition is not False
            continue
        if re.match(r"#\s*endif\b", stripped) and stack:
            parent, _ = stack.pop()
            active = parent
    return active


def require_not_windows_call(path: str, source: str, call: str, errors: list[str]) -> None:
    matches = list(re.finditer(rf"\b{re.escape(call)}\s*\(", source))
    if not matches:
        errors.append(f"{path}: generic non-Windows call disappeared entirely: {call}")
        return
    for match in matches:
        if windows_reachable(source, match.start()):
            line = source.count("\n", 0, match.start()) + 1
            errors.append(f"{path}:{line}: {call} is reachable in the Windows lower ICD")


def check_sources(s: dict[str, str]) -> list[str]:
    errors: list[str] = []
    ring = s[RING]

    require(
        RING,
        "Windows serialization facade",
        ring,
        (
            "#if !DETECT_OS_WINDOWS\n   struct vn_renderer_shmem *shmem",
            "#if !DETECT_OS_WINDOWS\n   /* size limit for cmd submission via ring shmem",
            "#if DETECT_OS_WINDOWS\n   /* A8: a Windows ring is only a small serialization facade",
            "mtx_init(&ring->mutex, mtx_plain)",
            "(void)layout",
            "(void)direct_order",
            "(void)is_tls_ring",
        ),
        errors,
    )
    for call in (
        "vn_encode_vkCreateRingMESA",
        "vn_encode_vkDestroyRingMESA",
        "vn_encode_vkNotifyRingMESA",
        "vn_encode_vkSubmitVirtqueueSeqnoMESA",
        "vn_async_vkWaitVirtqueueSeqnoMESA",
    ):
        require_not_windows_call(RING, ring, call, errors)
    require_not_windows_call(QUEUE, s[QUEUE], "vn_encode_vkWaitRingSeqnoMESA", errors)
    require_not_windows_call(MEMORY, s[MEMORY], "vn_encode_vkWaitRingSeqnoMESA", errors)

    init_ring = function(s[INSTANCE], "vn_instance_init_ring")
    require(
        INSTANCE,
        "no Windows generic ring layout",
        init_ring,
        (
            "#if DETECT_OS_WINDOWS",
            "vn_ring_create(instance, NULL, 0, false",
            "#else",
            "vn_ring_get_layout(buf_size, extra_size, &layout)",
        ),
        errors,
    )
    tls_ring = function(s[COMMON], "vn_tls_get_ring")
    require_order(
        COMMON,
        "no Windows TLS ring",
        tls_ring,
        ("#if DETECT_OS_WINDOWS", "return instance->ring.ring", "#else", "vn_ring_create"),
        errors,
    )
    for name, fragments in (
        ("vn_ring_get_seqno_status", ("#if DETECT_OS_WINDOWS", "return seqno == 0")),
        ("vn_ring_wait_seqno", ("#if DETECT_OS_WINDOWS", "return seqno == 0")),
        ("vn_ring_wait_all", ("#if DETECT_OS_WINDOWS", "(void)ring", "return")),
        ("vn_ring_current_seqno", ("#if DETECT_OS_WINDOWS", "return 0")),
        ("vn_ring_submit_roundtrip", ("#if DETECT_OS_WINDOWS", "*roundtrip_seqno = 0", "return VK_SUCCESS")),
        ("vn_ring_wait_roundtrip", ("#if DETECT_OS_WINDOWS", "assert(roundtrip_seqno == 0)")),
    ):
        require(RING, name, function(ring, name), fragments, errors)

    submit = function(ring, "vn_ring_submit_command")
    require_order(
        RING,
        "finite generated reply transaction",
        submit,
        (
            "#if DETECT_OS_WINDOWS",
            ".resourceId = 0",
            ".offset = 0",
            ".size = submit->reply_size",
            "prefix_size == 36",
            "HELIOS_HVR1_MAX_CHUNK_BYTES",
            "HELIOS_HNR2_MAX_PAYLOAD_BYTES - prefix_size",
            "vn_encode_vkSetReplyCommandStreamMESA",
            "memcpy(payload + prefix_size, submit->buffer.base",
            "vn_renderer_helios_control_generated",
            "submit->ring_seqno = 0",
            "submit->ring_seqno_valid = true",
        ),
        errors,
    )
    require(
        RING,
        "finite no-reply control transaction",
        function(ring, "vn_ring_submit_command_simple"),
        ("#if DETECT_OS_WINDOWS", "vn_renderer_helios_control_no_reply"),
        errors,
    )

    transact = function(s[SESSION], "helios_transact")
    require_order(
        SESSION,
        "SetReply and GENERATE_REPLY inside HNR2 COMMIT",
        transact,
        (
            "if (patch_generated_reply)",
            "resource_id != 0",
            ".payload_offset = 16",
            "HELIOS_HNR2_OPERAND_KIND_HOST_RESOURCE_ID32",
            "batch.patches = patch_generated_reply ? &slot_patch : NULL",
            "batch.has_reply = true",
            "helios_native_context_submit(s->control, &batch, true",
            "helios_native_context_wait(s->control, progress",
        ),
        errors,
    )
    generated = function(s[SESSION], "helios_session_control_generated")
    require(
        SESSION,
        "exact generated reply",
        generated,
        (
            "helios_transact",
            "&more, true",
            "more || total_bytes != reply_capacity",
            "*reply_bytes != reply_capacity",
        ),
        errors,
    )

    meson = s[MESON]
    for filename in (
        "vn_renderer_helios_hvm.c",
        "vn_helios_hwa2.c",
        "vn_helios_translation_session.c",
        "vn_helios_native_kmt.c",
        "vn_helios_record_submit.c",
        "vn_helios_direct_dispatch.c",
    ):
        if meson.count(f"files('{filename}')") != 1:
            errors.append(f"{MESON}: {filename} must be wired exactly once")
    require_order(
        MESON,
        "Windows header-only WSI dependency",
        meson,
        (
            "if with_platform_windows",
            "vn_deps += idep_vulkan_wsi_headers",
            "else",
            "vn_deps += idep_vulkan_wsi",
            "endif",
        ),
        errors,
    )
    generic_wsi_condition = compact(
        "if with_platform_wayland or with_platform_x11 or \\\n   (system_has_kms_drm and not with_platform_android)"
    )
    if generic_wsi_condition not in compact(meson):
        errors.append(f"{MESON}: generic vn_wsi.c condition is not non-Windows-only")
    if re.search(r"with_platform_windows[^\n]*\n[^\n]*vn_wsi\.c", meson):
        errors.append(f"{MESON}: Windows still compiles vn_wsi.c")
    libraries = {name.lower() for name in re.findall(r"find_library\(['\"]([^'\"]+)", meson)}
    forbidden_libraries = libraries & {"vulkan-1", "dxgi", "d3d11", "d3d12", "dcomp"}
    if forbidden_libraries:
        errors.append(f"{MESON}: lower ICD links forbidden libraries: {sorted(forbidden_libraries)}")
    if "helios_present_layer" in meson or "VK_LAYER_HELIOS_present" in meson:
        errors.append(f"{MESON}: A9 wired the future present layer into the lower ICD")

    for path, call in (
        (INSTANCE, "wsi_instance_entrypoints"),
        (DEVICE, "wsi_device_entrypoints"),
        (PHYSICAL, "wsi_physical_device_entrypoints"),
    ):
        match = re.search(rf"\b{call}\b", s[path])
        if not match or windows_reachable(s[path], match.start()):
            errors.append(f"{path}: {call} is not compile-time absent on Windows")
    for call in ("wsi_common_create_swapchain_image", "wsi_common_get_memory"):
        match = re.search(rf"\b{call}\s*\(", s[IMAGE])
        if not match or windows_reachable(s[IMAGE], match.start()):
            errors.append(f"{IMAGE}: {call} remains reachable in the Windows lower ICD")

    exports = [
        line.strip()
        for line in s[EXPORTS].splitlines()
        if line.strip() and line.strip().upper() != "EXPORTS"
    ]
    expected_exports = {
        "helios_icd_create_translator_v1",
        "vk_icdGetInstanceProcAddr",
        "vk_icdGetPhysicalDeviceProcAddr",
        "vk_icdNegotiateLoaderICDInterfaceVersion",
    }
    if len(exports) != 4 or set(exports) != expected_exports:
        errors.append(f"{EXPORTS}: lower ICD export boundary is not the exact four-symbol set")

    gate_line = 'python3 "$REPO/tools/mesa-a8-a9-gate.py" "$REPO" --mutations'
    if s[RETIREMENT].count(gate_line) != 1:
        errors.append(f"{RETIREMENT}: Mesa A8/A9 gate must be integrated exactly once")
    return errors


@dataclass(frozen=True)
class Mutation:
    name: str
    path: str
    old: str
    new: str


def mutation_cases() -> tuple[Mutation, ...]:
    return (
        Mutation("restore Windows shared storage", RING, "#if !DETECT_OS_WINDOWS\n   struct vn_renderer_shmem *shmem;", "#if DETECT_OS_WINDOWS\n   struct vn_renderer_shmem *shmem;"),
        Mutation("restore Windows CreateRing", RING, "#if DETECT_OS_WINDOWS\n   /* A8: a Windows ring is only", "#if !DETECT_OS_WINDOWS\n   /* A8: a Windows ring is only"),
        Mutation("create generic Windows layout", INSTANCE, "vn_ring_create(instance, NULL, 0, false", "vn_ring_create(instance, &layout, 4, false"),
        Mutation("create Windows TLS ring", COMMON, "return instance->ring.ring;\n#else", "return vn_ring_create(instance, NULL, 0, true);\n#else"),
        Mutation("nonzero reply resource id", RING, ".resourceId = 0,", ".resourceId = 1,"),
        Mutation("weaken reply prefix size", RING, "if (prefix_size == 36 &&", "if (prefix_size &&"),
        Mutation("restore independent roundtrip", RING, "*roundtrip_seqno = 0;", "*roundtrip_seqno = 1;"),
        Mutation("restore queue ring wait", QUEUE, "#if !DETECT_OS_WINDOWS\n   uint32_t local_data[8];", "#if DETECT_OS_WINDOWS\n   uint32_t local_data[8];"),
        Mutation("restore memory ring wait", MEMORY, "#if !DETECT_OS_WINDOWS\n   struct vn_renderer_submit_batch local_batch;", "#if DETECT_OS_WINDOWS\n   struct vn_renderer_submit_batch local_batch;"),
        Mutation("drop generated reply patch", SESSION, "batch.patches = patch_generated_reply ? &slot_patch : NULL;", "batch.patches = NULL;"),
        Mutation("drop HNR2 reply declaration", SESSION, "batch.has_reply = true;", "batch.has_reply = false;"),
        Mutation("link full Windows WSI", MESON, "vn_deps += idep_vulkan_wsi_headers", "vn_deps += idep_vulkan_wsi"),
        Mutation("compile Windows vn_wsi", MESON, "(system_has_kms_drm and not with_platform_android)\n  libvn_files", "(system_has_kms_drm and not with_platform_android) or with_platform_windows\n  libvn_files"),
        Mutation("duplicate direct dispatch TU", MESON, "libvn_files += files('vn_helios_direct_dispatch.c')", "libvn_files += files('vn_helios_direct_dispatch.c', 'vn_helios_direct_dispatch.c')"),
        Mutation("add lower ICD export", EXPORTS, "helios_icd_create_translator_v1", "helios_icd_create_translator_v1\nvkCreateInstance"),
        Mutation("restore Windows WSI dispatch", DEVICE, "#ifdef VN_USE_WSI_PLATFORM\n   vk_device_dispatch_table_from_entrypoints", "#if 1\n   vk_device_dispatch_table_from_entrypoints"),
    )


def run_mutations(sources: dict[str, str]) -> None:
    for case in mutation_cases():
        source = sources[case.path]
        count = source.count(case.old)
        if count != 1:
            raise SystemExit(f"A8/A9 mutation setup failed for {case.name}: anchor count {count}")
        mutated = dict(sources)
        mutated[case.path] = source.replace(case.old, case.new, 1)
        if not check_sources(mutated):
            raise SystemExit(f"A8/A9 mutation was accepted: {case.name}")
    print(f"OK: {len(mutation_cases())} in-memory Mesa A8/A9 mutations rejected")


def main() -> None:
    repo = os.path.abspath(sys.argv[1]) if len(sys.argv) > 1 and not sys.argv[1].startswith("--") else REPO_DEFAULT
    sources = load_sources(repo)
    errors = check_sources(sources)
    if errors:
        raise SystemExit("Mesa A8/A9 gate violated:\n" + "\n".join(errors))
    if "--mutations" in sys.argv[1:]:
        run_mutations(sources)
    print("OK: Mesa A8 Windows generic ring is unreachable and A9 lower-ICD wiring is closed")


if __name__ == "__main__":
    main()
