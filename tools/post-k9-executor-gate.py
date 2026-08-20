#!/usr/bin/env python3
"""Bounded post-K9 HVM1/Venus executor source and mutation gate.

The gate deliberately imports the complete K9/K11/K2a gate ancestry first.
Its local checks cover only the owner-authorized continuation: truthful role-4
storage, the generated A3/A4 Venus subset, direct bounded custody, private-copy
patching, stock nonzero-ring execution, real host terminals, and reverse drain.
"""

from __future__ import annotations

import os
import re
import runpy
import sys
from dataclasses import dataclass


REPO_DEFAULT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
K9 = runpy.run_path(
    os.path.join(os.path.dirname(__file__), "k9-ordered-engine-gate.py"),
    run_name="post_k9_imported_k9_gate",
)
k9_load_sources = K9["load_sources"]
k9_check_sources = K9["check_sources"]
live_rust = K9["live_rust"]
unique_function = K9["unique_function"]

LOGIC = "kmd_logic/src/lib.rs"
EXECUTOR = "kmd_logic/src/venus_executor.rs"
ALLOC = "kmd_render/src/ddi/create_allocation.rs"
NATIVE = "kmd_render/src/ddi/native_render.rs"
SESSION = "kmd_render/src/ddi/translation_session.rs"
TRANSPORT = "kmd_render/src/ddi/session_transport.rs"
SUBMIT = "kmd_render/src/ddi/submit_command.rs"
INTERRUPT = "kmd_render/src/ddi/interrupt.rs"
GPU = "kmd_render/src/virtio/gpu/mod.rs"
VENUS_COMMANDS = "kmd_render/src/virtio/venus/commands.rs"
VENUS_MOD = "kmd_render/src/virtio/venus/mod.rs"
VENUS_DEFINES = "icd/mesa/src/virtio/venus-protocol/vn_protocol_driver_defines.h"
VENUS_MEMORY = "icd/mesa/src/virtio/venus-protocol/vn_protocol_driver_device_memory.h"
VENUS_QUEUE = "icd/mesa/src/virtio/venus-protocol/vn_protocol_driver_queue.h"
VULKAN_CORE = "icd/mesa/include/vulkan/vulkan_core.h"
RETIREMENT_GATES = "tools/retirement-gates.sh"

EXTRA_SOURCES = (
    EXECUTOR,
    VENUS_COMMANDS,
    VENUS_MOD,
    VENUS_DEFINES,
    VENUS_MEMORY,
    VENUS_QUEUE,
    VULKAN_CORE,
)


def compact(source: str) -> str:
    return re.sub(r"\s+", "", source)


def load_sources(repo: str) -> dict[str, str]:
    sources = k9_load_sources(repo)
    for relative in EXTRA_SOURCES:
        with open(
            os.path.join(repo, relative), encoding="utf-8", errors="replace"
        ) as stream:
            sources[relative] = stream.read()
    return sources


def body(
    sources: dict[str, str], path: str, name: str, errors: list[str]
) -> str:
    found = unique_function(sources, path, name, errors)
    return "" if found is None else found[1]


def require(
    path: str,
    name: str,
    source: str,
    fragments: tuple[str, ...],
    errors: list[str],
) -> None:
    value = compact(source)
    for fragment in fragments:
        if compact(fragment) not in value:
            errors.append(
                f"{path}:{name}: required post-K9 fragment missing: {fragment}"
            )


def require_order(
    path: str,
    name: str,
    source: str,
    tokens: tuple[str, ...],
    errors: list[str],
) -> None:
    value = compact(source)
    cursor = 0
    for token in tokens:
        wanted = compact(token)
        position = value.find(wanted, cursor)
        if position < 0:
            errors.append(
                f"{path}:{name}: post-K9 order drifted at {token}: "
                f"{' -> '.join(tokens)}"
            )
            return
        cursor = position + len(wanted)


def numeric(source: str, name: str) -> int | None:
    patterns = (
        rf"\b{re.escape(name)}\s*=\s*([0-9_]+)",
        rf"#define\s+{re.escape(name)}\s+\(\([^)]*\)([0-9_]+)\)",
        rf"\bconst\s+{re.escape(name)}\s*:\s*u32\s*=\s*([0-9_]+)",
        rf"\bpub\s+const\s+{re.escape(name)}\s*:\s*u32\s*=\s*([0-9_]+)",
    )
    for pattern in patterns:
        match = re.search(pattern, source)
        if match:
            return int(match.group(1).replace("_", ""))
    return None


def check_generated_schema(sources: dict[str, str], errors: list[str]) -> None:
    executor = live_rust(sources.get(EXECUTOR, ""))
    defines = sources.get(VENUS_DEFINES, "")
    core = sources.get(VULKAN_CORE, "")
    commands = (
        ("OP_QUEUE_SUBMIT", "VK_COMMAND_TYPE_vkQueueSubmit_EXT"),
        ("OP_ALLOCATE_MEMORY", "VK_COMMAND_TYPE_vkAllocateMemory_EXT"),
        ("OP_FREE_MEMORY", "VK_COMMAND_TYPE_vkFreeMemory_EXT"),
        ("OP_QUEUE_BIND_SPARSE", "VK_COMMAND_TYPE_vkQueueBindSparse_EXT"),
        ("OP_SET_REPLY", "VK_COMMAND_TYPE_vkSetReplyCommandStreamMESA_EXT"),
        ("OP_QUEUE_SUBMIT2", "VK_COMMAND_TYPE_vkQueueSubmit2_EXT"),
    )
    for rust_name, generated_name in commands:
        rust_value = numeric(executor, rust_name)
        generated_value = numeric(defines, generated_name)
        if rust_value is None or rust_value != generated_value:
            errors.append(
                f"{EXECUTOR}: {rust_name}={rust_value!r} does not match "
                f"generated {generated_name}={generated_value!r}"
            )

    stypes = (
        ("ST_SUBMIT_INFO", "VK_STRUCTURE_TYPE_SUBMIT_INFO", core),
        ("ST_MEMORY_ALLOCATE_INFO", "VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO", core),
        ("ST_BIND_SPARSE_INFO", "VK_STRUCTURE_TYPE_BIND_SPARSE_INFO", core),
        ("ST_MEMORY_ALLOCATE_FLAGS_INFO", "VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_FLAGS_INFO", core),
        ("ST_DEVICE_GROUP_SUBMIT_INFO", "VK_STRUCTURE_TYPE_DEVICE_GROUP_SUBMIT_INFO", core),
        ("ST_DEVICE_GROUP_BIND_SPARSE_INFO", "VK_STRUCTURE_TYPE_DEVICE_GROUP_BIND_SPARSE_INFO", core),
        ("ST_EXPORT_MEMORY_ALLOCATE_INFO", "VK_STRUCTURE_TYPE_EXPORT_MEMORY_ALLOCATE_INFO", core),
        ("ST_MEMORY_DEDICATED_ALLOCATE_INFO", "VK_STRUCTURE_TYPE_MEMORY_DEDICATED_ALLOCATE_INFO", core),
        ("ST_PROTECTED_SUBMIT_INFO", "VK_STRUCTURE_TYPE_PROTECTED_SUBMIT_INFO", core),
        ("ST_TIMELINE_SEMAPHORE_SUBMIT_INFO", "VK_STRUCTURE_TYPE_TIMELINE_SEMAPHORE_SUBMIT_INFO", core),
        ("ST_MEMORY_OPAQUE_CAPTURE_ADDRESS_ALLOCATE_INFO", "VK_STRUCTURE_TYPE_MEMORY_OPAQUE_CAPTURE_ADDRESS_ALLOCATE_INFO", core),
        ("ST_SUBMIT_INFO2", "VK_STRUCTURE_TYPE_SUBMIT_INFO_2", core),
        ("ST_SEMAPHORE_SUBMIT_INFO", "VK_STRUCTURE_TYPE_SEMAPHORE_SUBMIT_INFO", core),
        ("ST_COMMAND_BUFFER_SUBMIT_INFO", "VK_STRUCTURE_TYPE_COMMAND_BUFFER_SUBMIT_INFO", core),
        ("ST_IMPORT_MEMORY_RESOURCE_INFO_MESA", "VK_STRUCTURE_TYPE_IMPORT_MEMORY_RESOURCE_INFO_MESA", defines),
    )
    for rust_name, generated_name, generated_source in stypes:
        rust_value = numeric(executor, rust_name)
        generated_value = numeric(generated_source, generated_name)
        if rust_value is None or rust_value != generated_value:
            errors.append(
                f"{EXECUTOR}: {rust_name}={rust_value!r} does not match "
                f"generated {generated_name}={generated_value!r}"
            )

    validate = body(sources, EXECUTOR, "validate_venus_stream", errors)
    require(
        EXECUTOR,
        "validate_venus_stream",
        validate,
        (
            "OP_SET_REPLY",
            "OP_ALLOCATE_MEMORY",
            "OP_FREE_MEMORY",
            "OP_QUEUE_SUBMIT",
            "OP_QUEUE_SUBMIT2",
            "OP_QUEUE_BIND_SPARSE",
            "return Err(VenusReject::UnknownOpcode)",
            "if c.offset != bytes.len()",
            "VenusReject::TrailingBytes",
        ),
        errors,
    )
    parser = compact(executor.split("#[cfg(test)]", 1)[0])
    for forbidden in (
        "Vec<",
        "HashMap<",
        "BTreeMap<",
        "unsafe{",
        "transmute",
        "from_raw_parts",
    ):
        if compact(forbidden) in parser:
            errors.append(f"{EXECUTOR}: generated subset parser gained forbidden {forbidden}")

    # The admitted structures must be the same generated encoder families Mesa
    # uses; this catches a hand-maintained shape silently diverging from the
    # pinned generator even when an opcode number stays unchanged.
    generated = compact(sources.get(VENUS_MEMORY, "") + sources.get(VENUS_QUEUE, ""))
    for fragment in (
        "vn_encode_vkAllocateMemory",
        "vn_encode_vkFreeMemory",
        "vn_encode_vkQueueSubmit",
        "vn_encode_vkQueueSubmit2",
        "vn_encode_vkQueueBindSparse",
        "vn_encode_VkImportMemoryResourceInfoMESA",
    ):
        if compact(fragment) not in generated:
            errors.append(f"Mesa generated encoder subset missing: {fragment}")


def check_role4_and_allocation_custody(
    sources: dict[str, str], errors: list[str]
) -> None:
    strict = body(sources, LOGIC, "choose_strict_device_local_memory_type", errors)
    require(
        LOGIC,
        "choose_strict_device_local_memory_type",
        strict,
        (
            "(flags & MEMORY_PROPERTY_DEVICE_LOCAL) != 0",
            "(flags & MEMORY_PROPERTY_HOST_VISIBLE) == 0",
            "return Some(MemoryTypeChoice::Exact(i))",
            "None",
        ),
        errors,
    )
    if compact(strict).count("returnSome(") != 1:
        errors.append(f"{LOGIC}: strict role-4 selector gained a fallback tier")
    allocate = body(sources, VENUS_COMMANDS, "allocate_device_local_memory_blob", errors)
    require(
        VENUS_COMMANDS,
        "allocate_device_local_memory_blob",
        allocate,
        (
            "choose_strict_device_local_memory_type",
            "MemoryPNext::Export",
            "EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF",
            "VIRTIO_GPU_BLOB_MEM_HOST3D",
            "VIRTIO_GPU_BLOB_FLAG_USE_SHAREABLE",
            "memory_type_index",
        ),
        errors,
    )
    if "VIRTIO_GPU_BLOB_FLAG_USE_MAPPABLE" in live_rust(allocate):
        errors.append(f"{VENUS_COMMANDS}: role-4 allocator became mappable")

    admit = body(sources, ALLOC, "admit_hvm1", errors)
    require_order(
        ALLOC,
        "admit_hvm1",
        admit,
        (
            "let cpu_visible = role.placement().cpu_visible",
            "if cpu_visible && !adapter.share_backing_store_with_kmd()",
            "const HVM1_CPU_VISIBLE_MAX_BYTES: u64 = 1024 * PAGE as u64",
            "if cpu_visible && record.byte_size > HVM1_CPU_VISIBLE_MAX_BYTES",
            "if matches!(role, Hvm1Role::VulkanDeviceLocal)",
            "allocate_device_local_memory_blob(adapter, record.byte_size)",
            "share_backing_store: cpu_visible",
        ),
        errors,
    )
    require(
        ALLOC,
        "admit_hvm1",
        admit,
        (
            "BackingSize::HostAuthoritative(blob.size)",
            "BackingSize::SharedBackingStore(record.byte_size)",
            "CREATE_HVM1_MEMORY_CLASS_REFUSED",
        ),
        errors,
    )

    facts = body(sources, ALLOC, "hnr2_execution_allocation_facts", errors)
    require(
        ALLOC,
        "hnr2_execution_allocation_facts",
        facts,
        (
            "allocation_object::is_current(ctx.generation)",
            "ctx.resource_id.load(Ordering::Acquire) == 0",
            "Hvm1Role::ReplyPool | Hvm1Role::VulkanHostVisible | Hvm1Role::Feedback",
            "BACKING_STORE_BOUND",
            "Hvm1Role::VulkanDeviceLocal",
            "BACKING_STORE_UNBOUND",
            "ctx.backing_store_va.load(Ordering::Relaxed) == 0",
            "BackingSize::HostAuthoritative(n) if n == byte_size",
            "ctx.venus_memory_id != 0",
            "HELIOS_HWA2_FLAG_RESOURCE_ASSOCIATED",
        ),
        errors,
    )
    open_use = body(sources, ALLOC, "open_allocation_execution_use", errors)
    require_order(
        ALLOC,
        "open_allocation_execution_use",
        open_use,
        (
            "open_allocation_context(h)",
            "identity.generation != expected_generation",
            ".acquire(passive, session, expected_generation)",
            "guard.allocation_generation != identity.generation",
            "guard.byte_size != identity.byte_size",
            "!role_matches",
            "Some(guard)",
        ),
        errors,
    )
    close = body(sources, ALLOC, "dxgkddi_close_allocation", errors)
    require_order(
        ALLOC,
        "dxgkddi_close_allocation",
        close,
        (
            "open.execution.take()",
            "execution.close",
            "open.reply_pool_session.take()",
            "close_reply_pool_binding",
        ),
        errors,
    )


def check_bounded_executor(sources: dict[str, str], errors: list[str]) -> None:
    native = live_rust(sources.get(NATIVE, ""))
    for fragment in (
        "const EXECUTOR_SLOTS: usize = HELIOS_HNR2_MAX_OUTSTANDING_SUBMISSIONS as usize;",
        "const BATCH_TICKETS: usize = HELIOS_HNR2_MAX_FRAGMENTS as usize;",
        "entries: [Option<crate::adapter::OrderedEngineTicket>; BATCH_TICKETS]",
        "slots.try_reserve_exact(EXECUTOR_SLOTS).ok()?;",
        "expected_operands.try_reserve_exact(HELIOS_HNR2_MAX_PATCH_RECORDS as usize)",
        "uses.try_reserve_exact(HELIOS_HVC1_ALLOCATION_LIST_ENTRIES as usize)",
    ):
        if compact(fragment) not in compact(native):
            errors.append(f"{NATIVE}: fixed executor storage missing: {fragment}")

    hvc1 = body(sources, SESSION, "admit_hvc1", errors)
    require_order(
        SESSION,
        "admit_hvc1 queue endpoint",
        hvc1,
        (
            "let endpoint_capacity =",
            "model.phase() != model::SessionPhase::Live",
            "next_queue_endpoint.fetch_update",
            "next < endpoint_capacity",
            "obj.endpoints.get(endpoint_index)",
            "obj.acquire()",
        ),
        errors,
    )

    append = body(sources, NATIVE, "append", errors)
    require_order(
        NATIVE,
        "BatchTickets::append",
        append,
        (
            "let prior_epoch = self.entries[..self.count as usize]",
            ".map(|old| old.epoch())",
            "let new_epoch = prior_epoch.is_none_or(|epoch| epoch != ticket.epoch())",
            "if new_epoch",
            "if any_live",
            "self.entries = [None; BATCH_TICKETS]",
            "if commit_record && self.commit_seen",
            "self.count.checked_add(1)",
            "next_count > fragment_count",
            "commit_record != (next_count == fragment_count)",
            "self.commit_seen |= commit_record",
        ),
        errors,
    )

    begin = body(sources, NATIVE, "begin_executor_batch", errors)
    require_order(
        NATIVE,
        "begin_executor_batch",
        begin,
        (
            "native.acquire_operation()",
            "staging_mut().checkout(header.total_payload_bytes)",
            "StagingCustody",
            "acquire_execution_operation(session)",
            "session_operation.transport_instance == 0",
            "BatchIdentity",
            "full_payload_crc64: header.full_payload_crc64",
            "SubmissionSlot::Collecting",
        ),
        errors,
    )
    require(
        NATIVE,
        "NativeContext::new",
        native,
        (
            "NativeClass::Control => ring_index != 0 || endpoint_id != 0",
            "NativeClass::Queue | NativeClass::Outer => ring_index == 0 || endpoint_id == 0",
            "matches!(class, NativeClass::Queue | NativeClass::Outer)",
            "session_generation == 0 || context_generation == 0",
            "outer_worker = if class == NativeClass::Outer",
        ),
        errors,
    )
    copy = body(sources, NATIVE, "copy_executor_fragment", errors)
    require(
        NATIVE,
        "copy_executor_fragment",
        copy,
        (
            "building.identity.full_payload_crc64 != header.full_payload_crc64",
            "copy_from_command",
            "building.payload.as_mut_slice()",
        ),
        errors,
    )
    prepare = body(sources, NATIVE, "prepare_executor_commit", errors)
    require_order(
        NATIVE,
        "prepare_executor_commit",
        prepare,
        (
            "crc64_ecma(building_ref.payload.as_slice())",
            "actual_crc != header.full_payload_crc64",
            "validate_venus_stream(building_ref.payload.as_slice(), expected_operands,)",
            "admission.operand_count as usize != patches.len()",
            "patch.payload_offset != expected.payload_offset",
            "patch.operand_kind != HELIOS_HNR2_OPERAND_KIND_HOST_RESOURCE_ID32",
            "open_allocation_execution_use",
            "guard.transport_instance != building_ref.session.transport_instance",
            "let exact_host_memory_type = matches!",
            "HELIOS_HVM1_ROLE_VULKAN_DEVICE_LOCAL",
            "import_guard.memory_type_index != admission.memory_type_index",
            "dst.copy_from_slice(&guard.resource_id.to_le_bytes())",
        ),
        errors,
    )
    require(
        NATIVE,
        "prepare_executor_commit",
        prepare,
        (
            "uses.len() != 2",
            "patches.len() != 2",
            "uses.len() != 1",
            "allocations.iter().any",
            "building.payload.as_mut_slice().get_mut(start..end)",
        ),
        errors,
    )
    if "pCommand" in live_rust(prepare):
        errors.append(f"{NATIVE}: executor patching escaped the host-private payload")

    submit = body(sources, NATIVE, "submit", errors)
    require_order(
        NATIVE,
        "submit queue",
        submit,
        (
            "native.class == NativeClass::Queue",
            "record.ring_index == 0",
            "admit_host_completion(fence, resubmission)",
            "let commit_record = record.payload_bytes != 0",
            "append_queue_ticket",
            "if commit_record",
            "SubmissionSlot::InFlight",
            "gpu.scanout_transport_instance() != transport_instance",
            "gpu.enqueue_native_submit",
            "NativeSubmitDisposition::Pending",
        ),
        errors,
    )
    if re.search(
        r"QueueSubmitAction::Enqueue.*?note_and_maybe_signal",
        live_rust(submit),
        re.S,
    ):
        errors.append(f"{NATIVE}: async executor gained the compatibility completion path")
    if compact("Ok(Some(Ok(_))) => NativeSubmitDisposition::Pending") not in compact(submit):
        errors.append(f"{NATIVE}: successful enqueue no longer waits for a host terminal")

    commit = body(sources, NATIVE, "commit", errors)
    require_order(
        NATIVE,
        "commit private-copy publication",
        commit,
        (
            "args.pDmaBufferPrivateData.is_null()",
            "size_of::<Hnr2KmdDmaPrivateV1>()",
            "prepare_executor_commit",
            "publish_dma_record",
            "finalize_executor_commit",
        ),
        errors,
    )


def check_terminal_and_teardown(sources: dict[str, str], errors: list[str]) -> None:
    finish = body(sources, NATIVE, "finish_with_cleanup", errors)
    require_order(
        NATIVE,
        "finish_with_cleanup",
        finish,
        (
            "exact_adapter",
            "exact_slot",
            "reply.finish(success)",
            "SubmissionSlot::Terminal",
            "settle_batch_tickets(adapter, tickets, success)",
            "drop(custody)",
        ),
        errors,
    )
    finish_live = compact(live_rust(finish))
    if finish_live.find("drop(custody)") < finish_live.find(
        "settle_batch_tickets(adapter,tickets,success)"
    ):
        errors.append(f"{NATIVE}: executor custody ends before its K9 terminal transition")
    settle = body(sources, NATIVE, "settle_batch_tickets", errors)
    require(
        NATIVE,
        "settle_batch_tickets",
        settle,
        (
            "complete_ordered_engine_submission(adapter, *ticket)",
            "fail_ordered_engine_submission(adapter, *ticket)",
        ),
        errors,
    )

    gpu_enqueue = body(sources, GPU, "enqueue_native_submit", errors)
    require(
        GPU,
        "enqueue_native_submit",
        gpu_enqueue,
        ("ring_idx == 0", "Some(completion)", "enqueue_submit_inner"),
        errors,
    )
    drain = body(sources, GPU, "drain_used", errors)
    native_arm_at = drain.find("ASYNC_COMPLETE_COUNT.fetch_add")
    native_arm = drain[native_arm_at:] if native_arm_at >= 0 else ""
    require_order(
        GPU,
        "drain_used native terminal",
        native_arm,
        (
            "VIRTIO_GPU_RESP_OK_NODATA",
            "if let Some(completion) = native_completion",
            "completion.terminal(response_ok)",
            "request_wddm_completion_dpc(adapter)",
        ),
        errors,
    )
    require(
        GPU,
        "drain_used native custody",
        drain,
        ("let native_completion = take_native_completion",),
        errors,
    )
    terminal_drain = body(sources, NATIVE, "drain_host_terminals", errors)
    require_order(
        NATIVE,
        "drain_host_terminals",
        terminal_drain,
        (
            "for _ in 0..crate::virtio::gpu::MAX_INFLIGHT",
            "gpu.begin_native_terminal_drain()",
            "terminal.finish(adapter)",
            "gpu.finish_native_terminal_drain(terminals)",
            "request_wddm_completion_dpc(adapter)",
        ),
        errors,
    )
    reset = body(sources, GPU, "finish_physical_reset_and_abort", errors)
    require_order(
        GPU,
        "finish_physical_reset_and_abort",
        reset,
        (
            "if !self.failed",
            "self.latch_failed_and_fail_inflight()",
            "if status == 0",
            "self.terminalize_native_after_physical_reset()",
        ),
        errors,
    )

    dpc = body(sources, INTERRUPT, "drain_used_and_complete", errors)
    require(
        INTERRUPT,
        "drain_used_and_complete",
        dpc,
        ("native_render::drain_host_terminals(adapter)",),
        errors,
    )
    submit_ddi = body(sources, SUBMIT, "dxgkddi_submit_command", errors)
    require_order(
        SUBMIT,
        "dxgkddi_submit_command",
        submit_ddi,
        (
            "NativeSubmitDisposition::Pending",
            "SubmitAck::Accepted",
            "NativeSubmitDisposition::Refused, ticket",
            "note_and_maybe_signal",
        ),
        errors,
    )

    close = body(sources, NATIVE, "close", errors)
    require_order(
        NATIVE,
        "NativeContext::close",
        close,
        (
            "rundown.open = false",
            "if let Some(worker) = self.outer_worker.as_ref()",
            "worker.close_and_wait()",
            "let scratch = unsafe { &mut *self.scratch.get() }",
            "let building = scratch.building.take()",
            "abandon_control_building(scratch, self)",
            "settle_batch_tickets(adapter, tickets, false)",
            "drop(building)",
            "drain_host_terminals(adapter)",
            "KeWaitForSingleObject",
            "drain_host_terminals(adapter)",
            "reap_terminal_slots",
        ),
        errors,
    )
    cleanup = body(sources, TRANSPORT, "cleanup_live_host", errors)
    require_order(
        TRANSPORT,
        "cleanup_live_host",
        cleanup,
        (
            "self.attachments.lock().pop()",
            "ctx_detach_session_resource",
            "encode_destroy_instance",
            "cleanup_session_resource",
        ),
        errors,
    )
    teardown = body(sources, TRANSPORT, "teardown", errors)
    require_order(
        TRANSPORT,
        "teardown",
        teardown,
        (
            "self.close_and_wait(passive)",
            "HostState::Dead",
            "self.cleanup_live_host",
        ),
        errors,
    )


def check_integration(sources: dict[str, str], errors: list[str]) -> None:
    retirement = sources.get(RETIREMENT_GATES, "")
    invocations = re.findall(
        r'(?m)^\s*python3\s+"\$REPO/tools/post-k9-executor-gate\.py"\s+"\$REPO"\s+--mutations\s*$',
        retirement,
    )
    if len(invocations) != 1:
        errors.append(
            f"{RETIREMENT_GATES}: post-K9 executor mutation gate is not integrated exactly once"
        )


def check_local_sources(sources: dict[str, str]) -> list[str]:
    errors: list[str] = []
    check_generated_schema(sources, errors)
    check_role4_and_allocation_custody(sources, errors)
    check_bounded_executor(sources, errors)
    check_terminal_and_teardown(sources, errors)
    check_integration(sources, errors)
    return errors


def check_sources(sources: dict[str, str]) -> list[str]:
    inherited = {
        path: source for path, source in sources.items() if path not in EXTRA_SOURCES
    }
    errors = [f"K9: {error}" for error in k9_check_sources(inherited)]
    errors.extend(check_local_sources(sources))
    return errors


@dataclass(frozen=True)
class Mutation:
    name: str
    path: str
    old: str
    new: str


def mutation_cases() -> tuple[Mutation, ...]:
    return (
        Mutation("accept host-visible role4", LOGIC, "pub fn choose_strict_device_local_memory_type(\n    memory_type_flags: &[u32],\n    memory_type_count: u32,\n    memory_type_bits: u32,\n) -> Option<MemoryTypeChoice> {", "pub fn choose_strict_device_local_memory_type(\n    memory_type_flags: &[u32],\n    memory_type_count: u32,\n    memory_type_bits: u32,\n) -> Option<MemoryTypeChoice> {\n    return Some(MemoryTypeChoice::Exact(0));"),
        Mutation("accept non-device-local role4", LOGIC, "pub fn choose_strict_device_local_memory_type(", "pub fn choose_any_memory_type("),
        Mutation("make role4 mappable", VENUS_COMMANDS, "ctrl::resource_create_blob_with_finalizer(\n                self.passive(),\n                adapter,\n                self.ctx_id(),\n                VIRTIO_GPU_BLOB_MEM_HOST3D,\n                VIRTIO_GPU_BLOB_FLAG_USE_SHAREABLE,", "ctrl::resource_create_blob_with_finalizer(\n                self.passive(),\n                adapter,\n                self.ctx_id(),\n                VIRTIO_GPU_BLOB_MEM_HOST3D,\n                VIRTIO_GPU_BLOB_FLAG_USE_SHAREABLE | VIRTIO_GPU_BLOB_FLAG_USE_MAPPABLE,"),
        Mutation("substitute host-visible allocator", ALLOC, "client.allocate_device_local_memory_blob(adapter, record.byte_size)", "client.allocate_memory_blob(adapter, record.byte_size, true, true)"),
        Mutation("double udmabuf bound", ALLOC, "const HVM1_CPU_VISIBLE_MAX_BYTES: u64 = 1024 * PAGE as u64;", "const HVM1_CPU_VISIBLE_MAX_BYTES: u64 = 2048 * PAGE as u64;"),
        Mutation("share role4 backing", ALLOC, "share_backing_store: cpu_visible,", "share_backing_store: true,"),
        Mutation("accept mapped role4 facts", ALLOC, "&& ctx.backing_store_va.load(Ordering::Relaxed) == 0", "&& true"),
        Mutation("drop role4 memory owner", ALLOC, "&& ctx.venus_memory_id != 0", "&& true"),
        Mutation("accept generation-only open", ALLOC, "|| guard.byte_size != identity.byte_size", "|| false"),
        Mutation("accept role drift at open", ALLOC, "|| !role_matches", "|| false"),
        Mutation("unbound executor slots", NATIVE, "const EXECUTOR_SLOTS: usize = HELIOS_HNR2_MAX_OUTSTANDING_SUBMISSIONS as usize;", "const EXECUTOR_SLOTS: usize = usize::MAX;"),
        Mutation("unbound batch tickets", NATIVE, "const BATCH_TICKETS: usize = HELIOS_HNR2_MAX_FRAGMENTS as usize;", "const BATCH_TICKETS: usize = 65536;"),
        Mutation("reuse queue endpoints after negotiated capacity", SESSION, "|next| (next < endpoint_capacity).then_some(next + 1),", "|next| Some(next.wrapping_add(1)),"),
        Mutation("allow early commit", NATIVE, "if commit_record && self.commit_seen {", "if false {"),
        Mutation("reuse only the first resubmission epoch", NATIVE, "let new_epoch = prior_epoch.is_none_or(|epoch| epoch != ticket.epoch());", "let new_epoch = self.resubmit_count == 0;"),
        Mutation("ignore commit position", NATIVE, "if next_count > fragment_count || commit_record != (next_count == fragment_count) {", "if next_count > fragment_count {"),
        Mutation("defer staging charge to commit", NATIVE, "let context = native\n        .acquire_operation()\n        .ok_or(STATUS_INVALID_DEVICE_REQUEST)?;\n    {\n        let mut state = native.state.lock();\n        if let Err(refusal) = state.staging_mut().checkout(header.total_payload_bytes)", "let context = native\n        .acquire_operation()\n        .ok_or(STATUS_INVALID_DEVICE_REQUEST)?;\n    {\n        let mut state = native.state.lock();\n        if let Err(refusal) = state.staging_mut().checkout(0)"),
        Mutation("permit ring zero executor", NATIVE, "NativeClass::Queue | NativeClass::Outer => ring_index == 0 || endpoint_id == 0,", "NativeClass::Queue | NativeClass::Outer => false,"),
        Mutation("drop fragment crc identity", NATIVE, "|| building.identity.full_payload_crc64 != header.full_payload_crc64", "|| false"),
        Mutation("skip full payload crc", NATIVE, "if actual_crc != header.full_payload_crc64", "if false"),
        Mutation("skip generated validation", NATIVE, "validate_venus_stream(\n        building_ref.payload.as_slice(),", "validate_venus_stream(\n        &[],"),
        Mutation(
            "patch before proving DMA-private capacity",
            NATIVE,
            "    if native.class == NativeClass::Queue\n"
            "        && (args.pDmaBufferPrivateData.is_null()\n"
            "            || (args.DmaBufferPrivateDataSize as usize) < size_of::<Hnr2KmdDmaPrivateV1>())\n"
            "    {",
            "    if false {",
        ),
        Mutation("allow operand count drift", NATIVE, "if admission.operand_count as usize != patches.len() {", "if false {"),
        Mutation("allow non-resource patch kind", NATIVE, "|| helios_protocol::native_render::hnr2_operand_width(patch.operand_kind)\n                != Some(patch.encoded_width)\n            || patch.operand_kind != HELIOS_HNR2_OPERAND_KIND_HOST_RESOURCE_ID32", "|| helios_protocol::native_render::hnr2_operand_width(patch.operand_kind)\n                != Some(patch.encoded_width)\n            || false"),
        Mutation("patch user source", NATIVE, "building.payload.as_mut_slice().get_mut(start..end)", "unsafe { core::slice::from_raw_parts_mut(args.pCommand.cast(), end) }.get_mut(start..end)"),
        Mutation("accept stale transport allocation", NATIVE, "if guard.transport_instance != building_ref.session.transport_instance", "if false"),
        Mutation("drop exact host-backed memory type", NATIVE, "|| (exact_host_memory_type\n                    && import_guard.memory_type_index != admission.memory_type_index)", "|| false"),
        Mutation("forge host resource constant", NATIVE, "let Some(dst) = building.payload.as_mut_slice().get_mut(start..end) else {\n            return Err(STATUS_INVALID_PARAMETER);\n        };\n        dst.copy_from_slice(&guard.resource_id.to_le_bytes());", "let Some(dst) = building.payload.as_mut_slice().get_mut(start..end) else {\n            return Err(STATUS_INVALID_PARAMETER);\n        };\n        dst.copy_from_slice(&1u32.to_le_bytes());"),
        Mutation("enqueue on fragment", NATIVE, "if commit_record {\n                            if let Some(batch)", "if true {\n                            if let Some(batch)"),
        Mutation("allow zero endpoint submit", NATIVE, "|| record.ring_index == 0", "|| false"),
        Mutation("bypass exact transport", NATIVE, "if gpu.scanout_transport_instance() != transport_instance {\n                        return None;\n                    }\n                    pending.take().map(|(meta, payload, completion)| {\n                        gpu.enqueue_native_submit(\n                            host_context_id,\n                            native.ring_index,", "if false {\n                        return None;\n                    }\n                    pending.take().map(|(meta, payload, completion)| {\n                        gpu.enqueue_native_submit(\n                            host_context_id,\n                            native.ring_index,"),
        Mutation("return completed before host", NATIVE, "Ok(Some(Ok(_))) => NativeSubmitDisposition::Pending,", "Ok(Some(Ok(_))) => NativeSubmitDisposition::HostCompleted(submit.SubmissionFenceId),"),
        Mutation("settle after custody release", NATIVE, "settle_batch_tickets(adapter, tickets, success);\n        }\n        // Keep the exact context/session/allocation/staging custody", "drop(custody);\n            settle_batch_tickets(adapter, tickets, success);\n        }\n        // Keep the exact context/session/allocation/staging custody"),
        Mutation("compat complete pending", SUBMIT, "| Some((crate::ddi::native_render::NativeSubmitDisposition::Pending, _)) => {", "| Some((crate::ddi::native_render::NativeSubmitDisposition::Refused, _)) => {"),
        Mutation("accept ring-zero transport enqueue", GPU, "if ctx_id == 0 || ring_idx == 0 {\n            return Err((", "if false {\n            return Err(("),
        Mutation("ignore host response", GPU, "if ring_idx != 0 {\n                        RING_COMPLETE_COUNT.fetch_add(1, Ordering::Relaxed);\n                    }\n                    let response_ok = written_length as usize == size_of::<VirtioGpuCtrlHdr>()\n                        && resp_type == Some(VIRTIO_GPU_RESP_OK_NODATA);", "if ring_idx != 0 {\n                        RING_COMPLETE_COUNT.fetch_add(1, Ordering::Relaxed);\n                    }\n                    let response_ok = true;"),
        Mutation("drop completion handoff recheck", NATIVE, "for _ in 0..crate::virtio::gpu::MAX_INFLIGHT {", "for _ in 0..0 {"),
        Mutation("terminalize before physical reset", GPU, "if status == 0 {\n            self.terminalize_native_after_physical_reset();", "if true {\n            self.terminalize_native_after_physical_reset();"),
        Mutation("drop context wait", NATIVE, "KeWaitForSingleObject(\n                self.drained.get()", "fake_wait(\n                self.drained.get()"),
        Mutation("drop reverse attachment detach", TRANSPORT, "let resource_id = self.attachments.lock().pop().map(|entry| entry.resource_id);", "let resource_id = None;"),
        Mutation("destroy instance before attachments", TRANSPORT, "loop {\n            let resource_id = self.attachments.lock().pop()", "let destroy = pure::encode_destroy_instance(instance_handle);\n        loop {\n            let resource_id = self.attachments.lock().pop()"),
        Mutation("drift queue submit opcode", EXECUTOR, "pub const OP_QUEUE_SUBMIT: u32 = 18;", "pub const OP_QUEUE_SUBMIT: u32 = 19;"),
        Mutation("drift allocate opcode", EXECUTOR, "pub const OP_ALLOCATE_MEMORY: u32 = 21;", "pub const OP_ALLOCATE_MEMORY: u32 = 20;"),
        Mutation("admit unknown opcode", EXECUTOR, "OP_QUEUE_SUBMIT | OP_QUEUE_SUBMIT2 | OP_QUEUE_BIND_SPARSE => {\n            if flags != 0 {\n                return Err(VenusReject::BadFlags);\n            }\n            parse_queue(&mut c, opcode)?\n        }\n        _ => return Err(VenusReject::UnknownOpcode),", "OP_QUEUE_SUBMIT | OP_QUEUE_SUBMIT2 | OP_QUEUE_BIND_SPARSE => {\n            if flags != 0 {\n                return Err(VenusReject::BadFlags);\n            }\n            parse_queue(&mut c, opcode)?\n        }\n        _ => VenusCommandClass::QueueSubmit,"),
        Mutation("allow trailing bytes", EXECUTOR, "_ => return Err(VenusReject::UnknownOpcode),\n    };\n    if c.offset != bytes.len() {\n        return Err(VenusReject::TrailingBytes);", "_ => return Err(VenusReject::UnknownOpcode),\n    };\n    if false {\n        return Err(VenusReject::TrailingBytes);"),
    )


def run_mutations(sources: dict[str, str]) -> None:
    for case in mutation_cases():
        count = sources.get(case.path, "").count(case.old)
        if count != 1:
            raise SystemExit(
                f"post-K9 mutation setup failed for {case.name}: expected one "
                f"anchor in {case.path}, found {count}"
            )
        mutated = dict(sources)
        mutated[case.path] = mutated[case.path].replace(case.old, case.new, 1)
        errors = check_local_sources(mutated)
        if not errors:
            inherited = {
                path: source
                for path, source in mutated.items()
                if path not in EXTRA_SOURCES
            }
            errors.extend(f"K9: {error}" for error in k9_check_sources(inherited))
        if not errors:
            raise SystemExit(
                f"post-K9 mutation was accepted by the real gate: {case.name}"
            )
    print(
        f"OK: {len(mutation_cases())} in-memory post-K9 executor mutations "
        "rejected by the real gate"
    )


def main() -> None:
    args = sys.argv[1:]
    mutations = False
    if "--mutations" in args:
        mutations = True
        args.remove("--mutations")
    if len(args) > 1:
        raise SystemExit("usage: post-k9-executor-gate.py [repo] [--mutations]")
    repo = os.path.abspath(args[0]) if args else REPO_DEFAULT
    sources = load_sources(repo)
    errors = check_sources(sources)
    if errors:
        print("post-K9 bounded executor gate violated:", file=sys.stderr)
        for error in errors:
            print(error, file=sys.stderr)
        raise SystemExit(1)
    print(
        "OK: truthful role-4 HVM1, generated bounded Venus admission, direct "
        "custody, private patching, nonzero execution, and real K9 terminals hold"
    )
    if mutations:
        run_mutations(sources)


if __name__ == "__main__":
    main()
