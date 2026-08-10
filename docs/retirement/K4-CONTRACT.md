# K4 — the allocation identity contract

Orchestrator decision, 2026-08-10. This file is **normative for the allocation
identity subsystem** and is the entry condition `METHOD.md` phase 1 requires
("the contract is written down") before any of K4 is authored. Where it
completes a gap the frozen reference leaves open it says so and gives the
argument; where it contradicts `lane-kmd-core.md` it wins, and the lane brief is
corrected in the same changeset.

Read with `FINDINGS.md` (measurements that supersede the reference) and
`OWNERSHIP.md` (who edits what).

---

## 0. Why this file exists: three holes recon found in the plan

Ground-truth survey of the tree on 2026-08-10, before any code was written.
⚠ **Every line citation in this section is anchored to the pre-change tree,
commit `befe8b1^`** — that is the point of a survey, and regenerating them
against today's tree would make the section describe holes that are closed.
Verify with `git show befe8b1^:protocol/src/wddm.rs`; the current locations are
given beside each.

1. ⛔ **HWA2 has no create-*input* contract.** `HeliosWddmAllocationDescV2::validate`
   (`protocol/src/wddm.rs:751` @ `befe8b1^`; now `:1209`) required
   `allocation_generation != 0`, so it could only ever validate the **output**
   side. Nothing in `protocol/` said what a UMD may legally *send* into
   `pfnAllocateCb`, and no validator enforced it. HVM1
   (`Hvm1Stage::{CreateInput,CreateOutput}`, `native_render.rs:1830-1836` @
   `befe8b1^`; now `:1832`) and HOC1 (`validate_create_input` /
   `validate_create_output`, `wddm.rs:3305,3322` @ `befe8b1^`; now
   `:3539,3556`) both had the pair. HWA2 was the odd one out, and it is the
   record K4 is built on. **Closed by §1**: `Hwa2Stage` at `wddm.rs:586`, the
   two validators at `:1224`/`:1238`, sharing the cross-field core that carries
   the stage rules at `:917-924` and `:958`.
2. ⛔ **The geometry HWA2 requires has no kernel-side source.** Offsets 32–60 are
   width/height/depth/mips/DXGI format/D3DDDI format/sample count/quality, and
   the cross-field rules hard-fail an image kind with zero geometry. For an OS
   *standard* allocation dxgkrnl supplies `D3DKMDT_STANDARDALLOCATION`; for an
   ordinary UMD texture it supplies nothing. Today that geometry reaches the
   kernel **only** through the UMD's retired `HeliosWddmAllocMeta` trailer
   (`umd/src/forward/resource.rs:302-324`,
   `umd12/src/forward12/resource12.rs:1839-1866`).
3. ⛔ **`protocol/src/wddm_legacy.rs:30` is false.** It says `GlobalVidMmTracker`
   was "folded into HWA2's own tracking-kind fields". `grep -in "track"
   protocol/src/wddm.rs` returns zero hits: there is no tracking kind, no
   cookie, no global-share field, no tracker flag bit. The mechanism has **no
   successor** — it dies with UMD-backing adoption.

§1 closes hole 1 and 2. §6 closes hole 3.

---

## 1. DECISION — HWA2 is a two-stage record, exactly like HVM1 and HOC1

The reference says two things that are individually clear and jointly
incomplete (§10.3, `HELIOS_PRESENT_SYNC_RETIREMENT.md:1030-1042`):

> "At create time it [KMD] writes a versioned, immutable descriptor into the
> `[in/out]` allocation-private buffer: package generation, allocation
> generation, allocation kind, size, format, plane/layout, memory/sharing
> flags, and reserved/version fields."

> "The creator allocates the complete buffer, KMD writes it only on create, and
> every opener treats it as const."

The buffer is `[in/out]`. The KMD performs the write. The KMD cannot invent
texel dimensions. The only completion consistent with all three is:

> **The UMD supplies a create-input HWA2. The KMD validates it in full, then
> writes the complete 168-byte output descriptor — echoing every field it
> validated and stamping the fields only the kernel can know.**

"KMD writes it only on create" stays literally true: the kernel performs the
write of all 168 bytes, and no opener ever writes any of them. The input is a
*request* whose acceptance is total — a rejected request creates nothing.

This is not a new pattern. It is the pattern `protocol/` already implements
twice, and adopting it for the third record removes an inconsistency rather
than adding a mechanism.

### 1.1 The field partition

| Fields | Offsets | Written by | Rule |
|---|---|---|---|
| `magic`, `abi_version`, `struct_size`, `package_generation` | 0–15 | **UMD**, validated by KMD, echoed | Exact on **both** stages. A UMD that cannot stamp the package generation has no business creating an allocation. |
| `allocation_generation` | 16 | **KMD only** | `== 0` on input; `!= 0` on output. Identical to HOC1's rule (`wddm.rs:3539-3552` / `:3556-3566`; cited as `:3308-3316,3324-3328` at survey time). |
| `flags & DIRECT_FLIP_COMPATIBLE` | 68 | **KMD only** | `== 0` on input. §10.3:1090-1096 — "KMD sets `DIRECT_FLIP_COMPATIBLE` only when the exact allocation is a non-protected, non-cross-adapter managed primary in a swizzle/layout class the selected display backend implements." An opener never infers it. |
| `flags & D3D12_RUNTIME_PRIMARY` | 68 | **KMD only** | `== 0` on input. §10.3:1086-1090 — set only when the D3D12 create record, the runtime `PRIMARY` flag, and `D3DDDI_ID_UNINITIALIZED` all agree; "the bit and sentinel are cross-validated and neither is inferred by an opener." |
| everything else | 24–63, 64–103, 104–167 | **UMD**, validated by KMD, echoed verbatim | The KMD refuses the create rather than correcting a field. Silent correction would make the descriptor disagree with the resource the UMD believes it made. |

⇒ Two new functions in `protocol/src/wddm.rs`, mirroring HOC1's naming
exactly so a reader who knows one knows the other:

```rust
pub enum Hwa2Stage { CreateInput, CreateOutput }

impl HeliosWddmAllocationDescV2 {
    pub fn validate_create_input(&self, package_generation: u64)
        -> Result<(), HeliosAllocDescRejection>;
    pub fn validate_create_output(&self, package_generation: u64)
        -> Result<(), HeliosAllocDescRejection>;
}
```

`validate()` keeps its current meaning — the **open-time / output** validator —
and the three entry points share one cross-field core so a rule can never be
enforced on one path and not another. New rejection variants are append-only:
`AllocationGenerationNonZeroOnInput`, `KmdOwnedFlagSetOnInput { bits }`.

### 1.2 What this does NOT authorise

* It does **not** add a field to HWA2. The 168-byte table and every offset
  assertion are unchanged; this is validation, not layout.
* It does **not** make the input a different record. The creator allocates
  exactly `HELIOS_HWA2_BYTES`, which is what §10.3 already says.
* It does **not** create a fallback. A descriptor that fails either stage fails
  the create; nothing selects a legacy parser (§10.3:1104-1107).

### 1.3 RULING — `byte_size` is the RESOURCE's extent, not the blob's

⭐ Amended 2026-08-10 after the three producers were authored in parallel and
**each guessed a different rule** — which is precisely why this had to become a
ruling rather than stay an open item:

| producer | what it assumed |
|---|---|
| `umd` (D3D11) | sends `max(pitch * height, 4096)` |
| `icd/mesa` | reads it as `byte_size >= effective_size` |
| `kmd_render` | `linear_blob_size(pitch, height)` = `pitch * round_up(height, 128) + 65536`, page-floored — **strictly larger than the D3D11 producer's number** |

⛔ Had the KMD demanded equality, **every D3D11 create would have failed**, and
it would have failed on the target with no Linux build able to see it.

**The rule, in two tiers**, split by **who authored the descriptor** — which is
the distinction the first draft of this ruling missed, at the cost of a blocker
(below).

* **Tier 1 — a `STANDARD` allocation, i.e. one the KMD authored itself** in
  `dxgkddi_get_standard_allocation_driver_data`. Two steps, in order:
  1. **Validate** exact equality with `linear_blob_size(plane0.row_pitch,
     height)` for the `LINEAR` swizzle class. This is not a demand on any UMD —
     it is the KMD checking that its **own** bytes came back unmodified. A
     mismatch is `AcSize` / `STATUS_INVALID_PARAMETER`.
  2. ⭐ **Then ADOPT the host's answer.** Once the backing exists, if it is
     `BackingSize::HostAuthoritative`, the KMD **overwrites** `byte_size` — and
     the plane record's offset/pitch — with the measured extent and Vulkan's
     real stride. §1.1's echo rule exists to protect a claim a *UMD* made; on
     this arm there is no UMD, and the KMD is replacing its own **pre-create
     estimate** with a measurement.
* **Tier 2 — every descriptor this driver did NOT author:** `byte_size != 0`,
  every plane record bounded inside it, and `byte_size <= ` the backing extent
  the KMD created. Exceeding the backing is refused; the reverse is admitted.
  Nothing is rewritten here — the UMD's claim stands or the create fails.
  §10.3 gives no computation rule for an ordinary UMD buffer or image, and
  **inventing one would refuse legal creates** — the trap the table above
  describes.

⛔ **Why Tier 1 needs step 2, recorded because the first draft got it wrong and
the review caught it as a blocker.** The KMD's estimate is *deliberately larger*
than the resource (128-row round-up plus a 64 KiB tail slack, both Xid-31
protection), while the `LinearScanoutImage` backing is sized by the **host**, and
the host's number is smaller: measured on the live desktop, `SdgLReq=7910400
SdgLPch=7680` gives `venus_alloc_size = 7_913_472` against
`linear_blob_size(7680,1030) = 8_912_896`. Enforcing the estimate therefore
refused **every OS shared primary** — no primary, no composited desktop — and
four independent review lenses converged on it. The pre-retirement code did
exactly what step 2 restores (`41d13f8:create_allocation.rs:2427`, `ap.size =
created.blob_size.bytes()`).

⛔ The undersize direction is the one that must be acted on for Tier 2, and it
is: a blob smaller than the image requirement binds "successfully" and then
MMU-faults when the sampler reads the slack region (host Xid 31, `FAULT_PTE
VIRT_READ`, killed the IDD feed live 2026-07-04). The oversize direction is only
*counted* (`LINEAR_BLOB_SIZE_DIVERGENCE`), because it measures how far the
empirical constants are from the host's real Vulkan requirement without acting
on a number nobody has justified acting on.

**The KMD's blob arithmetic stays KMD-private and is never exported.**
`linear_blob_size`'s two empirical constants (`NV_LINEAR_ROW_ALIGN`,
`NV_LINEAR_TAIL_SLACK`, both consumed by `fn linear_blob_size` in
`create_allocation.rs` — `rg -n 'NV_LINEAR_ROW_ALIGN|NV_LINEAR_TAIL_SLACK|fn
linear_blob_size' kmd_render/src/ddi/create_allocation.rs`; the survey's
`:392-400` no longer resolves) exist as *Xid-31
undersize protection* — the blob is deliberately larger than the resource. That
slack is a property of the backing, not of the resource's identity, and putting
it in a descriptor every opener reads would export a private hardware
work-around into the wire ABI. Read §10.3's "exact backing extent" as *the exact
extent of this resource's backing*, which the creator states and the kernel
honours.

⇒ The rejected alternative, recorded so it is not re-proposed: exporting the two
constants through `protocol/` so the UMD computes the identical number. It
couples every producer to an NVIDIA-specific measurement, and it makes a change
to the slack an ABI break.

⚠ **Consequence for the ICD**, which must be revisited when its import is
un-blocked (§5): under this rule `byte_size` may be *smaller* than a
page-rounded Vulkan `allocationSize`, so a `>=` comparison against
`allocationSize` is the wrong test. The ICD's import is a counted refusal today
so nothing depends on it yet, and the comparison carries a comment saying to
re-derive it against this ruling.

---

## 2. DECISION — HVM1 gets `from_private_data`

`HeliosVenusMemoryAllocationV1`'s whole impl **was** `new` / `new_reply_pool` /
`validate` (`native_render.rs:1906-2011` @ `befe8b1^`). It had no
length-and-alignment constructor, while HWA2 (`wddm.rs:715` @ `befe8b1^`; now
`:829`) and HOC1 (`wddm.rs:3217` @ `befe8b1^`; now `:3451`) both did, both
carrying the identical reason:

> "the runtime's buffer carries no alignment promise, and a KMD that read 168
> bytes out of a shorter one would take an out-of-bounds kernel read this crate
> could not catch. Every consumer must enter through here rather than casting
> the pointer."

K4 parses all three records out of the same `(pPrivateDriverData,
PrivateDriverDataSize)` pair. ⇒ HVM1 gains `from_private_data(&[u8]) ->
Result<Self, Hvm1Reject>` with the same bounds/alignment discipline and the
same doc reason. This is the single highest-value protocol gap recon found: it
is the one missing check that stands between a malformed user buffer and an
out-of-bounds **kernel** read. **Landed** at `native_render.rs:1963`, inside an
impl that now spans `:1912-2044`.

---

## 3. DECISION — K4's file set is corrected

`lane-kmd-core.md:160` gives K4 `create_allocation.rs`, `kobj.rs`, `backing.rs`,
`tracking.rs`. Measured against the tree, two of those four are wrong.

| File | Brief says | Measured | K4 |
|---|---|---|---|
| `ddi/create_allocation.rs` | K4 owns | **4585 lines** (3411 when surveyed — K4 added ~1170); **10 symbols consumed by `ddi/display.rs`** (an 11th name, `SCANOUT_ALLOCS`, appears only in a comment at `display.rs:862`) plus `build_paging_buffer.rs`, `cpu_host_aperture.rs`, `submit_command.rs`, `present_packet.rs` | **OWNS**, and it is added to `OWNERSHIP.md §1` as a single-owner file — it is the second most cross-lane-coupled file in the KMD after `lib.rs`. The exact symbol list and its line cites live in `OWNERSHIP.md §1`; re-measure with `rg -n 'create_allocation::' kmd_render/src/` rather than trusting either copy. |
| `adapter/kobj.rs` | "KMD allocation-object state … where the allocation generation must live" | ⛔ **Zero allocation state.** Its own module doc (`kobj.rs:1-3`) says "the five embedded **kernel dispatcher objects**": the venus and scanout mutexes, the HPD worker's wake/exit events and thread, and the VSync heartbeat timer/DPC pair. `rg 'alloc\|generation\|HWA2\|HVM1\|segment' kobj.rs` matches only the `ExAllocateTimer` extern. "kobj" is *kernel object*. The brief read the filename. | **DROPPED.** Taking it would give K4 an exclusive lock on the VSync heartbeat and HPD worker — display-lane machinery the brief's own §2 disclaims — for zero benefit. `lane-kmd-core.md` §6 ambiguity 4 is withdrawn. |
| `adapter/backing.rs` | K4 owns | ⭐ **Re-measured after the change** (`rg -n 'system_backings\|SystemBackingTable\|SystemBackingSnapshot' kmd_render/src/` → 22 hits, 6 of them the declarations inside `backing.rs`): **every one of the 16 remaining references is outside K4** — `build_paging_buffer.rs` (K3) 10, `adapter/mod.rs` 3 (re-export `:32`, field `:499`, init `:1066`), `display.rs` (display lane) 2 (`:632`, `:702`), and `create_allocation.rs` 1 — which is now a **tombstone comment** reading "`adapter.system_backings.remove(ctx.resource_id)` was here". The survey's "18 call sites, 14 outside K4 (13/6/1)" was both stale and self-inconsistent (13+6+1=20). ⚠ Line numbers are deliberately omitted for `create_allocation.rs`: it was being edited while this row was written, and a cite into a file in flight is stale before it is read. Re-run the `rg` above. | **NOT OWNED, and K4's obligation here is DONE** — its one call site is removed. The file's deletion is re-sequenced to **K3**, with a display-lane cross-lane request. |
| `adapter/tracking.rs` | "MODIFY/DELETE — flag" | *As surveyed:* `VidMmTrackerTable`'s only consumer was `create_allocation.rs` (register `:2596`, matches `:2436`, remove `:1823`) plus three lines in `adapter/mod.rs`. **Now: `kmd_render/src/adapter/tracking.rs` does not exist**, and the §8.6 gate in `tools/retirement-gates.sh` proves the retired identity symbols have **zero live occurrences** across every consuming root — the gate prints its own symbol/root/tombstone counts and is the place to read them, because they move whenever the gate is strengthened. | **DELETED**, done. It was the adoption mechanism's attestation half and died with it (§6). |

### 3.1 The allocation object needs a home, and it is a new file

There was, at survey time, **no file that owned "the KMD allocation object"**:
`AllocationContext` / `OpenAllocationContext` are still `Box::into_raw`'d inside
`create_allocation.rs` itself, at three sites (`grep -n 'Box::into_raw'
kmd_render/src/ddi/create_allocation.rs`; the survey's `:2606`, `:3032-3047` no
longer resolve). HWA2's `allocation_generation` therefore had to be *created*,
not moved.

⇒ **ADD `kmd_render/src/adapter/allocation_object.rs`** — the allocation
generation counter and its lifetime rules, with no other claimant. Do not
repurpose `kobj.rs`. **Landed**: 241 lines, `mint()` / `epoch_of()` /
`is_current()` / `invalidate_all()` / `minted_count()` plus the
`GENERATION_EXHAUSTED` and `GENERATION_EPOCH_BUMPS` statics.

⚠ **HWA2 is const; the allocation object is not.** ⭐ **Recounted 2026-08-10
against the whole `struct AllocationContext` field list in
`create_allocation.rs` — 43 fields: the mutable display subset is 22, not 21,
and the
`scanout_copy_*` group is ten, not eight** — three `vidpn_primary_*`,
`vidpn_present_epoch`, `vidpn_frame_watermark`, seven `vidpn_snap_*`, and ten
`scanout_copy_*` (`image_id`, `memory_id`, `conversion_image_id`,
`conversion_memory_id`, `conversion_init_pool_id`, `pool_id`,
`command_buffer_id`, `target_image_id`, `last_fence`, `owns_source_alias`).
Each carries a written 0ab-B argument for living on the allocation rather than
in a global. None of it can move into HWA2 and none of it may be deleted by K4.
The mutable object stays beside the const descriptor.

---

## 4. DECISION — what K4 does about K2, which is RESCOPED, not blocked

⛔ **CORRECTED 2026-08-10 (owner). An earlier draft of this section said K2's
host counterpart is "parked" and therefore "K4 waits indefinitely". That is
wrong, and it is exactly the misreading `FINDINGS.md` F5 exists to prevent** —
F5's own Consequence paragraph ends: *"The KMD memory lane no longer has a QEMU
dependency, and **no lane in flight has one**."*

HPM1 was not deferred; it was **declined**. F5's reasoning is maintenance: ~4,500
lines concentrated in one upstream QEMU file is the divergence that makes every
rebase expensive, and *"the scanout commits are light and cherry-pickable; this
one is not."* Introducing a new protocol into a stack we do not own is an
ongoing cost, and the owner's standing position is that the work belongs on the
KMD side instead. So:

* **K2 is rescoped, not blocked.** Its HPM1-negotiation half is gone by design —
  there is no host counterpart and there should not be one. Its BAR admission
  and immutable two-segment table are pure guest-side KMD work and were never
  blocked by anything.
* **§10.7's ordering rule is superseded, not pending.** "Before `QUERYSEGMENT4`
  can expose HLM1, `DxgkDdiStartDevice` must negotiate HPM1" is a precondition
  that existed *only because HPM1 existed*. F5 revises §10.7; it does not wait
  on it.
* **F2 already measured that the segment shape needs no negotiation at all**:
  `BarSegFlags=0x02` starts `OK / CM_PROB_NONE` and `helios_paintcap` shows a
  fully composited live desktop.
* The same correction applies to **K3**: the paging DMA is KMD-side encoding,
  not work waiting on a host executor.

⇒ Nothing below changes as a result — the rule was already written not to depend
on K2's schedule, which is why it survives the correction. **K4 treats
`HELIOS_SEGMENT_ID_HLM1` as the segment-id constant it records in HVM1
placement, and asks at runtime whether that segment is reported.** Concretely:

* `Hvm1Role::placement()` already hardcodes `preferred_segment:
  HELIOS_SEGMENT_ID_HLM1` (`native_render.rs:1772`). K4 records it and
  validates against it.
* ⭐ **RULING, amended 2026-08-10 — check the segment, do not hardcode the
  role.** The first draft of this clause said "role 4 is admitted and counted,
  not satisfied", and the review found that both readings built on it are wrong:
  the KMD refused *only* role 4 while admitting roles 1–3 onto
  `HELIOS_SEGMENT_ID_HLM1`, and the `kmd_logic` model refused *all four*.
  `Hvm1Role::placement()` (`native_render.rs:1763-1775`) hardcodes that segment
  for **every** role, so a role-1..3 create is handed a `preferred_segment` the
  segment table may not report — and is then refused **by dxgkrnl, outside the
  driver, with no Helios counter at all**. That is precisely what "every
  refused path gets a named counter" exists to prevent, and it is invisible in
  exactly the way this project has been burned by before.

  ⇒ The rule is neither "role 4" nor "all roles". The KMD **verifies that the
  role's `preferred_segment` is actually present in the segment table it
  reports**, and refuses with a named per-role counter when it is not. That is
  correct under both K2 states, it needs no knowledge of when K2 lands, and it
  cannot go stale — whereas a hardcoded role number is a claim about K2's
  schedule embedded in kernel code.

  ⚠ Do not assume the answer either way before running it: F2 measured that the
  HLM1 *flag shape* is already admitted by the OS (`BarSegFlags=0x02` starts
  `OK/CM_PROB_NONE` with a fully composited desktop), so segment id 2 may well
  be reported **today**, which would make some roles satisfiable before K2 lands
  at all. The check answers that at runtime; a constant cannot.
* K4 must **not** infer "K2 done" from "the segment shape is admitted". Get
  K2's end state in writing before wiring role-4 placement for real.

---

## 5. DECISION — the `resource_id` gap is named, not bridged

HWA2 deliberately carries **no host resource id** (`wddm.rs:393-397`, §10.3 —
"No host resource token, `resid`, PID, process handle, synchronization object,
mutable value, or independently usable identity"; the line cite was `:355-361`
at survey time and the block has moved, not changed). The retired
`HeliosWddmOpenIdentity` carried one at offset 24 and it **had** four readers —
`umd/src/forward/state.rs`, `umd12/src/forward12/resource12.rs`,
`icd/mesa/src/virtio/vulkan/vn_renderer_helios.c`, and the KMD's own
`read_alloc_identity` — all four of which K4 has now re-pointed or deleted; the
KMD's survives only as a tombstone comment in `create_allocation.rs` (`grep -n
read_alloc_identity kmd_render/src/ddi/create_allocation.rs`; the survey's
`:1577` no longer resolves).

The ICD's use is not decorative: `identity.resource_id` is passed straight into
`VkImportMemoryResourceInfoMESA::resourceId` and
`vn_renderer_bo_create_from_resource_id` (`vn_device_memory.c:267-277`) — it is
how the guest names the host object. The replacement is not a different field;
it is a different **mechanism**: the ICD stops naming host resources at all and
the KMD patches the host resid in from `HeliosNativeRenderPatch`
(`native_render.rs:622-642`, doc `:1799-1803`). That is mesa lane unit **A3**
(XL), plus K6.

⇒ **The "re-point the ICD from the 48-byte open-identity blob to HWA2" task is
NOT a substitution**, and must not be planned as one. K4's obligation is
exactly this:

* re-point every reader that consumes a field HWA2 *does* carry;
* make every reader of a field HWA2 does **not** carry fail **loudly, with a
  named counter naming A3**, never fall back and never fabricate;
* record that an ICD in this state cannot import, and that this is the
  retirement's intended intermediate state rather than a regression.

### 5.1 ⭐ Exactly which allocation paths refuse — "every `vkAllocateMemory`" is too broad

This document, `ROADMAP.md`, and `icd/mesa`'s own commit message all said "every
venus `vkAllocateMemory` on Helios fails until A3". **Measured against the
landed code, that is stated more broadly than it supports.** The commit message
is immutable and stays wrong; this is the correction of record.

`vn_AllocateMemory` (`vn_device_memory.c:661`) dispatches five ways. **Two
refuse:**

| path | where | counter | result |
|---|---|---|---|
| an allocation carrying `VkImportMemoryWin32HandleInfoKHR` | `vn_device_memory_import_win32` (`vn_device_memory.c:552`), *after* the NT handle opens and the HWA2 payload parses and validates as create-OUTPUT | `HELIOS_A3_GAP_IMPORT_WIN32` (`:597`) | `VK_ERROR_INVALID_EXTERNAL_HANDLE` |
| an allocation whose `export_handle_types` includes `VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_WIN32_BIT` | `vn_renderer_helios_external_memory_create` (`vn_renderer_helios.c:3365`), refused **before** `D3DKMTCreateAllocation2` so nothing is half-transferred, and after building + validating the exact HWA2 create input A3 will send | `HELIOS_A3_GAP_EXPORT_ADOPTION` (`:3421`) | `VK_ERROR_INVALID_EXTERNAL_HANDLE`, unwound by `vn_device_memory_unwind_external` |

**Three still succeed**, untouched: the plain arm (`vn_device_memory_alloc` →
`_alloc_simple` / `_alloc_guest_vram` / `_alloc_export` for non-Win32 handle
types), the `VkImportMemoryResourceInfoMESA` resource-id import, and the
`VkImportMemoryFdInfoKHR` dma-buf import. A third A3 refusal,
`HELIOS_A3_GAP_HANDLE_PROPERTIES` (`vn_device_memory.c:905`), is in
`vkGetMemoryWin32HandlePropertiesKHR` and is not an allocation at all.

⇒ The honest statement is **"every Win32 shared-memory import and export
refuses; ordinary venus allocation still works."** That is still enough to take
the desktop down — DWM's shared surfaces and the D3D11/D3D12 interop path are
exactly the OPAQUE_WIN32 traffic — but "every `vkAllocateMemory` fails" would
predict that a plain venus render allocation fails too, and it does not. Anyone
diagnosing a post-deploy failure needs the difference.

⚠ Second casualty, same shape: `identity.memory_type_index`. HWA2's
`memory_class` is a 3-value protocol enum and its doc is explicit — "no Vulkan
memory-type index. ⛔ The retired trailer carried `memory_type_index`; it is
gone."

---

## 6. DECISION — the VidMm tracker has no successor

`wddm_legacy.rs:30` is corrected to say so. `GlobalVidMmTracker`, the
`HELIOS_WDDM_ALLOC_KIND_TRACKING` kind, `adapter/tracking.rs::VidMmTrackerTable`
and the create-time attestation in `create_allocation.rs` are **all deleted
together** with UMD-backing adoption. §10.3's "no … independently usable
identity" forbids reintroducing the mechanism under another name. Nothing is
"folded into" HWA2 and nobody should go looking for it.

---

## 7. The cross-repo atomic pair `OWNERSHIP.md §2` is missing

§2 names two atomic pairs. **This is a third, and K4 sits on it.**

*As surveyed (pre-change):* `vn_renderer_helios.c` hand-declared
`helios_wddm_alloc_private`, `helios_wddm_alloc_meta`,
`helios_wddm_open_identity` and `helios_wddm_external_private`, guarded only by
`_Static_assert(... == 48/48/48/96)` — size, not offsets — with no include of
`protocol/include/*.h` anywhere in the ICD.

⭐ **Closed 2026-08-10 by `icd/mesa` HEAD `23ab160`, and the layout half is now
a compile-time gate, not a paragraph.** All four local declarations are deleted;
`vn_helios_hwa2.h:52` includes `helios_wddm.h`, so the header's 82 `offsetof`
assertions (25 of them on `HeliosWddmAllocationDescV2`) fire inside the ICD's
own TUs; and `icd/mesa/src/virtio/vulkan/meson.build:163-171` `error()`s if that
header is not reachable. See `OWNERSHIP.md` §2 pair 3 for the full measurement.

**What is still one change**, and is the reason this stays an atomic pair:
`vn_helios_hwa2.c` transcribes the *rules* — `from_private_data` and the two
stage validators — from `protocol/src/wddm.rs` by hand, and nothing checks that
transcription (`vn_helios_hwa2.c:17-20`). A rule added on one side without the
other is still silent.

⇒ The consequence of the flip, stated precisely (see §5 and the note there on
scope): **two `vkAllocateMemory` paths refuse, not all of them.**

---

## 8. Acceptance for K4

Not "it compiles". Per obligation, one of: implemented and forwarding; refused
with a named counter and a documented `NTSTATUS`; or explicitly recorded as
unreachable with the argument. ⛔ "Implemented but never exercised" is a third
state and must be reported as such (`METHOD.md` phase 1).

| # | Obligation | Where it is proven |
|---|---|---|
| 1 | Every create parses HWA2 through `from_private_data` — never a pointer cast — and validates `CreateInput` | `kmd_logic` model + KMD code review |
| 2 | HVM1 is parsed through `from_private_data` (§2) and admitted per role 1–4 with the exact flag sets | `kmd_logic` |
| 3 | HOC1 create is admitted through `validate_create_input`/`_output` | `kmd_logic` |
| 4 | The KMD writes a nonzero `allocation_generation` exactly once per allocation, at create | `kmd_logic` lifecycle model |
| 5 | `DxgkDdiOpenAllocation` writes **no** byte of the private buffer | grep gate: no write path reachable from open |
| 6 | UMD-backing adoption, the open-time restamp, `GlobalVidMmTracker` and `VidMmTrackerTable` are gone | grep gate in `tools/retirement-gates.sh` |
| 7 | Every refusal increments a named counter and returns a documented `NTSTATUS` from the DDI's legal set | KMD code review |
| 8 | `DXGK_ALLOCATIONINFO::Flags2` carries `DisablePartialResidency`, `RestrictedToSingleSegment` and `ExplicitResidencyNotification` per §10.7:2003-2011 | ⚠ **first-time union writes with zero field evidence** — treat as unexercised until measured on the target |
| 9 | The `resource_id` gap refuses loudly and names A3 (§5) | ICD counter |

⚠ Obligation 8 is the one with no prior art in the tree: **`grep -rn Flags2
kmd_render/src/` was empty when this contract was written**, which is what made
it three first-time union writes into a structure whose layout only the VM's WDK
can confirm. It is no longer empty — K4 itself put the hits there (`grep -rn
Flags2 kmd_render/src/`; two `set_` calls, `DisablePartialResidency` and
`RestrictedToSingleSegment`, plus a `diag::record` of the whole word), and
`set_ExplicitResidencyNotification` is written on the **WDDM2_0** flags word
rather than on `Flags2` — so obligation 8's third bit is not a `Flags2` write at
all, and grepping `Flags2` alone will not find it. The prior art is now K4's own, which is not
independent evidence, so the ⚠ stands unchanged: **treat as unexercised until
measured on the target.** The code says so at the write site too — `Flags2` is
a WDDM-3.2 field and `wddm_surface.rs` still declares `Wddm2_1GpuMmu`, so these
bits are **inert** until the activation commit (`OWNERSHIP.md` §3), and an
unchanged residency trace is therefore not evidence the writes are missing.
