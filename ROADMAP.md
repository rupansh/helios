# ROADMAP — Stage: Correctness and D3D12 (since 2026-08-05)

*The desktop first rendered end-to-end on 2026-07-05. The active architecture
changed on 2026-07-09: Helios is now a WDDM render+display adapter and owns the
virtio-gpu scanout; IddCx/Looking Glass is no longer the active display path.*

## ⭐⭐⭐⭐⭐ 2026-08-29 ROOT CAUSE FOUND — the application's map pointer is PROCESS HEAP

⛔ **This supersedes every theory below it, including "F16 / Lock2 is a copy",
"premature completion" and "the host never executes".** All three were looking
for a subtle divergence between two views of one buffer. There is no divergence
to find: **there are two different buffers and nothing ever connected them.**

### The defect, in three lines

* Every CPU-mappable D3D11 resource gets a WDDM allocation from the **UMD**,
  which hands it `pSystemMem = CpuBacking::new(bytes)` — a plain
  `alloc_zeroed` process-heap buffer (`umd_common/src/cpu_backing.rs`) — and
  publishes that same pointer as `HeliosResourceAssociationV1::cpu_mapping`
  (`umd/src/forward/resource.rs:920-937`, `:595-610`).
* The ICD, in record-only mode, returns that pointer **verbatim** as the
  application's Vulkan mapping: `vn_MapMemory2` →
  `*ppData = mem->helios_outer.cpu_mapping + offset`
  (`icd/mesa/src/virtio/vulkan/vn_device_memory.c:1619-1631`).
* The KMD backs the same allocation with a **host-side** venus
  `VkDeviceMemory` (`allocate_memory_blob`, the `Hwa2Backing::LinearMemory`
  arm, `create_allocation.rs:3200-3240`) and deliberately asks for **no** OS
  shared backing: `HWA2_SHARE_BACKING_STORE_WITH_KMD = false`, asserted at
  compile time (`create_allocation.rs:3341-3348`).

⇒ The GPU reads and writes host memory. The application reads and writes its
own heap. Neither is a view of the other, so **every GPU-routed read-back is an
untouched page and every CPU upload is invisible to the host.**

### Measured, three independent ways

| instrument | reading |
|---|---|
| `tools/d3d11_hostram_alias_probe.cpp` — `VirtualQuery` on the pointer `Map()` returned for a 4 MiB staging texture | `type=0x20000` **MEM_PRIVATE**, `state=MEM_COMMIT`, region 4,198,400 B. Ordinary committed process heap — not a section, not a WDDM view |
| QEMU `virtio_gpu_virgl_guest_blob_backing` over a whole probe run | **2** guest-backed blobs, both accounted for (the `HVR1` reply pool — its ASCII magic is readable at its first GPA — and one shmem). **None is the texture.** The 3 remaining 4 MiB blobs are `host3d_blob_charge`, i.e. host memory |
| the probe itself | stamps all 1,048,576 dwords with a self-naming pattern, GPU-clears an RT and `CopyResource`s onto it: `P3 AFTER_COPY stamped=1048576 cleared=0 zero=0 other=0` — the copy touches nothing |

The stamp pattern is `0xC0DE<low 16 bits of its own dword index>`, so a dword
read from guest RAM names its own offset. QMP `xp/4xw <gpa>` on every guest
blob created during the run, sampled every 4 s for 100 s, never showed one.

⭐ **The observer that settled it is outside the whole stack**: QEMU's own view
of guest physical memory, via QMP `human-monitor-command` `xp`, with the GPAs
taken from the already-enabled `virtio_gpu_virgl_guest_blob_backing` trace. It
is neither a query, nor mapped memory, nor a WARP control, so none of the three
lying instruments applies to it. No relaunch and no owner gate: read-only QMP on
the running VM.

### Why every earlier reading was consistent with this

* CPU-only round trips pass (`1 CPU match=4096/4096`) — they never leave the
  heap buffer.
* The poison survives every GPU route, and a +1 s re-read does not help — the
  GPU was never going to touch that page, at any time.
* The host GPU really does rasterise at 42–59% SM — it renders correctly, into
  its own memory.
* The KMD backing-store sampler found no poison in what it scanned. That verdict
  was **right**; only its identity accounting was sloppy.
* `helios_paintcap` is black because DWM's composed frames are produced into
  host memory and read back through a heap buffer.

### Why it is a regression, and against the code's own contract

`protocol/src/resource_association.rs:22` documents `cpu_mapping` as *"CPU view
of the exact outer allocation backing"*. `CpuBacking` is not a view of anything.
The hardware-accelerated desktop milestone (2026-08-05, Fire Strike GT1 ≈ 221)
predates the HPS2 retirement's HWA2/HVM1 memory model; the split arrived with it.

### ⛔ The obvious fix is CLOSED, measured: dxgkrnl requires the allocation to be SHARED

`FINDINGS.md` **F18** already built the mechanism this needs — Microsoft's
*Sharing the backing store with KMD* contract, 52/52 on
`tools/k2a_shared_backing_probe.c`, *"the exact guest pages back both Lock2 and
the renderer blob"*. HVM1 role 1 uses it today. **Extending it to HWA2 is not
possible.** The contract's four properties include *"The allocation must be
created as shared"*, and `D3DDDICB_ALLOCATE` (WDK 10.0.26100) has **no field**
through which a UMD could ask for it — dxgkrnl derives sharing from the runtime
resource, and ordinary D3D11 staging and dynamic resources are not shared.

`tools/k2a_unshared_backing_probe.c` asks dxgkrnl directly. The KMD cannot tell
`CreateShared` from a plain `CreateResource` — both arrive as the single
`Resource` bit — so the two roles differ only in the flag the KMD set:

| arm | role | shared | create |
|---|---|---|---|
| A | 1 — cpu-visible, `ShareBackingStoreWithKmd=1` | yes | **SUCCESS** |
| B | 1 — cpu-visible, `ShareBackingStoreWithKmd=1` | **no** | **`0xC000000D`** |
| C | 4 — not cpu-visible, no shared backing | yes | SUCCESS |
| D | 4 — not cpu-visible, no shared backing | **no** | SUCCESS |

**D is the control that splits the bucket**: identical create shape to B, and
the KMD admits it. So the KMD's own shape check is not what refuses B. ⇒ This
also re-attributes the historical `E_INVALIDARG` that the comment at
`create_allocation.rs:3341` blames on the DDI's scope.

⭐ Arm A's Lock2 pointer is **`MEM_MAPPED`** — a real section view of the
allocation backing — against **`MEM_PRIVATE`** for what a D3D11 staging texture
gets. The working path and the broken one are distinguishable in one
`VirtualQuery`.

### The two routes that remain

**(I) ICD-owned storage** — the ICD allocates the role-1 HVM1 allocation itself
(shared, guest-backed, `MEM_MAPPED` Lock2) and has the host import those pages,
instead of deferring the allocate into the outer stream. Written; **ships off**
(icd `7cc9ec2d432`, `HELIOS_HOST_VISIBLE_SHARED=1` is the arm). Everything it
needs now works except one thing:

| step | state |
|---|---|
| create the HVM1 allocation inside a record-only device | ✅ clean, proven by the `bo-only` control |
| KMD accepts the allocate render and patches the import operand | ✅ no refusal counter moves |
| host imports guest pages as `VkDeviceMemory` | ✅ **and the memory is BINDABLE** |
| host `vkAllocateMemory` returns success | ✅ once the memory type is right |
| the next command on the device's ring | ❌ **refused, session poisoned** |

⭐ **The importable-memory-type finding, measured on the host GPU outside the
stack** (`tools/udmabuf_import_probe.c`): NVIDIA's `vkGetMemoryFdProperties`
mask for a udmabuf is `0x9`, and **only type 0 — `propertyFlags = 0x00` —
actually imports; type 3, the host-visible one, returns
`VK_ERROR_OUT_OF_DEVICE_MEMORY`**, which is precisely what the guest saw. The
guest never needed a host-visible type: the host does not map this memory, the
GUEST holds the CPU view of the same pages. Rule now: fewest property flags.
A buffer then binds to it, so the GPU really can read and write guest RAM.

⛔ **The one remaining blocker, isolated by control rather than inferred.**
`vn_renderer_helios_allocate_memory` executes on `helios->bootstrap`, and a
session execute is not safe once the device's primary ring is live. The next
ring command — the uncached direct `vkCreateBuffer` that `vn_buffer.c`
legitimately issues in record-only mode (venus command type 50, read verbatim
out of the refused payload) — comes back `STATUS_INVALID_PARAMETER`, which
poisons the session and removes **every D3D11 device on the box**. The control
that proves it: `HELIOS_HOST_VISIBLE_SHARED=bo-only` builds the identical HVM1
allocation and skips only the session execute — probe clean, zero refusals.

⇒ **Next step: issue the allocate on the device's own ring, with its import
operand patched there**, instead of on the bootstrap session. That is also why
the D3D12-resource import path, which uses the same function, has never been
exercised.

⚠ It stays off because turning it on is strictly worse than the defect it
fixes: a black desktop becomes no desktop. Two sub-limits to carry forward — a
4 MiB cap per allocation (`HVM1_CPU_VISIBLE_MAX_BYTES`, the host's stock udmabuf
`list_limit` of 1024 pages), and page granularity (DXVK asks for 64-byte
host-visible allocations; the size is rounded up, and sub-page requests fall
back).

**(II) Restore the CPU host aperture — LANDED, NOT YET EFFECTIVE** (`4f4b949`,
KMD 22.22.397.0). K1 (`60a9988`) deleted `cpu_host_aperture.rs` as part of a
demolition whose rebuild K2 was **never started** — all three of K2's files are
still absent and HPM1 was later parked (F5) — so CPU visibility of allocation
content was left with no owner and the UMD's `CpuBacking` filled the hole.
`build_paging_buffer.rs` has asserted a false property since 2026-08-21.

What is in and working:

* `cpu_host_aperture.rs` restored, keeping the two lessons the deleted file paid
  for: ONE validation rule for both IRQL paths, and the legal-status rules
  (never `STATUS_UNSUCCESSFUL` — out of the DDI's set, costs the whole VidPn;
  defer with `STATUS_NO_MEMORY`).
* `ctrl::map_blob_at` — maps at the EXACT window offset dxgkrnl chose.
* Segment 2 (the LAST, per the Code-43 invariant) carries the window and
  advertises `SupportsCpuHostAperture` + `SupportsCachedCpuHostAperture` +
  `CacheCoherent`, union made exclusive by construction.
* The UMD takes `cpu_mapping` from `pfnLockCb`, `pSystemMem = NULL` — what
  `resource_association.rs` always documented it to be.
* ✅ **The adapter boots `CM_PROB_NONE` with the aperture-capable segment**,
  across five KMD versions. That was the single biggest risk in the change.

⛔ **The gap: `ChMc = 0`.** dxgkrnl never calls `DxgkDdiMapCpuHostAperture`, so
the Lock2 view is still system pages and the poison still survives. Forcing it
by removing the aperture from the allocation's supported segment set makes
`pfnAllocateCb` refuse every CPU-visible allocation with `E_INVALIDARG` —
measured on .394/.395/.396 across three segment-flag shapes including the
historical `BarSegFlags = 0x1C`, **so it is not the segment flags**.

⭐ **No QEMU dependency, and the retirement already ruled on this.** The aperture
maps blobs through the stock `RESOURCE_MAP_BLOB` window — no host change, no
HPM1. `FINDINGS.md` **F5** (2026-08-10, owner decision) parked the whole QEMU
memory lane and named what replaces it, which is worth knowing before anyone
reaches for a host-side mechanism again:

* **HPM1 is parked, not deleted** — branch `helios/hpm1-parked`, tag
  `helios-hpm1-parked-2026-08-10` in `qemu-helios`; ~4,500 lines, of which 2,442
  were in one upstream file. It was *unreviewed and unreachable*: HPM1 is entered
  only by explicit guest negotiation, and no guest negotiates.
* **The recommended alternative to HPM1 for C63** (resolving a
  `DXGKARG_SUBMITCOMMANDVIRTUAL` `DmaBufferVirtualAddress` to command bytes) is
  **guest-side arithmetic over the KMD's own permanent kernel mapping of the
  HOC1 pool**, not a host page-table walk:
  `validate gpuva ∈ [pool_base, pool_base+size)` → `offset = gpuva - pool_base`
  → `HOB1 = kernel_mapping + offset`, then an ordinary `SUBMIT_3D`. It is
  **stricter** than HPM1 — "the KMD never dereferences arbitrary user GPUVA"
  becomes "the KMD dereferences only its own allocation at a bounds-checked
  offset" — and needs no new mechanism, since §C65 already requires the pool,
  its lifetime lock and its stable GPUVA. ⚠ An argument from the text, never
  implemented or measured.
* **Option zero, which is what actually runs**: do not adopt GPUVA/HOB1 submit
  for D3D12 at all. vkd3d needs no D3D12 GPUVA semantics from the KMD.
* **For C55** (renderer-private resource-id substitution after Patch): if still
  wanted, do it **guest-side in the KMD** while building the `SUBMIT_3D`. The
  property traded away is "the host is the authority on identity", and that must
  be recorded wherever it is relied on. Its late-binding benefit is unrealized
  anyway — it only pays off once allocations actually move, and the working
  stack pins.
* **F19** then closed the door further: stock virtio-gpu/Venus is sufficient for
  one exact per-HTS1 host namespace; **K11 needs no HPM1 and no new QEMU
  protocol**. Reopening HPM1 means un-parking the branch *and* running its
  adversarial review first — review before reachable, not after.

### ⛔ 2026-08-30 — `ChMc = 0` IS STRUCTURAL. Route (II) cannot be finished by a knob.

The DxgKrnl ETW trace was taken (`Microsoft-Windows-DxgKrnl`, all keywords,
over one `d3d11_hostram_alias_probe` run, KMD 22.22.398.0). It names the
mechanism, and the answer is that **no allocation-level or segment-level knob
can put `DxgkDdiMapCpuHostAperture` on the CPU-lock path**, because VidMm does
not use it for `Lock`. Two independent reasons, both measured:

**1. The mapped class never reaches segment 2 at all.** Every resource DXVK
actually maps arrives at VidMm as:

```
AdapterAllocStart  Flags="CpuVisible |Shareable"  size=4194304
                   SupportedSegmentSet=1  PreferredSegment=1
```

Segment 1 is the linear aperture. That is **this driver's own rule** —
`hwa2_may_prefer_local_memory` excludes `HELIOS_HWA2_FLAG_SHARED` — so segment
2 and its aperture are not candidates for the whole mapped class. `BarLocalShare`
is the A/B that readmits them; it is off, because on its own it only moves the
class into case 2.

**2. A CPU lock EVICTS an allocation out of segment 2.** For the allocations
that do prefer it (`SupportedSegmentSet=3 PreferredSegment=2`):

```
ReserveResource  2, alloc   ->  ResidentInSeg seg=2   (paged in, fine)
PageIn ; MarkAlloc ; ResidentInSeg seg=2 -4 MiB
EvictAllocation  alloc
AllocationFault  alloc  DXGKETW_ALLOCATIONFAULT_NOT_RESIDENT
ReserveResource  3, alloc, Restriction=VidMmPlacementRestrictionApertureSegment
LockAllocationBackingStore  x16
```

VidMm answers the lock by *moving the allocation to an aperture segment* and
serving the pointer from the backing store. `MapCpuHostAperture` is not on that
path, and `PgTo` (blob -> system MDL) is the copy.

**Five single-variable arms, all falsified, each left in the tree as its A/B:**

| arm | knob | result |
|---|---|---|
| segment `CpuVisible` + `SupportsCpuHostAperture` | `BarSegFlagsX=4` | boots `CM_PROB_NONE`; ETW re-taken and **identical**, evict and all |
| `pfnLockCb` + `DonotEvict` | `UmdLockMode=1` | lock still succeeds, `MEM_PRIVATE`, stamps survive |
| `pfnLock2Cb` | `UmdLockMode=2` | identical to mode 0 |
| `restricted_to_single_segment` | — | **moot**: `ReserveResource` already reports `VidMmPlacementRestrictionNone` for these, and the restriction VidMm *chooses* on the lock is `ApertureSegment`. Pinning to one segment does not stop a move to a different one |
| `EvictionSegmentSet` / `AccessedPhysically` | — | same reason; neither is consulted on the lock path |

⚠ The .394/.395 `E_INVALIDARG` that argued against segment `CpuVisible` was
taken with the aperture ALSO removed from the supported set — a confound. The
pair alone boots fine. The bit is simply not what decides this.

### ⭐⭐ WHERE THE TWO HALVES ARE ACTUALLY OFFERED: `MAP_APERTURE_SEGMENT`

Because the mapped class lives in the **linear aperture segment**, its content
pages are ordinary guest system pages, and dxgkrnl hands them to this driver on
a plate:

```c
DXGK_OPERATION_MAP_APERTURE_SEGMENT
  { hAllocation, SegmentId, OffsetInPages, NumberOfPages, pMdl, MdlOffset }
```

`pMdl` **is** the allocation's real storage. `build_paging_buffer.rs` dropped it
— the operation fell into `PagingOperation::Other`, the null engine — while the
GPU side of the same allocation is a separately allocated *host-side* venus
blob. That is the two-buffer defect, at the exact DDI where WDDM offers to join
the two halves.

`767df92` NAMES the operation (no behaviour change yet) and adds the census the
next step needs: `PgAm`/`PgAu` counts, and `PgAbN`/`PgAbR`/`PgAbS`/`PgAbPlo`/
`PgAbPhi` for maps of >= 1024 pages. **48 aperture maps land before a probe even
runs.**

⭐ **The census is cross-validated by an oracle outside the guest.** `PgAbP*`
is a guest page frame; QEMU's own `virtio_gpu_virgl_guest_blob_backing` trace
reports byte-identical first GPAs for the same resource ids:

| KMD census | QEMU trace |
|---|---|
| resid 255, pfn 1907019 -> `0x1d194b000` | `res 0xff, size 4194304, ranges 221, first 0x1d194b000` |
| resid 258, pfn 8832424 -> `0x86c5a8000` | `res 0x102, ranges 872, first 0x86c5a8000` |
| resid 260, pfn 1338488 -> `0x146c78000` | `res 0x104, ranges 223, first 0x146c78000` |

So the KMD's view of guest physical memory is provably right, and QEMU can
already read any page the aperture names. ⛔ `xp` on all three during the
probe's stamp window reads **zero** — the application's stamps are not in the
allocation's own pages, which is the root cause restated with a new instrument
rather than a new theory.

### ⛔ 2026-08-30, later — the aperture join point DOES NOT EXIST, and route (II) is closed

Two corrections and one closure, all measured on 22.22.401-.402.

**The class attribution in the block above was wrong.** Joining
`AdapterAllocation` to `PagingOpMapApertureSegment` by `hVidMmGlobalAlloc`
splits a probe run's six allocations cleanly:

| flags | size | set/pref | aperture-mapped | what it is |
|---|---|---|---|---|
| `CpuVisible \| Shareable` | 4 MiB | 1 / 1 | YES | the HVM1 pool |
| `Protected \| FromEndOfSegment` | 15 MiB | 1 / 0 | YES (`hAllocation` NULL) | dxgkrnl's own |
| `CpuVisible \| Cached` | 4 MiB | **3 / 2** | **no** | the D3D11 allocations |

So the D3D11 class was never excluded from segment 2 — it PREFERS it, and
`BarLocalShare` is not the lever it was called. An 8-slot ring
(`PgR0..7{r,n,k,l,h}`, `d51c197`) confirms it from the driver side: every
resolved aperture map at idle and across a probe is `kind=1`, HVM1 role 1 or 2.
**`MAP_APERTURE_SEGMENT` never names a D3D11 allocation**, so it is not a
channel to their pages.

**Route (II) is closed.** `BarSegOnly` removes the linear aperture from a
local-preferring allocation's supported set — the only shape that can force a
CPU lock through the aperture DDI, since there is nowhere to evict to. Paired
with `BarSegFlagsX=4` it covers the cell the .394-.396 `E_INVALIDARG` never
did: that measurement moved the segment flags and the supported set together,
so "it is not the flags" was an inference. Measured: the adapter starts
`CM_PROB_NONE` and `pfnAllocateCb` still refuses every CPU-visible allocation
with `0x80070057`. dxgkrnl requires a system-memory-capable segment in the
supported set, and the segment's `CpuVisible` bit does not satisfy it.

### ⭐⭐⭐ THE REPLACEMENT, AND IT WORKS: the creator states its own pages

If WDDM will not hand the driver the application's pages, the application's
own UMD can. `HeliosWddmAllocationDescV2::cpu_backing_va` (the record is now
176 bytes; C mirror and `abi_parity.py` updated, gate green) carries a
page-aligned, page-rounded creator buffer; the KMD locks it with the
SEH-guarded probe, coalesces its page frames into memory entries, creates a
`VIRTIO_GPU_BLOB_MEM_GUEST` resource over them, and makes that resource the
allocation's `VkDeviceMemory` via `VkImportMemoryResourceInfoMESA`. Knobs
`Hwa2GuestMem` (KMD) + `UmdGuestBacking` (UMD), both OFF, only meaningful
together.

Measured on 22.22.408.0 with both on:

* QEMU: `guest_blob_backing res 0x4a4, size 4194304, ranges 797` — **the
  application's own scattered pages are a GPU resource**, through the udmabuf
  import QEMU already runs thousands of times a session for HVM1.
* The venus import succeeds. `GbImpMti = 0`, `GbImpMtf = 0x0` — the flagless
  memory type, which is exactly what `tools/udmabuf_import_probe.c` measured on
  the host GPU, chosen from the guest's own table rather than hard-coded.
* `GbProbe = 0`: `MmProbeAndLockPages` on the creator's VA SUCCEEDS, so
  `DxgkDdiCreateAllocation` runs in the creating process. (It is safe either
  way — the SEH shim turns a foreign VA into a counted refusal.)
* KMD knob on, UMD knob off: byte-identical to baseline. The KMD half carries
  no regression risk on its own.

⛔ **The one thing left, and it is F11, not a mystery.** With the UMD half on,
the UMD refuses its own create with `invalid HWA2 output
AllocationGenerationZero`. The KMD reached the write-back and stamped it —
`GbWbGen = 5`, `GbWbSz = 176`, a witness added for exactly this fork — so the
record the UMD validates is NOT the record the KMD wrote. `FINDINGS.md` **F11**
already measured why: a KMD write into `DXGK_ALLOCATIONINFO::pPrivateDriverData`
at create reaches nobody (7 creates -> 7 zeros,
`tools/hwa2_writeback_probe.c`), and `DxgkDdiOpenAllocation`'s `in/out` copy is
the only channel. Sending a nonzero `cpu_backing_va` makes the UMD's own input
fail the output validator it had been passing only because of that same
discard.

### ✅ 2026-08-30 — F11 CLOSED. It was not the record that never comes back; it was the record that never should have carried the field

The write-back does work on this path — the earlier reading was right that the
UMD saw generation 0, and wrong about why. `UmdGuestBacking=2`, a throwaway arm
that wrote a FAKE page-aligned `0x1000` into `cpu_backing_va` and changed
NOTHING else, reproduced the failure exactly:

| arm | `sent_gen` | `local_gen` | `ptr_gen` | `sent_va` |
|---|---|---|---|---|
| off | 0 | 4294967306 | 4294967306 | 0 |
| **fake VA only** | 0 | **0** | **0** | 0x1000 |
| full | 0 | 0 | 0 | 0x226aa93d000 |

⇒ **a nonzero TAIL BYTE in the create-input descriptor is what makes dxgkrnl
drop the KMD's create-output copy-back.** Not the buffer, not the mapping, not
the import. (`local` vs `ptr` also excludes a stale local: the UMD read back
through the pointer dxgkrnl was given and got the same bytes.)

**Fix** (`83088e0`): input-only data does not belong in an echoed record. The
offer moved to `HeliosCpuBackingV1`, a 24-byte record in the RESOURCE-level
private data (`DXGKARG_CREATEALLOCATION::pPrivateDriverData`), which nothing
echoes. `HeliosWddmAllocationDescV2` is back to its historical 168 bytes, so the
C mirror and `abi_parity.py` revert with it. Also fixed: the UMD had pointed
BOTH the resource- and allocation-level `pPrivateDriverData` at one buffer, so
dxgkrnl had two copy-backs into the same place.

Measured on 22.22.412.0, both knobs on: the probe runs to completion, no
`FAIL staging`, and the UMD logs `alloc_gen=0x100000006`.

### ⛔ WHERE IT STANDS NOW — the probe is not green, and the frontier moved

`P3 AFTER_COPY stamped=1048576 cleared=0` still, but the failure is different
and narrower: `HNR2 context REFUSED at render status=0xc000000d`, then
`context_lost`. The session's refused payload decodes to venus command **0x55 =
`vkCreateCommandPool`** — an ordinary object create, i.e. the
POISONED-SESSION symptom, not the cause. Something earlier in the stream is
refused and everything after it dies.

⛔ **It is NOT the memory type**, which was the obvious next suspect and is now
measured out (`fc47c02`). Importing into the HOST_VISIBLE|HOST_COHERENT type —
the one the ICD asks for and binds against — makes the IMPORT fail outright
(`GbImp` 0 -> 1, `GbOk` 1 -> 0). Only the flagless type accepts a dmabuf, on
the venus device exactly as `tools/udmabuf_import_probe.c` measured on the host
GPU. The guest never needed a host-visible type: the GUEST holds the CPU view,
the host does not map these pages.

### ✅ THE GUEST HALF IS VERIFIED. The page-mapping suspicion is excluded

Four readings from one live run that must all agree, and do:

| reading | value |
|---|---|
| UMD `gb-witness` | `va=0x25123a88000 bytes=4194304 head=0xb00b0000` |
| probe `MAP ptr` | `0x25123a88000` — the app maps OUR buffer |
| KMD `GbInVaLo` | `0x23a88000` — same VA, low 32 |
| KMD `GbHead` | `0xB00B0000` — read through the KMD's OWN mapping of the MDL |

`GbHead` settles it: a kernel read of the very MDL the import was built from
returns the creator's control pattern. The VA crosses, the MDL describes the
creator's buffer, and the application maps that same buffer.

⛔ **THE QMP PAGE READS THAT SAID OTHERWISE WERE RACING A DYING PROCESS.** With
the knobs on the probe EXITS during its P1 window, so by sample time the buffer
was freed and its pages recycled — the same GPA read zero, then `0x20` junk,
then zero. Two controls make that unarguable:

* `xp` is sound — real code at `0x100000`, an MZ header at `0x100000000`,
  `"RCRD"` in a live HVR1 pool.
* the two HVM1 blobs in the same run — **the WORKING kernel-MDL path** — also
  read zero at their first GPA, because an unused pool is zero.

⇒ **Zero at a guest blob's first GPA is not evidence of anything.** Any future
host-side reading needs the subject identified first, which is what `GbEntN` +
`GbGpaLo/Hi` are for: they publish the memory-entry count and first address,
i.e. exactly `ranges` and `first` in
`virtio_gpu_virgl_guest_blob_backing`. A probe run creates three or four 4 MiB
guest blobs and only one is the subject; without this key I had been reading
HVM1's.

⚠ Also corrected: `MmProbeAndLockPages` now uses a **UserMode** SEH shim for the
creator's range. The KernelMode one the K2a backing store uses skips the "is
this user address space in THIS process" check and would succeed over the wrong
pages silently. It changed nothing here (`GbProbe` stays 0, so the VA is valid
in that context and `DxgkDdiCreateAllocation` does run in the creating process),
but locking user pages with KernelMode access was wrong on its own terms.

⇒ **Next**: the probe DIES during P1 with the knobs on — that is the symptom,
and it is a GUEST-side refusal (`HNR2 context REFUSED at render 0xc000000d`,
then `context_lost`), not a page-mapping problem. Find the FIRST refused command
rather than the one that reports: no `Nr2*` predicate counter moves, so the
refusal is taken somewhere that does not name itself. Give the `D3DKMTRender`
`0xc000000d` path a per-predicate counter the way the aperture census was split,
then read it. Win condition unchanged: `P3 AFTER_COPY cleared=1048576`, and both
knob defaults flip together on it.

---

## ✅ CLOSED 2026-08-28 — session FREEZE: a redundant `pfnEvictCb` poisoned the device

**Fixed in `93d3601` (UMD only; KMD 22.22.380.0 unchanged).**
`ResidentAllocation::drop` called `pfnEvictCb` unconditionally, and every
teardown that drops the guard follows it with a `pfnDeallocateCb` covering the
same handle. The evict is redundant — `D3DKMTDestroyAllocation` takes the
allocation out of the residency list itself — and it is the call that races a
submitted-but-unretired DMA packet. `release_residency(resident, deallocated)`
now decides: only a successful deallocate suppresses the evict; an opened
allocation nobody deallocates, a failed deallocate and a missing callback all
still evict. `residency_evict_suppressed` is the last column of the
`DDI refusals:` line; `UmdEvictOnDeallocate=1` is the A/B disable.

| | before | after |
|---|---|---|
| boot ETW | 253,204 events / 36.6 s | 993,394 events / 102 s, 0 lost |
| `VidSchError*` | `EvictingWhileInUse` → `DriverFaulted` at boot+9 s | **zero, of any kind** |
| unreturned `DxgkDestroyAllocation` | 1 (the deadlock) | none |
| dwm CPU | 1.48 s, then 0.000 s over 8 s | 12.8 s and climbing |
| session-1 scheduled task | hangs at `SCHED_S_TASK_RUNNING` | runs, writes its file |
| terminal scanout signature | 20/20 boots | absent, 2/2 boots |

⛔ The desktop is **still black** (`D2PxNz=0`) — that producer defect is
independent, unchanged, and is now the top display defect. See below.

<details><summary>The chain as it was diagnosed (kept: the method and the two dead leads)</summary>

## The freeze chain — the DEVICE WAS POISONED BEFORE THE DEADLOCK

**2026-08-28, boot ETW (`Microsoft-Windows-DxgKrnl`, all keywords, autologger).
Reproduced twice on a hash-verified 22.22.380.0.** The freeze is a two-stage
chain and the deadlock is the SECOND stage. Recover all of it in one command:

```
# guest: tracerpt C:\heliosboot.etl -o C:\hb.csv -of CSV -y ; gzip to Z:\tmp
tools/etw-wedge-report.py tmp/hb.csv.gz            # summary
tools/etw-wedge-report.py tmp/hb.csv.gz 8.93 8.94  # microsecond window
```

### Stage 1 — the first cause, ~180 ms before anything looks wrong

```
t+0.0 us  DxgkRender  (dwm "dxvk-cs")  DMA buffer references allocation A
t+80 us   eid 178     the command buffer is QUEUED (216 B, 1 allocation)
t+92 us   DdiSubmitCommand Start   <- our KMD; this one does NOT complete inline
t+98 us   DxgkEvict   (same thread) on A
t+102 us  ★ VidSchErrorEvictingWhileInUse  -> device execution state := 7
t+160 us  GetDeviceState = 7, three times
t+270 us  DxgkMarkDeviceAsError -> VidSchErrorDriverFaulted
```

`VidSchErrorEvictingWhileInUse` is the **first error in the whole trace** (three
`VidSchError*` events total, 253k events). It is D3D11's normal free of a
resource — `D3DKMTEvict` then `D3DKMTDestroyAllocation` — landing inside the
window in which our `DxgkDdiSubmitCommand` has the referencing packet and has not
yet reported `DXGK_INTERRUPT_DMA_COMPLETED`. **3068 of 3185 submits complete the
packet inline inside `DdiSubmitCommand`; 117 do not, and the loser is one of
those 117.** dwm then sees DEVICE_REMOVED and tears its device down — correctly.

### Stage 2 — the teardown deadlocks

Everything previously recorded as "the freeze" is dwm's correct device-lost
teardown: the last MPO3 flip, `DdiSetVidPnSourceVisibility` (94–164 ms, two host
round-trips for park+disable), `SetVidPnSourceOwner`, then ~13 Evict/Destroy
pairs. The last one never returns — the KD stack still stands:

```
dxgkrnl!DxgkDestroyAllocationInternal ... DXGDEVICE::UnpinPrimaryAllocations
dxgmms2!VIDMM_GLOBAL::CloseOneAllocation+0x210 <- BLOCKED (waits on a _KEVENT**)
```

VidMm holds the DXGADAPTER core resource EXCLUSIVE while it waits ⇒
`AcquireCoreResourceShared` → win32k User fast-resource → loader lock
(`ImmDllInitialize`) → nothing starts in session 1. Session 0 never takes that
path, which is why SSH survives for a few minutes.

⭐ The teardown's own tell: **exactly one `DxgkRender` out of 3100 returns without
queuing a packet, and it is the render that references the allocation that then
deadlocks**, 200 us later. Same in both traces.

⛔ dxgkrnl blocks BEFORE calling the miniport, so `CaStep`/`DaStep` last-call
complete, `SxWait=0`, every ledger balanced and no TDR are all true and all
irrelevant. Do not re-instrument the teardown DDIs.

⇒ **Fix stage 1 first.** It is upstream, it is ours, and stage 2 may not be
reachable without it. The open question is why VidSch calls A "in use": the
candidates are the async-completion window itself and what our KMD reports as the
context's completed fence (`eid 553` names two monitored fences, both unsatisfied,
inside the failing Evict). That needs a KMD-side instrument, not more ETW.

### ⛔ Two leads that were live on 2026-08-27 and are now void

- **`ScPub`/`ScRet`/`ScOff` are fossils.** Their only writer,
  `kmd_render/src/adapter/scanout.rs`, was deleted in `60a9988`. `ScPub=6` vs
  `ScRet=5` is a registry fossil from before 2026-08-21, not a live imbalance.
  Check any counter with `tools/kmd-live-counter-names.sh` before reasoning from it.
- **The display DOES activate.** QEMU imports DWM's 4587520-byte primaries as
  OPTIMAL and composites them for 1–7 s on every boot (20 boots in
  `/tmp/helios-qemu-stderr.log`, one signature: rotate 3 primaries → park to
  res 0x4 → `set_scanout_blob res 0x0`). "Display output is not active" is the
  state AFTER our own disable. The `required=4587520 fd_size=4096000` messages are
  QEMU's first import attempt; it retries LINEAR and succeeds.

### ✅ FIXED 2026-08-28 — the KMD refused every batch with an empty allocation list

**`f7ef162`, KMD 22.22.382.0.** `render_outer_physical`'s entry guard required a
non-empty allocation list. DXVK's SECOND batch on every device it has ever
created is 416 bytes with zero allocation uses, so **every D3D11 device on the
box died on its second submit**: the KMD answered STATUS_INVALID_PARAMETER, the
UMD turned that into DeviceLost, and every texture read back zero.

```
A7 D3D11 HOB1 Render in  batch=1 cmdlen=2212 nalloc=2 ... -> hr=0x00000000
A7 D3D11 HOB1 Render in  batch=2 cmdlen=416  nalloc=0 ... -> hr=0x80070057
A7 D3D11 outer device lost at HOB1 Render
```

| | before | after |
|---|---|---|
| `Nr2OuterRej` | `0x000D0001`, +4 per probe run, code 1 | **absent (zero all boot)** |
| `Nr2OuterQ` / `Nr2OuterHost` | 47 / 48 | 102 / 102, balanced |
| UMD log | "HOB1 Render refused" then DeviceLost, every process | neither, in any process |
| host teardown | "19 leaked objects" | clean |

The guard's ten disjuncts all recorded one code, `PrivateData`, and that single
bucket is what cost the day — the counter named a predicate that was not the one
failing. Now split into `OuterClassMismatch` / `OuterCommandShape` /
`OuterPatchListNonEmpty` / `OuterAllocationList`.

⛔ **The desktop is still black and the probe's three GPU stages still read
zero.** This was a layer, not the end.

### ⛔ TOP DEFECT — the black desktop is NOT a display defect

**2026-08-28.** `tools/d3d11_roundtrip_split_probe.cpp` reproduces it headless,
in session 0, in two seconds — no window, no DWM, no scanout:

```
1 CPU     match=4096/4096 zero=0/4096    PASS   Map(WRITE)->Unmap->Map(READ)
2 UPLOAD  match=0/4096    zero=4096/4096 FAIL   staging -> CopyResource -> staging
3 INIT    match=0/4096    zero=4096/4096 FAIL   D3D11_SUBRESOURCE_DATA -> copy
4 CLEAR   px0=0x00000000  zero=4096/4096 FAIL   ClearRenderTargetView -> copy
```

**The CPU view is real and coherent. Everything that routes through the GPU
comes back exactly zero — not garbage.** Four older probes agree:
`helios_clear_test_321` RESULT: FAIL, `d3d11_staging_readback_probe` first bytes
`00 00 00 00`, `d3d11_upload_integrity_probe` **30/30 FAIL** with
`zeroBytes=8294400 of 8294400`, and `d3d11_shared_content_probe`'s
**A(dev1 self)=FAIL** — same device, no sharing.

⇒ Nothing about the display, the primary, the scanout or DWM is required to see
this. `D2PxNz=0` and the black `helios_paintcap` are downstream of it.

**Submission is not the missing piece.** A live `DxgKrnl` trace of one probe run
shows **414 `DxgkRender` calls, every one reaching `DdiRender`**. The work is
submitted and produces nothing the CPU can then read.

#### The three named host-side defects, with counts (all still open)

Per boot, `Microsoft-Windows-DxgKrnl` off, `HELIOS_VKR_DEBUG=validate` on:

| n | what |
|---|---|
| 79 | `vkCreateGraphicsPipelines`: SPIR-V `PhysicalStorageBufferAddresses` declared, `bufferDeviceAddress` **not enabled on the device** |
| 31 | `vkAllocateMemory`: `VK_MEMORY_ALLOCATE_DEVICE_ADDRESS_BIT` with the same feature off |
| 77 | `vkCreateBuffer`: `sharingMode=CONCURRENT`, `pQueueFamilyIndices[0] = 1000146003` |

⭐ **1000146003 is `VK_STRUCTURE_TYPE_MEMORY_REQUIREMENTS_2`** — a structure type
being read as a queue-family index, i.e. wire corruption, not a bad value. The
only CONCURRENT buffer in the stack is **`vn_feedback.c`'s feedback buffer**
(`.pQueueFamilyIndices = dev->queue_families`), and Venus feedback buffers are
how the guest observes **fence and timeline-semaphore completion**. A broken
feedback buffer makes DXVK believe a copy has finished and map the destination
before it has — which returns exactly zero, not garbage. **That is the
best-supported next hypothesis and it is untested.**

The bufferDeviceAddress rows are the same defect the ICD already names in
`vn_device.c` (2026-08-24: *"host validation reports the session device lacks
features (EXT buffer_device_address, sync2) the guest demonstrably enabled;
this names which layer loses them"*). The guest's own diag prints
`HD1 bdaEXT present=1 enable=1 capture=1` and the wire encoder handles both
feature structs, so the loss is at or past `vn_call_vkCreateDevice`.

#### Where it stands after `f7ef162` — and the 2026-08-29 correction

⛔ **The "everything reads back zero" framing was wrong, and it was wrong in a
way that pointed the whole investigation at the wrong layer.**
`tools/d3d11_poison_copy_probe.cpp` fills the DESTINATION staging texture with
`0xCDCDCDCD` through `Map(WRITE)`, confirms the poison reads back, and only then
runs the copy. The old probes never wrote the destination, so "reads zero" could
not distinguish "the GPU wrote zeros" from "these pages were never touched".

```
A CONTROL  poison=4096/4096   POISON INTACT   (poison round-trips: YES)
B S2S      poison=4096/4096   POISON INTACT   staging -> staging
C S2D2S    poison=4096/4096   POISON INTACT   staging -> DEFAULT -> staging
D CLEAR    poison=4096/4096   POISON INTACT   ClearRenderTargetView -> staging
D TIMESTAMP  freq=0 t0=0 t1=0 delta=0
```

⇒ **Nothing writes the CPU's view at all.** Not zeros — nothing. Every earlier
"exactly zero" reading was an untouched fresh page. So no hypothesis about *what
value* the GPU produced can be right; the question is why the GPU's writes and
the CPU's map are not the same memory.

`freq=0` is a second, independent finding: DXVK computes it as
`1e9f / limits.timestampPeriod` (`d3d11_query.cpp:346`), so the venus device is
reporting **`timestampPeriod == 0`**, and D3D11 timestamp queries cannot work.

#### What is proven to work, so it can stop being re-measured

Measured 2026-08-29 with six `virtio-gpu` trace events enabled live over QMP
(`/tmp/helios-tpm/mon.sock`, `trace-event-set-state`; no relaunch needed) and
read back from `/tmp/helios-qemu-stderr.log`:

| evidence | reading |
|---|---|
| `virtio_gpu_cmd_ctx_submit` | **1360** in one probe window — the command stream reaches the host in volume |
| `virtio_gpu_cmd_res_create_blob` | 12 per run: 2×4 MiB guest-backed, 2×4 MiB HOST3D, 2×16 KiB HOST3D |
| `virtio_gpu_virgl_guest_blob_backing` | **fires** for the host-visible 4 MiB pools, `ranges 886` / `ranges 379` — real guest PFN scatter lists cross to the host |
| KMD `ShBkFeat` | `1` — the K2a shared-backing feature is admitted this boot |
| KMD `ShBkOk` | moved **286 → 292** across a probe run — `DxgkDdiSetAllocationBackingStore` runs and succeeds for host-visible HVM1 allocations |
| UMD log | zero `DDI refusals:` of any kind; clean teardown |

So: guest pages are described to the host, blobs exist, commands arrive, objects
are created. The break is in the **last link only** — whether the host's
`VkDeviceMemory` for a guest-backed pool is actually bound to those guest pages.
`create_allocation.rs:3766` states the intent: *"Roles 1-3 receive their one
OS-owned K2a backing later through SetAllocationBackingStore"*, imported host-side
as one udmabuf scatter list capped at 1024 pages / 4 MiB.

⚠ Do not repeat this mistake: a first pass read `sort | uniq -c | sort -rn |
head -20` and concluded **no blob was ever created**, because 1360 `ctx_submit`
lines crowded the 12 single-occurrence `create_blob` lines off the list. Count
the event you care about explicitly; never read a null out of a truncated
histogram.

### ⭐⭐⭐⭐ 2026-08-29 FINAL: THE HOST RENDERS. THE GUEST CANNOT READ IT BACK.

⛔ **This supersedes the ranking immediately below it.** The decisive measurement
came from the one observer outside the entire stack: the host GPU.

`tools/d3d11_gpu_load_probe.cpp` (1024×1024, 64 full-viewport draws per
iteration, expensive pixel shader) with `nvidia-smi pmon -c 30 -s u -d 1` on the
Linux host:

```
0  104293  C+G  42  0 ... virgl_render_se
0  104293  C+G  58  0 ... virgl_render_se
0  104293  C+G  59  0 ... virgl_render_se      <- ~23 s sustained
```

pid 104293 is the render server **spawned for this probe**; every other render
server stayed at `-` throughout, and the GPU returned to 0% when it ended.

⇒ **THE HOST GPU EXECUTES OUR DRAWS — fragment shader and all.**

#### What that settles

| | verdict |
|---|---|
| Guest emits a correct venus stream | ✅ proven (`HRA2`) |
| KMD accepts, QEMU receives | ✅ proven |
| **Host rasterises it** | ✅ **proven (42–59% SM)** |
| Guest sees any result | ❌ never |

* **Theory 3 (host never executes) is REFUTED.** The occlusion/pipeline-statistic
  zeros were read-before-execute artifacts of the broken query channel, exactly
  as `PREFLUSH GetData` indicated.
* **Theory 2 (F16 — the guest CPU view is not the host's memory) is now the
  leading theory**, by elimination *and* on its own evidence. Everything in the
  pipeline works except the guest's ability to read what was rendered.
* **Theory 1 (premature completion) is demoted to contributing.** It is real and
  proven for the query path, but it cannot explain a poison that survives a
  +1 s re-read once execution is established.

⇒ **The next step is a code change to the readback path**, not another
measurement. The question to answer first is the one the invalid sampler failed
to: for a host-visible HVM1 allocation, is `D3DKMTLock2`'s pointer the same
physical memory the KMD locks, udmabufs and the host imports? Prove it with a
per-allocation mapping that records its own VA.

#### Two further defects found by the same probe (neither is the black desktop)

1. **`Nr2OuterRej = 0x00010006` — one `SlotExhausted`** under sustained load,
   from 0 before. Submission slots run out.
2. **The submit path then wedges**: a single loop iteration spun ~100 s of CPU
   across 19 threads without draining, while the host GPU sat idle and the probe
   never reached its own exit print. The adapter survived (`OK/CM_PROB_NONE`).
   This is almost certainly a retry spin on the exhausted slot, and it is a
   plausible contributor to the historical "freeze" reports.

### ⭐⭐⭐ 2026-08-29 END-OF-DAY: ranked theories for the next session

⛔ **Read this block before anything else in this file.** Several conclusions
above it were withdrawn the same day. What follows separates what is *measured*
from what is *inferred*, and ranks the theories by confidence.

#### PROVEN — measured, with the instrument itself validated

1. The desktop is **black** (`helios_paintcap`, 2026-08-29).
2. **The guest emits a textbook-correct venus stream.** Opcode dump of the
   recorded bytes (`HRA2`): `begin=1 end=1 draw=1 bindpipe=1 beginq=1
   render=1/1 vpcount=1 sccount=1 rasterdiscard=0` — Begin, BeginRendering,
   BindPipeline, SetViewportWithCount, SetScissorWithCount, BeginQuery, Draw,
   EndQuery, EndRendering, End.
3. It is flattened in the **correct order** (deferred → object cmds → command
   streams → queue submit) and emitted whole: `HRA1 streams=3
   command_bytes=2176 payload=160` → `HNS1 submit#18 bytes=2288`.
4. The KMD **accepts** it: `Nr2OuterQ` == `Nr2OuterHost` (+2/run),
   `Nr2OuterRej=0`.
5. **QEMU receives** it: `virtio_gpu_cmd_ctx_submit ctx 0x1b, size 2288`.
6. The host is **silent**: no `vkr_log` decode error, no `vkQueueSubmit` or
   command-buffer validation anywhere in a boot.
7. The UMD forwards **correct D3D11 state**: `RSSetViewports num=1
   first=(0,0 64x64)`, `DDI Draw: a=3 topo=4 vs=… ps=… rt0=64x64`.
8. ICD refusal counters are **all zero** except the deliberate WSI withhold
   list; DXVK's own log is clean.
9. `bufferDeviceAddress` was enabled on **neither** arm — 1672 host complaints a
   boot. **Fixed** (promoted Vulkan 1.2 arm + captureReplay retained), complaints
   → 0. The display did not change.

⇒ **Everything from the D3D11 DDI down to QEMU is correct.** Two weeks of
guest-side hypotheses are closed out by (2)–(8); do not re-open them.

#### ⛔ Instruments known to LIE on this stack — do not build on them

* **Any query-based measurement.** `PREFLUSH GetData` returns `S_OK` on Helios
  and `S_FALSE` on WARP: the occlusion query **resolves before its work is
  flushed**. `vn_GetQueryPoolResults`' record-only path
  (`vn_query_pool.c:441-486`) gates on `vn_helios_query_pool_progress`, and that
  progress is already satisfied at record time, so the guest reads the host's
  query pool before the batch executes. Occlusion/pipeline-statistics zeros are
  read-before-execute, **not** counts.
* **The KMD backing-store sampler** (`Nr2Bs*`). `Nr2BsVa` is the same VA on every
  scan: it reads one buffer N times.
* **A WARP control validates the probe, not the driver** — WARP is Microsoft's
  own rasterizer and DXVK is not in that path.

#### THEORY 1 — premature completion signalling (confidence: HIGH)

Record-only mode treats a batch as complete when the WDDM outer submit **joins**,
not when the host has executed it. `DxvkSubmissionQueue::completeRecordOnly
SubmissionsLocked` calls `joinHeliosOuterSubmit()` and then unconditionally
`notifyObjects()` / `reset()` / `recycleCommandList()` on every entry — it never
waits on a Vulkan fence. The query path proves the same premature signal
independently, and it is the *only* theory that explains all of:

* the query resolving before its work is flushed (**measured**);
* `D3D11_QUERY_EVENT` signalling in **0 ms** every time;
* a poisoned staging texture coming back untouched — the map happens before the
  copy runs;
* DWM compositing frames it believes are finished, i.e. a black desktop.

**Test:** make `joinHeliosOuterSubmit` actually wait for host completion (or add
a debug knob that does), then re-run `d3d11_poison_copy_probe`. If the poison is
replaced by the pattern, this is the root cause.
**Fix shape:** the record-only completion must be driven by a host-observed
fence, not by the submit handoff. `helios_scope_track_fence_and_submit1_signals`
and `vn_helios_query_pool_progress` are the two accounting sites.

⚠ Weakness to check first: the poison also survives a **+1 s** re-read. If the
work merely ran late, one second is ample. Either the completion signal is early
*and* the memory is not shared (Theory 2), or the work never runs at all.
Resolve this before committing to a fix.

#### THEORY 2 — the guest's CPU map is not the host's memory (confidence: MEDIUM)

`FINDINGS.md` F16, still **unresolved** — the sampler that was supposed to settle
it was invalid. The ICD takes every host-visible CPU pointer from `D3DKMTLock2`
(`vn_renderer_helios_hvm.c:401`, returned verbatim by `helios_bo_map`).
Independently supported by the poison surviving a +1 s re-read.

**Ruled out already:** supplying `pSystemMem` from the ICD (dxgkrnl ignores it
and reports its own pristine backing store, while `ShBkOk` keeps firing), and
clearing `AccessedPhysically` for the host-visible role (no change).
**Test:** a KMD instrument that maps **per allocation** and records its VA to
prove it (the last one did not). Write a signature through Lock2, read it in the
KMD through the MDL, confirm distinct VAs per allocation.

#### THEORY 3 — the host never executes command-buffer commands (confidence: LOW-MEDIUM)

Weakened but not eliminated. `vkr_context_submit_cmd` dispatches every command
and logs on failure; the log is silent; `vkr_dispatch_vkQueueSubmit` is a direct
passthrough with no deferral. But nothing has *positively* witnessed a
host-side draw, because the only witness available was the broken query channel.
**Test:** it needs a witness that is neither a query nor mapped memory — the
cleanest is a deliberate VUID violation planted in the recorded stream (e.g.
`vkCmdBeginQuery` on an unreset pool). If validation reports it, the stream
executes; if it never appears, it does not.

#### Cheapest order to attack

1. Settle **execute vs not** with a deliberate host-visible VUID (Theory 3's
   test). It is one ICD edit and it makes Theories 1 and 2 decidable.
2. If it executes → Theory 1, then Theory 2.
3. If it does not → the frontier is vkr dispatch of the flattened stream.

#### ⭐⭐⭐ 2026-08-29 FINAL: it is EXECUTION, not memory. Validated.

`tools/d3d11_execution_witness_probe.cpp`. An occlusion query and
PIPELINE_STATISTICS return through `vkGetQueryPoolResults` — a host CALL over
the venus reply channel, not mapped memory — so they separate "never executes"
from "executes into memory we do not map". **It runs the identical sequence on
WARP first**, because this session twice published a conclusion from an
instrument whose zero had never been shown capable of being nonzero:

| | occlusion | iaVertices | vsInvocations | psInvocations |
|---|---|---|---|---|
| **WARP** (control) | 4096/4096 | 3 | 3 | 4224 |
| **Helios** | **0**/4096 | **0** | **0** | **0** |

⇒ **The input assembler never sees a vertex.** Every memory-aliasing theory —
F16, Lock2, `pSystemMem`, `AccessedPhysically` — was the wrong layer, and that
is why none of them changed anything. The whole "is the CPU view the host's
memory" question is downstream of this and should be parked.

Screen checked directly the same day (`schtasks /run /tn helios_paintcap`):
**still black**. That is the goal and it is unmet.

#### The guest half is exonerated, byte by byte

| stage | evidence |
|---|---|
| D3D11 runtime calls our Draw DDI | `DDI bind_input_layout skipped` is logged from inside `draw()` (`pipeline.rs:290`) |
| DXVK records it | its own log (`*_helios_umd_dxvk.log`) has no error, no pipeline failure |
| ICD collects the recording | `HRA1 append streams=3 command_bytes=2176 payload=160` |
| ICD assembles in the right order | deferred → object commands → **command streams** → **queue submit** (`helios_record_append`) |
| ICD emits it | `HNS1 submit#18 bytes=2288 enq=17 cmp=17` |
| ICD refuses nothing | `queue_submit_without_scope=0`, all refusal counters 0 except the deliberate WSI withhold list |
| KMD accepts it | `Nr2OuterQ` and `Nr2OuterHost` both +2, `Nr2OuterRej=0` |
| QEMU receives it | `virtio_gpu_cmd_ctx_submit ctx 0x1b, size 2288` |
| host says nothing | no vkr decode error, no `vkQueueSubmit`/command-buffer validation anywhere in the boot |

⇒ **A correct, complete, 2288-byte batch containing Begin..Draw..End reaches the
host, and no vertex is processed.** The frontier is host-side decode/execute of
the flattened record-only stream, and nothing above it.

#### ✅ Fixed on the way, and it did NOT fix the display

`bufferDeviceAddress` was enabled on **neither** arm of the session device:
`dxvk_device_info.cpp` selected `VK_EXT_buffer_device_address` for record-only,
and the host then rejected every pipeline declaring
`PhysicalStorageBufferAddresses` — **769 `vkCreateGraphicsPipelines` + 29
`vkCreateComputePipelines` + 874 `vkAllocateMemory` complaints in one boot**.
Switched to the promoted Vulkan 1.2 feature (the EXT-only unbound-buffer arm is
not exercised: the sole caller queries an already-bound buffer). Verified at
both ends — guest `HD1 bdaEXT present=0` with sType 51 chained, host complaints
for the whole class → **0**. The occlusion query stayed at 0, so this removed a
real defect that was masking the layer below it.

#### ⛔ Superseded: the memory-aliasing branch

#### ⛔ 2026-08-29, LATER THE SAME DAY: the section below over-claims

`Nr2BsVa` (KMD 22.22.391.0) records the VA the sampler actually read. It is
**0xB8000000 on every scan**, for allocations that are supposed to be distinct
4 MiB pools. A per-allocation `MmMapLockedPagesSpecifyCache` returns a different
system VA each time, so the sampler reads **one buffer, N times**. `Nr2BsCd=0`
across 24 scans is therefore `Cd=0` for a single buffer, not for 24 independent
backing stores.

⇒ **"F16 confirmed" is withdrawn; the question is unresolved again.** What
survives is guest-side and independent of the sampler: the poison survives every
GPU route, and the blob/PFN/udmabuf/import chain is wired. Settle first whether
`args.pBackingStore` is genuinely per-allocation or a fixed aperture window the
KMD re-maps — until then this sampler must not be quoted.

⭐ Falsified along the way, so nobody repeats them:
* **ICD supplies `pSystemMem`** (the UMD's `CpuBacking` pattern). dxgkrnl ignores
  it and reports its own pristine backing store; `ShBkOk` keeps firing, so the
  OS-owned model is not optional here. Reverted.
* **Clear `AccessedPhysically` for the host-visible role** — the flag that makes
  VidMm withhold the real backing on Lock, and the one difference from HOC1,
  whose Lock2 view the design trusts. No measurable change. Reverted.

#### ⭐⭐ 2026-08-29 ROOT CAUSE: the Lock2 view is not the memory the host gets

⛔ **An earlier revision of this section claimed the opposite and it was wrong.**
It read 11 sampled backing stores holding a nonzero value and asserted they were
venus shmems carrying guest-written wire data, therefore that the two views
alias, therefore that `FINDINGS.md` **F16** was refuted. Nothing established
that they were shmems, or who wrote that value. Adding one counter — the size of
what was scanned — showed they were never shmems.

KMD 22.22.388.0 maps role-1 HVM1 backing stores at
`DxgkDdiSetAllocationBackingStore` and scans one at teardown.
`tools/d3d11_pool_churn_probe.cpp` churns **devices**, not textures, because
DXVK recycles one pool per device (240 textures moved `Nr2BsMap` by 2; 24
devices move it by 24):

```
24 devices, 144 poisoned staging textures, 72 GPU clear+copy
guest view:  poison_survived=72  clear_value=0  other=0
KMD:  Nr2BsSeen=24 Nr2BsScan=24 Nr2BsNz=24 Nr2BsCd=0 Nr2BsSz=4096KiB Val=0x5A
```

**`Nr2BsSz=4096KiB` is the DXVK staging pool at the CPU-visible cap**, so the
scanned allocation is exactly the one under test. The guest wrote 0xCDCDCDCD
across 1 MiB inside each 4 MiB pool; a 1024-point strided scan of that pool's
backing store expects ~256 hits and found **zero**, in all 24. Meanwhile the
guest's own read of those textures returned the poison 72 times out of 72.

⇒ **The CPU pointer the ICD hands Vulkan is not the memory whose pages go to the
host.** `helios_allocation_create` (`vn_renderer_helios_hvm.c:401`) takes it from
`D3DKMTLock2` and `helios_bo_map` returns it verbatim; the pages the KMD locks,
udmabufs and imports are a different buffer. F16 stands, and it explains
everything: CPU-only round trips pass because they stay inside the Lock2 buffer,
and every GPU-routed read comes back untouched because the GPU is working on the
other one.

⚠ Not yet established: who writes the `0x5A` the scan does find. It is the same
in every pool, so it has a deterministic writer — plausibly the host, into the
memory it imported. Worth one counter, but it does not change the conclusion:
the guest's own bytes are absent from the pages the host was given.

#### The fix, and the precedent already in-tree

The UMD does not have this problem, because it never asks for the backing store
— it supplies one. `umd/src/forward/resource.rs:610` and `:937`:

```rust
allocation_info.__bindgen_anon_1.pSystemMem = cpu_mapping.cast_const();
```

`helios_umd_common::cpu_backing::CpuBacking` is a page-aligned `alloc_zeroed`
handed to dxgkrnl, so the UMD's CPU pointer **is** the allocation's backing store
by construction. The ICD sets `info.pSystemMem = NULL` and then reaches for
Lock2. Give it the same treatment — allocate the buffer, pass it as
`pSystemMem`, use it as `allocation.cpu`, free it after
`D3DKMTDestroyAllocation2` — and the two views cannot diverge.

⚠ Open question for that change: the KMD records role-1 HVM1 allocations as
`BackingSize::SharedBackingStore` (`create_allocation.rs:3934`), the WDDM 3.1+
OS-owned-section model. Supplying `pSystemMem` makes the allocation
caller-backed, so it must be confirmed that dxgkrnl still routes it through
`DxgkDdiSetAllocationBackingStore` — that callback is what sends the PFNs to the
host, and `ShBkOk` is the counter that says whether it still fires.

⭐ Two threads, in order:

1. **The host-visible aliasing itself — the top defect.** The guest CPU pointer
   for every venus host-visible allocation comes from `D3DKMTLock2`
   (`vn_renderer_helios_hvm.c:401`, handed back verbatim by `helios_bo_map`).
   `docs/retirement/FINDINGS.md` **F16** measured that pointer, nine
   configurations deep on KMD 22.22.276.0, as *"a copy protocol: VidMm moves
   HLM1 content by paging transfer and hands the CPU the system backing"* —
   which is exactly a poison that survives every GPU write. The next step is to
   determine, on the CURRENT KMD, whether the host really imports the guest
   scatter list for res `0x122`/`0x124` or silently allocates its own memory;
   the guest-side counters cannot see that, so it needs host-side evidence.
   ⛔ The live KMD registers **no `DxgkDdiEscape`** and has no
   `cpu_host_aperture.rs`, so F16's suggested `HELIOS_ESCAPE_MAP_BLOB` route
   does not currently exist — that half of F16 is stale.
2. **`bufferDeviceAddress` is not enabled on the host device.** Unchanged and
   still unexplained: it fires on **every** host `vkAllocateMemory` (3 per probe
   run) and on `vkCreateComputePipelines`. `dxvk_device_info.cpp:502` picks the
   EXT arm and forces the core Vulkan 1.2 feature off; the host reports that
   *neither* arm is enabled, so the loss is at or below `vn_call_vkCreateDevice`.

#### ✅ Closed 2026-08-29: the corrupt CONCURRENT queue-family index

`pQueueFamilyIndices[0] = 1000146003` (**150 a boot**) was **not** `vn_feedback.c`
and not wire corruption. It is a dangling pointer in
`DxvkDevice::queryBufferMemoryRequirements` (`dxvk_device.cpp:129`):
`getSharingMode()` returns `DxvkSharingModeInfo` **by value**, `fill()` stores
`queueFamilies.data()` into `info.pQueueFamilyIndices`, and the temporary dies at
the semicolon — after which the compiler reuses that stack slot for the
`VkMemoryRequirements2 requirements` declared three lines down, whose `sType` is
`VK_STRUCTURE_TYPE_MEMORY_REQUIREMENTS_2` = **1000146003**. The value named its
own cause.

It matters because that query is the **outer-allocation sizing path** for every
D3D11 buffer (`d3d11_device.cpp:353`, which returns `E_FAIL` when the query
yields size 0), and venus turns it into a real host `vkCreateBuffer`.

Fixed by naming the temporary. Verified with
`tools/d3d11_buffer_reqs_probe.cpp` (24 buffers across every usage class, the
only path that calls it — which is why the texture-only probes never triggered
it): **0 complaints** against 150 in the boot before, from a fresh
`virgl_render_server` pid, so this is a real zero and not VVL's
duplicate-message limit.

⚠ Only 3 of the probe's 7 textures get WDDM allocations; stages 2 and 3 use
DXVK-internal memory end to end and STILL read zero, so this is not an
outer-allocation aliasing problem.

#### Bounded, not the cause: the per-process `D3DKMTRender` refusal

Every process logs exactly once, at ICD load:

```
HNS1 pid=N refused payload@0/24 kind=0: 0c00…0300…0000…
HNR2 context REFUSED at render count=1 status=0xc000000d: D3DKMTRender fragment 0/1 len=136
HOC1 drop pending=1 bytes=32
```

dwm included. It loses one context and drops one 32-byte deferred object
command. Open since 2026-08-24 (*"26 boots-worth of c000000d with no way to see
WHICH predicate refused"*), and now bounded two ways: dxgkrnl emits **no
`DxgkRender` scope at all** for it, so it is rejected in the thunk before the
traced body; and it is one render out of 415 in a probe run whose other 414 all
reach the miniport, so it cannot by itself be why nothing renders.

### Superseded: the primaries are BLACK

`D2PxN`=2–3 real primaries sampled through the canonical map, `D2PxNz`=0,
`D2PxMax`=0, while `D2PxPark` on the KMD's own parking blob reads nonzero. The
producer's pixels never reach the blob we scan out. Unrelated to the freeze
chain above and unaffected by it.

</details>

### Superseded: the CloseAllocation join (fixed, and a measurement artifact)


**2026-08-27.** The counter evidence that drove this whole investigation was
**measurement artifact**. The service key had hit a ~4000-value ceiling — 3000 of
them dead `S<num>` S-ring breadcrumbs, because `DiagLevel=1` rewrites them every
boot — and past that ceiling `RtlWriteRegistryValue` **silently fails to CREATE a
new value name** while still updating existing ones. `diag::record_named`
discards the status (`let _ = ...`), so it is invisible. Proven by a full
before/after `reg query` snapshot across a boot: **ADDED none, REMOVED none, 2497
CHANGED**.

⇒ `OaOutDrn` "absent" never meant the join failed to return — that name could
never be created. Same for `OaOutTag`/`OaOutWho`/`OaExeAct`/`OaExeDrn`/`CanN`.
And `OaOutAct=1` was a stale fossil from a pre-.377 build that an earlier
PowerShell clear had silently under-deleted.

**Re-measured with a sound instrument** (S-ring fossils deleted, all ten names
pre-created with sentinel `0x7E57`, QMP reset, image hash-verified .377): every
one of `OaOutAct`/`OaOutWho`/`OaOutDrn`/`OaExeAct`/`OaExeDrn`/`Nr2WkPend`/
`Nr2Retract`/`CanN`/`CanRel`/`CanMiss` still reads `0x7E57` — **never written** —
while `Nr2Sub`=7322 and `Nr2OuterQ`=23 update live. **And the session still
wedges.**

⇒ `close()`'s `if active != 0` branch never executes on .377. The outer rundown
join no longer blocks; `retract_parked_referencing` closed that holder class. The
live-KD stack that pinned `dxgkddi_close_allocation+0x131` was taken on **.370**,
before the fix. **The remaining freeze is a distinct, unidentified mechanism, and
it must be found with a fresh instrument** — not with the counters below.

⇒ `DxgkDdiCancelCommand` genuinely is never called: `CanN` now exists and would
update. That dead end is properly dead.

**Instrument rule, permanent:** a missing counter is evidence of NOTHING until its
name is pre-created (`0x7E57` sentinel). See
`kmd-registry-counters-are-append-only-fossils` memory for the one-shot cleanup.

### Historical (the .370-era mechanism, now fixed)


**Status 2026-08-25 05:00.** The mechanism below is proven and one holder class
is fixed and verified firing — but the freeze is NOT gone. On .375 with a
**cleared** counter key: `OaOutAct=1`, `OaOutDrn` absent, `wake_ran=False`.

**Landed:** `native_render::retract_parked_referencing`, called from
`dxgkddi_close_allocation` before the join, drops every zero-ticket parked
`Ready` batch whose custody references the dying allocation (`Nr2Retract` hit 17;
the parked leak fell 15 → 2). Plus the `OUTER_CONTEXTS` registry it needs, a real
`DxgkDdiCancelCommand` (never invoked by dxgkrnl — `CanN` always absent), and
before/after join diagnostics (`OaOutAct`/`OaOutDrn`, `OaExeAct`/`OaExeDrn`,
`Nr2WkPend`, `Nr2Retract`).

**Open:** one `OpenOuterBinding` guard is still outstanding at the join
(`OaOutAct=1`). Two claims made about it on 2026-08-25 are now **refuted from the
source**, and both were narrowing the search wrongly:

- ⛔ **`scratch.building` cannot be the holder.** `BuildingBatch` carries
  `identity`/`slot_index`/`payload`/`meta`/`staging`/`session` and **no allocation
  custody at all**. Every live `OpenOuterUse` in the driver sits in exactly one of
  `OuterCustody::{Physical,Virtual}` (so: a `Ready` batch, or the transport's
  in-flight `NativeHostCompletion`) or `OuterPending.command_pool`. It was the
  prime suspect and it is not reachable.
- ⛔ **`Nr2Sub == Nr2HostOk` does NOT exclude a live submission.**
  `NR2_HOST_SUBMIT_OK` is incremented immediately after `worker.enqueue` returns
  — before the work item runs, long before any host terminal. So "the holder is
  not an in-flight submission" was never established, and a submitted batch whose
  used-ring response never arrived is back in the candidate set, alongside a
  ticketed `Ready` batch and a leaked `OuterBindGuard`.

**Instrument (22.22.376.0).** `OaOutTag` was doubly useless — it never appeared in
the registry, and storing only the LAST tag could not have named a guard that was
never returned anyway. Replaced by per-tag outstanding counts maintained under the
rundown lock and packed into `OaOutAct` itself, which is the write proven to land:
`bits 0..7 active | 8..15 tag 1 (GPUVA/command pool) | 16..23 tag 2 (physical
list) | 24..31 tag 3 (bind guard)`. Beside it `OaOutWho` names the structure
still holding: `bits 0..5 parked no-ticket | 6..11 parked ticketed | 12..17 worker-queued
| 18..23 InFlight | 24..29 contexts walked | 30..31 = 0b01`.

**Also fixed:** `retract_parked_referencing` walked the context registry by index,
retaking the lock between entries. `unregister_outer_context` swap-removes, so a
concurrent unregister could move an unvisited context into an already-passed slot
and the walk would never see it — a silent miss during exactly the teardown storm
it runs in. It now guards every matching context in one pass.

⛔ A timeout on the join is a **use-after-free**, not a fix.

⛔ **The display-off timeout is NOT the trigger (measured 2026-08-26).** With
`VIDEOIDLE`=0 on AC and DC a fresh boot still wedges at ~2.4 min uptime, on
counters proven fresh for that boot. So reproduction needs no arming — boot and
wait ~3 min — and there is no lasting way to hold an unwedged session.
⛔ `shutdown /r` cannot recover a wedged guest (it needs the deadlocked win32k
session); confirm every reboot with `LastBootUpTime` and use QMP `system_reset`.
Full detail: `freeze-rootcaused-closealloc-unbounded-join` memory.

### ⭐ .377 measurement: the outstanding guard is UNATTRIBUTED

Fossil-free, image proven by **hash** (service `ImagePath` →
`..._13a0394f2729d86f`, SHA256 `674924D0D4DBCCA2` = the built package) rather
than by `DriverVersion`, which does not prove which image loaded.

`OaOutAct = 0x00000001` decodes to: `active`=1, tag1=tag2=tag3=0, and every
structural field 0 — no parked Ready batch (ticketed or not), no worker-queued
`OuterPending`, no `InFlight` slot. `OaOutDrn` absent (join never returned),
`OaExeAct` absent (the execution join is not involved).

`active` is incremented only in `acquire_tagged` and decremented only in
`release_tagged`, both under the same lock and both touching a tag counter in
the same critical section, so `active=1` with all tags 0 should be impossible —
and the counters are not underflowed (.377 uses `wrapping_sub` so an underflow
reads `0xF`). Leading reading: **stale rundown state, not a live holder** — a
different defect class from the lifetime inversion already fixed.

⛔ Instrument caveat: in `close()`'s blocking branch the FIRST and THIRD
`record_named_bytes` land and the SECOND never appears — across .375
(`OaOutTag`), .376 and .377 (`OaOutWho`), the last carrying the same value with
nothing called between the writes. Refuted and not to be re-tested: the name
(creatable by hand), a value cap (3998 values, takes new ones), the argument
being a call, a mangled name, driver-side deletion (none exists), a stale image.
**Put the answer on `OaOutAct`; never on a second name.**

### The mechanism (proven by live KD, then re-proven in-driver)

**It is NOT flip retirement.** With ntoseye (KD-over-serial) attached to the
wedged 22.22.370.0 guest, kernel stack walks prove: **dwm's `D3DKMTDestroyAllocation`
(rotating out old primaries after the SourceInvisible display-off drain) enters
our `DxgkDdiCloseAllocation`, which blocks forever in an unbounded
`KeWaitForSingleObject`** — `.map` pins the frame to `dxgkddi_close_allocation+0x131`
= the inlined `open.outer.close()` rundown join (`create_allocation.rs:5401` →
`:779`, `Timeout=NULL`). dxgkrnl holds the **DXGADAPTER core resource / DDI-sync
EXCLUSIVE** across that DDI, so LogonUI's `DxgkWaitForVerticalBlankEvent`
(`AcquireCoreResourceShared`) and the ENTIRE win32k User-lock chain (dwm, csrss
both sessions, winlogon, LogonUI, any `NtUserCreateSystemThreads`) deadlock
behind it. Session 0 (SSH) is unaffected. Matches every symptom (dwm 0 CPU,
counters byte-identical, no DEVICE_LOST, no bugcheck).

Layer-2 (why the join never releases): a queued/in-flight **OUTER native-render
batch** never retired its `OpenOuterUse` custody on the destroyed allocation
(`native_render.rs` OuterWorker / `drain_host_terminals`), so `outer.drained`
never signals. Not fully pinned — needs live device-ring/worker-queue offsets and
intersects the UNCOMMITTED dxvk sharing rework + A7 lane (owner-gated).

Fix: ⛔ a naive join timeout is a **use-after-free** (`drop(open)` frees the
binding while the batch still holds an `OpenOuterUse`). Correct shape (B): bound
the join + force the in-flight/queued entries terminal via the existing
physical-reset mechanism (`submit_command.rs:568`) before the free, loud counter
— upholds "a DDI must not deadlock the adapter"; touches TDR, owner-review. Or
(A, preferred/lower-risk): fix why the outer batch never retires. Full evidence +
KD-bridge tooling in the `freeze-rootcaused-closealloc-unbounded-join` memory.

## ⭐ FRONTIER A ROOT-CAUSED + FIXED, 2026-08-24 night (KMD 22.22.370.0 STAGED, NOT BOOTED)

The black desktop's producer defect was NOT the `VK_KHR_EXTERNAL_MEMORY_WIN32`
log line (red herring — shared creates succeed via associations). It was an A7
classifier gap: record-only defers every allocate/bind into the first outer
batch naming the allocation, but `vkCreateImageView`/`vkCreateBufferView`/
`vkUpdateDescriptorSets` rode the HVC1 ring, which is unordered against
pending batches — the host executed view creates on UNBOUND images (qemu
stderr validation, every worker; dwm's RTV 0x5c stuck in UNDEFINED across 9
submits), so views baked dead addresses and every composition write vanished.
§10.4 already classifies these as outer-allocation-backed; the sweep missed
them. Fix: the object-materialization lane — icd/mesa `64681966a0a` (device
FIFO, synthetic uses, skip/drop rules, HOC1 diag) + kmd_logic `cf08023`
(outer-stream grammar admits opcodes 52/53/57/58/79 between allocations and
recordings; 446 tests). Deployed to the DriverStore as 22.22.370.0 + ICD
F0FA9D90 — **binds at the next real boot; unverified**.

⛔ **QMP `system_reset` does NOT reboot this guest** (measured twice: render
workers with fresh context ids 6 s after "reset" = TDR recovery, not boot).
It resets only the devices → graphics TDRs and recovers, virtio-net dies
permanently (no SSH/ping; hot-replug did not revive it), and `inject-nmi`
breaks into the armed-but-unattached serial KD instead of bugchecking. The
guest was left running but invisible + unreachable — the VM needs an
owner-driven restart before any verification. See the
`object-lane-fix-landed-reset-does-not-reboot` memory for the full ladder.

## ⭐ TWO PRODUCER ROOT CAUSES FIXED, 2026-08-24 evening (KMD 22.22.369.0)

The ".363 silent in-flight stall" was a refusal cascade, not a sync-graph bug.
Fixed, one boot per predicate, each verified:

1. **Control lane refused ring-borne destroys** (`kmd_logic` 354f1d3, .365).
   The ICD's C60 classifier rings a destroy exactly when the buffer/image is
   UNBOUND; `validate_venus_control_stream`'s no-reply arm admitted only
   PureControl. First transient destroy of EVERY process → 0xc000000d →
   DEVICE_LOST → a process churning every ~5 s. `Nr2NoSchWho=0x60E` named it.
2. **Buffer WDDM backing undershot the venus import requirement** (umd
   c6e5f63 + dxvk-helios e957290e, .369). The buffer create arm passed
   `lower_memory_requirement=0` (tex2d preflights): 4096-byte backing vs a
   65536-byte requirement refused every dedicated buffer import. Fix =
   `PrepareBufferHelios` (maintenance4 query on the exact create info,
   assembly factored so UMD and DXVK cannot drift). ⚠ tex1d/tex3d still pass
   0 (resource.rs:1549/1601) — conformance backlog; now caught loudly.

**State on the .369 boot:** no churn, dwm alive and rendering continuously,
`Nr2OuterQ/OuterHost` climbing (70/56 vs the old ceiling of 3/2), sessions
live, zero Xids. Desktop still black (`MpoSetOk=1`, vnc-grab 0 px). Open:
- **Shared-resource creates fail** (`VK_KHR_EXTERNAL_MEMORY_WIN32 not
  supported`, dxvk_image.cpp:675) — DWM's shared surfaces. ⛔ Sits inside
  UNCOMMITTED dxvk-helios + icd/mesa rework (the HPS2 sharing retirement);
  owner call needed before finishing that in-flight work.
- **CreateDevice wedge persists**: a fresh probe hangs at D3DKMTCreateDevice
  even on the healthy boot. NEW `Nr2CmpStale=1` (an ordered-completion ticket
  dropped as Stale*/Poisoned = a fence dxgkrnl waits on forever) is the prime
  suspect; counter added this session (interrupt.rs).
- Diagnostics deployed: ICD `HNS1` submit/sync-point tracing + refused-payload
  hex, `HAM1` alloc-failure lines, `Nr2NoSchWho` site codes, DXVK association
  validator term logging. ETW autologger `autosession\helios_boot` still
  armed (bincirc 384 MB, C:\tmp\helios_boot.etl) — collect or delete.
Memory: `producer-chain-destroys-and-buffer-undersize-fixed.md`.

## ⚠ GUEST IN A REBOOT LOOP, 2026-08-24 ~02:15 — recover before resuming

QEMU exited during a verification boot and the guest came back looping. Owner is
relaunching **without the Helios GPU** so evidence can be gathered on the basic
display adapter.

**Leading suspect is not the driver.** Six QMP `system_reset`s in ~90 minutes,
several of them mid-boot (graceful `shutdown /r` does not reboot this guest when
the display stack is wedged), is exactly how Windows lands in Automatic Repair.
Last KMD deployed was 22.22.352.0; its diff is small and IRQL-safe on inspection,
which is not a measurement.

**The discriminator:** `C:\Windows\Minidump\*.dmp`. A dump names the faulting
driver; **no dump at all** means Windows never bugchecked and the loop is the
repair path, not us. Run `tools/collect-boot-failure-evidence.ps1` — it gathers
dumps, BugCheck/Kernel-Power events, `setupapi.dev.log`, `ConfigFlags`, the
service `Start` value, `UserModeDriverName`, the DriverStore version list, the
UMD logs and the live counters into `Z:\tmp\bootfail-<stamp>\`, and touches no
device. ⛔ Invoke it with `Invoke-Expression (Get-Content -Raw ...)`: machine
ExecutionPolicy is Restricted, so `& script.ps1` runs and prints nothing.

Rollback levers: `Services\helios_kmd_render\Start = 4` (inert with the GPU
present); backups in `C:\ProgramData\HeliosDeployBackups\<stamp>\`; every prior
version still published in the DriverStore. Last KMD that reached a full
display bring-up: **22.22.350.0**.

---

## ⛔ BLACK DESKTOP, 2026-08-24 — DWM NEVER PRESENTS, AND TWO OF OUR GATES ARE WHY

**Root cause found and two of its links fixed.** DWM composites fine and its
device is created fine; it simply never reaches a present, because this driver
refuses two DDIs and DWM answers by destroying the device. Per-boot UMD log
(`C:\ProgramData\Helios\umd-<pid>.log`, delete before the boot — the file is
appended across boots and pids are reused, which made an earlier read of it
worthless):

    DDI PresentBoundary: entry present=0 present1_single=0 present1_multi=0
      mpo=1 present attempt/success/failure/missing=0/0/0/0 early_refusal=1

| # | gate | evidence | state |
|---|------|----------|-------|
| 1 | `pfnAcquireResource` refused the DDI when the resource-side identity did not verify, and reported `E_INVALIDARG` to the runtime | `WDDM2.1 sync token identity unverified: MissingResource priv=0x0 slot=0x0 token=0x1` — the runtime passes **no resource at all** | FIXED `4884429` — advisory + counted (`sync_token_identity_unverified`), callback forwarded |
| 2 | `pfnPresentMultiplaneOverlay` refused on `StretchQuality != 0` | `failing=[stretch_quality] ... stretch=1 src=(0,0)-(1280,800) dst=(0,0)-(1280,800) clip=(0,0)-(1280,800)` — a 1:1 plane, and 0 is not a defined enumerator | FIXED `f401bc1` — test deleted; the gate already requires src==dst==clip |

⚠ **NOT YET MEASURED.** The build carrying fix 2 was deployed but QEMU exited
during the verification boot, so whether DWM presents after both fixes is
open. That is the next thing to run.

### The instrument that made this tractable

`DDI PresentBoundary` and the per-field refusal log. A gate that covers N
predicates behind ONE message costs a boot per guess; naming the failing field
answered the MPO gate in a single boot. Both refusals sit on DWM's first
present of every boot.

### Deploy notes learned the hard way

- `win_install_umd` **hangs** on its probe step: always pass `-NoProbe`.
- ⛔ NEVER pass `-KillUmdUsers -RestartDevice`: its `pnputil /disable-device`
  timed out and left `ConfigFlags=1` (CONFIGFLAG_DISABLED) on the device, which
  would have Code-22'd the next boot. Recovery: `Set-ItemProperty ... ConfigFlags 0`
  on `HKLM\SYSTEM\CurrentControlSet\Enum\PCI\...`; `pnputil /enable-device`
  answers "already enabled" and does NOT clear it.
- A **boot reverts `UserModeDriverName`** to a previous generation's value, so a
  hotplug + reboot can silently run the PREVIOUS UMD. Verify what DWM actually
  loaded: `(Get-Process dwm).Modules | ? ModuleName -match helios_umd`.
- `pnputil /restart-device` completes in seconds and reproduces the whole
  display bring-up — a full repro without a reboot.

---

## ⛔ THE SCANNED-OUT PRIMARY IS EMPTY, 2026-08-24 (KMD 22.22.350.0)

**The display path is exonerated by positive control. Nothing writes pixels into
the WDDM allocation dxgkrnl hands us as DWM's primary.** One boot, both content
oracles on (`D2ParkPaint=0x40`, `D2PxProbe=1`), commit e2cce68:

| blob | what it is | host readback | guest sample |
|------|-----------|---------------|--------------|
| res 4 | the KMD's parking image, painted 0x40 by the KMD itself | `nonzero 64000 max 64 csum 0x5cccbdb424318000` | `D2PxPark=0x1000040` |
| res 17 | a real DWM primary bind | `nonzero 0 max 0 csum 0` | — |
| res 67 | a real DWM primary bind | `nonzero 0 max 0 csum 0` | `D2PxRid=67 D2PxNz=0 D2PxMax=0 D2PxErr=0` |

Same map, same sampler, same flush path, same boot. `SET_SCANOUT_BLOB`,
`RESOURCE_FLUSH`, the venus blob export, the dma-buf and QEMU's readback all
carry a KMD-written blob end to end and show it on the host. The primary is zero
at **both** ends, so the defect is upstream of the KMD entirely.

DWM is genuinely on Helios: `dwm.exe` (session 1) has `helios_umd.dll` +
`vulkan_virtio.dll` loaded and **no `d3d10warp.dll`**. So the producer is our own
UMD/DXVK/ICD graph, and the open question is which memory DXVK's composited
frame actually lands in — the HRA1 association names the WDDM allocation, and
`a7_schema.rs`'s `VkImportMemoryResourceInfoMESA` operand is patched with a real
resource id by `native_render.rs`, so the intended aliasing exists on paper.
⚠ 281d2b7 asked exactly this and left it open ("whether the zero-copy route can
bind a LINEAR primary is still open, and is the next question"). Answered: the
route binds fine and the buffer is empty.

### The second, independent finding: the plane drains and never comes back

`D2DrnSet=6` — only two reasons ever fire, `ModeChange` first (`D2DrnWh1=3`) and
`SourceInvisible` last (`D2DrnLst=2`) — and after that last `SourceInvisible`
the OS never offers another address (`D2AdmRef=D4AdrRef=D2PlnRef=0`). Every boot
ends: 2-3 real binds → drain → park → `SET_SCANOUT_BLOB(res 0)` → dark, about
11 s after DWM starts. Even a correct primary would only be shown for that
window, so this needs its own answer.

### Next measurement (not yet taken)

Name the object we bind: publish, per real bind, whether the allocation has a
KMD-created `venus_image_id` or only a plain memory blob, its HWA2
swizzle/flags, and whether its resource id was ever substituted into a patched
`VkImportMemoryResourceInfoMESA` operand (`native_render.rs` `patch_order`).
That distinguishes "DXVK's memory does not alias this blob" from "it aliases it
and DWM composites somewhere else".

⛔ **Do not read fossil counters.** The service key is append-only across every
KMD ever installed: 286 live names against 3,951 registry values on 2026-08-24.
`ScFlu`, `ScSet`, `ScRid`, `ScCpy`, `ScWH`, `VsCnt`, `IrqN`, `DpcN`, `AsSub`,
`DspBnd` and ~600 others lost their writers in 60a9988 (2026-08-21) and still
read their last pre-deletion value. The previous session's lead ("D2FlshN=3 but
ScFlu=2") was two fossils. Use `tools/kmd-live-counter-names.sh`, which derives
the live set from `kmd_render/src` and emits the PowerShell that reads exactly
those. Note `& script.ps1` reads as empty on this VM (machine ExecutionPolicy is
Restricted) — use `Invoke-Expression (Get-Content -Raw ...)`.

---

## ✅ CLOSED 2026-08-23 (KMD 22.22.342.0) — `DisplayableFlagMissing`

*Fixed in e44d54d + 281d2b7; `D2AdmRef`/`D4AdrRef` 32 → 0. Kept for the chain and
the two claims it falsified. Its "What the fix turns on" section below is
superseded by the section above: the UMD now claims DIRECT (arm (a)) and the
allocation is admitted, bound, and flushed — and is empty.*

## ⛔ BLACK DESKTOP ROOT-CAUSED 2026-08-23 (KMD 22.22.342.0) — `DisplayableFlagMissing`

`DxgkDdiSetVidPnSourceAddress` refuses DWM's primary **32 times per boot**, and
the reason is now named: `Refusal::DisplayableFlagMissing` (code 0x53).
`D2AdmSL=0x80000` has exactly one bit set, so all 32 are the SAME reason, not a
mixture. The refusals arrive on the `SetVidPnSourceAddress` surface
(`D4AdrOk=2` / `D4AdrRef=32`); the DMA-flip surface is not used at all this boot
(`D4DmaOk=0` / `D4DmaRef=0`).

The chain, entirely guest-side:

1. `umd/src/forward/resource.rs:816` — the UMD's ONLY `Hwa2CreateInput`
   construction hard-codes `direct_scanout_primary: false`.
2. `umd/src/forward/alloc.rs:291` sets `HELIOS_HWA2_FLAG_DISPLAYABLE` only when
   that field is true ⇒ **no UMD-created allocation can ever carry it.**
3. `kmd_logic/src/direct_scanout_admission.rs:396` requires it unconditionally.
4. The only allocation that passes is the KMD's own standard/blank primary
   (`create_allocation.rs:5643`) — which is exactly the blank frame on screen.

⛔ **Two entries below are falsified by this measurement.** "dxgkrnl NEVER issues
`SetVidPnSourceAddress` (0 occurrences in a full ETW slice)" is wrong — it issues
it 34 times. And "the break is between `SetDisplayMode` (S_OK) and any
`SetVidPnSourceAddress`" is wrong — the break is inside our own validator.

### What the fix turns on (OPEN — needs an architecture decision)

`create_allocation.rs:2656-2675` documents two intended arms:

| arm | shape | who |
|-----|-------|-----|
| DIRECT | `DISPLAYABLE` + `SWIZZLE_OPAQUE_OPTIMAL`, zero-copy, QEMU reconstructs natively | the UMD's direct scan-out primary |
| COPY | `LINEAR`, blitted into the KMD-owned linear target — "the fail-safe direction … the proven desktop" | the OS standard primary |

`set_vidpn_source_address_d4` has **no copy arm**: it calls the direct validator
and returns its error, so a copy-path primary can never be admitted there. Either
(a) the UMD must claim DIRECT for DWM's primary — but `direct_scanout_primary`
also flips `swizzle_class` to `OPAQUE_OPTIMAL` (`alloc.rs:340`), so it is not a
one-bit change and the allocation must actually be that layout; or (b) the DDI
needs the copy arm the design describes. DWM's primary is 1280x800 fmt=87,
pitch 5120 (= 1280*4, linear-compatible), size 4587520 (= 5120*896, padded).

### Latent contradiction found and measured OUT (not the current defect)

`direct_scanout_admission.rs:357` refuses **every** immediate flip
(`ImmediateFlipRequested`, 0x4C), while `present_packet.rs`'s DMA-buffer flip
contract exists specifically to serve immediate flips, and `mpo3.rs` advertises
`PLANE_FLIP_IMMEDIATE`. It is not firing this boot because the DMA-flip surface
is unused — but it will refuse every flip the moment that surface is used.

### Instruments added (commit 4bffe1f)

- `refusal_code_and_detail()` in `kmd_logic` maps all 60 `Refusal` variants;
  code = declaration index + 63 (range 0x40..=0x7B), which reproduces the
  fourteen previously published codes exactly. Exhaustive match ⇒ a new variant
  is a build error, not a silent `0x7F` bucket.
- `D2AdmSL`/`D2AdmSH` — the SET of admission codes seen this boot, as a bitset
  over `code - 0x40`. `D2AdmWhy` is last-value and cannot distinguish one
  repeated refusal from a mixture of 32.
- `D2AdmDat` — the last refusal's diagnostic scalar.
- `D4AdrOk`/`D4AdrRef` vs `D4DmaOk`/`D4DmaRef` — the two call surfaces that share
  the admission validator, split. This is what falsified the ETW claim.
- `tools/decode-admission-refusals.py D2AdmSL D2AdmSH [D2AdmWhy] [D2AdmDat]`.

⚠ Deploy trap: `win_install_kmd`'s `devcon update` timed out at 180s and the tool
skipped the reboot, then `shutdown /r` wedged (error 1115, "a system shutdown is
in progress") without rebooting — the publish itself had succeeded. QMP
`system_reset` on `/tmp/helios-tpm/mon.sock` is the recovery (guest reset only).

---

## ⭐ In progress since 2026-08-09: the HPS2 retirement (WDDM 3.2 uplift)

A one-shot, all-or-nothing architecture change across nine repositories,
specified in `docs/HELIOS_PRESENT_SYNC_RETIREMENT.md` (frozen at corrective pass
82). It retires the `helios_present_sync_v2.bin` shared mapping, the private
Escape transport, the read-ledger, and the shared Venus rings — not by replacing
them with another registry, but by **correcting where translated work is
submitted**: DXVK and vkd3d become record-only, and the sealed Venus batch is
submitted as the real WDDM command on the runtime's own context. `WrittenPrimaries`
then becomes truthful and stock DXGI/dxgkrnl carries the DWM handoff.

Planning artefacts, all under `docs/retirement/`:

| File | What |
|---|---|
| `lane-*.md` (6) | per-lane briefs: current source inventory, DELETE/REWRITE/MODIFY/ADD per file, work units, and each lane's blockers |
| `OWNERSHIP.md` | who edits what, the atomic cross-repo pairs, and the activation switch |
| `FINDINGS.md` | ⭐ target measurements that **supersede** the frozen reference |

**Two of the reference's load-bearing assumptions have been falsified by
measurement (`FINDINGS.md`), and both were blockers:**

- **F1 — Core DDI 0116 negotiates on build 26100.** The "Windows 11 26H1 / build
  28000, no fallback" package minimum was an artefact of what WDK 26100's
  `d3d12umddi.h` *declares*. The inbox `26100.8737` runtime accepts the 0116
  token and drives `pfnCreateDevice` with it. The remaining input was a bindgen
  regen against WDK 28000 — **done 2026-08-10**, against the kit now *installed*
  on the VM rather than a staged tree (see "The build environment moved to WDK
  28000" below).
- **F2 — a CpuVisible memory segment does not Code-43.** §10.7's required HLM1
  segment shape is the shape the CLAUDE.md invariant called ETW-proven fatal.
  `BarSegFlags=0x02` starts `OK/CM_PROB_NONE` and `helios_paintcap` shows a
  fully composited live desktop. The invariant is corrected in place; the KMD
  memory lane is unblocked.

Both are bounded in `FINDINGS.md` — read the bounds before building on them.

**Landed so far:** `protocol/` carries the complete new wire ABI (HWA2, HOB1/HOS1/
HOC1, HQA1/HTS1, HVC1/HNR2/HVM1/HVR1, HPM1/HLM1, the §12.3 ETW schema) with
compile-time offset assertions and four C mirrors; `wddm_legacy.rs` keeps the
pre-retirement symbols alive so `kmd_render`/`umd`/`umd12` still build while each
migrates; vkd3d's Wine-Escape and `\\.\SharedGpuResource` transports are gone.

⛔ **F5's QEMU HPM1 protocol remains declined and parked.** Its three commits
remain preserved on `helios/hpm1-parked`; K2a did not revive their negotiation,
paging packets, or BAR-placement model. The owner later authorized one much
smaller, scoped exception for the documented WDDM shared-backing path:
`qemu-helios` was rebased onto upstream `master` and now imports a Venus
`VIRTIO_GPU_BLOB_MEM_GUEST` resource from the exact guest pages supplied by
`ShareBackingStoreWithKmd`. The five K2a commits are 143 additions / 32
deletions, require no virglrenderer fork, and keep the kernel's stock udmabuf
`list_limit=1024`; the 4 MiB role-1 pool coalesced to 143–853 ranges in measured
runs. This is a K2a backing-store bridge, not an HPM1 protocol reopening or a
general license to move retirement work into QEMU.

**Also landed:** `umd12`'s bindgen regenerated against WDK 28000 (355 `_0112` /
346 `_0116` symbols, `HRTFENCE`,
`pfnCreateNativeFenceCb`/`pfnOpenNativeFenceCb`).

⭐ **The WDDM 3.2 source/boot boundary, K2a's shared-backing CPU view, and K11's
per-session host-transport boundary are crossed, but the package remains
runtime-unadmitted.** Do not infer a working display from package state or the
successful K2a/K11 target exercises:

| Component | State | Why |
|---|---|---|
| `wddm_surface.rs` `SURFACE` | `Wddm3_2GpuMmu`; `KMD_D2_OWNER_ENABLED` and native-fence advertisement derive solely from it | D9's terminal callback/capability package cold-loads on build 26100. WDK 28000 remains only the compile-time binding authority. |
| K2a CPU view | WDDM `ShareBackingStoreWithKmd` + system-memory shared allocation + `DxgkDdiSetAllocationBackingStore`; roles 1–3 only | KMD 22.22.288.0 / `oem120.inf` exercised the exact 4 MiB renderer alias in both directions and passed role, repetition, wrong-process, stale-handle, and process-teardown probes. This does not admit DWM. |
| K11 session transport | One stock Venus context/object namespace and one private 4 KiB host-reply target per exact live HTS1 session; role-1 HVM1 remains the sole HVR1 carrier | KMD 22.22.296.0 / `oem128.inf` returned actual finite host INIT replies, kept simultaneous same-process sessions distinct, isolated another process, drained normal/abrupt/reset teardown, and left `TsSlotStuck=0`. Allocation-backed and GPU-dependent work still refuses. This does not admit DWM. |
| Bounded post-K9 executor | The current session owns bounded immutable HNR2 custody, generated A3/A4 schema validation, exact allocation-use closure, private-copy host-resource patching, nonzero-endpoint stock-Venus execution, same-context imported-fence ordering, and terminal completion through K9 | Root `e8819c1`; source/build validated only. The installed KMD was not replaced, restarted, or exercised, so the prior K11 target evidence does not validate this executor. |
| Mesa A3/A4 | One direct A1/K11 session per Windows `vn_instance`; escape-free HVM1/C57/HNF1 renderer and mode-owned record/normal HNR2 queue path | Mesa `2c2763b8b1a` + `5cbc0254f43`; Windows ICD builds and source/mutation gates pass. That checkpoint's command-buffer refusal is superseded by the A7 row below. No ICD was installed or exercised. |
| Mesa A5 and direct consumers | The fixed 112-byte/eleven-slot A5 table is unchanged. Package generation 3 adds the separate immutable 72-byte HRA1 creation/bind record; DXVK and vkd3d consume the sole explicit package-owned direct instance and carry each outer-UMD token to the exact Mesa allocation. No selected Helios path searches `vulkan-1`, creates a second instance, or owns an allocation registry. | Protocol `eec9564`; DXVK `1cf7e631`; vkd3d `9a2716c0`; root UMD integration `0fe5677`. Source/build validated only; nothing was installed or target-exercised. |
| Outer UMD/KMD execution | D3D11 owns a bounded device allocation set and resolves each token to its exact Render allocation-list index with current HWA2 generation; D3D12 resolves it to exact current GPUVA plus offset and generation. Both own HQA1/HQC1 context lifetime and emit field-by-field HOB1; D3D12 also emits exact HOS1. KMD validates the generated A7 subset, patches only its private copy, executes on the existing session endpoint, and completes through K9. | Protocol support `ccd2891`; minimum generated KMD admission `eccfd19`; K4 const-open/shared-backing follow-up `d75f649`; UMD11/12 `0fe5677`. Source/build validated only; the installed KMD remains unchanged. |
| Mesa A6-A9 | The fail-closed A6 profile is preserved; A7 now classifies control/deferred/GPU-dependent work and emits complete exact-token use and typed-operand closure with zero wire host-resource bytes. A8 makes the selected Windows generic-ring operations unreachable and folds replies into exact HNR2 COMMIT. A9 prunes the lower-ICD build without wiring the present layer. | Mesa A6 `d07d1d13687` + `478c71a0fff`; A7 `ae9c9f4c89d`; A8/A9 `fa61439bfd7`. Source/build validated only; no ICD was installed or exercised. |
| `VK_LAYER_HELIOS_present` B0-B9 | Mesa's HPS2 WSI writer is gone; the separate layer owns the loader/dispatch, exact-LUID D3D12/DXGI image graph, Vulkan-exported Ready/Release timelines, nine-state acquire/present machine, alias lifetime, retirement, and generated manifest. The lower ICD remains separate and escape-free. | Mesa B0 `bb7a787a5a1`; B1-B5 `e06abf3025f`; B6 `11d723ea10b`; B7/B8 `51054338012`; B9 `871c62bf8a0`; bounded review fix `8b3c9359b5a`. Source/build validated only; the layer was not registered, installed, or target-exercised. |
| Broad HPS2 source demolition and K14 | DXVK/UMD HPS2 and reverse-reader producers/consumers, KMD Escape/HAP/read-ledger/present-stream state, obsolete protocol/host carriers, and source packaging assumptions are gone. K8/K10 retain the D9/K2a/K7/K9/K11 graph and reverse teardown. | DXVK `418f5745`; root `b955a2c`, `60a9988`, `5e1fbda`, `e7bdb0f`, `3663956`, `e96b304`, `ce3765a`, `5901f55`, and K14 `dcf0e1e`. Generation 4 / KMD 22.22.297.0 package source requires one complete lower/layer/UMD payload. Source/build validated only; no package was assembled, installed, registered, or target-exercised. |

**THE BROAD HPS2 SOURCE DEMOLITION LANDED AND PASSED SOURCE/BUILD VALIDATION
ONLY; NO RETIREMENT PACKAGE OR PRESENT LAYER WAS INSTALLED, REGISTERED, OR
EXERCISED ON THE TARGET, THE WDDM 3.2 DISPLAY PACKAGE REMAINS
RUNTIME-UNADMITTED, AND RUNTIME HPS2 RETIREMENT AND PRODUCTION CORRECTNESS ARE
NOT ESTABLISHED.**

**AUTHORIZED F21 CONSUMER CUTOVER AND MESA A7-A9 LANDED (2026-08-21).** The
owner expanded the prior lower-ICD-only boundary to the bounded direct DXVK/
vkd3d plus UMD11/UMD12 creation and submission graph. HRA1 is the one immutable,
protocol-owned process-local association record; it is not a callable registry,
fallback key, or change to the A5 table shape. Outer tokens are nonzero,
monotonic, device-generation-owned, carried during direct creation/bind, and
removed before object reuse. D3D11 and D3D12 resolve them only through their
exact owning device/resource state before assembling complete HOB1/HOS1 into
the runtime-approved callback windows.

Mesa A7 removes the former command-buffer refusal only after complete exact-
allocation and typed-operand closure, keeps host-resource operand bytes zero on
the wire, performs the exact context join for GPU-dependent work, and retains
named refusals for missing, foreign, stale, duplicate, overflowing, or
unclassified state. A8 retires only the selected Windows generic-ring path;
generic non-Windows Venus remains. A9 completes the lower-ICD wiring and keeps
the present layer unwired. The minimum generated KMD decoder admits only the
required A7 subset over existing HNR2 and adds no host/QEMU protocol or executor.
That historical checkpoint stopped before `VK_LAYER_HELIOS_present`, broad
HPS2 demolition, packaging, deployment, or target exercise. The current table
and the broad-demolition checkpoint below supersede that stop boundary.

**`VK_LAYER_HELIOS_present` LOADS, RUNS, and now BUILDS A WSI DEVICE**
(`FINDINGS.md` F7 + its two addenda). Staged by
`tools/install-helios-present-layer.ps1`, driven by
`tools/run-helios-layer-app.ps1` (which carries a `-NoLayer` control arm),
interrogated by `tools/vk_external_handle_probe.cpp`.

Both §10.3 capability gates are closed and the private tag call
`vkSetHeliosPresentableImageHELIOS` is implemented (declared once in
`icd/mesa/src/vulkan/helios_private_wsi.h`, published only through
`vn_GetDeviceProcAddr`). **§10.3's image chain runs end to end**: four D3D12
committed textures import into Vulkan per swapchain, through
`CreateSharedHandle` → `vkGetMemoryWin32HandlePropertiesKHR` → dedicated import
→ bind → tag.

⭐ **CLOSED by measurement — the Ready/Release fence import is impossible, and
backwards** (`FINDINGS.md` **F8**, `tools/d3d12_shared_fence_probe.cpp`). An
`ID3D12Fence::CreateSharedHandle` handle is refused by
`D3DKMTOpenSyncObjectFromNtHandle2` with `STATUS_INVALID_PARAMETER` **on every
adapter on the box, including both Microsoft Basic Render Driver ones** — so it
is not a Helios defect and no change to `icd/mesa`, the KMD or the layer can fix
it. The controls make that readable: our own monitored fence, shared with
`D3DKMTShareObjects`, opens with `STATUS_SUCCESS` through the identical call in
the same process from two devices, and `ID3D12Device::OpenSharedHandle`
round-trips the D3D12 handle every time, so the call works and the handle is
valid. The object type is `DxgkSharedSyncObject` in both cases.

The actionable half is the last control: **`ID3D12Device::OpenSharedHandle`
accepts a monitored fence *we* created and shared, on Helios.** The two sides
can share a fence in exactly one direction. ⇒ §10.3's Ready/Release fences must
be **created by the ICD and opened by the D3D12 side**, not imported from
`ID3D12Fence` — a `lane-mesa` protocol change needing no new ICD capability,
since `helios_wddm_sync_create` + `helios_wddm_sync_share_nt` already build that
object. Nothing above `vkCreateSwapchainKHR` has run: no acquire, no present, no
frame.

That correction is now source-closed in Mesa B1-B5 `e06abf3025f`: the layer
creates lower-Vulkan-owned exportable timeline semaphores, exports each
`D3D12_FENCE_BIT` NT handle with `vkGetSemaphoreWin32HandleKHR`, opens it on the
exact owning D3D12 device, and closes the transient handle on every path. The
historical target measurement above was not repeated, and the new source has
not been registered, installed, or exercised.

⚠ Falling out of that work: **`ID3D12Fence::CreateSharedHandle` could never
have worked on Helios** — vkd3d refuses a shared fence unless the driver
reports `D3D12_FENCE_BIT` in `exportFromImportedHandleTypes`, and it did not.

The layer is deliberately **not** registered machine-wide: an implicit layer
enters dwm's `dxvk-helios` instances, which §2 item 8 forbids. ⚠ Check the
build with `ninja <the layer target>` — **mingw-w64 g++, the compiler Mesa is
written for**; a clang-cl side-check was tried and deleted (`REVIEW-ROUND-1.md`
review 4).

⛔ **The Vulkan loader ignores `VK_LAYER_PATH` / `VK_ADD_IMPLICIT_LAYER_PATH` /
`VK_INSTANCE_LAYERS` / `VK_DRIVER_FILES` in an elevated process, silently**, and
`win_exec` is elevated. Any Vulkan env-var experiment must go through a
scheduled task at RunLevel Limited (F7). This also makes
`tools/install-helios-icd.ps1`'s `VK_DRIVER_FILES` smoke test inert.

## ⭐ Sequencing decision 2026-08-10: the KMD lane goes next, and it may break the desktop

Asked to choose between finishing the Mesa WSI slice (visible progress, off the
critical path) and starting the KMD long pole (on the critical path, breaks the
working stack), the owner chose the KMD: *"its a dev box, i dont care if the vm
burns to ground"*.

⇒ **A working composited desktop is no longer a constraint on retirement
work.** `OWNERSHIP.md` §3 gates the activation switch on three KMD deliverables
— the display lane's complete MPO3/Display-Core table, a complete native-fence
DDI surface, and the armed cold-DWM admission gate — and `lane-kmd-core.md` K1
deletes Escape, the blob map and the CPU host aperture that the current stack
runs on. The retirement cannot be half-landed; that is now accepted rather than
routed around.

**Order:** K4 (allocation object model — HWA2 create-time descriptor, HVM1
roles 1–4, HOC1) is the hub. Mesa's C57 import is an HWA2 **reader**, the D3D12
lane's adopt path is the **writer**, and K3 (paging DMA) and K6 (HNR2) both sit
downstream of the allocation model. The D3D12 import landed on 2026-08-10
validates the *pre-retirement* `helios_wddm_open_identity` blob and is expected
to be re-pointed at HWA2 by K4.

### ⭐ K4 IS AUTHORED, ACROSS FIVE COMPONENTS, AND EVERY ONE OF THEM BUILDS

Landed 2026-08-10 as three commits — the protocol/model/gates half, the atomic
consumer flip, and the doc corrections. It is deliberately **the whole
allocation-identity subsystem**, not the KMD unit: recon showed K4 alone is a
flag day, because the moment `dxgkddi_create_allocation` stops accepting the
96-byte legacy pair, every producer that still sends one is refused.

| component | what it now does | verified |
|---|---|---|
| `protocol` | HWA2 gains `validate_create_input`/`validate_create_output` sharing one cross-field core with `validate`; HVM1 gains `from_private_data` | 146 tests (was 140) |
| `kmd_logic` | the executable state machines a single-record validator cannot express — the input→output transition, the generation lifecycle, HVM1 roles, HOC1 pools, **and the K5 HTS1 session** | 263 tests (was 211 at K4, 189 before it) |
| `kmd_render` | HWA2 written at create, HVM1/HOC1 admitted, adoption + open-restamp + VidMm tracker deleted, new `adapter/allocation_object.rs` | `cargo check` exit 0, **22 warnings — the same count as the pre-change baseline** |
| `umd` + `umd12` | both re-pointed as HWA2 producers, and both now validate the KMD's write-back, which nothing did before | release build exit 0 |
| `icd/mesa` | reads HWA2 from `protocol/include` — the hand-mirrored 48-byte records are gone, so the header's per-field `offsetof` asserts now fire in the ICD's own TU | ninja green |
| `tools/retirement-gates.sh` | the two §8 gates that were named but never written, plus A2's encoder gate and K5's session gate | 10 gates, ALL PASS |

⛔ **No HWA2 path has run.** Every one of them is *implemented but never
exercised* — `METHOD.md`'s distinct third state, not "done". No deploy:
**saturation** is the gate, not any single round (three have run; the arithmetic
and the full state classification are below), and `OWNERSHIP.md` §3's activation
conditions are not met (F9). ⚠ The blanket *"nothing in the changeset has ever
run on the target"* is **false** and keeps being repeated: `fa8489e` is inside
`d1c820a..HEAD`, and F6 measured its slot audit on the target in both arms. Scope
the claim to the HWA2 paths, which is where it is true.

⇒ **When it does deploy, every Win32 shared-memory import and export refuses and
the desktop dies**, until mesa **A3** lands. That follows from §10.3 forbidding
HWA2 to carry a host `resid` or a Vulkan memory-type index — both load-bearing
in the ICD's import — and A3 is XL and unstarted. It is the accepted cost of the
sequencing decision above, not a regression.

⛔ **This paragraph said "every venus `vkAllocateMemory` fails" until round 2 of
the review measured it. That is broader than the code supports** — see
`K4-CONTRACT.md` §5.1 for the path-by-path table. Exactly two of
`vn_AllocateMemory`'s five arms refuse: an allocation carrying
`VkImportMemoryWin32HandleInfoKHR` (`HELIOS_A3_GAP_IMPORT_WIN32`), and one whose
`export_handle_types` includes `..._OPAQUE_WIN32_BIT`
(`HELIOS_A3_GAP_EXPORT_ADOPTION`, refused before `D3DKMTCreateAllocation2`).
The plain arm, the resource-id import and the dma-buf import **still allocate**.
The desktop still dies, because DWM's shared surfaces and the D3D interop path
are exactly the OPAQUE_WIN32 traffic — but a plain venus render allocation
working is *not* evidence the deploy went well, and the old wording predicted
otherwise. ⚠ `icd/mesa`'s commit message `23ab160` carries the broad wording and
cannot be amended; this is the correction of record.

⚠ **A toolchain blocker was found and fixed on the way**, and it was not ours:
published `wdk-sys` 0.5.1 pins bindgen 0.71.1, which under libclang 22 emits a
size-1 opaque type *and* a real-layout assertion for every forward-declared
struct — 40 underflowing const-evals. Proven third-party by building a scratch
crate whose only dependency is `wdk-sys` and which contains no Helios code. It
had been latent since the LLVM 22 upgrade, masked by an Aug-8 cached `types.rs`;
the first fingerprint invalidation exposed it. Downgrading is measurably *not*
the fix (MSVC 14.44 regenerates the identical 39 opaque types), so
`kmd_render/Cargo.toml` now `[patch.crates-io]`-es the whole windows-drivers-rs
family to upstream main, which is already on bindgen 0.72.1, pinned to an exact
rev.

### ⭐ K4's contract is written down, and it corrects the plan in five places

`docs/retirement/K4-CONTRACT.md` (2026-08-10) is **normative for the allocation
identity subsystem** and is the entry condition `METHOD.md` phase 1 requires. A
ground-truth survey found the plan resting on facts the tree does not support:

- **HWA2 had no create-*input* contract.** `validate()` requires a nonzero
  `allocation_generation`, so it could only ever validate the output side, and
  nothing said what a UMD may legally *send*. HVM1 and HOC1 both carry the
  two-stage pair; HWA2, the record K4 is built on, was the odd one out. The
  contract adds `validate_create_input`/`validate_create_output` and pins the
  field partition — the UMD supplies a request, the KMD validates it in full and
  then writes all 168 bytes, which is the only reading consistent with both
  "the buffer is `[in/out]`" and "the KMD cannot invent texel dimensions".
- **`adapter/kobj.rs` holds zero allocation state** — its own module doc says
  *kernel dispatcher objects*: the venus/scanout mutexes, the HPD worker and the
  VSync timer/DPC pair. The brief read the filename. K4 drops it and adds
  `adapter/allocation_object.rs` instead, because there is today **no** file
  that owns the KMD allocation object.
- **`adapter/backing.rs` has 14 of its 18 call sites outside K4**, so its
  deletion is re-sequenced to K3 with a display-lane request.
- **The ICD re-point is not a substitution.** HWA2 deliberately carries no host
  `resid` and no Vulkan memory-type index, and both are load-bearing in the
  ICD's import today. What replaces them is a *mechanism* — mesa A3 plus K6 —
  so K4's obligation there is a loud named refusal, never a bridge.
- **A third cross-repo atomic pair**, which `OWNERSHIP.md` §2 was missing: the
  ICD hand-declared **four** records (three 48-byte plus one 96-byte) with
  `sizeof`-only `_Static_assert`s and included nothing from `protocol/include`,
  so the KMD-side and ICD-side edits are one change. ⭐ **The layout half is now
  a compile-time gate**: all four local declarations are deleted,
  `vn_helios_hwa2.h:52` includes `protocol/include/helios_wddm.h` so its 82
  `offsetof` assertions fire in the ICD's own TUs, and
  `icd/mesa/src/virtio/vulkan/meson.build:163-171` `error()`s if the header is
  not reachable. What is still hand-mirrored, and still one change, is the
  *rules* — `vn_helios_hwa2.c` transcribes `from_private_data` and the two stage
  validators from `protocol/src/wddm.rs` with nothing checking the
  transcription.

⚠ K4's stated dependency K2 is **rescoped, not blocked**, and `K4-CONTRACT.md`
§4 says so — HPM1 was declined rather than deferred, so K2's guest-side BAR
admission and segment table were never waiting on anything (F5 Consequence: *"no
lane in flight has"* a QEMU dependency).

⛔ **This paragraph reproduced a WITHDRAWN draft of §4** — it said K4 "admits
role 4 without satisfying it", and attributed that to the normative doc, which is
what made it worth correcting rather than merely stale. §4 was amended after the
review found *both* readings built on that draft were wrong: the KMD refused only
role 4 while admitting roles 1–3 onto a segment the table may not report, and the
`kmd_logic` model refused all four. **The ruling is "check the segment, do not
hardcode the role"**: the KMD verifies the role's `preferred_segment` is actually
in the table it reports and refuses per role with a named counter when it is not.
The code agrees: `rg -n 'segment_is_reported|AcSegRole' kmd_render/src/` shows
`fn segment_is_reported` with two call sites and the four counters
`AcSegRole1`…`AcSegRole4` selected per `Hvm1Role`. (Line numbers omitted on
purpose — `create_allocation.rs` was in flight while this was written, and a
cite into a moving file is stale before it is read.) A hardcoded role number
would have been a claim about K2's schedule embedded in kernel code.

### ⭐ Three review rounds have run, and the earliest saturation can arrive is round 5

`METHOD.md` §2 phase 2 is the loop; `METHOD.md` §3 is the test it turns on.

| round | scope | lenses | raw → survived | recorded in |
|---|---|---|---|---|
| 1 | `protocol`, `kmd_render`, `vkd3d-proton-helios` | — | see the doc | `docs/retirement/REVIEW-ROUND-1.md` |
| 2 | the whole changeset | 6 rotated, a skeptic per lens | 57 → 24 | ⚠ **only commit `2e04189`'s message**, plus `d803346` for the blocker it split out |
| 3 | the whole changeset | 8 rotated — security/§15, failure-policy §10.9 row by row, deployment-readiness, kernel-safety, producer/consumer field agreement, instrument attribution, claim-integrity-at-HEAD, gate-defeat — a skeptic per lens, plus a completeness critic | 53 → 30, **plus 6 confirmed acceptance-gate defeats** | `docs/retirement/REVIEW-ROUND-3.md` |

⚠ **Round 2 has no review document.** It is legible only as `git log -1 2e04189`,
which is not where anyone looks. Recorded as a gap rather than back-filled: a
reconstruction written from memory a round later would be a worse artefact than
the commit message it paraphrased.

⛔ **Round 3 is not dry, so `METHOD.md` §3 criterion 1 cannot be satisfied before
round 5.** The criterion is two *consecutive* dry rounds with **different** lens
compositions. Round 3 produced 30 surviving findings, so it is not the first dry
round; round 4 reviews the repairs those 30 force — new code, read for the first
time — so the best case is that round 4 is the first dry round and round 5 the
second. `REVIEW-ROUND-3.md` states the same conclusion in its own words. Two more
rounds is the **floor, not the estimate**: rounds 1, 2 and 3 each found real
defects in the previous round's repairs.

⚠ **Known weakening of all three rounds, recorded rather than glossed:** reviewer
and author are the same party, which `METHOD.md` §2 phase 3 forbids in the
parallel-lane case. What was actually done instead — each lens reads the whole
changeset rather than a slice, each finding goes to a *separate* skeptic
instructed to default to refuted, and the repairs are routed to authors over
disjoint file sets, none of whom reviewed — is a mitigation, not the control.
Weigh the findings accordingly, and see `REVIEW-ROUND-3.md`'s own note.

⛔ A **gate-defeat lens is now standing composition, not a one-off.** Round 2's
completeness critic did not argue that the §8.5 gate was weak — it wrote the
defeating patch and ran it (`2e04189`: a three-line alias hop passed the old gate
with exit 0), and round 3 confirmed six more acceptance-gate defeats the same
way. ⇒ **Ask a reviewer to defeat a gate, not to assess it.**

The runtime side is unchanged: §18's runtime gates remain unexecuted, except the
WDDM 3.2 slot audit, which has run on the target in **both** arms (`FINDINGS.md`
F6) — see state A below, because that row is regularly mis-filed as unexercised.
The QEMU review is moot (F5).

**Everything Linux-verifiable is green**, and it is now one command:
`tools/retirement-gates.sh` — now **10 gates, all PASS**: protocol tests, Rust↔C
ABI parity, the C mirrors compiling, `kmd_logic` tests, slot-audit staleness,
K4 §8.5 (open writes no private byte), K4 §8.6 (the retired identity symbols
survive only as tombstones), the cross-repo
`VKD3D_HEAP_FLAG_HELIOS_VENUS_EXPORT` mirror, **A2's HNR2 encoder gate**, and
**K5's HTS1/HQA1 session gate**. **`protocol` 146 tests, `kmd_logic` 263** — the
"140 / 189" that stood here was the pre-K4 count and already disagreed with the
component table above it in this same file.
Separately: `tools/umd12-host-check.sh`, vkd3d ninja, QEMU ninja.
Note there is no
workspace root — build `protocol` from `protocol/`, not with `-p` from the repo
root.

### ⭐ What K4 will actually do on deploy — the state classification `METHOD.md` §3 criterion 6 requires

Criterion 6 requires the changeset's own report to distinguish **implemented /
refused / unreachable / implemented-but-never-exercised**. K4 does not fit four
buckets: "implemented but never exercised" splits into ungated and knob-gated,
which have completely different blast radii; a whole class has **no producer
anywhere**, which is dead rather than unexercised; and one subsystem is
implemented *twice*. Seven rows below, each mapped to its METHOD state and each
re-measured against HEAD rather than inherited.

#### A. Implemented **and exercised on the target** — do not re-file this as unexercised

* **`DriverEntry` → `ddi::wddm32_slot_audit::verify`** — the 192-slot audit
  (`SLOT_COUNT = 192`) that refuses the load with
  `STATUS_DEVICE_CONFIGURATION_ERROR` (`0xC000_0182`, written as a literal) on any
  disagreement, before `DxgkInitialize` ever sees the table. It ran, it passed,
  and its refusal arm was proven with one deliberately flipped row
  (`FINDINGS.md` F6 — `22.22.261.0` refused at index 187 with `S1=0x0DA010BB` /
  `S2=0x0DA02002`; `22.22.262.0` `OK`/`CM_PROB_NONE` with a composited desktop).
  ⭐ **And the table it audited is the table at HEAD:**
  `git log d1c820a..HEAD -- kmd_render/src/lib.rs` ends at `fa63ba6` and
  `-- kmd_render/src/ddi/wddm32_slot_audit.rs` ends at `97ad14b`, both **older**
  than F6's measurement commit `d76b137`, so nothing K4 landed afterwards touched
  `build_ddi_table()` or the classification. Bound, F6's own: one row, one of the
  two failure arms (`0x0DA0_2001`, registered-but-classified-unreachable, is still
  unexecuted), and the walk was proven to reach index 187 of 192, not 191.

#### B. Implemented, never exercised, **FIRST to run on deploy, and ungated**

The highest-blast-radius unexercised code in the changeset. All of it is on DWM's
and GDI's own boot path and none of it is behind a knob — there is no arm in which
it is off and the desktop still runs.

* **`DxgkDdiGetStandardAllocationDriverData`** — the KMD now *authors* an HWA2
  (`PRIV_SIZE = HELIOS_HWA2_BYTES`) for the OS standard allocations:
  `SHAREDPRIMARYSURFACE`, `SHADOWSURFACE`, `STAGINGSURFACE` and `GDISURFACE`
  (including the OPTIMAL GDI-texture arm, which has no linear row layout).
* **`DxgkDdiCreateAllocation` → `admit_hwa2` → { `classify_hwa2` →
  `build_backing` → `allocation_object::mint` → the `validate_create_output`
  write-back }.** The write-back *model* is measured sound (F10 — dxgkrnl does
  propagate the KMD's create-time private data back to the creating UMD); the
  path through it is not.
* **`umd/src/forward/alloc.rs`**, the D3D11 HWA2 producer — **no kill switch
  exists.** `grep -i hwa2 umd/src/knobs.rs` is empty (verified), and none of the
  ten knobs that file does declare gates the allocation path. `UmdD3D12` is the
  D3D12 switch and covers none of this. dwm loads this UMD at boot.
* **`DxgkDdiOpenAllocation` → `read_open_descriptor`** and its `OaHwa2Rej`
  counter (which counts only buffers that were HWA2-shaped and still failed — a
  non-HWA2 168-byte record is not counted, deliberately).

#### C. Implemented, never exercised, behind a **default-OFF** knob

* The **entire `umd12` HWA2 producer** — `BoolKnob::new(c"UmdD3D12", false)`
  (`umd12/src/knobs12.rs`), the D3D12 kill switch.
* The **`Umd12CoreDdi=116` arm** — absent ⇒ **110**, and an unrecognised value is
  a *counted* refusal that falls back to 110 (`CoreDdiArm::selected`,
  `umd12/src/adapter12.rs`), never a token passed through to the runtime.
* **`VK_LAYER_HELIOS_present`** — 4135 lines
  (`icd/mesa/src/vulkan/helios-present-layer/helios_present_layer.cpp`, plus a
  243-line header), deliberately **not** registered machine-wide (an implicit
  layer would enter dwm's `dxvk-helios` instances, which §2 item 8 forbids). It
  loads only through `tools/install-helios-present-layer.ps1` +
  `tools/run-helios-layer-app.ps1`.

#### D. D9 local-source state (runtime admission still absent)

* **All six native-fence DDIs** — `Create` / `Destroy` / `Open` / `Close` /
  `UpdateMonitoredValues` / `UpdateCurrentValuesFromCpu` are registered, and
  `NATIVE_FENCE_ADVERTISED = matches!(SURFACE, Wddm3_2GpuMmu)` is now **true in
  the local source package**. K7 closes F9 with the complete six-slot table,
  per-adapter feature/LUID/lifecycle admission, HNF1 validation, correlated
  interrupt edge, and teardown ordering. The installed driver has not changed,
  so movement of the 21 counters (`NfCreateOk` … `NfLiveLocal`) is not claimed.
* **The three residency / `Flags2` union writes** —
  `set_ExplicitResidencyNotification` (on the **WDDM2_0** flags word, *not* on
  `Flags2`, so grepping `Flags2` will not find it), `set_DisablePartialResidency`
  and `set_RestrictedToSingleSegment`. The 3.2 source surface no longer makes
  `Flags2` inert, but these writes remain producer-unreachable because the HVM1
  placement arm still has no producer (state F). K4-CONTRACT §8 obligation 8
  therefore remains unexercised until measured on the target; an unchanged
  residency trace is *not* evidence that the writes are missing.
* **Allocation/native-fence generation invalidation is now paired.** TDR,
  Stop/Remove, failed/skipped Start and reset boundaries call
  `ddi::native_fence::invalidate_all` before
  `adapter::allocation_object::invalidate_all`, ahead of device-lost
  publication. This closes source request X1, but the ordering remains
  runtime-unadmitted with the rest of D9.

#### E. Refused loudly with a named counter (METHOD state: *refused*) — this part genuinely works

* **D3D11 open** — `hwa2_open_needs_mesa_a3` → `E_FAIL`
  (`umd/src/forward/resource.rs`, counter declared in `umd/src/forward.rs` and
  published in the `DDI refusals:` block).
* **The ICD's A3 gaps** — `HELIOS_A3_GAP_IMPORT_WIN32` and
  `HELIOS_A3_GAP_EXPORT_ADOPTION`, each with a self-explaining site name and
  reason string in `vn_helios_hwa2.c`, returning
  `VK_ERROR_INVALID_EXTERNAL_HANDLE`. (Plus `HELIOS_A3_GAP_HANDLE_PROPERTIES`,
  which is in `vkGetMemoryWin32HandlePropertiesKHR` and is not an allocation at
  all, and `HELIOS_A3_GAP_VIDMM_TRACKER`.)
* **D3D12 present** — `PresentIdentityNoResourceId`
  (`umd12/src/forward12/present12.rs`), which names mesa A3 rather than
  fabricating an identity §10.3 forbids the UMD to supply.

#### F. **No producer exists anywhere** — dead, not merely unexercised

Everything here compiles, is asserted, and is reached by nothing. Measured at
`2e04189`, `.rs` + its `protocol/include` C mirror where one exists. ⚠ **The line
counts below are a snapshot and will drift** — re-derive with `wc -l` rather than
citing them onward; what does not drift is that **nothing outside `protocol/`
calls any of it**, and that is the load-bearing half:

| dead surface | size | measurement |
|---|---|---|
| `protocol/src/translator_dispatch.rs` + `helios_translator_dispatch.h` | 4298 + 2264 = **6562** | **0 of its 65 top-level `pub` items** is referenced by `kmd_render`, `kmd_logic`, `umd`, `umd12`, `umd_common`, `icd/mesa/src`, `dxvk-helios/src` or `tools` |
| `protocol/src/translation_session.rs` + `helios_translation_session.h` | 2181 + 580 = **2761** | only `protocol/src/lib.rs` itself uses it (`check_package_generation` → `check_generation_match`) |
| `protocol/src/diagnostics.rs` + `helios_diagnostics.h` | 1414 + 696 = **2110** | the §12.3 ETW schema with **zero emitters**: no `EventWrite` / `TraceLoggingWrite` / `McGenEventWrite` anywhere in `kmd_render`, `umd`, `umd12`, `umd_common` or `protocol`. (The only `TraceLoggingWrite` in the tree is upstream Mesa's `gallium/frontends/mediafoundation`, unrelated to Helios.) |
| `protocol/src/physical_memory.rs` | **3155** | **55 of its 57** top-level `pub` items have no consumer; the two that do are `HELIOS_SEGMENT_ID_APERTURE` and `HELIOS_SEGMENT_ID_HLM1`. This is the HPM1 surface F5 **declined** |
| the HVM1 / HOC1 admission surface in `ddi/create_allocation.rs`, and its `AcSegRole1`…`AcSegRole4` + `AcSegHoc1` counters | — | nothing outside `protocol/` **constructs** an HVM1 or HOC1 record; `kmd_render` is a consumer and `kmd_logic` a model. The producer is mesa A3 (HVM1) and K5/K6 (HOC1), neither started |

≈ **14.4k lines** carried by a changeset in which none of it can run. That is a
deliberate consequence of authoring the protocol lane ahead of its consumers — but
it is also the largest single reservoir of unverifiable claims in the tree, and
every review round has found defects in it.

#### G. The fifth state — **implemented twice, exercised once, and the exercised copy is the one nothing ships**

⛔ **`kmd_render` calls ZERO functions from `kmd_logic::allocation_identity`.**
Measured per module: of `kmd_logic`'s 17 `pub mod`s, `kmd_render` references 15 —
`scanout_lease` 16 refs, `present_stream` 14, `snapshot_bind` 11,
`scanout_publish_txn` 10, `scanout_presentation_epoch` 6, … — and exactly two
have zero: `scanout_fast_bind` and **`allocation_identity`**.

`allocation_identity` implements precisely the decisions the KMD makes —
`hwa2_admit_create_input`, `hwa2_stamp_create_output`, `hwa2_admit_open`,
`hwa2_echoed_fields_equal`, `hvm1_admit_create_input`, `hvm1_admit_placement`,
`hvm1_written_flags_match`, `hoc1_admit_create_input`, `Hoc1Pool::reserve` /
`::retire`, `GenerationCounter::mint` / `::on_adapter_reset`,
`classify_alloc_private_data`. `kmd_render` re-derives every one of them in
`ddi/create_allocation.rs` (`classify_hwa2`, `admit_hwa2`, `admit_hvm1`,
`admit_hoc1`) and `adapter/allocation_object.rs` (`mint`, `is_current`,
`invalidate_all`). **It is a parallel re-implementation, not the shipped logic.**

⇒ **Consequence for acceptance.** `K4-CONTRACT.md` §8 rows 1–4 name `kmd_logic`
as where the obligation is proven. Those rows prove a *model of* the KMD, not the
KMD: **25 of `kmd_logic`'s 211 tests** live in `allocation_identity_tests`, and
those 25 exercise code that is in no driver image. (The other 186 are fine — they
cover modules `kmd_render` genuinely calls.) The §8 rows are being restated
accordingly by their owner this round; what belongs here is the outstanding work.

⇒ **Outstanding wiring unit — named `K4-W` here for the first time** (the name is
free: `grep -rn 'K4-W' docs/` was empty). Route `ddi/create_allocation.rs` and
`adapter/allocation_object.rs` **through** `kmd_logic::allocation_identity`
instead of beside it, exactly as `ddi/display.rs` already routes through
`kmd_logic::scanout_lease`. Until it lands, every `kmd_logic` allocation test is
assurance about a second implementation — the same defect class CLAUDE.md's last
invariant row names for `kmd_render`'s own `#[cfg(test)]` modules. Owner:
`ddi/create_allocation.rs`'s owner (`OWNERSHIP.md` §1 makes it single-owner);
sequence it with **K13**, the `kmd_logic` model rewrite, which is the only other
unit that touches both sides.

### ⭐ Cross-lane register — requests filed in source comments and tracked nowhere else

K4's authors could not make these edits: every target is in a file another unit
owns (`docs/retirement/OWNERSHIP.md` §1), and inventing a back channel to reach
one would have been worse than the gap. Until now they existed **only** as
comments inside the requesting file, which is not a tracker. Status re-derived at
HEAD; symbols cited rather than line numbers, because these files move.

| # | filed in | against | request | status |
|---|---|---|---|---|
| **X1** | `kmd_render/src/adapter/allocation_object.rs` (module header) | `ddi/lifecycle.rs::dxgkddi_stop_device`, `ddi/lifecycle.rs::dxgkddi_remove_device` (**K10**); `ddi/submit_command.rs::dxgkddi_reset_from_timeout` (**K9**) | call `adapter::allocation_object::invalidate_all()` beside the `ddi::native_fence::invalidate_all()` call the same units owe. Both are lock-free, allocation-free and legal at any IRQL. §14 requires the two generations be invalidated **together** (so they may not be split across two reset paths) and §18.2 fixes the order: capability invalidation precedes the device-lost wakeup | ✅ **CLOSED IN SOURCE BY K7; RUNTIME-UNADMITTED.** TDR, Stop/Remove, failed/skipped Start and reset boundaries now invoke `ddi::native_fence::invalidate_all` before `adapter::allocation_object::invalidate_all`, ahead of device-lost publication. D9/K7 gates preserve the order; no deployed counter movement is claimed. |
| **X2** | `umd12/src/bridge12.rs` (the deleted cxx declaration block, and again at the deleted wrapper) | `umd12/bridge/vkd3d_bridge.{h,cpp}` | delete the C++ member `HeliosVkd3dDevice::transfer_resource_ownership`. There is no adoption to transfer: HWA2 carries no host resource token (§10.3) and the KMD creates the backing rather than taking the guest's | ⛔ **OPEN.** The Rust side is gone; the C++ side is **fully intact** — declared in `vkd3d_bridge.h`, defined in `vkd3d_bridge.cpp`, and still resolving `helios_venus_memory_transfer_resource_ownership` via `GetProcAddress`, with its `g_vkd3dOwnershipTransferFailed` counter. An unused C++ member is not a build failure, which is exactly why it will rot silently |
| **X3** | `kmd_render/src/ddi/create_allocation.rs` (K4) | `kmd_render/src/ddi/display.rs` (display lane) | make the present-side A3 refusal distinguishable and countable. `PresentAllocInfo` is permanently `None`, so BOTH present consumers took their `else` arm on every call **through the pre-existing last-value breadcrumb `PBFlip`/`PBCpy = 0xE1`**, which in that file already means "dxgkrnl handed us a handle we could not resolve" — a handle-lifetime bug, an entirely different investigation — and a last-value write could not even say whether it fired once or per frame | ✅ **CLOSED 2026-08-10** (`01a4131`). The two sites now write **`0xEA`** and bump `create_allocation::PRESENT_NO_ALLOC_INFO` (**`PrNoRid`**), the symptom-side pair to `OaNoRid`'s cause side; `0xE1` stays reserved for its original meaning. Expected LARGE and rising until A3 — **revisit, do not merely zero** |
| **X4** | `umd/src/forward/resource.rs` (the `global_vidmm_tracker` out-parameter, in the tex2d create path) | `umd/bridge/` — the D3D11 cxx bridge | remove the `global_vidmm_tracker` out-parameter from `get_resource_alloc_identity`. It is a **write-only sink**: `GlobalVidMmTracker` has no successor at all (K4-CONTRACT §6 — HWA2 has no tracking kind, no cookie, no global-share field, no tracker flag bit, and §10.3 forbids reintroducing it under another name). The Rust extern and the C++ declaration must change in **one** changeset or the bridge stops linking | ⛔ **OPEN.** ⚠ This request was filed as "see the K4 report" — a document that **has never existed** (`docs/retirement/` holds `FINDINGS.md`, `K4-CONTRACT.md`, `OWNERSHIP.md`, the six `lane-*.md` and the review rounds; `grep -rln 'K4 report' docs/` is empty). Round 3 caught it and the citation is being re-pointed at `K4-CONTRACT.md` §6 by the owning author. **A cross-lane request that names a nonexistent document is untrackable by construction — that is why this register exists.** The request text now lives at the site as "an open cross-lane request against `umd/bridge/`" |

⇒ **A new cross-lane request gets a row here in the same edit that writes the
source comment.** A comment in the requesting file is a note to nobody: neither
the lane that owes the work nor the lane that will deploy it reads that file.

### ⭐ The build environment moved to WDK 28000 (2026-08-10, owner decision)

Round 3's completeness critic found that **`helios_umd12.dll` could be built on
exactly one computer**: `umd12/build.rs` `require_path`'d a `.gitignore`d
`tmp/wdk-28000/` and `require_core_0116()` panicked without it. The same
untracked tree silently decided whether `tools/retirement-gates.sh` ran or
skipped its slot-audit gate — **8 PASS in one checkout and 7 PASS + 1 SKIP in
another, at the same commit.**

⇒ Kit **10.0.28000.0 is now INSTALLED on the VM** and is what `wdk-build`
selects (highest installed kit, no override). Four NuGet packages at
`10.0.28000.2526`, version-scoped paths only so the 26100 kit's shared `wdf`,
`Catalogs` and legacy `bin` dirs are untouched; the result has the same
`Include`/`Lib` shape as 26100 beside it, which is what "keep only complete
kits" requires. **`TOOLCHAIN.md` §2.1 is the record** — recipe, all four
SHA-256s, and what changed in the build.

What a later lane needs to know:

* The umd12 **WDK-vs-SDK include split is retired.** It existed only because the
  staged package was a WDK with 11 `shared/` headers; the complete kit has 255
  and declares the two identifiers that forced the exclusion.
* ⚠ **The D3D12 bindings changed**: `umd12/bindgen/cached/d3d12umddi.rs`
  6,485,166 → 6,573,255 bytes, **65 items added / 3 removed** (28000-era kernel
  types; the removals are anonymous `__bindgen_ty_N` renumbering inside
  `_D3DKMT_VIDMM_ESCAPE`). Inspected, not blind-copied.
* ⭐ **`km/dispmprt.h` is VENDORED** at `kmd_render/tools/wdk-28000/km/dispmprt.h`
  (167 KB, README with provenance). The slot-audit gate runs on **Linux**, where
  no Windows kit exists, so installing on the VM did not help it — the gate's
  `if [ -f tmp/… ]` wrapper is **deleted**, and all 8 gates now pass with `tmp/`
  moved aside entirely. Regenerating against the vendored copy changed exactly
  the recorded `AUDITED_HEADER` path: **zero `SlotClass` changes**, still 192
  slots / 1544 bytes — checked *before* the regeneration was accepted, because
  `FINDINGS.md` F6 records what happens when it is not.
* ⚠ **There is no working `python3` on the VM** (App Execution Alias stub), so
  `build.rs`'s slot-audit check is advisory there. The enforcing run is
  `tools/retirement-gates.sh` on Linux.
* ⚠ **CI still installs only kit 26100** and is deliberately deferred to the
  **end** of the retirement (owner sequencing). The fix is to install 28000 on
  the runner the same way. The LLVM pin was raised 17.0.6 → 22.1.8 in the same
  pass, because this changeset had deleted
  `_ALLOW_COMPILER_AND_STL_VERSION_MISMATCH` while CI still passed it to the
  DXVK half — one job stating two opposite toolchain assumptions.

⭐ **Verification pattern to repeat, not just a result:** the package hash
matched the one the docs already pinned and three headers hashed identical
against the old tree, so the install is *provably* the same package the earlier
measurements used; and `kmd_render` was re-checked with `wdk-sys`
**force-cleaned first** (151.6 MiB removed) — 22 warnings, the pre-change
baseline. A green build proves nothing about today's toolchain unless you made
it regenerate.

### ⭐ Sequencing after round 4 — ⛔ THE REVIEW LOOP IS CLOSED (owner, 2026-08-10)

⛔⛔ **OWNER DIRECTIVE. There is no round 5, and `METHOD.md` §3's two-dry-rounds
criterion does not govern this changeset.** Verbatim: *"the METHOD was only for
the dx12 implementation, where it saved us a lot of time. a BSOD eats up a lot of
time, but they are also rare, and are generally caught with a simple code review,
a comment is not going to cause a bsod"*, and *"adversarial review is costing too
much time"*.

**The measurement behind it, so the decision is legible rather than merely
obeyed:** round 4 produced **54 raw findings, 30 survivors — and 8 of the 30 were
code.** The other 22 were documentation, claim-integrity, counter-grading and
gate-wording findings. The loop had started mining this project's own prose. See
`REVIEW-ROUND-4.md`.

⇒ Review of retirement code from here is an **ordinary code review of code**.
Keep the shape that pays (a skeptic refutes before a finding is routed; run the
command rather than quoting a doc) and drop the rest: no lens rotation schedule,
no claim-integrity lens, no dry-round accounting.

In order:

1. ~~**Repair what round 3 found**~~ — ✅ DONE (`01a4131`).
2. ~~**Round 4**~~ — ✅ DONE 2026-08-10. 7 lenses + a skeptic per finding + a
   completeness critic; 54 → 30 → **8 code defects, all repaired** in `b8ea245`
   + `icd/mesa` `33db3fd` + `vkd3d-proton-helios` `cdf1bce`. Verified: gates
   8/8, `kmd_render` check exit 0 at the 22-warning baseline, `umd` exit 0, mesa
   ICD + present layer link clean, vkd3d native build green.
3. **A1 → A2 → K5 → K6 → K2a → K11 → K9 → bounded post-K9 KMD
   executor/bootstrap → A3 → A4** ← **COMPLETED IN SOURCE/BUILD; TARGET
   RUNTIME UNEXERCISED.** A1 ✅ (`icd/mesa`
   `6ad43fb`, rewritten onto A2 in `1b97c64`), **A2 ✅** (`1b97c64`, gate
   `c20d162`), **K5 ✅** (gate `tools/hts1-attach-gate.sh`, acceptance
   `tools/hts1_session_probe.c` **15/15** on KMD 22.22.267.0), **K6 ✅**
   (2026-08-11 — gate `tools/hnr2-decode-gate.sh`, acceptance
   `tools/hnr2_native_probe.c` **15/15** on KMD **22.22.271.0**, with
   `hts1_session_probe` still 15/15 beside it), **K2a ✅** (KMD 22.22.288.0,
   52/52 plus an exact bidirectional 4 MiB alias), and **K11 ✅** (KMD
   22.22.296.0, actual host INIT replies plus per-session/process/reset teardown
   evidence), **K9 ✅ in source/build only** (`a4db0ad`; target runtime
   unexercised), **the bounded executor ✅ in source/build only** (`e8819c1`),
   **A3 ✅** (`2c2763b8b1a`), and **A4 ✅** (`5cbc0254f43`). The new KMD
   executor and Mesa ICD have not been installed or target-exercised. A4 keeps
   command-buffer closure refused until A7. Stop at the A5-A9 and present-layer
   handoff; see F20 and do not treat K9 itself as a host executor.
   ⭐ The HVM1 write-back blocker is **CLOSED** (`a8527e2`, `FINDINGS.md` F11);
   the private-data window blocker K6 hit is **CLOSED** (`FINDINGS.md` F13 —
   `DxgkDdiRender` must advance `pDmaBufferPrivateData`).

   ⛔ **"A3 is the only thing between here and a rendering desktop" is FALSE, and
   `FINDINGS.md` F14 is why.** An HVM1 allocation's `D3DKMTLock2` view is ordinary
   VidMm-backed guest RAM, disjoint from the venus blob the KMD allocated for it:
   `ctrl::map_blob_at` is the only code that aliases the two, its one caller
   refuses `!bar_eligible`, and `admit_hvm1` sets exactly that. So A1's landed
   role-1 reply pool was **bound but not reachable**, and A3's roles 1-3 would
   have shipped the same defect. That checkpoint identified two prerequisites:
   **K2a** (below) and **K11**. Both have since crossed their exact target gates;
   `session_init` now publishes a nonzero bounded endpoint capacity only after
   the distinct stock Venus context and actual finite host INIT reply exist.

#### K2a — the HLM1 CPU-view binding (started 2026-08-11)

Make an HVM1 allocation's Lock2 view alias its venus blob, by reporting segment 2
as §10.7's HLM1 (`CpuVisible=1`, `CpuTranslatedAddress = BAR GPA`) and mapping the
blob at the window offset VidMm placed it at. Owner chose this over reusing the
proven `SupportsCpuHostAperture` path; F2 already measured the `BarSegFlags=0x02`
flag shape booting `CM_PROB_NONE` with a live desktop.

**Landed:** the pure half, `kmd_logic::hlm1_placement` (293 → 308 tests, defeated
nine ways) — the paging-op placement classification, the window-offset bound, the
bind state machine, and the verify vocabulary the acceptance probe and the KMD
must share one declaration of.

⭐ **Two things the shipping WDK 28000 bindings say that the tree did not.**
`tmp/dxgk_bindings.rs` **was** a 2026-07-08 snapshot while the real bindings are
regenerated per build; they disagreed, and both disagreements mattered. ⭐ The
snapshot is now **refreshed from the shipping 28000 kit** with its provenance and
a staleness check in `tmp/dxgk_bindings.README.md`:

* **`TRANSFER2`/`FILL2`/`DISCARD_CONTENT2` are ordinals 23/24/25.**
  `protocol/src/physical_memory.rs:497-504` records §10.7's TRANSFER2/FILL2 as
  "= `VIRTUAL_TRANSFER`/`VIRTUAL_FILL`, not new ordinals". **Falsified.** `FILL2`
  is the only operation in the whole DDI carrying a bare
  `(SegmentId, SegmentAddress)` pair in this kit.
* **`NOTIFY_RESIDENCY2` has no `PhysicalAddress` and no `SizeInPages`** — its
  placement lives in a `DXGK_ADL`, and the `{PhysicalAddress | Mdl}` pointer union
  the snapshot shows does not exist here.

⚠ And the driver cannot see either: `PAGING_OP_SEEN_MASK` is masked `& 0xFFFF`
before it reaches the diag ring, so **ops 16-25 are invisible today**. The census
that says `TRANSFER`/`FILL` have never fired (`0x9B64`) is therefore sound about
ops 0-15 and silent above them.

**Deploy 1 is an instrument, not a fix** — which operation carries an HVM1
placement is not knowable read-only, and `PgDi` cannot answer it (F14's
correction). The `Hl*` counter block records, per HLM1-eligible allocation, the op
mask, the segment mask, the last placement (segment / page / length) **with the
operation that produced it**, the `UPDATE_PAGE_TABLE` PTE view that is
uninstrumented today, the IRQL each arrived at, and whether any offset would have
failed the window bound. Atomics only, above the IRQL gate, every arm returning
exactly what it returned before.

⭐ **DEPLOYED AND MEASURED — KMD 22.22.272.0, `FINDINGS.md` F15.** Two answers of
its four survive:

* **`NOTIFY_RESIDENCY` (15) is the hook**, and `NOTIFY_RESIDENCY2`/`TRANSFER2`/
  `FILL2` — the three the design ranked most likely — **never fire**.
* **`HlEirq = 0`**: every observation arrives at PASSIVE, so the bind may issue
  its host round-trip from that arm. That was deploy 2's open safety question.
* ⛔⛔ Its other two — "VidMm places the pool in the APERTURE, not HLM1" and the
  `BarSegFlags` corollary — are **WITHDRAWN by F16**. Both rested on `HlPlSg` and
  `HlPtSg`, which are last-writer-wins fields, and F15 said so itself in its own
  "Bound" section. The census F16 added reads `HlPt2 = 66…99` page-table batches
  in segment 2 against 33 in system memory, and on a two-pool run at defaults the
  final values are `HlPlSg = 2`, `HlPtSg = 2`. **The pool lives in HLM1.**

#### K2a deploys 2-5 — MEASURED, and the design's premise does not hold (`FINDINGS.md` F16)

KMD **22.22.276.0**, nine configurations, `hts1_session_probe` 15/15 and
`CM_PROB_NONE` in every arm but the two that break the page-in.

* ⭐ **The bind WORKS and is not the problem.** `Hlm1Bind` maps the blob at the
  window offset VidMm placed the allocation at, from `NOTIFY_RESIDENCY` and (since
  deploy 4) from `UPDATE_PAGE_TABLE`. Every arm: one bind, `HlBndR = 0` (the
  placement never moves), `HlBndE = HlBndQ = HlEwin = 0`, and the two arms agree
  on the offset where both fire.
* ⛔ **`Hlm1Only=1` breaks the page-in**, it does not redirect it: `MakeResident`
  succeeds, `D3DKMTLock2` returns `0xc0000001`, and dxgkrnl's ETW says
  "WORKER_THREAD: Unrecoverable page in failure" with no Helios paging counter
  moving. **Default stays 0. Do not flip it.**
* ⛔ **The guest's Lock2 view is never the blob.** The acceptance oracle
  (`HlRdbk` — the KMD reading the blob's own bytes at the offsets H5 writes)
  reads the K2a stamp, not H5's bytes, in all seven arms that could produce a
  view: `BarSegFlags` 0x1C / 0x02 / 0x06, `AccessedPhysically` on or off,
  `Hlm1FlagsOff=7`, and `Hlm1Bar=1` (BAR-eligible, so `MapCpuHostAperture` could
  serve it). `0x55`/`0xF7` are `stamp_byte(1, ·)` computed independently, so the
  oracle is proven to read the right memory.
* ⇒ **`D3DKMTLock2` is a copy protocol here**: VidMm moves HLM1 content by paging
  transfer (the 32-PTE scratch windows around `VIRTUAL_TRANSFER`/`VIRTUAL_FILL`)
  and hands the CPU the system backing. §10.7:1940 forbids a private copy, and a
  host-written reply pool cannot be one.

⛔ **NOT via an Escape.** `HELIOS_ESCAPE_MAP_BLOB` (`escape.rs:1377`) does return a
live aliased user-mode view and is production-proven — and §3:370-371 forbids
`D3DKMTEscape` outright, §18.1 gates on *absent Escape objects*, and **K1 deletes
`escape.rs` and `blob_map.rs`, where `map_io_pages_to_user` lives**. An earlier
draft of F16 recommended it; withdrawn.

⭐ **What §10.7 actually specifies is HPM1** — ":1939-1941: HLM1 is the segment and
'HPM1 is the actual paging/device protocol that binds each current VidMm placement
to the same renderer payload and copies the bytes on every placement transition'"
— **and F5 declined HPM1.** `lane-kmd-core.md:139` already says the forbidden byte
copy's replacement "was DECLINED, not deferred … so the deletion has no successor
mechanism today and must be an explicit recorded decision". F16 is that decision
arriving with numbers.

⭐ **ANSWERED by `FINDINGS.md` F17 (2026-08-11; 30 candidate mechanisms, 29 refuted,
1 survivor).** The escape-free mechanism is
`DxgkCbCreatePhysicalMemoryObject(Type = IO_SPACE, IOSpace.BaseAddress = the blob's
window GPA, CacheType = WRITE_COMBINED)` + `DxgkCbMapPhysicalMemory(AccessMode =
USER_MODE)` — dxgkrnl doing the mapping `escape_map_blob` does by hand — verified in
the shipping 28000 kit at `shared/d3dkmddi.h:10118-10232`. ⛔ **And both callbacks
live in the `DXGKRNL_INTERFACE` region gated at WDDM 2.9**
(`km/dispmprt.h:2290-2301`) while `wddm_surface.rs:64` declares **2.1**, and no
2.9-block callback has ever been invoked by this driver. ⇒ **K2a's CPU view is
downstream of the version uplift this retirement already plans** — display unit
**D3** (`ddi/mpo3.rs`, the seven MPO3 slots) then **D9** (flip `SURFACE` to
`Wddm3_2GpuMmu`, strictly last; `doc:2854` rejects a 3.2 package with an incomplete
MPO3/fence surface, `doc:5079` gates on DWM starting without `E_NOTIMPL`). The
`E_NOTIMPL` that keeps us at 2.1 is the boundary D3 now supplies while dormant;
D9 crosses it only with the complete package.

⇒ The two proposed probes above are historical. D9 subsequently supplied a
576-byte interface and enabled `DXGK_FEATURE_SHARE_BACKING_STORE_WITH_KMD`; K2a
then selected the documented allocation-backing contract rather than F17's
physical-memory-object route. `FINDINGS.md` F18 is the superseding result.

#### ⭐ K2a final design and target exercise — shared backing, four 1 MiB slots (2026-08-13)

The user identified Microsoft's **Sharing the backing store with KMD** contract
and authorized the narrow QEMU support it needs. K2a now does exactly that:

* StartDevice copies only the OS-supplied `DXGKRNL_INTERFACE.Size`, requires
  coverage through `DxgkCbQueryFeatureSupport`, queries
  `DXGK_FEATURE_SHARE_BACKING_STORE_WITH_KMD`, and fails cleanly if unavailable.
  The target supplied 576 bytes and returned `Enable=TRUE`.
* Roles 1–3 are shared, CPU-visible, system-memory-only HVM1 allocations with
  `ShareBackingStoreWithKmd=1`. `DxgkDdiSetAllocationBackingStore` receives the
  exact OS backing, locks its MDL pages, coalesces exact PFN runs, and creates one
  guest-backed Venus blob. Role 4 refuses before mutation. No pointer, handle,
  PID, resource ID, or lookup token enters HVM1/HNR2.
* The reply pool is **4 MiB split into four 1 MiB slots**. Each HVR1 chunk holds
  at most 1,048,496 payload bytes after its 80-byte header; a logical snapshot
  remains bounded at 64 MiB and therefore drains through continuation chunks.
  HNR2's separate 15 MiB command payload bound is unchanged.
* QEMU is rebased onto upstream and imports those exact guest pages with
  udmabuf; virglrenderer is unchanged. Measured 4 MiB allocations used 143–853
  coalesced ranges, below the stock 1024-entry limit; the conservative maximum
  is exactly 1024 pages. No Linux kernel parameter changed.

**Final-source runtime evidence.** KMD **22.22.288.0**, `oem120.inf`, loaded
after the authorized guest reboot on build 26100 with `CM_PROB_NONE` and dxdiag
WDDM 3.2. The deployed, re-signed
SYS is 849,144 bytes / SHA-256
`df5903586a067b3c2e4047713278f92b38e7573e695fcd12bdd63d25d2556b86`.
The KMD recorded `DXGKRNL_INTERFACE.Size=576` and feature enabled. The normal
probe passed **52/52**: roles 1–3, explicit role-4 refusal, eight repeated
create/map/unmap/destroy cycles, unclosed process teardown, fresh reuse,
wrong-process rejection, and stale-handle rejection. QEMU resource `0x1f` was
an exact 4 MiB renderer blob with 430 ranges; correlated first/last GPA writes
passed host→guest, and held host mappings read the guest's replacement values
back. QEMU returned to the identical 227-FD baseline: one `/dev/udmabuf` and
two `/dmabuf:` descriptors.

This admits K2a only. The current DWM still loads WARP and DisplayConfig reports
zero paths; at this checkpoint K11 and Mesa A3/A4 remained required before
cold-DWM admission.

#### ⭐ K11 final design and target exercise — one stock Venus namespace per HTS1 session (2026-08-14)

K11 closes K5/K6's exact host-handoff boundary without starting Mesa A3/A4 or
the later allocation/GPU execution units:

* The ordinary HVC1 control-context path creates one heap-pinned `SessionObject`.
  Its exact raw KMD device owns one distinct stock Venus context, one 4 KiB
  HOST3D/MAPPABLE reply target, and one `VkInstance`. The canonical role-1
  allocation/open object binds directly to that session and supplies the sole
  user-visible HVR1 carrier; no PID, name, global lookup, resource identity, or
  host token is accepted from user mode.
* INIT submits `vkSetReplyCommandStreamMESA` and `vkCreateInstance` as two
  fenced ring-zero operations. KMD validates the actual 24-byte host reply's
  opcode, status, pointer count, and instance identity before publishing a
  fresh nonzero generation, CSPRNG capability, or endpoint capacity. The
  endpoint/ring table is fixed at 64 entries maximum and reserves each granted
  nonzero ring before publication. Ring zero remains decode/pure-control only.
* The finite allowlist contains only this allocation-free synchronous INIT.
  Allocation-backed, queue, GPU-dependent, and general Venus work continue to
  refuse at named K6 boundaries until their owning later units land. An actual
  host reply is copied into the exact checked-out K2a role-1 slot as HVR1; four
  1 MiB slots and the 64 MiB logical snapshot ceiling remain unchanged.
* Outer HQA1 contexts retain direct strong session/endpoint references and
  revalidate the exact allocation and transport generation; SubmitCommand does
  no session discovery. Host-completed records use one context-local monotonic
  WDDM `SubmissionFenceId` watermark. There is no adapter-global boundary
  queue, independent shared timeline, polling, sleep, or synthetic completion.
* Teardown first closes admission, drains the exact in-flight rundown, destroys
  the host `VkInstance`, unrefs the private reply resource, destroys the Venus
  context, and only then permits session/K2a references to fall. Failed or
  repeated INIT aborts the exact checked-out slot before draining, so no reply
  slot is stranded. Reset, StopDevice, RemoveDevice, process/device/context
  destruction, and abrupt exit use the same revocation boundary.
* The bounded code-review pass found that carrying a per-session rundown or the
  WDDM notification lock through `DxgkCbSynchronizeExecution` can deadlock a
  concurrent failed-INIT teardown. The final source instead admits completion
  through one fixed per-adapter rundown spanning current-generation validation
  through the exact OS notification, while session/resource guards end before
  that callback. Reset/Stop/Remove close and join this rundown before host
  teardown; ordinary session destruction never waits on the callback it caused.
* HTS1/HVC1/HQA1/HNR2/HVM1/HVR1 remain fixed and pointer-free. No host context
  id, renderer resource id, pointer, handle, PID, or reusable lookup token was
  added. Escape and all HWQueue-family registrations remain NULL. QEMU and
  virglrenderer are unchanged from frozen K2a head `415a5ef`; HPM1 and kernel
  parameters remain untouched.

**Final-source runtime evidence.** KMD **22.22.296.0** loaded as `oem128.inf`
on build 26100 with `CM_PROB_NONE`. The active DriverStore SYS is 880,376 bytes,
SHA-256 `b12a1c914eb1a126c7170ad00974a5310f3828df65ed4d835bf09592c538167e`,
and carries a valid WDR test signature. The K11 probe passed **14/14**: two
simultaneous sessions in one process requested capacities four and two, received
distinct nonzero generations/capabilities, produced correlated final HVR1
bytes for the exact host `vkCreateInstance`, rejected a crossed session attach,
isolated an abrupt child process, repeated four INIT/teardown cycles, and created
a fresh session afterward. The standing HTS1, updated HNR2, and K2a probes
passed **15/15**, **15/15**, and **52/52** respectively. The HNR2 regression
case completed first rather than hanging while it deliberately refused a second
INIT on the same live session.

The held-reset arm reached READY, survived an exact PnP adapter restart, observed
the old capability drain, and exited cleanly. The adapter returned Code 0 and a
fresh K11 run passed 14/14 with `K11CtxNew=K11CtxDel=8`, all K11 failure
counters zero, `K11CmpOpenRej=0`, and `TsSlotRel=8` / `TsSlotStuck=0`; the host returned to its two-process
virgl-render-server baseline with no transient per-session server left. The K11
semantic gate rejects 62 in-memory
mutations covering namespace sharing/discovery, early or zero-capacity success,
forbidden work, polling/synthetic completion, unbounded storage, teardown
ordering, ABI tokens, alternate carriers, host-maintenance dependencies, and
false display claims.

This admits K11 only. DWM still loads WARP and DisplayConfig still reports zero
active paths after the reset. Mesa A3/A4 is the next handoff; no visible or
cold-DWM admission, HPS2 retirement, or production-correctness claim follows.

#### ⭐ K9 source/build checkpoint — ordered one-engine host completion (2026-08-14)

A4 pre-edit reconciliation found that normal-mode submissions may complete on
distinct nonzero host contexts, while WDDM exposes one render node/engine. The
old compatibility FIFO and K11's direct callback could therefore report a
later context before an earlier one. K9 now owns one fixed 256-entry
one-node/one-engine frontier per adapter:

* every SubmitCommand arrival admits its exact OS fence and receives a private
  epoch/serial/slot ticket before host work can become terminal;
* direct K11 and compatibility Venus completions mark only that exact ticket;
  the frontier retains early cross-context completions and reports only its
  contiguous ready head through `DXGK_INTERRUPT_DMA_COMPLETED`;
* notification failure keeps the same head and requests an ordinary DPC retry;
  transport failure, bounded-FIFO exhaustion, duplicate/backward admission, or
  an exact host refusal poison the generation instead of forging completion;
* reset/Stop/Remove join the current callback rundown before invalidating the
  frontier generation. Successful preemption reopens only after dxgkrnl accepts
  `DXGK_INTERRUPT_DMA_PREEMPTED`, and stale callback tickets cannot address a
  successor generation;
* K7's empty native-fence rescan remains downstream of a successfully delivered
  DMA edge and retains its pending edge across callback failure.

The pure model adds 11 focused tests. The inherited K7 and K11 mutation gates
remain green, and K9's own in-process/in-memory semantic gate rejects **42/42**
mutations.
The Windows KMD check remains green at the exact 13-warning baseline. This is a
source/build checkpoint only: K9 has not been deployed or exercised on the
target. Mesa A3/A4 remain unimplemented, the installed KMD remains
22.22.296.0, and no display admission, visible desktop, HPS2 retirement, or
production-correctness result follows.

#### ⭐ Post-K9 A3/A4 reconciliation and bounded KMD authorization (2026-08-19)

The next pre-edit pass proved that K9 answers only A4's one-engine ordering
question. It does not execute a batch. The current source still has four exact
boundaries:

* `admit_hvm1` refuses role 4 (`VulkanDeviceLocal`) through `AcHvm1Mem` because
  the current Venus client cannot yet select a truthful non-host-visible memory
  type; substituting host-visible memory is forbidden;
* `native_render.rs` executes only K11's one-fragment, reply-bearing INIT.
  Other payloads reach `NR2_NO_STAGE`, `NR2_NO_RESID`, and `NR2_NO_SCHEMA`, and
  SubmitCommand reaches `NR2_NO_HOST` rather than an actual host result;
* the refused disposition still enters `submit_command.rs`'s compatibility
  `note_and_maybe_signal` arm. That arm is not A4 completion and may not be used
  to synthesize success;
* normal Mesa instance construction still creates the renderer and raw-`res_id`
  ring before issuing `vn_call_vkCreateInstance`, while K11 already owns the one
  stock-Venus instance in the exact HTS1 namespace. A3/A4 need an explicit
  per-instance lifecycle transition, not a second instance, global discovery,
  or an A8-sized generic-ring demolition.

The owner authorizes the minimum continuation of the existing exact
`SessionObject`/endpoint/context/allocation graph needed by A3/A4: bounded
immutable payload custody; validation of only the exact admitted opcode and
typed-operand subset; KMD-only patching of a host-private copy from current
WDDM allocation ownership; stock Venus submission on the exact nonzero session
endpoint; actual host completion fed into K9 before WDDM completion; matching
fence ordering; and reverse-order reset/Stop/Remove/session teardown. It also
authorizes the minimum truthful role-4 allocation path. If role 4 or execution
cannot be implemented with existing stock virtio-gpu/Venus operations and the
fixed protocol ABIs, stop and report the exact dependency. The authorization
does not permit a new carrier/protocol, `NR2_NO_HOST` fallback, fabricated
identity, A7's whole-object classifier, or any later lane. See F20 for the
source trace and acceptance boundary.

#### ⭐ Post-K9 executor and Mesa A3/A4 source/build checkpoint (2026-08-19)

The authorized continuation is now dependency-first in root `e8819c1`, followed
by Mesa A3 `2c2763b8b1a` and A4 `5cbc0254f43`. The KMD owns immutable bounded
payload custody and the generated admitted Venus subset, patches only a private
copy from exact live allocation objects, submits through the exact nonzero
session endpoint, and admits K9 completion only from the terminal host result.
Role 4 uses a stock non-host-visible Venus memory type and never enters Lock2;
roles 1–3 retain K2a's exact process-local view.

The Windows Mesa renderer now owns one A1/K11 session per `vn_instance`, uses
HVM1 roles 1–4, C57 `D3D12_RESOURCE_BIT` import, and HNF1 native-fence import.
The A4 queue seam owns normal versus record-only mode per instance. Record-only
requires an exact live outer scope and creates no KMT work, completion, or
timeline; normal mode emits zero host-resource operands, supplies exact sparse
allocation closure, and orders imported waits, HNR2 submission, imported
signals, and C51 progress on the same context. Command-buffer submission remains
a named fail-closed result until A7 supplies its complete use/operand closure.

A3 and A8 were not codependent. Windows instance construction bypasses the
duplicate host-instance/ring creation using only a narrow endpoint allocator;
the generic ring implementation and its full retirement remain A8. A5/A6/A7/A8/
A9 and the present layer are unchanged by this tranche. Source gates, Linux
model/integration tests, and Windows compilation pass, but neither the new KMD
nor ICD was installed, restarted, or exercised. The installed KMD therefore
remains 22.22.296.0 / `oem128.inf`, and the prior K11/K2a target results cannot
be attributed to this source. DWM/WARP and zero active DisplayConfig paths remain
the last measured runtime state; no display-admission or HPS2-retirement claim
follows.

#### ⭐ Mesa A5/A6 source/build checkpoint and A7 stop (2026-08-20)

Mesa A5 landed at `4ea18b3512f`. The sole private export,
`helios_icd_create_translator_v1`, creates a direct-owned record-only instance
and returns the fixed package-generation-checked table. The table reaches only
the existing per-instance session, monotonic endpoint/HQA1, exact context, live
scope, refusal-counter, and teardown objects. Loader/layer procedure provenance
is rejected by owning module. No registry, PID/name lookup, global table, TLS
identity substitute, second session, or consumer cutover was added.

Fail-closed A6 landed at `d07d1d13687`, followed by the bounded review fix
`478c71a0fff` that makes the Win32 handle-properties query reject every HWA2
class the actual dedicated-image import rejects. The Windows lower ICD no longer
advertises or initializes Win32 WSI, presents exactly two guest memory types over
one queried HLM1 heap, remaps renderer memory indices and requirements exactly,
clamps the normal-loader closure to 4096 allocation uses / 8192 typed operands,
keeps sparse and second-queue emulation disabled, and implements strict
`D3D12_RESOURCE_BIT` dedicated-image and timeline `D3D12_FENCE_BIT` import
validation. Those import capabilities remain unadvertised because A7 has not
closed the allocation/operand path; no partial profile is exposed.

The final reviewed Windows ICD is 50,889,646 bytes with SHA-256
`82F99330AEBAAB7683F76AE939F7821246A7D69CC3D487F5D31EF17F73B8ACD8`.
Its PE import table and embedded strings contain no DXGI, D3D11, D3D12, DComp,
or `vulkan-1.dll`; its exports are exactly the three Vulkan loader entry points
plus `helios_icd_create_translator_v1`. The compiled `LoadLibraryA` and
`GetProcAddress` references remain confined to the three pre-existing Mesa
utility objects (`u_debug_stack.c`, `u_debug_symbol.c`, and `u_dl.c`), while the
selected Helios backend has no dynamic-loader fallback. The final serial
retirement run passed protocol 146 unit + 1 integration, kmd_logic 543 unit + 2
integrations, HNR2 encode/decode, and every standing mutation suite including
A5 20/20 and A6 31/31.

A7 stopped at the fixed identity boundary. The complete sealed-use table must
contain the opaque, nonzero UMD-assigned
`HeliosSealedResourceUseV1.outer_allocation_token`. The protocol explicitly
places its ingress on the DXVK/vkd3d plus UMD resource-creation path. No such
token appears in the current Mesa, DXVK, vkd3d, UMD, or UMD12 integration, and
the fixed 112-byte/eleven-slot `HeliosTranslatorDispatchV1` has no registration
slot by design. The live call flow is:

`vn_helios_queue_submit{,2}` →
`helios_submit{1,2}_deferred_use_gate` →
`HELIOS_RECORD_REFUSE_DEFERRED_USE`; independently, record-only
`helios_dispatch_payload(..., allocation_count > 0, ...)` returns
`VK_ERROR_FEATURE_NOT_PRESENT` before sealing. `vn_helios_record_scope_seal`
can validate/copy uses only after their exact outer token exists, while the
outer bridge is the owner that converts that token into an allocation-list
index or GPUVA. Mesa's local HVM1 allocation handle and generation are a
different identity and cannot be fabricated into that field.

Thus A7 requires either the explicitly out-of-scope DXVK/vkd3d and UMD cutover
or a new ABI/private carrier that this authorization forbids. No KMD validator
or schema extension can create the missing producer-side identity, so none was
made. A8 and A9 were not started because the requested dependency order stops at
A7. The present-layer B lane remains the next handoff only after an owner
authorizes the necessary consumer/UMD resource-creation integration. No source
in that lane, deployment, adapter restart, reboot, or target probe occurred.

#### ⭐ F21 direct-consumer closure and Mesa A7-A9 checkpoint (2026-08-21)

The owner explicitly authorized the bounded producer/consumer graph that F21
had identified. Protocol commit `eec9564` advances package generation to 3 and
adds the single 72-byte HRA1 immutable creation/bind association with its C
mirror; it does not change the 112-byte/eleven-slot A5 table. `ccd2891` moves
the already-required HNF1 and UMD adapter query records into the same
protocol-owned source-of-truth boundary. DXVK `1cf7e631` and vkd3d `9a2716c0`
consume the exact A5 instance and direct GIPA supplied through their explicit
package construction edge, preserve generic non-Helios loader behavior, and
carry HRA1 through their resource allocation graph into the exact Mesa memory
object. The selected Helios paths neither search loaded modules/`vulkan-1` nor
create a second Vulkan instance.

Root `0fe5677` gives each outer UMD device a bounded, monotonic, nonreusing token
owner and removes the association before resource storage can be reused. D3D11
resolves every sealed use to the exact current Render allocation-list entry and
HWA2 generation and refuses nonzero byte offsets. D3D12 resolves it to the
exact current GPUVA plus checked byte offset and generation. Both build HQA1
before the runtime context callback, retain direct endpoint/context ownership,
write every HOB1 identity field explicitly, and close a scope COMMITTED only
after the real callback accepts it. D3D12 supplies exactly one HOS1 record and
submits synchronously through the owning ECL callback. Abandoned work remains
abandoned; no allocation/GPU identity comes from a global table, pointer, HVM1
handle, resource ID, PID, or name.

The complete A7 payload required one minimum generated admission update, landed
at root `eccfd19`. It stays on existing HNR2 and the exact session endpoint,
patches host-resource operands only in KMD-private custody, and feeds only the
actual terminal result into K9. It adds no DDI, host protocol, QEMU/
virglrenderer operation, compatibility completion, independent timeline, or
resource fallback. Corrective gate review found and repaired one K4 violation at
`d75f649`: HOC1 now uses the existing WDDM 3.2 shared-backing callback for its
exact KMD snapshot while OpenAllocation remains const-only; the alias preserves
the allocation's write-combined cache type and never becomes identity.

Mesa A7 `ae9c9f4c89d` replaces the prior deferred-use refusal with bounded
immutable command-buffer allocation-use and typed-operand closure, including
exact-token/lifetime/range validation, C60 control/deferred/GPU-dependent
classification, context joins, zero wire host-resource bytes, and the existing
presentable-image tag plus layout/queue-family checks. A8/A9 `fa61439bfd7`
make the generic Venus ring operations and shared-head/tail watchdog machinery
unreachable only on Windows, move reply production into the exact HNR2 COMMIT
path, preserve finite control-only ring zero and generic non-Windows Venus, and
prune lower-ICD-only build dependencies. The present layer is not wired or
activated.

The focused protocol, KMD, direct-consumer, A7, A8/A9, source/mutation, native,
cross, and Windows build checks are source/build evidence only. The corrective
serial `tools/retirement-gates.sh` run passed in full; Windows KMD compilation
retained the exact 13-warning baseline and byte-identical WDK binding hash, and
the final Windows lower ICD remained a four-export, loader-free 49,244,922-byte
DLL with SHA-256
`5AA0C1F43691B56D82CEC0B703B7D557054A71BF33C96746FE1DE9F72466F885`.
Neither KMD nor ICD was installed, no adapter was restarted, and no target
runtime probe was run. The installed package remains KMD 22.22.296.0 /
`oem128.inf`; DWM on WARP with zero active DisplayConfig paths remains the last
measured target state.

#### ⭐ `VK_LAYER_HELIOS_present` B0-B9 source/build checkpoint (2026-08-21)

The owner-authorized present-layer cutover landed dependency-first in Mesa:
B0 `bb7a787a5a1`, B1-B5 `e06abf3025f`, B6 `11d723ea10b`, B7/B8
`51054338012`, and B9 `871c62bf8a0`. B0 deletes only Mesa's HPS2 WSI writer and
its generic-WSI additions. B1-B5 preserve the loader-layer chain, private next-
dispatch provenance, exact-LUID D3D12/DXGI image graph, canonical dedicated
offset-zero import and private tag, while applying F8 in the measured direction:
lower Vulkan creates and exports each Ready/Release timeline semaphore and the
exact D3D12 device opens the transient NT handle. No D3D12-created Ready/Release
fence is imported into Vulkan.

B6 implements the bounded nine-state, monotonic-epoch acquire/present machine.
Finite backpressure uses `SetEventOnCompletion`; Present exercises the ordinary
lower `vkQueueSubmit2` path before the exact Ready wait, copy, Release signal,
and `Present(1,0)` sequence. B7/B8 implement owner-scoped canonical/alias state,
NULL-swapchain pass-through, mixed bind status ordering, `ALIAS_ONLY` retention,
atomic `oldSwapchain` retirement, exact D3D/DXGI drain, final helper-queue
ownership restoration, reverse teardown, and device-loss mapping. B9 builds
the layer as its own DLL/manifest; it does not enter `libvulkan_wsi` or the
lower ICD.

The one bounded ordinary review then landed `8b3c9359b5a`: feature-chain
copying now preserves the loader's own device-create node when forcing existing
feature bits, and every post-validation Present failure writes the exact
per-swapchain `pResults` array before returning. No second review loop ran.

The Linux-host cross build and the authorized Windows `win_meson` build pass.
The Windows lower ICD is 49,251,322 bytes, SHA-256
`6CFE645EDEC6BDE779300DAB4DC257F1092713EB9D06BCBA0A844AF683EC5D14`,
and exports exactly `helios_icd_create_translator_v1` plus the three Vulkan ICD
loader entry points. The separate layer is 3,314,217 bytes, SHA-256
`0260E5AE530B4DAA0346F3AE2D4DAA53215E88C71486A8FFD57C12420CD73712`,
exports exactly its eight `.def` names, imports `d3d12.dll` and `dxgi.dll`, and
does not import `vulkan-1.dll`. The lower ICD imports none of DXGI, D3D11,
D3D12, DComp, or `vulkan-1.dll`. These are build-provenance facts only.

The one final serial `tools/retirement-gates.sh` run passed: protocol 151 unit
tests plus its integration/parity checks, kmd_logic 545 unit tests plus both
integrations, every standing retirement gate, A3/A6/A7 at 31/34/18 mutations,
and the new present-layer gate at 27 mutations.

Nothing was installed or registered, no installer or registry state was
touched, and no restart, reboot, VNC, cold-DWM, or target runtime probe ran.
The next handoff is broad HPS2 demolition in its separately owned repositories;
it is not part of this cutover.

#### ⭐ Broad HPS2 source-demolition and K14 checkpoint (2026-08-21)

The separately authorized demolition is now source-closed. DXVK `418f5745`
deletes its HPS2 publisher/lookup/reclaim and scanout-acquire machinery and all
active-package private Escape/`SharedGpuResource` transport in D3D9, D3D11,
and Win32 WSI. Root `b955a2c` removes the UMD11 vehicle, raw-resid snapshot,
named-present-fence, scanout-acquire, and publication paths while retaining
ordinary Present, exact allocation ownership, standard KMT sharing, and the
direct translator graph. The vkd3d audit found no remaining carrier after
`912a3d4d`, so its HEAD remains `9a2716c0`; Mesa remains at the complete B-lane
HEAD `8b3c9359b5a`.

KMD `60a9988` makes Escape, MapCpuHostAperture, and UnmapCpuHostAperture terminal
NULL slots and deletes the Escape/blob/HAP/read-ledger implementations, their
SEH shim and build reachability. `5e1fbda` removes the causally dependent
present-stream, reverse-reader, mapping-table, device/display/scanout/control-
queue state while retaining `render_user_copy.c`, canonical D2 plane custody,
K2a backing, K7 fences, K9 completion, K11 session transport, and the post-K9
executor. UMD12 cleanup `e7bdb0f`/`3663956` and protocol cleanup `e96b304`
remove only unreachable retired carriers; F21 outer-token association, HRA1,
HWA2/HVM1/HQA1/HNR2/HOB1/HOS1/HNF1, UMD12 identity and Venus-export heap
ownership remain intact.

The non-HPM1 K8/K10 closure is `ce3765a`: capabilities remain surface-derived,
K2a `ShareBackingStoreWithKmd` remains advertised, admission closes before
teardown, capability generations invalidate before device-lost wakeup, and
K9/session/native-fence/allocation/display ownership drains in documented
reverse order. No HPM1, K2/K3, hardware queue, QEMU, virglrenderer, new host
protocol, or compatibility carrier was opened. Host cleanup `5901f55` removes
the final source-side HPS2 creator/ACL/verification assumptions without deleting
any live machine file.

K14 `dcf0e1e` advances the source package exactly once to generation 4 and KMD
22.22.297.0. The manifest requires one generation-matched payload containing
both UMDs, the four-export lower ICD, and the separate eight-export present
layer plus generated manifest. The installer source rewrites the manifest to
the exact installed layer DLL path and preserves the translator-owned A5 layer
bypass; it has no HPS2 creation, ACL, mapper, or legacy-file deletion path.
Package assembly, installer execution, registry mutation, and ProgramData
cleanup were deliberately not run.

Focused demolition, K8/K10, and K14 gates reject 11, 8, and 12 in-memory
mutations. The final serial suite passed once with protocol 113 unit tests plus
integration/parity, kmd_logic 402 unit tests plus both integrations, all
standing D9/K2a/K7/K9/K11/post-K9/F21/A3-A9/B-lane gates, and every new gate.
Windows UMD11/UMD12/KMD release builds, the KMD warning/binding checks, DXVK
Linux/Windows builds, and Mesa Linux-host/Windows builds passed. The one bounded
ordinary review found one validation omission, then extended the demolition
gate to headers, export files, and build inputs; no production-code defect or
second review loop followed.

Current shipping-build provenance is: `helios_umd.dll` 6,529,536 bytes /
SHA-256 `41C7B2D49C59E015753E8167F0447A5B107492A1CDFA95F8EF5509B08766D03A`;
`helios_umd12.dll` 4,651,520 bytes /
`8A84C5D9180EBA40883BFE29E7ED66CB180EB1EAD5C97C677E66305EDB20481E`;
the compiler-linked release KMD image (not package-assembled or renamed to
`.sys`) 793,088 bytes /
`C15D6EA23BC594842333A9AE4C20A238BEB61DC3832FE5D2428103139F83C72D`;
`vulkan_virtio.dll` 49,251,322 bytes /
`89F7F0A2FDA0A872CC9971C012A828C936E903E15159E277D9FB30B9CDBE8943`;
and `VkLayer_HELIOS_present.dll` 3,314,217 bytes /
`0260E5AE530B4DAA0346F3AE2D4DAA53215E88C71486A8FFD57C12420CD73712`.
Export, dependency, object, and retired-string scans pass their exact lower/
layer/UMD/KMD boundaries. These are build facts only.

No retirement package or layer was assembled, installed, registered, or
exercised; no target binary was replaced; and no adapter restart, reboot, VNC,
cold-DWM, or target probe ran. The installed target therefore remains KMD
22.22.296.0 / `oem128.inf`, DWM on WARP, and zero active DisplayConfig paths.
Visible cold-DWM admission remains the next separately authorized boundary.

#### ⭐ 2026-08-23: THE DISPLAY LANE IS RUNTIME-ADMITTED (KMD 22.22.338.0)

Session-1 DisplayConfig reports **one ACTIVE path**: Helios `\\.\DISPLAY3`
`state=0x5` (attached-to-desktop, primary), current mode **1896x1030 32bpp
60Hz**, Generic PnP Monitor on HLS0001; auto-logon completes and DWM is stable.
Six stacked runtime fixes, each measured before written (KMD .325→.338,
uncommitted): (1) both KMD `vkCreateInstance` encoders pin apiVersion 1.4 — a
NULL `pApplicationInfo` made vkr default the host instance to 1.1, NULLing
every core-1.2/1.3 device proc and SIGSEGV'ing the render worker at each
client's first frame (2,835 host segfaults; the mesa/DXVK "maintenance4"
workarounds were misdiagnoses of this); (2) `CommittedModeStorage` initial
policy word = adapter+target POWERED (dxgkrnl issues zero boot SetPowerState
calls; the zero word refused every first flip `SourcePoweredOff`); (3) the
classic MODE_CHANGE flip is admissible while the source is still invisible
(measured order: Commit → SetVidPnSourceAddress(MODE_CHANGE) →
visibility(TRUE)); (4) scanout admission accepts dxgi 88 (the KMD's own
standard-primary author writes B8G8R8X8/XR24) and maps it to virtio format 2;
(5) a completion stale only by generation latches when the current mode still
names the same source+geometry (visibility transitions bump the stored
generation, so the first bind was stale BY CONSTRUCTION; poisoning there
killed the boot); (6) the vsync heartbeat reports
`CRTC_VSYNC_WITH_MULTIPLANE_OVERLAY3` under the D2 owner (plain CRTC_VSYNC is
ignored for flip retirement on an MPO3-capable adapter — 350 delivered ticks,
6 s wait, rollback). ⚠ (5) and (6) landed together; not separated by A/B.

⛔ Session-0 `GetDisplayConfigBufferSizes` reads **0 paths while session 1 has
an active path** — only the session-1 schtask (`HeliosDispCfg`) is evidence.
Remaining: the HOST cannot present the linear no-modifier scanout under SDL
**or egl-headless+VNC**. Owner-captured stderr 2026-08-23 08:16Z:
`egl-headless: modifier-less DMA-BUF rejected; refusing implicit LINEAR
reinterpretation` — an EXPLICIT gate (`egl-headless.c:549-558`) that returns
before the vk-linear/cpu-mmap arms, which only explicit-modifier bufs can
reach; sdl2-gl.c has no linear arm at all. Owner authorization same day:
*"its fine to add support for linear bufs in qemu back"* — the immutable-qemu
ruling is lifted for this one change (the failed exact-size OPTIMAL probe
already excludes the native-OPTIMAL alias the refusal guards against).
`helios_paintcap` currently wedges (own defect);
one dead venus context still wedges the whole transport. Details + trap list:
agent memory `display-lane-runtime-admission-chain`.

**HOST CHANGE LANDED 2026-08-23** (`qemu-helios` d1769f2a9c, awaiting a VM
relaunch to measure). Modifier-less buffers that failed BOTH the exact-size
OPTIMAL reimport and EGL now fall into the linear arms, gated on the existing
span-fits-fd check, in `egl-headless.c` and `sdl2-gl.c` alike. Two arms back
it: the LINEAR-VkImage arm now verifies its reimport's rowPitch/offset against
the declared ones and REFUSES on mismatch (a driver-chosen pitch reconstructs a
sheared picture), and below it a VkBuffer external-fd import carries no tiling
contract at all, so it cannot disagree with the declared stride. egl-headless
keeps cpu-mmap last.

⛔ **Two cheaper-looking alternatives are MEASURED OUT — do not re-open either.**
*(a) Declare a guest-side DRM modifier* (`VK_EXT_image_drm_format_modifier` on
the scanout image): three independent structural blockers. The wire has no field
for it — `virtio_gpu_set_scanout_blob` carries only `strides[4]`/`offsets[4]`.
What crosses is `VkDeviceMemory`, not a `VkImage` (`scanout.rs` builds the blob
from `memory_id`), and a modifier is a property of an image, so `MOD_INVALID` is
ACCURATE, not a gap. And QEMU's query cannot fire for a venus blob anyway:
`virgl_renderer_resource_get_info_ext` (virglrenderer 1.3.0, disassembled) fills
`has_dmabuf_export`/`modifiers` only via `virgl_egl_get_attrs_for_texture`, the
GL path gated on `use_context == 1`, so `res->dmabuf_modifier` keeps its
`virtio-gpu-virgl.c:900` INVALID init. It would also re-open the 38th-session
regression the T6/R901 ladder deletion exists to prevent (`bringup.rs:398-442`).
*(b) Use `SET_SCANOUT` instead of `SET_SCANOUT_BLOB`*: `virgl_cmd_set_scanout`
ends in `qemu_console_gl_scanout_texture(con, info.tex_id, ...)` and a venus
resource has no GL texture, so the classic command cannot carry the primary at
all. The variants that would display (`RESOURCE_CREATE_2D`, or `SET_SCANOUT_BLOB`
with `BLOB_MEM_GUEST`) put the framebuffer in guest RAM, costing a 7.9 MB/frame
GPU readback over the PCI BAR plus the same again across the wire — the
producer-side CPU present stall the 57th session forbids. The readback arm and
SET_SCANOUT do the SAME single copy; the only question is which side of the wire
it happens on, and host-local wins.

#### ⭐ THE CRITICAL PATH IS NOW THE DISPLAY LANE — decided 2026-08-11 by the owner

*"no probing or hacks, we go the proper way, i dont care if I dont see the desktop
immediately."* K2a's CPU view is reached by earning the WDDM 3.2 surface, not by
another placement experiment or an instrument (`FINDINGS.md` F17). The sequence is
`lane-kmd-display.md`'s own dependency graph, and it ends where K2a resumes:

Rows 1-11 preserve the evidence at each precursor checkpoint. Rows 12-14 carry
the authoritative direct-consumer, present-layer, and broad-demolition
source/build state and supersede their dormant/false-boundary statements without
changing the display-admission result.

| # | Unit | Why it is on THIS path | State |
|---|---|---|---|
| 1 | **D0** ETW substrate (`ddi/diag_etw.rs`, ADD) | D1/D2/D3 all emit through it | ✅ **LANDED AND EXERCISED ON THE TARGET.** Platform half uses the existing `wdk_sys` bindings, per-adapter epoch-tagged rundown plus a driver-wide provider-handle rundown, and the v7 `CollectDbgInfo` registration snapshot. KMD **22.22.277.0** cold-boots `CM_PROB_NONE`; `EnumerateTraceGuidsEx(TraceGuidQueryInfo)` sees the task GUID registered, and both standing probes remain **15/15**. |
| 2 | **D2** `direct_scanout.rs` (ADD) | D3 depends on it | **DISABLED KMD PLATFORM D2 VERTICAL LANDED; PRODUCTION KMD D2 REMAINS ABSENT.** `kmd_logic` models the exact final-HWA2/source/committed-mode/plane predicate, move-only real/parking lifetime, atomic display-backing references, domain/epoch-qualified resource-window outcomes, canonical `(context, resource)` associations, and attachment-bound context lease/census. Behind the compile-time-false `KMD_D2_OWNER_ENABLED` boundary, StartDevice constructs the sole canonical control owner from its exact initialized transport and final host-window geometry; the new platform half then consumes only canonical allocation-object facts (including transport instance), committed mode, exact source/plane attributes, and operation flags. One stable `PlaneState` owns candidate/current/backend/reset/mode-transition custody; a retained candidate is submitted by a unique nonzero fenced `SET_SCANOUT_BLOB`, accepted only with exact response flag/resource/ticket provenance, and ambiguous outcomes remain quarantined until verified physical reset. Explicit unbind replaces current with a permanent KMD-owned black parking blob before optional `SET(0)`; Stop/Remove/failed-Start/skipped-Stop/TDR and power/mode transitions compose without early release, and D0 plane events 7/8/9/10 are emitted at their causal transitions. The atomic-boundary gate now checks the exact false definition, guarded canonical authority, direct transport entry paths, and deliberate enabled/unguarded mutations. Windows `cargo check` is green at the exact 22-warning baseline, all 510 `kmd_logic` tests and the Linux retirement gates pass. This is source verification only: the legacy branch remains the sole production behavior; no activation, legacy removal, migration, deployment, or runtime-correctness claim occurred. The normative LINEAR producer profile remains intentionally unwired while today's OPAQUE producer disagrees. §6 items 6 and 8 remain resolved with immutable `qemu-helios` and parking-first unbind. |
| 3 | **D3** `mpo3.rs` (ADD) — the seven MPO3 slots | `doc:2854` rejects a 3.2 package without a complete MPO3 surface | **DISABLED KMD D3 MPO3 TABLE LANDED; PRODUCTION KMD D3 REMAINS ABSENT.** All seven WDK-28000 callbacks are registered and ABI-pinned. CheckMPO3 returns reserved-clean false unless the false D2 boundary is crossed, then admits only the exact one-primary HWA2/source/mode/attribute profile. SetMPO3 touches only its retry output and atomics above PASSIVE, uses the documented `STATUS_RETRY`/`PrePresentNeeded` continuation, and at PASSIVE revalidates the exact OS context allocation, context owner, nonzero GPUVA, operation flags, default refresh duration, and actual plane attributes before retaining the D2 candidate. Zero-plane input uses parking-first explicit unbind; no special output, PostPresent, Hsync, or HW-flip-queue flag is set. The cap callbacks expose only one RGB plane and unity transforms after activation; ValidateUpdate resolves the exact live open-allocation object and rejects every mutation; ControlMode marks every request unsatisfied; PostMPO is an always-success counted tripwire. The generated 192-slot audit now classifies all seven as implemented (89 implemented / 91 disabled / 4 pending / 8 retiring). The atomic-boundary gate covers the false owner switch, absent MPO capability, 2.1 surface, guarded authority, and deliberate enabled, unguarded, decoy-guarded, advertised-cap, and raised-surface mutations. Windows `cargo check` remains green at exactly 22 warnings; all 510 `kmd_logic` tests and every Linux retirement gate pass. `KMD_D2_OWNER_ENABLED=false` and `SURFACE=Wddm2_1GpuMmu` are unchanged: no legacy removal/migration, activation, deployment, adapter restart, reboot, or runtime-correctness claim occurred. |
| 4 | **D4** then **D5** (`display.rs`, `present_packet.rs`) | D5 is part of the MPO3 surface D9 gates on; D5 serializes after D4 (same file) | **DISABLED KMD D2/D3/D4/D5 DISPLAY AUTHORITY LANDED; PRODUCTION KMD D2/D3/D4/D5 REMAIN ABSENT.** D4: classic SetVidPn consumes the exact OS allocation/source/address/segment/flags through D2 admission; the DMA arm carries the exact device-specific open handle through an immutable canonical open/allocation association. Both retain a move-only candidate before accepting one exact fenced SET. Device-DIRQL uses four preallocated slots, a private adapter-bound non-cloneable capability, a bounded no-spin gate shared by every queue mutator, an exact-pinned allocator-disabled virtqueue, and a separately boxed queue with fail-closed teardown custody. The D4 source gate audits the full raised/synchronized call, macro, and indirect-call surface and mutation-rejects hidden helpers/macros, waits, allocation, signals, locks, PCI status access at raised IRQL, queue bypasses, dependency drift, forged proofs, publication/lifetime races, unguarded authority, legacy D4 mechanisms, and boolean-only activation. PASSIVE-only PCI reset is token-typed, raw-queue-lifetime-pinned, and completes before the transport spinlock is re-entered. The dormant D4 branch has no heuristic resource lookup, snapshot substitution, coalescer, fast bind, LINEAR fallback, or retry polling; D0 transitions occur at the first legal exact-completion DPC edge. D5: Present selects the `FlipWithMultiPlaneOverlay` union arm once and the false owner boundary returns counted `STATUS_NOT_SUPPORTED` before dereferencing it. The dormant arm admits exactly one enabled source-0/layer-0 plane after null/alignment/count/reserved-bit checks, validates its device-specific handle through both canonical live `OpenAllocation` projections and the current immutable HWA2 primary profile, and preserves the exact handle/segment/physical-address tuple. WDK 28000 marks the Win7+ patch output unused, so D5 emits the existing ordinary HERF packet with no invented allocation-list index, patch entry, private HPS record, or display lease; SetMPO3 remains the first display-custody transition. The D5 gate fixes the complete call surface and mutation-rejects guard/decoy/boolean-only activation, union reinterpretation, zero/disabled/multiple planes, pointer/count/profile/provenance/capacity weakening, heuristic identity, private tickets, display-lifetime/control work, PresentToHwQueue union reads, and D4 layout/DIRQL regressions. Windows `cargo check` remains green at exactly 22 warnings; all 510 `kmd_logic` tests, the unchanged D4 proof, and every Linux retirement gate pass. The false branch remains the only production authority, and activation is rejected while legacy authority/host-write paths exist. `KMD_D2_OWNER_ENABLED=false` and `SURFACE=Wddm2_1GpuMmu` remain unchanged; no deployment, reboot, adapter restart, runtime-correctness claim, HPS2 removal, legacy demolition, or activation occurred. |
| 5 | **native fences (K7)** | `doc:2854` rejects 3.2 on an incomplete FENCE surface too | **DORMANT NATIVE-FENCE SURFACE LANDED; PRODUCTION NATIVE-FENCE AUTHORITY REMAINS ABSENT.** StartDevice now publishes its exact `AdapterLuid`; stable per-adapter state owns feature admission, LUID, lifecycle, nonwrapping epochs/generations, and bounded populations. QueryAdapterInfo supplies the exact validated native-caps arm and publishes `NativeGpuFence` only through the full surface/D2-owner/LUID/feature/lifecycle conjunction. Six callback slots remain registered, both native-log slots remain NULL/Disabled, hardware queues remain unsupported, and `No64BitAtomics` plus optimized native interrupts remain zero. HNF1 stays 64-byte/pointer-free with input generation zero and successful output generation nonzero; create/open/update/teardown reject foreign, stale, malformed, overflowing, or partially valid input before mutation. A real completed WDDM submission plus a current monitored population is the only native-rescan edge, delivered through D4's audited DIRQL helper. Reset, stop, remove, and skipped-stop invalidation ordering is preserved. NF-UAF-1 is closed by dxgkrnl's documented global/local reference order, with a bounded refuse-and-retain fallback if Destroy ever arrives with locals outstanding. The K7 gate imports the unchanged D4/D5 source proofs and rejects 35 in-memory mutations through the real gate. Windows `cargo check` is green at 13 warnings—the prior nine F9 dead-symbol warnings are gone and no new category appeared; all 518 `kmd_logic` unit tests plus both integration suites and every Linux retirement gate pass; the callback audit remains 89 implemented / 91 disabled / 4 pending / 8 retiring; WDK-28000 bindings remain byte-identical at SHA-256 `148b75db41e093dc6783be4f5bb3ea84b2c7c39ef316fe711b3f2a5a0668bea2`. `SURFACE=Wddm2_1GpuMmu` and `KMD_D2_OWNER_ENABLED=false` are unchanged, so no callback or cap is reachable in production and no activation, deployment, legacy removal, runtime-correctness, or flip-readiness claim is made. |
| 6 | **D9** the slot audit + `SURFACE` → `Wddm3_2GpuMmu` | the flip itself | **THE LOCAL WDDM 3.2/D2/NATIVE-FENCE ACTIVATION PACKAGE IS DEPLOYED AS KMD 22.22.284.0 BUT REMAINS RUNTIME-UNADMITTED; HPS2 RETIREMENT AND PRODUCTION CORRECTNESS ARE NOT ESTABLISHED.** The 192 callback slots are terminal at 91 Implemented / 101 Disabled / 0 Pending / 0 Retiring. Escape and all seven HW-context/HW-queue-family registrations are absent; hardware queues, native-fence logs, `No64BitAtomics`, optimized native interrupts, Hsync/HW-flip/post-composition authority remain unsupported. The two exact WDK-28000 diagnostic callbacks validate IRQL, pointer/range/alignment, enum/type, payload/profile, and aliasing before publishing bounded local output. `SupportMultiPlaneOverlay`, `MaxOverlayPlanes=1`, Direct Flip, and every segment Direct-Flip bit now derive from the exact SURFACE-owned D2 package, with the one-primary/RGB/unity profile. The generated human/machine audits are fresh; `tools/d9-wddm32-activation-gate.py` rejects 73 in-memory mutations, including mixed/decoy activation, stale audit, unsafe diagnostics, reopened legacy display continuations, UMD/KMD MPO drift, physical-adapter-cap drift, the superseded build-28000 guest minimum, and false cold-admission claims. Windows `cargo check` passes at exactly 13 warnings; all 518 `kmd_logic` unit tests plus both integration suites, both integration suites, D4/D5/K7 safety proofs, all 35 K7 mutations, and every Linux retirement gate pass. VM-generated and offline WDK bindings agree at 3,834,340 bytes / SHA-256 `148b75db41e093dc6783be4f5bb3ea84b2c7c39ef316fe711b3f2a5a0668bea2`. The exact `.284` SYS is 838,904 bytes / SHA-256 `873767b730ec269e5b535829d80da650b321900f5fa38769776e98fcc4ad5ef2`, staged as `oem116.inf`, and cold-loaded on build 26100 with Code 0 and dxdiag reporting WDDM 3.2. DWM starts after boot, but there are zero active DisplayConfig paths and no KMD `SET_SCANOUT_BLOB`: DWM loads WARP rather than `helios_umd.dll`, and a direct Helios `D3D11CreateDevice` fails `0x80004005` after the selected Mesa ICD's legacy Gate-5a `D3DKMTEscape` receives `STATUS_NOT_SUPPORTED`. The owner explicitly rejects restoring Escape and accepts a dark display while the escape-free Mesa/KMD-core replacement advances. No cold-DWM admission, push, K2a work, wider K1/D6/D7/D8 demolition, full HPS2 retirement, or runtime/visible-desktop correctness claim occurred. CpuHostAperture and other §18 legacy retirement mechanisms remain live. |
| 7 | **K2a shared-backing CPU view** | D9 makes the WDDM 3.1+ `ShareBackingStoreWithKmd` contract available. K2a uses the exact WDDM allocation and OS-supplied backing-store MDL; Mesa retains the creating process's ordinary `D3DKMTLock2` VA. A scoped upstream-rebased QEMU change imports the exact guest pages into Venus without HPM1 or virglrenderer changes. | **LANDED AND EXERCISED ON THE TARGET.** Four 1 MiB reply slots; roles 1–3 only; role 4 refused. KMD 22.22.288.0 / `oem120.inf`, deployed SYS 849,144 bytes / SHA-256 `df5903586a067b3c2e4047713278f92b38e7573e695fcd12bdd63d25d2556b86`; interface size 576, feature enabled; lifecycle probe 52/52; exact 4 MiB renderer alias passed correlated boundary-byte tests in both directions; QEMU returned to 227 total FDs (one `/dev/udmabuf`, two `/dmabuf:`). Escape and HWQueue registrations remain NULL. K11 and Mesa A3/A4 are not started by this tranche; no visible/cold-DWM admission is claimed. |
| 8 | **K11 per-session host Venus transport** | K5/K6 already own exact WDDM process/device/context/session lifetimes and finite HNR2 admission. K11 binds one distinct stock Venus context/object namespace to each exact session, gets the real finite INIT reply through a private host target, and publishes it only through the K2a role-1 HVR1 pool. | **LANDED AND EXERCISED ON THE TARGET.** KMD 22.22.296.0 / `oem128.inf`; actual host INIT reply; K11 14/14, HTS1 15/15, updated HNR2 15/15, and K2a 52/52. Same-process sessions were distinct, another process was isolated, repeated and abrupt teardown left no stale session, and a held-capability adapter restart drained cleanly before a fresh 14/14. Four 1 MiB slots and the 64 MiB logical ceiling are unchanged; allocation/GPU work still refuses; QEMU/virglrenderer/HPM1/kernel parameters are unchanged. Escape and HWQueues remain NULL. Mesa A3/A4, visible admission, HPS2 retirement, and production correctness remain outstanding. |
| 9 | **bounded post-K9 KMD executor/bootstrap** | K9 orders terminal results but cannot retain, validate, patch, or execute general HNR2 work. | **LANDED; SOURCE/BUILD-VALIDATED ONLY** at root `e8819c1`. Truthful role 4, bounded session-owned custody, generated A3/A4 schema validation, exact private-copy patching, same-endpoint stock-Venus execution, same-context native-fence ordering, terminal completion into K9, and reverse drain are present. The installed target remains the older K11 KMD; no new runtime evidence exists. |
| 10 | **Mesa A3/A4** | Consume the exact session/executor without Escape, duplicate host instance, raw resource ID, or synthetic completion. | **LANDED; SOURCE/BUILD-VALIDATED ONLY** at Mesa `2c2763b8b1a` + `5cbc0254f43`. The escape-free HVM1 renderer and mode-dispatched HNR2 submit path build on Windows and pass their mutation gates. A7-dependent command-buffer closure stays refused, and A5-A9 plus the present layer remain the handoff. No ICD installation or target exercise occurred. |
| 11 | **Mesa A5/A6; F21 stop** | Expose only the fixed direct record-only interface and prepare the exact lower profile before closing all allocation use/operand identity. | **HISTORICAL SOURCE/BUILD CHECKPOINT** at Mesa `4ea18b3512f` + `d07d1d13687` + review fix `478c71a0fff`. It correctly stopped at the then-unauthorized outer-token producer boundary and is superseded by row 12. |
| 12 | **F21 direct consumers, outer UMD/KMD, Mesa A7-A9** | Carry the exact outer allocation token from its UMD owner through DXVK/vkd3d into Mesa, resolve it back to current WDDM identity at submission, then close A7 and retire only the selected Windows ring/build path. | **LANDED; SOURCE/BUILD-VALIDATED ONLY.** Protocol `eec9564` + `ccd2891`, KMD `eccfd19` + K4 follow-up `d75f649`, DXVK `1cf7e631`, vkd3d `9a2716c0`, UMD11/12 root `0fe5677`, Mesa A7 `ae9c9f4c89d`, and A8/A9 `fa61439bfd7`. The fixed A5 table is unchanged; no generic registry/fallback or new host carrier exists. Nothing was installed or target-exercised, and the present-layer B lane is the stop-boundary handoff. |
| 13 | **`VK_LAYER_HELIOS_present` B0-B9** | Remove the Mesa HPS2 WSI writer and move native Win32 WSI into the separate, acyclic layer using the landed A5-A9 lower contract and F8-correct fence direction. | **LANDED; SOURCE/BUILD-VALIDATED ONLY.** Mesa `bb7a787a5a1`, `e06abf3025f`, `11d723ea10b`, `51054338012`, `871c62bf8a0`, plus bounded review fix `8b3c9359b5a`. The layer and lower ICD build as separate DLLs with their exact export/dependency boundaries. Nothing was registered, installed, or target-exercised; broad HPS2 demolition remains the handoff. |
| 14 | **Broad HPS2 demolition, K1/K8/K10/K14** | Delete every active HPS2/reverse-reader/Escape/private-sharing carrier after the direct and present graphs exist, then source-activate one coherent retirement package. | **LANDED; SOURCE/BUILD-VALIDATED ONLY.** DXVK `418f5745`; root UMD/KMD/protocol/lifecycle/host/K14 commits `b955a2c`, `60a9988`, `5e1fbda`, `e7bdb0f`, `3663956`, `e96b304`, `ce3765a`, `5901f55`, `dcf0e1e`; vkd3d and Mesa unchanged after audit. Generation 4 / KMD 22.22.297.0 source is coherent and all focused/serial gates and affected builds pass. No package was assembled, installed, registered, or target-exercised. |

⚠ **The desktop stays dark for most of this**, by the owner's explicit acceptance.
D9, K2a, and K11 have crossed their source and exact-target runtime boundaries;
the post-K9 executor, Mesa A3-A9 direct-consumer cutover, present-layer B0-B9,
and broad HPS2/K1/K8/K10/K14 cutover have crossed source/build only. The display
therefore remains runtime-unadmitted. The last measured target still selected
the old installed ICD path, fell back to WARP before a primary was programmed,
and reported zero active paths. Escape must not be restored. This tranche stops
after source demolition and package-source activation, before any package
assembly, installation, registration, or runtime exercise; neither this
source/build evidence nor the historical target evidence evaluates the new path.
Only visible DWM startup on WDDM 3.2 admits the surface; a build, callback count,
Code 0, counter, map result, or log cannot. F1 permits the measured build-26100
target; WDK 28000 remains the compile-time header/binding authority.

4. **Protocol dead-carrier cleanup is source-closed** at `e96b304`; surviving
   legacy declarations remain only where a current HWA2/HVM1/HQA1/HNR2/HOB1/
   HOS1/HNF1 or released-platform contract still owns them.
5. **K1 and the non-HPM1 K8/K10 remainder are source-closed** at `60a9988`,
   `5e1fbda`, and `ce3765a`. K2/K3/HPM1 and runtime admission remain outside
   this tranche.

#### What round 4's 8 code defects were, since they are the class worth repeating

Two were kernel-side and would not have survived a deploy: `kmd_authored` was
derived from `HELIOS_HWA2_FLAG_STANDARD`, a wire bit **any** producer may set
(it is not in `HELIOS_HWA2_FLAG_KMD_OWNED_MASK`, and cannot be added to it —
the KMD's own standard-allocation data re-enters through the same DDI and would
refuse itself), so a hand-built HWA2 skipped the `AcSize` undersize guard;
and `venus/bringup.rs`'s non-saturating `round_up_page` became reachable with a
guest-supplied `byte_size`, which is a checked-add **panic inside
`DxgkDdiCreateAllocation`** holding the venus mutex. Four were present-layer C++
(a latched manual-reset event making `vkAcquireNextImageKHR` a spin loop; a
teardown wait satisfied by a stale auto-reset signal; three failure arms that
wedge `vkDestroySwapchainKHR` forever; a publish-before-fill race). One was the
D3D11 `byte_size` ignoring depth/array/mips. One was a vkd3d use-after-release in
a log line.

⇒ Every one is a **runtime** defect found by **reading code**. That is the review
worth keeping.

⛔ **`protocol/src/wddm_legacy.rs` CANNOT be deleted as a file, and its module
header is TRUE at HEAD, not stale.** Measured: the file declares **48**
top-level `pub` items (plus 10 inherent `pub fn`s). **19 of the 48 have live,
non-comment Rust callers**, in three groups: the `HELIOS_PRESENT_*` ticket family
with `HeliosPresentPrivateData` / `HeliosPresentRenderCmd` /
`HeliosPresentRefreshCmd` (**13 files** across `umd/`, `umd12/` and
`kmd_render/`, incl. `umd/src/forward/{present,snapshot,resource,state}.rs`,
`umd/src/device_funcs.rs`, `kmd_render/src/ddi/{submit_command,display}.rs`,
`kmd_render/src/device.rs`); `HeliosD3D12SubmitCmd` with its magic and version
(**7 files**, incl. `umd12/src/forward12/queue.rs`, `umd12/src/bridge12.rs`,
`kmd_render/src/ddi/{submit_command,present_packet}.rs`,
`kmd_render/src/virtio/gpu/mod.rs`); and
`HELIOS_WDDM_ALLOC_KIND_{DEVICE_MEMORY,STANDARD}`, whose only caller is
`kmd_render/src/ddi/display.rs`. Deleting the file fails to compile
`kmd_render`, `umd` **and** `umd12`, in a build only the VM can run. ⇒ the
header's *"`kmd_render`, `umd`, and `umd12` still call it and cannot be made to
compile without it"* is a correct description of HEAD.
⚠ **What is legitimate today is deleting the other 29 symbols.** The
present-ticket family waits on **K5**. And do not conflate this with the §8.6
gate: that gate lists **8** *allocation-identity* names
(`HeliosWddmOpenIdentity`, `HeliosWddmAllocPrivate`, `HeliosWddmAllocMeta`,
`GlobalVidMmTracker`, `VidMmTrackerTable`, `AdoptedUmdResource`,
`HELIOS_WDDM_ALLOC_KIND_TRACKING`, `write_open_identity`) — only **5** of which
are among the 48 (`VidMmTrackerTable`, `AdoptedUmdResource` and
`write_open_identity` no longer exist anywhere). "All eight retired symbols have
zero live callers" is correct **and is a statement about a proper subset**; it
does not license deleting the file.

⛔ **Deploying K4 without mesa A3 renders nothing at all — and that is the
retirement's intended intermediate state (`K4-CONTRACT.md` §5), not a
regression.** State the scope precisely, per §5.1: **every Win32 shared-memory
import and export refuses; ordinary venus allocation still works.** Exactly two
of `vn_AllocateMemory`'s five arms refuse (`VkImportMemoryWin32HandleInfoKHR`,
and an `export_handle_types` including `..._OPAQUE_WIN32_BIT`); the plain arm,
the `VkImportMemoryResourceInfoMESA` resource-id import and the dma-buf import
still allocate. *"Every `vkAllocateMemory` fails"* is the overstatement §5.1
exists to correct — a plain venus render allocation working is **not** evidence
the deploy went well.

#### ⭐ The A3 scope question is CLOSED (owner, 2026-08-10): **full A3 as the lane brief scopes it**

The choice was between the brief's full A3 and a narrow subset (the C57 import
carrier re-pointed at HWA2 plus a resid patch list on the existing
`HELIOS_ESCAPE_SUBMIT_VENUS` escape, which would have restored the desktop for
roughly one L + one M unit). The owner chose full A3. The rejected option is
recorded because it is the cheap path back to a live desktop if the schedule ever
needs one.

⛔ **The honest scope of "full A3" is FIVE units, four of them XL** — and the two
KMD ones are not optional, because A1/A2/A3 speak HTS1 and HNR2 to a kernel that
does not implement either:

| unit | file | size | state |
|---|---|---|---|
| **A1** HTS1 session | `vn_helios_translation_session.{c,h}` | L | ✅ **LANDED** `6ad43fb`; **rewritten onto A2's encoder in `1b97c64`** — see below, its own encoder was wire-invalid in three ways |
| **A2** native KMT lane | `vn_helios_native_kmt.{c,h}` | XL | ✅ **LANDED** `icd/mesa` `1b97c64`, reviewed and repaired in `f235ca4`, gate `c20d162`/`c90e5a8`. Cross-builds; its encoder half is **executed** by `tools/hnr2-encoder-gate.sh` against `protocol/`'s validator (18 batches / 86 fragments; 10 deliberate mutations caught). ⚠ The KMT half is **compile-verified only** — nothing executes it until K5 |
| **A3** renderer rewrite | `vn_renderer_helios_hvm.c` plus the narrow renderer/memory/instance seams | XL | ✅ **LANDED** `2c2763b8b1a`; escape/IOCTL/blob/present-stream/named-fence/raw-resource-ID backend removed, HVM1 roles 1–4 and exact C57/HNF1 imports source/build validated only |
| **K5** KMD HTS1 sessions | `kmd_render/src/ddi/translation_session.rs` | L | ✅ **LANDED AND NOW HOST-REACHABLE THROUGH K11.** The K5 session/slot/attach lifetime remains the authority; K11 supplies the distinct host namespace only after the exact role-1 allocation binds. HTS1 remains **15/15** on KMD 22.22.296.0. See the historical checkpoint below and the K11 final section above. |
| **K6** KMD HVC1/HNR2 render | `kmd_render/src/ddi/native_render.rs` | XL | ✅ **LANDED AND EXERCISED ON THE TARGET; K11 CLOSES ONLY ITS PURE INIT HOST HANDOFF.** The existing Render/Patch/SubmitCommand decoder and context-local slot ownership now execute one finite allocation-free `vkCreateInstance` INIT, validate its actual host reply, publish HVR1, and admit the exact host-completed WDDM record. Allocation-backed, queue, general-schema, residency, staging, and epoch work still stop at the named K6 counters. The updated HNR2 probe passes **15/15** on KMD 22.22.296.0 and verifies a second INIT on the same live session refuses without stranding its slot. |

⭐ **What de-risks it:** every wire record these five need is already written and
offset-asserted in `protocol/` — HTS1/HQA1, HVC1/HNR2/HVM1/HVR1 and their C
mirrors. That is exactly the "~14.6k lines with no consumer" population
`K4-CONTRACT.md` §10 records. **A1–A3 and K5–K6 are those consumers**, so the
work is call sites and state machines, not new ABI.

⛔⛔ **CORRECTION, measured 2026-08-10 while landing K5: `helios_translation_session_create`
HAS NO CALLER.** This section said "K5 is the unit that makes the A-lane testable
on the target", and that is **false in the direction nobody checked**. A1 and A2
are compiled into the ICD (`src/virtio/vulkan/meson.build:134,139`) and invoked
by **nothing**:

```
git -C icd/mesa grep -n 'helios_translation_session_create' -- src
  # only the definition (:540) and the declaration (.h:51)
grep -rn 'helios_session_generation\|helios_session_device_handle' icd/mesa/src/
  # empty outside the file that defines them
```

`helios_native_context_create` (A2) is called only by A1. ⇒ the **whole A-lane KMT
half is unreachable from the ICD**, and landing K5 does not change that: it makes
the *kernel* side exist, while the *guest* side still has no caller. The caller is
**A3**'s `vn_renderer_helios.c` rewrite. ⚠ Both greps are needed and they fail in
opposite directions — `git grep` does not descend into the `icd/mesa` submodule
from the parent repo, and `grep -r` descends into `.claude/worktrees/`
(`K4-CONTRACT.md` §9.1). Neither alone measures reachability.

⇒ **`tools/hts1_session_probe.c` exists because of this.** It performs A1's exact
KMT sequence plus the refusals K5 owes, checks each against a stated expectation,
and identifies the Helios adapter by its *legacy* context profile so the test is
not circular. Pre-K5 baseline, measured on the deployed driver: **4 pass / 7
fail**, with the two legacy-behaviour controls (E, F) passing. That is the
negative control that makes a post-deploy pass mean something.

⭐ The rest of the old paragraph stands: "untestable" was too strong, and A2
proved it. Running A1's encoder against `protocol/`'s own validator found three
defects a reading round had passed over (the use table written after the payload,
a zeroed `expected_allocation_generation`, a monitored fence missing all three
§10.7 flags). ⇒ **Split any remaining A-lane unit into a pure half and a KMT half,
and gate the pure half.**

#### K5 landed, and what it does NOT do

**Landed:** the pure half is `helios_kmd_logic::translation_session` (the session
phase machine, the bounded per-process session ledger, the generation source, the
endpoint table, the four-slot reply pool, the control-Render session admission,
the per-endpoint host-dispatch FIFO and the snapshot accounting) — **263 kmd_logic
tests, was 211**. The platform half is `kmd_render/src/ddi/translation_session.rs`
plus `device.rs`: 22 named counters, the RDRAND capability source, the refcounted
`SessionObject`, and `DxgkDdiCreateContext`'s private-data dispatch.
`cargo check` exit 0 at the **22-warning baseline**, measured before and after.

⭐ **Measured on the target, KMD 22.22.264.0** (`tools/hts1_session_probe.c`,
14/15 checks): the HVC1 control context is admitted and returns the §10.7:1734-1738
minima — **262144 / 4096 / 4096, `DmaBufferSegmentSet=0`** — while the legacy
D3D-runtime profile is unchanged and the desktop is unaffected. A second control
context, an unsupported HVC1 mode, an HVC1 *queue* context and an HQA1 with a
forged capability are all refused, each with the right counter and reason code
(`TsHvc1Rej=0x00010107` is exactly `Hvc1Reject::ModeUnsupported`). The role-1
64-MiB HVM1 pool is created, made resident, Lock2-mapped, and its first byte,
first slot boundary and last byte are all writable. `TsSessNew=1`, `TsSessFree=1`
— no leak.

⛔ **The `DmaBufferSegmentSet` value is a PER-ARM branch, not a flip.** `device.rs`
records a measured dxgmms2 null-deref in `VidMmInitDmaPool` when a runtime
context gets a zero segment set; §10.7:1981 keeps segment 1 as "the only nonzero
choice for existing D3D runtime contexts, while HVC1 selects zero". Both arms now
exist and the probe checks both. A global flip is a boot-killing regression.

⚠ **Historical K5 boundary, now closed only for finite INIT by K6/K11.** At the
K5 checkpoint the finite INIT had no HNR2 caller; after K6 it still refused
because no host Venus context existed. K11 now creates that context, validates
the actual finite host reply, reserves the bounded endpoint/ring namespace, and
only then lets `session_init` reach `Live`, so HQA1 validation is reachable.
This does not erase the boundary around allocation-backed or GPU-dependent
operations: those still refuse at the named K6 seams until their later owners
land. `K4-CONTRACT.md` §8 remains the historical three-state record.

#### ⭐ The write-back blocker is CLOSED (2026-08-11, KMD 22.22.267.0) — and it was neither of the two candidates

The K5 probe's one failure — `object_generation` / `segment_page_shift` /
`allocation_alignment` all zero in the caller's buffer, with `TsPoolBind=0` **and**
`TsPoolRej=0` — had two named explanations and the answer was a third:
**dxgkrnl discards a KMD write into `DXGK_ALLOCATIONINFO::pPrivateDriverData`
altogether** whenever the buffer came from user mode. Not "does not copy it back";
it does not keep it at all. The full measurement is `FINDINGS.md` **F11**; the
short version:

* `tools/hwa2_writeback_probe.c` issues the SAME record through four create-call
  shapes from one process. Seven admitted creates on 22.22.266.0 — HWA2 at 168
  bytes and HVM1 at 64, bare and resource-associated, both thunks — **seven
  zeros.** The call shape is not the variable.
* The diag ring shows the same bytes arriving **unstamped at the open**: 5 of 7
  HWA2 opens rejected (`0x0C02_00E6`). The two that validated are the OS standard
  allocations this driver authors itself, whose buffer dxgkrnl owns.
* The WDK headers said so: `DXGK_ALLOCATIONINFO::pPrivateDriverData` is `// in:`,
  `DXGK_OPENALLOCATIONINFO::pPrivateDriverData` is `// in/out:`, and
  `D3DDDI_ALLOCATIONINFO2`'s `in(out optional)` is fulfilled by the OPEN.
* ⇒ **`FINDINGS.md` F10 is FALSIFIED.** Its instrument was sound; its inference
  was under-determined, because the pre-retirement KMD wrote the identical
  48-byte record at BOTH create and open, so a pre/post diff across
  `pfnAllocateCb` could never say which write the bytes came from.

**Repaired** in `a8527e2`: `stamp_open_hvm1` completes the HVM1 create-output at
`DxgkDdiOpenAllocation`, through the pointer the WDK annotates `in/out`. On a
**cold-booted 22.22.267.0** `tools/hts1_session_probe.c` is **15/15** (was 14/15),
`OaHvm1Stamp=1`, and **`TsPoolBind=1`** — K5's reply-pool binding fires for the
first time. mesa A1 is unblocked. `K4-CONTRACT.md` §8.5's gate was amended, not
deleted (HWA2/HOC1 still untouchable; the one HVM1 write is itself checked and was
defeated 7 ways).

⛔ **STILL BROKEN, and now named: HWA2 has the same defect — but it is LATENT.**
HWA2's create-output is discarded exactly as HVM1's was, so the "written once at
create, carried to `OpenResource`" premise is false for every UMD-supplied
allocation. ⚠ **It is NOT what stops the D3D11 UMD today, and this document said
otherwise for one commit.** Measured with the HWA2-producing UMD hot-installed on
22.22.267.0: across 9 processes, `hwa2_output_invalid=0` and
`hwa2_create_venus_backing_needs_mesa_a3=1` — the UMD refuses *earlier*
(`umd/src/forward/resource.rs:247`) and no create reaches `pfnAllocateCb` at all.
⇒ the `dwmcore.dll` crash-loop in the table below **is** the A3 gap, as this
document originally said; the write-back defect sits behind it and becomes
reachable only when A3 lands. Repairing it means the same stamp for HWA2, which
§8.5 forbids and which *does* have openers that can disagree; the candidate
designs are in F11. **Owner decision, and no longer an urgent one.**

⭐ **`win_build_kmd` is FIXED.** The cert in `CurrentUser\WDRTestCertStore` was
removed so cargo-make's `generate-certificate` regenerated it; both signtool
invocations now succeed and `install-helios-kmd.ps1` re-imports the new `.cer`
into Root and TrustedPublisher. KMD builds are no longer blocked.

⭐ **A measurement that sharpens `K4-CONTRACT.md` §5, taken 2026-08-10 while
reverting a deploy.** "Deploying K4 without mesa A3 renders nothing at all" is
right, but it has TWO distinct failure shapes and which one you see depends on
the UMD half:

| KMD | UMD | `AcMagic` | symptom |
|---|---|---:|---|
| 22.22.265/266 (K4+K5) | 6,408,192 (pre-HWA2) | **1920** | every allocation create refused; DWM stable, desktop black |
| 22.22.266 (K4+K5) | 6,420,480 (HWA2 producer, built from HEAD) | **1** | allocations admitted; **DWM crash-loops** in `dwmcore.dll` (`0xc00001ad`), LogonUI faults, no explorer |

⇒ `AcMagic` is the discriminator for "is the UMD half matched", and a *low*
`AcMagic` is not good news on its own — it means the failure has moved
downstream to the identity A3 has not yet supplied. ⛔ Neither shape is K5's:
`AcMagic` counts `create_one` rejecting an unrecognised record magic, a path K5
does not touch, and DWM was stable under 265 with K5 already in it.

⚠ The box was left on the **pre-HWA2 UMD** (the first row) — owner's call:
*"i dont care about a compositing desktop until its expected to have a
compositing desktop."* Revert it with `win_install_umd`, not a KMD reinstall.

⛔ **`win_build_kmd` is currently BROKEN on the VM** and it is not a code fault:
`signtool` fails with "No certificates were found that met all the given
criteria" because the cert in `CurrentUser\WDRTestCertStore` reports
`HasPrivateKey=True` with a missing key container, and cargo-make's
`generate-certificate` skips regeneration whenever a cert merely exists. Removing
that cert so it regenerates is the fix; it needs a permission the agent was
denied. Until then, **UMD-only changes must go through `win_install_umd`**, which
needs no signing and no reboot.

⭐ **The gate is `tools/hts1-attach-gate.sh`** (gate 10 of 10): `tools/hts1_attach_probe.c`
builds HTS1 INIT and HQA1 records from `protocol/include/helios_translation_session.h`
— the header the ICD compiles — each carrying the verdict the guest expects, and
`kmd_logic/tests/hts1_attach_gate.rs` replays all 41 through the KMD's own session,
decoding by explicit §10.4 offset rather than by the `bytemuck` derive both halves
share. 6 admitted / 35 refused. **Defeated 5 ways to prove it can fail**: removing
the capability check, the engine-class comparison and the queue-index cross-check,
and making the context-generation watermark non-strict, all turn it RED. The fifth
— deleting `declare_endpoint`'s conflict branch — stayed green, correctly: that
branch is unreachable from `attach`, which passes the recorded descriptor as the
expectation, so `protocol`'s own validator catches the mismatch first. A unit test
covers it directly instead.

#### The A2 review, as the ordinary code review the owner asked for

5 lenses (encoder arithmetic vs the validator arm-by-arm, the D3DKMT contract vs
the real WDK headers, concurrency/lifetime, the A1 refactor vs its predecessor,
and "can the gate pass with a broken encoder") + a skeptic per finding. **9
findings → 5 survivors → 3 distinct defects**, all repaired in `f235ca4`: a
COMMIT writing past the `D3DDDI_ALLOCATIONLIST` after a mid-batch list shrink;
reply-slot exhaustion failing the call where §10.7:2046 says wait (the code cited
§10.7:1849, which is the KMD's *staging* pool — a rule from the right document
and the wrong pool); and an HVR1 arm that was a strict subset of protocol's
validator in three ways that each turn a malformed reply into a hang, a prefix
reported as a complete result, or bytes spliced at the wrong offset.

⭐ Two findings the skeptic **refuted as defects** still changed the tree: their
coverage argument stood, so they became corpus cases (`c90e5a8`). A refuted
finding is not always worth nothing — but what it buys is a test, not a patch.

⭐ **The A2 encoder gate is the shape to copy** (`tools/hnr2-encoder-gate.sh`):
the ICD source file is compiled and RUN on Linux, its output replayed through the
Rust validator the KMD will run, and the runner fails if the corpus is too small
to be real. It is the first ICD source file in this project executed by a test,
and it makes `lane-mesa.md` §5.4 ("every assertion this lane can make on the host
is a compile-time one") obsolete.

## Stage pivot, 2026-08-05

The **Performance, Stability, Conformance (PSC)** stage is closed as a *stage*;
its stability contracts remain permanently in force and its performance record
is kept below as WS2 — read it before opening any new perf work, because it is
mostly a list of things that have already been tried and measured.

**Why now.** The present-queue stall was root-caused and fixed (WS2, `PresentWmk`,
KMD 22.22.244.0), and the remaining limit is named rather than suspected: the WDDM
FIFO head now blocks on `stream_ready` — the frame's own producer completion on
the host — at `WfBStrm`/`WfBWire` ≈ 15220/161, against a render-thread producer
floor of ~3.7 ms/frame. There is no further sweep to run; the next perf gain needs
a new causal hypothesis, not another arm.

**The new order of business:**

1. **D3D11 correctness / conformance** — charter in `CONFORMANCE.md`, plan in WS3.
2. **D3D12** — charter in `DX12.md`, detail in `docs/dx12/`. **The strategy question is
   CLOSED as of 2026-08-05**: Helios ships a real D3D12 UMD, `helios_umd12.dll`,
   implementing `d3d12umddi` and forwarding into vkd3d-proton's `ID3D12*` COM
   objects — the D3D11 architecture with DXVK swapped for vkd3d and
   `UserModeDriverName[2]` swapped for `[3]`. The app-local vkd3d arm is Phase 0
   of that plan, not an alternative: it proves the whole lower half (vkd3d +
   dxil-spirv + venus + KMD + present) with zero Helios code. Decisions and the
   twelve-lane evidence merge: `docs/dx12/DECISIONS.md`. Checkpoints:
   `docs/dx12/GATES.md` (`D12-G0 … D12-G11`). Today `OpenAdapter12` still
   refuses, and must keep refusing until the commit that makes its body
   reachable.
   *Measured up front:* the guest satisfies vkd3d-proton's
   `VP_D3D12_FL_12_2_baseline` in full (zero feature/extension misses), and the
   KMD work list is empty for Phase 0 / three small items for the DDI arm.
3. **Stability** — WS1, unchanged and non-negotiable.
4. **Performance** — WS2, PAUSED. Do not reopen without a new hypothesis.

**Also landed with the pivot (2026-08-05), because a stage change is the right
time to stop shipping something nobody measured:**

- **Sane values are now the defaults.** Three knobs whose code default was OFF
  had been ON in the test VM's registry since 2026-08-03, so every accepted
  score was measured on a configuration no fresh install produced. A fresh
  install got the runtime's *emulated* command-list path — GT1 ≈ 184,
  Graphics ≈ 43.5k — instead of the measured GT1 221-227 / Graphics 49-52k.
  Flipped to ON, each with the evidence in the comment at its read site:
  `HELIOS_DXVK_CL_RETAIN_SAMPLER_REFS` (isolated same-boot A/B, GT1
  **53.609 → 181.938**), `UmdCommandLists`, `HELIOS_DXVK_CL_INLINE_REPLAY`.
  `VidMmVramMB` likewise went 0 → 4096, the configuration the VidMm work
  actually validated, re-confirmed on 22.22.251.0 before the flip.
  `HELIOS_DXVK_KMT_SHARED` was forced to "1" by the UMD in every process it
  ever created, so it was not a tunable at all; the engine now defaults it ON
  and the `_putenv_s` is gone. **Verified**: with `HKLM\SOFTWARE\Helios`
  completely empty and no service-key overrides, KMD 22.22.252.0 runs GT1
  **222.857**.
- **Retired**: the `probe/` and `host/` crates (orphans — no workspace, no CI,
  no build, cited only by already-archived docs); the write-only
  `TransportGeneration::page_table_window` the tree itself scheduled for
  deletion at R510; the duplicate unread `AdapterKnobs::dma_gpu_fence`;
  `tools/kmd-force-reject-sweep.ps1` (its knob was retired in T6),
  `tools/attach_idd.ps1` (IddCx-only), and the two completed one-shot DXVK
  source patchers.
- **Gates that could only pass are gone or fixed.** `kmd-gate-surface.ps1` and
  `kmd-counter-snapshot.ps1` were watching four counter/knob names the driver
  no longer writes; `umd-gate-surface.ps1` had three log patterns that could
  never match the emitted text. A gate that cannot fail is worse than no gate.
- **Four silent failure counters were surfaced** as `WdSigF` / `DmaNtfF` /
  `TxGone` / `RclBadH`. Each was incremented on a real refusal path and loaded
  by nobody, which is CLAUDE.md's "every refused path gets a named counter"
  rule being violated invisibly. **All four must read 0 on a healthy session.**
- **Docs archived**: `ARCH.md`, `OVERVIEW.md`, `KMD.md`, `ICD.md`,
  `WINDOWED_BLT_DESIGN.md`, `SCANOUT_DRM_MODIFIER_DESIGN.md` → `docs/archive/`.
  `TRANSPORT.md` deliberately stayed at root: its §1/§2 wire format is still
  ground truth and six `protocol/` comments cite it by section; its banner now
  says which sections are live and which are archived.
- **One real bug fell out of the audit**: `tools/escape_owner_probe.c` defined
  `HELIOS_ESCAPE_QUERY_SCANOUT` as `0x000B`, which is
  `HELIOS_ESCAPE_REGISTER_FENCE_EVENT`. The probe had been aiming a
  query-scanout buffer at the fence-event registrar. Fixed to `0x000D`;
  every other escape constant in that file was checked against
  `protocol/src/escape.rs` and is correct.

## Blender / native Win32 external memory — fixed (2026-08-07)

The Windows Venus ICD now implements `VK_KHR_external_memory_win32` for
`OPAQUE_WIN32`: buffer and optimal-image capability queries, export/import and
re-export by NT handle, and import by Win32 object name. The guest handle owns
or retains the exact WDDM allocation that adopted the renderer's exportable
Venus resource; no Win32 handle is sent over the Venus wire.

Advertising the extension exposed an independent DXVK-Helios bridge bug. DWM
uses the internal KMT shared-resource path, but `canShareImage()` applied that
bypass only while the native Win32 extension was absent. Once present, DXVK
queried Vulkan `OPAQUE_WIN32_KMT` support, correctly got none, and allocated
non-exportable primary memory. The UMD then passed a zero resource id to the
KMD, producing virglrenderer `mem is not exportable` / command `0x1200` errors
and a DWM crash loop. The KMT bridge is now selected independently of native
Win32 extension advertisement, and the UMD refuses every shared/present/primary
allocation that lacks a nonzero adoptable Venus resource id instead of
submitting a poisonous blob request.

Verified on the fresh WinBoat VM with Helios active: clean boot, normal Windows
restart, Helios PnP code 0, DWM primaries and snapshot-ring allocations carrying
nonzero resource ids, no export/`0x1200` host errors, Blender 5.2 surviving GPU
initialization, and the focused native probe passing buffer handle/name plus
optimal-image export/import/bind. The session-0 Blender run cannot prove visible
interactive presentation; that final observation belongs in the desktop/VNC
session.

## Dockur/WinBoat fresh installation — automatic path verified (2026-08-05)

A new Dockur Windows 11 VM was installed from an empty 90 GB qcow2 using the
WinBoat-pinned `ghcr.io/dockur/windows:6.03` image, then booted with bootstrap
VGA plus the 4 GiB-limited Helios/Venus adapter. The clean guest bound Dockur's
signed `viogpudo` as `oem14.inf` on the Helios PCI function. The first
`Install-Helios.ps1 -Automatic` pass enabled test-signing and returned 3010;
after reboot, the second pass removed `viogpudo` without prompting and selected
Helios on the same device instance.

The fresh guest exposed one additional packaging fault: `Import-Certificate`
committed the CI certificate to `Root` but returned E_ACCESSDENIED before
committing `TrustedPublisher`. Native `certutil -addstore` succeeded. The
installer now falls back to that native path when the exact thumbprint is
absent, and verifies through a newly opened `X509Store`; the PowerShell `Cert:`
provider was proven to retain a stale same-process view after the native add.
The repaired automatic pass completed, rebooted, and passed Vulkan, D3D11,
OpenGL 4.6/Zink, and OpenCL smoke tests on the RX 6600.

## RDP desktop lag — root-caused and CLOSED (2026-08-05)

**Symptom (owner):** over RDP, anything that *changes* the desktop is slow —
opening the Start menu, dragging an Explorer window, a closing window whose
frame lingers — while a static desktop, and the dragged window itself, stay
fluid and interactive. A second tester additionally reported frame tearing.

**Helios IS in the RDP path**, which is the fact the whole diagnosis rests on:
the RDP session's `dwm` renders the desktop on Helios, and RDP's indirect
display driver (`RDPIDD`, `SWD\REMOTEDISPLAYENUM\...&SESSIONID_nnnn`, hosted in
`WUDFHost`) is *also* a Helios D3D11 client. Its UMD log shows it creates one
resource of its own — `1920x1080 fmt=87 usage=3 cpu=0x30000`
(BGRA / `USAGE_STAGING` / `CPU_ACCESS_READ|WRITE`) — and `OpenResource`s DWM's
swapchain buffers. So every captured frame is
`CopyResource -> Map(READ) -> memcpy 8.3 MB -> Unmap`.

**Two independent causes. The first was ours; the second was not, and was the
dominant one.** Fixing only the first left the symptom essentially intact —
recorded here because the first fix's numbers look conclusive in isolation and
are not.

### Cause 1 (ours, FIXED) — HOST_CACHED memory was mapped write-combined

`vn_device_memory.c` set `prefer_cached_map` **only** for the WSI blit
destination, so every other host-visible allocation was mapped WC — including
ones DXVK makes on the `HOST_VISIBLE|HOST_COHERENT|HOST_CACHED` type precisely
to get fast CPU reads (`d3d11_texture.cpp:1015`, every `USAGE_STAGING`
resource). The host already reports `CACHED` for that type; `effective_map_cache()`
honours the ICD's request over the host's, so the ICD's own override was the
entire cause. Fixed: honour `HOST_CACHED`.

Same-boot A/B, `tools/d3d11_rdp_capture_probe.cpp` (new — replicates RDPIDD's
loop on RDPIDD's exact resource desc):

| per captured frame | before | after |
|---|---|---|
| `memcpy` out of the mapping | 25.209 ms — **313.8 MB/s** | 0.613 ms — **12906 MB/s** |
| `MOVNTDQA`, same pages | 0.839 ms — 9428.1 MB/s | 0.620 ms — 12756.3 MB/s |
| total | 25.069 ms | 0.942 ms |

**The discriminator is that `memcpy` and `MOVNTDQA` measure the same
afterwards.** Streaming loads only beat `memcpy` on write-combined memory, so
"30x faster with MOVNTDQA" *is* the WC signature, and its disappearance is the
proof. Cache maintenance stays free: the type is COHERENT, guest WB over host WB
is hardware-coherent under KVM, and `helios_bo_needs_cache_ops()` already
exempts exactly this flag combination. Write-only `DYNAMIC` resources are
unaffected — they request `HOST_VISIBLE|HOST_COHERENT`, which matches the
lower-indexed uncached type first, and WC is correct for them.

### Cause 2 (NOT ours, and the dominant one) — RDP's link estimate was fiction

With capture made cheap, the guest sat at **2% total CPU across 16 vCPUs** with
the encoder idle at 1.4%, and the desktop was still slow. During a drag:

    input frames/second 70.03   output frames/second 0.94
    frames skipped/second - insufficient network resources 69.08
    current tcp bandwidth 1536.00 (mean == max)   current tcp rtt 100.00 (mean == max)
    loss rate 0.00

Real RTT to the client is **0 ms** and the link is a **10 Gbps local virtio
bridge** with zero loss. RDP believed 1536 Kbps / 100 ms — its built-in default
profile — and discarded 69 of every 70 frames DWM produced. Cause:

    HKLM\SYSTEM\CurrentControlSet\Control\Terminal Server\WinStations\RDP-Tcp
        SelectNetworkDetect = 1

Per this box's own `C:\Windows\PolicyDefinitions\TerminalServer.admx`, `1` =
**connect-time detect OFF**, so RDP never measures the link. Set to `0` (both
connect-time and steady-state detection on); takes effect on the next
connection. `DWMFRAMEINTERVAL=15` is also set on `WinStations`, so this box has
had an "RDP optimization" pass — that is the likely provenance.

**Result after both, same repro** (`tools/rdp-measure.ps1 -Mode drag`):

| drag repro | original | cause 1 fixed | **both fixed** |
|---|---|---|---|
| input -> output fps | 43.6 -> 3.3 | 63.1 -> 15.8 | **36.1 -> 35.1** |
| frames skipped/s (network) | 39.6 | 45.4 | **0.05** |
| avg encoding time | 8.05 ms | 6.20 ms | **1.25 ms** |
| `WUDFHost` (RDPIDD) CPU | **87.0%** | 5.4% | 3.9% |

Owner-confirmed by eye: "RDP is smooth, perfect."

**Instruments, all reusable** — `tools/d3d11_rdp_capture_probe.cpp` +
`tools/d3d11-rdp-capture-probe.ps1` (per-phase capture cost, with a cached
heap->heap control and the MOVNTDQA memory-type classifier);
`tools/rdp-measure.ps1` -> `tools/rdp-lag-repro.ps1` + `tools/rdp-sample.ps1`
(damage workload in the interactive session + RemoteFX Graphics/Network and
per-process CPU sampling).

⚠ **Two traps this cost a cycle each, both now guarded in the scripts.**
(1) The RDP session id is **not stable** — a reconnect (e.g. after a reboot)
moves the same user to a new session, and a sampler hardcoding the old one
silently reports 0% CPU for a session that no longer exists. `rdp-sample.ps1`
resolves it at run time from `query session`. (2) The workload must be *proved*
to have run in the RDP session, not assumed: `rdp-lag-repro.ps1` writes its own
`SessionId` to its output file. The console/SDL session is a *different*
session with its own `dwm`, and a repro landing there measures the SDL scanout
path instead.

**RDP stale-tile/ghosting CLOSED (second tester, 2026-08-05).** This was not
scanline tearing or a FreeRDP presenter defect: old Explorer tiles remained
until mouse damage refreshed them, and an offline replay reproduced the exact
corruption from the recorded RDP stream. A synchronized capture then showed a
clean session-1 desktop while FreeRDP held stale/partial blocks, placing the
defect in RDPIDD's DWM-buffer capture path. The decisive WUDFHost log line was
`HeliosPresentSync: CreateFile(...helios_present_sync_v2.bin) failed: 5`:
RDPIDD runs as `NT AUTHORITY\LOCAL SERVICE`, but a table first created by DWM
inherited only read access for that principal. The consumer therefore issued
hundreds of imported-buffer reads with no publication table and no producer
fence wait.

Granting LOCAL SERVICE modify access was the same-session discriminator:
WUDFHost mapped HPS2, imported DWM's fence, and the owner reported **zero
glitches**, including through Explorer as a WinBoat-style RemoteApp. Permanent
fix: DXVK creates/repairs HPS2 with read/write access for Authenticated Users,
LOCAL SERVICE, and the Window Manager group; the package installer pre-creates
and repairs the same file so an upgrade cannot retain the old ACL.

## Current verified correction (2026-08-04, KMD 22.22.238.0)

- **Fullscreen presentation is not currently a broken SDL scanout path.** The
  owner corrected the viewer identity after the `.238` visible test: the
  hold/judder that looked like roughly 30–40 fps was observed through **VNC**,
  not SDL. Native QEMU SDL is owner-verified rock solid and smooth, and the
  tearing is gone. Treat the earlier claim that SDL independently reproduced
  the hold/burst defect as retracted. A VNC cadence observation is evidence
  about VNC update/encoding/client delivery only; it must not be used to blame
  KMD scanout, QEMU readback, or the D3D11 render path without a correlated
  boundary trace. Smooth SDL means smooth at the display refresh ceiling, not
  that all 150–220 rendered frames per second can be shown on a 60 Hz output.
- `.238` replaced the coarse fallback VSync timer with a high-resolution
  `ExAllocateTimer(EX_TIMER_HIGH_RESOLUTION)` source. In the targeted Combined
  trace its active VSync samples were stable at about 16.6 ms (p95 about
  17.1 ms, no gaps over 40 ms), and the owner now sees no tearing. This closes
  the fullscreen tearing/cadence symptom for SDL; VNC fluidity remains a
  separate frontend/client concern and is not a blocker for D3D11 throughput
  work.
- **Windowed 3D11 presentation remains open and is a different defect.** In
  the interactive standard Fire Strike flow, a blank titled `3DMark Workload`
  window appears and then disappears while 3DMark continues the workload and
  ultimately reports a score. The scheduled custom `FireStrikeCombinedC`
  window trace (`tmp/cadence-238-window-blt-accept.csv`) rendered successfully,
  but it does **not** validate this interactive path. Instrument the actual
  runtime entry point (ordinary Present, single/multi-surface Present1, or MPO)
  and its exact handles/allocations before changing policy. In particular,
  current `dxgi_present1` many-surface code deliberately passes no snapshot or
  stream correlation; that is a source-backed lead, not yet the proven cause.
- **The remaining Fire Strike performance gap is not a scanout-cadence
  diagnosis.** The current multithreaded command-list path recorded GT1
  221.337, GT2 220.996, Physics 125.986, and Combined 41.952 fps in
  `tmp/perf/fs-std.txt`; a later targeted Combined run reached 43.593 fps.
  Nevertheless, the owner observes only roughly 50–60% host-GPU utilization
  in Fire Strike/DX11, versus a sustained roughly 80–90% in Steel Nomad's
  Vulkan path. Use that differential to find where the D3D11-specific
  runtime/UMD/DXVK command-production pipeline fails to keep the GPU fed.
  Steel Nomad exonerates generic Vulkan throughput, but not D3D11 per-draw,
  command-list, synchronization, or submission economics. Do not spend the
  next performance session tuning scanout unless an epoch-correlated trace
  actually shows scanout back-pressure reaching rendering.

## Earlier direct-primary baseline (2026-07-23, KMD 22.22.142.0)

- `DisplayHalf=1` exposes one connected child and one VidPn source. DWM composes
  the whole desktop on Helios and `SetVidPnSourceAddress` selects the real
  primary for `SET_SCANOUT_BLOB`.
- `ScanoutDiag` is **deleted/off**. Mode 16 remains a diagnostic only and must
  never overwrite the real primary during a desktop test.
- The LINEAR diagnostic image is proven on NVIDIA. The old failure was a guest
  constant bug (`VK_IMAGE_TILING_LINEAR` was encoded as `0`; it is `1`). After
  the fix, same-boot breadcrumbs reached `SdgLStg=0x10`, host-visible/coherent
  memory was selected, and the owner saw its fill pattern in VNC.
- The real DWM primary is a dedicated, DMA_BUF-exportable Venus
  `VK_IMAGE_TILING_OPTIMAL` allocation. The UMD marks the actual
  `CDD_SHAREDPRIMARYSURFACE`; the KMD uses that allocation in
  `SetVidPnSourceAddress`. There is no heuristic selection and no guest-side
  primary-to-scanout copy.
- The QEMU fork propagates virglrenderer DMA_BUF modifier metadata and the
  existing `RESOURCE_CREATE_BLOB.size` internally, without changing the public
  virtio-gpu wire ABI. Plain OPTIMAL exports currently arrive as
  `DRM_FORMAT_MOD_INVALID`; EGL cannot describe that layout. QEMU reconstructs
  the exact producer VkImage, verifies its Vulkan memory requirement equals the
  original blob allocation size, copies image-to-staging on the host GPU, and
  publishes a CPU `DisplaySurface` to VNC. This is direct guest-primary scanout,
  but **not end-to-end zero-copy** because the host display backend reads back.
- Visible desktop output is verified. A DComp scheduled-task probe completed
  1576 Presents in 25 seconds (63.0 fps), and interaction was responsive while
  that continuous producer ran. This isolated the perceived lag to the
  idle-to-active scanout edge, not steady-state GPU throughput. The UMD now
  emits a refresh marker after the exact DWM primary operation. KMD
  `DxgkDdiRender` captures the current Venus wire-fence watermark under the
  statically witnessed notification lock; the used-ring DPC coalesces markers
  and dirties scanout only after all preceding Venus work retires. This does not
  depend on VidSch choosing `SubmitCommand` versus `SubmitCommandVirtual`.
- The v142 wake test advanced the live 16-refresh telemetry snapshot
  (`AsSub`/`AsDone` caught up, `WtOut=CtOut=QfRet=0`). Same-boot QEMU evidence
  then rebound the real 1896x1030 OPTIMAL primary and completed Vulkan readback
  in about 1.0–1.9 ms. The owner confirmed excellent idle-to-active
  responsiveness.
- The KMD watermark orders Venus commands which already exist when the marker
  reaches `DxgkDdiRender`; it cannot cover work still queued on DXVK's
  submission thread. With `PresentGateUs=0`, fast cursor motion exposed that
  producer race as stale cursor replicas. A 5 ms A/B still leaked six stale
  frames in one 128-present burst, so the direct-primary default is now a
  bounded 10 ms `HeliosWaitFrameComplete` before the kernel present callback.
  It sleeps on DXVK's submission-fence condition variable instead of polling.
  The 10 ms A/B measured 0.48 ms cumulative average after 384 presents and
  zero timeouts after its six startup expirations. The owner confirmed both
  excellent responsiveness and no cursor ghosting.
- The old synchronous KMD `RESOURCE_FLUSH` control roundtrip is gone from the
  frame path. One interrupt-completed async bind/flush is allowed in flight and
  later flips coalesce. Control DMA buffers are reaped/reused outside the
  spinlock. Mesa's Windows ring notifies an idle renderer eagerly, folds
  side-effect-free wait-only timeline submits on the guest, and reuses its
  escape staging buffer; per-submit shape logging is opt-in.
- The same exact OPTIMAL Vulkan fallback is shared by `egl-headless`, GTK EGL,
  GTK GLArea, and SDL OpenGL. `egl-headless`+VNC and SDL OpenGL on native
  Wayland are visually verified. The launcher leaves interactive EGL vendor
  selection to the compositor while pinning Venus/readback Vulkan to NVIDIA.
  GTK/Wayland still fails during the full run with repeated GDK
  `eglMakeCurrent` errors and remains unverified.

### VidMm / Task Manager validation (2026-08-04, KMD 22.22.250.0 / 22.22.254.0)

- Task Manager's 4.0 GiB dedicated capacity is now backed by the configured
  `VidMmVramMB=4096` local segment while the CPU-visible aperture remains
  separately capped. A live SDL-window check showed `0.5/4.0 GB` dedicated,
  `0.0/6.0 GB` shared and `0.5/10.0 GB` total.
- Venus `VkDeviceMemory` tracking allocations follow the Vulkan memory heap:
  device-local allocations use the local non-aperture segment without becoming
  BAR-mappable, while non-device-local allocations use the aperture/shared
  segment. Direct KMT and native-Vulkan four-by-64 MiB probes each measured
  exactly `+256.00 MiB` in the selected segment, no movement in the other
  segment, and a return to baseline after destroy.
- Exportable DXVK/Venus memory initially had two full VidMm charges: its local
  `VkDeviceMemory` tracking allocation and the WDDM allocation that adopts the
  same renderer resource in the aperture. The adopted allocation is now an
  identity-only one-page VidMm object only when the current ICD positively
  attests that the full-size tracker exists; missing exports and tracker
  failures retain the safe full-size adopted charge. Its private open identity
  and KMD context retain the exact renderer size. Eight shared 64 MiB D3D11
  render targets consequently measured exactly `+512.00 MiB` local and only
  `+0.03 MiB` aperture (eight pages), then released both. An attempted
  local-segment placement for the adopted WDDM allocation was rejected: its
  first `CreateTexture2D` device-removed the UMD, so that policy never shipped.
- The `.249` hardware gate passed 12/12 direct-KMT cycles, 12/12 native-Vulkan
  cycles and 12/12 shared-D3D11 cycles. A 40-allocation Vulkan high-water test
  charged exactly 2560 MiB locally with no aperture movement, and four
  concurrent eight-allocation processes charged exactly 2048 MiB locally;
  DWM kept the same responsive process throughout.
- The `.250` heap-aware gate passed exact local and non-local direct-KMT tests,
  exact local and non-local native-Vulkan tests, and the eight-allocation D3D11
  adoption test above. A pre-tracking ICD retained one full shared charge; an
  older tracking ICD without the attestation export retained both its exact
  local tracker and one conservative full shared charge, proving the mixed
  deployment cannot under-report. Private export lookup is pinned to one ICD
  module so a missing old export cannot fall through to a newer DLL and receive
  a foreign Vulkan handle. The UMD build now watches every compiled bridge
  source and header, preventing incremental builds from silently reusing stale
  C++ objects. The installed signed package reports `22.22.250.0`, PnP status
  is Code 0, DWM stayed responsive, and no new display/PnP/WHEA/BugCheck
  critical or error events appeared.
- The `.254` follow-up closes the cross-process lifetime boundary. Each tracker
  is now a globally shared WDDM resource, its global KMT handle travels in a
  typed private allocation flag/open identity, and an importer opens the same
  tracker before returning the shared D3D resource. The KMD shrinks the adopted
  payload to one page only when the cookie names a live tracker whose size
  matches the KMD's recorded adopted-blob size; either mixed-version direction
  therefore keeps the conservative full payload charge. If the shared tracker
  disappears during an import race, Mesa creates a full-size tracker in the
  imported memory's actual heap. If that fallback also fails, the bridge
  rejects the D3D shared-resource open.
- The `.254` cross-process gate created and cleared a 4096x4096 shared D3D11
  texture in a child, opened it in the parent, exited the creator, and retained
  exactly `+64.00 MiB` in both adapter-global and importer-process dedicated
  counters. The importer then read the expected `ffff00ff` pixel and returned
  both counters to baseline after its device was destroyed. The both-open and
  creator-exited checks then passed 50 consecutive cycles. Raw KMT shared and
  two-process probes independently retained exactly `+128.00 MiB` and
  `+64.00 MiB`, respectively, after creator handle/process teardown and
  returned to baseline after the final close.
- With the final ICD loaded in DWM, an automated interactive Task Manager smoke
  left both processes responsive and produced no DWM/Task Manager error event.
  During validation, three deliberate PnP restart cycles still reproduced
  defect 0z in the pre-existing `vn_ring_load_head` teardown path (also present
  in the pre-branch ICD); DWM recovered each time. This branch does not claim to
  fix that separate adapter-removal race.
- **Re-gated after the merge, on the version that actually ships (2026-08-05).**
  The bullets above say `.254`; `kmd_render/driver-version.env` says
  **22.22.255.0** (the branch bumped 252 -> 255 directly), so read `.254` as the
  development build and `.255` as the shipped one. The merge also joined this
  branch to the ICD `HOST_CACHED` mapping fix, two changes that had never seen
  each other — the submodule conflict resolved to `e7ad5b238ec`, which strictly
  contains the branch's own `c3262452217`. Re-gated on the merged image:
  `d3d11_xproc_lifetime_probe` **PASS** (both-open and creator-exited each
  retained exactly `+64.00 MiB` adapter and process, pixel `ffff00ff` survived
  the creator's exit, exact return to baseline); `vidmm_tracking_probe` **PASS**
  in all four modes — local, `nonlocal`, `shared` (each exactly `+256.00 MiB`
  for 4x64 MiB) and the new `crossproc` (`+64.00 MiB` retained past creator
  exit). PnP `OK`/`CM_PROB_NONE`, desktop composites (screenshot), no
  display/Dxgkrnl/WHEA/BugCheck critical or error events since boot,
  `WdSigF`/`DmaNtfF`/`TxGone`/`RclBadH` all **0**, and `umd-gate-surface.ps1`
  reports `UMD GATE SURFACE CLEAN` with its must-not-appear set `all clear`.
- The Task Manager-triggered DWM abort was a mixed-source Mesa deployment: the
  installed ICD combined the old `vn_queue.c` with only four files from the
  newer VidMm work. Deploying one coherent Mesa `1a02ba9` image restored the
  imported-Win32-timeline path; Task Manager then stayed open with a stable DWM
  process. The separate, pre-existing PnP-restart DWM fault remains defect 0z.

## Current priorities

1. **DONE (2026-07-28) — the Phase-1 quality refactor of `kmd_render` and
   `umd` is COMPLETE.** Eleven tranches (T0, T1a, T1b, T2, T3, T4a, R614, T4b,
   T5, T6, T7, T8) from `REFACTOR_REVIEW.md`'s 300 findings / 177
   recommendations, every one landed and gated on hardware. Final image:
   **KMD 22.22.190.0 + UMD `DB343F02…`**, T8 gate passed on the 2026-07-28
   15:39:45 cold boot.

   **The tranche-by-tranche record — every gate result, every scope
   correction, every dropped item and its evidence — is
   `docs/archive/REFACTOR_TRANCHES_T0_T8.md`.** The review itself, its two
   kickoff prompts and the T7-crash brief are archived beside it. Code
   comments cite the review by NAME (`REFACTOR_REVIEW.md R802`); those
   citations still resolve, the same convention the other archived design docs
   use.

   Two directives from that work stay in force for all later changes: never
   fold a `BUG` fix into a structure move, and preserve the direct primary,
   completion ordering, loud-failure contracts, registry ABI and diagnostic
   names unless a reviewed change explicitly migrates them.

   **Owed, recorded with the measurements that justify deferring them** (see
   7m/7n in the archived record):
   - **R1103's `VirtioGpu` sub-structs.** `ResourceTables` is genuinely
     field-disjoint; `CtrlQueue`+`FenceTables` needs **six** method hoists on
     the completion path, not the three the review budgeted. Needs its own
     tranche and gate.
   - **R1108's vehicle-TLS sealing** — `take_present_source()` plus the four
     `dxgi_present` call sites that touch the cell.
   - **R1015** — whether the production surface ever takes the
     QUERYSEGMENT3/legacy paths. Needs a `DiagLevel=1` boot.
   - ~~The pre-existing **6-handles-per-device teardown leak** (7d(b))~~ —
     **CLOSED 2026-07-28**, root-caused and fixed. See the WS1 entry below.
   - **WS1 defect 0z** — `pnputil /restart-device` access-violates dwm,
     Explorer, SearchHost and ApplicationFrameHost inside
     `vulkan_virtio-*.dll`. Pre-existing, reproduced on every restart.
   - ~~**WS1 defect 0aa** — fullscreen scan-out pinned to ONE resource~~ —
     **ROOT-CAUSED AND FIXED 2026-07-29** (KMD 22.22.201.0), host-verified.
   - **WS1 defect 0ab — black-frame flashes. SPLIT IN TWO 2026-07-29, one half
     FIXED, one half OPEN.** First measured directly on the displayed surface
     (VNC RFB sampler + QEMU trace, both on the host clock) instead of inferred.
     - **0ab-A — the bind-edge RESOURCE_FLUSH was submission-ordered**, firing
       ~10 ms before the frame it named finished on the host, so the host read
       the frame's clear. **FIXED, KMD 22.22.206.0**: Fire Strike Combined
       (23 fps) unfinished displayed frames **22.0 % → 0.7 %**.
     - **0ab-B — at ~165 fps (GT1 fullscreen) the flashes REMAIN**: ~15 % of
       published frames are entirely black in EVERY configuration we own. Five
       mechanisms built, deployed, falsified; then a same-boot **2×2 factorial**
       (lease × BindFlushMode, 9 runs, 46 681 frames, 2026-07-29 evening) closed
       the whole ordering family WITH data: whole-flush black is 14.5–16.6 % in
       all four cells, and the knobs only move black between populations
       (bind-triggered first reads vs surplus refresh re-reads). **The mechanism
       is now PROVEN, not inferred**: the first read of a binding — the very
       event that ends its lease — finds the buffer already cleared 13–17 % of
       the time under a live lease gate, which no WDDM release chain can permit.
       The app's clear rides venus and never enters a DMA buffer, so the
       scheduler-side allocation sync that real flip-model relies on to defer it
       DOES NOT EXIST in this stack. The one variable that predicts black is
       bind→read age (<3 ms ⇒ 0.4–5.6 %; 6–12 ms ⇒ 34–60 %).
       **FIX SHIPPED — KMD 22.22.217.0 (owner-approved D1+D2+D3, 2026-07-29
       late evening): GT1 whole-flush black 14.5–16.6 % → 2.1 / 0.7 / 2.0 %**
       (age-standardised 2.2/0.9/2.0 — not an age-mix artifact), fps 169–186
       (UP: 25–33 % fewer synchronous host readbacks), Combined 0ab-A gate
       PASS (1.3 %, completion ordering intact), desktop 1:1 binds:flushes,
       Start menu opens, windowed-app coexistence verified, `WvTorn` 0.
       The win is the OWNERSHIP GATE (D2): the 34–49 %-black 2nd-read
       population (1090–1669/run) collapsed to 9–26; the 6–12 ms bucket kept
       its flush share but went 56 % → 0.5 % black — the wrong reads stopped
       being issued, not the timing. See the build-1 subsection below +
       `tmp/handoff-0ab-b-lease/analysis/build1-results.md`.
       **OWNER-CONFIRMED BY EYE 2026-07-29 late night: GT1 visually clean,
       overall Fire Strike >25k (was ~20k). 0ab-B's main population is
       CLOSED.**
     - **0ab-C — residual black-frame stuttering in GRAPHICS TEST 2 at
       ~210 fps. CLASSIFIED 2026-07-29/30: the first-publish bind-edge margin
       race (population (a)), the exact population build 1 left open.** Two
       oracle GT2 runs on .217: whole-flush black 7.3 %/6.0 % (GT1 post-fix
       0.7–2.1 %), all first reads at 1–3 ms bind age; the ownership gate
       holds unchanged (6–12 ms bucket 0.2–0.4 %, rereads ~1 %). Guest half:
       worker bind cadence bimodal (1–3 ms vs 10–14 ms stall modes),
       `BeOvw` ×~30 GT1's rate. Minorities: 0ad's transition window
       (~12–23 %), coalesce-holds (dup 3–5 %). **Fix arc = the D1(ii)
       DISPATCH-bind family, four builds in one night**: .218 bugchecked (a
       PRE-EXISTING `wait_block` TOCTOU the new load armed — root-caused
       from dumps, fixed in .219, three clean batteries since); .219 halved
       GT2 black (4.0/3.4 %); .220/.221 closed the fast-path coverage gap to
       99 % and thereby PROVED the GT2 residual is not bind timing (x = y;
       0/439 black at 0–1 ms bind age — the venus-executed clear lands in
       the READ window). **GT1's residual was eliminated outright
       (1.9 → 0.3 %, best recorded). SHIPPING: 22.22.221.0. GT2 residual
       ~3.5–4 % needs D4 (venus acquire, owner-gated). 0ab-C = reduced, not
       closed; owner's eye pending.** Corpus:
       `tmp/handoff-0ab-c-gt2/analysis/{CLASSIFICATION,FIX-DESIGN-d1ii,BUGCHECK-0xA-218,build219-results,build220-results,build221-results}.md`.

   ⚠ **One standing gate line remains NOT OBTAINABLE on this box** and should
   not be retried as written: **suspend/resume** (`powercfg /a` reports every
   sleep state unsupported by the VM firmware — which also means the
   same-context PnP stop/start carry-over path, `StRst`/`RfUnb`, can never be
   provoked here). The other one — **same-boot QEMU scanout evidence** — is
   RESOLVED: since 2026-07-29 the VM runs `HELIOS_DISPLAY=egl-vnc` and the
   per-flush oracle (`tools/qmp_trace.py` + `tools/scanout_oracle_report.py`)
   provides it routinely; verify with `/proc/<qemu>/cmdline` before relying
   on it.


2. Continue soaking the current direct-primary path across DWM buffer rotation,
   resize, device restart and cold boot. **Suspend/resume is struck from this
   list**: `powercfg /a` on this VM reports S1, S2, S3, hibernate and S0ix all
   unsupported by the firmware, so it is untestable here until the machine type
   changes — and with it, the same-context PnP stop/start carry-over path
   (`StRst`, `RfUnb`) has no way to be provoked on this box at all.
3. Pursue true host zero-copy only with a layout contract the display importer
   can consume. An explicit DRM modifier is one possible route, but enabling the
   modifier/DMA_BUF extensions on every DXVK device is prohibited: it inflated
   ordinary shared OPTIMAL import requirements and caused valid undersized-import
   refusal, DWM failures, and NVIDIA Xid 31 when bypassed.
4. Continue D3D11 stability and conformance work now that the quality pass is done.

## Historical PSC workstreams

The dated IDD/Looking Glass investigations below explain how the display pivot
was reached. They are historical evidence, not descriptions of the active
display architecture, and are superseded by the baseline above wherever they
conflict.

1. **D3D11 windowed apps render transparent — investigate & fix.** Windowed
   D3D11 swapchains (FaceWorks, Fire Strike windowed) show a transparent/black
   client area even when placed on-screen at the right size, while the desktop
   and window frames composite fine. Established this session: the app DOES
   render correct content (`HELIOS_PRESENT_READBACK` source non-black at
   1264×681); it is NOT alpha (`HELIOS_PRESENT_FORCE_OPAQUE` no-op, owner-
   confirmed); it is NOT a two-memory split (the KMD adopt path backs the alloc
   with the DXVK venus image — an earlier "KMD zeroes the resid" reading was a
   UMD struct-layout misread, see below); and it is NOT the IDD (both the D3D11
   fallback and the dead D3D12 path capture the same single composed IddCx
   surface — D3D12 is dead only because our UMD has no D3D12). The live thread:
   **DXGI `EnumOutputs`/`GetDisplayModeList` racily return `0x887a0022`** on the
   Helios adapters, DWM never imports the app's flip backbuffer (its max
   imported resid trails the app's). **ROOT-CAUSED 2026-07-08 (34th) — see
   `WINDOWED_BLT_DESIGN.md` (full design + implementation plan) and memory
   `windowed-blt-occluded-root-34th-session`.** It is specifically the legacy
   **`DXGI_SWAP_EFFECT_DISCARD` (BLT) swap model** returning `DXGI_STATUS_OCCLUDED`;
   **flip composites fine** (proven with `tools/d3d11_triangle.cpp`). Falsified:
   alpha, IDD, two-memory-split, phantom-LUID/EnumOutputs, adapter selection, AND
   the cross-adapter cap (present is same-adapter, not cross). Real cause: a legacy
   DISCARD windowed present needs a **real active VidPn output** in the path; ours
   has none — the only monitor is the indirect IddCx one, enumerated on Helios's
   **runtime-synthesized *facade* output** (`NumOfSources=0`), and there is no real
   display adapter to cross-adapter to. FIX = give Helios a **real (virtual) VidPn
   source** (RDP/display-miniport model) so DISCARD resolves a real output; do the
   **Stage-0 WARP A/B first** (WINDOWED_BLT_DESIGN.md §6). The "two Helios adapters"
   are the Helios render adapter + the Looking Glass IddCx adapter (which inherits
   the render adapter's name) — NOT stale residue.
   **STAGE 0 DONE + STAGE 1 IMPLEMENTED (2026-07-08, 35th):** Stage 0 validated
   Option A on-screen (disable Helios → WARP presentable → BLT composites). §6.3
   resolved from MS docs — a VidPn source+target+monitor must be same-adapter, so
   Helios gets its OWN 2nd (virtual, no-scanout) monitor (owner-approved; IDD
   renders unchanged, monitor unobserved). **Built KMD v22.22.63.0** with the full
   display half behind a `DisplayHalf` REG_DWORD knob (default 0 = today's
   render-only surface): `start_device` sources/children=1 + child DDIs +
   GetChildContainerId; new `ddi/vidpn.rs` (viogpudo-style
   EnumVidPnCofuncModality/RecommendMonitorModes, single 1920x1080@60); VidPn DDIs
   in `display.rs` (IsSupportedVidPn=TRUE, RecommendFunctionalVidPn=NO_RECOMMENDED,
   Commit/SetAddr/SetVisibility=SUCCESS no-op scanout). Compiles + signed; NOT yet
   installed. **NEXT: owner install (reboot) → `DisplayHalf`=1 + `pnputil
   /restart-device` → BLT triangle un-occlusion test** (WINDOWED_BLT_DESIGN.md §9,
   memory `windowed-blt-display-half-implemented-35th`). Honest caveat: docs don't
   tie OCCLUDED to a VidPn source — the knob A/B is the arbiter.

2. **Slow first-paint on some windows — our UMD makes DWM wait.** Settings app,
   parts of Explorer on fresh open, and (easiest repro) the **UAC dimmed
   window** take several seconds to render. Suspected a UMD-side present/consumer
   wait or a per-window gate stalling DWM's first composition of these surfaces.
   Likely related to #1's consumer/import path. NEXT: measure — instrument the
   present-wait / gate-flush / consumer-wait counters against a UAC-window repro,
   find which wait blocks and why it only bites first-paint.

3. **Codebase cleanup (HIGH).** Many paths accreted across bring-up sessions add
   overhead or cause minor misbehaviours: retired diagnostic scaffolding, dead
   knobs, superseded present/staging paths, force-* diagnostics, staged-probe
   machinery, and now-falsified experiments (e.g. the `DECLARE_CROSS_ADAPTER_RESOURCE`
   line, broad-adopted-BAR remnants). Audit the UMD present path, dxvk-helios
   staging/refresh layers, and KMD segment/adopt code; delete or gate what is not
   load-bearing, with before/after behaviour verified. Do this before large new
   feature work so #1/#2/#4 land on a clean base.

4. **Performance — Fire Strike fullscreen (~100 fps @1080p, GT1).** Owner
   believes near-2× is reachable. Render path is healthy (fullscreen renders
   correctly). Measure first (venus submit/fence latency, copy/acquire gates,
   present-to-scanout) then remove known costs. See WS2 for the levers already
   mapped (feedback-shadow retire, dcomp vehicle, copy-latency).

## Fullscreen scan-out — 0aa FIXED, 0ab STILL OPEN (2026-07-29, KMD 22.22.201.0)

⚠ **0aa is fixed and host-verified; the owner still sees black-frame FLASHES
(defect 0ab).** Do not read the two fixes below as closing the visible
artifact. What they closed is measurable and closed; what remains has a
different shape (brief, frequent flashes vs a lasting stale frame) and is
recorded at the end of this section.


**Symptom.** With a fullscreen D3D11 app, the guest published ONE scan-out
resource to the host for the whole run, at the app's frame rate, with ZERO
`SET_SCANOUT_BLOB`; the desktop before and after rotated a three-buffer chain
with a blob per flip. Owner saw black frames, "some for longer". Five sessions
of inference produced four wrong mechanisms; one run of an UNSAMPLED instrument
named the cause.

**The instrument comes first.** `kmd_render/src/ddi/scanout_trace.rs` separates
accumulation (unsampled, atomics-only, legal at any IRQL, every call recorded)
from publication (one throttled PASSIVE dump). Read it with
`tmp/perf/scanout-trace.ps1`; run a workload with it via
`tmp/perf/run-gt1-trace.ps1`. Host side: `virtio_gpu_cmd_*` over QMP
(`/tmp/helios-tpm/mon.sock`) — see the recipe in the 57th-session memory.
Every `Sc*`/`Rf*`/`PB*` value is SAMPLED (1st + every 600th) and registry values
persist across boots; that is what produced the four wrong mechanisms.

**Root cause 1 — the bind was never asked to move.** `DXGK_FLIPCAPS` advertised
`FlipOnVSyncMmIo` only, which covers nonzero-interval flips. Measured: DWM
presents at `FlipInterval=1` with `pDmaBuffer == NULL` (the MMIO contract,
which dxgkrnl completes through `SetVidPnSourceAddress`); a fullscreen app
presents at `FlipInterval=0` (IMMEDIATE) with a DMA buffer — the DMA-buffer
flip contract, where the driver must program the display itself, and which
`SetVidPnSourceAddress` never follows. `DXGKARG_PRESENT.Flags` is IDENTICAL
(`Flip|FlipWithNoWait`) in both cases, so the flags are not the discriminator —
the flip interval is. 839 flips in one run, 0 MMIO, 0 blobs,
`SetVidPnSourceAddress` silent for 36 s. **Fix: also advertise
`FlipImmediateMmIo`** — honest, because a Helios flip IS a `SET_SCANOUT_BLOB`
and has no vblank to wait for. `FlipCapsX` (service key, default 0) overrides
the whole word; `FlipCapsX=2` restores the old advertisement for an A/B.

**Root cause 2 — a bind is itself a dirty edge.** `SET_SCANOUT_BLOB` changes
which resource the host reads; it does not make the host read it. The zero-copy
arm deliberately produced no dirty edge ("the matching Render marker and
used-ring retirement are the sole producers"), which was true only while the
bind never moved. Once it rotated per flip, a freshly bound buffer sat on screen
unread. QMP-measured bind→same-resource-flush latency over a full run:

| KMD | binds | p50 | p90 | max | ≥20 ms |
|---|---|---|---|---|---|
| 22.22.200.0 | 793 | 0.5 ms | 100.2 ms | 2634 ms | 265 (**33.4 %**) |
| 22.22.201.0 | 829 | 0.3 ms | 0.6 ms | 81.8 ms | 2 (0.2 %) |

**Fix: request a refresh for the exact resource after a bind that changed the
binding.** Fire Strike Combined 23.1 → 25.5.

**FALSIFIED here, with full-coverage measurements — do not re-propose.**
- *The bind races the app's rendering.* The per-buffer watermark census
  (`Bw`) is `CONTENT_TRACKED` on 425/425 binds and `CONTENT_PENDING` on ZERO.
  dxgkrnl issues the flip on the app's DMA fence, which `DmaGpuFence=1` already
  retires on host GPU completion, so the buffer is always complete at bind time.
  The `BindWait` gate that came out of this hypothesis is LANDED but inert
  (`ScNotRdy` ~1/boot, `ScBForce`=0); keep or delete deliberately.
- *A stuck programming gate / dead VSync heartbeat.* `VpGate`=0 and `VpVsN`
  advancing at ~60/s throughout the stall.
- *`SetVidPnSourceAddress` binds are coalesced to ~1.5/s* (an in-tree code
  comment). Measured at ~64/s with `VpCoal`=0 on the desktop.

### NOT a defect — the one-shot `scanout_dmabuf` import failure

`/tmp/helios-qemu-stderr.log` shows `sdl2_gl_scanout_dmabuf: failed` with
`OPTIMAL DMA-BUF shape mismatch required=8773632 fd_size=7913472`, and 229 of
229 attempts fail. It looks alarming and IS NOT A DEFECT: owner-verified
2026-07-29, it fires ONCE PER RUN at startup and every subsequent
`SET_SCANOUT_BLOB` is fine. It is a one-shot capability probe that falls back
to the working blob path. Recorded here so the 100 %-failure ratio is not
"discovered" again — the ratio is over ATTEMPTS, and there are only one or two
attempts per boot.

### Defect 0ab — 0ab-A FIXED 2026-07-29, 0ab-B STILL OPEN

**Shipping state: KMD 22.22.209.0** = 0ab-A only. 22.22.207.0 and .208.0 were
the two falsified 0ab-B attempts below and are fully reverted; .209.0 is .206.0
plus nothing.

**OWNER-CONFIRMED on .209.0, full Fire Strike suite (2026-07-29)** — and the
result is the sharpest constraint we have on what is left:

| test | presents | owner verdict |
|---|---|---|
| Graphics Test 1 (~165 fps) | high | **black-frame stutter** |
| Graphics Test 2 (high fps) | high | **black-frame stutter** |
| Physics | none (CPU) | clean |
| Combined (~23 fps) | low | **clean** |

Score 20k overall / 40k graphics / 4k combined — **unchanged**, so 0ab-A cost
nothing. This confirms both halves of the instrument's reading: 0ab-A is fixed
(Combined went 22.0 % → 0.7 % unfinished frames and the owner now sees it
clean), and **0ab-B scales with FRAME RATE, not with workload**. Physics is the
control: no presents, no artifact.

**The instrument came first, and it is the reusable part.** Five sessions
argued about 0ab from guest counters. What settled it was watching the thing
itself: `tools/vnc_frame_probe.py` samples QEMU's VNC surface at ~30/s and
stamps each frame with `time.time()`, which is the SAME CLOCK as the
`virtio_gpu_cmd_*` trace lines the QEMU `log` backend writes; and it scores each
frame with a **completeness oracle** — the mean brightness of 3DMark's fps bar,
which is present in every finished frame and absent in every unfinished one.
Whole-frame brightness cannot tell "dark scene" from "unfinished frame"; the
oracle can. `tools/vnc_scanout_correlate.py` joins the two.

⚠ Two traps in the probe, each of which cost a cycle: sending RFB
`SetPixelFormat` makes QEMU stop answering FramebufferUpdateRequests entirely
(its native format is already the one you want), and writing PNGs inside the
sample loop throttles it to 3/s — which is coarser than the artifact and
silently biases the sample.

⚠ `screendump` is NOT an alternative, under `sdl,gl=on` OR under `egl-vnc`: the
console's `scanout.kind` is DMABUF, so `qemu_console_surface()` returns NULL and
QMP answers `"no surface"` even though the VNC path is happily reading a live
surface. That is why the RFB client exists.

**0ab-A root cause: the bind-edge RESOURCE_FLUSH was submission-ordered, so the
host read the frame's CLEAR.**

`program_vidpn_source_inner` fired `request_scanout_refresh_for(target)`
immediately after every SET_SCANOUT_BLOB that changed the binding — the 0aa
"a bind is itself a dirty edge" fix. Under the DMA-buffer flip contract that is
~10 ms too early: `arm_dma_flip` runs in `DxgkDdiSubmitCommand` **with the
flip's DMA fence still outstanding** (deliberately — that is the contract), and
the app's real work never travels in that DMA buffer at all; it goes to the host
over the Venus escape channel. So at bind time the frame is SUBMITTED, not
COMPLETE. A RESOURCE_FLUSH is the host's instruction to READ, and what it read
was the frame's own clear — hence a fully black frame, never a partial one.

The `425 of 425 binds found the buffer's watermark already retired` claim the
arm shipped with was measured on the MMIO contract, where dxgkrnl retired the
flip BEFORE calling `SetVidPnSourceAddress`. It did not survive the move to the
DMA contract, and nothing re-measured it.

**Measured on what is actually DISPLAYED**, Fire Strike Combined, one QEMU
`virtio_gpu_cmd_*` trace over QMP correlated with an RFB sampler on the VNC
surface (`tools/vnc_frame_probe.py` + `tools/vnc_scanout_correlate.py`), using
3DMark's fps bar as a "did this frame finish?" oracle:

| | 22.22.205.0 | 22.22.206.0 |
|---|---|---|
| displayed frames that are UNFINISHED | **98/445 = 22.0 %** | **3/431 = 0.7 %** |
| …and entirely black | 98 of 98 | 3 of 3 |
| binds whose flush was within 2 ms of the bind | 465/475 = 98 % | 60/521 = 12 % |
| bind → first flush of that resource, p50 | **0.2 ms** | **10.4 ms** |
| Combined score | 23.06 | 23.14 |

10.4 ms is the app's own GPU frame time — the flush now lands when the frame
lands. Guest-side, the same thing reads as `BeDef` 3096 vs `BeRdy` 1478 over
4576 binds: **two thirds of binds under a fullscreen DMA-flip workload had
outstanding Venus work at bind time**, i.e. two thirds of them used to publish a
half-drawn buffer. GT1 180.19 vs 183.06 — inside the run-to-run band.

**The fix is an ordering, not a stall.** The bind edge now arms through the
Venus watermark (`AdapterContext::arm_completion_ordered_refresh` →
`VirtioGpu::note_scanout_refresh`), so the flush is issued from the completion
DPC by `take_ready_scanout_refresh`. No CPU thread waits anywhere, which is
exactly why the deleted producer-side gate was the wrong shape for this. It also
keeps everything the bind edge exists for: a bind that changed the binding is
still guaranteed a flush naming its own resource (defect 0aa stays fixed).

**Residual inside 0ab-A, named rather than hand-waved.** ~12 % of binds still
find the watermark already retired and flush immediately (`BeRdy`), and one of
those produced the single remaining black frame in a 21 s Combined window.
`note_scanout_refresh` samples `next_wire_fence` at the call, so "everything
below it has retired" is trivially true if the frame's Venus commands have not
reached the virtio ring yet. Closing that needs the watermark captured at flip
SUBMISSION and carried in `PresentFlipPrivate`, not sampled at bind time.

### 0ab-B — at ~180 fps the flashes REMAIN (OPEN)

**Do not read 0ab-A's numbers as closing the visible artifact.** The same probe
on Fire Strike **GT1 fullscreen (~180 fps)**, KMD 22.22.206.0:

| | GT1 after 0ab-A |
|---|---|
| displayed frames entirely black | **157/856 = 18.3 %** (sampler 29.5/s) |
| binds whose flush was within 2 ms | 1595/4409 = 36 % |
| bind → first flush, p50 / p90 | 0.9 ms / 10.3 ms |
| frame sampled AFTER the completion-ordered flush | 41/249 = **16 %** unfinished |

The bind-edge fix DID take effect here (98 % → 36 % immediate flushes), and the
black frames did not go away — so the early read is not what produces them at
this frame rate. The 16 % figure is the load-bearing one: the buffer is
unfinished *after* a flush that is correctly ordered on its content.

Per-run counter deltas over that GT1 run (deltas, not absolutes — registry
values persist across boots):

    VpBind +5015   VpSkip +1046 (21 % already-bound)   VpCoal +310
    MkTot  +5645   MkBound +824 = 14.6 % of presents
    BeRdy  +1875   BeDef  +3139

`MkBound` at **14.6 %** is write-while-displayed: the app finished writing the
buffer that was, at that instant, the bound scan-out. That is the previous
session's mechanism, which the DMA-flip contract drove to 0.45 % on Combined and
which is plainly back at 180 fps, alongside `VpCoal`/`VpSkip` at ~20 % of binds.
The app rotates only TWO scan-out resources in fullscreen (host trace: strict
A,B,A,B alternation, zero consecutive same-resource binds), so a bind that lags
one flip leaves the display pointed at the buffer the app has just been handed
back and cleared.

#### 0ab-B: two mechanisms BUILT, DEPLOYED, MEASURED, and FALSIFIED

Both were implemented in full, installed, and measured against the same probe.
Neither moved the artifact. **Both are reverted; do not re-propose either
without new evidence.** The code is gone, this record is the point.

| attempt | what it did | result |
|---|---|---|
| **Flip-fence ordering** (was 22.22.207.0) | Held a DMA-buffer flip's `DXGK_INTERRUPT_DMA_COMPLETED` until that flip's own SET_SCANOUT_BLOB had completed, via a monotonic ticket minted in `arm_dma_flip_programming` and released by the display worker. Premise: completing the flip fence on Venus retirement alone lets dxgkrnl recycle a still-displayed buffer. | Gate demonstrably live — **4844 of 5028 flips held** — and it did what it claimed: `VpSkip` 1046 → 274, no fps cost (GT1 162.0 vs 164.7). **Black frames unchanged: 21.2 % → 21.6 %.** |
| **Dirty-edge identity gate** (was 22.22.208.0) | Dropped a refresh whose armed resource was not the bound one, *provided the bound one had already been flushed once* (a binding epoch, so defect 0aa stays fixed). Premise: a marker armed for frame B was re-reading buffer A. | Desktop stayed healthy (61 binds / 61 flushes over 3 s of mouse movement, so the epoch qualifier did fix the 2026-07-28 delivery collapse). But the gate fired only ~128 times per run. **Black frames unchanged: 18.0 %.** |

**What the falsification actually taught us**, and it is the useful part:

* A fence census over one GT1 run (`virtio_gpu_fence_ctrl` type 0x207 →
  `fence_resp`, joined to the flushes) found the app had **NO GPU work in
  flight for 94 % of flushes**, and the dark rate was **identical (19 %)**
  whether 0, 1 or ≥2 submissions were outstanding. So the host is not reading a
  buffer mid-render: the buffer is quiescent and genuinely contains a clear.
* The dark rate is **FLAT against every timing variable** — age of the current
  binding (21/16/18/14/16/29 % across <3…≥20 ms), time to the next bind
  (18/20/16/16/17 %), time since the last flush (22/21/18/19/11/11 %). A race
  against our bind/flush cadence would show a gradient. There is none.
* The flashes are **163 isolated single-frame events in 31 s** (~5/s) plus the
  two workload-transition fades; run-length is 1 for all of them.
* Fullscreen rotates exactly **two** scan-out resources in strict A,B,A,B order
  (**zero** consecutive same-resource binds in 4418), so this is not the
  desktop primary being interleaved and not a lost rotation at our level.

**Where that leaves 0ab-B.** The remaining shape is "the guest's own presented
buffer contains a clear", which points UPSTREAM of the scan-out path — at the
UMD/DXVK swapchain (`dxgi_rotate_resource_identities`/`rotate_ring`, and DXVK's
reuse of a presented image) rather than at the KMD. Note the standing constraint
that the app's CONTENT is correct was established with 3DMark's offline frame
output, which does **not** exercise the swapchain rotation — so it does not
cover this. ⚠ Also note the sampler tops out at ~30/s while GT1 presents at
~165/s, so it cannot resolve an individual presented frame there; the next
instrument needs either a slower workload or a guest-side timestamped trace.

**Also still true and still not the cause** (kept because each cost a session):
- The host runs `-display sdl,gl=on` or `egl-vnc`; the artifact reproduced on
  BOTH, which is what ruled the backend out. Under `egl-vnc` QMP `screendump`
  still answers `"no surface"` (the console is in DMABUF scanout kind), so the
  RFB path is the only way to see the surface — hence the probe.
- `VpCoal` and the burst bind pacing are real and unexplained, and are NOT this.

**RESULT — producer-side ordering is NOT the cause, and the old ESTABLISHED #1
is now wrong.** Owner ran `PresentOrder=0` + `PresentGateUs=200000` (a
completion gate that cannot expire) on top of the 0aa fixes: **black frames
still flash, only less frequently.** The handoff recorded that config as
"visually clean"; it is not. A producer-side stall REDUCES the artifact — which
is why it read as clean at lower frequency — but does not remove it, so
whatever is producing 0ab survives the app's own GPU work being finished before
the present is published.

⛔ **The `PresentGateUs` and `PresentOrder` knobs were DELETED by owner
directive (2026-07-29) and must not be reintroduced.** They were the
producer-side CPU present gate. It is a hack in both directions: on expiry it
publishes the present with work still outstanding (the exact thing it exists to
prevent), and when it does hold it removes all CPU/GPU overlap (Fire Strike GT1
158 → 136). The measurement above is the last word on it — it does not even fix
what it was being kept for. The ordinary present path is now unconditionally
SUBMITTED-ordered with no knob; the vehicle path keeps its own
`VehicleFlipGateUs` COMPLETE wait, which answers a different question (ICD
image RECYCLE, not present ordering). See the ⛔ note in `umd/src/knobs.rs`.

**MECHANISM MEASURED 2026-07-29 (KMD 22.22.203.0).** `MkBound` counts present
markers whose resource was, at that instant, the BOUND scan-out — i.e. the app
finished writing the buffer the host was displaying. Over one Combined run:
**145 of 1245 markers (11.6 %)**, and the per-window deltas correlate EXACTLY
with dropped binds:

| window | ΔMkBound | ΔVpCoal |
|---|---|---|
| desktop 0–10.9 s | 0 | 0 |
| 11.8–16.6 s (app start) | 19, 22, 36, 18 | 2, 6, 27, 9 |
| 18.7–22.6 s | 0 | 0 |
| 26.7–29.1 s | 11, 17, 15, 5 | 10, 11, 12, 3 |
| 31.8–46.7 s | 0 | 0 |

Run totals `coalesced=80`, `alreadyBound=82` — tracking 1:1. Reading: a dropped
pending bind makes the NEXT bind find the same buffer already bound, so nothing
is issued and the display stays on a buffer the app then overwrites.

**Why binds are dropped at all is the load-bearing question.** With
`MaxQueuedFlipOnVSync = 1`, dxgkrnl should not issue a second
`SetVidPnSourceAddress` until the first flip has retired — which requires our
CRTC_VSYNC to carry its address, which requires us to have bound it. Two
pendings therefore should never coexist, yet `VpCoal` is 80. The implication is
that **dxgkrnl retires an IMMEDIATE flip on the DDI's RETURN, not on
CRTC_VSYNC** — and our DIRQL half returns STATUS_SUCCESS having only stashed the
handle for a PASSIVE worker. That makes the success return a lie for exactly the
flip class `FlipImmediateMmIo` opted us into: dxgkrnl frees the previous buffer
to the app immediately, then issues the next flip, while we have programmed
nothing.

**CONFIRMED, AND THE DMA-BUFFER FLIP CONTRACT IS IMPLEMENTED (KMD
22.22.205.0).** `FlipImmediateMmIo` is withdrawn; `ddi/present_packet.rs`'s
`PresentFlipPrivate` carries the flip's allocation + `DXGK_ALLOCATIONLIST`
physical address in the kernel-only DMA private data, and `submit_command` arms
the scan-out programming from there while the flip's DMA fence is still
outstanding. Two supporting pieces were needed:

* `create_allocation::SCANOUT_ALLOCS` — Present holds only
  `hDeviceSpecificAllocation`, the scan-out path keys on the GLOBAL
  `AllocationContext*`, and NOTHING bridges them (`DXGK_OPENALLOCATIONINFO`
  carries a `D3DKMT_HANDLE`, and the create-time private data is UMD-visible so
  a kernel pointer must not travel through it). A 32-slot registry keyed by
  venus resource id, populated for direct-scan-out allocations only, is the
  bridge. Measured before it existed: `VpDmaF=165, VpDmaA=0, VpPrF=165`.
* `PresentFlipPrivate::take` CONSUMES the record, because dxgkrnl recycles DMA
  private-data buffers and a left-behind record would re-arm a bind for a stale
  allocation.

Result on the same Combined workload:

| | binds | coalesced | alreadyBound | write-while-displayed |
|---|---|---|---|---|
| MMIO immediate (22.22.203.0) | 1034 | 80 | 82 | **145 / 1245 = 11.6 %** |
| DMA flip (22.22.205.0) | 1272 | **2** | **6** | **6 / 1327 = 0.45 %** |

`VpDmaF=946, VpDmaA=946` — every immediate flip programs the scan-out.
`ScAlcFul=0` (registry never overflowed), `VpPrF=0`, `ScSetErr=0`, `RfFail=0`.
Combined 23.06, inside the run-to-run band. **Owner visual confirmation is still
outstanding — the guest census is not the artifact.**

**The reasoning that got here, kept because it is the reusable part** (an ordered pending QUEUE fixes lost flips but not a lag whose
bound is "whenever the worker runs"; implementing the DMA-buffer flip contract
puts completion back under driver control via the DMA fence, which is what that
contract exists for). The confirming measurement is a DIRQL-side record of
whether `last_primary_address` had already advanced to the previous pending
handle's address when the next `SetVidPnSourceAddress` arrives.

**FALSIFIED WITH EVIDENCE 2026-07-29 — do not re-propose:**
- *The app's rendered CONTENT is black.* Owner ran 3DMark's frame-output (image
  quality) dump: no black frames in the output, and none visible while it ran.
  Note that frame output renders scenes offline and does NOT exercise the
  display/scan-out path, so it is a content oracle only — which is exactly what
  makes it decisive here. **The defect is strictly in what we DISPLAY.**
- *`dxgi_present1`'s multi arm fails to copy src->dst.* CORRECT as written:
  `DXGI_DDI_ARG_PRESENT1` documents that when many resources are presented
  `hDstResource` is NULL and the driver must translate only the LAST source
  handle for `pfnPresentCb`. There is no destination to populate.
- *`BltDXGI` leaves the DWM shared surface / full-screen PROXY surface empty.*
  The `dxgi-presentation-path` doc makes this the obvious suspect — both the
  windowed shared surface and the full-screen proxy are filled by `BltDXGI`,
  and our `dxgi_blt1` refuses CONVERT/STRETCH outright while the caps advertise
  16x stretch. It is nevertheless NOT the cause: **`DXGI Blt` appears 0 times in
  every UMD log.** Modern flip-model swapchains bypass the Blt path entirely.
  (The unbacked stretch/convert caps remain a separate honesty problem.)
- *A mid-run scan-out DISABLE blanks the screen.* `set_scanout_blob res 0x0`
  appears ONCE in the entire host log.
- *DWM keeps binding the scan-out during a fullscreen run, alternating with the
  app.* DWM's ids freeze the moment the workload starts (`Vs` 36/39/40 stop at
  86/85/85) and its flushes stop with them.

**Older suspects, now subordinate to the above:**
1. **Bind lag / burst pacing.** `VpCoal` ~85 per run, and binds arriving two
   3 ms apart then a 40 ms gap against a 25 fps app. A late bind can point the
   display at a buffer dxgkrnl has already recycled to the app — and with
   `gl=on` the app's clear-to-black is then on screen live.
2. **The programming gate's VSync suppression** as the cause of that burst
   pacing: `vsync_dpc_routine` early-returns while `vidpn_programming` is
   raised, so dxgkrnl cannot retire, flips queue, and the queue drains in a
   burst when the gate drops.
3. The consumer half of `publish_present_order`
   (`Global\HeliosPresentFence_<pid>_<id>`) still has no consumer — a GPU-side
   wait is the non-hack form of the ordering the deleted gate was faking.

#### The QEMU-side scan-out oracle (built 2026-07-29, needs a VM relaunch)

Every 0ab-B conclusion so far is statistical, because `tools/vnc_frame_probe.py`
samples at ~30/s while GT1 flushes at ~142/s. The owner authorised working in
the QEMU scan-out path, so the oracle now lives where the pixels are — inside
the flush itself, one line per displayed frame, no sampling:

| trace event | site | what it settles |
|---|---|---|
| `helios_scanout_blob_layout` | `virgl_cmd_set_scanout_blob` | the guest's own view: fd, blob size, `offsets[0]`, computed `fb.offset`, stride. ⚠ `virtio_gpu_create_dmabuf` builds every `QemuDmaBuf` at **offset 0** and drops `fb.offset`; a nonzero guest offset would mean the host reads the wrong bytes, so it is now visible rather than assumed away. |
| `helios_scanout_bind` | `egl_scanout_dmabuf` | per bind: resource id → DMA-BUF inode/size/backing/stride, which readback path took it (`vk-optimal` / `vk-linear` / `cpu-mmap` / `egl-texture`), and whether the readback cache reused an entry. Two resource ids sharing one inode = aliased buffers. |
| `helios_scanout_read` | `egl_scanout_flush` | per FLUSH: `bound_ino` (what the guest has bound, fstat'd now) vs `read_ino` (what the active readback actually imported), the flush rect, and a content verdict over the surface the VNC encoder is about to read — sampled every 4th pixel: `sampled`/`nonzero`/`max` plus an FNV-1a `csum` that separates new content from a re-read. |

`nonzero == 0` **is** the black-frame flash, decided on the exact pixels that go
out. Enable with `tools/qmp_trace.py on helios_scanout_read helios_scanout_bind
helios_scanout_blob_layout`; the lines land in `/tmp/helios-qemu-stderr.log`
with the same ISO8601 UTC prefix as the `virtio_gpu_cmd_*` traces. Report:
`tools/scanout_oracle_report.py /tmp/helios-qemu-stderr.log <label>`.

Zero cost when the events are off (both emitters return on
`trace_event_get_state_backends`), and the only behavioural change in QEMU is
that a `QemuDmaBuf` now carries its producer's id (`qemu_dmabuf_set_source_id`).

#### 0ab-B ANSWERED (2026-07-29, KMD 22.22.209.0, GT1 183.6 fps, 54 s, 2965 flushes)

One GT1 run through the oracle. **The host reads the right buffer; the guest's
buffer contains a clear.** Hypothesis (B) is dead, and it died on identity, not
on argument:

| oracle question | answer |
|---|---|
| are the two rotating resources distinct memory? | **yes** — res 191 ino 27926 fd 423, res 195 ino 27930 fd 303, both 4587520 B, `guest_offset` 0, `fb_offset` 0. No aliasing. |
| did any flush read a buffer other than the bound one? | **0 of 2965.** `read_ino == bound_ino` every time. |
| what fraction of PUBLISHED frames are entirely black? | **619 / 2965 = 20.9 %** (res 191 20.4 %, res 195 22.0 %) — and that independently reproduces the 30/s VNC sampler's 18–21 %, on a per-flush instrument. |

**And the artifact has a sharp timing structure the sampler could not see.** The
previous session's "no timing gradient — the dark rate is flat against binding
age" was an artifact of sampling at 30/s a thing that happens at 90 flushes/s.
Per flush, the bind→flush latency is **bimodal**, and the black frames live
entirely in the late mode:

| bind → flush | flushes | black | |
|---|---|---|---|
| 1.0–3.0 ms | 1261 | 13 | **1.0 %** |
| 3.0–6.0 ms | 26 | 6 | 23.1 % |
| 9.0–12.0 ms | 1317 | 541 | **41.1 %** |
| ≥12 ms | 301 | 58 | 19.3 % |

p50 bind→flush: **LIT 1.8 ms, BLACK 11.1 ms**. And 611 of the 619 black flushes
land **within 0.5 ms of the NEXT bind** (every other time-to-next-bind bucket is
0–5 % black). So a black frame is a flush that arrives ~2 app frames after its
own bind, at the instant the next flip is being programmed.

**Root cause, confirmed in the source.** `VirtioGpu::note_scanout_refresh` arms
the marker on `let watermark = self.next_wire_fence` — *everything submitted so
far*, sampled at the call. The bind-edge arm
(`program_vidpn_source_inner` → `arm_completion_ordered_refresh`) runs in the
PASSIVE display worker, long after the flip was submitted, and at 183 fps the
app has already pushed frame N+1 (and more) into the ring by then. So the flush
for frame N waits for frame **N+1** to complete — one whole frame too long — and
with only two rotating buffers the app has had buffer A handed back and cleared
it for N+2 by the time QEMU reads it. That is the entirely-black frame: not a
half-drawn one, a *re-cleared* one. It also explains why the artifact scales
with frame rate (at Combined's 23 fps the extra frame still fits inside the
buffer's time on screen) and why both falsified attempts missed — neither
changed *when the flush is issued*.

This is precisely the residual 0ab-A shipped with and named: *"`note_scanout_refresh`
samples `next_wire_fence` at the call… closing that needs the watermark captured
at flip SUBMISSION and carried in `PresentFlipPrivate`, not sampled at bind
time."* The oracle turned that from a footnote into the measured cause.

⚠ Also note `scanout_refresh_watermark` is a SINGLE slot: a second arm before
the first fires overwrites it. Binds 3867 vs flushes 2965 in this run — ~900
arms were dropped that way.

#### FALSIFIED — capturing the boundary at flip SUBMISSION (22.22.210.0)

Built, deployed, measured, reverted. `arm_dma_flip` captured
`next_wire_fence` in `DxgkDdiSubmitCommand` and carried it to the bind edge in
`AdapterContext::pending_flip_watermark`. **The gate was live** — `BeCar` 6504
vs `BeSmp` 1694, so 79 % of binds used the carried boundary — and **black
frames got WORSE: 20.9 % → 27.1 %** (GT1 162.3 fps, 52 s, 3689 flushes). The
bimodal split survived unchanged: 0.2 % black at 1–3 ms, **63.1 %** at 6–12 ms.

The raw event stream says why, and it is worth reading as the actual shape of
the defect — a 13 ms cycle, identical on .209.0 and .210.0:

    +1.76 ms  res 332  BIND
    +12.66 ms res 332  read BLACK     <- flushed 10.9 ms after its bind
    +12.72 ms res 335  BIND           <- the NEXT flip, 60 us later
    +13.94 ms res 335  read lit       <- flushed 1.3 ms after ITS bind
    +14.26 ms res 332  BIND

Two flips arrive as a burst ~1.5 ms apart, then nothing for 11 ms. The buffer
bound FIRST in a burst is flushed inside the same burst (1.3 ms → lit); the one
bound LAST waits for the next burst (11 ms → black), by which time the app has
cycled both buffers and re-cleared it. **The flush for frame N is released
within 60 µs of frame N+1's bind, every cycle** — i.e. the carried boundary
still covered frame N+1. dxgkrnl submits a flip's DMA buffer about a frame
after the app presented, so `next_wire_fence` at SUBMISSION is already too
late. `BeRdy` 3925 / `BeDef` 4273 matches the 58/42 early/late flush split
exactly: the black frames ARE the deferred arms.

#### RESULT — 0ab-B is NOT a flush-ordering defect. The ordering is irrelevant.

**The artifact bundle for the follow-up static-analysis session is
`tmp/handoff-perf/` — `INDEX.md` lists it, `HANDOFF.md` is the paste-able
prompt, `reports/measurements.md` is every number in one place.**

Three builds settled it, and the last one settled it by removing the ordering
entirely rather than by arguing about it.

| KMD | bind-edge ordering | GT1 fps | black flushes |
|---|---|---|---|
| 22.22.209.0 | boundary sampled at the bind | 183.6 | 20.9 % |
| 22.22.210.0 | boundary carried from flip SUBMISSION | 162.3 | **27.1 %** |
| 22.22.211.0 | boundary carried from the PRESENT MARKER, per buffer | 171.7 | 21.0 % |
| 22.22.212.0 `BindFlushMode=0` | as .211 | 184.0 | 15.1 % |
| 22.22.212.0 `BindFlushMode=1` | **none — flush AT the bind** | 173.7 | 15.8 % |

`BindFlushMode=1` was demonstrably live (`BeDef` 0, `BeRdy` 4747, every bind
flushed immediately) and it changed **nothing**. Note also the run-to-run spread
on identical logic (.211 21.0 % vs .212 mode-0 15.1 %): treat anything under
~6 points as noise.

**The measurement that ends the ordering theory.** Even with every bind flushed
immediately, the flushes that still land 6–12 ms after a bind — the
marker-driven ones — are **50.2 % black**, against 4.2 % for those landing
1–3 ms after. The black rate is a function of HOW LONG AFTER THE BIND the read
happens, not of what triggered it:

| flush lands after its bind | `BindFlushMode=0` | `BindFlushMode=1` |
|---|---|---|
| 1–3 ms | 1.3 % black | 4.2 % black |
| 6–12 ms | 35.5 % black | **50.2 %** black |

**So the bound buffer's content is destroyed ~6 ms after we bind it, and no
flush-timing change can fix that — it can only race it.** The guest-side counter
agrees to within a point: `MkBound/MkTot = 1268/5955 = 21.3 %` of present
markers named the buffer that was, at that instant, the bound scan-out.

`BeWMax=3 / BeWLst=1 / BeWRng=1` also disposes of the "something unrelated is
blocking the boundary" reading: a deferred arm waits on one to three fences,
all on the host GPU ring — the app's own recent frames.

**What it actually is: a buffer-LIFETIME defect.** A Helios scan-out is not a
continuous scan-out; the host reads the buffer only when a `RESOURCE_FLUSH`
tells it to. So the buffer must be immutable from its bind until that flush
completes. It is not: dxgkrnl retires the flip, frees the previous buffer to the
app, and the app clears it for the next frame while it is still the bound
scan-out and still unread.

Two directions, both real, neither yet built:
1. **Enforce the lifetime.** Do not retire a flip's DMA fence until the
   PREVIOUS buffer's final `RESOURCE_FLUSH` has completed on the host. ⚠ Not
   the same as the 2026-07-29 falsified attempt, which held the fence until the
   flip's own `SET_SCANOUT_BLOB` completed — that waited for the BIND, never for
   the READ. Cost: it serialises the app against our readback.
   → **BUILT AND FALSIFIED, 22.22.216.0. See the next section.**
2. **Shrink the window.** The safe zone is measured: <3 ms after the bind is
   ~1 % black. Today the bind lands ~10 ms after the flip is submitted (PASSIVE
   display worker) and binds arrive in bursts of two with an 11 ms gap. Getting
   bind+flush inside a couple of milliseconds would shrink the race to nothing
   and would help latency generally.

#### FALSIFIED — the presentation-LEASE ownership gate (22.22.213.0-216.0)

Direction 1 above was built in full, deployed, and measured against the QEMU
per-flush oracle on one boot with a same-boot control. **The gate works
mechanically and does not close the defect.** Do not rebuild it.

**What was built** (`helios_kmd_logic::scanout_lease` + `AdapterContext`'s
`scanout_{present,bound,read}_epoch`). Every DMA-buffer flip mints a
monotonically increasing PRESENTATION EPOCH in `arm_dma_flip`, stamped on the
allocation itself (`AllocationContext::vidpn_present_epoch`) so the coalescing
single-slot `pending_vidpn_allocation` cannot pair one flip's handle with
another flip's epoch. The display worker publishes `bound_epoch` after the
`SET_SCANOUT_BLOB` returns; a `RESOURCE_FLUSH` carries a typed
`ScanoutFlushToken` snapshotting `bound_epoch` at ISSUE, and its used-ring
response advances `read_epoch`. A flip's `DXGK_INTERRUPT_DMA_COMPLETED` **and**
its `last_primary_address` publication (the CRTC_VSYNC edge — both are reuse
edges, and gating only the first is what 22.22.207.0 did) are withheld until
`read_epoch >= lease`. Escapes, all loud and counted, never a timeout: a
later bind of a DIFFERENT resource supersedes (virtio FIFO proves no read
remains), and enqueue failure / host error / retire / reject / preempt / reset /
transport failure cancel.

**The gate is provably live, not inert** — the 22.22.208.0 trap was checked for
explicitly. Per GT1 run: `LsMint 5590` == `VpDmaF 5590` (one epoch per flip),
`LsRel 5590` (one release per mint), `LsBlk 20668` retirements actually blocked,
and **`LsEndR 4996` of them ended on a REAL HOST READ** vs `LsSupe 573`
superseded, `LsCanc 0`, `LsTear 0`. It also fixed the bind pipeline outright:
`VpCoal` ~500 → **21** and `VpSkip` ~500 → **23**, i.e. essentially every flip
now binds and gets its own read (the "349 of 4127 bind intervals with no read"
gap is closed).

**And it does not move the artifact.** The decisive metric is the black rate of
the FIRST read after each bind — the exact population the invariant makes
impossible, because at that instant the flip is still held:

| build | 1st-read-after-bind black | GT1 fps |
|---|---|---|
| 22.22.212.0 control, same boot | **18.3 %** (771/4203) | 170.8 |
| 22.22.216.0 lease, run a | **14.7 %** (752/5128) | 168.4 |
| 22.22.216.0 lease, run b | **12.5 %** (676/5397) | 179.4 |
| 22.22.216.0 lease, run c | **16.6 %** (874/5257) | 171.7 |

Mean 14.6 % against a 4.1-point spread on the lease side alone, i.e. inside the
documented ~6-point run-to-run band — and nowhere near the ~0 % the invariant
predicts. fps is unaffected (mean 173.2 vs 170.8; instrumentation-off control
168.4).

**What that proves, and it is the reusable part.** dxgkrnl's flip retirement is
NOT what returns the buffer to the app on this stack. The gate demonstrably
back-pressures dxgkrnl — the flip stream serialised, `VpCoal` collapsed — and
the app still clears the bound buffer before our read of it. That is consistent
with the one structural fact this driver has always had: **the app's render work
never travels through a WDDM DMA buffer at all**; it goes to the host over the
Venus escape channel, so no WDDM completion notification can order the app's
writes against our host read. Holding a flip only throttles `Present`; it cannot
stop a clear that is already queued on the host GPU.

⇒ **No KMD-side notification gating can fix 0ab-B.** The whole
"which completion notification releases the allocation" family — 207.0's
bind-hold, this lease gate, and any successor — is closed. The producing write
has to be ordered where it is issued: the UMD/DXVK swapchain, or the host.

⚠ TRAP THAT COST THREE RUNS: `3DMarkCmd` launched from `win_exec` lands in
**session 0**, which has no desktop. The workload reaches `SINGLE_INIT_BEGIN`
and then null-derefs (`0xc0000005`, `rcx=0`) inside its own module with
`helios_umd.dll` NOT EVEN LOADED — it looks exactly like a driver regression and
is not one. Launch it through a session-1 scheduled task
(`helios_lease_gt1` / `helios_trace_fs`, principal `Rupansh` Interactive
Highest). ⚠ Also: re-signing a package without bumping `DriverVer` makes the
installer refuse to bind (correctly) — bump the version for every deploy.

#### Superseded: capture at the PRESENT MARKER, keyed by buffer (22.22.211.0)

The marker is the last point at which "everything submitted so far" still means
"this frame and nothing after it" — the app records it inside its own Present.
`arm_present_marker_refresh` records `(resource → next_wire_fence)` in a
4-slot table on `AdapterContext`; `arm_bind_refresh` takes the entry for the
buffer being bound and arms against it, falling back to sampling (`BeSmp`) when
no marker named it (the MMIO/desktop path, where dxgkrnl retires the flip
before calling us, so "now" IS that frame's boundary).

Falsifiable prediction: **`BeDef` collapses toward zero** (the boundary has
already retired by bind time), the 6–12 ms flush population disappears, and the
black rate approaches the 0.2 % the early population already measures. If
`BeDef` stays near half, the whole watermark family is dead and the answer is a
lifetime contract instead — do not let dxgkrnl recycle the buffer until the
host's RESOURCE_FLUSH for it has completed.

⚠ The table lives on `AdapterContext`, NOT on `VirtioGpu`: adding 64 bytes to
`VirtioGpu` cost **2448 bytes of boot-chain frame** (17488 → 19936, over the
17936 ceiling), because that struct is built on the `DxgkDdiStartDevice` stack.
Re-measured at 17488 after the move.

#### CLOSED AS AN ORDERING QUESTION — the 2×2 factorial + metric validation (2026-07-29 evening)

Full reports: `tmp/handoff-0ab-b-lease/analysis/{factorial-runs.md,
metric-validation.md, SYNTHESIS.md}` (+ 89 raw artifacts under `analysis/logs/`).
Same boot per build, runs interleaved (A,B,A,B,A then C,D,C,D), same UMD binary
across both builds (hash-verified), oracle on for every run, per-run counter
deltas, mode latched via `pnputil /restart-device` (`BndFM` echo checked).

| | **A** .216 lease + mode 1 | **B** .216 lease + mode 0 | **C** .212 + mode 1 | **D** .212 + mode 0 |
|---|---|---|---|---|
| 1st-read-after-bind black | **2.6 %** | 13.8 % | **5.1 %** | 14.8 % |
| 2nd-read-in-binding black | 43.4 % | 5.3 % | 32.3 % | 10.5 % |
| whole-flush black | 15.4 % | 14.8 % | 14.5 % | 16.6 % |
| age-standardised | 17.5 % | 13.3 % | 15.2 % | 13.7 % |
| GT1 fps | 164.5 | 173.6 | 177.9 | 174.4 |
| `VpCoal` per run | 206–642 | **27–34** | 796–873 | 621–886 |

**No cell moves the whole-flush number** (pooled mode effect 0.66 pp, lease
effect 0.39 pp, both under the within-cell spread). `BindFlushMode=1` does not
remove black publications, it RE-LABELS them: the bind-triggered flush reads
~1 ms after the bind and is nearly clean, while the surplus refresh flushes
(187 flushes/s against 131 binds/s) re-read the same buffer several ms later at
32–43 % black. Age-standardised, mode 1 is WORSE, not better.

**Three corrections to the lease section above, from this data:**
1. *"The gate demonstrably back-pressures dxgkrnl (`VpCoal` 500 → 21)"* — the
   coalescing collapse appears in **lease × mode 0 only** (cell B). Under the
   lease at mode 1, `VpCoal` is 206–642, same order as pre-lease. It was a
   lease×latency artifact (slow mode-0 reads → longer withholding → spaced
   flips), not a lease property.
2. *"the '349 of 4127 bind intervals with no read' gap is closed"* — it is not:
   9.5–10.7 % of binding generations still get zero reads under lease+mode0
   (metric validation, `reuse`-delta-confirmed).
3. The lease-vs-control black comparison: with real n and interleaving,
   **B 14.8 % vs D 16.6 %** (raw; 13.3 vs 13.7 age-standardised) — if the lease
   helps it helps by ≲2 points. The original 18.3-vs-14.6 rested on an n=1,
   run-first control and omitted the lowest lease run (215-mode0, 10.3 %).

**The mechanism is PROVEN (upgrade from the inference above).** The first read
of binding N is the event that ends lease N — and it finds the buffer already
cleared **12.9–17.2 %** of the time across every lease run. At that instant no
WDDM edge can have returned buffer N to the app (reuse of N requires flip N+1's
completion → read N+1 → FIFO-after read N). Supersede/cancel escapes are
excluded (`LsSupe` = 0 in cell A entirely; post-supersede generations are
15–20× LESS black; `LsCanc` 0). In real flip-model the app's next clear is
deferred by the SCHEDULER via the allocation list of its render DMA buffer;
Helios's clears never enter a DMA buffer, so that primitive does not exist
here. Corollary of the metric validation: the black is always a complete
opaque clear (`nonzero==0 && max==0 && csum==0`, zero exceptions), 96–98 %
isolated single frames — one ~7–8 ms flash at a time.

**Where the fix lives (decision list for the owner; measured populations):**

black ≈ (share of publishes issued late) × (P(clear executed by then)) — two
independent terms, two owners:

- **D1 — publish once, promptly, content-ordered (KMD).** Bind-triggered
  first reads are already 2.6–5.1 % black; <3 ms-old reads are 0.4–5.6 %.
  Two sub-items: (i) the mode-0 deferred half (`BeDef` ≈ 41 % of binds even at
  1:1 bind rate) fits the **mark-overwrite window** — a bind landing >2 frame
  periods after its present takes the buffer's NEXT present's watermark
  (`record_frame_watermark` replaces same-resource entries), waiting a frame
  too long; fix = consume the recorded mark at `arm_dma_flip` time (dxgkrnl
  submits flips ~1 frame after present, always before the overwrite) and carry
  it in `PresentFlipPrivate`. Confirm with the R5 delta counter before
  building. (ii) flip→read latency: a DISPATCH-level async bind
  (fire-and-forget `SET_SCANOUT_BLOB` from the flip arm; the completion-
  ordered flush arm already fires from the drain DPC) — T6/R902 deleted
  `set_scanout_blob_async` as *unreachable dead code*, not as a falsified
  design, so this is unexplored.
- **D2 — never publish a binding the app may own (KMD, ⛔-adjacent, needs
  explicit owner sign-off).** The surplus re-publishes are 32–43 % black and
  ~30 % of all publishes at mode-1 cadence; suppressing/deferring a refresh
  whose armed identity ≠ the active binding when the active binding already
  had its first publish would take whole-flush to ≈ the first-read rate.
  This is 22.22.208.0's identity gate — inert then only because mode-0
  cadence starved its precondition — and it is ADJACENT TO THE REJECTED
  `BindFlushMode=2`. The old objections now have answers (the lease's epochs
  repair ownership; DWM's same-buffer re-presents mint fresh epochs and are
  never suppressed; a suppressed stale re-read trades a 40 %-black flash for
  a one-frame hold), but the ⛔ stands until the owner says otherwise.
- **D3 — lease disposition.** The DMA_COMPLETED/address withholding is proven
  inert against the defect (this table) and its hang-suspicion is retired
  (defect 0ac reproduced on .212); the EPOCH bookkeeping is what D2 needs.
  Keep epochs, consider retiring the withholding (or bounding it loudly).
- **D4 — the true ~0 % ceiling** is a venus-level acquire: a GPU-side wait so
  the clear executes only after the host read (the "non-hack form" already
  anticipated at the `publish_present_order` consumer note above), or the
  host-side equivalent (read-at-bind atomically in QEMU, owner-gated). Whether
  the ~1–4 % residual after D1+D2 justifies it is an owner call.

Also out of this campaign: defect **0ac** (guest bugcheck 0xD1 on 22.22.212.0,
dump preserved) and defect **0ad** (fullscreen→desktop transition drops the
host readback ~250 ms), filed in the WS1 list below.

#### BUILD 1 SHIPPED — D1(i)+D2+D3, KMD 22.22.217.0 (2026-07-29 ~22:15)

Owner approved D1+D2+D3 and deferred D4. Design:
`tmp/handoff-0ab-b-lease/analysis/FIX-DESIGN-build1.md`; implementation +
review record: `analysis/build1-implementation.md`; acceptance numbers:
`analysis/build1-results.md` (raw artifacts `analysis/logs/b1-*`).

**What landed** (all uncommitted, like the rest of the tree):
- **D2 — the ownership gate** on the flush executor
  (`scanout.rs::queue_active_scanout_refresh_locked`): identity arm (armed ≠
  active → drop, `OgIdn`) + epoch arm (`helios_kmd_logic::scanout_lease::
  surplus_republish`, 5 host tests, `OgEpo`) with a third `tracked` operand
  (`AdapterContext::scanout_epoch_tracked`, mirrored as `LsTrk`) that disarms
  the gate on the desktop's first MMIO bind — the MMIO contract mints no
  epochs, so without it a stale `present>bound` after any app run would have
  frozen the desktop (0aa). LsTrk's disarm was verified on hardware.
- **D1(i) — allocation-carried frame marks** (`AllocationContext::
  vidpn_frame_watermark`, taken at flip-arm time): LIVE (96.9 % of
  frame-boundary binds use the carried mark) but **INERT — the mark-overwrite
  mechanism (R5) is DEAD**: `BeOvw` 11/1/6 per run (0.02–0.25 %), not the ~41 %
  the hypothesis predicted, and `BeDef` stays 45–48 %. The deferred half
  defers because the frame's content genuinely has not retired at bind time —
  which is CORRECT completion ordering, and harmless now that D2 stops the
  surplus reads. Kept: it is 0ab-A-protective and the census cost nothing.
  (Sixth falsified sub-mechanism of 0ab, this time for the price of a counter
  riding a winning build.)
- **D3 — the lease's completion withholding retired, epochs kept** (they are
  D2's predicate). `LsBlk/LsRel/LsPump/LsWait/LsPubG` removed with their
  mechanisms and one-shot-zeroed at StartDevice; the liveness pump deleted;
  `WddmPending` no longer carries a lease.
- **0ac riders**: the `WvTorn` tripwire (with_virtio; the review caught its
  failure arm releasing the lock through the corrupted pointer — fixed to a
  pre-acquire hoisted address, codegen verified) and PDB archiving in the
  deploy script (fired on its first real run: sys+pdb+map in
  `HeliosDeployBackups\20260729-221346\staged`).

**Acceptance (all this-boot deltas, oracle per run):** GT1 ×3 whole-flush
black **2.1 / 0.7 / 2.0 %** (was 14.5–16.6 % in every factorial cell),
first-read 1.9/0.6/1.8 %, fps 185.7/169.2/182.0, duplicate-content
0.2–1.5 % (was 2.9–10.3 %), `OgIdn` 1057–1483 + `OgEpo` 6–13 per run
(24.5–33.4 % of would-be flushes dropped), 2nd-read population 1090–1669 →
**9–26**, 6–12 ms bucket 56 % → **0.5 %** black at unchanged flush share.
Combined ×1: **PASS** (1.3 % black, 61.5 % of binds still content-deferred =
0ab-A ordering intact, `OgEpo` 0). Desktop: binds:flushes 412:415, Start menu
opens (0w closed stays closed), windowed D3D11 + desktop coexist with
`OgIdn` 7.5 % and no starvation — the self-healing argument held, no escape
hatch needed. `WvTorn` 0, `IrqlBad` 0, no bugcheck in 4 runs (not evidence on
0ac's ~1-in-10 base rate).

**Attribution caveat, recorded honestly:** D2 and D3 shipped together, so the
build cannot A/B them — but the dropped-population accounting is
mechanism-level attribution to D2 (the reads that vanished are exactly the
population that was black), and the factorial had already measured D3's
withholding as inert on black.

**Residual ~0.7–2.1 %**: the 1–3 ms margin race at the bind edge (most of
what remains), workload transitions (12+ ms bucket, adjacent to defect 0ad),
and a 3–6 ms bucket that reads 50 % on n≈6 (noise; do not quote it). Paths
below ~1 % if ever wanted: D1(ii) (DISPATCH-level async bind — shrinks the
margin race) and D4 (the venus acquire — the true ~0 % ceiling). Neither is
scheduled; owner's call after the visual check.

**Owner visual verdict (same night): GT1 clean, overall score >25k (was
~20k) — 0ab-B's main population CLOSED. Residual black-frame stutter
observed in GT2 around ~200 fps → filed as 0ab-C**, classification plan and
levers in `tmp/handoff-0ab-c-gt2/HANDOFF.md` (next session).

### 0ab-C — CLASSIFIED 2026-07-29/30 night: the first-publish margin race at GT2's operating point

Two instrumented GT2 runs on 22.22.217.0 (a GT2-only schtask `helios_gt2`
now exists; real GT2 runtime **68–69 s**, fps **209.5/210.1** — the owner's
"~200 fps" is GT2's actual average; the T6-era ~63 fps figure is obsolete).
Full verdict: `tmp/handoff-0ab-c-gt2/analysis/CLASSIFICATION.md`; raw
artifacts `tmp/handoff-0ab-b-lease/analysis/logs/c1-gt2-*`; predictions were
registered in advance (`PREDICTIONS.md`) and scored.

- **Population (a) — the bind-edge margin race on the FIRST publish —
  dominates (~75–85 % of black), the exact population build 1 left open.**
  Whole-flush black 7.3 %/6.0 % (GT1 post-fix: 0.7–2.1 %), carried by first
  reads (7.3 %/6.2 %) in the 1–3 ms bind-age bucket (10.1 %/8.6 %); 96–98 %
  isolated single-frame flashes at ~116–120 flushes/s ⇒ ~7–8 flashes/s = the
  visible stutter. The 6–12 ms bucket stays 0.2–0.4 % and 2nd reads ~1 %
  black — **the 217.0 ownership gate holds unchanged at 210 fps**; identity
  clean (0 mismatches), 2-buffer rotation, `WvTorn` 0 both runs.
- **Guest-side half measured**: inter-bind gaps are bimodal — ~45–48 % at
  1–3 ms but 19–24 % at 10–14 ms and 10–11 % ≥20 ms (mean worker cycle ~7 ms
  vs the 4.8 ms flip cadence). `BeOvw` (bind landing after the same buffer's
  next present) 180/194 per run vs GT1's 1–11, climbing within-run with the
  black. Within-run black tracks scene PHASE (flip rate is flat 197–223/s);
  between operating points it tracks the margin 1/F − latency, which crossed
  zero between GT1's 5.9 ms and GT2's 4.8 ms period.
- Minorities: **(c)/0ad** — the undersized `res 6` transition bind fires at
  t≈63.7 s in BOTH runs (scene-end window w12, each run's worst 5-s window,
  ~12–23 % of total black; separate defect, unchanged). **(b)** duplicates
  3–5 % (GT1 0.2–1.5 %), `VpCoal` 15.5–16 % — real, minor. **(d)** absent.
- **Fix: D1(ii) — DISPATCH-level fire-and-forget SET_SCANOUT_BLOB from the
  flip arm**, design + review checklist + registered predictions in
  `tmp/handoff-0ab-c-gt2/analysis/FIX-DESIGN-d1ii.md`. Pure accelerator: the
  worker path and the DestroyAllocation cancel semantics are untouched;
  values-only in-flight entry; wire-order seq guards bookkeeping; failure =
  count + existing worker ladder.
- **22.22.218.0 (D1(ii) build 1) BUGCHECKED 0xA deterministically under GT2
  (2/2, ~50 s) — ROOT-CAUSED from two kernel dumps, and it is NOT the D1(ii)
  logic:** a pre-existing TOCTOU in the sync-wait protocol.
  `wait_block`'s lock-free `is_done()` early exit let the waiter pop its
  stack frame between the drain Sync arm's `done.store(Release)` and its
  `KeSetEvent` (an ISR or KVM vm-exit stalls the draining CPU mid-window);
  the drain then memcpy'd + signaled a popped `SyncWaitBlock` on the HPD
  worker's stack. .218 armed it by doubling ctrl traffic and lengthening
  drain holds (sync waits started outliving the 15.6 ms wait slice, so the
  poll finally ran inside the window). Dumps show the worker one call past
  its sync bind, spinning on the drain's own lock — the race photographed.
  All abandon/timeout counters zero; fast-bind machinery clean (`FpErr` 0,
  `FpSeq == FpAppSq`, desktop `Fp*` Δ0). Full record:
  `tmp/handoff-0ab-c-gt2/analysis/BUGCHECK-0xA-218.md`; forensics transcript
  `tmp/dump0xA/`; dumps preserved in `C:\HeliosDumps`.
  **Fix = 22.22.219.0: delete the `is_done()` fast path — the signal (or the
  lock-serialized timeout-abandon) becomes the only exit.** (The same
  deletion also fixed the identical latent TOCTOU on the `wait_fence` path,
  which shares `wait_block`.)
- **22.22.219.0 battery (c3-*): the TOCTOU fix HOLDS — 5/5 cells, zero
  bugchecks where .218 died 2/2.** All gates pass: GT1 black 0.7 %
  (0ab-B closed), Combined first-read 0.5 % with `BeDef`-dominant ordering
  (0ab-A closed), desktop inert (`FpBind` Δ0) and clean, `WvTorn`/`IrqlBad`/
  `FpErr` 0 throughout. **GT2: black HALVED, 7.3/6.0 % → 4.0/3.4 %
  (fps 211.4/213.0, up), but the registered ≤2.5 % target was missed.**
  Early/mid-scene windows collapsed to 0.5–1.6 % black; the residual lives
  late-scene. Per-poll attribution: the fast path's ALREADY_BOUND skip is a
  FLAT ~28–34 % coverage gap (`FsC0` 2140/2192 per run — the predicate
  compares against the *applied* active resource, 1–2 flips behind at
  2-deep pipelining), and the late-scene black climbs with `BeOvw` (worker
  bind lateness) — i.e. black ≈ flat-uncovered-fraction × climbing-worker-
  lateness. Secondary (not black): presents outrun reads 2.3:1, `OgEpo`
  416–538/run (correct surplus drops), duplicates 11 %, in-scene flush rate
  −22 % under the doubled ctrl load — display-freshness economics, a
  separate lever. Full numbers + registered next-step predictions:
  `tmp/handoff-0ab-c-gt2/analysis/build219-results.md`.
- **22.22.220.0 = D1(ii)-b** (wire-resource skip predicate): predicate
  confirmed live (`FsC0` 2140→296) but the freed flips became `FpBusy`
  (61→1776) — the SINGLETON bind command buffer was the real coverage
  bottleneck (it only returns at the guest DPC drain, which lags the host
  consume by several flip periods). Exact identity all cells:
  `FpBind + FpSkip + FpBusy = VpDmaF`. GT2 black flat; GT1 1.9 % at
  190 fps (confounded). `tmp/handoff-0ab-c-gt2/analysis/build220-results.md`
  (includes the x/y identifiability argument that motivated the last build).
- **22.22.221.0 = the bind command POOL (depth 4; `Vec`, not array — the
  inline array measured 18128 B on the StartDevice chain vs the 17936
  ceiling) — THE DISCRIMINATING RUN, and it discriminated:**
  `FpBusy` → 0, coverage → **99.1/99.3/99.9 %**, `FpBind = VpDmaF − FpSkip`
  exact — and **GT2 black did not move by one decimal (3.5 %/4.1 %)** ⟹
  x = y in the mixture model: flip-time binds go black at the worker-timed
  rate. **The bind-timing family is EXHAUSTED WITH PROOF for GT2** — 0/439
  pooled black in the 0–1 ms bind-age bucket; the loss is in the READ
  window, to the venus-executed clear no publish timing can outrun. The
  remaining GT2 lever is **D4 (venus-level acquire / host read-at-bind,
  owner-gated)**, plus the flush-freshness economics as a separate quality
  item. **The same pool FIXED GT1: 1.9 % → 0.3 % — the best GT1 black
  recorded, below the whole .217-era band** (at 178–190 fps the margin is
  wide enough for flip-time binds to win; at 210+ fps it is not).
  Three consecutive clean batteries on the TOCTOU fix (c3/c4/c5, 15
  workload cells, zero bugchecks). GT2 fps 214.1/212.5 (up), dup% improved
  (12.6→7.6 %), `OgEpo` calmed (338→83). Watch: GT1/Combined scores c5
  181.0/20.11 vs c4's 190.3/21.51 (c4 looks like the high outlier vs c3/b1;
  re-measure before calling it a cost). Full record:
  `tmp/handoff-0ab-c-gt2/analysis/build221-results.md`.
- Shipping state after the four-build night: 22.22.221.0 (`DspBnd=1`,
  `BndFM=0`; superseded by 22.22.222.0 below).
  **OWNER EYE VERDICT (2026-07-30): GT1 visually CLEAN — the GT1 half of
  0ab-C is CLOSED by the ground-truth rule. GT2 still visibly flashes,
  ~24 black frames over a full run** (≈0.5/s visible vs the oracle's
  ~3.3–3.9 black flushes/s — VNC delivery samples roughly 1-in-7 into
  displayed frames). The GT2 residual is proven out of
  KMD-publish-timing scope → **next session = the D4 family**, handoff at
  **`tmp/handoff-gt2-d4/HANDOFF.md`**. Deferred hygiene items: the fast
  path's mint-before-enqueue staleness (benign, one-line remedy documented
  in the 220 implementor report); the `FpCoal`-heavy same-res coalescing
  semantics if the pool ever deepens further.
- **D4a v1 BUILT, LIVE, and MEASURED INERT on GT2 black — the in-flight
  conditional's blind spot found (2026-07-30, KMD 22.22.222.0 + UMD/DXVK,
  battery c6)**. The full acquire chain shipped and is proven end-to-end:
  per-resid READ LEDGER page (escape 0x000E, `RdIss == RdRet` exact in
  every cell, zero overflow/orphans), persistent retirement events
  (0x000F), `ScanoutFlushToken` carries resource identity with
  Drop-guaranteed retirement, DXVK arms conditional GPU-side timeline
  waits at the reuse submission (GT2: armed ≈5 % of frames, signals
  sub-8 ms, `residMiss=0`; GT1 0.4 %; desktop ~0), knob `ScanoutAcquire`
  (default ON, free: **GT1 192.6 and Combined 22.44 — both records — with
  it on**; GT2 213.5/209.8 fps in band; 0ab-A gate 0.5 % exact; zero
  bugchecks). **GT2 black: 3.9 %/4.0 % — unchanged from .221's
  3.5/4.1 %.** The design's §8 falsifier fired with a sharper
  localization: at ~2 frames of CPU run-ahead the reuse-list's ledger
  check races the bind-edge flush ISSUE of the buffer's own last present —
  the killer read is not yet in flight when the only sound check can run,
  and the watermark orders that read only against its OWN frame's content
  (0ab-A's guarantee, which held), not against the NEXT reuse's clear.
  **The in-flight-only conditional (the −40 %-trap cost guard itself) is
  therefore structurally insufficient at GT2's operating point.** Both
  closures are owner-gated: **D4a v2** (settle-semantics wait — UMD-side
  per-resid present counter closes the race by program order; KMD signals
  settlement from the existing lease edges; cost = throttles run-ahead
  toward flip cadence, unmeasured, same family as the §3(a) trap → build
  knob-gated with a registered fps envelope) or **D4b** (host-side
  read-at-completion in qemu-helios; structural kill, slow owner-gated
  loop). Record: `tmp/handoff-gt2-d4/analysis/build222-results.md`
  (+ `FIX-DESIGN-d4a.md` with scored predictions).
- Interim shipping state 22.22.222.0 (D4a live-but-inert; superseded below).
- **D4b — THE ORDERED SNAPSHOT CHAIN: BUILT, SHIPPED, and ORACLE-CLOSES the
  GT2 residual (2026-08-02, KMD 22.22.224.0, battery c8). GT2 oracle black
  3.9–4.1 % → 0.02–0.1 %.** Owner selected D4b; the design conversation
  established that a QEMU-only fix cannot order against the venus-ring
  clear (packaged render server, content destroyed before flush arrival),
  so the snapshot/copy rides the ONLY viable insertion point — the app's
  own submission stream: at present time DXVK records a GPU blit of the
  presented primary into a 4-slot ring of DXVK-internal DirectOptimalScanout
  images (queue-ordered after frame N, before clear N+2, NO waits); the
  UMD substitutes the snapshot's full descriptor into the present private
  data (`HELIOS_PRESENT_PRIVATE_FLAG_SNAPSHOT`, wire struct 40→48 B,
  capability-gated by the 0x000E PROBE caps); the KMD carries it BY VALUE
  into the flip and binds/flushes the snapshot while ALL flip bookkeeping
  stays on the real allocation. Nothing ever clears a snapshot — the race
  died structurally. QEMU/virglrenderer: zero changes. Census EXACT:
  `SnSub = VpDmaF = FpBind`, `SnFbk = 0`, `FpSkip = 0`, `BeCar` dominant
  (0ab-A preserved), `RdIss == RdRet` everywhere, zero bugchecks; GT1
  black 0.2 % with the fps↔margin correlation broken (the mechanism's
  differential signature), Combined 22.68 = record, desktop/MMIO path
  untouched. **The .223 detour's permanent lesson: dxgkrnl does NOT
  forward Present private data to DxgkDdiPresent on DMA flips (PBIdOk=2
  across three generations) — per-present data for the flip arm must ride
  the Render command; .224 stashes it per-context at DxgkDdiRender and
  takes+clears at every Present.** Records:
  `tmp/handoff-gt2-d4/analysis/{FIX-DESIGN-d4b-snapshot,build224-results}.md`
  (+ build222-results.md for the D4a-v1 falsification that motivated it).
  Residual black counts are 0ad-class transition edges. **0ab-C is
  oracle-closed; the owner's eye (baseline ~24 visible flashes/run) is the
  ground-truth close.** Note: the c8 battery ran at 1896×1030 (EDID/viewer
  geometry after a host reboot) — fps not comparable to 1280×800
  baselines; a `ScanoutSnapshot=0` A/B isolates the blit cost if wanted.
- **0ab-C: CLOSED 2026-08-02, OWNER-CONFIRMED FIXED.** Close-out build
  **22.22.225.0**: sane defaults made code defaults (`DisplayHalf` now
  defaults ON — the render+display miniport is the product; the production
  `reg add` is gone), leftover diagnostics retired (`PresentProbe` registry
  value cleared; the dead PresentCb-private channel — decode, `PBIdOk`,
  and the UMD's write — deleted outright, since dxgkrnl never forwarded it
  on flip presents), census log cadences quieted to steady-state
  (per-16384). Smoke on .225: GT2 black 1/4766 = 0.02 %, `SnSub == VpDmaF`,
  ledger exact, zero faults. The entire 0ab arc (D1(ii) family + D4a + D4b)
  is committed and pushed (main `wddm`, dxvk-helios `master`, qemu-helios
  `helios-11.0.1`).
- **SHIPPING STATE: 22.22.225.0**, all knobs at code defaults; kill
  switches `ScanoutSnapshot=0` / `ScanoutAcquire=0` / `DispatchBind=0` /
  `DisplayHalf=0`.
- **NEXT FOCUS: PERFORMANCE (WS2) — the STRUCTURAL bottleneck** — Fire
  Strike Graphics 43k → 70k+, Combined → 10k+. Handoff:
  **`tmp/handoff-perf-structural/HANDOFF.md`** (owner: one structural
  point, not micro-levers; Steel Nomad Vulkan is only ~10 % off native ⇒
  the shared ICD/venus/KMD substrate is exonerated, the gap is
  D3D11-side). Supersedes `tmp/handoff-perf-saturation/HANDOFF.md` (its
  attribution + lever outcomes stand in
  `tmp/handoff-perf-saturation/reports/p1-attribution.md`).
  **66th session (2026-08-03): the prime suspect is CONFIRMED** — at
  THREADING caps = 0 the runtime EMULATES command lists: Fire Strike's
  ~15 workers record into software deferred contexts (SWDC_* frames,
  worker sample) and the render thread replays every call through the
  immediate DDI (`SWCL_CommandList::Execute` = #1 d3d11 function by
  direct RIP; replay markers in 42.5 % of UMD samples, lower bound).
  Evidence: `tmp/handoff-perf-structural/reports/
  p0-commandlist-verification.md`. Build plan (phases A thread-safety →
  B FREETHREADED → C COMMANDLISTS_BUILD_2 → DXVK's stock
  D3D11DeferredContext, each knob-gated):
  `tmp/handoff-perf-structural/PLAN-commandlists.md`. DXVK fork and cxx
  bridge need ZERO changes — the work is entirely in `umd/src`.
  **Phase A LANDED (`ed7efe1`, thread-safe forward layer) and Phase B
  LANDED (`b47bdd4`, FREETHREADED behind `UmdFreeThreaded`, absent=ON).**
  First Phase B canonical run: GT1 190.92 / GT2 208.92 / Combined 25.25 /
  Physics 112.11 — GT2 +3.4 % on the tight metric, GT1+Combined at/above
  the historical best, all gates green. (Standard-preset scores are
  display-mode-independent — owner-confirmed; comparable to baseline
  directly. Owed: 3-run median + optional UmdFreeThreaded=0 knob A/B for
  attribution, cold-boot gate.)
  **Phase C LANDED AND MEASURED (67th session, 2026-08-03): NEGATIVE on
  GT1/GT2, POSITIVE on Combined — knob stays default OFF.** Full DC/CL
  DDI surface (tag-discriminated HDEVICE namespace, context-local handle
  copies, BUILD_2 recycle flow → DXVK's stock D3D11DeferredContext)
  landed in `2fccb5c`+`9fbeeb6`+`fa1d75b` behind `UmdCommandLists`
  (bring-up default OFF; requires UmdFreeThreaded). Same-boot A/B:
  ON = GT1 49 / GT2 144 / Combined 28.6–31.2; OFF = GT1 184.7 / GT2 210.6 /
  Combined 25.3–26.7 (= the Phase B 3-run baseline GT1 184.23 / GT2
  209.65 / Comb 25.01, so environment+ICD exonerated). The replay share
  DID move off the render thread (d3d11.dll 42.5 %→3.4 % of samples) and
  submits are EQUAL (AsSub 110.7k vs 115.1k — no flush storm); the loss
  is DXVK's constant per-CL costs on the single dxvk-cs consumer at the
  runtime's granularity (GT1 942k, GT2 2.66M FinishCommandList cycles
  per run; recorded trailing state reset in EVERY CL; double reset +
  chunk flush per execute; render thread 47 % parked on CS
  backpressure). Numbers, scored predictions and the identified next
  lever (make DXVK per-CL costs content-proportional — fork changes now
  in scope): `tmp/handoff-perf-structural/reports/p2-phase-c-outcome.md`.
  ⚠ Two host-stack robustness defects found en route (WS1):
  `tmp/xid109-evidence/INCIDENT.md` — a venus worker can wedge inside the
  NVIDIA driver and QEMU's virtio-gpu serializes behind it forever (all
  contexts starve; one instance drew an Xid-109), and worker death is not
  handled as context-loss. Hit 2× on 2026-08-03 during 3DMark
  probe/loading phases, then 2 clean runs — timing-sensitive.
  **67th session: wedge #3 captured LIVE with debuginfod symbols and the
  wedge class FIXED guest-side** — the ring thread was executing the
  guest's async `vkWaitSemaphores(UINT64_MAX)` (Mesa venus upstream's
  feedback race-closer) inside the NVIDIA driver for a channel NVRM had
  already Xid-killed; stock virglrenderer passes the guest timeout
  verbatim, so bounding it guest-side frees the ring. icd/mesa
  `f0c7bcd3465` (`VN_HELIOS_RING_WAIT_BOUND_MS`, default 8000);
  4 subsequent FS runs, zero wedges/Xids; A/B-proven perf-neutral.
  Evidence: `tmp/xid109-evidence/wedge3/WEDGE3.md`. Still open (WS1):
  QEMU treating worker death as context-loss, and the Xid trigger if it
  recurs with the bound in place.
  **Phase D attempted (68th session, 2026-08-03): dxvk-helios sweep
  elision measured INSUFFICIENT — knob stays OFF; the Xid-109 trigger is
  now the gating item for any further CL-path work.** Levers 1+2 of
  HANDOFF-PHASE-D.md landed in dxvk-helios `3daacecc` (parent `7470eda`):
  the immediate context elides redundant `ResetCommandListState` sweeps
  via CS-stream tail tracking (`m_heliosCsState`), and `FinishCommandList`
  stops recording the trailing sweep into every CL (`EndsClean=false`,
  leftover state restored by the EmitCs funnel; kill switch
  `HELIOS_DXVK_CL_FAST=0` = stock). Producer side sped up as designed
  (2.89M finishes in 44 s ≈ 65k/s vs stock ~8k/s) but scores barely
  moved: ON+fast GT1 53.8 / GT2 145.1 / Comb 33.2 vs ON-stock 49/144/29–31
  — the reset sweeps were NOT the dominant per-CL cost. Remaining CS-side
  suspects: per-chunk dispatch/wakeup overhead (EmitToCsThread → one
  chunk + one queue op per tiny CL) and the CL content itself. GT1-ON
  runs at ~15 % GPU (host trace) — still guest-CPU-bound.
  **The Xid-109 trigger characterized (WS1, still unfixed): NVRM CTX
  SWITCH TIMEOUT on the workload's venus channel, mid-GT1 only,
  native-CL path only — 2 of 3 fast-path runs (+24 s, +53 s), ~1 of 3 at
  stock Phase C rates (wedge3), never on the emulated path at 184 fps.**
  New presentation with the ring bound in place: the channel dies, the
  next `vkQueueSubmit2` dispatch fails ("resulted in CS error"), the
  guest sees it at `vkEndCommandBuffer` (dxvk-cs exception) — a
  PER-CONTEXT death with clean `pnputil /restart-device` recovery, no
  QEMU relaunch (containment proven live twice). KMD counters clean both
  times (AsSub==AsDone, no storm); healthy-run scores are tight, which
  argues against systematic garbage draws; the leading remaining theory
  is a timing-sensitive lost/misordered device-side signal (the WS1
  "never signal a wire fence before host completion" suspect —
  rate-amplified, execute-count-correlated: GT1 has 2× GT2's executes).
  **A persistent host-side evidence trap is ARMED** (`tmp/xid-trap/`,
  nohup, survives sessions): on any NVRM Xid line it captures journal
  context, nvidia-smi, QEMU stderr tail, per-thread states and gdb
  backtraces of every virgl_render_server (ptrace_scope=0). Operating
  rule going forward: NO knob-ON benchmark runs except with the trap
  armed and a specific hypothesis to discriminate — the next occurrence
  must pay for itself in stacks, not scores.
  ⛔ **STALE as of the 72nd — the GT1 half of this entry no longer
  reproduces.** Owner report, 2026-08-05: **GT1 never triggers Xid-109
  any more**; it was fixed in work that landed after `18dba5f` and this
  section was not updated. Everything above about "2 of 3 fast-path GT1
  runs", the rate-amplification model and the trap operating rule is
  history. ⚠ The `tmp/xid-trap/` trap is still armed and last captured on
  2026-08-03; there is no live GT1 Xid for it to catch.

  ⭐ **72nd: what IS live is a D3D12-only reproducer, and it is a
  different animal.** `test_uav_counter_null_behavior_dxbc` and `…_dxil` from
  vkd3d-proton's own suite fire it **on demand, ~6-8 s in, two for two**
  (dxbc start 16:02:20 → `Xid 109 … name=vkr-ring-346, channel 0x1b, CTX
  SWITCH TIMEOUT` at 16:02:26; dxil start 16:11:23 → Xid at 16:11:31, a
  different ring). Run one test headless in ~30 s instead of waiting for
  2-of-3 fast-path GT1 runs. `tests/d3d12_descriptors.c:4440` builds UAVs
  with a **null counter resource** and dispatches counter ops on them —
  the test calls it *"technically undefined, but all drivers behave
  robustly here"*, and its neighbour records *"Observed on NV: Blue screen
  of death (?!?!)"* for the analogous case, so the family hard-faults
  NVIDIA hardware rather than returning zeroes.
  ⚠ **Do not read this as evidence about the old GT1 Xid** — that one is
  fixed and gone (see above), so there is no rate-amplification theory
  left to discriminate against. This is a **self-contained D3D12
  robustness defect**: a guest application can fault the host GPU context
  with a null UAV counter descriptor, and the guest then hangs instead of
  seeing an error. It is scoped to the D3D12 workstream and it is
  excluded from routine gate runs (below), so it does not gate anything.
  ⚠ **Containment reconfirmed, and the real defect named:** the Xid kills
  ONE channel — the `D12-G1` bridge probe passed all 28 steps *while the
  wedged process was still alive* — but **nothing propagates the loss to
  the guest**: `/tmp/helios-qemu-stderr.log` has no entry at all for
  either Xid, and the guest's vkd3d fence thread sleeps in the venus ICD
  forever (6+ min, 0.17 s CPU).
  ⭐ **Why the ring bound does not cover this, and it is structural rather
  than a missed site.** That fix bounds a *ring* wait
  (`VN_HELIOS_RING_WAIT_BOUND_MS`, default 8000,
  `icd/mesa/.../vn_queue.c:2824`), and venus's own escalation ladder
  (`vn_relax`, `vn_common.c:248`) checks `VK_RING_STATUS_FATAL_BIT_MESA`
  and the `ALIVE` bit through `vn_watchdog_timeout()`. **Every one of
  those signals is ring liveness.** Xid 109 kills the GPU *channel* while
  the `vkr-ring-NNN` thread stays perfectly healthy and keeps marking
  itself alive — so the watchdog is watching the wrong thing, and the
  guest waits on a fence whose GPU work is already dead. ⛔ Do not "fix"
  this by shortening the ring bound; the signal needed is *host
  submission/fence failure*, which today reaches neither QEMU's log nor
  the guest. **A lost host context must become a guest-visible error
  (device removal / TDR), not an unbounded wait** — that is the WS1 fix,
  and it is now testable in half a minute.
  ⚠ Note for the record: the two tests exercise behaviour vkd3d itself
  calls undefined, so they are **excluded by name** from routine G2/G9
  runs (`test-runner.sh -x`, fork commit `fd205b2c`, which prints
  `EXCLUDED <name>` for every one) and kept as the dedicated repro. The
  exclusion is a scheduling decision, not a verdict: a guest application
  being able to fault the host GPU context is a real robustness defect and
  stays open here.
  Evidence: `tmp/dx12/gates/G2/hang/` (stacks + `/ma` dumps),
  `tmp/dx12/gates/G2/hangs.txt`, `docs/dx12/GATES.md` §4.3.
  **NEW WS1 watch item (67th): `ScStale` ≈ 4,000/run under FS (23 % of
  18k flips; ScUnav ~10/run) — PRE-EXISTING load signature, masked until
  now because every historical gate check followed a counter-zeroing
  device restart.** The KMD's ticketed gate-clear refusal handles it
  (display correct, scores normal), but `adapter/mod.rs`'s "interleave
  does not occur today" invariant is falsified under FS flip load. The
  kmd-gate-surface must-be-zero list needs a policy answer (expected-
  under-load vs defect) before the next KMD tranche.

## Workstream 1 — Stability

**IDD frame freeze: DIAGNOSED 2026-07-05 (17th session), live on the frozen boot** — full chain
in memory `idd-freeze-root-cause-chain`. Summary: (1) routine multi-second completion stalls
(per-present full-GPU drain in `rotate_resource_backings` + event-cadence desktop) →
(2) the 4×8 s sem-deadline latch declares CONTEXT LOST on a healthy-but-slow stack →
(3) dxvk teardown on DEVICE_LOST resets command pools with host work pending (= the
`vkResetCommandPool` VUs; symptom, not cause) → (4) post-loss, `submitCmdLists` drops cmdlists
WITHOUT `notifyObjects()` → in-use refs leak → next `Map` → `waitForResource` (no timeout, no
lost-check) wedges dwm permanently; win32k session-1 GDI hangs behind it. Falsified: the
early-fence/helios_sync theory for the steady-state stall — 0 of 251,810 submissions carry
ring≠0; the vn win32-sync signal path never fires; cross-process sync is dxvk-helios-internal.
**Status 2026-07-06 (18th session): the whole chain is now closed** — (1) the "stall" was the
sem-deadline misreading idle wait-before-signal waits (fixed, defect 1 below) plus the rotate
drain (fixed, WS2); (2) the latch no longer fires on idle desktops; (3)+(4) fixed 17th session.
Remaining: cold-boot + multi-hour soak, and the forced-loss test for the loss path (defect 2).

Open defects, roughly ordered:

0w. **CLOSED 2026-07-28 — the Start menu never opened: our own QFOT re-acquire
    claimed `DxvkAccess::Write` on a still-open command list, making
    `SyncSharedTexture`'s wait unsatisfiable.** Owner report: "clicking the
    Windows button or the search bar does not open them — it's literally not
    there, I can't interact with it either", plus "if I keep clicking the
    search bar, it sometimes appears".

    **Symptom, measured.** `tools/start_menu_repro.ps1` and
    `tools/start_invoke_probe.ps1` (session-1 tasks) press the hotkey / click
    the real Start button and report each participant's CPU delta, window tree,
    UMD-log growth and screen pixels:
    - `StartMenuExperienceHost` burned **0.000 s of CPU** across 6/6 Win-key
      presses AND a real Start-button click, owned **zero** top-level windows,
      and its UMD log grew **+0 bytes**. It never woke.
    - Controls passed: Win+R opened a Run dialog from the same probe (so the
      synthetic input was real) and `SearchHost` painted its flyout in ~150 ms.
      The desktop, dwm, composition and the taskbar were all healthy.
    - Its UMD log ended mid-call — **8 `calling DXVK CreateTexture2D` vs 7
      `returned`** — on a 704x576 `fmt=65` (A8_UNORM) `misc=0x802`
      (SHARED|SHARED_NTHANDLE) texture, i.e. a XAML glyph atlas.

    **Root cause.** `DxvkContext::acquireSharedImagesFromExternal` — the
    Helios-only queue-family-ownership-transfer re-acquire, which upstream DXVK
    does not have at all (0 occurrences upstream vs 9 in the fork) — ran at the
    START of every command list and did
    `m_cmd->track(image, DxvkAccess::Write)` for every shared image the
    previous list touched. `track(obj, access)` records access "for the purpose
    of CPU access synchronisation" (`dxvk_cmdlist.h`), which is exactly what
    `DxvkDevice::waitForResource` tests. So a brand-new command list — still
    OPEN, never submitted — held a write reference on the texture, and
    `D3D11Initializer::SyncSharedTexture`'s `waitForResource(image, Write)`
    could only be released by that list being submitted, by the very thread now
    blocked in the wait. On an idle XAML startup that is a permanent deadlock:
    the UI thread parked forever and the Start menu's CoreWindow was never
    created. A queue-ownership transfer does not write image contents, so the
    Write claim was simply wrong.

    Chain, symbolised from a live minidump (`take-minidump.ps1` + `cdb` with
    the release PDB):

    ```
    Windows_UI_Xaml → dcomp → d3d11
      → helios_umd::forward::resource::create_resource
        → dxvk::D3D11Device::CreateTexture2D → CreateTexture2DBase
          → dxvk::D3D11Initializer::SyncSharedTexture
            → dxvk::DxvkDevice::waitForResource
              → DxvkSubmissionQueue::synchronizeUntil → SleepConditionVariableSRW
    ```

    **A TIMEOUT WOULD HAVE BEEN THE WRONG FIX (owner directive), and the dump
    proved it**: all three DXVK workers were parked on their own idle condition
    variables — `dxvk-submit` in `submitCmdLists`, `dxvk-queue` in
    `finishCmdLists`, `dxvk-cs` in `threadFunc` — so every queue was drained
    while `isInUse(Write)` was still true. Nothing was in flight to wait for;
    the reference was leaked. The wedged instruction
    `mov rax,qword ptr [r14+10h]` gave `m_useCount = 0x0000010000000002`, which
    with `getIncrement = 1 << (access*20)` and `Write = 2` decodes as
    **Write 1, Read 0, refcount 2** — exactly one unreleased `acquire(Write)`.
    Both documented deliberate-leak paths were excluded: `m_lastError` is
    sticky and never reset, and the wait did not take its `DEVICE_LOST`
    bail-out, so no device loss occurred.

    **Fix (ordering/accounting, not a bound):** both QFOT sites now take a
    lifetime-only reference, `m_cmd->track(image)`. `DxvkObjectRef` holds a
    strong `Rc` released when the list retires, so the barrier still keeps the
    image alive; genuine content accesses recorded into the list continue to
    track Read/Write normally, so `waitForResource` keeps its real meaning. The
    release side was changed to match — it is not a deadlock source on its own
    (its list is submitted immediately) but an asymmetric pair invites the bug
    back.

    **Verified by A/B under a forced race.** The bug is ~1-in-6 at XAML startup
    and did **not** reproduce in 35 scripted attempts (25 quiet restarts + 10
    `restart-device` churn cycles), because `ExecuteFlush` only *injects* the
    flush chunk and the caller usually beats the CS thread to the wait. Adding
    a temporary 150 ms sleep between the flush and the wait made the caller
    lose that race deterministically:
    - **before:** wedged on iteration 0; instrumented log read
      `QFOT-ACQ res=X inUseWrite=1` → `WFR-ENTRY res=X inUse=1` →
      `UNSATISFIABLE waitForResource … submission queue fully drained`.
    - **after:** 8/8 creates returned, `wedged=0`, every `WFR-ENTRY` read
      `inUse=0`, zero unsatisfiable waits.
    Shipping build re-verified end to end: desktop composites and the Start
    menu renders in full (`Z:\tmp\start_menu_open.png`), plus a
    `restart-device` churn soak.

    **Kept as permanent instrumentation:** a loud check in `waitForResource` —
    if a resource is still in use while `DxvkSubmissionQueue::isDrainedLocked()`
    reports both stages empty, nothing pending can release the reference, so it
    logs the resource, its access bits and trackId once and counts the
    occurrence (`waitForResource STALLED`). It deliberately does not change
    behaviour; bounding the wait would return with the reference still held.
    All investigation-only logging (`QFOT-ACQ`, `WFR-ENTRY`) and the sleep knob
    were removed. To re-provoke the race for a regression test, re-add a sleep
    between `ExecuteFlush()` and `waitForResource` in `SyncSharedTexture`
    (noted in the comment there) and run
    `tools/d3d11_shared_wedge_repro.cpp --clear --watchdog-ms 25000`.

    ⚠ **Read that counter as a RATE, not as pass/fail — the check is a warning,
    not a proof of deadlock.** It fires whenever no completion is currently
    possible, which is terminal only if no other thread will submit the holding
    command list. That is what made 0w fatal (a single-threaded XAML UI
    thread), but **dwm trips it exactly once per start and recovers**, because
    another of its threads submits the list. Verified on the fixed build: dwm
    pid 8184 logged `occurrences=1` at startup, then stayed responsive with a
    live desktop and the count flat. A repeating or never-cleared occurrence is
    the one that matters; one line in dwm's log at startup is expected and is
    NOT a 0w regression.

    ⚠ **Deploy trap that cost two build cycles and invalidated two runs:** the
    default ProgramData UMD hotplug does **not** reach new processes. dxgkrnl
    caches the UMD path at **device** start, so freshly launched processes kept
    loading the previously deployed DLL; two builds' worth of instrumentation
    appeared absent because the old image was still being loaded. Confirm with
    `(Get-Process -Id N).Modules` and deploy with `-KillUmdUsers
    -RestartDevice -NoProbe` whenever the new code must actually run.

    ⚠ **Probe trap:** neither `EnumWindows` nor a `GetWindow(GW_HWNDNEXT)`
    Z-order walk enumerates the Start/Search flyout CoreWindows — nor even
    `Shell_TrayWnd`, which `FindWindowW` finds instantly. "0 top-level windows"
    is NOT proof a flyout is absent; the host process's CPU delta and the
    screen pixels are the discriminators.

    Tooling added: `tools/d3d11_shared_wedge_repro.cpp` (forced-race repro with
    watchdog), `tools/start_menu_repro.ps1`, `tools/start_invoke_probe.ps1`,
    `tools/start_menu_churn_hunt.ps1`, `tools/start_menu_poke.ps1`,
    `tools/start_menu_shot.ps1` (schtasks `helios_startrepro`,
    `helios_startinvoke`, `helios_pokestart`, `helios_startshot`).
0y. **CLOSED 2026-07-28 — the 6-handles-per-device leak, and the ~350-device
    fail-fast with it. ONE root cause for both.** Reported by the ownership
    soak since T5, constant at 5.99/device across T5/T6/T7/T8.

    **Root cause: `helios_umd.dll` and the venus ICD are loaded and UNLOADED
    once per D3D11 device, and nothing releases a module's process- or
    thread-lifetime state on unload.** A Rust `static`/`OnceLock` is never
    dropped, the loader closes no handles a module opened, and tss destructors
    run at THREAD exit — so on a thread that outlives the module they never
    run at all. Measured, not inferred: `GetModuleHandleW("helios_umd.dll")`
    reads NO / yes / NO across one `D3D11CreateDevice` + `Release`, and the
    UMD's once-per-DLL-instance `UMD module:` line appears once per device.

    The six, each named by type, module and creating stack (the module
    attribution is a `LoadLibrary`-pin bisect; the stacks are
    `tools/helios_handle_origins.cpp` + `addr2line` on the mingw DWARF):

    | # | type | site |
    |---|------|------|
    | 1 | `File` | `helios_umd` `log.rs`, the `OnceLock` log handle (`umd-<pid>.log`, access 0x00120194) |
    | 2 | `Event` | ICD `vn_renderer_helios.c:1584` `helios_fence_event_get`, the per-thread tss fence event |
    | 3,4 | `Event`+`Thread` | winpthreads registering the caller's NATIVE thread (event + duplicated thread handle) |
    | 5,6 | `Semaphore` ×2 | libgcc `emutls.c:104` `emutls_init` → `__gthread_mutex_lock` |

    Fixes: the UMD closes its log handle in `DllMain(DLL_PROCESS_DETACH)`
    (`FreeLibrary` case only, `try_lock` so it cannot deadlock under the loader
    lock, refusals counted in `LOG_CLOSE_CONTENDED`); the ICD **pins its own
    module** (`GetModuleHandleExW(..._PIN)`, refusals counted in
    `helios_module_pin_failures`), because four of its five handles are inside
    the statically linked mingw runtime and unreachable from any detach hook we
    could write, and the fifth belongs to threads the module does not own.

    **Measured, 1000 devices / 10 000 resources:** 6.00 → 5.00 (UMD fix alone)
    → **-0.00 handles/device**; working set +3,584 KiB where 300 devices alone
    used to cost +39,048 KiB; modules +0, dwm +0, failures 0/0;
    `OWNERSHIP SOAK PASS`, exit 0. **The WARP control still reads +0.00**, so
    the fix is in the driver and not in the measurement. ★ **The soak also
    completes at 1000 devices for the first time** — 7d(b) recorded that scale
    as unreachable on any build (deterministic `0xC0000409` fail-fast in
    `ucrtbase` between cycle 301 and 400), so the DLL churn was that too.

    Residue, all recorded rather than hidden:
    - The soak now fails on handle **growth**, not on any drift. With the leak
      gone it settled at a fixed −2 (identical at 300 and 1000 cycles), and the
      two are **ALPC Port** handles the Windows RPC runtime closes on its own
      idle schedule inside the window — its old exactly-equal criterion called
      that a Helios defect.
    - The soak's working-set tolerance (16 MiB) is calibrated per **1000**
      resource cycles but the default is 10 000, so the WARP control fails its
      own default scale on working set (+80,484 KiB, linear in the documented
      ~8 MiB). Read the control on its HANDLE number.
    - ⚠ **New hazard from the pin:** a process that survives an ICD redeploy
      now keeps the OLD ICD image loaded alongside the new one. The codebase
      already anticipates two live ICD images (see the
      `helios_venus_query_scanout` comment on decoding a DXVK `VkInstance`),
      but a deploy still wants a `restart-device` or a dwm restart, not just a
      manifest swap.
    - The deeper fix is upstream of both: a DXVK that shares one `VkInstance`
      across D3D11 devices would end the load/unload churn at its source and
      stop paying loader enumeration + ICD load per device. Not attempted here
      — different blast radius.

0ac. **NEW 2026-07-29 (factorial campaign, run D-2x): guest BUGCHECK `0xD1`
   DRIVER_IRQL_NOT_LESS_OR_EQUAL in `helios_kmd_render.sys` — on
   22.22.212.0, the PRE-LEASE build**, 41 s into a Fire Strike GT1 run at
   BindFlushMode=0. Read of `0x00000001'59b430d0` at DISPATCH_LEVEL from
   `helios_kmd_render+0x21076`; two frames up the stack sits the VALID kernel
   pointer `0xffffd38f'59b43050` (same region, high dword ffffd38f→00000001,
   low +0x80) — shape of a torn/truncated 64-bit pointer dereference.
   WinDbg bucket (public symbols, nearest-symbol hint only) names the
   virtio transport / `WdkHal` / `DxgkConfigAccess` region. **Evidence
   preserved**: kernel dump `C:\HeliosDumps\MEMORY-D2-212-mode0-20260729-2002.DMP`
   (1 GB), minidump `C:\Windows\Minidump\072926-6359-01.dmp`, counter polls to
   t+41 s and the oracle slice under
   `tmp/handoff-0ab-b-lease/analysis/logs/D-2-VOID-bugcheck.*`. Intermittent
   (~1 in 10 GT1 runs); the same config re-ran clean before and after.
   3DMark logged `workload did not respond` for 11 s before the crash while
   the oracle still saw 390–490 flushes/s — the graphics pipeline was live,
   the workload's IPC was not. **This retires "treat the lease gate as the
   prime suspect" for the 2026-07-29 19:08 hard hang** — the class reproduces
   on the rollback build.
   **TRIAGED 2026-07-29 (no matching PDB — resolved via `.pdata` bounds + a
   487/506-byte match against the current build's `.map`)** →
   `tmp/handoff-0ab-b-lease/analysis/bugcheck-d1.md`. The faulting function is
   `AdapterContext::with_virtio` reached from `hpd_thread_routine →
   process_deferred_vidpn_source_address → … → arm_bind_refresh`; the
   dereference is the `Option<VirtioGpu>` discriminant test at `+0x80`
   (`locks.rs:263`), first touch after `virtio_lock`. The CONTEXT record shows
   `rsi/rdi/r13/rbx/r12` correct and **`r14` alone** holding
   `0x00000001'59b43050` where `&AdapterContext` belongs — corrupted ACROSS
   `KeAcquireSpinLockRaiseToDpc` (the consecutive `mov r14,rcx` /
   `lea rsi,[rcx+0xb28]` had a good `rcx`; the real `virtio_lock` reads held).
   Ruled out with dump evidence: torn reads, in-flight reuse, DMA recycling,
   stack overflow, pool corruption, enlightened-spinlock path
   (`HvlEnlightenments=0`), lease code. Since no x86-64 instruction writes
   only a GPR's high dword, `r14` was RESTORED from a damaged image:
   **H1** a VM-exit register round-trip (PLE exits during the contended spin,
   `ple_gap=128`; contention was climbing — ISR 521→943/s in the final 10 s) /
   **H2** a 4-byte `1` over a saved-`r14` stack slot in the DIRQL interrupt
   chain (our ISR is verifiably clean; the INTx line is shared with the
   balloon, 29th-session memory) / **H3** marginal host CPU / **H4** the
   `hv-*` enlightenment set changing exit paths. Per the host-proven-good
   rule none is promotable without host-side evidence.
   **Mitigations landing in 22.22.217.0 (build 1):** the `WvTorn` tripwire in
   `with_virtio` (converts a recurrence into a counter + graceful Err instead
   of a bugcheck) and PDB archiving next to every deployed `.sys`.
   **OWNER-GATED discriminating A/B (the actual fix path — host config only):**
   ≥20-run GT1 loops under (a) baseline, (b) `kvm_intel ple_gap=0`,
   (c) `hv-spinlocks`/`hv-avic`/`hv-evmcs` dropped, (d) balloon device removed
   (shares IRQ 22); watch `dmesg -w` + `/sys/kernel/debug/kvm/*/pause_exits`
   during the loops. (b)/(c) stopping it ⇒ H1/H4; surviving all ⇒ H2/H3.

0ad. **NEW 2026-07-29 (metric validation): the fullscreen→desktop transition
   sends one undersized `SET_SCANOUT_BLOB` and the host readback goes dark for
   ~250 ms.** In every trace, at the transition, res 6 arrives with
   `blob_size 4096000` (= 1280×800×4, the LINEAR size) against the OPTIMAL
   import's `required=4587520`; the vk-optimal path refuses the shape, the
   vk-linear import is rejected for all usages, and the egl-texture path
   refuses the modifier-less reinterpretation (`glEGLImageTargetTexture2DOES`
   0x502). `egl_scanout_dmabuf` had already deactivated the previous readback
   on entry, so NO readback exists until the next successful bind ~250 ms
   later. ⚠ The refusals themselves are correct — the undersize guard is
   Xid-31 protection and must NOT be relaxed (38th-session memory); the defect
   is guest-side: whatever binds res 6 at the transition presents a
   LINEAR-sized blob to a path that needs the padded OPTIMAL size, and the
   gap in coverage is user-visible as a transition blackout.

0z. **NEW, 2026-07-27 (R614 gate): the Mesa venus ICD does not survive adapter teardown —
   `pnputil /restart-device` ACCESS-VIOLATES every process holding a venus device.** Each
   restart-device cycle logs Application-log id 1000 faults with
   `Faulting module name: vulkan_virtio-<hash>.dll, Exception code: 0xc0000005` for dwm.exe plus
   3-4 shell processes (Explorer, SearchHost, StartMenuExperienceHost, ApplicationFrameHost,
   ShellExperienceHost). The desktop self-recovers — dwm restarts and composites, which is why
   this was never noticed — but "a new dwm pid is expected, not a crash" was too generous: it IS
   a crash, just a survivable one. The same cluster appears at every clean shutdown/reboot.
   **PRE-EXISTING, NOT caused by R614, and the evidence is unambiguous:** 889 such faults in the
   Application log spanning 2026-07-07 → 2026-07-27, and **T4a's own nine-consecutive-restart-device
   soak on 22.22.184.0 produced them on every cycle** (08:56:45, 08:56:58, 08:57:12, 08:57:25,
   08:57:39 … a ~13 s cadence matching the nine cycles).
   **Why every gate so far called that soak clean:** the gates check the SYSTEM log for
   4101/dxgkrnl/LiveKernelEvent, and those stay legitimately empty — a user-mode ICD access
   violation is not a TDR. Nothing checked the APPLICATION log. `tmp/r614-tdr-check.ps1` and
   `tmp/r614-icd-fault-history.ps1` now do; fold them into the standing gate.
   Next step is a stack: the deployed UMD/ICD PDBs are present, so
   `tools/take-minidump.ps1 -ProcessId <dwm>` under a restart-device, then
   `minidump-stackwalk` on Linux, should name the ICD site directly. Most likely shape is the ICD
   touching a venus object (or its ring/reply BAR mapping) after StopDevice destroyed the host
   context — i.e. the ICD has no adapter-loss path, which is the same class as the 17th-session
   DEVICE_LOST chain but on teardown rather than on a slow present.
   **2026-07-29 factorial campaign: reproduced 6/6** — every `pnputil /restart-device`
   crashed dwm/Explorer/SearchHost/StartMenuExperienceHost/ApplicationFrameHost in
   `vulkan_virtio-*.dll` (0xc0000005); desktop recovered within ~10 s each time
   (screenshot-verified before every run). restart-device is NOT a free operation.

0a. **Ghosting — ROOT-CAUSED AND FIXED IN LAYERS (21st session, 2026-07-06).** Owner
   insight proved out: the dirty-rect *attribution* was fine; STUTTER caused the
   ghosting. Evidence chain (all same-day): dwm's composed primary is paintcap-CLEAN
   under a bouncing-window probe while the LG client trails catastrophically → the
   corruption is in the IDD→KVMFR→client delivery; 1843 consecutive IddCx acquires
   show frame-delta exactly 1 (no skipped presents) and zero move-regions; WUDFHost
   consumer waits showed timeouts=0 while trails persisted → the wait "succeeded"
   against the WRONG instant. Root cause: the WS1 #4 consumer wait ran at cmdlist
   START (refreshHeliosStagedImages), re-reading the publish slot when the list
   BEGAN — under load that predates the acquire the list's copy serves by a full
   consumer cycle, so the wait targeted an already-retired value and the copy read
   ring-stale content inside freshly-reported damage rects. Fixes landed:
   - dxvk-helios `6eab004c`: bounded present-wait ON THE CS THREAD at copy-execution
     time (copyImage/copyImageToBuffer, imported sources). The acquired buffer can't
     be re-presented while held, so the slot value at that moment IS the acquired
     present's value. Silent no-slot returns (unordered reads) + fast-path hits now
     counted in the `present-wait:` line (`fast=`, `noslot=`).
   - dxvk-helios `35fe0912`: WUDFHost.exe app profile heliosPresentWaitUs=500000 —
     the copy-time wait exposed real producer lag (dwm fence frozen 250ms+ behind
     its publishes under churn; 32ms bound timed out exactly when ordering mattered).
     dwm keeps the tight 32ms default.
   - LGIdd `13630d7f`: D3D11 readback path (the ACTIVE path — D3D12 device creation
     fails 0x887A0004 by design, the D3D12 partial-copy path is DEAD code) finishes
     the acquired frame AFTER the staging Map (was: at CopyResource submit — dwm
     could re-render the buffer before the copy executed), reuses the staging
     texture (was: CreateTexture2D per frame = venus alloc/free per frame), adds
     per-stage QPC telemetry (`D3D11 path stats:` 1 line/300 frames), and a
     `HeliosForceFullDamage` registry knob (diagnostic; owner-verified to hide
     ghosting).
   - LGIdd `37356f83`: pending-damage debt — frames dropped after their dirty rects
     were consumed (LGMP queue full fired exactly at the login→desktop transition:
     giant permanent cold-boot ghosts, owner screenshot) bank their damage for the
     next delivered frame; new-subscriber re-post now carries full damage
     (damageRectsCount=0) instead of the last frame's partial rects.
   - LGIdd `5d36c512`: IddCx MOVE REGIONS delivered as damage (DestRect per move) —
     previously never queried, silently dropped (window drags are moves+edge-dirt;
     zero moves observed from programmatic MoveWindow, so this wasn't the trail
     cause, but it was a real hole).
   - LG client `b27eb1d0`: malformed frames (zero geometry) and transient
     onFrameFormat failures skip-and-retry instead of exiting — a client exit tears
     down the whole VM via the launcher (observed live: render-server killed dwm's
     venus context → torn frame → client exit → VM shutdown).
   Remaining for 60fps IDD (owner target): the serialized IDD cycle measured
   map(wait-inclusive)+memcpy ≈ 60-95ms under churn. `AllocCached` KMD fix (22.22.53)
   targets the 36ms WC-read memcpy; producer completion lag (dwm CS submission lag +
   a RECURRING ~1.49s stall, constant across configs — hunt with QMP fence tracing)
   is the other half. See WS2. **22nd session: the ~1.49s stall is ROOT-CAUSED AND
   FIXED (our own staged-probe diagnostics — see WS2); present-wait timeouts now 0.**

0b. **NEW DEFECT — venus pipeline-layout lookup failure killed dwm's context
   (2026-07-06, host-side evidence).** virgl render server:
   `vkr: failed to look up object 626238 of type 19 (pipeline layout)` →
   `vkCreateGraphicsPipelines resulted in CS error` → fatal decoder state → context
   1613 (dwm.exe) destroyed → all subsequent blob creates refused → LG client choked
   on the torn frame and exited → launcher shut the VM down. First host-side proof
   of a guest venus protocol violation.
   **22nd session (2026-07-06): the fork-early-out suspicion is FALSIFIED as the
   cause.** Static audit: vn_pipeline.c/vn_common.c are byte-identical to upstream
   (the relaxed tail load in wait_all is upstream's own); the fork's divergence is
   confined to vn_ring.c. Evidence: the never-read ring-diag file at
   `C:\Windows\Temp\helios_icd_diag.log` (13.5 MB across 6 boots) shows the
   shared-valid early-out NEVER fired and the wait_seqno fatal-abandon fired
   exactly twice — both AFTER the host had already latched ring-FATAL
   (consequences of the CS error, not causes; on the death boot the first
   guest-side anomaly IS the post-FATAL abandon, head frozen at 141452 with
   ~1.2 KB undecoded). The sync-vs-async structure is also sound: TLS-ring
   pipeline creates are `vn_call_` (synchronous), so destroy-after-create races
   are excluded. The initiating violation left NO guest-side trace. Landed
   (mesa `a0412461012`, ICD `vulkan_virtio-7c9ddf378055` deployed):
   `vn_ring_wait_all` barrier skips/abandons now log LOUD
   (`BARRIER SKIPPED/ABANDONED in wait_all`) to the ProgramData diag log —
   either line on a healthy process is the smoking gun; ring diag rerouted
   from `C:\Windows\Temp` (unwritable for WUDFHost — its ring diags silently
   vanished) to `C:\ProgramData\Helios\helios_icd_diag.log`; post-fatal
   per-submit spam rate-limited (power-of-two); NEW env knob
   `VN_HELIOS_PIPELINE_TRACE` traces (ring, primary_tail seqno, object id)
   for layout create/destroy + the barrier + pipeline creates. NEXT on
   recurrence: read the BARRIER/PL-TRACE lines before theorizing; host-side
   `HELIOS_VKR_DEBUG=validate` relaunch (ask owner) — NOT VIRGL_LOG_LEVEL
   (HOST.md §5.1: venus runs in the render-server child; only WARN+ reaches
   the qemu stderr log).

1. **Stall — ROOT-CAUSED AND FIXED (18th session, 2026-07-06).** The strike attribution
   (mesa `f7a816f182f`: sem id, wait reason, signal queue/family/ring, signal value+age in the
   strike line) cracked it in one boot: **every observed strike had `sig_age_ms ≤ 14`** — the
   waiter had legally parked a wait-before-signal on the NEXT frame's timeline value while the
   desktop idled; the moment the next frame's burst submitted the signal, the old deadline
   (which measured time-since-counter-movement, gated only on a-signal-is-pending-NOW) fired
   against a milliseconds-old signal. Explorer striking at exactly hh:mm:00 = the taskbar clock
   tick submitting that signal. The strikes were false positives BY CONSTRUCTION on an idle
   desktop, and 4 of them during login churn are what tripped the DEVICE_LOST latch (act one
   of the 2026-07-05 freeze cascade). FIX: the deadline now measures how long a submitted
   signal has been pending with zero movement (`pending_signal_since_ns`); a genuine zombie
   (Xid-109: pending forever) still latches at strikes×deadline. RESULT: strikes went from
   ~1/min/process to ZERO across dwm+explorer+WUDFHost (fixed ICD, ~10 min incl. idle).
   The second stall contributor — the rotate drain — is fixed under WS2 (presents now track
   damage rate: ~10/s under the 10 Hz flasher vs ~6/s before). 19th-session soak data
   point (2026-07-06, warm boot): ZERO non-forced strikes and zero DEVICE_LOST over the
   whole session (~1 h incl. heavy GPU probe workloads); present-gate timeouts flat at the
   startup value; rotate-perf 1–4 µs; HELIOS_QUEUE_PERF live in dwm, submit phases all
   µs-class (submit_avg ~4 µs). Remaining: multi-hour/day soak.
2. **dxvk-helios device-loss hygiene — FIXED and FORCED-LOSS-VERIFIED (19th session,
   2026-07-06):** (a) dropped post-loss submissions now `notifyObjects()` + recycle (was:
   permanent in-use ref leak → dwm compositor wedge); (b) `waitForResource` bails on
   DEVICE_LOST (loud warn); (c) on lost, bounded 2 s grace wait then deliberate cmdlist
   leak instead of resetting a pool with host-pending buffers (was: the vkResetCommandPool
   VUs). End-to-end forced-loss test PASSED (`VN_HELIOS_SEM_DEADLINE_MS=1` +
   `_STRIKES=1` on d3d11_upload_integrity_probe): latch fired on the first genuine 1 ms
   pending window with full attribution, probe ran to completion with loud FAILs (reads
   return zeros post-loss) and exited — no wedge; all three hygiene paths logged in the
   dxvk log (`leaking command list with unretired host work`, `waitForResource aborted`,
   repeated loud submit failures); dwm untouched. Note: once the ICD latches, host
   retirement becomes unobservable, so the deliberate leak fires even on a healthy host —
   by design.
3. **dwm shared-resource creation failure — FALSIFIED as written (18th session, 2026-07-05)**:
   every `DXVK memory not importable` line (current boot AND the freeze-evidence tail) belongs
   to `misc=0x0` PRIVATE textures, where suballocation is correct. Shared/present creations
   already get dedicated importable memory (`forceDedicated`, dxvk_image.cpp:634): dwm's
   backbuffers allocate `kind=DEVICE_MEMORY` blobs (blob=venus mem id, KMD creates the wire
   resource) and the IDD imports them (resids 32/33/36, live content probe-verified). The
   `res_id=0` in the allocate log is normal for KMD-created blobs, not a failure. Log line now
   scoped: loud `SHARED RESOURCE WITHOUT IMPORTABLE BACKING` only when the resource actually
   needs one (shared/keyedmutex/present/primary); private-texture noise trace-gated. The IDD's
   per-acquire re-resolve is the alias-staging refresh design, not a missing-export symptom.
   Residual noise, harmless: dwm warns `Failed to write shared resource info` 9× (dxvk shared
   metadata runs before the UMD stamps KMT handles; WINE-escape fallback fails by design).
4. **KMD wire-fence semantics / consumer-side present ordering — MECHANISM PROVEN
   END-TO-END (19th session, 2026-07-06).** `tools/vk_ring_fence_probe.cpp` (schtasks
   `helios_ringprobe`, MEDIUM integrity — see tooling gotcha) demonstrates the full chain
   on the live stack: vkQueueSubmit2 signaling an exported win32 timeline semaphore →
   vn ring_idx≥1 fence-only SUBMIT_VENUS (`vkWaitRingSeqnoMESA` cs, orders the fence
   behind the ring-decoded queue submission) → KMD INFO_RING_IDX wire fence (the transport
   already honored ring_idx; in-flight table is per-fence token-matched, out-of-order-safe)
   → QEMU 11.0.1 → proxy → render-server vkr queue sync thread → host GPU completion
   (qemu fence trace: retire at 263 ms on a 269 ms workload; ring-0 retires in µs at
   decode) → used-ring completion → **new ICD retire thread** (mesa `4a6aa14f17b`: on
   every SHARED-sync signal, a per-renderer thread WAIT_FENCEs the wire fence and signals
   the shared WDDM monitored fence in retire order; helios_sync now refcounted; also fixed
   the `helios_sync_append_locked(fence_id=0)` blind-signal that cleared older pendings)
   → consumer `vkWaitSemaphores` on the re-imported semaphore returns at 97–98 % of
   T_gpu, never early. KMD `22.22.52` (BUILT, NOT yet installed — needs owner reboot):
   WDDM pending-FIFO watermark now counts ring-0 fences only (ring≥1 stay in flight for
   the whole GPU-work duration; counting them would couple GDI/paging DMA pacing to
   multi-ms GPU work) + RING_SUBMIT/RING_COMPLETE counters (TDR report v4).
   **PRODUCTION INTEGRATION DEPLOYED (20th session, 2026-07-06) — A/B running with
   `PresentGateUs=0`.** The design changed twice against reality:
   - **KMT is impossible**: dxgkrnl rejects a MONITORED fence with `Shared=1` and no
     `NtSecuritySharing` (0xc000000d, proven live), i.e. no global-DWORD flavor exists.
     The ICD's legacy-D3DDDI_FENCE fallback silently produced syncs whose FromCpu waits
     hang forever (wedged the probe) — fallback REMOVED, KMT semaphore caps no longer
     advertised (mesa `d5d698aaec5`). Rendezvous = **NAMED NT sharing**: standard
     `VkExportSemaphoreWin32HandleInfoKHR::name` / import-by-name
     (`D3DKMTShareObjects` w/ named OBJECT_ATTRIBUTES → `D3DKMTOpenSyncObjectNtHandleFromName`).
     dwm CAN create `Global\` names (SeCreateGlobalPrivilege verified on its token).
     `vk_ring_fence_probe named` rc=0 (98 % of T_gpu; NT regression 97 %).
   - **LGIdd needed NO changes**: its per-acquire copy runs on OUR UMD/dxvk inside
     WUDFHost (that instance imports dwm's backbuffers as alias images — resids
     probe-verified) — the consumer wait lives in dxvk-helios's
     `refreshHeliosStagedImages`, `dxvk.heliosPresentWaitUs` (default 0, WUDFHost.exe
     profile = 100000; imported-mode images only, so dwm never grows a CS-thread wait).
   Shipped shape: producer (`present_sync_publish`, umd `9d22620`+`a7f2dfa`, dxvk
   `b15bc42f`+`8c612b3d`) creates one named fence per D3D11 device
   (`Global\HeliosPresentFence_<pid>_<fenceId>` — dwm owns SEVERAL devices, per-pid
   names collided live; Everyone-DACL), records signalFence(++counter) on the frame's
   OPEN cmdlist (no present-thread wait), publishes (resid → pid, fenceId, value) in a
   HPS2 seqlock-slotted mapped FILE `C:\ProgramData\Helios\helios_present_sync_v2.bin`
   (4096 fixed 32-byte slots; 131104 bytes total; both
   principals have ProgramData rights; do NOT delete the live file — mapped views
   split-brain until the mappers restart). Consumer imports by name (cached per
   pid+fenceId), `getValue` fast-path, bounded wait, timeout → copy anyway + loud
   `present-wait:` telemetry. Kill switches: `HKLM\SOFTWARE\Helios!PresentSyncPublish=0`
   (producer), `dxvk.heliosPresentWaitUs=0` (consumer). Deployed: UMD
   `helios_umd_8301bc9779a48b99.dll`, ICD `vulkan_virtio-cf1280da750c.dll`, verified in
   dwm+WUDFHost; cross-session import proven live ("imported fence of producer pid").
   First numbers (gate=0, 10 Hz flasher stress): ~900 consumer waits, avg 5–6 ms,
   ~4 % bounded timeouts in bursts when dwm's CS runs >100 ms behind (structural:
   the consumer chases the latest published value; bounded + loud by design), ZERO
   strikes, desktop paintcap-clean, submit_avg ~4 µs unchanged.
   **Owner cold-boot report (same session): ghosting + frame drops PERSISTED — the
   WUDFHost-only scoping was WRONG.** The old gate had been ordering EVERY producer's
   present (apps included), so registry gate=0 also unordered the app-backbuffer →
   dwm-composition edge; the artifacts were in dwm's OWN composed primary (stale
   sliver + content overhang in the owner's screenshot). Fix (dxvk `90b76c5c`, UMD
   `helios_umd_f2c6f833d7d293eb.dll`): `heliosPresentWaitUs` defaults to **32000 for
   every consumer** — the wait fires only for imported surfaces with a published
   slot (dwm/IDD cross-process edges), the wait DAG is acyclic (IDD → dwm → apps),
   every edge bounded; uniform 32 ms also caps the IDD stall bursts that read as
   frame drops (was 100 ms). Verified live: dwm imports app producer fences
   ("imported fence 1 of producer pid <app>"), zero dwm-side timeouts, churn
   paintcaps coherent. Timeout warns now log the fence's current value — churn
   bursts are retire-lag (publish outruns GPU completion by a few frames).
   **Remaining:** owner eyeball via Looking Glass + multi-hour soak with gate=0;
   then retire `PresentGateUs` (flip compiled default to 0) and drop the gate from
   the hot path. Differential levers if ghosting recurs: `DXVK_CONFIG
   "dxvk.heliosPresentWaitUs = N"` per process, or registry `PresentGateUs=32000`
   restore. Known residue: old-ICD probe runs leaked 4 idle render-server workers
   host-side (virgl-63/65/95/97); clears on VM restart, did not recur with the new ICD.
5. **dxgkrnl "Driver returned an invalid NTSTATUS 0xC00000BB"** (ETW
   AzureTriage) — some query answered STATUS_NOT_SUPPORTED where that return
   is illegal. Tolerated today; find and fix the query.
6. **WUDFRd cold-boot race** ("SCM not ready", boot+23s) — LGIdd loads late;
   pairing is resilient now but the race window is still there.
7. **In-place KMD update flakiness** — CM_PROB_FAILED_POST_START limbo until
   reboot is expected, but keep the version-coherence gotcha (one site since
   2026-07-26: `kmd_render/driver-version.env`) and
   backup ladder in mind. 2026-07-06 state (19th session END, reboot-verified): ACTIVE
   driver = **oem59.inf = 22.22.52 (pkg 155b7345f9360525)** — per-ring watermark +
   counters live. `UserModeDriverName` → ProgramData `helios_umd_b3615be0ce9de13e.dll`
   (RELEASE profile), dwm ICD = **`vulkan_virtio-5535366186bd.dll`** (retire thread) —
   both verified by the fresh dwm's loaded-module list + paintcap. **NEW GOTCHA: a KMD
   `devcon update` install creates a new DriverStore dir and RESETS `UserModeDriverName`
   to the DriverStore copy, which ships the package's DEBUG-profile UMD — after EVERY
   KMD install, rerun `win_install_umd` (release dll + `-KillUmdUsers -RestartDevice
   -NoProbe`) and re-verify dwm's loaded module. The DriverStore UMD copy staying locked
   during that redeploy (script exit 1) is benign — ProgramData + registry are what
   load.** Backup: `C:\ProgramData\HeliosDeployBackups\20260706-021734`.
8. **GDI byte-path / executor retirement (REFRAMED 20th session)**: post-cold-boot,
   GDI content renders while RenderGdi (GdiE), MapCpuHostAperture (ChMn) and
   paging (Pg*) counters all stay idle — the canonical CPU path likely already
   carries the bytes. RESEARCH CONFIRMED (2026-07-06): GDI HW acceleration is
   OPTIONAL (`SupportKernelModeCommandBuffer` is a "should… only if" per
   gdi-hardware-acceleration.md); **viogpu3d** (vendored kvm-guest-drivers, the
   closest existing WDDM driver to Helios, near-identical cap set) never sets the
   bit and implements NO RenderKm/RenderGdi; WARP works render-only in the IDD
   slot. The in-tree "LOAD-MANDATORY" bisect (query_adapter_info.rs:166,
   2026-07-02 v22.22.34) predates Option A and is confounded by the then-broken
   CPU-visible story — retest under BarSegMode 10 via a new `GdiAccelMode`
   service-key knob + pnputil restart-device (AzureTriage names any FAILED_ADD).
   If the desktop renders accel-off: verify byte flow (GdXn stays 0, watch for a
   two-memory-split regression = black/stale GDI windows), verify the BAR CPU
   mapping is WB-cacheable (CPU GDI is read-modify-write), re-run the Doom
   stutter differential (its WSI BitBlt storm currently traverses the executor),
   then retire the gdi_blit executor (a guest-CPU blitter behind a DMA round
   trip — strictly worse than win32k's rasterizer, source of the 48%-drop bug
   class).
   **21st session (2026-07-06): both knobs SHIPPED in 22.22.53** (`85ad16a`):
   `GdiAccelMode` (default 1) gates SupportKernelModeCommandBuffer per the plan
   above; `AllocCached` (default 1) additionally answers the WB-cacheable
   question for ALL CpuVisible allocations — user views were mapped WC (only
   CpuVisible was set at create_allocation; the WDDM2 `Cached` flag was never
   set), measured at ~200 MB/s reads = 36 ms per 7.8 MiB IDD readback frame.
   **22nd session: GdiAccelMode=0 A/B PASSED — the 2026-07-02 "LOAD-MANDATORY"
   bisect is OVERTURNED under BarSegMode 10.** Evidence (all same-boot, via
   `pnputil /restart-device`): adapter re-adds `CM_PROB_NONE`; desktop composes;
   classic-GDI text renders (fresh cmd-console canary echoes + regedit tree text
   + desktop-listview labels, paintcap-verified); every Gd* counter FROZEN this
   boot (GdiE 339, GdXn 24 — zero executor traffic; the KMD's `GdiM` mirror
   flipped 1→0 proving the mode took); no two-memory-split regression under
   mover churn across three device restarts. **The knob is LIVE at 0 for soak**
   (service-key value set; revert = `reg delete ...\helios_kmd_render /v
   GdiAccelMode /f` + restart-device). NEXT: owner-attended Doom stutter
   differential (its WSI BitBlt storm no longer traverses the executor), then
   retire the gdi_blit executor + flip the compiled default.
   **CLOSED 2026-07-26 (T1b, 22.22.180.0)** — owner directive: the driver does
   not and should not advertise GDI acceleration, so both the advertisement and
   the executor are gone (R903/x-dup-dead-20, pulled forward from T6).
   Reachability was re-proven the same boot before deleting anything: every
   `Gd*` service value deleted, then explorer restart + maximized notepad + GDI
   canary + repaint + EnumWindows + two paintcaps, and **not one `Gd*` value
   reappeared** (the executor flushes its block on its first batch, so absence
   is proof, not throttling). `SupportKernelModeCommandBuffer` is now hard-coded
   0; `DxgkDdiRenderGdi`/`RenderKm` stay registered as record-and-advance
   (a null slot bugchecks — `DdiRenderGdi+0x140`). This also deletes T1b's
   R301/R304/R305/R306, which only hardened the deleted executor.

## Workstream 2 — Performance

- **THE PRESENT BLOCK IS ATTRIBUTED AND HALVED (2026-08-04/05, KMD
  22.22.243.0 → 22.22.244.0).** `umd_present_callback` (548–661 µs/frame, the
  single largest ours-attributable cost on the app's render thread) is not CPU
  and not our `DxgkDdiPresent` (7.9 µs mean): it is **one dxgkrnl wait**.
  A `Microsoft-Windows-DxgKrnl` ETW slice names it — `BlockThread` `Reason=2`
  on 21.1 % of presents, mean 2448 µs, **516 µs amortised = the whole callback**
  — and pins the mechanism exactly: **89 of 91 blocks began with exactly 3
  `PresentQueuePacket`s outstanding** (non-blocked presents saw 0/1/2) and
  **90 of 91 unblocks landed within 200 µs of a `PresentQueuePacket Stop`**
  (median 12.1 µs). dxgkrnl allows three outstanding present packets; the
  fourth present blocks until one retires.
  **Why the queue filled was OUR defect:** `note_wddm_submission` gated every
  non-paging WDDM fence on `async_retired_up_to(next_wire_fence, IncludingGpu)`
  — *every transport entry enqueued before the buffer* — although each
  submission already carries its exact dependency (`stream_ready` on its live
  present stream boundary, present on 95.8 % of them: `PmHit` 12452 /
  `PmLeg` 13003). The DXVK CS thread runs ahead of the presenting thread, so
  the superset routinely covered LATER frames' work. `arm_dma_flip`'s 0ab-B
  note recorded the same over-wait from the flush side in 22.22.210.0 and fixed
  it there; the fence path kept it.
  **Fix: `PresentWmk` (service key, default 1 since 22.22.244.0; `0` is the
  same-boot A/B disable).** A submission carrying a LIVE boundary is gated on
  that boundary alone. FIFO order, monotonic fence completion, the
  stale-generation cancellation path, paging, WindowedBlt admission and every
  scanout bind/flush ordering rule are untouched.
  **Measured** (same boot, `pnputil /restart-device` between cells):
  `DxgkDdiSubmitCommand`→DMA_COMPLETED 5.825→4.854 ms mean, 6.110→3.995 ms p50;
  flip packet lifetime 8.594→7.398 ms; `umd_present_callback` 626–661→359 µs;
  presents/run 6793–6892→7184. **GT1 +4.30 % / +3.74 % paired**, and on the
  canonical STANDARD preset **Graphics 45 365 / 46 500 → 47 875 (median
  45 933 → 49 405, +7.6 %)**, GT2 carrying most of it (183.8/190.2 → 196.7–211.7).
  Host-GPU envelope barely moves (mean 59.8→60.7 %, p50 64→68 %): the extra
  frames come out of the guest, and the host still has headroom.
  **The residual is now NAMED, not guessed:** `.244` adds `WfBWire` /
  `WfBStrm` / `WfBBlt` (exactly one moves per blocked look at the WDDM FIFO
  head). One GT1 gives `WfBStrm=15482`, `WfBWire=93`, `WfBBlt=0` — the
  over-wait is gone (0.6 %), and what paces retirement is `stream_ready`, i.e.
  the frame's own producer completion on the host. That is physics. Its shape
  is bursty: flip packets retire in **106.6 bursts/s, mean size 2.20,
  inter-burst gap p50 10.06 ms**, so the app issues three presents and waits
  for the next burst — **that burst cadence is the next question.**
  Frame budget under the fix: **producer 3.716 ms (p10 3.273) + kernel
  0.571 ms = 4.288 ms**; even a perfect kernel leaves ~269 fps.
  Artifacts: `tmp/perf/present-watermark-arm/` (prediction + outcome),
  `tmp/perf/flip-queue-arm/` (the REJECTED `MaxQueuedFlipOnVSync` arm and the
  ETW that replaced it), `tmp/perf/etw-slice-242/`, `tmp/perf/etw-slice-pwmk1-243/`,
  `tmp/perf/fs-std-244/`. Reusable: `tmp/perf/run-gt1-arm.ps1` +
  `launch-gt1-arm.ps1` (feed trace + counters + read ledger + scanout timeline
  around one GT1, `-Extra NAME=VALUE` for env-knob arms),
  `tmp/perf/ab-presentwmk.ps1` / `ab-env.ps1` (interleaved A/B — GT1 drifts
  across a session, so all-A-then-all-B cannot separate knob from drift),
  `tmp/perf/sample-host-gpu.sh` (host-side GPU envelope).
  **REJECTED, do not retry:** `MaxQueuedFlipOnVSync` (the `FlipQueueN` knob,
  default 1) at depth 4 with and without `FlipOnVSyncWithNoWait` — inert on
  both fps and present-callback time; and
  `HELIOS_DXVK_LOCAL_ALLOC_CACHE_FALLBACK=1` **re-tested under the fix**
  (+1.41 / −0.60 / −0.07 % paired), which also kills the "the present block was
  absorbing its CPU saving" hypothesis.

- **THE FRAME IS ATTRIBUTED (2026-08-02, 65th session, measurement-only):
  GT1 is guest render-thread bound; the host GPU idles at ~35 % busy
  (~200 W of 400 W) and NOTHING else in the pipeline is saturated** —
  venus decode thread 36 % of a core (decode-saturation KILLED), QEMU main
  loop 13 %, dxvk-queue/cs/submit 38/24/6 %, no vCPU over ~70 %. The app's
  render thread runs 88–93 % and RIP+stack sampling (no build; PDB-symbolized)
  decomposes it: ~8 % COM refcount churn (samplers alone 5.6 % — R8 is real),
  ~6.5 % DXVK CS chunk-pool mutex contention (`AllocCsChunk` parks),
  ~5.4 % DxvkMemoryAllocator on the Map-DISCARD path, ~5.1 % heap, 5.6 %
  d3d11-runtime device-critsec contention under `CUseCountedObject::Release`
  (named via MS public PDBs — msdl is curl-reachable, recipe in the report),
  3.6 % frame gate (contract), 1.2 % log.rs with tracing off. Wire path is µs-class (submit 40 µs,
  escape ~100 µs, retire 0.75 ms). Full report + ranked levers:
  `tmp/handoff-perf-saturation/reports/p1-attribution.md`; raw artifacts in
  `tmp/handoff-perf-saturation/logs/`; the samplers (reusable) in
  `tmp/handoff-perf-saturation/tools/`.
  **Lever outcomes (2026-08-03, same session; canonical config =
  owner-picked STANDARD FS preset, task `helios_fs_std`, baseline
  GT1 170.6 / GT2 202.2 / Combined 21.38 ⇒ Graphics 42.6k):**
  LEVER 1 LANDED (`e610d1c` — COM churn out of the binding DDIs; refcount
  samples 8 %→1.5 %; Combined 21.4→≥22.9 on every subsequent run, best
  25.26; GT1 likely +3–6 % but GT1 single runs swing ±5 % on identical
  code — use 3-run medians; all stability gates + GT2 oracle green).
  LEVER 2 NEGATIVE, reverted: the "chunk-pool contention" was a
  stack-scan artifact — those parks are the FRAME GATE's
  `SynchronizeCsThread` CS-drain; corrected gate cost ≈9.6 % of the
  render thread (~0.55 ms/frame; DXVK's `CsSyncCount`/`CsSyncTicks`
  already instrument it). LEVER 3 reverted: two real discoveries — the
  per-context `DxvkLocalAllocationCache` mask is ZERO on venus (no
  DEVICE_LOCAL+HOST_VISIBLE+HOST_COHERENT global-buffer type), and GT1
  runs ~11 M small (48–128 B) buffer allocations through the locked
  allocator per run (~950/frame, likely with a failed-properties attempt
  + fallback each); enabling the cache (mask sans DEVICE_LOCAL → 99.7 %
  hit rate) coincided with GT2 202→193–195, confounded by late-session
  drift — unproven either way, do not re-land without answering why a
  live cache would cost GT2 through venus. Run table + honest noise
  discussion: report §3c.

- **HISTORICAL OPEN MEASUREMENT (2026-07-26, found while taking the T0 baseline):
  DComp producer cadence ~50 fps, not the then-documented ~63; present-gate avg
  ~2.0 ms, not the then-documented ~0.48 ms.** Measured on an
  idle box (CPU ~1 %) with `helios_dcomp_probe` (25 s runs): 1236 / 1152 / 1253 frames
  **before** the T0 deploy and 1227 / 1307 **after**, i.e. 46–52 fps throughout — so this
  is NOT a T0 effect and not a debug-vs-release-UMD effect. An earlier stored run of the
  same probe on this box recorded 1576 frames (63.0 fps), which is where the documented
  figure comes from. dwm `present-gate:` reads avg 2018 µs / max 14595 µs / 30 timeouts in
  3072 presents on the release UMD. This count is a producer-side DComp observation,
  not proof of a visible fullscreen scanout defect. The 2026-08-04 owner correction
  verifies native SDL smooth and retracts the prior SDL hold/burst report; VNC cadence
  must be measured separately. Revisit this probe only when targeting DWM/desktop
  producer cadence, and correlate it through presentation before assigning blame.
  Repeatable baseline procedure: two `helios_dcomp_probe` runs, then read the last
  `present-gate:` line from the live dwm UMD log.
- **IDD delivered-frame cycle — MEASURED (21st session, `D3D11 path stats:` line,
  1/300 frames in the LGIdd log):** the swapchain thread is fully serialized, so
  stage sums ARE the delivered fps. Under a 50Hz bouncing-window probe:
  rects ~14µs | staging ~0 (reuse landed; was a venus CreateTexture2D per frame) |
  copy-submit ~25µs | map 13-59ms avg with a RECURRING ~1.5s max (suspiciously
  constant across configs/builds — a fixed timeout somewhere; hunt with QMP fence
  tracing `virtio_gpu_fence_ctrl/resp` during a mover run) | memcpy 34-38ms
  (7.8 MiB at ~200 MB/s = WC reads) | lgmp ~0. Owner target: 60fps IDD.
  **RESOLVED IN TWO STEPS (same session):** (1) `AllocCached` (22.22.53) did NOT
  move the stage — the ICD maps venus blobs through its own escape path using the
  host's per-blob map_info (the WDDM2 Cached flag only affects dxgkrnl-owned
  mappings); a one-shot BW probe (now permanent in LGIdd) split the sides:
  src strided-read 622 MB/s, memcpy 157 MB/s, IVSHMEM memset 27 GB/s — the READS
  of the WC venus mapping were the whole cost. (2) LGIdd `1d03b685`: MOVNTDQA
  streaming loads (CopyFromWC) → probe streamcpy 5.5 GB/s, frame memcpy stage
  36.5ms → **1.4ms**, delivered rate 14-18fps → **32fps = the probe's damage
  rate** (map now ~8ms avg; pipeline headroom ~100fps — a 60Hz producer should
  see 60).
- **The RECURRING ~1.5s stall — ROOT-CAUSED AND FIXED (22nd session,
  2026-07-06, dxvk-helios `bdbbc2ea`): it was OUR OWN staged-content-probe
  diagnostics.** QMP fence tracing (`virtio_gpu_fence_ctrl/resp` during a mover
  run) split it in one pass: host ctrl→resp ≤ 11.5 ms across 7891 fences (p50
  0.17 ms — host exonerated again), but ALL guest submissions went SILENT in
  ~1.49 s beats, 3 per cluster, every 600 IddCx frames, landing at tick
  600k+30 = the probe HARVEST tick (`HeliosProbeRecurTick=600`,
  `HeliosProbeHarvestTick=30` in refreshHeliosStagedImages). The harvest
  scanned the ~7.8 MiB probe readback BYTE-WISE through the WC venus mapping
  ON THE CS THREAD (~0.75 s per probe at WC single-byte rates × 2 probes per
  staged image × 3 imported dwm backbuffers = 3 beats/cluster), while WUDFHost
  held the acquired IddCx frame — starving dwm and every producer behind it.
  Corroboration: probe issue/result log lines at ticks 2400/3000/3600 match
  the stall frames exactly; the 48-probe cap matched the observed stall
  cutoff (last cluster at tick 3600, none after); this also explains the
  "constant across configs" property (the probes shipped in every build), the
  LGIdd map-max 1.47-1.54 s, the "dwm fence frozen ≥500 ms" producer-lag
  signature, AND all 10 consumer present-wait timeouts (fence exactly one
  behind = dwm's next signal parked behind the block). FIX:
  `dxvk.heliosStagedProbes` config knob, default OFF (re-enable per process
  via DXVK_CONFIG for black-surface triage); harvest now bulk-copies WC →
  cacheable before scanning (~50 ms, not ~1.5 s, if ever re-enabled).
  VERIFIED live (mover churn re-trace, UMD `helios_umd_81a033e237bef769.dll`):
  max silence 48.5 ms (was 1493 ms), submission rate flat through every
  600-frame boundary, `present-wait: timeouts=0`, desktop paintcap-clean.
- **Producer (dwm) completion lag:** publishes outrun the fence by 1-4+ presents
  under churn; vkQueueSubmit2 phases are µs-class (queue_perf), so any residual
  lag is dxvk-CS-thread backlog + venus decode/retire path, NOT the submit
  call. The dominant "frozen fence" signature was the staged-probe stall
  (above, fixed). RE-MEASURE under 60Hz content before more work here;
  if residue: instrument dwm CS latency (record→execute) and the ICD retire
  thread's WAIT_FENCE throughput; consider moving dwm's own consumer waits from
  list-start to copy/sample-time like the IDD fix.

- **Per-present full-GPU drain — FIXED (18th session, `3579ef7` + `ef6689b`):**
  `rotate_resource_backings` drained the whole device (event query + `Sleep(1)` spin —
  the timer quantization WAS the 15–25 ms) on dwm's present thread every present. Now a
  CS-side identity rotation mirroring upstream `D3D11SwapChain::RotateBackBuffers` via
  `InjectCsOrderedAfterPending` (dispatch open chunk + ordered inject, NO CS-thread wait —
  the first iteration's `SynchronizeCsThread` blocked up to **1.9 s/present** behind login-
  churn CS backlogs = the post-cold-boot "occasional framerate dips"). `rotate-perf` after:
  **1–4 µs/rotation**; presents track damage rate (~10/s under the 10 Hz flasher, was ~6/s).
  WARNING for future work: do NOT per-image `waitForResource` on the present thread — a
  bound backbuffer RTV is re-recorded into every new open cmdlist, so `isInUse` never
  clears and dwm wedges (proven with a live minidump).
- **Ghosting after the drain removal — GATED (18th session cont., `4f0a96c`):** the drain
  had been accidentally serializing dwm's GPU work against the IddCx consumer's per-acquire
  copy; with it gone, the copy occasionally read a just-presented buffer whose GPU writes
  were in flight (dwm's venus rendering produces NO dxgkrnl-visible DMA fences — nothing
  else orders the cross-process read). Fix: bounded frame-completion gate in the DXGI
  present DDI (`HeliosWaitFrameComplete`: flush, then poll the submission fence — signals
  at GPU completion) before `pfnPresentCb` makes the flip visible.
  `HKLM\SOFTWARE\Helios!PresentGateUs` (default 32000; 0 = off, the A/B lever). Measured:
  avg 3.6–5.3 ms, timeouts only during startup churn, present rate unaffected
  (`present-gate:` telemetry, 1 line/128). The ARCHITECTURAL fix stays WS1 #4: per-ring
  wire fences + cross-process win32-sync so the consumer waits GPU-side.
- **Release-profile UMD is the deploy default (18th session `32cf4a4`; made the actual
  default 2026-07-26, T0/R102):** dev profile is opt-level 1; release is opt 2 + thin LTO,
  with `debug="full"` keeping the GUID-matched PDB for minidump symbolization. Both
  `tools\install-helios-kmd.ps1` and `tools\hotplug-helios-umd.ps1` now default `-UmdDll`
  to `umd\target\release\helios_umd.dll`, and the install plan prints `UmdProfile` next to
  `UmdSource` so the deployed profile is a line in the log. Until R102 the defaults were
  `...\target\debug\...`, so a default `win_install_kmd` signed the DEBUG DLL into the
  DriverStore while reporting success — every cadence and wake-latency number taken that
  way measured the wrong binary. Build with `win_cargo crate_dir:"umd" args:["build","--release"]`;
  pass `-UmdDll ...\target\debug\helios_umd.dll` for a deliberate debug deploy. A missing
  release DLL now aborts the install (`UMD DLL not found`) instead of quietly shipping debug.
  dxvk-helios stays meson debugoptimized (-O2 already).
- **Frame-update slowness** (owner-visible): remaining suspects after the drain fix =
  dxvk-helios persistent-refresh (14th session, alias-image staging + per-frame refresh).
  Quantify with `HELIOS_QUEUE_PERF` (machine env set 18th session; reaches dwm after the
  next reboot; one aggregate line per 300 submits to
  `C:\ProgramData\Helios\helios_queue_perf.log`).
- **Diagnostics overhead — GATED 2026-07-05** (default quiet; knobs restore):
  UMD per-op log I/O was open/write/close per line on per-frame paths → now a
  persistent handle + `trace_line!` behind `HKLM\SOFTWARE\Helios!UmdTrace`
  (e88f2c6; umd log 8612→681 lines/30 s under churn). ICD submit/shmem trace
  (33 MB+1.9 MB per session, fopen per submission) behind `HELIOS_SUBMIT_TRACE`
  env (mesa 4bb43194e5d). KMD: S-ring `diag::record` off + gdi_blit's 20-value
  per-batch registry dump deferred to every 64th batch behind service-key
  `DiagLevel` (bf0ab37, **22.22.51 ACTIVE since the 2026-07-05 reboot**, pkg
  c393e58c1b189688 / oem58). Also cleared the stale
  `HKLM\SOFTWARE\Helios!RotateSample=16` debug knob (was CPU-reading back the
  whole swapchain ring every 16 rotations). GOTCHA found while measuring: pids
  get reused and `umd-<pid>.log` appends — delete logs before comparing runs.
  Present-rate deltas across dwm restarts are confounded by IDD-pairing state;
  compare within one dwm session only. 18th-session regression found+fixed
  (`a6780f1`): the persistent Rust log handle made the bridge's `fopen_s`
  (deny-sharing) fail on every call — ALL `[dxvk-bridge]` lines incl.
  rotate-perf were silently dead in post-e88f2c6 builds; bridge now uses
  `_fsopen(_SH_DENYNO)`. If a telemetry stream goes quiet, check for a
  string-in-binary vs lines-in-log mismatch before trusting it.
- **Doom present path — async WSI worker LANDED (22nd session cont.,
  mesa `808c7e4a786`, ICD `vulkan_virtio-1b44e8d36fe4` deployed): 120 → 160 fps.**
  The sw WSI present serialized the frame-fence wait (5-6 ms: GPU frame +
  venus retire) + StretchDIBits (0.65-0.75 ms) on Doom's present thread
  (`helios-doom-wsi-perf.txt` wait_avg/stretch_avg = the 120 fps ceiling).
  Now: per-swapchain worker thread does wait+invalidate+blit; acquire's
  IDLE condvar is the back-pressure (run-ahead = image_count-1); kill
  switch `HELIOS_WSI_ASYNC_PRESENT=0`. vkcube 6,600 fps; worker wait_avg
  4.3 ms now overlaps the app's next frame. **CRASH POST-MORTEM in the same
  arc: an unconditional 5-image sw-swapchain bump crashed Doom at renderer
  init ×2 (idTech sizes per-image arrays to the REQUESTED count — unhandled
  C++ FatalError, Crash.00003/00004). Spec-legal ≠ app-safe; extra depth is
  now opt-in `HELIOS_WSI_EXTRA_IMAGES=N` (default 0), for engines that
  re-query.** OWNER DECISION (2026-07-06): the sw present path is FROZEN at
  160 fps; the next present work is HW-accelerated present. The async sw
  worker remains the fallback path + kill switches. Host-side during Doom:
  p50 fence 0.05 ms; a tight ~10.7 ms class at 53/s = ring≥1 GPU-completion
  fences seeing the pipelined queue backlog (healthy).
  **D3DKMTPresent FEASIBILITY RESEARCH (same session, 26100 SDK d3dkmthk.h +
  live-counter evidence):**
  - `D3DKMT_PRESENT` itself is fully documented (hContext — gate5a already
    owns one — hWindow, hSource allocation, Flags, INLINE PresentHistoryToken
    built by the CALLER). The wall is the TOKEN: every redirected model
    (GDI/GDI_SYSMEM/BLT/FLIP/COMPOSITION) requires `hLogicalSurface`/
    `hPhysicalSurface` (win32k logical-surface handles) or dxgi-private
    rendezvous state (`dxgContext`, `hCompSurf`, `confirmationCookie`,
    `PresentLimitSemaphoreId`) minted only by the D3D runtimes through
    win32k-private (win32u NtGdiDdDDI*) calls. NO documented path lets an
    ICD mint a valid redirected token.
  - **Blt-model is DEAD on 26100 regardless**: the KMD's DxgkDdiPresent
    feasibility trace (display.rs PBcall/PBsrc/PBdst, present since .34-era)
    has NEVER fired across weeks of desktop uptime (dwm, steamwebhelper,
    Settings, taskmgr, D3D11 apps) — modern win32k drives flip/composition
    models exclusively; a real KMD present-blit would serve a path Windows
    no longer invokes. (Also: a KMD GPU blit needs venus Vulkan encoding —
    viogpu3d's equivalent rides VIRGL_CCMD_RESOURCE_COPY_REGION, a virgl
    primitive venus lacks; a kernel venus Vulkan client is weeks of work
    for that dead path.)
  - Composition-swapchain API (documented "present from Vulkan" API) needs
    a real D3D11/D3D12 device at CreatePresentationFactory → recursion veto
    (and no D3D12 UMD exists). DEAD.
  - **Viable roads, pick after owner gameplay numbers:** (1) Helios-private
    independent-flip-at-the-consumer: for an unoccluded fullscreen-sized
    Vulkan window, the ICD publishes (resid, fence, geometry) via the WS1 #4
    present-sync channel and LGIdd's per-acquire copy sources the APP's blob
    instead of dwm's composition — semantically DXGI independent flip
    implemented at the IDD; every piece exists (publisher wire format,
    LGIdd dxvk import-by-resid, bounded consumer waits); the ONE design risk
    is the foreground/occlusion contract (must provably fall back to dwm
    composition the moment the window isn't the whole visible screen).
    (2) Pinned-build flip-model RE: reverse the dxgi↔win32k rendezvous
    (win32u NtGdiDdDDI* on 26100, which Helios pins as the guest build) to
    mint real flip tokens = true zero-copy dwm flip for windowed too; high
    RE effort, build-pinned fragility. (3) sw path stays the composed-window
    contract (already async; blit 0.7 ms; the dwm-side staged upload is the
    remaining per-composite cost). **(4) NEW — the dzn/zink-style dcomp road
    (in-tree PROOF in wsi_common_win32.cpp's dxgi branch, used by dzn):
    flip-model presents WITHOUT minting tokens — DXGI + DirectComposition
    mint them: `CreateSwapChainForComposition(device_or_queue, FLIP_SEQUENTIAL,
    ALLOW_TEARING)` + `DCompositionCreateDevice(NULL)` (dcomp needs NO
    rendering device) + `CreateTargetForHwnd(hwnd)` → `visual->SetContent
    (swapchain)` → `Commit()` — all documented public API. The one real
    device required is the swapchain's presenting device; a WARP D3D11
    device satisfies it with NO helios_umd recursion (d3d10warp.dll,
    self-contained). Frame flow: venus host-visible frame (cached mapping)
    → CPU copy into the WARP backbuffer → Present(0); dwm opens the WARP
    buffer cross-adapter (proven path — this box ran a WARP-composited
    desktop pre-milestone). Throughput ≈ the sw path (the bytes still cross
    guest→host once per composite, plus one extra CPU copy vs StretchDIBits);
    the wins are flip-model pacing/damage semantics, tear control, and no
    GDI redirection surface. Zink itself is only a consumer: it rides the
    underlying ICD's VK_KHR_win32_surface via kopper — on venus that is our
    sw path; dzn is the driver that exercises the dcomp branch.**
    **OUR OWN D3D11 PRESENT MODEL (read 2026-07-06, umd forward.rs
    `dxgi_present`): windowed D3D11 is ALREADY hardware-presented end-to-end.
    The DXGI runtime solves the rendezvous FOR the UMD — `DXGI_DDI_ARG_PRESENT`
    delivers BOTH `hSurfaceToPresent` and `hDstResource` (the win32k
    redirection/destination surface as a first-class UMD resource); the UMD
    does `CopySubresourceRegion(dst←src)` = the present blit runs GPU-side
    through venus (zero CPU bytes), then WS1 #4 present-fence publish + the
    bounded frame gate + `pfnPresentCb(hSrc, hDst)` — the KERNEL mints the
    history token (allowed in kernel mode; that's why DxgkDdiPresent never
    fires — the blit already happened in the UMD). dwm's own presents are
    flip-model onto the IddCx buffers (no dst → copy no-ops). CONSEQUENCE:
    only the VULKAN client class (native VK games + vkd3d-proton D3D12) lacks
    a HW present — a Vulkan ICD has no runtime handing it the destination
    surface; that missing hand-off IS the entire gap roads (1)/(2)/(4) exist
    to fill. Gotcha: `UmdTrace` is cached at device init — toggling it needs
    a process restart to take effect.**
    **ROAD 4 ON OUR OWN ADAPTER — PROVEN LIVE (2026-07-06,
    `tools/dcomp_present_probe.cpp`, schtask `helios_dcomp_probe`):** a D3D11
    device on the HELIOS adapter (our UMD; owner-approved vehicle use — the
    nesting is bounded and dwm already runs multiple DXVK→ICD stacks per
    process) + `CreateSwapChainForComposition(FLIP_SEQUENTIAL)` + dcomp
    target/visual on the HWND: every call S_OK, 1023 flip presents, dwm
    composes the animation (paintcap ×2, gradient advancing). This upgrades
    road 4 from "WARP + CPU copy" to the REAL zero-CPU-byte design:
    the DXGI backbuffers are OUR KMD allocations = venus blobs, so the ICD
    copies its frame into the current backbuffer GPU-side with its OWN venus
    device (import-by-resid, the dwm/WUDFHost machinery), then Present() on
    the vehicle device mints the token via pfnPresentCb (flip model → the
    UMD's dst-copy no-ops).
    **DESIGN REFINED (owner, 2026-07-06): reverse the import direction and
    special-case in the UMD present DDI via D3D10DDI_HRESOURCE.** The ICD
    never learns backbuffer resids (kills the COM→DDI texture-query problem
    entirely): the ICD publishes ITS OWN frame resid + fence value (WS1 #4
    slot), hands (resid, value, geometry) to a tiny in-process UMD export
    that stores it in TLS, and calls Present() on the same thread.
    `dxgi_present` consumes the TLS slot: the vehicle's DXVK imports the ICD
    frame by resid — the battle-tested alias-import path INCLUDING the
    `6eab004c` copy-time consumer wait, so the published slot orders the
    copy against the ICD's GPU writes for free — copies into
    `hSurfaceToPresent`'s pDrvPrivate DXVK texture, and publishes with its
    OWN fence (correct: the vehicle genuinely wrote the backbuffer). The
    double-publish conflict stops existing — single writer, single
    publisher, gate on the real writing device. Same GPU copy count, zero
    CPU bytes; WSI images become device-local/tiled (no linear cpu_map
    constraint — a rendering win). Remaining engineering: (a) vehicle
    lifecycle off the ICD hot path (the residual-risk contract), (b) the
    UMD export + TLS present-source slot + dxgi_present special case,
    (c) ICD publish of its frame resid (C writer for the seqlock format),
    (d) present-mode mapping, (e) resize/teardown lifecycle. Verify item:
    the ICD's WSI images must be created as shared-importable dedicated
    blobs (wire resources) so the vehicle context can import them. The
    async sw worker stays as fallback wherever the vehicle fails.
    **ROAD 4 IMPLEMENTED END-TO-END (23rd session, 2026-07-06; mesa
    `8a4e331ea9e..737cb2309b3`, dxvk-helios `6069f323`, main `6521d93` +
    `41fa1c4`). Kill switch `HELIOS_WSI_DCOMP_PRESENT`, DEFAULT OFF —
    flip-on is owner-gated (ladder e). Shipped shape:** vehicle lifecycle
    on a dedicated worker (parks after READY and owns the COM release, so
    the nested D3D11→UMD→DXVK→ICD2 teardown never runs on an ICD1 thread);
    frame images stay OPTIMAL buffer-blit images but their dedicated memory
    exports OPAQUE_FD → USE_SHAREABLE blobs; per-chain named present-order
    timeline (`Global\HeliosPresentFence_<pid>_<0x8000_0000|n>` — ICD id
    space is the high-bit half, the UMD producer counter owns the low half)
    signaled on every pre-present submit; seqlock publish from a
    byte-compatible C writer (wsi_helios_present_sync.c); UMD exports
    `helios_umd_set_present_source`/`_wait_last_present` (TLS, same-thread
    contract); dxgi_present special case alias-imports the frame by resid
    (typed identity, per-resid cache — 3 imports per geometry for 30k+
    presents) and copies at the DXVK image level from the LIVE storage
    (staging ALIAS if present; the COM CopySubresourceRegion path would
    read the never-refreshed private image), publishes the backbuffer with
    the vehicle's own fence, no gate, pfnPresentCb as today. Ladder:
    (a) probe PASS 1082 presents; (b) vkcube READY→LIVE, ~64 fps FIFO
    vsync-paced, 30k+ presents ZERO import/copy/geometry/overwrite
    failures, copy-time wait timeouts=0 noslot=0, dwm imports the vehicle
    backbuffers + fence (val tracking live), venus ctx destroyed at exit,
    resize-recreate exercised live (800x600→1896x959); (d) mover-churn
    paintcap clean, WUDFHost timeouts=0, BARRIER=0. **Flip no-copy
    invariant CONFIRMED: every vehicle 'DXGI Present:' has dst=0x0.**
    GOTCHAS: a maximized vehicle chain gets promoted to direct/independent
    flip — correct on the display but ABSENT from GDI-based paintcaps
    (eyeball via Looking Glass; owner-confirmed live); UmdTrace was ON for
    the invariant check — it confounds fps measurements.
    **Doom (immediate, menu): 40 fps v1 → 75 fps** after two measured
    fixes (frame-latency-waitable DROP for non-FIFO — windowed Present()
    otherwise blocks at dwm's compose pace; skip the worker's 4.1 ms
    frame-fence wait while the vehicle serves — ordering is the copy-time
    wait + pre-present throttle). Remaining gap to the 160 sw baseline is
    the image-recycle guard `wait_last_present` (6.1 ms serial,
    present-gate telemetry) stacked on the copy-time wait (8.4 ms
    overlapped): both are venus fence-OBSERVATION latency (ring≥1 retire →
    retire thread → NT signal; ICD2 submission-fence poll), not GPU time
    (~0.3 ms copy). NEXT LEVERS: (1) GPU-side recycle gating — export the
    vehicle's (fenceId, value) back through the UMD, import the named
    fence in ICD1 once, and make image reuse a timeline WAIT at acquire
    instead of a worker-serial CPU wait (fully pipelined; the run-ahead
    absorbs the latency); (2) shave the retire→signal path itself (helps
    every WS1 #4 consumer). sw async worker remains the default and the
    per-frame fallback (the pre-present blit still runs in vehicle mode —
    skip it only with numbers, it is what makes fallback seamless).
    **LEVER 1 IMPLEMENTED (24th session, 2026-07-06; mesa `10f72c50104` +
    `3e5fa9eb1ce`, main `1c34671`; UMD `helios_umd_c78f1a7e01df20b4`, ICD
    `vulkan_virtio-d49cd875438d`).** dxgi_present hands the vehicle's
    (fenceId, value) back via `helios_umd_get_present_result` (TLS,
    same-thread, counted misses/overwrites); the WSI imports
    `Global\HeliosPresentFence_<pid>_<fenceId>` once per chain and gates
    image reuse at ACQUIRE (bound `HELIOS_WSI_VEHICLE_WAIT_US`, 0=off;
    `acquire-gate:` diag telemetry per 512; drops recycle ungated;
    fallback = the old serial wait, `gate_fb` counter; teardown drains max
    pending value). Measured: worker cycle 13→4.4 ms, present-gate serial
    lines GONE, gate avg 2.7 ms overlapped, timeouts 0, gate_fb 0; vkcube
    FIFO ~60 clean; ladder clean (mover, WUDFHost timeouts flat,
    BARRIER=0).
    **CRASH FOUND+FIXED en route (first gated vkcube run froze the guest
    and degraded the whole desktop): vkr resolves vkSignalSemaphore only
    for >=1.2 devices or with KHR_timeline_semaphore enabled at create and
    dispatches it with NO null check** — vn_WaitSemaphores' imported-win32
    post-wait counter sync on a Vulkan 1.0 app (vkcube) = ip=0 segfault of
    the per-context render worker (`journalctl: vkr-ring-<ctx> segfault at
    0`), which the guest NEVER sees: qemu logs only
    `virgl_renderer_context_create_fence: Operation not permitted`
    (proxy_context_submit_fence → dead worker socket → -1), the poisoned
    submission's fence never retires, the app wedges in vkWaitForFences
    and everything else crawls on the backpressure. Fix (mesa
    `3e5fa9eb1ce`): append KHR_timeline_semaphore at device create for WSI
    devices on <1.2 instances (also legitimizes the pre-existing
    present-order timeline signals on 1.0/1.1 apps) + skip the host
    counter sync when the procs cannot exist. dwm/probe never hit it (1.3
    devices). Recovery for a wedged desktop: Stop-Process dwm.
    **OPEN — Doom menu capped at exactly 60.0 fps on BOTH paths today**
    (sw AND vehicle, gate on/off identical, QMP fence-trace on/off
    identical, foreground attempt identical; drops=0, gate timeouts=0,
    worker 4.4 ms, sw wait_avg 3.7 ms — nothing visible adds to 16.6 ms).
    The 22nd/23rd 120/160/75 numbers came from the same unattended
    launcher, so this is a NEW environmental cap, not the acquire gate
    (gate-off A/B proves it). Suspects to disambiguate with the owner:
    idTech background/unfocused throttle state, Steam relaunch settings,
    LGIdd/dwm restart state vs yesterday's boot. Tool:
    `tools/read-vehicle-counters.ps1` samples live minted/drops/gate
    counters from a running process (the perf line never prints on
    pure-vehicle runs); `Z:\tmp\movewin_target.txt` + helios_movewin
    foregrounds an arbitrary window title. Owner gameplay: ~70-80 fps.
    **STALE-FRAME STUTTER FIXED (24th session, cont.; owner-confirmed
    "much better"; main `5a9d5d5`).** Root cause: direct/independent-flip
    presents (Doom's near-fullscreen window) are ordered only on the KMD's
    decode-complete DMA fence — the backbuffer scanned out before the
    venus copy landed, showing the buffer's 3-presents-old occupant. dwm's
    consumer wait protects only COMPOSED presents. Fix: vehicle presents
    now run the frame-completion gate on the WORKER before pfnPresentCb
    (`VehicleFlipGateUs`, default 32 ms, 0=off; 6.7 ms avg = the copy's
    fence-observation latency; acquire-gate wait dropped 2.7→1.7 ms).
    Pipelined follow-up: dxgkrnl WaitForSynchronizationObjectFromGpu on
    the present packet + the producer fence before the copy flush (also
    retires the copy-time CPU wait for vehicle copies). Related cleanup
    (dxvk `7255c1e6`): the redundant pre-6eab004c list-start consumer wait
    in refreshHeliosStagedImages removed for the alias variant.
    **THE 200-fps LEVER FOUND — escape-parked fence waits convoy submits
    (mesa `fbf38f4cc63` has the telemetry + stopgap).** Phase splits
    ([prep/ring/win32] on QueueSubmit2, [mutex/escape/sync] on
    helios_submit) proved: the pre-present submit's 2.5-2.9 ms is entirely
    the win32-signal SUBMIT_VENUS D3DKMTEscape, which serializes at the
    dxgkrnl escape layer behind the retire thread's blocking WAIT_FENCE
    escapes (parked up to 250 ms/slice; processes with no parked waits —
    dwm, the sw path — submit at 3-5 µs). Slice 250 ms → 2 ms bounded the
    convoy (escape 2.9 → ~1.0-1.6 ms) and CONFIRMED the mechanism; Doom
    stays ~60-70 because its OWN vkWaitForFences ride the same parked
    escapes. NEXT SESSION (the fix): **KMD-signaled usermode fence
    events** — non-blocking REGISTER_FENCE_EVENT escape (fence_id, event
    HANDLE), KMD DPC KeSetEvent at wire-fence retirement (in-flight table
    already has ISR→DPC), retire thread + helios_wait park on the EVENT
    in usermode. Kills the convoy class entirely AND collapses the
    fence-observation latency every consumer pays (app fence waits, flip
    gate, acquire gate, retire→signal chain). KMD unit: event table with
    ObReferenceObjectByHandle lifetime + process-teardown cleanup +
    version bump; ICD unit: register/park/cancel in helios_ioctl_wait_fence
    path. Deployed at session end: UMD `helios_umd_89e95497a7d6d08f`, ICD
    `vulkan_virtio-2900b457e859`, dxvk `7255c1e6`, mesa `fbf38f4cc63`.
    **USERMODE FENCE EVENTS SHIPPED (25th session, 2026-07-06; main
    `16da0eb` = KMD 22.22.54, mesa `e5f35c18bf9`; deployed ICD
    `vulkan_virtio-9384eb059a8f`, UMD unchanged `89e95497a7d6d08f`).**
    REGISTER/UNREGISTER_FENCE_EVENT escapes: PASSIVE
    ObReferenceObjectByHandle → bounded (fence_id → PKEVENT) table;
    retirement DPC KeSetEvent + ObDereferenceObjectDeferDelete; one-shot;
    already-retired = atomic check under the device spinlock (no lost
    wakeups); capability probe (fence_id=0/handle=0 → PROBE_ACK, old KMD
    → NOT_IMPLEMENTED → loud diag + blocking-escape fallback). ICD:
    per-thread tss event, register → WaitForSingleObject → cancel;
    UNREGISTER NOT_FOUND + signaled event = raced (complete), +
    unsignaled = teardown purge (loud, INCOMPLETE). Retire thread: 2 ms
    slice stopgap retired from the event path — one WFMO on {fence
    event, retire_stop_event}, 60 s deadline; slice loop survives only
    as the old-KMD fallback. All counters in QUERY_STATS v2 + a
    `fence_events` perf-summary line.
    **BEFORE/AFTER (Doom vehicle, unit-0 baseline 23:00 → post-deploy
    23:35): submit_phases escape 465-786 µs → 111-126 µs; QueueSubmit2
    [prep 11.2/ring 45.5/win32 351 µs] → [5.2/2.3/87 µs] (submit_avg
    390 → 89 µs); acquire-gate (vkcube) 2.6-2.9 ms avg max 11-21 ms →
    196-273 µs avg max <1 ms; fence_events waits=2455 imm=245 raced=0
    timeouts=0 fallbacks=0 lost=0; vkcube FIFO ~60 clean; WUDFHost/dwm
    timeouts 0, BARRIER 0.** Gates passed: escape µs-class ✓, win32
    <100 µs ✓. PENDING: owner Doom gameplay fps + stale-frame stutter
    eyeball on the new stack (baseline gameplay flip-gate max was
    20-29.7 ms = the 2-3-vblank hitch signature; event waits should
    collapse it), present/flip-gate gameplay averages, ladder e soak.
    **DOOM LEVEL-LOAD FATAL ROOT-CAUSED + FIXED (same session; main
    `aedd8ba` = KMD 22.22.55): "Cannot map buffer with usage BU_STATIC"
    = MAP_BLOB → STATUS_INSUFFICIENT_RESOURCES = the adapter-global
    user-VA MappingTable still sized to the ORIGINAL MAX_BLOBS (256)
    after blobs grew to 8192** — desktop held ~223 mappings, the level
    load's vkMapMemory burst blew the remaining ~33 (probe-proven:
    map-and-hold refused at held=33 with 0xC000009A; size sweep 1-256
    MiB all passed, falsifying the MDL-CSHORT hypothesis; failure
    signature present at 05:28 pre-change = NOT a fence-event
    regression). Fix: MAX_MAPPINGS 8192 + the three indistinguishable
    0xC000009A refusal sites individually counted (mapping-full /
    map-pages / window-alloc) in QUERY_STATS v2.
    `tools/blob_map_size_probe.c` = size sweep + concurrent-headroom +
    v2 stats reader (gcc on the VM, no vcvars needed).
    **STALE-FRAME A/B VERDICT + KERNEL-ENFORCED FLIP ORDERING PROVEN
    (25th session cont., 2026-07-07; main `77dffe2`).** Owner A/B on the
    fence-event stack: **sw path 200 fps (was 160 — the event waits
    bought sw +40 fps) with ZERO stale frames; vehicle path 120-130 fps
    with stale-frame stutter still present** → the leak is the
    vehicle/UMD flip path, pre-existing, fps-scaled. Counted leak sites
    that window: flip gate 11 timeouts + acquire gate 3 timeouts per
    ~41k presents (each "proceeds loudly" = a stale flip candidate);
    fence-event machinery itself clean (0 fallbacks/0 lost). NEXT ROAD —
    **kernel-enforced vehicle flip ordering** (replaces the bounded CPU
    worker gate): queue D3DKMTWaitForSynchronizationObjectFromGpu on the
    copy's monitored fence AHEAD of the present packet, so dxgkrnl holds
    the flip until the retire thread's CPU signal lands — no usermode
    cap to leak, and the 6.6 ms worker serial gate retires (fps win).
    `tools/vehicle_flipwait_probe.c` PROVES the primitive live on our
    software-scheduled adapter (queued signal held behind an unsatisfied
    wait, drained ~10 ms after the CPU signal; ZERO KMD changes) and the
    topology: raw cross-device sync handles are REJECTED 0xC000000D —
    the fence must be NT-shared (D3DKMTShareObjects) and reopened via
    OpenSyncObjectFromNtHandle2 on the device owning the waiting context
    (the WS1 #4 named-share consumer pattern). Implementation sketch:
    UMD opens the vehicle fence (fence_id known from the present result)
    on the device owning `dev.h_context`, queues the wait right before
    pfnPresentCb; CPU gate kept as fallback knob + wedge watchdog
    (a never-signaled fence would park the context queue). Second half
    (same primitive): producer-fence wait before the copy flush retires
    the 7 ms copy-time CPU wait. Ladder: wait-honored telemetry → flip
    gate CPU path off → owner stutter eyeball → fps before/after.
    **IMPLEMENTED + WEDGED (main `e9cdae6`, default OFF; VM registry
    VehicleKernelFlipWait=0 guards the deployed default-ON UMD
    `fb564d5072f8a58e`).** First live test (vkcube): present #1 armed +
    queued OK, but the enqueueWait signal NEVER fired (exported present
    fence counter never read ≥1 in the producer's own process — likely
    an untested vn_GetSemaphoreCounterValue/vn_WaitSemaphores
    exported-win32 path), the watchdog unwedge released the flip but the
    app never presented again (the WSI worker parks on something after
    pfnPresentCb — find it), and TerminateProcess on the wedged instance
    HUNG THE ENTIRE GUEST — no bugcheck, no dump (kernel deadlock;
    dxgkrnl/KMD teardown of a context with a parked queue + queued
    monitored-fence wait whose signal source died). QMP system_reset
    recovered. **26TH-SESSION TOP PRIORITY: root-cause + fix all three
    (see the flip-kwait-wedge memory: NMI-crash/live-KD recipe, suspect
    KMD teardown paths, retire-thread-signal design fallback).**
    **26TH SESSION (2026-07-07): DEFECT (a) ROOT-CAUSED + FIXED (mesa
    `b2f47c780d2`, deployed ICD `vulkan_virtio-e31ec528ac79`); the
    GUEST "HANG" CLASS SOLVED — it was the SERIAL KERNEL DEBUGGER.**
    (a) The producer's EXPORTED present fence was unobservable in its
    own process: `is_external` skips the feedback slot, the queue
    signal routes out-of-band onto the helios_sync/WDDM fence (retire
    thread only), and both vn_GetSemaphoreCounterValue (host ring
    round-trip: 0 forever) and vn_WaitSemaphores (win32 fast-path was
    imported-only) never read the WDDM fence. Fix: fold
    vn_renderer_sync_read into the counter read for ANY win32-backed
    payload + widen the wait fast-path (event wait); host-counter sync
    stays imported-only (exported host object has a pending GPU signal
    op). Verified: knob=1 vkcube presents #1-2 armed AND RELEASED
    (was: #1 wedged); first-rescue diag `wddm=852/1307 host=0`.
    (c) The whole-guest freeze reproduced WITHOUT any kill (knob=1
    vkcube after ONE wedge+watchdog-unwedge cycle, ~3 min fuse) —
    QMP `inject-nmi` → bugcheck 0x80 minidump `070726-4890-01.dmp`
    (copy in Z:\tmp): 15/16 vCPUs in nt!KiFreezeTargetExecution, CPU#6
    polling kdcom!READ_PORT_UCHAR — the guest had dropped into the
    SERIAL KERNEL DEBUGGER (bcdedit debug=Serial port 1, NO kdcom
    client exists — ntoseye is a gdbstub) and froze forever. With KD
    enabled, dxgkrnl asserts / the ERESOURCE deadlock detector / TDR
    BREAK instead of bugchecking — that is why neither hang ever left
    a dump. TerminateProcess was never the root cause.
    (b) Caught red-handed in the same dump (stack scavenge of the
    frozen vkcube thread): a runtime thread inside
    DxgkWaitForSynchronizationObjectFromGpu blocked MINUTES in
    nt!ExpWaitForResource on a dxgkrnl ERESOURCE (holder unknown —
    triage dump has one stack) until nt!ExpResourceTimeoutCaptureLiveDump
    → KiBreakpointTrap → KD entry. So the worker's forever-park after a
    flip-kwait stall = a dxgkrnl-internal ERESOURCE convoy seeded by
    the stalled flip, not a WSI wait (all WSI waits audited bounded).
    RESIDUAL OPEN: present #3's wire fence never retired (fence stalled
    at 2 with 3 queued; #1-2 clean) — the remaining true stall; and the
    WSI perf-counter oddity (presents=0/ready=0 while 3 vehicle
    presents + gate-ARM demonstrably ran). TOOLKIT NOW ARMED:
    CrashDumpEnabled=2 (full kernel dump next time → !locks names the
    ERESOURCE holder); PROPOSED: bcdedit /debug off so every future
    freeze self-converts to dump+reboot (testsigning + ntoseye/gdbstub
    unaffected). Registry knob back to 0; ICD fix deployed + desktop
    cold-verified healthy on it.
    **26TH SESSION CONT.: THE DEADLOCK CLASS KILLED — ERESOURCE HOLDER
    NAMED AND FIXED (mesa `2cc63d82468`, deployed ICD
    `vulkan_virtio-178211300d5f`; bcdedit /debug off applied).** The
    owner-approved second NMI (full kernel dump, CrashDumpEnabled=2)
    caught it red-handed: the exclusive holder of all 3 contended
    ERESOURCEs was the process's own next VENUS ESCAPE —
    HardwareAccess=1 escapes run dxgkrnl
    AcquireCoreResourceExclusive → DXGPROCESS::FlushAllDevice →
    VidSchWaitForCompletionEvent, i.e. they WAIT FOR EVERY CONTEXT
    QUEUE TO DRAIN while holding the core resource; with a kwait-parked
    queue whose signal comes from user mode, every
    SignalSynchronizationObjectFromCpu (dxvk waiter, watchdog — user
    dump caught the watchdog parked IN the signal syscall, so the
    25th-session "unwedge released the flip" was FALSE — and the ICD
    retire thread) convoys behind it = deadlock; wedge point #1/#2/#3
    varied by race. Fix = unit 3's HardwareAccess=0 (correctness, not
    just perf; `HELIOS_ESCAPE_HW=1` env kill switch) + the exported-sem
    fold + a signalTo monotonicity guard (main `6d41f8e`, deployed UMD
    `helios_umd_3b4a9e66394e9523`). **LADDER RESULTS (2026-07-07
    02:45-02:53): 24,064 kwait presents, 0 wedges / 0 arm fails /
    0 queue fails / 0 signal fails; mid-run kill (owner) → clean
    teardown, no zombie, guest healthy. BUT THE OWNER EYEBALL RUNG
    FAILED: vkcube showed the Doom-class STALE-FRAME STUTTER from
    ~40 s in — kernel flip ordering alone does NOT close the stale
    class. Plus a new observation: a freshly launched (unfocused)
    vkcube alternates between TWO stale frames until the window is
    clicked, then rendering progresses. DEFAULTS STAY OFF
    (VehicleKernelFlipWait code default OFF, registry knob back to 0).**
    **OWNER CONFIRMED: both issues are ABSENT on the sw present path**
    — they are properties of the dcomp VEHICLE presentation layer, not
    of fences/venus in general (matches the A/B: sw 200 fps zero
    stale). 27th-session hypotheses, vehicle-layer-first:
    (c1) DXGI_STATUS_OCCLUDED — Present() on an occluded/background
    window returns a SUCCESS status (0x087A0001) and does not display;
    wsi_win32_queue_present_vehicle checks only FAILED(hr)
    (wsi_common_win32.cpp:1985-1995) so occluded presents "succeed"
    silently → add per-present hr!=S_OK logging (cheap, likely explains
    the click-gated launch behavior); (c2) dwm/dcomp consumption pacing
    for background visuals, direct-flip vs composed transitions;
    (a) backbuffer clobber — the venus copy lands at Present-call time
    while up to frame-latency flips sit parked, so rotation may
    overwrite a buffer whose flip has not scanned out (instrument
    backbuffer ptr + flip completion pacing; consider bounding armed
    depth); (b) U:=V fires at ring-fence retirement measured at 97-98%
    of T_gpu — check the tail. First step: knob=0 vehicle A/B of the
    unfocused-launch behavior + the hr logging.
    **27TH SESSION (2026-07-07): (c1) FALSIFIED + full static analysis of
    the HW present path.** hr-transition/drop-streak instrumentation
    deployed (mesa `e3d2fdf61c1`, ICD `vulkan_virtio-1545839fc535`;
    `odd_hr` counter in the perf line; paintcap_hidden schtask = capture
    without the console occluder/focus steal): under a 45 s full-screen
    occluder the knob=0 chain presented at a steady 60 Hz, hr == S_OK
    throughout, acquire-gate timeouts 0 — composition swapchains never
    report OCCLUDED and dwm keeps consuming occluded chains; presents
    flowing says NOTHING about display. Owner clarified the property set:
    launch alternation + random self-healing stutter are both cured by ANY
    rendering activity (notepad open — no focus change!) ⇒ the lever is
    activity, not focus. Static analysis (3-leg map, agent reports in the
    session transcript): every vehicle ordering protection is a bounded
    32 ms wait that LEAKS stale on timeout (consumer copy-wait
    dxvk_context.cpp:9507 "copying anyway"; flip gate forward.rs:5902
    "flipping anyway"; acquire release-gate wsi_common_win32.cpp:1683
    proceed; dwm-side staged-refresh wait ditto) — all funnel into named-
    fence advancement by the per-process ICD retire thread, whose event
    wait is the ONE chain link with NO self-heal: KMD fence events are
    interrupt-edge-driven (drain_used from the ISR/DPC or opportunistic
    drains on the process's OWN escape traffic only; no KMD timer;
    register-vs-retire race proven closed, gpu.rs:1317/escape.rs:186), so
    a lost/deferred INTx for a mostly-IDLE process (dwm on a static
    desktop!) parks its retire thread ≤60 s with nothing to kick it.
    PRIME SUSPECT (fits all 4 properties + why vkcube-side counters were
    all clean): the stall is in DWM's process — dwm consumes the vehicle
    backbuffers through the alias-staging refresh + 32 ms consumer wait;
    a parked dwm retire thread ⇒ dwm composes stale staged copies of the
    2 ever-refreshed backbuffers (= the TWO alternating frames) while the
    app's flips rotate healthily; dwm idle ⇒ few escapes ⇒ no drains ⇒
    self-sustaining until any dirty region makes dwm submit (notepad,
    click) ⇒ drain ⇒ unstick. Secondary: retire-thread serial FIFO
    convoy (one stuck head delays every later signal = stutter burst,
    self-heals = P3/P4); vehicle named release fence created LAZILY
    inside present #1 (dxvk_bridge.cpp:1434) + consumer import-failure
    negative-cache of 256 LOOKUPS (dxvk_context.cpp:9482) = long
    unordered window at chain birth. SOLUTION CHOSEN (owner: NO HACKS):
    prove the failing link with ONE owner repro, then fix that link at
    its ROOT — (a) event registered-but-never-signaled + INT_ROUTINE
    stalled ⇒ interrupt delivery ⇒ MSI-X (MSISupported=0 is a bring-up
    leftover); (b) used entry never posted ⇒ submission/doorbell path;
    (c) event signaled but fence behind ⇒ ICD retire logic. The sliced-
    polling retire wait + KMD timer backstop are REJECTED as hacks;
    eager vehicle fence + time-based import retry are clean but staged
    AFTER the repro so they cannot mask it. Evidence kit already
    complete: dwm_helios_umd_dxvk.log present-wait lines (import lines
    prove the channel live; ZERO timeout lines in healthy runs),
    helios_icd_diag.log retire "GIVING UP ... stays UNSIGNALED" lines
    (PROVEN to fire — 2 instances 2026-07-06), QUERY_STATS v2
    FENCE_EVENT_*/INT_ROUTINE via tools/blob_map_size_probe.c,
    paintcap_hidden content diffs. REPRO IS OWNER-ONLY: schtasks
    launches always take foreground (fg=1 across 4 steal attempts), and
    automated probing IS the curing activity (self-defeating). Owner
    recipe: reproduce the alternation, hands off ~90 s (>60 s arms the
    GIVING-UP diag), note the time; then read the four channels.
    **27TH SESSION VERDICT + FIX DEPLOYED (KMD 22.22.56, main `1a07814`).**
    Owner repro delivered the discriminator: during a LIVE two-frame
    alternation (~60 s, hands-off recovery) EVERY sync in the DAG was
    green — vkcube 60 fps/acquire-gate 64 µs/0 timeouts, vehicle 29k
    copies 0 fails/flip-gate 390 µs, dwm waits 7.9 ms 0 timeouts 0
    noslot on the live chain, WUDFHost 6.3 ms 0/0 — so the fence class
    was EXONERATED (retire/interrupt fixes rejected as the wrong tree).
    Second audit pair: our flip-emulation identity rotation is provably
    self-consistent (copy dst == flip src per present; storage+identity
    rotations = same permutation; publish resid tracks rotation; no
    in-tree pairing skew), BUT the caps surface lied:
    `DXGK_DRIVERCAPS.SupportDirectFlip=1` (NO bisect provenance — never
    load-mandatory) + aperture-segment DirectFlip flags, on an adapter
    with ZERO scanout displaying through IddCx-captured COMPOSITION.
    dwm promotes the eligible dcomp vehicle visual (flip-model +
    IGNORE-alpha + unoccluded) to direct/independent flip and STOPS
    COMPOSING it — two alternating stale frames = dwm's last composed
    pair; any dirty-region recompose demotes it (taskbar clock minute
    repaint = the hands-off ~60 s recovery; console-overlapped schtasks
    launches never repro'd because occlusion kills eligibility; the
    23rd-session "maximized chains vanish from paintcaps" was the same
    promotion). The UMD already denied CheckDirectFlipSupport — the KMD
    now agrees: SupportDirectFlip + the 3 aperture DirectFlip flags are
    DENIED by default behind the `DirectFlipCaps` service knob (0 =
    deny, 1 = legacy A/B via reg add + devcon restart; state in diag
    0x01D7 bit 2, DiagLevel-gated). Display-less-adapter guidance
    (mcdm-implementation-guidelines.md) mandates 0. DEPLOYED + cold-boot
    verified: 22.22.56 CM_PROB_NONE, release UMD re-pinned
    (helios_umd_3b4a9e66394e9523; devcon reset it to debug — known
    trap; DriverStore copy sync failed file-in-use, cosmetic), desktop
    composited healthy. OWNER VERDICT: NO CHANGE — direct-flip denial
    falsified as the mechanism (kept as the truthful cap surface).
    **TRUE ROOT CAUSE FOUND + FIX DEPLOYED (dxvk `1cdf0837`, mesa
    `10cefe67c64`; UMD `helios_umd_746b0242cf664825`, ICD
    `vulkan_virtio-390d1b583dba`).** Discriminating evidence: owner
    repro with per-window counter sampling — during a LIVE dance dwm
    performed ZERO consumer reads for 20+ s while composing 60 fps
    (waits/fast/noslot all frozen), and mouse movement cured it
    INSTANTLY (both dance and stutter) — consumer freshness tracked
    dxvk COMMAND-LIST CADENCE, not producer progress: staged imports
    re-stage only at list starts, an idle dwm's CS chunks span many
    frames (~60 s to fill when idle = the hands-off recovery), so its
    sampled staged copies froze while every fence stayed green; the
    front-buffer rotation cycled 2-3 differently-aged frozen copies =
    the two-frame dance; chunk-threshold oscillation = the stutter;
    activity = per-frame flushes = cure. sw path immune because
    GdiAccelMode=0 routes GDI content via CPU redirection surfaces
    (not our staged imports). THE FIX (dxvk-helios, 3 parts):
    (1) bind-time staleness gate — staged-SRV binds arm a sticky
    per-context flag; draws/dispatches on the immediate context compare
    each bound staged image's published present-sync value against the
    value its last re-stage observed and Flush() when the producer
    advanced (≤1 flush per published value per image via a claim CAS;
    non-consumer processes pay one branch per draw); (2) zombie-refresh
    unenroll — texture dtor flags the image, the refresh loop erases it
    (was: 15k+ no-slot full-image copies per DEAD vkcube chain until
    the 3600-tick prune, Rc-pinning the corpse's venus resources);
    (3) present-sync slot recycling by PRODUCER CREATION TIME
    (reserved2 repurposed; pid liveness alone let cross-boot pid reuse
    keep 64/64 slots stale — publishes were being DROPPED table-full).
    Along the way: 366-368-vs-389-391 resid "mismatch" resolved as two
    chain generations (live-chain publish/lookup rendezvous verified
    healthy, 3 rotating slots advancing per frame). AWAITING owner
    verdict on the fix. Then: ladder rungs (Doom fps vs 120-130,
    kwait default decision, HELIOS_WSI_DCOMP_PRESENT default), and the
    deferred cleanups: per-present sw-fallback insurance blit on
    vehicle chains ("measure before removing"), in-process present-sync
    slot/named-fence round-trips (set_source carries a dead
    fence_value), eager vehicle fence at init, time-based import
    negative cache, DirectFlipCaps knob retirement decision.
    **FIX CONFIRMED (owner: "working very well"); 28TH SESSION =
    WINDOWED DOOM PERF.** Doom telemetry (pid 9444, 1880x943 windowed
    vehicle, immediate/tearing=1, ~105 fps): flip gate avg 5.57 ms
    (worker-serial) + acquire gate avg 4.06 ms (app), both timeouts=0 —
    pure fence-observation latency of the vehicle copy;
    queue_present_avg 5.96 ms. FULLSCREEN "200 fps rock-stable" = THE
    SW PATH: the fullscreen (1896x1030) chain's vehicle build FAILED at
    stage='dcomp target/visual' hr=0x88980800 and latched → sw direct
    path at ~0.85 ms/frame CPU (creates=2 fails=1 in
    helios-doom-wsi-perf.txt). NEW DEFECT: vehicle re-create for the
    same hwnd fails (one-dcomp-target-per-hwnd on resize/fullscreen;
    vkd3d likely creates a NEW VkSurface for the same hwnd → per-surface
    target cache misses → second CreateTargetForHwnd fails). dwm
    post-fix: noslot=54 (was 68k), fence_events 0 fallbacks/0 lost,
    escape 64 µs. Doom perf files: C:\Users\Rupansh\helios-doom-perf.txt
    + helios-doom-wsi-perf.txt (owner's launcher tees them).
    **28TH SESSION: the dcomp target re-create defect FIXED (mesa
    `bbf5e33f314`, ICD `vulkan_virtio-437986e5fcc4` deployed).** One
    composition target per hwnd is a Windows rule; the target/visual
    cache was per-VkSurface, so a new VkSurface for the same hwnd
    (vkd3d resize/fullscreen) failed CreateTargetForHwnd 0x88980800 and
    latched onto the sw path. Fix: process-global refcounted
    hwnd→target registry under the vehicle runtime mutex; the visual's
    content owner (current_swapchain) lives on the shared entry so a
    retired chain on the OLD surface cannot blank the new chain's
    content; `tgt_reuse` counter added to the perf line. Proven by
    tools/vk_surface_recreate_probe.cpp (schtask `helios_vk_recreate`,
    session 1, time-based phases — frame-count phases finish before the
    ~5 s async vehicle build, first attempt exercised nothing): chain A
    LIVE holding the target → chain B (new surface, SAME hwnd, A alive)
    READY+LIVE → chain+surface A destroyed under live B, B presented on
    (acquire-gate 0 timeouts). The honest fullscreen-vehicle Doom A/B
    is now unblocked (owner run — expect creates=2 fails=0 and the
    fullscreen chain LIVE instead of the 0x88980800 latch).
    **28TH SESSION cont. — kwait rung 1 GREEN + copy-side wait FALSIFIED
    + both cleanup counters live (mesa `06e27a05ea3`, dxvk `7c82271f`;
    deployed ICD `vulkan_virtio-43feb2709167`, UMD
    `helios_umd_801a8571aff69c67`, `VehicleKernelFlipWait=1` in the
    registry).** (a) Kernel flip-wait smoke: vkcube dcomp 4608/4608
    presents kwait_armed, 0 arm/queue fails, acquire-gate 66 µs avg
    0 timeouts, 40 s no wedge, content advancing in paintcap diffs —
    the 5.6 ms worker-serial flip gate is retired whenever the knob is
    on; Doom A/B vs the 105 fps baseline = OWNER RUNG (knob already
    live). (b) The 25th-session "producer-fence wait before copy flush"
    idea is FALSIFIED as a lever: Doom's 28k-present run logged ZERO
    copy-time consumer waits (no present-wait/unordered/noslot lines in
    umd-9444.log) — the copy-time CPU wait always fast-paths; the
    4.06 ms acquire gate is copy COMPLETION+OBSERVATION latency, not a
    CS-thread wait. Not built (measure-first). (c) Insurance blit:
    HELIOS_WSI_INSURANCE_BLIT=0 skips the per-present image->buffer
    fallback blit on vehicle-serving chains (insurance_skipped counter
    in the common perf line; vkcube A/B clean, 2780 skips, content
    correct) — default ON until the Doom A/B numbers land. (d)
    dwm-side staleness-gate cost now visible: gate_flushes in the
    present-wait line — ~50 flushes / 13.7k consumer reads (~0.4%) on
    a live desktop, the 27th-session fix is cheap. OWNER LADDER: Doom
    windowed (kwait live) fps + gates vs 105; fullscreen re-try
    (target-registry fix) — expect vehicle LIVE, honest fullscreen
    number vs the 200 fps sw path; optional HELIOS_WSI_INSURANCE_BLIT=0
    leg; stale class must stay dead throughout (vkcube shows no
    regression). THEN: kwait + DCOMP default decisions, residual
    stutter triage (105-vs-60 Hz judder, gate max-spikes).
    **OWNER DOOM VERDICT (same-process windowed→fullscreen, kwait=1 +
    insurance=0): no fps change; stutter still present WINDOWED, GONE
    FULLSCREEN.** Evidence (pid 6220): the fullscreen 1896x1030 chain
    went VEHICLE (READY+LIVE on the same hwnd as the windowed chain —
    the target-registry fix confirmed in the wild); kwait_armed
    6144/6144, 0 arm/queue fails, no wedge; insurance_skipped
    13176/13200; queue_present_avg 5.96→2.81 ms (flip gate retired) BUT
    acquire-gate 4.06→7.69 ms avg (max ~20 ms, timeouts 0) — the
    latency the worker gate used to absorb moved to acquire: the fps
    limiter is the vehicle copy's COMPLETION+OBSERVATION latency
    (~1 host-GPU frame at saturation), not CPU gates. Gates are now
    honest pipeline measurements, nothing left to delete guest-CPU-side.
    STUTTER LOCALIZED: same process/gates/fps, stutter only when dwm
    COMPOSES the chain (windowed); fullscreen (no dwm compose leg)
    smooth ⇒ the stutter lives in dwm's consumption leg. dwm telemetry
    during the run: gate_flushes 2480 (~60/s — the freshness gate does
    per-frame work under a game, expected) and consumer waits avg
    8.9 ms when they fire (~8% of reads) — 9 ms stalls ON DWM'S CS
    THREAD = composition hitches. HYPOTHESIS (next session): the
    staged-refresh loop re-stages ALL enrolled vehicle backbuffers at
    list start, including the just-presented one whose copy is still
    in flight — dwm waits ~9 ms for a buffer it is not composing this
    frame. Fix shape (no hacks — matches real-hw dwm semantics of
    composing the newest COMPLETE frame): skip-if-unretired in the
    refresh (keep the current staged bytes, let the bind-time gate
    force the re-stage next list) instead of the bounded 32 ms wait;
    kwait guarantees the flip itself never outruns its copy, so
    skipping cannot resurrect the stale class. DEFAULTS PROPOSAL:
    kwait code-default ON (Doom+vkcube green, deadlock class dead);
    insurance knob keep (no measurable cost either way at Doom res —
    the copy hides under GPU latency); DCOMP default AFTER the dwm
    stutter fix.
    **STUTTER FIX IMPLEMENTED + DEPLOYED (dxvk `6bcbd282`, main
    `83f9697`; UMD `helios_umd_b70f3e5b23cc5e03`): skip-if-unretired
    staged refresh for kwait-ordered publishes.** Producers advertise
    kernel-held flips in the present-sync slot (fenceId bit 30 — free
    in both id spaces; UMD sets it on vehicle publishes when
    flip_wait_setup succeeded, never on dwm→IddCx publishes; mesa
    publishes never set it); the consumer refresh keeps its current
    staged bytes for an unretired kwait value (that image cannot be
    the sampled front buffer — dxgkrnl holds its flip), re-arms the
    bind-time gate, counts (refresh_skips in the present-wait line;
    dxvk.heliosSkipUnretiredRefresh default ON = kill switch).
    heliosProducerFence factors the import cache; VehicleKernelFlipWait
    CODE DEFAULT NOW ON (registry 0 = kill switch). VERIFIED LIVE:
    dwm skips ~60/s on vkcube's 3 backbuffers with ZERO blocking waits
    since restart and content advancing (paintcap pair); WUDFHost
    refresh_skips=0, waits unchanged 5.8 ms/0 timeouts (the IddCx
    orderer untouched); vkcube kwait 4096/4096 armed via the code
    default. AWAITING owner windowed-Doom eyeball: the 8.9 ms dwm CS
    stalls are gone — stutter verdict decides the DCOMP default next.
    **OWNER VERDICT: STUTTER FIXED (fps ~same, as expected — the
    limiter is copy completion+observation, a separate lever). DCOMP
    DEFAULT FLIPPED ON (mesa `f5037d701ad`, ICD
    `vulkan_virtio-d6e68f7a3322` deployed): the vehicle now serves
    every Vulkan swapchain by default; HELIOS_WSI_DCOMP_PRESENT=0 =
    per-process kill switch. Verified: env-free vkcube goes vehicle
    LIVE, kwait 1536/1536 armed, acquire-gate 84 µs / 0 timeouts,
    content advancing (paintcap pair). The vehicle class (WS2 road 4)
    is DONE: kwait ON by default, skip-if-unretired ON by default,
    hwnd→target registry, insurance knob available. REMAINING WS2
    PERF LEVER: the vehicle copy's completion+observation latency
    (acquire gate ≈ 7.7 ms under Doom ≈ 1 host-GPU frame) — measure
    host-side scheduling before touching. Deferred cleanups: in-process
    slot round-trips (dead set_source fence_value), eager vehicle
    fence at init, WSI perf-counter oddity, cold-boot re-verify,
    GdiAccelMode retirement, DirectFlipCaps knob retirement.**
    **29TH SESSION — COPY-LATENCY ROOT CAUSE FOUND: QEMU delivers venus
    wire-fence completions by POLLING (10 ms fence_poll timer + an
    opportunistic poll on every guest ctrl-queue kick); the async
    fence-callback path is NOT active in the running config.** The whole
    stack below it is fast. Measurement chain (all landed, deployed):
    (a) `copy-lat` in the umd bridge (publish→waiter-observation of the
    vehicle copy fence; umd log, every 512): vkcube on an IDLE GPU avg
    ~13.6 ms, dominant bucket 10-20 ms — the Doom 7.69 ms acquire gate is
    just the tail of this past the app's natural slack. (b) `retire_lat`
    in the ICD (submit→wire-fence-retirement-observed, HELIOS_PERF
    summary + histogram): BIMODAL — ~half <1 ms, ~half 10-20 ms, max
    ≈11 ms (vehicle) / ≈16 ms (producer), RATE-INDEPENDENT (260 fps
    immediate-mode vkcube: slow mode persists and stacks, max 28 ms).
    (c) Host driver EXONERATED: tools/vk_fence_wake_probe.c (Linux,
    empty vkQueueSubmit+WaitForFences on the idle NVIDIA queue) =
    0.23 ms avg / 0.48 ms worst. (d) HOST-SIDE PROOF from the 07-06
    traced run in /tmp/helios-qemu-stderr.log (virtio_gpu_fence_ctrl/
    resp): a lone in-flight fence gets its response +10.5 ms; two
    fences submitted together both complete fast (the second submit's
    handle_ctrl kick polls the first through) — the exact
    poll+kick signature of hw/display/virtio-gpu-gl.c
    virtio_gpu_gl_handle_ctrl → virtio_gpu_virgl_fence_poll and the
    10 ms fence_poll timer. QEMU 11.0.1 enables async fences only when
    `qemu_egl_display` is set + virglrenderer ≥1.1.2 (both look
    satisfied: egl-headless display, virgl 1.3.0, callbacks v4) — WHY
    it is not active is the open host-side question (verify via gdb
    `print qemu_egl_display` on the live process or a traced relaunch:
    `--trace 'virtio_gpu_fence_*'`). FIX DIRECTION (owner decision, VM
    relaunch territory): get VIRGL_RENDERER_ASYNC_FENCE_CB active —
    expected win: every venus fence wait in the system (vehicle copy
    observation, dwm consumer waits avg 8.9 ms, IddCx waits 5.8 ms,
    D3D11 fence waits) drops from ~5-15 ms to sub-ms; the ~105 fps
    windowed Doom ceiling should lift substantially. Vkr map (host,
    for later levers): per-context worker PROCESSES, no cross-context
    CPU locks; per-guest-queue host VkQueue, family passed verbatim;
    ring≥1 retirement = empty marker submit + per-queue sync thread;
    VK_EXT/KHR_global_priority plumbed end-to-end; transfer-family
    queues map 1:1 (both usable if GPU-side contention ever becomes
    the limiter — it is NOT today). Waiter named: the vehicle device's
    `wait_calls fast=0 timeout≈2571` = DxvkFence::run() (dxvk_fence.cpp
    10 ms slice loop) servicing present_flip_wait_arm enqueueWaits —
    slices are benign event-backed re-loops. New tooling: schtask
    `helios_vkcube_imm` (immediate-mode cube, perf files
    vkcube_imm_*); helios_vkcube now also sets HELIOS_PERF →
    vkcube_renderer_perf.txt.
    **29TH SESSION cont. (owner-collaborative) — REFINED + WORKAROUND
    SHIPPED: feedback-shadow retire (mesa `b578e7d42a3`, ICD
    `vulkan_virtio-32e87a6fc919` deployed, device restarted, desktop
    verified).** Refinements from live host debugging (owner gdb/strace/
    sysctls): the async fence path IS configured (proxy flags 962);
    worker retires fences in 50-150 µs (strace); the io_uring fdmon
    theory was FALSIFIED (kernel.io_uring_disabled=2 + reboot: epoll
    shows kick→IRQ p50 33 µs yet guest retire_lat unchanged); vkr ring
    thread + guest KMD interrupt handling audited clean (spec + Linux
    virtgpu cross-checked by the owner; real non-10 ms inefficiencies
    noted: INTx shares IRQ 22 with the balloon → KMD MSI-X someday;
    serial retire-thread waits). Guest no-WSI probe
    (tools/vk_fence_wake_probe_win.c): the vn FEEDBACK channel is
    0.61 ms while the WIRE-fence channel carries the 10-20 ms class.
    DOOM (instrumented, owner run): copy-lat 11.4 ms avg (65% in
    10-20 ms, 0% <1 ms), acquire-gate 6.9 ms of the 9.5 ms frame, game
    device 97% of fences 6-20 ms — CONFIRMED as THE fps ceiling.
    OWNER DECISION: QEMU fix out of scope → WORKAROUND: the ICD retire
    thread now observes exported-fence completion via the semaphore's
    GPU-written vn feedback slot (slots now allocated for exported
    timelines — self-signaled only on this stack; counter VA attached to
    the sync, detached before pool recycling; poll ladder yield/Sleep to
    a 50 ms budget; wire path = fallback + kill switch).
    `HELIOS_RETIRE_FEEDBACK` DEFAULT ON, =0 restores wire behavior
    (A/B-proven). vkcube: retire_lat 5.6-9.2 ms → 0.25-0.33 ms (100%
    <1 ms, fb fast=100%); copy-lat 13.6 ms → 0.8 ms; content advancing;
    kill-switch run bimodal again. retire_fb fast/fallback/wire counters
    in the perf summary. PENDING: owner Doom re-run (expect the acquire
    gate to collapse and fps to rise toward the ~200 sw-path bound);
    HELIOS_RING_NOTIFY_EAGER diag knob (mesa `4d0c7d21514`, default off,
    falsified as a lever); host sysctls to revert when debugging ends:
    kernel.yama.ptrace_scope=0 (→1), kernel.io_uring_disabled=2 (owner's
    call — QEMU fence behavior identical either way).
- **Capture path**: IddCx frame drop policy vs D3D12 copy queue saturation;
  KVMFR bandwidth; 10 bpc default.
- Candidates list from the NVIDIA fix era lives in `docs/archive/ICD.md`.

## Workstream 3 — D3D11 Conformance  ← **PRIORITY 1 since 2026-08-05**

**The charter is `CONFORMANCE.md`** — what "conformant" means for this stack,
the refusal/no-op counter surface and how to read it, the ~40 `tools/` probes
catalogued into a suite, the open gaps, and how to add a test. Everything below
this line in WS3 is the session-by-session record that produced it; read the
charter first.

### Open items carried into the new stage

1. **The `DDI refusals:` counters must reach 0 against real workloads.** Two are
   known to move under 3DMark and each names a real gap:
   `gs_so_declaration_dropped` and `tess_sig_fallback`. Definition of done is a
   3DMark standard run plus a desktop session with every counter at 0, read
   through `tools/umd-gate-surface.ps1`.
   ⚠ Two corrections found while writing `CONFORMANCE.md`: the line carries
   **eleven** counters, not the nine this document used to claim (R1010 added
   `alloc_meta_format_unknown` and `readback_stride_unsafe`) — and **the
   noop-DDI hit counter, which CLAUDE.md names as the headline WS3 metric, is
   currently unreadable**: `DEVICE_NOOP_LOG_COUNT` is incremented and loaded by
   nobody, with no summary line and no gate pattern. Making it readable is
   backlog item C1 in `CONFORMANCE.md` and is a prerequisite for the rest of
   this item.
2. **3DMark Fire Strike reports `103 Display Mode List not found for given
   format` and `402`** on a failed 2026-07-24 run
   (`3DMark-Firestrike-FAILED-20260724221433.3dmark-result`). This was sitting
   in a scratch file at the repo root rather than in the roadmap; it is a real
   DXGI mode-enumeration conformance datapoint and belongs to this workstream.
   Not reproduced since — first job is to establish whether it still occurs.
3. **DXGI format coverage audit** — the format round-trip carrier landed; the
   coverage matrix does not exist.
4. **Remaining 11.1 DDI plumbing.** The threading/command-list surface is now
   real and on by default (see the 2026-08-05 stage-pivot note), which changes
   what "remaining" means — re-survey before planning.
5. **FL11 MSAA** — status recorded below as PARTIAL with un-deployed WIP
   (`ff14979`). Verify whether that is still true before treating it as open.
6. ✅ **RESOLVED 2026-08-06 — `kmd_render` had five `#[test]` functions that could never run**
   (`present_stream_tests` in `src/virtio/gpu/mod.rs`): the crate is a
   `panic=abort` no_std cdylib and cannot host a libtest harness, and CI runs no
   `cargo test` at all. They were assurance that is not real. They and the pure
   helpers they cover were moved into `kmd_logic` as
   `helios_kmd_logic::present_stream_boundary_tests`, beside the
   `present_stream` module; `grep -c 'cfg(test)'` over `kmd_render/src` is now
   **0**, and the only trace left in `gpu/mod.rs` is the note recording the move
   (*"Do not reintroduce tests in this file"*). ⚠ **The count is five, and three
   places disagree about it** — `git show 3e750c0:…/gpu/mod.rs` counts **5**
   `#[test]`s, which is what this item and `gpu/mod.rs`'s note say and what
   `PENDING.md`'s wave-1 correction #5 established against `PENDING.md` §6's
   "six". But `present_stream_boundary_tests` now holds **six**: the move
   recovered five and **added one** (`slot_63_and_new_generation_never_alias`),
   which is why `kmd_logic`'s own doc comment says *"These six tests lived in
   `kmd_render`"* — that sentence is wrong about provenance, not about arithmetic.

## Workstream 4 — D3D12  ← **PRIORITY 2 since 2026-08-05**

**The charter is `DX12.md`; the implementation set is `docs/dx12/`.
`docs/dx12/DECISIONS.md` is authoritative over both for ARCHITECTURE, and
⭐ `docs/dx12/METHOD.md` is authoritative over both for SEQUENCING.**

⛔⛔ **HOW THIS WORKSTREAM IS WORKED CHANGED ON 2026-08-06 (owner directive).** The
probe-driven loop — implement a bit, run a probe, repeat until it passes, discover the
contract violations only when a later probe trips over them — is **retired**. The loop
is now: implement a whole subsystem to its contract (UMD + KMD + ICD + engine
together) → adversarial review of the entire changeset, fanned out by lens with every
finding refuted before it is routed → repair → **repeat until saturated** → then
deploy. `METHOD.md` §3 states the saturation test; §2 Phase 4 records that a BSOD or a
dead DWM is a diagnosis on a dev box and therefore that **fear of a crash may not shape
the implementation**.
⇒ Every `D12-G*` gate named below is now an **acceptance** step, not a driver of work,
and `GATES.md` is demoted accordingly. ⛔ A gate passing is not evidence the code is
right: with `Umd12EclDelayUs=50000`, `D12-G8` rung 0 **passed** with correct pixels
while the fence wait stayed 0.6 µs and the dependency it existed to prove was absent.
⇒ **The target is real D3D12 applications and benchmarks** (owner: *"Rendering triangle
is useless … unless we can render REAL DX12 apps and benchmarks"*), so a rung that
renders something is not a milestone unless a real workload follows it.

### ⭐⭐ THE GOAL, set by the owner 2026-08-06 — three deliverables, in this order

> **1. Visible D3D12 pixels the owner can see. 2. Time Spy success. 3. Port Royal success.**

Not a triangle, not a rung, not a green suite. `docs/dx12/PENDING.md` is the full gap inventory;
this is the **critical path through it**, and the ordering is forced by dependencies rather than
chosen. `docs/dx12/METHOD.md` governs *how* each stage is worked (implement the whole subsystem to
its contract → adversarial review → repair → saturate → deploy).

**Stage 0 — repair the fence bridge. Nothing runs until this is done.** It landed 2026-08-06,
has **never executed**, and has six known defects (`PENDING.md` §1). Two are blocking by
themselves: **A1**, a `pthread_cond_wait` with no timeout that hangs the app's own thread inside
`pfnExecuteCommandLists` whenever a queued `Wait` precedes the drain — every real engine does this,
and no probe can reach it; and **A5**, the adapter-global head-of-line FIFO, which decides whether
**the desktop survives the first D3D12 frame**. Plus **A4**, the prefix watermark, which violates
the *"frame's OWN boundary"* invariant.

**Stage 1 — visible pixels, WINDOWED.** ⭐ **Windowed D3D12 needs no KMD present work at all** —
the `PRESENT_FLAGS_HISTOGRAM` census (`FlOvf = 0`, only `0x1` and `0xC` ever seen; `RedirectedFlip`
zero occurrences in `kmd_render`) shows a DWM-composited flip-model present never reaches
`DxgkDdiPresent`. So Stage 1 is entirely UMD-side identity: `PENDING.md` §S-3 items 1→6, in that
order, because item 1 (venus-exportable memory) is a **hard prerequisite** — nothing in this driver
can obtain a non-zero venus resid today. **No `DIRECT_SCANOUT` bit, no stride question, no 0ab
machinery.** ⚠ Evidence trap: a maximized or promoted window is **absent** from `helios_paintcap`,
so keep the window windowed and partially overlapped, or use the VNC samplers.

**Stage 2 — Time Spy.** Everything in Stage 1, plus, and any one of these makes it fail:
`S-1` the **KMD's GPU clock** (zero-filled today ⇒ the score is zero regardless of correctness);
`S-2` **cross-queue sync**, which does not reach the engine at all — Time Spy has an async-compute
subtest, and a kernel-only `Wait` orders **nothing** here, so this is wrong pixels not slow ones;
`S-4` **`ExecuteIndirect`**; `S-5` **binding tier 3 + heap tier 2**. ⚠ And the two largest
*unquantified* risks land here, because Time Spy is the first workload to touch them: the
**map/upload path** (`MapHeapCalls = 0` — never run, and it rests on a whole-heap span buffer its
own code flags as the riskiest construct in the lane) and **8 of 15 descriptor slots** (~95 % of
view translation, every cube/array/3D/MSAA/mip-subrange arm — never executed once).
⚠ **Decide windowed-vs-fullscreen early.** Fullscreen re-introduces the `DIRECT_SCANOUT` bit, the
frozen host **stride agreement** (`align(width*bpp,256)`, unestablished for an ordinary OPTIMAL
vkd3d back buffer — getting it wrong is a *sheared* picture, not a failure), and the whole 0ab
family. Prove windowed first. ⚠ The runner cannot extract a non-Fire-Strike score
(`tmp/perf/run-fs.ps1:149` greps Fire-Strike-only label keys) — a Time Spy run through it is
indistinguishable from the "score = 0 with a result file" failure.

**Stage 3 — Port Royal. A new subsystem: DXR.** ⭐ **The substrate is CONFIRMED capable** — the
guest exposes `VK_KHR_acceleration_structure`, `ray_tracing_pipeline`, `ray_query`,
`deferred_host_operations` and `pipeline_library` (`research/guest-vulkaninfo-full.txt:1010-1057`),
and the engine measures **`RaytracingTier = 11`, i.e. DXR 1.1** (`baselines/d3d12-caps.csv:42`).
⛔ The driver reports NOT_SUPPORTED, and **not by choice**: the shader-model list stops at 6.0, and
that one short list forces raytracing, mesh shaders, VRS and sampler feedback off as a family
(`PENDING.md` §5). So Stage 3 is: raise the SM list to ≥ 6.3, report the measured tier, then
implement the ten raytracing slots — state objects, acceleration-structure build/copy/prebuild-info,
`DispatchRays`, shader identifiers and local root signatures — plus acceleration-structure resource
state and shader tables. **L**, and it depends on every stage above it.

State, so this file is not silent on it:

- **The strategy is CLOSED (2026-08-05):** Helios ships a real D3D12 UMD,
  `helios_umd12.dll`, implementing `d3d12umddi` and forwarding into vkd3d-proton's
  `ID3D12*` COM objects — the D3D11 architecture with DXVK swapped for vkd3d and
  `UserModeDriverName[2]` for `[3]`. ⛔ There is **no app-facing vkd3d arm**
  (owner directive, DECISIONS D2): Helios never ships or measures vkd3d's
  `d3d12.dll` as an application's D3D12.
- **P0 is complete (2026-08-05) and it was the load-bearing risk.** `D12-G0`
  (mingw cross build, `--list-tests` = 557) and `D12-G1` (the headless bridge
  probe: `helios_vkd3d_create_device` → DXIL SM 6.0 triangle → `READBACK` →
  exact pixels, 28 steps / 0 failures) both green on the first run.
  ⇒ **vkd3d demonstrably runs on venus**, which is what stood between a wrong
  assumption and ~200 DDI slots written on top of it.
- ⭐ **P2 is complete (2026-08-06): the engine is proven in its SHIPPING shape,
  and `umd_common` + `umd12` are built out to S3.** Four things landed, each with
  its evidence under `tmp/dx12/gates/`:
  - **`D12-G1` re-run against the STATIC clang-cl archives and PASSED**
    (`G1-static/RESULT.md`). The earlier pass was against the *mingw* DLL; D4's
    shipping artifacts had never drawn a pixel. Same probe source, one
    `-DHELIOS_G1_STATIC`, so the 28 steps cannot drift apart — normalised, the
    whole diff between the two arms is six lines of banner. ⭐ Plus the assertion
    the DLL arm could never pass: **the probe imports no `dxgi.dll`**.
    ⭐ **Measured minimal link set, which `umd12/build.rs` hard-codes at S4:
    `libhelios_d3d12_static.a` + `gdi32.lib`** — one archive, because it is a
    union of every vkd3d / dxil-spirv / dxbc-spirv object; ⛔ never `dxgi`.
    ⚠ It cost a fork fix: the archive was **not** self-contained as D4 claimed
    (D4 checked one symbol name, `CreateDXGIFactory`, and never asked about any
    other). Linking it alone gave 19 unresolved externals, five of them
    `vkd3d_debug_control_*` predicates `libvkd3d` calls unconditionally and which
    lived in the one object the static target omits. Fork `8ee4440b` splits them
    into `libs/d3d12core/debug_control.c`.
  - **S1 + S2 complete ⇒ `DECISIONS.md` D3b is done.** `slot`, the three shared
    C++ bridge headers, `log` (with `init(basename)`), `knobs`, `refusals` and
    `noop` all live in `umd_common`. Both stages carry the full check list —
    Fire Strike 3-run medians at parity (S1 GT1 220.10/GT2 211.85; S2 GT1
    222.90/GT2 206.07 against a ~221/208 baseline) and a **cold boot** with
    **zero id-1000 events of any kind**. S2's headline: `log_knob_inventory()`
    comes out **byte-identical**, one SHA256 across the pre-move DLL, the
    post-move DLL and the cold boot.
  - **S3: `d3d12umddi.h` is bindgen'd with `layout_tests(true)`** — 1 904
    assertion blocks, 102 874 lines, 399 `PFND3D12DDI*`, 15 s cold. Closes
    `UNVERIFIED-2`: it does not hurt, do not narrow the allowlist. ⭐
    `helios_umd12.dll` is **104 960 bytes, byte-for-byte what it was before** —
    5.4 MB of generated ABI, zero bytes shipped, because nothing references it.
  - ⛔ **`OpenAdapter12` still refuses**, in both DLLs, and must until S5.
    Nothing was deployed for D3D12: no INF, no registry, no `UserModeDriverName[3]`.
- **The substrate ceiling is measured, not predicted: FL 12_2 and SM 6.8.**
  `DECISIONS.md` H5 — whether vkd3d's `maintenance7` layered-`driverID` swizzle
  fires on venus — was the one open question that moved the ceiling, and
  `tools/vk_layered_driverid_probe.cpp` answered it: the nested
  `VkPhysicalDeviceDriverProperties` carries `NVIDIA_PROPRIETARY`. Confirmed on a
  live device at G1 (`ResourceBindingTier 3`, `TiledResourcesTier 4`,
  `ConservativeRasterizationTier 3`, `RaytracingTier 1_1`,
  `TypedUAVLoadAdditionalFormats 1`).
- `umd/src/adapter.rs` and `umd12/src/lib.rs` both export `OpenAdapter12` and both
  **still refuse**, and must keep refusing until the commit that makes the body
  reachable (R908) — that is **S5**, where `umd` drops its export, `umd12`'s
  becomes reachable, the INF registers slot 3 and the `UmdD3D12` kill switch lands,
  all in one commit. No D3D12 DDI code exists yet. The earlier scaffolding
  (hand-written `D3d12Ddi*` structs, eight `d3d12_*` handlers,
  `D3D12_SUPPORTED_DDI_VERSIONS`) was deleted by T6/R908 and this starts from zero
  rather than from a half-built surface.
- ⭐ **S4 is COMPLETE (2026-08-06): the engine has drawn a pixel from inside
  `helios_umd12.dll`.** `bridge/vkd3d_bridge.{h,cpp}` + `src/bridge12.rs` carry
  `helios_vkd3d_create_device` and `helios_vkd3d_serialize_root_signature` — both
  needed, because the probe's root signature is unbuildable without the second —
  through the shared `bridge_guard`, with `build.rs` linking the measured set
  (one archive + `gdi32`, never `dxgi`).
  - **`D12-G1` now has a third arm**, `-DHELIOS_G1_UMD12`, in the *same* probe
    source: 28 steps, 0 failures, pixels exact, `device final Release() -> refcount
    0`. `tmp/dx12/gates/G1-umd12/RESULT.md`. What it proves that the static arm
    could not: the engine works inside a Rust `cdylib` with `panic = "abort"`,
    `lto = "thin"`, cxx glue and the MSVC CRT — a different artifact from a probe
    `.exe`. `arm-diff.txt`: only steps 01–03 differ (prologue + the per-boot LUID);
    **steps 04–28 are byte-identical**.
  - ⭐ `helios_umd12.dll` (4 124 672 B) imports **no `dxgi.dll`**, and of its 154
    exports the 149 `cxxbridge1$…` are cxx's own leaked ABI (ARCHITECTURE §6.3
    predicts them) — the other five are exactly `DllMain`, `OpenAdapter12` and the
    three `helios_umd12_probe_*_v1`. **Zero `helios_umd_*` names**, so the Mesa
    ICD's first-hit-wins module walk cannot mistake this DLL for the D3D11 vehicle.
  - ⛔ Still nothing deployed: no INF, no registry, no `UserModeDriverName[3]`, and
    `OpenAdapter12` refuses in both DLLs.
  - ⚠ Two things S4 changed in the surrounding tooling because it had to:
    adding `cxx` pulls `link-cplusplus`, whose build script dies cross-compiling
    from Linux (`failed to find tool "lib.exe"`) and would have taken the host
    cross-check away from every S6 lane — fixed with cargo build-script overrides
    in `tools/umd12-host-check.sh`; and the `static_assert` invariant check had
    been counting its own documentation (reporting 3) since before P2.
- ⭐ **S4b is COMPLETE (2026-08-06): one venus ICD module per process, whichever UMD
  loads first.** Both cdylibs export `helios_icd_anchor_v1`; the module walk moved to
  `umd_common/bridge/bridge_icd_anchor.{h,cpp}` (ONE source compiled into both — that
  is the mechanism, not duplication); a mismatch **refuses** device creation rather
  than adopting the other module, and counts `IcdAnchorMismatch`.
  - **Gate: 18 steps, 0 failures, in BOTH load orders** (`tmp/dx12/gates/S4b/RESULT.md`).
    Both DLLs' logs name the same ICD; ⭐ the **publisher** changes with load order and
    the **answer** does not.
  - ⛔ **A correction to `ARCHITECTURE.md` §6.4, settled by the run.** §6.4's criterion
    *"both venus context ids non-zero and EQUAL"* is **wrong**: each engine builds its
    own `VkInstance`, the ICD mints a context per instance, and
    `helios_venus_current_ctx_id` is last-writer-wins — so `normal` order gives 23/23
    and `reverse` gives 25/27 **on one ICD module**. Equality is an artifact of
    ordering; the invariant is one ICD **module** per process. §6.4 not edited — owner
    call.
  - ⚠ The deploy this needed faulted four live Vulkan clients (Explorer, dwm,
    SearchHost, ApplicationFrameHost, `0xc0000005`) **inside the venus ICD**
    (`vulkan_virtio-*.dll`) when `-RestartDevice` removed the PCI device. Pre-existing
    ICD fragility at device removal, not a UMD regression — no id-1000 names
    `helios_umd`, and the desktop was verified composited afterwards. Worth a stability
    item of its own.
- ⭐ **S5 is COMPLETE (2026-08-06): `helios_umd12.dll` holds `UserModeDriverName[3]`
  and `OpenAdapter12` no longer refuses.** ONE commit, because `DECISIONS.md` §7.1 /
  R908 makes atomicity non-negotiable: the eight `D3D12DDI_ADAPTERFUNCS_0109` slots,
  the `UmdD3D12` kill switch (default OFF), the deletion of `umd`'s duplicate
  `OpenAdapter12` export, the four `.inx` edits and the `cargo make` staging of the
  second UMD all land together.
  - **`D12-G6` PASSES** (`tmp/dx12/gates/G6/RESULT.md`): four `UserModeDriverName`
    entries with `[3]` on the deployed `helios_umd12`, `InstalledDisplayDrivers` the
    two-entry form, `D3D12CreateDevice` → `0x887A0004` with the knob absent, zero
    `helios_umd*` id-1000s, desktop composited, and `umd`'s rustc warning count
    **15 → 15** measured by reverting `adapter.rs` alone.
  - ⭐ **`ARCHITECTURE.md` §13 UNVERIFIED-1 is CLOSED — slot 3 IS served
    independently.** The D3D12 client loaded the slot-3 DLL by its content-addressed
    name and logged to its own `umd12-<pid>.log` with `OpenAdapter12=1`, so the
    refusal the app saw is **ours**, not DXGI's generic answer.
  - ⛔ **Two things the doc set had backwards or open, corrected by the knob-ON run.**
    `pfnGetCaps` is called **BEFORE** `pfnGetSupportedVersions` (§1.2 had it the other
    way): `GetCaps(1074)`, `GetCaps(1007)`, then the two version calls. ⇒ **the caps
    answer cannot depend on a negotiated version, because there is not one yet**, and
    refusing 1074/1007 aborts device creation two calls in — which is why `D12-G7` is
    not reachable until L1 lands. And `pfnGetSupportedVersions` really is the two-call
    count-then-fill pair (`DDI_REFERENCE.md` §1.3's UNVERIFIED, closed without needing
    the §15 spy).
  - ⚠ The DriverStore package still carries no `helios_umd12.dll`: the INF and the
    packaging task are committed, but the `win_build_kmd` + `win_install_kmd` + reboot
    that publishes them is deliberately deferred until after S6-0, so one boot
    validates a device that can actually be created. A cold boot has no D3D12 UMD
    until then — harmless while the kill switch is off.
- ⭐ **`DECISIONS.md` D12 (2026-08-06): the DDI version is `_0110`, advertised as
  exactly ONE token, filling the `_0109`-generation tables.** `PARALLEL.md` §8's last
  not-parallelisable decision, made before the fan-out. One token means the runtime
  either negotiates `_0110` or fails the handshake, which makes §12 trap 2's closed
  enum exhaustive with a single legal arm and a wrong-sized table fill
  *unrepresentable*. The thirteen `VulkanOn12` obligations are accepted and become
  **lane** obligations — each one a lane cannot honour gets a named refusal counter.
- ⭐ **`DECISIONS.md` D13 (2026-08-06, owner): private data that CROSSES a module
  boundary is declared once, in `helios_protocol`.** The requirement traces to
  `DX12.md` §4.3 row 6 → D3c → `ResourceHeaps.md:198` (*"private data … consumable by
  their D3D11 driver"*), and the thing it names **already exists**:
  `HeliosWddmAllocPrivate` (`'HWDM'`) and `HeliosWddmOpenIdentity` (`'HIDN'`) in
  `protocol/src/wddm.rs`, already read by `umd`, `kmd_render` and the Mesa ICD. L4 and
  L8 reuse them **verbatim** — same struct, same magic, same version — which discharges
  D3c in code. ⛔ `umd_common` would have been the wrong home: `kmd_render` is `no_std`
  and does not depend on it. Per-object `pDrvPrivate` blocks stay local and typed.
- ⭐ **S6-0 is COMPLETE (2026-08-06): all 206 device / command-list / queue slots carry
  PER-SLOT counting noops, and the eleven-lane sequencer is written.**
  - **Per-slot, not per-table**, because `PARALLEL.md` §9.2 makes *"its noop hit
    counters read zero for its slots"* a per-lane definition of done and
    `CONFORMANCE.md` reads the same instrument. One const-generic
    `slot_noop<TABLE, SLOT>` monomorphises 206 times, and **the slot ordinal is
    `offset_of!(Table, field) / 8`** — derived from the ABI, so a mis-ordered name list
    cannot mis-attribute a hit.
  - ⭐ **The compile-time ABI-order proof is the real deliverable**: per table,
    `OFFSETS.len() == size_of::<T>()/8` and `OFFSETS[i] == i*8` for every `i`, on every
    build of either platform. That is `DECISIONS.md` §4.1's "slots 38-40" scar — a
    `sed` line offset misread as a member index — made unrepresentable.
  - ⭐ **Install order is structural**: `Filling<'a, T, Stage>` is `#[must_use]`, carries
    the `&mut`, and each lane's `install` names the previous lane's marker. S6-0 wrote
    the whole chain, so a lane's diff against `tables12.rs` is **empty** — fewer merge
    points than §5's original one-line-per-lane protocol, not more.
  - **Evidence: 40 steps, 0 failures** (`tmp/dx12/gates/S6-0/RESULT.md`). Driven by
    `tools/d3d12_fill_table_probe.cpp` through two probe exports, because the runtime
    cannot reach `pfnFillDDITable` until L1 answers caps. It poisons a buffer, asks for
    `size − 8`, and checks the **guard band is untouched** — the R702 failure a
    prefix-only test cannot see.
  - ⭐ **And the sizes came back 992 / 600 / 56**, exactly what `D12-G5` measured this
    runtime handing WARP at `_0110`: the bindgen structs are byte-identical to what the
    runtime negotiates, confirming D12 from the driver's own side.
- **S6-0b is COMPLETE (2026-08-06): `device12.rs`** — the private block,
  `pfnCalcPrivateDeviceSize` / `pfnCreateDevice` / `pfnDestroyDevice`, the engine
  device, and the `DeviceUnderConstruction` unwind guard. Validate-before-construct,
  one function of `Flags` for the size, the corelayer union arm fixed at `_0062` by
  D12's one-token set, and a per-device teardown readout that makes the block's fields
  genuinely *read* rather than merely stored (the R908 rule forces that choice).
- ⭐ **L1 (caps) is LANDED (2026-08-06), and `D3D12CreateDevice` now builds a real
  vkd3d `ID3D12Device` through the DDI.** `pfnGetCaps` answers all 43 types: the ~13
  with an explicit "device creation fails" runtime string individually, the rest by
  §11.2's measured safe default. The 43-way policy was derived and then
  **adversarially verified**, three lenses per risky answer; eight drew a refutation
  that survived and two changed the code.
  - ⛔ **The load-bearing decision is a COUPLING: this ships FEATURE LEVEL 11_0**, and
    every OPTIONS tier is legal only because of that. The level is asserted by the
    driver, never inferred, and asserting 12_0 arms cap floors that are lies on a
    driver whose descriptor/resource/recording lanes are counting noops — `D12-G5`
    measured that exact failure. Each raise belongs to the lane that earns it and must
    move the level and its floors **together**.
  - ⛔ **Four caps where the §11.2 zero-fill default is ILLEGAL**, which is the least
    obvious thing in the lane: `1002` (`IOCoherent` must be TRUE on amd64), `1004`
    (zeroed lane counts fail device creation), `1003` (zeroed alignments are four
    separate errors), `1088` (`EXECUTE_INDIRECT_TIER` has no zero enumerator, so a
    zero-fill writes an out-of-range tier the runtime clamps **silently**).
  - ⛔ ~~**`MaxSamplerDescriptorHeapSize` is 2048, not `SUBSTRATE.md` §4.5's ">= 4000".**~~
    **FALSIFIED 2026-08-06 by the runtime itself, at `D12-G7`.** ETW
    `Microsoft-Windows-Direct3D12`: `Driver's MaxSamplerDescriptorHeapSize is too small`
    (strings:113) with 2048. **§4.5 was right and this "correction" was a LAYER
    CONFUSION**, which is the part worth keeping: both arguments for 2048 —
    `D3D12_MAX_SHADER_VISIBLE_SAMPLER_HEAP_SIZE` and
    `baselines/d3d12-caps.csv:85` — are **API-level**, and the runtime is what clamps
    the DDI value down to them. `d3d12_caps_dump.cpp` reads the *post-clamp* number
    through the API, so it could not have disagreed with 4000 whatever the driver
    reported. ⇒ **an API-level capture cannot falsify a DDI-level requirement.** The
    value is now 4000, which is also exactly the guest's `maxSamplerAllocationCount`.
  - ⛔ **Refusing an unknown `pfnFillDDITable` type LOSES THE DEVICE.** The runtime asks
    for `D3D12DDI_TABLE_TYPE_0096_EXTENDED_FEATURES` (27, 32 B) on a baseline device.
    Unknown tables are now stub-filled at the runtime's own byte count and counted —
    filling selects no *shape*, which is what §7.4 actually forbids, while a refused
    table has NULL slots the runtime may still call through.
- **`D12-G7`'s FIRST failure, and the blocker it named — now CLOSED by L1's second
  half below, kept because the chain it measured is still the reference**
  (`tmp/dx12/gates/G7/RESULT.md`). The whole chain runs: `OpenAdapter12` → caps →
  versions → `CalcPrivateDeviceSize` → **`CreateDevice` building a real vkd3d device on
  venus ctx 19** → all four table fills at **992 / 600 / 56 / 32**, with the
  command-list table filled **twice** and both `hRTTable` handles (`0x3E0`, `0x638`)
  stashed → `DestroyDevice` → `CloseAdapter`. It fails at **`0x887A0020`**, which is the
  runtime rejecting an inconsistent caps **set** — not our own `0x887A0004` refusal.
  ⭐ The HRESULT moving is the result: the failure went from *"the driver said no"* to
  *"the driver said something wrong"*.
  - ⇒ **The blocker is three device-core slots still counting noops.** The runtime calls
    them **2 824 times inside `D3D12CreateDevice`**: `pfnCheckFormatSupport` 93 times
    (the 91-format sweep §11.1 predicted), `pfnCheckMultisampleQualityLevels` **2 730**,
    `pfnQueryNodeMap` once. A noop returns 0, i.e. *"no format supports anything"*.
    They need a `bridge12` entry point into `ID3D12Device::CheckFeatureSupport`
    (C++, VM-only). ⭐ `umd/src/forward/format_caps.rs` is the D3D11 precedent and its
    `D3D10_DDI_FORMAT_SUPPORT` bits are **identical** to `D3D12DDI_FORMAT_SUPPORT`'s —
    including the trap: `DXGI_FORMAT_R10G10B10_XR_BIAS_A2_UNORM` (89) must be refused
    with the explicit `_NOT_SUPPORTED` sentinel `0x8000_0000` and **not a bare 0**,
    which the D3D11 runtime rejected with the *same* `0x887A0020` on the same box.
- ⚠ **A UMD path change needs a device restart to take effect.** A deploy without
  `-RestartDevice` rewrote `UserModeDriverName[3]` and the next new process still loaded
  the previous content-addressed DLL: dxgkrnl caches the resolved UMD path. Cost one
  confusing gate run whose log showed the old hash and the old counter names.
- ⭐ **L1's SECOND HALF is LANDED (2026-08-06, `2c7460e`): the format/MSAA slots are
  real and `D3D12 noop DDI hits:` reads `slots=0/206`.** Every one of the 2 824 calls
  the runtime made into counting noops inside `D3D12CreateDevice` now reaches a body —
  `pfnCheckFormatSupport`, `pfnCheckMultisampleQualityLevels`, `pfnGetMipPacking`, plus
  `pfnQueryNodeMap` and `pfnGetImplicitPhysicalAdapterMask` in L9's file, landed early
  because the same sweep needs them. Full write-up: `tmp/dx12/gates/G7/RESULT.md`.
  - ⭐ **No cxx bridge was needed, and that widened the fan-out rather than narrowing
    it.** The handoff specified a C++ module into `ID3D12Device::CheckFeatureSupport`;
    `bridge12` already hands Rust a borrowed `ID3D12Device`, so the `windows` crate's
    vtable call reaches the identical slot. ⇒ the lane type-checks on the **Linux
    host** (`PARALLEL.md` §7), which the C++ route would have taken away. The added
    `Win32_Graphics_Dxgi_Common` feature is types only: `dumpbin /IMPORTS` on the
    release DLL is unchanged — **no `dxgi.dll`, no `d3d12.dll`**.
  - ⭐ **MEASURED: `pfnCheckFormatSupport` writes the small `D3D12DDI_FORMAT_SUPPORT`
    enum, NOT API-level `D3D12_FORMAT_SUPPORT1`.** This had to be settled by experiment
    rather than by reading the header, because the D3D11 side of this project holds the
    *opposite* result for its own DDI (`umd/src/forward/format_caps.rs:15-19`: "D3D11
    harmonized the DDI with the API enum … translating regresses even a plain
    `D3D11CreateDevice`"). `Umd12FormatCaps=1` (API passthrough) truncates the runtime's
    format sweep at **12 formats / 271 MSAA queries** against **23 / 600** for the DDI
    encoding. Arm 0 is the default; arm 1 stays reachable (CLAUDE.md rule 8).
- ⭐⭐ **`D12-G7` PASSES (2026-08-06, `23fbf44`): a real `ID3D12Device` exists on the
  Helios adapter, built by the D3D12 runtime through Helios' own `d3d12umddi`
  implementation on top of vkd3d on venus.** `D3D12CreateDevice` → `S_OK` at FL 11_0,
  `nodes=1`, `final Release()` → refcount 0. The runtime then builds its **own** objects
  through the DDI — a root signature, two graphics PSOs, a command pool, the
  extended-features handshake — and reports
  `UMAdapterVersion = UMDeviceVersion = 0xC0050006E0000`, D12's single `_0110` token
  negotiated end to end. Full write-up: `tmp/dx12/gates/G7/RESULT.md`.
  - ⭐ **The fix was already in this repository, in the D3D11 driver.** Six
    build/deploy/run cycles were spent bisecting the runtime's per-format contract
    before the answer turned out to be written down:
    `umd/src/forward/queries.rs:104-164` does **not** forward the engine's
    quality-level answer, it *derives* it from the same predicate that decides the
    format-support multisample bits, *"because the Microsoft runtime validates
    `CheckFormatSupport` and `CheckMultisampleQualityLevels` as a coherent
    feature-level contract"*. ⛔ **Two independent engine queries make a coherent pair
    a coincidence; one shared predicate makes disagreement unrepresentable.**
  - ⛔ And the predicate rests on `umd_common/src/format.rs`, whose `msaa_ineligible`
    field doc is the entire answer to where the sweep was stopping — the depth/stencil
    **read** views `R32_FLOAT_X8X24_TYPELESS` (21), `X32_TYPELESS_G8X24_UINT` (22),
    `R24_UNORM_X8_TYPELESS` (46), `X24_TYPELESS_G8_UINT` (47): *"WARP reports zero
    quality levels above 1x and the runtime rejects advertising them as MSAA render
    targets."* The sweep stopped at **21**, and four earlier arms each changed the
    format bits while still forwarding non-zero levels — so the one answer that works,
    *neither bits nor levels*, was never tried.
  - Two further answers the runtime named in English over ETW, one cycle each:
    `ROW_MAJOR_LAYOUT_SUB_CAPS::DepthPitchAlignment` **512 → 256** (strings:85 — the
    bound is *relative*: the identical 512 in `BaseOffsetAlignment` passes, because a
    depth pitch is `RowPitch * Height` and `RowPitch` is only `PitchAlignment`-aligned);
    and `OPTIONS_0102::MaxSamplerDescriptorHeapSize` **2048 → 4000** (strings:113).
  - **Counters, all expected-non-zero and documented as such:**
    `CapsFormatSupportCalls=93` and `CapsMsaaCalls=2730` — byte-for-byte what `D12-G5`
    measured the runtime handing WARP; `CapsTextureLayoutSetEnd=2` (the enumeration
    terminating as WARP's contract does); ⭐ `CapsFormatNotSupportedSentinel=1` — format
    89's `_NOT_SUPPORTED` trap discharged and **observed**; `CapsMsaaIneligibleFormat=124`;
    `CapsMsaaBitsDropped=4`.
  - **Gate criteria:** `HwQRef` never moved; the knob off restores `0x887A0004` exactly
    (`D12-G6` still passes); knobs deleted; zero id-1000 events naming `helios_umd`
    across ten device restarts (all 60 name the venus ICD — pre-existing); desktop
    composited afterwards with `dwm` started after the last restart; the D3D11 UMD hash
    byte-identical throughout.
- ⚠ **25 noop slots were hit on the passing run, and they ARE the fan-out's work list** —
  `DDI_REFERENCE.md` §14.0's prediction landing exactly: *"`D3D12CreateDevice` alone
  drives 27 of the 124 core slots"*, the runtime building its own internal pipelines.
  Blend / depth-stencil / rasterizer state, `pfnCalcPrivateShaderSize`,
  `pfnCreateVertexShader`, `pfnCreateComputeShader`, PSO create/destroy, root signature,
  command pool, `pfnMakeResident`, `pfnGetDebugAllocationInfo`. Those are **L6**, **L2**
  and **L4**, and driving them to zero is those lanes' definition of done
- ⭐⭐ **S6 ROUND 1 IS COMPLETE (2026-08-06): four DDI lanes landed and
  `D3D12 noop DDI hits:` reads `slots=0/206`.** Every one of the 25 slots the passing
  `D12-G7` run hit now reaches a real body. Static coverage went **5/206 -> 105/206**
  (device-core 98/124, command-queue **7/7**, command-list 0/75). Evidence:
  `tmp/dx12/gates/G7-s6r1/RESULT.md`; commits `81cf82d` -> `1ce9939`.
  - **L2+L7** (30 slots, `forward12/queue.rs` + `fence.rs`) — queues, command pools,
    recorders, command lists, command signatures, fences, query heaps. ⭐ The WDDM-context
    question is settled **decisively and not by the doc**: the runtime enforces the scoping
    itself (*"CreateContextCb or CreateContextVirtualCb called outside of queue creation"*,
    fullstrings:10597), so a lane that skips it makes the object **unobtainable for every
    later lane**, L8's `pfnPresent` included. ⭐ And grepping the D3D11 driver confirmed
    what its own context is FOR — every use of `HeliosDevice::context` is present-path
    (`umd/src/forward/present.rs:786`), never submission — so §6.4's *"cardinality, not
    kind"* is confirmed from source. ⚠ The context class is **legacy**, which forecloses
    `pfnSubmitCommandCb`; the doc-set contradiction (D5/§9.2 say VIRTUAL) is recorded at the
    site with its cost rather than left for L8.
  - **L4** (16 slots, `forward12/resource12.rs`) — committed / heap / placed creates,
    map/unmap, residency, the four introspection slots. Cross-process sharing
    (`pfnOpenHeapAndResource`) refused with named counters: it is what discharges D3c and it
    needs `helios_protocol` verbatim, which an in-process triangle does not.
  - **L5** (15 slots, `forward12/descriptors.rs`) — heaps and all six view creates. ⭐ Found
    the fifteenth slot (`pfnCreateSamplerFeedbackUnorderedAccessView`, appended late to the
    `_0109` struct far from its siblings, which is why a reader counts 14). ⭐⭐ The
    `ead692e` struct-return hazard is **CLEARED with both halves quoted**: vkd3d's
    `resource.c:9146` takes a hidden out-pointer and windows-rs 0.58's vtable
    (`Direct3D12/mod.rs:652`) declares exactly that — they agree, no shim.
  - **L6** (38 slots, `forward12/pso.rs` + `shaders.rs`) — ⭐⭐ **the DDI's shader bytecode is
    unusable by vkd3d as delivered.** It arrives as a bare `DxilProgramHeader` and three
    engine readers reject a non-`'DXBC'` tag, so the lane **synthesises a DXBC container**.
    `DDI_REFERENCE.md` §12.3 had left this open; the gate closed it — `L6ShaderDxbcContainerSeen=0`
    on both shaders the runtime built. ⭐ And the `DepthBias` `INT`/`FLOAT` trap is resolved
    **by never converting it**: the pipeline-state STREAM's `RASTERIZER2` keeps it a
    `FLOAT`, which also carries mesh/amplification shaders the legacy struct cannot express.
  - ⭐ **The fan-out shape worked, and the pre-VM adversarial pass paid for itself twice.**
    Four lanes authored concurrently in isolated worktrees, each verified by an adversarial
    refuter and repaired before merge: that caught two **blockers** (`Slot::clear()` on a
    live heap; array-ness decided from `ArraySize` alone) and a missing null-descriptor arm
    that would have met a **legal** D3D12 call with `pfnSetErrorCb`, i.e. device removal.
    None of it cost a VM lease.
  - ⭐ **The `PARALLEL.md` §10 lens review then found six more**: 7 reviewers, 27 raw
    findings, **21 rejected** by an adjudicator that re-verified each against source. The
    survivors included a **blocker** — `pfnDestroyHeapAndResource` freed a heap it did not
    own, contradicting the create site's own guard, so the first `Release()` of any placed
    resource tore down its live parent heap — and the one enum proof of 21 that compared
    against a transcribed literal inside the block whose preamble says none are.
- ⛔ **TWO counters were graded for a world that had ended, in one merge, and that is now a
  thing to check rather than a coincidence.** L5's `ViewResourceUnavailable` still said
  *"expected non-zero until L4 lands"* after L4 landed in the same batch — where every hit
  now removes the device; and `DebugAllocationInfoEmpty` said *"a zero reading is the
  finding"* and then read zero on a healthy run. **A counter's grading is a claim and it
  goes stale like any other.** The first was caught by a reviewer, the second by the gate.
- ⚠ **`pfnGetDebugAllocationInfo` went from 4 calls to 0** between the two `D12-G7` runs, on
  otherwise byte-identical device creates (`Flags=0x0` both times). Why is **not
  established** and is recorded as unknown: either the four calls were the runtime reacting
  to the noop'd path (a zero-byte PSO private block), or the slot is debug-layer traffic
  enabled some other way. A run with the debug layer deliberately on settles it.
- ⭐ **`tools/umd12-slot-coverage.sh` is new**, and it exists because a slot with **two
  owners is silent**: the install chain runs lanes in order over one table, so the later
  wins and the earlier handler is unreachable — it compiles, both files look complete, both
  lanes report the slot done. Now an exit code. ⚠ It was wrong three times in one sitting
  (counted its own documentation; missed raw-pointer slots; missed rustfmt-wrapped
  assignments) before the matcher was rebuilt to join continuation lines. Its two remaining
  blind spots both over-report and are recorded at the site.
- ⭐⭐ **S6 ROUND 2 IS COMPLETE (2026-08-06): the command-list table went 0/75 -> 73/75 and
  static coverage is 203/206.** Only L8's three present slots remain. Commits `3682203` ->
  `4d263e3`. Evidence: `tmp/dx12/gates/G7-s6r2/` and `tmp/dx12/gates/G8-r0/RESULT.md`.
  - **The spine first**: `D3D12DDI_HCOMMANDLIST` promoted to a boxed payload carrying
    `h_device`, `h_rt_list` and the list class. Every one of the 75 command-list slots takes
    that handle and **nothing else**, and 74 of the 75 return `VOID`, so the handles a
    recording failure needs can only be captured at `pfnCreateCommandList`.
  - **Four lanes, authored concurrently in worktrees and adversarially verified before the
    VM**: **L3a** `cmdlist.rs` 23, **L3b** `rootargs.rs` 21, **L3c** `copy.rs` 13, **L9**
    `misc.rs` 44. Then the `PARALLEL.md` §10 lens review: 7 lenses, 20 raw findings on top of
    **135 the lenses refuted themselves**, adjudicated to **12 confirmed**.
  - ⛔⛔ **The two blockers were both DEVICE REMOVAL ON A LEGAL CALL, and both came from the
    spine rather than from a lane.**
    1. **The whole recording surface was reporting on the wrong callback.** The spine wrote
       *"a recording failure can only be reported through the device-scoped `pfnSetErrorCb`"*
       and three lanes copied that sentence into 49 call sites. It is false:
       **`pfnSetCommandListErrorCb` sits one field BELOW `pfnSetErrorCb`** in the same
       `_0062` struct this driver already reads `pfnSetCommandListDDITableCb` out of, so it
       was reachable the whole time. The spec: *"the runtime will drop all calls into the
       driver which record commands on the specified command list"*
       (`CPUEfficiency.md:2143-2158`) — one list quarantined and the application told at
       `Close()`, which **is** D3D12's existing recording-error contract, against
       `pfnSetErrorCb`'s *"Removing device due to bad UMD error"*, which takes the whole
       device and the compositor with it if the device is DWM's. All 51 sites repointed;
       `device12::set_error` now survives only where it is correct.
    2. **Bundles succeeded at create and removed the device at reset.**
       `D3D12DDIARG_CREATE_COMMAND_RECORDER_0040` carries no bundle bit, so no BUNDLE
       `ID3D12CommandAllocator` can ever be minted and the paired reset was **structurally**
       guaranteed to fail. `pfnCreateCommandList` refuses `Type == BUNDLE` up front now
       (`L2BundleListRefused`), so the application gets a failed create instead of a dead
       device. Durable fix named at both sites: one allocator per (pool, class).
- ⛔⛔ **The obligation Round 1 routed to L3a was already discharged BY THE ENGINE, and the
  document that predicted it was what made it look open.** `SUBSTRATE.md` §4.5 is right that
  the `DYNAMIC_*` PSO flags are hints and the baked depth bias and strip-cut must be
  re-applied on every `pfnSetPipelineState` — but `d3d12_command_list_SetPipelineState` does
  exactly that (`libs/vkd3d/command.c:12711-12733`, *"For any optionally dynamic state, we
  need to re-apply the corresponding static state that the PSO was created with"*).
  Forwarding discharges it; re-implementing would issue the state twice.
  ⇒ **Read the engine before spending a gate on an UNVERIFIED.** Three items this round were
  settled that way and none needed a run.
- ⛔ **`DX12.md` §4.3 row 4's second "silent ABI hazard" was FALSE and is struck.**
  `D3D12DDI_ROOT_CONSTANTS` and `D3D12_ROOT_CONSTANTS` are field-order **identical** in
  `d3d12umddi.h:1310`, `d3d12.h:4016` and the Win32 metadata. It had reached a lane's brief.
  The other half of that row (descriptor-heap flags colliding on `0x1`) is real and stays —
  ⚠ a half-true row is worse than a false one, because the true half lends it credibility.
- ⛔ **The recurring shape, third merge running: a claim written when it was true, left
  standing after the code moved under it.** This round it hit a SAFETY argument, three
  counter gradings and two `file:line` citations — and **one grading went stale inside the
  merge that corrected it**, because it named a condition a later commit in the same batch
  removed. L2's own append-only refusal-array rule was violated by the commit that quotes
  it. ⇒ when a grading is a cross-reference to another slot's state, check that state at the
  **end** of the merge, not at the moment of writing.
- ⛔⛔ **`D12-G8` RUNG 0 FAILED. ⚠ ITS FIRST ATTRIBUTION WAS WRONG; the corrected one is
  "the fence does not wait for the work", four bullets down.**
  `tools/d3d12_clear_probe.cpp` is new (`GATES.md` §4.9 has named it as rung 0 since it was
  written; it did not exist). It renders offscreen, copies back through a placed footprint
  and compares integers — no swapchain, no DWM, no shaders. Full account:
  `tmp/dx12/gates/G8-r0/RESULT.md`.
  - **WARP passes the identical sequence** (65536/65536 pixels exactly `(0, 51, 102, 255)`),
    so the probe is right and the defect is ours. That control arm is the whole reason this
    is a finding rather than an argument.
  - **Every DDI in the chain reaches the driver and is forwarded** — traced with
    `Umd12Trace=1`: `ResetCommandList` (`type=0 allocatorType=0`, classes matched),
    `ClearRenderTargetView`, `ResourceBarrier lowered=1`, `CopyTextureRegion`, `Close` S_OK,
    `ExecuteCommandLists`, `Signal` S_OK, fence `completed=1`, `Map` S_OK.
  - ⛔⛔ **THE 2026-08-06 ATTRIBUTION ABOVE WAS WRONG AND IS SUPERSEDED. THE GPU WORK LANDS;
    THE FENCE DOES NOT WAIT FOR IT.** Measured by the `--settle` arm,
    `tmp/dx12/gates/G8-r0-settle/`: on Helios `WaitForSingleObject` returns in **0.8–1.1 µs**
    against WARP's **561 µs**, the surface is **0/65536 exact at T+0** and **65536/65536
    exact at +2000 ms** (and again after an `Unmap`+`Map`), every mapped pointer is the same
    address throughout, and `GetDeviceRemovedReason` is `0` at every point. Three arms,
    identical result. ⇒ ***`EclNoWddmSubmission=1` is not a standing gap, it is the
    defect.*** The application's `ID3D12Fence` completes with **no causal dependency on the
    engine's Vulkan work**, so the probe maps and reads before the guest → venus → host copy
    has landed. Suspect (3) — ranked last — was the answer.
  - ⛔ **The `--sentinel` elimination of `pfnMapHeap` was an INVALID INFERENCE**, even though
    heap identity does happen to be sound. A CPU write/read round-trip proves the mapping is
    *self-consistent*; it says nothing about whether the GPU wrote those bytes. The same
    reading is produced by "the work has not landed **yet**", which is what was happening.
    (Identity is separately fine: both resources take the committed arm, so
    `HeapState::map_anchor` **is** the copy destination, and `pfnCheckSubresourceInfo`
    reports offset 0 for subresource 0.)
  - ⛔ **"vkd3d's own `WARN`/`FIXME` are compiled out of the static build" was FALSE, and the
    evidence was already on the VM.** `vkd3d-proton-helios/meson.build:53-55` defines only
    `-DVKD3D_NO_TRACE_MESSAGES`; `VKD3D_NO_DEBUG_MESSAGES` is defined nowhere in the fork, so
    only `TRACE` is out. Nothing appeared on stderr because
    **`umd12/bridge/vkd3d_bridge.cpp:216-244` redirects the engine's log to
    `C:\ProgramData\Helios\umd12-<pid>-vkd3d.log`**, and `tmp/dx12/run-g8r0.ps1:99` collected
    only `umd12-<pid>.log`. The failing run's own `VKD3D_DEBUG=warn` log is now at
    `tmp/dx12/gates/G8-r0/umd12-3040-vkd3d.log`: **zero `err:` and zero `warn:` from
    `ClearRenderTargetView`, `CopyTextureRegion`, `Close`, `ExecuteCommandLists` or
    `Signal`** — which is what refutes every silent-drop path in the engine, since each of
    them logs at WARN or ERR (`libs/vkd3d/command.c:22827`, `:10360`, `:10367`, `:24402`,
    `:24631`, `device.c:12047`) and `b_ndebug` defaults false so vkd3d's asserts are live.
    ⇒ *the log sink is part of the instrument; an absent output is not an absent finding.*
  - ⭐ **Settled for free by the same run: vkd3d's GPU work DOES flow out-of-band through the
    venus ICD with no WDDM submission.** That was recorded as proven for *DXVK on its own
    Vulkan device* only; a second `VkDevice` in the process now demonstrably renders and
    copies. `DDI_REFERENCE.md` §8.3's doubt on this point can be closed.
  - **The fix is a fence/completion bridge**, designed and costed in
    `tmp/dx12/FENCE-BRIDGE-DESIGN.md`. ⛔ The obvious route is **not buildable**:
    `D3D12DDI_FENCE` is `{FenceValue.BaseAddress, FenceMonitoredValue.BaseAddress, Flags}`
    (`d3d12umddi.rs:51094-51098`) with **no `D3DKMT_HANDLE` and no `hRTFence`**, both
    `BaseAddress`es arrive **0**, and every `pfnSignal*Cb` names its target by
    `D3DKMT_HANDLE` (`:20949-20953`, `:21053-21064`) — so the driver can never name the
    application's fence. ⚠ `DDI_REFERENCE.md` §10.4's row *"`pfnSignalFence` →
    `pfnSignalSynchronizationObjectFromGpuCb`"* is a reconstruction that cannot be
    implemented; four comment sites in `fence.rs`/`queue.rs` inherit it.
  - ⛔ **A SECOND DEFECT, downstream of the same root cause: prompt teardown WEDGES and loses
    the Vulkan device.** The one arm that tore down immediately after the fence wait (no
    `--settle`) never exited — 4 threads in Wait, and the only two `err:` lines of the whole
    round: `d3d12_command_allocator_Release: … still 1 pending command lists awaiting
    execution …! Deferring release`, `d3d12_command_queue_wait_idle: Failed to wait for
    virtual queue idle, vr -4`, then `d3d12_device_mark_as_removed: … VK_ERROR_DEVICE_LOST`.
    Nothing ties object lifetime to engine completion either, so an application that believes
    its own fence destroys objects with work in flight. ⚠ **Not deterministic** — the
    identical arm completed in the previous round — and **no host-side evidence**:
    `/tmp/helios-qemu-stderr.log` has no entry for the window. Blast radius was nil (dwm
    survived, desktop composited), but a lost `VkDevice` inside the process-shared venus ICD
    module is a stability risk the fence bridge has to close too, not just a correctness one.
  - ⛔⛔ **OWNER DECISION 2026-08-06: no stopgap. Design C — the real `pfnRenderCb` submission,
    with the KMD changes it needs.** *"stop gaps are not acceptable, we must do the correct,
    expected and performant implementation, do it right the first time. doesn't matter if its
    complex or if changes are needed to be done in KMD. … the next session is going to focus on
    kmd changes to unblock the UMD."* ⇒ `DECISIONS.md` **D5a**; the full, ordered work list is
    **`docs/dx12/KMD_IMPACT.md` §14a**, which replaces that document's "three items, none
    required for the first triangle".
  - ⭐ **The instrument round landed and settled three things** (`tmp/dx12/gates/G8-r0-F1-*`):
    **`pfnSignalFence` is NEVER CALLED** — `FenceSignalForwarded=0` and no trace line ever
    emitted, matching `DDI_REFERENCE.md` §14.0's WARP reading, so any design routed through
    that slot is dead; the resolved copy footprint is **exact** (`fmt=28, 256x256x1,
    rowPitch=1024, off=0`, matching the probe's own `GetCopyableFootprints`); and record →
    `Close` → ECL is provably **one list** (the closed list's `pDrvPrivate` is byte-identical to
    the submitted entry). ⚠ The two knob-gated delays could **not** answer whether the runtime's
    fence advance rides our context: delaying a slot that is never entered measures nothing, and
    delaying the end of ECL only shifts everything in time when the context carries no packets.
    ⛔ With `Umd12EclDelayUs=50000` rung 0 **passed** while the fence wait stayed **0.6 µs** —
    pixels correct, dependency absent. *A gate that reads only the exit code would have called
    that a fix.*
  - **The two unknowns that gate C, and the one experiment that separates them** — `KMD_IMPACT.md`
    §14a.1. **UV1**: does dxgkrnl release the runtime's queued monitored-fence signal behind *our*
    DMA packets? **UV3**: does vkd3d's venus work retire at host GPU completion or at **decode**?
    (The ICD says the synchronous `SUBMIT_VENUS` path does not propagate `ring_idx`, and
    `IncludingGpu` only means GPU completion for `ring_idx >= 1`.) ⭐ **The first step needs ZERO
    KMD change**: `dma_gpu_fence` defaults to 1, so a bare `pfnRenderCb` already takes
    `RetireDomain::IncludingGpu` + `watermark = next_wire_fence` in shipped, exercised code.
  - ⛔⛔ **UV3 IS ANSWERED (✗) FROM SOURCE, AND IT INVALIDATES THAT EXPERIMENT'S DECISION TABLE**
    (2026-08-06; full account and citations in `KMD_IMPACT.md` §14a.1's correction block).
    **For a D3D12 frame this driver usually sees no wire fence at all.** vkd3d's command stream
    rides the shared venus ring and never touches virtio (`vn_ring.c:630-636`); the only virtio
    submission a frame can make is the ring-0 `vkNotifyRingMESA` doorbell, and that is sent only
    when the host ring advertises IDLE and then only past a **1 ms limiter**
    (`vn_ring.c:673-690`) — so a ring busier than 1 ms emits nothing, `next_wire_fence` freezes,
    and `async_retired_up_to` returns true instantly. ⇒ ***the measured 0.8–1.1 µs is an EMPTY
    WATERMARK, not a fence bug.***
    - ⛔ **UV3's own cited evidence was stale in both halves**: `SUBMIT_VENUS` *does* propagate
      `ring_idx` (`vn_renderer_helios.c:1702`) and is **not** synchronous (`:1672-1678` — ASYNC,
      *"returns at QUEUE time"*). Right conclusion, wrong mechanism — which is why the fix it
      implied (ICD-1, "the last submitted wire fence") was also wrong and is now **replaced** by a
      per-queue `vkWaitRingSeqnoMESA` barrier submitted on `queue->ring_idx`.
    - ⛔ **The bare-`pfnRenderCb` reading cannot answer UV1**, so the table's *"N flat under both
      ⇒ UV1 ✗"* row must not be used. K-F1 settles the **plumbing** (and **P7**, free);
      **UV1's only clean test is the deliberate KMD-side hold**, which is therefore no longer
      conditional on K-F1.
    - ⛔ **The "one free run" UV3 pre-check is dead twice over**: `RING_SUBMIT_COUNT` /
      `RING_COMPLETE_COUNT` appeared **only** in the `'HDBG'` `DxgkDdiCollectDbgInfo` report, i.e.
      readable only by provoking a TDR — and `SCANOUT_RING_IDX = 1`, so they count **this driver's
      own** scanout/BLT copies from *three* internal producers — `gpu/mod.rs`'s
      `enqueue_async_submit_windowed_blt` and `enqueue_scanout_submit`, plus `ctrl.rs`'s
      `submit_venus_async_present` through the generic `enqueue_async_submit`. ⚠ **Cited by symbol:
      the old `gpu/mod.rs:3375`, `:3442`, `ctrl.rs:1541-1547` are all stale** — `gpu/mod.rs` grew
      983 lines in the D3D12 changeset. (`enqueue_async_submit_present_stream` is a fourth submit
      site but not a fourth internal producer: its ring is the guest's, forwarded.)
      Now mirrored as `RngSub`/`RngCmp` with a guest-originated split
      (`EscSub`/`EscSubRing`) counted at the escape wrapper — attribution, not a subtraction.
    - ⭐ **Why D3D11 is truthful and D3D12 is not, in one line:** the only `ring_idx >= 1` producer
      on Windows is `vn_signal_win32_external_semaphore` (`vn_queue.c:1714-1724`, `:1986-1994`),
      which needs an **OPAQUE_WIN32** semaphore. DXVK's present path signals one per frame; vkd3d
      signals an internal timeline semaphore and a non-shared `ID3D12Fence` has no Vulkan
      semaphore at all. ⇒ `EscSubRing` has a nonzero **control** reading from DWM alone, so the
      run needs an idle-desktop vs. probe-running arm over the same window.
    - ⛔ A ring-0 fence is not even a venus *decode* fence here: without
      `VIRTIO_GPU_FLAG_INFO_RING_IDX` (stamped only for `ring_idx != 0`, in `gpu/mod.rs`'s
      `enqueue_submit_inner`; was `:3482`) QEMU routes it to the legacy
      `virgl_renderer_create_fence`, which ignores `ctx_id`
      (`qemu-helios/hw/display/virtio-gpu-virgl.c:1167-1186`).
  - ⛔ **A LIVE DEFECT ON THE SHIPPING PRESENT PATH, found on the way and independent of D3D12**:
    `virtio/gpu/mod.rs`'s `present_stream_marker_boundary` bounds the guest-supplied
    `value` in no way, so an absurd `present_value` yields a *live* boundary
    `present_stream_slot_ready` can never satisfy — and `wddm_pending` is an
    **adapter-global head-of-line FIFO** (`take_one_ready_wddm`), so it blocks every
    context including DWM's, bounded only by the FIFO's own 256-entry overflow escape (the
    `wddm_pending.len() >= MAX_WDDM_PENDING` arm of `note_wddm_submission` → `overflow_wddm_pending`,
    `WDDM_PENDING_OVERFLOWS`) or TDR. `KMD_IMPACT.md` §14a.2 **K-F2**; its own commit.
    ⚠ **All five line numbers this bullet used to carry are stale and are SYMBOLS now**
    (`:4721-4744`, `:945-953`, `take_one_ready_wddm:5718-5765`, `:5664-5685`, and `MAX_WDDM_PENDING`)
    — `gpu/mod.rs` grew 983 lines in the D3D12 changeset, and every one of them drifted 250–600
    lines. ✅ **And the prescribed repair has since LANDED**: `WddmHeadMs` (default 250 ms) bounds how
    long the FIFO head may block, `rebase_blocked_head` rebases to the conservative wire watermark,
    `WfBReb` counts it, and `present_stream_marker_boundary` gained two magnitude instruments —
    deliberately INSTRUMENT ONLY, returning a byte-identical boundary.
  - ⛔⛔ **The obvious fix is REFUTED and would be a correctness regression, not a hardening**
    (2026-08-06, static, no VM needed). Copying the tag path's comparison —
    refuse unless `value <= slot.submitted_value` — inverts a CONSUMER predicate into the
    PRODUCER's: on the shipping default the marker is delivered **before** the frame's
    `vkQueueSubmit` **on purpose**. `UmdAsyncPresentStream` is absent = ON (`umd/src/knobs.rs`'s `UMD_ASYNC_PRESENT_STREAM`)
    and `async_stream_eligible` then **skips** `HeliosWaitFrameSubmitted`
    (`umd/src/forward/present.rs:1479-1528`) *because* the marker carries the dependency instead.
    The value is minted on the app thread (`umd/bridge/dxvk_bridge.cpp:1316`) while the tag that
    advances `submitted_value` rides DXVK's submission thread (`HeliosSignalPresentFence` is
    `EmitCs` only, `d3d11_context_imm.cpp:1150-1162` → `vn_queue.c:1994` → `:1736-1744`), so
    `value == submitted_value + 1` is the steady state and the check would refuse ~every
    legitimate frame. Each refusal falls back to `wire_fence_watermark()`
    (`adapter/scanout.rs:256-270`), which does **not** cover the unsubmitted frame — with the UMD's
    gate also skipped that is the 0ab-B stale/black-frame class returning, plus `PresentWmk`
    silently demoted. ⇒ the guard has to be **consumer-side liveness** (bound how long the FIFO
    head may block on a stream boundary, then rebase to the legacy watermark, counted — K-F0's
    fourth-arm plumbing), which also covers "submitted but never retired". An acceptance-side
    lookahead bound cannot stand alone: legitimate lookahead reaches DXVK's
    `MaxNumQueuedCommandBuffers = 32` (`dxvk-helios/src/dxvk/dxvk_limits.h:17`), and a forged value
    whose process simply stops presenting is unsatisfiable at any bound.
- ⭐ **Three UNVERIFIED rows from `G7-s6r1` are SETTLED by that run**, and they needed it:
  **U-B** — `pfnCreateContextCb` succeeds for a D3D12 queue (`CreateCommandQueue:
  CreateContext hr=0 hContext=0x… cmd=…/262144 allocList=…/256 patchList=…/256`).
  ⛔ **That line existed in no log before now**: the handoff said to read it out of the `G7`
  log and it was not there, because no D3D12 queue had ever been created on this adapter.
  **U-D** — command-list table index 0 is the right `hRTTable`, proved not by a zero counter
  but by recording DDIs demonstrably arriving at *this driver's* table. **U-F** — the bundle
  question, settled by the headers and acted on. ⚠ **U-E** is only half settled: `Signal` ran,
  `pfnWaitForFence` was never issued by this workload.
- ⛔⛔ **PRESENT IS NOT THREE SLOTS AWAY, and this is the round's largest scoping finding.**
  `PFND3D12DDI_PRESENT_0051` **outputs `D3DKMT_HANDLE`s** (`BroadcastSrc/DstAllocation`) and
  this driver has none, by design: the venus ICD mints every allocation through its own
  D3DKMT, `resource12.rs`'s `pfnCheckResourceAllocationHandle` answers **0** and says so, and
  `DDI_REFERENCE.md` §9.7 already records that *"pure passthrough with no `pfnAllocateCb` is
  not viable"*. The D3D11 driver, which presents successfully, **calls `pfnAllocateCb`
  itself** — ⚠ **one** call site, not three: the fn-ptr fetch is `umd/src/forward/resource.rs:217`,
  the call is **`:374`**, the success return is `:459`, and it has four callers (tex2D/primary
  `:543`, buffer `:804`, tex1D `:1143`, tex3D `:1235`); it feeds `hSrcAllocation` at
  `present.rs:1164`. ⭐ **And the shape is ADOPT, not import**: the engine allocates the Vulkan
  memory first (`resource.rs:1045`), the UMD reads it back (`:487`) and wraps it with
  `HeliosWddmAllocPrivate.adopt_resource_id` (`:300`), then transfers ownership (`:561`). **The KMD
  already accepts exactly that arm** (`create_allocation.rs:2377-2379` →
  `AllocationBacking::AdoptedUmdResource`), so present needs **no new KMD allocation shape**. The
  full field-by-field scope is `docs/dx12/KMD_IMPACT.md` **§14a.3**. ⇒ the gap is a
  **kernel-allocation-identity bridge at the L4/L8 seam**,
  which is also where `DECISIONS.md` D3c and `PARALLEL.md` §5's *"the first lane that needs a
  crossing record adds it"* finally bite. `D12-G8` rungs 1 and 2 are blocked on it; rung 0 is
  not, and rung 0 is where the current defect lives.
- ⚠ **The deferred INF / cold-boot half of S5 is still deferred** and is now worth doing: the
  DriverStore package carries no `helios_umd12.dll`, so a cold boot has no D3D12 UMD.
  Harmless while `UmdD3D12` is off, and one reboot would validate a device that can actually
  be created.
  (`PARALLEL.md` §9.2).
- ⭐⭐ **THE FEATURE-LEVEL TARGET IS FL 12_1. FL 12_2 IS OUT OF SCOPE, AND THE BLOCKER
  IS WDDM, NOT CAPS** (owner, 2026-08-06). `DX12.md` **§4.4** is the ladder. FL 11_0 is
  what `caps12.rs` ships today and is a **staging value**.
  - ⛔ **FL 12_2 requires a WDDM 2.9 adapter; Helios declares 2.1 deliberately.**
    `kmd_render/src/ddi/wddm_surface.rs`'s module doc has the mechanism: 2.1 is *"below
    the MPO3 requirement boundary"*, and at **2.2+** DWM treats the adapter as a
    Display-Core/MPO3 presentation device — Helios registers no MPO3 KMD interface, so
    it *"fails fast with `E_NOTIMPL`"*, which is exactly why the 3.2 level is not
    deployable. ⇒ 12_2 costs a new `WddmSurface` level across **five coupled sites**,
    **plus** the MPO3 interface, **plus** re-validating the display path that currently
    composites the whole desktop. A display-stack workstream wagered against a milestone
    already met — **revisit it as its own effort**, with `WddmSurface` + MPO3 as the
    deliverables and the feature level as a consequence.
  - ⭐ **FL 12_1 needs no KMD change at all, and no lane `D12-G8` did not already need.**
    Its five floors are typed-UAV-load + `ResourceBindingTier >= 2` +
    `TiledResourcesTier >= 2` (12_0 → **L5**, **L4**, **L2**) and ROVs + conservative
    raster ≥ 1 (12_1 → **L6**) — the triangle's own L2 → L6 → L5 → L4 order. **L9 and
    L3c stay able to trail**, as `PARALLEL.md` §4 says.
  - ⭐ **The substrate is not the constraint at any level** — vkd3d logs `DX Ultimate
    supported!` and §4.4 tabulates all 23 floors against `baselines/d3d12-caps.csv`.
    None of the five 12_1 floors is marginal (binding tier 3, tiled tier 4, conservative
    raster 3 all exceed requirement).
  - ⛔ **The level and its floors move in ONE commit, by the lane that earned them.**
    `D12-G5` measured the retail failure verbatim.
  - ⚠ FL 12_1 is *expected* to be reachable at the current WDDM 2.1 surface (D3D12 needs
    only WDDM 2.0, and no 12_1 floor is a display-path feature). The commit that raises
    the level must **confirm** that — a WDDM-shaped ETW refusal at 12_0/12_1 falsifies it.
  - ⛔ **CORRECTION, kept because it cuts the other way: tiled resources are NOT a
    `kmd_render` dependency.** An earlier note called `TiledResourcesTier >= 3` "the long
    pole" and "not a UMD-only job", on `caps12.rs`'s claim that the decorative page
    tables cannot give the zero-read guarantee. Wrong twice: the hard guarantee is at
    **tier 2** (required at FL 12_0, so the 12_1 target needs it), and it is **host-side
    and already true** — tiled resources ride Vulkan **sparse binding**
    (`vkQueueBindSparse`), no guest page table is in the path, and the guest exposes
    `residencyNonResidentStrict = true` beside `sparseResidencyImage2D/3D`,
    `sparseResidencyAliased` and the standard block shapes
    (`research/guest-vulkaninfo-full.txt`). ⇒ **UMD-only**: L4 plus L2's two tile-mapping
    slots. ⚠ Backed on paper, **unexercised** — a `D12-G9` verify item.
  - ⭐ **The lesson, and it runs both ways:** *a code comment asserting a dependency is
    not evidence of one* (one grep killed the tiled-resources claim after it had spread
    to three documents) — and the dependency that IS real was also sitting in a module
    doc. **Read the KMD's own docs before costing a feature level.**
  - ⚠ SM `>= 6_5` is a **12_2** floor, so `shader_models`' short `{5.1, 6.0}` list stays
    legal all the way to 12_1; L6 raises it when the shader creates become real.
- **Next: `D12-G8`** — a triangle through the DDI, owner-visible. That needs the 25 slots
  above, i.e. the `PARALLEL.md` §4 fan-out: **L2** first (it mints the WDDM context),
  then **L6** → **L5** → **L4**, then L3a/L3b, then L8 — followed by the §10 review pass.
- **Three instruments landed with L1's second half**, each of which paid for itself:
  - `tools/d3d12_format_matrix_probe.cpp` — the probe `GATES.md` §3.2 names. It
    `LoadLibrary`s the deployed `helios_umd12.dll`, takes a borrowed `ID3D12Device` off
    `helios_umd12_probe_create_device_v1`, and dumps the **engine's** per-format
    `Support1`/`Support2` and quality levels at counts 1/2/4/8/16/32 as CSV. No adapter
    restart, no `UmdD3D12` knob, no D3D12 runtime, no `d3d12.lib`/`dxgi.lib`. It is what
    made the engine's answers checkable against the driver's at all.
  - **`trace_line!` reaches `umd12`** with `caps12`'s multisample slot as its first
    per-op consumer: all 2 730 calls, **zeros included**, gated on `Umd12Trace`. Two
    runs had been spent inferring the rejected `(format, sample count)` from call
    *counts*; the trace answered it in one.
  - The `CheckFormatSupport` evidence line now carries the **engine's raw `s1`/`s2`**
    beside the driver's answer. Without it the first failure could not be diagnosed from
    the log at all — it took an ETW capture — because the log recorded only the derived
    value and the whole question was how it was derived.
- ⭐ **S6 is the bulk — 214 driver-side slots — and it FANS OUT. The split is
  `docs/dx12/PARALLEL.md`:** 11 lanes with exclusive file ownership, an append-only
  protocol for the four shared files, and a lease on the VM. Two things gate the
  fan-out and are worth knowing before planning around it:
  - ⭐ **Lanes compile on the LINUX HOST, so VM contention never arises.**
    `tools/umd12-host-check.sh` type-checks the whole 214-slot
    surface in seconds with no WDK: bindgen runs on the VM and `umd12/build.rs`
    serves the committed `umd12/bindgen/cached/d3d12umddi.rs` into `OUT_DIR` on a
    non-Windows host. ⛔ Never used for a shipping DLL — Windows regenerates from
    the header every time and warns `… is STALE …` on drift. Proven by fault
    injection, which also settled that the DDI typedefs are **`extern "C"`, not
    `extern "system"`** — the same ABI, a different Rust type, and a mistake that
    would otherwise have been made 214 times.
  - ⛔ **Authoring parallelises; validating does not.** There is one VM and one
    adapter. `win_install_umd` disables the PCI device, benchmarks are exclusive,
    and `HKLM\SOFTWARE\Helios` knobs are machine-global — one agent's A/B arm
    silently applies to another's measurement. Gates are run once, by the
    integrator, against merged code.
  - **S6-0 is the keystone**: stub all 214 slots with counting noops *before* any
    lane starts, so every lane is substitutive rather than additive, `D12-G7` is
    green before any lane lands, and the noop hit counters become each lane's
    progress metric (which is already `CONFORMANCE.md`'s charter).
- **The fork has content now.** `vkd3d-proton-helios` is on branch `helios` at
  `github.com/rupansh/vkd3d-proton` (remote `helios`; the checkout's `origin` is
  still upstream — ⛔ push to `helios`): D4's two DXGI-free exports, plus a
  Windows-bash fix to `tests/test-runner.sh`. The "zero Helios divergence, nobody
  knows why it was vendored" note is history.
- **The KMD is not on the critical path** — three small items (`K1` NodeOrdinal
  validation, `K2` `NoPatchingRequired`, `K3` `ApertureSegmentCommitLimit`), none
  required for the first triangle, and none of the 31 reachable-but-unset
  `DxgkDdi*` slots is required for a baseline D3D12 device. The multi-engine /
  residency / tiled-resource expectations in the earlier version of this bullet
  were wrong: `docs/dx12/KMD_IMPACT.md` walks each one.
- **P1 is complete (2026-08-05) — `D12-G5`, the WARP spy proxy.**
  `tools/d3d12_spy/` is a proxy `d3d10warp.dll` that forwards to Microsoft's own
  D3D12 UMD with a counting thunk on **all 206 driver slots** (+32 armed DXGI
  slots), driven by four workloads, six caps-mutation arms, four version-floor
  arms, and one run on the real Helios adapter. Containment check passes: 23
  caps types asked, all within the 43, none of the 7 deprecated. Artifacts
  `tmp/dx12/gates/G5/{spy.log,answers.md}`; `DX12.md` §4.2 has the summary and
  `DDI_REFERENCE.md` §15.0 the merged result. Six results that change P2-P4:
  - ⭐ **`D3D12DDI_SUPPORTED_0040` is accepted by this Windows build and a
    triangle presents on it** — 96 core + 58 CL slots instead of 124 + 75, i.e.
    **169 baseline slots instead of 214**. The default the runtime negotiates is
    `_0110`, not `_0109`. The choice between them belongs to P3.
  - ⭐ **`pfnCreateShader` receives a RAW stream, never a DXBC container**
    (`dword[0]=(type<<16)|(major<<4)|minor`, `dword[1]=length in dwords`,
    `dword[2]='DXIL'`), and the runtime converts SM 5.1 DXBC to DXIL first.
  - ⭐ **The caps set is validated as ONE contract at retail** —
    `D3D12CreateDevice` fails `0x887A0020` with an English reason on ETW
    **`Microsoft-Windows-Direct3D12`** (⛔ *not* `DxgKrnl`/`AzureTriage`, which
    said nothing). But an **out-of-range tier is clamped silently** — the
    advertise-what-is-backed hazard with the loud failure removed.
  - **No DXGI table**: `D3D12DDI_TABLE_TYPE_DXGI` is never requested, across 20
    flip-model presents. Present arrives on the command-list table only.
  - **A WDDM 2.1 adapter is not a barrier** — forced `_0110` negotiated cleanly
    on the real Helios adapter, so `Wddm2_1GpuMmu` does not cap the DDI version.
  - **`pfnGetPresentPrivateDriverDataSize` is called once per present**, right
    before `pfnPresent` — a second candidate identity channel, to test at G8
    beside the `pfnRenderCb` plan rather than instead of it.
  ⛔ Traps worth keeping: `C:\ProgramData\Helios` has a junction loop, so a
  wildcard *inside* a path silently finds nothing (use `-Filter`); Route B needs
  `pnputil /restart-device` because dxgkrnl caches the UMD path at StartDevice;
  and the spy's own `UmdD3D12Spy` gate refuses everything when the knob is
  absent, which faked four "the runtime rejected this DDI version" results.
- **Next: P2 / `D12-G6`** — the `umd_common` extraction (stages S1-S2), the
  first phase that writes driver code. `OpenAdapter12` still refuses.


**30th session (2026-07-07) — 3DMark bring-up: LUID gap FIXED, FL11 ceiling
root-caused.**

- **Vulkan/DXGI LUID identity gap — FIXED, DEPLOYED, VERIFIED (mesa
  `23b10bb6d80`, main `e0b462f`; ICD `vulkan_virtio-c2919595f95d`).** The venus
  ICD reported `VkPhysicalDeviceIDProperties::deviceLUIDValid=false` ("Phase 6
  concern" stub in `helios_init_renderer_info`), so no VkPhysicalDevice carried
  the guest WDDM adapter LUID that DXGI reports. UL/3DMark Steel Nomad (Vulkan)
  selects an adapter via DXGI then matches into Vulkan by `deviceLUID` → found
  nothing → "VkPhysicalDevice with device LUID X not found". Fix: plumb
  `helios->adapter_luid` (captured from D3DKMTEnumAdapters2 at open, == the DXGI
  AdapterLuid) into `info->id` (has_luid=true, node_mask=1, luid verbatim).
  Verified same-boot: vulkaninfo `deviceLUID dfb16300-00000000` == WDDM adapter
  `luid 00000000:0063b1df`, `deviceLUIDValid=true`. (LUIDs change per
  device-restart — the fix reads adapter_luid dynamically, tracks it. dxvk's
  own findAdapterByLuid / D3DKMTOpenAdapterFromLuid also benefit.) **Owner to
  re-test Steel Nomad.**
- **Fire Strike (D3D11 FL11_0) "no GPU" — the Helios adapter is FL10_0; being
  raised gate-by-gate. It is NOT a KMD/adapter ceiling (that theory falsified)
  — it's a sequence of UMD caps bugs.** The engine is genuinely FL11 (bridge
  creates the dxvk device at `D3D_FEATURE_LEVEL_11_0`; all FL11 DDIs wired).
  Everything is behind `HKLM\SOFTWARE\Helios!FeatureLevel11`, now an integer
  MODE (0=explicit FL10 fallback, 1=full FL11 and the absent-value default as
  of 2026-07-24, 2=diagnostic pipeline-only); knob=0 = exact FL10 baseline,
  dwm-safe. **THE tool that cracked it: the
  `Microsoft-Windows-DXGI` ETW provider prints d3d11.dll's exact rejection
  string** (the debug layer / DXGI InfoQueue / DBWIN / DxgKrnl-AzureTriage all
  gave 0 messages — the failure is device-less). Recipe: `logman start
  helios_dxgi -p Microsoft-Windows-DXGI 0xFFFFFFFFFFFFFFFF 0xff -o x.etl -ets` +
  `logman update helios_dxgi -p Microsoft-Windows-Direct3D11 ... -ets`, run the
  probe, `logman stop`, `tracerpt x.etl -o x.xml -of XML -y`, read `<Data
  Name="Message">`/`Code`. Gates cleared: (1) **"Driver returned invalid
  pipeline caps"** — 3DPIPELINESUPPORT is a BITMASK
  `(1<<Level)` OR'd, not the bare enum; we wrote 11_0=2 = bit1-only = invalid;
  fixed FL11=0x7, FL10=0x1 (the old 10_1=1 worked by luck = the 10_0 bit).
  (2) **"Driver doesn't support compute on FL11"** — SHADER caps now advertise
  0x2 (compute). (3) **"MSAA quality reported to be 0"** — FL11 requires every
  render-target format to support 4x MSAA and does NOT exempt 96-bit R32G32B32;
  CheckMultisampleQualityLevels now floors RT formats to >=1 at 1/2/4/8 +
  check_format_support advertises MULTISAMPLE_RENDERTARGET for RTs — PARTIAL:
  the runtime advances past formats 5-8 but still hits the MSAA error on a later
  format/count. **NEXT (owner directive): conform to the D3D11.3 functional
  spec** (https://microsoft.github.io/DirectX-Specs/d3d/archive/D3D11_3_FunctionalSpec.htm)
  — exact per-format/per-sample-count FL11 MSAA requirements. Committed main
  `ff14979` (WIP, un-deployed source; the all-count MSAA-log build was not
  installed). Probe: `tools/d3d11_fl_probe.cpp`, schtasks `helios_flprobe`,
  session 1 (session-0 win_exec fails all levels for a context reason, not FL).
  ✅ **The owner directive's READ half is DONE, 2026-08-05** — §19.2.5 *Required
  Multisample Support*: 1x/4x/8x required with standard patterns, **4x for ALL**
  output formats (so this entry's "does NOT exempt 96-bit R32G32B32" was right),
  **8x only below 128 bits per sample**, and *"Other MSAA counts and patterns are
  optional"*. ⇒ Helios meets the first two and **over-reports two**: 8x on
  128-bit formats, and 2x/16x which are not required at all. ⛔ Not changed in
  code — the residual rejection above is still UNVERIFIED and narrowing could
  move which format/count first trips it. Full table + the decision that remains:
  `CONFORMANCE.md` §4(a) and backlog **C6**.

**31st session (2026-07-07) — Fire Strike launch blocker moved past FL11:
legacy DXGI output mode-list fails on the IddCx logical output.**

- Owner rebooted after an IDD/client wedge; desktop composition is healthy again
  (`HeliosRenderAdapter=1`, both Display devices `OK`, IDD frames flowing with
  dirty-rect D3D11 fallback copies). The latest Fire Strike result
  (`3DMark-FireStrike-FAILED-20260707155504.3dmark-result`) exits both Demo and
  GT1 during `SINGLE_INIT_BEGIN` before workload rendering:
  `IDXGIOutput::GetDisplayModeList` returns `DXGI_ERROR_NOT_CURRENTLY_AVAILABLE`
  (`0x887a0022`), then 3DMark reports "Workload produced no results".
- Repro probe on the same boot: DXGI enumerates `\\.\DISPLAY6` under a fixed
  logical Helios adapter LUID `00000000:000078c5`; every
  `IDXGIOutput::GetDisplayModeList` / `GetDisplayModeList1` call returns
  `0x887a0022` for the tested formats. `D3DKMTOpenAdapterFromGdiDisplayName` on
  that same `\\.\DISPLAY6` opens the real Helios render adapter LUID
  `00000000:0038c127`, and `D3DKMTGetDisplayModeList` succeeds with 1064 modes.
  DXGI ETW around the probe records `IDXGIOutput_GetDisplayModeList` stop events
  with `m_Ret=2289696802` (`0x887a0022`) and the output object bound to
  `\\.\DISPLAY6`, but no richer rejection string.
- Falsified: RDP/session-context cause. The workload and probes run in active
  console session 1, not an RDS session. Also falsified: missing IDD modes in
  general (CCD/KMT mode lists exist), and UMD WDDM1.3 Present1/MPO callback
  table as the immediate launch blocker (current UMD logs show the DXGI 1.3 base
  table populated and D3D11 device creation succeeds in probes).
- Current read: this is the architectural split between the IddCx/IndirectKmd
  display adapter that owns the output and the real Helios render adapter that
  owns D3D/Venus. Fire Strike's legacy fullscreen init assumes
  `IDXGIOutput::GetDisplayModeList` works on the visible output. A proper fix is
  not a UMD present-path hack; it likely requires a real display/VidPN owner for
  the visible output (or equivalent OS-supported path that makes DXGI's output
  mode-list resolve on the render/display adapter pair). If reviving KMD display
  ownership, use the archived viogpu3d/VidPN research and treat it as a full
  display-miniport implementation, not as stubbed VidPN callbacks.

**32nd session (2026-07-07) — Fire Strike/FaceWorks missing surfaces: first
real D3D11 correctness bug fixed, owner visual validation pending.**

- Owner used Windows Settings -> Display -> Detect multiple displays; there was
  no visible display change, but Fire Strike moved past the legacy
  `IDXGIOutput::GetDisplayModeList` launch blocker and now reaches fullscreen
  rendering. Current symptom: many missing surfaces. Do not spend time on
  `DxgkDdiPresent`: this is a render-only adapter path, and the desktop already
  proves the presentation/capture leg is alive.
- FaceWorks is the faster repro. Its initial dxbc-spv assertion in
  `shd_instruction.cpp` came from D3D11 DDI tessellation patch-constant sysval
  names; `umd/bridge/dxvk_bridge.cpp` now remaps the DDI tess-factor sysvals to
  DXBC `SV_TessFactor` / `SV_InsideTessFactor` signatures. The sample then
  opened a black window in owner-visible testing. A scheduled-task run is not a
  valid visual proxy yet: it exits during DXUT validation before any present.
- Concrete bug found in `umd/src/forward.rs`: DDI Texture2D views were always
  translated to non-MS D3D11 view dimensions. For multisampled Texture2D
  resources this is wrong: RTV/DSV/SRV must use
  `TEXTURE2DMS`/`TEXTURE2DMSARRAY`, not `TEXTURE2D`/`TEXTURE2DARRAY`. This is a
  real correctness hole for Fire Strike-class MSAA workloads and can make view
  creation fail or bind the wrong resource interpretation.
- Fix deployed in UMD `helios_umd_ac10566f81de7294.dll` (SHA256
  `AC10566F81DE72944B472131DF1AE8CBAD719938DCC804C9A936FC14F6643B19`):
  `rtv_desc`, `dsv_desc`, and `srv_desc` now query the underlying
  `ID3D11Texture2D::GetDesc().SampleDesc.Count` and select the MSAA view
  dimensions/unions when `Count > 1`. Adapter hotplug only; no guest reboot.
  Active registry and live DWM/explorer modules point at the new ProgramData
  UMD, Helios device is `CM_PROB_NONE`.
- Evidence: new `tools/d3d11_msaa_view_probe.cpp` passes on Helios FL11_0. It
  creates 4x `R8G8B8A8_UNORM` RT/SRV and `D24_UNORM_S8_UINT` depth resources,
  creates explicit 2DMS RTV/SRV/DSV views, clears, resolves, stages, maps, and
  reads pixel `64,127,191,255`. UMD log for pid 10120 shows
  `MSAA q fmt=28 c=4 -> 1`, successful `create_rtv`, `create_srv`, and
  `create_dsv` on `dim=3` MSAA resources. **NEXT:** owner reruns FaceWorks and
  Fire Strike against this UMD; if surfaces are still missing, inspect fresh UMD
  logs for failed Create*View, ResolveSubresource, Copy/Discard/ClearView, and
  noop-DDI counter movement.

**33rd session (2026-07-07) — FaceWorks/Fire Strike "missing surfaces" reframed:
render is FINE, the problem is windowed compositing. Two earlier theories
FALSIFIED.**

- **FaceWorks is not a render/coherence bug.** `HELIOS_PRESENT_READBACK` shows the
  present source non-black at 1264×681 (multi-pass scene: 1060 draws to the
  backbuffer, 937 to a 184×161 SSS RT, DrawIndexed to 1024×1024).
  `tools/d3d11_shared_draw_probe.cpp` proves float + SINT-indexed+CB + textured/
  blend draws propagate cross-device via OpenSharedResource1. Owner: **Fire Strike
  renders fine fullscreen** — the render pipeline is healthy.
- **FALSIFIED — "two-memory split / KMD zeroes the adopted resid."** The UMD's
  `allocate_wddm_resource` "private mutated" log (`res_id 22301→0, blob→0x3b4000`)
  is the UMD reading the KMD's NEW `HeliosWddmOpenIdentity` writeback with the OLD
  `HeliosWddmAllocPrivate` layout — offset 0 is `venus_alloc_size` (0x3b4000 =
  3883008), and the fields it prints as res_id/kind are `reserved` words. The KMD
  adopt path (`create_allocation.rs adopt_blob_for_allocation`) preserves resid
  22301 and mints no redundant blob; the allocation IS backed by the DXVK venus
  image. Do not re-chase this.
- **FALSIFIED — alpha.** Owner confirmed `HELIOS_PRESENT_FORCE_OPAQUE` changes
  nothing; DWM composites the flip swapchain opaque.
- **FALSIFIED — the IDD.** Both the live D3D11 fallback (`SwapChainNewFrameD3D11`)
  and the D3D12 path capture the same single composed IddCx surface; the D3D12
  path is dead only because our UMD implements no D3D12. The IDD faithfully
  forwards whatever DWM composited (`CopyFromScreen`/paintcap show the same black
  client area).
- **The live root cause is the render-adapter / DXGI-output topology (→ priority
  #1 above).** DXGI enumerates TWO identically-named "Helios vGPU Render Adapter"
  entries (LUIDs …fba8, …7896) that both resolve to the same physical WDDM adapter
  (a stale-LUID residue from repeated device restarts) plus 2× WARP; every
  adapter's `EnumOutputs` returns `0x887a0022` intermittently (one run showed
  `outputs=1` on …7896). CCD `QueryDisplayConfig(ONLY_ACTIVE_PATHS)` pins the live
  Looking Glass output to …fba8 (`\\.\DISPLAY2`), while `QDC_ALL_PATHS` fails
  `ERROR_GEN_FAILURE` on a broken second path (ghost QEMU/DEFAULT monitors). Apps
  that need an `IDXGIOutput` get `NOT_CURRENTLY_AVAILABLE`, fabricate a mode, and
  drop the window off-screen; and DWM never imports the app's flip backbuffer.
  Probes: `tools/{dxgi_output_modes,ccd_adapter}_probe.cpp` (session 1). Memory:
  `phantom-adapter-luid-enumoutputs-33rd`, `faceworks-black-d3dflip-twomemory-split-33rd`.
- KMD 22.22.61: `create_allocation` now writes the `HeliosWddmOpenIdentity`
  trailer for adopted allocations too (live resid for cross-process openers).

Current state (surveyed 2026-07-05):

- UMD (`umd/`, Rust d3d10umddi frontend → cxx bridge → DXVK C++ engine):
  advertises `D3D11_1_DDI_SUPPORTED` (11.15.0) + `D3D11_0` (11.10.2), fills
  `D3D11_1DDI_DEVICEFUNCS`; `forward.rs` implements ~220 DDI functions;
  unfilled slots route to counted noop handlers (`ddi_noop_device/dxgi` —
  loud, not silent). Feature level 11_0 reported to the runtime.
- dxvk-helios: ~58 files / +3.8k lines diverged from upstream DXVK (venus
  import model, GDI staging, alias-image detile, persistent refresh,
  undersized-import refusal).
- Known DDI gaps (from bring-up sessions): ClearView logging blind (log
  budget), partial Discard handling fixed for the common case, Rotate
  implemented minimally; keyed mutex path exercised only by probes.

Plan:
1. Enumerate the noop-slot hit counters after a real workload day — every
   nonzero noop is a conformance gap with a caller.
2. Run dxvk-tests / d3d11-triangle / d3d9-on-11 samples inventory
   (`dx-samples-research-only/`), then 3DMark (installed on the VM).
3. DXGI format coverage audit (the format round-trip carrier landed at
   `bfb5121`; verify beyond BGRA8).
4. Map remaining 11.1 features (deferred contexts? threading modes? UAVs at
   FL11_0) against DXVK capabilities — most exist in the engine; the work is
   the DDI plumbing.

## Tooling (keep alive; this stage depends on it)

- **What is on the SCREEN, sampled at ~30/s** — `tools/vnc_frame_probe.py` +
  `tools/vnc_scanout_correlate.py` (added 2026-07-29 for defect 0ab; needs
  numpy + pillow, host-side only, a venv is fine).
  The probe is an RFB client against QEMU's VNC server. It stamps every
  framebuffer update with `time.time()` — the SAME CLOCK as the
  `virtio_gpu_cmd_*` lines QEMU's `log` trace backend writes to
  `/tmp/helios-qemu-stderr.log` — so a displayed frame can be attributed to a
  specific `res_flush`. Enable the events over QMP first:
  `python3 qmp trace-event-set-state virtio_gpu_cmd_set_scanout_blob /
  _res_flush / _res_unref` on `/tmp/helios-tpm/mon.sock`.
  Its **completeness oracle** is what makes it decisive: `--hud x0,y0,x1,y1`
  names a rectangle that is bright in every FINISHED application frame
  (3DMark's fps bar by default), which separates "the app rendered a dark
  scene" from "we displayed a frame the app had not finished". Whole-frame
  brightness cannot do that and led two sessions astray.
  ⚠ `screendump` is not an alternative under `sdl,gl=on` OR `egl-vnc`: the
  console's scanout kind is DMABUF, so QMP answers `"no surface"`.
  ⚠ Use `--exclusive`; QEMU refuses a SHARED client while an exclusive viewer
  (most viewers) is connected, and drops it silently after ClientInit.
- **Registry knobs** (service key, active KMD reads) — this list is now the
  complete set and is checked against `kmd_render/src/diag.rs`'s `pub mod knobs`:
  `DiagLevel`, `AllocCached`, `DmaGpuFence`, `BindFlushMode`, `DispatchBind`,
  `PresentProbe`, `DisplayHalf`, `DirectFlipCaps`, `CrossAdaptCaps`,
  `BarSegFlags`, `BarSegBaseMB`, `BarSegMode`, `VidMmVramMB`, `FlipCapsX`,
  `FlipQueueN`, `PresentWmk`.
  It used to list `ScanoutDiag`, which the very next bullet says was RETIRED in
  T6/R901, and to omit six knobs that do exist. Do not add a knob here without
  adding it there, or the reverse.
  **`PresentWmk` (default 1 since 22.22.244.0)** gates a WDDM submission that
  carries a live present stream boundary on that boundary alone instead of on
  the whole `next_wire_fence` backlog; `0` restores the historical superset for
  a same-boot A/B. Advertised value mirrored in `PwExact`; the FIFO-head block
  reason in `WfBWire`/`WfBStrm`/`WfBBlt`. **`FlipQueueN` (default 1)** sets
  `DXGK_DRIVERCAPS::MaxQueuedFlipOnVSync` (mirrored in `FlipQueV`); depth 4 was
  MEASURED INERT on 2026-08-04 with and without `FlipCapsX=3`, so it exists as a
  bisect handle only. Both are read at AddAdapter/transport init, so
  `pnputil /restart-device` applies them with no reboot.
  `DisplayHalf=1` enables the render+display adapter shape. `AllocCached=0`
  is the CpuVisible cached-allocation kill switch. `DirectFlipCaps` and `CrossAdaptCaps` are
  explicit cap-advertisement probes; leave off unless bisecting.
  `BarSegFlags`/`BarSegBaseMB` bisect BAR descriptor flags/base. `DiagLevel`
  enables the generic S-ring registry breadcrumbs.
  **`BarSegMode` now has exactly TWO legal values** (T4b/R904, KMD 22.22.187.0):
  `10` (default, absent = production: aperture id 1 + BAR id 2) and `0` (the
  recovery baseline: aperture id 1 + paging-RAM cpu-host id 2, no BAR). The
  historic Code-43 bisect arms `1`, `2`, `5` and `11` are DELETED, along with the
  `probe_only` BAR segment and its 16 MiB contiguous RAM block. Any other value
  is coerced to `10` and recorded in the new `BarMCo` counter carrying the stale
  number — so a VM left set from an old bisect now binds and says so instead of
  reporting a segment no allocation may use. Nothing reports segment id 3 any more.
- **ScanoutDiag — RETIRED in T6/R901 (KMD 22.22.188.0).** The knob, its 16 modes
  and `ddi/scanout_diag.rs` are GONE from the driver; any `ScanoutDiag` value or
  `Sdg*`/`S2d*` name still in the service key is a stale leftover. What it bought
  and why it went: the lab published its colour-bar blobs through the PRODUCTION
  publish word, so at the type level a KMD-owned fill image was indistinguishable
  from the Windows-designated primary, and a leftover `ScanoutDiag >= 4` selected
  a 5-extension `VkDevice` (the 38th-session global-modifier-enable regression
  class) on the one device every render/scanout/GDI path uses. Neither is
  representable now. **`Sdg*` names that SURVIVE, written by the production
  LINEAR fallback:** `SdgLStg SdgLReq SdgLBit SdgLTyc SdgLImg SdgLMem SdgLPch
  SdgLOff` (zeroed each StartDevice by `zero_linear_scanout_breadcrumbs`), plus
  `SdgMt SdgMf SdgBFl` and `SdgDevR SdgDevX` (ext tier; numbering unchanged, 1 =
  export trio, 2 = none).
- **Scanout counters** (service key fixed names): `Sc*` =
  `SetVidPnSourceAddress` scanout, `CSc*` = create-time scanout bind attempt,
  `PSc*` = Present/HWQ diagnostic-only scanout candidate, `Sdg*` = diagnostic
  scanout allocator/bind path, `Rf*` = periodic active-scanout refresh. Values
  persist across boots; trust movement plus same-boot QEMU traces.
- **SAMPLED counters, 22.22.180.0+** (R316): the `PB*` IDENTITY values written by
  `DxgkDdiPresent` — `PBcall PBflag PBcnt PBalst PBDma PBPatch PBpdsz PBkpsz`,
  the `PBs*`/`PBd*` surface identity sets, `PBstrk`/`PBdtrk`, and the flip arm's
  `PBsrc PBsw PBsh PBsDir PBIdOk PBFlip=1` — refresh on the 1st present and
  every 600th thereafter at `DiagLevel=0`, NOT per frame. **A `PB*` identity
  value can therefore be up to ~10 s stale; do not read one as live.** Set
  `DiagLevel=1` (+ `pnputil /restart-device`) to restore the per-call cadence.
  `PBRet=STATUS_SUCCESS` follows the same first/every-600th cadence beginning
  with 22.22.240.0; every non-success `PBRet` remains immediate. UNTHROTTLED,
  always current: `PBCpy` (all arms), `PBFnc`, `PBSyWt`, `PBSyCp`, and
  `PBFlip`'s `0xE1`/`0xE2` failure arms.
- **RETIRED 22.22.180.0** (R903/x-dup-dead-20 — do not look for these; they are
  gone from the driver, and any value still in the service key is a stale
  leftover): the `GdiAccelMode` knob and the whole `Gd*` counter family —
  `GdiM`, `GdiE`, `GdiS`, `GdFa`, `GdFg`, `GdFs`, `GdFb`, `GdFm`, `GdFi`,
  `GdFr`, `GdTc`, `GdDs`, `GdCn`, `GdCr`, `GdCc`, `GdCg`, `GdBn`, `GdBr`,
  `GdBg`, `GdXn`, `GdXz`, `GdXr`. The KMD no longer advertises
  `SupportKernelModeCommandBuffer` in any configuration and no longer contains a
  GDI raster executor; GDI renders through win32k's CPU redirection path.
- **Counters** (service key): Ch* (CpuHostAperture),
  Pg* (paging engine; `PgEv` nonzero means unresolved virtual paging transfer),
  AE* (8-slot allocation create/open ring: resid, dimensions, ctx/open marker)
  — all failure counters must stay 0; S-ring breadcrumbs persist across boots
  and high indices go stale after short boots.
- **Direct-primary producer gate — DELETED, do not reintroduce.** `PresentGateUs`
  and `PresentOrder` were removed on 2026-07-29 by owner directive and this
  inventory entry described them as live for a week afterwards.
  `umd/src/knobs.rs` carries the reasoning: a producer-side CPU stall hides an
  ordering defect instead of fixing it, it costs Fire Strike GT1 158 -> 136 fps
  when it holds, and it publishes the present anyway when it expires. Ordering
  belongs on the GPU timeline (`ScanoutAcquire` + a consumer-side wait), never
  on a blocked CPU thread.
- **UMD `DDI refusals:` counters, T6/R911** — nine names on ONE bounded log
  line, read by `tools/umd-gate-surface.ps1`: `srv_raw_hazard`,
  `resource_raw_hazard`, `text_filter_size_ignored`,
  `staging_busy_assumed_free`, `discard_partial`, `clear_view_unsupported`,
  `gs_so_declaration_dropped`, `tess_sig_fallback`,
  `unhandled_resource_dimension`. Emitted at `DestroyDevice` and on each
  counter's FIRST hit — never on a per-present path (that cost is what T2
  measured and reduced). All nine should read 0 on a healthy DWM session;
  **`gs_so_declaration_dropped` and `tess_sig_fallback` are expected to MOVE
  under 3DMark** and each names a real WS3 conformance gap. The UMD still has
  no registry counter surface, so the log line is the readout — check the line
  exists, not just the `fetch_add`.
- **RETIRED in T6** — do not look for these; they are gone from the driver:
  the `ScanoutDiag` knob and the whole diagnostic `Sdg*`/`S2d*` lab (R901; the
  production `SdgL*` LINEAR ladder plus `SdgMt SdgMf SdgBFl SdgDevR SdgDevX`
  SURVIVE), `ScForceReject`/`ScFrc` (owner-approved), `RbRid`/`RbFail` (R902,
  replaced by `RfUnb`), and on the UMD side the `PresentSyncPublish` and
  `VehicleKernelFlipWait` knobs with the whole kwait subsystem (R912a).
  `helios_umd_get_present_result` REMAINS EXPORTED, returning -1 — the mesa ICD
  resolves it by name and fails the dcomp vehicle with `E_NOINTERFACE` if it is
  absent. Two verbs now have no in-tree consumer and are kept as read-only ABI:
  `HELIOS_ESCAPE_QUERY_SCANOUT` / `helios_venus_query_scanout` (R910), and the
  UMD-side `HeliosPresentRefreshCmd` sender (R910 — the KMD still issues its own
  'HERF' marker in `display.rs`, so the refresh-marker ordering is intact).
- **Launcher/display path**: `tools/launch-helios-gtk.sh` supports
  `HELIOS_DISPLAY=egl-vnc` for `-display egl-headless` + VNC, intended as the
  reliable display-output inspection path, and `HELIOS_DISPLAY=sdl` is visually
  verified on native Wayland. It uses the `qemu-helios` submodule build, whose
  egl-headless/GTK/SDL OpenGL backends share exact OPTIMAL Vulkan readback when
  EGL cannot import a modifier-less native image. Interactive modes leave EGL
  vendor selection to the compositor while NVIDIA remains selected for
  Venus/readback Vulkan; globally forcing NVIDIA EGL breaks Wayland context
  creation on the development host. GTK is still blocked by its later GDK
  `eglMakeCurrent` failure.
  `HELIOS_QEMU_RENDER_GPU=nvidia` is the current owner preference; render-node
  defaults are tracked in the script. The old force-LINEAR LD_PRELOAD shims were
  experiments, not supported display paths. `HELIOS_QEMU_TRACE` can enable
  `virtio_gpu_cmd_set_scanout_blob`, `virtio_gpu_cmd_res_flush`,
  `virtio_gpu_cmd_res_create_blob`, and `virtio_gpu_cmd_ctx_submit`; the trace
  file `/tmp/helios-qemu-stderr.log` is ground truth for scanout shape.
- **ETW**: `logman create trace -p Microsoft-Windows-DxgKrnl 0xFFFFFFFFFFFFFFFF
  0xFF` → tracerpt → grep `AzureTriage` = dxgkrnl failure reasons in plain
  text. Found the segment rule in minutes.
- **AddAdapter iteration**: `pnputil /restart-device` re-runs AddAdapter with
  the loaded image — registry-knob experiments need no reboot.
- **T3 refusal instrument `ScForceReject` — RETIRED in T6 (owner-approved).**
  ⚠ **This leaves the T3 gate line "force each of the seven deferred-programming
  exits and confirm the matching counter moved" with NO mechanism behind it.**
  The `Sc*Err` counters below are still written at their real sites; what is gone
  is the only way to provoke them. Knob names are still capped at **14 chars**
  (`diag::MAX_CONFIG_NAME`) — a build failure, previously a silent always-default.
- **T3 counters** (all must read 0; reset at StartDevice so movement is
  this-boot): `ScBadAlc ScBadExt ScBadLay ScBadFmt ScLinErr ScSetErr ScNoTgt
  ScCpyErr` (one per refusal class), `ScUnav` (HPD dropped a dirty bit),
  `ScRetry`/`ScGaveUp` (R506's bounded retry), `ScStale` (a completion tried to
  clear an interval that was not its own), `ScGateCx` (the DIRQL raise CAS
  exhausted its budget), `HpdStTo` (the HPD prologue fell back to its 500 ms
  bound instead of the real start edge).
- ⚠ **Kernel stack budget on the boot path**: `DxgkDdiStartDevice` +
  `VirtioGpu::init` are the binding chain — **17568 B of the 17936-B known-good
  ceiling as of 22.22.184.0** (8408 + 9160), i.e. 368 B of headroom in a 24 KB
  kernel stack. Overflow at boot = `0xc0000001`/Startup Repair with **no dump and
  no bugcheck event**, and it does NOT reproduce on a live `devcon` restart.
  **Run `tools/kmd-frame-sizes.ps1` on every image** — it reads the frames out of
  the built `.sys` + linker `.map` with `llvm-objdump` (no PDB, no debugger),
  handles sub-page frames, sums the declared call CHAINS rather than every symbol
  measured, and **exits 1 over the ceiling**. `-Symbols`/`-Chains` extend it.
- **Counter snapshots**: `tools/kmd-counter-snapshot.ps1 -Label <name>` dumps the
  whole service key to `Z:\tmp\kmd-counters-<name>.txt` and prints the
  transport/venus/scanout subset. Registry values PERSIST ACROSS BOOTS — take one
  before a workload and one after and diff the files; a single read proves nothing.
- **T4a counters** (22.22.184.0+; every one must read 0 or be absent on a healthy
  boot): `VnEncOvf` (venus command-stream overflow, absent), `VnRingFt`/`VnRingWd`
  (ring fatal latch / head-wait ms, absent), `VnRingSz` (undersized ring mapping,
  absent), `VnMtDown` (memory-type downgrade, absent), `CpNoDrn` (prepared-copy
  drain skipped because nothing was submitted), `PBTdErr` (partial Present-BLT
  teardown), `CtNotOurs` (sync token named another entry), `WtTbl`
  (`FENCE_WAIT_TABLE_FULL`, split out of `WtOut`), `AbnDrop` (fences discarded by
  a TDR/preempt/reset epoch), `ChSzMm`/`ChSzDl`/`ChSzPv` (aperture size-provenance
  cross-checks), `MapDup` (duplicate blob map refused at commit time),
  `PciCapOob` (PCI capability tail outside config space), `WnRcf` (window-reserve
  reconfiguration refused). CollectDbgInfo is **version 6 / `[u32; 38]`**, with
  `FENCE_WAIT_TABLE_FULL` at index 37; word 25 is still `FENCE_WAIT_TIMEOUTS`.
- **Guest probes** (schtasks, session 1; SSH lands in session 0):
  `helios_paintcap` (screenshot → `Z:\tmp\screen_copy.png`), `helios_repaint`,
  `helios_flasher`, `helios_dstate`, `helios_enum_windows`, `helios_regedit`.
  `FindWindow('Progman')` is broken on this box — EnumWindows only.
- **Handle-leak instruments** (`handle.exe` is NOT installed on this box and
  these replace it; both run from SSH, no scheduled task needed):
  `tools/helios-handle-types.ps1` answers *what* — per-type counts either side
  of a run of device cycles, each new handle's type / granted access / kernel
  object address / name, the handles that CLOSED during the run, the
  transient-module set around ONE device cycle, and TlsAlloc/FlsAlloc
  high-water. `-Pin <module-prefix|all>` holds modules loaded, which attributes
  a leak to a module with no hooking at all: the per-device rate drops by
  exactly that module's own never-released statics.
  `tools/helios-handle-origins.ps1` answers *where* — IAT-hooks the
  handle-minting kernel32 entry points (matching slots by resolved address, so
  the kernel32/KernelBase/api-ms-win-core aliasing needs no spelling list) and
  prints a stack per handle that one device leaves behind. **Two traps, each
  cost a run:** the provider modules must be excluded or `kernel32!CreateFileW`
  recurses through its own IAT into the hook (`0xC00000FD`, no output), and the
  **ANSI** spellings are not redundant — the ICD is mingw-built, so its
  `CreateSemaphore` IS `CreateSemaphoreA`. ICD frames are DWARF, which dbghelp
  cannot read: resolve `module+0xRVA` with
  `x86_64-w64-mingw32-addr2line -f -C -e <dll> $((ImageBase + RVA))` on the
  Linux side (get ImageBase from `objdump -p`).
- **User-mode stack dumps**: `tools/take-minidump.ps1 -ProcessId <pid> -Path <dmp>`
  (P/Invoke MiniDumpWriteDump; the `rundll32 comsvcs.dll,MiniDump` trick writes
  TRUNCATED dumps on this box — do not use). Analyze on Linux:
  `~/.cargo/bin/minidump-stackwalk --symbols-path <breakpad-syms> <dmp>`;
  make syms with `~/.cargo/bin/dump_syms <pdb>` (dir layout
  `syms/<name>.pdb/<GUID+age>/<name>.sym`; fix the MODULE line name if the pdb
  was renamed). The deployed UMD build's PDB must GUID-match the dump's module
  (check with `llvm-pdbutil dump --summary`).
- **KMD build/deploy**: `win_build_kmd` (bumps the three version sites with a
  coherence check, then cargo-make package build) → `win_install_kmd`
  (install script + recommended, toggleable graceful guest reboot — the only
  reliable activation path). Manual fallback: `win_cargo` +
  `tools/install-helios-kmd.ps1` (ExecutionPolicy Bypass,
  `-AllowRebootRequired`); version bump = the single `HELIOS_KMD_VERSION` line in
  `kmd_render/driver-version.env` (build.rs renders the FILEVERSION numerics and
  the version strings from it; Cargo.make stampinf reads it via `env_files`);
  backups under
  `C:\ProgramData\HeliosDeployBackups`. New tools appear after the win MCP
  server restarts (new session).
- **dxvk staged-content probes** (`dxvk.heliosStagedProbes`, default OFF since
  `bdbbc2ea` — they were the ~1.5 s stall): full-surface raw+post-copy readback
  characterization at fixed refresh ticks for black-surface triage. Re-enable
  per process via `DXVK_CONFIG "dxvk.heliosStagedProbes = True"` (no rebuild).
- **Venus pipeline object trace** (`VN_HELIOS_PIPELINE_TRACE` env, per-process,
  default off): (ring, primary_tail seqno, object id) lines in the ICD diag log
  for pipeline-layout create/destroy, the vn_get_target_ring wait_all barrier,
  and graphics-pipeline creates — the defect-0b recurrence kit. The barrier
  skip/abandon lines (`BARRIER SKIPPED/ABANDONED in wait_all`) are ALWAYS on.
- **ICD sem-deadline strike log** (`helios_icd_diag.log`, always on): each strike
  line carries `sem=` (venus object id), `reason=` (vn_relax reason),
  `sig_queue=/family=/ring=` + `sig_value=/sig_age_ms=` (the most recent submitted
  signal op for that semaphore, recorded at submission prepare) and `pending_ms=`
  (how long a signal had been pending with zero movement — the quantity the
  deadline gates on). `sig_age_ms` near 0 on a strike = wait-before-signal false
  positive (should no longer happen post-f7a816f182f); large `pending_ms` = a
  genuinely stuck host channel.
- **Queue-submit phase timing**: `HELIOS_QUEUE_PERF=1` + `HELIOS_PERF_FILE`
  machine env (live in dwm since the 2026-07-06 reboot) — one aggregate line
  per 300 vkQueueSubmit2 calls (tls/wsi-flush/cache-flush/submit/fence-wait
  phase averages) to `C:\ProgramData\Helios\helios_queue_perf.log`.
- **Ring-fence probe**: `tools/vk_ring_fence_probe.cpp` → schtasks
  `helios_ringprobe` (wrapper `C:\Users\Rupansh\helios-probe\run_ring_probe.cmd`,
  `/rl LIMITED`; `helios_ringprobe_named` runs the NAMED-import mode against the
  dev ICD build via `icd_devbuild.json`). Proves/regression-tests the WS1 #4
  chain: rc=0 + "consumer wait tracked GPU completion". Build on the VM with
  the WinLibs g++ (`g++ -O2 -o ... Z:\tools\vk_ring_fence_probe.cpp -I <VulkanSDK>\Include
  C:\Windows\System32\vulkan-1.dll`) — no clang-cl on the box. **GOTCHA (cost a diagnosis detour): the Vulkan loader
  silently ignores `VK_DRIVER_FILES`/`VK_ICD_FILENAMES` in ELEVATED processes** —
  win_exec/SSH shells are High-IL (and `runas /trustlevel:0x20000` still reads as
  elevated), so an "env-override" probe actually tests the REGISTRY ICD. Run ICD
  A/B probes through a `/rl LIMITED` scheduled task.
- **QEMU fence tracing without restart**: QMP on `/tmp/helios-tpm/mon.sock` →
  `trace-event-set-state` for `virtio_gpu_fence_ctrl`/`virtio_gpu_fence_resp`
  (output → `/tmp/helios-qemu-stderr.log`; ctrl→resp gap per fence id = decode-
  vs GPU-completion retirement; disable after use — it logs 2 lines per fence).
  NOTE: `-d guest_errors` is already on, but virglrenderer's vkr_log/proxy_log
  are INFO-level = SILENT on the release build — absence of host log lines
  proves nothing below WARNING; a real host-side bisect needs a relaunch with
  `VIRGL_LOG_LEVEL=debug`.
