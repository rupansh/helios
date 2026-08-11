# Lane: QEMU host (HPM1 paging, HOB1 execution, plane lifetime) + packaging / tools / docs

Reference: `docs/HELIOS_PRESENT_SYNC_RETIREMENT.md` (static-analysis snapshot
2026-08-09). Everything below cites it by line range. Where this brief
and the reference disagree, the reference wins — except where §6 records that the
reference is silent or self-contradictory, in which case the fail-closed reading
named here is the working rule until the owner rules otherwise.

⛔ **Doc line numbers here were swept +10 on 2026-08-10 and mechanically
verified.** The doc is **5973 lines** (`wc -l`) — 5918 when this brief was
written, 5928 after commit `c17f17c` inserted a ten-line banner at `:11-20`, and
5973 after `afebe66` **appended** the SUPERSEDED CLAIMS INDEX at `:5932`. Only
the banner moved anything: it shifted every body line after 8 by +10, so every
citation written here before 2026-08-10 was 10 too low. All of them have now
been shifted, and the sweep was checked by requiring each `§N.M`-anchored cite
to land inside §N.M's own heading-to-heading range — not by adding 10 on faith. ⚠ **Source-file line numbers were deliberately NOT touched** — `foo.rs:123`, `virtio-gpu-virgl.c` function ranges and the like are cites into the tree, not into the doc, and the banner never moved them. If a number here is not a doc line, it was correct before this sweep and is correct now.
Re-grep the cited text before relying on any number below.

⛔ **OWNER AMENDMENT 2026-08-11: `qemu-helios` is immutable for this retirement.**
No custom protocol, plane callback, listener op, acknowledgement, or tracepoint
may be added. All **H0-H9** QEMU implementation rows below are retained only as
historical reconnaissance and are non-actionable; **P1/P2/T1/T2** keep their
separate guest/package/tool ownership. F5 already declined HPM1, and the display
lane now uses exact standard fenced nonzero `SET_SCANOUT_BLOB` completion plus a
permanent KMD parking/black blob for explicit unbind. Pinned QEMU `SET(0)` is not
an old-reader-release boundary. Do not read any later `MODIFY` verdict as
authorization to touch the submodule.

Provenance checked at write time: `qemu-helios` is a **git submodule** pinned at
`d4fde50ccb5ec51a635003fda441d0ea4bbb2818` (detached HEAD, worktree clean) —
matching §1's table (doc line 86). Every QEMU line number below is exact for that
commit.

---

## 1. Normative sources

| Section | Doc lines | What it binds for this lane |
|---|---|---|
| §2 executive decision | 122-368 | *Why*: points 1, 4, 6 are this lane's charter — the WDDM command carries the actual work (QEMU executes it), display-reader lifetime becomes exact per-plane state (retires the `resid` ledger), and every native-Vulkan allocation is one WDDM allocation whose bytes HPM1 makes authoritative in QEMU. Point 4's last sentence is the *reason* the whole read-ledger route dies. |
| §3 hard constraints | 369-406 | Bounds every choice: no Escape, no global registry/service/polling thread/sleep, no heuristic identity, no CPU wait on GPU completion in steady state, no version/feature fallback, "never delete the old mapped file while any legacy process can still map it". |
| §10.7 native-Vulkan KMT execution | 1685-2710 | The bulk of the host contract. Load-bearing sub-ranges: **1974-2022** (segment table, HLM1 flags, no CPU-host-aperture, HPM1 physical resolver domains, physical-ADL-only profile); **2023-2035** (C64 per-ProcessContext page tables, paging completion withheld until QEMU publishes the mapping/TLB epoch, `SetRootPageTable` record-and-ignore deleted); **2080-2133** (BuildPagingBuffer/Patch/SubmitCommand split, `HeliosPhysicalMemoryDmaV1` header + 24-byte run record, device-side execute/publish/revoke ordering, reset generation invalidation); **1852-1906** (48-byte `HNR2PhysicalCapability`, QEMU revalidates + resolves through HPM1 page owner + one `SUBMIT_3D` with `FLAG_FENCE \| FLAG_INFO_RING_IDX`, ring-0 means decode/reply only, nonzero ring is the VkQueue terminal fence); **2135-2178** (HVR1 reply snapshot, host writes payload + HVR1 + release publication *before* completing its context/ring fence; no `vn_ring`); **2180-2185** (teardown/adapter-stop drain order). Sub-ranges 2206-2710 (WSI layer) are the Mesa/ICD lane's; this lane reads them only for the §18.5 per-frame budget. |
| §10.8 display consumption | 2711-2843 | Plane lifetime as amended at doc lines 14-19: exact successful fenced nonzero replacement completes latch+prior release; explicit unbind first replaces with permanent KMD parking, and `SET(0)` never releases parking. **2841-2842** still deletes the `read_ledger`, 65-slot page, event registration, 10 ms signaler, and snapshot fallback. |
| §12.3 ETW/diagnostics | 3250-3337 | Provider GUID `{6D9A1A95-2B6A-4DEF-BCF7-847B6F158B0E}`, ETW descriptor version 1, one 72-byte align-8 `HeliosGraphicsEtwPayloadV1` descriptor, event IDs 1-12 (7/8/9 remain logically distinct KMD plane events), keyword bits 0-4. ETW may drop; no event delivery or counter participates in lifetime. |
| §17.7 host/packaging/docs manifest | 4506-4619 | The QEMU plane-callback portion is withdrawn. Standard virtio-gpu fencing is consumed by KMD; packaging/tool history below remains reference material. All HPM1 and custom host-submit rows are independently declined/non-actionable under F5 and the owner amendment. |
| §17.8 atomic activation | 4620-4643 | Nine ordered steps. Steps 2/3/5 are host-side staging + mutual generation/feature-bitmap exchange before KMD admits any allocation or command; steps 6/7/8 are the installer's verify → prove-no-mapper → delete `helios_present_sync_v2.bin`. |
| §18.4 display/reader gates | 5077-5114 | Acceptance is guest-observable: exact flag/id validation, latch+prior release once per successful nonzero replacement, parking-first unbind, parking retention across `SET(0)`, stale/error/timeout refusal, and no `read_ledger` page/event/thread/Escape. |
| §18.5 perf/deployment gates | 5115-5151 | One HPM1/complete-linear-BAR negotiation per adapter; one real paging DMA + mandatory side-effect-free Patch + device completion per placement transaction; one Lock2/Unlock2 per HVM1 CPU-map lifetime; prove no CPU-host-aperture callback/protocol, scan, polling, retry sleep, **CPU-side QEMU wait**, or Present-path mapping work exists; deployment test must reject every mixed generation. |
| Appendix A | 5644-5788 | Current-state disposition rows this lane owns: 5687 (installer HPS/ACL), 5737 (virgl context/ring fences are the completion source), 5738 (`read_ledger_dump.c`, `window_burst_capture.ps1:246-296`, `CONFORMANCE.md:245`), 5739 (`scanout_timeline_dump.c` + 3 blob probes), 5740 (5 dev probes), 5741 (`ui/egl-headless.c:93-233`, `ui/trace-events:189-190`, `hw/display/trace-events:234` — **may remain**, passive diagnostics), 5742 (documentation-only), 5772-5776 + 5784 + 5786 + 5787 (residual closure: archive labelling, rutabaga non-selection proof, BAR/segment foundation, virgl host endpoint, probe rewrites). |
| Appendix B | 5789-5928 | B.5 (5893-5920) is the exact reverse edge this lane deletes; B.1-B.4 are producer-side context only. |

---

## 2. Current source inventory — historical QEMU reconnaissance

### 2.1 QEMU — files the withdrawn manifest named under "Modify:"

**NON-ACTIONABLE:** every verdict in this table records the rejected plan's
historical disposition only. Under immutable `qemu-helios`, read every
`MODIFY`, `ADD`, or `RETAIN` cell as **NO QEMU CHANGE**; none assigns work.

| Path (all under `qemu-helios/`) | Lines | What it does today | Historical verdict (withdrawn) |
|---|---:|---|---|
| `hw/display/virtio-gpu-virgl.c` | 1671 | The Helios host endpoint. Blob map/unmap into the hostmem BAR (`virtio_gpu_virgl_map_resource_blob` 189-265, `..._unmap_resource_blob` 270-342, async 3-step MR teardown that suspends cmd processing via `b->renderer_blocked++` + `cmdq_resume_bh`); `virgl_cmd_submit_3d` 638-667 (finite `virgl_renderer_submit_cmd` of an exactly copied stream — the precedent §17.7:4594 says to keep); `virgl_cmd_resource_create_blob` 845-963 incl. the HOST3D blob budget; `virgl_cmd_resource_map_blob` 965-1005; `virgl_cmd_set_scanout_blob` 1032-1119; `virtio_gpu_virgl_process_cmd` 1120-1249 incl. the ring-idx context-fence creation at 1224-1236; `virgl_write_fence` 1251-1279 / `virgl_write_context_fence` 1281-1301 / `virtio_gpu_virgl_reset_async_fences` 1303-1317 / `virtio_gpu_virgl_async_fence_bh` 1318-1366 / `push_async_fence` 1367-1383 / `virgl_write_async_{,context_}fence` 1384-1398 (Appendix A row 5727: *the* completion source); `virtio_gpu_fence_poll` 1469-1483 (**the 10-ms timer**, re-arms itself at 1481); `virtio_gpu_virgl_reset` 1500-1521; `virtio_gpu_virgl_init` 1523-1599 (creates `gl->fence_poll` at 1577, selects `VIRGL_RENDERER_ASYNC_FENCE_CB` only when `qemu_egl_display` and virglrenderer ≥ 1.1.2). | **MODIFY (heavy)** |
| `hw/display/virtio-gpu.c` | 1808 | Generic virtio-gpu. Relevant: `virtio_gpu_update_scanout` 598-621 and `virtio_gpu_do_set_scanout` 622-698 (the classic + blob scanout binding; calls `virtio_gpu_update_dmabuf` at 654), `virtio_gpu_set_scanout_blob` 810-849, `virtio_gpu_disable_scanout` 381-401, `virtio_gpu_resource_unref` 423-446, `virtio_gpu_reset_bh` 1618-1647 / `virtio_gpu_reset` 1649-1679 (destroys all resources, replaces every surface with NULL, flushes cmdq/fenceq). | **MODIFY** |
| `hw/display/virtio-gpu-gl.c` | 248 | GL/virgl device wrapper. `virtio_gpu_gl_handle_ctrl` (…-88) ends every ctrl batch with `virtio_gpu_virgl_fence_poll(g)`; realize at 145-160 owns the hostmem background mapping (`qemu_ram_mmap` + `memory_region_init_ram_ptr(&gl->hostmem_background …)` + `memory_region_add_subregion(&b->hostmem, 0, …)`) — §17.7:4571-4573 says this must participate in HLM1 initialization and **drained** teardown; unrealize at 177-205 frees the timers and calls `virgl_renderer_cleanup`. | **MODIFY** |
| `include/hw/virtio/virtio-gpu.h` | 411 | `struct virtio_gpu_simple_resource` 44-66, `virtio_gpu_framebuffer` 68-74, `virtio_gpu_scanout` 76-84, `VirtIOGPUBase` 148-166 (owns `MemoryRegion hostmem`), `VirtIOGPU` 187-224 (owns `dmabuf.primary[]`), `virtio_gpu_virgl_context_fence` 240-245, `VirtIOGPUGL` 255-268 (`fence_poll`, `async_fenceq`, `hostmem_background`, `hostmem_mmap`). | **MODIFY** (add HPM1/plane state; do not disturb the rutabaga/vhost-user members) |
| `include/standard-headers/linux/virtio_gpu.h` | 460 | Verbatim copy of the Linux uapi header. 433-458 is `resource_map_blob` / `resp_map_info` / `resource_unmap_blob` (Appendix A:5776 cites exactly this). | **DO NOT MODIFY** — see §6 item 4. The Helios vendor protocol belongs in a *separate generated* header, which is what §17.7:4514-4516 actually asks for ("plus a generated Helios vendor-protocol header"). |
| `hw/display/trace-events` | 234 | virtio-gpu tracepoints; line 234 is `helios_scanout_blob_layout(...)`. Appendix A:5731 explicitly permits the `helios_scanout_*` tracepoints to remain (passive, no producer reads them). | **MODIFY** (add batch/paging/plane events; keep line 234) |
| `ui/egl-headless.c` | 761 | The active display backend. `helios_scanout_stats` 93-135, `helios_dmabuf_ino` 137-151, readback cache glue 153-185, `egl_trace_scanout_bind` 186-217 / `egl_trace_scanout_read` 218-240 (the A:5731 diagnostics), CPU dmabuf map/flush 241-443, `egl_scanout_disable` 450-461, `egl_scanout_texture` 462-488, **`egl_scanout_dmabuf` 489-575** (binds the new reader), `egl_release_dmabuf` 598-614 (**releases the old reader — today synchronously**), `egl_scanout_flush` 625-672, `egl_ops` 673-688. | **MODIFY** |
| `ui/trace-events` | 190 | 189-190 are `helios_scanout_bind` / `helios_scanout_read`. | **MODIFY** (add latch/release; keep 189-190) |
| "the display-listener headers that carry exact plane latch, replacement, and release" | — | Resolves to `include/ui/console.h` (481 lines; `DisplayChangeListenerOps` 239-266 incl. `dpy_gl_scanout_dmabuf` 252 and `dpy_gl_release_dmabuf` 262; free-function decls 327-347). | **MODIFY** — but see §6 item 1: a header alone cannot carry the calls. |

### 2.2 QEMU — files the withdrawn manifest did **not** name

**NON-ACTIONABLE:** “required”, “unavoidable”, `MODIFY`, and `ADD` below describe
the superseded design, not current work. No row authorizes a source edit, new
file, build-list change, generated header, or submodule-pointer update.

| Path | Lines | Historical reason it appeared necessary | Historical verdict (withdrawn) |
|---|---:|---|---|
| `ui/console.c` | 1616 | The DCL dispatcher. `dpy_gl_scanout_dmabuf` 1050-1070, `dpy_gl_release_dmabuf` 1101-1121, listener registration 563. Any new latch/release op is dead without a dispatcher here. | **MODIFY (required)** |
| `hw/display/virtio-gpu-udmabuf.c` | 247 | **The actual plane latch/replacement/release site.** `virtio_gpu_create_dmabuf` 185-211, `virtio_gpu_free_dmabuf` 153-162 (calls `dpy_gl_release_dmabuf` then immediately frees), `virtio_gpu_update_dmabuf` 213-247 (installs `g->dmabuf.primary[scanout_id] = new_primary`, calls `dpy_gl_scanout_dmabuf`, then **unconditionally frees the old primary in the same call**), `virtio_gpu_fini_udmabuf` 164-183. §10.8:2826-2828 and §18.4:5107-5108 are literally about these lines. | **MODIFY (required)** |
| `hw/display/meson.build` | 144 | Source lists for `virtio-gpu`/`virtio-gpu-gl` modules (lines 70-83). Any added `.c` is unbuildable without it. | **MODIFY (required if any file is added)** |
| `ui/gtk.c`, `ui/sdl2.c`, `ui/spice-display.c`, `ui/dbus-listener.c`, `ui/dbus-console.c` | — | The five other `DisplayChangeListenerOps` tables that set `dpy_gl_scanout_dmabuf` / `dpy_gl_release_dmabuf`. A new mandatory op needs at minimum a compile-clean absent/NULL path in each; a *non-acknowledging* backend must be refused, not silently treated as "released". | **MODIFY (minimal, required)** |
| `hw/display/virtio-gpu-pci.c` | 128 | `virtio_gpu_pci_base_realize` 29-69: BAR4 = `pow2ceil(g->conf.hostmem)`, prefetchable 64-bit, `virtio_pci_add_shm_cap(vpci_dev, 4, 0, g->conf.hostmem, VIRTIO_GPU_SHM_ID_HOST_VISIBLE)`. §17.7:4569-4571 says this "remains the source" — i.e. **retain**; A.4:5784 adds "Fail cold adapter start on any capability/base/length/flag mismatch", which needs a check *somewhere*. | **RETAIN** (+ possibly one admission assert; see §6 item 5) |
| `hw/display/virtio-vga.c` | 300 | Same BAR4 realization for the VGA wrapper, lines 101-153. | **RETAIN** |
| `hw/display/virtio-gpu-rutabaga.c` | 1141 | Alternate backend. A.4:5773 requires a package/backend test proving it is not selected for this generation. | **RETAIN**, add a non-selection proof (§6 item 15) |
| `hw/display/virtio-gpu-base.c` | 361 | `virtio_gpu_base_reset`, hostmem region ownership. Touched only if HPM1 state hangs off `VirtIOGPUBase`. | **MODIFY (probable, small)** |
| `include/hw/virtio/virtio-gpu-helios.h` (new) | 0 | The §17.7:4514-4516 "generated Helios vendor-protocol header" landing site. | **ADD** |
| `hw/display/virtio-gpu-helios-hpm1.c` (new) | 0 | HPM1 state, BAR/HLM1 admission, paging execution, C64 page tables. | **ADD** |
| `hw/display/virtio-gpu-helios-batch.c` (new) | 0 | HOB1 virtual-submit execution + HNR2 COMMIT capability verification. | **ADD** |

### 2.3 packaging/windows

| Path | Lines | Today | Verdict |
|---|---:|---|---|
| `Install-Helios.ps1` | 353 | Named at §17.7:4521 and A.1:5687. **Lines 198-217 create `C:\ProgramData\Helios\helios_present_sync_v2.bin` and `icacls`-grant it to Authenticated Users / LOCAL SERVICE / the Window Manager group** — the only HPS2 lifecycle owner outside the graphics binaries. Also: 110-114 resolves `payload\driver\helios_kmd_render.inf`; 179-196 builds `install-state.json` (schemaVersion 1, runtimeFiles + sha256); 218-232 copies `mesa`/`opencl`/`loaders`/`smoke` payloads; 295-307 `pnputil /add-driver …/install` + active-INF assertions; 319-332 writes `helios_vulkan.json` and registers it under `HKLM:\SOFTWARE\Khronos\Vulkan\Drivers`, sets `OpenGLDriverName/Version/Flags`, registers the OpenCL vendor; 339-341 copies the state scripts; 347-351 runs Verify. There is **no** implicit-layer registration and **no** generation/host-attestation check. | **MODIFY (heavy)** |
| `Install-Helios.cmd` | 13 | Elevation shim → `Install-Helios.ps1 -EnableTestSigning`; prints a reboot hint for `%HELIOS_RC%==194`. Nothing HPS-related. | **MODIFY (cosmetic only)** — named at §17.7:4522; the only defensible change is the exit-code/reboot text if `Install-Helios.ps1`'s codes change. Do not invent work here. |
| `Verify-Helios.ps1` | 94 | Hash-verifies `runtimeFiles`, checks PnP status/class key. **Not in the manifest**, but §17.8 step 6 ("The guest installer verifies the active KMD, both UMDs, ICD, translators, WSI layer, manifests, and host-attested generation; no legacy DLL may load") is unimplementable without it. | **MODIFY (required)** — cross-lane request |
| `Uninstall-Helios.ps1` | 117 | Reverses loaders/registry/driver. Does **not** touch the HPS file. §17.8 step 8's "installer cleanup after quiescence" most naturally lives here. **Not in the manifest.** | **MODIFY (required)** — cross-lane request |
| `Helios-PackageCommon.ps1` | 212 | Shared helpers (`Get-HeliosSha256`, `Write-HeliosJson`, `Invoke-HeliosNative`, device/class-key lookup). A generation-compare helper belongs here. **Not in the manifest.** | **MODIFY (probable)** |
| `README.md` | 91 | Describes the ProgramData layout and flows. | **MODIFY (small)** |
| `probes/{d3d11-smoke.cpp,opengl-smoke.c,opencl-smoke.c,vulkan-smoke.c}` | — | No Escape / no HPS references (verified by grep). | **RETAIN** |
| `compat/adl-shim` | — | Unrelated. | **RETAIN** |

### 2.4 tools/ — all 13 manifest-named existing files verified present

Delete (§17.7:4523-4531) — every one exists at the named path:

| Path | Lines | Why it dies |
|---|---:|---|
| `tools/read_ledger_dump.c` | 325 | `HELIOS_ESCAPE_MAP_READ_LEDGER` consumer (A.1:5738) |
| `tools/scanout_timeline_dump.c` | 268 | `HELIOS_ESCAPE_QUERY_SCANOUT_TIMELINE` ring reader (A.2:5739) |
| `tools/blob_capacity_probe.c` | 200 | private Escape blob ABI |
| `tools/blob_map_size_probe.c` | 276 | private Escape `MAP_BLOB` sweep |
| `tools/escape_owner_probe.c` | 380 | Escape trust-boundary probe |
| `tools/d3d11_shared_blob_truth_probe.cpp` | 361 | `HELIOS_ESCAPE_MAP_BLOB` content probe |
| `tools/d3dkmt_alloc_probe.c` | 225 | venus context over Escape |
| `tools/d3dkmt_sync_probe.cpp` | 238 | old sync-object form probe |
| `tools/vehicle_flipwait_probe.c` | 333 | vehicle/flip-wait model |
| `tools/vidmm_tracking_probe.c` | 1042 | old tracking/blob model |
| `tools/d3d11_kmt_shared_probe.cpp` | 270 | superseded by C57 admission (§17.3:3986 also orders this removal — shared with the Mesa lane) |
| `tools/vk_ring_fence_probe.cpp` | 365 | OPAQUE_WIN32 named-timeline model |

Add (§17.7:4532-4543) — **none exists today**; all eight are new files:
`tools/native_fence_context_probe.cpp`, `tools/wddm_batch_completion_probe.cpp`,
`tools/wddm_physical_paging_probe.cpp`, `tools/wddm_linear_bar_lock_probe.cpp`,
`tools/finite_venus_reply_probe.cpp`, `tools/translation_session_attach_probe.cpp`,
`tools/direct_flip_mpo_profile_probe.cpp`, `tools/helios_etw_capture.ps1`.
Constraint (4540-4543): the ETW tool enables the fixed §12.3 GUID and decodes only
its versioned event IDs/payload, sends no KMD request or acknowledgement; the probes
use only ordinary runtime/KMT objects, the OS display path, and C42 ETW — **no new
tool invokes a private Escape**.

Modify:

| Path | Lines | Today | Verdict |
|---|---:|---|---|
| `tools/window_burst_capture.ps1` | 509 | Named at §17.7:4544 and A.1:5738. Lines 245-296+ hard-code `C:\ProgramData\Helios\scanout_timeline_dump.exe` and `read_ledger_dump.exe`, plus `Get-TimelineCursor` (`--cursor`) and `Invoke-CausalSnapshot` writing `read-ledger-<phase>.csv` and `timeline-<phase>-cursor.txt`. Both tools are deleted. | **MODIFY (heavy)** |
| `tools/kmd-gate-surface.ps1` | — | Comment references `escape_owner_probe.c (QUERY_STATS)`. Not in the manifest. | **MODIFY (comment only)** |

Grep confirms the complete `tools/` + `packaging/` closure that touches
`HELIOS_ESCAPE` / `D3DKMTEscape` / `read_ledger` / `scanout_timeline` /
`present_sync` is exactly: the 10 Escape-using probes above + `window_burst_capture.ps1`
+ `Install-Helios.ps1`. No other tool has a hidden dependency.

### 2.5 docs — all exist

`CONFORMANCE.md` 570 · `ROADMAP.md` 4672 · `DX12.md` 485 · `TRANSPORT.md` 611 ·
`docs/dx12/PRESENT.md` 1900 · `ARCHITECTURE.md` 2008 · `DECISIONS.md` 947 ·
`KMD_IMPACT.md` 1094 · `PENDING.md` 687 · `GATES.md` 2281 · `SUBSTRATE.md` 2211 ·
`PARALLEL.md` 396 · `research/R4-umd-template-and-split.md` 1120 ·
`R5-kmd-gap.md` 799 · `R6-d3dkmt-surface.md` 669 · `R7-present-swapchain.md` 916 ·
`R9-test-conformance.md` 899 · `research/guest-vulkaninfo-full.txt` 1732
(§17.7:4553-4555: label it **historical target evidence, not live behavior**).
All **MODIFY**. `docs/archive/**` stays immutable and labelled historical
(A.4:5772) — do not edit, do not resurrect.

Known dangling citations the doc update must resolve (found by grep):
`CONFORMANCE.md:210,214,236,239,240,241,242,243,244,245,246,250` describe eleven
of the twelve deleted probes; `ROADMAP.md:2206,2233,2856,2872,3037,4654,4659` and
`DX12.md:102` cite `vk_ring_fence_probe.cpp`, `vehicle_flipwait_probe.c`,
`blob_map_size_probe.c` as *live* evidence; `ROADMAP.md:79,428` cite
`escape_owner_probe.c` / `vidmm_tracking_probe`.

---

## 3. Work decomposition

Ordered. "Owns" is the exhaustive file set for parallel-safety; two units may run
concurrently only if their owned sets are disjoint.

**H0-H9, including H0 and H9, are WITHDRAWN historical decomposition, not a
queue.** Their goals, ownership cells, dependencies, sizes, and imperative verbs
must not be scheduled or used to assign any `qemu-helios` file. Only P/T/D rows
remain potentially actionable in their respective non-QEMU trees.

| id | Historical goal for H rows; current goal for P/T/D rows | Historical ownership for H rows; current ownership otherwise | Historical dependency for H rows; current dependency otherwise | Size |
|---|---|---|---|---|
| **H0 — WITHDRAWN** | Historical proposal to land the generated Helios vendor-protocol C header and wire it into the build. It would have carried the HPM1 feature/generation + BAR admission fields, `HeliosPhysicalMemoryDmaV1` + 24-byte run record, the 48-byte `HNR2PhysicalCapability`, HOB1/HOS1 layouts, the plane latch/release records, and the one shared package-generation constant (§17.1:3768-3776, 3807-3808). | Historical only: no generated QEMU header and no Meson edit. | None; never schedule. | — |
| **H1** | HPM1 device state + StartDevice negotiation: reserve/validate the complete prefetchable 64-bit host-visible BAR, negotiate byte capacity + page shift (12 in this generation), initialize bounded physical-placement/renderer-view state **before** KMD exposes segments; refuse any logical-ADL/IOMMU profile; fail cold start on capability/base/length/flag mismatch (§10.7:1974-1995, 2012-2021; §17.7:4566-4575). | `qemu-helios/hw/display/virtio-gpu-helios-hpm1.c` (new), `include/hw/virtio/virtio-gpu.h`, `hw/display/virtio-gpu-gl.c`, `hw/display/virtio-gpu-base.c` | H0 | L |
| **H2** | HPM1 paging-DMA execution: per packet validate package/adapter/allocation generation, operation, segment/ADL page runs, range, prior epoch; perform copy/fill/discard/resource-view bind; retain source/destination/aperture/local placement + host-job refs; publish the new placement epoch only after bytes are ready; return the terminal acknowledgement that alone permits KMD's paging completion; hold old mappings until copy + queued users retire, then address-space/RCU revoke before acknowledging (§10.7:2110-2133; §17.7:4577-4592). Local placement = exact HLM1 segment offset + complete BAR bounds; system placement = exact OS physical ADL runs only. | `virtio-gpu-helios-hpm1.c`, `hw/display/virtio-gpu-virgl.c` | H1 | XL |
| **H3** | C64 address-space service: `SetRootPageTable` root + monotonically increasing generation, `UPDATE_PAGE_TABLE` / `COPY_PAGE_TABLE_ENTRIES` / `FLUSH_TLB` / map / unmap, atomic mapping+TLB epoch publication before paging completion is granted (§10.7:2023-2035). | `virtio-gpu-helios-hpm1.c`, `include/hw/virtio/virtio-gpu-helios.h` | H1; **KMD lane** must co-fix the PTE encoding (§6 item 11) | L |
| **H4** | HOB1 virtual-submit execution: consume only the KMD-originated process/address-space generation + GPUVA/size descriptor, walk the *current* HPM1 root/PTE/TLB state, read the complete HOB1 (112-byte header, 40-byte use records, 16-byte typed operands, doc lines 1267-1306), validate every range/generation/typed operand, substitute renderer IDs **only in a host-private copy**, issue one fenced `SUBMIT_3D`. HOS1 is never sent as a command. Stale/unmapped PTE, cross-process page, checksum mismatch, or page-table generation change fails **before** renderer dispatch and can never advance `SubmissionFenceId` (§17.7:4560-4566; §10.9:2872). | `qemu-helios/hw/display/virtio-gpu-helios-batch.c` (new), `hw/display/virtio-gpu-virgl.c` | H0, H2, H3 | XL |
| **H5** | HNR2 COMMIT path: accept one finite COMMIT + its patched 48-byte physical-capability table, verify every entry against HPM1 (segment 2 only when its exact range is current; segment 1 only when the contiguous aperture range resolves to the current system-page runs), substitute renderer IDs in the host-private copy, one `VIRTIO_GPU_CMD_SUBMIT_3D` (§10.7:1878-1906; §17.7:4594-4596). | `virtio-gpu-helios-batch.c`, `hw/display/virtio-gpu-virgl.c` | H4 (shares both files → **serialize with H4 or give one owner**) | L |
| **H6** | Session/ring/fence discipline: exactly one object namespace and one `VkInstance` per HTS1 session/host context (not per process/queue/KMT device); ring 0 for CPU/decode control only, unique nonzero GPU rings; report `(ctx_id,ring_idx,fence_id)` completion only after the timeline's actual contract (processing + reply publication for zero, VkQueue completion for nonzero); reject a generated GPU opcode on zero; never report a ring-0 event as device/queue idle; tag every context/ring/fence callback with the owning session/package generation and reject duplicate nonzero ring bindings, a second instance in one context, cross-session object use, stale KMD host-dispatch serial, and late completion after reset; expose no session capability or renderer ID to user mode. **Delete the 10-ms fence timer** (§17.7:4596-4613). | `hw/display/virtio-gpu-virgl.c`, `hw/display/virtio-gpu-gl.c`, `include/hw/virtio/virtio-gpu.h` | H0; **serialize with H2/H4/H5 on `virtio-gpu-virgl.c`** | L |
| **H7** | Finite-reply admission probe: send `SetReplyCommandStreamMESA` plus a harmless reply-producing command in one direct stream; validate header, token, size, and context fence; failure rejects this package generation and never re-enables `vn_ring` (§17.7:4604-4608). | `virtio-gpu-helios-batch.c` | H5, H6 | M |
| **H8 — WITHDRAWN** | Custom plane latch/release records, callbacks, listener ops, acknowledgements, and tracepoints are forbidden by the owner amendment. D2 instead treats one exact successful standard fenced nonzero `SET_SCANOUT_BLOB` response as latch+prior-release. Explicit unbind first replaces with the permanent KMD parking/black blob; optional `SET(0)` never releases parking. | **No QEMU files.** Guest changes live in `kmd_render/src/virtio/{ctrl.rs,gpu/mod.rs}` and the display plane state machine. | None; `qemu-helios` remains read-only. | — |
| **H9 — WITHDRAWN** | Historical reset/teardown proposal for the declined HPM1 and custom plane designs (§10.7:2130-2133, 2180-2185; §17.7:4572-4573). Standard pinned-QEMU reset behavior remains unchanged; guest KMD owns its own reset/candidate/current/parking cleanup. | No QEMU files. | None; never schedule. | — |
| **P1** | Installer: delete the HPS create+ACL block (lines 198-217) and every `icacls` grant for it; add the shared package-generation constant to `install-state.json` and to the payload-manifest check; add the implicit Vulkan layer JSON + `HKLM:\SOFTWARE\Khronos\Vulkan\ImplicitLayers` registration for `VK_LAYER_HELIOS_present`; install both UMDs, ICD, translators, KMD/INF, protocol and host component as one generation; reject incomplete/mismatched payloads (§17.7:4615-4618). Add the §17.8-step-8 cleanup of `C:\ProgramData\Helios\helios_present_sync_v2.bin` — see §6 item 8 for where it may legally run. | `packaging/windows/Install-Helios.ps1`, `packaging/windows/Install-Helios.cmd`, `packaging/windows/Helios-PackageCommon.ps1` | protocol lane (generation constant); Mesa/ICD lane (layer JSON name/path) | M |
| **P2** | Verify/Uninstall/README: host-attested generation check, "no legacy DLL may load" assertion, prove-no-mapper before deletion (§17.8 steps 6-8). | `packaging/windows/Verify-Helios.ps1`, `packaging/windows/Uninstall-Helios.ps1`, `packaging/windows/README.md` | P1 | S |
| **T1** | Add the 7 new C++ probes + `tools/helios_etw_capture.ps1`. | the 8 new `tools/` paths | protocol lane (§12.3 event IDs / payload) for the ETW decoder; KMD lane for the provider actually emitting | L |
| **T2** | Rework `tools/window_burst_capture.ps1`: replace `Get-TimelineCursor` / `Invoke-CausalSnapshot` with an ETW session started/stopped around the burst; drop the `read-ledger-*.csv` and `timeline-*-cursor.txt` outputs rather than repurposing their columns. | `tools/window_burst_capture.ps1`, `tools/kmd-gate-surface.ps1` | T1 (`helios_etw_capture.ps1`) | M |
| **T3** | Delete the 12 probes. **Must run after T1+T2 and after D1**, so no live script or doc points at a missing file at any commit. | the 12 `tools/` paths | T1, T2, D1 | S |
| **D1** | `CONFORMANCE.md`: replace the deleted-probe inventory (lines 210, 214, 236, 239-246, 250) with the eight new probes and their pass criteria. | `CONFORMANCE.md` | T1 | M |
| **D2** | `ROADMAP.md`, `DX12.md`, `TRANSPORT.md`: relabel every deleted-probe evidence citation as historical, retire the Escape/present-stream transport sections, and record the new stage state. | `ROADMAP.md`, `DX12.md`, `TRANSPORT.md` | D1 | L |
| **D3** | `docs/dx12/{PRESENT,ARCHITECTURE,DECISIONS,KMD_IMPACT,PENDING,GATES,SUBSTRATE,PARALLEL}.md` + research `R4,R5,R6,R7,R9`; label `research/guest-vulkaninfo-full.txt` historical target evidence. | those 13 paths | D2 | L |

**WITHDRAWN historical QEMU serialization map — DO NOT SCHEDULE:**
the former H2/H4/H5/H6/H9 `virtio-gpu-virgl.c`, H4/H5/H7 batch-file,
H8/H9 `virtio-gpu.c`, and H1/H6 header collisions assign no owner or sequence.
There is no parallel QEMU unit: immutable `qemu-helios` means H0-H9 never start.

---

## 4. Shared-file hazards (with other lanes)

QEMU-file and submodule-pointer rows below are historical collision notes only;
they assign no owner and authorize no serialization or edit. Non-QEMU rows retain
their stated ownership.

| File | Other lane | Collision and serialization |
|---|---|---|
| `tools/d3d11_kmt_shared_probe.cpp` | Mesa/ICD (§17.3, doc line 3986) orders the same deletion that §17.7:4530 orders | Delete **exactly once**. Assign to this lane (T3); the Mesa lane must not touch `tools/`. |
| `qemu-helios/include/hw/virtio/virtio-gpu-helios.h` (withdrawn) | protocol (§17.1:3768 "generated QEMU C declarations", 3784 "generated C bindings") | **Historical only.** No QEMU C header is generated, checked in, wired into the build, or assigned to either lane. |
| `packaging/windows/Install-Helios.ps1` | Mesa/ICD lane needs the `VK_LAYER_HELIOS_present` implicit-layer JSON registered; UMD12/vkd3d lane needs `UserModeDriverName[3]`/translator binaries staged | Only this lane edits the file. Other lanes file the requirement as a cross-lane request naming the exact registry path, JSON filename, and payload subdirectory. |
| `CONFORMANCE.md` | Named in §17.7 only, but it is the D3D11 correctness charter that the UMD11 lane's counters feed | Only this lane edits it. UMD11 lane supplies counter names. |
| `ROADMAP.md`, `DX12.md`, `docs/dx12/*` | Every lane produces stage state | Only this lane edits them, at the end (D2/D3), consuming the other lanes' finished summaries. A mid-flight edit by another lane will conflict. |
| `qemu-helios` submodule pointer | root repo | **No action.** Immutable QEMU means no submodule commit and no root pointer bump; the historical collision cannot arise. |
| `ci/windows/Assemble-Package.ps1` | **no lane** — outside §17's entire manifest | It writes `packageId`/manifest and stages the payload the installer consumes. P1's payload/generation changes are inert without it. See §6 item 9. |

---

## 5. Build and verification

### QEMU — Linux host, verified working today (2026-08-09)

The existing build directory was **stale**: it pinned `/usr/lib/libvulkan.so.1.4.350`
while the host now ships `1.4.357`, and `ninja` failed with
`missing and no known rule to make it`. The working sequence is:

```sh
cd /home/rupansh/helios-vgpu/qemu-helios/build-helios
meson setup --reconfigure . ..      # required whenever a host lib version moves
ninja -j"$(nproc)"
```

Verified result: 3246 steps, **exit 0**, 38 s wall on 24 cores, producing
`qemu-helios/build-helios/qemu-system-x86_64` (87 MB) plus the modular
`ui-egl-headless.so` / `hw-display-virtio-gpu-gl.so`.

From-scratch configuration (recorded verbatim from `build-helios/config.status:34`):

```sh
cd /home/rupansh/helios-vgpu/qemu-helios
mkdir -p build-helios && cd build-helios
../configure --target-list=x86_64-softmmu --enable-kvm --enable-opengl \
             --enable-virglrenderer --enable-modules --enable-vnc --disable-werror
ninja -j"$(nproc)"
```

Host dependency versions this build resolved against: virglrenderer **1.3.0**,
vulkan-loader **1.4.357**, QEMU project version **11.0.1**, ninja 1.13.2, meson.
`build-helios/` is `.gitignore`d (`.gitignore:3`), so rebuilding it does not dirty
the submodule worktree.

Runtime shape the launcher uses (`tools/launch-helios-gtk.sh:591`), which is what
the §18.4/§18.5 gates run against:

```
virtio-gpu-gl-pci, max_outputs=1, venus=true, blob=true,
hostmem=8589934592 (8 GiB, HELIOS_GPU_HOSTMEM_BYTES), max_hostmem=same,
display: egl-headless (+ -vnc), optional host3d_blob_limit
```

⚠ Per CLAUDE.md, changing `tools/launch-helios-gtk.sh`, the QEMU display/debug
transport, or launcher env vars means **stop and ask the owner to restart the VM**.
The withdrawn H1-H9 plan drives no launcher or QEMU change. Any future proposal
to change either is outside this retirement and requires a new owner decision.

### packaging / tools

Nothing in `packaging/windows/**` compiles on Linux. PowerShell scripts can be
lint-parsed only where `pwsh` exists (not assumed present here). The 7 new `.cpp`
probes compile **only on the win11 VM** (WinLibs g++ or MSVC + WDK 28000 headers;
see `ROADMAP.md:4659` for the existing g++ recipe pattern) — everything in T1 is
**Windows-VM-only verification**, and this lane must not drive the VM.

### docs

No build. The only mechanical check available is a link/citation sweep: every
`tools/<name>` referenced from `CONFORMANCE.md`, `ROADMAP.md`, `DX12.md`, and
`docs/dx12/**` must exist after T3. Run it as the D3 exit criterion.

### What this lane can prove on Linux, and what it cannot

The former QEMU compile, HPM1/HOB1/plane-state, generated-header, and backend
selection checks are historical exit criteria and are not run for this lane.
**Not** provable here: every §18.4 display gate, every §18.5 per-frame budget, the
finite-reply admission probe (needs a real guest ICD), the installer, and all 8 new
tools. Those are VM/owner-gated and must be reported as such, never as passing.

---

## 6. Blockers, ambiguities, and contradictions

**16 items.**

1. ⭐ **RESOLVED / WITHDRAWN.** There is no Helios plane wire record. KMD uses the
   standard virtio-gpu fence flag and exact echoed nonzero `fence_id`; logical ETW
   latch and reader-release events share that transaction identity.
2. ⭐ **RESOLVED without editing `virtio-gpu-udmabuf.c`.** Its nonzero replacement
   path installs the new primary and synchronously frees the old one before the
   fenced response. Its `SET(0)` path does neither, so explicit unbind first binds
   the permanent KMD parking/black blob and retains parking through optional
   `SET(0)` until a later nonzero replacement or transport reset.
3. ⭐ **WITHDRAWN.** No display-listener header, dispatcher, backend table, or
   release-acknowledgement op changes. The exact standard command completion is
   consumed entirely by the guest KMD state machine.
4. **`include/standard-headers/linux/virtio_gpu.h` should not be edited.** §17.7:4514
   lists it for modification, but it is a verbatim mirror of the Linux uapi header
   (QEMU regenerates it from Linux with `scripts/update-linux-headers.sh`); local
   edits are silently reverted by any future header sync. The same bullet already
   says "**plus** a generated Helios vendor-protocol header", and A.4:5773 says to
   "retain standards declarations needed by unrelated backends". Conservative reading:
   leave the uapi file byte-identical and put every Helios opcode/field in the new
   generated header. If the owner insists on touching it, that decision must be
   recorded at the edit site, because it will be lost on the next header sync.
5. **BAR size vs. hostmem size — "the complete linear host-visible BAR" is
   ambiguous.** `virtio-gpu-pci.c:53` and `virtio-vga.c:143` register BAR4 at
   `pow2ceil(g->conf.hostmem)` while `virtio_pci_add_shm_cap(..., 0, g->conf.hostmem,
   VIRTIO_GPU_SHM_ID_HOST_VISIBLE)` declares only `hostmem` bytes at offset 0 (this is
   what commit `d4fde50 virtio-gpu: allow non-power-of-two hostmem sizes` introduced).
   §10.7:1974-1978 requires StartDevice to "reserve the complete prefetchable 64-bit
   host-visible BAR range" and makes "the negotiated HPM1 byte capacity and page shift
   exactly the size and page granularity later reported for HLM1", while §10.7:1986
   sets `CpuTranslatedAddress = BAR guest-physical base`. With the default 8 GiB
   hostmem the two are equal and the ambiguity is invisible; with any non-power-of-two
   hostmem, HLM1 would either over-report (padding is not backed) or the "complete BAR"
   requirement is unmet. Fail-closed rule: **HLM1 size = the SHM-capability length =
   the negotiated HPM1 capacity**, the padding tail is never placeable, and cold start
   fails if the BAR is smaller than the capability length or the base is not the SHM
   region base. Flag to the owner.
6. **Removing the 10-ms fence timer conflicts with the non-async virglrenderer
   configuration.** §17.7:4604 says "Remove the 10-ms fence timer as correctness
   fallback." But `virtio_gpu_fence_poll` (`virtio-gpu-virgl.c:1469-1483`) is the only
   completion pump when `VIRGL_RENDERER_ASYNC_FENCE_CB` is *not* selected — and
   `virtio_gpu_virgl_init:1528-1537` only selects it when `qemu_egl_display` is
   non-NULL **and** `VIRGL_CHECK_VERSION(1,1,2)`. It is also called unconditionally at
   the end of every ctrl batch (`virtio-gpu-gl.c:87`). Deleting the timer without
   another change silently converts "slow" into "never completes" on any host without
   an EGL display. Fail-closed rule: make async fence callbacks **mandatory** for the
   Helios generation — fail `virtio_gpu_virgl_init` when `VIRGL_RENDERER_ASYNC_FENCE_CB`
   cannot be enabled (§3's "no version or feature fallback" supports this) — then delete
   both the timer and the end-of-batch poll call. The doc never states this
   prerequisite; record it at the deletion site.
7. **"No CPU-side QEMU wait" (§18.5:5122) vs. the RCU/revoke ordering
   (§17.7:4586-4588).** HPM1 must keep old local/aperture mappings and host refs alive
   "until copy and queued users retire, after which QEMU performs any required
   address-space/RCU revoke before acknowledging completion". QEMU's only existing
   mechanism for that is the blob-unmap dance in
   `virtio_gpu_virgl_unmap_resource_blob:270-342`, which increments
   `b->renderer_blocked`, sets `*cmd_suspended = true`, and resumes from
   `virtio_gpu_virgl_hostmem_region_finalize` via `cmdq_resume_bh`. Meanwhile
   §17.7:4590-4592 says "asynchronous self-heal paths are not used for HVM1 or
   paging". Reading: the suspend/resume BH is a *deferred completion*, not a poll,
   sleep, self-heal, or CPU wait — it is the only legal shape. The "self-heal" ban
   targets the old retry/rescan behaviour. This must be stated explicitly in the code,
   or a later reviewer will read the suspend as a banned wait.
8. **§17.8 step 8 is self-contradictory with the installer's own automatic mode.**
   Step 8 (doc 4637-4640) says deleting `helios_present_sync_v2.bin` is "an installer
   cleanup after quiescence, **never startup behavior**". But `Install-Helios.ps1`
   registers `HeliosGraphicsProvisioning` as an **AtStartup** scheduled task
   (lines 45-54) that re-runs `Install-Helios.ps1 -Automatic`; any cleanup placed in
   that script *is* startup behaviour in the automatic path. Fail-closed reading: put
   the deletion in `Uninstall-Helios.ps1` and in a non-`-Automatic`, post-verify arm
   of `Install-Helios.ps1` gated on "no process can still map it" (step 7), and never
   in the AtStartup provisioning path. §3:395 ("Never delete the old mapped file while
   any legacy process can still map it") makes the conservative reading mandatory
   anyway. Needs owner ratification because §17.7 does not list `Uninstall-Helios.ps1`.
9. **The installer cannot be made "one generation" without files no lane owns.**
   §17.7:4615-4618 requires packaging to install "the matching layer DLL/JSON, ICD,
   both UMDs, KMD/INF, translator binaries, protocol and host component as one
   generation" and to "reject incomplete/mismatched payloads"; §17.8 step 6 requires
   the installer to verify a **host-attested** generation. Three of the four files
   that would implement this are outside §17's manifest entirely:
   `packaging/windows/Verify-Helios.ps1`, `packaging/windows/Uninstall-Helios.ps1`,
   and `ci/windows/Assemble-Package.ps1` (which is what actually writes `packageId`
   and stages `payload/`). Also, "host component" has no meaning on a Windows guest —
   QEMU is on the Linux host — so "installs … the host component as one generation"
   can only mean *verifies the host-attested generation*, per §17.8 step 5's mutual
   exchange. **Cross-lane request** filed below.
10. **How the host attests its generation before KMD admits work is not specified.**
    §17.8 step 5 requires host and KMD to "mutually exchange and compare the exact
    protocol/package generation and required feature bitmap" before KMD admits any
    allocation or command, and §17.7:4566-4568 puts that at `DxgkDdiStartDevice` /
    HPM1 negotiation. But no opcode, capset, config-space field, or record is named
    for the exchange. Fail-closed rule: carry it in the HPM1 negotiation packet
    defined by §17.1's `physical_memory.rs` ("exact HPM1 feature/generation and
    complete-HLM1-BAR admission fields") — i.e. HPM1 negotiation **is** the
    attestation — and fail adapter start on mismatch. Needs protocol-lane agreement.
11. **The guest page-table (PTE) encoding QEMU must walk is undefined.** §17.7:4562
    requires QEMU to "walk C64's current HPM1 root/PTE/TLB state" and §10.7:2026-2029
    lists what the paging packets carry ("every copied PTE/run needed by the device"),
    but no bit layout, level count, page-size set, or permission encoding appears
    anywhere in the document. C64 (doc 648) only cites the Microsoft
    `UPDATE_PAGE_TABLE` structure, which describes what dxgkrnl gives the **KMD**, not
    what the KMD writes for the device. This is a genuine co-design item with the KMD
    lane and must be settled in `protocol/src/physical_memory.rs` before H3/H4 can
    start. Conservative default: a single-level, 4-KiB-granule (segment page shift 12,
    per §10.7:1960) flat table with an explicit valid bit, segment id, and
    read/write bits, plus an address-space generation stamped in every entry batch.
12. **`tools/scanout_timeline_dump.c` cannot be deleted before its replacement
    exists.** §17.7:4524-4525 says to "capture the C42 ETW provider with standard ETW
    tooling instead", but the C42 provider is emitted by the **KMD lane**, and
    `tools/window_burst_capture.ps1` is the only causal-capture instrument in the tree.
    Deleting the tool first leaves a window with no causal instrument at all. Hard
    ordering: T1 (`helios_etw_capture.ps1`) → KMD provider lands → T2 (rework
    burst-capture) → T3 (delete). Recorded as a dependency in §3.
13. **`tools/helios_etw_capture.ps1`'s enable path is under-specified against
    §12.3.** §17.7:4539-4541 says the tool "enables the fixed section-12.3 provider
    GUID and decodes only its versioned event IDs/payload; it sends no KMD request or
    acknowledgement". §12.3:3255-3258 says `DxgkDdiControlEtwLogging` changes only an
    atomic enabled bit + max level and that an event is constructed only when **both**
    that OS gate and `EtwProviderEnabled(...)` accept it. A user-mode `logman`/`wpr`
    enable of the provider GUID drives `EtwProviderEnabled`, but whether dxgkrnl also
    calls `DxgkDdiControlEtwLogging` is an OS behaviour the document does not assert.
    Fail-closed rule: the script uses only standard `logman`/`wpr` on the GUID, and the
    KMD's failure to emit is reported by the script as "provider not gated on", never
    worked around with a private call. Note that §12.3:3298-3300 already makes dropped
    events legal, so a partial trace is not a correctness failure.
14. **Deleting twelve probes destroys the evidence base that live docs cite.**
    `ROADMAP.md:2206,2233,4654,4659` cite `vk_ring_fence_probe.cpp` as the proof that
    signals retire at host GPU completion; `ROADMAP.md:2872` and `DX12.md:102` cite
    `vehicle_flipwait_probe.c` as proving queued GPU-side monitored-fence waits on this
    software-scheduled adapter (a claim the D3D12 native-fence design leans on);
    `ROADMAP.md:2856,3037` cite `blob_map_size_probe.c`. §17.7 lists those docs for
    update but never says the *evidence* must be re-derived. Fail-closed rule: D1/D2
    must either relabel each claim "historical, proven on the retired Escape/ledger
    generation" **or** name the new probe that re-proves it — never leave a live claim
    resting on a deleted tool. `tools/native_fence_context_probe.cpp` is the natural
    successor for the two fence claims.
15. **A.4:5773 mandates a test with no home.** "Package/backend tests must prove the
    alternate rutabaga module is not selected for this Helios generation." No file in
    §17.7 (or anywhere in §17) is assigned this. `hw/display/virtio-gpu-rutabaga.c`
    (1141 lines) is built as a separate module (`hw/display/meson.build:89-90`) whenever
    rutabaga is configured. Fail-closed rule: assert it in the same place that
    negotiates HPM1 (H1) — refuse adapter start if the realized device type is not
    `virtio-gpu-gl` — and record the module inventory in the build/verification step.
16. **The §17.7 doc-update list is inconsistent with Appendix A.4.**
    `docs/dx12/METHOD.md` (236 lines) is absent from the §17.7 list even though
    `CLAUDE.md` declares it authoritative over SEQUENCING for all of `docs/dx12/` and
    it describes the working loop for the retired probe-driven ladder; research
    snapshots `R1, R2, R3, R8, R10, R11, R12` are likewise absent even though
    A.4:5772 explicitly names `docs/dx12/research/R3-vkd3d-internals.md` as a residual
    literal hit that must be labelled historical. Fail-closed rule: D3 also touches
    `METHOD.md` and `R3`, labelling rather than rewriting the rest, and records that
    it went beyond the manifest.

*Explicit non-issue, recorded so a later pass does not "fix" it:* the two
`Start-Sleep -Seconds 2` calls in `Install-Helios.ps1` (around `pnputil`) are PnP
settling in an installer, not the runtime "polling thread, sleeps" that §3:373-376
bans. Leave them.

---

## CROSS-LANE REQUESTS

1. **protocol lane (§17.1) — H0 request WITHDRAWN.** It must not generate QEMU C
   declarations, HPM1 negotiation fields, or plane latch/release records. Any
   still-live guest/package constants are requested separately by their owning
   non-QEMU lanes; this historical row is not their dependency.
2. **KMD lane** — must emit the §12.3 provider (events 7/8/9) before T2/T3 can
   proceed, and owns exact standard scanout-fence validation (item 1 resolved)
   plus the PTE encoding (item 11).
3. **Mesa/ICD lane** — supply the exact `VK_LAYER_HELIOS_present` manifest filename,
   DLL name, and payload subdirectory so P1 can register the implicit layer; and do
   **not** delete `tools/d3d11_kmt_shared_probe.cpp` (this lane owns that deletion).
4. **Whoever owns CI** — `ci/windows/Assemble-Package.ps1` is outside §17's manifest
   but writes the payload/manifest the installer consumes (item 9). Either add it to a
   lane or accept that P1's generation gate cannot be end-to-end verified.
5. **Owner decision required** on items 5, 6, 8, 10, and 16 before the
   corresponding units start; items 2, 3, 4, 9, 12, 15 are manifest gaps this brief
   proposes to fill without changing any stated rule.
