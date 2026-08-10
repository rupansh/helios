# Phase-2 adversarial review, round 4 — and the last one

**Date** 2026-08-10 · base `d1c820a`, reviewed at `82a6474` · `icd/mesa`
`8559b66..1a432bf` · `vkd3d-proton-helios` `f3918d5..912a3d4` · `qemu-helios`
unchanged by design (F5).

**7 rotated lenses, a skeptic per finding, a completeness critic.
54 raw → 30 survived → 8 were code.**

⛔ **Round 4 is the last round.** The owner closed the loop on the measurement
below; `METHOD.md` carries the amendment and `ROADMAP.md` "Sequencing after round
4" carries the consequence. There is no round 5.

---

## 1. The measurement that closed the loop

| lens | new? | raw | survived | of which CODE |
|---|---|---|---|---|
| the previous round's own repairs | yes | 6 | 4 | 1 |
| gate defeat, 2nd pass | standing | 9 | 8 | 0 |
| model (`kmd_logic`) vs shipped (`kmd_render`) | yes | 7 | 2 | 0 |
| the files no lens had ever opened | yes | 8 | 4 | 4 |
| arithmetic & integer boundaries | yes | 8 | 2 | 2 |
| end-to-end record simulation, 4 producers × 1 consumer | yes | 4 | 1 | 1 |
| claim integrity over the repair round's own docs | rotated | 12 | 9 | 0 |
| completeness critic | standing | — | 9 | 1 |

**22 of the 30 survivors were prose** — documentation, claim integrity, counter
gradings, gate wording. The loop had started mining this project's own writing.
That is the argument for stopping, and it is a measurement rather than a mood.

⭐ The lens that found the most code was **"the files no lens had ever opened"**,
for the second round running. Round 3's version of it found a build input that
made `helios_umd12.dll` buildable on one computer; round 4's found four live
defects in the present layer. **"What did nobody look at?" outperformed every
lens designed to find a specific defect class.**

---

## 2. The eight code defects, all repaired

`b8ea245` (main) · `33db3fd` (`icd/mesa`) · `cdf1bce` (`vkd3d-proton-helios`)

**Kernel — would not have survived a deploy**

1. **`kmd_authored` was a wire bit any caller may set.**
   `HELIOS_HWA2_FLAG_STANDARD` is not in `HELIOS_HWA2_FLAG_KMD_OWNED_MASK`, and
   ⛔ *cannot be added to it* — the KMD's own standard-allocation private data
   re-enters through the same `DxgkDdiCreateAllocation` and would refuse itself.
   A hand-built HWA2 (`STANDARD` + `GDISURFACE` + `OPAQUE_OPTIMAL`,
   `byte_size = 1 GiB`) therefore skipped the `AcSize` undersize guard and
   published a descriptor over a 16 KiB backing.
   **Repair:** pin `byte_size` on **both** swizzle classes to the arithmetic
   `dxgkddi_get_standard_allocation_driver_data` actually uses, so a claimant must
   reproduce this driver's own number. ⛔ *Not* by widening the guard — that
   compares the OPTIMAL arm's notional LINEAR estimate against the host's TILED
   requirement and refuses legal GDI creates, which is round 3's defect §2.1.
2. **`venus/bringup.rs::round_up_page` was non-saturating** under a `DIVERGES`
   note whose premise ("callers all pass a host-reported requirement") K4
   falsified: `allocate_memory_blob` now takes an HWA2 `byte_size` from a user
   buffer, unbounded above. In the `dev` profile the KMD ships from, a size
   within 4095 of `u64::MAX` is a checked-add **panic inside a DDI**, in the
   `with_venus_client` closure — a permanent PASSIVE spin holding the venus
   mutex. Now the one saturating `kmd_logic` copy; there is no third.

**Present layer (`icd/mesa`), all four from the never-opened lens**

3. `state_event` is manual-reset, Set every present, never reset ⇒
   `vkAcquireNextImageKHR`'s only blocking primitive returns instantly and burns
   a core for the whole inter-frame wait, re-arming `SetEventOnCompletion` each
   pass. Reset under `sc->lock` before state is re-read.
4. `helios_wait_release` trusted one `WaitForSingleObject` on an **auto-reset**
   event with **two** consumers, so a stale latched signal could report
   "released" while the GPU still reads — with teardown as the caller. Loop on
   the fence value.
5. Three present-side failure arms left the slot `PRESENT_QUEUED` with
   `Release[i]` never signalled and `sc->lost` unset ⇒ `vkDestroySwapchainKHR`
   blocks forever. `helios_present_fail_slot` signals from the CPU.
6. `helios_admit` published `computed = true` then unlocked before filling the
   record ⇒ a racing query got `admitted=false / "not evaluated"` on an
   admissible adapter, plus a data race. Compute local, publish under the lock.

**UMD / engine**

7. D3D11's `byte_size` was `pitch × mip0 height` — one slice of one mip — while
   the descriptor declares depth, array size and mips and the KMD sizes the blob
   *from* it. A 64³ shared Texture3D got 16 KiB for 1 MiB: the Xid-31 shape,
   which `AcSize` cannot catch because the backing is built from the value under
   test. Now over-estimates; the desktop's shape is byte-for-byte unchanged.
8. vkd3d `CreateSharedHandle` formatted `resource` after releasing it.

**Verified after repair:** `retirement-gates.sh` 8/8 · `kmd_render` check exit 0
at the 22-warning baseline · `umd` release exit 0 · mesa ICD + present layer link
clean · vkd3d native build green.

---

## 3. What was found and deliberately NOT repaired

The 22 prose survivors, and the gate-defeat lens's 8 confirmed defeats (the §8.5
allow-list resolving an `impl`-scoped callee to a same-named free function; an
assignment rule suppressed by a substring test; `abi_parity` comparing
`#define`s with no preprocessor awareness; the §8.6 root set being a hardcoded
six-crate list; the heap-flag gate scoped to one file per side; the suite having
no automated invocation anywhere).

They are real. They are also, every one of them, defects in **checkers and
documents** rather than in code that runs — which is precisely the population the
owner's directive says to stop paying for. Recorded here so nobody re-derives
them, and so a future session that wants to harden a gate has the list.

⚠ **Two are worth acting on if a gate is ever relied upon as evidence again:**
`K4-CONTRACT.md` §8 rows 5 and 6 name `tools/retirement-gates.sh` as their
**sole** evidence, and that suite has now been defeated in three consecutive
rounds, each time after being hardened. A gate that is the only evidence for an
acceptance claim needs a different kind of assurance than another round of
patching.
