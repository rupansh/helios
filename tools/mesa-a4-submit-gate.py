#!/usr/bin/env python3
"""Mesa A4 record-only/HNR2 submission source and mutation gate."""

from __future__ import annotations

import os
import re
import sys
from dataclasses import dataclass


REPO_DEFAULT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
A4 = "icd/mesa/src/virtio/vulkan/vn_helios_record_submit.c"
A4_H = "icd/mesa/src/virtio/vulkan/vn_helios_record_submit.h"
NATIVE = "icd/mesa/src/virtio/vulkan/vn_helios_native_kmt.c"
NATIVE_H = "icd/mesa/src/virtio/vulkan/vn_helios_native_kmt.h"
INSTANCE = "icd/mesa/src/virtio/vulkan/vn_instance.c"
INSTANCE_H = "icd/mesa/src/virtio/vulkan/vn_instance.h"
DEVICE = "icd/mesa/src/virtio/vulkan/vn_device.c"
QUEUE = "icd/mesa/src/virtio/vulkan/vn_queue.c"
QUEUE_H = "icd/mesa/src/virtio/vulkan/vn_queue.h"
RENDERER_HVM = "icd/mesa/src/virtio/vulkan/vn_renderer_helios_hvm.c"
MESON = "icd/mesa/src/virtio/vulkan/meson.build"
ICD = "icd/mesa/src/virtio/vulkan/vn_icd.c"
RETIREMENT = "tools/retirement-gates.sh"

SOURCES = (A4, A4_H, NATIVE, NATIVE_H, INSTANCE, INSTANCE_H, DEVICE, QUEUE, QUEUE_H, RENDERER_HVM, MESON, ICD, RETIREMENT)


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
            errors.append(f"{path}:{name}: missing A4 fragment: {fragment}")


def require_order(path: str, name: str, source: str, tokens: tuple[str, ...], errors: list[str]) -> None:
    value = compact(source)
    cursor = 0
    for token in tokens:
        wanted = compact(token)
        found = value.find(wanted, cursor)
        if found < 0:
            errors.append(f"{path}:{name}: A4 order drifted at {token}")
            return
        cursor = found + len(wanted)


def check_sources(sources: dict[str, str]) -> list[str]:
    errors: list[str] = []
    a4 = sources[A4]
    if sources[MESON].count("vn_helios_record_submit.c") != 1:
        errors.append(f"{MESON}: A4 translation unit must be wired exactly once")
    if "HeliosIcdCreateTranslatorV1" in sources[ICD] or "helios_icd_create_translator_v1" in sources[ICD]:
        errors.append(f"{ICD}: A4 exposed A5's direct-dispatch entry point")

    owner_decl = compact(a4)
    require(
        INSTANCE_H,
        "vn_instance",
        sources[INSTANCE_H],
        ("struct vn_helios_submit_instance *helios_submit",),
        errors,
    )
    if re.search(r"(?m)^\s*static\s+struct\s+vn_helios_submit_instance\s+\w+\s*[;=]", a4):
        errors.append(f"{A4}: A4 mode/session owner became process-global")

    init = function(a4, "vn_helios_submit_instance_init")
    require(
        A4,
        "vn_helios_submit_instance_init",
        init,
        (
            "owner->instance = instance",
            "owner->mode = VN_HELIOS_SUBMISSION_MODE_NORMAL",
            "vn_renderer_helios_session_generation(instance->renderer)",
            "tss_create(&owner->scope_key, NULL)",
            "mtx_init(&owner->lock, mtx_plain)",
            "instance->helios_submit = owner",
        ),
        errors,
    )
    if re.search(r"getenv|os_get_option|GetEnvironmentVariable", init):
        errors.append(f"{A4}: submission mode is environment-selected")

    instance_create = function(sources[INSTANCE], "vn_create_instance_internal")
    require_order(
        INSTANCE,
        "vn_create_instance_internal",
        instance_create,
        ("vn_instance_init_renderer(instance)", "vn_helios_submit_instance_init(instance)", "vn_instance_init_ring(instance)"),
        errors,
    )
    instance_destroy = function(sources[INSTANCE], "vn_DestroyInstance")
    require_order(
        INSTANCE,
        "vn_DestroyInstance",
        instance_destroy,
        ("vn_instance_fini_ring(instance)", "vn_helios_submit_instance_fini(instance)", "vn_renderer_destroy(instance->renderer"),
        errors,
    )

    queue_init = function(a4, "vn_helios_submit_queue_init")
    require(
        A4,
        "vn_helios_submit_queue_init",
        queue_init,
        (
            "owner->live_queue_count++",
            "if (owner->queue_admission_failed)",
            "const enum vn_helios_submission_mode mode = owner->mode",
            "if (mode == VN_HELIOS_SUBMISSION_MODE_RECORD_ONLY)",
            "vn_helios_direct_register_queue",
            "return result",
            "queue->helios_native_context = shared_queue->helios_native_context",
            "queue->helios_native_context_owner = false",
            "helios_native_context_create(device, HELIOS_NATIVE_CONTEXT_QUEUE, queue_family, queue_index",
            "queue->helios_native_context_owner = true",
        ),
        errors,
    )
    require_order(
        A4,
        "vn_helios_submit_queue_init",
        queue_init,
        (
            "mtx_lock(&owner->lock)",
            "if (owner->queue_admission_failed)",
            "const enum vn_helios_submission_mode mode = owner->mode",
            "owner->live_queue_count++",
            "mtx_unlock(&owner->lock)",
            "if (mode == VN_HELIOS_SUBMISSION_MODE_RECORD_ONLY)",
        ),
        errors,
    )
    record_context = function(a4, "vn_helios_record_context_create")
    require(
        A4,
        "vn_helios_record_context_create",
        record_context,
        ("endpoint_id != queue->ring_idx",),
        errors,
    )
    acquire_ring = function(sources[INSTANCE_H], "vn_instance_acquire_ring_idx")
    require(
        INSTANCE_H,
        "vn_instance_acquire_ring_idx",
        acquire_ring,
        (
            "instance->helios_next_ring_idx",
            "next < instance->renderer->info.max_timeline_count",
            "instance->helios_next_ring_idx = next + 1",
        ),
        errors,
    )
    require(
        INSTANCE,
        "vn_create_instance_internal",
        instance_create,
        ("instance->helios_next_ring_idx = 2",),
        errors,
    )
    require(
        RENDERER_HVM,
        "helios_renderer_info_init",
        function(sources[RENDERER_HVM], "helios_renderer_info_init"),
        ("helios_session_endpoint_capacity(helios->session) + 1",),
        errors,
    )
    queue_fini = function(a4, "vn_helios_submit_queue_fini")
    require_order(
        A4,
        "vn_helios_submit_queue_fini",
        queue_fini,
        ("helios_native_context_destroy", "mtx_destroy(&queue->helios_record_mutex)", "owner->live_queue_count--"),
        errors,
    )

    open_scope = function(a4, "vn_helios_record_scope_open")
    require(
        A4,
        "vn_helios_record_scope_open",
        open_scope,
        (
            "owner->mode != VN_HELIOS_SUBMISSION_MODE_RECORD_ONLY",
            "context->next_batch_id == UINT64_MAX",
            "helios_current_scope(owner)",
            "context->active_scope",
            "scope->batch_id = ++context->next_batch_id",
            "scope->thread = thrd_current()",
            "tss_set(owner->scope_key, scope)",
        ),
        errors,
    )
    append = function(a4, "helios_record_append")
    require(
        A4,
        "helios_record_append",
        append,
        (
            "helios_current_scope(owner)",
            "helios_scope_on_calling_thread(scope)",
            "scope->context->owner != owner || scope->context->queue != queue",
            "if (scope->payload_bytes)",
            "HELIOS_TRANSLATOR_STATUS_BATCH_BOUND_EXCEEDED",
            "HELIOS_HOB1_HEADER_BYTES + total_payload",
            "if (assembled > HELIOS_HOB1_MAX_BYTES",
            "helios_scope_reserve_payload(scope, total_payload)",
            "memcpy(scope->payload + write_offset, record->payload",
            "memcpy(scope->payload + write_offset, buffer->base",
            "memcpy(scope->payload + write_offset, payload",
            "scope->payload_bytes = total_payload",
        ),
        errors,
    )
    seal = function(a4, "vn_helios_record_scope_seal")
    require(
        A4,
        "vn_helios_record_scope_seal",
        seal,
        (
            ".package_generation = HELIOS_PACKAGE_GENERATION",
            ".session_generation = scope->context->owner->session_generation",
            ".context_generation = scope->context->context_generation",
            ".batch_id = scope->batch_id",
            ".payload_bytes = scope->payload_bytes",
            ".payload_crc64 = helios_crc64(scope->payload, scope->payload_bytes)",
            ".endpoint_id = scope->context->endpoint_id",
            ".context_flags = scope->context->context_flags",
            ".use_count = scope->use_count",
            ".operand_count = scope->operand_count",
            "helios_translator_check_sealed_batch",
        ),
        errors,
    )
    copy = function(a4, "vn_helios_record_scope_copy")
    require(
        A4,
        "vn_helios_record_scope_copy",
        copy,
        ("helios_translator_check_sealed_batch_copy", "memcpy(destination->payload", "memcpy(destination->uses", "memcpy(destination->operands"),
        errors,
    )
    close = function(a4, "vn_helios_record_scope_close")
    require_order(
        A4,
        "vn_helios_record_scope_close",
        close,
        ("helios_translator_check_scope_close", "tss_set(owner->scope_key, NULL)", "context->active_scope = NULL", "free(scope)"),
        errors,
    )

    entry_gate = function(a4, "helios_record_entry_gate")
    require(
        A4,
        "helios_record_entry_gate",
        entry_gate,
        ("owner->mode != VN_HELIOS_SUBMISSION_MODE_RECORD_ONLY", "helios_current_scope(owner)", "helios_scope_on_calling_thread(scope)", "scope->context->queue != queue"),
        errors,
    )
    if "helios_submit1_deferred_use_gate" in a4 or \
       "helios_submit2_deferred_use_gate" in a4:
        errors.append(f"{A4}: the pre-A7 command-buffer refusal remains live")

    sparse = function(a4, "helios_sparse_add_memory")
    require(
        A4,
        "helios_sparse_add_memory",
        sparse,
        (
            "helios_object_owned(&mem->base.vk.base, dev)",
            "mem->base_bo->allocation_handle",
            "mem->base_bo->allocation_generation",
            "HELIOS_HNR2_MAX_USE_RECORDS",
            "HELIOS_HNR2_ACCESS_READ | HELIOS_HNR2_ACCESS_WRITE",
            ".expected_generation = mem->base_bo->allocation_generation",
        ),
        errors,
    )
    classify_semaphore = function(a4, "helios_classify_semaphore")
    require(
        A4,
        "helios_classify_semaphore",
        classify_semaphore,
        ("helios_object_owned(&sem->base.vk, dev)", "HELIOS_RECORD_REFUSE_FOREIGN_HANDLE"),
        errors,
    )
    validate_fence = function(a4, "helios_validate_fence")
    require(
        A4,
        "helios_validate_fence",
        validate_fence,
        ("helios_object_owned(&fence->base.vk, dev)", "HELIOS_RECORD_REFUSE_FOREIGN_HANDLE"),
        errors,
    )

    dispatch = function(a4, "helios_dispatch_payload")
    require(
        A4,
        "helios_dispatch_payload",
        dispatch,
        (
            "if (owner->mode == VN_HELIOS_SUBMISSION_MODE_RECORD_ONLY)",
            "if (allocation_count)",
            "HELIOS_RECORD_REFUSE_DEFERRED_USE",
            "helios_record_append",
            ".patches = NULL",
            ".patch_count = 0",
            "helios_native_context_submit_ordered",
        ),
        errors,
    )

    for fini_name in (
        "helios_submit1_copy_fini",
        "helios_submit2_copy_fini",
        "helios_sparse_copy_fini",
    ):
        require(A4, fini_name, function(a4, fini_name), ("if (copy->aux)",), errors)

    for public, refusal, sizeof_call in (
        ("vn_helios_queue_submit", "HELIOS_RECORD_REFUSE_QUEUE_SUBMIT", "vn_sizeof_vkQueueSubmit"),
        ("vn_helios_queue_submit2", "HELIOS_RECORD_REFUSE_QUEUE_SUBMIT2", "vn_sizeof_vkQueueSubmit2"),
    ):
        body = function(a4, public)
        require(
            A4,
            public,
            body,
            (
                "helios_record_entry_gate",
                refusal,
                sizeof_call,
                "bounded_payload",
                "HELIOS_HNR2_MAX_PAYLOAD_BYTES",
                "VN_HELIOS_SUBMISSION_MODE_RECORD_ONLY",
                "copy.waits.count || copy.signals.count",
                "HELIOS_RECORD_REFUSE_CONTROL_CLASS",
                "helios_dispatch_payload",
                "VN_HELIOS_SUBMISSION_MODE_NORMAL",
                "total_payload_bytes",
            ),
            errors,
        )
    require(
        A4,
        "vn_helios_queue_bind_sparse",
        function(a4, "vn_helios_queue_bind_sparse"),
        (
            "helios_record_entry_gate",
            "HELIOS_RECORD_REFUSE_QUEUE_BIND_SPARSE",
            "vn_sizeof_vkQueueBindSparse",
            "bounded_payload",
            "HELIOS_HNR2_MAX_PAYLOAD_BYTES",
            "VN_HELIOS_SUBMISSION_MODE_RECORD_ONLY",
            "HELIOS_RECORD_REFUSE_DEFERRED_USE",
            "VK_ERROR_FEATURE_NOT_PRESENT",
            "VN_HELIOS_SUBMISSION_MODE_NORMAL",
            "total_payload_bytes",
            "helios_dispatch_payload",
        ),
        errors,
    )

    native_submit = function(sources[NATIVE], "helios_native_context_submit_ordered")
    require(
        NATIVE,
        "helios_native_context_submit_ordered",
        native_submit,
        ("c->enqueued - c->completed >= HELIOS_HNR2_MAX_OUTSTANDING_SUBMISSIONS", "helios_native_wait_locked_release", "c->next_batch_token == UINT64_MAX", "helios_native_enqueue_fence_points_locked", "D3DKMTRender(&render)", "helios_native_signal_locked"),
        errors,
    )
    require_order(
        NATIVE,
        "helios_native_context_submit_ordered",
        native_submit,
        ("helios_native_resize_lists", "c->next_batch_token == UINT64_MAX", "helios_native_enqueue_fence_points_locked(c, waits", "D3DKMTRender(&render)", "helios_native_enqueue_fence_points_locked(c, signals", "helios_native_signal_locked"),
        errors,
    )
    native_signal = function(sources[NATIVE], "helios_native_signal_locked")
    require(
        NATIVE,
        "helios_native_signal_locked",
        native_signal,
        ("c->enqueued == UINT64_MAX",),
        errors,
    )
    native_destroy = function(sources[NATIVE], "helios_native_context_destroy")
    require_order(
        NATIVE,
        "helios_native_context_destroy",
        native_destroy,
        ("D3DKMTDestroyContext", "D3DKMTDestroySynchronizationObject"),
        errors,
    )
    native_destroy_compact = compact(native_destroy)
    if native_destroy_compact.count("D3DKMTDestroySynchronizationObject") != 1:
        errors.append(f"{NATIVE}: progress fence must be revoked exactly once after context drain")
    native_points = function(sources[NATIVE], "helios_native_enqueue_fence_points_locked")
    require(
        NATIVE,
        "helios_native_enqueue_fence_points_locked",
        native_points,
        ("op.hContext = c->context", "D3DKMTWaitForSynchronizationObjectFromGpu", "D3DKMTSignalSynchronizationObjectFromGpu"),
        errors,
    )
    if compact(native_points).count("op.hContext=c->context;") != 2:
        errors.append(f"{NATIVE}: native wait and signal must both name the submitting context")

    queue_submit = function(sources[QUEUE], "vn_QueueSubmit")
    queue_submit2 = function(sources[QUEUE], "vn_QueueSubmit2")
    queue_sparse = function(sources[QUEUE], "vn_QueueBindSparse")
    require(QUEUE, "vn_QueueSubmit", queue_submit, ("#if DETECT_OS_WINDOWS", "vn_helios_queue_submit(vn_queue", "#else", "vn_queue_submit"), errors)
    require(QUEUE, "vn_QueueSubmit2", queue_submit2, ("#if DETECT_OS_WINDOWS", "vn_helios_queue_submit2", "#else"), errors)
    require(QUEUE, "vn_QueueBindSparse", queue_sparse, ("#if DETECT_OS_WINDOWS", "vn_helios_queue_bind_sparse", "#else", "vn_queue_submission_prepare"), errors)
    require(QUEUE, "vn_QueueWaitIdle", function(sources[QUEUE], "vn_QueueWaitIdle"), ("vn_helios_queue_wait_idle",), errors)
    require(DEVICE, "vn_DeviceWaitIdle", function(sources[DEVICE], "vn_DeviceWaitIdle"), ("vn_helios_device_wait_idle",), errors)
    device_destroy = function(sources[DEVICE], "vn_DestroyDevice")
    require(DEVICE, "vn_DestroyDevice", device_destroy, ("for (uint32_t i = dev->queue_count; i > 0; i--)", "vn_queue_fini(&dev->queues[i - 1])"), errors)
    queue_init_all = function(sources[DEVICE], "vn_device_init_queues")
    require(
        DEVICE,
        "vn_device_init_queues",
        queue_init_all,
        ("for (uint32_t k = count; k > 0; k--)", "vn_queue_fini(&queues[k - 1])"),
        errors,
    )
    device_idle = function(a4, "vn_helios_device_wait_idle")
    require(
        A4,
        "vn_helios_device_wait_idle",
        device_idle,
        ("vn_device_from_vk(scope->context->queue->base.vk.base.device) != dev", "HELIOS_RECORD_REFUSE_FOREIGN_HANDLE"),
        errors,
    )
    queue_fini_device = function(sources[DEVICE], "vn_queue_fini")
    require_order(DEVICE, "vn_queue_fini", queue_fini_device, ("thrd_join", "vn_helios_submit_queue_fini", "vn_DestroyFence"), errors)
    if queue_fini_device.count("vn_helios_submit_queue_fini(queue)") != 1:
        errors.append(f"{DEVICE}: queue context teardown must occur exactly once after producer join")

    record_forbidden = (
        r"D3DKMTRender\s*\(",
        r"D3DKMTSubmitCommand",
        r"SubmitCommandToHwQueue",
        r"vn_ring_submit",
        r"vn_renderer_submit",
        r"Sleep\s*\(",
        r"WaitForSingleObject\s*\(",
    )
    for pattern in record_forbidden:
        if re.search(pattern, a4):
            errors.append(f"{A4}: record/mode dispatcher contains forbidden direct execution {pattern}")

    if compact(owner_decl).count("volatileLONG64refusals") != 1:
        errors.append(f"{A4}: refusal counters are not owned exactly once by the instance")

    gate_line = 'python3 "$REPO/tools/mesa-a4-submit-gate.py" "$REPO" --mutations'
    if sources[RETIREMENT].count(gate_line) != 1:
        errors.append(f"{RETIREMENT}: Mesa A4 mutation gate must be integrated exactly once")
    return errors


@dataclass(frozen=True)
class Mutation:
    name: str
    path: str
    old: str
    new: str


def mutation_cases() -> tuple[Mutation, ...]:
    return (
        Mutation("default record-only", A4, "owner->mode = VN_HELIOS_SUBMISSION_MODE_NORMAL;", "owner->mode = VN_HELIOS_SUBMISSION_MODE_RECORD_ONLY;"),
        Mutation("drop session generation", A4, "vn_renderer_helios_session_generation(instance->renderer);", "1;"),
        Mutation("remove TLS scope", A4, "tss_create(&owner->scope_key, NULL)", "false"),
        Mutation("create record queue context", A4, "if (mode == VN_HELIOS_SUBMISSION_MODE_RECORD_ONLY) {\n      if (queue_family >= dev->physical_device->queue_family_count)", "if (false) {\n      if (queue_family >= dev->physical_device->queue_family_count)"),
        Mutation("duplicate emulated context", A4, "queue->helios_native_context = shared_queue->helios_native_context;", "queue->helios_native_context = NULL;"),
        Mutation("retry ambiguous endpoint", A4, "if (owner->queue_admission_failed) {", "if (false) {"),
        Mutation("allow mismatched record endpoint", A4, "endpoint_id != queue->ring_idx", "false"),
        Mutation("reuse allocation bootstrap endpoint", INSTANCE, "instance->helios_next_ring_idx = 2;", "instance->helios_next_ring_idx = 1;"),
        Mutation("recycle Windows endpoint", INSTANCE_H, "instance->helios_next_ring_idx = next + 1;", "instance->helios_next_ring_idx = 1;"),
        Mutation("drop control-ring count", RENDERER_HVM, "helios_session_endpoint_capacity(helios->session) + 1;", "helios_session_endpoint_capacity(helios->session);"),
        Mutation("drop live-scope gate", A4, "if (!scope || !helios_scope_on_calling_thread(scope)) {\n      helios_record_refuse(owner, refusal);", "if (false) {\n      helios_record_refuse(owner, refusal);"),
        Mutation("allow foreign queue scope", A4, "if (!scope || !helios_scope_on_calling_thread(scope)) {\n      helios_record_refuse(owner, refusal);\n      return VK_ERROR_VALIDATION_FAILED_EXT;\n   }\n   if (scope->context->owner != owner || scope->context->queue != queue)", "if (!scope || !helios_scope_on_calling_thread(scope)) {\n      helios_record_refuse(owner, refusal);\n      return VK_ERROR_VALIDATION_FAILED_EXT;\n   }\n   if (false)"),
        Mutation("allow second queue opcode", A4, "if (scope->payload_bytes) {", "if (false) {"),
        Mutation("unbound sealed bytes", A4, "assembled > HELIOS_HOB1_MAX_BYTES", "false"),
        Mutation("forge payload CRC", A4, "helios_crc64(scope->payload, scope->payload_bytes)", "0"),
        Mutation("forge session generation", A4, ".session_generation = scope->context->owner->session_generation,", ".session_generation = 1,"),
        Mutation("forge context generation", A4, ".context_generation = scope->context->context_generation,", ".context_generation = 1,"),
        Mutation("reuse batch ID", A4, "scope->batch_id = ++context->next_batch_id;", "scope->batch_id = 1;"),
        Mutation("wrap batch ID", A4, "if (context->next_batch_id == UINT64_MAX) {", "if (false) {"),
        Mutation("free TLS-live scope", A4, "if (tss_set(owner->scope_key, NULL) != thrd_success)", "if (false)"),
        Mutation("drop sparse generation", A4, ".expected_generation = mem->base_bo->allocation_generation,", ".expected_generation = 1,"),
        Mutation("allow foreign sparse memory", A4, "helios_object_owned(&mem->base.vk.base, dev)", "true"),
        Mutation("allow foreign semaphore", A4, "struct vn_semaphore *sem = vn_semaphore_from_handle(handle);\n   if (!sem || !helios_object_owned(&sem->base.vk, dev))", "struct vn_semaphore *sem = vn_semaphore_from_handle(handle);\n   if (!sem || !true)"),
        Mutation("allow foreign fence", A4, "if (fence && helios_object_owned(&fence->base.vk, dev))", "if (fence && true)"),
        Mutation("truncate sparse uses", A4, "copy->allocation_count >= HELIOS_HNR2_MAX_USE_RECORDS", "false"),
        Mutation("invent patch table", A4, ".patches = NULL,", ".patches = (void *)payload,"),
        Mutation("submit record-only through KMT", A4, "return helios_record_append(queue, refusal, payload, payload_bytes,\n                                  NULL, 0, NULL, false);", "return helios_native_context_submit_ordered(queue->helios_native_context, NULL, 0, NULL, NULL, 0, false, NULL, NULL);"),
        Mutation("drop outstanding bound", NATIVE, "c->enqueued - c->completed >=\n             HELIOS_HNR2_MAX_OUTSTANDING_SUBMISSIONS", "false"),
        Mutation("wrap HNR2 batch token", NATIVE, "if (c->next_batch_token == UINT64_MAX) {", "if (false) {"),
        Mutation("wrap progress value", NATIVE, "if (c->enqueued == UINT64_MAX)", "if (false)"),
        Mutation("revoke progress before context", NATIVE, "if (c->context) {\n      D3DKMT_DESTROYCONTEXT", "if (c->fence) {\n      D3DKMT_DESTROYSYNCHRONIZATIONOBJECT ds = { .hSyncObject = c->fence };\n      HELIOS_IGNORE_STATUS(D3DKMTDestroySynchronizationObject(&ds));\n   }\n   if (c->context) {\n      D3DKMT_DESTROYCONTEXT"),
        Mutation("signal native fence before render", NATIVE, "c, signals, signal_count, true);", "c, waits, wait_count, false);"),
        Mutation("move imported wait to other context", NATIVE, "D3DKMT_WAITFORSYNCHRONIZATIONOBJECTFROMGPU op;\n         memset(&op, 0, sizeof(op));\n         op.hContext = c->context;", "D3DKMT_WAITFORSYNCHRONIZATIONOBJECTFROMGPU op;\n         memset(&op, 0, sizeof(op));\n         op.hContext = 0;"),
        Mutation("unguard failed-copy cleanup", A4, "helios_submit1_copy_fini(struct helios_submit1_copy *copy)\n{\n   if (copy->aux) {", "helios_submit1_copy_fini(struct helios_submit1_copy *copy)\n{\n   if (true) {"),
        Mutation("drop submit payload preflight", A4, "const size_t bounded_payload = vn_sizeof_vkQueueSubmit(", "const size_t bounded_payload = sizeof_vkQueueSubmit("),
        Mutation("drop record native-fence refusal", A4, "if (copy.waits.count || copy.signals.count) {\n         /* HOB1 has no native-fence carrier", "if (false) {\n         /* HOB1 has no native-fence carrier"),
        Mutation("route QueueSubmit to old ring", QUEUE, "return vn_error(dev->instance, vn_helios_queue_submit(\n                                     vn_queue, submitCount, pSubmits, fence));", "return vn_queue_submit(NULL);"),
        Mutation("destroy initialized queues forward", DEVICE, "for (uint32_t k = count; k > 0; k--)", "for (uint32_t k = 0; k < count; k++)"),
        Mutation("destroy owner before borrower", DEVICE, "for (uint32_t i = dev->queue_count; i > 0; i--)", "for (uint32_t i = 0; i < dev->queue_count; i++)"),
        Mutation("destroy context before producer join", DEVICE, "thrd_join(queue->async_present.thread, NULL);", "vn_helios_submit_queue_fini(queue);\n      thrd_join(queue->async_present.thread, NULL);"),
        Mutation("substitute ring queue idle", QUEUE, "return vn_result(dev->instance, vn_helios_queue_wait_idle(queue));", "return vn_queue_submit(NULL);"),
        Mutation("expose A5", ICD, "#include \"vn_instance.h\"", "#include \"vn_instance.h\"\nconst char *helios_icd_create_translator_v1 = \"HeliosIcdCreateTranslatorV1\";"),
    )


def run_mutations(sources: dict[str, str]) -> None:
    for case in mutation_cases():
        source = sources[case.path]
        if source.count(case.old) != 1:
            raise SystemExit(f"A4 mutation setup failed for {case.name}: anchor count {source.count(case.old)}")
        mutated = dict(sources)
        mutated[case.path] = source.replace(case.old, case.new, 1)
        if not check_sources(mutated):
            raise SystemExit(f"A4 mutation was accepted: {case.name}")
    print(f"OK: {len(mutation_cases())} in-memory Mesa A4 mutations rejected")


def main() -> None:
    repo = os.path.abspath(sys.argv[1]) if len(sys.argv) > 1 and not sys.argv[1].startswith("--") else REPO_DEFAULT
    sources = load_sources(repo)
    errors = check_sources(sources)
    if errors:
        raise SystemExit("Mesa A4 submit gate violated:\n" + "\n".join(errors))
    if "--mutations" in sys.argv[1:]:
        run_mutations(sources)
    print("OK: Mesa A4 mode ownership, sealing, exact closure, same-context ordering, and drains checked")


if __name__ == "__main__":
    main()
