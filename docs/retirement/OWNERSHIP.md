# Retirement implementation: file ownership and sequencing

This is the orchestrator's map, distilled from section 4 of the six lane briefs.
It exists to make parallel implementation safe in one shared worktree. It is
**normative for who edits what**; the reference
(`docs/HELIOS_PRESENT_SYNC_RETIREMENT.md`) remains normative for *what* the code
must do.

A lane that needs a change in a file it does not own files a **cross-lane
request** naming the exact edit. It does not make the edit.

## 1. Single-owner files (concurrent edits forbidden)

| File | Owner | Why it cannot be shared |
|---|---|---|
| `kmd_render/src/lib.rs` | KMD core | One `build_ddi_table()`. The display lane needs 9 slot registrations, the native-fence lane 4; both submit their slot list to the owner as data. |
| `kmd_render/src/ddi/mod.rs` | KMD core | One module list. |
| `kmd_render/src/ddi/create_allocation.rs` | KMD core (**K4**) | Added 2026-08-10. It is the second most cross-lane-coupled file in the KMD after `lib.rs`, and K4 rewrites it. Measured with `rg -n 'create_allocation::' kmd_render/src/`: **`ddi/display.rs` — the display lane — imports or calls ten of its symbols**: `present_alloc_info`, `PresentAllocationStorage`, `ScanoutTarget` (`:14`), `present_alloc_diag` (`:349,350`), `scanout_allocation_for_resource` (`:866`), `allocation_resource_id` (`:1375`), `set_vidpn_primary_address` (`:1379,1635`), `scanout_alloc_info` (`:1502,2276`), `SCANOUT_ALLOC_FULL` (`:2061`), `submit_primary_scanout_copy` (`:2806`). (An eleventh name, `SCANOUT_ALLOCS` at `:862`, appears only in a comment.) Plus `ddi/build_paging_buffer.rs:60` (`paging_alloc_info`, `set_bar_placement` — K3), `ddi/cpu_host_aperture.rs:37,143,150,264` (`paging_alloc_info`, `PagingAllocInfo`, `APERTURE_MISSING_CPU_VISIBLE`, `LINEAR_BLOB_SIZE_DIVERGENCE` — K1 deletes that file, so K1 must re-home or delete those two counters), `ddi/submit_command.rs:212` (`RECLAIM_BAD_HANDLE` — K6), the six DDI re-exports at `ddi/mod.rs:54`, and a comment reference in `ddi/present_packet.rs:81`. Any rename or deletion in that set breaks another lane's file at compile time, in a build only the VM can run. |
| `kmd_render/build.rs` | KMD core | One bindgen invocation and allowlist. |
| `kmd_render/src/adapter/scanout.rs` | KMD display | KMD core's read-ledger deletions are a cross-lane request against it. |
| `kmd_logic/src/lib.rs` | shared, partitioned | **8144** lines at HEAD (`wc -l`; this row said 5596, and `lane-kmd-core.md` §2.6 said 6344 — both were wrong on the day they were written, so **do not cite a figure from a doc, run `wc -l`**). Four lanes now, not three: batch/queue, native-fence lifecycle, plane, **and `allocation_identity` (the K4 model)**. Partition by `pub mod` block; merge by whole-module insert; never interleave. Enumerate the blocks with `rg -n -e '^pub mod ' -e '^mod ' kmd_logic/src/lib.rs` — ⛔ **`-e`, not `'^pub mod \|^mod '`**: `rg` reads `\|` as a literal pipe and returns nothing. |
| `umd_common/**` | D3D11 UMD | `umd12` compiles the same sources into a second cdylib. `umd_common/src/window.rs` must survive — D3D11 still needs it after D3D12 drops the legacy context. |
| `packaging/windows/Install-Helios.ps1`, `.cmd` | host/packaging | Mesa needs the implicit-layer JSON registered; UMD12 needs `UserModeDriverName[3]`. Both file requests. |
| `ROADMAP.md`, `DX12.md`, `CONFORMANCE.md`, `docs/dx12/**` | host/packaging, **last** | Consumes other lanes' finished summaries. A mid-flight edit conflicts. |
| `tools/**` | host/packaging | Both §17.3 and §17.7 order `tools/d3d11_kmt_shared_probe.cpp` deleted. Delete **once**, here. |
| `protocol/**` | protocol | Every other lane is a pure consumer and must never declare a wire record locally. |

## 2. The three atomic pairs

Changes that are only correct if they land in the same commit. (This section
named two until 2026-08-10; the third was found by K4's ground-truth survey and
is the one K4 sits on.)

1. **`VKD3D_HEAP_FLAG_HELIOS_VENUS_EXPORT`** — hand-mirrored across two
   repositories, `vkd3d-proton-helios/libs/vkd3d/vkd3d_private.h` and
   `umd12/src/forward12/resource12.rs`, with no compile-time cross-check. The
   vkd3d-side and umd12-side edits are one change; the adoption path is broken
   in between. CLAUDE.md already names this the highest-risk divergence in the
   fork.
2. **`umd/bridge/bridge_icd_exports.*` resolver and the ICD-side export** —
   the D3D11 lane deletes the resolver, the Mesa lane deletes
   `helios_venus_memory_res_id`. One package, so one changeset.
3. **The ICD's allocation-record declaration and the KMD's create-time
   admission** (added 2026-08-10; K4-CONTRACT §7). ⭐ **This pair was declared
   here while it was enforced by nothing but this paragraph. It is now enforced
   at compile time, and the paragraph is rewritten to say what changed.**

   *What it said, and why:* `vn_renderer_helios.c` hand-declared
   `helios_wddm_alloc_private`, `helios_wddm_alloc_meta`,
   `helios_wddm_open_identity` and `helios_wddm_external_private` in C, guarded
   only by `_Static_assert(sizeof(...) == 48/48/48/96)` — **size, not offsets** —
   and the ICD included nothing from `protocol/include`. A field reorder inside
   the right number of bytes therefore passed every check on both sides.

   *What now enforces it* (measured on `icd/mesa` HEAD `23ab160`, "helios: read
   HWA2 from protocol/, delete the hand-mirrored 48-byte records"):

   - All four local declarations are **gone**; `grep -rn
     'helios_wddm_alloc_private\|helios_wddm_alloc_meta\|helios_wddm_open_identity\|helios_wddm_external_private'
     icd/mesa/src/` returns only comment tombstones (`vn_renderer_helios.c:243-244`).
   - `vn_helios_hwa2.h:52` does `#include "helios_wddm.h"`, so
     `protocol/include/helios_wddm.h`'s **82 `offsetof` assertions** (of 113
     `HELIOS_WDDM_STATIC_ASSERT`s total; 25 of them on
     `HeliosWddmAllocationDescV2` and 3 on `HeliosWddmPlaneRecordV2`) are
     evaluated inside the ICD's own translation units — `vn_helios_hwa2.c`,
     `vn_renderer_helios.c` and `vn_renderer.h`. That is strictly stronger than
     the `sizeof` guards it replaces: a reorder now fails the ICD's build.
   - The include path is **checked, not assumed**:
     `icd/mesa/src/virtio/vulkan/meson.build:163-171` derives
     `<mesa source root>/../../protocol/include` and calls `error()` if
     `helios_wddm.h` is not there, so a mesa checked out away from the parent
     tree fails configure with a named message instead of finding a stale copy.

   ⇒ Mesa unit **A0** as it was scoped here — "adopt the generated C header,
   assert every offset" — is **done**, and this pair is no longer the
   ungated one.

   *What remains manual, and is the residual risk:* the header pins the
   **layout**, and nothing pins the **rules**. `vn_helios_hwa2.c` transcribes
   `HeliosWddmAllocationDescV2::from_private_data` and the two stage validators
   from `protocol/src/wddm.rs` by hand, in the Rust's order, and its own banner
   (`:17-20`) says so: "there is no compile-time link between the two … until
   `tools/retirement-gates.sh` has a diff gate over these two bodies, this
   pairing is maintained by review." The KMD-side and ICD-side edits are still
   one change for that reason, and a rule added on one side without the other is
   still silent.

## 3. The activation switch

`kmd_render/src/ddi/wddm_surface.rs`'s `SURFACE` constant is the single atomic
activation switch for the D2/native-fence package. **D9 crossed that boundary on
2026-08-13**: it is now `Wddm3_2GpuMmu`, with `KMD_D2_OWNER_ENABLED` and native-
fence advertisement derived solely from `SURFACE`. The terminal callback table,
MPO3/Display-Core surface, and native-fence surface landed together before the
flip.

That source/boot activation is **not** the activation of the whole HPS2
retirement. KMD 22.22.296.0 now also carries K2a's documented shared-backing CPU
view and K11's per-session stock-Venus host transport, but the display remains
runtime-unadmitted. Mesa A3/A4's escape-free consumer cutover still precedes the
cold-DWM visible-admission retry and every HPS2 demolition step.

Do not infer display admission from the raised surface, callback counts, Code 0,
WDDM 3.2, K2a mapping, counters, or hashes. Escape and the HWQueue family remain
NULL; no later lane may add a second activation switch or fallback carrier.

K11 has one ownership root: the ordinary HVC1-created, heap-pinned
`SessionObject` reached through the exact raw KMD device. That object owns one
distinct stock Venus context/object namespace and private host reply target;
the canonical role-1 allocation/open object supplies a direct strong binding,
and HQA1 outer contexts retain direct session/endpoint references. Neither INIT
nor submit may discover that graph by PID, global/name lookup, renderer resource
id, or heuristic. A fixed per-adapter completion rundown spans only the final
current-generation admission through the exact WDDM notification; it owns no
session identity, host namespace, queue, or lookup, and reset/Stop/Remove close
and join it before host teardown. Teardown closes session admission, drains
exact host operations,
destroys the host instance/resource/context, and only then releases the session
and K2a backing references. This boundary landed and was reset-exercised on
22.22.296.0; it grants no ownership of Mesa A3/A4, K1 demolition, or later
allocation/GPU work.

## 4. Orchestrator decision: the private direct-dispatch ABI has one home

Three lanes independently reported this as unspecified (mesa A3, dxvk 6.14,
vkd3d 9). §10.4:1192-1194 requires "a private, versioned in-process interface
… between the D3D UMD bridge, DXVK/vkd3d, and Helios Mesa" with no global
discovery, and §2.8 requires translators to reach the ICD through it rather
than the Vulkan loader — but the reference never names the entry point, its
signature, or its versioning.

Left to the lanes, three repositories would invent three ABIs.

**Decision.** It gets exactly one declaration, in `protocol/`, consistent with
the standing directive that shared private data has one declaration there:

- `protocol/src/translator_dispatch.rs` — the Rust source of truth: the entry
  point name, the versioned table layout, the generation handshake, and the
  `RECORD_ONLY` submission-mode constant.
- `protocol/include/helios_translator_dispatch.h` — its mechanical C mirror,
  with `_Static_assert` twins, for Mesa, DXVK and vkd3d.

Mesa **implements** it; DXVK, vkd3d, `umd/bridge` and `umd12/bridge`
**consume** it. No consumer may declare its own copy of the table, the entry
point name, or the mode constant.

## 5. Submodule pointer bumps

`dxvk-helios`, `icd/mesa`, `qemu-helios`, `vkd3d-proton-helios` are submodules.
Every change is a submodule commit **plus** a root-repo pointer bump, and the
pointer bump is a root-tree change that conflicts with any other lane's root
commit. Batch one pointer bump per submodule at the end of that submodule's
work, never per unit.

## 6. Build reachability

| Lane | Verifiable on | Note |
|---|---|---|
| `protocol` | Linux | `cd protocol && CARGO_TARGET_DIR=target/linux cargo test` — there is **no workspace root**, so `-p` from the repo root fails. |
| `vkd3d-proton-helios` | Linux | `build-native-codex && ninja` — green. Tests are **545**, run by `./tests/test-runner.sh build-native-codex/tests/d3d12`, NOT by `meson test` (which reports "No tests defined"). ⚠ The old "102/102" here and the "215/215" in agent memory were both wrong and disagreed with each other. Expect **2 failures**, `test_nvx_cubin` and `test_destruction_notifier_interfaces`; both reproduce on unmodified upstream `2c7ba22c` and neither is ours — see `REVIEW-ROUND-1.md`. The second is concurrency-dependent, so its count varies with `-j` and with machine load. |
| `qemu-helios` | Linux | `build-helios && ninja qemu-system-x86_64` — green. ⛔ **The QEMU HPM1 lane remains PARKED — see F5.** The owner made one scoped K2a exception on 2026-08-13: rebase the existing Helios scanout commits onto upstream and import WDDM `ShareBackingStoreWithKmd` guest pages into Venus through stock udmabuf. Five commits, 143 additions / 32 deletions, no HPM1 negotiation/paging protocol and no virglrenderer change. K11 landed through existing stock virtio-gpu/Venus operations with this repository frozen at `415a5ef`; it creates no QEMU, virglrenderer, HPM1, or kernel-parameter dependency for later lanes. |
| `kmd_render`, `umd`, `umd12` | **VM only** | WDK/bindgen. Serialize: the VM is one machine. |
| `icd/mesa` | **VM only** | `win_meson`. |

The VM is a serial resource. Lanes may **write** concurrently; they **verify**
one at a time.

## 7. Orchestrator decision: allocation identity has one normative contract

`docs/retirement/K4-CONTRACT.md` is **normative for the allocation identity
subsystem** — HWA2, HVM1 and HOC1 as they cross `DxgkDdiCreateAllocation` /
`DxgkDdiOpenAllocation`. It exists for the same reason §4 does: the frozen
reference leaves real holes, and left to the lanes each would fill them
differently.

It settles four things no lane may re-decide on its own:

- **HWA2 is a two-stage record.** The UMD supplies a create-*input* HWA2; the
  KMD validates it in full and performs the write of all 168 output bytes. The
  reference's "KMD writes it only on create" stays literally true. Nothing in
  `protocol/` enforced an input contract before — `validate()` requires
  `allocation_generation != 0`, so it can only ever check the output side, while
  HVM1 and HOC1 both already have the pair. `Hwa2Stage::{CreateInput,
  CreateOutput}` and the two validators belong to the **protocol lane**,
  mirroring HOC1's naming exactly. (⛔ **Re-derived 2026-08-10 — all three line
  numbers in the earlier version of this bullet were wrong, and they disagreed
  with `K4-CONTRACT.md`, which is what makes a reader distrust both documents.**
  Cite the **symbols**, which cannot drift: `pub enum Hwa2Stage`
  (`protocol/src/wddm.rs:586`, not `:568`); the shared cross-field core is `fn
  validate_stage` (`:875`, doc block from `:858` — the old `:852` lands inside
  `pub const fn has_flag`, a different function); and the rejection variants
  `AllocationGenerationNonZeroOnInput` / `KmdOwnedFlagSetOnInput` are at
  `:769`/`:776`, not `:743`/`:750`. Regenerate with
  `rg -n -e 'pub enum Hwa2Stage' -e 'fn validate_stage' -e 'AllocationGenerationNonZeroOnInput'
  -e 'KmdOwnedFlagSetOnInput' protocol/src/wddm.rs`.)
- **HVM1 gains `from_private_data`.** It was the one record of the three with no
  length-and-alignment constructor; without it a short user buffer is an
  out-of-bounds *kernel* read. (Measured 2026-08-10: landed at
  `protocol/src/native_render.rs:1963`.)
- **The ICD re-point is NOT a substitution.** HWA2 deliberately carries no host
  resource id and no Vulkan memory-type index, so the fields four consumers read
  out of the retired 48-byte `HeliosWddmOpenIdentity` have **no successor
  field** — the replacement is a different mechanism (the KMD patches the host
  resid in from `HeliosNativeRenderPatch`), which is Mesa unit **A3** plus K6.
  Any reader of a field HWA2 does not carry must fail loudly with a named
  counter that names A3; none may fall back or fabricate. See §5 of the
  contract.
- **The VidMm tracker has no successor.** `GlobalVidMmTracker`,
  `HELIOS_WDDM_ALLOC_KIND_TRACKING`, `adapter/tracking.rs` and the create-time
  attestation die together. `protocol/src/wddm_legacy.rs`'s claim that it was
  "folded into HWA2's own tracking-kind fields" is false — `grep -in track
  protocol/src/wddm.rs` returns nothing — and the protocol lane corrects that
  line. (Measured 2026-08-10: the correction is already in the working tree, and
  `kmd_render/src/adapter/tracking.rs` has been deleted.)

Where K4-CONTRACT contradicts `lane-kmd-core.md`, the contract wins and the lane
brief is corrected in the same changeset (done 2026-08-10: §2.1, §2.2's
`create_allocation.rs` row, §2.3's `kobj.rs`/`backing.rs`/`tracking.rs` rows,
§3's K4 row, §4, §6 ambiguity 4).
