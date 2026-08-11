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

## F9 — K7's native-fence surface is four DDI slots, not a surface. Nine of its own symbols are unreachable, and `OWNERSHIP.md` §3's second activation gate is not met.

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
| `DXGKQAITYPE_NATIVE_FENCE_CAPS` (=37) arm | ⛔ **absent** — `grep -n 'NATIVE_FENCE_CAPS' kmd_render/src/ddi/query_adapter_info.rs` is empty; `fill_native_fence_caps` has no caller |
| `DXGK_FEATURE_NATIVE_FENCE` enablement | ⛔ **absent** — `query_feature_support` / `ensure_feature_admitted` have no caller |
| `DXGK_VIDSCHCAPS::NativeGpuFence=1`, `No64BitAtomics=0` | ⛔ not written |
| `DXGK_INTERRUPT_NATIVE_FENCE_SIGNALED` (=19) reporting | ⛔ **absent** — `signal_native_fence_signaled` / `notify_routine` have no caller |

### Why this is a sequencing fact and not a defect

The caps arm belongs to **K8** (`query_adapter_info.rs` end state) and the
interrupt to **K9** (`interrupt.rs`); both are unstarted, and both files are
owned by units other than K7. Writing the helper next to its subject and leaving
the call site to its owning unit is a legitimate choice. ⇒ Nothing here needs
fixing. What needs fixing is the **claim**.

⛔ **`OWNERSHIP.md` §3 gates the `SURFACE` flip on "the native-fence DDI surface
is complete".** It is not, and the phrase "the `kmd_render` native-fence surface
is WIRED IN" — true of the four slots and the slot audit — reads as though it
is. Whoever flips `SURFACE` must check the six rows above, not the sentence.

**Bound.** This says which symbols have no caller. It does not say the four
registered slots are wrong, and it does not re-open F6, which proved the slot
audit's refusal path on the target in both index and direction. The 22-warning
baseline was taken on a clean tree at `fb9b09a` and is the control arm for the
K4 changeset's own build.

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
