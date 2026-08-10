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

Ground-truth survey of the tree on 2026-08-10, before any code was written:

1. ⛔ **HWA2 has no create-*input* contract.** `HeliosWddmAllocationDescV2::validate`
   (`protocol/src/wddm.rs:751`) requires `allocation_generation != 0`
   (`:784-786`), so it can only ever validate the **output** side. Nothing in
   `protocol/` says what a UMD may legally *send* into `pfnAllocateCb`, and no
   validator enforces it. HVM1 (`Hvm1Stage::{CreateInput,CreateOutput}`,
   `native_render.rs:1830-1836`) and HOC1 (`validate_create_input` /
   `validate_create_output`, `wddm.rs:3305,3322`) both have the pair. HWA2 is
   the odd one out, and it is the record K4 is built on.
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
| `allocation_generation` | 16 | **KMD only** | `== 0` on input; `!= 0` on output. Identical to HOC1's rule (`wddm.rs:3308-3316,3324-3328`). |
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

**The rule.** `byte_size` is **UMD-supplied** and means *the exact extent the
resource requires*. The KMD:

* validates `byte_size != 0`, that every plane record is bounded inside it, and
  that `byte_size <= ` the backing extent the KMD is about to create;
* **refuses** the create if `byte_size` exceeds the backing — the safe
  direction, since it can never admit an over-bind;
* **never rewrites it**, exactly as §1.1 says of every non-KMD-owned field.

**The KMD's blob arithmetic stays KMD-private and is never exported.**
`linear_blob_size`'s two empirical constants (`NV_LINEAR_ROW_ALIGN`,
`NV_LINEAR_TAIL_SLACK`, `create_allocation.rs:392-400`) exist as *Xid-31
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

`HeliosVenusMemoryAllocationV1`'s whole impl is `new` / `new_reply_pool` /
`validate` (`native_render.rs:1906-2011`). It has no length-and-alignment
constructor, while HWA2 (`wddm.rs:715`) and HOC1 (`wddm.rs:3217`) both do, both
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
out-of-bounds **kernel** read.

---

## 3. DECISION — K4's file set is corrected

`lane-kmd-core.md:160` gives K4 `create_allocation.rs`, `kobj.rs`, `backing.rs`,
`tracking.rs`. Measured against the tree, two of those four are wrong.

| File | Brief says | Measured | K4 |
|---|---|---|---|
| `ddi/create_allocation.rs` | K4 owns | 3411 lines; **11 symbols consumed by `ddi/display.rs`** plus `build_paging_buffer.rs`, `cpu_host_aperture.rs`, `submit_command.rs`, `present_packet.rs` | **OWNS**, and it is added to `OWNERSHIP.md §1` as a single-owner file — it is the second most cross-lane-coupled file in the KMD after `lib.rs`. |
| `adapter/kobj.rs` | "KMD allocation-object state … where the allocation generation must live" | ⛔ **Zero allocation state.** Its own module doc (`kobj.rs:1-3`) says "the five embedded **kernel dispatcher objects**": the venus and scanout mutexes, the HPD worker's wake/exit events and thread, and the VSync heartbeat timer/DPC pair. `rg 'alloc\|generation\|HWA2\|HVM1\|segment' kobj.rs` matches only the `ExAllocateTimer` extern. "kobj" is *kernel object*. The brief read the filename. | **DROPPED.** Taking it would give K4 an exclusive lock on the VSync heartbeat and HPD worker — display-lane machinery the brief's own §2 disclaims — for zero benefit. `lane-kmd-core.md` §6 ambiguity 4 is withdrawn. |
| `adapter/backing.rs` | K4 owns | 18 call sites; **14 are outside K4**: `build_paging_buffer.rs` (K3) has 13, `display.rs` (display lane) 6, `create_allocation.rs` 1 | **NOT OWNED.** K4 removes its one call site (`create_allocation.rs:1836`). The file's deletion is re-sequenced to **K3**, with a display-lane cross-lane request. |
| `adapter/tracking.rs` | "MODIFY/DELETE — flag" | `VidMmTrackerTable`'s only consumer is `create_allocation.rs` (register `:2596`, matches `:2436`, remove `:1823`) plus three lines in `adapter/mod.rs` | **DELETE**, unambiguously. It is the adoption mechanism's attestation half and dies with it (§6). |

### 3.1 The allocation object needs a home, and it is a new file

There is today **no file that owns "the KMD allocation object"**:
`AllocationContext` / `OpenAllocationContext` are `Box::into_raw`'d inside
`create_allocation.rs` itself (`:2606`, `:3032-3047`). HWA2's
`allocation_generation` therefore has to be *created*, not moved.

⇒ **ADD `kmd_render/src/adapter/allocation_object.rs`** — the allocation
generation counter and its lifetime rules, with no other claimant. Do not
repurpose `kobj.rs`.

⚠ **HWA2 is const; the allocation object is not.** `AllocationContext` carries
21 mutable display fields (`vidpn_primary_*`, `vidpn_present_epoch`,
`vidpn_frame_watermark`, seven `vidpn_snap_*`, eight `scanout_copy_*`), each
with a written 0ab-B argument for living on the allocation rather than in a
global (`create_allocation.rs:96-133,949-963,1339-1352`). None of it can move
into HWA2 and none of it may be deleted by K4. The mutable object stays beside
the const descriptor.

---

## 4. DECISION — what K4 does about K2, which is blocked

K4 nominally depends on K2 (segments/placement). K2 is not merely unstarted: its
host counterpart is **parked** (`FINDINGS.md` F5 — `qemu-helios` reset to
`d4fde50ccb`, the HPM1 commits on branch `helios/hpm1-parked`), and §10.7
requires `DxgkDdiStartDevice` to negotiate HPM1 *before* `QUERYSEGMENT4` can
expose HLM1. So "K4 waits for K2" means "K4 waits indefinitely", which
contradicts the owner's sequencing decision that K4 goes first as the hub.

⇒ **K4 treats `HELIOS_SEGMENT_ID_HLM1` as the segment-id constant it records in
HVM1 placement, and does not depend on that segment existing yet.** Concretely:

* `Hvm1Role::placement()` already hardcodes `preferred_segment:
  HELIOS_SEGMENT_ID_HLM1` (`native_render.rs:1772`). K4 records it and
  validates against it.
* Role 4 (device-local) placement is **admitted and counted, not satisfied**:
  until K2 reports the HLM1 segment, a role-4 create returns the documented
  failure with a named counter, never a silent substitution onto the aperture
  segment. F2 measured that the segment *shape* is admitted by the OS
  (`BarSegFlags=0x02` starts `OK/CM_PROB_NONE`), which is why this is a
  sequencing gap and not a design one.
* K4 must **not** infer "K2 done" from "the segment shape is admitted". Get
  K2's end state in writing before wiring role-4 placement for real.

---

## 5. DECISION — the `resource_id` gap is named, not bridged

HWA2 deliberately carries **no host resource id** (`wddm.rs:355-361`, §10.3).
The retired `HeliosWddmOpenIdentity` carried one at offset 24 and it has four
readers: `umd/src/forward/state.rs:336-361`,
`umd12/src/forward12/resource12.rs:87-131`,
`icd/mesa/src/virtio/vulkan/vn_renderer_helios.c:146,255,349,3729,3848`, and the
KMD's own `read_alloc_identity` (`create_allocation.rs:1577`).

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

`icd/mesa/src/virtio/vulkan/vn_renderer_helios.c:229-267` hand-declares
`helios_wddm_alloc_private`, `helios_wddm_alloc_meta`,
`helios_wddm_open_identity` and `helios_wddm_external_private`, with
`_Static_assert(... == 48/48/48/96)` at `:345-351` and **no include of
`protocol/include/*.h` anywhere in the ICD** (`grep -rn
"helios_wddm.h\|protocol/include" icd/mesa/src icd/mesa/meson.build
icd/win-build` → empty). Live call sites: `:3374,3380` (TRACKING create),
`:3520,3530` (TRACKING validate), `:3613,3623` (DEVICE_MEMORY external create),
`:3848,3860` (open-identity read).

⇒ The moment `dxgkddi_create_allocation` stops accepting a 48-byte
`HeliosWddmAllocPrivate`, **every venus `vkAllocateMemory` on Helios fails**.
The KMD-side and ICD-side edits are one change. Added to `OWNERSHIP.md §2`.

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

⚠ Obligation 8 is the one with no prior art in the tree: `grep -rn Flags2
kmd_render/src/` is empty today. Three first-time union writes into a structure
whose layout only the VM's WDK can confirm.
