#!/usr/bin/env python3
"""Mesa A7 exact-allocation classifier and mutation gate."""

from __future__ import annotations

import os
import re
import sys
from dataclasses import dataclass


REPO_DEFAULT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
RECORD = "icd/mesa/src/virtio/vulkan/vn_helios_record_submit.c"
MEMORY = "icd/mesa/src/virtio/vulkan/vn_device_memory.c"
COMMAND = "icd/mesa/src/virtio/vulkan/vn_command_buffer.c"
DESCRIPTOR = "icd/mesa/src/virtio/vulkan/vn_descriptor_set.c"
BUFFER = "icd/mesa/src/virtio/vulkan/vn_buffer.c"
IMAGE = "icd/mesa/src/virtio/vulkan/vn_image.c"
DEVICE = "icd/mesa/src/virtio/vulkan/vn_device.c"
PHYSICAL = "icd/mesa/src/virtio/vulkan/vn_physical_device.c"
DIRECT = "icd/mesa/src/virtio/vulkan/vn_helios_direct_dispatch.c"
RING = "icd/mesa/src/virtio/vulkan/vn_ring.c"
PRIVATE_WSI = "icd/mesa/src/vulkan/helios_private_wsi.h"
RETIREMENT = "tools/retirement-gates.sh"

SOURCES = (
    RECORD,
    MEMORY,
    COMMAND,
    DESCRIPTOR,
    BUFFER,
    IMAGE,
    DEVICE,
    PHYSICAL,
    DIRECT,
    RING,
    PRIVATE_WSI,
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
            errors.append(f"{path}:{label}: missing A7 fragment: {fragment}")


def require_order(path: str, label: str, source: str, tokens: tuple[str, ...], errors: list[str]) -> None:
    value = compact(source)
    cursor = 0
    for token in tokens:
        wanted = compact(token)
        found = value.find(wanted, cursor)
        if found < 0:
            errors.append(f"{path}:{label}: A7 order drifted at {token}")
            return
        cursor = found + len(wanted)


def check_sources(s: dict[str, str]) -> list[str]:
    errors: list[str] = []

    reserve = function(s[MEMORY], "vn_device_memory_reserve_outer_association")
    require(
        MEMORY,
        "immutable association ingress",
        reserve,
        (
            "association_count != 1",
            "VN_HELIOS_SUBMISSION_MODE_RECORD_ONLY",
            "association->s_type != HELIOS_RESOURCE_ASSOCIATION_STRUCTURE_TYPE",
            "association->struct_bytes != HELIOS_RESOURCE_ASSOCIATION_BYTES",
            "association->abi_version != HELIOS_RESOURCE_ASSOCIATION_ABI_VERSION",
            "association->package_generation != HELIOS_PACKAGE_GENERATION",
            "association->device_generation != expected_generation",
            "!association->outer_allocation_token",
            "!association->outer_allocation_bytes",
            "alloc_info->allocationSize > association->outer_allocation_bytes",
            "~HELIOS_RESOURCE_ASSOCIATION_FLAG_MASK",
            "association->reserved1",
            "mem->helios_outer = *association",
            "mem->helios_outer.p_next = NULL",
            "VN_HELIOS_MAX_OUTER_ALLOCATIONS",
            "live->helios_outer.outer_allocation_token == association->outer_allocation_token",
            "list_addtail(&mem->helios_outer_link, &dev->helios_outer_allocations)",
        ),
        errors,
    )
    release = function(s[MEMORY], "vn_device_memory_release_outer_association")
    require_order(
        MEMORY,
        "reverse association teardown",
        release,
        (
            "record->reserved_context_generation",
            "record->reserved_batch_id",
            "list_delinit(&mem->helios_outer_link)",
            "dev->helios_outer_allocation_count--",
            "mem->helios_outer_registered = false",
            "memset(&mem->helios_outer, 0",
        ),
        errors,
    )
    binding = function(s[MEMORY], "vn_device_memory_helios_binding_live")
    require(
        MEMORY,
        "live binding validation",
        binding,
        (
            "!binding->device_generation",
            "!binding->outer_allocation_token",
            "!binding->byte_length",
            "end < binding->byte_offset",
            "end > binding->outer_allocation_bytes",
            "&dev->helios_outer_allocations",
            "live->helios_outer.device_generation == binding->device_generation",
            "live->helios_outer.outer_allocation_token == binding->outer_allocation_token",
        ),
        errors,
    )
    deferred_alloc = function(s[MEMORY], "vn_device_memory_defer_outer_allocate")
    require_order(
        MEMORY,
        "deferred exact allocate",
        deferred_alloc,
        (
            "!mem->helios_outer_registered",
            ".resourceId = 0",
            "vn_encode_vkAllocateMemory",
            "placeholder != 0",
            "vn_device_memory_helios_record_create",
            "vn_device_memory_helios_record_install",
        ),
        errors,
    )

    refusals = (
        "HELIOS_RECORD_REFUSE_CONTROL_CLASS",
        "HELIOS_RECORD_REFUSE_DEFERRED_USE",
        "HELIOS_RECORD_REFUSE_BATCH_BOUND",
        "HELIOS_RECORD_REFUSE_FOREIGN_HANDLE",
        "HELIOS_RECORD_REFUSE_WITHHELD_PROC",
        "HELIOS_RECORD_REFUSE_REENTRANT_JOIN",
    )
    require(RECORD, "named refusals", s[RECORD], refusals, errors)
    add_binding = function(s[RECORD], "helios_cmd_add_binding")
    require(
        RECORD,
        "command allocation classifier",
        add_binding,
        (
            "!binding->valid",
            "access_flags & ~HELIOS_HOB1_ACCESS_MASK",
            "!vn_device_memory_helios_binding_live(dev, binding)",
            "binding_end < binding->byte_offset",
            "binding_end > binding->outer_allocation_bytes",
            "HELIOS_HOB1_MAX_USE_RECORDS",
            ".binding = *binding",
        ),
        errors,
    )
    append = function(s[RECORD], "helios_record_append")
    require_order(
        RECORD,
        "immutable first exact batch",
        append,
        (
            "scope->context->owner != owner || scope->context->queue != queue",
            "if (scope->payload_bytes)",
            "command_use_count > HELIOS_HOB1_MAX_USE_RECORDS",
            "!cmd->builder.helios_closure_complete",
            "!vn_device_memory_helios_binding_live",
            "HeliosSealedResourceUseV1 sealed_use = {",
            ".outer_allocation_token = command_use->binding.outer_allocation_token",
            ".reserved0 = 0",
            ".reserved1 = 0",
            "record->reserved_context_generation",
            "record->reserved_batch_id",
            "if (zero != 0)",
            "HELIOS_HOB1_OPERAND_KIND_GENERATED_RESOURCE",
            ".reserved = 0",
            "assert(write_offset == total_payload)",
        ),
        errors,
    )
    if "res_id" in append or "hvm1" in append.lower():
        errors.append(f"{RECORD}: sealed A7 closure contains a local resource/HVM identity fallback")

    streams = function(s[RECORD], "helios_command_streams_add_recursive")
    require(
        RECORD,
        "complete recursive command streams",
        streams,
        (
            "depth == 64",
            "MESA_VK_COMMAND_BUFFER_STATE_EXECUTABLE",
            "!cmd->builder.helios_closure_complete",
            "vn_cs_encoder_get_fatal(&cmd->cs)",
            "vn_cs_encoder_get_len(&cmd->cs)",
            "cmd->builder.helios_secondaries[i]",
            "depth + 1",
            "streams->commands[streams->count++] = cmd",
        ),
        errors,
    )
    collect = function(s[RECORD], "helios_collect_command_buffer_uses")
    require_order(
        RECORD,
        "command use collection",
        collect,
        (
            "cmd->base.vk.pool->base.device != &dev->base.vk",
            "!cmd->builder.helios_closure_complete",
            "helios_command_streams_add_recursive",
            "vn_device_memory_helios_binding_live",
            "helios_submit_use_add",
        ),
        errors,
    )
    for name, collector, refusal in (
        ("vn_helios_queue_submit", "helios_submit1_collect_uses", "HELIOS_RECORD_REFUSE_QUEUE_SUBMIT"),
        ("vn_helios_queue_submit2", "helios_submit2_collect_uses", "HELIOS_RECORD_REFUSE_QUEUE_SUBMIT2"),
    ):
        submit = function(s[RECORD], name)
        require_order(
            RECORD,
            name,
            submit,
            (
                "VN_HELIOS_SUBMISSION_MODE_RECORD_ONLY",
                collector,
                "command_streams",
                "helios_encode_submit",
                "helios_record_append",
                refusal,
                "helios_scope_track_fence",
            ),
            errors,
        )
    for obsolete in ("helios_submit1_deferred_use_gate", "helios_submit2_deferred_use_gate"):
        if obsolete in s[RECORD]:
            errors.append(f"{RECORD}: obsolete command-buffer refusal remains live: {obsolete}")

    seal = function(s[RECORD], "vn_helios_record_scope_seal")
    require_order(
        RECORD,
        "sealed operand closure",
        seal,
        (
            "scope->payload_bytes == 0",
            "scope->uses[i].first_operand != next_operand",
            "helios_translator_check_sealed_use",
            "next_operand != scope->operand_count",
            "helios_translator_check_sealed_operand",
            "helios_translator_check_operand_payload_zero",
            "scope->is_sealed = true",
        ),
        errors,
    )
    close = function(s[RECORD], "vn_helios_record_scope_close")
    require_order(
        RECORD,
        "accepted scope close",
        close,
        (
            "HELIOS_TRANSLATOR_SCOPE_DISPOSITION_COMMITTED && !scope->is_sealed",
            "helios_scope_note_allocation_progress",
            "helios_scope_retire_deferred",
            "disposition == HELIOS_TRANSLATOR_SCOPE_DISPOSITION_COMMITTED",
            "context->active_scope = NULL",
        ),
        errors,
    )

    require(
        RING,
        "pure control through HVC1",
        function(s[RING], "vn_ring_submit_command_simple"),
        ("#if DETECT_OS_WINDOWS", "vn_renderer_helios_control_no_reply"),
        errors,
    )
    require(
        RECORD,
        "GPU-dependent exact joins",
        s[RECORD],
        ("vn_helios_direct_join_current", "vn_helios_direct_join_all"),
        errors,
    )

    command = s[COMMAND]
    minimums = {
        "HELIOS_TOUCH_BUFFER": 32,
        "HELIOS_TOUCH_IMAGE": 20,
        "HELIOS_REFUSE": 15,
        "vn_helios_cmd_touch_image_view": 10,
        "vn_helios_cmd_touch_descriptor_set": 2,
        "vn_helios_cmd_touch_device_address": 2,
        "vn_helios_cmd_merge_secondary": 1,
    }
    for token, minimum in minimums.items():
        if command.count(token) < minimum:
            errors.append(f"{COMMAND}: command classifier coverage for {token} fell below {minimum}")
    require(
        COMMAND,
        "representative typed command closure",
        command,
        (
            "vkCmdBindDescriptorSets_EXT",
            "vkCmdBeginRendering_EXT",
            "vkCmdPipelineBarrier2_EXT",
            "vkCmdExecuteCommands_EXT",
            "vkCmdBindResourceHeapEXT_EXT",
            "vkCmdBuildAccelerationStructuresKHR",
        ),
        errors,
    )

    tag = function(s[IMAGE], "vn_SetHeliosPresentableImageHELIOS")
    require(
        IMAGE,
        "exact presentable image tag",
        tag,
        (
            "img->base.vk.base.device != &dev->base.vk",
            "!swapchainId",
            "VK_SHARING_MODE_EXCLUSIVE",
            "VN_HELIOS_SUBMISSION_MODE_RECORD_ONLY",
            "VK_EXTERNAL_MEMORY_HANDLE_TYPE_D3D12_RESOURCE_BIT",
            "img->helios_presentable.tagged",
            "img->helios_presentable.swapchain_id != swapchainId",
        ),
        errors,
    )
    gdpa = function(s[DEVICE], "vn_GetDeviceProcAddr")
    require(
        DEVICE,
        "normal next-layer-only tag publication",
        gdpa,
        (
            "HELIOS_SET_PRESENTABLE_IMAGE_NAME",
            "VN_HELIOS_SUBMISSION_MODE_RECORD_ONLY ? NULL",
            "vn_SetHeliosPresentableImageHELIOS",
        ),
        errors,
    )
    require(
        DIRECT,
        "record-only tag withholding",
        function(s[DIRECT], "helios_direct_withheld_proc"),
        ("HELIOS_SET_PRESENTABLE_IMAGE_NAME", "strstr(name, \"Present\")"),
        errors,
    )
    barrier = function(s[COMMAND], "vn_cmd_fix_image_memory_barrier_common")
    require(
        COMMAND,
        "PRESENT_SRC external ownership validation",
        barrier,
        (
            "img && img->helios_presentable.tagged",
            "VK_SHARING_MODE_EXCLUSIVE",
            "*new_layout != VK_IMAGE_LAYOUT_GENERAL",
            "*dst_qfi != VK_QUEUE_FAMILY_EXTERNAL",
            "*old_layout != VK_IMAGE_LAYOUT_GENERAL",
            "*new_layout != VK_IMAGE_LAYOUT_PRESENT_SRC_KHR",
            "*src_qfi != VK_QUEUE_FAMILY_EXTERNAL",
            "if (record_only)",
            "result.valid = false",
        ),
        errors,
    )
    if "vkSetHeliosPresentableImageHELIOS" not in s[PRIVATE_WSI]:
        errors.append(f"{PRIVATE_WSI}: private presentable-image declaration missing")

    gate_line = 'python3 "$REPO/tools/mesa-a7-classifier-gate.py" "$REPO" --mutations'
    if s[RETIREMENT].count(gate_line) != 1:
        errors.append(f"{RETIREMENT}: Mesa A7 classifier gate must be integrated exactly once")
    return errors


@dataclass(frozen=True)
class Mutation:
    name: str
    path: str
    old: str
    new: str


def mutation_cases() -> tuple[Mutation, ...]:
    return (
        Mutation("accept zero outer token", MEMORY, "!association->outer_allocation_token ||", "false ||"),
        Mutation("accept stale generation", MEMORY, "association->device_generation != expected_generation ||", "false ||"),
        Mutation("retain caller pNext", MEMORY, "mem->helios_outer.p_next = NULL;", "mem->helios_outer.p_next = association->p_next;"),
        Mutation("skip reverse association removal", MEMORY, "list_delinit(&mem->helios_outer_link);", "list_inithead(&mem->helios_outer_link);"),
        Mutation("emit nonzero wire resource id", MEMORY, ".resourceId = 0,", ".resourceId = mem->base.id,"),
        Mutation("accept nonzero generated operand", MEMORY, "placeholder != 0)", "false)"),
        Mutation("skip command binding liveness", RECORD, "!vn_device_memory_helios_binding_live(dev, binding)", "false"),
        Mutation("use local memory identity", RECORD, ".outer_allocation_token =\n            command_use->binding.outer_allocation_token", ".outer_allocation_token =\n            command_use->binding.outer_allocation_bytes"),
        Mutation("accept incomplete command closure", RECORD, "!cmd->builder.helios_closure_complete ||\n       vn_cs_encoder_get_fatal(&cmd->cs)", "false ||\n       vn_cs_encoder_get_fatal(&cmd->cs)"),
        Mutation("omit recursive secondaries", RECORD, "cmd->builder.helios_secondaries[i], stack,\n         depth + 1", "cmd, stack,\n         depth + 1"),
        Mutation("restore submit1 refusal", RECORD, "result = helios_submit1_collect_uses(", "result = helios_submit1_refuse_uses("),
        Mutation("restore submit2 refusal", RECORD, "result = helios_submit2_collect_uses(", "result = helios_submit2_refuse_uses("),
        Mutation("skip wire-zero validation", RECORD, "helios_translator_check_operand_payload_zero(", "helios_translator_skip_operand_payload_zero("),
        Mutation("commit unsealed scope", RECORD, "HELIOS_TRANSLATOR_SCOPE_DISPOSITION_COMMITTED &&\n       !scope->is_sealed", "false &&\n       !scope->is_sealed"),
        Mutation("publish tag to record-only", DEVICE, "VN_HELIOS_SUBMISSION_MODE_RECORD_ONLY\n                ? NULL", "VN_HELIOS_SUBMISSION_MODE_NORMAL\n                ? NULL"),
        Mutation("tag arbitrary external image", IMAGE, "VK_EXTERNAL_MEMORY_HANDLE_TYPE_D3D12_RESOURCE_BIT", "VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_WIN32_BIT"),
        Mutation("accept foreign present owner", COMMAND, "*dst_qfi != VK_QUEUE_FAMILY_EXTERNAL", "false"),
    )


def run_mutations(sources: dict[str, str]) -> None:
    for case in mutation_cases():
        source = sources[case.path]
        count = source.count(case.old)
        if count != 1:
            raise SystemExit(f"A7 mutation setup failed for {case.name}: anchor count {count}")
        mutated = dict(sources)
        mutated[case.path] = source.replace(case.old, case.new, 1)
        if not check_sources(mutated):
            raise SystemExit(f"A7 mutation was accepted: {case.name}")
    print(f"OK: {len(mutation_cases())} in-memory Mesa A7 mutations rejected")


def main() -> None:
    repo = os.path.abspath(sys.argv[1]) if len(sys.argv) > 1 and not sys.argv[1].startswith("--") else REPO_DEFAULT
    sources = load_sources(repo)
    errors = check_sources(sources)
    if errors:
        raise SystemExit("Mesa A7 classifier gate violated:\n" + "\n".join(errors))
    if "--mutations" in sys.argv[1:]:
        run_mutations(sources)
    print("OK: Mesa A7 exact-token command closure, joins, and presentable tag are closed")


if __name__ == "__main__":
    main()
