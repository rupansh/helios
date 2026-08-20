#!/usr/bin/env python3
"""Mesa A5 private direct-dispatch source and in-memory mutation gate."""

from __future__ import annotations

import os
import re
import sys
from dataclasses import dataclass


REPO_DEFAULT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
DIRECT = "icd/mesa/src/virtio/vulkan/vn_helios_direct_dispatch.c"
DIRECT_H = "icd/mesa/src/virtio/vulkan/vn_helios_direct_dispatch.h"
INSTANCE = "icd/mesa/src/virtio/vulkan/vn_instance.c"
INSTANCE_H = "icd/mesa/src/virtio/vulkan/vn_instance.h"
RECORD = "icd/mesa/src/virtio/vulkan/vn_helios_record_submit.c"
MESON = "icd/mesa/src/virtio/vulkan/meson.build"
EXPORTS = "icd/mesa/src/virtio/vulkan/vn_helios_exports.def"
PROTOCOL = "protocol/include/helios_translator_dispatch.h"
RETIREMENT = "tools/retirement-gates.sh"

SOURCES = (
    DIRECT,
    DIRECT_H,
    INSTANCE,
    INSTANCE_H,
    RECORD,
    MESON,
    EXPORTS,
    PROTOCOL,
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
            errors.append(f"{path}:{name}: missing A5 fragment: {fragment}")


def require_order(path: str, name: str, source: str, tokens: tuple[str, ...], errors: list[str]) -> None:
    value = compact(source)
    cursor = 0
    for token in tokens:
        wanted = compact(token)
        found = value.find(wanted, cursor)
        if found < 0:
            errors.append(f"{path}:{name}: A5 order drifted at {token}")
            return
        cursor = found + len(wanted)


def check_sources(sources: dict[str, str]) -> list[str]:
    errors: list[str] = []
    direct = sources[DIRECT]
    protocol = sources[PROTOCOL]

    if sources[MESON].count("vn_helios_direct_dispatch.c") != 1:
        errors.append(f"{MESON}: A5 translation unit must be wired exactly once")
    if sources[MESON].count("vn_helios_exports.def") != 1:
        errors.append(f"{MESON}: A5 export definition must be wired exactly once")

    exports = [
        line.strip()
        for line in sources[EXPORTS].splitlines()
        if line.strip() and line.strip() != "EXPORTS" and not line.lstrip().startswith(";")
    ]
    expected_exports = [
        "vk_icdNegotiateLoaderICDInterfaceVersion",
        "vk_icdGetInstanceProcAddr",
        "vk_icdGetPhysicalDeviceProcAddr",
        "helios_icd_create_translator_v1",
    ]
    if exports != expected_exports:
        errors.append(f"{EXPORTS}: export surface is not the three loader exports plus the one A5 entry")

    require(
        PROTOCOL,
        "fixed direct ABI",
        protocol,
        (
            '#define HELIOS_ICD_CREATE_TRANSLATOR_V1_NAME "helios_icd_create_translator_v1"',
            "sizeof(HeliosTranslatorDispatchV1) == 112",
            "24-byte header + 11 slots",
            "(sizeof(HeliosTranslatorDispatchV1) - 24) / 8 == 11",
        ),
        errors,
    )

    require(
        DIRECT,
        "HeliosTranslatorInstance_T",
        direct,
        (
            "struct vn_instance *instance",
            "HeliosTranslatorHostCallbacksV1 host",
            "HeliosTranslatorDispatchV1 dispatch",
            "endpoints[HELIOS_HTS1_MAX_ENDPOINTS_PER_SESSION + 1u]",
            "struct helios_direct_context *contexts",
            "mtx_t lock",
            "tss_t join_key",
        ),
        errors,
    )
    if re.search(r"(?m)^\s*static\s+struct\s+HeliosTranslatorInstance_T\s+\w+\s*[;=]", direct):
        errors.append(f"{DIRECT}: direct translator ownership became process-global")

    from_handle = function(direct, "helios_direct_from_handle")
    require(
        DIRECT,
        "helios_direct_from_handle",
        from_handle,
        (
            "direct->instance->helios_direct != direct",
            "VN_HELIOS_SUBMISSION_MODE_RECORD_ONLY",
        ),
        errors,
    )
    from_instance = function(direct, "helios_direct_from_vk_instance")
    require(
        DIRECT,
        "helios_direct_from_vk_instance",
        from_instance,
        (
            "struct HeliosTranslatorInstance_T *direct = instance->helios_direct",
            "direct->instance != instance",
            "VN_HELIOS_SUBMISSION_MODE_RECORD_ONLY",
        ),
        errors,
    )

    module = function(direct, "helios_module_from_address")
    require(
        DIRECT,
        "helios_module_from_address",
        module,
        (
            "GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS",
            "GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT",
            "GetModuleHandleExW",
        ),
        errors,
    )
    get_proc = function(direct, "helios_direct_get_instance_proc_addr")
    require_order(
        DIRECT,
        "helios_direct_get_instance_proc_addr",
        get_proc,
        (
            "helios_direct_from_vk_instance",
            "helios_direct_withheld_proc",
            "vn_GetInstanceProcAddr",
            "helios_module_from_address",
            "module != direct->dispatch.icd_module_base",
            "vn_helios_record_note_loader_provenance",
        ),
        errors,
    )
    withheld = function(direct, "helios_direct_withheld_proc")
    require(
        DIRECT,
        "helios_direct_withheld_proc",
        withheld,
        (
            '"vkCreateInstance"',
            '"vkGetInstanceProcAddr"',
            "HELIOS_SET_PRESENTABLE_IMAGE_NAME",
            'strstr(name, "Surface")',
            'strstr(name, "Swapchain")',
            'strstr(name, "Present")',
            'strstr(name, "AcquireNextImage")',
        ),
        errors,
    )

    register = function(direct, "vn_helios_direct_register_queue")
    require(
        DIRECT,
        "vn_helios_direct_register_queue",
        register,
        (
            "direct->instance != instance",
            "const uint32_t endpoint_id = queue->ring_idx",
            "endpoint_id < 2",
            "endpoint_id > endpoint_capacity",
            "!endpoint->ever_registered",
            "!endpoint->live",
            "endpoint->ever_registered = true",
            "endpoint->live = true",
            "direct->endpoint_count++",
        ),
        errors,
    )
    enumerate_endpoints = function(direct, "helios_direct_enumerate_endpoints")
    require(
        DIRECT,
        "helios_direct_enumerate_endpoints",
        enumerate_endpoints,
        (
            "endpoint_bytes != HELIOS_TRANSLATOR_ENDPOINT_BYTES",
            "if (!endpoints)",
            "if (*endpoint_count < count)",
            "endpoints[written++] = direct->endpoints[i].desc",
        ),
        errors,
    )

    build = function(direct, "helios_direct_build_queue_attach")
    require_order(
        DIRECT,
        "helios_direct_build_queue_attach",
        build,
        (
            "helios_translator_check_queue_attach_request",
            "request->context_generation <= direct->last_context_generation",
            "!direct->endpoints[request->endpoint_id].live",
            "endpoint->desc.engine_class != request->engine_class",
            "vn_renderer_helios_build_queue_attach",
            "direct->last_context_generation = request->context_generation",
        ),
        errors,
    )

    create_internal = function(sources[INSTANCE], "vn_create_instance_internal")
    require_order(
        INSTANCE,
        "vn_create_instance_internal",
        create_internal,
        (
            "vn_instance_init_renderer(instance)",
            "vn_helios_submit_instance_init(instance)",
            "vn_helios_submit_instance_set_record_only(instance)",
            "vn_instance_init_ring(instance)",
        ),
        errors,
    )
    create_direct = function(sources[INSTANCE], "vn_helios_create_direct_instance")
    require(
        INSTANCE,
        "vn_helios_create_direct_instance",
        create_direct,
        (
            'pApplicationName = "Helios direct translator"',
            "vn_create_instance_internal(&create, NULL, out_instance, true",
        ),
        errors,
    )

    create = function(direct, "helios_icd_create_translator_v1")
    require_order(
        DIRECT,
        "helios_icd_create_translator_v1",
        create,
        (
            "helios_translator_check_create_info(create_info, HELIOS_PACKAGE_GENERATION)",
            "helios_translator_check_host_callbacks",
            "helios_direct_reset_output",
            "vn_helios_create_direct_instance",
            "direct->instance = instance",
            "direct->host = *create_info->host_callbacks",
            "helios_module_from_address((const void *)helios_icd_create_translator_v1)",
            ".package_generation = HELIOS_PACKAGE_GENERATION",
            ".get_instance_proc_addr = helios_direct_get_instance_proc_addr",
            ".enumerate_endpoints = helios_direct_enumerate_endpoints",
            ".build_queue_attach = helios_direct_build_queue_attach",
            ".destroy_instance = helios_direct_destroy_instance",
            "instance->helios_direct = direct",
            "out_instance->submission_mode = HELIOS_TRANSLATOR_SUBMISSION_MODE_RECORD_ONLY",
            "helios_translator_check_instance",
            "helios_translator_check_dispatch",
        ),
        errors,
    )
    destroy = function(direct, "helios_direct_destroy_instance")
    require_order(
        DIRECT,
        "helios_direct_destroy_instance",
        destroy,
        (
            "if (direct->context_count || direct->endpoint_count)",
            "direct->destroying = true",
            "instance->helios_direct = NULL",
            "direct->instance = NULL",
            "vn_DestroyInstance",
            "tss_delete",
            "mtx_destroy",
            "free(direct)",
        ),
        errors,
    )

    queue_init = function(sources[RECORD], "vn_helios_submit_queue_init")
    require(
        RECORD,
        "vn_helios_submit_queue_init",
        queue_init,
        (
            "if (mode == VN_HELIOS_SUBMISSION_MODE_RECORD_ONLY)",
            "vn_helios_direct_register_queue",
        ),
        errors,
    )
    queue_fini = function(sources[RECORD], "vn_helios_submit_queue_fini")
    require(RECORD, "vn_helios_submit_queue_fini", queue_fini, ("vn_helios_direct_unregister_queue",), errors)

    forbidden = (
        r"LoadLibrary(?:A|W)?\s*\(",
        r"GetProcAddress\s*\(",
        r"D3DKMTEscape\s*\(",
        r"DeviceIoControl\s*\(",
        r"GetCurrentProcessId\s*\(",
        r"GetEnvironmentVariable",
    )
    for pattern in forbidden:
        if re.search(pattern, direct):
            errors.append(f"{DIRECT}: direct path contains forbidden {pattern}")

    gate_line = 'python3 "$REPO/tools/mesa-a5-direct-gate.py" "$REPO" --mutations'
    if sources[RETIREMENT].count(gate_line) != 1:
        errors.append(f"{RETIREMENT}: Mesa A5 mutation gate must be integrated exactly once")
    return errors


@dataclass(frozen=True)
class Mutation:
    name: str
    path: str
    old: str
    new: str


def mutation_cases() -> tuple[Mutation, ...]:
    return (
        Mutation("drop direct TU", MESON, "libvn_files += files('vn_helios_direct_dispatch.c')", "libvn_files += files()"),
        Mutation("rename sole export", EXPORTS, "helios_icd_create_translator_v1", "helios_icd_create_translator_v0"),
        Mutation("weaken package generation", DIRECT, "helios_translator_check_create_info(create_info,\n                                          HELIOS_PACKAGE_GENERATION)", "helios_translator_check_create_info(create_info, 0)"),
        Mutation("skip host callbacks", DIRECT, "checked = helios_translator_check_host_callbacks(\n      create_info->host_callbacks, HELIOS_PACKAGE_GENERATION);", "checked = HELIOS_TRANSLATOR_STATUS_OK;"),
        Mutation("drop direct backpointer", DIRECT, "direct->instance->helios_direct != direct", "false"),
        Mutation("accept normal instance", DIRECT, "vn_helios_submit_instance_mode(direct->instance) !=\n          VN_HELIOS_SUBMISSION_MODE_RECORD_ONLY", "false"),
        Mutation("drop module from-address", DIRECT, "GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS |", "0 |"),
        Mutation("accept foreign module", DIRECT, "!module || (const void *)module != direct->dispatch.icd_module_base", "false"),
        Mutation("expose presentable tag", DIRECT, "!strcmp(name, HELIOS_SET_PRESENTABLE_IMAGE_NAME)", "false"),
        Mutation("reuse bootstrap endpoint", DIRECT, "queue->emulated || endpoint_id < 2 ||", "queue->emulated || endpoint_id < 1 ||"),
        Mutation("reuse endpoint", DIRECT, "!endpoint->ever_registered && !endpoint->live", "!endpoint->live"),
        Mutation("omit endpoint descriptor", DIRECT, "endpoints[written++] = direct->endpoints[i].desc;", "written++;"),
        Mutation("reuse context generation", DIRECT, "request->context_generation <= direct->last_context_generation ||", "false ||"),
        Mutation("skip exact HQA1", DIRECT, "vn_renderer_helios_build_queue_attach(", "helios_skip_hqa1("),
        Mutation("select normal direct mode", INSTANCE, "vn_helios_submit_instance_set_record_only(instance)", "VK_SUCCESS"),
        Mutation("publish normal mode", DIRECT, "out_instance->submission_mode = HELIOS_TRANSLATOR_SUBMISSION_MODE_RECORD_ONLY;", "out_instance->submission_mode = HELIOS_TRANSLATOR_SUBMISSION_MODE_NORMAL;"),
        Mutation("drop final dispatch validation", DIRECT, "checked = helios_translator_check_dispatch(\n         out_instance->dispatch, HELIOS_PACKAGE_GENERATION);", "checked = HELIOS_TRANSLATOR_STATUS_OK;"),
        Mutation("destroy live contexts", DIRECT, "if (direct->context_count || direct->endpoint_count)", "if (false)"),
        Mutation("skip queue registration", RECORD, "vn_helios_direct_register_queue(", "helios_skip_queue_registration("),
        Mutation("skip queue unregister", RECORD, "vn_helios_direct_unregister_queue(dev->instance, queue);", "(void)queue;"),
    )


def run_mutations(sources: dict[str, str]) -> None:
    for case in mutation_cases():
        source = sources[case.path]
        count = source.count(case.old)
        if count != 1:
            raise SystemExit(f"A5 mutation setup failed for {case.name}: anchor count {count}")
        mutated = dict(sources)
        mutated[case.path] = source.replace(case.old, case.new, 1)
        if not check_sources(mutated):
            raise SystemExit(f"A5 mutation was accepted: {case.name}")
    print(f"OK: {len(mutation_cases())} in-memory Mesa A5 mutations rejected")


def main() -> None:
    repo = os.path.abspath(sys.argv[1]) if len(sys.argv) > 1 and not sys.argv[1].startswith("--") else REPO_DEFAULT
    sources = load_sources(repo)
    errors = check_sources(sources)
    if errors:
        raise SystemExit("Mesa A5 direct-dispatch gate violated:\n" + "\n".join(errors))
    if "--mutations" in sys.argv[1:]:
        run_mutations(sources)
    print("OK: Mesa A5 is versioned, direct-owned, provenance-checked, and endpoint-exact")


if __name__ == "__main__":
    main()
