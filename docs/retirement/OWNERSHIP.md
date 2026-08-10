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
| `kmd_render/build.rs` | KMD core | One bindgen invocation and allowlist. |
| `kmd_render/src/adapter/scanout.rs` | KMD display | KMD core's read-ledger deletions are a cross-lane request against it. |
| `kmd_logic/src/lib.rs` | shared, partitioned | 5596 lines, three lanes (batch/queue, native-fence lifecycle, plane). Partition by `pub mod` block; merge by whole-module insert; never interleave. |
| `umd_common/**` | D3D11 UMD | `umd12` compiles the same sources into a second cdylib. `umd_common/src/window.rs` must survive — D3D11 still needs it after D3D12 drops the legacy context. |
| `packaging/windows/Install-Helios.ps1`, `.cmd` | host/packaging | Mesa needs the implicit-layer JSON registered; UMD12 needs `UserModeDriverName[3]`. Both file requests. |
| `ROADMAP.md`, `DX12.md`, `CONFORMANCE.md`, `docs/dx12/**` | host/packaging, **last** | Consumes other lanes' finished summaries. A mid-flight edit conflicts. |
| `tools/**` | host/packaging | Both §17.3 and §17.7 order `tools/d3d11_kmt_shared_probe.cpp` deleted. Delete **once**, here. |
| `protocol/**` | protocol | Every other lane is a pure consumer and must never declare a wire record locally. |

## 2. The two atomic pairs

Changes that are only correct if they land in the same commit.

1. **`VKD3D_HEAP_FLAG_HELIOS_VENUS_EXPORT`** — hand-mirrored across two
   repositories, `vkd3d-proton-helios/libs/vkd3d/vkd3d_private.h` and
   `umd12/src/forward12/resource12.rs`, with no compile-time cross-check. The
   vkd3d-side and umd12-side edits are one change; the adoption path is broken
   in between. CLAUDE.md already names this the highest-risk divergence in the
   fork.
2. **`umd/bridge/bridge_icd_exports.*` resolver and the ICD-side export** —
   the D3D11 lane deletes the resolver, the Mesa lane deletes
   `helios_venus_memory_res_id`. One package, so one changeset.

## 3. The activation switch

`kmd_render/src/ddi/wddm_surface.rs`'s `SURFACE` constant is the single atomic
activation switch for the entire retirement, and **it is the last edit**. It
stays `Wddm2_1GpuMmu` until:

- the display lane's complete MPO3/Display-Core table is registered, **and**
- the native-fence DDI surface is complete, **and**
- the cold-DWM admission gate is armed.

A premature flip reproduces the `E_NOTIMPL` DWM failure the module's own docs
record at `wddm_surface.rs:25-28`. Whoever flips it cites both lanes.

## 4. Orchestrator decision: the private direct-dispatch ABI has one home

Three lanes independently reported this as unspecified (mesa A3, dxvk 6.14,
vkd3d 9). §10.4:1182-1184 requires "a private, versioned in-process interface
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
| `vkd3d-proton-helios` | Linux | `build-native-codex && ninja` — green, 102/102, tests included. |
| `qemu-helios` | Linux | `build-helios && ninja qemu-system-x86_64` — green. ⛔ **The HPM1/HLM1 memory lane is PARKED — see `FINDINGS.md` F5.** The submodule is reset to its pre-retirement state; the three HPM1 commits live on branch `helios/hpm1-parked`. Do not re-open them without running their adversarial review first, and do not add a new QEMU dependency to any lane. |
| `kmd_render`, `umd`, `umd12` | **VM only** | WDK/bindgen. Serialize: the VM is one machine. |
| `icd/mesa` | **VM only** | `win_meson`. |

The VM is a serial resource. Lanes may **write** concurrently; they **verify**
one at a time.
