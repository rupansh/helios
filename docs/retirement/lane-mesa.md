# Lane: Mesa/ICD — record-only submit, direct dispatch, HTS1 session, native KMT lane, and the new WSI layer

⛔ **Doc line numbers here were swept +10 on 2026-08-10 and mechanically
verified.** `docs/HELIOS_PRESENT_SYNC_RETIREMENT.md` is **5976 lines** (`wc -l`)
— 5918 when this brief was written, 5928 after commit `c17f17c` inserted a
ten-line banner at `:11-20`, and 5973 after `afebe66` **appended** the SUPERSEDED
CLAIMS INDEX at `:5932`; later append-only index corrections raised the total to
5976 without moving a body line. Only the banner moved anything: it shifted every body
line after 8 by +10, so every citation written here before 2026-08-10 was 10 too
low. All of them have now been shifted, and the sweep was checked by requiring
each `§N.M`-anchored cite to land inside §N.M's own heading-to-heading range —
not by adding 10 on faith. ⚠ **Source-file line numbers were deliberately NOT touched** — `foo.rs:123`, `virtio-gpu-virgl.c` function ranges and the like are cites into the tree, not into the doc, and the banner never moved them. If a number here is not a doc line, it was correct before this sweep and is correct now. ⚠ The one row already corrected on 2026-08-10 (the
`HWA2 (168 B)` wire-record row in §2) was **not** shifted again. Re-grep the
cited text before relying on any number below, and read the SUPERSEDED CLAIMS
INDEX at `:5932` before treating any doc line as a requirement.

**2026-08-13 K2a dependency checkpoint.** The CPU-view dependency beneath A1/A3
is now landed and target-exercised through WDDM
`ShareBackingStoreWithKmd`/`DxgkDdiSetAllocationBackingStore`, not F17's
physical-memory-object mapping. The A1 reply pool is 4 MiB split into four 1 MiB
slots; HVR1 payload per slot is 1,048,496 bytes and 64 MiB logical snapshots use
continuation chunks. The current two Mesa files contain that geometry update and
still build cleanly. A3/A4 remain future consumers: they must retain the exact
creating process's ordinary Lock2 mapping and exact allocation lifetime, never
discover a mapping by pointer/PID/name/token or restore Escape. Larger future
role-2/role-3 allocations also need an explicit policy for QEMU udmabuf's stock
1024-range limit; K2a proves 4 MiB allocations, not arbitrary sizes. See
`FINDINGS.md` F18.

**2026-08-19 post-K9 checkpoint and owner authorization.** K11 is target-
exercised; K9 is landed and source/build validated but not deployed. The A3/A4
pre-edit reconciliation in `FINDINGS.md` F20 found that current K6/K11 still
executes only finite INIT: general HNR2 work stops at `NR2_NO_STAGE`,
`NR2_NO_RESID`, `NR2_NO_SCHEMA`, and `NR2_NO_HOST`, and role-4 HVM1 stops at
`AcHvm1Mem`. The owner authorizes the bounded KMD executor/bootstrap and
truthful role-4 continuation defined by F20 before A3/A4. This is not authority
for A5-A9, the present layer, a new host protocol, or a fallback. No A3/A4 Mesa
source has changed at this checkpoint.

**2026-08-19 A3/A4 implementation checkpoint.** The prerequisite landed at
root `e8819c1`; A3 landed in Mesa `2c2763b8b1a` and A4 in `5cbc0254f43`.
The selected Windows backend is now `vn_renderer_helios_hvm.c`: it has no
Escape/IOCTL/private-blob/present-stream/named-fence/raw-resource-ID path,
owns one exact A1/K11 session per `vn_instance`, retains K2a Lock2 views only
for roles 1–3, leaves role 4 unmapped, and implements exact C57/HNF1 imports.
The A4 owner dispatches `QueueSubmit`, `QueueSubmit2`, and `QueueBindSparse` by
per-instance mode. Record-only mode requires a live exact scope and returns a
bounded immutable batch without KMT submission or completion; normal mode uses
the exact nonzero HVC1 context, zero wire host-resource operands, KMD-private
patching, same-context imported-fence ordering, and C51 joins. Command-buffer
closure remains a named refusal pending A7; sparse closure is exact.

A3 and A8 were not codependent. The Windows lifecycle skips duplicate host
instance/ring creation and reserves only the dedicated bootstrap endpoint;
generic `vn_ring` retirement remains A8. A5-A9 and sub-lane (b) were not
started. Source/mutation gates and the Windows ICD build pass, but neither the
new KMD nor ICD was installed or target-exercised, so no display-admission,
HPS2-retirement, or production-correctness claim follows. This checkpoint
overrides the pre-implementation inventory below where it says A3/A4 files are
absent.

**2026-08-20 A5/A6 implementation and A7 stop checkpoint.** A5 landed at
`4ea18b3512f`: the sole versioned export returns a package-generation-checked,
direct-owned record-only instance and its fixed 112-byte/eleven-slot dispatch
table. It uses the existing instance/session/endpoint/context/scope graph and
rejects loader/layer procedure provenance. A6 landed fail-closed at
`d07d1d13687`, followed by review fix `478c71a0fff` that makes the handle-
properties query reject every non-image HWA2 class the import rejects: Windows
lower-ICD WSI advertisement and initialization are gone;
the guest profile has exactly two memory types over one HLM1 heap, exact memory-
index/requirements remapping, bounded normal closure, no sparse/second queue,
and strict D3D12 resource/fence imports. The import capability stays
unadvertised until A7 is complete.

A7 cannot be completed within this lower-ICD-only authority. Its sealed use
requires the nonzero UMD-assigned `outer_allocation_token`, but the protocol
places token ingress on the DXVK/vkd3d plus UMD resource-creation path and no
current consumer implements that integration. The fixed A5 table has no
allocation-registration slot. A4 therefore continues to refuse command buffers
in `helios_submit1_deferred_use_gate` /
`helios_submit2_deferred_use_gate`, and record-only `helios_dispatch_payload`
refuses any nonempty allocation closure. Mesa's local HVM1 handle/generation is
not the outer token and was not substituted. A7 would require either the out-of-
scope consumer/UMD cutover or a forbidden new carrier, so dependency order left
A8/A9 unstarted. No KMD/schema change, present-layer work, deployment, or target
exercise occurred. See `FINDINGS.md` F21.

Reconnaissance brief. No implementation code was written. Every line/symbol
reference below was re-verified against the working tree at
`icd/mesa` commit `8559b66299a8f91fcde30edfdd23310195cc7ca6` — the same commit
the retirement doc records in its §1 provenance table, so the doc's citations
into this repository are trustworthy and are reproduced as-is.

Scope of this lane: `icd/mesa/src/vulkan/wsi/**` and
`icd/mesa/src/virtio/vulkan/**`. Nothing else. Two sub-lanes:

- **(a) lower ICD** — the `vn_*` Venus driver: record-only submit, private
  direct dispatch, HTS1 session, HVC1/HNR2 native KMT lane, HVM1 memory, C57
  import carrier, ring retirement.
- **(b) `VK_LAYER_HELIOS_present`** — the new Vulkan WSI layer above the ICD.

They share exactly two files (`src/vulkan/wsi/meson.build` and the C header that
declares the ICD's private presentable-image tag call) and are otherwise
independently implementable.

---

## 1. Normative sources

| Section | Doc lines | What it contributes to this lane |
|---|---|---|
| §2 executive decision | 122–368 | *Why*: items 1, 6, 7, 8, 9 are this lane. Item 6 is the whole native-KMT carrier; item 7 is the whole WSI layer; item 8 is the acyclicity rule that forbids any ICD→DXGI edge; item 9 is HTS1 pre-queue control. The responsibility table (302–317) names the replacement owner for every retired mechanism. |
| §3 hard constraints | 369–406 | The bounding non-goals: no Escape, no global registry/polling/PID keys, **no ICD call to DXGI/D3D11/D3D12**, no CPU wait on GPU completion in steady state, no cross-adapter inference, no version/feature fallback. |
| §10.3 resource identity | 1014–1189 | The `HeliosWddmAllocationDescV2` (HWA2, 168 bytes) create-time descriptor this lane must *read* on import; the "Native Vulkan WSI" subsection (1121–1188) is the exact canonical import chain: `VkExternalMemoryImageCreateInfo{D3D12_RESOURCE_BIT}` → `vkGetMemoryWin32HandlePropertiesKHR` → `VkImportMemoryWin32HandleInfoKHR` + `VkMemoryDedicatedAllocateInfo` → C57 `D3DKMTQueryResourceInfoFromNtHandle` + `D3DKMTOpenResourceFromNtHandle`; plus the Ready/Release `D3D12_FENCE_BIT` timeline-semaphore import chain (1180–1188). |
| §10.4 record-only translation | 1190–1430 | HTS1 session creation per `vn_instance`; the 72-byte HQA1 create-context packet (the ICD *produces* the endpoint descriptor, the outer UMD sends the packet); the C60 three-way control classification (pure control / outer-allocation-backed / GPU-dependent, 1345–1376); the five-point record-only submit contract (1378–1406); `HELIOS_TRANSLATOR_SUBMISSION_MODE_RECORD_ONLY`; the rejection rule for any queue entry point without a live outer scope (1397–1406). |
| §10.7 native KMT + WSI layer | 1685–2710 | The single largest source. Sub-parts: normal-ICD KMT carrier (1687–1758, incl. the 32-byte HVC1 table at 1708–1717 and the legacy `DXGK_CONTEXTINFO` minima at 1735–1750); HNR2 (1759–1850, header table 1765–1787, 24-byte use / 16-byte patch records 1789–1793, fragmentation arithmetic 1820–1829); Patch/SubmitCommand expectations (1866–1906); the one unshared monitored progress fence per context (1908–1934); HVM1 (1936–1972, table 1949–1962); segment/HLM1/HPM1 admission (1974–2021); reply pool + Lock2 rules (2037–2060); the two exposed memory types (2062–2078); HVR1 reply/snapshot storage (2135–2178); teardown (2180–2185). Then the WSI layer: virtual surface ownership (2206–2233), extension discovery/dispatch closure (2235–2272), `vkCreateDevice` interception + helper-queue construction (2274–2311), singleton device-group + swapchain-memory alias contract (2313–2445), the normative copy-only surface profile table (2447–2487), D3D12 object creation + canonical import + C57 carrier (2489–2561), fence import (2563–2579), presentable-image tagging (2581–2588), sharing mode / helper queue (2590–2606), Acquire (2608–2643), Present (2645–2709). |
| §11.3 native Vulkan sequence | 2980–3025 | The end-to-end mermaid ordering for allocation → paging → Lock2 → HNR2 BEGIN/COMMIT → Patch → SUBMIT_3D → reply decode. Use it as the acceptance ordering for sub-lane (a). |
| §11.4 native Vulkan WSI sequence | 3026–3050 | The Acquire/Present arrow order for sub-lane (b). |
| §12.2 native-WSI ICD-side fence contract | 3197–3249 | Exact `D3DKMT_OPENNATIVEFENCEFROMNTHANDLE` field rules (`EngineAffinity=1u<<0`, `Shared=1`+`NtSecuritySharing=1` only, 64-byte HNF1 private data, acceptance conditions), and the wait/signal placement rule: `D3DKMTWaitForSynchronizationObjectFromGpu` before the acquire barrier, `D3DKMTSignalSynchronizationObjectFromGpu` (**not** `FromGpu2`) after the release barrier, both on the submitting ICD context. |
| §13 no-recursion proof | 3338–3416 | §13.1 allowed call graph, §13.2 the enforced-dispatch bullet list (incl. the static import-table test that must reject `dxgi.dll`/`d3d11.dll`/`d3d12.dll` from the ICD DLL), §13.3 the lock/reentrancy rules that constrain every mutex this lane adds. |
| §17.3 Mesa/ICD manifest | 3888–4083 | The file manifest and the "Required result" prose. This is the checklist. |
| §18.3 WSI gates | 4975–5076 | Acceptance criteria for sub-lane (b) (and the two fence bullets at 5028–5034 for sub-lane (a)). |
| Appendix A.1/A.2/A.3/A.4 | 5650–5688, 5689–5744, 5745–5762, 5763–5788 | Current-state dependency inventory. The rows that name this lane are A.1 (`wsi_helios_present_sync.*`, `wsi_common.c:2647-2665`, `wsi_common_private.h:288-299`, `wsi_common_win32.cpp:998-1054,1591-1609,2100-2147`, `meson.build:30-31`, `vn_queue.c:4356-4425`+`vn_renderer.h:245-308`+`vn_renderer_helios.c:4708-4865`), A.2 (`vn_renderer_helios.c:3582-3949`, `:708-760`, `vn_wsi.c:133-162,239-242`, `wsi_common.c:2996-3080`+`vn_image.c:559-622,717-760`, `wsi_common_win32.cpp:229-266,268-352,850-887,1940-2057,2233-2255`, `vn_queue.c:1688-1795,3540-3546`, `vn_renderer_helios.c:1520-1611,2069-2209`), A.3 (`vn_renderer_helios.c:1399-1464,1663-1689,1780-2001,2271-2309,2482-2551` and `:3991,4163,4200,4243-4266,4314-4496,4630,5040-5042,5171-5184`), A.4 (`vn_renderer_helios.c:95-106,531-538,1333-1397,5076-5079,5235`, `vn_renderer.h:245-308,11-29,83-104,175-179`, `vn_renderer_util.h:21-35`, `vn_ring.c:298-344,701-730,887-958`, `vn_instance.c:177-184,309-342`, `vn_device.c:83-104,573-582,721-734`, `vn_buffer.c:262-300`, `vn_image.c:386-407`, `vn_pipeline.c:685-687,1801-1817`, `vn_query_pool.c:201-260`, `vn_queue.c:1494-1504,2796-2879`, `vn_renderer_helios.c:2005-2023,2120-2145`). |
| §17.7 (read-only, other lane) | 4506–4618 | Only for the cross-lane facts: packaging installs "the matching layer DLL/JSON" (4615–4618); QEMU keeps one `VkInstance` per HTS1 session and ring-0 semantics (4594–4613). |

---

## 2. Current source inventory

All line counts are `wc -l` on the working tree today. **Every file the manifest
names exists at the stated path.** Nothing in §17.3's Modify/Delete list is
misplaced. The four "Add" files under `src/virtio/vulkan/` and the four under
`src/vulkan/wsi/` do not exist yet, as expected.

### 2a. `icd/mesa/src/vulkan/wsi/` (sub-lane b + shared)

| File | Lines | What it does today | Verdict |
|---|---:|---|---|
| `wsi_helios_present_sync.h` | 41 | HPS2 wire ABI mirror (`resid → pid/start/fenceId/value/kwait`), byte-compatible with `dxvk_helios_present_sync.h`. | **DELETE** |
| `wsi_helios_present_sync.c` | 351 | HPS2 mapper/publisher/releaser for WSI images: id/mapping/generation (`:57-163`), publish/release (`:247-349`). No reader. | **DELETE** |
| `meson.build` | 94 | Builds `libvulkan_wsi` (static, `gnu_symbol_visibility:'hidden'`, `build_by_default:false`) which is `link_whole`'d into `vulkan_virtio.dll` via `idep_vulkan_wsi`. Lines 29–35 add `wsi_common_win32.cpp` + `wsi_helios_present_sync.c` + `dep_dxheaders` on Windows. | **MODIFY** — drop line 31 (`wsi_helios_present_sync.c`); drop `dep_dxheaders` from `platform_deps` once the vehicle is gone; **add a new `shared_library()` target for the layer plus a `configure_file()` for `VkLayer_HELIOS_present.json`**. The layer target must *not* be part of `libvulkan_wsi` (see §4). |
| `wsi_common.c` | 3786 | Upstream WSI core, **151 Helios-modified lines**: perf counters (`:55-130`), insurance-blit knob (`:144`), async software-present worker (`:194-336`, `helios_async` queue), vehicle-serving gate (`:224-225`), and the vehicle producer timeline signal (`:2647-2665`). Upstream singleton device-group returns at `:2996-3015` and `wsi_common_create_swapchain_image` at `:3017-3074`. | **MODIFY** — delete every `helios_*` addition (perf, insurance blit, async worker, `helios_present_order` signal, `helios_vehicle_serving`). `:2996-3015` and `:3017-3074` are the *precedent* the layer re-implements; leave upstream behavior for non-Windows builds untouched. |
| `wsi_common.h` | 369 | 3 Helios lines: `wsi_device.win32.get_helios_resource_identity` fn ptr (`:152-157`) and the vehicle's named-release timeline import (`:265`). | **MODIFY** — delete both. |
| `wsi_common_private.h` | 697 | 20 Helios lines: `wsi_image_info::helios_image_export_handle_types` (`:122-127`), `wsi_image::helios_present_value` (`:186-189`), `wsi_helios_present_job`/`wsi_helios_async_present_enabled`/`wsi_helios_vehicle_enabled`/`wsi_helios_present_worker_finish` (`:229-253`), `wsi_swapchain::helios_async` (`:267-286`), `::helios_present_order` (`:288-299`), `::helios_vehicle_serving` (`:301-310`). | **MODIFY** — delete all of it. |
| `wsi_common_win32.cpp` | 2702 | **193 Helios lines.** The whole D3D11/DXGI/DComp vehicle: counters (`:87-115`), design comment (`:230-268`), UMD export typedefs (`:268-275`), vehicle state (`:277-460`), process-wide runtime + `LoadLibraryA("d3d11.dll"/"dxgi.dll"/"dcomp.dll")` (`:470-540`), per-HWND dcomp target cache (`:541-616`), UMD export resolution (`:711-735`), vehicle build/thread/start/finish (`:770-1107`), plus the ordinary Win32 WSI backend (surface `:1109-1437`, DXGI image path `:1439-1537`, GDI/DIB image path `:1539-1612`, acquire `:1746-1882`, present `:1884-2372`, swapchain create `:2374-2633`, init/finish `:2635-2701`). | **REWRITE** (near-total). §2.8 line 271 is explicit: "The current `wsi_common_win32.cpp` D3D11/DComp vehicle is **deleted, not adapted**." §10.7 line 2209 additionally stops the ICD advertising Win32 surface/swapchain at all. Conservative fail-closed result: the file survives as a compile unit whose `wsi_win32_init_wsi` registers **no** surface/swapchain implementation and whose every entry point returns the documented unsupported result; the vehicle, GDI/DIB blit path, DXGI path, HPS publish/release and the three UMD export bindings are gone. See ambiguity **A6**. |
| `helios_present_layer.cpp` | — | **does not exist** | **ADD** |
| `helios_present_layer.h` | — | **does not exist** | **ADD** |
| `helios_present_layer.def` | — | **does not exist** | **ADD** |
| `VkLayer_HELIOS_present.json.in` | — | **does not exist** | **ADD** — template: `src/vulkan/overlay-layer/VkLayer_MESA_overlay.json.in` + its `meson.build:28-58` (`shared_library` + `configure_file` into `explicit_layer.d`). Note this layer is **implicit**, not explicit — see ambiguity **A7**. |

### 2b. `icd/mesa/src/virtio/vulkan/` (sub-lane a)

| File | Lines | What it does today | Verdict |
|---|---:|---|---|
| `vn_renderer_helios.c` | 5304 | The only Windows `vn_renderer` backend and the centre of gravity for this lane. Regions: hand-mirrored protocol structs + `_Static_assert` size guards (`:255-352`); diag (`:657-712`); **10 `__declspec(dllexport)` raw-identity exports** (`:711-915`, incl. `helios_venus_memory_res_id` at `:752-767`); WDDM sync create/open/share/destroy/signal/wait (`:915-1192`); retired IOCTL residue (`:1318-1420`, `dev` is permanently `INVALID_HANDLE_VALUE`); `D3DKMTEscape` helper (`:1421-1519`); present-stream register/unregister (`:1520-1618`); Escape CTX create/destroy (`:1663-1686`); adapter/device probe (`:1687-1779`); Escape `SUBMIT_VENUS` (`:1780-1958`); GPU-fence submit + `helios_venus_queue_gpu_fence` export (`:1959-2270`); Escape blob alloc/map/release/attach + fence wait/event (`:2271-2554`); sync retire thread (`:2555-2928`); perf (`:2929-3077`); `helios_open_d3dkmt` (`:3078-3245`); VidMm mirror alloc/free/open-shared (`:3246-3580`); **external-memory create/open/export/get-handle/destroy (`:3564-3960`)**; `helios_submit`/`helios_wait` (`:3961-4193`); shmem/BO ops (`:4194-4549`); sync ops + named export/import (`:4550-4915`); renderer info/init/destroy/create (`:4916-5266`). | **REWRITE** (largest single unit in the lane). Everything Escape-, IOCTL-, blob-, present-stream-, named-fence- and raw-`res_id`-shaped is deleted; what survives is the KMT device/adapter open, the VidMm/allocation plumbing (re-pointed at HVM1), and the external-memory open (re-pointed at C57/`D3D12_RESOURCE_BIT`). |
| `vn_renderer.h` | 668 | The renderer vtable + the Windows-only Helios declarations at `:245-309` (named sync create/share/export, feedback shadow, present-stream register/unregister) and `:331-400` (external memory create/open/prepare_export/get_handle/destroy, VidMm alloc). `struct vn_renderer_shmem`/`_bo` expose raw `res_id` (`:11-29,83-104,175-179`). | **MODIFY** — delete `:253-273` (named sync), `:282-309` (feedback shadow + present stream); rewrite `:340-377` (external memory) for `D3D12_RESOURCE_BIT` and drop `out_resource_id`; replace `res_id` in the shmem/bo structs with an opaque HVM1 allocation capability + generation. |
| `vn_renderer_internal.c` / `.h` | 225 / 58 | The shmem-size-bucket cache (`vn_renderer_shmem_cache_*`) plus `vn_renderer_bo_export_sync_file_internal`. | **MODIFY** — the cache stays, but the cached object's identity becomes the HVM1 capability, not `res_id`. |
| `vn_ring.c` / `.h` | 992 / 152 | The shared-memory command ring: `vkCreateRingMESA` (`:545`), submit/seqno bookkeeping (`:594-870`), `vkNotifyRingMESA` (`:862`), `vkSetReplyCommandStreamMESA` (`:901`), `vn_ring_submit_command` (`:907`), `vn_ring_submit_roundtrip` + `vkWaitVirtqueueSeqnoMESA` (`:970-991`). | **MODIFY** — on Windows the entire ring is replaced by direct HNR2 dispatch. `vkSetReplyCommandStreamMESA` survives conceptually but moves into the HNR2 COMMIT prefix (§10.7:2172). See ambiguity **A10** (§17.3 says preserve the generic path for non-Helios builds; Appendix A.4 says "Delete `vn_ring`"). |
| `vn_queue.c` / `.h` | 4566 / 311 | `vn_QueueSubmit` (`:2014`), `vn_QueueSubmit2` (`:2174`), `vn_QueueBindSparse` (`:2445`), `vn_QueueWaitIdle` (`:2504`); present-stream tag on exported-semaphore signal (`:1740-1795`) and its unregister (`:3540-3546`); the semaphore create-info sanitizer that **strips `D3D12_FENCE_BIT`** (`:3371-3399`); named export/import of Win32 semaphores (`:4356-4425`); fence/semaphore feedback (`:1494-1504`, `:2796-2879`). | **REWRITE** of the submit half; **MODIFY** elsewhere. Record-only mode: seal-and-return, no lower queue submission. Normal mode: HNR2 fragment/COMMIT construction + same-context imported-native-fence wait/signal placement. Delete present-stream tagging and the named-semaphore path; **stop stripping `D3D12_FENCE_BIT`** (it becomes a supported import type). |
| `vn_device_memory.c` / `.h` | 1127 / 104 | `vn_device_memory_import_resource_id` (`:255`); alloc dispatch (`:541-548`); `vn_device_memory_import_win32` (`:550-583`) which currently gates on `OPAQUE_WIN32_BIT` and threads a raw `resource_id`; unwind (`:585-600`); `vn_AllocateMemory` dispatch incl. the win32 import/export arms (`:669-738`); `mem->helios_external_memory`; `.h:61` documents the "Native OPAQUE_WIN32 payload … owns/retains the Venus resource". | **REWRITE** — every ordinary `vkAllocateMemory` becomes one HVM1 `D3DKMTCreateAllocation2` allocation (`pSystemMem=NULL`) in one of the four roles; `D3D12_RESOURCE_BIT` import becomes a separate allocation class; delete `vn_device_memory_import_resource_id` and the `OPAQUE_WIN32` arms; only host-visible roles reach `D3DKMTLock2`. |
| `vn_physical_device.c` / `.h` | 3301 / 214 | Queue-family init incl. the Android emulated second queue (`:908-984`, exactly the doc's `:909-979`); external memory handle types (`:1119-1145`, `:2816-2846`, `:3032-3150`) — all `OPAQUE_WIN32`; external semaphore handle types (`:1255-1290`) — `OPAQUE_WIN32` only; `KHR_external_memory_win32` (`:1345`), `KHR_external_semaphore_win32` (`:1291`); WSI extension advertisement `KHR_swapchain`/`_maintenance1`/`_mutable_format` (`:1350-1377`). | **MODIFY** — advertise `D3D12_RESOURCE_BIT` (memory, IMPORTABLE + DEDICATED_ONLY) and `D3D12_FENCE_BIT` (semaphore, IMPORTABLE); stop advertising `KHR_swapchain*` on Windows; expose exactly two memory types over the one HLM1 heap; clamp descriptor-indexing and every related limit so an indivisible operation's allocation closure ≤ 4096 and typed operand occurrences ≤ 8192 in normal-loader mode (record-only mode keeps Tier-3). **Do not** reuse `emulate_second_queue`. |
| `vn_instance.c` / `.h` | 478 / 129 | Instance extension table incl. `KHR_surface`/`KHR_surface_maintenance1`/`KHR_surface_protected_capabilities` (`:39-43`) and `KHR_win32_surface` (`:62-65`); ring bring-up (`:177-184`, `:309-342`); `vn_ring_submit_command` use (`:448`). | **MODIFY** — stop advertising Win32 surface on Windows; one HTS1 session per `vn_instance`; ring bring-up replaced by HVC1 control context + finite INIT. |
| `vn_device.c` / `.h` | 765 / 83 | Device extension gating (`:257-366`, incl. `KHR_external_memory_win32`/`KHR_external_semaphore_win32`), queue construction, ring/renderer wiring (`:83-104`, `:573-582`, `:721-734`). | **MODIFY** — owns device/queue/context/ring generation and drain order; one HVC1 queue context + one unshared monitored progress fence per real lower queue; `vkDeviceWaitIdle` joins every nonzero queue milestone. |
| `vn_buffer.c` / `.h` | 611 / 78 | Buffer create + the cached-memory-requirements path (`:262-300`). | **MODIFY** — C60 classification: requirement queries are pure control; bind records the exact HVM1 allocation/offset/range. |
| `vn_image.c` / `.h` | 1174 / 111 | `vn_CreateImage` (`:546`) with `VkImageSwapchainCreateInfoKHR` handling at `:559-622`; memory-requirement cache (`:386-407`); `vn_image_bind_wsi_memory` (`:719`) and `vn_BindImageMemory2` (`:759`) with `VkBindImageMemorySwapchainInfoKHR` at `:737-739`. | **MODIFY** — the swapchain-alias arms move **up into the layer**; the ICD's job becomes (i) ordinary external-image creation with `D3D12_RESOURCE_BIT`, (ii) the presentable-image tag that legalises `PRESENT_SRC_KHR` for tagged images (§10.7:2581-2588), (iii) C60 classification for requirements/create/bind. |
| `vn_pipeline.c` / `.h` | 2019 / 76 | Pipeline create + cache data (`:685-687`, `:1801-1817`). | **MODIFY** — descriptor-independent compilation is pure control; pipeline-cache data keeps the `VK_INCOMPLETE` partial rule for results > 64 MiB. |
| `vn_query_pool.c` / `.h` | 471 / 40 | `vn_GetQueryPoolResults` (`:201-260`). | **MODIFY** — query `WAIT` is GPU-dependent (HQC1 join first); result data > 64 MiB partitions into consecutive query ranges after one GPU join; `VK_NOT_READY` preserved. |
| `vn_wsi.c` / `.h` | 1112 / 107 | `vn_wsi_get_helios_resource_identity` (`:132-163`), `vn_wsi_init` forcing `sw_device=true` on Windows (`:165-246`), `vn_CreateSwapchainKHR` (`:904`), `vn_AcquireNextImage2KHR` (`:966`), `vn_QueuePresentKHR` (`:1098`), extension gating (`:625`, `:648`). | **MODIFY** — delete `vn_wsi_get_helios_resource_identity` and the `win32.get_helios_resource_identity` hook; on Windows the swapchain/present entry points become unreachable (see ambiguity **A5**). |
| `vn_icd.c` / `.h` | 26 / 31 | `vk_icdGetInstanceProcAddr` + `vn_icd_supports_api_version`. | **MODIFY** — §17.3 lists it but no prose says why. Conservative reading: this is where the **one** exported private direct-dispatch entry point is declared/exported alongside the loader entry points, and where the loader-vs-direct provenance check lives. See ambiguity **A2**. |
| `meson.build` | 174 | `libvn_files` (`:56-79`), Windows arm adding `vn_renderer_helios.c` + `setupapi` + `gdi32` + the WDK include (`:120-135`), `vn_wsi.c` gate (`:137-142`), `libvulkan_virtio` shared library (`:157-174`). | **MODIFY** — add the four new `vn_helios_*.c` files; drop `setupapi` (its only user is the dead IOCTL residue). |
| `vn_helios_record_submit.{c,h}` | landed at `5cbc0254f43` | Per-instance record/normal submission owner, exact live-scope recording, bounded immutable batch sealing, normal HNR2 dispatch, imported-fence ordering, and C51 joins. | **A4 DONE; SOURCE/BUILD-VALIDATED ONLY** |
| `vn_helios_direct_dispatch.{c,h}` | — | **do not exist** | **ADD** |
| `vn_helios_translation_session.{c,h}` | — | **do not exist** | **ADD** |
| `vn_helios_native_kmt.{c,h}` | 1206 / 248 | **A2, landed `1b97c64`.** Pure half: CRC-64/ECMA-182, the HNR2 fragment planner and encoder. Windows half: HVC1 control/queue contexts, `D3DKMTRender` + buffer re-adoption + the resize transition, the per-context monitored progress fence, the C51 waits and the queue/device idle joins. | done |

### 2c. Capability facts the manifest's wording understates

Three things the doc calls a "rewrite of the current live path" are in fact
**new capability**, verified by grep across `src/virtio/vulkan/**`:

1. **`VK_EXTERNAL_MEMORY_HANDLE_TYPE_D3D12_RESOURCE_BIT` does not appear
   anywhere in the ICD.** `vn_renderer_helios_external_memory_open`
   (`vn_renderer_helios.c:3696-3865`) hard-gates on
   `VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_WIN32_BIT` at `:3708-3712`. The
   query/open *shape* the doc says "already has this shape" is genuinely there
   (`:3747-3807`), but the handle type, the advertisement, the
   IMPORTABLE/DEDICATED_ONLY external-image query, and the dedicated-import
   requirement are all absent.
2. **`VK_EXTERNAL_SEMAPHORE_HANDLE_TYPE_D3D12_FENCE_BIT` appears exactly twice**
   and both are *removals*: a comment at `vn_queue.c:3361` and the mask that
   strips it from export handle types at `vn_queue.c:3391`. Import support,
   `D3DKMTOpenNativeFenceFromNtHandle`, and the HNF1 private-data validation of
   §12.2 are entirely new.
3. **The ICD has no C mirror of `protocol/`.** `vn_renderer_helios.c:255-352`
   hand-declares `helios_wddm_open_identity` etc. and guards them with
   `_Static_assert(sizeof(...) == N)` only — **size, not offsets, and no
   cross-repository check**. Every new record this lane must parse (HWA2 168 B,
   HVC1 32 B, HNR2 112 B, HVM1 64 B, HVR1 80 B, HQA1 72 B) will land in the same
   hand-mirrored, uncheckable state unless the protocol lane emits a C header.
   This is the exact failure class `CLAUDE.md` flags for
   `VKD3D_HEAP_FLAG_HELIOS_VENUS_EXPORT`. See **CROSS-LANE REQUESTS**.

---

## 3. Work decomposition

Ordered. `Deps` names units that must land first. Sizes: S ≈ ≤200 lines
touched, M ≈ 200–800, L ≈ 800–2000, XL ≈ >2000.

### Sub-lane (a) — lower ICD

| Id | Goal | Files it owns (exclusive) | Deps | Size |
|---|---|---|---|---|
⭐ **STATE 2026-08-10.** **A0 is done** (`icd/mesa` `23ab160` — the four local
record declarations are gone, `vn_helios_hwa2.h` includes `helios_wddm.h`, and
`meson.build` `error()`s if the header is unreachable; `OWNERSHIP.md` §2 pair 3
has the measurement). **A1 is done** (`6ad43fb`,
`vn_helios_translation_session.{c,h}`, builds clean under `win_meson`) — but it
was *implemented and never exercised* at that checkpoint: its INIT refused until
KMD unit **K5** existed. K5 and K11 have since landed and been target-exercised;
**A2 is done** (`1b97c64`), **A3 is done** (`2c2763b8b1a`), **A4 is done**
(`5cbc0254f43`), **A5 is done** (`4ea18b3512f`), and fail-closed **A6 is done**
(`d07d1d13687` + review fix `478c71a0fff`). The new KMD executor and Mesa A3-A6
remain source/build-only; A7 is stopped at F21's missing outer-token ingress.

⭐ **A2 amendment, 2026-08-10.** A2 landed with an inverted dependency and a new
gate, both deliberate:

- **A2 does not depend on A1.** `helios_native_context_create` takes a raw
  `D3DKMT_HANDLE` device, so *A1 depends on A2* instead. The alternative was two
  HNR2 encoders in one ICD, and A1's — written first — was wire-invalid in three
  ways (`UseRecordOffsetMismatch` from writing the use table after the payload,
  `AllocationGenerationZero`, and a monitored fence with none of §10.7's three
  flags). One encoder, one owner.
- **§5.4 is obsolete.** A2's file splits into a platform-independent encoder half
  and a Windows KMT half, so `tools/hnr2-encoder-gate.sh` compiles and RUNS the
  encoder on Linux and replays every command buffer it emits through
  `protocol/`'s `validate` + `validate_commit_tables`. The host can now make
  behavioural assertions about this lane, not only compile-time ones. The owner closed the
A3 scope question on 2026-08-10 in favour of **full A3 as this table scopes
it**; `ROADMAP.md` records that the honest scope is five units — A1, A2, A3 plus
KMD **K5** and **K6** — because A1–A3 speak HTS1 and HNR2 to a kernel that
implements neither. B1–B4 and B9 (the present layer) are also done; B5 is
partial and F8 reverses its fence direction.

| **A0** | Consume the protocol C header: one `helios_protocol.h` include point, package-generation constant, and `_Static_assert` on **every offset** of HWA2/HQA1/HVC1/HNR2/HVM1/HVR1 + the 24/16/40/48-byte records. No new struct is hand-declared in the ICD. | *(none — adds only `#include` + asserts; lands as part of A1's first commit)* | protocol lane (§17.1) | S |
| **A1** | HTS1: raw KMT device per `vn_instance`, one HVC1 control context (both queue ordinals `UINT32_MAX`), the **4 MiB / 4×1 MiB** role-1 reply pool (create + residency + one process-local `D3DKMTLock2` held to teardown), finite `INIT`, session generation/capability/endpoint-capacity capture, C51 event-backed reply, slot checkout/publication/retire state machine, HVR1 validation + exact-next-offset continuation for logical snapshots up to 64 MiB. | `vn_helios_translation_session.{c,h}` | A0, K2a | L |
| **A2** | Native KMT lane: HVC1 queue contexts (`NodeOrdinal=0`, `EngineAffinity=0`, `Flags.Value=0`, `ClientHint=VULKAN`), `DXGK_CONTEXTINFO` minima validation, the HNR2 encoder/fragmenter (≤15 MiB, ≤64 fragments, COMMIT-metadata reservation), `D3DKMTRender` call shape + returned-buffer adoption + `ResizeAllocationList`/`ResizePatchLocationList` transition, CRC64-ECMA, one unshared monitored fence per context (`D3DKMTCreateSynchronizationObject2`, `NoSignalMaxValueOnTdr=1`, `NoGPUAccess=1`) + `SignalSynchronizationObjectFromGpu`/`WaitForSynchronizationObjectFromCpu` C51 waits, per-queue/device idle joins. | `vn_helios_native_kmt.{c,h}` | A1 | XL |
| **A3 ✅** | Renderer rewrite: delete Escape/IOCTL/blob/present-stream/named-fence/raw-`res_id`-export machinery; HVM1 allocation create/free (`D3DKMTCreateAllocation2`, `pSystemMem=NULL`, shared CPU-visible roles 1–3, role 4 non-CPU-visible); retain the exact allocation/process Lock2 view supplied by K2a and `Unlock2` it at the matching lifetime boundary; shmem/BO identity becomes an opaque HVM1 capability+generation, never a mapping token; make any size/range shape beyond the proven udmabuf bound a named hard failure; C57 import carrier (`D3D12_RESOURCE_BIT` only, arrays sized from the query, zeroed allocation array = hard failure, HWA2 validated, `D3DKMTDestroyAllocation2{hResource,NULL,0,0}` close); `D3DKMTOpenNativeFenceFromNtHandle` per §12.2. | `vn_renderer_helios_hvm.c`, `vn_renderer.h`, `vn_renderer_internal.{c,h}` | A1, A2, K2a, F20's authorized KMD continuation | XL; landed `2c2763b8b1a` |
| **A4 ✅** | Record-only submit + normal submit: `vn_QueueSubmit`/`_2`/`vn_QueueBindSparse` become mode-dispatched; record-only seals an immutable batch (version, session/endpoint/context generations, context-local batch id, length, CRC, complete use table) and returns it synchronously — **no** KMT queue-work call; normal mode drives A2's HNR2 path and places imported-native-fence wait/signal around it; queue entry points without a live outer scope are refused; `vn_QueueWaitIdle`/`vkDeviceWaitIdle`/teardown use the C51 joins. | `vn_helios_record_submit.{c,h}`, `vn_queue.c`, `vn_queue.h` | A2, A3, K9, F20's authorized KMD continuation | XL; landed `5cbc0254f43` |
| **A5 ✅** | Private direct dispatch: the single exported entry point that hands DXVK/vkd3d a versioned function table (package-generation-checked); the non-forgeable record-only instance tag; provenance rejection of any proc whose owning module is the loader or the layer; endpoint descriptor export for HQA1. | `vn_helios_direct_dispatch.{c,h}`, `vn_icd.c`, `vn_icd.h` | A1, A4 | M; landed `4ea18b3512f` |
| **A6 ✅ (capability withheld)** | Physical-device / instance profile: stop advertising `KHR_win32_surface` + `KHR_swapchain*` on Windows; implement exact `D3D12_RESOURCE_BIT`/`D3D12_FENCE_BIT` IMPORTABLE + DEDICATED_ONLY answers but leave them unadvertised until A7; two memory types over one HLM1 heap; normal-loader limit clamps (≤4096 closure / ≤8192 operands); leave `emulate_second_queue` unused. | `vn_physical_device.{c,h}`, `vn_instance.{c,h}`, `vn_wsi.{c,h}` | A3 | L; landed `d07d1d13687`, review fix `478c71a0fff` |
| **A7 ⛔ F21** | C60 classifier across the object files: pure-control calls on HVC1, allocation-backed calls become deferred records consumed by the first outer batch that names the allocation, GPU-dependent calls do the HQC1 join first. Includes the presentable-image tag call + `PRESENT_SRC_KHR`/`QUEUE_FAMILY_EXTERNAL` validation for tagged images. | `vn_device.{c,h}`, `vn_buffer.{c,h}`, `vn_image.{c,h}`, `vn_pipeline.{c,h}`, `vn_query_pool.{c,h}` | A1, A4, and the not-yet-authorized DXVK/vkd3d/UMD outer-token ingress | Blocked; not started |
| **A8** | Ring retirement: remove `vkCreateRingMESA`/`vkNotifyRingMESA`/`vkWaitRingSeqnoMESA`/`vkWaitVirtqueueSeqnoMESA`/shared head-tail/spin-sleep watchdogs from the Windows path; `SetReplyCommandStreamMESA` + `GENERATE_REPLY` move into HNR2 COMMIT. | `vn_ring.c`, `vn_ring.h` | A1, A2 | M |
| **A9** | Build wiring for sub-lane (a): add the four `vn_helios_*.c`, drop `setupapi`. | `src/virtio/vulkan/meson.build` | A1–A8 | S |

**Serialization notes for (a).** `vn_renderer_helios.c` (A3) and `vn_queue.c`
(A4) are each single-owner by construction — do not let A2 edit either; A2
exposes its API through `vn_helios_native_kmt.h` and A3/A4 call it. A0's
asserts land inside A1's first commit so no unit ships an unasserted mirror.
A6 must not land before A3, or the ICD will advertise an import type whose
implementation does not exist (the doc's fail-closed rule at §10.7:2552 —
"any failure refuses the import and leaves the `D3D12_RESOURCE_BIT` capability
unadvertised").

### Sub-lane (b) — `VK_LAYER_HELIOS_present`

| Id | Goal | Files it owns (exclusive) | Deps | Size |
|---|---|---|---|---|
| **B1** | Layer skeleton: `vkNegotiateLoaderLayerInterfaceVersion`, instance/device dispatch capture, `vkGetInstanceProcAddr`/`vkGetDeviceProcAddr` returning exactly the §10.7:2250-2267 lists and `NULL` elsewhere, both extension-enumeration forms with the two-call/`VK_INCOMPLETE` contract for both `pLayerName` forms, instance/device create-chain copy + consumption + lower-name filtering, the generated entry-point manifest compared byte-for-byte. | `helios_present_layer.h`, `helios_present_layer.def`, `VkLayer_HELIOS_present.json.in` | — | L |
| **B2** | Device creation: require lower Vulkan 1.3; add `VK_KHR_external_memory_win32` + `VK_KHR_external_semaphore_win32`; force `timelineSemaphore` + `synchronization2` without duplicating feature structs or mutating app memory; the **one** private helper queue appended to the app's own canonical `flags=0` record (`q-1` reported, private index hidden from both getters); singleton device-group admission. | `helios_present_layer.cpp` (shared — see note) | B1 | L |
| **B3** | Surface + profile: `vkCreateWin32SurfaceKHR`/`vkDestroySurfaceKHR`, support/caps/formats/present-modes/`capabilities2`, `vkGetPhysicalDeviceWin32PresentationSupportKHR`, `vkGetPhysicalDevicePresentRectanglesKHR`, `vkGetDeviceGroupPresentCapabilitiesKHR`, `vkGetDeviceGroupSurfacePresentModesKHR`; the §10.7:2454-2463 admission table enforced as *queries return unsupported*, not as later failures; `(0,0)` client extent; resize/minimize → `OUT_OF_DATE`, destroyed HWND → `SURFACE_LOST`. | `helios_present_layer.cpp` (shared) | B2 | L |
| **B4** | D3D12/DXGI object graph: adapter selection by exact LUID; D3D12 device + copy queue; `CreateSwapChainForHwnd` FLIP_DISCARD / `DXGI_SCALING_NONE` / `ALPHA_MODE_IGNORE` / `CheckColorSpaceSupport`+`SetColorSpace1`; `S[i]` `CreateCommittedResource` with exactly the §10.7:2492-2500 flag set; two `CreateFence(0, SHARED)` per slot; one `CreateSharedHandle` per object; per-slot allocator/list. | `helios_present_layer.cpp` (shared) | B3 | L |
| **B5** | Canonical import: per slot, lower external `VkImage` → `vkGetMemoryWin32HandlePropertiesKHR` → dedicated `VkImportMemoryWin32HandleInfoKHR` → offset-zero bind → ICD presentable-image tag, **all before `vkGetSwapchainImagesKHR` can expose the image**; permanent `D3D12_FENCE_BIT` timeline-semaphore import for `Ready[i]`/`Release[i]` with the transient NT handle closed immediately after. | `helios_present_layer.cpp` (shared) | B4, **A3**, **A6**, **A7** | L |
| **B6** | Acquire/Present state machine: the nine-state slot machine, epoch `e[i]`, `GetCompletedValue` selection, `SetEventOnCompletion` blocking with no polling, `VK_NOT_READY`/`VK_TIMEOUT`; the single `vkQueueSubmit2` release on the app's queue, per-slot release command buffers, `Ready[i]=e` signal; then per-`(swapchain,image)` D3D wait → `GetCurrentBackBufferIndex` → `CopyResource` → `Release[i]=e` → `Present(1,0)`; per-swapchain `pResults`. | `helios_present_layer.cpp` (shared) | B5 | XL |
| **B7** | C45 alias images: intercept `vkCreateImage`/`vkBindImageMemory2`/`vkDestroyImage`; VUID 00995/01630/01631/01644 enforcement; `swapchain=VK_NULL_HANDLE` consume-and-strip pass-through; mixed batches preserving order and `VkBindMemoryStatus`; `ALIAS_ONLY` survival past swapchain destruction. | `helios_present_layer.cpp` (shared) | B6 | L |
| **B8** | Teardown/retirement: `oldSwapchain` (new set first, then atomic retire), destruction drain of in-flight D3D Release + DXGI Present, the final `EXTERNAL → canonical-family` `GENERAL→GENERAL` barrier on the helper queue, backing lifetime held while any alias lives, device-loss/`VK_ERROR_DEVICE_LOST` mapping. | `helios_present_layer.cpp` (shared) | B7 | M |
| **B9** | Build + manifest wiring: the layer `shared_library()` (must **not** enter `libvulkan_wsi`), `.def` exports, `configure_file` of the JSON, mingw `d3d12`/`dxgi`/`dxguid` link. | `src/vulkan/wsi/meson.build` (shared with A? no — shared with the `wsi_helios_present_sync.c` removal, unit B0 below) | B1 | S |
| **B0** | Delete the HPS2 writer and every `helios_*` addition to upstream WSI. | `wsi_helios_present_sync.{c,h}` (delete), `wsi_common.c`, `wsi_common.h`, `wsi_common_private.h`, `wsi_common_win32.cpp` | — | L |

**Serialization note for (b), and it is the important one.** The manifest names
exactly **one** `.cpp` for the whole layer, so B2–B8 all write
`helios_present_layer.cpp`. They cannot be parallelised as written.
Recommended (and, I believe, permitted — §17 line 3745 calls the manifest "the
minimum exact file manifest … may not omit a listed live owner", not a
prohibition on additional files): keep `helios_present_layer.cpp` as the live
owner of the loader/dispatch surface (B1/B2) and add
`helios_present_layer_surface.cpp`, `helios_present_layer_swapchain.cpp`,
`helios_present_layer_d3d12.cpp`, `helios_present_layer_alias.cpp` as internal
compilation units declared by `helios_present_layer.h`. If that is refused,
B2→B8 must run strictly serially in one worktree.

`B0` and `B9` both edit `src/vulkan/wsi/meson.build` — land B0 first, then B9.

---

## 4. Shared-file hazards

Files this lane touches that another lane also names, or that another lane's
output constrains:

| File / artefact | Other lane | Serialization |
|---|---|---|
| `protocol/src/native_render.rs`, `translation_session.rs`, `wddm.rs`, `physical_memory.rs` + **their generated C declarations** | §17.1 protocol lane | Hard dependency. Sub-lane (a) cannot write a byte of HNR2/HVC1/HVM1/HQA1/HVR1/HWA2 parsing until the C declarations exist. A0 is the gate. **This lane must not author a second declaration of any of these records** (standing directive: shared private data has one declaration, in `protocol/`). |
| The private direct-dispatch table (`vn_helios_direct_dispatch.h`) | §17.2 DXVK, §17.5 vkd3d/UMD12 | The header is authored here but *consumed* across two other repositories (`dxvk-helios/`, `vkd3d-proton-helios/`) and by `umd/bridge/`. Today those consumers reach the ICD purely by `GetProcAddress` on names (`umd/bridge/bridge_icd_exports.cpp:308-344`), which the doc deletes. Agree the table's shape and its `extern "C"` header location **before** A5 lands, or three lanes will invent three ABIs. See CROSS-LANE REQUESTS. |
| HQA1 (72 B) | §17.4 D3D11 UMD, §17.5 UMD12, §17.6 KMD | This lane *produces* the endpoint id / engine class / queue family+index values and consumes nothing; the UMD builds and sends the packet; KMD validates. Only the endpoint-descriptor accessor is ours. |
| HWA2 (168 B) | §17.6 KMD (writer), §17.4 D3D11 UMD (opener) | This lane is a **reader only**, at C57 import. Any field-layout drift breaks the import silently unless offsets are asserted (A0). ⛔ **Corrected 2026-08-10 per `docs/retirement/K4-CONTRACT.md` §5: re-pointing this ICD from the retired 48-byte `helios_wddm_open_identity` to HWA2 is NOT a field substitution, and must not be planned as one.** HWA2 deliberately carries **no host resource id** and **no Vulkan memory-type index** (`protocol/src/wddm.rs:393-394` — "No host resource token, `resid`, PID, process handle, synchronization object, mutable value, or independently usable identity"; and `:326-327` on `memory_class` — "no Vulkan memory-type index. ⛔ The retired trailer carried `memory_type_index`; it is gone"). Today `identity.resource_id` (read at `vn_renderer_helios.c:3848-3862`) is passed straight into `VkImportMemoryResourceInfoMESA::resourceId` and `vn_renderer_bo_create_from_resource_id` (`vn_device_memory.c:267-277`) — it is how the guest names the host object, and it has **no successor field**. The replacement is a different *mechanism*: the ICD stops naming host resources at all and the KMD patches the host resid in from `HeliosNativeRenderPatch` (`protocol/src/native_render.rs:622-642`). That is this lane's unit **A3** plus KMD **K6**. ⇒ Until both land, every reader of a field HWA2 does not carry must fail **loudly, with a named counter that names A3** — never fall back, never fabricate — and "an ICD in this state cannot import" is the retirement's intended intermediate state, not a regression to be papered over. |
| `D3DKMTRender` with the resize flags and `CommandLength=0` | §17.6 KMD (`DxgkDdiRender`) | The KMD must tolerate the buffer-resize transition Render that carries no HNR2 (§10.7:1744-1750). Cross-lane request below. |
| `packaging/windows/Install-Helios.ps1` / `.cmd` | §17.7 | The layer's JSON manifest must be registered as a Windows **implicit** layer (registry key, not a filesystem search path). This lane produces `VkLayer_HELIOS_present.json` + the DLL; §17.7's lane installs them (§17.7:4615-4618). |
| `tools/d3d11_kmt_shared_probe.cpp`, `tools/vk_ring_fence_probe.cpp` | §17.7 | §17.3:3986 says "Remove `tools/d3d11_kmt_shared_probe.cpp`" but the file lives in the `tools/` lane and §17.7:4530 already lists it for deletion. **This lane must not delete it.** Reported below. |
| `src/vulkan/wsi/meson.build` | inside this lane only (B0 vs B9) | B0 first. |

No other lane edits anything under `icd/mesa/`.

---

## 5. Build and verification

### 5.1 The finding that matters: this lane builds on the Linux host

The documented path is `win_meson` on the win11 VM (`TOOLCHAIN.md`,
`tools/win-mcp/src/main.rs` `win_meson`), reading `Z:\icd\mesa` and building
into `C:\Users\Rupansh\helios-mesa-build` with WinLibs mingw-w64 gcc. That path
is unavailable to a lane forbidden from touching the VM.

**It is not needed.** I cross-compiled the current ICD to
`vulkan_virtio.dll` on this Linux host, from a clean tree, in one pass. Exact
reproduction:

```
# one-time: mesa's build needs python mako + packaging
python3 -m venv $SCRATCH/venv
$SCRATCH/venv/bin/pip install mako packaging

# one-time: a mingw CROSS file (the repo only ships a NATIVE one,
# icd/win-build/mingw-native.ini, for building on Windows)
cat > $SCRATCH/mingw-cross.ini <<'EOF'
[binaries]
c = 'x86_64-w64-mingw32-gcc'
cpp = 'x86_64-w64-mingw32-g++'
ar = 'x86_64-w64-mingw32-ar'
strip = 'x86_64-w64-mingw32-strip'
windres = 'x86_64-w64-mingw32-windres'
ninja = 'ninja'
[host_machine]
system = 'windows'
cpu_family = 'x86_64'
cpu = 'x86_64'
endian = 'little'
[built-in options]
c_link_args = ['-static', '-static-libgcc']
cpp_link_args = ['-static', '-static-libgcc', '-static-libstdc++']
EOF

cd icd/mesa
PATH=$SCRATCH/venv/bin:$PATH meson setup $SCRATCH/mesa-cross-build . \
  --cross-file $SCRATCH/mingw-cross.ini \
  "-Dc_args=-include$PWD/../win-build/helios_win_compat.h" \
  -Dvulkan-drivers=virtio -Dgallium-drivers= -Dplatforms=windows \
  -Dvideo-codecs= -Dvulkan-layers= -Degl=disabled -Dgbm=disabled \
  -Dglx=disabled -Dopengl=false -Dgles1=disabled -Dgles2=disabled \
  -Dllvm=disabled -Dshader-cache=disabled -Dbuild-tests=false \
  -Dperfetto=false --buildtype=debugoptimized \
  -Dhelios-wdk-include=$PWD/../win-build/wdk-include
PATH=$SCRATCH/venv/bin:$PATH ninja -C $SCRATCH/mesa-cross-build
```

Result today: `[256/256] Linking target src/virtio/vulkan/vulkan_virtio.dll`
(50 MB, debugoptimized), warnings only. The vendored
`subprojects/DirectX-Headers-1.0` resolves, mingw supplies `libd3d12.a` and
`libdxgi.a`, and `icd/win-build/wdk-include` supplies the real
`d3dkmthk.h`/`d3dkmdt.h`/`d3dukmdt.h`. **The whole of sub-lane (a) and the
compile half of sub-lane (b) are therefore verifiable on the Linux host with no
VM, no WDK and no owner-gated action.** Nothing in `TOOLCHAIN.md`, `ROADMAP.md`
or the retirement doc says this; it should be written down.

Caveats: the cross toolchain is gcc 16 mingw-w64 from the distro, not the
WinLibs UCRT gcc 16.1 the VM uses, so ABI-identical artefacts are not
guaranteed — treat the host build as a **compile/link/import-table oracle**,
not as a deployable binary. Always re-build on the VM before deploying.

### 5.2 Static gates that also run on the host

§13.2's import-table test is a host command:

```
x86_64-w64-mingw32-objdump -p .../vulkan_virtio.dll | grep -i 'DLL Name'
```

Today's output is `GDI32 / KERNEL32 / USER32 / api-ms-win-crt-* /
api-ms-win-core-synch-l1-2-0` — **already free of `dxgi.dll`, `d3d11.dll`,
`d3d12.dll` and `dcomp.dll`**, because the current vehicle loads them with
`LoadLibraryA` (`wsi_common_win32.cpp:486-508`) rather than importing them. The
gate must therefore *also* assert that no `LoadLibrary` of those names remains,
or it will pass vacuously. After B9 the same objdump run on
`VkLayer_HELIOS_present.dll` must show `d3d12.dll`/`dxgi.dll` (that DLL is
allowed them) while `vulkan_virtio.dll` must still show none — and
`vulkan_virtio.dll` must not import `vulkan-1.dll`.

### 5.3 What is VM-only

Everything behavioural: `D3DKMTCreateDevice`/`CreateContext`/`Render`/`Lock2`
round trips, HTS1 INIT against the real KMD, the C57 open against a real D3D12
shared texture, native-fence open, the §18.3 gates, Vulkan validation-layer
traces, and every pixel result. None of it can be reached from this lane.

### 5.4 What passes today

- The tree cross-builds clean (above).
- There is **no unit-test harness** for the ICD in this repo — `-Dbuild-tests=false`
  is part of both the VM and host configure lines, and Mesa's own tests do not
  cover `src/virtio/vulkan`. Every assertion this lane can make on the host is a
  compile-time one, which is precisely why A0's offset `_Static_assert`s matter
  more here than anywhere else in the project.

---

## 6. Blockers, ambiguities, and contradictions

**14 items.** Each states both sides.

**A1 — No C declaration source exists for any of the new records, and the doc
never says who emits one.** §17.1 asks the protocol lane for "generated QEMU C
declarations" (3758) and "generated C bindings" for diagnostics (3784), and for
`wddm.rs` to "Generate/assert both Rust and C offsets" (3780-3781) — but says
nothing about C declarations for HVC1/HNR2/HVM1/HVR1, which only Mesa and KMD
consume. The current tree's answer is hand-mirroring with size-only asserts
(`vn_renderer_helios.c:255-352`). *Conservative reading:* refuse to hand-mirror;
require a generated header (CROSS-LANE REQUEST 1) and, until it exists, block
A0/A1. *If overruled:* mirror with **offset** asserts
(`_Static_assert(offsetof(...) == N)`) on every field, not size asserts.

**A2 — `vn_icd.c`/`vn_icd.h` are in the Modify list with no stated change.**
§17.3:3914 lists them; no sentence in §10.4, §10.7, §13 or §17.3 says what
changes. Both files together are 57 lines and contain only
`vk_icdGetInstanceProcAddr` and `vn_icd_supports_api_version`. *Conservative
reading:* this is where the one private direct-dispatch export is declared and
where a loader-provenance check belongs (§13.2:3365-3368 requires rejecting a
proc "whose owning module is the Vulkan loader or WSI layer"). Implement exactly
that and nothing else.

**A3 — The private direct-dispatch entry point is never named or specified.**
§2.8:260-263 says translators "load the Helios ICD through a private direct
dispatch entry point rather than the Vulkan loader"; §10.4:1192-1194 says "the
UMD receives the function table directly while creating its translator
instance"; §13.2:3366 says "vkd3d's instance creation receives the same private
direct-ICD proc table". No name, no signature, no version negotiation, no
package-generation check location. Meanwhile the mechanism being deleted
(`umd/bridge/bridge_icd_exports.cpp:308-344`, ten `GetProcAddress`-by-name
exports) is exactly the "loaded-module discovery" A.4:5777 forbids. *Conservative
reading:* exactly **one** exported symbol, versioned, generation-checked,
returning a const table; every other `__declspec(dllexport)` in
`vn_renderer_helios.c:711-915` deleted. Shape must be agreed with the DXVK and
UMD12 lanes before A5 (CROSS-LANE REQUEST 2).

**A4 — The presentable-image tag call has no name, ABI, or discovery rule.**
§10.7:2581-2588 requires "a device/image-scoped private dispatch call" from the
layer into the lower ICD that makes `VK_IMAGE_LAYOUT_PRESENT_SRC_KHR` legal for
`S[i]` and enables validation of the `PRESENT_SRC_KHR → GENERAL →
VK_QUEUE_FAMILY_EXTERNAL` release. But §2.8/§13.1 put the layer *above* the
loader, so its only channel to the ICD is the next-chain
`vkGetDeviceProcAddr` — i.e. a non-standard device function name that
`vn_GetDeviceProcAddr` must return. That works, but it is a second private ABI
that no section defines. *Conservative reading:* a single
`vkSetHeliosPresentableImageHELIOS`-style device command resolved through the
captured next-layer GDPA, refusing if the lower chain does not provide it (which
then fails `vkCreateSwapchainKHR`, never silently degrades).

**A5 — Does `vn_QueuePresentKHR` survive on Windows?** §17.3:3997-3998 says make
it "unavailable to **translator instances**", which implies it stays available
to normal instances. §10.7:2209 says "The ICD itself stops advertising Win32
surface and swapchain entry points to ordinary loader clients", and §17.3:3954
says lower WSI names/functions are filtered by the layer — which makes the ICD's
swapchain path unreachable by anyone on Windows. *Conservative reading:* on
Windows, `KHR_swapchain`/`KHR_win32_surface` are not advertised at all and
`vn_QueuePresentKHR` is compiled out (or returns
`VK_ERROR_EXTENSION_NOT_PRESENT`-class failure) for **every** instance; the
non-Windows path is untouched.

**A6 — What is left of `wsi_common_win32.cpp`?** §2.8:271 says the vehicle is
"deleted, not adapted" and §17.3:3900-3902 says "remove the entire D3D11/DXGI/
DComp vehicle, HPS publish/release, named fences, GDI fallback selected by that
vehicle, and recursive callbacks" — but the file stays in the Modify list, and
§10.7 removes the ICD's Win32 surface/swapchain entirely, which removes the
*reason* for the remaining 1600 lines (surface queries, GDI/DIB images, acquire,
present, swapchain create). The doc never says whether the file is emptied or
dropped from `meson.build`. Note the qualifier "GDI fallback **selected by that
vehicle**" — read strictly, the non-vehicle GDI path is not named for deletion.
*Conservative reading:* keep the compile unit; `wsi_win32_init_wsi` registers no
surface implementation; every entry point returns its documented unsupported
result; delete the vehicle, HPS, and the three `helios_umd_*` bindings. That is
fail-closed and reversible. Do **not** silently keep a working GDI presenter —
it would be a second, unadvertised present path.

**A7 — Implicit vs explicit layer manifest.** §10.7:2208 says "one mandatory
**implicit** layer, `VK_LAYER_HELIOS_present`". The only templates in-tree
(`src/vulkan/*-layer/VkLayer_MESA_*.json.in` + their `meson.build`) produce
`"type": "GLOBAL"` manifests installed into `explicit_layer.d`. An implicit
layer needs a different install location and — per the Vulkan loader spec — a
`disable_environment` key, which the doc never specifies. On Windows both
implicit and explicit layers are discovered through `HKLM\SOFTWARE\Khronos\
Vulkan\{Implicit,Explicit}Layers`, so this is also a packaging-lane input.
*Conservative reading:* emit an implicit manifest with a
`disable_environment` key and document it; flag to the packaging lane.

**A8 — The imported `D3D12_RESOURCE_BIT` memory has no memory type, but Vulkan
requires one.** §10.7:2076-2078: "C57 `D3D12_RESOURCE_BIT` imports are a
separate allocation class: they never masquerade as an ordinary HVM1 allocation,
are **never exposed through an ordinary memory type**, and are never locked."
§10.7:2062-2064 exposes exactly two memory types (role 4 device-local, role 2
device-local+host-visible+coherent). But `vkAllocateMemory` needs a
`memoryTypeIndex`, and §10.3:1153-1155 / §10.7:2524-2526 require
`vkGetMemoryWin32HandlePropertiesKHR` to report a **compatible** memory-type
bit, which must be one of those two. The three statements cannot all be literally
true. *Conservative reading:* `vkGetMemoryWin32HandlePropertiesKHR` returns the
role-4 (device-local-only) type bit; the resulting `VkDeviceMemory` is tagged
import-class, has no HVM1 role record, is never `D3DKMTLock2`'d, and
`vkMapMemory` on it fails. "Never exposed through an ordinary memory type" then
means *never allocated from* one, not *never reported as compatible with* one.

**A9 — HVC1 `EngineAffinity=0` vs native-fence/monitored-fence
`EngineAffinity=1u<<0`.** §10.7:1704-1705 specifies context creation with
"zero-based `EngineAffinity=0`"; §10.7:1915 specifies the monitored progress
fence with `EngineAffinity=1u << 0`; §12.2:3207-3209 specifies native-fence open
with `EngineAffinity=1u << 0` and says "Zero, multiple bits, overflow, or a bit
outside the queried device topology is rejected before the call". These are
different structures with different documented semantics, so it is probably not
a contradiction — but it is exactly the kind of inconsistency an implementer
"harmonises". *Rule for the implementer:* transcribe each literally; never
propagate one value to the other site.

**A10 — "Preserve `vn_ring` for non-Helios builds" vs "Delete `vn_ring`".**
§17.3:4058-4059: "Preserve generic upstream ring code only for non-Helios
platform builds." Appendix A.4:5782 (last sentence): "Delete `vn_ring` and every
user-visible host resource ID or shared head/status sample." *Conservative
reading:* §17 is the implementation manifest and A.4 is a disposition summary;
keep `vn_ring.c`/`.h` compiled only when `!with_platform_windows`, and make
every Windows call site a compile error rather than a runtime branch (no
fallback between modes — §3:391-392, §17.3:4082).

**A11 — Where is the provisional HTS1 session created?** §10.4:1199-1202 says
the ICD "creates a raw KMT device, and creates one legacy nonvirtual HVC1
control context. KMD binds that raw device … **and creates a provisional,
refcounted `TranslationSession`**" — i.e. at device create. §10.7:1696-1700 says
"At `D3DKMTCreateDevice`, the KMD device object records the exact
`hKmdProcess`/adapter/package generation but **creates no host object
namespace**. … It first creates one ordinary control context; **that HVC1
context creates the provisional HTS1 session**". Reconcilable if "session
object" and "host namespace" are separated, but the two sentences attribute the
provisional-session creation to two different KMT calls. Immaterial to the ICD's
call order (device then context either way); material to the KMD lane. Reported
below.

**A12 — The buffer-resize `D3DKMTRender` conflicts with "all flags … zero".**
§10.7:1811-1814 requires the ordinary Render to have "all flags/history/broadcast
fields zero"; §10.7:1744-1750 requires the ICD to use `ResizeAllocationList` and
`ResizePatchLocationList` (which are `D3DKMT_RENDERFLAGS` bits) and to "adopt all
three returned pointers/sizes even on failure", with a transition that "carries
no application work". *Conservative reading:* the resize is a **separate**
`D3DKMTRender` whose only nonzero flags are the two resize bits, with
`CommandLength=0` and `AllocationCount=0`; ordinary Renders keep all flags zero.
This requires the KMD's `DxgkDdiRender` to accept a zero-length command (CROSS-LANE
REQUEST 3), which nothing in §10.7's KMD paragraph (1831-1850) allows for — it
begins by validating `CommandLength` against the advertised maximum and taking a
slot.

**A13 — Command-pool host synchronization across the app queue and the helper
queue is unspecified.** §10.7:2513-2515 gives each slot "one resettable
lower-Vulkan command pool with separate per-slot present-release and acquire
barrier command buffers". The acquire buffer is recorded and submitted on the
private helper queue (2622-2623); the release buffer is recorded and submitted
on the **application's** queue inside `vkQueuePresentKHR` (2660-2673). Vulkan
requires external synchronization of a `VkCommandPool` across all recording and
reset. §13.3:3398-3400 only says the layer "transitions state under a short
per-image lock" and §10.7:2603-2604 says "No mutex is held while waiting for
completion or across a D3D/DXGI call". *Conservative reading:* the per-slot lock
covers pool reset and both `vkBeginCommandBuffer`/`vkEndCommandBuffer` windows
for that slot; it is released before `vkQueueSubmit2` and before any D3D/DXGI
call. State that explicitly in the implementation, because "short per-image
lock" does not obviously cover command-pool recording.

**A14 — A native Vulkan process must host two `vn_instance`s in two modes
simultaneously, and nothing says how the direct-dispatch table and the loader
instance stay isolated.** §10.7:2489-2490 says the layer "creates a D3D12
device/queue and DXGI flip-model swapchain on the exact adapter. **D3D12 uses
the selected actual ordinary-context translator path**" — so a native Vulkan
app's Present drives UMD12 → vkd3d → a *record-only* `vn_instance` inside
`vulkan_virtio.dll`, while the app's own *normal-loader* `vn_instance` lives in
the same module. §10.4:1221-1223 and §17.3:4011-4012 confirm two sessions, two
host contexts, two namespaces. But nothing states that the ICD's process-wide
state (module pin, diag, TLS, `vn_renderer` globals — today
`vn_renderer_helios.c:3063-3077,5124-5141`) is safe under that, nor how the
record-only tag is prevented from leaking to the loader instance. *Conservative
reading:* every piece of ICD state that is not per-`vn_instance` today must
become per-`vn_instance` in this lane, and the record-only tag must be set only
on the instance created through the direct-dispatch entry point, never through
`vk_icdGetInstanceProcAddr`.

---

## CROSS-LANE REQUESTS

1. **Protocol lane (§17.1): emit a C header.** *(Partially in flight: the
   protocol lane has started `protocol/include/`, currently containing only
   `helios_diagnostics.h`. That is the right convention and directory — please
   extend it to the records below.)* This lane must parse HWA2 (168 B),
   HQA1 (72 B), HVC1 (32 B), HNR2 (112 B + 24 B use + 16 B patch), HVM1 (64 B),
   HVR1 (80 B) from C. Today the ICD hand-mirrors protocol structs with
   size-only `_Static_assert`s (`vn_renderer_helios.c:255-352`) and there is no
   generated header anywhere in the tree. Please emit one header (cbindgen or a
   checked-in generated file) with `offsetof` assertions, buildable by mingw
   gcc, and make it reachable from `icd/mesa/src/virtio/vulkan/` (an include dir
   in `meson.build` is fine — the ICD is a submodule, so a *copied* header would
   reintroduce exactly the two-repository hand-mirror hazard `CLAUDE.md` warns
   about).
2. **DXVK (§17.2) + UMD12/vkd3d (§17.5) lanes: agree the direct-dispatch ABI
   before A5.** One exported symbol from `vulkan_virtio.dll`, versioned,
   package-generation-checked, returning a const function table that includes
   translator-instance creation, the record-only tag, batch sealing, and the
   endpoint descriptor for HQA1. The header should live wherever the protocol
   header lands so all three repositories share one declaration. All ten current
   `helios_venus_*` name exports go away with it.
3. **KMD lane (§17.6): `DxgkDdiRender` must accept the buffer-resize Render.**
   Per A12, the ICD's `ResizeAllocationList`/`ResizePatchLocationList`
   transition is a `D3DKMTRender` with `CommandLength=0`, `AllocationCount=0`
   and no HNR2 payload. §10.7:1831-1850 as written validates `CommandLength`
   against a maximum and takes a slot; please make the zero-length resize
   Render an explicit accepted, counted, no-op arm rather than an error.
4. **KMD lane (§17.6): resolve A11** — is the provisional `TranslationSession`
   created at `DxgkDdiCreateDevice` (§10.4:1201) or at the HVC1
   `DxgkDdiCreateContext` (§10.7:1698-1700)? The ICD's call order is the same
   either way; the KMD's object lifetime is not.
5. **Packaging lane (§17.7): register `VkLayer_HELIOS_present` as an implicit
   layer** under `HKLM\SOFTWARE\Khronos\Vulkan\ImplicitLayers`, and settle the
   `disable_environment` key name with this lane (A7).
6. **Tools lane (§17.7): `tools/d3d11_kmt_shared_probe.cpp` and
   `tools/vk_ring_fence_probe.cpp`.** §17.3:3986 tells the Mesa lane to remove
   the first; both live outside this lane's scope and are already listed at
   §17.7:4530. This lane will not touch them.
7. **Docs (§17.7): record the Linux cross-build recipe** in `TOOLCHAIN.md`. The
   Mesa ICD cross-compiles to `vulkan_virtio.dll` on the Linux host with
   `x86_64-w64-mingw32-gcc` + a meson cross file + a `mako`/`packaging` venv
   (§5.1 above, verified today, 256/256 targets). It removes the VM from the
   ICD's inner loop and makes the §13.2 import-table gate a host command.
