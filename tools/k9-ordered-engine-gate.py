#!/usr/bin/env python3
"""K9 one-engine ordered-completion source and in-memory mutation gate."""

from __future__ import annotations

import os
import re
import runpy
import sys
from dataclasses import dataclass


REPO_DEFAULT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
K11 = runpy.run_path(
    os.path.join(os.path.dirname(__file__), "k11-session-transport-gate.py"),
    run_name="k9_imported_k11_gate",
)
k11_load_sources = K11["load_sources"]
k11_check_sources = K11["check_sources"]
live_rust = K11["live_rust"]
unique_function = K11["unique_function"]

LOGIC_LIB = "kmd_logic/src/lib.rs"
MODEL = "kmd_logic/src/ordered_engine.rs"
LOCKS = "kmd_render/src/adapter/locks.rs"
ADAPTER = "kmd_render/src/adapter/mod.rs"
INTERRUPT = "kmd_render/src/ddi/interrupt.rs"
SUBMIT = "kmd_render/src/ddi/submit_command.rs"
GPU = "kmd_render/src/virtio/gpu/mod.rs"
LIFECYCLE = "kmd_render/src/ddi/lifecycle.rs"
SCHEDULER = "kmd_render/src/ddi/scheduler.rs"
RETIREMENT_GATES = "tools/retirement-gates.sh"
ROADMAP = "ROADMAP.md"
CORE_LANE = "docs/retirement/lane-kmd-core.md"

EXTRA_SOURCES = (CORE_LANE,)


def compact(source: str) -> str:
    return re.sub(r"\s+", "", source)


def braced_end(source: str, brace: int) -> int | None:
    depth = 0
    for index in range(brace, len(source)):
        if source[index] == "{":
            depth += 1
        elif source[index] == "}":
            depth -= 1
            if depth == 0:
                return index + 1
    return None


def load_sources(repo: str) -> dict[str, str]:
    sources = k11_load_sources(repo)
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


def struct_body(
    sources: dict[str, str], path: str, name: str, errors: list[str]
) -> str:
    live = live_rust(sources.get(path, ""))
    match = re.search(rf"\bstruct\s+{re.escape(name)}(?:\s*<[^{{}}]*>)?\s*\{{", live)
    if match is None:
        errors.append(f"{path}: expected one {name} struct")
        return ""
    brace = live.find("{", match.start())
    end = braced_end(live, brace)
    if end is None:
        errors.append(f"{path}: unterminated {name} struct")
        return ""
    return live[brace:end]


def require_fragments(
    path: str,
    name: str,
    source: str,
    fragments: tuple[str, ...],
    errors: list[str],
) -> None:
    value = compact(source)
    for fragment in fragments:
        if compact(fragment) not in value:
            errors.append(f"{path}:{name}: required K9 fragment missing: {fragment}")


def require_order(
    path: str,
    name: str,
    source: str,
    tokens: tuple[str, ...],
    errors: list[str],
) -> None:
    """Require successive occurrences, including repeated tokens."""
    value = compact(source)
    cursor = 0
    for token in tokens:
        wanted = compact(token)
        position = value.find(wanted, cursor)
        if position < 0:
            errors.append(
                f"{path}:{name}: K9 order drifted at {token}: "
                f"{' -> '.join(tokens)}"
            )
            return
        cursor = position + len(wanted)


def check_model(sources: dict[str, str], errors: list[str]) -> None:
    model = live_rust(sources.get(MODEL, ""))
    model_compact = compact(model)
    lib = compact(live_rust(sources.get(LOGIC_LIB, "")))
    if lib.count("pubmodordered_engine;") != 1:
        errors.append(f"{LOGIC_LIB}: K9 model must be exported exactly once")
    if model_compact.count(
        "pubconstMAX_ORDERED_ENGINE_SUBMISSIONS:usize=256;"
    ) != 1:
        errors.append(f"{MODEL}: K9 frontier bound must remain exactly 256")
    if re.search(
        r"\b(?:Vec|VecDeque|HashMap|BTreeMap|Mutex|SpinLock|thread|sleep\w*|poll\w*|timer\w*)\b",
        model,
        re.I,
    ):
        errors.append(f"{MODEL}: K9 model gained dynamic storage, a wait, or a timer")

    engine = struct_body(sources, MODEL, "OrderedEngine", errors)
    require_fragments(
        MODEL,
        "OrderedEngine",
        engine,
        (
            "slots: [Slot; N]",
            "head: usize",
            "len: usize",
            "epoch: u64",
            "next_serial: u64",
            "last_retired_serial: u64",
            "last_admitted_fence: u32",
            "open: bool",
            "poisoned: bool",
            "epoch_exhausted: bool",
        ),
        errors,
    )

    initialize = body(sources, MODEL, "initialize_at", errors)
    require_order(
        MODEL,
        "initialize_at",
        initialize,
        (
            "addr_of_mut!((*destination).slots).cast::<Slot>()",
            "for index in 0..N",
            "first.add(index).write(Slot::EMPTY)",
            "addr_of_mut!((*destination).head).write(0)",
            "addr_of_mut!((*destination).len).write(0)",
            "addr_of_mut!((*destination).epoch).write(1)",
            "addr_of_mut!((*destination).next_serial).write(0)",
            "addr_of_mut!((*destination).last_retired_serial).write(0)",
            "addr_of_mut!((*destination).last_admitted_fence).write(0)",
            "addr_of_mut!((*destination).open).write(false)",
            "addr_of_mut!((*destination).poisoned).write(false)",
            "addr_of_mut!((*destination).epoch_exhausted).write(false)",
        ),
        errors,
    )

    reopen = body(sources, MODEL, "reopen", errors)
    require_order(
        MODEL,
        "reopen",
        reopen,
        (
            "self.open",
            "self.len != 0",
            "self.poisoned",
            "self.epoch_exhausted",
            "N == 0",
            "return false",
            "self.last_admitted_fence = completed_fence",
            "self.open = true",
        ),
        errors,
    )

    admit = body(sources, MODEL, "admit", errors)
    require_order(
        MODEL,
        "admit",
        admit,
        (
            "if self.poisoned",
            "AdmissionRefusal::Poisoned",
            "if !self.open",
            "AdmissionRefusal::Closed",
            "if self.len >= N",
            "self.poison()",
            "AdmissionRefusal::Capacity",
            "fence_is_forward(self.last_admitted_fence, fence)",
            "self.poison()",
            "AdmissionRefusal::NotForward",
            "self.next_serial.checked_add(1)",
            "self.poison()",
            "AdmissionRefusal::SerialExhausted",
            "let slot = (self.head + self.len) % N",
            "u32::try_from(slot)",
            "self.poison()",
            "AdmissionRefusal::SlotIndexExhausted",
            "SlotState::Free",
            "self.poison()",
            "AdmissionRefusal::CorruptOccupiedSlot",
            "SubmissionTicket",
            "state: SlotState::AwaitingHost",
            "self.next_serial = serial",
            "self.last_admitted_fence = fence",
            "self.len += 1",
        ),
        errors,
    )

    mark = body(sources, MODEL, "mark_host_completed", errors)
    require_order(
        MODEL,
        "mark_host_completed",
        mark,
        (
            "ticket.epoch != self.epoch",
            "CompletionDisposition::StaleEpoch",
            "if self.poisoned",
            "CompletionDisposition::Poisoned",
            "slot >= N || !self.slots[slot].matches(ticket, slot)",
            "CompletionDisposition::StaleTicket",
            "SlotState::AwaitingHost",
            "self.slots[slot].state = SlotState::HostCompleted",
            "retained_early: slot != self.head",
            "SlotState::HostCompleted => CompletionDisposition::AlreadyCompleted",
        ),
        errors,
    )

    fail = body(sources, MODEL, "fail_submission", errors)
    require_order(
        MODEL,
        "fail_submission",
        fail,
        (
            "ticket.epoch != self.epoch",
            "FailureDisposition::StaleEpoch",
            "if self.poisoned",
            "FailureDisposition::AlreadyPoisoned",
            "slot >= N || !self.slots[slot].matches(ticket, slot)",
            "FailureDisposition::StaleTicket",
            "self.poison()",
            "FailureDisposition::Poisoned",
        ),
        errors,
    )

    peek = body(sources, MODEL, "peek_ready", errors)
    require_order(
        MODEL,
        "peek_ready",
        peek,
        (
            "if !self.is_open() || self.len == 0",
            "let slot = self.slots[self.head]",
            "if !matches!(slot.state, SlotState::HostCompleted)",
            "ticket: slot.ticket(self.head)",
            "fence: slot.fence",
        ),
        errors,
    )
    if re.search(r"\b(?:iter|position|find)\s*\(", peek):
        errors.append(f"{MODEL}:peek_ready: ordered head became a scan or lookup")

    retire = body(sources, MODEL, "retire_ready", errors)
    require_order(
        MODEL,
        "retire_ready",
        retire,
        (
            "if self.poisoned",
            "RetirementRefusal::Poisoned",
            "self.peek_ready()",
            "RetirementRefusal::NotReady",
            "if head != ready",
            "self.poison()",
            "RetirementRefusal::WrongHead",
            "self.last_retired_serial = head.ticket.serial",
            "self.slots[self.head] = Slot::EMPTY",
            "self.head = (self.head + 1) % N",
            "self.len -= 1",
        ),
        errors,
    )

    retired = body(sources, MODEL, "ticket_was_retired", errors)
    require_fragments(
        MODEL,
        "ticket_was_retired",
        retired,
        (
            "ticket.epoch == self.epoch",
            "ticket.serial != 0",
            "ticket.serial <= self.last_retired_serial",
        ),
        errors,
    )

    invalidate = body(sources, MODEL, "invalidate", errors)
    require_order(
        MODEL,
        "invalidate",
        invalidate,
        (
            "if !self.open && self.len == 0 && !self.poisoned",
            "epoch_advanced: false",
            "let dropped = self.len",
            "for slot in &mut self.slots",
            "*slot = Slot::EMPTY",
            "self.head = 0",
            "self.len = 0",
            "self.last_admitted_fence = 0",
            "self.open = false",
            "self.poisoned = false",
            "self.epoch.checked_add(1)",
            "self.epoch = next",
            "self.epoch_exhausted = true",
            "self.poisoned = true",
        ),
        errors,
    )

    required_tests = (
        "closed_frontier_admits_nothing",
        "early_completion_is_retained_until_the_head_completes",
        "notification_failure_keeps_the_same_ready_head",
        "reset_invalidates_a_late_callback_and_allows_a_fresh_generation",
        "capacity_exhaustion_poisoned_the_generation_instead_of_bypassing",
        "duplicate_or_backward_scheduler_fence_fails_closed",
        "submission_fence_wrap_uses_wddm_half_range_ordering",
        "host_rejection_never_advances_a_later_completion",
        "a_reused_physical_slot_does_not_accept_its_old_ticket",
        "zero_capacity_never_opens",
        "in_place_initialization_matches_the_const_constructor",
    )
    for test in required_tests:
        if len(re.findall(rf"\bfn\s+{re.escape(test)}\s*\(", model)) != 1:
            errors.append(f"{MODEL}: missing exact K9 model test {test}")


def check_adapter_ownership(sources: dict[str, str], errors: list[str]) -> None:
    adapter = live_rust(sources.get(ADAPTER, ""))
    locks = live_rust(sources.get(LOCKS, ""))
    gpu = live_rust(sources.get(GPU, ""))
    adapter_compact = compact(adapter)
    locks_compact = compact(locks)
    gpu_compact = compact(gpu)

    for fragment in (
        "ordered_engine:UnsafeCell<Box<locks::OrderedEngineFrontier>>",
        "ordered_engine_native_rescans:AtomicU32,",
        "ordered_engine:UnsafeCell::new(locks::allocate_ordered_engine_frontier())",
        "ordered_engine_native_rescans:AtomicU32::new(0)",
    ):
        if adapter_compact.count(compact(fragment)) != 1:
            errors.append(f"{ADAPTER}: exact per-adapter K9 owner missing: {fragment}")
    if re.search(
        r"(?m)^\s*(?:pub(?:\([^)]*\))?\s+)?static\s+\w+\s*:\s*[^=;]*(?:OrderedEngine|SubmissionTicket|ReadySubmission)",
        adapter + locks,
    ):
        errors.append(f"{ADAPTER}: K9 authority became module-global")

    allocate = body(sources, LOCKS, "allocate_ordered_engine_frontier", errors)
    require_order(
        LOCKS,
        "allocate_ordered_engine_frontier",
        allocate,
        (
            "Box::<OrderedEngineFrontier>::new_uninit()",
            "OrderedEngineFrontier::initialize_at(frontier.as_mut_ptr())",
            "frontier.assume_init()",
        ),
        errors,
    )
    if locks_compact.count(
        "pub(super)typeOrderedEngineFrontier=OrderedEngine<{MAX_ORDERED_ENGINE_SUBMISSIONS}>;"
    ) != 1:
        errors.append(f"{LOCKS}: K9 adapter frontier lost the shared fixed bound")

    owner = body(sources, LOCKS, "ordered_engine_mut", errors)
    require_fragments(
        LOCKS,
        "ordered_engine_mut",
        owner,
        ("&mut *self.adapter.ordered_engine.get()",),
        errors,
    )
    if (adapter + locks).count("ordered_engine.get()") != 1:
        errors.append(f"{LOCKS}: K9 frontier gained an access outside WddmNotifyGuard")

    if gpu_compact.count(
        "constMAX_WDDM_PENDING:usize=helios_kmd_logic::ordered_engine::MAX_ORDERED_ENGINE_SUBMISSIONS;"
    ) != 1 or compact(
        "MAX_WDDM_PENDING == helios_protocol::translation_session::HELIOS_HTS1_MAX_HOST_DISPATCH_FIFO_DEPTH as usize"
    ) not in gpu_compact:
        errors.append(f"{GPU}: compatibility and endpoint FIFO bounds drifted from K9")

    pending = compact(struct_body(sources, GPU, "WddmPending", errors))
    if pending.count("engine_ticket:crate::adapter::OrderedEngineTicket") != 1:
        errors.append(f"{GPU}: WddmPending must retain one direct K9 ticket")
    if "fence:u32" in pending:
        errors.append(f"{GPU}: WddmPending resurrected a scalar scheduler identity")

    # The authorized post-K9 executor retains the direct K9 tickets alongside
    # its immutable context-local batch until the exact stock-Venus terminal.
    # It is the only extension of K9's private ticket lifetime surface.
    allowed_ticket_paths = {ADAPTER, LOCKS, INTERRUPT, SUBMIT, GPU, "kmd_render/src/ddi/native_render.rs"}
    unexpected = sorted(
        path
        for path, source in sources.items()
        if path.endswith(".rs")
        and "OrderedEngineTicket" in live_rust(source)
        and path not in allowed_ticket_paths
    )
    if unexpected:
        errors.append(f"K9 ticket escaped private KMD lifetime state: {unexpected}")

    invalidate = body(sources, LOCKS, "invalidate_ordered_engine", errors)
    require_order(
        LOCKS,
        "invalidate_ordered_engine",
        invalidate,
        (
            "self.ordered_engine_mut().invalidate()",
            "invalidation.epoch_advanced",
            "invalidation.dropped != 0",
            "ordered_engine_native_rescans",
            ".store(0, Ordering::Release)",
        ),
        errors,
    )
    reopen = body(sources, LOCKS, "reopen_ordered_engine", errors)
    require_order(
        LOCKS,
        "reopen_ordered_engine",
        reopen,
        (
            "let completed = self.completed_fence()",
            "self.ordered_engine_mut().reopen(completed)",
        ),
        errors,
    )
    rescan = body(sources, LOCKS, "note_ordered_engine_native_rescan", errors)
    require_fragments(
        LOCKS,
        "note_ordered_engine_native_rescan",
        rescan,
        (
            "if count == 0",
            "fetch_update(",
            "pending.saturating_add(count)",
        ),
        errors,
    )
def check_lifecycle(sources: dict[str, str], errors: list[str]) -> None:
    close = body(sources, ADAPTER, "close_k11_completions_and_wait", errors)
    require_order(
        ADAPTER,
        "close_k11_completions_and_wait",
        close,
        (
            "self.k11_completion.close_completion_and_wait(passive)",
            "self.with_wddm_notify_lock",
            "guard.invalidate_ordered_engine()",
        ),
        errors,
    )
    reopen = body(sources, ADAPTER, "reopen_k11_completions", errors)
    require_order(
        ADAPTER,
        "reopen_k11_completions",
        reopen,
        (
            "guard.reopen_ordered_engine()",
            "if engine_open",
            "self.k11_completion.reopen()",
        ),
        errors,
    )

    abandon = body(sources, SUBMIT, "abandon_pending_submissions", errors)
    require_order(
        SUBMIT,
        "abandon_pending_submissions",
        abandon,
        (
            "let retain_for_resubmit = matches!(&outcome, AbandonOutcome::Preempted",
            "adapter.with_wddm_notify_lock",
            "guard.invalidate_ordered_engine()",
            "v.preempt_flush(o)",
            "signal_dma_preempted_locked(guard, dxgkrnl, fence)",
            "if retain_for_resubmit && status == STATUS_SUCCESS",
            "guard.reopen_ordered_engine()",
        ),
        errors,
    )
    live_all = "\n".join(live_rust(source) for source in sources.values())
    if live_all.count("invalidate_ordered_engine(") != 3:
        errors.append("K9 invalidation must have one definition and two exact callers")
    if live_all.count("reopen_ordered_engine(") != 3:
        errors.append("K9 reopen must have one definition and two exact callers")


def check_admission_and_completion(
    sources: dict[str, str], errors: list[str]
) -> None:
    admission_enum = re.search(
        r"\bpub\s+enum\s+WddmAdmission\s*\{(?P<body>.*?)\n\}",
        live_rust(sources.get(GPU, "")),
        re.S,
    )
    variants = (
        re.findall(r"(?m)^\s*(\w+)\s*,", admission_enum.group("body"))
        if admission_enum
        else []
    )
    if variants != ["HostTerminal", "Pending", "Failed"]:
        errors.append(f"{GPU}: WddmAdmission must remain the exact three-state proof")

    note = body(sources, GPU, "note_wddm_submission", errors)
    require_order(
        GPU,
        "note_wddm_submission",
        note,
        (
            "if self.failed",
            "self.wddm_pending.clear()",
            "return WddmAdmission::Failed",
            "if self.wddm_pending.is_empty()",
            "return WddmAdmission::HostTerminal",
            "if self.wddm_pending.len() >= MAX_WDDM_PENDING",
            "self.overflow_wddm_pending(current)",
            "return WddmAdmission::Failed",
            "self.wddm_pending.push_back(WddmPending",
            "engine_ticket",
            "WddmAdmission::Pending",
        ),
        errors,
    )
    note_compact = compact(note)
    if (
        note_compact.count("WddmAdmission::Failed") != 2
        or note_compact.count("WddmAdmission::HostTerminal") != 1
        or note_compact.count("WddmAdmission::Pending") != 1
        or "signal_dma_completed" in note
    ):
        errors.append(f"{GPU}: compatibility admission can fabricate or bypass K9 completion")

    submit = body(sources, SUBMIT, "note_and_maybe_signal", errors)
    require_order(
        SUBMIT,
        "note_and_maybe_signal",
        submit,
        (
            "adapter.with_wddm_notify_lock",
            "guard.admit_ordered_engine_submission(fence)",
            "v.note_wddm_submission(",
            "ticket",
            ".unwrap_or(crate::virtio::gpu::WddmAdmission::Failed)",
            "WddmAdmission::HostTerminal => Some(ticket)",
            "WddmAdmission::Pending => None",
            "WddmAdmission::Failed",
            "guard.fail_ordered_engine_submission(ticket)",
            "if let Some(ticket) = complete_now",
            "complete_ordered_engine_submission(adapter, ticket)",
        ),
        errors,
    )
    if "signal_dma_completed" in submit:
        errors.append(f"{SUBMIT}: submission admission bypasses the K9 frontier")

    signal = body(sources, SUBMIT, "signal_dma_completed", errors)
    require_order(
        SUBMIT,
        "signal_dma_completed",
        signal,
        (
            "let last = guard.completed_fence()",
            "fence_is_forward(last, fence)",
            "return STATUS_INVALID_PARAMETER",
            "DXGK_INTERRUPT_DMA_COMPLETED",
            "SubmissionFenceId = fence",
            "NodeOrdinal = 0",
            "EngineOrdinal = 0",
            "notify_at_dirql(dxgkrnl, &mut interrupt, true)",
            "if status == STATUS_SUCCESS",
            "guard.set_completed_fence(fence)",
        ),
        errors,
    )
    live_all = "\n".join(live_rust(source) for source in sources.values())
    if live_all.count("signal_dma_completed(") != 2:
        errors.append("K9 must own the sole DMA_COMPLETED caller plus its definition")
    if live_all.count("set_completed_fence(") != 2:
        errors.append("K9 completed watermark gained an alternate writer")

    drain = body(sources, INTERRUPT, "drain_ordered_engine_locked", errors)
    require_order(
        INTERRUPT,
        "drain_ordered_engine_locked",
        drain,
        (
            "guard.ordered_engine_ready()",
            "fence_is_forward(guard.completed_fence(), ready.fence(),)",
            "guard.fail_ordered_engine_submission(ready.ticket())",
            "signal_dma_completed(guard, dxgkrnl, ready.fence())",
            "if status != STATUS_SUCCESS",
            "guard.note_ordered_engine_notify_retry()",
            "DxgkCbQueueDpc",
            "break",
            "guard.retire_ordered_engine_ready(ready)",
            "delivered = delivered.saturating_add(1)",
            "guard.note_ordered_engine_native_rescan(delivered)",
        ),
        errors,
    )
    drain_compact = compact(drain)
    for fragment, expected in (
        ("guard.ordered_engine_ready()", 1),
        ("signal_dma_completed(guard, dxgkrnl, ready.fence())", 1),
        ("if status != STATUS_SUCCESS", 1),
        ("guard.retire_ordered_engine_ready(ready)", 1),
    ):
        if drain_compact.count(compact(fragment)) != expected:
            errors.append(
                f"{INTERRUPT}:drain_ordered_engine_locked: exact notify/retire transition duplicated or bypassed: {fragment}"
            )
    if re.search(r"\b(?:sleep\w*|poll\w*|timer\w*|KeWaitForSingleObject)\b", drain, re.I):
        errors.append(f"{INTERRUPT}: ordered completion gained a wait, poll, or timer")

    direct = body(sources, INTERRUPT, "complete_ordered_engine_submission", errors)
    require_order(
        INTERRUPT,
        "complete_ordered_engine_submission",
        direct,
        (
            "guard.mark_ordered_engine_host_completed(ticket)",
            "CompletionDisposition::Marked",
            "CompletionDisposition::AlreadyCompleted",
            "drain_ordered_engine_locked(adapter, guard).delivered",
            "CompletionDisposition::StaleEpoch",
            "CompletionDisposition::StaleTicket",
            "CompletionDisposition::Poisoned",
            "service_native_fence_rescans(adapter, guard)",
            "if delivered != 0",
            "request_wddm_completion_dpc(adapter)",
        ),
        errors,
    )
    if re.search(
        r"\b(?:sleep\w*|poll\w*|timer\w*|timeline\w*|SubmissionFenceId)\b",
        direct,
        re.I,
    ):
        errors.append(f"{INTERRUPT}: direct completion gained synthetic authority")

    rescan = body(sources, INTERRUPT, "service_native_fence_rescans", errors)
    require_order(
        INTERRUPT,
        "service_native_fence_rescans",
        rescan,
        (
            "guard.ordered_engine_native_rescans()",
            "if pending == 0",
            "has_possible_progress_edge(adapter)",
            "adapter.dxgkrnl_opt()",
            "signal_native_fence_signaled(adapter, dxgkrnl)",
            "if status != STATUS_SUCCESS",
            "request_wddm_completion_dpc(adapter)",
            "guard.retire_ordered_engine_native_rescans(pending)",
        ),
        errors,
    )
    if "swap(0" in rescan or "store(0" in rescan:
        errors.append(f"{INTERRUPT}: native rescan can clear its edge before callback acceptance")

    dpc = body(sources, INTERRUPT, "drain_used_and_complete", errors)
    require_order(
        INTERRUPT,
        "drain_used_and_complete",
        dpc,
        (
            "v.take_one_ready_wddm(o)",
            "let ticket = ready.engine_ticket()",
            "guard.mark_ordered_engine_host_completed(ticket)",
            "drain_ordered_engine_locked(adapter, guard)",
            "!guard.ordered_engine_ticket_is_live(ticket)",
            "ready.delivered()",
            "CompletionDisposition::StaleTicket",
            "guard.ordered_engine_ticket_was_retired(ticket)",
            "ready.delivered()",
            "v.requeue_wddm_front(o, ready)",
            "drain_ordered_engine_locked(adapter, guard)",
            "service_native_fence_rescans(adapter, guard)",
        ),
        errors,
    )
    if compact(dpc).count("ready.delivered()") != 2:
        errors.append(f"{INTERRUPT}: compatibility ownership has an uncorrelated discharge")


def check_docs_and_integration(sources: dict[str, str], errors: list[str]) -> None:
    gates = sources.get(RETIREMENT_GATES, "")
    invocations = re.findall(
        r'(?m)^\s*python3\s+"\$REPO/tools/k9-ordered-engine-gate\.py"\s+"\$REPO"(?:\s+--mutations)?\s*$',
        gates,
    )
    if len(invocations) != 1 or "--mutations" not in invocations[0]:
        errors.append(f"{RETIREMENT_GATES}: K9 source+mutation gate must be integrated once")

    lane = sources.get(CORE_LANE, "")
    row = next((line for line in lane.splitlines() if line.startswith("| **K9** |")), "")
    if not row:
        errors.append(f"{CORE_LANE}: K9 ownership row is missing")
    if "EXERCISED ON THE TARGET" in row or "RUNTIME-ADMITTED" in row:
        errors.append(f"{CORE_LANE}: source-only K9 gained a target-runtime claim")

    roadmap = sources.get(ROADMAP, "")
    section = re.search(
        r"#### ⭐ K9 source/build checkpoint(?P<body>.*?)(?:\n#### |\Z)",
        roadmap,
        re.S,
    )
    if section is None:
        errors.append(f"{ROADMAP}: K9 source/build checkpoint is missing")
    text = section.group("body") if section else ""
    if "EXERCISED ON THE TARGET" in text or "RUNTIME-ADMITTED" in text:
        errors.append(f"{ROADMAP}: source-only K9 gained a target-runtime claim")
    if re.search(r"(?:visible desktop|display admission).{0,80}(?:proved|established|passed)", text, re.I | re.S):
        errors.append(f"{ROADMAP}: K9 source checkpoint gained a false display claim")


def check_local_sources(sources: dict[str, str]) -> list[str]:
    errors: list[str] = []
    check_model(sources, errors)
    check_adapter_ownership(sources, errors)
    check_lifecycle(sources, errors)
    check_admission_and_completion(sources, errors)
    check_docs_and_integration(sources, errors)
    return errors


def check_sources(sources: dict[str, str]) -> list[str]:
    inherited = {path: source for path, source in sources.items() if path not in EXTRA_SOURCES}
    errors = [f"K11: {error}" for error in k11_check_sources(inherited)]
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
        Mutation("unbound frontier", MODEL, "pub const MAX_ORDERED_ENGINE_SUBMISSIONS: usize = 256;", "pub const MAX_ORDERED_ENGINE_SUBMISSIONS: usize = usize::MAX;"),
        Mutation("dynamic frontier", MODEL, "slots: [Slot; N],", "slots: Vec<Slot>,"),
        Mutation("open zero-capacity frontier", MODEL, "        if self.open || self.len != 0 || self.poisoned || self.epoch_exhausted || N == 0 {", "        if self.open || self.len != 0 || self.poisoned || self.epoch_exhausted {"),
        Mutation("bypass capacity poison", MODEL, "        if self.len >= N {\n            self.poison();\n            return Err(AdmissionRefusal::Capacity);\n        }\n", ""),
        Mutation("allow duplicate fence", MODEL, "        if !crate::scanout_lease::fence_is_forward(self.last_admitted_fence, fence) {", "        if false {"),
        Mutation("wrap ticket serial", MODEL, "self.next_serial.checked_add(1)", "Some(self.next_serial.wrapping_add(1))"),
        Mutation("reuse occupied slot", MODEL, "        if !matches!(self.slots[slot].state, SlotState::Free) {", "        if false {"),
        Mutation("publish slot before awaiting state", MODEL, "state: SlotState::AwaitingHost,", "state: SlotState::HostCompleted,"),
        Mutation("accept stale epoch completion", MODEL, "        if ticket.epoch != self.epoch {\n            return CompletionDisposition::StaleEpoch;\n        }", "        if false {\n            return CompletionDisposition::StaleEpoch;\n        }"),
        Mutation("complete by scalar slot", MODEL, "        if slot >= N || !self.slots[slot].matches(ticket, slot) {\n            return CompletionDisposition::StaleTicket;\n        }", "        if slot >= N {\n            return CompletionDisposition::StaleTicket;\n        }"),
        Mutation("erase early retention", MODEL, "retained_early: slot != self.head", "retained_early: false"),
        Mutation("host failure does not poison", MODEL, "        self.poison();\n        FailureDisposition::Poisoned\n", "        FailureDisposition::Poisoned\n"),
        Mutation("scan for any ready completion", MODEL, "let slot = self.slots[self.head];", "let slot = self.slots.iter().find(|slot| matches!(slot.state, SlotState::HostCompleted)).copied().unwrap();"),
        Mutation("retire wrong head", MODEL, "        if head != ready {", "        if false {"),
        Mutation("leave retired slot occupied", MODEL, "        self.slots[self.head] = Slot::EMPTY;\n", ""),
        Mutation("wrap frontier head incorrectly", MODEL, "self.head = (self.head + 1) % N;", "self.head += 1;"),
        Mutation("recognize retired ticket across epoch", MODEL, "        ticket.epoch == self.epoch\n", "        true\n"),
        Mutation("wrap reset epoch", MODEL, "self.epoch.checked_add(1)", "Some(self.epoch.wrapping_add(1))"),
        Mutation("stack materialize frontier", LOCKS, "let mut frontier = Box::<OrderedEngineFrontier>::new_uninit();", "let mut frontier = Box::new(OrderedEngineFrontier::new()).into();"),
        Mutation("global frontier authority", LOCKS, "pub(crate) static ORDERED_ENGINE_ADMITTED", "static GLOBAL_ORDERED_ENGINE: OrderedEngineFrontier = OrderedEngineFrontier::new();\npub(crate) static ORDERED_ENGINE_ADMITTED"),
        Mutation("frontier access without guard", LOCKS, "unsafe { &mut *self.adapter.ordered_engine.get() }", "unsafe { global_ordered_engine() }"),
        Mutation("split compatibility bound", GPU, "const MAX_WDDM_PENDING: usize =\n    helios_kmd_logic::ordered_engine::MAX_ORDERED_ENGINE_SUBMISSIONS;", "const MAX_WDDM_PENDING: usize = 512;"),
        Mutation("restore scalar WDDM fence", GPU, "struct WddmPending {\n", "struct WddmPending {\n    fence: u32,\n"),
        Mutation("complete transport failure", GPU, "            self.windowed_blt.terminal.clear();\n            return WddmAdmission::Failed;", "            self.windowed_blt.terminal.clear();\n            return WddmAdmission::HostTerminal;"),
        Mutation("complete compatibility overflow", GPU, "            self.overflow_wddm_pending(current);\n            return WddmAdmission::Failed;", "            self.overflow_wddm_pending(current);\n            return WddmAdmission::HostTerminal;"),
        Mutation("skip K9 compatibility admission", SUBMIT, "            None => guard.admit_ordered_engine_submission(fence)?,", "            None => forged_ticket(fence),"),
        Mutation("complete failed compatibility work", SUBMIT, "            crate::virtio::gpu::WddmAdmission::Failed => {\n                let _ = guard.fail_ordered_engine_submission(ticket);\n                None\n            }", "            crate::virtio::gpu::WddmAdmission::Failed => Some(ticket)"),
        Mutation("notify before host completion", INTERRUPT, "        let Some(ready) = guard.ordered_engine_ready() else {", "        let Some(ready) = newest_submission() else {"),
        Mutation("retire before notify acceptance", INTERRUPT, "        if !guard.retire_ordered_engine_ready(ready) {", "        let _ = guard.retire_ordered_engine_ready(ready);\n        if status != STATUS_SUCCESS {"),
        Mutation("drop failed notify retry", INTERRUPT, "            guard.note_ordered_engine_notify_retry();\n", ""),
        Mutation("skip direct completion ticket mark", INTERRUPT, "        let disposition = guard.mark_ordered_engine_host_completed(ticket);\n        let delivered = match disposition {", "        let disposition = CompletionDisposition::AlreadyCompleted;\n        let delivered = match disposition {"),
        Mutation("drop direct cleanup DPC", INTERRUPT, "    if delivered != 0 {", "    if false {"),
        Mutation("clear native edge before callback", INTERRUPT, "    let pending = guard.ordered_engine_native_rescans();", "    let pending = clear_ordered_engine_native_rescans();"),
        Mutation("discharge live compatibility owner", INTERRUPT, "            if mark_owned && !guard.ordered_engine_ticket_is_live(ticket) {", "            if mark_owned {"),
        Mutation("drop retired compatibility cleanup", INTERRUPT, "                && guard.ordered_engine_ticket_was_retired(ticket)\n", ""),
        Mutation("drop undelivered compatibility owner", INTERRUPT, "            let _ = guard.with_virtio(|o, v| v.requeue_wddm_front(o, ready));", "            ready.delivered();"),
        Mutation("invalidate after FIFO abandonment", SUBMIT, "        guard.invalidate_ordered_engine();\n        let dropped = guard", "        let dropped = guard"),
        Mutation("reopen before preempt acceptance", SUBMIT, "        if retain_for_resubmit && status == STATUS_SUCCESS {", "        if retain_for_resubmit {"),
        Mutation("reopen callbacks before engine", ADAPTER, "        if engine_open {\n            self.k11_completion.reopen();\n        }", "        self.k11_completion.reopen();\n        let _ = engine_open;"),
        Mutation("invalidate engine before callback join", ADAPTER, "        self.k11_completion.close_completion_and_wait(passive);\n", "        self.with_wddm_notify_lock(|guard| guard.invalidate_ordered_engine());\n        self.k11_completion.close_completion_and_wait(passive);\n"),
        Mutation("forge completed watermark", SUBMIT, "        guard.set_completed_fence(fence);", "        guard.set_completed_fence(fence.wrapping_add(1));"),
        Mutation("claim K9 target runtime", CORE_LANE, "SOURCE/BUILD-VALIDATED", "EXERCISED ON THE TARGET"),
    )


def run_mutations(sources: dict[str, str]) -> None:
    for case in mutation_cases():
        count = sources.get(case.path, "").count(case.old)
        if count != 1:
            raise SystemExit(
                f"K9 mutation setup failed for {case.name}: expected one anchor "
                f"in {case.path}, found {count}"
            )
        mutated = dict(sources)
        mutated[case.path] = mutated[case.path].replace(case.old, case.new, 1)
        errors = check_local_sources(mutated)
        if not errors:
            inherited = {
                path: source for path, source in mutated.items() if path not in EXTRA_SOURCES
            }
            errors.extend(f"K11: {error}" for error in k11_check_sources(inherited))
        if not errors:
            raise SystemExit(f"K9 mutation was accepted by the real gate: {case.name}")
    print(f"OK: {len(mutation_cases())} in-memory K9 mutations rejected by the real gate")


def main() -> None:
    args = sys.argv[1:]
    mutations = False
    if "--mutations" in args:
        mutations = True
        args.remove("--mutations")
    if len(args) > 1:
        raise SystemExit("usage: k9-ordered-engine-gate.py [repo] [--mutations]")
    repo = os.path.abspath(args[0]) if args else REPO_DEFAULT
    sources = load_sources(repo)
    errors = check_sources(sources)
    if errors:
        print("K9 ordered-engine gate violated:", file=sys.stderr)
        for error in errors:
            print(error, file=sys.stderr)
        raise SystemExit(1)
    print(
        "OK: K9 fixed per-adapter admission, exact host-terminal tickets, "
        "ordered DMA retirement, retry, and stale-generation closure hold"
    )
    if mutations:
        run_mutations(sources)


if __name__ == "__main__":
    main()
