# Lane: DXVK record-only + D3D11 UMD uplift

Reconnaissance brief for the HPS2 retirement. Reference document:
`docs/HELIOS_PRESENT_SYNC_RETIREMENT.md` (**5976** lines, normative).

⛔ **Doc line numbers here were swept +10 on 2026-08-10 and mechanically
verified.** The doc was 5918 when this brief was written, 5928 after commit
`c17f17c` inserted a ten-line banner at `:11-20`, and **5973** after `afebe66`
**appended** the SUPERSEDED CLAIMS INDEX at `:5932`. Only the banner moved
anything: it shifted every body line after 8 by +10, so every citation written
here before 2026-08-10 was 10 too low. All of them have now been shifted, and
later append-only index corrections bring the current file to 5976 without
moving any cited body line. The sweep was checked by requiring each
`§N.M`-anchored cite to land inside
§N.M's own heading-to-heading range — not by adding 10 on faith. ⚠ **Source-file line numbers were deliberately NOT touched** — `foo.rs:123`, `virtio-gpu-virgl.c` function ranges and the like are cites into the tree, not into the doc, and the banner never moved them. If a number here is not a doc line, it was correct before this sweep and is correct now. ⚠ The two rows
already corrected on 2026-08-10 (the `§10.3 Resource identity` row in §1 and the
HWA2 two-stage row) were **not** shifted again. Re-grep the cited text before
relying on any number below, and read the SUPERSEDED CLAIMS INDEX at `:5932`
before treating any doc line as a requirement.
Scope: `dxvk-helios/src/**` (sub-lane A) and `umd/**` + `umd_common/**`
(sub-lane B). No implementation code was written for this brief.

All line numbers below were re-derived from the working tree on 2026-08-09 at
root commit `d1c820a` / dxvk-helios `0c71456`, i.e. the commits section 1 of the
reference pins. Where the reference and the tree disagree, that is called out in
§6.

---

## 1. Normative sources

### Assigned ranges (binding)

| Section | Lines | What it contributes to this lane |
|---|---|---|
| §2 Executive decision | 122–368 | *Why*: translated work becomes actual WDDM work; the Mesa device is record-only at queue submit; the outer UMD owns submission; one-shot deployment with no fallback (362–366). Items 1, 3, 5, 8, 9 are this lane's charter. |
| §3 Hard constraints | 369–406 | No Escape, no global registry/table/polling/thread, no ICD→DXGI/D3D edge, no CPU wait on GPU completion in steady state, no version/feature fallback, no cross-adapter inference. |
| §10.3 Resource identity | **1014–1189** | ⚠ **+10 vs. what this brief originally said** — commit `c17f17c` inserted a ten-line banner at `HELIOS_PRESENT_SYNC_RETIREMENT.md:11-20`, so *every* doc line number written in this brief before 2026-08-10 is 10 too low. Re-grep before citing. Only 1016–1101 (**D3D11**) and 1102–1120 (D3D12↔D3D11 open direction) bind this lane; 1121–1189 is the native-WSI lane. Defines the 168-byte `HeliosWddmAllocationDescV2` (HWA2) byte table (**1043–1069**), the `D3D12_RUNTIME_PRIMARY`/`DIRECT_FLIP_COMPATIBLE` rules (1071–1080), and the **inversion** of the current adoption direction (1082–1093). |
| ⛔ HWA2's two stages | — | **Added 2026-08-10 from `docs/retirement/K4-CONTRACT.md` §1, which is normative for the allocation identity seam and wins over anything below.** The reference's "the KMD writes it only on create" and "the buffer is `[in/out]`" are jointly incomplete: the KMD cannot invent texel dimensions, so **this lane supplies a create-*input* HWA2 and the KMD performs the write of all 168 output bytes.** B4 must therefore leave three things **zero on input** or the create is refused: `allocation_generation` (offset 16), `flags & DIRECT_FLIP_COMPATIBLE` and `flags & D3D12_RUNTIME_PRIMARY` (offset 68) — all three are KMD-only. Every other field this lane supplies is validated and **echoed verbatim**; the KMD refuses rather than silently correcting, so a wrong `byte_size` or plane layout is a create failure, not a fix-up. The validators are `Hwa2Stage::CreateInput` / `validate_create_input` in `protocol/src/wddm.rs`. ⛔ Second, HWA2 carries **no host resource id and no Vulkan memory-type index** (K4-CONTRACT §5) — the `resource_id` at `HeliosWddmOpenIdentity` offset 24 that `umd/src/forward/state.rs` and `umd/src/forward/resource.rs` read today has **no successor field**, so B4 is not a field re-point: those readers must fail loudly with a named counter, never fall back and never fabricate. |
| §10.4 Record-only translation | 1190–1430 | HTS1 pre-queue control (1196–1223), the 72-byte HQA1 create-context PDD table (1230–1245), the 112-byte HOB1 header table (1267–1286), 40-byte use records / 16-byte typed operands (1288–1306), the 64-byte HOS1 (D3D12 only, 1316–1335), the C60 three-way control classification (1345–1376), and the five-clause record-only submit contract (1378–1406). Bounds: 4096 uses, 8192 operands, 15 MiB, no HOB1 fragmented across two outer submissions. |
| §10.5 D3D11 execution and Present | 1431–1463 | The WDDM-2.1-or-later callback-table uplift on a still-physical Render context; the six-step immediate-context flush sequence; DWM opens the source through ordinary OpenResource; Present1's release callback stays a normal runtime callback. |
| §10.8 Display / host-reader retirement | 2711–2843 | The one-primary Display-Core profile the D3D11/DXGI UMD must mirror (2724–2734) and the `pfnCheckDirectFlipSupport` C43/C44 admission rules (2736–2753). 2754–2843 is KMD/QEMU but constrains what the UMD may promise. |
| §17.2 DXVK repository | 3810–3887 | The exact DXVK delete/modify manifest and its "required result" paragraph, plus the Wine-Escape / `\\.\SharedGpuResource` removal and the D3D9-sharing fail-closed rule. |
| §17.4 D3D11 UMD | 4084–4150 | The exact `umd/`+`umd_common/` manifest, the MPO/Direct-Flip replacement, the WDDM-2.1+ uplift, the HQA1/HQC1 bridge duties, and the 4096/8192 closure requirement. |
| §18.2 Queue and Present causal gates | 4783–4974 | Acceptance. 4867–4900 (record-only `vn_instance`) and 4902–4921 (per-queue + the D3D11-specific paragraph) are this lane's gates. |
| Appendix A | 5644–5788 | Per-callsite disposition for every live HPS2/scanout/Escape reader and writer in this lane. A.1 rows 5654–5677 and 5684–5687, A.2 rows 5695, 5698, 5699, 5704–5707, 5713, 5714, 5716, 5724–5726, A.3 rows 5753, 5756, A.4 rows 5777, 5780. |
| Appendix B | 5789–5928 | B.2 (5822–5844) is the exact producer/consumer sequence this lane deletes; B.5 (5893–5920) is the reverse scanout-acquire edge it deletes. |

### Supplementary ranges that bind this lane's files but were not assigned

Read these before implementing; they are normative and they govern files this
lane owns.

| Section | Lines | Why it binds |
|---|---|---|
| §10.1 Invariants | 910–972 | Inv. 1, 2, 5 are the D3D11 submission contract; inv. 6 forbids the ICD→D3D edge; inv. 13 fixes HQA1 as once-only at create-context; inv. 15 fixes HQC1 join semantics. |
| §10.2 Version/feature admission | 973–1013 | Row **999** ("D3D11 callback/Render model") and row **1002** ("D3D11 Direct Flip") are the conjunctive admission gates this lane must satisfy or fail closed. |
| §10.6 D3D11On12 | 1648–1665 | `d3d11_on_12.cpp/.h` are in the §17.2 manifest; this is the only place the required behaviour is specified. |
| §13.1–13.3 | 3338–3415 | §13.2 is the enforcement spec for `vulkan_loader.cpp/h` and `dxvk_instance.cpp/h`; §13.3 forbids holding a translator/Mesa lock across a runtime callback. |
| §17.1 Protocol | 3748–3809 | Declares the HWA2 / HQA1 / HOB1 / HOS1 types this lane *consumes*. `protocol/src/escape.rs` is deleted there, which breaks `umd/src/scanout_acquire.rs` by construction. |
| §18.1 Static/build gates | 4646–4782 | 4657–4676 and 4705–4708 are this lane's static gates (import scans, zero-Escape, WDDM-2.1+ table proof). |

---

## 2. Current source inventory

Every file the manifest names **exists** at the stated path. There are no
missing-file errors in §17.2 or §17.4. Line counts are `wc -l` on the working
tree.

### 2a. DXVK sub-lane — `dxvk-helios/src/**`

| File | Lines | What it does today | Verdict |
|---|---:|---|---|
| `dxvk/dxvk_helios_present_sync.h` | 39 | HPS2 ABI: `class HeliosPresentSync` — `publish/release/lookup/processStartTime/noteGateFlush/gateFlushCount`. | **DELETE** |
| `dxvk/dxvk_helios_present_sync.cpp` | 487 | Maps `C:\ProgramData\Helios\helios_present_sync_v2.bin`, 4096-slot seqlock table, PID-generation reclaim sweep. | **DELETE** |
| `dxvk/dxvk_helios_scanout_acquire.h` | 188 | `class DxvkHeliosScanoutAcquire` — the reverse host-reader gate. | **DELETE** |
| `dxvk/dxvk_helios_scanout_acquire.cpp` | 416 | `GetProcAddress`-resolves `helios_scanout_acquire_enabled` / `helios_venus_memory_res_id` (lines 48, 85), owns generation-keyed private fences and the event-or-10 ms signaler thread. | **DELETE** |
| `dxvk/meson.build` | 117 | Builds both files above at lines **49** and **50**. | **MODIFY** — remove exactly those two entries. |
| `dxvk/dxvk_context.cpp` | 11905 | HPS consumer: `#include` at 11; imported-read sampling 9682–9711; consumer GPU wait 198–204, 9714–9778; named-timeline open/cache 9781–9826; IDD/DWM bounded CPU wait 577–595, 694–707, 9609–9679; scanout-acquire arm/prearm 9390–9405, 9829–10001; staged refresh 10066–10145. | **MODIFY** (large) — delete every HPS/scanout call and the state that feeds it. |
| `dxvk/dxvk_context.h` | 2816 | Consumer state: 1721–1741 (staged), 1743–1763 (probes), 1765–1796 (`HeliosPresentFenceKey`/wait fences), 1798–1814 (imported reads/waited), 1816–1836 (scanout prearm/counters), 1841–1864 (counters), decls 2434–2445. | **MODIFY** (large) |
| `dxvk/dxvk_memory.cpp` | 3066 | `HeliosPresentSync::release` on allocation destruction at **287**. Import identity plumbing 350–425. | **MODIFY** |
| `dxvk/dxvk_memory.h` | 1641 | `heliosResourceId/heliosAllocSize/heliosMemoryTypeIndex` import identity 56–64; `setHeliosPresentSlot`/`takeHeliosPresentSlot`/`m_heliosPresentSlot` 617–640, 728; dedicated-import fields 1089–1094. | **MODIFY** — delete the present-slot triple; replace the raw-`resid` identity with the outer-UMD allocation wrapper. |
| `dxvk/dxvk_device.cpp` | 987 | Constructs `m_heliosScanoutAcquire(this)` at **39**; `shutdown()` at **82**. | **MODIFY** |
| `dxvk/dxvk_device.h` | 819 | `#include` at 11, accessor 164–165, member 804. Also the record-only mode flag will live here. | **MODIFY** |
| `dxvk/dxvk_image.cpp` | 1175 | Stamps the typed Venus ID onto imports at 634–645. | **MODIFY** |
| `dxvk/dxvk_image.h` | 1172 | `heliosDirectImportAlias / heliosScanoutPrimary / heliosDirectOptimalScanout / heliosCrossContextOptimal / heliosLinearScanoutTarget` (79–102); GDI-staging accessors 889–975. | **MODIFY** |
| `dxvk/dxvk_options.cpp` | 37 | Reads `heliosPresentWaitUs` (22), `heliosStagedProbes` (23), `heliosSkipUnretiredRefresh` (24). | **MODIFY** — delete those three. |
| `dxvk/dxvk_options.h` | 126 | Declares the same three at 55–86. | **MODIFY** |
| `dxvk/dxvk_instance.cpp` | 430 | `DxvkInstance(DxvkInstanceFlags)` at 19–23 delegates to the import ctor at 25. `DxvkInstanceImportInfo` already exists as the injection seam. | **MODIFY** — add the non-forgeable record-only tag; refuse Win32 surface/swapchain extensions on a tagged instance (§13.2 3369–3370). |
| `dxvk/dxvk_instance.h` | 232 | Ctors at 80–90. | **MODIFY** |
| `dxvk/dxvk_queue.cpp` / `.h` | 394 / 254 | The submission thread that drains `DxvkSubmitQueue` and calls into `DxvkCommandList::submit`. | **MODIFY** — a record-only device must have no background submitter (§10.4 1404: "No background worker may submit GPU work after the outer DDI has returned"). |
| `dxvk/dxvk_cmdlist.cpp` | 1161 | `DxvkCommandSubmission::submit` calls **`vkQueueSubmit2` at 96 and 104** (helios_feed-traced arm and plain arm). | **MODIFY** — this is the seal-and-return point. |
| `dxvk/dxvk_cmdlist.h` | 1514 | Command-list/submission structures the sealed batch is built from. | **MODIFY** |
| `dxvk/dxvk_sparse.cpp` | 826 | `vkQueueBindSparse` at **528**. | **MODIFY** — lower to the outer paging model or do not advertise sparse. |
| `dxvk/dxvk_sparse.h` | 1038 | Sparse binder types. | **MODIFY** |
| `dxvk/dxvk_presenter.cpp` | 1366 | `vkQueuePresentKHR` at **212**. | **MODIFY** — unreachable from a record-only instance. |
| `dxvk/dxvk_presenter.h` | 395 | Presenter decls. | **MODIFY** |
| `dxvk/dxvk_fence.h` / `.cpp` | 181 / 286 | **Not in the §17.2 Modify list** but named by Appendix A.1 row 5673 (`dxvk_fence.h:23-43`, `.cpp:6-87`): the `OPAQUE_WIN32` export/import-by-name producer/consumer timeline. | **MODIFY** — manifest gap, see §6.12. |
| `d3d11/d3d11_context.cpp` | 6513 | `#include` at 3; staged-SRV freshness reader 3584–3648 (`HeliosPresentSync::lookup` at 3623, `noteGateFlush` at 3637). | **MODIFY** |
| `d3d11/d3d11_context_imm.cpp` | 1592 | `#include "../dxvk/dxvk_helios_scanout_acquire.h"` at 8; `armFence` at 1219 inside the 1205–1240 prearm block. | **MODIFY** |
| `d3d11/d3d11_texture.cpp` | 1873 | Stamps Venus ID 85–92; **`D3DKMT_ESCAPE_UPDATE_RESOURCE_WINE` at 937, `D3DKMTEscape` at 942**, then `setSharedMetadata` fallback 945–969, guarded by `heliosKmtOnlySharedResources()` at 933. | **MODIFY** |
| `d3d11/d3d11_device.cpp` | 4194 | `heliosKmtOnlySharedResources()` at 45; `\\.\SharedGpuResource` caller at 2629–2637. | **MODIFY** |
| `d3d11/d3d11_on_12.cpp` / `.h` | 157 / 98 | `D3D11on12Device` — `CreateWrappedResource` QIs `ID3D12DXVKInteropDevice`; Acquire/Release/Flush are thin. | **MODIFY** — implement §10.6's same-queue contract (1648–1665). |
| `d3d11/d3d11_main.cpp` | 456 | DLL entry / factory. | **MODIFY** |
| `d3d9/d3d9_common_texture.cpp` | 815 | `D3DKMT_ESCAPE_UPDATE_RESOURCE_WINE` at **667**, `D3DKMTEscape` at **672**, `setSharedMetadata` fallback 675–700. | **MODIFY** — delete; refuse D3D9 sharing if it cannot be expressed through the ordinary WDDM descriptor (§17.2 3882–3886). |
| `d3d9/meson.build` | 84 | Builds the D3D9 target (Escape path is compiled). | **MODIFY** |
| `wsi/win32/wsi_window_win32.cpp` | 364 | Three `D3DKMT_ESCAPE_SET_PRESENT_RECT_WINE` sends at **216/220**, **260/264**, **321/325**. | **MODIFY** — delete all three and the private enum use. |
| `util/util_gdi.h` | 531 | Declares the Wine escape enum at **176–180**, `D3DKMT_ESCAPE` at 195–204, `D3DKMTEscape` at **516**. | **MODIFY** — delete only those three; the keyed-mutex/sync declarations stay. |
| `util/util_gdi.cpp` | 190 | Non-Windows `D3DKMTEscape` stub at **62–65**. | **MODIFY** |
| `util/util_shared_res.h` / `.cpp` | 30 / 66 | Opens `\\.\SharedGpuResource` (`.cpp:14`) and drives OPEN/SET/GET `DeviceIoControl`. | **DELETE** (both files) — §17.2 3878 and A.2 5697 both say delete implementation, declarations, build entry and callers. §17.2's "Modify" verdict for these two is superseded by the prose; see §6.11. |
| `util/meson.build` | 37 | Builds `util_shared_res.cpp` (1–14 region). | **MODIFY** |
| `util/config/config.cpp` | 1824 | The WUDFHost profile override for `dxvk.heliosPresentWaitUs` at 26–35. | **MODIFY** |
| `vulkan/vulkan_loader.cpp` | 108 | `loadVulkanLibrary()` (12–41) `LoadLibraryA("winevulkan.dll"/"vulkan-1.dll")`. The injected-proc ctor `LibraryLoader(PFN_vkGetInstanceProcAddr)` already exists at **48–50**. | **MODIFY** — disable the loader search in Helios UMD builds (§13.2 3365). |
| `vulkan/vulkan_loader.h` | 542 | `LibraryLoader` decls 24–34. | **MODIFY** |

Not in the manifest but touched by the required result — see §6.12:
`d3d11/d3d11_resource.cpp` (`heliosKmtOnlySharedResources` at 18, shared-handle
paths 337–363).

### 2b. D3D11 UMD sub-lane — `umd/**`, `umd_common/**`

| File | Lines | What it does today | Verdict |
|---|---:|---|---|
| `umd/src/scanout_acquire.rs` | 744 | Whole module: global runtime-adapter capture, `pfnEscapeCb` ledger probe/map/event-register, process-global `Mutex<Vec<DeviceEntry>>`, `#[no_mangle] helios_scanout_acquire_enabled` (611) and the by-name lookup/snapshot exports. | **DELETE** (module + `mod scanout_acquire;` at `lib.rs:39`) |
| `umd/src/vehicle_exports.rs` | 84 | `helios_umd_set_present_source` (25), `helios_umd_wait_last_present` (52), `helios_umd_get_present_result` (73). | **DELETE** (module + `mod vehicle_exports;` at `lib.rs:40`) |
| `umd/src/forward/vehicle.rs` | 301 | Per-thread `VEHICLE` slot, `set_source` backing (84), `wait_last_present` backing (134). | **DELETE** the vehicle implementation. Nothing in the new architecture has a "vehicle". |
| `umd/src/forward/snapshot.rs` | 541 | `SnapshotPurpose::{DirectFlip,WindowedBlt}` planning; capability gates call `scanout_acquire::*` at 259–260; `resid`-keyed descriptor at ~384. | **REWRITE/DELETE** — §17.4 4095 says "delete the raw-`resid` snapshot transport". With `read_ledger`/`ScanoutFlushToken` gone (§10.8 2841) the whole snapshot ring loses its reason to exist; see §6.13. |
| `umd/src/forward/present.rs` | 2528 | Present/Present1/MPO/Blt/Blt1/rotate. HPS fold writer 1370–1391; HPS fallback writer 1403–1435; retired-code note 1460–1464; async-stream gate 1479–1528; `RuntimePresentDependencies`/`RuntimeSubmission` 364–448; `pfnRenderCb` submit 774–860; `DXGI_MPO_MAX_PLANES = 16` at **1917**; MPO caps constants and both cap DDIs **2121–2217**; `dxgi_present_mpo` **2219–2367**. | **MODIFY** (large) |
| `umd/src/forward/transfer.rs` | 573 | `check_direct_flip_support_11_1` at **369–382** — unconditional `*supported = 0`. | **MODIFY** |
| `umd/src/forward/tables.rs` | 307 | `install` (72), `install_11_1` (240), `install_wddm1_3` (290); DXGI 1.3 installer 28–40 wires the MPO cap DDIs; `pfnCheckDirectFlipSupport` at **267**. | **MODIFY** — add the WDDM-2.1 installer; keep 267 wired. |
| `umd/src/forward/resource.rs` | 1632 | `pfnAllocateCb` create path 200–585 (private data built at 264–333, pre/post compare 393–421); open-identity parse 1300–1329; 1470–1507. The **adoption direction**: DXVK/Mesa memory is created first and its `resid` handed down. | **REWRITE** of the create/open identity path |
| `umd/src/forward/state.rs` | 1284 | `AllocationOwnership` (40–77); `HeliosWddmAllocPrivate`+`Meta` record (196–197); `read_alloc_meta` (281–311); open-identity parse (336–371). | **MODIFY** (large) |
| `umd/src/forward/alloc.rs` | 117 | `adopt_resource_id()` (87–89) and the venus-backing descriptor it serves. | **DELETE**/REWRITE — nothing adopts any more. |
| `umd/src/forward/state_objects.rs` | 315 | Per-thread vehicle note at 307–315. | **MODIFY** |
| `umd/src/forward.rs` | 569 | Module root; re-exports `HeliosWddmAllocPrivate`, `HeliosWddmOpenIdentity`, `HeliosWddmAllocMeta` at **82–83**; context-null note at 457. | **MODIFY** — the protocol import list changes wholesale. |
| `umd/src/device_funcs.rs` | 1289 | `HeliosDevice` (355–380) incl. `direct_scanout_allocations`; `RuntimeContext` windows (165); `create_runtime_context` (**1014–1065**) — `D3DDDICB_CREATECONTEXT` with `NodeOrdinal=0`, `EngineAffinity=0`, **no private driver data**; teardown 965–1007 (`scanout_acquire::teardown_for_device` at 971). | **MODIFY** (large) — HQA1 PDD + HQC1 creation land here. |
| `umd/src/adapter.rs` | 616 | `SUPPORTED_DDI_VERSIONS` (**33–37**), `NegotiatedInterface` (**52–80**) and the compile-time lockstep assert (**94–99**) that records why WDDM2.1 was retired at T6/R918; `note_runtime_adapter` (233); device-funcs dispatch (460–495); scanout init (505–519). | **MODIFY** (large) — this is the WDDM-2.1+ uplift site. |
| `umd/src/knobs.rs` | 341 | `resolved_inventory() -> [(&str,u32); 10]` (200); `vehicle_flip_gate_us` (280), `scanout_acquire_knob` (289), `scanout_snapshot_knob` (299), `present_batch_fold` (307), `umd_async_present_stream` (314). | **MODIFY** — delete those five and shrink the inventory array. |
| `umd/src/lib.rs` | 111 | `mod scanout_acquire;` (39), `mod vehicle_exports;` (40), knob re-exports (~83). | **MODIFY** |
| `umd/build.rs` | 290 | cxx-build of `bridge/*.cpp`, bindgen over the WDK `d3d10umddi.h`, links the prebuilt DXVK static libs. | **MODIFY** — new bridge sources; possibly a new bindgen header set for the 2.1 device-funcs table. |
| `umd/src/bridge.rs` | 807 | The cxx bridge: `publish_present_order` (184–190 decl, 600–623 wrapper), `set_scanout_acquire_event` (198 decl, 627–630 wrapper), `present_vehicle_copy`, `present_snapshot_copy`, `present_frame_gate`, `rotate_resource_backings`, resource/shader creates. | **MODIFY** (large) |
| `umd/bridge/dxvk_bridge.cpp` | 1729 | `HeliosDxvkDeviceImpl` present-timeline state **266–301**; `publish_present_order` **1130–1360** (named-fence mint with NULL DACL at 1188–1255, signal+publish at 1301–1340); `set_scanout_acquire_event` **1371–1386**; `new DxvkInstance(DxvkInstanceFlags())` at **1654**, `createDevice()` at **1673**. | **MODIFY** (large) |
| `umd/bridge/dxvk_bridge.h` | 220 | `publish_present_order` (153), `set_scanout_acquire_event` (165), decls 143–165. | **MODIFY** |
| `umd/bridge/bridge_icd_exports.cpp` | 550 | `find_helios_icd_in_loaded_modules` module walk; `HeliosIcdExport::MemoryResId → "helios_venus_memory_res_id"` at **320**; present-stream registration 308–329, 534–546. | **MODIFY** (large) — every raw-ID export resolution goes. |
| `umd/bridge/bridge_icd_exports.h` | 47 | Declares `venus_memory_resource_id_from_handle`, `venus_memory_transfer_resource_ownership`, `venus_memory_vidmm_global_identity_from_handle`, `venus_register_present_stream`, etc. | **MODIFY** — the surviving surface is the private direct-dispatch table only. |
| `umd_common/bridge/bridge_icd_anchor.cpp` / `.h` | 243 / 158 | `helios_icd_anchor_v1(void*)` — the process-single canonical venus-ICD module, exported from **both** cdylibs. | **MODIFY** — the anchor must now publish/validate the private direct-dispatch entry point + package generation, not a loaded-module handle. **Shared with the umd12 lane.** |

Positive finding: **the current call order already satisfies §10.4 line 1339–1343.**
`bridge::BridgeDevice::create` runs at `adapter.rs:390`, i.e. *before*
`device_funcs::create_runtime_context` at `adapter.rs:442`. So HTS1 can be
created before `pfnCreateContextCb` without restructuring device creation.

---

## 3. Work decomposition

Units are ordered. Units that share a file are merged or marked serialized.

| Id | Goal | Files owned | Depends on | Size |
|---|---|---|---|---|
| **A0** | Stand up the Linux mingw cross-build as the DXVK inner loop and record the baseline (see §5). | none (build dir only) | — | S |
| **A1** | Delete `dxvk_helios_present_sync.{h,cpp}` + `dxvk_helios_scanout_acquire.{h,cpp}` and their two `meson.build` entries; delete every consumer callsite and its state. | `dxvk/dxvk_helios_present_sync.*`, `dxvk/dxvk_helios_scanout_acquire.*`, `dxvk/meson.build`, `dxvk/dxvk_context.{cpp,h}`, `dxvk/dxvk_memory.{cpp,h}`, `dxvk/dxvk_device.{cpp,h}`, `dxvk/dxvk_fence.{h,cpp}`, `d3d11/d3d11_context.cpp`, `d3d11/d3d11_context_imm.cpp`, `dxvk/dxvk_options.{cpp,h}`, `util/config/config.cpp` | A0 | **XL** |
| **A2** | Delete every live `D3DKMTEscape` call/type and the `\\.\SharedGpuResource` IOCTL registry with all callers. | `d3d11/d3d11_texture.cpp`, `d3d11/d3d11_device.cpp`, `d3d11/d3d11_resource.cpp`, `d3d9/d3d9_common_texture.cpp`, `d3d9/meson.build`, `wsi/win32/wsi_window_win32.cpp`, `util/util_gdi.{h,cpp}`, `util/util_shared_res.{h,cpp}`, `util/meson.build` | A0 | **L** |
| **A3** | Record-only instance/device mode: non-forgeable tag, no Win32 WSI extensions, direct-dispatch proc table instead of the loader search. | `dxvk/dxvk_instance.{cpp,h}`, `vulkan/vulkan_loader.{cpp,h}`, `dxvk/dxvk_device.{cpp,h}` *(serialized after A1)* | A1, **ICD lane** (entry-point name/ABI) | **L** |
| **A4** | Seal-and-return submission: `vkQueueSubmit2` → sealed HOB1 payload + use/operand tables returned to the bridge; no submitter thread on a record-only device. | `dxvk/dxvk_cmdlist.{cpp,h}`, `dxvk/dxvk_queue.{cpp,h}` | A3, **protocol lane** (HOB1 types) | **XL** |
| **A5** | Refuse/lower the remaining queue entry points on a record-only instance: `vkQueueBindSparse`, `vkQueuePresentKHR`, queue idle. Static coverage for every C60 synchronous callsite. | `dxvk/dxvk_sparse.{cpp,h}`, `dxvk/dxvk_presenter.{cpp,h}` | A4 | **L** |
| **A6** | Replace the raw-`resid` import identity with the outer-UMD allocation wrapper; drop the punned-HANDLE reopen. | `dxvk/dxvk_image.{cpp,h}`, `dxvk/dxvk_memory.{cpp,h}` *(serialized after A1)*, `d3d11/d3d11_texture.cpp` *(serialized after A2)* | A1, A2, B4 | **L** |
| **A7** | D3D11On12 on the same queue: seal Acquire/Release barriers + D3D11 commands into the owning vkd3d queue's batch. | `d3d11/d3d11_on_12.{cpp,h}`, `d3d11/d3d11_main.cpp` | A4, **umd12 lane** | **M** |
| **B0** | Delete the three dead modules and every symbol that names them. | `umd/src/scanout_acquire.rs`, `umd/src/vehicle_exports.rs`, `umd/src/forward/vehicle.rs`, `umd/src/lib.rs`, `umd/src/knobs.rs`, `umd/src/adapter.rs` *(233, 505–519 only)*, `umd/src/device_funcs.rs` *(971 only)*, `umd/src/forward/state_objects.rs` | **protocol lane** (escape.rs deletion) | **M** |
| **B1** | Delete the HPS producer half from the bridge and its cxx surface. | `umd/bridge/dxvk_bridge.{cpp,h}`, `umd/src/bridge.rs`, `umd/src/forward/present.rs` *(1370–1435, 1460–1528 only — serialized with B5)* | B0, A1 | **L** |
| **B2** | Delete the raw-ID/present-stream ICD export resolution; rebuild the anchor around the private direct-dispatch table + package generation. | `umd/bridge/bridge_icd_exports.{cpp,h}`, `umd_common/bridge/bridge_icd_anchor.{cpp,h}` | A3, **ICD lane**, **umd12 lane** | **L** |
| **B3** | WDDM-2.1+ table uplift: add `0x000b_0022`, the `Wddm2_1` variant, the fifth device-funcs fill, and audit/implement every newly live slot incl. Acquire/ReleaseResource. Fail device creation on any missing modern callback. | `umd/src/adapter.rs`, `umd/src/forward/tables.rs`, `umd/build.rs`, `umd/src/ddi.rs` | — | **XL** |
| **B4** | HWA2: KMD-created backing + immutable create/open descriptor. Delete adoption in both directions. | `umd/src/forward/resource.rs`, `umd/src/forward/state.rs`, `umd/src/forward/alloc.rs`, `umd/src/forward.rs` | **protocol lane** (HWA2), **KMD lane** | **XL** |
| **B5** | Actual-batch assembly: HOB1 into the Render command window, exact allocation list with per-arm read/write flags, capacity validation, fail-closed counters. Delete the snapshot/marker transport. | `umd/src/forward/present.rs`, `umd/src/forward/snapshot.rs` *(serialized with B1)* | A4, B3, B4 | **XL** |
| **B6** | HQA1 + HQC1: fill the create-context PDD, create the private monitored fence, add the direct-table C60 synchronous-progress operation (flush → FromGpu signal → one FromCpu event wait, no lock held). | `umd/src/device_funcs.rs`, `umd/src/bridge.rs` *(serialized with B1)*, `umd/bridge/dxvk_bridge.{cpp,h}` *(serialized with B1)* | B1, B3, **protocol lane** (HQA1), **KMD lane** | **XL** |
| **B7** | MPO one-primary profile: replace the caps and gate `dxgi_present_mpo`. | `umd/src/forward/present.rs` *(1917, 2121–2367 — serialized with B5)* | B5 | **M** |
| **B8** | `CheckDirectFlipSupport` C43/C44 exact-pair comparison, still returning FALSE until the KMD validator lands in the same package. | `umd/src/forward/transfer.rs`, `umd/src/forward/tables.rs` *(serialized with B3)* | B4, **KMD lane** | **M** |

**Shared-file serialization inside this lane** (these cannot run concurrently):
`umd/src/forward/present.rs` → B1, B5, B7. `umd/src/bridge.rs` and
`umd/bridge/dxvk_bridge.*` → B1, B6. `umd/src/forward/tables.rs` → B3, B8.
`dxvk/dxvk_memory.*` and `dxvk/dxvk_device.*` → A1, A6/A3. `d3d11_texture.cpp`
→ A2, A6.

---

## 4. Shared-file hazards (other lanes)

| File | Other lane | Serialization |
|---|---|---|
| `protocol/src/wddm.rs` | Protocol lane (§17.1 3777–3793) | **Hard break.** `umd/src/forward.rs:82-83` imports `HeliosWddmAllocPrivate`, `HeliosWddmOpenIdentity`, `HeliosWddmAllocMeta`. The moment the protocol lane replaces them, `umd` stops compiling. Protocol lane must land HWA2/HOB1/HOS1 **first**; B4/B5 consume them. Do not shim. |
| `protocol/src/escape.rs` | Protocol lane (deleted) | `umd/src/scanout_acquire.rs` is this lane's only remaining consumer. B0 must land in the same change as the protocol deletion, or before it. |
| `protocol/src/translation_session.rs` (new) | Protocol lane (§17.1 3750–3757) | B6 consumes HQA1 from here. Blocking. |
| `umd_common/bridge/bridge_icd_anchor.{cpp,h}` | **umd12 lane** — §17.4 names it here, §17.5 will also touch it. Exported from both cdylibs by design. | Single owner required. Recommend this lane owns the file and the umd12 lane files a cross-lane request against it. |
| `umd_common/src/{knobs,refusals,noop,slot,log}.rs` | umd12 lane | `knobs.rs`/`refusals.rs` are shared counter infrastructure; deleting a UMD11 knob must not break umd12's inventory. Coordinate the shared arity. |
| `umd/bridge/bridge_icd_exports.{cpp,h}` | **ICD/Mesa lane** deletes `helios_venus_memory_res_id` (A.2 5705) and the present-stream registration (A.4 5771). | This lane deletes the *resolver*; the ICD lane deletes the *export*. Either order works only if both land together; they are in one package, so land in one changeset. |
| `dxvk-helios/src/dxvk/dxvk_instance.cpp`, `vulkan/vulkan_loader.cpp` | ICD lane supplies the private direct-dispatch entry point; **umd12 lane** (§13.2 3366: "vkd3d's instance creation receives the same private direct-ICD proc table") must use the identical mechanism. | Define the entry-point name/signature/generation handshake **once**, in the ICD lane, and have A3 + umd12 consume it. |
| `packaging/windows/Install-Helios.ps1:198-217` | Packaging lane | Creates the HPS2 ProgramData file + ACL. Deleting the DXVK mapper without deleting the installer step leaves dead global state. |
| `tools/read_ledger_dump.c`, `tools/window_burst_capture.ps1:246-296`, `CONFORMANCE.md:245` | Tools/docs lane (A.1/A.2 5728) | Consume the ledger this lane deletes. |
| `dxvk-helios/src/d3d9/**`, `src/dxgi/**`, `src/d3d8/**` | No other lane, but the meson tree still builds standalone `d3d9.dll`/`d3d8.dll`/`dxgi.dll`/`d3d11.dll` that use `dxvk_presenter.cpp`. | See §6.5 — do not compile the presenter out; gate at runtime on the record-only tag. |

---

## 5. Build and verification

### ⭐ DXVK cross-compiles on the Linux host — verified today

`meson`, `ninja`, `glslang`, and `x86_64-w64-mingw32-g++` are all installed.
Both commands were run for this brief and **succeeded**:

```
cd /home/rupansh/helios-vgpu/dxvk-helios
meson setup --cross-file build-win64.txt --buildtype release <builddir>
ninja -C <builddir>            # 357/357 targets, zero errors
```

The full build includes `src/dxvk/libdxvk.a`, `src/d3d11/libhelios_d3d11_static.a`
(the exact static library `umd/build.rs` links), `d3d11.dll`, `d3d9.dll`,
`d3d8.dll`, `dxgi.dll`, and both Helios files this lane deletes
(`dxvk_helios_present_sync.cpp.obj`, `dxvk_helios_scanout_acquire.cpp.obj`).
Single-object rebuilds work: `ninja src/dxvk/libdxvk.a.p/<file>.cpp.obj`.

**Caveats, stated so nobody over-trusts it:**
- mingw-w64 GCC is *not* the shipping toolchain. The deployed UMD is built with
  clang-cl + `-Db_vscrt=mt` (MSVC ABI, static CRT) via `win_dxvk`; `umd/build.rs`
  documents that the DXVK libs, the cxx shim and the Rust crate must all share
  the MSVC ABI. The mingw arm is a **compile/typecheck oracle**, not an artifact.
- `util_gdi.cpp` compiles the *non-Windows* stubs under mingw (`D3DKMTEscape:
  Not available on this platform`), so the mingw arm does **not** exercise the
  real gdi32 Escape import. The zero-Escape gate (§18.1 4669–4676) must be run
  against the clang-cl artifact.
- MSVC-only constructs and `/MT` link behaviour are invisible here.
- Use it for A1/A2/A4/A5/A6 iteration; confirm every unit on the VM before it is
  called done.

### `umd_common` and `protocol` build on Linux

```
CARGO_TARGET_DIR=target/linux cargo check --manifest-path umd_common/Cargo.toml   # PASSES
CARGO_TARGET_DIR=target/linux cargo check --manifest-path protocol/Cargo.toml
```
`umd_common/Cargo.toml` states the Linux-buildability requirement explicitly and
forbids a build script or an unconditional `windows` dependency there.

### `umd` is Windows-only, with no host-side check

`umd/src/lib.rs:24-29` is a hard `compile_error!` on non-Windows targets;
`umd/build.rs` bindgens the WDK `d3d10umddi.h` and links prebuilt DXVK static
libs from a local `C:` tree. There is **no `umd11` analogue of
`tools/umd12-host-check.sh`**. The inner loop is:

- `tools/umd-check.ps1 -Mode check -Crate umd` on the VM (filters ~115 clang
  warnings that otherwise blow the MCP output cap; full log at
  `Z:\tmp\umd-check.log`), driven through `win_cargo` / `win_exec`.
- `win_dxvk` for the engine, then `win_cargo` for the UMD (order matters —
  memory 27TH).
- `win_install_umd` to deploy.

### Only verifiable on the VM
- Every §18.2 gate (4783–4974).
- The WDDM-2.1 device-funcs table shape, `D3DWDDM2_1_DDI_SUPPORTED`'s build
  number, `D3DWDDM2_1DDI_DEVICEFUNCS` slot count, and whether
  `pfnCreateSynchronizationObject2Cb` / FromGpu / FromCpu are non-null under the
  negotiated table. The bindgen output lives only in `$OUT_DIR` on Windows.
- Any `D3DDDICB_CREATECONTEXT` field-offset claim.
- The zero-Escape import scan on the shipped binaries.

---

## 6. Blockers, ambiguities, and contradictions

**6.1 — §17.4 says the UMD "requests 4096 runtime allocation/output-patch
entries"; the DDI has no such request.** Reference 4147–4148:
"the UMD requests 4096 runtime allocation/output-patch entries and splits only
between complete operations." In the WDDM DDI, `D3DDDICB_CREATECONTEXT`'s
`AllocationListSize` and `PatchLocationListSize` are `[out]` — `device_funcs.rs:1029-1060`
reads them as outputs of `pfnCreateContextCb`, and they originate in the KMD's
`DxgkDdiCreateContext`. A UMD cannot request them.
*Conservative fail-closed reading to implement:* the KMD reports ≥4096 in both;
the UMD validates `capacity >= required` per batch (extending the existing check
at `present.rs:396-407`) and returns `E_FAIL` with a named counter on shortfall;
device creation fails if the reported capacities are below the package minimum.
**Cross-lane request to the KMD lane** — see §7.

**6.2 — the 15 MiB HOB1 ceiling versus the KMD-chosen D3D11 command window.**
Reference 1268: total bytes "at most 15 MiB, **and no larger than the current
runtime-approved command buffer**"; 1310–1311: "if that inequality fails, the
associated cap is not exposed." For D3D11 the command window is
`arg.CommandBufferSize` from CreateContext, again KMD-chosen — and D3D11 exposes
no cap that bounds a single draw's allocation count, so "the associated cap is
not exposed" has no D3D11 referent. *Conservative reading:* fail device creation
when `CommandBufferSize < 15 MiB` or `AllocationListSize < 4096`, rather than
silently splitting an indivisible operation (which 1312–1314 forbids).

**6.3 — the HOB1 `D3D11_PHYSICAL=1` flag name versus the actual D3D11 context.**
Reference 1267 requires "exactly one of `D3D11_PHYSICAL=1`, `D3D12_VIRTUAL=2`",
but 1410–1412 puts D3D11 on "its normal Render command window/allocation list",
and the KMD declares `Wddm2_1GpuMmu` (`kmd_render/src/ddi/wddm_surface.rs`,
CLAUDE.md line 19–21) — a GpuMmu adapter, not a physical-addressing one.
Reference 1287 resolves it: "D3D11 KMD converts type 1 to DMA-local physical
capabilities during Render/Patch." *Reading:* the flag names the **operand
identity kind** (allocation-list index, `identityKind=1`), not the context's
addressing model. Implement as such; do not infer a physical-addressing context.

**6.4 — "The D3D11 generator" has no referent in the tree.** Reference 4143–4149
assigns the transitive allocation closure and a "generated compile-time
inequality" to a "D3D11 generator". No such component exists in `umd/`. §10.4
clause 3 (1388–1391) places the "complete resource-use table" in the record-only
*translator's* seal, i.e. Mesa/DXVK. *Conservative reading:* the use/operand
tables are produced by the record-only backend and returned through the bridge;
the UMD is a **validator** that maps them onto the runtime allocation list and
fails closed above 4096/8192; the compile-time bound assertions live in
`protocol/` beside the HOB1 limits. This crosses three lanes — see §7.

**6.5 — §17.2's "private D3D builds cannot reach `vkQueuePresentKHR`" versus the
shared meson tree.** `dxvk_presenter.cpp:212` is reached by the standalone
`d3d9.dll` / `d3d8.dll` / `dxgi.dll` / `d3d11.dll` targets, which the same
`meson.build` still produces (all 357 targets build today). Compiling the
presenter out breaks them. *Conservative reading:* enforce at runtime on the
non-forgeable record-only instance tag (§13.2 3369) and return a hard failure —
never a silent no-op — leaving the standalone targets intact. If the standalone
DXVK DLLs are meant to be dropped from the package entirely, that is a decision
the reference does not state.

**6.6 — the UMD's `OverlayCaps` value is under-specified.** §10.8 2726–2727 gives
the **KMD** profile `OverlayCaps.Value=0`; 2728–2730 says the D3D11/DXGI UMD
"returns the same one-plane truth instead of its current 16-plane, 16x
stretch/shrink, RGB/BILINEAR/SHARED/IMMEDIATE advertisement". It does not say
whether the UMD's `DXGI_DDI_MULTIPLANE_OVERLAY_GROUP_CAPS::OverlayCaps` becomes
`0` or `RGB` alone. *Conservative fail-closed reading:* `0`, matching the KMD
literally. `MaxPlanes = NumPlanes = 1`, `MaxStretchFactor = MaxShrinkFactor = 1.0`,
`NumCapabilityGroups = 1`, `StereoCaps = 0`. Note this is behaviour-affecting —
`present.rs:2139-2144` already records that DWM picks its composition strategy
from these numbers and that the reduction was deferred pending same-boot evidence
— and that CLAUDE.md operating rule 8 requires the evidence to sit at the read
site.

**6.7 — `dxgi_present_mpo` admission has no UMD-side extent test.** §17.4 4117
says "accept/forward only that profile"; §10.8 2768–2777 states the equality
requirements ("the full output extent", equal src/dst/clip) for the **KMD's**
CheckMPO3. The UMD does not know the output extent. *Conservative reading:* the
UMD refuses `PresentPlaneCount > 1` and any enabled plane with non-identity
rotation, any blend, any stretch quality, or unequal `SrcRect`/`DstRect`/`ClipRect`,
returning `DXGI_ERROR_UNSUPPORTED` with a named counter; the extent comparison
stays KMD-side.

**6.8 — the WDDM-2.1 uplift is not derivable on Linux and the tree records why it
was removed.** `umd/src/adapter.rs:87-93` states that `0x000b_0022`
(`D3DWDDM2_1_DDI_INTERFACE_VERSION`) is above the advertised maximum, so the
fifth device-funcs fill was unreachable and was deleted at T6/R918, and that
making it live "means ADDING a version above — a behaviour change (DWM would
negotiate a 170-slot table and the AcquireResource/ReleaseResource DDIs would
start being called) that needs its own validation". That is exactly what §10.5
1433–1442 and §17.4 4128–4141 now require. The blockers: the exact
`D3DWDDM2_1_DDI_SUPPORTED` build word, the `D3DWDDM2_1DDI_DEVICEFUNCS` slot
inventory, and whether negotiating that funcs table actually changes which
`D3DDDI_DEVICECALLBACKS` slots are non-null. **All three require the WDK 28000
headers on the VM.** The reference itself treats the last as unproven — 1440–1442
says the table, callback size/caps, physical-context creation, Render, FromGpu
signal and FromCpu wait "must all be observed together in the build-28000 gate".
Implement fail-closed: any missing modern callback fails `CreateDevice`.

**6.9 — `CheckDirectFlipSupport` must land dark.** §17.4 4124–4126: "this callback
stays false until the KMD's classic SetVidPn validator/lease path is in the same
package", while §10.8 2736–2753 requires it as the *earlier* of two gates. Not a
contradiction — both are in one package — but it is a hard intra-package
serialization. The implementer must write the full C43/C44 comparison and keep
the result forced to FALSE until the KMD unit lands, and must not "fix the
refusal" as a standalone improvement.

**6.10 — `util_shared_res.{h,cpp}` are listed under "Modify" but the prose says
delete.** §17.2 3839 lists them under *Modify*; §17.2 3878 and Appendix A.2 5697
both say "Delete implementation, declarations, build entry, and callers; never
replace it with another IOCTL, file, or service." *Conservative reading:* delete
both files. The Modify placement is a manifest slip.

**6.11 — `util_gdi.h`/`util_gdi.cpp` cannot be deleted wholesale.** They also
declare the keyed-mutex and synchronization-object D3DKMT entry points DXVK uses
outside the Escape path. Delete only: the `D3DKMT_ESCAPETYPE` enum (176–180), the
`D3DKMT_ESCAPE` struct (195–204), the `D3DKMTEscape` declaration (516) and the
`.cpp:62-65` stub.

**6.12 — three live owners are missing from the §17.2 manifest.**
(a) `dxvk/dxvk_fence.{h,cpp}` — Appendix A.1 row 5673 names `dxvk_fence.h:23-43`,
`.cpp:6-87` as the producer/consumer named-timeline (`OPAQUE_WIN32` export/import
by name, security descriptor, "name branch skips ordinary KMT-handle
bookkeeping"), but §17.2's Modify list omits it. It is a direct HPS2 participant.
(b) `d3d11/d3d11_resource.cpp` — hosts `heliosKmtOnlySharedResources()` (line 18)
and the shared-handle paths at 337–363 that gate the Escape at
`d3d11_texture.cpp:933`; A.2 5697 names `d3d11_device.cpp:2629-2637` but not this
file. (c) `umd/src/forward/views.rs` (1000 lines) owns the six-stage
descriptor/resource bindings that §17.4 4143–4144 requires in the transitive
closure, and is absent from §17.4. §17's preamble (3745–3746) permits additions
— "may not omit a listed live owner" — so adding these is legal, but the
implementer must not treat the manifest as exhaustive.

**6.13 — the snapshot ring's disposition is stated only by implication.** §17.4
4085 says `umd/src/forward/snapshot.rs` deletes "the raw-`resid` snapshot
transport", implying the module survives. But §10.8 2841–2842 deletes "the
former `read_ledger`, Escape mapping, 65-slot page, event registration, 10 ms
signaler, DXVK scans, **and snapshot fallback**", and A.3 5710 says "Remove the
custom snapshot/direct-read routes." `snapshot.rs:259-260` gates both purposes on
`scanout_acquire::*`, which is deleted, so nothing can enable either purpose.
*Conservative reading:* delete the whole module and its `SnapshotPurpose` seam in
`present.rs`; retain nothing that could be re-enabled by a knob.

**6.14 — the private direct-dispatch entry point is unnamed.** §2 item 8 (260–271)
and §13.2 3365–3368 require DXVK and vkd3d to reach the Helios ICD through "a
private direct-dispatch entry point rather than the Vulkan loader", with every
proc resolved from that table and any loader/WSI-layer-owned pointer rejected.
Neither its symbol name, its signature, nor its version-handshake shape is
specified anywhere in the reference. `DxvkInstanceImportInfo` +
`LibraryLoader(PFN_vkGetInstanceProcAddr)` (`vulkan_loader.cpp:48-50`) are the
existing seam, and `helios_icd_anchor_v1` is the existing process-single
publisher — but the anchor today publishes an `HMODULE`, not a proc table.
**Blocker for A3/B2** until the ICD lane defines it.

**6.15 — Appendix A line anchors are accurate but symbol-anchored edits are still
required.** Every anchor spot-checked for this brief matched exactly
(`present.rs:1370-1391`, `1403-1435`, `2125-2217`; `transfer.rs:369-382`;
`tables.rs:267`; `dxvk_cmdlist.cpp:96-104`; `dxvk_sparse.cpp:528`;
`dxvk_presenter.cpp:212`). They will drift the moment the first unit lands. Every
subsequent unit must re-locate by symbol, not by line.

**6.16 — the knob inventory has a fixed arity.** `umd/src/knobs.rs:200` returns
`[(&'static str, u32); 10]` and `log_knob_inventory` runs at adapter open.
Deleting five knobs (A.4 5770: "Remove vehicle/read-ledger/snapshot/present-stream
gates and their counters with the mechanisms") changes that arity and the
`lib.rs` re-export list. Mechanical, but it will break the build in a file no
unit lists if it is forgotten.

**6.17 — "Never delete the old mapped file while any legacy process can still map
it" (§3 line 395) is satisfied by atomicity, not by code in this lane.** DXVK
creates `C:\ProgramData\Helios\helios_present_sync_v2.bin`
(`dxvk_helios_present_sync.cpp:23`); the installer creates it with a
cross-principal ACL (`Install-Helios.ps1:198-217`, A.1 row 5687). §2 362–366
forbids any old/new interoperation, so no legacy process survives the swap. This
lane must therefore retain **no** compatibility parser, mapper, or reader — the
constraint is discharged by the one-shot deployment, and reading it as licence to
keep a fallback would invert it.

---

## 7. Cross-lane requests

1. **KMD lane** — `DxgkDdiCreateContext` must report `AllocationListSize ≥ 4096`,
   `PatchLocationListSize ≥ 4096`, and `CommandBufferSize ≥ 15 MiB` for a D3D11
   HQA1 context, and must accept the 72-byte HQA1 as create-context private data.
   Without this, §6.1/§6.2 have no implementable form.
2. **Protocol lane** — land HWA2 (168 B), HQA1 (72 B), HOB1 (112 B header + 40 B
   use + 16 B operand) and the 4096/8192/15 MiB bound constants, with the
   compile-time inequality of §6.4, **before** B4/B5/B6.
3. **ICD/Mesa lane** — define the private direct-dispatch entry point (symbol,
   signature, package-generation handshake) and the record-only submission-mode
   token `HELIOS_TRANSLATOR_SUBMISSION_MODE_RECORD_ONLY` (§10.4 1378–1379). Also
   confirm which side produces the HOB1 use/operand tables (§6.4).
4. **umd12 lane** — agree a single owner for
   `umd_common/bridge/bridge_icd_anchor.{cpp,h}` and use the identical
   direct-dispatch mechanism (§13.2 3366).
5. **Packaging lane** — remove `Install-Helios.ps1:198-217` in the same changeset
   that deletes the DXVK mapper.
