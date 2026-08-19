#!/usr/bin/env python3
"""K2a ShareBackingStoreWithKmd source and temporary-tree mutation gate."""

from __future__ import annotations

import os
import re
import runpy
import sys
from dataclasses import dataclass


REPO_DEFAULT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
D9 = runpy.run_path(
    os.path.join(os.path.dirname(__file__), "d9-wddm32-activation-gate.py"),
    run_name="k2a_imported_d9_gate",
)
load_d9_sources = D9["load_sources"]
d9_check_sources = D9["check_sources"]
live_rust = D9["live_rust"]
unique_function = D9["unique_function"]

ADAPTER = "kmd_render/src/adapter/mod.rs"
LIFECYCLE = "kmd_render/src/ddi/lifecycle.rs"
ALLOC = "kmd_render/src/ddi/create_allocation.rs"
DDI_MOD = "kmd_render/src/ddi/mod.rs"
LIB = "kmd_render/src/lib.rs"
SEH = "kmd_render/src/seh_shim.c"
OWNER = "kmd_render/src/virtio/control_owner.rs"
CTRL = "kmd_render/src/virtio/ctrl.rs"
CLASSES = "kmd_render/tools/wddm32_slot_classes.tsv"
AUDIT_RS = "kmd_render/src/ddi/wddm32_slot_audit.rs"
AUDIT_MD = "docs/retirement/d9-wddm32-slot-audit.md"
PROTO_GPU = "protocol/src/virtio_gpu.rs"
PROTO_NATIVE = "protocol/src/native_render.rs"
PROTO_HEADER = "protocol/include/helios_native_render.h"
MESA = "icd/mesa/src/virtio/vulkan/vn_helios_translation_session.c"
HTS_PROBE = "tools/hts1_session_probe.c"
HNR_PROBE = "tools/hnr2_native_probe.c"
RETIREMENT_GATES = "tools/retirement-gates.sh"
QEMU_VIRGL = "qemu-helios/hw/display/virtio-gpu-virgl.c"
QEMU_GPU = "qemu-helios/hw/display/virtio-gpu.c"
QEMU_UDMABUF = "qemu-helios/hw/display/virtio-gpu-udmabuf.c"
QEMU_UDMABUF_STUB = "qemu-helios/hw/display/virtio-gpu-udmabuf-stubs.c"
QEMU_HEADER = "qemu-helios/include/hw/virtio/virtio-gpu.h"
QEMU_TRACE = "qemu-helios/hw/display/trace-events"
WDK_HEADER = "kmd_render/tools/wdk-28000/km/dispmprt.h"
K2A_PROBE = "tools/k2a_shared_backing_probe.c"

EXTRA_SOURCES = (
    SEH,
    MESA,
    HTS_PROBE,
    HNR_PROBE,
    QEMU_VIRGL,
    QEMU_GPU,
    QEMU_UDMABUF,
    QEMU_UDMABUF_STUB,
    QEMU_HEADER,
    QEMU_TRACE,
    K2A_PROBE,
    PROTO_HEADER,
)


def compact(source: str) -> str:
    return re.sub(r"\s+", "", source)


def load_sources(repo: str) -> dict[str, str]:
    sources = load_d9_sources(repo)
    for relative in EXTRA_SOURCES:
        with open(os.path.join(repo, relative), encoding="utf-8", errors="replace") as stream:
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
            errors.append(f"{path}:{name}: required K2a fragment missing: {fragment}")


def require_order(
    path: str, name: str, source: str, tokens: tuple[str, ...], errors: list[str]
) -> None:
    value = compact(source)
    positions = [value.find(compact(token)) for token in tokens]
    if any(position < 0 for position in positions) or positions != sorted(positions):
        errors.append(f"{path}:{name}: K2a order drifted: {' -> '.join(tokens)}")


def check_interface(sources: dict[str, str], errors: list[str]) -> None:
    copy = body(sources, ADAPTER, "copy_dxgkrnl_interface", errors)
    require_order(
        ADAPTER,
        "copy_dxgkrnl_interface",
        copy,
        (
            "offset_of!(DXGKRNL_INTERFACE, DxgkCbQueryFeatureSupport)",
            "size_of::<DXGKCB_QUERYFEATURESUPPORT>()",
            "let supplied = unsafe { (*source).Size as usize }",
            "if supplied < REQUIRED",
            "Box::<DXGKRNL_INTERFACE>::new_uninit()",
            "let bytes = supplied.min(core::mem::size_of::<DXGKRNL_INTERFACE>())",
            "core::ptr::write_bytes",
            "core::ptr::copy_nonoverlapping(source as *const u8",
            "copy.assume_init()",
        ),
        errors,
    )
    copy_live = compact(copy)
    if "dxgkrnl:unsafe{*source}" in copy_live or "copy_nonoverlapping(source,copy.as_mut_ptr(),1)" in copy_live:
        errors.append(f"{ADAPTER}: DXGKRNL_INTERFACE is copied beyond its supplied Size")

    admitted = body(sources, LIFECYCLE, "share_backing_store_admitted", errors)
    require_fragments(
        LIFECYCLE,
        "share_backing_store_admitted",
        admitted,
        (
            "let Some(query) = dxgkrnl.DxgkCbQueryFeatureSupport else { return false; }",
            "DXGK_FEATURE_SHARE_BACKING_STORE_WITH_KMD",
            "DriverSupportState = DXGK_FEATURE_SUPPORT_STABLE_VALUE",
            "let status = unsafe { query(&mut args) }",
            "status == STATUS_SUCCESS",
            "args.DeviceHandle == dxgkrnl.DeviceHandle",
            "args.FeatureId as u32",
            "args.DriverSupportState == DXGK_FEATURE_SUPPORT_STABLE_VALUE",
            "args.Enabled == 1",
        ),
        errors,
    )
    start = body(sources, LIFECYCLE, "dxgkddi_start_device", errors)
    require_order(
        LIFECYCLE,
        "dxgkddi_start_device",
        start,
        (
            "StartedState::copy_dxgkrnl_interface(dxgkrnl_interface)",
            "share_backing_store_admitted(&dxgkrnl)",
            "if !share_backing_store_with_kmd",
            "VirtioGpu::init",
            "StartedState::boxed(dxgkrnl, share_backing_store_with_kmd",
        ),
        errors,
    )
    forbidden = (
        "DxgkCbCreatePhysicalMemoryObject",
        "DxgkCbMapPhysicalMemory",
        "DxgkCbUnmapPhysicalMemory",
        "DxgkCbDestroyPhysicalMemoryObject",
    )
    selected = "\n".join(
        live_rust(sources.get(path, "")) for path in (ADAPTER, LIFECYCLE, ALLOC, CTRL)
    )
    for symbol in forbidden:
        if symbol in selected:
            errors.append(f"K2a restored the superseded physical-memory callback path: {symbol}")

    header_match = re.search(
        r"typedef struct _DXGKRNL_INTERFACE\s*\{(.*?)\}\s*DXGKRNL_INTERFACE",
        sources.get(WDK_HEADER, ""),
        re.S,
    )
    callback_order = (
        re.findall(r"\b(DxgkCb[A-Za-z0-9_]+)\s*;", header_match.group(1))
        if header_match
        else []
    )
    used_callbacks: set[str] = set()
    for path, source in sources.items():
        if path.startswith("kmd_render/src/") and path.endswith(".rs"):
            used_callbacks.update(re.findall(r"\bDxgkCb[A-Za-z0-9_]+\b", live_rust(source)))
    if "DxgkCbQueryFeatureSupport" not in callback_order:
        errors.append(f"{WDK_HEADER}: cannot derive DXGKRNL_INTERFACE callback order")
    else:
        covered = set(callback_order[: callback_order.index("DxgkCbQueryFeatureSupport") + 1])
        uncovered = sorted(used_callbacks - covered)
        if uncovered:
            errors.append(
                f"{ADAPTER}: callback reads exceed Size-proven prefix: {uncovered!r}"
            )


def check_allocation(sources: dict[str, str], errors: list[str]) -> None:
    placement = body(sources, ALLOC, "hvm1_placement", errors)
    require_fragments(
        ALLOC,
        "hvm1_placement",
        placement,
        (
            "let preferred = contract.preferred_segment",
            "preferred_segment: preferred",
            "supported_segments: segment_bit(preferred)",
            "cpu_visible: contract.cpu_visible",
        ),
        errors,
    )
    placement_live = compact(placement)
    if not re.search(
        r"\bsupported_segments\s*:\s*segment_bit\s*\(\s*preferred\s*\)\s*,",
        placement,
    ):
        errors.append(
            f"{ALLOC}:hvm1_placement: supported segment set is not exactly the preferred aperture"
        )
    for forbidden in ("hlm1_only", "hlm1_bind", "hlm1_flags_off", "HELIOS_SEGMENT_ID_HLM1"):
        if forbidden.lower() in placement_live.lower():
            errors.append(f"{ALLOC}:hvm1_placement: parked HLM1 authority restored: {forbidden}")

    admit = body(sources, ALLOC, "admit_hvm1", errors)
    require_fragments(
        ALLOC,
        "admit_hvm1",
        admit,
        ("if !shape.is_shared_single_resource_allocation()",),
        errors,
    )
    require_order(
        ALLOC,
        "admit_hvm1",
        admit,
        (
            "let cpu_visible = role.placement().cpu_visible",
            "if cpu_visible && !adapter.share_backing_store_with_kmd()",
            "if record.byte_size == 0",
            "record.byte_size & (PAGE as u64 - 1) != 0",
            "record.byte_size > u32::MAX as u64",
            "if cpu_visible && record.byte_size > HVM1_CPU_VISIBLE_MAX_BYTES",
            "let placement = hvm1_placement(role)",
            "allocation_object::mint()",
            "share_backing_store: cpu_visible",
            "BackingSize::SharedBackingStore(record.byte_size)",
        ),
        errors,
    )
    require_fragments(
        ALLOC,
        "admit_hvm1",
        admit,
        (
            "const HVM1_CPU_VISIBLE_MAX_BYTES: u64 = 1024 * PAGE as u64",
            "if matches!(role, Hvm1Role::VulkanDeviceLocal)",
            "allocate_device_local_memory_blob(adapter, record.byte_size)",
        ),
        errors,
    )
    create = body(sources, ALLOC, "create_one", errors)
    require_fragments(
        ALLOC,
        "create_one",
        create,
        (
            "AtomicU32::new(resource_id)",
            "AtomicU32::new(BACKING_STORE_UNBOUND)",
            "backing_store_va: AtomicUsize::new(0)",
            "k11_session_binding: AtomicUsize::new(0)",
            "set_ShareBackingStoreWithKmd(1)",
            "info.SupportedWriteSegmentSet = placement.supported_segments",
            "SupportedReadSegmentSet = placement.supported_segments",
        ),
        errors,
    )

    callback = body(sources, ALLOC, "dxgkddi_set_allocation_backing_store", errors)
    require_order(
        ALLOC,
        "dxgkddi_set_allocation_backing_store",
        callback,
        (
            "KeGetCurrentIrql()",
            "share_backing_store_with_kmd()",
            "resolve_alloc(args.hDriverAllocation)",
            "Hvm1Role::from_u32(ctx.hvm1_role)",
            "ctx.kind != ALLOC_KIND_HVM1",
            "!role.placement().cpu_visible",
            "allocation_object::is_current(ctx.generation)",
            "ctx.ctx_id != adapter.venus_ctx_id()",
            "current_transport != Some(ctx.transport_instance)",
            "matches!(ctx.size_provenance, BackingSize::SharedBackingStore(n) if n == bytes)",
            "bytes > (u32::MAX as u64 - (PAGE as u64 - 1))",
            "(args.pBackingStore as usize) & (PAGE as usize - 1) != 0",
            "entries.try_reserve_exact(page_count)",
            "compare_exchange(BACKING_STORE_UNBOUND, BACKING_STORE_BINDING",
            "IoAllocateMdl",
            "helios_mm_probe_and_lock_pages_seh(mdl)",
            "role == Hvm1Role::ReplyPool",
            "k2a_mdl_system_va(mdl, bytes)",
            "helios_mm_get_mdl_pfn_array(mdl)",
            "pfn > (u64::MAX >> HELIOS_HVM1_SEGMENT_PAGE_SHIFT)",
            "let end = last.addr.checked_add(last.length as u64)",
            "last.length <= u32::MAX - PAGE as u32",
            "sum.checked_add(entry.length as u64)",
            "exported != Some(bytes)",
            "VIRTIO_GPU_BLOB_FLAG_USE_MAPPABLE | VIRTIO_GPU_BLOB_FLAG_USE_SHAREABLE",
            "Hvm1Role::VulkanDeviceLocal",
            "resource_create_guest_blob",
            "ctx.resource_id.store(resource_id, Ordering::Release)",
            "ctx.backing_store_va.store(kernel_va, Ordering::Relaxed)",
            "ctx.backing_store_state.store(BACKING_STORE_BOUND, Ordering::Release)",
        ),
        errors,
    )
    require_fragments(
        ALLOC,
        "dxgkddi_set_allocation_backing_store",
        callback,
        (
            "return STATUS_INVALID_DEVICE_REQUEST",
            "return STATUS_NOT_SUPPORTED",
            "MmUnlockPages(mdl); IoFreeMdl(mdl)",
            "Hvm1Role::ReplyPool | Hvm1Role::Feedback => VIRTIO_GPU_BLOB_FLAG_USE_MAPPABLE",
        ),
        errors,
    )
    if "Err(error)=>{returnerror.into();}" not in compact(callback):
        errors.append(
            f"{ALLOC}:SetAllocationBackingStore: post-dispatch failure must remain terminal"
        )
    if compact(callback).count("ctx.resource_id.store(") != 1:
        errors.append(
            f"{ALLOC}:SetAllocationBackingStore: resource identity must publish exactly once after CREATE"
        )
    if compact(callback).count("ctx.backing_store_va.store(") != 1:
        errors.append(
            f"{ALLOC}:SetAllocationBackingStore: stable K2a CPU view must publish exactly once after CREATE"
        )
    if compact(callback).count(
        "ctx.backing_store_state.store(BACKING_STORE_BOUND,Ordering::Release)"
    ) != 1:
        errors.append(
            f"{ALLOC}:SetAllocationBackingStore: BOUND must release-publish the complete resource and CPU alias"
        )

    system_va = body(sources, ALLOC, "k2a_mdl_system_va", errors)
    require_order(
        ALLOC,
        "k2a_mdl_system_va",
        system_va,
        (
            "mdl.is_null()",
            "(*mdl).ByteCount",
            "(*mdl).MdlFlags",
            "(*mdl).MappedSystemVa",
            "MmMapLockedPagesSpecifyCache",
            "_MEMORY_CACHING_TYPE::MmCached",
            "K2A_MDL_MAP_PRIORITY",
        ),
        errors,
    )
    require_fragments(
        ALLOC,
        "k2a_mdl_system_va",
        system_va,
        (
            "MmMapLockedPagesSpecifyCache(mdl, 0, _MEMORY_CACHING_TYPE::MmCached, core::ptr::null_mut(), 0, K2A_MDL_MAP_PRIORITY,)",
        ),
        errors,
    )
    callback_live = compact(callback).lower()
    for forbidden in (
        "escape",
        "ioctl",
        "registry",
        "zwopenfile",
        "pid",
        "processid",
        "lookup",
        "poll",
        "hlm1bind",
        "hlm1only",
    ):
        if forbidden in callback_live:
            errors.append(f"{ALLOC}:SetAllocationBackingStore gained forbidden fallback {forbidden}")


def check_mdl_and_owner(sources: dict[str, str], errors: list[str]) -> None:
    seh = compact(sources.get(SEH, ""))
    for fragment in (
        "typedefcharhelios_mdl_size_must_be_48[(sizeof(MDL)==48)?1:-1];",
        "MmProbeAndLockPages(Mdl,/*KernelMode*/0,/*IoModifyAccess*/2);",
        "__except(EXCEPTION_EXECUTE_HANDLER)",
        "return(unsignedlonglong*)(Mdl+1);",
    ):
        if compact(fragment) not in seh:
            errors.append(f"{SEH}: locked-MDL proof missing: {fragment}")

    request = body(sources, CTRL, "create_blob_request", errors)
    require_fragments(
        CTRL,
        "create_blob_request",
        request,
        (
            "entries.is_empty()",
            "entries.len() > u32::MAX as usize",
            "entry.addr & 0xFFF != 0",
            "entry.length == 0",
            "entry.length & 0xFFF != 0",
            "entry.padding != 0",
            "total.checked_add(entry.length as u64)",
            "total != cmd.size",
            "cmd.nr_entries = entries.len() as u32",
            "request.extend_from_slice(entry_bytes)",
        ),
        errors,
    )
    guest = body(sources, CTRL, "resource_create_guest_blob", errors)
    require_order(
        CTRL,
        "resource_create_guest_blob",
        guest,
        (
            "if mdl == 0",
            "if !super::control_owner::KMD_D2_OWNER_ENABLED",
            "ResourceBackingFinalizer::guest_pages(mdl)",
            "VIRTIO_GPU_BLOB_MEM_GUEST",
            "blob_flags",
            "size",
            "entries",
        ),
        errors,
    )
    if compact(guest).count("ResourceBackingFinalizer::guest_pages(mdl)") != 2:
        errors.append(f"{CTRL}:resource_create_guest_blob: MDL custody must cover refusal and CREATE")
    release = body(sources, CTRL, "release_guest_pages", errors)
    require_order(
        CTRL,
        "release_guest_pages",
        release,
        (
            "if finalizer.guest_mdl == 0",
            "finalizer.guest_mdl = 0",
            "MmUnlockPages(mdl)",
            "IoFreeMdl(mdl)",
        ),
        errors,
    )
    finish = body(sources, OWNER, "finish_resource", errors)
    require_order(
        OWNER,
        "finish_resource",
        finish,
        (
            "finish_resource_work(observed)",
            "apply_resource_control(action)",
            "ResourceFinishEffect::UnrefCompleted",
            "begin_resource_payload_release(pending)",
            "finalize(backing.finalizer)",
            "ack_resource_payload_release(finalized)",
        ),
        errors,
    )
    reset = body(sources, OWNER, "finish_physical_reset", errors)
    require_order(
        OWNER,
        "finish_physical_reset",
        reset,
        (
            "verify_raw_zero(raw_status)",
            "authorize_reset(verified)",
            "next_reset_action()",
            "ResourceBackingFinalizer::none()",
            "finalize_resource_backing_after_reset(passive, finalizer,)",
            "assume_payloads_finalized()",
        ),
        errors,
    )
    reset_compact = compact(reset)
    assume = reset_compact.find("assume_payloads_finalized()")
    ack = reset_compact.find("ack_reset_action(finalized)", assume)
    quarantine = reset_compact.find("core::mem::take(&mutstate.finalizer_quarantine)", ack)
    quarantine_finalize = reset_compact.find(
        "finalize_resource_backing_after_reset(passive,finalizer)", quarantine
    )
    if min(assume, ack, quarantine, quarantine_finalize) < 0:
        errors.append(f"{OWNER}:finish_physical_reset: reset finalization/ack order drifted")
    if reset_compact.count("finalize_resource_backing_after_reset(") != 2:
        errors.append(f"{OWNER}:finish_physical_reset: canonical and quarantined MDLs must both drain")


def check_abi_and_user_mode(sources: dict[str, str], errors: list[str]) -> None:
    native = live_rust(sources.get(PROTO_NATIVE, ""))
    native_compact = compact(native)
    if "pub const HELIOS_HVM1_SIZE: u16 = 64;" not in native:
        errors.append(f"{PROTO_NATIVE}: HVM1 is no longer exactly 64 bytes")
    if "preferred_segment: HELIOS_SEGMENT_ID_APERTURE," not in native:
        errors.append(f"{PROTO_NATIVE}: HVM1 placement is not aperture-only")
    match = re.search(r"pub struct HeliosVenusMemoryAllocationV1\s*\{([^}}]*)\}", native, re.S)
    expected = {
        "magic", "abi_version", "struct_size", "package_generation",
        "object_generation", "byte_size", "role", "access", "cache_policy",
        "segment_page_shift", "allocation_alignment", "reserved",
    }
    fields = set(re.findall(r"pub\s+(\w+)\s*:", match.group(1))) if match else set()
    if fields != expected:
        errors.append(f"{PROTO_NATIVE}: HVM1 field set drifted: {sorted(fields)!r}")
    pointerish = {field for field in fields if re.search(r"ptr|pointer|handle|pid|resource|token|lookup", field, re.I)}
    if pointerish:
        errors.append(f"{PROTO_NATIVE}: identity carrier entered HVM1: {sorted(pointerish)!r}")

    for fragment in (
        "pub const HELIOS_HNR2_MAX_PAYLOAD_BYTES: u64 = 15 * 1024 * 1024;",
        "pub const HELIOS_HVM1_REPLY_POOL_BYTES: u64 = 4 * 1024 * 1024;",
        "pub const HELIOS_HVM1_REPLY_SLOT_BYTES: u64 = 1024 * 1024;",
        "pub const HELIOS_HVM1_REPLY_SLOT_COUNT: u32 = 4;",
        "HELIOS_HVM1_REPLY_SLOT_BYTES * HELIOS_HVM1_REPLY_SLOT_COUNT as u64 == HELIOS_HVM1_REPLY_POOL_BYTES",
        "pub const HELIOS_HVR1_MAX_SNAPSHOT_BYTES: u64 = 64 * 1024 * 1024;",
        "pub const HELIOS_HVR1_MAX_CHUNK_BYTES: u64 = HELIOS_HVM1_REPLY_SLOT_BYTES - HELIOS_HVR1_HEADER_SIZE as u64;",
        "HELIOS_HVR1_MAX_CHUNK_BYTES < HELIOS_HNR2_MAX_PAYLOAD_BYTES",
        "HELIOS_HVR1_HEADER_SIZE as u64 + HELIOS_HVR1_MAX_CHUNK_BYTES == HELIOS_HVM1_REPLY_SLOT_BYTES",
    ):
        if compact(fragment) not in native_compact:
            errors.append(f"{PROTO_NATIVE}: bounded 1-MiB reply geometry drifted: {fragment}")

    header = compact(sources.get(PROTO_HEADER, ""))
    for fragment in (
        "#define HELIOS_HNR2_MAX_PAYLOAD_BYTES UINT64_C(15728640)",
        "#define HELIOS_HVM1_REPLY_POOL_BYTES UINT64_C(4194304)",
        "#define HELIOS_HVM1_REPLY_SLOT_BYTES UINT64_C(1048576)",
        "#define HELIOS_HVM1_REPLY_SLOT_COUNT 4u",
        "#define HELIOS_HVR1_MAX_SNAPSHOT_BYTES UINT64_C(67108864)",
        "#define HELIOS_HVR1_MAX_CHUNK_BYTES UINT64_C(1048496)",
        "HELIOS_HVR1_MAX_CHUNK_BYTES < HELIOS_HNR2_MAX_PAYLOAD_BYTES",
        "(uint64_t)HELIOS_HVR1_HEADER_SIZE + HELIOS_HVR1_MAX_CHUNK_BYTES == HELIOS_HVM1_REPLY_SLOT_BYTES",
    ):
        if compact(fragment) not in header:
            errors.append(f"{PROTO_HEADER}: C mirror of 1-MiB reply geometry drifted: {fragment}")

    for path in (MESA, HTS_PROBE, HNR_PROBE):
        source = compact(sources.get(path, ""))
        for fragment in (
            "info.pSystemMem=NULL;",
            "ca2.Flags.CreateResource=1;" if path == MESA else "ca.Flags.CreateResource=1;",
            "ca2.Flags.CreateShared=1;" if path == MESA else "ca.Flags.CreateShared=1;",
            "ca2.Flags.NtSecuritySharing=1;" if path == MESA else "ca.Flags.NtSecuritySharing=1;",
        ):
            if compact(fragment) not in source:
                errors.append(f"{path}: shared allocation create shape missing: {fragment}")
    mesa = compact(sources.get(MESA, ""))
    unlock = mesa.find("D3DKMTUnlock2(&u)")
    destroy = mesa.find("D3DKMTDestroyAllocation2(&d)")
    if unlock < 0 or destroy < 0 or unlock >= destroy:
        errors.append(f"{MESA}: Lock2 view must be revoked before allocation destroy")
    for fragment in (
        "s->pool_resource=ca2.hResource;",
        "d.hResource=s->pool_resource;",
    ):
        if compact(fragment) not in mesa:
            errors.append(f"{MESA}: shared resource lifetime missing: {fragment}")

    for path in (HTS_PROBE, HNR_PROBE):
        source = compact(sources.get(path, ""))
        for fragment in ("pool_resource=ca.hResource;", "da.hResource=pool_resource;"):
            if compact(fragment) not in source:
                errors.append(f"{path}: shared resource lifetime missing: {fragment}")

    probe = compact(sources.get(K2A_PROBE, ""))
    for fragment in (
        "info.pSystemMem=NULL;",
        "create.Flags.CreateResource=1;",
        "create.Flags.CreateShared=1;",
        "create.Flags.NtSecuritySharing=1;",
        "out->resource=create.hResource;",
        "HELIOS_HVM1_ROLE_REPLY_POOL",
        "HELIOS_HVM1_ROLE_VULKAN_HOST_VISIBLE",
        "HELIOS_HVM1_ROLE_FEEDBACK",
        "HELIOS_HVM1_ROLE_VULKAN_DEVICE_LOCAL",
        "check(status!=STATUS_SUCCESS_NT,"
        '"role 4 is refused before a shared CPU view exists",status);',
        "D3DKMTUnlock2(&unlock)",
        "D3DKMTDestroyAllocation2(&destroy)",
        "destroy.hResource=allocation->resource;",
        '"--child-exit"',
        '"--foreign-handle"',
        '"allocation handle is rejected in another process"',
        '"destroyed allocation handle stays stale"',
        "return status == STATUS_SUCCESS_NT ? 1 : 0;",
        "ALIAS_HOST_FIRST",
        "ALIAS_HOST_LAST",
        "ALIAS_GUEST_FIRST",
        "ALIAS_GUEST_LAST",
    ):
        if compact(fragment) not in probe:
            errors.append(f"{K2A_PROBE}: runtime acceptance shape missing: {fragment}")
    probe_unlock = probe.find(compact("D3DKMTUnlock2(&unlock)"))
    probe_destroy = probe.find(compact("D3DKMTDestroyAllocation2(&destroy)"))
    if probe_unlock < 0 or probe_destroy < 0 or probe_unlock >= probe_destroy:
        errors.append(f"{K2A_PROBE}: Lock2 view must be revoked before allocation destroy")
    if probe.count(compact("D3DKMTDestroyAllocation2(")) != 2:
        errors.append(f"{K2A_PROBE}: unexpected allocation destroy path entered the probe")


def check_registration(sources: dict[str, str], errors: list[str]) -> None:
    lib = compact(live_rust(sources.get(LIB, "")))
    assignment = "data.DxgkDdiSetAllocationBackingStore=Some(ddi::dxgkddi_set_allocation_backing_store);"
    if lib.count(assignment) != 1:
        errors.append(f"{LIB}: SetAllocationBackingStore must be registered exactly once")
    ddi_mod = compact(live_rust(sources.get(DDI_MOD, "")))
    if ddi_mod.count("dxgkddi_set_allocation_backing_store") != 1:
        errors.append(f"{DDI_MOD}: SetAllocationBackingStore export drifted")
    classes = sources.get(CLASSES, "")
    row = "DxgkDdiSetAllocationBackingStore\tImplemented\t"
    if classes.count(row) != 1:
        errors.append(f"{CLASSES}: SetAllocationBackingStore is not terminal Implemented")


def check_qemu(sources: dict[str, str], errors: list[str]) -> None:
    virgl_source = sources.get(QEMU_VIRGL, "")
    virgl = compact(virgl_source)
    gpu = compact(sources.get(QEMU_GPU, ""))
    udmabuf = compact(sources.get(QEMU_UDMABUF, ""))
    for fragment in (
        "cblob.blob_mem==VIRTIO_GPU_BLOB_MEM_GUEST",
        "iov_size(res->base.iov,res->base.iov_cnt)!=cblob.size",
        "import_args.blob_mem=VIRGL_RENDERER_BLOB_MEM_GUEST_VRAM;",
        "import_args.fd_type=VIRGL_RENDERER_BLOB_FD_TYPE_DMABUF;",
        "virgl_renderer_resource_import_blob(&import_args)",
        "flags|=VIRGL_RENDERER_USE_GUEST_VRAM;",
        "DMA_DIRECTION_FROM_DEVICE:DMA_DIRECTION_TO_DEVICE",
        "trace_virtio_gpu_virgl_guest_blob_backing(cblob.resource_id,cblob.size,res->base.iov_cnt",
    ):
        if compact(fragment) not in virgl:
            errors.append(f"{QEMU_VIRGL}: guest-VRAM import proof missing: {fragment}")
    unref_match = re.search(
        r"static\s+int\s+virtio_gpu_virgl_resource_unref\b.*?(?=\nstatic\s)",
        virgl_source,
        re.S,
    )
    unref_body = compact(unref_match.group(0)) if unref_match else ""
    unref = unref_body.find("virgl_renderer_resource_unref(res->base.resource_id);")
    cleanup = unref_body.find(
        "virtio_gpu_cleanup_mapping_dir(g,&res->base,DMA_DIRECTION_FROM_DEVICE);",
        unref,
    )
    if unref < 0 or cleanup < 0 or cleanup < unref:
        errors.append(f"{QEMU_VIRGL}: renderer UNREF must precede guest DMA revocation")
    if unref_body.count(
        "virtio_gpu_cleanup_mapping_dir(g,&res->base,DMA_DIRECTION_FROM_DEVICE);"
    ) != 1:
        errors.append(f"{QEMU_VIRGL}: guest DMA revocation must occur exactly once")
    info_error = virgl.find("if(res->guest_udmabuf){virgl_renderer_resource_unref(cblob.resource_id);")
    if info_error < 0:
        errors.append(f"{QEMU_VIRGL}: post-import metadata failure revokes backing too early")
    for fragment in (
        "dma_memory_map(VIRTIO_DEVICE(g)->dma_as,a,&len,dir",
        "dma_memory_unmap(VIRTIO_DEVICE(g)->dma_as,iov[i].iov_base,iov[i].iov_len,dir",
        "virtio_gpu_cleanup_mapping_dir(g,res,DMA_DIRECTION_TO_DEVICE)",
    ):
        if compact(fragment) not in gpu:
            errors.append(f"{QEMU_GPU}: direction-matched DMA lifetime missing: {fragment}")
    for fragment in (
        "offset-list->list[count-1].offset==list->list[count-1].size",
        "res->iov[i].iov_len<=UINT64_MAX-list->list[count-1].size",
        "list->count=count",
        "returnfd;",
    ):
        if compact(fragment) not in udmabuf:
            errors.append(f"{QEMU_UDMABUF}: exact/coalesced udmabuf proof missing: {fragment}")
    qemu_joined = "\n".join(sources.get(path, "") for path in (QEMU_VIRGL, QEMU_GPU, QEMU_UDMABUF))
    for forbidden in ("HPM1", "Hlm1Bind", "D3DKMTEscape"):
        if forbidden in qemu_joined:
            errors.append(f"QEMU guest backing gained forbidden dependency {forbidden}")


def check_gate_integration(sources: dict[str, str], errors: list[str]) -> None:
    retirement = sources.get(RETIREMENT_GATES, "")
    static = re.findall(
        r'(?m)^\s*python3\s+"\$REPO/tools/k2a-share-backing-store-gate\.py"\s+"\$REPO"\s*$',
        retirement,
    )
    mutations = re.findall(
        r'(?m)^\s*python3\s+"\$REPO/tools/k2a-share-backing-store-gate\.py"\s+"\$REPO"\s+--mutations\s*$',
        retirement,
    )
    if len(static) != 1:
        errors.append(f"{RETIREMENT_GATES}: K2a static gate is not integrated exactly once")
    if len(mutations) != 1:
        errors.append(f"{RETIREMENT_GATES}: K2a mutation gate is not integrated exactly once")


def check_local_sources(sources: dict[str, str]) -> list[str]:
    errors: list[str] = []
    check_interface(sources, errors)
    check_allocation(sources, errors)
    check_mdl_and_owner(sources, errors)
    check_abi_and_user_mode(sources, errors)
    check_registration(sources, errors)
    check_qemu(sources, errors)
    check_gate_integration(sources, errors)
    return errors


def check_sources(sources: dict[str, str]) -> list[str]:
    errors = [f"D9: {error}" for error in d9_check_sources(sources)]
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
        Mutation("copy full interface", ADAPTER, "let bytes = supplied.min(core::mem::size_of::<DXGKRNL_INTERFACE>());", "let bytes = core::mem::size_of::<DXGKRNL_INTERFACE>();"),
        Mutation("accept short callback table", ADAPTER, "if supplied < REQUIRED {", "if supplied == 0 {"),
        Mutation("accept disabled feature", LIFECYCLE, "&& args.Enabled == 1", "&& args.Enabled == 0"),
        Mutation("bypass feature admission", LIFECYCLE, "if !share_backing_store_with_kmd {", "if false {"),
        Mutation("accept bare shared allocation", ALLOC, "if !shape.is_shared_single_resource_allocation() {", "if !shape.is_bare_single_allocation() {"),
        Mutation("restore HLM1 placement", ALLOC, "let preferred = contract.preferred_segment;", "let preferred = HELIOS_SEGMENT_ID_HLM1;"),
        Mutation("add foreign aperture segment", ALLOC, "supported_segments: segment_bit(preferred),", "supported_segments: segment_bit(preferred) | segment_bit(2),"),
        Mutation("map role 4", ALLOC, "|| !role.placement().cpu_visible", "|| false"),
        Mutation("accept unaligned size", ALLOC, "|| record.byte_size & (PAGE as u64 - 1) != 0", "|| false"),
        Mutation("omit shared backing bit", ALLOC, ".set_ShareBackingStoreWithKmd(1);", ".set_ShareBackingStoreWithKmd(0);"),
        Mutation("publish before guest create", ALLOC, "let resource_id = match crate::virtio::ctrl::resource_create_guest_blob(", "ctx.resource_id.store(1, Ordering::Release);\n    let resource_id = match crate::virtio::ctrl::resource_create_guest_blob("),
        Mutation("omit stable kernel alias", ALLOC, "ctx.backing_store_va.store(kernel_va, Ordering::Relaxed);", "let _ = kernel_va;"),
        Mutation("publish incomplete alias", ALLOC, "ctx.resource_id.store(resource_id, Ordering::Release);", "ctx.backing_store_state.store(BACKING_STORE_BOUND, Ordering::Release);\n    ctx.resource_id.store(resource_id, Ordering::Release);"),
        Mutation("weaken complete alias publication", ALLOC, "ctx.backing_store_state\n        .store(BACKING_STORE_BOUND, Ordering::Release);", "ctx.backing_store_state\n        .store(BACKING_STORE_BOUND, Ordering::Relaxed);"),
        Mutation("map wrong MDL extent", ALLOC, "k2a_mdl_system_va(mdl, bytes)", "k2a_mdl_system_va(mdl, bytes - 1)"),
        Mutation("map K2a alias noncached", ALLOC, "_MEMORY_CACHING_TYPE::MmCached", "_MEMORY_CACHING_TYPE::MmNonCached"),
        Mutation("retry ambiguous create", ALLOC, "Err(error) => {\n            return error.into();", "Err(error) => {\n            ctx.backing_store_state.store(BACKING_STORE_UNBOUND, Ordering::Release);\n            return error.into();"),
        Mutation("skip PASSIVE check", ALLOC, "if unsafe { KeGetCurrentIrql() } != crate::irql::PASSIVE_LEVEL_IRQL {", "if false {"),
        Mutation("skip exact allocation provenance", ALLOC, "if ctx.kind != ALLOC_KIND_HVM1\n        || !role.placement().cpu_visible", "if false\n        || !role.placement().cpu_visible"),
        Mutation("accept stale allocation generation", ALLOC, "|| ctx.resource_id() != 0\n        || !allocation_object::is_current(ctx.generation)", "|| ctx.resource_id() != 0\n        || false"),
        Mutation("accept stale transport generation", ALLOC, "|| current_transport != Some(ctx.transport_instance)", "|| false"),
        Mutation("skip exact shared size", ALLOC, "if !matches!(ctx.size_provenance, BackingSize::SharedBackingStore(n) if n == bytes)", "if false"),
        Mutation("overflow PFN address", ALLOC, "if pfn > (u64::MAX >> HELIOS_HVM1_SEGMENT_PAGE_SHIFT) {", "if false {"),
        Mutation("unchecked coalesced end", ALLOC, "let end = last.addr.checked_add(last.length as u64);", "let end = Some(last.addr + last.length as u64);"),
        Mutation("skip exact exported size", ALLOC, "if !valid || exported != Some(bytes) {", "if !valid {"),
        Mutation("remove SEH probe", ALLOC, "if unsafe { helios_mm_probe_and_lock_pages_seh(mdl) } == 0 {", "if false {"),
        Mutation("role2 not shareable", ALLOC, "VIRTIO_GPU_BLOB_FLAG_USE_MAPPABLE | VIRTIO_GPU_BLOB_FLAG_USE_SHAREABLE", "VIRTIO_GPU_BLOB_FLAG_USE_MAPPABLE"),
        Mutation("wrong blob memory", CTRL, "ctx_id,\n        VIRTIO_GPU_BLOB_MEM_GUEST,\n        blob_flags,", "ctx_id,\n        VIRTIO_GPU_BLOB_MEM_HOST3D,\n        blob_flags,"),
        Mutation("drop MDL finalizer", CTRL, "None,\n        ResourceBackingFinalizer::guest_pages(mdl),", "None,\n        ResourceBackingFinalizer::none(),"),
        Mutation("destroy before unmap", CTRL, "MmUnlockPages(mdl);\n        IoFreeMdl(mdl);", "IoFreeMdl(mdl);\n        MmUnlockPages(mdl);"),
        Mutation("release before host unref", OWNER, "ResourceFinishEffect::UnrefCompleted => table", "ResourceFinishEffect::CreateCompleted => table"),
        Mutation("release before verified reset", OWNER, "let verified = match unsafe { preparation.verify_raw_zero(raw_status) }", "crate::virtio::ctrl::finalize_resource_backing_after_reset(passive, ResourceBackingFinalizer::none());\n            let verified = match unsafe { preparation.verify_raw_zero(raw_status) }"),
        Mutation("change HVM1 size", PROTO_NATIVE, "pub const HELIOS_HVM1_SIZE: u16 = 64;", "pub const HELIOS_HVM1_SIZE: u16 = 72;"),
        Mutation("restore 64 MiB reply pool", PROTO_NATIVE, "pub const HELIOS_HVM1_REPLY_POOL_BYTES: u64 = 4 * 1024 * 1024;", "pub const HELIOS_HVM1_REPLY_POOL_BYTES: u64 = 64 * 1024 * 1024;"),
        Mutation("restore 16 MiB reply slot", PROTO_NATIVE, "pub const HELIOS_HVM1_REPLY_SLOT_BYTES: u64 = 1024 * 1024;", "pub const HELIOS_HVM1_REPLY_SLOT_BYTES: u64 = 16 * 1024 * 1024;"),
        Mutation("couple reply chunk to HNR2", PROTO_NATIVE, "HELIOS_HVM1_REPLY_SLOT_BYTES - HELIOS_HVR1_HEADER_SIZE as u64;", "HELIOS_HNR2_MAX_PAYLOAD_BYTES;"),
        Mutation("drift C reply slot mirror", PROTO_HEADER, "#define HELIOS_HVM1_REPLY_SLOT_BYTES UINT64_C(1048576)", "#define HELIOS_HVM1_REPLY_SLOT_BYTES UINT64_C(16777216)"),
        Mutation("protocol restores HLM1 placement", PROTO_NATIVE, "preferred_segment: HELIOS_SEGMENT_ID_APERTURE,", "preferred_segment: HELIOS_SEGMENT_ID_HLM1,"),
        Mutation("add pointer to HVM1", PROTO_NATIVE, "pub reserved: u64,", "pub reserved: u64,\n    pub user_pointer: u64,"),
        Mutation("remove Mesa resource create", MESA, "ca2.Flags.CreateResource = 1;", "ca2.Flags.CreateResource = 0;"),
        Mutation("remove Mesa shared create", MESA, "ca2.Flags.CreateShared = 1;", "ca2.Flags.CreateShared = 0;"),
        Mutation("restore Mesa global share", MESA, "ca2.Flags.NtSecuritySharing = 1;", "ca2.Flags.NtSecuritySharing = 0;"),
        Mutation("destroy Mesa allocation outside resource", MESA, "d.hResource = s->pool_resource;", "d.hResource = 0;"),
        Mutation("destroy before Unlock2", MESA, "HELIOS_IGNORE_STATUS(D3DKMTUnlock2(&u));", "HELIOS_IGNORE_STATUS(D3DKMTDestroyAllocation2((D3DKMT_DESTROYALLOCATION2 *)&u));"),
        Mutation("probe uses existing system memory", K2A_PROBE, "info.pSystemMem = NULL;", "info.pSystemMem = (void *)1;"),
        Mutation("probe omits resource create", K2A_PROBE, "create.Flags.CreateResource = 1;", "create.Flags.CreateResource = 0;"),
        Mutation("probe omits shared create", K2A_PROBE, "create.Flags.CreateShared = 1;", "create.Flags.CreateShared = 0;"),
        Mutation("probe restores global share", K2A_PROBE, "create.Flags.NtSecuritySharing = 1;", "create.Flags.NtSecuritySharing = 0;"),
        Mutation("probe destroys outside resource", K2A_PROBE, "destroy.hResource = allocation->resource;", "destroy.hResource = 0;"),
        Mutation(
            "probe accepts role 4",
            K2A_PROBE,
            'check(status != STATUS_SUCCESS_NT,\n         "role 4 is refused before a shared CPU view exists", status);',
            'check(status == STATUS_SUCCESS_NT,\n         "role 4 is refused before a shared CPU view exists", status);',
        ),
        Mutation("probe accepts a foreign-process handle", K2A_PROBE, "return status == STATUS_SUCCESS_NT ? 1 : 0;", "return status == STATUS_SUCCESS_NT ? 0 : 1;"),
        Mutation(
            "probe destroys before unlock",
            K2A_PROBE,
            "IGNORE_STATUS(D3DKMTUnlock2(&unlock));\n      allocation->cpu = NULL;",
            "IGNORE_STATUS(D3DKMTDestroyAllocation2((D3DKMT_DESTROYALLOCATION2 *)&unlock));\n      allocation->cpu = NULL;",
        ),
        Mutation("QEMU import wrong memory", QEMU_VIRGL, "import_args.blob_mem = VIRGL_RENDERER_BLOB_MEM_GUEST_VRAM;", "import_args.blob_mem = VIRGL_RENDERER_BLOB_MEM_HOST3D;"),
        Mutation("QEMU mislabels udmabuf", QEMU_VIRGL, "import_args.fd_type = VIRGL_RENDERER_BLOB_FD_TYPE_DMABUF;", "import_args.fd_type = VIRGL_RENDERER_BLOB_FD_TYPE_SHM;"),
        Mutation("QEMU map guest read-only", QEMU_VIRGL, "? DMA_DIRECTION_FROM_DEVICE : DMA_DIRECTION_TO_DEVICE,", "? DMA_DIRECTION_TO_DEVICE : DMA_DIRECTION_TO_DEVICE,"),
        Mutation("remove exact backing trace", QEMU_VIRGL, "trace_virtio_gpu_virgl_guest_blob_backing(\n            cblob.resource_id, cblob.size, res->base.iov_cnt,", "trace_virtio_gpu_cmd_res_create_blob(\n            cblob.resource_id, cblob.size);\n        if (false && res->base.iov_cnt)"),
        Mutation("QEMU revoke before unref", QEMU_VIRGL, "virgl_renderer_resource_unref(res->base.resource_id);\n\n    if (res->guest_udmabuf) {", "if (res->guest_udmabuf) {\n        virtio_gpu_cleanup_mapping_dir(g, &res->base, DMA_DIRECTION_FROM_DEVICE);\n    }\n    virgl_renderer_resource_unref(res->base.resource_id);\n\n    if (false) {"),
        Mutation("register Escape", LIB, "data.DxgkDdiSetAllocationBackingStore = Some(ddi::dxgkddi_set_allocation_backing_store);", "data.DxgkDdiSetAllocationBackingStore = Some(ddi::dxgkddi_set_allocation_backing_store);\n    data.DxgkDdiEscape = Some(ddi::dxgkddi_escape);"),
        Mutation("add forbidden lookup fallback", ALLOC, "let adapter = unsafe { &*(h_adapter as *const AdapterContext) };\n    if !adapter.share_backing_store_with_kmd() {", "let adapter = unsafe { &*(h_adapter as *const AdapterContext) };\n    let lookup_pid = 1u32;\n    if !adapter.share_backing_store_with_kmd() {"),
    )


def run_mutations(sources: dict[str, str]) -> None:
    for case in mutation_cases():
        count = sources.get(case.path, "").count(case.old)
        if count != 1:
            raise SystemExit(
                f"K2a mutation setup failed for {case.name}: expected one anchor "
                f"in {case.path}, found {count}"
            )
        mutated = dict(sources)
        mutated[case.path] = mutated[case.path].replace(case.old, case.new, 1)
        errors = check_local_sources(mutated)
        if not errors:
            errors.extend(f"D9: {error}" for error in d9_check_sources(mutated))
        if not errors:
            raise SystemExit(f"K2a mutation was accepted by the real gate: {case.name}")
    print(f"OK: {len(mutation_cases())} in-memory K2a mutations rejected by the real gate")


def main() -> None:
    args = sys.argv[1:]
    mutations = False
    if "--mutations" in args:
        mutations = True
        args.remove("--mutations")
    if len(args) > 1:
        raise SystemExit("usage: k2a-share-backing-store-gate.py [repo] [--mutations]")
    repo = os.path.abspath(args[0]) if args else REPO_DEFAULT
    sources = load_sources(repo)
    errors = check_sources(sources)
    if errors:
        raise SystemExit("K2a ShareBackingStoreWithKmd gate violated:\n" + "\n".join(errors))
    if mutations:
        run_mutations(sources)
    else:
        print(
            "OK: K2a feature admission, shared allocation, exact PFN export, "
            "guest-VRAM import, and reverse-order lifetime are statically enforced"
        )


if __name__ == "__main__":
    main()
