//! The UMD's `HKLM\SOFTWARE\Helios` registry knobs, with their defaults as data.
//!
//! Four accessors used to be four literal copies of one ~33-line body: the same
//! `advapi32!RegGetValueA` redeclaration, the same `HKEY_LOCAL_MACHINE` /
//! `RRF_RT_REG_DWORD` constants, the same `SOFTWARE\Helios` subkey, the same
//! `OnceLock`, differing only in the value name and in an unlabelled tail
//! expression that decided what "absent" meant. That is policy-in-boilerplate:
//! a knob copy-pasted with the wrong tail is silently the wrong value on every
//! machine that never wrote the value, with no counter and no log.
//!
//! Here the default is a constructor argument, so it is impossible to write a
//! knob without stating what an absent value means, and the FFI call exists at
//! one audited site instead of four.
//!
//! **The registry value names, the hive and the `RRF` flag are the owner's
//! debugging ABI and are unchanged.** So are the remaining defaults:
//!
//! | Value | Type | Absent |
//! |---|---|---|
//! | `UmdTrace` | DWORD | `false` (explicit non-zero enables) |
//! | `FeatureLevel11` | DWORD | `1` |
//! | `UmdFreeThreaded` | DWORD | `true` (explicit 0 reverts the threading surface) |
//! | `UmdCommandLists` | DWORD | `true` (explicit 0 reverts to emulated lists) |
//! | `UmdDeferredDiagnostics` | DWORD | `false` (diagnostic atomics, opt-in) |
//!
//! The surviving policies are `BoolKnob` ("absent = off, non-zero = on") and
//! `DwordKnob` ("absent = this default, else the stored value").
//!
//! **Not covered here:** the environment-variable knobs, which are process
//! environment rather than registry state and have their own `OnceLock`s —
//! `HELIOS_DXGI_NO_REDIRECTION` (`lib.rs`), and `HELIOS_PRESENT_READBACK` /
//! `HELIOS_PRESENT_FORCE_OPAQUE` / `HELIOS_PRESENT_OPTIMIZE_COMPOSITION` /
//! `HELIOS_PRESENT_DUMP_DIR` (`forward.rs`). They are listed here so the knob
//! inventory is readable in one place even though the reader is not shared.

use helios_umd_common::knobs::{BoolKnob, DwordKnob};

// ⚠ `reg_dword` (the single audited advapi32 FFI site), `DwordKnob` and
// `BoolKnob` moved to `helios_umd_common::knobs` (`DECISIONS.md` D3b, stage S2).
// ⛔ THE KNOB VALUES DID NOT MOVE and must not: D3b says "the knob values stay
// per-crate -- `umd12` declares its own set including `UmdD3D12`". Sharing the
// table would make one driver's A/B lever silently apply to the other, and
// `UserModeDriverName[3]` is meant to be the only coupling between them.

// --- The knob set ----------------------------------------------------------
//
// Every registry knob the UMD reads is declared here. Adding one anywhere else
// is the drift this module exists to stop.

/// Per-frame/per-op DDI chatter (`trace_line!`). Absent = OFF.
pub(crate) static UMD_TRACE: BoolKnob = BoolKnob::new(c"UmdTrace", false);

/// Feature-level profile selector. Absent = 1 (the full FL11 profile).
pub(crate) static FEATURE_LEVEL_11: DwordKnob = DwordKnob::new(c"FeatureLevel11", 1);

/// FREETHREADED THREADING-caps kill switch (Phase B of the command-list
/// build, `tmp/handoff-perf-structural/PLAN-commandlists.md`). Absent = ON;
/// explicit 0 reverts the adapter to THREADING caps = 0 without a redeploy
/// (a fresh process re-reads it; the caps answer is per-process).
///
/// ON reports `D3D11DDICAPS_FREETHREADED`: the runtime stops taking its
/// device critical section around create/destroy/calc DDIs, so they arrive
/// concurrently with immediate-context DDIs — the contention this removes
/// was measured at 5.6 % of the render thread (65th session,
/// `RtlpEnterCriticalSectionContended` under `CUseCountedObject::Release`).
/// The state this exposes went thread-safe in Phase A (`ShaderCaches` mutex,
/// `CtxBindings` atomics and shader-cache mutexes).
/// This knob NEVER enables command-list caps — see
/// `device_funcs::threading_caps`.
pub(crate) static UMD_FREE_THREADED: BoolKnob = BoolKnob::new(c"UmdFreeThreaded", true);

/// Native deferred-context/command-list DDIs (Phase C of the command-list
/// build, `tmp/handoff-perf-structural/PLAN-commandlists.md`).
///
/// **Default ON since 2026-08-05**; explicit 0 reverts to the runtime's
/// emulated path. The bring-up comment used to say the default flips "only
/// after the full Phase C gate set passes" — it has passed: with this on, plus
/// FREETHREADED and the DXVK CL fast/inline/recycle/sampler-retention set,
/// `tmp/handoff-perf-structural/reports/p3-227-recovery-outcome.md` records
/// **GT1 224.16 / GT2 229.17 / Graphics 52,126 / Combined 8,465**, against
/// GT1 ~184 and Graphics ~43.5k on the emulated path. Every accepted score
/// since 2026-08-03 was measured with it on, supplied by the test VM's
/// registry, so leaving the code default OFF meant a fresh install shipped a
/// materially slower driver than the one being measured.
///
/// ON (and only with [`UMD_FREE_THREADED`] also on — COMMANDLISTS requires
/// FREETHREADED) reports `D3D11DDICAPS_COMMANDLISTS_BUILD_2`: the runtime
/// stops emulating command lists (worker-thread SWDC recording + render-
/// thread SWCL replay, the verified #1 render-thread cost) and instead
/// records through our deferred-context DDI onto DXVK's stock
/// `D3D11DeferredContext`, handing finished `ID3D11CommandList`s to
/// `pfnCommandListExecute`. The DDI slots themselves are real and installed
/// unconditionally; this knob only controls whether the caps bit invites the
/// runtime to use them. See `device_funcs::threading_caps`.
pub(crate) static UMD_COMMAND_LISTS: BoolKnob = BoolKnob::new(c"UmdCommandLists", true);

/// Deferred-context/command-list success-path counters and sampled logs.
///
/// The native command-list path finishes and executes work on many worker
/// threads. Its evidence counters used to perform two process-global atomic
/// RMWs per successful finish/execute (the counter and `LogThrottle`), which
/// turns instrumentation into a contended cache line in the benchmarked path.
/// Keep the evidence available for an explicit diagnostic run, but absent =
/// OFF means a timed run does no diagnostic atomic RMW at all.
pub(crate) static UMD_DEFERRED_DIAGNOSTICS: BoolKnob =
    BoolKnob::new(c"UmdDeferredDiagnostics", false);

/// The knob inventory, so the set is enumerable instead of grep-discoverable.
///
/// Each entry is `(value name, resolved value as text)`. Resolving forces every
/// `OnceLock`, which is why this is not called on any hot path — it exists for
/// a one-shot dump at load, and for anyone asking "what knobs are there".
/// Emit this crate's knob inventory through the shared reader, once per process.
///
/// The thin wrapper D3b's split implies: the READER is shared
/// (`helios_umd_common::log::log_knob_inventory`) because the log format is the
/// evidence contract, while the SET is per-crate. `crate::log_knob_inventory()`
/// keeps resolving at its one call site in `open_adapter_common`, and the
/// emitted lines are byte-identical to before the move — which is `S2-check`.
pub(crate) fn log_knob_inventory() {
    helios_umd_common::log::log_knob_inventory(&resolved_inventory());
}

pub(crate) fn resolved_inventory() -> [(&'static str, u32); 5] {
    [
        ("UmdTrace", UMD_TRACE.get() as u32),
        ("FeatureLevel11", FEATURE_LEVEL_11.get()),
        ("UmdFreeThreaded", UMD_FREE_THREADED.get() as u32),
        ("UmdCommandLists", UMD_COMMAND_LISTS.get() as u32),
        (
            "UmdDeferredDiagnostics",
            UMD_DEFERRED_DIAGNOSTICS.get() as u32,
        ),
    ]
}

// ── The typed accessors ──────────────────────────────────────────────────────
//
// Moved verbatim out of `lib.rs` by T8/R1106, beside the knobs they read.
// `lib.rs` re-exports the accessors used outside this module.

/// Resolve `HKLM\SOFTWARE\Helios!UmdTrace` (REG_DWORD) != 0, forcing its
/// `OnceLock`. Read once per process.
///
/// ⚠ This is the KNOB. The GATE that `trace_line!` consults is
/// `helios_umd_common::log::trace_enabled()`, which caches this answer in a
/// relaxed `AtomicBool` at `log::init` time (stage S2). Two names for what used
/// to be one function, and the split is deliberate: `trace_line!` expands at
/// ~430 sites, many per-op, so the gate must be one relaxed load and not a
/// `OnceLock` walk through a knob table the shared crate cannot see.
/// `crate::trace_enabled()` now re-exports the GATE, so every call site reads
/// the cached answer.
///
/// Errors, one-shots and refusals keep using `log_error!` unconditionally —
/// only known-hot repeat traffic (Present, OMSetRenderTargets,
/// ResolveSharedResource, per-op stamps) sits behind the gate.
pub(crate) fn umd_trace_knob() -> bool {
    UMD_TRACE.get()
}

/// Selects whether the adapter advertises the full D3D11 feature-level profile
/// or the conservative FL10_0 fallback:
/// `HKLM\SOFTWARE\Helios!FeatureLevel11` (REG_DWORD). Absent = full FL11
/// profile; explicit 0 = FL10_0 opt-out. Read once
/// per process, so an already-running dwm keeps the level it created its
/// device at while freshly-launched apps pick up the new value.
///
/// This gate MUST cover the three caps together — the 3DPIPELINESUPPORT
/// pipeline level, `check_format_support`'s multisample bits, and
/// `CheckMultisampleQualityLevels` — because the Microsoft runtime validates
/// them as one coherent feature-level contract during
/// `CDevice::LLOCompleteLayerConstruction`; a partial change is rejected with
/// DXGI_ERROR_UNSUPPORTED. FL11_0 additionally requires real multisample
/// support, which the FL10_0 profile deliberately suppresses.
///
/// 30th/31st-session ETW evidence (Microsoft-Windows-DXGI) showed this is a UMD
/// caps sequence, not a KMD/adapter ceiling: the runtime reaches
/// CreateDevice/venus CTX_CREATE and rejects each bad caps contract with a
/// concrete string. Gates fixed so far: 3DPIPELINESUPPORT is a bitmask,
/// SHADER compute cap is 0x2, and MSAA/format support must match D3D11.3
/// §19.2.5. knob=0 remains the exact FL10_0 baseline opt-out for A/B.
///   absent = full FL11 profile
///   0 = FL10_0 profile
///   1 = full FL11_0 (pipeline 11_0 + real MSAA + unmasked format bits)
///   2 = DIAGNOSTIC: pipeline claims 11_0 but keeps the FL10 MSAA/format caps —
///       isolates pipeline-level validation from the later FL11 caps gates.
pub(crate) fn feature_level_mode() -> u32 {
    FEATURE_LEVEL_11.get()
}

/// FREETHREADED THREADING-caps kill switch:
/// `HKLM\SOFTWARE\Helios!UmdFreeThreaded` (REG_DWORD). Read once per process.
/// Absent = ON; explicit 0 reverts to caps = 0. See [`UMD_FREE_THREADED`].
pub(crate) fn umd_free_threaded() -> bool {
    UMD_FREE_THREADED.get()
}

/// Native command-list enable: `HKLM\SOFTWARE\Helios!UmdCommandLists`
/// (REG_DWORD). Read once per process. Absent = ON; explicit 0 reverts to the
/// runtime's emulated command lists. Forced off when [`umd_free_threaded`] is off —
/// COMMANDLISTS caps require FREETHREADED, so `UmdFreeThreaded=0` remains the
/// one kill switch that reverts the whole threading surface at once. See
/// [`UMD_COMMAND_LISTS`].
pub(crate) fn umd_command_lists() -> bool {
    UMD_COMMAND_LISTS.get() && UMD_FREE_THREADED.get()
}

/// Whether deferred-context/command-list success-path diagnostics are enabled:
/// `HKLM\\SOFTWARE\\Helios!UmdDeferredDiagnostics` (REG_DWORD). Read once per
/// process. Absent = OFF so the timed command-list path performs no diagnostic
/// counter or log-throttle atomic RMWs.
pub(crate) fn umd_deferred_diagnostics() -> bool {
    UMD_DEFERRED_DIAGNOSTICS.get()
}
