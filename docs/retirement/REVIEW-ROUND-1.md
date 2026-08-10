# Phase-2 adversarial review, round 1

`docs/dx12/METHOD.md` is authoritative for sequencing: implement a subsystem to
its contract, then adversarially review the **whole changeset** by lens, repair,
and repeat with different lenses until saturated. This file records round 1 of
that loop over the HPS2 retirement changeset.

**Date** 2026-08-10 · **Reviewer and author are the same party**, which METHOD.md
§2 Phase 3 forbids in the parallel-lane case. Recorded here as a known weakening
of the protocol rather than glossed: no reviewer held a slice the author did not.

Four reviews were planned. The QEMU one is **moot** — `FINDINGS.md` F5 parked
the HPM1 memory lane and reset the submodule, so there is no changeset to
review. The three that ran are below.

⛔ **This is round 1, and round 1 is not saturation.** METHOD.md §3 requires two
consecutive dry rounds with *different* lens compositions, a completeness critic
returning nothing, and every mechanical check passing as an exit code. Only the
third of those is satisfied. The 85th session's lesson stands: four converging
lenses were all wrong because three of them shared one stale comment, so a
round that finds things is evidence the lenses worked, not that the code is now
clean.

---

## What became an exit code

The durable output of this round is `tools/retirement-gates.sh` — the Linux-side
mechanical checks as one command, per METHOD.md §3 criterion 3. Two of these
were run by hand during the 2026-08-10 translator-dispatch repair, found real
defects, and were never checked in; the next session re-derived one from
scratch. A gate that lives in a transcript is not a gate.

| gate | what it would have caught |
|---|---|
| `protocol` unit tests | — |
| `protocol` Rust↔C ABI parity (`protocol/tools/abi_parity.py`) | the mirror generated from an older revision |
| `protocol` C mirrors compile | a `_Static_assert` nobody evaluated |
| `kmd_logic` unit tests | — |
| WDDM 3.2 slot audit not stale vs its generator | **the landmine below** |
| `VKD3D_HEAP_FLAG_HELIOS_VENUS_EXPORT` agrees across both repos | CLAUDE.md's named highest-risk fork divergence |

---

## Review 1 — `protocol/`

Lenses: ABI & tables (mechanical, exhaustive), cross-lane seams, loud failure,
contract completeness, claim integrity.

**Clean.** 396 layout claims compared across the 36 mirrored types: 0 mismatch,
0 one-sided, 0 unpinned. All 9 `reserved` fields across 7 records are checked by
their validators. Every record type Rust declares is mirrored except the two
opaque handles and one Rust-only newtype, none of which have a C representation.

**One finding, confirmed after refutation** (`be2185b`).
`HeliosTranslatorRefusalCountersV1` was the only record in the ABI with no
validator and no C `check_` twin, while `query_refusal_counters` hands its
storage to a **separately compiled binary** as a `*mut`. Its own field
documentation had always said `struct_bytes` is "`== HELIOS_TRANSLATOR_
REFUSAL_COUNTERS_BYTES`, set by the caller" — a stated precondition with no code
behind it.

The refutation worth recording, because it nearly killed the finding: create-time
`abi_version` negotiation already refuses a mismatched revision. But that argument
applies verbatim to all 14 other records, which re-check anyway, because what
per-record `struct_bytes` catches is a record whose **size** changed without the
**version** being bumped — precisely what create-time negotiation cannot see.
The decisive evidence was the sibling: `HeliosSyncProgressResultV1` has the
identical `*mut`-filled-by-the-other-binary shape and has always been validated.
An oversight, not a decision.

⚠ **Bound: the cross-lane seam check passes vacuously.** OWNERSHIP.md §4 forbids
any consumer declaring its own copy of the dispatch ABI. No consumer references
it at all — not `icd/mesa`, `dxvk-helios`, `vkd3d-proton-helios`, `umd` or
`umd12`. "Wired in" for `translator_dispatch` means it compiles inside
`protocol/`, **not** that anything uses it. That is METHOD.md §3 criterion 6's
fourth state — *implemented but never exercised* — and it must not be reported
as done.

---

## Review 2 — `kmd_render` native-fence surface

Lenses: handles & lifetimes, loud failure, concurrency, ABI & tables.

Mechanically clean: 0 `unsafe` blocks without a `SAFETY` comment, no
`unwrap`/`expect`/`panic!`/`todo!`, and the slot audit itself now gates
registration-against-classification as an exit code.

**Three findings, one structural cause.** Every decision in this module is taken
from a `model_of` snapshot and published by a **separate** atomic operation, so
any rule reading both `state` and `local_refs` has a window between deciding and
acting.

1. **FIXED — destroy racing the last close leaks the object** (`8d55faf`).
   `model_of` reads `refs = 1`, the rule refuses; the last close then retires
   refs to 0 and its own `DRAINING → DEAD` exchange **fails** because the state
   is still `LIVE`; destroy only then publishes `DRAINING`. Result: `DRAINING`
   with zero references, no further close will ever arrive, and the object plus
   its bounded `LIVE_GLOBAL` slot leak forever. Closed by re-checking after
   `DRAINING` is published, which is sound precisely because both parties
   converge on a state that is by then visible.
2. **FIXED — destroy on an already-`DRAINING` object with zero refs** refused
   without freeing, and nobody else was coming.
3. ⛔ **NOT FIXED — `NF-UAF-1`, open racing destroy is a use-after-free.**
   Recorded in the module header at the increment site. A destroy that samples
   `local_refs == 0` frees the object between `open`'s rule decision and its
   `fetch_add`, leaving that call writing freed memory and handing dxgkrnl a
   `LocalFenceObject` whose `global` pointer dangles.

   A re-read of the state after the increment only **narrows** this, so it was
   deliberately not applied — METHOD.md §5 rejects stopgaps by name, and a
   narrowed race in kernel code is one wearing a fix's clothes. The fix is to
   pack `state` and `local_refs` into one `AtomicU64` so the rule and the
   transition are a single compare-exchange, keeping `kmd_logic` authoritative
   by handing it the decoded word and publishing against that exact value. It
   needs `nf::destroy_global` to express "refused, and park in `Draining`" as a
   transition rather than a bare `Err` — a `kmd_logic` signature change plus
   tests.

⚠ **All three are unreachable today**: `NATIVE_FENCE_ADVERTISED` is false while
`wddm_surface::SURFACE` is `Wddm2_1GpuMmu`, so dxgkrnl never calls these DDIs.
They become reachable at the retirement's single activation switch — which is
exactly when a kernel use-after-free is most expensive to discover by running.
This is the case for reviewing before flipping, not after.

⚠ **The fixes are not compiled.** `kmd_render` is VM-only and this is batched
with the next KMD build rather than spending a build+install+reboot cycle of its
own.

---

## Review 3 — `vkd3d-proton-helios`

Lenses: cross-lane seams, contract completeness, claim integrity.

**The Escape/SharedGpuResource retirement (`912a3d4d`) is complete.** The only
surviving references to `SharedGpuResource` in `libs/` and `include/` are two
comments explaining what was removed.

**The named high-risk divergence agrees.** `VKD3D_HEAP_FLAG_HELIOS_VENUS_EXPORT`
is `1 << 30` in `libs/vkd3d/vkd3d_private.h` and `HELIOS_HEAP_FLAG_VENUS_EXPORT`
is `1 << 30` in `umd12/src/forward12/resource12.rs`. CLAUDE.md calls this the
highest-risk divergence in the fork *because nothing checked it*; verifying it by
reading is the same failure mode as reading a hand-maintained C mirror, so it is
now a gate.

**Finding — the fork's test claim was unreproducible and stated two different
numbers.** `OWNERSHIP.md` §6 says "green, 102/102, tests included"; the agent
memory says "vkd3d 215/215". Neither is reproducible from `meson test`, which
reports **"No tests defined"** — vkd3d-proton runs its suite through
`tests/test-runner.sh` against a built `tests/d3d12` binary, outside meson's
harness entirely. The suite actually has **545** tests. A number that two
records disagree about, and that the obvious command contradicts, is a claim to
re-derive rather than inherit (METHOD.md §3 criterion 5).

### The two failures, attributed

`./tests/test-runner.sh build-native-codex/tests/d3d12` → **2 failures**:
`test_nvx_cubin` and `test_destruction_notifier_interfaces`. Both were run
against **unmodified upstream `2c7ba22c`** on the same host, and **neither is a
Helios regression**:

| test | fork | upstream `2c7ba22c` |
|---|---|---|
| `test_nvx_cubin` | 512 failed assertions, all readbacks zero | **identical**: 512 failed |
| `test_destruction_notifier_interfaces` | SIGSEGV in `IUnknown_AddRef` | **also SIGSEGV**, at matched build flags |

`test_destruction_notifier_interfaces` is **concurrency-dependent**, and a
single process never crashes:

```
concurrency 1: 0/1     concurrency 4: 2/4
concurrency 2: 1/2     concurrency 8: 5/8      (12 concurrent: 24/24)
```

That it requires ≥2 concurrent processes rules out a per-process defect in the
test's own logic and points below vkd3d — concurrent Vulkan/CUDA device creation
on this host (RTX PRO 6000 Blackwell, driver 610.43.03). It is why the 27-way
parallel suite run showed the failure and an isolated re-run did not.

⚠ **Two hypotheses were formed and both were killed by measurement, which is the
part worth keeping:**

1. *"The embedded cubin predates Blackwell."* Refuted: the fatbin carries
   `sm_120` as both PTX and cubin, and the host is compute 12.0.
2. *"`tests[]` holds 13 pointers to locals and only `pipeline_library` is
   initialised, so a failed creation is dereferenced as garbage."* The code
   really is written that way and the backtrace fits it exactly — but an
   interleaved A/B of NULL-initialising all 13, **24 runs per arm under
   identical load, gave 24/24 crashes in both**. The change was reverted rather
   than committed.

⛔ **The near-miss is the lesson.** The NULL-init arm first measured 0/12
against a 4/12 baseline and looked like a fix at p≈0.008. It was not: the
baseline had run while the 545-test suite was still loading the machine and the
"fix" ran after it finished. An all-A-then-all-B comparison could not separate
the change from the load, exactly as CLAUDE.md rule 7 says. Interleaving the
arms is what turned a confident false fix into a refutation.

---

## Round 2 must rotate the lenses

Not yet run. METHOD.md §3 criterion 1 requires at least two lenses changed.
Candidates this round did **not** apply:

* **Contract completeness** against `DDI_REFERENCE.md` / the WDK headers — reads
  the *contract* against the diff rather than the diff against itself. This is
  the lens for "an obligation nothing implements and nothing refuses", and it is
  the one the probe-driven loop structurally cannot have.
* **Instrument attribution** — every counter this changeset adds, graded. The
  `Nf*` family was read as "all 21 zero" at the pre-flip baseline; zero on an
  unreachable path attributes nothing, and METHOD.md's own anti-pattern list
  names trusting a zero.
* **Engine contract** — the vkd3d fork's obligations, unreviewed here beyond the
  deletion's completeness.
* **`icd/mesa`** — deliberately not read. See below: it was **compiled**
  instead, which is the same judgement carried out.

---

## Review 4 — `icd/mesa`, done by compiler rather than by reading

`helios_present_layer.{h,cpp}` was 4269 lines that had never been through a
compiler. Rather than spend a reading pass on it, it got a `clang-cl
-fsyntax-only` — no meson configure, no Mesa dependencies. **17 errors**, and
the prediction held exactly: the file had drifted from *itself*.

The dispatch tables were written against a **newer shape than the declarations
above them**:

| what | evidence |
|---|---|
| `HeliosEntry` missing a 4th member | every table row supplies one and `GetInstanceProcAddr` reads `e.phys` |
| 3 gate constants used, never defined | `HELIOS_GATE_{DEVICE_GROUP_CREATION_KHR,PHYSDEV_PROPS2_KHR,BIND_MEMORY2_KHR}` — and **both gate evaluators already had `case` arms for them** |
| `slots.resize(n)` on a non-movable type | `HeliosSlot` owns a `std::mutex`; `resize` requires MoveInsertable |

⛔ **The finding a compile alone would not have finished.** The three
`*_enabled` flags those gates read were never declared **and never set**.
Declaring them to satisfy the compiler would have left them `false` forever,
silently withholding `vkEnumeratePhysicalDeviceGroupsKHR`,
`vkGetPhysicalDeviceQueueFamilyProperties2KHR` and the bind-memory2 aliases with
no counter and no log line — a green build hiding a permanently dark path. They
are now observed during `CreateInstance`/`CreateDevice`, and *observed* is the
operative word: they are the app's extensions, the lower ICD implements them,
so they still travel down in `lower_exts` untouched.

**A wrong inference, caught.** clang warns
`-Wdll-attribute-on-redeclaration` on all eight exported loader entry points,
because the Vulkan headers declare them without `dllexport`. The natural
reading — "the exports are being dropped, the layer needs a `.def`" — is
**false**: `llvm-readobj --coff-directives` shows all eight `/EXPORT:`
directives present in the object. Asserting it would have produced an
unnecessary `.def` and a confident wrong claim in the record.

### And then it was actually built, which found two more

`-Dvulkan-layers=helios-present` now produces `VkLayer_HELIOS_present.dll` and
its loader manifest. The real build uses **mingw-w64 g++**, and it found two
things the clang-cl pass had not:

* **`name_prefix`.** mingw emits `libVkLayer_*.dll`, which does not match the
  manifest's `library_path`. ⚠ **Silent failure mode** — the loader simply never
  finds the layer and nothing reports why.
* **`-Wunused-function`** on `helios_is_layer_owned_instance_extension`, which
  turned out to be genuinely dead: its three names are a strict subset of the
  list that does the stripping, and the `CreateInstance` loop cannot use it
  because it must know *which* of the three matched.

### ⛔ Correction: the clang-cl detour was not worth what it looked like

This section first claimed "neither compiler alone was sufficient" and kept a
`tools/mesa-layer-syntax.ps1` clang-cl gate on that basis. **Both halves of the
claim were wrong, and it was never tested before being written down.**

Measured afterwards by reintroducing the same `HeliosEntry::phys` drift and
building with mingw:

```
ninja exit=1  elapsed=2s
  error: too many initializers for 'const HeliosEntry'   (x2)
  error: 'const struct HeliosEntry' has no member named 'phys'
```

* **Sufficiency.** mingw catches it. All 17 original errors were hard errors —
  undeclared identifiers, incomplete types, missing members, excess initializers
  — that any conforming compiler rejects. clang-cl found a strict **subset** of
  what mingw finds, not a complement.
* **Speed.** The incremental ninja rebuild is **2 seconds**, so the "cheap
  check" argument for a separate script was worth nothing either.

And the clang-cl pass was actively the weaker instrument: it compiled against
the **MSVC STL and Windows SDK**, which is not the standard library the shipping
DLL uses, and with **none of Mesa's `-Werror` set** — so it could as easily have
invented errors that do not exist in the real configuration.

⇒ **Use the compiler the project is written for.** The script is deleted; the
check is `ninja <the layer target>`.

Verified past "it links": `objdump -p` on the built DLL shows all eight loader
entry points exported, and the generated manifest's `library_path` matches the
filename actually produced. The layer also moved to its own directory, because
§2 item 8's acyclicity rule and §13.2's ban on the ICD importing d3d12/dxgi are
both violated by an arrangement that could fold these objects into
`idep_vulkan_wsi`.

⚠ **"Builds" is not "installed."** Packaging and loader registration are a
cross-lane request against `packaging/windows/Install-Helios.ps1`
(`OWNERSHIP.md` §1). Nothing loads this yet.
