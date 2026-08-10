# Lane: vkd3d-proton record-only + D3D12 UMD Core-0116 native fences and virtual submit

Reconnaissance brief. **No implementation code was written.** Source scope:
`vkd3d-proton-helios/**` and `umd12/**`.

Reference: `docs/HELIOS_PRESENT_SYNC_RETIREMENT.md` (**5973** lines, "the doc"
below).

⛔ **Doc line numbers here were swept +10 on 2026-08-10 and mechanically
verified.** The doc was 5918 when this brief was written, 5928 after commit
`c17f17c` inserted a ten-line banner at `:11-20`, and **5973** after `afebe66`
**appended** the SUPERSEDED CLAIMS INDEX at `:5932`. Only the banner moved
anything: it shifted every body line after 8 by +10, so every citation written
here before 2026-08-10 was 10 too low. All of them have now been shifted, and
the sweep was checked by requiring each `§N.M`-anchored cite to land inside
§N.M's own heading-to-heading range — not by adding 10 on faith. ⚠ **Source-file line numbers were deliberately NOT touched** — `foo.rs:123`, `virtio-gpu-virgl.c` function ranges and the like are cites into the tree, not into the doc, and the banner never moved them. If a number here is not a doc line, it was correct before this sweep and is correct now. ⚠ The §10.3
bullet in §1 and its `§17.5:4259-4263` cite were already corrected on 2026-08-10
and were **not** shifted again. Re-grep the cited text before relying on any
number below, and read the SUPERSEDED CLAIMS INDEX at `:5932` before treating
any doc line as a requirement.
Repo state read at root `d1c820a`, submodule `vkd3d-proton-helios` = `f3918d5e`
(matches the doc's §1 provenance table, line 85).

---

## 1. Normative sources

| Section | Lines | What it contributes to this lane |
|---|---:|---|
| §2 Executive decision | 122-368 | *Why*: items 1, 2, 8 are this lane's charter — translated work becomes actual WDDM work; D3D12 fences use Core-0116 native identity; translators bypass the Vulkan loader through a private direct-dispatch entry point. Line 319-352 is the all-or-nothing version/feature list (build 28000, WDDM 3.2, Core 0116, `D3D12DDICAPS_TYPE_0112_NATIVE_FENCE_SUPPORT`, `DXGK_FEATURE_NATIVE_FENCE`, **traditional kernel submission — no HWS/HWQueue**, every fence `NATIVE`/`OPENED_NATIVE`). |
| §3 Hard constraints | 369-406 | The bounds on every design choice: no Escape, no global registry/service/polling/sleep, **no semaphore-only or delayed-completion command may claim `WrittenPrimaries`**, no ICD→DXGI/D3D11/D3D12 call, no CPU wait on GPU completion in steady state, no cross-adapter inference, **no version or feature fallback**. |
| §10.4 Record-only translation and actual WDDM submission | 1190-1430 | ⭐ The lane's primary ABI section. HTS1 pre-queue control; the **72-byte HQA1** create-context packet (byte table at 1230-1246); the **112-byte HOB1** header (1267-1286) plus its **40-byte use records** and **16-byte typed operands** (1288-1306); the **64-byte HOS1** private-data descriptor (1316-1330); the five-point record-only submit contract (1383-1395); the "no queue-work" rule and the queue-entry-point refusal list (1397-1406). |
| §10.6 D3D12 execution, fences, and Present | 1464-1684 | ⭐ The lane's primary behavioural section. Queue creation (5 steps, `pfnCreateContextVirtualCb` + HQC1); the **64-byte HOC1** record and the C65 command-buffer pool rules (1487-1542); ExecuteCommandLists (6 steps, 1544-1586); Queue Wait/Signal on Core-0116 native fences (1594-1638); tile mapping (1640-1646); D3D11On12 (1648-1664); Present (1666-1683). |
| §11.1 Components and object graph | 2884-2944 | The object graph this lane must produce, and the translation-session/outer-attach sequence diagram (2920-2943). |
| §11.2 Composed D3D12 FLIP sequence | 2945-2979 | The end-to-end call order the ECL/Present implementation must match. |
| §11.5 Direct/independent transition | 3051-3076 | Confirms nothing in this lane changes on promotion/demotion: "promotion/demotion changes consumer, not producer identity". |
| §11.6 Required ten answers | 3077-3106 | The ten answers the finished lane must be able to give; answers 3, 5, 6 and 8 are its own. |
| §12.1 Core-0116 native-fence contract | 3130-3196 | ⭐ The **64-byte HNF1** `HeliosNativeFencePddV1` byte table (3136-3146) and the seven-step create/open/wait/signal/CPU/close/multi-adapter contract. |
| §12 object table rows | 3113, 3114, 3115, 3118 | The four rows that are literally this lane's objects: D3D12 virtual context/submission, D3D12 HOB1 GPUVA pool, D3D12 native fence, outer translated progress HQC1. Read them as the lifetime spec. |
| §17.5 vkd3d-proton and D3D12 UMD | 4151-4264 | ⭐ The file manifest and the required result, including the exact lower-queue callsite audit list and the Wine-Escape/`SharedGpuResource` deletions. |
| §18.2 Queue and Present causal gates | 4783-4974 | The acceptance suite. Lines 4867-4900 (record-only `vn_instance`), 4902-4921 (per D3D queue), 4923-4970 (D3D12-specific) are the pass criteria this lane is graded on. |
| Appendix A.2/A.4 | 5696, 5735, 5736, 5760, 5777, 5778, 5779 | The current-state dependency rows naming exactly which lines of `umd12/` and `vkd3d-proton-helios/` are live and what must happen to them. |
| Appendix B.3 | 5846-5871 | The current (broken) D3D12 producer→DWM sequence, and the sentence that names the gap: "UMD12/vkd3d has no live HPS2 writer… it does not turn that work into the runtime context's DMA submission". |

**Companion sections that are normative for this lane but were NOT in the assigned
ranges** — read them before touching the named file:

* **§10.3, lines 1014-1189** (⚠ **+10 vs. what this brief originally said** —
  commit `c17f17c` inserted a ten-line banner at
  `HELIOS_PRESENT_SYNC_RETIREMENT.md:11-20`, so *every* doc line number written
  in this brief before 2026-08-10 is 10 too low; re-grep before citing. The HWA2
  byte table is **1043-1069**) — the `HeliosWddmAllocationDescV2` (`HWA2`)
  168-byte create-time descriptor. `resource12.rs` is in this lane's manifest
  and its `pfnAllocateCb` path supplies this record.
  ⛔ **Corrected 2026-08-10 per `docs/retirement/K4-CONTRACT.md` §1: the UMD does
  not *write* HWA2 — it supplies a create-*input* HWA2 and the KMD performs the
  write of all 168 output bytes.** The split matters at three fields the UMD
  must leave **zero** on input or the create is refused: `allocation_generation`
  (offset 16), `flags & DIRECT_FLIP_COMPATIBLE` and `flags &
  D3D12_RUNTIME_PRIMARY` (offset 68). Everything else the UMD supplies is
  validated and echoed verbatim — the KMD refuses rather than correcting a
  field, so a `byte_size` or geometry the UMD guesses is a create failure, not a
  silent fix-up. Validate against `Hwa2Stage::CreateInput` /
  `HeliosWddmAllocationDescV2::validate_create_input` in `protocol/src/wddm.rs`.
  ⛔ And HWA2 carries **no host resource id and no Vulkan memory-type index**
  (K4-CONTRACT §5): `resource12.rs`'s current reads of the retired 48-byte
  `HeliosWddmOpenIdentity` have no successor field, so they must fail loudly
  with a named counter rather than be re-pointed at an HWA2 field that does not
  exist. §17.5:4259-4263 also makes HWA2 the replacement for vkd3d's deleted
  Wine metadata.
* **§17.1, lines 3748-3809** — where HOB1/HOS1/HOC1/HQA1 must be *declared*
  (`protocol/`). This lane consumes those declarations; it may not author them.
* **§18.1, lines 4646-4782** — the static/build gates, notably 4639-4642
  (WDK-28000 bindings expose Core 0116 exactly) and 4649-4652 (a record-only
  translator cannot reach any lower `QueueSubmit{,2}` / `QueueBindSparse` /
  `QueuePresentKHR` / queue-idle path).

---

## 2. Current source inventory

### 2a. `vkd3d-proton-helios/` — manifest §17.5 lines 4153-4168

Submodule commit `f3918d5e`, branch `helios`. Divergence from the upstream fork
point (`git -C vkd3d-proton-helios log --oneline 2c7ba22c..HEAD`, run 2026-08-09,
9 commits — do **not** copy that count anywhere, re-run it):

```
f3918d5e vkd3d: support Time Spy heap and hull shader paths
21c0fa86 vkd3d: export committed buffers for Helios WDDM adoption
9410a5b3 vkd3d: VKD3D_HEAP_FLAG_HELIOS_VENUS_EXPORT ...
4c26d855 vkd3d: ID3D12DXVKInteropDevice4::GetVulkanResourceMemoryInfo ...
8ee4440b d3d12core: split debug_control out of main.c ...
7c00173d meson: add helios_d3d12_static, the shipping arm
fd205b2c tests: add -x/--exclude to test-runner.sh ...
e571d71a tests: make test-runner.sh work under a native Windows bash
fc35d37d helios: DXGI-free device + root-signature exports ...
```

| File | Lines | What it does today | Verdict |
|---|---:|---|---|
| `libs/d3d12core/helios_entry.c` | 197 | `helios_vkd3d_create_device` — builds `vkd3d_device_create_info` with `vk_physical_device = VK_NULL_HANDLE` (`:180`, self-documented at `:172-179` as **not LUID matching**) and `parent = NULL` (no `IDXGIAdapter`). Also `helios_vkd3d_serialize_root_signature`. | **MODIFY** — A.3 row 5760: select the lower physical device by exact runtime adapter LUID + device/driver UUID; a mismatch is fatal (§3 "no cross-adapter inference"). Also the site where the record-only mode flag and the HTS1 session must be established (§17.5:4216-4218, "During `vkd3d_create_device`, direct Mesa creates exactly one HTS1 session for that `vn_instance` before any D3D12 command queue exists"). |
| `libs/vkd3d/command.c` | 26534 | Queue/list implementation. Direct lower-queue execution at the exact lines §17.5:4209 cites — **all six verified present at the pinned SHA**: `:291`, `:23670`, `:23889`, `:24264`, `:24629`, `:25612` are `vkQueueSubmit2`; `:25099` is `vkQueueBindSparse`. `:24340-24670` is queue admission; `:25179-25364` is the immutable worker state (A.2 row 5736). `d3d12_command_queue_acquire_serialized` at `:25202-25217` is the untimed drain the UMD currently uses. | **MODIFY (XL)** — every one of those becomes sealed outer work, a C34 WDDM paging operation, or a capability refusal. Queue Wait/Signal must emit **no** Vulkan operation (§10.6 step 5, line 1611). |
| `libs/vkd3d/device.c` | 12145 | `vkd3d_create_device`, physical-device selection (`vkd3d_select_physical_device` at `:3491-3573`), and the shared-metadata callers at `:7682` (`vkd3d_set_shared_metadata`) and `:7797` (`vkd3d_get_shared_metadata`) — both verified. | **MODIFY (L)** — delete both metadata callers; expose the stable bounded physical-lower-queue endpoint per logical command queue (§17.5:4218-4222); create the HTS1 session in `vkd3d_create_device`. |
| `libs/vkd3d/d3dkmt.c` | 449 | `:145-195` is the `D3DKMT_ESCAPE_UPDATE_RESOURCE_WINE` stamp (verified: `:145` is `if (resource->kmt_local)`, `:148` builds `D3DKMT_ESCAPE escape = {0}`). | **MODIFY (S)** — delete the Escape block (§17.5:4256-4257). §18.1 requires **zero** live outbound `D3DKMTEscape` calls/imports across shipped vkd3d. |
| `libs/vkd3d/shared_metadata.c` | 68 | The entire `\\.\SharedGpuResource` IOCTL implementation; `:24-26` define `IOCTL_SHARED_GPU_RESOURCE_{SET,GET}_METADATA`/`_OPEN`. | **DELETE** (§17.5:4257, A.2 row 5708). |
| `libs/vkd3d/resource.c` | 11449 | `:3796` `vkQueueBindSparse` (verified). `:189`, `:762-764`, `:4454-4457`, `:4572` are the `VKD3D_HEAP_FLAG_HELIOS_VENUS_EXPORT` chain. | **MODIFY (L)** — sparse becomes a WDDM page-table operation (§10.6:1644-1646); the venus-export chain is **deleted** with the adoption direction (A.4 row 5779). |
| `libs/vkd3d/heap.c` | 440 | `:340` consumes `VKD3D_HEAP_FLAG_HELIOS_VENUS_EXPORT`. | **MODIFY (S)** — delete the private-bit branch. |
| `libs/vkd3d/memory.c` | 2277 | `:458` is a `vkQueueSubmit2` transfer clear (verified). | **MODIFY (S)** — recorded into the owning ECL batch or refused. |
| `libs/vkd3d/vulkan_procs.h` | 438 | The Vulkan entry-point table the engine dispatches through. | **MODIFY (S)** — the direct-dispatch/record-only surface (§2 item 8: translators bypass the loader). |
| `libs/vkd3d/vkd3d_private.h` | 7197 | `:1088-1119` declares `VKD3D_HEAP_FLAG_HELIOS_VENUS_EXPORT ((D3D12_HEAP_FLAGS)(1u << 30))` (**verified at `:1119`**, with the hand-mirror warning at `:1091-1118`). `:3727-3818` immutable worker state. `:7166-7167` declares `vkd3d_{set,get}_shared_metadata` (verified). | **MODIFY (M)** — delete the private heap bit and both metadata declarations. |
| `libs/vkd3d/meson.build` | 126 | `:111-113` adds `shared_metadata.c` on the Windows platform arm (verified). | **MODIFY (S)** — drop the source; also the site to prove `swapchain.c`'s Vulkan WSI path unreachable in the private Windows UMD build (§17.5:4214). |
| `include/private/vkd3d_d3dkmt.h` | 381 | `:120-122` declares `D3DKMT_ESCAPE_UPDATE_RESOURCE_WINE = 0x80000000` (verified). | **MODIFY (S)** — delete the private enum. |
| `libs/vkd3d/swapchain.c` | 4179 | `:398`, `:2375`, `:2719` are `vkQueueSubmit2`; `:3112` is `vkQueuePresentKHR` (**all four verified**). | ⚠ **NOT IN THE MANIFEST'S MODIFY LIST** but named in the audit paragraph (§17.5:4211-4214). See §6 item 12 for the fail-closed reading. |

### 2b. `umd12/` — manifest §17.5 lines 4170-4201

| File | Lines | What it does today | Verdict |
|---|---:|---|---|
| `bridge/vkd3d_bridge.cpp` | 1238 | The cxx bridge to the statically linked engine. A.4 row 5777 names `:306-525` and `:669-780` as loaded-module discovery of thread/process Venus context, raw memory ID/allocation metadata exports, and sampled lower-queue wire fences, with graceful old-ICD degradation. | **REWRITE (L)** — delete raw-ID/export resolution, queue drain/sample, and every graceful old-ICD branch. Replace with package-matched direct record-only dispatch returning sealed batch bytes + resource-use table. Any generation mismatch fails device creation. |
| `bridge/vkd3d_bridge.h` | 247 | The C++ side of the same surface. | **REWRITE (M)** — same. |
| `src/bridge12.rs` | 720 | The cxx bridge declarations. `ResourceVenusIdentity` (`:417`), `IdentityStatus` (`:460-511`), `drain_queue` (`:570`), `drain_queue_with_fence` (`:649`), `sample_queue_fence` (`:697`). A.4 row 5777 names `:280-480` and `:630-720`. | **REWRITE (L)** — §17.5:4249-4252: `bridge12.rs` exposes **only** bounded pure-control and HQC1-join callbacks to the record-only Mesa instance. `drain_queue*`/`sample_queue_fence`/`ResourceVenusIdentity` all go. |
| `src/forward12/identity12.rs` | 419 | Process-global `OnceLock<Mutex<IdentityRegistry>>` (`:310-318`) keyed by engine COM address, second index by raw `venus_res_id`; `record`/`lookup`/`take` at `:340`/`:398`/`:414`. | **DELETE** — §17.5:4175-4177 and A.4 row 5778 are explicit: delete the registry, the raw-ID index, the private `1 << 30` export/adopt flag, replacement-on-address-reuse, and its counters. Its module declaration in `forward12/mod.rs` goes with it. |
| `src/forward12/queue.rs` | 4925 | ⭐ The lane's centre of mass. `create_command_queue` (`:1069`) → `create_wddm_context` (`:1236`) mints a **legacy** context via corelayer `pfnCreateContextCb` and stores the three `ContextWindows` (`:452-533`). `submit_wddm_render` (`:2773-2905`) is the **only** `pfnRenderCb` call site; `ecl_submit_command` (`:2664`) builds the 16-byte `HeliosD3D12SubmitCmd`. `execute_command_lists` (`:3108-3548`). `fence_operation` (`:3709`) + `signal_fence` (`:3923`) / `wait_for_fence` (`:3974`) forward to the **engine shadow** fence with a watermark gate. `update_tile_mappings` (`:3580`) / `copy_tile_mappings` (`:3601`) refuse. A.2 row 5735 names `:2589-2604,2700-2905,2931-3497`. | **REWRITE of the submission spine, MODIFY elsewhere (XL)** — replace `pfnCreateContextCb` + three windows + `pfnRenderCb` with `pfnCreateContextVirtualCb` + HQA1 private data + HQC1 + ordinary `pfnSubmitCommandCb` (§17.5:4222-4228). `submit_wddm_render` and the `HeliosD3D12SubmitCmd` packet are deleted. `fence_operation` becomes the exact-context `pfnWaitForSynchronizationObjectFromGpuCb` / `pfnSignalSynchronizationObjectFromGpuCb` calls of §12.1 steps 3-4. Tile mapping becomes `pfnUpdateGpuVirtualAddressCb` (§10.6:1640-1643). |
| `src/forward12/command_pool12.rs` | — | **Does not exist.** | **ADD (L)** — C65's HOC1 64-MiB pool, extent allocator + seal metadata, per-owner HQC1 retirement, event-backed backpressure, poisoning, teardown. Export from `forward12/mod.rs`. |
| `src/forward12/fence.rs` | 952 | `FenceState` (`:297`) holds an engine `ID3D12Fence` shadow plus a signalled watermark and `driver_signals_issued`. `create_fence` (`:493`) reads only `{FenceCount, Fences}`; the module doc (`:37-54`) records the measured `valueVA=0x0 monitoredVA=0x0` and the (then-correct) conclusion that the driver can never name a D3D12 fence to the kernel. Also owns 3 query-heap slots (`:691-813`). A.2 row 5735 names `fence.rs:1-885`. | **REWRITE (L)** — Core-0116 `NATIVE` / `OPENED_NATIVE` only; store `HRTFENCE`, local `hSyncObject`, `D3DDDI_NATIVEFENCEMAPPING`, HNF1; fail-closed on a `MONITORED` branch (§10.6:1635-1638); fail-closed destruction (§12.1 step 6). ⚠ The query-heap half is unaffected and must survive the rewrite. |
| `src/forward12/resource12.rs` | 4523 | Committed-resource creation calls `pfnAllocateCb` at `:1936` (UMD-**adopted** backing; the `HELIOS_HEAP_FLAG_VENUS_EXPORT` mirror is at `:222`, set at `:788` and `:2317`). `open_heap_and_resource` (`:3382`) **refuses** with `L4_REFUSALS.open_heap_and_resource_refused`. A.2 row 5696 names `:90-133,1450-1540,1583-2040,3308-3394`; A.4 row 5778 names `:193,222,765-797,1618-2089,2956-2968,3884-3935`. | **MODIFY (L)** — flip `pfnAllocateCb` from UMD-adopted to **KMD-created** backing carrying HWA2 (§10.3); delete `HELIOS_HEAP_FLAG_VENUS_EXPORT` and every use; **implement** `pfnOpenHeapAndResource` (the D3D11-created→D3D12 direction; A.2 row 5696 warns it is *not* DWM's D3D12-open carrier). |
| `src/forward12/present12.rs` | 760 | `get_present_private_driver_data_size` (`:112`) returns **0**. `present` (`:163-575`) resolves `queue::present_context`, calls `queue::submit_present_identity` (the UP-9 `HeliosPresentRenderCmd` Render packet), then fills the out-structs. A.2 row 5735 names `:88-118,220-570`. | **MODIFY (M)** — §17.5:4188-4189: "ordinary Present with exact source/destination allocation and truthful fields; no Present-HWQueue path". The `submit_present_identity` seam disappears with the legacy context (see §6 item 7). |
| `src/forward12/misc.rs` | 2639 | `pfnCalcPrivateSchedulingGroupSize` / `pfnCreateSchedulingGroup` / `pfnDestroySchedulingGroup` at `:2043-2045`; `create_scheduling_group` already **refuses** with `L9SchedulingGroupRefused` (`:632-667`). | **MODIFY (S)** — §17.5:4190 "remove/refuse scheduling-group/HWS paths". Already refuses; verify the refusal survives, and confirm no HWS cap is advertised anywhere (§10.6 step 5, line 1474-1475). |
| `src/caps12.rs` | 2473 | 43 `D3D12DDICAPS_TYPE` answers off the adapter table; `options_0110` at `:1544`. | **MODIFY (L)** — report `D3D12DDICAPS_TYPE_0112_NATIVE_FENCE_SUPPORT` truthfully; one physical node; **no** LDA, **no** cross-adapter resource/fence support (§12.1 step 7). |
| `src/device12.rs` | 609 | `HeliosD3D12Device` (`:58`) already retains `kt_callbacks` and `um_callbacks`; `create_device` (`:217`) calls `BridgeDevice12::create(0, 0)` at `:308` — "(0,0) means do not match on LUID" (A.3 row 5760). | **MODIFY (M)** — retain `pKTCallbacks` + the Core-0116 callbacks + exact virtual-context handles; pass a real adapter LUID down. |
| `src/lib.rs` | 648 | Module list (`:83-90`), `DllMain` (`:636`). | **MODIFY (S)** — module list follows `identity12` deletion / `command_pool12` addition. |
| `src/probe12.rs` | 355 | Six `#[unsafe(no_mangle)]` `helios_umd12_probe_*_v1` test exports. | **MODIFY (M)** — remove stale vehicle/mutable-open-identity probes; add probes for HOB1/HOS1/HQA1/HNF1 and the C65 pool that §18.2's gates can drive. |
| `build.rs` | 426 | bindgen against `HELIOS_WDK_INCLUDE` default `…\Windows Kits\10\Include\10.0.26100.0` (`:113-115`); `compare_or_refresh_cache` (`:207`) warns (does not fail) when the committed cache drifts; `build_vkd3d_bridge` (`:229+`) links `libhelios_d3d12_static.a` + `gdi32`. | **MODIFY (S)** — retarget the SDK include to 10.0.28000.0 and narrow the allowlist to the implemented DDI versions. ⛔ Do **not** weaken `layout_tests(true)`. |
| `Cargo.toml` | ~150 | `helios_protocol`, `helios_umd_common`, `cxx`, `windows 0.58` (4 features), `bindgen`, `cxx-build`. | **MODIFY (S)** — retain the `helios_protocol` dependency (it is where HOB1/HOS1/HOC1/HQA1 must be declared); update the `OpenAdapter12`-refusal note. |
| `bindgen/d3d12umddi_wrapper.h` | ~40 | Includes `<d3d12umddi.h>`; banner says "Built against SDK 10.0.26100 um+shared". | **MODIFY (S)** — retarget the banner; the header include itself is unchanged. |
| `bindgen/cached/d3d12umddi.rs` | 102874 | ⛔ **Generated from SDK 10.0.26100 and tops out at `D3D12DDI_DEVICE_FUNCS_CORE_0109` / build token `_0110`.** No `pfnCreateFence_0116`, no `D3D12DDI_FENCE_TYPE`, no `pNativeFenceArgs`, no `pfnCreateNativeFenceCb`/`pfnOpenNativeFenceCb`, no `D3D12DDICAPS_TYPE_0112_NATIVE_FENCE_SUPPORT`. The D3DDDI-side kernel types **are** present: `_D3DDDICB_CREATECONTEXTVIRTUAL` (`:21437`, 40 bytes), `_D3DDDICB_SUBMITCOMMAND` (`:21577`, with `NumPrimaries` at offset 548 and `WrittenPrimaries: [D3DKMT_HANDLE; 16]` at 552), `pfnWaitForSynchronizationObjectFromGpuCb` / `pfnSignalSynchronizationObjectFromGpuCb` / `pfnWaitForSynchronizationObjectFromCpuCb` / `pfnCreateSynchronizationObject2Cb` (`:22772-22795`), `_D3DDDI_NATIVEFENCEINFO` (`:6799`), `_D3DKMT_OPENNATIVEFENCEFROMNTHANDLE` (`:30900`). | **REGENERATE (BLOCKING)** — see §6 item 1. |

### 2c. Files this lane must change that the manifest does **not** name

| File | Lines | Why it must change |
|---|---:|---|
| `umd12/src/adapter12.rs` | 600 | Owns `SUPPORTED_DDI_VERSIONS` (`:97-99`, a **single** entry, `INTERFACE_VERSION_R8` + `BUILD_VERSION_0110`) and the closed `Ddi12Interface::R8_0110` set (`:110-120`), plus `get_supported_versions` (`:382`). §17.5 assigns "negotiate Core 0116" to `caps12.rs`, but the token lives here. |
| `umd12/src/forward12/mod.rs` | 72 | §17.5:4181 itself says "export it from `forward12/mod.rs`" for `command_pool12`, and the `identity12` declaration (`:71`) must be removed — yet the file is not in the manifest list. |
| `umd12/src/forward12/tables12.rs` | 449 | Pins the three driver-side table types at `:67-71`: `DEVICE_FUNCS_CORE_0109` (124), `COMMAND_LIST_FUNCS_3D_0108` (75), `COMMAND_QUEUE_FUNCS_CORE_0001` (7). Any 0116 table-shape change lands here. |
| `umd12/src/forward12/noop12.rs` | 425 | Compile-time asserts 124 + 75 + 7 = 206 slots and the ABI *order* of every slot name against the bindgen structs (`:90-140`). A regenerated header that adds or reorders a slot breaks this file first. |
| `umd12/src/knobs12.rs` | 627 | Owns `Umd12EclSubmit`, `Umd12EclDrain`, `Umd12EclFence`, `Umd12EclSubmitStrict`, `Umd12EclDelayUs`, `Umd12FenceSignalDelayUs`. Every one of them describes the retired `pfnRenderCb`/engine-shadow topology. A.4 row 5780 states the rule: "no old knob may select an old path." |

---

## 3. Work decomposition

Units are ordered. **U0 is a hard prerequisite for everything that follows.**
Files listed under "owns" are exclusive to that unit; a file appearing in two
units means those two units are **serialized**, and that is called out.

| id | goal | owns (exclusive) | depends on | size |
|---|---|---|---|:--:|
| **U0** | Regenerate the D3D12 DDI bindings from WDK 28000.2526 so Core 0116 types exist and the Linux host cross-check can type-check the rest of the lane. | `umd12/bindgen/cached/d3d12umddi.rs`, `umd12/bindgen/d3d12umddi_wrapper.h`, `umd12/build.rs` | ⛔ **A Windows-VM action** (WDK 28000 install + `win_cargo`). Nothing else in the lane compiles first. | M |
| **U1** | Consume the protocol declarations: bring HQA1/HOB1/HOS1/HOC1/HNF1 into `umd12` as `helios_protocol` imports and write the encode/validate helpers (no DDI wiring yet). | `umd12/src/forward12/wire12.rs` (new) | U0; **protocol lane** must land §17.1's `translation_session.rs` + `wddm.rs` rewrite + the HNF1 home (§6 item 10) | M |
| **U2** | vkd3d: excise the forbidden transports — Wine Escape, `\\.\SharedGpuResource`, and the private venus-export heap bit. | `libs/vkd3d/d3dkmt.c`, `libs/vkd3d/shared_metadata.c` (delete), `include/private/vkd3d_d3dkmt.h`, `libs/vkd3d/meson.build`, `libs/vkd3d/heap.c` | none (self-contained; verifiable on Linux) | M |
| **U3** | vkd3d: record-only submission mode — the private direct-dispatch interface, `HELIOS_TRANSLATOR_SUBMISSION_MODE_RECORD_ONLY`, batch sealing, and the per-logical-queue physical endpoint. | `libs/vkd3d/command.c`, `libs/vkd3d/vkd3d_private.h`, `libs/vkd3d/vulkan_procs.h`, new `libs/vkd3d/helios_record.{c,h}` | U2 (shares `vkd3d_private.h`); **ICD lane** for the private table's shape (§6 item 9) | XL |
| **U4** | vkd3d: the non-queue lower-execution callsites — sparse binding, transfer clears, swapchain unreachability proof, LUID-exact physical-device selection. | `libs/vkd3d/resource.c`, `libs/vkd3d/memory.c`, `libs/vkd3d/device.c`, `libs/d3d12core/helios_entry.c` | U3 (shares nothing textually, but the record-only mode must exist to route into) | L |
| **U5** | UMD12 bridge: replace the discovery/drain/sample surface with package-matched record-only dispatch. | `umd12/bridge/vkd3d_bridge.cpp`, `umd12/bridge/vkd3d_bridge.h`, `umd12/src/bridge12.rs` | U1, U3 | L |
| **U6** | UMD12: delete the process-global identity registry and move per-resource WDDM state into the owning DDI objects. | `umd12/src/forward12/identity12.rs` (delete), `umd12/src/forward12/mod.rs`, `umd12/src/lib.rs` | ⚠ **shares `mod.rs`/`lib.rs` with U8** — serialize U6 before U8 | M |
| **U7** | UMD12: `resource12.rs` — KMD-created backing + HWA2, drop the venus-export flag, implement `pfnOpenHeapAndResource`. | `umd12/src/forward12/resource12.rs` | U1, U6; **KMD lane** for HWA2 write-back | L |
| **U8** | UMD12: the C65 HOB1 GPUVA pool. | `umd12/src/forward12/command_pool12.rs` (new), `umd12/src/forward12/mod.rs` | U1; ⚠ serialized after U6 on `mod.rs`; **KMD lane** for real GPUVA (§6 item 8) | L |
| **U9** | UMD12: the queue spine — `pfnCreateContextVirtualCb` + HQA1 + HQC1, ECL writes HOB1 into the pool extent and calls `pfnSubmitCommandCb` with 64 HOS1 bytes, exact-context fence wait/signal, `pfnUpdateGpuVirtualAddressCb` tile mapping. | `umd12/src/forward12/queue.rs`, `umd12/src/knobs12.rs` | U1, U5, U8; **U10 shares nothing but must land in the same changeset** (queue.rs calls into fence.rs state) | XL |
| **U10** | UMD12: Core-0116 native fence objects — `NATIVE`/`OPENED_NATIVE`, `HRTFENCE`, local `hSyncObject`/mappings, HNF1 validate, fail-closed destroy; preserve the query-heap half. | `umd12/src/forward12/fence.rs` | U0, U1; must be co-committed with U9 (the queue's wait/signal reads `FenceState`) | L |
| **U11** | UMD12: version + caps — advertise Core 0116, truthful native-fence caps, one node, no LDA/cross-adapter. | `umd12/src/adapter12.rs`, `umd12/src/caps12.rs`, `umd12/src/device12.rs`, `umd12/src/forward12/tables12.rs`, `umd12/src/forward12/noop12.rs` | U0 | L |
| **U12** | UMD12: Present + HWS removal. | `umd12/src/forward12/present12.rs`, `umd12/src/forward12/misc.rs` | U9 (present resolves the queue's virtual context) | M |
| **U13** | UMD12: probes/exports for §18.2's gates. | `umd12/src/probe12.rs`, `umd12/Cargo.toml` | U9, U10 | M |

**Shared-file serializations inside this lane, stated once:**

* `libs/vkd3d/vkd3d_private.h` — **U2 and U3**. Land U2 first.
* `umd12/src/forward12/mod.rs` and `umd12/src/lib.rs` — **U6 and U8**. Land U6 first.
* `umd12/src/forward12/queue.rs` — U9 only. Nothing else may touch it; it is
  4925 lines and the single densest file in the lane.
* `umd12/src/forward12/fence.rs` and `queue.rs` — separate files, but the
  wait/signal contract spans both. **One changeset, two files.**
* `umd12/src/forward12/tables12.rs` + `noop12.rs` — U11 only. `mod.rs`'s own
  doc calls these the *spine* and says no lane edits them; U11 is the exception
  and must say so in its commit message.

---

## 4. Shared-file hazards (other lanes)

| File | Other lane(s) that touch it | How to serialize |
|---|---|---|
| `protocol/src/wddm.rs`, `protocol/src/translation_session.rs` (new) | **Protocol lane (§17.1)** — sole author. This lane is a pure consumer. | ⛔ This lane must **never** edit `protocol/`. HQA1/HOB1/HOS1/HOC1 (and HNF1, §6 item 10) land there first; U1 blocks on that. |
| `umd_common/` (`slot.rs`, `refusals.rs`, `window.rs`, `throttle.rs`, `hr.rs`, `bridge/bridge_icd_anchor.cpp`) | **D3D11 UMD lane (§17.4)** — `umd/` shares every one of these. `umd12/build.rs` compiles `../umd_common/bridge/bridge_icd_anchor.cpp` into *both* cdylibs. | `helios_umd_common::window::Window` becomes dead for D3D12 once the legacy context goes (only `pfnRenderCb` users need it) — but D3D11 still needs it (§10.5 keeps the physical Render context). ⛔ Do not delete `umd_common/src/window.rs`; just stop importing it in `queue.rs`. Any *other* `umd_common` change must be requested from the D3D11 lane. |
| `kmd_render/src/ddi/submit_command.rs`, `.../create_allocation.rs`, `.../gpummu.rs`, `.../scheduler.rs`, `.../interrupt.rs` | **KMD lane (§17.6)** | This lane never edits `kmd_render/`. It *depends* on: HQA1 validation at `DxgkDdiCreateContext`, `DxgkDdiSubmitCommandVirtual` GPUVA enqueue, `DmaBufferPrivateDataSize` ≥ 64 + KMD suffix, HOC1 admission, real GPUVA/page tables, `DxgkDdiCreateNativeFence`/`OpenNativeFence`, and HWA2 create-time write-back. All are CROSS-LANE REQUESTS. |
| `icd/mesa/src/virtio/vulkan/vn_helios_translation_session.c` (new), `vn_queue.c`, `vn_device.c` | **Mesa/ICD lane (§17.3)** | The private direct-dispatch table's name, header path and byte layout are the ICD lane's to define and this lane's to consume. It is **unspecified in the doc** (§6 item 9) — do not invent it unilaterally. |
| `dxvk-helios/` | **DXVK lane (§17.2)** | Shares the same record-only Mesa contract and the same `HELIOS_TRANSLATOR_SUBMISSION_MODE_RECORD_ONLY` constant. Whatever U3 defines for vkd3d must be identical for DXVK — coordinate through the protocol/ICD lane, not through a second copy. |
| `vkd3d-proton-helios/libs/vkd3d/resource.c`, `heap.c`, `vkd3d_private.h` | Nobody else — but the `VKD3D_HEAP_FLAG_HELIOS_VENUS_EXPORT` value is mirrored **across repositories** in `umd12/src/forward12/resource12.rs:222`. | U2 (vkd3d side) and U7 (umd12 side) must be **one atomic change**, or the adoption path breaks in between. See §6 item 4. |
| `tools/umd12-host-check.sh`, `tools/umd-check.ps1` | Shared harness. | Read-only for this lane; if the cargo `--config` overrides need to change after U0, request it. |

---

## 5. Build and verification

### Verified working **today**, on Linux, no VM

| What | Command | Result (run 2026-08-09) |
|---|---|---|
| umd12 type-check (all 214 DDI slots + 1904 bindgen layout asserts, cross-compiled to `x86_64-pc-windows-msvc` against the **cached** bindings) | `tools/umd12-host-check.sh --message-format short` | ✅ `Finished dev profile in 4.76s`, one expected warning: *"HOST CROSS-CHECK — using bindgen/cached/d3d12umddi.rs, not the SDK header."* |
| umd12 lint (the §10 per-lane merge bar) | `tools/umd12-host-check.sh --clippy -- -D warnings` | not run in this recon; the script exists for it |
| **vkd3d fork native Linux build** — CLAUDE.md's claim, **CONFIRMED** | `ninja -C /home/rupansh/helios-vgpu/vkd3d-proton-helios/build-native-codex` | ✅ 89/89 targets, including `libhelios_d3d12_static.a`, `helios_vkd3d.so`, `tests/d3d12`, `demos/triangle` |
| vkd3d fresh configure | `cd vkd3d-proton-helios && meson setup build-native-codex` (native gcc; `enable_tests=true`, `enable_extras=true`) | build dir already present and reconfigures |
| vkd3d toolchain presence | `meson` `/usr/bin/meson`, `ninja` `/usr/bin/ninja`, `widl` `/usr/bin/widl`, `glslangValidator` `/usr/bin/glslangValidator`, `glslang` `/usr/bin/glslang` | ✅ all present |
| vkd3d nested submodules | `git -C vkd3d-proton-helios submodule status` | ✅ all three checked out (`SPIRV-Headers`, `Vulkan-Headers`, `dxil-spirv`) |
| vkd3d test suite | `vkd3d-proton-helios/tests/test-runner.sh` (fork-added `-x/--exclude`) | present; needs a Vulkan device — not run in recon |

⇒ **Every vkd3d change in U2/U3/U4 is verifiable on the Linux host with no VM and
no WDK.** That is the lane's fast inner loop and it should be used relentlessly.

### VM-only (⛔ this recon did not and must not run these)

* `tools\umd-check.ps1 -Mode release -Crate umd12` — the shipping umd12 build
  (clang-cl + MSVC STL + the vkd3d archive).
* `win_cargo` / `win_vkd3d` / `win_install_umd` MCP tools.
* **bindgen regeneration from the real SDK header** — the only way to produce
  the Core-0116 cache U0 needs. `umd12/build.rs` regenerates on Windows every
  build and only *compares* against the committed cache, warning (not failing)
  on drift.
* Everything in §18.2. All of it is runtime causal evidence on the guest.

### What a clean Linux run does **not** cover

Per `tools/umd12-host-check.sh`'s own banner: the cxx bridge's C++ compilation,
the link set, bindgen regeneration from the real SDK header, and anything that
runs. Add to that: **no ABI claim at all** while the cached bindings are the
26100 generation.

---

## 6. Blockers, ambiguities, and contradictions

**1. ⛔ BLOCKING: the committed bindgen cache is SDK 10.0.26100 and stops at Core
0110. Nothing in the Core-0116 half of this lane can even be type-checked until
it is regenerated on the VM against WDK 28000.2526.**
Evidence: `umd12/build.rs:113-115` defaults `HELIOS_WDK_INCLUDE` to
`…\Include\10.0.26100.0`; `umd12/bindgen/d3d12umddi_wrapper.h` says "Built
against SDK 10.0.26100"; the cache's highest device table is
`D3D12DDI_DEVICE_FUNCS_CORE_0109` and its highest version token is `_0110`;
`pfnCreateFence_0116`, `D3D12DDI_FENCE_TYPE`, `pNativeFenceArgs`,
`pNativeFenceOpenArgs`, `pfnCreateNativeFenceCb`, `pfnOpenNativeFenceCb` and
`D3D12DDICAPS_TYPE_0112_NATIVE_FENCE_SUPPORT` are **all absent**. The doc says
the same thing from the other side at line 98: WDK 10.0.26100.6584 "ends at Core
build 0110; it cannot supply the selected open association."
⇒ U0 is a Windows-VM action, and CLAUDE.md forbids this lane's recon agent from
performing it. The implementer must obtain it before writing a line of U9/U10/U11.
Interim: U2/U3/U4 (all of vkd3d) and U6 are unaffected and can proceed.

**2. §17.5 assigns "negotiate Core 0116" to `caps12.rs`, but the negotiated
version token lives in `adapter12.rs`, which the manifest does not list.**
`umd12/src/adapter12.rs:97-99` — `SUPPORTED_DDI_VERSIONS` is a deliberately
**single-element** set of `(INTERFACE_VERSION_R8, BUILD_VERSION_0110)`, and
`:110-120` makes `Ddi12Interface` a closed one-member enum so "every other pair
is a counted refusal". `caps12.rs` only answers `D3D12DDICAPS_TYPE` queries.
⇒ The manifest is incomplete. Add `adapter12.rs` to the lane (U11). The
single-element design is *correct* and should be preserved — just retargeted.

**3. Four more umd12 files must change and are not in the manifest:
`forward12/mod.rs`, `forward12/tables12.rs`, `forward12/noop12.rs`,
`src/knobs12.rs`.**
§17.5:4181 itself instructs "export it from `forward12/mod.rs`" while not listing
that file. `tables12.rs:67-71` pins the three table types; `noop12.rs:90-140`
compile-time-asserts 124+75+7 slots *and their ABI order* against those structs —
a regenerated 0116 header that adds or reorders a slot fails **there** first, not
in a handler. `knobs12.rs`'s six `Umd12Ecl*`/`Umd12Fence*` knobs all describe the
retired `pfnRenderCb` + engine-shadow topology, and A.4 row 5780 rules that "no
old knob may select an old path."

**4. `VKD3D_HEAP_FLAG_HELIOS_VENUS_EXPORT` is hand-mirrored across two
repositories — and the doc orders it DELETED, not cross-checked.**
Verified values, both `1u << 30`:
`vkd3d-proton-helios/libs/vkd3d/vkd3d_private.h:1119`
`#define VKD3D_HEAP_FLAG_HELIOS_VENUS_EXPORT ((D3D12_HEAP_FLAGS)(1u << 30))`
`umd12/src/forward12/resource12.rs:222`
`const HELIOS_HEAP_FLAG_VENUS_EXPORT: D3D12_HEAP_FLAGS = D3D12_HEAP_FLAGS(1 << 30);`
The vkd3d declaration's own comment (`:1114-1118`) states they "must be kept in
sync by hand, because the D3D12 API word is the only channel between them."
**Answer to the task's question: this changeset must NOT add a compile-time
cross-check.** Appendix A.4 row 5779 is explicit — *"Delete the private API bit
and adoption direction. The KMD-created WDDM allocation/backing is wrapped by
record-only translation; it is never rediscovered through a Vulkan memory ID."*
A cross-check would be a guard built for a mechanism that must not survive the
changeset. ⚠ The real hazard is **ordering**: the flag is what makes the current
committed-texture adoption path work, so deleting it on one side and not the
other silently breaks resource creation. U2 (vkd3d side, `vkd3d_private.h` +
`heap.c` + `resource.c`) and U7 (umd12 side, `resource12.rs`) must be one atomic
change, landing together with the KMD-created-backing direction.

**5. HOS1's `HOB1 bytes` field is 4 bytes; HOB1's `total bytes` field is 8 bytes.**
§10.4: HOB1 offset 48, size 8, "total bytes … at most 15 MiB" (line 1268); HOS1
offset 36, size 4, "HOB1 bytes | equals `CommandLength`/`DmaBufferSize`" (line
1317). The narrowing is safe only because of the 15-MiB cap.
**Fail-closed reading:** validate `total_bytes != 0 && total_bytes <= 15 MiB`
*before* narrowing to `u32`, and refuse the submit (device removal, per §10.6's
mismatch paragraph at 1580-1586) on any value that does not round-trip.

**6. `WrittenPrimaries` is `[D3DKMT_HANDLE; 16]` in the header; the doc's bound
is 8 — and §19.9 and §10.6/§18.2 disagreed about what to do at 9.**
`_D3DDDICB_SUBMITCOMMAND` at cache `:21586` declares a 16-element array.
The 8 is Microsoft's documented ResourceHeaps limit, cited by the doc at C1
(line 585) and C28 (line 612) — so the *header array size is not the bound* and
must never be used as one.
The apparent contradiction: §19.9 (line 5470-5474) says an over-eight ECL "is
sliced only at a parsed translated-command boundary", while §10.6 step 4 (line
1563-1566) says the UMD "never filters, partitions, merges, or invents an
ordinary primary" and §18.2 (4936-4939) says an over-eight single association
"fails before SubmitCommandCb". **Resolved, not open:** §19.10 (line 5484,
5500-5502) explicitly *supersedes* pass 81's slicing paragraph — "Helios does not
partition a runtime-owned primary set; an unrepresentable association fails
before SubmitCommandCb." ⇒ implement the fail-closed reading; do not re-litigate.

**7. The doc requires `pfnCreateContextVirtualCb` + `pfnSubmitCommandCb`, which
reverses a decision `queue.rs` documents as "decided here irreversibly" — and
silently retires UP-9's D3D12 present-identity Render packet.**
`umd12/src/forward12/queue.rs:93-151` is a long, explicit record of choosing the
**legacy** `pfnCreateContextCb`, and `:129-139` states the consequence:
*"`pKTCallbacks->pfnSubmitCommandCb` is off the table for this driver."*
§10.6 step 2 (line 1475-1478) and §17.5:4223-4226 require exactly the opposite.
The chain of consequences the doc does not spell out:
* `D3DDDICB_CREATECONTEXTVIRTUAL` returns **no** command/allocation/patch
  windows (verified: cache `:21437-21462` has `NodeOrdinal`, `EngineAffinity`,
  `Flags`, `pPrivateDriverData`, `PrivateDriverDataSize`, `hContext` — nothing
  else). So `ContextWindows` (`queue.rs:452-533`), `re_latch`, and the
  `helios_umd_common::window::Window` import all die.
* `submit_wddm_render` (`queue.rs:2773`) is the *only* `pfnRenderCb` call site
  and it has **two** users: ECL's `HeliosD3D12SubmitCmd` packet and L8's
  `submit_present_identity` (`:2907`, the UP-9 `HeliosPresentRenderCmd` record).
  ECL's is replaced by HOB1/HOS1. **The present-identity record has no
  replacement named anywhere in the doc.**
**Fail-closed reading:** the present identity record is *retired*, not migrated.
Support: §10.6's Present section (1666-1683) says presentation is carried
entirely by ordinary DXGI/dxgkrnl on the exact allocation and the queue/context
association; §11.6 answer 3 says "no Helios Present fence is created"; §17.5:4188
reduces `present12.rs` to "ordinary Present with exact source/destination
allocation and truthful fields"; A.4 row 5780 removes the present-stream
gates/counters. ⚠ Flagged because it deletes a shipped, working mechanism on
inference rather than on an explicit sentence — confirm with the owner before U12.

**8. HOB1 must live at a real submitted command GPUVA, but Helios' GpuMmu is
currently decorative.** `queue.rs:2709-2710` states it in the code:
*"Helios' GpuMmu is decorative, so there is nothing to patch."* §10.6's pool
section (1489-1490) requires `pfnAllocateCb` + Lock2 + a **reserved/mapped GPUVA
range** with paging-fence handling, and step 6 (1575-1578) requires KMD to
"enqueue the GPUVA submit without dereferencing it" while QEMU/HPM1 walks the
process page tables. §17.5 does not list this as a dependency; §17.6's
`gpummu.rs` / `build_paging_buffer.rs` / `submit_command.rs` rows are where it
actually lands. ⇒ **U8 and U9 cannot be validated until the KMD lane makes GPUVA
real.** Treat as the lane's second hard cross-lane blocker after U0.

**9. ⛔ The private record-only interface between UMD12/vkd3d and Mesa is
mandated but never specified.** The doc names it four times and defines it zero
times: §10.4:1192-1194 ("A private, versioned in-process interface … the UMD
receives the function table directly while creating its translator instance");
§2 item 8 (line 260-263, "a private direct-dispatch entry point rather than the
Vulkan loader"); §10.4:1378-1379 (`HELIOS_TRANSLATOR_SUBMISSION_MODE_RECORD_ONLY`);
§17.3:4003-4005 (`vn_helios_translation_session.c` returns generation/capability
and endpoint descriptors "only through the private direct table"). There is **no**
byte layout, symbol name, header path, versioning rule, or owning file — and
§17.1's `protocol/` list does not include it, even though it crosses three
repositories (ICD → vkd3d → umd12, and separately ICD → DXVK).
This is the single largest under-specification in the lane. **Fail-closed
reading:** the interface is package-generation-stamped and declared exactly once
in `protocol/` (per the owner directive "shared private data has ONE declaration
— in `protocol/`"); a generation mismatch fails device creation with no graceful
branch (§17.5:4254 and A.4 row 5777 both say mismatch is fatal). ⇒ CROSS-LANE
REQUEST; do not invent it in `umd12/` or `vkd3d-proton-helios/`.

**10. HNF1 (`HeliosNativeFencePddV1`) has no declared home.** §12.1:3132-3146
gives the exact 64-byte layout and says it is "object-associated KMD private
data". It crosses UMD12 → dxgkrnl → KMD, and §12.2/C49 (line 633) says the
*native-Vulkan ICD* validates the same payload on
`D3DKMTOpenNativeFenceFromNtHandle`. §17.1's five protocol modules
(`translation_session.rs`, `native_render.rs`, `physical_memory.rs`, `wddm.rs`,
`diagnostics.rs`) mention it in **none** of them. ⇒ CROSS-LANE REQUEST to the
protocol lane. **Fail-closed reading:** declare it in `protocol/`, with
size/alignment/offset asserts, and have UMD12, KMD and the ICD all consume that
one declaration — never a second copy in `umd12/src/forward12/fence.rs`.

**11. §10.4's "no queue-work" contract collides with what `Umd12EclDrain` exists
for, and the resolution deletes a measured mechanism.** §10.4 contract point 5
(line 1393-1395) forbids the translator making any queue-work
Render/SubmitCommand/HWQueue call and signalling any independent GPU timeline;
point 4 requires the sealed bytes be returned **synchronously**. Today
`queue.rs:191-212` and `knobs12.rs` carry `UMD12_ECL_DRAIN` precisely because
vkd3d's worker submits asynchronously (`d3d12_command_queue_acquire_serialized`,
`command.c:25202-25217`, an untimed `pthread_cond_wait`), and
`bridge12::sample_queue_fence` samples a venus wire fence through
`vkd3d_lock_vk_queue`. In record-only mode there is **no lower queue to drain and
no venus timeline to sample**, so `Umd12EclDrain`, `Umd12EclFence`,
`drain_queue`, `drain_queue_with_fence`, `sample_queue_fence` and the
`EclFence*` counter family are all deleted, not retargeted. ⚠ Note the coupling
`queue.rs:3683-3694` documents: the fence watermark gate is *also* what keeps the
drain from deadlocking. Both sides of that coupling go together or neither does.

**12. `swapchain.c` is named in §17.5's audit paragraph but not in its
"Modify vkd3d" file list.** §17.5:4211-4214 requires auditing "the conditional
Vulkan-swapchain submissions/present in `swapchain.c:398,2375,2719,3112`" and
proving "the private Windows UMD build must prove `swapchain.c`'s Vulkan WSI path
unreachable" — while lines 4155-4168 do not license editing that file. All four
line citations verified accurate at `f3918d5e`.
**Fail-closed reading:** prove unreachability by *build exclusion* in
`libs/vkd3d/meson.build` (which **is** licensed) rather than by editing
`swapchain.c`; §18.1:4659-4662 accepts a symbol/import-scan proof. If exclusion
turns out to be impossible without touching the source, that is a manifest
correction to request, not to take.

**13. The doc never says how a D3D12 HOB1 GPUVA use-record maps back to the
`D3DKMT_HANDLE` that `pfnMakeResidentCb` needs.** §10.6 step 2 (line 1553-1554)
requires the UMD to "make all referenced WDDM heaps resident and resolve any
paging-fence dependency before access", but §10.4's D3D12 arm of the use table is
`identityKind=2` = **GPUVA** (line 1292-1293), and residency callbacks take
allocation handles. §17.5:4175-4177 gives the shape of the answer without stating
the mechanism — *"delete the process-global engine-pointer/raw-Venus-ID registry;
exact per-resource WDDM state replaces it."*
**Fail-closed reading:** each DDI resource/heap object owns its own
`{hAllocation, GPUVA base, byte length, allocation generation}` and the sealed
batch's use records are resolved against *those* objects, per-device, with no
process-global table (§3 forbids one) and no address-keyed map (that is exactly
what `identity12.rs` did and A.4 row 5778 deletes).

**14. `D3D12DDIARG_CREATE_FENCE` carries no `HRTFENCE` in the ABI this tree can
see, so the entire fence rewrite is unverifiable — including by type-check —
until item 1 is resolved.** Cache `:51121-51134`: the struct is 16 bytes,
`{FenceCount, Fences}` only. `fence.rs:37-54` additionally records the *measured*
Helios behaviour that both `BaseAddress`es arrive `0`
(`CreateFence: valueVA=0x0 monitoredVA=0x0`). Everything §12.1 asserts about
`pfnCreateFence_0116` comes from WDK 28000 headers read from `/tmp` during the
doc's research and **not committed anywhere in this repo**. Treat every 0116
signature in the doc as unverified-locally until U0 lands.

**15. Minor: the HQA1 and HOB1 `flags` fields describe the same encoding two
different ways.** §10.4 line 1244 (HQA1): "bit 0 D3D11 physical Render, bit 1
D3D12 virtual Submit; exactly one set". Line 1267 (HOB1): "exactly one of
`D3D11_PHYSICAL=1`, `D3D12_VIRTUAL=2`". Bit 1 == value 2, so they agree — but
they must be one shared enum in `protocol/`, written once, not two hand-matched
constants. (This is the same class of defect as item 4.)

**16. Minor: the CLAUDE.md fork-provenance line and the doc's §1 table use
different bases and neither gives the current divergence.** CLAUDE.md's tree
entry says the fork is off `2c7ba22c` (9 commits behind HEAD as of this recon);
the doc's §1 table (line 85) says `helios` is "ahead of `helios/helios` by 4" at
`f3918d5e`. Both can be true — different bases. CLAUDE.md's own ⛔⛔ warning is
the operative rule: never write a count or a SHA, run
`git -C vkd3d-proton-helios log --oneline 2c7ba22c..HEAD`.

---

## CROSS-LANE REQUESTS

1. **Protocol lane:** declare `HeliosNativeFencePddV1` (HNF1, §12.1's 64-byte
   table) in `protocol/`. §17.1 omits it. (Item 10.)
2. **Protocol lane:** declare the record-only translator interface — its
   version/generation stamp, `HELIOS_TRANSLATOR_SUBMISSION_MODE_RECORD_ONLY`,
   the sealed-batch descriptor, and the physical-endpoint descriptor — once, in
   `protocol/`. §17.1 omits it and §10.4/§17.3 only gesture at it. (Item 9.)
3. **Protocol lane:** declare the D3D11-physical / D3D12-virtual flag as one
   shared enum consumed by both HQA1 and HOB1. (Item 15.)
4. **KMD lane:** HQA1 validation at `DxgkDdiCreateContext` with a strong direct
   session/endpoint reference; `DmaBufferPrivateDataSize` sized for a 64-byte UMD
   prefix + KMD suffix and `DmaBufferUmdPrivateDataSize == 64` accepted for an
   HQA1 D3D12 context (§10.4:1332-1334); HOC1 admission as nonprimary/nonshared/
   CPU-visible/WC with no HAP flags (§10.6:1510-1512); **real GPUVA/page tables**
   (item 8); `DxgkDdiCreateNativeFence`/`DxgkDdiOpenNativeFence`/close/destroy;
   HWA2 create-time write-back for `resource12.rs`.
5. **Mesa/ICD lane:** the concrete shape of `vn_helios_translation_session.c`'s
   private direct table, and the guarantee that `vkd3d_create_device` can obtain
   an HTS1 session **before** the first `pfnCreateCommandQueue`
   (§10.4:1339-1343: "If either current call order cannot meet that rule, device
   creation fails rather than attaching late").
6. **D3D11 UMD lane:** confirm `umd_common/src/window.rs` stays — D3D12 stops
   importing it, D3D11 still needs it (§10.5 keeps the physical Render context).
7. **Owner decision:** confirm the retirement of the UP-9 D3D12 present-identity
   `HeliosPresentRenderCmd` Render packet (item 7). It is a working, shipped
   mechanism being deleted on inference.
8. **VM action (blocking, U0):** regenerate `umd12/bindgen/cached/d3d12umddi.rs`
   against WDK `Microsoft.Windows.WDK.x86` 10.0.28000.2526 and commit it. Nothing
   in the Core-0116 half of this lane can be written or checked before that.

## AMBIGUITIES

Recorded above as §6 items 5, 6, 7, 9, 10, 12, 13 and 15, each with the
fail-closed reading this brief recommends. Items 6 is resolved by §19.10
superseding §19.9 and is listed only so it is not re-litigated.
