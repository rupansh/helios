# Retirement target-gate findings

Measurements taken against the real target that **revise claims in the frozen
reference** (`docs/HELIOS_PRESENT_SYNC_RETIREMENT.md`). The reference is frozen
as of corrective pass 82 and is not rewritten in place; where a measurement
contradicts it, the measurement wins and is recorded here.

Each entry states what was measured, on what, and — importantly — the **bound**
on the claim, so a later reader cannot inflate it.

---

## F1 — Core DDI 0116 negotiates on build 26100. The "build 28000" package minimum is a header artifact.

**Date** 2026-08-10 · **Revises** §2 (lines 124-125, 319-327), §3 (line 391),
§10.2 (line 992), §10.9 (line 2853), §18.1 (line 4649)

⛔ **Anchors re-derived 2026-08-10 (round-3 repair).** Every number above was 10
too low — commit `c17f17c` inserted a ten-line banner at the frozen doc's
`:11-20` and shifted every body line after 8. They were re-derived by grepping
the cited text, not by adding 10: `:124-125` is the "**Decision: retire HPS2
only with an atomic Windows 11 26H1/build 28000**" sentence, `:391` is "No
version or feature fallback. Windows build 28000+", `:992` is the `OS/runtime`
admission row, `:2853` is §10.9's `Core DDI <0116` row, and `:4649` is "WDK
28000 bindings expose Core 0116 exactly".

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
§10.9 line 2868 and the preamble sentence at line 27 of their assumed failure
mode

⛔ **Anchors re-derived 2026-08-10 (round-3 repair)**, same +10 banner shift as
F1, verified by grepping the text rather than by arithmetic: `:2868` is §10.9's
`HLM1 descriptor/BAR/linear-offset/Lock2/WC/coherency admission failure` row
(was cited `2858`), and `:27` is "HLM1's cold two-segment admission, and the
other section-18 target gates pass" (was cited "§2 line 17" — ⚠ **and the
section label was wrong even before the shift**: line 27 is in the document
*preamble*, above §1 at `:84`, not in §2, which starts at `:122`).

The kmd-core lane brief called this "the single highest-risk item in the lane":
§10.7:1983-1986 requires HLM1 to be `Aperture=0, CpuVisible=1,
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

### F7 addendum — the image half is implemented, and the sizing above was wrong

`33d3a10678b` (icd/mesa) makes the ICD import `D3D12_RESOURCE_BIT`. The
measured matrix is now:

```
  D3D12_RESOURCE  VK_SUCCESS  DEDICATED_ONLY IMPORTABLE  compatible=0x40 export=0x0
  OPAQUE_WIN32    VK_SUCCESS  EXPORTABLE IMPORTABLE      compatible=0x2  export=0x2
```

and the layer's refusal moves to the next gate in §10.3's chain,
`D3D12_FENCE_BIT not IMPORTABLE`.

**What the paragraph above got wrong.** It named "the lower-ICD import chain
(§10.3's C57 carrier: `D3DKMTQueryResourceInfoFromNtHandle` →
`D3DKMTOpenResourceFromNtHandle`)" as the next unit, which read as work to be
written. **That carrier already existed** — it is what the `OPAQUE_WIN32`
import has always used. What was missing was only the handle type's
advertisement and routing, because on this stack the two types name the same
object: `helios_umd12` sets `VKD3D_HEAP_FLAG_HELIOS_VENUS_EXPORT` on **every**
committed create (`resource12.rs:2317`, unconditional on the fused
heap+resource arm), so a committed D3D12 resource is already backed by
venus-exported dedicated memory carrying the identity blob the carrier reads.

⚠ A wrong intermediate conclusion, recorded because it nearly cost the session
an XL rewrite: reading `heap_flags(..., PrimaryTranslation::VenusExport)` alone
suggested the export flag was tied to `HEAP_PRIMARY`, i.e. scanout only, from
which it followed that a *shared* committed resource would have no venus
identity and the import would need HWA2 and the KMD allocation model (K4, 0%).
The call site two hundred lines away says the opposite in a comment that
explains why. **A flag's meaning is where it is set, not where it is
translated.**

### The semaphore half, and what it needs

`D3D12_FENCE_BIT` is the same shape of problem with one extra consumer.
vkd3d-proton does not merely accept it — it *requires* it: `command.c:626-644`
creates shared fences with `export_info.handleTypes = D3D12_FENCE_BIT` and
refuses unless `exportFromImportedHandleTypes` reports it, and `device.c:7687`
/ `:7771` export and import under that type. So the ICD must advertise it both
EXPORTABLE and IMPORTABLE, and ⇒ **`ID3D12Fence::CreateSharedHandle` cannot
work on Helios today either**, independently of the present layer.

The underlying object is again the same: a venus timeline semaphore exported
through the Helios WDDM sync path. The work is `vn_physical_device.c`
(advertise), `vn_queue.c` (stop stripping `D3D12_FENCE_BIT`, accept it in the
named export/import arms), and it must be recorded that the two names denote
one object *on this stack* — that is a property of Helios, not of Vulkan.

### F7 addendum 2 — both §10.3 capability gates are closed; the layer creates a WSI device

Implemented and measured on 22.22.263.0, in order, each gate closed by the
refusal the previous one produced:

| gate | closed by |
|---|---|
| `D3D12_RESOURCE_BIT` not supported | `33d3a10678b` — advertise + route into the existing C57 carrier |
| `D3D12_FENCE_BIT` not IMPORTABLE | `becb64a8c70` — same, for the WDDM monitored fence |
| the layer asked about a BINARY semaphore | `26f0930bd4f` — chain `VkSemaphoreTypeCreateInfo{TIMELINE}` |
| `create_device_refused_missing_device_proc` (unnamed) | `26f0930bd4f` — pair each proc with its name |
| `vkQueueSubmit2` NULL on a 1.4 device | the app declared apiVersion 1.1; the loader filters core functions above it |

The layer now reports:

```
[helios-wsi] physical device ... admitted: canonical family 0, q=16
[helios-wsi] device ... created (wsi=1 canonical=0 private=1)
[helios-wsi] REFUSE swapchain_refused_tag_call_absent (-3):
             vkSetHeliosPresentableImageHELIOS is not provided by the lower chain
```

**The next unit is named by the layer itself**: `vkSetHeliosPresentableImage-
HELIOS`, the private device-scoped tag call of §10.7:2571-2578 (lane-mesa
ambiguity A4) that records a `VkImage` as a swapchain slot's presentable image
and thereby legalises `VK_IMAGE_LAYOUT_PRESENT_SRC_KHR` for it. The layer
resolves it only through the captured next-layer `vkGetDeviceProcAddr` and
refuses `vkCreateSwapchainKHR` outright when it is absent, which is what it is
doing now.

⚠ **`ID3D12Fence::CreateSharedHandle` was broken on Helios independently of any
of this**, and nothing had noticed. vkd3d refuses to create a shared fence
unless the driver reports `D3D12_FENCE_BIT` in
`exportFromImportedHandleTypes`; it did not. That is a D3D12 correctness fix
that happens to fall out of the WSI lane.

**Bound.** Everything above `vkCreateSwapchainKHR` is still unexecuted: no
image has been imported, no fence shared, no frame presented. The capability
queries are answered and the device is built; the import chain itself has run
zero times. `helios_paintcap` shows a fully composited live desktop after both
ICD installs, so the DXVK/dwm path is undisturbed.

### F7 addendum 3 — the tag call is implemented; four D3D12 textures import; the fence stops it

`vkSetHeliosPresentableImageHELIOS` now exists in the ICD, declared once in
`icd/mesa/src/vulkan/helios_private_wsi.h` and included by both the layer and
the ICD (`OWNERSHIP.md` §4's rule applied to a second private ABI). It is
published **only** through `vn_GetDeviceProcAddr` — it is not in `vk.xml` so the
generated table cannot carry it, and it is deliberately not a DLL export
because the layer's contract resolves it exclusively through the next-layer
`vkGetDeviceProcAddr`.

It is a gate, not a rubber stamp: it refuses an image not created with
`D3D12_RESOURCE_BIT` (checked against `vk_image::external_handle_types`, which
the ICD already records), and refuses a re-tag naming a *different* slot while
staying idempotent for the same one.

**With it in place the swapchain build gets much further, and the import chain
actually runs.** The ICD diag shows four D3D12 committed textures imported into
Vulkan:

```
memory_transfer_resource_ownership mem=... res=447 ctx=59
memory_transfer_resource_ownership mem=... res=449 ctx=59
memory_transfer_resource_ownership mem=... res=451 ctx=59
memory_transfer_resource_ownership mem=... res=453 ctx=59
```

⇒ D3D12 device creation, the DXGI flip swapchain, `CreateSharedHandle` on the
resources, `vkGetMemoryWin32HandlePropertiesKHR`, the dedicated import, the
bind and the tag all succeed. §10.3's **image** chain works end to end.

### ⛔ Open: the Ready/Release fence import fails, and the fence may not be ours

```
[helios-wsi] REFUSE swapchain_refused_semaphore_import (-3)
sync_open_nt failed nt2_status=0xc000000d legacy_status=0xc000000d
             handle=00000000000003a8 dev=0x40000600
```

Both `D3DKMTOpenSyncObjectFromNtHandle2` and the legacy open reject the handle
the layer got from `ID3D12Fence::CreateSharedHandle` with
`STATUS_INVALID_PARAMETER`.

**What is proven:** the ICD created **no** WDDM sync object in that process
during that run — there is no `sync_create` line for the run's pid, and the
older ones in the log belong to a recycled pid. So the handle was not produced
by this ICD's export path, and `vkGetSemaphoreWin32HandleKHR` was never called.

**What is not yet proven** — the hypothesis to test next, not a conclusion:
`ID3D12Fence` may not be a vkd3d object at all under the UMD arm. In the real
D3D12 architecture the runtime owns fences over dxgkrnl monitored fences and
the user-mode driver has no fence DDI, in which case the shared handle is a
**dxgkrnl** sync object of a different class from the ICD's own, and vkd3d's
`d3d12_shared_fence` path (`libs/vkd3d/command.c:612-656`,
`d3dkmt.c:55-78`) only applies to the app-local vkd3d arm where vkd3d *is* the
whole D3D12 implementation. vkd3d's own capability query is not the problem: it
correctly chains `VkSemaphoreTypeCreateInfo{TIMELINE}` and would now pass.

Decide it by measurement — whether a `D3D12_FENCE_FLAG_SHARED` fence's shared
handle on the Helios adapter is openable by `D3DKMTOpenSyncObjectFromNtHandle2`
at all, and which component created it — before changing either repository.

**Bound.** No frame has been presented. Acquire, present, the copy and teardown
remain unexecuted. `helios_paintcap` shows a fully composited live desktop after
every install in this sequence.

### The diagnostic that made this readable

`helios_wddm_sync_open_nt` reported only the *fallback* open's status, so a
failure in the informative Nt2 call was reported as the legacy call's
`0xc000000d`. It now reports both. That is the third time in this session that
a refusal naming nothing cost a round trip — the other two were the
external-image query's `VkResult` and the required-device-proc check.

---

## F8 — An `ID3D12Fence` shared handle cannot be opened through the D3DKMT sync path *on any adapter*. The failure is not ours, and the sharing direction is backwards.

`F7 addendum 3` left the Ready/Release fence import open with a hypothesis and
an instruction to measure before touching either repository. Measured, on
22.22.263.0, by `tools/d3d12_shared_fence_probe.cpp`, with three controls.

### What was measured

Per adapter: an `ID3D12Fence` created with `D3D12_FENCE_FLAG_SHARED`, its
`CreateSharedHandle` NT handle put through the kernel's own object-type query
and then through every D3DKMT open the ICD could plausibly use — against a
device on the *same* adapter, against devices on *other* adapters, with
`Flags=0` and with `NtSecuritySharing`, plus the legacy open, the native-fence
open, and the resource-info query. Alongside it, three controls that decide
what a failure means.

```
                                      Helios     MBRD       MBRD/WARP
  NT object type of the handle        DxgkSharedSyncObject  (all three, granted 0x001F0003)
  ID3D12Device::OpenSharedHandle      OK         OK         OK
  D3DKMTOpenSyncObjectFromNtHandle2   0xC000000D 0xC000000D 0xC000000D   (both flag settings,
                                                                          same + other adapters)
  D3DKMTOpenSyncObjectFromNtHandle    0xC000000D 0xC000000D 0xC000000D
  D3DKMTOpenNativeFenceFromNtHandle   0xC000000D 0xC000000D 0xC000000D
  D3DKMTQueryResourceInfoFromNtHandle 0xC000000D 0xC000000D 0xC000000D

  CONTROL our own monitored fence, D3DKMTShareObjects -> ...FromNtHandle2
                                      SUCCESS    SUCCESS    SUCCESS      (same device, 2nd device,
                                                                          both flag settings)
  CONTROL ID3D12Device::OpenSharedHandle(our KMT fence) -> ID3D12Fence
                                      OK         OK         OK
```

### What follows, and what does not

**It is not a Helios defect.** The identical refusal appears on both Microsoft
Basic Render Driver adapters, one of them WARP. Nothing in `icd/mesa`, the KMD
or the layer caused it, and no change to any of them can fix it. ⇒ **Do not
change either repository for this**, which was the instruction F7 addendum 3
left.

**The instrument is sound.** The control fence — created by
`D3DKMTCreateSynchronizationObject2(MONITORED, Shared, NtSecuritySharing)` and
shared by `D3DKMTShareObjects` — opens with `STATUS_SUCCESS` through the exact
same call, in the same process, from two different devices, on all three
adapters. So the refusal is a statement about the object, not about the call,
the process, the device or the adapter.

**The handle is valid.** `ID3D12Device::OpenSharedHandle` round-trips it back
into an `ID3D12Fence` every time. A D3D12 fence handle is a real
`DxgkSharedSyncObject` of the same NT type and granted access as ours — the
type name is not the discriminator, which is why reading it was necessary
before assuming a class difference.

⇒ **`F7 addendum 3`'s hypothesis is confirmed in its consequence, not in its
mechanism.** vkd3d's `d3d12_shared_fence` path (`libs/vkd3d/command.c:612-656`,
`d3dkmt.c:55-78`) can only work where vkd3d itself created the fence — the
app-local arm. Under the UMD arm the fence belongs to the D3D12 runtime and no
user-mode component can open it through a public D3DKMT entry point. *Why*
dxgkrnl refuses is still unmeasured and this finding does not claim it.

### ⭐ The actionable half: the direction is backwards

The last control is the one that changes the work. `ID3D12Device::OpenSharedHandle`
**accepts a plain D3DKMT monitored fence** that we created and shared —
returning a working `ID3D12Fence` — **and it does so on Helios.**

So the two sides can share a fence; only one direction works:

* D3D12 → Vulkan (import an `ID3D12Fence`): impossible, everywhere.
* Vulkan/KMT → D3D12 (the ICD creates and exports, D3D12 opens): **works.**

⇒ §10.3's Ready/Release fences must be **created by the ICD and opened by the
D3D12 side**, not imported from `ID3D12Fence`. This costs no new ICD
capability: `helios_wddm_sync_create` + `helios_wddm_sync_share_nt`
(`vn_renderer_helios.c:~900-1075`) already build exactly the object the control
arm used. It is a change to the *layer's* protocol, and it belongs to
`lane-mesa`.

### Two rows that measured nothing, recorded so nobody re-runs them

* `D3DKMTOpenNativeFenceFromNtHandle` refuses the control fence too, and no
  adapter on this box advertises native fences. The row therefore does **not**
  show that the native-fence path is or is not the right one for an
  `ID3D12Fence`; it shows only that it is unavailable today. Re-run it after
  K7 + the `SURFACE` flip if the question still matters.
* `D3DKMTShareObjects` with two sync objects in one NT handle is itself
  refused (`0xC000000D`), so the "the runtime shared a composite handle"
  explanation could not be tested this way. It is neither supported nor
  eliminated.

**Bound.** This says what cannot be opened and which direction can. It does not
say the redirected design works: no fence has been signalled or waited across
the boundary, no acquire, no present, no frame. `ID3D12Fence::CreateSharedHandle`
itself succeeds — the D3D12 correctness fix recorded in F7 addendum 2 stands.

---

## F9 — RESOLVED AS A DORMANT SOURCE TRANCHE (2026-08-13). K7 had four DDI slots, not a complete surface.

Measured incidentally while taking a pre-change `cargo check` baseline of
`kmd_render` on the VM (2026-08-10). The build is green; the finding is in its
22 warnings, which nothing had read.

```
win_cargo kmd_render check --message-format=short   -> exit 0, 22 warnings, of which:
  native_fence.rs:316  constant `FEATURE_DECLINED` is never used
  native_fence.rs:326  constant `DXGK_FEATURE_SUPPORT_STABLE_VALUE` is never used
  native_fence.rs:391  function `ensure_feature_admitted` is never used
  native_fence.rs:421  function `query_feature_support` is never used
  native_fence.rs:1215 function `fill_native_fence_caps` is never used
  native_fence.rs:1250 struct `NotifyCtx` is never constructed
  native_fence.rs:1263 function `notify_routine` is never used
  native_fence.rs:1303 function `signal_native_fence_signaled` is never used
  native_fence.rs:1355 function `has_live_fences` is never used
```

That is **nine symbols, not eight** — six functions (`ensure_feature_admitted`,
`query_feature_support`, `fill_native_fence_caps`, `notify_routine`,
`signal_native_fence_signaled`, `has_live_fences`), two constants
(`FEATURE_DECLINED`, `DXGK_FEATURE_SUPPORT_STABLE_VALUE`) and one struct
(`NotifyCtx`). The heading said "eight functions" while the block listed nine
non-functions-included symbols; corrected in place, since the nine-symbol list
is what the rest of this finding argues from.

Each has **zero references outside `native_fence.rs`** (`grep -rn '\b<name>\b'
kmd_render/src/ | grep -v native_fence.rs` → 0 for all nine, re-verified after
the K4 changeset), and rustc's "never used" means no reachable use inside it
either.

### What is actually wired

| K7 obligation (`lane-kmd-core.md` §3) | State |
|---|---|
| `DxgkDdiCreateNativeFence` / `Destroy` / `Open` / `Close` | **registered** — `lib.rs:282-285` |
| `DxgkDdiSetNativeFenceLogBuffer`, `UpdateNativeFenceLogs` | not registered — classified Disabled, consistent with F6 |
| `DXGKQAITYPE_NATIVE_FENCE_CAPS` (=37) arm | ✅ **dormant and wired** — exact size/alignment validation precedes a local zeroed result; padding, mapping, range, and reserved bytes are fixed |
| `DXGK_FEATURE_NATIVE_FENCE` enablement | ✅ **dormant and wired** — the exact PASSIVE callback is cached per adapter/StartDevice generation and every absence, failure, unstable answer, or decline fails closed |
| `DXGK_VIDSCHCAPS::NativeGpuFence=1`, `No64BitAtomics=0` | ✅ **dormant and conjunctive** — `NativeGpuFence` is conditional; `No64BitAtomics` and optimized interrupts stay zero |
| `DXGK_INTERRUPT_NATIVE_FENCE_SIGNALED` (=19) reporting | ✅ **dormant and wired** — a completed WDDM submission plus a current monitored population is required, and the empty-rescan packet uses the shared audited DIRQL notifier |

### Dormant K7 resolution

K7 now owns the narrow query/capability, interrupt, and lifecycle seams that the
old numerical K8/K9/K10 decomposition had left unwired. That does **not** start
those broader units: their physical-memory caps, ordered-engine completion, and
full adapter teardown work remain separate.

Authority is now one ref-counted `NativeFenceAdapterState` per adapter. It
caches the exact StartDevice LUID and feature answer for that generation and
owns the lifecycle, nonwrapping epoch/object generations, and bounded live and
monitored populations. Driver handles still directly reference bounded objects;
there is no registry, name, scan, raw ID, Escape, or ticket. HNF1 input requires
generation zero and successful output assigns a nonzero generation. Update DDIs
bound all counts and validate the complete batch before any storage write.

`NF-UAF-1` is resolved without inventing serialization: Microsoft's native-fence
reference contract says the global handle remains while any process-local
reference exists, and the final process teardown calls Close before Destroy.
The renderer additionally refuses and retains the bounded global object if an
unexpected Destroy arrives with local references, rather than freeing named
storage. See [Microsoft's global/local handle lifetime](https://learn.microsoft.com/en-us/windows-hardware/drivers/display/native-gpu-fence-objects#global-and-local-handles-for-shared-fences).

The one bounded ordinary review repaired three concrete issues before the
checkpoint: Open's HNF1 direction is input-zero/output-nonzero; OS-owned value
storage is written before diagnostic/population mirrors; and the K7 gate now
rejects undeclared global authority plus explicit `No64BitAtomics` and hardware-
queue mutations.

**Verification bound.** Windows `cargo check` is green at 13 warnings: exactly
the nine warnings listed above disappeared, with no new warning category. All
511 `kmd_logic` unit tests pass. The normal retirement suite includes a K7 gate
whose 35 mutations each alter a temporary source tree and invoke the real gate;
the existing D4 and D5 proofs remain green. The generated callback audit remains
89 Implemented / 91 Disabled / 4 Pending / 8 Retiring. The VM-generated and
offline WDK-28000 bindings remain 3,834,340 bytes with SHA-256
`148b75db41e093dc6783be4f5bb3ea84b2c7c39ef316fe711b3f2a5a0668bea2`.

`SURFACE=Wddm2_1GpuMmu` and `KMD_D2_OWNER_ENABLED=false` remain unchanged.
Therefore all native-fence feature/cap/interrupt authority remains unreachable
in production. This resolves F9 only as a dormant source-composition finding;
it is not an activation, deployment, runtime-correctness, or WDDM-3.2-flip
claim.

---

## F10 — ⛔ FALSIFIED BY F11 (2026-08-11). ~~dxgkrnl DOES propagate the KMD's create-time private-data write back to the creating UMD. HWA2's write-back model is sound.~~

⛔⛔ **READ F11 FIRST. This finding's conclusion is WRONG and its title must not
be cited.** dxgkrnl discards a KMD write into
`DXGK_ALLOCATIONINFO::pPrivateDriverData` for any user-supplied buffer; the
channel that works is the write at `DxgkDdiOpenAllocation`, which the
pre-retirement KMD *also* performed. The instrument below was sound and the
inference from it was under-determined — the same 48-byte record was written at
BOTH sites, so a pre/post diff across `pfnAllocateCb` cannot say which write the
bytes came from. The rest of this entry is kept unedited, because the
correction it already carries ("a pre/post byte comparison over a reinterpreted
buffer proves *that* bytes changed and can never, on its own, say *which field*
changed") is one step short of the one that mattered: it also cannot say **which
write site**.

⚠ The lesson is not "the instrument was bad". It is that a measurement over a
system with two writers pinned neither, and nobody asked how many writers there
were. F11's probe answers it by removing one writer at a time.

The D3D12 producer named this the single highest-risk assumption in the K4
changeset: *"dxgkrnl propagates the KMD's create-time private-data write back to
the UMD's buffer … not established anywhere in the doc set. If the assumption is
false, every committed D3D12 resource create fails."* HWA2's whole two-stage
contract rests on it — the KMD stamps `allocation_generation` at create and the
UMD must be able to read it back.

**It is established now, on the live 22.22.263.0 build, by a pre/post comparison
the D3D11 UMD has been logging all along** (`umd/src/forward/resource.rs`, the
`private mutated` line, which brackets `pfnAllocateCb` with a copy of the buffer
taken before the call):

```
DDI allocate_wddm_resource private mutated:
  pre  blob=0x1a  res_id=508 ctx=71 kind=1 vas=512     mti=1
  ->
  post blob=0x200 res_id=0   ctx=71 kind=0 vas=512     mti=1

  pre  blob=0x2c  res_id=511 ctx=71 kind=1 vas=4988928 mti=1
  -> post blob=0x4c2000 res_id=0 ctx=71 kind=0 vas=4988928 mti=1
```

Bytes differ after the call returns. The kernel wrote them; nothing in user mode
did. ⇒ **the create-time `[in/out]` buffer round-trips.**

### ⛔ Correction — the field names above were a layout misread. The conclusion is unchanged and is now *stronger*.

This finding originally read: *"Three fields — `blob_id`, `adopt_resource_id`
and `kind` — differ after the call returns."* Those are the names the D3D11 UMD
printed, but they are **not** the fields the kernel wrote, and stating it that
way would send a reader looking for a KMD write to `blob_id`.

What the pre-retirement KMD actually did at create was stamp a **whole
48-byte `HeliosWddmOpenIdentity` over bytes 0..48 of the same buffer** —
`write_open_identity` (`git show 41d13f8:kmd_render/src/ddi/create_allocation.rs`,
definition `:1617`, create-time call site `:2481`), which the log then read back
through the *other* record's field names. The two retired 48-byte layouts
(`protocol/src/wddm_legacy.rs:163` and `:388`) overlay like this:

| offset | read as `HeliosWddmAllocPrivate` | actually written as `HeliosWddmOpenIdentity` |
|---|---|---|
| 0–7 | `blob_id` | `venus_alloc_size` |
| 8–15 | `size` | `blob_size` |
| 16–19 | `magic` = `0x4857444D` | `magic` = `0x4849444E` |
| 20–23 | `version` | `version` |
| 24–27 | `blob_mem` | `resource_id` |
| 28–31 | `blob_flags` | `memory_type_index` |
| 32–35 | `ctx_id` | `ctx_id` — *same offset, same value* |
| 36–39 | `map_cache` | `kind` |
| 40–43 | `kind` | `reserved[0]` (tracker share) |
| 44–47 | `adopt_resource_id` | `reserved[1]` (tracker cookie) |

⇒ The log's three "changed fields" are the overlay: post-`blob_id` is the
identity's `venus_alloc_size`, post-`kind` is `reserved[0]`, post-`adopt_resource_id`
is `reserved[1]`. `ctx=71` was unchanged in both rows because `ctx_id` sits at
the same offset in both records with the same value — not because the kernel
skipped it.

**The arithmetic proves it, and this is why the finding gets stronger rather
than weaker.** In both logged rows the post `blob=` value equals the `vas=`
value exactly — `0x200` = 512 = `vas=512`, and `0x4c2000` = 4988928 =
`vas=4988928`. Nothing in user mode ever wrote the venus allocation size into
the `blob_id` slot; that byte pattern can only have come from the kernel's
identity write. The write-back is not merely "three fields moved", it is a
**48-byte kernel-authored record arriving intact at a known offset**, which is
precisely the property HWA2's two-stage contract needs.

⚠ **What the misread was, since that is what this file is for:** the instrument
was right and the field names were wrong. The UMD's pre/post comparison reads
the buffer through the struct it *sent*, and the kernel replied in a *different*
struct at the same address — so every name in the log line is the sender's name
for a byte range, not the writer's. A pre/post byte comparison over a
reinterpreted buffer proves *that* bytes changed and can never, on its own, say
*which field* changed. Attributing per-field semantics to it required opening
both layouts, which the original write-up did not do.

### The instrument that could NOT have answered it, recorded so it is not re-used

`umd12`'s `AllocPrivateWrittenBack` counter looked like the right instrument and
was not. **In the pre-retirement build** it fired only when the write-back
*differed* from what the UMD sent (`private.meta.pitch != pitch ||
venus_alloc_size != … || memory_type_index != …`), so a KMD that faithfully
echoed the UMD's own values would have left it at zero, exactly like a KMD whose
write never arrived. A D3D12 clear probe run on that build produced **no `umd12`
log at all** — the log file is created lazily on the first `log_error!` — which
under that counter was equally consistent with both hypotheses. The D3D11 line
answered it because it compares a **pre-call snapshot** against the post-call
buffer rather than against an expectation.

⚠⚠ **The counter has since been RE-GRADED, and this paragraph is history — do
not read it as a description of the deployed build.** At HEAD the predicate it
quotes cannot even be evaluated: the `HeliosWddmAllocMeta` trailer is gone from
both UMDs (only tombstone comments name it), so there is no `private.meta`.
`alloc_private_written_back` is now a **success census** — in
`umd12/src/forward12/resource12.rs` it is bumped once per create whose HWA2
write-back *arrived and validated*, immediately after `validate_create_output`
succeeds, and the bump site is explicitly annotated *"Not a refusal — the census
that answers recon's open question with a number"*. Its declaration carries the
⚠⚠ re-grade banner and states the new expectation: **equal to
`IdentityRecorded`**, with `Hwa2WriteBackAbsent` as its complement. (Symbols,
not lines, on purpose — `rg -n alloc_private_written_back
umd12/src/forward12/resource12.rs` gives the declaration, the constructor, the
single bump site and the summary-set entry.) The wire name was deliberately **not** changed, because `D3D12 DDI
refusals:` lines are diffed across builds. ⇒ The lesson below still stands as a
lesson; the counter it was drawn from no longer has that shape.

⇒ Same lesson as `WfBWire` / `RENDER_COUNT` / `RING_SUBMIT_COUNT`: a counter
whose firing condition is "the value changed from what I predicted" cannot
distinguish "no write" from "the write agreed with me".

**Bound.** This proves the buffer the UMD passes to `pfnAllocateCb` is copied
back after the kernel writes it, for the D3D11 arm at 96 bytes. It does **not**
prove the same for 168 bytes (the size changes), nor for the D3D12
`pfnAllocateCb` arm, nor that any particular `PrivateDriverDataSize` is accepted.
Those remain to be measured on the deployed K4 build; both producers already
refuse loudly if the generation comes back zero, so the failure mode is a
counted refusal rather than a corrupt descriptor. ⚠ **The two counters have
different names** — the D3D12 arm's is `Hwa2WriteBackAbsent`, declared in
`umd12/src/forward12/resource12.rs` and nowhere else; the D3D11 arm's is
`hwa2_output_invalid`, declared in `umd/src/forward.rs` and listed in that
file's `DDI_REFUSAL_SET`, and it is broader — it fires on a zero generation, a
mutated echo, or a package-generation mismatch alike. Grepping the tree for
`Hwa2WriteBackAbsent` alone will miss the D3D11 half. ⛔ **Line numbers are
deliberately omitted here** (round-3 repair): the earlier `:4925` / `:475` /
`:505` cites drifted inside a single review round, and both files are edited by
other lanes. Locate them with
`rg -n -e Hwa2WriteBackAbsent -e hwa2_output_invalid umd/src umd12/src`.

Incidental, from the same run: `tools/d3d12_clear_probe.cpp` **passes** on the
deployed build — 65536/65536 pixels exactly `(0,51,102,255)`, `SetEventOn-
Completion` signalled in 0.6 µs, `GetDeviceRemovedReason` clean. That is the
pre-change D3D12 control arm for the K4 changeset.

---

## F11 — dxgkrnl DISCARDS the KMD's create-time private-data write. `DxgkDdiOpenAllocation` is the only channel, and K4 had closed it.

**Measured 2026-08-11** with `tools/hwa2_writeback_probe.c`, which issues the
same record through four create-call shapes from one process so that record
type, size, adapter and driver image are all held fixed and only the call shape
moves. On **22.22.266.0** (K4+K5, before the repair):

| variant | record | shape | `allocation_generation` / `object_generation` back |
|---|---|---|---|
| A | HWA2 168 B | bare, `D3DKMTCreateAllocation2` | **0** |
| B | HWA2 168 B | `Flags.CreateResource`, `…2` | **0** |
| C | HWA2 168 B | `CreateResource` + resource-level private data, `…2` | **0** |
| D | HWA2 168 B | bare, `D3DKMTCreateAllocation` (v1 struct) | **0** |
| E | HWA2 168 B | `CreateResource` + resource-level private data, v1 | **0** |
| F | HVM1 64 B | bare, `…2` (mesa A1's exact shape) | **0** |
| G | HVM1 64 B | `CreateResource` | refused, `AcShape` (§10.7's bare-shape rule) |
| H | HVM1 64 B | bare, v1 | **0** |

Seven admitted creates, seven zeros. The **call shape is not the variable** —
resource association, resource-level private data, the thunk version and the
record type all move without moving the result.

**And the same bytes arrive unstamped at the open.** With the diag ring cleared
and the driver image reloaded, the probe's run recorded 7 HWA2 opens
(`0x0C21_00A8`) of which **5 were rejected** by `read_open_descriptor`
(`0x0C02_00E6`) — exactly the five the probe created. The two that validated are
the OS **standard allocations this driver authors itself** in
`DxgkDdiGetStandardAllocationDriverData` (`0x0C02_0012` / `0x0C02_0022` in the
same ring: types 1 and 2), whose buffer dxgkrnl owns rather than copies from user
mode. `validate_create_output` requires a nonzero generation and
`validate_create_input` forbids the producer from supplying one, so "validated at
open" is a *proof* that the KMD's create-time write survived — and it survives
only for that one KMD-authored case.

⇒ **A KMD write into `DXGK_ALLOCATIONINFO::pPrivateDriverData` reaches nobody**:
not the caller, not `DxgkDdiOpenAllocation`, not a later opener.

### The headers said so, and this project read one field's annotation as the other's

* `d3dkmddi.h` `DXGK_ALLOCATIONINFO::pPrivateDriverData` — `// in:`
* `d3dkmddi.h` `DXGK_OPENALLOCATIONINFO::pPrivateDriverData` — `// in/out:`
* `d3dukmdt.h` `D3DDDI_ALLOCATIONINFO2::pPrivateDriverData` — `// in(out optional):`

The user-mode `out` is fulfilled by the **open**, not the create. Both header
copies are in-tree (`tmp/wdk-28000/Include/10.0.28000.0/shared/`,
`icd/win-build/wdk-include/`) and were readable throughout.

### Confirmation, on the other side of the repair

`22.22.267.0` moves the HVM1 stamp to `DxgkDdiOpenAllocation`
(`stamp_open_hvm1`). On a **cold boot**:

* `tools/hts1_session_probe.c` **15/15** (was 14/15); H2 passes.
* `OaHvm1Stamp=1`, **`TsPoolBind=1`** — K5's reply-pool binding fires for the
  first time. It had never been reachable.
* Variants F and H come back stamped (`object_generation` 4294967307 /
  4294967309, `segment_page_shift` 12, `allocation_alignment` 4096) — both
  thunks.
* Variants A–E still return 0, because only HVM1 is stamped.

**Bound.** This is measured on build 26100.8875 with this driver. It says
nothing about a Windows build that behaves differently, and it does not claim
the create-time write is *illegal* — only that it is not observable anywhere.
The KMD-authored standard-allocation exception is the one case where a
create-time write does survive, and it is not a channel a UMD can use.

### ⛔ The open consequence: HWA2 has the same defect — but it is LATENT, and the first version of this section over-claimed

HWA2's create-output is discarded exactly as HVM1's was (variants A-E above), so
the retirement's *"written once at create, dxgkrnl carries the identical bytes to
`OpenResource`"* premise is false for **every** UMD-supplied allocation, not only
for HVM1. That much is measured.

⛔ **What is NOT true — and this entry asserted it for one commit — is that the
defect is what stops the D3D11 UMD today.** Measured 2026-08-11 on 22.22.267.0
with the HWA2-producing UMD hot-installed (`win_install_umd`, then reverted):
across **9 processes** that loaded it,

```
hwa2_output_invalid=0   hwa2_create_venus_backing_needs_mesa_a3=1   (7 of 9)
```

and **no `allocate_wddm_resource` line with `info=168` was logged at all**. The
UMD refuses *earlier*, at `umd/src/forward/resource.rs:247`, whose own comment
says so: *"Until A3 lands, every shared / keyed-mutex / present / primary D3D11
texture create fails here."* No create reaches `pfnAllocateCb`, so nothing ever
reads a write-back.

⇒ The `dwmcore.dll` `0xc00001ad` crash-loop is **the A3 gap**, which is what
`ROADMAP.md` and `K4-CONTRACT.md` §5 said before this finding claimed otherwise.
The HWA2 write-back defect sits *behind* that refusal: repairing it would not
move the desktop, and it becomes reachable only when A3 lands. That lowers its
urgency and it does not lower its reality.

⚠ **The method note, since it is the second time in one session:** the HVM1
measurement made an HWA2 story feel obvious, and the obvious story was wrong. A
mechanism that is real is not thereby the mechanism you are looking at. The
instrument that settled it — reading the UMD's own refusal counters under the
UMD in question — cost one reversible install.

Repairing it means the same open-time stamp for HWA2 — the write
`K4-CONTRACT.md` §8.5 still forbids, and unlike HVM1 an HWA2 *can* have several
openers that could disagree. Options, none taken here: stamp at open and mint
per-open (identity becomes per-(device, allocation), which is the scope Render
and Patch resolve in anyway); have the producer supply the generation and the
KMD record it (the guest→kernel direction is the one channel that provably
carries, since the create-input bytes reach the open intact); or return
identities through the HNR2 reply channel. It is an owner decision.

---

## F12 — Every `DxgkDdiRender` on this driver copies a user-mode buffer with no probe and no exception frame. K6 fixed its own arm and deliberately left the other three.

**Read 2026-08-11 while writing K6's Render arm; not measured, because the
failure it describes is a bugcheck and provoking it is not a free experiment.**

`DXGKARG_RENDER::pCommand` is the context's command buffer. It is mapped, and
**writable**, in the submitting process for the whole call — that is the whole
point of it: the UMD writes commands there and passes `CommandOffset`. Three
DDIs read it with a bare `copy_nonoverlapping`:

| site | buffer | guard |
|---|---|---|
| `submit_command.rs` `dxgkddi_render` tail | `pCommand` → `pDmaBuffer` | none |
| `dxgkddi_render`'s `HeliosPresentRefreshCmd` / `HeliosPresentRenderCmd` decodes | `pCommand` → a local | none |
| `dxgkddi_render_km` | `pCommand` → `pDmaBuffer` | none |
| `dxgkddi_render_gdi` | `pCommand` → `pDmaBuffer` | none |

Failure scenario, and it needs no malice: a process issues `D3DKMTRender`, and
a second thread in that process unmaps or shrinks the command-buffer range
before `DxgkDdiRender` reaches the copy. The read raises, the raise unwinds out
of a `panic = abort` no_std DDI, and the machine bugchecks. `seh_shim.c` exists
in this driver for the *same class* of trap on a different pointer, and its
header comment names the reachable-from-any-process property as the reason.

⚠ **This is not new with K6 and K6 did not fix it.** `src/render_user_copy.c`
guards the HNR2 arm only. The other three are on the path DWM composites the
entire desktop through, and swapping their copy for a probed one is a change to
the hottest code in the driver whose failure mode — `ProbeForRead` refusing a
legitimate buffer — is a dead desktop. Doing it as part of K6 would have been an
unforced risk on an unrelated unit.

**What would settle it cheaply.** `render_user_copy.c` returns distinct codes
for a probe raise and a copy raise. Route ONE of the three legacy sites through
it behind a registry knob, default off, and read `Nr2ProbeFlt` on a desktop +
Fire Strike run: a nonzero probe-fault count means `pCommand` is not a plain
user mapping on that path and the conversion is unsafe as written; zero across a
real workload is the evidence needed to convert all three. That experiment is
not K6's and is not scheduled.

⛔ **Do not "fix" this by adding `ProbeForRead` to the legacy sites without that
measurement.** The probe raises on a *kernel* address as readily as on an
unmapped one, so if dxgkrnl ever hands those paths a kernel alias the probe
converts a working driver into one that refuses every Render.

---

## F13 — `DxgkDdiRender` must advance `pDmaBufferPrivateData`, or dxgkrnl reports an EMPTY private-data submission window. No Helios code has ever advanced it.

**Measured 2026-08-11 on KMD 22.22.270.0 → 22.22.271.0**, both arms, with
`tools/hnr2_native_probe.c` as the workload.

K6's `DxgkDdiSubmitCommand` arm read its 64-byte `Hnr2KmdDmaPrivateV1` out of
`pDmaBufferPrivateData + DmaBufferPrivateDataSubmissionStartOffset`, bounded by
`…EndOffset` — the shape `decode_legacy_present_fence` already uses. It refused
every record. The staging pool showed it as `Nr2Slot = 3` checkouts against
`Nr2SlotRet = 0` retirements with `Nr2SlotUnd = 0`, i.e. `retire()` was never
even called.

Rather than guess between "the window is empty", "the total is short" and "the
offsets are elsewhere", 22.22.270.0 reported the numbers:

```
DmaBufferPrivateDataSize                    = 64      ← the buffer is there
DmaBufferPrivateDataSubmissionStartOffset   = 0
DmaBufferPrivateDataSubmissionEndOffset     = 0       ← and describes nothing
```

`publish_dma_record` had already SUCCEEDED at Render (it returns before
`Nr2Commit`, which read 3), so the buffer existed and was exactly one record
long. It is the **window** that was empty.

### The experiment, and the answer

22.22.271.0 advances `args.pDmaBufferPrivateData` past the record at the end of
`DxgkDdiRender` — the way `pDmaBuffer` has always been advanced — on the
hypothesis that the field is `in/out` and that the advance is how a driver tells
dxgkrnl how much private data it produced. The same build kept an offset-0
fallback behind `Nr2WinFB`, gated on `DmaBufferPrivateDataSize == 64` so it can
only fire for a buffer holding exactly one record (which cannot be two batched
Renders). One run separates the hypotheses:

```
Submit window : total=64  start=0  end=64
Nr2WinFB      = 0
Nr2Slot=3  Nr2SlotRet=3  Nr2SlotUnd=0  Nr2SubNoRec=0
```

⇒ **`DXGKARG_RENDER::pDmaBufferPrivateData` is an `in/out` pointer.** The
advance populated the window, the fallback never fired, and the staging pairs
exactly. The WDK header carries no annotation on the field
(`d3dkmddi.h:136-153`), which is why this had to be measured.

### ⚠ The consequence for the LEGACY path is a hypothesis, not a measurement

**No code in this driver has ever advanced that pointer** — not
`dxgkddi_render`'s D3D12/present arms, not `render_km`, not `render_gdi`. That
is very likely why `decode_present_fence` (`submit_command.rs:838-864`) carries
an **offset-0 fallback** after trying `start..end`, and why `PmSta`/`PmEnd` have
never had anything to say. If so, every present and every D3D12 ECL boundary on
this driver has been decoded through that fallback since the path was written.

⛔ **Not acted on, and deliberately.** Making the legacy Render arms advance the
pointer would change the DDI DWM composites the entire desktop through, on a
hunch, as a side effect of an unrelated unit. What would settle it: read
`PmSta`/`PmEnd`/`PmOff` on a boot with a live desktop — `PmOff` is
`PRESENT_MARKER_LAST_OFFSET` and already records which arm won. On the boot this
was found, every `Pm*` read 0, so the instrument could not answer; it needs a
run where the present path is actually exercised.

---

## F14 — An HVM1 allocation's `D3DKMTLock2` CPU view is guest system RAM, not the venus blob the KMD allocated for it. A3's whole memory half is blocked on a KMD unit, and A1's landed reply pool is already in this state.

**Established 2026-08-11 by static reachability over `kmd_render/`, not by a
target run.** Stated that way deliberately: the conclusion follows from *which
code exists*, and the one instrument that could have caught it cannot.

### What `admit_hvm1` builds

`create_allocation.rs:3848-3856` gives every admitted HVM1 role a real host
venus blob (`build_backing` → `Hwa2Backing::LinearMemory{ mappable }`). So the
KMD does own renderer-side bytes per allocation, exactly as §10.7:1936-1940
requires.

### Why the guest CPU never reaches them

`ctrl::map_blob_at` is the **only** mechanism in this driver that makes guest CPU
pages alias venus blob bytes. It has exactly one caller — `cpu_host_aperture.rs:407`,
inside `DxgkDdiMapCpuHostAperture` — and that path returns `STATUS_NO_MEMORY`
before reaching it whenever `!alloc.bar_eligible` (`:387`, counter `ChEa`).

`admit_hvm1` sets `bar_eligible: false` (`create_allocation.rs:3917`), on purpose:
*"An HVM1 object's placement is HLM1 plus the ordinary aperture (§10.7:1999-2002);
the CpuHostAperture BAR path is the mechanism §17.6 deletes."*

The second candidate route is closed the same way: every paging operation on a
non-eligible allocation is a **counted no-op reported as success** —
`build_paging_buffer.rs:819` (virtual transfer), `:929` (transfer), `:1072` (fill),
`:1131`, `:1422` (VirtualFill) → `PagingOpOutcome::NotOurs`, counter `PgDi`.

⇒ VidMm backs the allocation, Lock2 maps *that* backing, and no code copies
between it and the blob in either direction.

### The mechanical cause is one flag word

§10.7:1983-1986 specifies HLM1 as `Aperture=0, CpuVisible=1, CacheCoherent=0,
SupportsCpuHostAperture=0` with `CpuTranslatedAddress = BAR guest-physical base`
— i.e. VidMm maps the BAR directly and needs no CpuHostAperture DDI at all. At
the shipping default `BarSegFlags = 0x1C` (`adapter/mod.rs:230,254`),
`from_bar_flags` (`query_adapter_info.rs:787-833`) yields `cpu_visible=false`,
`cache_coherent=true`, `supports_cpu_host_aperture=true`,
`cpu_access = CpuAccess::HostAperture`. **Neither reported segment is
`CpuVisible=1`.** The HLM1 shape is `BarSegFlags = 0x02`, and **F2 already
measured that value booting `CM_PROB_NONE` with a live composited desktop** — so
the shape is admissible; it is simply not what is reported, and nothing places an
HVM1 allocation's blob at the VidMm-assigned offset inside that window.

### ⛔ The consequence for what has already landed

`tools/hts1_session_probe.c` **cannot detect this**, and its H5 check is where
the false confidence lives: it writes three bytes through `lk.pData` and reads
them back *through the same pointer* (`:371-382`). A private buffer passes that
check identically. `TsPoolBind=1` therefore proves the KMD bound a pool object —
not that the pool is a channel the host can write a reply into.

⇒ **A1's role-1 HVM1 reply pool is not yet a reply channel.** HVR1 replies would
be read out of guest RAM the host never wrote. That has not caused a failure
because `session_init` grants zero endpoints until K11 (`translation_session.rs:879`)
and no HVR1 has ever been produced.

### ⭐ Measured on the target, and it also settles the design question

Counter diff across one `hts1_session_probe.exe` run on the deployed
**22.22.271.0**, `BarF = 28` (0x1C):

```
TsPoolBind : 2 -> 3     one role-1 HVM1 pool created, resident, Lock2-mapped
PgDi       : 3 -> 5     +2  BAR_DEVICE_OP_SKIPS — two paging ops for that pool
ChMn ChMc ChEa : 0 -> 0 (unchanged)  MapCpuHostAperture was never called at all
```

What follows is the skip, and only the skip: **+2 `PgDi` per pool**, with no
other `Pg*` counter moving — both operations returned at a `!alloc.bar_eligible`
early-return before any other site could count them. `ChMn`/`ChMc`/`ChEa` flat at
zero closes the one alternative worth ruling out: `ChEa` is a **failure** entry,
which forces an immediate flush, so an HVM1 reaching `MapCpuHostAperture` would
have moved it.

### ⛔ Correction — this measurement does NOT establish that a hook exists

The first version of this section read `PgDi +2` as *"VidMm does issue
placement-bearing paging operations for an HVM1 allocation, so the binding has a
hook."* **That does not follow, and the error is worth keeping visible**, because
it is the same shape as the H5 self-round-trip it was written to expose: a number
that moves is not a number that means what you wanted.

`PgDi` has **four** increment sites, not the five stated above
(`build_paging_buffer.rs:823`, `:933`, `:1073`, `:1423`; the `!bar_eligible`
return inside `bar_harvest_page_table` at `:1131` is a bare `return` and is
**completely uninstrumented**). They split on whether a segment was ever proven:

| site | operation | segment-gated before the skip? | descriptor carries SegmentId + SegmentAddress? |
|---|---|---|---|
| `:933` | `TRANSFER` | yes (`:917-920`) | **yes** |
| `:1073` | `FILL` | yes (`:1063-1065`) | **yes** |
| `:823` | `VIRTUAL_TRANSFER` | ⛔ no gate | ⛔ **no** — GPU virtual addresses only |
| `:1423` | `VIRTUAL_FILL` | ⛔ no gate | ⛔ **no** — `DestinationVirtualAddress` only |

⇒ `PgDi +2` is fully consistent with two segment-blind virtual ops on an
allocation VidMm placed in the **aperture**, in which case no placement ever
reached the KMD at all.

And the driver's own op census says the two are exactly that. `PAGING_OP_SEEN_MASK`,
read out of the diag S-ring (tag `0x0F01`) on the same boot, is **0x9B64**:

```
seen:     DISCARD_CONTENT(2) MAP_APERTURE_SEGMENT(5) UNMAP_APERTURE_SEGMENT(6)
          VIRTUAL_TRANSFER(8) VIRTUAL_FILL(9) UPDATE_PAGE_TABLE(11)
          FLUSH_TLB(12) NOTIFY_RESIDENCY(15)
NOT seen: TRANSFER(0) FILL(1) ...
```

⭐ **`TRANSFER` and `FILL` have never fired on this driver.** It declares
`Wddm2_1GpuMmu`, so VidMm uses the *virtual* content operations — which carry no
segment address — and reports placement through `UPDATE_PAGE_TABLE` and the
residency notifications instead. `bar_transfer` and `bar_fill`, the two arms that
do carry a `SegmentAddress`, are dead code on this target.

⚠ Bound on the census itself: `diag_dump_gpummu_atomics` masks the value `& 0xFFFF`,
so ops 16-22 are **not** observable this way. `NOTIFY_RESIDENCY2` (21) — which
carries `hAllocation`, an outer `SegmentId` and a `D3DGPU_PHYSICAL_ADDRESS`, and
which every HVM1 role already opts into via `explicit_residency_notification`
(`protocol/src/native_render.rs:1838`) — is therefore neither confirmed nor ruled
out. It is the strongest candidate hook and it currently falls through
`PagingOperation::parse`'s wildcard to `STATUS_SUCCESS`, uncounted.

⇒ **Which operation carries an HVM1 placement is not knowable read-only.** The
binding unit's first deploy is an instrument, not a fix.

### Bound, and what would falsify it

The disjointness itself is still a reachability argument, not a byte-level
measurement: no run has yet shown a host-originated write failing to appear
through an HVM1 Lock2 VA, because no host producer exists (K11). It is falsified
by any run in which one does appear. ⭐ The oracle that can settle it **without**
K11 already exists: `with_blob_bytes` (`build_paging_buffer.rs:548-586`) maps the
blob's real pages into kernel space via `map_blob_prepare` + `MmMapIoSpace`, so
the KMD can read back what the guest wrote through Lock2 and compare. That is the
acceptance check the binding unit owes.

### What it costs mesa A3

A3's HVM1 allocator, its role vocabulary, its create/close shapes and its
validator are all still correct to write — the contract does not move. What A3
**cannot** deliver until a KMD unit binds the CPU view is *bytes*: role 1/2/3
memory whose contents the host and guest agree on. Two candidate mechanisms, both
KMD-side and both small, are recorded in ROADMAP under "the HVM1 CPU-view
binding".

---

## F15 — VidMm places the role-1 HVM1 pool in the **aperture**, not HLM1, and the segment flag shape does not change that. `NOTIFY_RESIDENCY` is the hook; every observation arrives at PASSIVE.

**Measured 2026-08-11 on KMD 22.22.272.0**, the K2a deploy-1 instrument, one
`hts1_session_probe.exe` run per arm, counters zeroed at each StartDevice.

### The reading

```
HlElig=1   HlPlN=102  HlPlOp=15  HlPlSg=1  HlPlPg=640  HlPlLn=16384
HlOpMs=0x8B20  HlSgMs=0x7  HlPtSg=0  HlPtPg=6466528
HlEirq=0   HlEwin=0   HlFrgn=35  HlEvic=2  HlEnb=1
```

`HlOpMs = 0x8B20` names every paging operation that touched the pool:
**MAP_APERTURE_SEGMENT(5), VIRTUAL_TRANSFER(8), VIRTUAL_FILL(9),
UPDATE_PAGE_TABLE(11), NOTIFY_RESIDENCY(15)** — and nothing else. In particular
**no `NOTIFY_RESIDENCY2`(21), no `TRANSFER2`(23), no `FILL2`(24)**, the three the
design ranked most likely.

### Four answers, each of which changes the next unit

1. ⭐ **`NOTIFY_RESIDENCY` (15) is the hook.** It fires for the pool, it carries
   `hAllocation` + a `D3DGPU_PHYSICAL_ADDRESS`, and `HlPlOp = 15` says it wrote
   the last placement. `HlPlLn = 16384` pages = exactly the 64 MiB pool, so the
   descriptor describes the whole allocation.
2. ⭐ **`HlEirq = 0` — every observation arrived at PASSIVE_LEVEL.** This is the
   safety answer deploy 2 needed: `map_blob_at` needs a host round-trip, and the
   arm that carries the placement can legally issue one. Had this read nonzero the
   binding would have needed a deferral design.
3. ⛔ **The placement is segment 1, the APERTURE — not HLM1.** `HlPlSg = 1`, and
   the GPU page table agrees: `HlPtSg = 0`, i.e. PTE[0] maps the pool out of
   **system memory**. `hvm1_placement` keeps the aperture in
   `supported_segments` (§10.7:2001-2002's "system-residency physical-address
   domain") and VidMm takes it. `preferred_segment = HLM1` is a hint, not a
   choice. This is exactly the C5 hazard the design flagged and deferred.
4. ⛔ **The flag flip does not fix it.** Re-run at `BarSegFlags = 0x02` — HLM1's
   exact shape, `BarF` moved 28 → 2, `SegRule = 0`, `CM_PROB_NONE`, probe still
   15/15 — reproduces the reading **identically**: `HlPlSg = 1`, `HlPtSg = 0`,
   same op mask, same counts, only VidMm's chosen offset differs (640 → 128
   pages). The hypothesis that segment 2 being `CpuVisible = 0` was what forced
   the aperture is **falsified**. One `pnputil /restart-device`, no rebuild —
   which is the whole reason rule 8 requires the opposite value to stay reachable.

⇒ **Deploy 2's first move is not the bind and not the flag.** It is making VidMm
place the allocation in HLM1 at all, and the only remaining lever is the one the
plan deferred: drop `HELIOS_SEGMENT_ID_APERTURE` from `hvm1_placement`'s
supported set. That risks a hard `MakeResident` failure, so it belongs behind a
knob defaulting to today's measured behaviour.

### Bound — what this does NOT establish

`HlSgMs = 0x7` says the pool was named on segments **0, 1 and 2** across its life,
so something did name segment 2; the placement triple is last-writer-wins and only
records that the LAST placement-bearing observation was the aperture. A per-(op,
segment) histogram would be needed to say the pool was never in HLM1. What is not
in doubt is the page table: `HlPtSg = 0` is where the GPU was told to find it.

⚠ `HlFrgn = 35` and `HlEnb = 1` are both expected, not faults: 35 observations
named a segment this allocation is not bound to (the aperture maps, by
construction), and the pool died unbound because nothing binds yet.

### Two things the instrument proved about itself

* `HlEvic = 2` — `NOTIFY_RESIDENCY` fires in **both** directions and the eviction
  is the last notification a destroyed allocation gets. Without the `Resident()`
  check the review added, those two would have overwritten the residency answer
  with a zeroed address and this finding would have read
  "NOTIFY_RESIDENCY carries no placement" — the exact inverse.
* Every `Hl*` value read **0** after a `pnputil /restart-device`, which is the
  second StartDevice in one image load. The first version called
  `CounterBlock::flush()`, whose own throttle writes nothing in that case; the
  repaired `hlm1_reset_counters` writes literal zeros. Validated on the target,
  not argued.

## F16 — An HVM1 allocation's `D3DKMTLock2` view is VidMm's SYSTEM backing, and no HLM1 configuration changes that. F15's "VidMm is not using HLM1 at all" is FALSIFIED. The channel that does alias a blob already exists, and it is an escape.

**Measured 2026-08-11 on KMD 22.22.273.0 → 22.22.276.0**, nine configurations,
`hts1_session_probe.exe` per arm, counters zeroed at each StartDevice,
`CM_PROB_NONE` and **15/15** in every arm except where noted.

### The oracle this finding rests on

`hlm1_readback` (`HlRdbk`) reads the **blob's** bytes at the three offsets
`hts1_session_probe` H5 writes, from kernel space through `map_blob_prepare` +
`MmMapIoSpace`, and publishes them raw. H5 writes three bytes through `lk.pData`
and reads them back through the SAME pointer, so it passes identically on a
private buffer — that is why F14 was invisible to it. This reads the other side of
the alias, and the KMD is told neither H5's constants nor the stamp's, so the
verdict is a human's.

`Hlm1Bind=2` additionally stamps the blob with `kmd_logic::hlm1_placement`'s
vocabulary, which turns the oracle into its own positive control:

```
HlRdbk = 0x5500F7 = { 0x55 at offset 0, 0x00 at 16 MiB-1, 0xF7 at 64 MiB-1 }
stamp_byte(1, 0)          = 0x55   ← a stamped sample offset
stamp_byte(1, 64 MiB - 1) = 0xF7   ← the last stamped sample
                            0x00   ← 16 MiB-1 is not a sample: untouched host memory
HlDgst = 0x4CAB8D42 = digest(1, the whole sample set), computed independently
```

⇒ the readback reads the venus blob, correctly, and **the blob still holds the
KMD's stamp. H5's `0xA5`/`0x5A`/`0xC3` are nowhere in it.** F14 is now proven by
bytes rather than by reading code.

### The matrix

| # | `BarSegFlags` | `Hlm1Only` | `Hlm1FlagsOff` | `Hlm1Bind` | probe | `HlRdbk` | bind |
|---|---|---|---|---|---|---|---|
| 1 | 0x1C | 0 | 0 | 0 | 15/15 | — | — |
| 2 | 0x1C | **1** | 0 | 0 | **13/15, H4 `0xc0000001`** | — | — |
| 3 | 0x02 | **1** | 0 | 0 | **13/15, H4 `0xc0000001`** | — | — |
| 4 | 0x1C | 0 | 0 | **2** | 15/15 | `0x5500F7` | op 15, pg 1932 |
| 5 | 0x02 | 0 | 0 | **2** | 15/15 | `0x5500F7` | op 15, pg 4108 |
| 6 | 0x02 | 0 | **1** | **2** | 15/15 | `0x5500F7` | op 11, pg 4108 |
| 7 | 0x06 | 0 | **1** | **2** | 15/15 | `0x5500F7` | op 11, pg 1932 |
| 8 | 0x02 | 0 | **7** | **2** | 15/15 | `0x5500F7` | op 11, pg 1932 |
| 9 | 0x1C | 0 | 0 | 0 + **`Hlm1Bar=1`** | 15/15 | `0x000000` | — |

In every arm where a bind ran it ran **once** (`HlBndN=1`, `HlBndR=0`,
`HlBndE=0`, `HlBndQ=0`, `HlEwin=0`): the placement is stable, in window range, at
PASSIVE, and the host honoured the fixed map. The bind is not the problem.

### Four things the matrix settles

1. ⛔ **`Hlm1Only=1` does not make VidMm choose HLM1 — it breaks the page-in.**
   `MakeResident` still succeeds, then `D3DKMTLock2` returns `0xc0000001` and
   dxgkrnl's own ETW says why: **"WORKER_THREAD: Unrecoverable page in failure"**
   (`Microsoft-Windows-DxgKrnl`, all keywords, `AzureTriage`). No paging error
   counter of ours moves — `PgEb`/`PgEm`/`PgEx`/`PgEc` all 0 — so the refusal is
   dxgkrnl's, not the miniport's. The aperture bit in `supported_segments` is
   load-bearing exactly as §10.7:2001-2002 describes it. **The knob's default
   stays 0 and it must not be flipped.**
2. ⛔ **F15's headline was its own caveat coming true.** F15 read `HlPlSg=1` /
   `HlPtSg=0` as "VidMm places the pool in the APERTURE, not HLM1". Both are
   last-writer-wins fields. With the census added: **`HlPt2 = 66…99` of the
   page-table batches map the pool out of segment 2** against 33 out of system
   memory, `HlAdMs` shows BOTH op 11 and op 15 producing admitted segment-2
   placements, and on the two-pool run at defaults the last values are
   `HlPlSg = 2` and `HlPtSg = 2`. The pool lives in HLM1. F15's ⛔ bullets 3 and 4
   are withdrawn; its `NOTIFY_RESIDENCY`/`HlEirq` findings stand.
3. ⭐ **`AccessedPhysically` is what brings the aperture in, and clearing it takes
   `NOTIFY_RESIDENCY` away.** At `Hlm1FlagsOff=1`: `HlSgMs` 0x7 → 0x5 (no segment
   1 ever named), `HlOpMs` 0x8B20 → 0xB00 (no `MAP_APERTURE_SEGMENT`, and **no
   `NOTIFY_RESIDENCY` at all**), `HlEvic` 2 → 0. So the residency notification is
   a service for physically-accessed allocations, and a bind that hooks only it
   dies in that configuration — which is why `UPDATE_PAGE_TABLE` is now a hook
   too. `DisablePartialResidency` and `RestrictedToSingleSegment` change nothing.
4. ⛔ **Neither the flag word nor BAR eligibility produces an aliased view.**
   `CpuVisible` with `CpuTranslatedAddress = BAR GPA` (0x02), with
   `CacheCoherent` added (0x06), and `SupportsCpuHostAperture` (0x1C) all read
   `0x5500F7`; and making the allocation BAR-eligible so the proven
   `MapCpuHostAperture` path could serve it (arm 9) reads `0x000000` — the blob
   untouched, no `Ch*` counter moved.

### What the mechanism actually is

The 32-PTE page-table batches are the paging process's scratch windows around
`VIRTUAL_TRANSFER`/`VIRTUAL_FILL` — two thirds addressing the pool in segment 2
and one third in system memory, with `PgVs`/`PgVd`/`PgVp` nonzero. That is VidMm
treating HLM1 as ordinary video memory whose content it **moves by paging
transfer**, and handing the CPU the allocation's system backing for the lock.
`D3DKMTLock2` on this build is a copy protocol for such an allocation, not a
window onto it.

⇒ **§10.7's HLM1 CPU-view model does not hold on build 26100.8875.** A copy is
not admissible either: §10.7:1940 says an HVM1 allocation's "bytes are never an
independent private copy", and a reply pool the host writes asynchronously cannot
be a snapshot taken at Unlock.

### ⛔ The channel that works is FORBIDDEN, and the design's own answer was declined

`HELIOS_ESCAPE_MAP_BLOB` (`escape.rs:1377`, `escape_map_blob`) does deliver exactly
this: `map_blob_prepare` + `map_io_pages_to_user` in the caller's process, returning
`out_user_va`, a **live aliased** user-mode view of the blob's window pages, torn
down at `DxgkDdiDestroyDevice`, production-proven by the D3D11 ICD and DXVK.

⛔ **It is not available and must not be proposed.** §3:370-371 is a hard
constraint — "No `D3DKMTEscape` for transport, discovery, metadata,
synchronization, completion, lifetime, diagnostics, or fallback" — §18.1's static
gate is *absent Escape objects* with `DxgkDdiEscape` left NULL (:4494), and **K1
deletes `escape.rs`, `blob_map.rs` (which is where `map_io_pages_to_user` lives),
`cpu_host_aperture.rs` and `MappingTable`** (`lane-kmd-core.md:111-112,150,200`).
An earlier draft of this finding recommended the escape; that recommendation is
**withdrawn** — it contradicted §3.

⭐ **What the design actually specifies, and why nothing implements it.**
§10.7:1939-1941, in full: "HLM1 is the package's fully CPU-visible linear
local-memory segment; **HPM1 is the actual paging/device protocol that binds each
current VidMm placement to the same renderer payload and copies the bytes on every
placement transition**." So §10.7 never claimed a permanently aliased window — it
claimed a paging protocol that keeps the placement and the payload in sync across
transitions, which is precisely the copy-on-transition behaviour measured above.

⇒ **The mechanism this finding shows missing is HPM1, and F5 DECLINED HPM1.**
`lane-kmd-core.md:139` already records the consequence in terms: the forbidden
byte copy's "named replacement — 'HPM1 is the actual paging/device protocol' — was
**DECLINED, not deferred** … so the deletion has no successor mechanism today and
must be an explicit recorded decision, not a side effect." F16 is that decision
arriving with numbers attached. It is not a new gap; it is F5's gap, measured.

### The non-Escape options, ranked by cost

1. **Finish the diagnosis before changing the channel** (no rebuild). Two things
   are untested. (a) An ETW `Microsoft-Windows-DxgKrnl` all-keywords slice around
   ONE probe run — the same instrument that answered "what is dxgkrnl doing to my
   thread" for WS2 — read for `Lock`/`AllocationResidency`/`PagingQueue`: does
   dxgkrnl evict to system memory *at the lock*, and does it ever consider the
   allocation resident in segment 2 at that moment? (b) `VidMmVramMB=1024`:
   **segment 2 currently reports 8 GiB while the KMD's VidMm partition is
   `bar.size` = 1 GiB** (`BAR_SEGMENT_MAX_BYTES`), and the KMD's own blob
   allocator owns `[1 GiB, 8 GiB)` of the same window — two allocators in one
   range, which §10.7:2013-2015's "`BaseAddress`, `Size`, `CommitLimit` … match
   the one negotiated local physical range" forbids and which is a latent host
   subregion overlap regardless of this unit. Registry-only, graded by `HlRdbk`.
2. ⛔ ~~**Guest-backed storage for roles 1 and 3.**~~ **FALSIFIED 2026-08-11 from
   the ICD's own source — this option is dead and the claim behind it was wrong.**
   Upstream venus does NOT keep reply/feedback storage in guest shmem; it
   explicitly REJECTED that, for Helios' exact reason.
   `icd/mesa/src/virtio/vulkan/vn_renderer_virtgpu.c:1535-1556`,
   `virtgpu_init_shmem_blob_mem`: "VIRTGPU_BLOB_MEM_GUEST allocates from the guest
   system memory. They are logically contiguous in the guest but are sglists
   (iovecs) in the host. That makes them slower to process in the host. **With host
   process isolation, it also becomes impossible for the host to access sglists
   directly.**" … `gpu->shmem_blob_mem = VIRTGPU_BLOB_MEM_HOST3D;`. Helios' own
   backend does the same and says why (`vn_renderer_helios.c:4180-4187`: "The
   command-stream ring + cs/reply pools need genuinely host-coherent, mappable
   memory the renderer can both read and write"), and host process isolation is
   unconditional here — `qemu-helios/hw/display/virtio-gpu-virgl.c:1546-1548` sets
   `VIRGL_RENDERER_VENUS | VIRGL_RENDERER_RENDER_SERVER` for any venus device.
   ⇒ **All four roles are host memory. The problem does not invert.**
3. **Un-decline HPM1** (F5). The design's own mechanism, blocked on the parked
   QEMU memory lane.
4. **Accept the paging-transfer copy for roles 1 and 3.** Closer to §10.7 than it
   looks, since §10.7:1940 is what HPM1's copy-on-transition sentence qualifies —
   but a host-written reply pool needs live visibility, not a snapshot taken at
   `Unlock2`, so this is a fallback for feedback storage at best.

### Bound — what this does NOT say

It does not say a CPU-visible memory segment cannot alias a blob in principle; it
says that on this build, for an allocation with §10.7's flag set, dxgkrnl never
built the CPU VA from `CpuTranslatedAddress + SegmentAddress` in any of the nine
configurations tried. Arm 9's `MapCpuHostAperture` route is refuted only in the
weak sense: the DDI was never called, and the `Ch*` block is flushed on refusals
and mode sets, so "never called" is inference from `ChEa = 0` (a failure entry,
whose increment forces a flush) rather than from a positive trace.

## F17 — The escape-free CPU view is a WDDM **2.9** callback pair, and the driver declares **2.1**. K2a is not a memory-model problem; it is downstream of the version uplift the retirement already plans.

⛔ **HISTORICAL; superseded by F18.** D9 raised the driver to WDDM 3.2 and
K2a selected `ShareBackingStoreWithKmd`, not the physical-memory-object pair.

**Researched 2026-08-11** (30 candidate mechanisms across six corpora, 29 refuted,
1 survivor; the shipping-kit and ICD citations below re-verified by hand). This
finding supersedes F16's recommendation section.

### The survivor

`DXGKCB_CREATEPHYSICALMEMORYOBJECT` + `DXGKCB_MAPPHYSICALMEMORY`. Verified in the
shipping WDK **28000** kit on the target:

```
Include\10.0.28000.0\shared\d3dkmddi.h:10118  DXGK_PHYSICAL_MEMORY_TYPE
                                      :10123    DXGK_PHYSICAL_MEMORY_TYPE_IO_SPACE
                                      :10129    DXGK_ACCESS_MODE_USER_MODE
                                      :10183  DXGKCB_CREATEPHYSICALMEMORYOBJECT
                                      :10219  DXGKCB_MAPPHYSICALMEMORY
```

`Type = IO_SPACE` with `IOSpace.BaseAddress` = the blob's window guest-physical
address, `CacheType = WRITE_COMBINED`, then `AccessMode = USER_MODE` — dxgkrnl does
the mapping the Escape does by hand today, and Microsoft states that intent in
terms: "the ability to map CPU virtual addresses from the physical memory in both
user mode and kernel mode, with a specified cache type" and "The ability to express
IO space ranges is also required"
(`windows-driver-docs-research-only/…/display/iommu-dma-remapping.md:137,141`).

### ⛔ And it is gated above the level this driver reports

```
Include\10.0.28000.0\km\dispmprt.h:2290  #if (DXGKDDI_INTERFACE_VERSION >= DXGKDDI_INTERFACE_VERSION_WDDM2_9)
                                  :2292    DXGKCB_CREATEPHYSICALMEMORYOBJECT DxgkCbCreatePhysicalMemoryObject;
                                  :2294    DXGKCB_MAPPHYSICALMEMORY          DxgkCbMapPhysicalMemory;
                                  :2301  #endif
```

`kmd_render/src/ddi/wddm_surface.rs:64` declares `WddmSurface::Wddm2_1GpuMmu`, i.e.
`DXGKDDI_INTERFACE_VERSION_WDDM2_1` (`:75`). **No callback in the 2.9 block has
ever been invoked by this driver.** So the mechanism sits above what we report and
below the WDDM 3.2 the retirement is already heading to — `lane-kmd-display.md`
unit **D3** adds `ddi/mpo3.rs` with the seven MPO3 slots, unit **D9** flips
`SURFACE` to `Wddm3_2GpuMmu` *strictly last*, `doc:2854` rejects a 3.2 package
whose MPO3/fence surface is incomplete (no 2.1 fallback), and `doc:5079` gates on
"DWM starts on the 3.2 surface without `CDDisplaySwapChain`/`E_NOTIMPL`". The
`E_NOTIMPL` recorded at `wddm_surface.rs:25-28` is exactly what D3 removes.

⇒ **K2a's CPU view is downstream of D3 → D9, not a new detour**, unless dxgkrnl
populates the 2.9 block for a 2.1-declaring driver anyway.

### The ~10-line instrument that sizes the work — and audits a latent over-read

`adapter/mod.rs:354` does `dxgkrnl: unsafe { *dxgkrnl }`, a full copy of the
`DXGKRNL_INTERFACE` **as bindgen shapes it against the 28000 headers** (576 bytes),
and **nothing in the driver reads `DXGKRNL_INTERFACE.Size`**. Two things follow, and
one StartDevice instrument answers both: record `.Size` and the two 2.9 function
pointers.

* If the pointers are non-NULL at 2.1, K2a unblocks NOW, ahead of D3/D9.
* If they are NULL, K2a sequences behind the uplift — a real dependency, but a
  planned one.
* Either way: if `.Size` < 576 we have been copying past the end of a structure the
  OS sized for the version we declared. Unaudited since bring-up.

### Two claims this research falsified — both were written earlier the same day

1. ⛔ **Guest-backed storage for roles 1/3 is dead**, and F16's option 2 is struck
   above: upstream venus explicitly rejected `VIRTGPU_BLOB_MEM_GUEST` for shmem
   because "with host process isolation, it also becomes impossible for the host to
   access sglists directly" (`vn_renderer_virtgpu.c:1535-1556`). All four roles are
   host memory.
2. ⛔ **Helios' D3D11 desktop does NOT composite through
   `DxgkDdiMapCpuHostAperture`** — it composites through `HELIOS_ESCAPE_MAP_BLOB`.
   `CPU_HOST_MAP_COUNT` increments unconditionally at `cpu_host_aperture.rs:304`
   *before* any segment or eligibility filter and reads **0** on a live composited
   desktop, and all seven `ChE*` refusal counters are `failure: true` entries whose
   change forces an immediate flush regardless of the throttle (`diag.rs:444-459`),
   so `ChEa = 0` is not a flush artifact. F16's arm-9 bound is therefore stronger
   than F16 claimed, and the "proven aliasing path" this session cited for the
   aperture was the Escape all along.

⇒ **§17.6's deletion of the CPU host aperture is CORRECT — it serves nothing.**
`cpu-host-aperature.md:10` scopes the feature to "32bit OS discrete GPUs, which
don't support resizable BAR", which is not this device. Land the deletion coupled
with clearing `SupportsCpuHostAperture` from the segment descriptor, and *after* a
replacement CPU-view channel is chosen — §17.6's stated successor is HPM1, and F5
declined HPM1.

### Why `Lock2` copies, now with the documented condition

`allocation-usage-tracking.md:36`: "all CPU-accessible allocations in
non-CPU-accessible memory segments must contain an aperture segment in their
supported segment set. **This requirement guarantees that VidMm is able to place
the allocation within system memory and provide a virtual address**", and `:41`
"CPU-accessible allocations are backed by **section objects** that can't point
directly to the GPUs frame buffer." F16's arms 1/4/9 are that guarantee being
honoured; arms 2/3 (`Hlm1Only=1` → unrecoverable page-in) are its other half. The
one unconditional promise of a direct pointer, `:35`, is scoped to "a fully
CPU-accessible memory segment **(resized using the resizable BAR)**" — and
virtio-gpu exposes no PCIe Resizable BAR capability. ⚠ INFERENCE, not yet measured:
the field that records VidMm's own verdict is
`D3DKMT_QUERYSTATISTICS_SEGMENT_INFORMATION::SegmentProperties.FullyCPUVisible`
(`shared/d3dkmthk.h:4119`, inside the 3.2 gate at `:4117`). `tools/vidmm_tracking_probe.c:275-292`
already issues the query per segment id; adding four fields to its printout is the
cheapest open measurement in this finding — user-mode recompile only, no KMD build,
no registry write, no reboot.

---

## F18 — K2a uses WDDM shared allocation backing, not F17's physical-memory-object route. The exact guest pages now back both Lock2 and the renderer blob.

**Implemented and measured 2026-08-13.** This finding supersedes F17's selected
mechanism and its statement that the driver still declares WDDM 2.1. F17 remains
the historical record of why ordinary HLM1 placement and Lock2 did not alias the
renderer blob.

### The documented contract that closes the handoff

Microsoft's [Sharing the backing store with KMD](https://learn.microsoft.com/en-us/windows-hardware/drivers/display/sharing-backing-store-with-kmd)
contract is available to WDDM 3.1+ drivers. KMD must query
`DXGK_FEATURE_SHARE_BACKING_STORE_WITH_KMD`, set
`DXGK_ALLOCATIONINFOFLAGS2::ShareBackingStoreWithKmd` only when enabled, and
receive the backing through `DXGKDDI_SETALLOCATIONBACKINGSTORE`; the allocation
must be shared, CPU-visible, system-memory-only, and must not use supplied
existing system memory. UMD then obtains its process-local address through the
ordinary `D3DKMTLock2` path.

That contract resolves F17's unanswered user-mode handoff without putting a
pointer, handle, PID, renderer resource ID, or reusable lookup token in HVM1 or
HNR2. The creating/opening process receives and retains its own Lock2 VA through
normal WDDM lifetime objects; KMD receives the same backing-store pages through
the new DDI. The physical-memory Create/Map/Unmap/Destroy callback family is not
used by K2a.

### Landed shape

* D9 already raised `SURFACE` to `Wddm3_2GpuMmu`. StartDevice now copies only
  `min(DXGKRNL_INTERFACE.Size, sizeof(DXGKRNL_INTERFACE))`, after requiring Size
  coverage through the last callback it reads,
  `DxgkCbQueryFeatureSupport`. Missing coverage or a disabled feature fails
  StartDevice; there is no partial-callback or fallback arm.
* HVM1 roles 1–3 are shared system-memory allocations with
  `ShareBackingStoreWithKmd=1`. Role 4 refuses before backing mutation.
  `DxgkDdiSetAllocationBackingStore` is PASSIVE-only, accepts the exact live
  allocation object once, locks the supplied MDL, coalesces its PFNs with
  checked size/range arithmetic, and creates an exact-size
  `VIRTIO_GPU_BLOB_MEM_GUEST` Venus resource. Its three-state
  UNBOUND/BINDING/BOUND publication is fail-closed: pre-dispatch failures return
  to UNBOUND; an ambiguous post-dispatch failure remains BINDING and cannot be
  retried as a second identity.
* Normal teardown prevents new use, unrefs the renderer resource, observes the
  fenced host response, and only then unlocks the backing MDL. Failed create
  unwinds only completed steps in reverse order. The exact allocation/open/
  process/session provenance and HVM1 generation remain the authority.
* Role 1 is 4 MiB split into four 1 MiB slots. HVR1's 80-byte header leaves
  1,048,496 bytes per reply chunk. A logical snapshot remains bounded at
  64 MiB and uses continuation chunks; HNR2's independent 15 MiB command-payload
  limit is unchanged. Package generation is 2.
* `qemu-helios` was rebased onto upstream base `d49f87606a`; the five scoped K2a
  commits end at `415a5ef078` and total 143 additions / 32 deletions. QEMU creates
  one udmabuf from the exact guest ranges, imports it into the renderer, maps it
  bidirectionally, and revokes it after renderer unref. virglrenderer is
  unchanged; the HPM1 branch remains parked.

The Linux host retained the stock udmabuf `list_limit=1024`. A 64 MiB trial
needed 3,543 coalesced ranges and was correctly rejected. Measured 4 MiB
allocations used 143–853 ranges, and even the page-by-page worst case is exactly
1024, so the four 1 MiB role-1 slots need no kernel parameter change. This does
**not** prove that a future arbitrarily large role-2/role-3 allocation fits:
Mesa A3 must preserve an explicit size/fragmentation bound or fail loudly rather
than assuming coalescing. K2a neither hides nor solves that future consumer
policy.

### Final-source target evidence

KMD **22.22.288.0** loaded after the authorized guest reboot as `oem120.inf` on
Windows build 26100. The deployed, re-signed SYS is **849,144 bytes**, SHA-256
`df5903586a067b3c2e4047713278f92b38e7573e695fcd12bdd63d25d2556b86`.
PnP is `CM_PROB_NONE`, dxdiag reports WDDM 3.2, and the KMD recorded
`DXGKRNL_INTERFACE.Size=576` plus feature-enabled = 1. These are necessary only.

`tools/k2a_shared_backing_probe.c` passed **52/52**: roles 1–3, explicit role-4
refusal, eight create/map/unmap/destroy repetitions, teardown of an unclosed
mapped allocation at process exit, fresh allocation after teardown,
wrong-process raw-handle rejection, and stale-handle rejection. The stronger
alias run correlated QEMU renderer resource `0x1f`, exact size 4 MiB, 430 guest
ranges, first GPA `0x133c75000`, and last byte GPA `0x46055dff8`. Windows read
the host's first/last sentinels; held host mappings then read Windows' replacement
sentinels. Both sides exited successfully, and QEMU returned to its identical
227-total-FD baseline: one `/dev/udmabuf` and two `/dmabuf:` descriptors.

The display is still **runtime-unadmitted**: current DWM loads WARP and
DisplayConfig reports zero paths. K11 and Mesa A3/A4 remain; Escape stays NULL.
A successful map, Code 0, WDDM 3.2 string, counter, hash, or frozen frame is not
visible-desktop evidence.

---

## F19 — Stock virtio-gpu/Venus is sufficient for one exact per-HTS1 host namespace; K11 needs no HPM1 or new QEMU protocol.

**Implemented and measured 2026-08-14.** This finding supersedes the historical
K11 inventory in `lane-kmd-core.md` where it names HPM1 packets, a new QEMU lane,
or a wholesale shared-ring rewrite. F5 remains in force, and K2a's QEMU exception
remains frozen at `415a5ef078`.

### The re-derived K5/K6 handoff

K5 already provides the exact ordinary WDDM ownership graph: dxgkrnl's
`ProcessContext`, raw `DeviceContext`, HVC1 control `NativeContext`, heap-pinned
`SessionObject`, canonical role-1 allocation/open object, and direct HQA1 outer
context references. K6 already provides bounded HNR2 fragment decoding, the
context-local slot claim, Patch/SubmitCommand records, and the finite-control
call site. Its missing boundary was narrower than the old lane row: no distinct
host Venus context, no actual host reply producer, and therefore no truthful
nonzero INIT endpoint publication.

K11 attaches host state only to that exact `SessionObject`. It does not put a
PID, name, renderer resource id, pointer, KMT handle, host context id, or lookup
token into HTS1/HVC1/HQA1/HNR2/HVM1/HVR1 and does not add submit-time discovery.
Multiple sessions in one process therefore remain distinct by construction:
each raw device/control context owns a different session object and each session
creates a different stock Venus context/object namespace.

### Landed stock-Venus shape

* INIT first projects the exact live K2a role-1 allocation and transport
  generation. It creates a stock Venus context plus one private 4096-byte
  HOST3D/MAPPABLE reply resource, maps only that private resource in KMD, and
  borrows the canonical `(owner, context, resource)` pair directly.
* Ring zero submits a finite `vkSetReplyCommandStreamMESA` at fence 1 and
  `vkCreateInstance` at fence 2. The second used-ring response is terminal for
  decode and reply write. KMD then reads exactly 24 private bytes and validates
  opcode, zero status, pointer count, and the expected instance handle. Mere
  command acceptance or a counter is not INIT evidence.
* Only after that validation does KMD generate the nonzero HTS1 generation and
  CSPRNG capability, reserve the complete fixed endpoint/ring namespace, copy
  the exact final reply into the checked-out K2a role-1 slot as HVR1, and make
  the session externally live. The capacity is nonzero, no larger than the
  request, and bounded by 64; ring zero remains control-only and every granted
  endpoint has its nonzero ring reserved before publication.
* K11 executes only that finite allocation-free INIT. Allocation-backed,
  queue, GPU-dependent, general-schema, residency, and epoch operations retain
  K6's named refusals. The four 1 MiB HVR1 slots, 1,048,496-byte per-chunk
  payload, and 64 MiB logical snapshot ceiling are unchanged.
* Each HVC1 context has one local WDDM `SubmissionFenceId` watermark admitted
  only after the host reply and HVR1 publication. No adapter-global boundary
  queue, shared timeline, forged completion, polling, sleep, or synthetic
  completion exists.

Teardown closes new admission and drains the exact operation rundown before
issuing the optional fenced `vkDestroyInstance` at fence 3. It then unmaps and
detaches the private reply resource, unrefs it, destroys the host context, and
only afterward permits session and K2a backing references to fall. Ambiguous
cleanup quarantines the owner/context until reset instead of releasing backing
out of order. Failed or repeated INIT aborts the exact in-flight slot before
session draining; this ordering was found by the updated HNR2 probe and modeled
explicitly so refusal cannot strand a reply slot or fabricate C51 publication.

The bounded ordinary review found a second ordering edge: keeping either the
per-session owner rundown or the WDDM notification lock held across
`DxgkCbSynchronizeExecution` lets a concurrent failed-INIT Render teardown wait
on the SubmitCommand whose callback is waiting on that Render. The final source
uses a fixed per-adapter completion rundown from current-generation admission
through the exact notification, releases session/resource guards before the OS
callback, and closes/joins the adapter rundown only for reset, Stop, and Remove.
It carries no session identity, host context, queue, timeline, or lookup.

### Final-source target evidence

KMD **22.22.296.0** loaded as `oem128.inf` on Windows build 26100 with
`CM_PROB_NONE`. The active DriverStore SYS is **880,376 bytes**, SHA-256
`b12a1c914eb1a126c7170ad00974a5310f3828df65ed4d835bf09592c538167e`, with a
valid WDR test signature.

`tools/k11_session_transport_probe.c` passed **14/14**. Two simultaneous
sessions in one process requested endpoint capacities four and two and received
distinct nonzero generations/capabilities. Both carried actual correlated final
HVR1 bytes for their own host `vkCreateInstance`; a crossed capability attach
failed. A child process could not attach the inherited key, abrupt child exit
drained, four repeated INIT/teardown cycles succeeded, and a fresh session
succeeded afterward. The standing HTS1 probe passed **15/15**, the updated HNR2
probe passed **15/15** including deliberate same-session repeated-INIT refusal,
and the unchanged K2a probe passed **52/52**. HNR2 was deliberately run first
against the final image and completed promptly rather than reproducing the two
review-build deadlocks.

A held-capability run reached READY, an exact PnP adapter restart succeeded, the
old capability drained, and the process exited cleanly. The adapter returned
Code 0; a fresh K11 run passed 14/14 with `K11CtxNew=K11CtxDel=8`, every K11
failure counter zero, `K11CmpOpenRej=0`, and `TsSlotRel=8` / `TsSlotStuck=0`.
No transient per-session host process remained. The source/mutation gate
rejected 62 semantic mutations, including
shared namespaces, discovery, early/zero capacity, forbidden work, polling or
synthetic completion, unbounded storage, reply-pool early release, ABI identity
tokens, alternate carriers, new host dependencies, weakened prerequisite gates,
and false visible-desktop claims.

The display is still **runtime-unadmitted** after reset: DWM loads WARP and
DisplayConfig reports zero active paths. Mesa A3/A4 remain the next handoff;
Escape and HWQueues stay NULL. INIT, Code 0, hashes, counters, or a clean reset
are not visible or cold-DWM admission and do not establish HPS2 retirement or
production correctness.

---

## F20 — K9 orders real completions but does not execute A4; the bounded KMD executor/bootstrap continuation is explicitly authorized.

**Reconciled and authorized 2026-08-19.** This finding records the pre-edit
answer after root commit `a4db0ad` landed K9. It does not claim that the
continuation, Mesa A3, or Mesa A4 has been implemented. K9 is source/build
validated only; the installed target KMD remains 22.22.296.0, whose K11/HTS1/
HNR2/K2a probes passed 14/14, 15/15, 15/15, and 52/52 respectively. DWM still
uses WARP and DisplayConfig still reports zero active paths.

### The exact remaining call flow

The shipping Mesa path in `vn_CreateInstance` calls
`vn_instance_init_renderer`, initializes both shmem pools, calls
`vn_instance_init_ring`, reads renderer versions, and finally issues
`vn_call_vkCreateInstance`. The Windows ring still publishes
`ring->shmem->res_id` in `VkRingCreateInfoMESA`. In contrast, K11's exact HTS1
session already owns one stock-Venus context and one `VkInstance` with its
fixed private host handle. A3 must connect the renderer lifetime directly to
that exact A1/K11 session and bypass the duplicate instance/ring construction
on this backend without changing generic non-Windows Venus or starting A8's
whole generic-ring retirement. Two `vn_instance` objects remain distinct
because each owns its own raw KMT device, HVC1 control context, heap-pinned
`SessionObject`, K11 host context/object namespace, generation, capability, and
teardown; no PID, TLS, name, global lookup, or heuristic participates.

K11 deliberately executes only the finite, copied, one-fragment,
reply-bearing INIT. For every other record, `native_render.rs` currently:

* validates the bounded HNR2 fragment/use/patch layout but does not retain a
  host payload (`NR2_NO_STAGE`);
* leaves typed host-resource operands zero because no exact allocation
  substitution exists (`NR2_NO_RESID`);
* has no generated opcode/operand executor schema (`NR2_NO_SCHEMA`); and
* returns `NativeSubmitDisposition::Refused` at SubmitCommand because there was
  no actual host completion (`NR2_NO_HOST`).

`submit_command.rs` routes that refused disposition to the legacy
`note_and_maybe_signal` compatibility arm. A4 may not reinterpret that arm as a
host result: doing so would create the synthetic WDDM completion the contract
forbids. K9 accepts an exact scheduler fence ticket, retains early results from
different contexts, and exposes only the contiguous one-engine prefix. It can
order a real completion; it cannot manufacture or execute one. Therefore the
K9 ordered frontier is necessary for multi-context A4, but it is not the
missing executor.

The allocation half has one independent hard boundary. `admit_hvm1` rejects
role 4 (`VulkanDeviceLocal`) at `CREATE_HVM1_MEMORY_CLASS_REFUSED` / `AcHvm1Mem`
because the current Venus client selects a single host-visible/coherent memory
type and cannot truthfully express the requested non-CPU-visible class.
Role 4 may never be Lock2-mapped, and host-visible substitution is not an
implementation. Roles 1–3 remain governed by F18: the retained exact Lock2
view must use K2a backing, and CPU-visible success must obey the proven stock
udmabuf bound. A worst-case 4 MiB allocation is exactly 1024 pages; anything
larger needs a proven range bound or a named failure.

### Authorized continuation and its stop boundary

The owner authorizes the minimum KMD work required for correct A3/A4 execution:

* truthful role-4 HVM1 allocation using existing stock Venus facilities, with
  no Lock2 view;
* bounded immutable HNR2 payload custody tied directly to the current session,
  nonzero endpoint, context, allocation generations, and teardown rundown;
* validation of only the exact A3/A4 opcode and typed-operand subset, leaving
  every other class at a named refusal rather than absorbing A7's whole-object
  classifier;
* KMD-only patching of a host-private command copy from the exact current WDDM
  allocation references, while the wire and Mesa copy keep host-resource
  operands zero;
* stock-Venus submission on the exact nonzero K11 endpoint, with imported
  native-fence wait/signal ordering on that same context;
* actual terminal host completion delivered through K9 before the exact WDDM
  fence, and reverse-order revocation/drain across session destruction, reset,
  Stop, and Remove; and
* the smallest Mesa instance/ring lifecycle seams needed to consume that path,
  without implementing A5 or performing A8's generic-ring demolition.

This authorization does **not** permit a new DDI, wire record, host protocol,
QEMU or virglrenderer change, HPM1, kernel-parameter change, resource-ID token,
lookup table, adapter-global work queue, independent timeline, polling, sleep,
forged completion, or compatibility fallback. It does not start K1, K8, K10,
A5-A9, the present-layer lane, DXVK/vkd3d cutover, packaging activation, or a
cold-DWM retry. If truthful role 4 or the exact executor cannot be completed
inside these limits with existing stock virtio-gpu/Venus operations, the next
implementation must stop with exact symbols, call flow, and runtime evidence
instead of widening the maintenance surface.

### Implementation checkpoint (2026-08-19)

The bounded continuation described above landed at root `e8819c1`. It uses the
existing stock virtio-gpu/Venus operations and fixed ABIs: role 4 selects a
truthful non-host-visible memory class and has no CPU mapping; general admitted
HNR2 custody is fixed-size and tied to the exact session, endpoint, context,
allocation generations, and rundown; only the generated A3/A4 Venus subset is
accepted; host resource operands remain zero on the wire and are patched only
in a host-private copy from current allocation objects; and only the terminal
host result reaches K9. Context/session/reset/Stop/Remove paths revoke new work,
join callbacks, and release custody in reverse order. Refusal never enters the
compatibility completion arm as an A4 success.

Mesa A3 landed at `2c2763b8b1a` and A4 at `5cbc0254f43`. The Windows renderer
owns exactly one direct A1/K11 session per `vn_instance`; roles 1–3 keep their
exact K2a Lock2 views, role 4 never locks, C57 imports only the exact
`D3D12_RESOURCE_BIT` handle, and HNF1 opens only the exact `D3D12_FENCE_BIT`
handle on that KMT device. A4 owns submission mode per instance, requires a live
exact scope in record-only mode, seals bounded immutable batches synchronously,
and performs no KMT work or completion there. Normal mode emits zero host
resource operands and brackets HNR2 work with imported waits/signals and C51 on
the same context. Sparse submissions carry exact allocation closure;
command-buffer submissions remain a named refusal until A7 provides their
complete use and typed-operand tables.

A3 did not require A8. The only shared lifecycle change is the smallest Windows
seam that skips duplicate host-instance/ring creation and assigns nonzero queue
endpoints after the dedicated bootstrap endpoint; generic ring demolition stays
A8. A5-A9 and the present-layer lane were not started. The new KMD and ICD pass
source/mutation gates and Windows builds but were not installed, restarted, or
exercised on the target. The installed KMD remains 22.22.296.0 / `oem128.inf`;
DWM/WARP and zero active DisplayConfig paths remain the last measured runtime
state. This checkpoint establishes neither display admission, HPS2 retirement,
nor production correctness.

## F21 — The lower-ICD A7 stop was the missing direct outer-token producer graph; the bounded graph is now landed.

**Original reconciliation and stop, 2026-08-20.** Mesa A5 landed at `4ea18b3512f`,
fail-closed A6 at `d07d1d13687`, and the ordinary review's exact-image handle-
query fix at `478c71a0fff`. They are source/build validated only. The
installed ICD and KMD were not replaced or exercised, so the last runtime state
remains KMD 22.22.296.0 / `oem128.inf`, DWM on WARP, and zero active
DisplayConfig paths.

The fixed protocol makes the A7 identity boundary explicit.
`HeliosSealedResourceUseV1.outer_allocation_token` is an opaque nonzero scalar
assigned by the outer UMD. The ICD may echo and deduplicate it but may not infer
or interpret it. `protocol/include/helios_translator_dispatch.h` and
`protocol/src/translator_dispatch.rs` state that it reaches the ICD on the
DXVK/vkd3d and UMD resource-creation path alongside the deferred frontend
handles. `HeliosTranslatorDispatchV1` is fixed at 112 bytes: a 24-byte header
and exactly eleven down-call slots. Those slots cover proc lookup, endpoint/HQA1
selection, direct context attachment, scope open/seal/copy/close, refusal
counters, and destruction; none registers an allocation identity.

No current DXVK, vkd3d, UMD, UMD12, or Mesa resource-creation path consumes A5
and attaches that outer token to a Mesa object. Mesa owns only the exact local
HVM1 allocation handle and generation. They identify the KMD allocation behind
the lower ICD; they are not the UMD-assigned outer allocation token, allocation-
list index, or GPUVA required by the sealed-use and HOB1 contracts.

The live fail-closed call flow is exact:

1. `vn_helios_queue_submit` / `vn_helios_queue_submit2` call
   `helios_submit1_deferred_use_gate` /
   `helios_submit2_deferred_use_gate`.
2. Any command buffer increments `HELIOS_RECORD_REFUSE_DEFERRED_USE` and returns
   `VK_ERROR_FEATURE_NOT_PRESENT`, because its complete allocation-use and
   typed-operand closure cannot be named.
3. Independently, record-only `helios_dispatch_payload` refuses any nonzero
   `allocation_count`; it explicitly forbids leaking an HVM1 handle or
   fabricating a token.
4. `vn_helios_record_scope_seal` can validate and expose immutable uses only
   after their exact tokens exist. The outer bridge—not Mesa—must resolve each
   token to the current outer allocation-list index/GPUVA and generation before
   HOB1 submission.

A generated KMD validator/schema entry cannot create this missing producer-side
association. Completing A7 therefore requires either the DXVK/vkd3d plus UMD
resource-creation cutover that this tranche explicitly excluded or a new
identity carrier/ABI that it explicitly forbade. No KMD/schema change was made.
Dependency order stops A8/A9 as well. The present-layer B lane, DXVK/vkd3d
cutover, K1 demolition, HPS2 deletion, packaging/deployment, and target probing
remain unstarted.

**Resolved in source under bounded owner authorization, 2026-08-21.** The
producer-side identity was not a KMD-schema problem, and it was not repaired by
substituting Mesa's HVM1 identity. Protocol commit `eec9564` advances package
generation to 3 and declares HRA1 once in Rust plus its C mirror: a 72-byte,
immutable, process-local resource creation/bind association containing the
direct device generation, opaque nonzero outer token, allocation byte bound,
and optional direct CPU-view fact. It contains no host resid, WDDM handle,
allocation-list index, GPUVA, allocation generation, session capability,
resource ID, PID/name, or pointer used as identity. The A5 dispatch ABI remains
112 bytes with exactly eleven slots and no registration function.

DXVK `1cf7e631` and vkd3d `9a2716c0` now accept the exact package-owned module/
table construction edge, consume the sole A5 instance and direct GIPA, and carry
HRA1 through each resource allocation to the exact Mesa memory object. Generic
non-Helios construction retains its normal loader path; the selected Helios
paths do not enumerate modules, search `vulkan-1`, or create a second instance.
Mesa rejects missing, zero, duplicate, foreign-device, stale-generation,
overflowing, or out-of-range associations and removes each association before
allocation storage is reusable.

Root `0fe5677` owns the other half. Each D3D11/D3D12 device assigns monotonically
increasing, nonreused tokens in a bounded device-owned allocation set and tears
the reverse association down directly. D3D11 resolves each sealed token to the
exact allocation-list entry selected for that Render window, writes the current
HWA2 allocation generation, and refuses nonzero `byte_offset`. D3D12 resolves
the token to the exact current allocation GPUVA plus checked byte offset and
current HWA2 generation. Its old process-global `OnceLock`/`HashMap` identity
owner is gone. Neither side uses an engine pointer, KMT/NT handle, local HVM1
handle, Vulkan memory object, host resource ID, or fallback key as the token.

The same root commit consumes the A5 endpoint descriptors, creates HQA1 before
the runtime context callback, preserves bootstrap endpoint 1 and monotonic
nonreused real endpoints, and owns direct HQC1 scope lifetime. D3D11 assembles
complete HOB1 in the runtime-approved Render command/allocation windows. D3D12
uses its bounded GPUVA allocation state, emits complete HOB1 plus exactly 64
HOS1 bytes, and calls the owning ECL `pfnSubmitCommandCb` synchronously. Both
write `address_or_index`, `expected_allocation_generation`, and `identity_kind`
field by field; wire host-resource operand bytes remain zero. A scope becomes
COMMITTED only after the actual runtime callback accepts the batch and the
exact-context signal value is known. Failure closes it abandoned.

After that complete consumer closure existed, the generated payload inventory
proved a minimum KMD admission update was required. Root `eccfd19` generates and
validates only the needed A7 Venus opcode/typed-operand subset over existing
HNR2, retains immutable bounded custody on the exact session/endpoint/context,
patches only a KMD-private copy from live allocation objects, and feeds only the
actual terminal host result through K9. It introduces no new executor class,
wire field, host/QEMU/virglrenderer operation, compatibility carrier, synthetic
completion, polling, or independent timeline.

The corrective aggregate gate pass then exposed one real K4 defect in the first
HOC1 integration: OpenAllocation had attempted to repair an input generation.
Root `d75f649` removes that write. HOC1 obtains its exact KMD command-pool
snapshot only through the already-admitted WDDM 3.2 shared-backing callback,
retains a write-combined alias on the exact allocation object, and keeps
OpenAllocation const-only. The shared pointer is never an identity or lookup
key.

Mesa A7 `ae9c9f4c89d` completes the C60 classifier. Pure control uses the owned
HVC1 path; allocation-backed calls become immutable deferred records consumed
by the first exact outer batch naming all allocations; GPU-dependent calls join
the exact context first. Command buffers now produce bounded complete sealed-use
and typed-operand tables, including descriptor, buffer, image, query, and memory
lifetime/range closure. Unsupported opcode, incomplete closure, overflow,
foreign device, stale generation, and unclassified object remain named
refusals. The presentable-image tag and exact `PRESENT_SRC_KHR` /
`QUEUE_FAMILY_EXTERNAL` checks are complete, but the present layer remains
unwired.

Mesa A8/A9 `fa61439bfd7` makes `vkCreateRingMESA`, `vkNotifyRingMESA`, the ring/
virtqueue wait operations, shared head/tail state, and spin/sleep watchdogs
unreachable in the selected Windows backend. Reply stream setup and generation
are part of exact HNR2 COMMIT; ring zero remains finite and control-only; generic
non-Windows Venus is unchanged. The lower-ICD build is wired once and dependencies
used only by the retired Windows backend are removed. No present-layer source is
wired or activated.

Protocol/parity, KMD logic/integration, generated schema, direct-consumer,
token/lifetime, A7 classifier, A8/A9 ring-unreachability, source/mutation,
native, cross, and Windows compilation checks are source/build evidence only.
The corrective serial retirement suite passed completely. Windows KMD
compilation retained the exact 13-warning baseline and WDK binding SHA-256
`148B75DB41E093DC6783BE4F5BB3EA84B2C7C39EF316FE711B3F2A5A0668BEA2`;
the final Windows lower ICD was 49,244,922 bytes with SHA-256
`5AA0C1F43691B56D82CEC0B703B7D557054A71BF33C96746FE1DE9F72466F885`,
exactly four exports, and no forbidden loader or D3D/DXGI/DComp dependency.
Neither new KMD nor ICD was installed, no adapter was restarted, and no target
runtime probe was run. The last measured target therefore remains KMD
22.22.296.0 / `oem128.inf`, DWM on WARP, and zero active DisplayConfig paths.
The present-layer B lane is the exact handoff; broad HPS2 demolition and every
later unit remain outside this tranche.

**THE ESCAPE-FREE MESA A5-A9 LOWER-ICD CUTOVER LANDED AND PASSED SOURCE/BUILD
VALIDATION ONLY; IT WAS NOT INSTALLED OR EXERCISED ON THE TARGET, THE WDDM 3.2
DISPLAY PACKAGE REMAINS RUNTIME-UNADMITTED, AND THE PRESENT-LAYER CUTOVER, HPS2
RETIREMENT, AND PRODUCTION CORRECTNESS ARE NOT ESTABLISHED.**

## F22 — The F8-correct `VK_LAYER_HELIOS_present` B lane is source-closed as a separate artifact; no target admission was exercised.

**Landed source boundary, 2026-08-21.** The bounded Mesa B lane landed in the
required dependency order: B0 `bb7a787a5a1`, B1-B5 `e06abf3025f`, B6
`11d723ea10b`, B7/B8 `51054338012`, and B9 `871c62bf8a0`. No protocol, KMD,
UMD11, UMD12, DXVK, vkd3d, QEMU, virglrenderer, Looking Glass, packaging, Cargo
manifest, or lockfile changed.

The one bounded ordinary review landed `8b3c9359b5a`. It found and fixed two
in-scope issues: copying a feature-chain prefix now includes the loader's own
`VkLayerDeviceCreateInfo`, and every post-validation Present refusal writes all
per-swapchain `pResults`. No adversarial or rotating-lens review followed.

B0 removes `wsi_helios_present_sync.{c,h}` and the Helios additions that made
generic Mesa WSI publish HPS2 while preserving ordinary non-Windows WSI and the
Windows lower-ICD fail-closed boundary. B1-B4 were re-derived against the landed
A5-A9 contract: loader negotiation, next-chain dispatch provenance, extension
enumeration, copied create chains, lower-name filtering, Vulkan-1.3/external-
feature admission, hidden helper queue, exact surface profile, exact-LUID
adapter selection, D3D12 copy queue, DXGI flip swapchain, and committed shared
image graph remain owner-scoped. Translator-owned A5 instances never enter the
layer; no loader/module/filename search or compatibility registry was added.

**F8 is corrected in source.** Each slot creates lower-Vulkan-owned exportable
Ready and Release timeline semaphores. The layer calls
`vkGetSemaphoreWin32HandleKHR` for `D3D12_FENCE_BIT`, opens each handle through
the exact owning `ID3D12Device::OpenSharedHandle`, and closes the transient NT
handle on every success and refusal path. The live B lane contains no
Ready/Release `ID3D12Fence::CreateSharedHandle`, no
`vkImportSemaphoreWin32HandleKHR`, and no assumption that a D3D12-created fence
handle can enter the KMT/Vulkan open path. The valid F7 image direction remains:
committed D3D12 texture → resource NT handle → memory-handle properties →
dedicated import → offset-zero bind → next-GDPA private tag, all before image
exposure.

B6 owns the documented nine states and checked monotonic slot epoch. Acquire
selects only Release-proven reusable slots and uses `SetEventOnCompletion` for
finite waits; no poll/sleep/watchdog or synthetic completion exists. Present
uses the ordinary lower `vkQueueSubmit2` path for the canonical-to-EXTERNAL
barrier and exact Ready signal before D3D waits, copies the current backbuffer,
signals Release, and calls `Present(1,0)` with per-swapchain results.

B7/B8 retain exact instance/device/swapchain/slot associations and enforce the
alias VUIDs, NULL-swapchain pass-through, mixed-bind/status order, `ALIAS_ONLY`
lifetime, replacement-before-retirement, D3D/DXGI drain, final helper-queue
ownership restoration, retained backing, reverse teardown, and device-loss
mapping. B9 emits the layer as its own DLL/generated manifest. It does not enter
`libvulkan_wsi` or the lower ICD.

**Build provenance.** Focused source/mutation gates, the Linux-host cross build,
and the Windows `win_meson` build pass. The Windows lower ICD is 49,251,322
bytes with SHA-256
`6CFE645EDEC6BDE779300DAB4DC257F1092713EB9D06BCBA0A844AF683EC5D14` and
exports exactly `helios_icd_create_translator_v1`,
`vk_icdGetInstanceProcAddr`, `vk_icdGetPhysicalDeviceProcAddr`, and
`vk_icdNegotiateLoaderICDInterfaceVersion`. It imports none of DXGI, D3D11,
D3D12, DComp, or `vulkan-1.dll`.

The separate layer is 3,314,217 bytes with SHA-256
`0260E5AE530B4DAA0346F3AE2D4DAA53215E88C71486A8FFD57C12420CD73712` and
exports exactly the four layer-enumeration names, `vkGetInstanceProcAddr`,
`vkGetDeviceProcAddr`, `vkNegotiateLoaderLayerInterfaceVersion`, and
`vk_layerGetPhysicalDeviceProcAddr`. Its import table contains the permitted
`d3d12.dll` and `dxgi.dll` dependencies and no `vulkan-1.dll`; `dxguid` is a
static link input and therefore is not a DLL import.

The final serial `tools/retirement-gates.sh` run passed once. It included
protocol 151 unit tests plus integration/parity, kmd_logic 545 unit tests plus
both integrations, every standing gate, the updated A3/A6/A7 mutation suites
at 31/34/18, and 27 rejected present-layer mutations.

**Bound.** These facts establish source/build provenance only. The layer and
lower ICD were not installed or registered; no installer, registry, scheduled
task, adapter restart, reboot, VNC, cold-DWM exercise, or other target probe ran.
The installed target remains KMD 22.22.296.0 / `oem128.inf`, with DWM on WARP
and zero active DisplayConfig paths as the last measured state. The next owner
handoff is broad HPS2 demolition; it was not started.

**THE VK_LAYER_HELIOS_PRESENT B-LANE SOURCE CUTOVER LANDED AND PASSED
SOURCE/BUILD VALIDATION ONLY; IT WAS NOT REGISTERED, INSTALLED, OR EXERCISED
ON THE TARGET, THE WDDM 3.2 DISPLAY PACKAGE REMAINS RUNTIME-UNADMITTED, AND
BROAD HPS2 RETIREMENT AND PRODUCTION CORRECTNESS ARE NOT ESTABLISHED.**
