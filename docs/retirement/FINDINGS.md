# Retirement target-gate findings

Measurements taken against the real target that **revise claims in the frozen
reference** (`docs/HELIOS_PRESENT_SYNC_RETIREMENT.md`). The reference is frozen
as of corrective pass 82 and is not rewritten in place; where a measurement
contradicts it, the measurement wins and is recorded here.

Each entry states what was measured, on what, and — importantly — the **bound**
on the claim, so a later reader cannot inflate it.

---

## F1 — Core DDI 0116 negotiates on build 26100. The "build 28000" package minimum is a header artifact.

**Date** 2026-08-10 · **Revises** §2 (lines 114-115, 309-317), §3 (line 381),
§10.2 (line 982), §10.9 (line 2843), §18.1 (line 4639)

The reference makes "Windows 11 26H1 OS build 28000 or later" part of the
package ABI with "no version or feature fallback", and §10.9 fails
adapter/device/fence creation below Core DDI 0116. Its own §1 justifies the
28000 minimum solely by the observation that WDK 26100's `d3d12umddi.h` "ends at
Core build 0110; it cannot supply the selected open association" — a statement
about a **header**, not about a kernel.

**Measured.** Guest build 26100.8875, inbox runtime (`d3d12.dll` and
`d3d12core.dll` both `10.0.26100.8737`, no Agility redist). Advertising a
one-element supported-version set containing `D3D12DDI_SUPPORTED_0116`:

```
GetSupportedVersions: advertising _0116 token=0x000c005000740000
CalcPrivateDeviceSize: Flags=0x0 -> 40
CreateDevice: _0116 Interface=0x000c0050 (major=12 minor=80) Version=0x00740000 (build=116)
```

The control arm advertising `_0110` received build 110 and created a device
normally, byte-identical to the pre-knob driver. The runtime's own mismatch
string *"Failed to find matching DDI versions"* — which is present in
`d3d12core`'s string table — never fired in any arm. The conclusion rests on the
**pairing across arms**, not on a single line.

**Bound.** This shows the runtime does not *reject* 0116 and hands it back. It
does **not** show the runtime exercises 0116 semantics — whether it asks
`D3D12DDICAPS_TYPE_0112_NATIVE_FENCE_SUPPORT` or expects `pfnCreateFence_0116`
is untested by design, because the experiment's refusal gate fires at
`pfnCreateDevice` before the caps gauntlet and before any `pfnFillDDITable`.
That question needs the 0116 tables to exist first.

**Consequence.** Core 0116 is reachable on this guest. The remaining input is
regenerating `umd12/bindgen/cached/d3d12umddi.rs` against WDK 28000, whose
headers are staged at `tmp/wdk-28000/` (gitignored, nupkg SHA-256 matches the
hash §1 records) and readable from the VM over `Z:\`. `umd12/build.rs` already
honours `HELIOS_WDK_INCLUDE`.

Knob: `Umd12CoreDdi`, default 110. Both arms stay reachable per CLAUDE.md rule 8.

---

## F2 — A CpuVisible memory segment does not Code-43. The HLM1 flag shape is admitted.

**Date** 2026-08-10 · **Revises** the CLAUDE.md invariant table · **relieves**
§10.9 line 2858 and §2 line 17 of their assumed failure mode

The kmd-core lane brief called this "the single highest-risk item in the lane":
§10.7:1973-1976 requires HLM1 to be `Aperture=0, CpuVisible=1,
CacheCoherent=0, SupportsCpuHostAperture=0, SupportsCachedCpuHostAperture=0`,
which is precisely the shape CLAUDE.md recorded as *"classic CpuVisible memory
segments are rejected — AddAdapter Code 43 (ETW-proven 2026-07-05)"*. §10.9
makes such a rejection a package rejection with no fallback.

**Measured.** `BarSegFlags=0x02` → `pnputil /restart-device` → adapter
`Status=OK`, `Problem=CM_PROB_NONE`, mode preserved at 1896x1030, and
`helios_paintcap` returned a **fully composited live desktop** — wallpaper,
taskbar, icons, live clock. Reverted to the `0x1C` default in the same script;
also OK. The `BarF` breadcrumb moved `28 → 2 → 28` across the arms, so the knob
provably took effect that boot rather than being read stale.

**Bound.** This is the **current** segment table with the BAR segment's flags
changed. It is not §10.7's exact two-segment profile (aperture id 1 + HLM1 id 2
with their exact base/size/CommitLimit), and the driver still **registers** the
Map/UnmapCpuHostAperture callbacks that §17.6 deletes. So the flag combination
is admitted and non-fatal; the full HLM1 profile remains an unexecuted gate.

**Consequence.** The KMD memory lane is not blocked on a Code-43 wall. The
CLAUDE.md invariant has been corrected in place with this evidence.

---

## F3 — The guest is Windows 11, and `ProductName` says otherwise.

**Date** 2026-08-10

`HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\ProductName` reads
**"Windows 10 IoT Enterprise LTSC 2024"**, but the desktop watermark reads
**"Windows 11 IoT Enterprise LTSC, Build 26100"**. 26100 is the 24H2 kernel and
carries WDDM 3.2. Do not classify this guest from `ProductName` — it is stale,
and reading it as "Windows 10" produces a false conclusion that the target OS is
wrong.

Corroborating: WDK 26100's `d3dkmddi.h`/`dispmprt.h` already declare every KMD
native-fence slot (`DXGKQAITYPE_NATIVE_FENCE_CAPS`, `DXGK_NATIVE_FENCE_CAPS`,
`DXGK_INTERRUPT_NATIVE_FENCE_SIGNALED`) and all seven MPO3/Display-Core slots.

---

## F4 — `pfnFillDDITable` runs after `pfnCreateDevice`.

**Date** 2026-08-10 · **Revises** `docs/dx12/ARCHITECTURE.md` §1.2

Measured order on 26100.8737:

```
OpenAdapter12 -> GetCaps(1074) -> GetSupportedVersions x2 -> CalcPrivateDeviceSize
  -> CreateDevice -> GetCaps x24 -> GetOptionalDDITables -> FillDDITable x5
```

This matters for the Core-0116 uplift: the table-shape hazard is at
`pfnCreateDevice`, not at `pfnFillDDITable`, so a version gate placed at
CreateDevice is upstream of every fill.

---

## F5 — HPM1 is not the only way to satisfy C63, and its C55 benefit is unrealized. The QEMU memory lane is parked.

**Date** 2026-08-10 · **Owner decision** · **Revises** §10.7, §17.7, and the
C55/C63/C64 rows of the constraint table · **Supersedes** the QEMU half of the
implementation state recorded in agent memory

### What was parked, and where it went

`qemu-helios` is reset from `6a7398a72d` to `d4fde50ccb`, dropping the three
retirement commits — `6d4f8ec90e` (the 726-line HPM1 C mirror), `8bad754bbd`
(HPM1 negotiation + HLM1 BAR admission, ~970 lines), and `6a7398a72d` (the
paging-DMA executor, ~2,780 lines, of which 2,442 are in
`hw/display/virtio-gpu-virgl.c`).

They are **preserved, not deleted**: branch `helios/hpm1-parked` and tag
`helios-hpm1-parked-2026-08-10` in the submodule. Nothing walking the tree
reaches them, which is the point — an agent picking up "review the paging
executor" should not find 2,442 lines of unreachable DMA parser to work on.

The six pre-retirement commits stay. They are what makes the desktop composite
(native Venus OPTIMAL scanout, modifier-backed scanout reconstruction, the SDL
EGL validation, the scanout oracle, the HOST3D blob budget, non-power-of-two
hostmem) and CLAUDE.md names the fork by them. `ninja qemu-system-x86_64` is
green at the reset state.

### Why

Three reasons pointing the same way.

1. **Maintenance.** ~4,500 lines concentrated in one upstream file is the
   divergence that makes every rebase expensive. The scanout commits are light
   and cherry-pickable; this one is not.
2. **It was unreviewed *and* unreachable.** Its adversarial review never ran,
   and HPM1 is entered only by explicit guest negotiation — BAR admission at
   realize just validates a BAR that already exists — so today's guest never
   negotiates and the executor never runs. Dead code that parses guest-supplied
   DMA is the worst of both: all of the review debt, none of the value.
3. **The benefit it buys is not one we are collecting.** See C55 below.

### C63 without a host-side resolver

§C63 is a real constraint: `DXGKARG_SUBMITCOMMANDVIRTUAL` gives the KMD a
`DmaBufferVirtualAddress` and size, *not* a CPU pointer to the command bytes.
The reference concludes that QEMU/HPM1 must read the guest's page tables.

It does not follow, because §C65 already constrains the buffer those GPUVAs
point into: the D3D12 HOC1 command pool is **one** allocation — 64 MiB, created
by `pfnAllocateCb{hResource=NULL}`, Lock2-mapped for its whole lifetime, with a
stable reserved GPUVA and at most 256 live 64-KiB-aligned extents. Our KMD
created that allocation, so it can hold a permanent kernel mapping of it, and
resolution is arithmetic:

```
validate DmaBufferVirtualAddress ∈ [pool_base_gpuva, pool_base_gpuva + pool_size)
offset   = DmaBufferVirtualAddress - pool_base_gpuva
HOB1     = kernel_mapping + offset
```

No page-table walk, no mapping at `DISPATCH_LEVEL`, no host-side resolver. It is
**stricter** than HPM1 on the invariant C63 states — "KMD never dereferences
arbitrary user GPUVA" becomes "KMD dereferences only its own allocation at a
bounds-checked offset" — so this is a narrowing of privilege, not a workaround.
The KMD then emits an ordinary `SUBMIT_3D`.

⚠ **Bound.** This is an argument from the two sections' text, not a measurement.
It has not been implemented or run. What makes it credible rather than merely
plausible is that it needs no new mechanism: the pool, its lifetime lock, and
its stable GPUVA are all already required by §C65 for other reasons.

There is also **option zero**, which is what is actually running: do not adopt
GPUVA/HOB1 submit for D3D12 at all. vkd3d translates to Vulkan and needs no
D3D12 GPUVA semantics *from the KMD*. The D3D12 lane reached device + queues +
command lists + DXIL PSOs on the existing submit path with no HPM1, and its one
blocker — `CreateSwapChainForHwnd` → `ShareObjects` — is not something HPM1
would fix.

### What C55 buys beyond isolation

§C55 has QEMU substitute renderer-private resource IDs into a host-private
command copy *after* VidMm's final Patch. Two things come out of that:

- **Isolation** — no host resource ID reaches user-mode storage or protocol
  state, so a buggy or hostile guest UMD cannot forge one. In Helios's threat
  model the guest is the user's own VM and host virglrenderer already validates
  resource IDs per context, so this is defence in depth.
- **Late binding of placement** — because substitution happens after Patch, a
  command names the placement current at execution, which makes WDDM eviction
  and relocation correct without the UMD re-recording. This is the genuine
  architectural benefit, and it is not a security property.

**But it only pays off if allocations actually move, and today they do not.**
The working stack pins, and HWA2's `expected_allocation_generation` already
refuses a stale batch. So the late-binding benefit is unrealized and stays
unrealized until real eviction exists. If the substitution is still wanted, it
can be done guest-side in the KMD as it builds the `SUBMIT_3D` — losing "the
host is the authority on identity", which is the property being traded away and
should be recorded as such wherever it is relied on.

### Consequence

The KMD memory lane no longer has a QEMU dependency, and no lane in flight has
one: the translator dispatch ABI is in-process guest code, the native-fence DDI
surface and the WDDM 3.2 slot audit are guest-side, and D3D12 is guest-side.
Reopening HPM1 means un-parking the branch **and** running its adversarial
review first — review before reachable, not after.

---

## F6 — The slot audit's refusal path is proven on the target. `fa8489e` is verified in both halves.

**Date** 2026-08-10 · **Grades** §18.1:4653-4656

`verify()` returning `Ok` was verified earlier the same day (ring
`S1 = 0x0DA00001`, adapter `OK`/`CM_PROB_NONE`). That half establishes almost
nothing on its own: a `verify` that returned `Ok` unconditionally would have
produced byte-identical evidence. The refusal path had never executed.

**Measured.** One row of the classification flipped and nothing else:
`DxgkDdiSetNativeFenceLogBuffer`, index 187 of 192, `Disabled` -> `Implemented`.
Pre-flighted statically first — the slot is never assigned in
`build_ddi_table()`, so it is genuinely NULL and the mismatch is real rather
than assumed. Built as `22.22.261.0` and cold-booted. The breadcrumb ring was
deleted to zero entries beforehand, so every value below was written by that
boot.

```
Status  : Error
Problem : CM_PROB_FAILED_DRIVER_ENTRY          (ProblemCode 37)
DEVPKEY_Device_ProblemStatus : 3221225858      = 0xC0000182
DEVPKEY_Device_DriverVersion : 22.22.261.0

ring count = 3
  S0 = 0x0D000001     DriverEntry entry
  S1 = 0x0DA010BB     audit failed at slot index 187   (0x0DA0_1000 | 187)
  S2 = 0x0DA02002     NULL slot classified Implemented
```

Four things this establishes that the positive did not:

1. **The refusal fires.** The driver did not load.
2. **It names the row that actually disagreed.** `0x0DA010BB` decodes to index
   187 — the row that was flipped, not merely "some failure".
3. **It names the direction.** `0x0DA02002` is the NULL-but-classified-
   `Implemented` arm, which is the arm this flip should take.
4. **`DxgkInitialize` was never called.** The ring holds exactly three records
   and `0x0D000002` is absent, so the disagreeing table never reached dxgkrnl.
   Refusing "at the one point at which the table is still ours" is what
   `DriverEntry`'s comment claims, and it is what happened.

The strongest single line is `ProblemStatus = 0xC0000182`: PnP propagated the
audit's own `STATUS_DEVICE_CONFIGURATION_ERROR` verbatim, so a Code 37 in
Device Manager is traceable to this check rather than to a coincidental load
failure with the same symptom.

**Bound.** One row, and one of the two failure arms. The mirror arm —
registered-but-classified-unreachable, `0x0DA0_2001` — is the sibling branch of
the same `if` and remains unexecuted. The walk was proven to reach index 187 of
192, which rules out an early-terminating loop, but not to reach 191.

**Consequence.** The audit can be relied on as a gate by the lanes that will
reclassify rows — in particular the eight `Retiring` -> `Disabled` flips the
KMD-core and host/tools lanes owe. Reverted to the classification of `97ad14b`
(byte-identical, generator `--check` clean) and reinstalled as `22.22.262.0`:
`OK`/`CM_PROB_NONE`, `S1` back to `0x0DA00001`, and `helios_paintcap` returns a
fully composited live desktop.

### A defect this found on the way in

The audit file says "GENERATED — do not edit by hand" and names its regenerate
command. Running that command **reverted the arming fix**: the `Retiring`
class — whose own doc comment says "without this class the audit cannot be
armed at all" — existed only in the hand-edited `.rs`, never in
`gen_wddm32_slot_audit.py` or `wddm32_slot_classes.tsv`. Regenerating
reclassified the eight live slots (`DxgkDdiEscape` and the seven HWS/HWQueue
slots) back to `Disabled`, the state they will *reach* rather than the state
they are *in*, and `verify` would then have refused to load a correct driver.
Fixed in `97ad14b`; the generator now round-trips byte-identical and `--check`
is clean. Check the checker against the thing it checks — the same lesson the
audit table itself taught when it classified 8 live slots as "must be NULL".

---

## F7 — `VK_LAYER_HELIOS_present` loads and runs. It refuses every device, and the reason is measured: the ICD supports exactly one Win32 external handle type, and it is not one the reference asks for.

**Date** 2026-08-10 · target 22.22.263.0, loader 1.4.350, Mesa
`26.2.0-devel (git-8559b66299)`.

The layer had never executed. It does now — installed by
`tools/install-helios-present-layer.ps1`, run through
`tools/run-helios-layer-app.ps1`:

```
[helios-wsi] instance ... created (surface=1 win32=1 caps2=1)
[helios-wsi] REFUSE physdev_refused_no_external_image_import (-3): external image
             query: FORMAT_NOT_SUPPORTED - the lower ICD does not support
             D3D12_RESOURCE_BIT for the C37 tuple
[helios-wsi] REFUSE create_device_refused_not_admitted (-7)
[helios-wsi] counters (vkDestroyInstance):
[helios-wsi]   physdev_refused_no_external_image_import       1
```

`vulkaninfo --summary` lists `VK_LAYER_HELIOS_present` and completes; a native
Vulkan app that asks for `VK_KHR_swapchain` gets
`VK_ERROR_EXTENSION_NOT_PRESENT` out of `vkCreateDevice`. The layer's own
`vkNegotiateLoaderLayerInterfaceVersion` was exercised separately by the
installer: `VkResult=0 version=2 gipa=ok gdpa=ok gpdpa=ok`.

### The capability matrix, measured (`tools/vk_external_handle_probe.cpp`)

For the layer's exact C37 tuple — `B8G8R8A8_UNORM` / `2D` / `OPTIMAL` /
`COLOR_ATTACHMENT|TRANSFER_SRC|TRANSFER_DST`, straight at the ICD with no
layer in the chain:

| handle type | `vkGetPhysicalDeviceImageFormatProperties2` | features |
|---|---|---|
| `D3D12_RESOURCE` | `VK_ERROR_FORMAT_NOT_SUPPORTED` | — |
| `D3D12_HEAP` | `VK_ERROR_FORMAT_NOT_SUPPORTED` | — |
| `D3D11_TEXTURE` | `VK_ERROR_FORMAT_NOT_SUPPORTED` | — |
| `D3D11_TEXTURE_KMT` | `VK_ERROR_FORMAT_NOT_SUPPORTED` | — |
| `OPAQUE_WIN32` | `VK_SUCCESS` | `EXPORTABLE IMPORTABLE`, compatible=0x2 |
| `OPAQUE_WIN32_KMT` | `VK_ERROR_FORMAT_NOT_SUPPORTED` | — |
| **(none) — control** | `VK_SUCCESS` | (the tuple itself is fine) |

External semaphore, timeline: `D3D12_FENCE` reports **no** features and
`compatibleHandleTypes=0`; `OPAQUE_WIN32` is `EXPORTABLE IMPORTABLE`.

The control row is what makes this attributable: the format/usage tuple is
supported, so the refusal is about the external handle type and nothing else.
`VK_KHR_external_memory_win32` and `VK_KHR_external_semaphore_win32` ARE
advertised — the layer's extension-presence gate passes and the *capability*
gate is what fails, which is why the extension check alone was not enough to
see this.

### What this costs the mesa lane

§10.3:1121-1188 is normative and specific: each swapchain image is a shareable
committed D3D12 texture imported with
`VK_EXTERNAL_MEMORY_HANDLE_TYPE_D3D12_RESOURCE_BIT` + a dedicated allocation,
and Ready/Release are D3D12 fences imported with `D3D12_FENCE_BIT`. Neither
exists in the ICD today: `grep -r D3D12_RESOURCE icd/mesa/src/virtio` is empty,
and venus' Windows external-memory path is `OPAQUE_WIN32` only
(`vn_device_memory.c`). `OPAQUE_WIN32` is not a substitute — the spec confines
it to payloads a Vulkan implementation exported, and these are created by D3D12.
And even for `OPAQUE_WIN32` the ICD does **not** report `DEDICATED_ONLY`, which
§10.3 requires of the admitted type.

⇒ The layer is complete enough to be blocked on someone else. The next mesa
unit is the lower-ICD import chain (§10.3's C57 carrier:
`D3DKMTQueryResourceInfoFromNtHandle` → `D3DKMTOpenResourceFromNtHandle`),
plus advertising `D3D12_RESOURCE_BIT` as `IMPORTABLE|DEDICATED_ONLY` and
`D3D12_FENCE_BIT` as importable. Until then no native Vulkan app can create a
device with `VK_KHR_swapchain` through the layer — loudly, which is correct.

**Bound.** One physical device, one format tuple, `OPTIMAL` tiling only. The
probe did not attempt an actual import, so it establishes what the ICD
*reports*, not that a report of `IMPORTABLE` would work. Everything above the
device-creation gate — surfaces, swapchains, acquire, present, teardown, the
D3D12/DXGI half — remains unexecuted.

### ⛔ The trap that hid this: the Vulkan loader ignores its environment in an elevated process

`win_exec`/SSH lands **elevated** (`IsInRole(Administrator) = True`). The
loader's `loader_secure_getenv` drops `VK_LAYER_PATH`,
`VK_ADD_IMPLICIT_LAYER_PATH`, `VK_INSTANCE_LAYERS`, `VK_LOADER_LAYERS_ENABLE`
and `VK_DRIVER_FILES` for such a process, **silently** — with
`VK_LOADER_DEBUG=layer` it prints every registry directory it searches and
never mentions the env-var path at all. A staged layer looks exactly like a
layer that failed to build. `tools/run-helios-layer-app.ps1` exists for this:
it runs the app from a scheduled task as the interactive user at RunLevel
Limited, which is unelevated, and which also puts the app in session 1 where a
WSI layer can have a window.

⚠ The same trap is live in `tools/install-helios-icd.ps1`, whose smoke test
sets `VK_DRIVER_FILES` before running `vulkaninfo`. That has always been inert;
it passes because the ICD is also registered in HKLM.

### Why the layer is NOT registered machine-wide

An implicit layer loads into **every** Vulkan instance, which on this box
includes `dxvk-helios` under dwm — and §2 item 8 makes "translators never enter
this layer" an acyclicity rule. The manifest's `disable_environment` cannot
carve dwm back out, because of the elevation rule above. So
`install-helios-present-layer.ps1` stages by default and writes the
`ImplicitLayers` value only under an explicit `-Register Implicit`.

⇒ **This answers the `OWNERSHIP.md` §1 cross-lane request against
`packaging/windows/Install-Helios.ps1` with a "not yet, and here is why".** The
bundle must not register this layer while the layer refuses every device: it
would gain nothing and put unfinished code in the compositor's path.
`ci/windows/build-mesa.sh` does not build it either (`-Dvulkan-layers=`), so
the value would name a file the payload does not contain.
