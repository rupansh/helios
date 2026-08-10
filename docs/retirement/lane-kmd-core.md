# Lane: KMD core — native fence, native render/HNR2, translation session, HVM1/HPM1 memory

Working plan for the HPS2 retirement lane that owns `kmd_render/` (minus the
display DDIs) and `kmd_logic/`. Everything normative here is traceable to
`docs/HELIOS_PRESENT_SYNC_RETIREMENT.md` (5928 lines, "the doc" below) at the
line ranges in §1. Where the doc is silent or self-contradictory that is said
plainly in §6 — do not invent a reading; take the fail-closed one and record it.

**Scope discipline reminder:** this lane does *not* own `ddi/display.rs`,
`ddi/present_packet.rs`, `ddi/vidpn.rs`, `ddi/scanout_trace.rs`,
`ddi/scanout_timeline.rs`, `ddi/hpd.rs`, `ddi/child.rs`, `ddi/base.rs`,
`ddi/add_device.rs`, or the new `ddi/mpo3.rs` / `ddi/direct_scanout.rs` /
`ddi/diag_etw.rs`. It *does* own `adapter/scanout.rs` by the letter of the
source scope, which is a hazard — see §4.

---

## 1. Normative sources

⛔ **Line numbers here are regenerated with `grep`, never trusted.** They have
already shifted once: commit `c17f17c` inserted a ten-line frozen-document
banner at `docs/HELIOS_PRESENT_SYNC_RETIREMENT.md:11-20`, so **every doc line
citation written before 2026-08-10 in this file, in the other five lane briefs,
and in the recon reports is 10 too low.** The table below and every inline
citation in §2, §3, §4, §5 and §6 were re-derived on 2026-08-10 by grepping the
section headings (`grep -n '^## \|^### ' docs/HELIOS_PRESENT_SYNC_RETIREMENT.md`)
and the cited text itself, not by adding 10 on faith. Before citing a line,
re-grep it. The file is 5928 lines (`wc -l`).

| Doc section | Lines | What it contributes to this lane |
|---|---|---|
| §2 Executive decision | 122–368 | *Why*: items 1, 6, 9 are this lane. Item 6 is the whole native-Vulkan-on-WDDM design (HVC1/HNR2/HVM1/HLM1/HPM1, "no CPU Host Aperture"); item 9 is HTS1/HQA1/HVC1 control. Item 1's last clause is the KMD contract: walk C64 page tables, execute HOB1, raise `DXGK_INTERRUPT_DMA_COMPLETED` only after host completion. |
| §3 Hard constraints | 369–406 | The bounds every design choice must respect: no Escape for anything; no global/adapter-wide discovery table (the `ProcessContext` may hold *only* a bounded HTS1 session list, consulted *only* at create-context); no CPU wait on GPU completion in steady state; no version/feature fallback; no cross-adapter inference. |
| §10.1 Invariants | 910–972 | The 16 numbered invariants. #3, #10, #11, #12, #13, #14, #15, #16 are directly this lane's acceptance criteria. |
| §10.2 Version/feature admission | 973–1013 | The conjunctive admission table. Rows *KMD feature*, *native caps*, *topology*, *normal native ICD*, *translated pre-queue control*, *package generation* are this lane's gates. |
| §10.3 Resource identity | 1014–1189 | `HeliosWddmAllocationDescV2` (HWA2) byte table at 1043–1069 — 168 bytes, written by KMD on create only, const on open. Plus the C57 `D3DKMTQueryResourceInfoFromNtHandle` + `D3DKMTOpenResourceFromNtHandle` KMD-side open path (1159–1178). |
| §10.4 Record-only translation | 1190–1430 | HTS1 pre-queue control + HQA1 (72 B table, 1230–1245), HOB1 (112 B header table, 1267–1286) with its 40-byte use / 16-byte operand records, HOS1 (64 B, 1318–1330), endpoint FIFO/arrival-order rule (1253–1261), pure/deferred/GPU-dependent opcode classes (1345–1370). |
| §10.6 D3D12 exec/fences | 1464–1684 | KMD-visible parts: HOC1 (64 B, 1496–1508) admission rules at 1510–1517; `DmaBufferPrivateDataSize` = 64 UMD prefix + KMD suffix and `DmaBufferUmdPrivateDataSize=64` (1332–1334, restated 1558); virtual submit enqueue without CPU-dereferencing the GPUVA. |
| §10.7 Native Vulkan KMT + WSI | 1685–2710 | **The core of this lane.** HVC1 (32 B, 1708–1717), `DXGK_CONTEXTINFO` shape (1735–1738), HNR2 (112 B, 1765–1787) with 24-byte use / 16-byte patch records, `DxgkDdiRender` contract (1831–1850), `HNR2PhysicalCapability` 48 B + output-patch rule (1852–1864), `DxgkDdiPatch` contract (1866–1876), physical `DxgkDdiSubmitCommand` (1878–1906), C51 progress fences (1908–1934), HVM1 (64 B, 1946–1962; the byte table itself is 1949–1962), the two-segment table (1974–1995; the two-item list is 1980–1986), allocation flags (1997–2011), HPM1 page tables/C64 (2023–2035), reply pool (2037–2060), the two Vulkan memory types (2062–2078), paging DMA + HPM1 packet (2080–2133), HVR1 (80 B, 2144–2159), teardown (2180–2185). Lines 2206–2710 are the WSI layer — **not this lane**, read only for the ICD-side contract HNR2 must satisfy. |
| §10.9 Failure policy | 2844–2881 | The required result for every failure class. Rows for HNR2, HPM1, HLM1, C57, HTS1 INIT, HQA1, HOS1/HOB1, HOC1, C60, HVR1, 4096/8192 overflow, HQC1, and C51 are this lane's error returns. |
| §12.1 Core-0116 native-fence contract | 3130–3196 | `HeliosNativeFencePddV1` (HNF1, 64 B table 3136–3146) and the create/open/wait/signal/CPU/close/multi-adapter sequence. KMD identity is **exclusively** `hGlobalNativeFence`/`hLocalNativeFence`. |
| §13 No-recursion proof | 3338–3416 | §13.2 bullets 5–6 are KMD obligations; §13.3 is the lock/reentrancy rule set: slot-pool lock released before KMT/scheduler/host; ProcessContext session-list lock never held across KMT/runtime/host/allocation/GPU waits; endpoint FIFO lock held only to insert/remove. |
| §14 Lifetime/identity/reset | 3417–3469 | The proof table rows *Allocation reuse*, *Multiple queues*, *Multiple devices/processes*, *Device removal/TDR*, *Adapter reset*, *Late host callback*, plus the two ownership chains at 3446–3459. |
| §15 Security | 3470–3541 | HTS1 capability is a CSPRNG nonce, zeroed out of DMA, never logged/persisted/reused; HVC1/HNR2/HVM1 carry no pointer/handle/PID/host ID; HPM1 placement keyed by live KMD allocation generation. |
| §17.6 KMD and logic manifest | 4265–4505 | The file-level manifest: delete/modify/add lists, the C58–C64 assignment to `device.rs`+`translation_session.rs`, the HNR2/Patch/SubmitCommand assignment, the `cpu_host_aperture` deletion, the `create_allocation` HOC1 admission, the `pci_caps`/segment ownership split, and the WDDM-3.2 slot audit. |
| §18.1 Static/build gates | 4646–4782 | What "done" means statically: absent Escape objects, byte-identical ABI assertions across protocol/ICD/KMD/QEMU, HTS1 property tests, HPM1/HLM1/HVM1 tests, the `render_user_copy.c` single-C-object rule. |
| §18.2 Queue/Present causal gates | 4783–4974 | The 11 normal-Vulkan gates + 5 record-only-translator gates + 8 per-D3D-queue gates. These are the acceptance suite this lane must be able to pass. |
| Appendix A.2/A.3/A.4 | 5689–5788 | Per-callsite disposition for existing KMD code. Rows citing `kmd_render/` and `kmd_logic/` are the authoritative "what does this current line become". |
| Appendix B.5 | 5893–5928 | The current read-ledger reverse edge, drawn — the thing this lane deletes. |

---

## 2. Current source inventory

⚠ **Every measurement in §2, §3 and §5 was taken on 2026-08-10 at HEAD
`41d13f8` while several sibling authors were writing to this same worktree.**
File-level state can therefore have advanced past what is written here — during
the corrections pass alone, `adapter/tracking.rs` was deleted and
`adapter/allocation_object.rs` appeared. Treat every count and line number as a
regeneration command, not a fact.

Line counts were `wc -l` at commit `d1c820a`; the file-existence column of §2.1
was **re-measured on 2026-08-10 at HEAD `41d13f8`** and one row changed — K7
landed. Re-measure with
`for f in …; do [ -f "$f" ] && echo "EXISTS $(wc -l <$f) $f" || echo "ABSENT $f"; done`.
"Manifest" = §17.6.

### 2.1 Manifest-named files: which exist and which do not

Re-measured 2026-08-10. ⚠ The heading used to read "Files the manifest names
that DO NOT EXIST" and the table was taken as an all-absent list; that is no
longer true.

| Manifest path | Reality (measured 2026-08-10, HEAD `41d13f8`) |
|---|---|
| `kmd_render/src/ddi/native_render.rs` | ABSENT. **ADD.** (Manifest lists it under "Add", 4317.) |
| `kmd_render/src/ddi/native_fence.rs` | ⭐ **EXISTS — 1439 lines.** K7 landed. Wired at `ddi/mod.rs:24` (`pub(crate) mod native_fence;`) with six re-exports at `:76-80`, and four slots registered at `lib.rs:282-285`. ⚠ Everything in it is gated on `native_fence.rs:158 NATIVE_FENCE_ADVERTISED = matches!(SURFACE, Wddm3_2GpuMmu)`, which is **false** today, and `query_adapter_info.rs` still has **no** `DXGKQAITYPE_NATIVE_FENCE_CAPS` arm (`rg NATIVE_FENCE kmd_render/src/ddi/query_adapter_info.rs` → empty), so the caps fill has no caller. K7 is not done; that gap is K8's line item. (4318) |
| `kmd_render/src/ddi/translation_session.rs` | ABSENT. **ADD.** (4319) |
| `kmd_render/src/ddi/native_linear_memory.rs` | ABSENT. **ADD.** (4320) |
| `kmd_render/src/ddi/native_physical_memory.rs` | ABSENT. **ADD.** (4321) |
| `kmd_render/src/adapter/physical_memory.rs` | ABSENT. **ADD.** (4322) |
| `kmd_render/src/render_user_copy.c` | ABSENT. **ADD.** (4325) — the task's source-scope line lists it as if present; it is not. |
| `kmd_render/src/ddi/mpo3.rs`, `direct_scanout.rs`, `diag_etw.rs` | ABSENT (all three, re-checked). **ADD, but not by this lane** — see §4. |
| `kmd_render/src/ddi/wddm32_slot_audit.rs` | ⭐ **EXISTS — 1688 lines**, and is *not* named in the manifest. K0's audit half. Wired at `ddi/mod.rs:33` and called fail-closed from `lib.rs:104`. Listed here so a reader of the manifest does not conclude it is missing. |

Everything else in the source scope exists. `kmd_render/src/seh_shim.c` exists
(45 lines) and is a **DELETE**.

### 2.2 `kmd_render/src/ddi/` — existing

| File | Lines | What it does today | Verdict |
|---|---:|---|---|
| `escape.rs` | 1532 | The whole private `D3DKMTEscape` surface: graphics context/submit/blob map/wait/present-stream verbs, `MAP_READ_LEDGER` + `SCANOUT_EVENT` (`:372-376,751-954`), `QUERY_STATS`, `QUERY_SCANOUT_TIMELINE` (`:361-365,489-558,966-1100`), stream register/unregister + `SUBMIT_VENUS` (`:330-342,1125-1157,1195-1249`). Entry `dxgkddi_escape` at `:254`. | **DELETE** (4269). `DxgkDdiEscape` must be left NULL in the init table (4494). |
| `blob_map.rs` | 193 | User-mode MDL mapping helpers for Escape MAP_BLOB: `map_io_pages_to_user`, `map_nonpaged_page_to_user_readonly`, `unmap_io_pages_from_user` (`:190`), plus two pure enum converters `map_cache_to_mm` (`:58`) and `effective_map_cache` (`:68`). | **DELETE** (4270), **but** `map_cache_to_mm` is still called from `build_paging_buffer.rs:576` and must be *moved* to `mapping.rs` (4396–4398). `effective_map_cache` has no non-Escape caller → deleted with the file. |
| `cpu_host_aperture.rs` | 488 | `DxgkDdiMapCpuHostAperture` (`:294`) / `Unmap` (`:429`), the whole-allocation blob mapper, ~20 `BAR_AP_*` counters, `ValidatedApertureRange` (`:180`). | **DELETE** (4282–4283, 4410–4413). Listed under "Modify:" with an inline "— delete"; the delete reading is the only one consistent with §10.7:1991–1993 and §18.1:4747–4749. |
| `wddm_surface.rs` | 103 | One value `SURFACE: WddmSurface = Wddm2_1GpuMmu` (`:64`) driving five coupled sites; `Wddm3_2GpuMmu` variant already exists (`:56`) with the `E_NOTIMPL` history in the module docs (`:25-28`). | **MODIFY** — change `:64` to `Wddm3_2GpuMmu`. One line, but see §4/§6: it must land in the same commit as the display lane's MPO3 table. |
| `query_adapter_info.rs` | 1287 | `dxgkddi_query_adapter_info` (`:27`) dispatch over 15 `DXGKQAITYPE_*`; `SegmentDescriptorSpec` (`:710`) with `aperture()` (`:732`), `cpu_host_memory()` (`:759`, sets `supports_cpu_host_aperture: true`), `from_bar_flags()` (`:787`, `BarSegFlags` knob, default `0x1C`); writers `write_into_v4` (`:839`) / `v3` (`:902`) / legacy (`:926`); `query_segments` (`:1013`), `query_segments3` (`:1174`), `query_segments_legacy` (`:1213`); `dxgkddi_get_node_metadata` (`:1261`); `query_driver_caps` (`:186`); `query_physical_memory_caps` (`:519`). | **MODIFY** (4277, 4443–4447). Segment table becomes exactly `[aperture id 1, HLM1 id 2]`; id 2 gets `CpuVisible=1, Aperture=CacheCoherent=SupportsCpuHostAperture=SupportsCachedCpuHostAperture=0`, `CpuTranslatedAddress = BAR base`; id 2 never in `DmaBufferSegmentSet`; the query must fail adapter init while HPM1 is not `READY`. Add `DXGKQAITYPE_NATIVE_FENCE_CAPS` (=37) arm. Delete the `cpu_host_memory()` spec and the `supports_*_cpu_host_aperture` fields entirely. |
| `bar_segment.rs` | 242 | BAR sizing/topology decision (`BAR_SEGMENT_MAX_BYTES = 1<<30` at `:12`, `VIDMM_VRAM_MIN/MAX_MB`), `BarSegTopology`. | **MODIFY** (4278, 4439–4441). Owns immutable segment/BAR sizing; the "blob window prefix" concept dies with the aperture. |
| `segment_table.rs` | 188 | `SegmentTable` (`:113`) — the reported-topology record. | **MODIFY** (4279, 4439–4441). Becomes the immutable two-segment table. |
| `gpummu.rs` | 228 | GpuMmu geometry constants: `VIRTUAL_ADDRESS_BIT_COUNT=40`, 4 levels, 4 KiB tables, `APERTURE_SEGMENT_ID=1` (`:78`), `MEMORY_SEGMENT_ID=2` (`:95`), `SYSTEM_MEMORY_SEGMENT_ID=0` (`:99`), `root_page_table_size_bytes` (`:217`). | **MODIFY** (4284). Segment ids already match the doc. The page-table geometry becomes authoritative under C64 rather than decorative. |
| `create_allocation.rs` | 3411 | `dxgkddi_create_allocation`, `open_allocation`, `describe_allocation`, `close/destroy_allocation`, `get_standard_allocation_driver_data`. Live UMD-backing **adoption** at `:1548-1618,2035-2140,2330-2525` (A.2 row **5703**); create-time private write at `:2456-2505` (keep/revise); **illegal** open-time restamp at `:3025-3095` (A.2 row **5694** — delete, validate/read only). Two further doc rows this table used to omit: `:698` cites `create_allocation.rs:1612-1652` (`write_open_identity`) inside §8.1, and A.3 row `:5758` cites `:2035-2290` under "KMD-created-backing groundwork". ⚠ Rows 5694 and 5703 **overlap at `:2456-2505`** — they are not disjoint edits. | **MODIFY** (4280, 4414–4420, 4448–4453). Delete adoption; write HWA2 at create; admit HVM1 roles 1–4 and HOC1 with the exact flag sets; every other shape fails. ⭐ **`docs/retirement/K4-CONTRACT.md` is normative for this file** and completes what §10.3 leaves open (HWA2's create-*input* stage, the geometry source, the dead VidMm tracker). ⛔ It is also a **single-owner file** — see `OWNERSHIP.md §1`: `ddi/display.rs` alone consumes ten of its symbols. |
| `build_paging_buffer.rs` | 1518 | `dxgkddi_build_paging_buffer` (`:1312`), `dxgkddi_set_root_page_table` (`:1484`, record-and-ignore), `dxgkddi_get_root_page_table_size` (`:1501`); `PagingPteShadow` (`:341`), `MdlWindow` (`:487`), synchronous CPU copies into blobs, `MAX_PAGING_SYSTEM_PTES=65_536` (`:292`), ~35 `BAR_*` counters; calls `blob_map::map_cache_to_mm` at `:576`. A.3 row 5785 cites `:804-1229,1400-1475` as the substrate that CPU-copies/no-ops several paging classes. | **MODIFY** (4281, 4421–4428). Becomes real HPM1 DMA encoding: no synchronous copy, no QEMU edit, `MultipassOffset` as the sole continuation, real `SetRootPageTable`/`UPDATE_PAGE_TABLE`/`COPY_PAGE_TABLE_ENTRIES`/`FLUSH_TLB`/map/unmap/residency packets. |
| `submit_command.rs` | 2019 | ~30 public counters (`:23-184`); `dxgkddi_submit_command_virtual` (`:1059`), `dxgkddi_submit_command` (`:1100`), `dxgkddi_preempt_command` (`:1239`), `reset/restart_from_timeout` (`:1270`/`:1304`), `dxgkddi_render` (`:1326`, decodes the D3D12 `HeliosD3D12SubmitCmd` 16 B record and the present marker), `render_km` (`:1779`), `render_gdi` (`:1836`), **`dxgkddi_patch` (`:1877`) which is a bare counter + null-check + `STATUS_SUCCESS`**, `query_current_fence` (`:1889`), `collect_dbg_info` (`:1919`). A.3 row 5785 cites `:1320-1877`; A.4 row 5780 cites `:1470-1665` (present-stream marker/snapshot/raw resid). | **MODIFY** (4285, 4362–4380, 4428–4433). Largest single edit in the lane. `dxgkddi_render` becomes the HNR2 validator/assembler + capability/output-patch generator; `dxgkddi_patch` becomes the infallible idempotent snapshotter; physical `dxgkddi_submit_command` becomes the epoch validator + one `SUBMIT_3D`; `submit_command_virtual` becomes the HOS1 validator + nonblocking GPUVA enqueue. `render_gdi` should die with the GDI path (already retired per memory 42ND) — confirm before deleting. |
| `scheduler.rs` | 591 | HW-context/HW-queue stubs (`dxgkddi_create_hw_context :127`, `create_hw_queue :182`, `submit_command_to_hw_queue :200`, `present_to_hw_queue :240`), `query_dependent_engine_group` (`:57`), `query_engine_status` (`:74`), `reset_engine` (`:97`), `cancel_command` (`:257`), GPU clock calibration (`:440`), `set_stable_power_state` (`:544`), `set_virtual_machine_data` (`:560`), and `fabricated_success_counters()` (`:377`). A.2 row 5734 cites `:122` (present-stream reset drain). | **MODIFY** (4276, 4385). Remove the HWQueue callbacks and cap entirely (A.3 row 5759 + 4385 "Do not advertise HWS/HWQueue"); keep engine/reset/preempt honest. |
| `interrupt.rs` | 414 | `request_wddm_completion_dpc` (`:36`), `drain_used_and_complete` (`:57`), `dxgkddi_interrupt_routine` (`:310`), `dxgkddi_dpc_routine` (`:356`), `dxgkddi_control_interrupt` (`:393`). A.2 row 5734 cites `:124` (present-stream counter). | **MODIFY** (4288). Add `DXGK_INTERRUPT_NATIVE_FENCE_SIGNALED` (=19) reporting; make DMA-completed reporting the ordered one-engine frontier (§10.7:1896–1900). |
| `lifecycle.rs` | 589 | `dxgkddi_start_device` (`:96`, ignores `DXGK_START_INFO` per A.3 row 5759), `bring_up_venus` (`:63`), `stop_device` (`:404`), `remove_device` (`:488`), `dispatch_io_request` (`:511`), `set_power_state` (`:545`); `adapter.read_ledger.init_page()` at `:169`. | **MODIFY** (4289, 4402–4406). StartDevice must store the adapter LUID from `DXGK_START_INFO`, negotiate HPM1, validate/reserve the complete linear BAR, and initialize placement/epoch state **before** any segment query; non-`READY` fails adapter init. `bring_up_venus` (adapter-global Venus) is deleted — the namespace moves to HTS1. |
| `mod.rs` | 93 | Module list + `pub use` re-exports. | **MODIFY** (4300). Drop `escape`/`blob_map`/`cpu_host_aperture` modules and exports; add the five new modules. **Shared with the display lane** — see §4. |
| `add_device.rs`, `base.rs`, `child.rs`, `hpd.rs` | 41/79/225/324 | Not named in §17.6. `base.rs` owns `dxgkddi_control_etw_logging`, which §17.6:4482–4488 reassigns to the ETW work. | Out of this lane's edit set; flag `base.rs::dxgkddi_control_etw_logging` as a cross-lane request. |
| `display.rs`, `present_packet.rs`, `vidpn.rs`, `scanout_trace.rs`, `scanout_timeline.rs` | 2962/1066/1238/1067/227 | Display lane. | **Not this lane** (except the mechanical deletions listed in §4). |

### 2.3 `kmd_render/src/adapter/`

| File | Lines | What it does today | Verdict |
|---|---:|---|---|
| `mod.rs` | 1630 | `AdapterContext` (`:411`) — the god object: `StartedState` (`:65`), `AdapterKnobs` (`:122`, incl. `bar_seg_flags` default `0x1C` at `:231`), `ScanoutMode` (`:345`), `TransportGeneration` (`:400`), programming-gate machinery (`:825-983`), `impl AdapterContext` (`:985-1591`), `Drop` (`:1592`). Owns `read_ledger` (`:27,34-36`) and the Venus/adapter-global bring-up state cited by A.3 row 5783 (`:395-409`). | **MODIFY** (4292). Delete the read-ledger member/exports and adapter-global Venus context/ring state; add HPM1 placement state ownership (or delegate to the new `adapter/physical_memory.rs`) and the bounded ProcessContext session list wiring. |
| `locks.rs` | 331 | Lock-order documentation + `WddmNotifyGuard` (`:58`), `NotifyOrdered` (`:76`), `ScanoutGuard` (`:140`). `:8-9` documents the read-ledger leaf lock. | **MODIFY** (4293). Re-derive the lock order for the HTS1 session list / endpoint FIFO / slot pool per §13.3. |
| `read_ledger.rs` | 660 | The 65-slot raw-`resid` ledger page + 16-entry event table + generation-preserving reset. | **DELETE** (4296, A.2 row 5718). |
| `segments.rs` | 99 | `PagingRam` (`:27`), `BarSegment` (`:41`, `gpa`/`size`/`seg_id`), `alloc_contiguous_ram` (`:66`). | **MODIFY** (4295, 4439–4441). Becomes immutable BAR/segment sizing with no CPU-aperture partition concept. |
| `physical_memory.rs` | — | Does not exist. | **ADD** (4322) — physical-placement ownership, epochs, ordering. |
| `kobj.rs` | 630 | ⛔ **CORRECTED 2026-08-10 — this row used to read "KMD allocation-object state". It is not that.** `kobj` = *kernel object*. Its own module doc (`kmd_render/src/adapter/kobj.rs:1-3`) reads: "The five embedded **kernel dispatcher objects** and their lifecycles: the venus and scanout mutexes, the HPD worker's wake/exit events and thread, and the VSync heartbeat timer/DPC pair." `rg 'alloc\|generation\|HWA2\|HVM1\|segment' kmd_render/src/adapter/kobj.rs` matches only `ExAllocateTimer` / "allocation failed" / "allocation leak" prose and `pending_vidpn_allocation` (`:592`, the display lane's pending-flip slot) — **zero allocation state and no generation of any kind**. `impl AdapterContext` really is at `:68` and the 630-line count is right; only the description was wrong, which is exactly how a stale line survives review. | **NOT K4's FILE.** Removed from K4's exclusive set per K4-CONTRACT §3. It is HPD/VSync lifecycle machinery called from `ddi/lifecycle.rs` (`:384,391,435,436,494,583,586`) and `adapter/scanout.rs:1297`; giving K4 an exclusive lock on the VSync heartbeat buys the allocation model nothing and manufactures a serialization hazard against K10 and the display lane. Natural claimant: **K10**. **The allocation object gets a new file instead — `kmd_render/src/adapter/allocation_object.rs` (ADD, K4, no other claimant), K4-CONTRACT §3.1.** There is nothing to move it *from*: `AllocationContext`/`OpenAllocationContext` are `Box::into_raw`'d inside `create_allocation.rs` (`:2606`, `:3032-3047`), so `allocation_generation` is **created**, not migrated. |
| `backing.rs` | 108 | `SystemBackingSnapshot` (`:25`) / `SystemBackingTable` (`:38`) — the paging system-backing mirror. ⚠ Precision the old row lacked: `pages` is `Arc<[u64]>` of **PFNs**, not bytes (`build_paging_buffer.rs:734,745` push `pa >> 12`), so the table is a *descriptor mirror*; what §10.7:1940–1943 forbids is the byte **copy** it enables (`copy_blob_system_pages`, `build_paging_buffer.rs:597`). ⛔ That clause's named replacement — "HPM1 is the actual paging/device protocol" — was **DECLINED, not deferred** (`FINDINGS.md` F5, and `K4-CONTRACT.md` §4: rescoped, not blocked), so the deletion has no successor mechanism today and must be an explicit recorded decision, not a side effect. | **DELETE the file — but the deletion belongs to K3, not K4.** Measured 2026-08-10 with `rg -n 'system_backings\|SystemBackingTable\|SystemBackingSnapshot' kmd_render/src/`: **`ddi/build_paging_buffer.rs` 10 references (K3)** — `use :59`, construct `:751`/`:883`, `replace :758`/`:890`, `snapshot :778`, `remove :898`/`:984`/`:1406`, `contains :1432`; **`adapter/mod.rs` 3** (export `:32`, field `:500`, init `:1060`); **`ddi/display.rs` 2** (`contains :632` and `:702`, feeding `mirror_present_system_backing` at `:729`/`:1038`) — **display lane**; **`ddi/create_allocation.rs` 1** (`remove :1836`) — **K4**. ⇒ K4 removes exactly one line; the file dies with K3, gated on the display-lane cross-lane request in §4. ⭐ **Re-measured after K4 landed** (re-run the `rg` above rather than trusting these): K4's call site is gone, replaced by a tombstone comment reading "`adapter.system_backings.remove(ctx.resource_id)` was here"; `build_paging_buffer.rs` still 10, `adapter/mod.rs` 3 (`:32`/`:499`/`:1066`), `display.rs` 2 (`:632`/`:702`). **All 16 remaining references are outside K4.** ⚠ No `create_allocation.rs` line number is given: that file was in flight while this was written. |
| `tracking.rs` | 67 | `TrackerIdentity` (`:9`) / `VidMmTrackerTable` (`:14`). Its single behavioural effect is `create_allocation.rs:2517` — `let vidmm_size = if tracker_attested { PAGE } else { size };` — so an attested DEVICE_MEMORY adopt is charged **one page** to VidMm instead of its real size. | **DELETE, unambiguously. No longer a flag.** Measured `rg -n 'vidmm_trackers\|VidMmTrackerTable\|TrackerIdentity' kmd_render/src/`: the only consumers are `create_allocation.rs` (`remove :1823`, `matches :2436`, `register :2596`) plus three lines in `adapter/mod.rs` (`:40` export, `:503` field, `:1061` init). Nothing else in the tree touches it. Its wire carrier has no successor: HWA2 carries no tracker/cookie/global-share field (`rg -in tracker protocol/src/wddm.rs` → empty) and its kind vocabulary (`protocol/src/wddm.rs:135-150`) has no `TRACKING` and no `DEVICE_MEMORY` member. K4-CONTRACT §6 makes this final: nothing was "folded into" HWA2. ⚠ **Record, do not absorb:** every previously-attested adopt now charges its full size to VidMm — a Task-Manager-visible accounting change. The three `adapter/mod.rs` lines are a cross-lane request (that file is K1/K2/K10-contended, §3). |
| `scanout.rs` | 1300 | Display/scanout binding + the raw-`resid` ticket issue at `:243-260,403` and `:1025-1054,1181-1187` (A.2 rows 5720, 5732). | **MODIFY, but display lane owns the semantics.** This lane only removes the read-ledger ticket calls. **Serialize** — §4. |

### 2.4 `kmd_render/src/` top level

| File | Lines | Today | Verdict |
|---|---:|---|---|
| `lib.rs` | 224 | `DriverEntry` (`:68`) + `build_ddi_table()` (`:85`). `data.Version` from `wddm_surface::SURFACE` (`:103`); registers `DxgkDdiEscape` (`:182`), `MapCpuHostAperture`/`Unmap` (`:154-155`), the six HW-queue/context slots (`:166-172`), `Render`/`RenderKm`/`RenderGdi`/`Patch` (`:207-210`), `SetRootPageTable`/`GetRootPageTableSize` (`:217-218`). | **MODIFY** (4312, 4327–4333, 4460–4504). The WDDM-3.2 slot-and-cap audit lives here. **Shared with the display lane.** |
| `device.rs` | 481 | `DeviceContext` (`:20`, holds `adapter` + `creator_process`), `ContextContext` (`:38`, holds the D4b snapshot stash and the present-stream marker `SpinLock<Option<(u32,u32,u64)>>`), `ContextHandleRef` (`:100`), `DeviceHandleRef` (`:208`), `ProcessContext` (`:247`, **deliberately empty**), `dxgkddi_create_device` (`:256`), `destroy_device` (`:288`, unmaps user MDLs at `:323`, `read_ledger.reclaim_events_for_owner` at `:352`), `create_context` (`:389`, a stub per A.3 row 5783), `destroy_context` (`:435`), `create_process` (`:451`), `destroy_process` (`:471`). | **MODIFY** (4297, 4335–4358). `ProcessContext` gains the bounded HTS1 session list; `DeviceContext` records the exact `hKmdProcess`/adapter/package generation; `create_context` parses HQA1 (D3D) or HVC1 (raw) and stores a direct strong session/endpoint ref in `ContextContext`; both stashes (`snap_*`, `present_stream_marker`) are deleted. |
| `diag.rs` | 759 | `DiagLevel`-gated breadcrumb ring, `FaultCounter` (`:131`) with a compile-time name-collision proof (`:307`), `CounterBlock`/`CounterEntry`, registry writers. | **MODIFY** (4298). Keep bounded counters as internal state only; every userspace-visible query route dies with Escape. ETW is the new external surface (`diag_etw.rs`, other lane). |
| `mapping.rs` | 237 | `MappingTable` (`:92`) — user-VA blob mapping registry, `MAX_MAPPINGS=8192` (`:41`), `READ_LEDGER_MAPPING_ID` (`:50`), `MAPPING_FULL_REJECTS` (`:54`). | **MODIFY** (4299, 4396–4398). The whole `MappingTable` is Escape MAP_BLOB state → delete it; the file survives *only* as the home for the moved pure cache-enum conversion. See §6 for the ambiguity. |
| `irql.rs` | 165 | `PASSIVE_LEVEL_IRQL` (`:77`), `IRQL_ASSUME_BAD` (`:96`), `PassiveLevel` token (`:113`). | **MODIFY** (4313). New PASSIVE-only DDIs (`DxgkDdiRender`, native-fence create/open) thread the token; paging Submit at DISPATCH must not. Small. |
| `sync.rs` | 186 | `SpinLock` + fixed-capacity vector. | Not named. Reuse as-is for the session list / endpoint FIFO / slot pool. |
| `dxgk.rs`, `error.rs` | 23/33 | Bindgen include + NTSTATUS helpers. | Grows mechanically with new bindings. |
| `seh_shim.c` | 45 | `__try/__except` wrapper for `MmMapLockedPagesSpecifyCache(UserMode)`. | **DELETE** (4271). |
| `render_user_copy.c` | — | Does not exist. | **ADD** (4325, 4393–4396) — bounds-check + probe + copy `pCommand[0..CommandLength)` under `__try/__except`, returning `STATUS_INVALID_USER_BUFFER` on exception. It is the **only** C object in the package (§18.1:4680–4683). |

### 2.5 `kmd_render/src/virtio/`

| File | Lines | Today | Verdict |
|---|---:|---|---|
| `gpu/mod.rs` | 6663 | The virtio-gpu command engine. A.2 row 5733 cites `:1725-1950,2114-2143,3683-3821,4761-5177,5869-6220,6332-6574` — present-stream slots and the adapter-global cross-lane `wddm_pending` correlation/FIFO; A.3 row 5757 cites `:5869-6220,6331-6593` — the adapter-global WDDM boundary queue with immediate/overflow/rebase paths. | **MODIFY** (4303). Replace adapter-global correlation with exact per-context `SubmissionFenceId` state; add the HNR2/HPM1 private hardware packets. Largest file in the tree — plan a sub-decomposition. |
| `gpu/resource_tables.rs` | 606 | Host resource-id tables. | **MODIFY** (4304, 4442) — becomes "allocation renderer views/generations" only. |
| `ctrl.rs` | 1710 | Async control-queue plumbing; `:1177-1182,1236-1242` call `read_ledger.note_alloc_retired`; `:1323-1419` attaches stream markers (A.2 row 5732); A.3 row 5785 cites `:861-911,1038-1119` (scan/retry/sleep around one raw resource id). | **MODIFY** (4306). |
| `counters.rs` | 562 | Transport counters; `:432-493` are present-stream counters (A.2 row 5734). | **MODIFY** (4307). |
| `pci_caps.rs` | 208 | `HostVisibleWindow` (`:31`); A.3 row 5784 cites `:15-35,101-140`. | **MODIFY** (4305, 4435–4438). Must discover the exact guest-physical base and **non-padded** length of the realized prefetchable 64-bit host-visible BAR; StartDevice requires those to equal the later HLM1 descriptor and refuses missing/short/overlapping/overflowing/mismatched. |
| `venus/bringup.rs` | 562 | `allocate_host_visible_blob` (`:31`), `VenusRing::bring_up`, `VenusInstance` — adapter-global Venus namespace (A.3 row 5783 cites `:73-176`). | **MODIFY** (4308) → effectively rewritten: one namespace per HTS1 session, no bring-up ring. |
| `venus/ring.rs` | 792 | `KernelMap`, `RingMap`, `ReplyCheck`, spin/sleep head waits (A.3 row 5783 cites `:571-740`). | **MODIFY** (4309) → the shared ring and head/tail polling are **deleted** (§2 item 6: "Shared Venus rings and their head/tail polling are deleted"; §10.7:2176–2178: no `vkCreateRingMESA`, `NotifyRing`, `WaitRingSeqno`, `WaitVirtqueueSeqno`). Retain only the `Writer`/`EncodedStream` encoder if still needed. |
| `venus/protocol.rs` | 227 | `OptimalImageTransport` etc. | **MODIFY** (4310). |
| `venus/scanout.rs` | 721 | Scanout Venus commands. | **MODIFY** (4311) — display-adjacent; coordinate. |
| `venus/commands.rs`, `venus/present.rs`, `venus/mod.rs`, `venus/diagnostics.rs` | 1476/1436/469/115 | Not named individually in §17.6. `present.rs` is the windowed-BLT/present path. | Almost certainly **MODIFY/DELETE**; not named → flag as an inference (§6). |
| `mod.rs`, `hal.rs`, `config.rs` | 89/417/77 | `VirtioError` (`:40`), virtio HAL over WDK, PCI config access. | Largely unchanged; `mod.rs` may need new error variants. |

### 2.6 `kmd_logic/`

| File | Lines | Today | Verdict |
|---|---:|---|---|
| `src/lib.rs` | **6344** (was 5596) | **211** passing host tests (was 189, was 172 — regenerate, do not cite; §5. The "189" stood here while `tools/retirement-gates.sh` printed 211, which is the whole reason this cell says *regenerate*). Module offsets re-measured 2026-08-10 with `rg -n '^pub mod \|^mod ' kmd_logic/src/lib.rs`: `vsync_deadline` (`:30`), the Venus `Writer`/encoders + `tests` (`:957`), `sorted_splice_tests` (`:1877`), `scanout_retire` (`:2056`), `scanout_lease` (`:2101`), `scanout_cadence` (`:2559`), `scanout_worker_bind` (`:2694`), `scanout_presentation_epoch` (`:2774`), `scanout_publish_txn` (`:2814`), `scanout_fast_bind` (`:2950`), `scanout_refresh` (`:3060`), **`scanout_read_ledger` (`:4079`) + `scanout_read_ledger_tests` (`:4335`)**, `snapshot_bind` (`:4564`), `windowed_blt_token` (`:4700`), `wddm_head_bound` (`:4980`), `wddm_boundary` (`:5142`), **`present_stream` (`:5346`) + `present_stream_tests` (`:5476`) + `present_stream_boundary_tests` (`:5530`)**, ⭐ `native_fence_lifecycle` (`:5625`) + tests (`:6089`) — **added by commit `476f9ce`, i.e. K13 is partly landed**. | **MODIFY** (4315). Delete `scanout_read_ledger*` and `present_stream*` (A.2 row 5721, A.2 row 5734). Add executable models per §17.6:4315 "exact batch/queue/flip tests" and A.2 row 5721: per-context submission completion, Core-0116 native-fence lifecycle, plane candidate/current/backend-release, reset, mode transition, destruction, multi-queue/swapchain. |
| `Cargo.toml` | 15 | Deliberately dependency-free (no `wdk-sys`, no `helios_protocol`). | **Decision point** — see §6: HNR2/HOB1/HPM1 parser models want the protocol structs, but adding a `helios_protocol` edge breaks the crate's stated invariant. |

### 2.7 `kmd_render/` build inputs

| File | Lines | Today | Verdict |
|---|---:|---|---|
| `build.rs` | 311 | `generate_dxgk_bindings()` over `ntddk.h`+`dispmprt.h`+`d3dkmddi.h`, `compile_version_resource()`, **`compile_seh_shim()`**, `configure_binary_build()`, links `displib`. Header comment pins WDK 10.0.26100 and leaves `DXGKDDI_INTERFACE_VERSION` at the header default. | **MODIFY** (4301, 4327, 4391–4394). Replace `compile_seh_shim` with `compile_render_user_copy`; update the comment so no Escape/user-mapping symbol survives. |
| `Cargo.toml` | 67 | `cc = "1"` build-dep documented as "Compiles src/seh_shim.c". | **MODIFY** (4302). Retain `cc`, retarget the comment to `render_user_copy.c` (4392–4394). |
| `helios_kmd_render.inx` | 120 | `UserModeDriverName` REG_MULTI_SZ (`:102`) `helios_umd.dll ×3, helios_umd12.dll`; `InstalledDisplayDrivers` (`:103`); `MSISupported=0`, `MessageNumberLimit=1` (`:74-75`). | **MODIFY** (4314). Package-generation stamping, any new service parameters, and removal of retired knobs. The doc does not say *what* changes here — see §6. |
| `Cargo.make.toml` | — | Not named in §17.6. Stages both UMDs, stampinf from `driver-version.env`. | Unchanged unless the new C object needs a build step. |
| `driver-version.env` | — | The **single** KMD version site (CLAUDE.md invariant). | Bump once, in the activation commit. |

---

## 3. Work decomposition

Ordered. Each unit lists the files it **exclusively owns** for parallel safety.
Units sharing a file are called out explicitly.

| ID | Goal | Owns (exclusive) | Depends on | Size | Status in the tree (grepped 2026-08-10, HEAD `41d13f8`) |
|---|---|---|---|---|---|
| **K0** | WDDM-3.2 slot-and-cap audit + module wiring skeleton. Generate the full WDK `DRIVER_INITIALIZATION_DATA` layout, classify every slot through 3.2 as implemented or cap-disabled, register the new modules as loudly-refusing stubs, leave `DxgkDdiEscape = NULL`, drop the HW-queue and CPU-host-aperture slots. Flip `SURFACE` to `Wddm3_2GpuMmu` **last**, after K0-display lands. | `src/lib.rs`, `src/ddi/mod.rs`, `src/ddi/wddm_surface.rs`, `build.rs`, `Cargo.toml` | protocol lane (package-generation constant); **display lane** for the MPO3/Display-Core slots | M | **PARTIAL.** `ddi/wddm32_slot_audit.rs` EXISTS (1688 l), wired `ddi/mod.rs:33`, called fail-closed at `lib.rs:104`. ❌ `DxgkDdiEscape` still registered (`lib.rs:229`), `MapCpuHostAperture` still registered (`lib.rs:201`), HW-context/HW-queue slots still registered (`lib.rs:213,215`); none of the five new modules exists to be stubbed. |
| **K1** | Demolition: delete Escape, blob map, CPU host aperture, read ledger, SEH shim, and every reference to them. Move `map_cache_to_mm` into `mapping.rs` and delete `MappingTable`. | `src/ddi/escape.rs` (del), `src/ddi/blob_map.rs` (del), `src/ddi/cpu_host_aperture.rs` (del), `src/adapter/read_ledger.rs` (del), `src/seh_shim.c` (del), `src/mapping.rs`, `src/render_user_copy.c` (add) | K0 (table slots removed first, or the build breaks) | M | **NOT STARTED.** All five delete targets present: `escape.rs` 1532, `blob_map.rs` 193, `cpu_host_aperture.rs` 488, `adapter/read_ledger.rs` 660, `seh_shim.c` 45; `mapping.rs` still 237; `render_user_copy.c` ABSENT. |
| **K2** | HPM1 negotiation + BAR admission + the immutable two-segment table. StartDevice negotiates HPM1, validates/reserves the complete prefetchable 64-bit host-visible BAR, allocates fixed nonpaged `HeliosPhysicalMemoryState`; QuerySegment4 then reports exactly `[aperture id 1, HLM1 id 2]`. | `src/adapter/physical_memory.rs` (add), `src/ddi/native_linear_memory.rs` (add), `src/adapter/segments.rs`, `src/ddi/bar_segment.rs`, `src/ddi/segment_table.rs`, `src/virtio/pci_caps.rs` | protocol `physical_memory.rs`; K1 (HAP gone) | L | **NOT STARTED.** `adapter/physical_memory.rs` and `ddi/native_linear_memory.rs` ABSENT; `segment_table.rs` still the `[Aperture, Bar]` shape. ⚠ De-risked but not done — `FINDINGS.md` F2. |
| **K3** | HPM1 paging DMA packet encoding + C64 authoritative page tables. `BuildPagingBuffer` emits real bounded HPM1 work for every reachable transfer/transfer2/fill/fill2/discard/map/unmap/PTE/TLB/residency op; paging `Patch` becomes the mandatory infallible no-op; paging `SubmitCommand` runs at DISPATCH and only enqueues. | `src/ddi/native_physical_memory.rs` (add), `src/ddi/build_paging_buffer.rs`, `src/ddi/gpummu.rs` | K2; protocol `physical_memory.rs`; **QEMU lane** for the terminal acknowledgement | XL | **NOT STARTED, and the host half is PARKED** (`FINDINGS.md` F5). `ddi/native_physical_memory.rs` ABSENT; `build_paging_buffer.rs` unchanged at 1518 lines. |
| **K4** | Allocation object model: HWA2 create-time descriptor, HVM1 roles 1–4, HOC1, and the deletion of UMD-backing adoption + open-time restamping. ⭐ **`docs/retirement/K4-CONTRACT.md` is normative for this unit and wins over this row wherever they differ.** | `src/ddi/create_allocation.rs`, `src/adapter/allocation_object.rs` **(add)**, `src/adapter/tracking.rs` (del). ⛔ **`src/adapter/kobj.rs` REMOVED from this set** — it is kernel dispatcher objects, not allocation state (§2.3). ⛔ **`src/adapter/backing.rs` REMOVED** — K4 owned 1 of its non-definition references and has now removed it, leaving **16 references, none of them K4's** (§2.3 has the per-file re-measurement); the file's deletion is re-sequenced to K3 (§2.3, §4). ⚠ `create_allocation.rs` is a single-owner file (`OWNERSHIP.md §1`): renaming or deleting its scanout/present accessor family breaks `ddi/display.rs` at compile time, in a build only the VM can run. | K2 (segments/placement) — but see K4-CONTRACT §4: **K2 is RESCOPED, not blocked** (HPM1 was declined, not deferred; F5's Consequence is that no lane in flight has a QEMU dependency), so K4 does not wait on it. ⛔ **This cell said K4 "refuses role-4 placement with a named counter until K2 lands" — that is the WITHDRAWN draft of §4 and it is wrong in both directions.** The ruling is **check the segment, do not hardcode the role**: K4 records `HELIOS_SEGMENT_ID_HLM1`, verifies at runtime that the role's `preferred_segment` is in the table the driver actually reports, and refuses **per role** with `AcSegRole1`…`AcSegRole4` when it is not. Protocol `wddm.rs` + `native_render.rs` are complete and asserted today. | XL | ⭐ **AUTHORED, ALL FIVE COMPONENTS BUILD, NOTHING HAS RUN.** (This cell said "NOT STARTED … `create_allocation.rs` unchanged at 3411 lines" through the changeset that landed it.) Now: `create_allocation.rs` grew by ~1200 lines (**do not cite a figure — it was still being edited while this cell was written**; run `wc -l`); `adapter/allocation_object.rs` added; `adapter/tracking.rs` deleted; `fn segment_is_reported` plus the four per-role counters `AcSegRole1`…`AcSegRole4` (`rg -n 'segment_is_reported|AcSegRole' kmd_render/src/`). Verified by `tools/retirement-gates.sh` **8/8 PASS** — including §8.5 (open writes no byte of the private buffer) and §8.6 (the retired identity symbols have zero live occurrences; the gate prints its own symbol and tombstone counts, which move as it is strengthened). ⛔ Every HWA2 path is **implemented but never exercised**, `METHOD.md`'s distinct third state. |
| **K5** | HTS1 translation sessions + HQA1 attach. `CreateProcess` allocates one bounded session list; `CreateDevice` records the exact `hKmdProcess`/adapter/package generation; HVC1 control creation makes a provisional session; INIT creates one host Venus context/`VkInstance` and returns generation+CSPRNG capability+endpoint capacity; D3D context create parses HQA1 once and stores a direct strong ref. | `src/ddi/translation_session.rs` (add), `src/device.rs` | K2+K4 (the role-1 HVM1 pool), protocol `translation_session.rs` | L | **NOT STARTED.** `ddi/translation_session.rs` ABSENT; `rg 'HTS1\|session' kmd_render/src/device.rs` → empty. |
| **K6** | HVC1 legacy contexts + HNR2 Render/Patch/SubmitCommand + the context-local slot pool + `DXGK_CONTEXTINFO`. Includes `submit_command_virtual`'s HOS1 validation and nonblocking GPUVA enqueue. | `src/ddi/native_render.rs` (add), `src/ddi/submit_command.rs`, `src/ddi/scheduler.rs` | K1 (`render_user_copy.c`), K3, K4, K5; protocol `native_render.rs`+`wddm.rs` | XL | **NOT STARTED.** `ddi/native_render.rs` ABSENT; `submit_command.rs` unchanged at 2019 lines. |
| **K7** | Native-fence DDI surface: `DxgkDdiCreateNativeFence` / `OpenNativeFence` / `CloseNativeFence` / `DestroyNativeFence` (+ `SetNativeFenceLogBuffer` / `UpdateNativeFenceLogs` classified), `DXGKQAITYPE_NATIVE_FENCE_CAPS`, `DXGK_FEATURE_NATIVE_FENCE` enablement, `DXGK_VIDSCHCAPS::NativeGpuFence=1`, `No64BitAtomics=0`, HNF1 PDD validation. Bounded objects referenced directly by driver global/local handles — no table, no scan, no name. | `src/ddi/native_fence.rs` (add) | K0 (table slots); UMD12 lane consumes the other side | L | ⭐ **LANDED BUT INERT — "implemented and never exercised".** `ddi/native_fence.rs` EXISTS (1439 l); four slots at `lib.rs:282-285`; six re-exports at `ddi/mod.rs:76-80`. ❌ Everything is gated on `native_fence.rs:158 NATIVE_FENCE_ADVERTISED = matches!(SURFACE, Wddm3_2GpuMmu)` = **false**, and `query_adapter_info.rs` has no `DXGKQAITYPE_NATIVE_FENCE_CAPS` arm, so the caps fill has **no caller**. Do not read "the file exists" as "K7 is done". |
| **K8** | Adapter caps + query surface: `query_adapter_info.rs` end state (driver caps, WDDM device caps, node metadata, native-fence caps, physical-memory caps, the segment writers). | `src/ddi/query_adapter_info.rs` | K2, K7 | L | **NOT STARTED.** `rg NATIVE_FENCE kmd_render/src/ddi/query_adapter_info.rs` → empty; `cpu_host_memory()` spec still present. |
| **K9** | Completion ordering + interrupts: `DXGK_INTERRUPT_DMA_COMPLETED` only after exact host completion, one-engine ordered frontier with retained early completions, `DXGK_INTERRUPT_NATIVE_FENCE_SIGNALED`, reset/stale-callback generation invalidation. | `src/ddi/interrupt.rs`, `src/adapter/locks.rs` | K6, K7 | M | **NOT STARTED.** `rg NATIVE_FENCE_SIGNALED kmd_render/src/ddi/interrupt.rs` → empty. |
| **K10** | Adapter/device lifecycle: StartDevice LUID + HPM1 ordering, StopDevice/RemoveDevice drain order, reset generation transitions, deletion of `bring_up_venus`. | `src/ddi/lifecycle.rs`, `src/adapter/mod.rs` | K2, K5, K6 | L | **NOT STARTED.** `bring_up_venus` alive at `lifecycle.rs:63`; `adapter.read_ledger.init_page()` still called at `lifecycle.rs:169`. |
| **K11** | Host transport rewrite: per-context `SubmissionFenceId` state replacing the adapter-global boundary queue; HNR2/HPM1 private hardware packets; one Venus namespace per session; ring/polling deletion. | `src/virtio/gpu/mod.rs`, `src/virtio/gpu/resource_tables.rs`, `src/virtio/ctrl.rs`, `src/virtio/counters.rs`, `src/virtio/venus/*`, `src/virtio/mod.rs` | K5, K6, K3; **QEMU lane** | XL | **NOT STARTED.** `git log d1c820a..HEAD -- kmd_render/src/virtio` is empty — the whole subtree is untouched by the retirement. |
| **K12** | Diagnostics: keep bounded counters as internal state only, remove every Escape-reachable query, keep `FaultCounter`'s compile-time name proof, wire the OS-invoked `CollectDbgInfo{,2}`/`CollectDiagnosticInfo` snapshots. | `src/diag.rs`, `src/irql.rs` | K1; **display/ETW lane** owns `diag_etw.rs` and `base.rs::dxgkddi_control_etw_logging` | M | **NOT STARTED.** `diag.rs` unchanged at 759 lines; the Escape-reachable query routes are still live in `escape.rs`. |
| **K13** | `kmd_logic` model rewrite: delete `scanout_read_ledger*` + `present_stream*`; add HNR2 fragment-assembly, use/operand closure, slot-pool, endpoint-FIFO arrival order, one-engine ordered retirement, native-fence lifecycle, and HPM1 epoch models with tests. | `kmd_logic/src/lib.rs`, `kmd_logic/Cargo.toml` | K6, K7 conceptually; **can start early** — it is pure logic and is the only part testable on Linux | L | **PARTIAL.** ✅ `pub mod native_fence_lifecycle` added at `kmd_logic/src/lib.rs:5625` (tests `:6089`). ❌ `scanout_read_ledger` still `:4079` (tests `:4335`) and `present_stream` still `:5346` (tests `:5476`, `:5530`). |
| **K14** | Packaging: INF changes, driver version bump, package-generation stamping. **Activation commit only.** | `helios_kmd_render.inx`, `driver-version.env` | everything | S | **NOT STARTED — correct.** Activation only. `driver-version.env` still `HELIOS_KMD_VERSION=22.22.263.0`; `helios_kmd_render.inx` untouched. |

**Serialization notes.**

* K0 and K1 both touch `src/ddi/mod.rs` and `src/lib.rs` — K0 first, then K1
  removes the modules it no longer registers. Do not run them concurrently.
* K6 and K9 both touch completion reporting; K6 owns `submit_command.rs`, K9
  owns `interrupt.rs`. The shared surface is `note_wddm_submission` /
  `drain_used_and_complete` — agree the signature in K6 before K9 starts.
* K10 and K2 both touch `lifecycle.rs::dxgkddi_start_device`. K2 adds the HPM1
  negotiation call site; K10 owns the rest of the function. Split by an agreed
  helper boundary (`hpm1_negotiate(passive, adapter) -> Result<..>`) or run
  serially.
* K11 is the only unit that touches `virtio/`. Nothing else may.
* `adapter/mod.rs` is touched by K1 (read-ledger removal), K10 (lifecycle/Venus
  state) and K2 (placement state) — and now K4, which needs three lines gone
  (`:40` export, `:503` field, `:1061` init) when `tracking.rs` dies, plus one
  new per-adapter `AtomicU64` for the allocation generation. **Serialize all
  four**, or have K1 land the read-ledger removal first as a standalone commit.
* `adapter/backing.rs` is **K3's** deletion, not K4's: K3 owns 10 of the
  non-definition references, the display lane 2, `adapter/mod.rs` 3, and K4
  owned exactly 1. ⭐ **K4 has now removed its one line** (a tombstone comment
  stands where it was), so all **16** remaining references are outside K4 —
  the "14" above was the survey figure and did not include `adapter/mod.rs`.
  K3 removes the file. See §4 for the display-lane request that unblocks it.

---

## 4. Shared-file hazards

| File | Other claimant | Why | Serialization |
|---|---|---|---|
| `src/lib.rs` | Display lane (§17.6:4463–4504) must register `DxgkDdiCheckMultiPlaneOverlaySupport3`, `SetVidPnSourceAddressWithMultiPlaneOverlay3`, `GetMultiPlaneOverlayCaps`, `GetPostCompositionCaps`, `ValidateUpdateAllocationProperty`, `ControlModeBehavior`, `PostMultiPlaneOverlayPresent`, `CollectDbgInfo2`, `CollectDiagnosticInfo`. This lane registers the four native-fence slots and removes Escape/HAP/HW-queue slots. | One `build_ddi_table()` function. | **K0 owns the file.** The display lane submits its slot list to K0 as data; K0 lands the complete table in one commit. No concurrent edits. |
| `src/ddi/mod.rs` | Same. | One module list. | Same as above. |
| `src/ddi/wddm_surface.rs` | Display lane. The `SURFACE` flip to 3.2 is *causally owned by the display lane* (it is what makes DWM take the MPO3 path). | One `const`. | **The flip is the last edit of the whole retirement.** Land it only after the display lane's MPO3 table is complete and the cold-DWM admission gate is armed. Until then keep `Wddm2_1GpuMmu` — a premature flip reproduces the `E_NOTIMPL` failure recorded in the module docs at `:25-28`. |
| `build.rs` | Display lane needs bindings for the MPO3/Display-Core structures; ETW work needs `EtwRegister` reachable. | One bindgen invocation. | K0 owns; other lanes request header/allowlist additions. |
| `src/adapter/scanout.rs` | Display lane owns the scanout binding semantics; this lane's source scope includes `adapter/*`. | This lane only deletes the read-ledger ticket calls at `:243-260,403` and `:1025-1054,1181-1187`. | **Display lane owns the file.** This lane files the two deletions as a CROSS-LANE REQUEST rather than editing. |
| `src/ddi/display.rs` — the two `SystemBackingTable` probes | Display lane. Measured 2026-08-10: `adapter.system_backings.contains(...)` at `display.rs:632` (inside a `with_virtio` closure, i.e. at DISPATCH under `virtio_lock`) and at `:702`, whose result gates `mirror_present_system_backing` at `:729` and `:1038`. | `adapter/backing.rs` cannot be deleted while these two calls exist. K3 owns the other 10 references; K4 owns only `create_allocation.rs:1836`. | **CROSS-LANE REQUEST to the display lane:** remove both `contains` probes and the `mirror_present_system_backing` calls they gate, in the same changeset as K3's `backing.rs` deletion. ⛔ Not a mechanical removal — the mirror is what keeps a paged-out CPU-rasterized standard allocation's bytes coherent, and its named replacement (HPM1) is parked (`FINDINGS.md` F5). The display lane must either accept the regression **in writing** or keep the mirror until HPM1 returns. |
| `src/ddi/display.rs` — the ten `create_allocation::` symbols | Display lane. `present_alloc_info`, `PresentAllocationStorage`, `ScanoutTarget` (`:14`), `present_alloc_diag` (`:349,350`), `scanout_allocation_for_resource` (`:866`), `allocation_resource_id` (`:1375`), `set_vidpn_primary_address` (`:1379,1635`), `scanout_alloc_info` (`:1502,2276`), `SCANOUT_ALLOC_FULL` (`:2061`), `submit_primary_scanout_copy` (`:2806`). (`SCANOUT_ALLOCS` at `:862` is a comment reference, not an import.) | K4 rewrites the file these come from. | **K4 must not rename or delete any of the ten.** Any signature change is a CROSS-LANE REQUEST to the display lane, and the breakage only surfaces on the VM. |
| `src/ddi/scanout_trace.rs` | Display lane. Calls `read_ledger_dump_counters()` at `:962` and `read_ledger_reset_counters()` at `:1054`. | K1 deletes those functions. | CROSS-LANE REQUEST — the display lane must remove the two calls in the same changeset as K1. |
| `src/virtio/ctrl.rs` | This lane (K11). But `:1177-1182,1236-1242` call `read_ledger.note_alloc_retired`, needed by K1. | Ordering. | K1 lands the two-line removal; K11 rewrites the file later. |
| `src/ddi/base.rs` | Not in this lane's scope, but owns `dxgkddi_control_etw_logging`, which §17.6:4482–4488 rewires to `diag_etw.rs`. | ETW lane. | CROSS-LANE REQUEST. |
| `src/virtio/venus/scanout.rs`, `venus/present.rs` | Display lane's scanout path runs through these. | K11 rewrites the Venus namespace under them. | Agree the `VenusClient` surface with the display lane before K11 starts. |
| `protocol/src/*` | Protocol lane (§17.1). | Every wire struct this lane parses is defined there. | This lane **consumes** `helios_protocol`; it never defines a wire struct locally. §18.1:4684–4690 requires the sizes/offsets be asserted identically in protocol, ICD, KMD, and QEMU. |
| `kmd_logic/Cargo.toml` | Nobody. | See §6 for the dependency-edge decision. | — |

---

## 5. Build and verification

### What compiles on Linux today

⛔ **Regenerate these numbers; do not cite them.** The previous values (`172` and
`13`) were both stale within days. Run the commands:

```bash
cd /home/rupansh/helios-vgpu/kmd_logic && CARGO_TARGET_DIR=target/linux cargo test
cd /home/rupansh/helios-vgpu/protocol  && CARGO_TARGET_DIR=target/linux cargo test
```

Measured 2026-08-10 at HEAD `41d13f8`, **twice within one session, while sibling
authors were writing to this same worktree** — the numbers moved between the two
runs, which is the whole reason this section now says "regenerate":

| Crate | Recorded in this brief (2026-08-09) | Run 1 (clean tree at `41d13f8`) | Run 2 (same HEAD, sibling lanes' edits in the working tree) |
|---|---:|---:|---:|
| `protocol` | 13 | **140** | **144** |
| `kmd_logic` | 172 | **189** | **214** |

The `13` was off by an order of magnitude — the retirement's whole wire ABI
landed after this brief was written. ⇒ **Never quote a test count from a
document.** Run `bash tools/retirement-gates.sh`, which wraps both crates plus
four more gates (protocol Rust↔C ABI parity, the C mirrors' `_Static_assert`s,
slot-audit staleness against its generator, and the
`VKD3D_HEAP_FLAG_HELIOS_VENUS_EXPORT` cross-repo mirror) into one exit code.

`kmd_logic` and `protocol` are the **only** parts of this lane verifiable on the
Linux host. `kmd_logic` has no `wdk-sys`/bindgen edge by design; `protocol`
builds on both platforms.

### What requires the Windows VM

`kmd_render` cannot be built on Linux at all: `build.rs` runs bindgen over
`ntddk.h`/`dispmprt.h`/`d3dkmddi.h`, shells to `rc.exe` for the version
resource, compiles a C object with `cc`, and links `displib.lib`. The build path
is (per TOOLCHAIN.md / CLAUDE.md):

* `win_cargo` — mirrors `Z:\` to `C:\Users\Rupansh\helios-vgpu`, sets the
  **local** `CARGO_TARGET_DIR` and `LIBCLANG_PATH`. Never build from `Z:\`
  (Rust file IO fails with OS error 87 on the 9p/virtio share).
* `win_build_kmd` — `cargo make` in `kmd_render/`, which extends
  `target/rust-driver-makefile.toml` (hardcoded `target/`, does **not** honor
  `CARGO_TARGET_DIR`), stamps `DriverVer` from `driver-version.env`, and stages
  both UMDs. Requires a prior `win_dxvk` and `win_vkd3d`.
* `win_install_kmd` — sign + DriverStore deploy.

**This recon task must not invoke any `win_*` tool.** The implementer will.

### Header availability (checked this session, host-side, read-only)

* `tmp/dx12/sdk/{d3dkmddi.h,dispmprt.h,d3dukmdt.h}` (WDK 26100 vintage,
  in-repo) **already declare every KMD-side native-fence slot**:
  `DxgkDdiCreateNativeFence`, `DxgkDdiDestroyNativeFence` (dispmprt.h `:3005-3006`),
  `DxgkDdiOpenNativeFence`, `DxgkDdiCloseNativeFence`,
  `DxgkDdiSetNativeFenceLogBuffer`, `DxgkDdiUpdateNativeFenceLogs`
  (`:3033-3036`), `DxgkDdiCollectDbgInfo2` (`:3038`), and all seven MPO3/
  Display-Core slots (`:2893-2898,2934-2935,2978`). `DXGKQAITYPE_NATIVE_FENCE_CAPS = 37`,
  `DXGK_NATIVE_FENCE_CAPS`, `DXGK_VIDSCHCAPS::NativeGpuFence`, and
  `DXGK_INTERRUPT_NATIVE_FENCE_SIGNALED = 19` are all present in
  `d3dkmddi.h`; `D3DDDI_NATIVE_FENCE_PDD_SIZE = 64` and
  `D3DDDI_NATIVE_FENCE = 7` are in `d3dukmdt.h`.
* `/tmp/helios-wdk-28000-src/c/Include/10.0.28000.0/` contains **only**
  `shared/` and `um/` — there is **no `km/dispmprt.h`** in that extraction. The
  28000 `DRIVER_INITIALIZATION_DATA` layout the §18.1:4653 slot audit demands is
  therefore not readable from this host; it must come from the WDK installed on
  the VM.

### The acceptance suite

§18.1 (static) and §18.2 (causal) are the gate. The concrete artifacts §17.7
adds for this lane are `tools/wddm_batch_completion_probe.cpp`,
`tools/wddm_physical_paging_probe.cpp`, `tools/wddm_linear_bar_lock_probe.cpp`,
`tools/finite_venus_reply_probe.cpp`, `tools/translation_session_attach_probe.cpp`,
`tools/native_fence_context_probe.cpp` — all owned by the tooling lane, all
run on the VM under `schtasks` (session-0 launches fake regressions; CLAUDE.md).

---

## 6. Blockers, ambiguities, and contradictions

**1. HLM1's required segment shape is exactly the shape CLAUDE.md records as
ETW-proven to fail AddAdapter with Code 43.**
Doc §10.7:1983–1986 requires segment 2 to be `Aperture=0, CpuVisible=1,
CacheCoherent=0, SupportsCpuHostAperture=0, SupportsCachedCpuHostAperture=0,
CpuTranslatedAddress=BAR base`, restated at §17.6:4444–4447 and gated at
§18.1:4743–4749. CLAUDE.md's invariant table says: *"A SupportsCpuHostAperture
segment must be the LAST reported segment; classic CpuVisible memory segments
are rejected — AddAdapter Code 43 (ETW-proven 2026-07-05)"*, and
`query_adapter_info.rs:757-763` documents the same conclusion in code
(*"CpuVisible deliberately stays 0. If it is set, dxgkrnl treats the union as
CpuTranslatedAddress and does not create the CPU-host-aperture segment
attributes VidMm needs"*). The doc is aware — §10.9:2868 makes "any
CPU-host-aperture callback is observed" *or* Code 43 a package rejection, and
the frozen file's front matter at :27 (NOT §2 — the old §2:17 was a front-matter line, not a §2 line) makes "HLM1's cold two-segment admission" a blocking gate. **This is the
single highest-risk item in the lane and it is cheaply falsifiable before any
code is written**: `BarSegFlags` (REG_DWORD, `adapter/mod.rs:231`, default
`0x1C`) already parameterizes exactly these bits — `0x02` = CpuVisible only,
which is the doc's required shape. Set it, restart the device, and read the
result before committing to K2. Report the outcome; if it Code-43s, the doc's
own failure policy says the package is rejected, not that a fallback is written.

**2. `cpu_host_aperture.rs` and `adapter/read_ledger.rs` appear under
"Modify:" but say "— delete".**
§17.6:4282–4283 and 4296. The prose elsewhere is unambiguous (4410–4413 "Delete
`cpu_host_aperture.rs`, its initialization/table registrations, both
Map/UnmapCpuHostAperture callbacks…"; A.2 row 5718 "Delete all of it"). Taken as
**DELETE**. Recorded because a reader skimming the Modify list would get it
wrong.

**3. `mapping.rs` survives with nothing to hold.**
§17.6:4299 lists it under Modify and 4396–4398 says to move the pure virtio
cache-enum conversion into it. But `mapping.rs`'s entire content is
`MappingTable` — the user-VA blob-mapping registry created by Escape MAP_BLOB,
including `READ_LEDGER_MAPPING_ID` (`:50`), which A.2 row 5718 explicitly lists
for deletion (`src/mapping.rs:44`). §18.1:4677–4679 requires that "no…user-MDL
mapping field…remains". **Conservative reading: delete `MappingTable` entirely;
`mapping.rs` retains only `map_cache_to_mm`.** The doc never says this in one
sentence.

**4. ⛔ WITHDRAWN 2026-08-10 — the `kobj.rs` half of this item was false, and it
is left here (not deleted) so a reader who saw the old text learns why.**

The item used to read, in part: *"`kobj.rs` is where a KMD allocation object's
generation must live for HWA2/HVM1/HPM1"*, and K4's file set in §3 inherited
that claim.

**Why it was withdrawn.** It was an inference drawn from the filename, and the
file says otherwise in its own first three lines. `kmd_render/src/adapter/kobj.rs:1-3`:
*"The five embedded **kernel dispatcher objects** and their lifecycles: the venus
and scanout mutexes, the HPD worker's wake/exit events and thread, and the VSync
heartbeat timer/DPC pair."* `kobj` = **kernel object**, not "KMD object".
`rg 'alloc\|generation\|HWA2\|HVM1\|segment' kmd_render/src/adapter/kobj.rs`
matches only `ExAllocateTimer` prose and the display lane's
`pending_vidpn_allocation` (`:592`). There is no allocation state and **no
generation of any kind** in the file, so nothing could "live" there already and
nothing should be moved there. The coincidence that made the line survive review
is that its other two facts — 630 lines, `impl AdapterContext` at `:68` — are
both correct.

**What replaces it.** K4-CONTRACT §3.1: there is today **no** file that owns the
KMD allocation object (`AllocationContext`/`OpenAllocationContext` are
`Box::into_raw`'d inside `create_allocation.rs` at `:2606` and `:3032-3047`), so
`allocation_generation` is *created*, in a new file
`kmd_render/src/adapter/allocation_object.rs`. `kobj.rs` is dropped from K4
entirely and left to K10. §2.3's row and §3's K4 row are corrected to match.

**The rest of the item stands, restated precisely.** `adapter/backing.rs`,
`adapter/tracking.rs` and `virtio/venus/{commands,present,mod,diagnostics}.rs`
are genuinely unnamed in §17.6 and cannot survive unchanged; the manifest's own
preamble (§17:3745) says it "may not omit a listed live owner", and these are
live owners that were omitted. Two corrections to how `backing.rs` was described:
its `pages` field is `Arc<[u64]>` of **PFNs**, not bytes, so it is a descriptor
mirror rather than the byte copy §10.7:1940–1943 forbids — the forbidden copy is
`copy_blob_system_pages` (`build_paging_buffer.rs:597`), which the table exists
to drive; and that clause's named replacement, HPM1, is **parked**
(`FINDINGS.md` F5), so deleting the mirror today removes a coherency mechanism
with nothing behind it. `tracking.rs` is no longer ambiguous: it is a **DELETE**
(§2.3). §17.6 assigns "allocation renderer views/generations" to
`resource_tables.rs` (4442), which is **K11's** file — so K4 cannot land the
generation there either, and the fail-closed reading of §13.3 (no new
adapter-global map consulted across a host call) puts the generation on the
`AllocationContext` box, reachable directly from `hAllocation`.

**5. `native_linear_memory.rs` vs `native_physical_memory.rs` vs
`adapter/physical_memory.rs`: the doc names three new files and never says what
goes in which.**
The only clue is §17.6:4421 ("`build_paging_buffer.rs` and
`native_physical_memory.rs` must encode actual bounded HPM1 paging DMA") and
4439–4441 ("`adapter/segments.rs`, `bar_segment.rs`, `segment_table.rs`, and
`adapter/physical_memory.rs` own immutable segment/BAR sizing,
physical-placement ownership/epochs, and ordering"). `native_linear_memory.rs`
is named at 4320 and **never mentioned again**. Proposed split, recorded as an
inference: `native_linear_memory.rs` = HLM1 segment descriptor construction +
BAR-linear placement + Lock2 view rules; `native_physical_memory.rs` = HPM1 DMA
packet construction/validation; `adapter/physical_memory.rs` = the
`HeliosPhysicalMemoryState` nonpaged object, placement ownership, epochs.

**6. HNR2's "at most 8192 patch records" is not the WDDM patch-location list.**
§10.7:1780 caps HNR2 *typed patch records* (internal operand descriptors inside
the copied command) at 8192, while §10.7:1735–1736 advertises a **4096**-entry
`DXGK_CONTEXTINFO` patch-location capacity and §10.7:1860 says the *output*
`D3DDDI_PATCHLOCATIONLIST` count "equals use count and never exceeds 4096".
Also §10.7:1836 requires `PatchLocationListInSize=0` on input. These are three
distinct quantities that a careless implementer will conflate. Not a
contradiction — recorded because the failure mode is silent truncation, which
§10.9:2876 forbids.

**7. Every byte table in this lane's ranges is internally consistent
(field sizes sum to the declared struct size). Verified:**
HWA2 168, HQA1 72, HOB1 header 112 + 40-byte use + 16-byte operand, HOS1 64,
HOC1 64, HVC1 32, HNR2 header 112 + 24-byte use + 16-byte patch,
`HNR2PhysicalCapability` 48, HVM1 64, HVR1 80, HNF1 64, HPM1 run 24. The
worst-case aggregate arithmetic at §10.7:1822 (`15*1048576 + 64*112 + 4096*24 +
8192*16 = 15,965,184 < 64*262,144 = 16,777,216`) is correct. **No arithmetic
defect found** — stated affirmatively so nobody re-derives it.

**8. `kmd_logic` must model HNR2/HOB1/HPM1 parsing, but its `Cargo.toml`
forbids the dependency that would make the models real.**
`kmd_logic/Cargo.toml` says the empty `[dependencies]` is deliberate: *"A
dependency edge to `wdk-sys` or the generated `dxgk` bindings would let this
arithmetic read a DXGKARG_* field."* `helios_protocol` is neither of those and
is `no_std`, so a `helios_protocol` edge does **not** violate the stated rule —
but it does violate the literal "Intentionally empty". §17.6:4315 demands "exact
batch/queue/flip tests" in `kmd_logic`; §18.1:4727–4731 demands protocol parsers
prove bounds/checksum/generation/closure. **Recommendation: add
`helios_protocol = { path = "../protocol" }` to `kmd_logic` and rewrite the
comment to name the actual invariant (no `wdk-sys`, no `dxgk` bindings, no
`AdapterContext`).** This is a decision the implementer must make explicitly,
not silently.

**9. WDK version: the doc pins 28000, but the KMD side does not need it.**
§2:124–125 and §18.1:4649 require WDK 10.0.28000 because
`d3d12umddi.h` 26100 stops at Core DDI 0110. That is a **UMD12-lane**
constraint. Verified this session: the in-repo 26100-vintage
`tmp/dx12/sdk/{dispmprt.h,d3dkmddi.h,d3dukmdt.h}` already declare every KMD
native-fence slot, `DXGKQAITYPE_NATIVE_FENCE_CAPS`, `DXGK_NATIVE_FENCE_CAPS`,
`DXGK_INTERRUPT_NATIVE_FENCE_SIGNALED`, and all seven MPO3/Display-Core slots.
So the KMD lane is **not header-blocked** on the WDK upgrade. However
§18.1:4653 demands a slot audit of the **28000** `DRIVER_INITIALIZATION_DATA`,
and the 28000 extraction available on this host has no `km/` directory — the
audit must be done against the VM's installed WDK. **Do not assume 26100 and
28000 have the same `DRIVER_INITIALIZATION_DATA` size; a shorter struct passed
to a longer-expecting dxgkrnl is exactly the `STATUS_REVISION_MISMATCH` /
`BUFFER_TOO_SMALL` class that `build.rs:28-34` already documents.**

**10. `dxgkddi_patch` is currently a no-op and the doc makes it
un-failable.**
`submit_command.rs:1877-1886` is `PATCH_COUNT++; null-check; STATUS_SUCCESS`.
§10.7:1866–1876 requires it to infallibly snapshot `hDeviceSpecificAllocation`,
access bit, `SegmentId`, physical address, allocation generation, byte range and
HPM epoch into the DMA-local capability record, be idempotent, and perform no
host call/mapping/allocation/refcount transfer — *"an impossible output-list/
internal-state mismatch is a driver bug because returning an error from Patch
bugchecks Windows"*. This is the hardest correctness constraint in the lane: all
validation must happen in Render, and Patch must be provably total over
everything Render emitted. §18.2 gate 3 requires invoking Patch **twice** with
the same placement and proving byte-identical DMA.

**11. `DXGKARG_SUBMITCOMMANDVIRTUAL` does not carry `WrittenPrimaries`.**
§10.6:1568–1571 says so explicitly, and §10.9:2872 forbids claiming KMD
revalidates it. The KMD therefore has **no** way to check the primary
association; QEMU independently validates every HOB1 GPUVA/resource operand.
Implementers coming from the current present-marker code will look for the
field. It is not there and must not be faked.

**12. `render_gdi` and the D4b snapshot stash have no disposition.**
`submit_command.rs:1836` (`dxgkddi_render_gdi`) and `device.rs::ContextContext`'s
`snap_*` atomics + `present_stream_marker` are live today. A.4 row 5780 deletes
"present-stream marker, snapshot descriptor, raw resource ID". The GDI path was
retired earlier (memory 42ND) but the DDI is still registered at `lib.rs:209`.
**Conservative reading: delete both the stash and the GDI Render slot**, because
neither can survive §3's "no heuristic identity" and §10.4's "Render private
data is reserved-zero and is never used for attachment" (§2:290,
§10.4:1337–1339). Confirm with the display lane before deleting `render_gdi`,
which is where the legacy BLT present used to land.

**13. The INF change is named but not specified.**
§17.6:4314 lists `helios_kmd_render.inx` under Modify and never says what
changes. §17.1:3806–3808 requires "one protocol/package generation constant
shared by protocol, Mesa, UMD11, UMD12, KMD, QEMU, and installer", and §15:3537
says "No feature registry override is installed (C6)". **Conservative reading:
the INF changes are (a) whatever registry knobs the retirement deletes must be
removed from `Helios_DeviceSettings`, and (b) nothing that looks like a feature
override may be added.** Do not invent an `AddReg` for the package generation
without checking whether the installer (§17.7) owns it instead.

**14. Reply-pool geometry: 64 MiB pool / four 16 MiB slots / 15 MiB per
transaction / 64 MiB per snapshot / four snapshots / 256 MiB per session.**
§10.4:1203–1204 and §10.7:2037–2046 give the pool; §10.7:2135–2141 gives the
snapshot bounds. Note the asymmetry, which is easy to misread: a *slot* is
16 MiB and one HNR2 transaction publishes at most 15 MiB into it, but a
*logical snapshot* may be up to 64 MiB and is drained by multiple 15-MiB
continuation chunks. The `reply capacity bytes` field (HNR2 offset 80) is
`80 + maxChunkBytes`, i.e. it includes the HVR1 header. Consistent, but three
different numbers named "the reply size".

**15. `dxgkddi_create_context` is a stub and must become the single
admission point for three different context kinds.**
`device.rs:389`. After the uplift it must distinguish: (a) raw HVC1 control
(both queue ordinals `UINT32_MAX`), (b) raw HVC1 queue, (c) D3D HQA1 (physical
Render for D3D11 / virtual Submit for D3D12, flags bit 0 vs bit 1, exactly one
set). §10.7:1719 additionally requires `DXGK_CREATECONTEXTFLAGS::VirtualAddressing=0`
for HVC1 and all other unsupported flags zero, while the D3D12 arm arrives
through `DxgkDdiCreateContextVirtual` (retained per §17.6:4371–4372). The doc
never enumerates the full flag set that must be zero for HVC1 — implement it as
"reject any flag bit not explicitly required", which is the fail-closed reading.
