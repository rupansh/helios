#!/usr/bin/env python3
"""K11 per-session stock-Venus transport source and in-memory mutation gate."""

from __future__ import annotations

import os
import re
import runpy
import sys
from dataclasses import dataclass


REPO_DEFAULT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
K2A = runpy.run_path(
    os.path.join(os.path.dirname(__file__), "k2a-share-backing-store-gate.py"),
    run_name="k11_imported_k2a_gate",
)
k2a_load_sources = K2A["load_sources"]
k2a_check_sources = K2A["check_sources"]
live_rust = K2A["live_rust"]
unique_function = K2A["unique_function"]

LOGIC = "kmd_logic/src/lib.rs"
ADAPTER = "kmd_render/src/adapter/mod.rs"
ADAPTER_KOBJ = "kmd_render/src/adapter/kobj.rs"
TRANSPORT = "kmd_render/src/ddi/session_transport.rs"
LIFECYCLE = "kmd_render/src/ddi/lifecycle.rs"
SCHEDULER = "kmd_render/src/ddi/scheduler.rs"
SESSION = "kmd_render/src/ddi/translation_session.rs"
NATIVE = "kmd_render/src/ddi/native_render.rs"
ALLOC = "kmd_render/src/ddi/create_allocation.rs"
DEVICE = "kmd_render/src/device.rs"
OWNER = "kmd_render/src/virtio/control_owner.rs"
CTRL = "kmd_render/src/virtio/ctrl.rs"
SUBMIT = "kmd_render/src/ddi/submit_command.rs"
LIB = "kmd_render/src/lib.rs"
DDI_MOD = "kmd_render/src/ddi/mod.rs"
PROTO_NATIVE = "protocol/src/native_render.rs"
PROTO_SESSION = "protocol/src/translation_session.rs"
PROTO_NATIVE_H = "protocol/include/helios_native_render.h"
PROTO_SESSION_H = "protocol/include/helios_translation_session.h"
PROBE = "tools/k11_session_transport_probe.c"
RETIREMENT_GATES = "tools/retirement-gates.sh"
ROADMAP = "ROADMAP.md"

EXTRA_SOURCES = (
    ADAPTER_KOBJ,
    PROTO_SESSION_H,
    PROBE,
    ROADMAP,
)


def compact(source: str) -> str:
    return re.sub(r"\s+", "", source)


def live_c(source: str) -> str:
    """Remove C comments while preserving code and string/character literals."""
    out = list(source)
    i = 0
    state = "code"
    while i < len(source):
        pair = source[i : i + 2]
        if state == "code":
            if pair == "//":
                out[i] = out[i + 1] = " "
                i += 2
                state = "line-comment"
                continue
            if pair == "/*":
                out[i] = out[i + 1] = " "
                i += 2
                state = "block-comment"
                continue
            if source[i] == '"':
                state = "string"
            elif source[i] == "'":
                state = "character"
        elif state == "line-comment":
            if source[i] == "\n":
                state = "code"
            else:
                out[i] = " "
        elif state == "block-comment":
            if pair == "*/":
                out[i] = out[i + 1] = " "
                i += 2
                state = "code"
                continue
            if source[i] != "\n":
                out[i] = " "
        elif state in ("string", "character"):
            if source[i] == "\\":
                i += 2
                continue
            if (state == "string" and source[i] == '"') or (
                state == "character" and source[i] == "'"
            ):
                state = "code"
        i += 1
    return "".join(out)


def load_sources(repo: str) -> dict[str, str]:
    sources = k2a_load_sources(repo)
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


def require_fragments(
    path: str, name: str, source: str, fragments: tuple[str, ...], errors: list[str]
) -> None:
    value = compact(source)
    for fragment in fragments:
        if compact(fragment) not in value:
            errors.append(f"{path}:{name}: required K11 fragment missing: {fragment}")


def require_order(
    path: str, name: str, source: str, tokens: tuple[str, ...], errors: list[str]
) -> None:
    value = compact(source)
    positions = [value.find(compact(token)) for token in tokens]
    if any(position < 0 for position in positions) or positions != sorted(positions):
        errors.append(f"{path}:{name}: K11 order drifted: {' -> '.join(tokens)}")


def struct_fields(
    sources: dict[str, str], path: str, name: str, errors: list[str]
) -> list[str]:
    live = live_rust(sources.get(path, ""))
    match = re.search(rf"\bpub\s+struct\s+{re.escape(name)}\s*\{{(.*?)\n\}}", live, re.S)
    if match is None:
        errors.append(f"{path}: expected one public ABI struct {name}")
        return []
    return re.findall(r"\bpub\s+(\w+)\s*:", match.group(1))


ABI_FIELDS: tuple[tuple[str, str, tuple[str, ...]], ...] = (
    (
        PROTO_SESSION,
        "HeliosTranslationSessionInitV1",
        (
            "magic",
            "abi_version",
            "struct_size",
            "package_generation",
            "capset",
            "requested_endpoint_capacity",
            "reserved",
        ),
    ),
    (
        PROTO_SESSION,
        "HeliosTranslationSessionReplyV1",
        (
            "magic",
            "abi_version",
            "struct_size",
            "package_generation",
            "session_generation",
            "capability_low",
            "capability_high",
            "capset",
            "endpoint_capacity",
            "reserved",
        ),
    ),
    (
        PROTO_SESSION,
        "HeliosQueueAttachV1",
        (
            "magic",
            "abi_version",
            "struct_size",
            "package_generation",
            "session_generation",
            "capability_low",
            "capability_high",
            "endpoint_id",
            "engine_class",
            "queue_family",
            "queue_index",
            "context_generation",
            "flags",
            "reserved",
        ),
    ),
    (
        PROTO_NATIVE,
        "HeliosVulkanContextV1",
        (
            "magic",
            "abi_version",
            "struct_size",
            "package_generation",
            "capset",
            "mode",
            "queue_family",
            "queue_index",
        ),
    ),
    (
        PROTO_NATIVE,
        "HeliosNativeRenderV2",
        (
            "magic",
            "abi_version",
            "header_size",
            "package_generation",
            "batch_token",
            "total_payload_bytes",
            "fragment_payload_offset",
            "fragment_payload_bytes",
            "fragment_index",
            "fragment_count",
            "use_record_offset",
            "use_record_count",
            "patch_record_offset",
            "patch_record_count",
            "reply_allocation_list_index",
            "flags",
            "reply_offset",
            "reply_capacity_bytes",
            "fragment_crc64",
            "full_payload_crc64",
            "reply_slot_generation",
        ),
    ),
    (
        PROTO_NATIVE,
        "HeliosVenusReplyV1",
        (
            "magic",
            "version",
            "header_size",
            "package_generation",
            "session_generation",
            "slot_generation",
            "batch_token",
            "snapshot_generation",
            "opcode",
            "status",
            "total_bytes",
            "chunk_offset",
            "chunk_bytes",
            "flags",
        ),
    ),
)


def check_abi(sources: dict[str, str], errors: list[str]) -> None:
    for path, name, expected in ABI_FIELDS:
        found = struct_fields(sources, path, name, errors)
        if found != list(expected):
            errors.append(f"{path}:{name}: fixed pointer-free field set drifted: {found!r}")
    if "pub(crate) mod session_transport;" not in live_rust(sources.get(DDI_MOD, "")):
        errors.append(f"{DDI_MOD}: K11 transport module is not privately wired")
    if "pub(crate) mod protocol;" in live_rust(sources.get("kmd_render/src/virtio/venus/mod.rs", "")):
        errors.append("K11 exposed the existing Venus protocol module as a new carrier surface")


def check_fixed_ownership(sources: dict[str, str], errors: list[str]) -> None:
    session = compact(live_rust(sources.get(SESSION, "")))
    for fragment in (
        "owner:DeviceOwner,",
        "endpoints:[SessionEndpointObject;model::ENDPOINT_SLOTS],",
        "ring_index:u32,",
        "transport:SessionTransport,",
        "slots:SpinLock<[Option<NonNull<SessionObject>>;SESSION_SLOTS]>,",
        "constSESSION_SLOTS:usize=helios_protocol::translation_session::HELIOS_HTS1_MAX_SESSIONS_PER_PROCESSasusize;",
        "endpoints:core::array::from_fn",
        "ring_index:indexasu32+1,",
        "transport:SessionTransport::new()",
    ):
        if compact(fragment) not in session:
            errors.append(f"{SESSION}: fixed per-session ownership missing: {fragment}")
    for unbounded in ("Vec<Session", "HashMap<", "BTreeMap<", "LinkedList<"):
        if compact(unbounded) in session:
            errors.append(f"{SESSION}: unbounded session/endpoint storage returned: {unbounded}")

    transport_live = live_rust(sources.get(TRANSPORT, ""))
    statics = re.findall(
        r"(?m)^\s*pub\s*\(crate\)\s+static\s+(K11_[A-Z0-9_]+)\s*:",
        transport_live,
    )
    expected_statics = [
        "K11_CONTEXT_CREATED",
        "K11_CONTEXT_DESTROYED",
        "K11_HOST_INIT_OK",
        "K11_HOST_INIT_REJECT",
        "K11_HOST_REPLY_REJECT",
        "K11_REPLY_PUBLISHED",
        "K11_RUNDOWN_WAITED",
        "K11_COMPLETION_WAITED",
        "K11_COMPLETION_REOPEN_REJECT",
        "K11_STALE_TRANSPORT",
        "K11_CLEANUP_REJECT",
    ]
    if statics != expected_statics:
        errors.append(f"{TRANSPORT}: only the fixed diagnostic atomics may be static: {statics!r}")
    foreign_static = re.findall(
        r"(?m)^\s*(?:pub\s*\(crate\)\s+)?static\s+(?!K11_)(\w+)\s*:",
        transport_live,
    )
    if foreign_static:
        errors.append(f"{TRANSPORT}: adapter/global host namespace storage appeared: {foreign_static!r}")
    # The post-K9 continuation adds one session-owned attachment ledger.  It is
    # a Vec only so reverse teardown can pop in attachment order; construction
    # reserves the complete protocol maximum before publication and every push
    # is preceded by both capacity and semantic-bound checks below.  No other
    # dynamic namespace container is admitted.
    for unbounded in ("HashMap<", "BTreeMap<", "LinkedList<", ".insert("):
        if compact(unbounded) in compact(transport_live):
            errors.append(f"{TRANSPORT}: host transport storage is not fixed: {unbounded}")
    for fragment in (
        "constMAX_SESSION_ATTACHMENTS:usize=helios_protocol::native_render::HELIOS_HNR2_MAX_USE_RECORDSasusize;",
        "attachments.try_reserve_exact(MAX_SESSION_ATTACHMENTS).ok()?;",
        "attachments.len()==attachments.capacity()||attachments.len()>=MAX_SESSION_ATTACHMENTS",
        "attachments.push(SessionAttachment{resource_id,references:1,state:AttachmentState::Attaching,});",
    ):
        if compact(fragment) not in compact(transport_live):
            errors.append(f"{TRANSPORT}: bounded session attachment ledger missing: {fragment}")
    if compact(transport_live).count("Vec<") != 1 or compact(transport_live).count(".push(") != 1:
        errors.append(f"{TRANSPORT}: attachment ledger gained another dynamic storage path")

    new_session = body(sources, SESSION, "new_session", errors)
    require_order(
        SESSION,
        "new_session",
        new_session,
        (
            "DeviceOwner::new(raw_device)",
            "Box::new(SessionObject",
            "endpoints: core::array::from_fn",
            "transport: SessionTransport::new()",
            "Box::into_raw(obj)",
            "transport.init_event()",
        ),
        errors,
    )

    claim = body(sources, ALLOC, "claim_k11_reply_pool_session", errors)
    require_order(
        ALLOC,
        "claim_k11_reply_pool_session",
        claim,
        (
            "resolve_alloc(allocation as HANDLE)",
            "ctx.kind != ALLOC_KIND_HVM1",
            "Hvm1Role::ReplyPool",
            "allocation_object::is_current(ctx.generation)",
            "ctx.k11_session_binding",
            "compare_exchange(0, session.as_ptr() as usize",
        ),
        errors,
    )
    bind = body(sources, SESSION, "bind_reply_pool", errors)
    require_order(
        SESSION,
        "bind_reply_pool",
        bind,
        (
            "claim_k11_reply_pool_session",
            "obj.transport.bind_reply_pool(canonical_allocation)",
            ".bind_reply_pool(role, byte_size, object_generation)",
            "if transport_bound && model_bound",
            "obj.acquire()",
            "return Some(session)",
        ),
        errors,
    )


def check_host_init(sources: dict[str, str], errors: list[str]) -> None:
    transport = live_rust(sources.get(TRANSPORT, ""))
    for fragment in (
        "const VENUS_CAPSET_ID: u32 = 4;",
        "const SESSION_INSTANCE_HANDLE: u64 = 1;",
        "const SESSION_REPLY_BYTES: u64 = 4096;",
        "const SESSION_SET_REPLY_FENCE: u64 = 1;",
        "const SESSION_CREATE_INSTANCE_FENCE: u64 = 2;",
        "const SESSION_DESTROY_INSTANCE_FENCE: u64 = 3;",
        "struct LiveHost",
        "k2a_resource_id: u32",
        "reply_resource_id: u32",
        "context_id: u32",
        "instance_handle: u64",
        "reply_map: SessionReplyMap",
        "struct SessionReplyMap",
    ):
        if fragment not in transport:
            errors.append(f"{TRANSPORT}: private host namespace shape missing: {fragment}")
    if re.search(r"\bpub(?:\s*\(crate\))?\s+struct\s+LiveHost\b", transport):
        errors.append(f"{TRANSPORT}: host context/resource identities escaped their private owner")

    initialize = body(sources, TRANSPORT, "initialize", errors)
    require_order(
        TRANSPORT,
        "initialize",
        initialize,
        (
            "self.acquire()",
            "HostState::Provisional",
            "HostState::Initializing",
            "k11_reply_pool_facts(allocation)",
            "pure::admit_reply_range",
            "crate::virtio::ctrl::ctx_create_session",
            "VENUS_CAPSET_ID",
            "crate::virtio::ctrl::resource_create_session_reply_blob",
            "SESSION_REPLY_BYTES",
            "crate::virtio::ctrl::map_session_reply_blob",
            "SessionReplyMap::new(prep)",
            "borrow_venus_session_pair",
            "self.create_instance",
            "publish(facts, &evidence)",
            "HostState::Live(live)",
            "K11_HOST_INIT_OK.fetch_add",
        ),
        errors,
    )
    if compact(initialize).count(compact("crate::virtio::ctrl::ctx_create_session")) != 1:
        errors.append(f"{TRANSPORT}: INIT must create exactly one host context")
    if compact(initialize).count(compact("resource_create_session_reply_blob")) != 1:
        errors.append(f"{TRANSPORT}: INIT must create exactly one private reply resource")
    if compact(initialize).count(compact("map_session_reply_blob")) != 1:
        errors.append(f"{TRANSPORT}: INIT must map exactly one private reply resource")
    if compact(initialize).count("HostState::Live") != 1:
        errors.append(f"{TRANSPORT}: INIT may publish the live host owner exactly once")
    if compact(initialize).count("drop(pair)") != 3:
        errors.append(f"{TRANSPORT}: INIT pair lease must drop once on each terminal path")
    if "HostState::Live(live);drop(pair);K11_HOST_INIT_OK.fetch_add" not in compact(initialize):
        errors.append(f"{TRANSPORT}: INIT released private host-pair custody before final publication")
    if "ctx_attach_session_resource" in initialize or re.search(
        r"borrow_venus_session_pair\s*\([^)]*facts\.resource_id", initialize, re.S
    ):
        errors.append(f"{TRANSPORT}: K2a was attached or selected as the renderer reply target")

    live_check = body(sources, TRANSPORT, "with_live_on_current_transport", errors)
    require_order(
        TRANSPORT,
        "with_live_on_current_transport",
        live_check,
        (
            "self.acquire()",
            "HostState::Live(host)",
            "current_transport(adapter)",
            "borrow_venus_session_pair",
            "k11_reply_pool_facts(host.allocation)",
            "facts.resource_id != host.k2a_resource_id",
            "facts.transport_instance != host.transport_instance",
            "Some(operation())",
        ),
        errors,
    )
    attach = body(sources, SESSION, "admit_hqa1", errors)
    require_order(
        SESSION,
        "admit_hqa1",
        attach,
        (
            "list.acquire_by_key",
            "with_live_on_current_transport",
            "obj.model.lock().attach(&packet)",
            "match admission",
        ),
        errors,
    )
    host_submit = body(sources, SESSION, "with_current_host_submission", errors)
    require_order(
        SESSION,
        "with_current_host_submission",
        host_submit,
        (
            "with_live_on_current_transport",
            "obj.model.lock().phase() != model::SessionPhase::Live",
            "Some(operation())",
            ".flatten()",
        ),
        errors,
    )

    create = body(sources, TRANSPORT, "create_instance", errors)
    require_order(
        TRANSPORT,
        "create_instance",
        create,
        (
            "reply_map.prepare_create_reply()",
            "pure::encode_set_reply_command_stream",
            "pure::encode_create_instance",
            "reply_map.read_create_reply()",
            "pure::validate_create_instance_reply",
            "evidence.opcode != pure::CMD_CREATE_INSTANCE",
            "evidence.status != 0",
            "HostInitEvidence",
        ),
        errors,
    )
    if compact(create).count(compact("submit_venus_session_sync")) != 2:
        errors.append(f"{TRANSPORT}: CREATE must use exactly two finite direct submissions")
    require_order(
        TRANSPORT,
        "create_instance",
        create,
        (
            "SESSION_SET_REPLY_FENCE",
            "SESSION_CREATE_INSTANCE_FENCE",
            "reply_map.read_create_reply()",
        ),
        errors,
    )
    if compact(
        "pure::encode_set_reply_command_stream("
        "reply_resource_id, SESSION_REPLY_OFFSET, "
        "pure::HOST_CREATE_INSTANCE_REPLY_BYTES,)"
    ) not in compact(create):
        errors.append(f"{TRANSPORT}: SET_REPLY does not name the exact private SHM resource")

    target = body(sources, LOGIC, "encode_set_reply_command_stream", errors)
    require_order(
        LOGIC,
        "encode_set_reply_command_stream",
        target,
        (
            "CMD_SET_REPLY_COMMAND_STREAM_MESA",
            "reply_resource_id",
            "reply_offset",
            "reply_bytes",
        ),
        errors,
    )
    encode = body(sources, LOGIC, "encode_create_instance", errors)
    require_order(
        LOGIC,
        "encode_create_instance",
        encode,
        (
            "CMD_CREATE_INSTANCE",
            "CMD_FLAG_GENERATE_REPLY",
            "ST_INSTANCE_CREATE_INFO",
            "instance_handle",
        ),
        errors,
    )
    if "CMD_CREATE_INSTANCE" in target or "CMD_SET_REPLY_COMMAND_STREAM_MESA" in encode:
        errors.append(f"{LOGIC}: reply-target setup and CREATE must remain separate streams")
    validate = body(sources, LOGIC, "validate_create_instance_reply", errors)
    require_order(
        LOGIC,
        "validate_create_instance_reply",
        validate,
        (
            "opcode != CMD_CREATE_INSTANCE",
            "status != 0",
            "pointer_count != 1",
            "instance != expected_instance",
            "HostInitEvidence { opcode, status }",
        ),
        errors,
    )
    calls = sum(
        live_rust(source).count("submit_venus_session_sync(")
        for path, source in sources.items()
        if path.endswith(".rs")
    )
    if calls != 4:  # one definition, SET_REPLY, CREATE, DESTROY
        errors.append(f"K11 finite direct-submit call surface drifted: found {calls}, expected 4")


def check_capacity_and_publish(sources: dict[str, str], errors: list[str]) -> None:
    admit = body(sources, LOGIC, "admit_reply_range", errors)
    require_order(
        LOGIC,
        "admit_reply_range",
        admit,
        (
            "reply_capacity == 0",
            "reply_offset % slot_bytes != 0",
            "reply_capacity > slot_bytes",
            "reply_offset.checked_add(reply_capacity)",
            "reply_end > pool_bytes",
            "payload_offset",
            "final_end > reply_end",
            "Ok(ReplyRange",
        ),
        errors,
    )
    native_proto = compact(live_rust(sources.get(PROTO_NATIVE, "")))
    for exact in (
        "pubconstHELIOS_HVM1_REPLY_POOL_BYTES:u64=4*1024*1024;",
        "pubconstHELIOS_HVM1_REPLY_SLOT_BYTES:u64=1024*1024;",
        "pubconstHELIOS_HVR1_MAX_SNAPSHOT_BYTES:u64=64*1024*1024;",
    ):
        if exact not in native_proto:
            errors.append(f"{PROTO_NATIVE}: K11 physical/logical reply bound drifted: {exact}")

    complete = body(sources, LOGIC, "complete_init", errors)
    require_order(
        LOGIC,
        "complete_init",
        complete,
        (
            "reply.validate",
            "let mut endpoints = [SessionEndpoint::UNASSIGNED; ENDPOINT_SLOTS]",
            "while i < admission.endpoint_capacity as usize",
            "endpoints[i].ring_index = i as u32 + 1",
            "self.endpoint_capacity = admission.endpoint_capacity",
            "self.endpoints = endpoints",
            "self.phase = SessionPhase::Live",
        ),
        errors,
    )

    reject_init = body(sources, SESSION, "reject_session_init", errors)
    require_order(
        SESSION,
        "reject_session_init",
        reject_init,
        (
            "obj.begin_draining_once",
            "TS_INIT_REJECT.fetch_add",
            "Err(status)",
        ),
        errors,
    )

    abort_slot = body(sources, LOGIC, "abort_slot", errors)
    require_order(
        LOGIC,
        "abort_slot",
        abort_slot,
        (
            "self.pool.as_mut()",
            "slot.state != SlotState::InFlight",
            "slot.generation != slot_generation",
            "retired_generation: slot_generation",
            "..ReplySlot::IDLE",
            "Ok(())",
        ),
        errors,
    )

    cancel = body(sources, SESSION, "abort_control_slot", errors)
    require_order(
        SESSION,
        "abort_control_slot",
        cancel,
        (
            ".abort_slot(slot_index, slot_generation)",
            "TS_SLOT_RELEASED.fetch_add",
            "TS_SLOT_STUCK.fetch_add",
        ),
        errors,
    )

    finish_failed = body(sources, SESSION, "finish_failed_session_init", errors)
    require_fragments(
        SESSION,
        "finish_failed_session_init",
        finish_failed,
        ("obj.teardown",),
        errors,
    )

    session_init = body(sources, SESSION, "session_init", errors)
    require_order(
        SESSION,
        "session_init",
        session_init,
        (
            "try_pod_read_unaligned::<HeliosTranslationSessionInitV1>",
            "obj.model.lock().admit_init(&record)",
            "obj.transport.initialize",
            "mint_capability()",
            "SESSION_GENERATION.lock().mint()",
            "snapshot_generations.lock().mint()",
            ".complete_init(requested, requested, generation, capability)",
            "opcode: host.opcode",
            "status: host.status",
            "flags: helios_protocol::native_render::HELIOS_HVR1_FLAG_FINAL",
            "SessionTransport::publish_hvr1",
            "TS_INIT_OK.fetch_add",
        ),
        errors,
    )
    publish = body(sources, TRANSPORT, "publish_hvr1", errors)
    require_order(
        TRANSPORT,
        "publish_hvr1",
        publish,
        (
            "reply_offset % HELIOS_HVM1_REPLY_SLOT_BYTES != 0",
            "reply_capacity < needed",
            "reply_capacity > HELIOS_HVM1_REPLY_SLOT_BYTES",
            "end > facts.byte_size",
            "write_unaligned(dst.cast::<u32>(), 0)",
            "copy_nonoverlapping(payload.as_ptr()",
            "unpublished.magic = 0",
            "dst.cast::<AtomicU32>()",
            "magic.store(HELIOS_HVR1_MAGIC.to_le(), Ordering::Release)",
            "K11_REPLY_PUBLISHED.fetch_add",
        ),
        errors,
    )


def check_allowlist_and_completion(sources: dict[str, str], errors: list[str]) -> None:
    commit = body(sources, NATIVE, "commit", errors)
    require_order(
        NATIVE,
        "commit",
        commit,
        (
            "let k11_init = native.class == NativeClass::Control",
            "header.fragment_count == 1",
            "HeliosTranslationSessionInitV1",
            "native.class == NativeClass::Control && !accept.has_reply",
            "native.class == NativeClass::Control && !k11_init",
            "staging_mut().checkout(header.total_payload_bytes)",
            "let prepared_executor = if native.class == NativeClass::Queue",
            "publish_dma_record",
            "let status = control_render",
            "mark_dma_host_completed(args, header.batch_token)",
        ),
        errors,
    )
    require_fragments(
        NATIVE,
        "commit",
        commit,
        (
            "if native.class == NativeClass::Control { let status = control_render",
        ),
        errors,
    )
    control = body(sources, NATIVE, "control_render", errors)
    require_order(
        NATIVE,
        "control_render",
        control,
        (
            "let outcome = run_control_payload",
            "ControlPayloadOutcome::Published",
            "release_control_slot",
            "ControlPayloadOutcome::Refused",
            "abort_control_slot",
            "ControlPayloadOutcome::InitFailed",
            "abort_control_slot(session, slot_index, admission.slot_generation);",
            "finish_failed_session_init",
            "outcome.status()",
        ),
        errors,
    )
    payload = body(sources, NATIVE, "run_control_payload", errors)
    require_order(
        NATIVE,
        "run_control_payload",
        payload,
        (
            "header.fragment_count != 1",
            "header.total_payload_bytes != INIT_BYTES as u64",
            "copy_from_command",
            "translation_session::session_init",
            "admission.reply_offset",
            "admission.reply_capacity_bytes",
            "admission.slot_generation",
            "admission.batch_token",
        ),
        errors,
    )
    submit = body(sources, NATIVE, "submit", errors)
    control_marker = submit.find(
        "if record.flags != HELIOS_HNR2_KMD_DMA_FLAG_HOST_COMPLETED"
    )
    control_submit = submit[control_marker:] if control_marker >= 0 else ""
    require_order(
        NATIVE,
        "submit",
        control_submit,
        (
            "record.flags != HELIOS_HNR2_KMD_DMA_FLAG_HOST_COMPLETED",
            "NR2_NO_HOST.fetch_add(1, Ordering::Relaxed)",
            "with_current_host_submission(",
            "let fence = submit.SubmissionFenceId",
            ".admit_host_completion(fence, resubmission)",
            "NR2_HOST_SUBMIT_OK.fetch_add",
            "NativeSubmitDisposition::HostCompleted(fence)",
        ),
        errors,
    )
    if "submit_venus_session_sync" in submit:
        errors.append(f"{NATIVE}:submit: DISPATCH_LEVEL submit started host work")

    borrow_pair = body(sources, CTRL, "borrow_venus_session_pair", errors)
    require_order(
        CTRL,
        "borrow_venus_session_pair",
        borrow_pair,
        (
            "borrow_session_pair(owner, reply_resource_id, context_id)",
            "VenusSessionGuard",
        ),
        errors,
    )
    direct = body(sources, CTRL, "submit_venus_session_sync", errors)
    require_order(
        CTRL,
        "submit_venus_session_sync",
        direct,
        (
            "control_fence_id == 0 || stream.is_empty()",
            "u32::try_from(stream.len())",
            "VIRTIO_GPU_CMD_SUBMIT_3D",
            "cmd.hdr.flags = VIRTIO_GPU_FLAG_FENCE | VIRTIO_GPU_FLAG_INFO_RING_IDX",
            "cmd.hdr.ctx_id = session.context_id",
            "cmd.hdr.fence_id = control_fence_id",
            "cmd.hdr.ring_idx = 0",
            "ctrl_roundtrip_ok_finite(passive, adapter, bytes_of(&cmd), Some(stream))",
        ),
        errors,
    )
    for shared_timeline in (
        "next_wire_fence",
        "new_fence_id",
        "alloc_fence_id",
        "SubmissionFenceId",
    ):
        if shared_timeline in direct:
            errors.append(
                f"{CTRL}:submit_venus_session_sync: K11 used a shared or WDDM fence source: "
                f"{shared_timeline}"
            )

    finite = body(sources, CTRL, "ctrl_roundtrip_ok_finite", errors)
    require_fragments(
        CTRL,
        "ctrl_roundtrip_ok_finite",
        finite,
        (
            "CtrlRoundtripMode::FiniteEvent",
        ),
        errors,
    )
    for wrapper, inner in (
        ("ctx_create_session", "ctx_create_mode"),
        ("ctx_detach_session_resource", "ctx_detach_resource_mode"),
        ("ctx_destroy_session", "ctx_destroy_mode"),
    ):
        wrapper_body = body(sources, CTRL, wrapper, errors)
        require_fragments(
            CTRL,
            wrapper,
            wrapper_body,
            (inner, "CtrlRoundtripMode::FiniteEvent"),
            errors,
        )

    resource_create = body(sources, CTRL, "resource_create_session_reply_blob", errors)
    require_order(
        CTRL,
        "resource_create_session_reply_blob",
        resource_create,
        (
            "VIRTIO_GPU_BLOB_MEM_HOST3D",
            "VIRTIO_GPU_BLOB_FLAG_USE_MAPPABLE",
            "0,",
            "size,",
            "&[]",
            "Some(owner)",
            "ResourceBackingFinalizer::none()",
            "CtrlRoundtripMode::FiniteEvent",
        ),
        errors,
    )
    if re.search(r"USE_(?:SHAREABLE|CROSS_DEVICE)", resource_create):
        errors.append(f"{CTRL}: private K11 reply resource became shareable")
    for wrapper in (
        "map_session_reply_blob",
        "unmap_session_reply_blob",
        "resource_unref_session_reply",
    ):
        wrapper_body = body(sources, CTRL, wrapper, errors)
        require_order(
            CTRL,
            wrapper,
            wrapper_body,
            (
                "resource_owned_by(Some(owner), context_id, resource_id)",
                "CtrlRoundtripMode::FiniteEvent",
            ),
            errors,
        )

    roundtrip = body(sources, CTRL, "ctrl_roundtrip_observed", errors)
    require_fragments(
        CTRL,
        "ctrl_roundtrip_observed",
        roundtrip,
        (
            "if mode == CtrlRoundtripMode::LegacyRetry { v.drain_used(adapter); } let queued",
            "if mode == CtrlRoundtripMode::FiniteEvent { return CtrlRoundtripOutcome::DefiniteNotEnqueued(VirtioError::QueueFull); } if budget.charge_slice()",
            "CtrlRoundtripMode::LegacyRetry => wait_block(passive, adapter, block, timeout_ms)",
            "CtrlRoundtripMode::FiniteEvent => wait_block_once(passive, block, timeout_ms)",
            "CtrlRoundtripMode::FiniteEvent => { adapter.with_virtio(|v| v.abandon_sync(token, block.as_ptr())) }",
        ),
        errors,
    )
    roundtrip_live = live_rust(roundtrip)
    if (
        roundtrip_live.count("drain_used(adapter)") != 2
        or roundtrip_live.count("sleep_ms(passive, RETRY_SLICE_MS)") != 1
    ):
        errors.append(
            f"{CTRL}:ctrl_roundtrip_observed: K11 finite mode gained a used-ring poll or sleep"
        )
    once = body(sources, CTRL, "wait_block_once", errors)
    require_fragments(
        CTRL,
        "wait_block_once",
        once,
        ("KeWaitForSingleObject", "== STATUS_SUCCESS"),
        errors,
    )
    if once.count("KeWaitForSingleObject") != 1 or re.search(
        r"\b(?:loop|while|for|sleep_ms|drain_used|reap_parked)\b", live_rust(once)
    ):
        errors.append(f"{CTRL}:wait_block_once: K11 event wait gained polling, retry, or sleep")

    state = body(sources, LOGIC, "admit_host_completion", errors)
    require_fragments(
        LOGIC,
        "admit_host_completion",
        state,
        (
            "if !self.observed { self.observed = true; self.last_fence = fence; return Ok(()); }",
            "if fence == self.last_fence { return if resubmission { Ok(()) } else { Err(HostSubmissionRefusal::Duplicate { fence }) }; }",
            "if !fence_is_forward(self.last_fence, fence)",
            "self.last_fence = fence; Ok(())",
        ),
        errors,
    )
    native_live = compact(live_rust(sources.get(NATIVE, "")))
    for fragment in (
        "host_submissions:SpinLock<HostSubmissionState>",
        "host_submissions:SpinLock::new(HostSubmissionState::new())",
    ):
        if compact(fragment) not in native_live:
            errors.append(f"{NATIVE}: context-local SubmissionFenceId state missing: {fragment}")

    submit_ddi = body(sources, SUBMIT, "dxgkddi_submit_command", errors)
    require_order(
        SUBMIT,
        "dxgkddi_submit_command",
        submit_ddi,
        (
            "let disposition = adapter.with_k11_completion(||",
            "guard.admit_ordered_engine_submission(fence)",
            "native_render::submit(native, session, submit, ticket)",
            "NativeSubmitDisposition::HostCompleted(",
            "if exact_fence == fence",
            "complete_k11_host_submission(adapter, ticket)",
            "fail_ordered_engine_submission(",
            "NativeSubmitDisposition::Revoked",
            "Some((disposition, ticket))",
            "NativeSubmitDisposition::HostCompleted(_)",
            "NativeSubmitDisposition::Pending, _",
            "NativeSubmitDisposition::Revoked, _",
            "NativeSubmitDisposition::Refused, ticket",
            "note_and_maybe_signal(adapter, fence, is_paging, None, Some(ticket))",
        ),
        errors,
    )
    revoked = re.search(
        r"Some\(\(\s*[^\n]*NativeSubmitDisposition::Revoked\s*,\s*_\s*\)\)"
        r"\s*\|\s*None\s*=>\s*\{(?P<body>.*?)\n\s*\}",
        live_rust(submit_ddi),
        re.S,
    )
    revoked_body = revoked.group("body") if revoked else ""
    if (
        revoked is None
        or "SubmitAck::Accepted" not in revoked_body
        or re.search(
            r"note_and_maybe_signal|note_wddm_submission|signal_dma_completed",
            revoked_body,
        )
    ):
        errors.append(
            f"{SUBMIT}:dxgkddi_submit_command: revoked K11 work gained a completion fallback"
        )
    complete = body(sources, SUBMIT, "complete_k11_host_submission", errors)
    require_fragments(
        SUBMIT,
        "complete_k11_host_submission",
        complete,
        (
            "super::interrupt::complete_ordered_engine_submission(adapter, ticket)",
        ),
        errors,
    )
    complete_live = live_rust(complete)
    if compact(
        "fn complete_k11_host_submission(adapter: &AdapterContext, ticket: crate::adapter::OrderedEngineTicket,"
    ) not in compact(live_rust(sources.get(SUBMIT, ""))):
        errors.append(
            f"{SUBMIT}:complete_k11_host_submission: exact K9 ticket parameter missing"
        )
    if "with_wddm_notify_lock" in complete_live:
        errors.append(
            f"{SUBMIT}:complete_k11_host_submission: exact completion reacquired the notification lock"
        )
    if re.search(
        r"\b(?:signal_dma_completed|note_wddm_submission|note_and_maybe_signal|wddm_pending|sleep\w*|poll\w*|timer\w*|rebase\w*)\b",
        live_rust(complete),
        re.I,
    ):
        errors.append(f"{SUBMIT}:complete_k11_host_submission: K11 completion bypassed K9 or gained a queue, poll, timer, or synthetic fallback")

    completion_admit = body(sources, TRANSPORT, "acquire_owned", errors)
    require_order(
        TRANSPORT,
        "acquire_owned",
        completion_admit,
        (
            "!state.open || state.active == u32::MAX",
            "state.active == 0",
            "KeClearEvent(self.drained.get())",
            "state.active += 1",
            "K11CompletionOperation",
            "owner: NonNull::from(self)",
        ),
        errors,
    )
    completion_close = body(sources, TRANSPORT, "close_completion_and_wait", errors)
    require_order(
        TRANSPORT,
        "close_completion_and_wait",
        completion_close,
        (
            "state.open = false",
            "state.active",
            "if active == 0",
            "K11_COMPLETION_WAITED.fetch_add",
            "KeWaitForSingleObject",
        ),
        errors,
    )
    if completion_close.count("KeWaitForSingleObject") != 1 or re.search(
        r"\b(?:loop|while|for|sleep\w*|poll\w*)\b", live_rust(completion_close)
    ):
        errors.append(
            f"{TRANSPORT}:close_completion_and_wait: adapter completion rundown gained polling, retry, or sleep"
        )
    transport_live = compact(live_rust(sources.get(TRANSPORT, "")))
    for fragment in (
        "pub(crate)structK11CompletionRundown{state:SpinLock<CompletionRundownState>,drained:UnsafeCell<KEVENT>,}",
        "implDropforK11CompletionOperation",
        "state.active=state.active.saturating_sub(1);",
        "KeSetEvent(owner.drained.get(),0,0)",
    ):
        if compact(fragment) not in transport_live:
            errors.append(f"{TRANSPORT}: fixed adapter completion rundown missing: {fragment}")
    adapter_live = compact(live_rust(sources.get(ADAPTER, "")))
    for fragment in (
        "k11_completion:crate::ddi::session_transport::K11CompletionRundown",
        "self.k11_completion.with_admitted(operation)",
        "self.k11_completion.close_completion_and_wait(passive)",
        "self.k11_completion.reopen()",
    ):
        if compact(fragment) not in adapter_live:
            errors.append(f"{ADAPTER}: K11 completion rundown seam missing: {fragment}")
    if "self.k11_completion.init_event()" not in compact(
        live_rust(sources.get(ADAPTER_KOBJ, ""))
    ):
        errors.append(f"{ADAPTER_KOBJ}: K11 completion event is not initialized in place")

    selected = "\n".join(
        live_rust(sources.get(path, "")) for path in (TRANSPORT, SESSION)
    )
    forbidden = (
        r"(?i)\b(?:pid|process_id|processid|getcurrentprocessid)\b",
        r"(?i)\b(?:ioctl\w*|hpm1\w*|qemu\w*|virglrenderer\w*|sysctl\w*|modprobe\w*)\b",
        r"(?i)\b(?:zwopenfile|zwcreatefile|regopenkey|regqueryvalue|regsetvalue)\b",
        r"(?i)\b(?:poll\w*|sleep\w*|delayexecution\w*)\b",
        r"(?i)\b(?:find_session|lookup_session|global_session|session_by_name)\b",
        r"\b(?:signal_dma_completed|SubmissionFenceId)\b",
    )
    for pattern in forbidden:
        if re.search(pattern, selected):
            errors.append(f"K11 gained forbidden discovery/carrier/poll/completion token: {pattern}")


def check_teardown(sources: dict[str, str], errors: list[str]) -> None:
    cleanup = body(sources, TRANSPORT, "cleanup_live_host", errors)
    require_order(
        TRANSPORT,
        "cleanup_live_host",
        cleanup,
        (
            "current_transport(adapter)",
            "borrow_venus_session_pair",
            "pure::encode_destroy_instance",
            "submit_venus_session_sync",
            "SESSION_DESTROY_INSTANCE_FENCE",
            "drop(pair)",
            "cleanup_session_resource",
        ),
        errors,
    )
    if "ctx_destroy_session(" in cleanup:
        errors.append(
            f"{TRANSPORT}:cleanup_live_host: host VkInstance must retire before context destroy"
        )
    cleanup_context = body(sources, TRANSPORT, "cleanup_session_resource", errors)
    require_order(
        TRANSPORT,
        "cleanup_session_resource",
        cleanup_context,
        (
            "unmap_session_reply_blob",
            "drop(reply_map)",
            "if !unmap_terminal",
            "ctx_detach_session_resource",
            "resource_unref_session_reply",
            "cleanup_empty_context",
        ),
        errors,
    )
    if cleanup_context.count("quarantine_failed_cleanup") != 3:
        errors.append(f"{TRANSPORT}: every private-resource cleanup refusal must quarantine")
    cleanup_empty = body(sources, TRANSPORT, "cleanup_empty_context", errors)
    require_order(
        TRANSPORT,
        "cleanup_empty_context",
        cleanup_empty,
        (
            "ctx_destroy_session",
            "K11_CONTEXT_DESTROYED.fetch_add",
            "quarantine_failed_cleanup",
        ),
        errors,
    )
    begin_context_destroy = body(sources, OWNER, "begin_context_destroy", errors)
    require_order(
        OWNER,
        "begin_context_destroy",
        begin_context_destroy,
        (
            "context_handle_by_id(context_id)",
            "context_owner(row)",
            "close_context_admission(row)",
            "begin_context_destroy(row)",
            "dispatch_context(prepared)",
        ),
        errors,
    )
    quarantine = body(sources, TRANSPORT, "quarantine_failed_cleanup", errors)
    require_order(
        TRANSPORT,
        "quarantine_failed_cleanup",
        quarantine,
        (
            "K11_CLEANUP_REJECT.fetch_add",
            "quarantine_session_context(owner, context_id)",
        ),
        errors,
    )
    owner_quarantine = body(sources, OWNER, "quarantine_session_context", errors)
    require_order(
        OWNER,
        "quarantine_session_context",
        owner_quarantine,
        (
            "context_handle_by_id(context_id)",
            "context_owner(context)",
            "owner != Some(owner)",
            "table.quarantine()",
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
            "if let HostState::Live(host) = old",
            "self.cleanup_live_host",
        ),
        errors,
    )
    fail = body(sources, TRANSPORT, "fail_initialization", errors)
    require_order(
        TRANSPORT,
        "fail_initialization",
        fail,
        ("HostState::Draining", "self.rundown.lock().open = false"),
        errors,
    )
    close = body(sources, SESSION, "close_reply_pool_binding", errors)
    require_order(
        SESSION,
        "close_reply_pool_binding",
        close,
        (
            "binding_matches(allocation)",
            "obj.teardown",
            "release_k11_reply_pool_session",
            "SessionObject::release",
        ),
        errors,
    )
    drain = body(sources, SESSION, "drain_all", errors)
    require_order(
        SESSION,
        "drain_all",
        drain,
        (
            "let mut pending = [None; SESSION_SLOTS]",
            "ptr.as_ref() }.acquire()",
            "drop(slots)",
            "obj.teardown(passive)",
            "SessionObject::release",
        ),
        errors,
    )
    destroy_device = body(sources, DEVICE, "dxgkddi_destroy_device", errors)
    require_order(
        DEVICE,
        "dxgkddi_destroy_device",
        destroy_device,
        (
            "d.session.lock().take()",
            "release_device_session(session, list)",
            "release_blobs_for_owner",
            "destroy_contexts_for_owner",
        ),
        errors,
    )
    require_fragments(
        DEVICE,
        "dxgkddi_destroy_device",
        destroy_device,
        (
            "let blobs = crate::virtio::ctrl::release_blobs_for_owner(passive, adapter, device_owner);",
        ),
        errors,
    )
    if destroy_device.count("release_blobs_for_owner") != 1:
        errors.append(
            f"{DEVICE}:dxgkddi_destroy_device: physical pool release must occur exactly once"
        )

    skipped_stop = body(sources, LIFECYCLE, "retire_skipped_stop_transport", errors)
    require_order(
        LIFECYCLE,
        "retire_skipped_stop_transport",
        skipped_stop,
        (
            "crate::adapter::allocation_object::invalidate_all()",
            "adapter.close_k11_completions_and_wait(passive)",
            "adapter.close_control_owner_transport()",
            "adapter.retire_control_owner_transport(passive)",
        ),
        errors,
    )
    start = body(sources, LIFECYCLE, "dxgkddi_start_device", errors)
    start_compact = compact(start)
    if (
        start_compact.count(compact("adapter.reopen_k11_completions()")) != 1
        or start_compact.rfind(compact("adapter.set_transport_generation(Some"))
        > start_compact.find(compact("adapter.reopen_k11_completions()"))
    ):
        errors.append(
            f"{LIFECYCLE}:dxgkddi_start_device: K11 completion admission did not open once after transport publication"
        )
    stop = body(sources, LIFECYCLE, "dxgkddi_stop_device", errors)
    require_order(
        LIFECYCLE,
        "dxgkddi_stop_device",
        stop,
        (
            "crate::adapter::allocation_object::invalidate_all()",
            "adapter.close_k11_completions_and_wait(passive_stop)",
            "adapter.close_control_owner_transport()",
            "adapter.retire_control_owner_transport(passive_stop)",
            "adapter.remove_virtio_and_reset_scanout_bind_generation(passive_stop)",
        ),
        errors,
    )
    remove = body(sources, LIFECYCLE, "dxgkddi_remove_device", errors)
    require_order(
        LIFECYCLE,
        "dxgkddi_remove_device",
        remove,
        (
            "crate::adapter::allocation_object::invalidate_all()",
            "adapter.close_k11_completions_and_wait(passive_remove)",
            "adapter.close_control_owner_transport()",
            "adapter.retire_control_owner_transport(passive_remove)",
            "adapter.remove_virtio_and_reset_scanout_bind_generation(passive_remove)",
        ),
        errors,
    )
    reset = body(sources, SUBMIT, "dxgkddi_reset_from_timeout", errors)
    require_order(
        SUBMIT,
        "dxgkddi_reset_from_timeout",
        reset,
        (
            "crate::adapter::allocation_object::invalidate_all()",
            "adapter.close_k11_completions_and_wait(passive)",
            "abandon_pending_submissions(adapter, AbandonOutcome::Silent)",
            "adapter.close_control_owner_transport()",
            "adapter.retire_control_owner_transport(passive)",
            "adapter.remove_virtio_and_reset_scanout_bind_generation(passive)",
        ),
        errors,
    )
    restart = body(sources, SUBMIT, "dxgkddi_restart_from_timeout", errors)
    require_order(
        SUBMIT,
        "dxgkddi_restart_from_timeout",
        restart,
        (
            "adapter.set_reset_venus_context(venus_context)",
            "adapter.reopen_k11_completions()",
            "native_fence::resume_after_reset(adapter)",
        ),
        errors,
    )
    reset_engine = body(sources, SCHEDULER, "dxgkddi_reset_engine", errors)
    require_order(
        SCHEDULER,
        "dxgkddi_reset_engine",
        reset_engine,
        (
            "adapter.close_k11_completions_and_wait(passive)",
            "abandon_pending_submissions",
            "purge_all_present_streams_ordered",
            "adapter.reopen_k11_completions()",
        ),
        errors,
    )
    destroy_process = body(sources, DEVICE, "dxgkddi_destroy_process", errors)
    require_order(
        DEVICE,
        "dxgkddi_destroy_process",
        destroy_process,
        (
            "Box::from_raw(h_process as *mut ProcessContext)",
            "process.sessions.drain_all()",
            "drop(process)",
        ),
        errors,
    )
    borrow = body(sources, OWNER, "borrow_session_pair", errors)
    require_order(
        OWNER,
        "borrow_session_pair",
        borrow,
        (
            "context_handle_by_id(context_id)",
            "context_owner(context)",
            "owner != Some(owner)",
            "resource_handle_by_id(resource_id)",
            "borrow_pair_use(resource, context)",
        ),
        errors,
    )
    owner_live = compact(live_rust(sources.get(OWNER, "")))
    if owner_live.count("table.return_pair_use(lease).is_ok()") != 1:
        errors.append(f"{OWNER}: exact K11 pair-use lease is not returned once")


def check_probe_and_claims(sources: dict[str, str], errors: list[str]) -> None:
    probe = compact(live_c(sources.get(PROBE, "")))
    required = (
        "vn_helios_translation_session.h",
        "#include\"../icd/mesa/src/virtio/vulkan/vn_helios_translation_session.c\"",
        "helios_translation_session_create(luid,4,&first)",
        "helios_translation_session_create(luid,2,&second)",
        "session_host_init_evidence(first)",
        "session_host_init_evidence(second)",
        "reply.opcode==VENUS_WIRE_VK_CREATE_INSTANCE_OPCODE",
        "reply.status==0",
        "reply.slot_generation==session->slots[0].generation",
        "one.generation!=two.generation",
        "one.capability_low!=two.capability_low",
        "try_attach(luid,&one,1)==STATUS_SUCCESS_NT",
        "crossed.capability_low=two.capability_low",
        "try_attach(luid,&crossed,2)!=STATUS_SUCCESS_NT",
        "run_abrupt_child(&one)",
        "ExitProcess(0)",
        "strcmp(argv[1],\"--hold-reset\")==0",
        "K11_HOLD_READY",
        "K11_HOLD_DRAINED",
        "stale==STATUS_SUCCESS_NT",
        "REPEAT_SESSIONS",
        "created>=expected",
        "created==destroyed",
        "created==initialized",
        "initialized==published",
        "delta(before.host_reply_reject,after.host_reply_reject)==0",
        "delta(before.stale_transport,after.stale_transport)==0",
        "delta(before.cleanup_reject,after.cleanup_reject)==0",
        "read_counters(&before,true)",
        "read_counters(&after,false)",
        "query.Type=KMTQAITYPE_UMDRIVERNAME",
        "umd.Version=KMTUMDVERSION_DX11",
        "returnmatches==1",
    )
    for fragment in required:
        if compact(fragment) not in probe:
            errors.append(f"{PROBE}: K11 runtime acceptance shape missing: {fragment}")

    roadmap = sources.get(ROADMAP, "")
    false_claim = re.compile(
        r"(?is)K11.{0,180}(?:INIT|Code\s*0|counter|hash).{0,180}"
        r"(?:proves?|establishes?|demonstrates?|claims?).{0,80}"
        r"(?:visible|desktop|DWM\s+admission)"
    )
    if false_claim.search(roadmap):
        errors.append(f"{ROADMAP}: K11 evidence was promoted into a visible-desktop claim")


def check_gate_integration(sources: dict[str, str], errors: list[str]) -> None:
    gates = sources.get(RETIREMENT_GATES, "")
    invocations = re.findall(
        r'(?m)^\s*python3\s+"\$REPO/tools/k11-session-transport-gate\.py"\s+"\$REPO"(?:\s+--mutations)?\s*$',
        gates,
    )
    if len(invocations) != 1 or "--mutations" not in invocations[0]:
        errors.append(
            f"{RETIREMENT_GATES}: K11 source+mutation gate must be integrated once"
        )


def check_local_sources(sources: dict[str, str]) -> list[str]:
    errors: list[str] = []
    check_abi(sources, errors)
    check_fixed_ownership(sources, errors)
    check_host_init(sources, errors)
    check_capacity_and_publish(sources, errors)
    check_allowlist_and_completion(sources, errors)
    check_teardown(sources, errors)
    check_probe_and_claims(sources, errors)
    check_gate_integration(sources, errors)
    return errors


def check_sources(sources: dict[str, str]) -> list[str]:
    inherited = {path: source for path, source in sources.items() if path not in EXTRA_SOURCES}
    errors = [f"K2a: {error}" for error in k2a_check_sources(inherited)]
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
        Mutation("share one transport globally", SESSION, "transport: SessionTransport,", "transport: &'static SessionTransport,"),
        Mutation("remove exact reply-pool claim", ALLOC, "compare_exchange(\n            0,\n            session.as_ptr() as usize,", "compare_exchange(\n            ctx.k11_session_binding.load(Ordering::Relaxed),\n            session.as_ptr() as usize,"),
        Mutation("reuse adapter Venus context", TRANSPORT, "let context_id = match crate::virtio::ctrl::ctx_create_session(", "let context_id = match Ok(adapter.venus_ctx_id()) /* no ctx_create */ .and_then(|id| Ok(id)) {"),
        Mutation("skip private SHM reply creation", TRANSPORT, "let reply_resource_id = match crate::virtio::ctrl::resource_create_session_reply_blob(", "let reply_resource_id = match Ok(facts.resource_id) /* K2a is not a renderer reply target */ .and_then(|id| Ok(id)) {"),
        Mutation("make private reply resource shareable", CTRL, "VIRTIO_GPU_BLOB_FLAG_USE_MAPPABLE,\n        0,", "VIRTIO_GPU_BLOB_FLAG_USE_MAPPABLE | helios_protocol::VIRTIO_GPU_BLOB_FLAG_USE_SHAREABLE,\n        0,"),
        Mutation("select K2a as SET_REPLY target", TRANSPORT, "pure::encode_set_reply_command_stream(\n            reply_resource_id,\n            SESSION_REPLY_OFFSET,", "pure::encode_set_reply_command_stream(\n            self.k2a_resource_id,\n            SESSION_REPLY_OFFSET,"),
        Mutation("combine SET_REPLY with CREATE", LOGIC, "pub fn encode_create_instance(instance_handle: u64) -> Writer {\n        let mut stream = Writer::new();", "pub fn encode_create_instance(instance_handle: u64) -> Writer {\n        let mut stream = Writer::new();\n        stream.header(CMD_SET_REPLY_COMMAND_STREAM_MESA, 0);"),
        Mutation("add PID discovery", TRANSPORT, "let _operation = self.acquire()", "let process_id = 1u32;\n        let _operation = self.acquire()"),
        Mutation("add global namespace", TRANSPORT, "const VENUS_CAPSET_ID: u32 = 4;", "static GLOBAL_SESSION_CONTEXT: AtomicU32 = AtomicU32::new(0);\nconst VENUS_CAPSET_ID: u32 = 4;"),
        Mutation("zero-capacity success", LOGIC, "if reply_capacity == 0 {", "if false {"),
        Mutation("publish endpoint capacity zero", SESSION, ".complete_init(requested, requested, generation, capability)", ".complete_init(requested, 0, generation, capability)"),
        Mutation("publish capacity before ring ownership", LOGIC, "self.endpoint_capacity = admission.endpoint_capacity;\n            self.endpoints = endpoints;", "self.endpoints = endpoints;\n            self.endpoint_capacity = admission.endpoint_capacity;"),
        Mutation("assign control ring to endpoint", LOGIC, "endpoints[i].ring_index = i as u32 + 1;", "endpoints[i].ring_index = i as u32;"),
        Mutation("publish model before host create", TRANSPORT, "let evidence = match self.create_instance(", "*self.state.lock() = HostState::Live(LiveHost { allocation, resource_id: facts.resource_id, transport_instance: facts.transport_instance, context_id, instance_handle: SESSION_INSTANCE_HANDLE });\n        let evidence = match self.create_instance("),
        Mutation(
            "leave a failed INIT reusable",
            SESSION,
            "// `finish_failed_session_init` drains and destroys the host namespace.\n    obj.begin_draining_once();",
            "// `finish_failed_session_init` drains and destroys the host namespace.\n    let _ = obj;",
        ),
        Mutation(
            "destroy failed INIT before cancelling its reply slot",
            NATIVE,
            "hts1::abort_control_slot(session, slot_index, admission.slot_generation);\n            hts1::finish_failed_session_init(session);",
            "hts1::finish_failed_session_init(session);\n            hts1::abort_control_slot(session, slot_index, admission.slot_generation);",
        ),
        Mutation("allow arbitrary control payload", NATIVE, "if native.class == NativeClass::Control && !k11_init {", "if false {"),
        Mutation("run queue payload as control", NATIVE, "if native.class == NativeClass::Control {\n        let status = control_render", "if true {\n        let status = control_render"),
        Mutation("poll host reply", TRANSPORT, "core::sync::atomic::fence(Ordering::Acquire);", "poll_host_reply();\n        core::sync::atomic::fence(Ordering::Acquire);"),
        Mutation(
            "mark host completion before host call",
            NATIVE,
            "        let status = control_render(\n"
            "            session,\n"
            "            args,\n"
            "            header,\n"
            "            accept,\n"
            "            &scratch.uses[..use_count],\n"
            "            list_count,\n"
            "        );",
            "        unsafe { mark_dma_host_completed(args, header.batch_token) };\n"
            "        let status = control_render(\n"
            "            session,\n"
            "            args,\n"
            "            header,\n"
            "            accept,\n"
            "            &scratch.uses[..use_count],\n"
            "            list_count,\n"
            "        );",
        ),
        Mutation("retry a full K11 control queue", CTRL, "        None,\n        CtrlRoundtripMode::FiniteEvent,\n", "        None,\n        CtrlRoundtripMode::LegacyRetry,\n"),
        Mutation("retry K11 context lifecycle", CTRL, "        Some(owner),\n        CtrlRoundtripMode::FiniteEvent,\n    )\n}\n\nfn ctx_create_mode", "        Some(owner),\n        CtrlRoundtripMode::LegacyRetry,\n    )\n}\n\nfn ctx_create_mode"),
        Mutation("poll K11 with adaptive drains", CTRL, "            CtrlRoundtripMode::FiniteEvent => wait_block_once(passive, block, timeout_ms),\n", "            CtrlRoundtripMode::FiniteEvent => wait_block(passive, adapter, block, timeout_ms),\n"),
        Mutation("poll used ring before K11 enqueue", CTRL, "                if mode == CtrlRoundtripMode::LegacyRetry {\n                    v.drain_used(adapter);\n                }\n", "                v.drain_used(adapter);\n"),
        Mutation("poll used ring after K11 timeout", CTRL, "                CtrlRoundtripMode::FiniteEvent => {\n                    adapter.with_virtio(|v| v.abandon_sync(token, block.as_ptr()))\n                }\n", "                CtrlRoundtripMode::FiniteEvent => adapter.with_virtio(|v| {\n                    v.drain_used(adapter);\n                    v.abandon_sync(token, block.as_ptr())\n                }),\n"),
        Mutation("drop K11 host-terminal fence", CTRL, "    cmd.hdr.flags = VIRTIO_GPU_FLAG_FENCE | VIRTIO_GPU_FLAG_INFO_RING_IDX;\n", "    cmd.hdr.flags = 0;\n"),
        Mutation("drop K11 context timeline identity", CTRL, "    cmd.hdr.flags = VIRTIO_GPU_FLAG_FENCE | VIRTIO_GPU_FLAG_INFO_RING_IDX;\n", "    cmd.hdr.flags = VIRTIO_GPU_FLAG_FENCE;\n"),
        Mutation("move K11 pure control off ring zero", CTRL, "    cmd.hdr.ring_idx = 0;\n", "    cmd.hdr.ring_idx = 1;\n"),
        Mutation("forge K11 control fence from WDDM", CTRL, "    cmd.hdr.fence_id = control_fence_id;\n", "    cmd.hdr.fence_id = SubmissionFenceId as u64;\n"),
        Mutation("route K11 through adapter boundary queue", SUBMIT, "                        complete_k11_host_submission(adapter, ticket);\n", "                        let _ = note_and_maybe_signal(adapter, exact_fence, false, None, Some(ticket));\n"),
        Mutation(
            "forge a later K11 SubmissionFenceId",
            SUBMIT,
            "                    let ticket = adapter.with_wddm_notify_lock(|guard| {\n"
            "                        guard.admit_ordered_engine_submission(fence)\n"
            "                    })?;\n",
            "                    let ticket = adapter.with_wddm_notify_lock(|guard| {\n"
            "                        guard.admit_ordered_engine_submission(fence.wrapping_add(1))\n"
            "                    })?;\n",
        ),
        Mutation("complete revoked K11 work through legacy queue", SUBMIT, "                Some((crate::ddi::native_render::NativeSubmitDisposition::Revoked, _))\n                | None => {\n                    // The host-completed marker belonged to a session whose\n                    // exact transport/fence authority was revoked before this\n                    // callback, or reset already closed the adapter completion\n                    // epoch. Do not forge completion through the legacy queue.\n                    SubmitAck::Accepted\n                }\n", "                Some((crate::ddi::native_render::NativeSubmitDisposition::Revoked, ticket))\n                | None => {\n                    note_and_maybe_signal(adapter, fence, is_paging, None, Some(ticket))\n                }\n"),
        Mutation("skip current session generation at submit", NATIVE, "    let disposition = crate::ddi::translation_session::with_current_host_submission(\n", "    let disposition = Some(\n"),
        Mutation("drop adapter completion rundown", SUBMIT, "                .with_k11_completion(|| {\n", "                .with_k11_completion_unchecked(|| {\n"),
        Mutation("drop exact K11 completion after admission", SUBMIT, "                        complete_k11_host_submission(adapter, ticket);\n", "                        let _ = (exact_fence, ticket);\n"),
        Mutation("make completion rundown unbounded", TRANSPORT, "    state: SpinLock<CompletionRundownState>,\n", "    state: SpinLock<Vec<CompletionRundownState>>,\n"),
        Mutation("skip TDR completion drain", SUBMIT, "    adapter.close_k11_completions_and_wait(passive);\n", "    let _ = passive;\n"),
        Mutation("skip StopDevice completion drain", LIFECYCLE, "        adapter.close_k11_completions_and_wait(passive_stop);\n", "        let _ = passive_stop;\n"),
        Mutation("drop context-local submission state", NATIVE, "    host_submissions: SpinLock<HostSubmissionState>,\n", "    host_submissions: &'static SpinLock<HostSubmissionState>,\n"),
        Mutation("bypass context-local fence admission", NATIVE, "        .admit_host_completion(fence, resubmission);\n", "        .admit_host_completion(fence, true);\n"),
        Mutation("unbounded endpoint array", SESSION, "endpoints: [SessionEndpointObject; model::ENDPOINT_SLOTS],", "endpoints: Vec<SessionEndpointObject>,"),
        Mutation("unbounded process sessions", SESSION, "slots: SpinLock<[Option<NonNull<SessionObject>>; SESSION_SLOTS]>,", "slots: SpinLock<Vec<Option<NonNull<SessionObject>>>>,"),
        Mutation("release reply pool before revoke", SESSION, "if !obj.transport.binding_matches(allocation) {", "let _ = crate::ddi::create_allocation::release_k11_reply_pool_session(allocation, session);\n    if !obj.transport.binding_matches(allocation) {"),
        Mutation("leave failed K11 cleanup open to legacy sweep", TRANSPORT, ".quarantine_session_context(owner, context_id);", ".resource_is_live(context_id);"),
        Mutation("add ABI host context id", PROTO_SESSION, "pub reserved: u64,\n}\n\n/// Why an HTS1 INIT", "pub reserved: u64,\n    pub host_context_id: u64,\n}\n\n/// Why an HTS1 INIT"),
        Mutation("restore Escape", LIB, "data.DxgkDdiSetAllocationBackingStore = Some(ddi::dxgkddi_set_allocation_backing_store);", "data.DxgkDdiSetAllocationBackingStore = Some(ddi::dxgkddi_set_allocation_backing_store);\n    data.DxgkDdiEscape = Some(ddi::dxgkddi_escape);"),
        Mutation("add IOCTL fallback", TRANSPORT, "let _operation = self.acquire()", "let ioctl = 1u32;\n        let _operation = self.acquire()"),
        Mutation("add HPM1 dependency", TRANSPORT, "const VENUS_CAPSET_ID: u32 = 4;", "const HPM1_REQUIRED: bool = true;\nconst VENUS_CAPSET_ID: u32 = 4;"),
        Mutation("add QEMU dependency", TRANSPORT, "const VENUS_CAPSET_ID: u32 = 4;", "const QEMU_PATCH_REQUIRED: bool = true;\nconst VENUS_CAPSET_ID: u32 = 4;"),
        Mutation("weaken K2a alias publication", ALLOC, "ctx.backing_store_state\n        .store(BACKING_STORE_BOUND, Ordering::Release);", "ctx.backing_store_state\n        .store(BACKING_STORE_BOUND, Ordering::Relaxed);"),
        Mutation(
            "weaken HVR1 release publication",
            TRANSPORT,
            "                payload.as_ptr(),\n"
            "                dst.add(header_bytes as usize),\n"
            "                payload.len(),\n"
            "            );\n"
            "            let mut unpublished = *header;\n"
            "            unpublished.magic = 0;\n"
            "            core::ptr::copy_nonoverlapping(\n"
            "                bytemuck::bytes_of(&unpublished).as_ptr(),\n"
            "                dst,\n"
            "                header_bytes as usize,\n"
            "            );\n"
            "        }\n"
            "        // K2a mappings and 1-MiB slot offsets are page aligned, so the magic\n"
            "        // word satisfies AtomicU32's alignment. This release store is the exact\n"
            "        // publication edge for every header/payload byte copied above.\n"
            "        let magic = unsafe { &*dst.cast::<AtomicU32>() };\n"
            "        magic.store(HELIOS_HVR1_MAGIC.to_le(), Ordering::Release);",
            "                payload.as_ptr(),\n"
            "                dst.add(header_bytes as usize),\n"
            "                payload.len(),\n"
            "            );\n"
            "            let mut unpublished = *header;\n"
            "            unpublished.magic = 0;\n"
            "            core::ptr::copy_nonoverlapping(\n"
            "                bytemuck::bytes_of(&unpublished).as_ptr(),\n"
            "                dst,\n"
            "                header_bytes as usize,\n"
            "            );\n"
            "        }\n"
            "        // K2a mappings and 1-MiB slot offsets are page aligned, so the magic\n"
            "        // word satisfies AtomicU32's alignment. This release store is the exact\n"
            "        // publication edge for every header/payload byte copied above.\n"
            "        let magic = unsafe { &*dst.cast::<AtomicU32>() };\n"
            "        magic.store(HELIOS_HVR1_MAGIC.to_le(), Ordering::Relaxed);",
        ),
        Mutation("forge host reply opcode", SESSION, "opcode: host.opcode,", "opcode: 0,"),
        Mutation("bypass renderer reply validation", TRANSPORT, "pure::validate_create_instance_reply(\n            &raw_reply,\n            SESSION_INSTANCE_HANDLE,\n        )", "Ok(pure::HostInitEvidence { opcode: 0, status: 0 })"),
        Mutation("drop pair-use rundown", CTRL, "let pair = adapter\n        .control_owner()\n        .borrow_session_pair(owner, reply_resource_id, context_id)?;", "let pair = ();"),
        Mutation("release pair before HVR1 publication", TRANSPORT, "let result = match publish(facts, &evidence) {", "drop(pair);\n        let result = match publish(facts, &evidence) {"),
        Mutation("destroy context before host instance", TRANSPORT, "let destroy = pure::encode_destroy_instance(instance_handle);", "let _ = crate::virtio::ctrl::ctx_destroy_session(passive, adapter, owner, context_id);\n            let destroy = pure::encode_destroy_instance(instance_handle);"),
        Mutation("leave K11 context attachment admission open", OWNER, "        table\n            .close_context_admission(row)\n            .map_err(owner_refusal)?;\n", ""),
        Mutation("release physical pool early", DEVICE, "let blobs = crate::virtio::ctrl::release_blobs_for_owner(passive, adapter, device_owner);", "let _ = crate::virtio::ctrl::release_blobs_for_owner(passive, adapter, device_owner);\n        let blobs = 0;"),
        Mutation("restore 64 MiB physical pool", PROTO_NATIVE, "pub const HELIOS_HVM1_REPLY_POOL_BYTES: u64 = 4 * 1024 * 1024;", "pub const HELIOS_HVM1_REPLY_POOL_BYTES: u64 = 64 * 1024 * 1024;"),
        Mutation("shrink logical snapshot ceiling", PROTO_NATIVE, "pub const HELIOS_HVR1_MAX_SNAPSHOT_BYTES: u64 = 64 * 1024 * 1024;", "pub const HELIOS_HVR1_MAX_SNAPSHOT_BYTES: u64 = 4 * 1024 * 1024;"),
        Mutation("hide post-run counter read in a comment", PROBE, "if (read_counters(&after, false)) {", "if (true) { /* read_counters(&after, false) */"),
        Mutation("claim visible desktop from INIT", ROADMAP, "# ROADMAP — Stage: Correctness and D3D12 (since 2026-08-05)", "# ROADMAP — Stage: Correctness and D3D12 (since 2026-08-05)\n\nK11 INIT counters prove a visible desktop."),
    )


def run_mutations(sources: dict[str, str]) -> None:
    for case in mutation_cases():
        count = sources.get(case.path, "").count(case.old)
        if count != 1:
            raise SystemExit(
                f"K11 mutation setup failed for {case.name}: expected one anchor "
                f"in {case.path}, found {count}"
            )
        mutated = dict(sources)
        mutated[case.path] = mutated[case.path].replace(case.old, case.new, 1)
        errors = check_local_sources(mutated)
        if not errors:
            inherited = {
                path: source for path, source in mutated.items() if path not in EXTRA_SOURCES
            }
            errors.extend(f"K2a: {error}" for error in k2a_check_sources(inherited))
        if not errors:
            raise SystemExit(f"K11 mutation was accepted by the real gate: {case.name}")
    print(f"OK: {len(mutation_cases())} in-memory K11 mutations rejected by the real gate")


def main() -> None:
    args = sys.argv[1:]
    mutations = False
    if "--mutations" in args:
        mutations = True
        args.remove("--mutations")
    if len(args) > 1:
        raise SystemExit("usage: k11-session-transport-gate.py [repo] [--mutations]")
    repo = os.path.abspath(args[0]) if args else REPO_DEFAULT
    sources = load_sources(repo)
    errors = check_sources(sources)
    if errors:
        raise SystemExit("K11 per-session host transport gate violated:\n" + "\n".join(errors))
    if mutations:
        run_mutations(sources)
    else:
        print(
            "OK: K11 distinct stock-Venus sessions, finite INIT reply, direct ownership, "
            "and reverse-order teardown source invariants hold"
        )


if __name__ == "__main__":
    main()
