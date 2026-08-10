# Phase-2 adversarial review, round 3

`docs/dx12/METHOD.md` is authoritative for sequencing: implement a subsystem to
its contract, adversarially review the **whole changeset** by lens, repair, and
repeat with different lenses until saturated. This file records round 3 over the
HPS2 retirement changeset.

**Date** 2026-08-10 · base `d1c820a`, reviewed at `2e04189` · `icd/mesa`
`8559b66299a..23ab1600820`, `vkd3d-proton-helios` `f3918d5e40a..912a3d4d04e`,
`qemu-helios` unchanged by design (`FINDINGS.md` F5).

**Eight lenses, a skeptic per lens, and a completeness critic. 53 raw findings,
30 survived refutation, 6 acceptance-gate defeats confirmed by running them.**

⛔ **Round 3 is NOT dry, and it is not close.** METHOD.md §3 criterion 1 needs two
consecutive dry rounds with different lens compositions. Round 4 must repair what
round 3 found, so round 4 cannot be the first dry round either: **the earliest
saturation can be reached is round 5.**

> Reviewer and author are the same party, which METHOD.md §2 phase 3 forbids in
> the parallel-lane case. As in round 1, recorded as a known weakening rather than
> glossed. Mitigations actually applied: each lens read the whole changeset rather
> than a slice, each finding was handed to a **separate** skeptic instructed to
> refute and to default to refuted, and the repairs were routed to seven authors
> over disjoint file sets none of whom reviewed.

---

## 1. Lens composition, and why these eight

Round 1 used ABI & tables, cross-lane seams, loud failure, contract completeness,
claim integrity, handles & lifetimes, concurrency, engine contract. Round 2 used
six rotated lenses plus a gate-defeating critic (recorded only in commit
`2e04189`'s message — ⚠ there is no `REVIEW-ROUND-2.md`, and that gap is why this
file exists in the form it does).

| lens | never applied before? | raw | survived |
|---|---|---|---|
| **security / §15**, row by row, plus the kernel threat model over user-supplied private data | yes | 3 | 1 |
| **failure policy §10.9**, row by row | yes | 4 | 1 |
| **deployment readiness** — what happens the moment this is installed | yes | 8 | 3 |
| **kernel safety** — IRQL, panics, unbounded work, `unsafe` — applied to the K4 code (round 1 applied it to `native_fence.rs` only) | to this code, yes | 4 | 1 |
| **producer/consumer field agreement** across all five components | yes (round 1's "cross-lane seams" checked *signature* drift) | 6 | 4 |
| **instrument attribution** — every counter graded | listed as a round-2 candidate, never run | 12 | 7 |
| **claim integrity re-derived against HEAD** | round 2 corrected claims *in the same commit*, so nothing re-checked them against the merged tree | 8 | 7 |
| **gate defeat** — write the patch, run the gate, report the exit code | yes | 8 | 6 |

The three the owner named — security/§15, §10.9 row by row, deployment-readiness
— produced 5 survivors between them. **The three biggest yields came from
elsewhere**: gate-defeat, claim-integrity, and instrument-attribution. Worth
recording, because the lens that finds the most is not the lens you expect.

---

## 2. The two code defects, and what they have in common

Both are **round 2's own repairs overreaching by one arm**. That is the same
shape as round 2's blocker, which came from the orchestrator's `byte_size`
ruling — see `orchestrating-parallel-authors` in the agent memory. A repair is
a change, and changes need the same review as the code they repair.

### 2.1 [major] The Tier-1 host-extent adoption fired on an arm with no stride to adopt

`create_allocation.rs`, the adoption block in `admit_hwa2`. **Three independent
lenses converged** — failure-policy, kernel-safety and producer/consumer — and
each skeptic re-derived it and failed to refute.

`HELIOS_HWA2_FLAG_STANDARD` is true for **both** surfaces
`dxgkddi_get_standard_allocation_driver_data` authors: the LINEAR scan-out
primary and the `OPAQUE_OPTIMAL` GDI texture. Round 2's repair keyed the
adoption on that flag and gated only the *plane* half on `created.pitch != 0`.
The OPTIMAL arm returns `pitch: 0` by construction, so `byte_size` moved to the
host's **tiled** requirement while `planes[0]` kept the author's 256-byte-aligned
**linear** estimate — two numbers with no defined relationship. Whenever the
tiled requirement came in under `cross_adapter_pitch(w) * h`, the KMD published a
descriptor that fails its **own** `validate_create_output` at
`PlaneRangeExceedsByteSize`, bumped `AcHwa2Out` — documented *"**Must read 0** —
a nonzero value is a driver bug"* — and refused a legal GDI-surface create. That
is DWM's redirected-window texture.

Second half, from the same block: `AcSize`'s undersize guard is `!kmd_authored`,
justified by "the KMD-authored arm has just SET `byte_size` to the backing's
size, so the comparison is an identity". True on the adopting arm; **false on the
OPTIMAL arm**, which therefore lost its Xid-31 undersize protection too.

**Repaired** by making the adoption what §1.3 Tier 1 step 2 actually says it is —
a *pair*: `byte_size` **and** the plane record move together, or neither moves.
Where there is no real stride to adopt there is no adoption.

⚠ **Two honest bounds the skeptics added, which the finding did not carry.**
First, the arithmetic is unverified: nobody has measured
`vkGetImageMemoryRequirements` for a tiled BGRA image on this host, so the
specific vectors (65×1024, 100×900) are constructed, not observed. The
*structural* defect does not depend on them — the guard compared a 256-aligned
linear estimate against a tiled requirement, quantities with no defined
relationship, where before the adoption the comparison was an identity. Second,
the trigger is a shape **class**, not a vector: any GDI texture where the tiled
requirement falls below `round_up_page(cross_adapter_pitch(w) * h)`. The
precedent that the host's number comes in *under* the KMD's estimate is already
on the record for the LINEAR arm (`SdgLReq=7910400` vs `linear_blob_size` =
8,912,896).

⭐ What the repaired OPTIMAL arm keeps instead, and why that is correct rather
than merely safe: `byte_size` is sized **from** the notional stride by its own
author precisely so the plane fits; §10.3 forbids a zero `row_pitch` on a
declared plane, so "describe the tiling honestly" is not expressible in this
record at all; nothing byte-addresses an `OPAQUE_OPTIMAL` allocation (`pitch`
resolves to 0, `bar_eligible` excludes the class, VidMm is charged
`max(backing, byte_size)`); and the backing **is** the image's own
`vkGetImageMemoryRequirements` answer, so it is exactly right for the image. This
is not the Xid-31 shape, which is a blob smaller than the requirement for the
*same* layout.

### 2.2 [major] `mappable` and `bar_eligible` disagreed, and BAR ⇒ mappable

`classify_hwa2` newly derives the venus blob's `mappable` flag from
`HELIOS_HWA2_FLAG_CPU_VISIBLE`; `allocate_memory_blob` then omits
`VIRTIO_GPU_BLOB_FLAG_USE_MAPPABLE`. `bar_eligible` had **no** CPU-visibility
term, so a D3D12 `D3D12_HEAP_TYPE_DEFAULT` resource (`CPU_NOT_AVAILABLE` ⇒ flag
clear ⇒ non-mappable blob) was published BAR-eligible, `vidmm_placement`
*preferred* it into the BAR, and the paging engine would issue
`VIRTIO_GPU_CMD_RESOURCE_MAP_BLOB` against a blob the host was never asked to
make mappable. Pre-K4 this could not happen: `mappable` was hardcoded `true`, so
every BAR-eligible blob was mappable by construction.

**Repaired** by adding the CPU-visible term and stating the invariant — *BAR ⇒
mappable* — at the site that depends on it. The `OPAQUE_OPTIMAL` exclusion
already said the same thing from the other side (`scanout.rs`: the OPTIMAL GDI
image "is deliberately not mappable").

⚠ Bounds: the live D3D11 desktop cannot reach it (the D3D11 producer sets
`CPU_VISIBLE` unconditionally) and `umd12` is behind the default-OFF `UmdD3D12`
knob, so this fires on the first boot with `UmdD3D12=1` plus an eviction — a
named, scheduled flip. Also, "the map fails" is asserted, not established:
virglrenderer is not in this tree, so the host's response to `RESOURCE_MAP_BLOB`
on a non-MAPPABLE blob is unverified. Even if the host tolerated it, the KMD
would be mapping memory it never asked to be mappable and `BAR_ERR_MAP` would
name the symptom rather than the cause.

---

## 3. The gate defeats — six, each with an exit code

⭐ **The method is the finding.** Round 2's completeness critic did not argue that
the §8.5 gate was weak, it *wrote the defeating patch and ran it*. Round 3 did
the same and broke the hardened gate four more ways. `K4-CONTRACT.md` §8 rows 5
and 6 name `tools/retirement-gates.sh` as their **sole** evidence, so a defeated
gate is an acceptance claim that is not true.

| # | gate | the defeat, reproduced at exit 0 |
|---|---|---|
| 1 | §8.5 | **compound assignment** through a tracked alias: `*stamp \|= 0x8000_0000`. The assignment regex excludes every compound operator |
| 2 | §8.5 | **pointer methods are never vetted**. `copy_from_nonoverlapping` is not a substring of `copy_nonoverlapping`; `copy_from` and `replace` are not on the 11-name list at all. `copy_to` **is** listed but `copy_from` is not; `.write(` **is** but `.replace(` is not |
| 3 | §8.5 | **`<*mut T>::as_mut()` breaks the alias chain** — the most idiomatic pointer→`&mut` conversion in Rust. Two plain **safe** field writes through it hit no rule. They land on `allocation_generation` (obligation 4's write-exactly-once field) and the flags word (`hwa2_is_direct_scanout_primary`'s routing key) — precisely the two values round 2 was spent repairing |
| 4 | §8.5 | **a callee is resolved by bare identifier**, `create_allocation.rs` searched first, module path never consulted. ⚠ Needs no planted decoy: 4 of the 421 top-level `fn` names in `kmd_render/src` already collide across files, and `round_up_page` is one of them |
| 5 | heap-flag cross-repo | a stale `/* Was: … 1u << 30 … */` comment above a live `1u << 31` makes it print `both sides agree: 1 << 30`. Two bare `re.search` calls with no comment awareness — in the one file that carries a 90-line comment/string/code classifier built for this |
| 6 | `abi_parity.py` | **never compares constant VALUES.** Mutation-tested all 176 shared constants: **103 diverge silently**, including every record magic and every ABI version |

Plus, not a defeat but the same class: **gate 5 SKIPs and the suite still prints
"ALL LINUX-SIDE RETIREMENT GATES PASS"**. It is conditional on `tmp/wdk-28000/`,
which `.gitignore` excludes. Demonstrated with an intentionally stale slot audit:
the same tree exits **1** with the headers present and **0** without them. This
bit the round itself — the gate-defeat lens's baseline was 7 PASS + 1 SKIP while
the orchestrator's was 8 PASS, on the same commit.

⛔ **What resisted, recorded so nobody "fixes" what works**: `core::ptr::copy`,
`core::mem::swap` and a bare `*mut` on an alias line are all caught; free-function
laundering fails loudly *by design*; every layout attack on the ABI gates failed
(all 36 mirrored structs have 100 % Rust-side offset-assertion coverage); and a
hex respelling of the vkd3d define fails **loudly** rather than silently.

⛔ **And one round-3 finding was itself false at HEAD**, caught by the
completeness critic: the gate-8 item is stated as a HEAD condition ("the live
`#define` is `1u << 31`"). Re-derived: `vkd3d_private.h` and
`resource12.rs` both say `1 << 30` — the sentence described the lens's **own
adversarial patch**. The gate weakness is real; the evidence was not. Since
CLAUDE.md names this the highest-risk divergence in the fork, a repair lane
reading that sentence would have "fixed" a value that is already correct.
METHOD.md §3 criterion 4 was failed by a round-3 lens, and the critic is what
caught it — which is the argument for having one.

---

## 4. The completeness critic: a fifth of the changeset had never been opened

~55 changed files no lens in any round had read. Most were benign; four were not,
and they are the round's most important output.

### 4.1 ⛔ `helios_umd12.dll` can be built on exactly one computer

`umd12/build.rs` hard-requires WDK 10.0.28000.2526 at
`C:\Users\Rupansh\helios-vgpu\tmp\wdk-28000\`, `require_path`s it, and **panics**
the build if the generated bindings cannot name `D3D12DDI_DEVICE_FUNCS_CORE_0116`.
On Windows `main()` always regenerates — the cached-bindings branch is
`if !cfg!(windows)`. `.gitignore` excludes `/tmp/`; `git ls-files tmp/` is empty.
No CI job stages it, no installer provisions it, and **`TOOLCHAIN.md`, which this
changeset edited**, still documents only kit 10.0.26100.0. The same untracked tree
is what gate 5 and `WDDM32_SLOT_AUDIT.md` depend on. This is the round-2 lesson
repeating one notch bigger: the unreviewed file was a **build input**, not code.

### 4.2 ⛔ K4-CONTRACT §8 rows 1–4 prove the wrong artifact

Rows 1–4 are proved by "`kmd_logic` model". **Measured: `kmd_render` calls zero
functions from `kmd_logic::allocation_identity`**, while calling 20 other
`kmd_logic` modules (`scanout_lease` 16 references, `present_stream` 13,
`snapshot_bind` 11, `round_up_page` 4, `choose_host_visible_memory_type` 2, …).
Every other `kmd_logic` module **is** the shipped implementation; this one is a
parallel re-implementation of ~856 lines with ~947 lines of tests beside it.

CLAUDE.md's invariant — *"New KMD unit tests go in `kmd_logic` … a `#[cfg(test)]`
module there can never run, so it is assurance that is not real"* — was satisfied
by **duplicating** the logic instead of **moving** it. That is the letter without
the purpose: 211 green tests over code the driver never executes.

**Ruling (orchestrator, 2026-08-10):** rows 1–4 are **downgraded from proven to
modelled**, and wiring `kmd_render` to call `kmd_logic::allocation_identity`
becomes a named outstanding unit. `round_up_page` and
`choose_{host_visible,device_local}_memory_type` are the precedent for how the
KMD already consumes `kmd_logic`.

⭐ This exposes a **fifth state** METHOD.md §3 criterion 6's four do not name:
**implemented twice, exercised once, and the exercised copy is the one nothing
ships.**

### 4.3 ~14.4k lines have no producer, no consumer, and no unreachability record

Unlike the native-fence surface — which *is* documented as unreachable at the
declared WDDM surface, with the argument written down — this population is in
none of METHOD.md's four states and no document says so:

| module | lines (`.rs` + C mirror) | measured |
|---|---|---|
| `protocol/src/translator_dispatch.rs` | 4298 + 2264 | **0 of 65** pub symbols used outside `protocol/`, though `OWNERSHIP.md` §4 names five consumers |
| `protocol/src/translation_session.rs` | 2181 + 580 | 0 real consumers |
| `protocol/src/diagnostics.rs` | 1414 + 696 | **0 ETW emitters anywhere in the tree** |
| `protocol/src/physical_memory.rs` | ~3000 of 3155 | 4 of 57 pub symbols; the rest is the HPM1 surface F5 **declined** |
| the KMD's HVM1/HOC1 admission surface | — | `HeliosVenusMemoryAllocationV1` / `HeliosOuterCommandAllocationV1` appear outside `protocol/` **only** in the consumer and the model |

⛔ **Consequence that matters operationally:** an **absent** `AcHvm1*` /
`AcSegRole1..4` / `AcSegHoc1` counter is not evidence about the segment table.
K4-CONTRACT §4's amended ruling presupposes role-1..3 creates arrive and are
refused; none can arrive. Both `admit_hvm1` and `admit_hoc1` now carry a
`⚠⚠ NO PRODUCER EXISTS` banner saying so, and the two comments that attributed
their unreachability to a **liftable** condition (the segment check, the `SURFACE`
flip) are corrected: the producer is mesa unit **A3**, and neither K2 nor the
activation switch changes that.

### 4.4 Four cross-lane requests filed in source comments and tracked nowhere

The sharpest is **`allocation_object::invalidate_all()` has no call site at all**,
so an adapter reset never invalidates an allocation generation and §14's
"all generations change together" is not met — the anti-stale mechanism is
defeated. Now registered, with the other three, in `ROADMAP.md`'s new K4
cross-lane register.

---

## 5. What else was repaired

Grouped; the per-finding detail is in the commits.

* **Instrument attribution.** `LINEAR_BLOB_SIZE_DIVERGENCE` (`BlbSzD`) had been
  left unscoped and was comparing the host's **page-rounded** blob size against
  the **UMD's** resource extent — two quantities §1.3 *requires* to differ — so it
  had degenerated into a census of allocations whose size is not a multiple of
  4096, i.e. most textures, while three places still described it as a
  measurement of `NV_LINEAR_ROW_ALIGN`/`NV_LINEAR_TAIL_SLACK`. Re-scoped to the
  KMD-authored arms, compared against the extent **as authored**, and re-graded
  at the declaration with the old grading quoted so the counter cannot be read
  under it. `BackingSize`'s doc, which explained itself in terms of `ap.size` and
  "ADOPTED allocations" — a mechanism §6 deletes — re-derived against HEAD.
* **The present-side A3 refusal was silent.** `PresentAllocInfo` is permanently
  `None`, so both `ddi/display.rs` consumers refuse on every call — through the
  pre-existing last-value breadcrumb `PBFlip`/`PBCpy = 0xE1`, which in that file
  already means "dxgkrnl handed us a handle we could not resolve", a
  handle-lifetime bug. The intended intermediate state was indistinguishable from
  a real defect, and a last-value write could not say whether it fired once or per
  frame. Now `0xEA` plus a named counter, `PrNoRid` — the symptom-side pair to
  `OaNoRid`'s cause side.
* **A validated-then-discarded field.** `admit_hvm1` passed `mappable: true` for
  all four roles under a comment that argued only roles 1–3, and
  `allocate_memory_blob` always allocates from the single host-visible type
  chosen at bring-up — so role 4 (`VulkanDeviceLocal`, `cpu_visible = false`,
  *"rejects map and has no CPU VA"*) would have been backed by mappable
  host-visible memory. `mappable` is now **derived** from the role's own
  placement, and a role whose memory class this client cannot honour is **refused
  by name** (`AcHvm1Mem`) rather than silently substituted. ⚠ That is a
  *capability* statement, not a schedule claim — it must not be confused with
  `segment_is_reported`, which §4 requires to stay a runtime check.
* **The ICD's hand-transcribed validator had drifted from the Rust**, in exactly
  the way `OWNERSHIP.md` §2 pair 3 warns: the layout is compile-time gated, the
  *rules* are not. `vn_helios_hwa2.c` applied `PRIMARY_VIDPN_SOURCE_NOT_CONCRETE`
  on **both** stages where `protocol` applies it only on `CreateOutput` — the one
  rule whose stage guard the Rust's own comment records as load-bearing, because
  without it the only legal create-input shape for a C44 D3D12 runtime primary has
  no legal spelling at all. Latent (no ICD producer builds a primary create-input),
  and its value is as **evidence that an unchecked transcription has in fact
  drifted once** — i.e. the concrete argument for the diff gate the file's own
  banner asks for.
* **Claim integrity.** The −10 banner shift that `lane-kmd-core.md` §1 records as
  applying to "the other five lane briefs" had been swept in **one** file; F1/F2
  and two lane briefs still carried pre-banner numbers. Four `befe8b1^`-era code
  comments cited `protocol/src/wddm.rs` line ranges that now land on a different
  struct — re-pointed at **symbols**, since that file churns. Eight `rg` commands
  the docs instruct the reader to run return **zero hits** because `\|` is a
  literal pipe in `rg`'s syntax; one of them is the command whose output the
  `kmd_logic` inventory cell claims to be, which is why all 22 of its module
  offsets are wrong. `lane-kmd-core.md` §2.6 said the file was 6344 lines when it
  has been 8144 since `07364d6` — written stale by the commit titled *"make the
  planning docs agree with the tree"*.
* **CI.** The changeset deletes `_ALLOW_COMPILER_AND_STL_VERSION_MISMATCH` on the
  argument that clang moved to 22.1.8, while `.github/workflows/windows-stack.yml`
  still pins LLVM **17.0.6** and `ci/windows/Build-Driver.ps1` still passes the
  define to the DXVK half — one job, one runner, two opposite toolchain
  assumptions. ⚠ `umd` is built **before** `umd12`, so this is the *first* failure
  rather than one hidden behind the pre-existing `HELIOS_VKD3D_BUILD` break; and
  the workflow triggers only on `wddm`, not this branch, so it has not fired.

---

## 6. Round 4 must rotate the lenses again

Not yet run. Candidates round 3 did **not** apply:

* **The repairs themselves.** Both of round 3's code defects were round 2's
  repairs overreaching by one arm, and round 2's blocker was the orchestrator's
  own ruling. A lens whose only job is to review *this round's diff* is the one
  the evidence asks for.
* **Data-flow through `kmd_logic` vs `kmd_render`**, once the wiring unit from
  §4.2 lands — the two implementations must be compared, not just compiled.
* **The unopened files**, now that §4 has named them: `translator_dispatch.rs`
  (4298 lines, still unread by any lens), `helios_present_layer.h`,
  `vn_renderer.h`'s rewritten output contract, `adapter12.rs`'s Core-DDI-0116
  negotiation surface.
* **A second gate-defeat pass** against the hardened gates. The §8.5 gate has now
  been defeated in two consecutive rounds, both times after being hardened.

⚠ And the standing bound, restated because it is the whole point of the exercise:
**saturation is not correctness.** Criteria 1–6 cannot see dxgkrnl's behaviour or
the host GPU's. Nothing in this changeset has ever run.
