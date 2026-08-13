# Lane: KMD display — MPO3, direct scanout binding, ETW diagnostics, WDDM 3.2 table audit

Reconnaissance brief for the HPS2 retirement. Normative reference is
`docs/HELIOS_PRESENT_SYNC_RETIREMENT.md` ("the doc" below); every line citation
is `doc:NNNN`. Repository line counts and struct shapes were read on 2026-08-09
at root commit `d1c820a` (branch `wddm-dx12`).

⛔ **Doc line numbers here were swept +10 on 2026-08-10 and mechanically
verified.** The doc is **5976 lines** (`wc -l`) — 5918 when this brief was
written, 5928 after commit `c17f17c` inserted a ten-line banner at `:11-20`, and
5973 after `afebe66` **appended** the SUPERSEDED CLAIMS INDEX at `:5932`. Only
later append-only index corrections raised the total to 5976; none moved a body
line. Only the banner moved anything: it shifted every body line after 8 by +10, so every
`doc:` citation written here before 2026-08-10 was 10 too low. All of them have
now been shifted, and the sweep was checked by requiring each `§N.M`-anchored
cite to land inside §N.M's own heading-to-heading range — not by adding 10 on
faith. ⚠ **Source-file line numbers were deliberately NOT touched** — `foo.rs:123`
and the like are cites into the tree, not into the doc, and the banner never
moved them. Re-grep the cited text before relying on any number below.

⛔ **Read the SUPERSEDED CLAIMS INDEX at `doc:5932` before treating any line in
the doc as a requirement.** HPM1 is DECLINED (`FINDINGS.md` F5), build 28000 is
not a package minimum (F1), the Code-43 segment wall does not exist (F2), and
§10.3's `ID3D12Fence` import is impossible and reverses direction (F8).

This brief is a working plan, not an authorization. The doc's own status line
(`doc:3-7`) permits disabled-by-default implementation only; activation and the
`WddmSurface` flip stay gated on §18 (see unit **D9**).

---

## 1. Normative sources

| Section | Lines | What it contributes to this lane |
|---|---|---|
| §2 executive decision, item 4 | `doc:167-176` | Display-reader lifetime becomes exact per-plane display state; PresentId is evidence, never a reader-release fence; this retires the `read_ledger`. |
| §2 executive decision, item 5 | `doc:177-193` | The one-primary Display-Core/MPO3 surface, the shared KMD validator on **both** classic SetVidPn and MPO3, the D3D12 any-source sentinel, and the mandatory cold-DWM admission gate. |
| §2 required version/feature list | `doc:319-352` | "changing only `WddmSurface`/version/caps is forbidden"; the package is all-or-nothing. |
| §2 HPS2 responsibility table | `doc:302-317` rows "Backbuffer/scanout reuse", "Diagnostics" | Names the replacement owners this lane implements. |
| §3 hard constraints / non-goals | `doc:369-406` | No Escape; no global table/registry/polling/sleep/heuristic identity; no CPU wait on GPU completion in the Present path; no version fallback. Bounds every design choice below. |
| §10.8 display consumption and host-reader retirement | `doc:2711-2843` | **The charter.** Cap values, the UMD gate, `validate_direct_scanout_binding`, `CheckMPO3`/`SetMPO3` rules, the `FlipWithMultiPlaneOverlay` Present arm, composed/direct/transition/cancel state machine, and the explicit deletion list at `doc:2841-2842`. |
| §10.9 failure policy | `doc:2844-2881`, esp. rows at `2854`, `2864`, `2880` | 3.2-without-MPO3 or failed cold DWM admission ⇒ reject the package (no 2.1 fallback); candidate cancellation releases only the candidate; epoch overflow refuses further presents. |
| §12 object contract, row "Current display-plane binding" | `doc:3122` | One exact object per active VidPn source/MPO plane; retain-before-accept; latch replaces prior; prior releases only after replacement **plus** backend release. |
| §12.3 ETW and OS-diagnostic contract | `doc:3250-3337` | Provider GUID, the 72-byte `HeliosGraphicsEtwPayloadV1` byte table, the 12 event IDs, the 5 keyword bits, `EtwRegister`/`EtwProviderEnabled`/`EtwWrite`/`EtwUnregister` lifecycle, IRQL and no-allocation rules, "ETW can drop events". |
| §17.6 KMD and logic manifest | `doc:4265-4505` (display/ETW/audit focus `doc:4460-4505`) | The file-level Delete/Modify/Add manifest, the registered display DDI list, the `diag_etw.rs` mandate, `DxgkDdiEscape=NULL`, `PostMultiPlaneOverlayPresent` registration, and the "explicit slot-and-cap audit". |
| §18.1 static/build gates | `doc:4653-4656`, `4715-4722` | The generated `DRIVER_INITIALIZATION_DATA` slot audit and the ETW register/rundown/decode tests. |
| §18.4 display/reader gates | `doc:5077-5114` | Acceptance criteria this lane is graded on. |
| Constraint rows C31/C41/C42/C43/C44 | `doc:625`, `635`, `636`, `637`, `638` | Primary-source citations for `SetVidPnSourceAddress`, the MPO3 table, the ETW/diagnostic callbacks, `CheckDirectFlipSupport`, and the D3D12 runtime-primary sentinel. |
| HWA2 allocation-descriptor byte table | `doc:1043-1080` | The immutable create-time descriptor the validator reads: `PRIMARY`/`DISPLAYABLE`/`DIRECT_FLIP_COMPATIBLE`/`D3D12_RUNTIME_PRIMARY`/`PROTECTED`/`CROSS_ADAPTER` flags at offset 68, VidPn source at 80, swizzle/layout class at 88, plane count at 96, four plane records at 104. |
| Appendix A.2 rows | `doc:5697`, `5698`, `5699`, `5700`, `5701`, `5718`, `5720`, `5721` | Current-state disposition for `wddm_surface.rs`, the UMD MPO caps, the Direct-Flip slot, `display.rs:1246-1670`, `present_packet.rs:807-860`, `read_ledger.rs`, `adapter/scanout.rs:1025-1054,1181-1187`, and `kmd_logic`'s `scanout_read_ledger` module (⛔ this row said `kmd_logic:4079-4460`; the module is at **`:4035-4290`** and its tests at **`:4291-4519`** — re-derive from the neighbouring `mod` starts, §2.4). |
| Appendix A.3 / A.4 rows | `doc:5756`, `5780` | The whole `resid` discovery/ledger route is deleted; `scanout_trace.rs:959-963,1050-1055` kill switches/counters go with their mechanisms. |
| §17.4 (D3D11 UMD, **not owned**) | `doc:4084-4126` | The paired UMD contract: one-primary MPO caps and the C43/C44 `CheckDirectFlipSupport` body. This lane must not implement it and must not assume it ran. |

---

## 2. Current source inventory

### 2.1 Files the task names (source scope)

| Path | Lines | What it does today | Verdict |
|---|---:|---|---|
| `kmd_render/src/ddi/display.rs` | 2962 | Everything display: `dxgkddi_present` (+`_inner`), the DMA-flip arm, `arm_dma_flip_programming`, `fast_bind_from_flip`, `set_vidpn_source_address_dirql`, the deferred/PASSIVE `apply_*` chain, `program_vidpn_source{,_inner}`, `ScanoutReject`, the retry budget, the LINEAR fallback (`production_linear_scanout`), `service_windowed_blt`, and 13 thin VidPn/pointer/system-display DDI wrappers. | **REWRITE (in place)** — see 2.3 for the exact ranges. |
| `kmd_render/src/ddi/present_packet.rs` | 1066 | `PresentPayload::decode` (3-arm union), `PresentAllocationList`, `PresentAllocations`, `PresentSubmissionPrivate`, `PatchCapacity`, the DMA/patch writers. `PresentPayload::MultiPlaneOverlay` is a **refusal-only** variant (`:807-862`). | **MODIFY** — replace the refusal variant with a bounded decoded `MultiPlaneOverlay(DXGK_PRESENTMULTIPLANEOVERLAYINFO)` arm; leave the allocation-list machinery alone. |
| `kmd_render/src/ddi/mpo3.rs` | — | **DOES NOT EXIST.** | **ADD** |
| `kmd_render/src/ddi/direct_scanout.rs` | — | **DOES NOT EXIST.** | **ADD** |
| `kmd_render/src/ddi/diag_etw.rs` | — | **DOES NOT EXIST.** | **ADD** |
| `kmd_render/src/ddi/scanout_trace.rs` | 1067 | Unsampled atomics + 16-slot ring + 8 histograms, published to the driver service key by `dump`/`dump_periodic`/`reset`. ~55 counters, most of them for mechanisms the retirement deletes (fast bind, snapshot, lease, refresh, windowed BLT, present stream, read ledger). | **REWRITE (shrink)** — keep only counters whose mechanism survives; delete `RETIRED_VALUE_NAMES`, the read-ledger census call (`:959-963`), and every histogram of a deleted mechanism. See §6 item 12 on whether the service-key mirror survives at all. |
| `kmd_render/src/ddi/scanout_timeline.rs` | 227 | 32768-slot × 64-byte lock-free ring, `note()` writer (33 call sites, 7 files), plus the **userspace read interface** `cursor()`/`capacity()`/`read()` consumed only by `ddi/escape.rs:499-534` (`QUERY_SCANOUT_TIMELINE`). 27 `kind::*` constants, 12 `flag::*` bits. | **REWRITE** — `doc:4326-4327`: "bounded ETW event construction with no userspace cursor/read interface". Ring, cursor, read, capacity, and `Event` all go. |
| `kmd_render/src/adapter/scanout.rs` | 1300 | Display refresh policy on `AdapterContext`: epoch minting, lease begin/end, bind-refresh arming, `mint_scanout_bind_seq`, `queue_active_scanout_refresh` (issues the ledger ticket at `:1032` and the flush token), `retire_scanout_allocation{,_locked}` (`:1114-1300`, ledger reclaim at `:1187`). | **REWRITE (in place)** — this becomes the candidate/current/backend-release plane state machine. |
| `kmd_render/src/adapter/read_ledger.rs` | 660 | The 4 KiB adapter page, 65 generation-qualified `{resid,generation,issued,retired}` slots, the 16-entry event table, `issue`/`retire`/`note_alloc_retired`/`register_event`, and the `RdIss`/`RdRet`/`RdOvf`/`AqReg`/… counters. | **DELETE** (`doc:4296`, `doc:5718`). |

### 2.2 Manifest-named files that this lane needs but does **not** own

`ddi/mod.rs`, `lib.rs`, `build.rs` are **core-owned** by task assignment. The
exact additions this lane needs there are listed in §4.

### 2.3 `display.rs` — exact ranges that change

| Range | Symbol | Change |
|---|---|---|
| `:1-62` | module docs, `PRESENT_*` atomics, `ScanoutFormat` const-assert | Rewrite docs; keep the `ScanoutFormat` ↔ virtio const-assert block verbatim. |
| `:64-144` | `production_linear_scanout` | **DELETE.** `doc:2777` — "The KMD never identifies this allocation from its geometry"; a LINEAR copy fallback is a guessed allocation path, forbidden by `doc:2189-2190` ("Neither layer enters a guessed allocation path"). |
| `:167-177` | `diag_dump_present_atomics` | Keep or fold into the surviving trace module (unit D8). |
| `:179-215` | `dxgkddi_present` wrapper | Keep; replace the `scanout_timeline::note(PRESENT_RETURN,…)` call with an ETW emit or delete it (§6 item 9). |
| `:217-992` | `dxgkddi_present_inner` | **Rewrite.** `:248-261` currently refuses `FlipWithMultiPlaneOverlay` with `PBmpo` + `STATUS_NOT_SUPPORTED`; `doc:2801-2808` requires the bounded MPO arm instead. `:295-320` (snapshot stash, present-stream marker) are deleted mechanisms. |
| `:993-1065` | `service_windowed_blt` | **DELETE** with the WindowedBlt/snapshot mechanism (`doc:5714`, `doc:5780`). |
| `:1108-1244` | `display_half_on`, pointer/VidPn wrappers | Keep; revalidate under the 3.2 table (`doc:4480` for `UpdateMonitorLinkInfo`). |
| `:1246-1325` | `dxgkddi_set_vidpn_source_address` | **Rewrite** to `doc:4468-4472` / `doc:5091-5095`: shared validator, bounded nonpageable DIRQL-safe candidate retention, identical candidate/current/backend-release machine, loud failure before latch. |
| `:1327-1446` | `arm_dma_flip_programming` | Rewrite: keep the exact-allocation pairing, delete the snapshot substitution and the `pending_vidpn_allocation` single-slot coalescer (a coalescer silently discards intermediate primaries — incompatible with per-plane candidate retention). |
| `:1448-1607` | `fast_bind_from_flip` | **DELETE** — knob-gated accelerator for a mechanism being replaced (`doc:5780` "Remove vehicle/read-ledger/snapshot/present-stream gates and their counters with the mechanisms"). |
| `:1609-1668` | `set_vidpn_source_address_dirql` | Fold into the new `direct_scanout` candidate path. |
| `:1670-1815` | `process_deferred_vidpn_source_address`, `apply_deferred_*_locked` | Rewrite as the candidate→latch continuation. |
| `:1816-1878` | `apply_vidpn_source_address` | Rewrite. |
| `:1867-1978` | `ScanoutReject` + its 8 counters | Keep the *shape* (typed refusal + named counter is exactly CLAUDE.md rule 2); re-populate the variants from the new validator's rejection set. |
| `:1980-2046` | `SCANOUT_RETRY_BUDGET`, `note_retry_attempt`, `RetryDecision` | **DELETE** — a retry budget over a host bind is the "retry/polling compatibility arm" `doc:5754` forbids. `STATUS_RETRY`/`PrePresentNeeded` (`doc:2787-2789`) is the only sanctioned retry, and it is the OS's protocol, not ours. |
| `:2108-2209` | `apply_vidpn_source_address_locked`, `release_leases_for_*` | Rewrite onto candidate/current/backend-release. |
| `:2210-2832` | `ProgramTrace`, `program_vidpn_source{,_inner}` | **Rewrite** — this is the body that becomes `direct_scanout::latch`. Note `:2333-2340` (`source.width != 0 ? source.width : mode_w`) is a geometry inference the doc forbids; the HWA2 descriptor supplies the extent and a mismatch is a rejection, not a substitution. |
| `:2833-2961` | monitor-modes / HW-capability / scan-line / system-display wrappers | Keep; audit under the 3.2 table. |

### 2.4 Manifest gaps — files this lane must touch that §17.6 does **not** list

These are named plainly because the manifest is wrong, not because the work is optional.

| Path | Lines | Why this lane needs it |
|---|---:|---|
| `kmd_render/src/ddi/base.rs` | 79 | Owns `dxgkddi_control_etw_logging` (`:42-49`, a `{}` no-op) and `dxgkddi_unload` (`:17-20`). §12.3 and `doc:4486-4491` both retarget those two. **Absent from §17.6 entirely.** |
| `kmd_render/src/ddi/hpd.rs` | 324 | The PASSIVE display worker that drains `pending_vidpn_allocation` and drives `ScanoutRefreshQueue`. The candidate/latch continuation lives here. Absent from §17.6. |
| `kmd_render/src/ddi/vidpn.rs` | 1238 | Owns the committed mode/topology the validator must consume (`doc:2756-2757` "current committed target mode"). Absent from §17.6. |
| `kmd_render/src/adapter/kobj.rs` | 630 | VSync timer/DPC; publishes `last_primary_address` into `DXGK_INTERRUPT_CRTC_VSYNC` and calls `scanout_timeline::note`. MPO3 flips retire through `DXGK_INTERRUPT_CRTC_VSYNC_WITH_MULTIPLANE_OVERLAY3` instead. Absent from §17.6. |
| `kmd_logic/src/lib.rs` | run `wc -l` | §17.6 lists it, but the task's source scope does not. `scanout_read_ledger` + `scanout_read_ledger_tests` must be deleted and replaced by the plane candidate/current/backend-release model (`doc:5721`). This is the only place KMD logic can have running tests. ⛔ **Re-measured 2026-08-10: the old figures in this row were all wrong.** The file is **8144** lines, not 5596; `scanout_read_ledger` starts at **`:4035`** (not 4079) and its tests at **`:4291`** (not 4335), and the module pair ends at **`:4519`**, where `snapshot_bind` begins — so the stated `:4079-4460` / `:4335-4563` both started late and stopped short, and the excision they describe would have cut into two neighbours. **Do not cite a line number from this row**: locate the two `mod` blocks with `rg -n -e '^pub mod ' -e '^mod ' kmd_logic/src/lib.rs` (⛔ **`-e`, not `'^pub mod \|^mod '`** — `rg` reads `\|` as a literal pipe and returns nothing) and excise between the two neighbouring `mod` starts. |

### 2.5 WDK binding facts verified offline

`tmp/dxgk_bindings.rs` is the offline struct-shape oracle. ⚠ **REFRESHED
2026-08-11**: it is now 106,125 lines generated from **10.0.28000.0**, the kit the
driver actually builds against; the copy that produced §2.5's shapes below was
2026-07-08 / **26100** and three claims derived from it were falsified (see
blocker 3 and `tmp/dxgk_bindings.README.md`). The shapes below were confirmed
against the old copy and have NOT been re-confirmed. Confirmed shapes the
implementer will need:

```
DXGKARG_CHECKMULTIPLANEOVERLAYSUPPORT3 {
    PlaneCount: UINT, ppPlanes: **DXGK_MULTIPLANE_OVERLAY_PLANE_WITH_SOURCE2,
    PostCompositionCount: UINT, ppPostComposition: **DXGK_MULTIPLANE_OVERLAY_POST_COMPOSITION_WITH_SOURCE,
    Supported: BOOL, ReturnInfo: DXGK_CHECK_MULTIPLANE_OVERLAY_SUPPORT_RETURN_INFO }
DXGK_MULTIPLANE_OVERLAY_PLANE_WITH_SOURCE2 { hAllocation, VidPnSourceId, LayerIndex, PlaneAttributes }
DXGKARG_SETVIDPNSOURCEADDRESSWITHMULTIPLANEOVERLAY3 {
    VidPnSourceId, InputFlags, OutputFlags, PlaneCount, ppPlanes: **DXGK_MULTIPLANE_OVERLAY_PLANE3,
    pPostComposition, Duration, pHDRMetaData, TargetFlipTime }
DXGK_MULTIPLANE_OVERLAY_PLANE3 { LayerIndex, PresentId, InputFlags, OutputFlags,
    MaxImmediateFlipLine, ContextCount, ppContextData: **DXGK_PRIMARYCONTEXTDATA,
    DriverPrivateDataSize, pDriverPrivateData, PlaneAttributes }   // NO hAllocation, NO Enabled
DXGK_PRIMARYCONTEXTDATA { hContext, hAllocation, {SegmentId|MmuId}, {SegmentAddress|VirtualAddress} }
DXGKARG_GETMULTIPLANEOVERLAYCAPS { VidPnSourceId, MaxPlanes, MaxRGBPlanes, MaxYUVPlanes,
    OverlayCaps: DXGK_MULTIPLANEOVERLAYCAPS, MaxStretchFactor: f32, MaxShrinkFactor: f32 }
DXGKARG_GETPOSTCOMPOSITIONCAPS { VidPnSourceId, MaxStretchFactor: f32, MaxShrinkFactor: f32 }
DXGKARG_POSTMULTIPLANEOVERLAYPRESENT { VidPnTargetId, PhysicalAdapterMask, LayerIndex, PresentID }
DXGKARG_CONTROLMODEBEHAVIOR { Request, Satisfied, NotSatisfied }   // all DXGK_MODE_BEHAVIOR_FLAGS
DXGKARG_VALIDATEUPDATEALLOCPROPERTY { hAllocation, SupportedSegmentSet, PreferredSegment, Flags, <union> }
DXGKARG_COLLECTDBGINFO2 { Reason, pBuffer, BufferSize, pExtension, TdrType, TdrPayloadSize, TdrPayload }
DXGKARG_COLLECTDIAGNOSTICINFO { hAdapter, Type, BucketingString[64], DescriptionString[128], <union>,
    BufferSizeIn, BufferSizeOut, pBuffer }   // note: arg1 is IN_CONST_PDEVICE_OBJECT, not a handle
DXGKARG_PRESENT.__bindgen_anon_1 { pAllocationList | pAllocationInfo | pPresentMultiPlaneOverlayInfo }
DXGK_PRESENTMULTIPLANEOVERLAYINFO { VidPnSourceId, PlaneListCount, pPlaneList: *DXGK_PRESENTMULTIPLANEOVERLAYLIST }
DXGK_PRESENTMULTIPLANEOVERLAYLIST { LayerIndex, Enabled: BOOL, hDeviceSpecificAllocation, <union>, PhysicalAddress }
DXGKDDI_CONTROL_ETW_LOGGING = fn(Enable: BOOLEAN, Flags: ULONG, Level: UCHAR)   // returns VOID
DXGK_INTERRUPT_CRTC_VSYNC_WITH_MULTIPLANE_OVERLAY3 = 18
  arm: { VidPnTargetId, PhysicalAdapterMask, MultiPlaneOverlayVsyncInfoCount,
         pMultiPlaneOverlayVsyncInfo: *DXGK_MULTIPLANE_OVERLAY_VSYNC_INFO3, GpuFrequency, GpuClockCounter }
DXGK_MULTIPLANE_OVERLAY_VSYNC_INFO3 { LayerIndex: DWORD, FirstFreeFlipQueueLogEntryIndex: ULONG }
```

Table-audit arithmetic, measured: `_DRIVER_INITIALIZATION_DATA` has **193
slots** in the 26100 bindings; `lib.rs::build_ddi_table` assigns **84** of them
plus `Version`. The audit therefore has ~108 unregistered slots to classify.
The display/diagnostic slots currently NULL and named by §17.6 are:
`DxgkDdiCheckMultiPlaneOverlaySupport3`,
`DxgkDdiSetVidPnSourceAddressWithMultiPlaneOverlay3`,
`DxgkDdiGetMultiPlaneOverlayCaps`, `DxgkDdiGetPostCompositionCaps`,
`DxgkDdiPostMultiPlaneOverlayPresent`, `DxgkDdiValidateUpdateAllocationProperty`,
`DxgkDdiControlModeBehavior`, `DxgkDdiCollectDbgInfo2`,
`DxgkDdiCollectDiagnosticInfo`. Also currently NULL, in the same families and
**not** named by §17.6: `…OverlaySupport`, `…OverlaySupport2`,
`SetVidPnSourceAddressWithMultiPlaneOverlay{,2}`, `Create/Update/Flip/DestroyOverlay`,
`DxgkDdiQueryDiagnosticTypesSupport`, `DxgkDdiControlDiagnosticReporting`,
`DxgkDdiResetDisplayEngine`, `DxgkDdiSetInterruptTargetPresentId`,
`DxgkDdiSetTimingsFromVidPn`, `DxgkDdiSetTargetAdjustedColorimetry{,2}`,
`DxgkDdiSetDisplayPrivateDriverFormat`, `DxgkDdiNotifyFocusPresent`,
`DxgkDdiDisplayDetectControl`, `DxgkDdiRecommendVidPnTopology`.

ETW kernel APIs (`EtwRegister`, `EtwUnregister`, `EtwWrite`,
`EtwProviderEnabled`, `REGHANDLE`, `EVENT_DESCRIPTOR`, `EVENT_DATA_DESCRIPTOR`)
appear **nowhere** in the tree today. `wdk-sys` 0.5.1's `Base` ApiSubset
bindgens `ntifs.h` + `ntddk.h` + `ntstrsafe.h` with no allowlist, and those pull
in `wdm.h` → `evntprov.h`, so they should land in `wdk_sys::ntddk::*` /
`wdk_sys::*`. **This is inference from the crate's build script, not an
observation** — verify with a grep of the VM's generated `ntddk.rs` before
writing `diag_etw.rs` (unit D0 step 0).

---

## 3. Work decomposition

Dependency shorthand: **P** = protocol lane (§17.1), **C** = KMD-core lane
(`ddi/mod.rs`, `lib.rs`, `build.rs`, `create_allocation.rs`, `query_adapter_info.rs`,
`interrupt.rs`, `lifecycle.rs`, `virtio/*`, `device.rs`), **U** = D3D11 UMD
lane (§17.4). `qemu-helios` is read-only for this retirement (§6 item 6).

| Id | Goal | Files owned | Depends on | Size |
|---|---|---|---|---|
| **D0** | ETW substrate: `EtwRegister` at PASSIVE `DriverEntry`, retained `REGHANDLE`, atomic enabled-bit/level pair, `EtwProviderEnabled`-gated emit for all 12 event IDs, one 72-byte non-paged data descriptor per `EtwWrite`, bounded event-site rundown, one `EtwUnregister` at unload, and a failed-registration record for the next diagnostic snapshot. | `ddi/diag_etw.rs` (ADD) | **P**: `protocol/src/diagnostics.rs` (GUID, descriptors, 72-byte payload, flags, keywords). **C**: `ddi/mod.rs` + `lib.rs` (see §4). Also needs the `base.rs` ETW-callback retarget (§6 item 1). | M |
| **D1** | Retire the private timeline ring; every surviving causal boundary becomes an ETW event. | `ddi/scanout_timeline.rs` (REWRITE) | D0; **C** must have deleted `ddi/escape.rs` (its only reader). Touches 33 `note()` call sites across 7 files → see §4. | M |
| **D2** | The one bounded, non-pageable `validate_direct_scanout_binding` routine + the candidate/current/fenced-release plane object graph, one object per VidPn source/plane. Consumes only the OS-supplied allocation, VidPn source, committed target mode, operation flags, and actual plane attributes. A permanent KMD-owned black parking blob is the explicit-unbind replacement, never a presentation fallback. | `ddi/direct_scanout.rs` (ADD) | **P**: HWA2 descriptor. **C**: `create_allocation.rs` must expose an HWA2 read for an `hAllocation`; `virtio/ctrl.rs` + `virtio/gpu/mod.rs` must mint and exactly validate standard fenced nonzero `SET_SCANOUT_BLOB` completions (§6 item 6). D0 for events 7/8/9. **No QEMU edit.** | L |
| **D3** | `DxgkDdiCheckMultiPlaneOverlaySupport3`, `DxgkDdiSetVidPnSourceAddressWithMultiPlaneOverlay3`, `DxgkDdiGetMultiPlaneOverlayCaps`, `DxgkDdiGetPostCompositionCaps`, `DxgkDdiPostMultiPlaneOverlayPresent`, `DxgkDdiValidateUpdateAllocationProperty`, `DxgkDdiControlModeBehavior`. | `ddi/mpo3.rs` (ADD) | D2, D0. **C** for the `lib.rs` slot registrations. | L |
| **D4** | Classic `DxgkDdiSetVidPnSourceAddress` and the DMA-flip arm rebuilt on D2; delete the LINEAR fallback, the fast-bind accelerator, the retry budget, the snapshot substitution, and the single-slot coalescer. | `ddi/display.rs` (all ranges in §2.3 except the Present arm) | D2, D0. | XL |
| **D5** | The `FlipWithMultiPlaneOverlay` Present union arm: bounded `PlaneListCount`, validate every `hDeviceSpecificAllocation`, never read `pAllocationList`, emit the ordinary exact-allocation packet, claim no lease. | `ddi/present_packet.rs`, `ddi/display.rs::dxgkddi_present_inner` | **Serialized after D4** — shares `display.rs`. | M |
| **D6** | `adapter/scanout.rs` becomes the plane lifetime owner: delete ledger issue/retire, delete the snapshot/windowed-BLT/present-marker refresh routes, install latch-once / release at the exact fenced nonzero replacement boundary, including the two-step parking unbind. | `adapter/scanout.rs` | D2. Touches `AdapterContext` fields in `adapter/mod.rs` (**C**). | L |
| **D7** | Delete the read ledger and every call site. | `adapter/read_ledger.rs` (DELETE) | D6. Call sites outside this lane: `adapter/mod.rs:27,34-36,492,1058,1266-1269,1622-1626`, `adapter/locks.rs:8-9`, `device.rs:352`, `ddi/lifecycle.rs:169`, `virtio/ctrl.rs:1177-1182,1236-1242`, `virtio/gpu/mod.rs:47,602,5421,5538,5584,5640,5750`, `ddi/escape.rs` (deleted by **C**). | M |
| **D8** | Shrink `scanout_trace.rs` to the counters whose mechanism survives; delete the read-ledger census and every retired-value name. | `ddi/scanout_trace.rs` | **Serialized after D4, D6, D7** — it is the publication point for their counters. | M |
| **D9** | The WDDM 3.2 slot-and-cap audit: classify all 192 callback slots plus `Version` (193 fields total), prove every cap-disabled family unreachable, prove `DxgkDdiEscape == NULL`, prove no `PostPresentNeeded`/Hsync/HW-flip-queue flag is ever set, and flip `SURFACE` to `Wddm3_2GpuMmu`. | generated human artifact `docs/retirement/d9-wddm32-slot-audit.md`; machine audit `ddi/wddm32_slot_audit.rs`; `ddi/wddm_surface.rs` is **C**-owned | D3, D5, **and the native-fence lane**. Strictly LAST: `doc:2854` rejects a 3.2 package whose MPO3 or fence surface is incomplete. | M |
| **D10** | `kmd_logic` model: delete `scanout_read_ledger` + its tests; add a host-tested plane candidate/current/backend-release/reset/mode-transition model. | `kmd_logic/src/lib.rs` (SHARED — §4) | D2 (contract), D7. | M |

**Same-file constraints, stated explicitly:** D4 and D5 both write
`display.rs` → serialize D5 after D4 (or merge). D4/D6/D7 all remove
`scanout_trace` counters → D8 runs after all three. D1 changes the
`scanout_timeline::note` signature, which 5 files outside this lane call → D1 is
a cross-lane rendezvous, not a private edit.

---

## 4. Shared-file hazards

| File | Other owners | Serialization |
|---|---|---|
| `kmd_render/src/ddi/mod.rs` | **Core** (declares/`pub use`s every DDI module). | Core-owned. This lane needs added: `pub(crate) mod mpo3; pub(crate) mod direct_scanout; pub(crate) mod diag_etw;` and the `pub use` of `dxgkddi_check_multi_plane_overlay_support3`, `dxgkddi_set_vidpn_source_address_with_multi_plane_overlay3`, `dxgkddi_get_multi_plane_overlay_caps`, `dxgkddi_get_post_composition_caps`, `dxgkddi_post_multi_plane_overlay_present`, `dxgkddi_validate_update_allocation_property`, `dxgkddi_control_mode_behavior`, `dxgkddi_collect_dbg_info2`, `dxgkddi_collect_diagnostic_info`, `dxgkddi_control_etw_logging` (retargeted). Removed: `mod blob_map`/`mod escape` and their re-exports, `pub use cpu_host_aperture::*`. |
| `kmd_render/src/lib.rs` | **Core**. | Core-owned. This lane needs 9 new `data.DxgkDdi*` assignments (list in §2.5), `data.DxgkDdiEscape = None`, the `data.DxgkDdiControlEtwLogging` retarget, and — **last of all** — nothing else, because `data.Version` follows `wddm_surface::SURFACE`. |
| `kmd_render/build.rs` | **Core**. | This lane needs the bindgen allowlist to keep producing the MPO3/diagnostic types (it already does — `allowlist_type("DXGK_.*")`), and needs the WDK pinned at **28000** (§6 item 3). Also `compile_seh_shim` is deleted by core; nothing in this lane depends on it. |
| `kmd_render/src/ddi/wddm_surface.rs` | **Core** (§17.6 Modify). | The `SURFACE` const flip is the single atomic activation switch for the whole retirement. It must land exactly once, after this lane's D3/D5/D9 **and** the native-fence lane. Whoever flips it must cite both. |
| `kmd_render/src/ddi/base.rs` | Nobody — **not in the manifest**. | Owns `dxgkddi_control_etw_logging` and `dxgkddi_unload`. Both are retargeted by §12.3. Needs an owner assignment (§6 item 1). |
| `kmd_render/src/ddi/query_adapter_info.rs` | **Core** (§17.6 Modify, segments/caps). | This lane needs the 3.2 `DXGK_DRIVERCAPS` display bits (`SupportMultiPlaneOverlay` at offset 540, one past the current `REQUIRED_DRIVER_CAPS_SIZE` bound — see the warning at `:118-122`) consistent with `MaxPlanes=1`. Coordinate the `VersionedOut` bound raise with core. |
| `kmd_render/src/ddi/interrupt.rs`, `submit_command.rs` | **Core**. | `signal_crtc_vsync` (`submit_command.rs:663-681`) must gain a `DXGK_INTERRUPT_CRTC_VSYNC_WITH_MULTIPLANE_OVERLAY3` sibling for MPO3 flip retirement; both files also call `scanout_timeline::note` (D1). |
| `kmd_render/src/adapter/mod.rs`, `adapter/locks.rs` | **Core** (§17.6 Modify). | Field removals for D6/D7: `read_ledger`, `pending_vidpn_allocation`, the lease/refresh/fast-bind atomics, and the ledger's leaf lock in the lock-order comment. |
| `kmd_render/src/adapter/kobj.rs` | Nobody — **not in the manifest**. | VSync DPC; calls `scanout_timeline::note` and reads `last_primary_address`. |
| `kmd_render/src/virtio/ctrl.rs`, `virtio/gpu/mod.rs` | **Core** + this lane. | `ScanoutFlushToken`, `stage_scanout_bind`, `begin_scanout_resource_retire`, `cancel_publication_exact`, and 5 `scanout_timeline::note` sites. Standard fence minting plus exact response flag/id validation for `SET_SCANOUT_BLOB` lands here; there is no QEMU callback or wire extension (§6 item 6). |
| `kmd_logic/src/lib.rs` | **Core** (batch/queue models) + **native-fence lane** (Core-0116 lifecycle model) + this lane (plane model). | One file (**8144** lines at HEAD — `wc -l`, do not trust an older figure; this row said 5596), four lanes now that `allocation_identity` is in it. Partition by `pub mod` block and merge by whole-module insert; do not interleave edits. Delete `scanout_read_ledger` (**`:4035-4290`**) and `scanout_read_ledger_tests` (**`:4291-4519`**) as one contiguous excision — re-derive both bounds from the neighbouring `mod` starts before cutting, because this row's earlier `:4079-4334` / `:4335-4563` were wrong at both ends. |
| `protocol/src/diagnostics.rs` | **Protocol lane** (§17.1). | D0 hard-blocks on it. |

---

## 5. Build and verification

**Linux host (works today, verified 2026-08-09):**

```
cd /home/rupansh/helios-vgpu/kmd_logic \
  && CARGO_TARGET_DIR=/home/rupansh/helios-vgpu/target/linux cargo test
# -> 172 passed; 0 failed
```

That is the **only** compilable/testable part of this lane on Linux, and after
D10 it is where the plane state machine is proven.

**`kmd_render` does not build on Linux at all.** `build.rs` runs bindgen over
`ntddk.h`/`dispmprt.h`/`d3dkmddi.h`, shells to `rc.exe`, and links `displib.lib`.
There is no `cargo check` feedback loop for D0–D9 on the host. Consequences:

- Write against `tmp/dxgk_bindings.rs` (the offline struct oracle) and expect
  the first VM build to surface type errors in bulk.
- Every `unsafe` field access must be cross-checked against that file before
  it is written; there is no compiler between the author and the VM.

**Windows VM (owner-gated; do not invoke `win_*` from this lane's recon):**

```
win_build_kmd     # robocopy Z:\ -> C:\Users\Rupansh\helios-vgpu, then
                  #   cargo make --makefile Cargo.make.toml   (in kmd_render)
win_install_kmd   # re-sign + DriverStore publish + reboot (a new KMD image loads only at BOOT)
```

**Verifiable only on the VM, and only at cold boot:**

- `doc:5079` DWM starts on the 3.2 surface without `CDDisplaySwapChain`/`E_NOTIMPL`.
  A desktop screenshot (`helios_paintcap` → `Z:\tmp\screen_copy.png`) is the only
  admissible rendering evidence (CLAUDE.md rule 6).
- `doc:5083-5090` the MPO3 accept/reject matrix and the interrupt-level bound.
- `doc:5109-5112` mode/power/reset/DWM-restart/adapter-stop unbind and the
  stale/duplicate backend-callback cases.
- ETW: `tools/helios_etw_capture.ps1` (added by the tools lane, `doc:4539`).
  Until it exists there is no decode path for the provider.
- `doc:4653-4656` the generated slot audit: machine half in `ddi/wddm32_slot_audit.rs`, human half in `docs/retirement/d9-wddm32-slot-audit.md` (§6 item 2 resolved).

**Standing measurement trap (CLAUDE.md rule 6):** service-key counters persist
across boots. Any counter this lane keeps must be proven to *move this boot*
before it is read as evidence.

---

## 6. Blockers, ambiguities, and contradictions

**1. `DxgkDdiControlEtwLogging` has no manifest owner.**
§12.3 (`doc:3255-3257`) and §17.6 (`doc:4486-4488`) both retarget the callback —
"changes only an atomic enabled bit and maximum logging level" — and §17.6
assigns the provider to `diag_etw.rs`. But the callback's body is
`kmd_render/src/ddi/base.rs:44-49`, and **`base.rs` appears nowhere in §17.6's
Delete/Modify/Add lists** (`doc:4267-4315`). Same for `dxgkddi_unload`
(`base.rs:17-20`), which §12.3 (`doc:3260-3261`) makes the `EtwUnregister` site.
*Conservative reading:* `diag_etw.rs` exports the real bodies; `base.rs` keeps
only two one-line forwarders. That is still an edit to a file this lane does not
own → **CROSS-LANE REQUEST** to core.

**2. ⭐ RESOLVED 2026-08-11 — the generated `DRIVER_INITIALIZATION_DATA` audit
has one machine home and one in-tree human-review home.**
`doc:4460-4463` demands "Generate the full WDK-28000 `DRIVER_INITIALIZATION_DATA`
layout and classify every slot"; `doc:4499` demands "The generated table/flag
test enforces that invariant"; `doc:4653-4656` grades the audit. But
`kmd_render` is a `panic = "abort"` `no_std` cdylib and **cannot host a libtest
harness** (CLAUDE.md key invariant, `kmd_render/Cargo.toml` comment), and
`kmd_logic` is deliberately dependency-free with **no `wdk-sys`/bindgen edge**,
so it cannot see `DRIVER_INITIALIZATION_DATA` at all. There is no third home.
*Owner decision:* the audit is (a) `const _: () = assert!(…)` blocks
inside `kmd_render` over `offset_of!`/`size_of!` and a `const` table value —
these are compile-time and legal in `no_std` — plus (b) a checked-in generated
Markdown classification of all 192 callback slots plus `Version` (193 fields)
at `docs/retirement/d9-wddm32-slot-audit.md`, regenerated by
`kmd_render/tools/gen_wddm32_slot_audit.py`. D9 is no longer blocked on location.

**3. WDK version mismatch (blocker for every struct shape in this lane).**
The doc's baseline is WDK **28000.2526** (`doc:97`, `doc:4649-4652`,
`doc:4461`). `kmd_render/build.rs` binds against whatever WDK
`Config::from_env_auto()` finds, and the checked-in `tmp/dxgk_bindings.rs` was
generated from **26100**. Every MPO3 struct shape quoted in §2.5 is therefore
*26100* truth.

⭐ **RESOLVED 2026-08-11, and it was a real blocker, not a theoretical one.** The
VM has kits 22621 / 26100 / **28000** installed and `wdk-build` selects 28000
(`TOOLCHAIN.md` §2.1). `tmp/dxgk_bindings.rs` has been regenerated from that kit
and its provenance recorded in `tmp/dxgk_bindings.README.md`. The 26100 copy was
not merely older — it disagreed: `DXGK_OPERATION_TRANSFER2`/`FILL2`/
`DISCARD_CONTENT2` (23/24/25) do not exist in it, and
`DXGK_BUILDPAGINGBUFFER_NOTIFYRESIDENCY2` has a different shape. `FINDINGS.md`
F15 records what that cost. **Re-confirm §2.5's shapes against the new copy
before D2/D3 start.** → **CROSS-LANE REQUEST** to core (`build.rs` /
toolchain).

⭐ **PARTIALLY DISCHARGED 2026-08-11.** The two load-bearing MPO3 shapes are now
confirmed directly against `Include\10.0.28000.0\shared\d3dkmddi.h` — see item 4:
`DXGK_MULTIPLANE_OVERLAY_PLANE3` (`:6623`) and
`DXGK_PLANE_SPECIFIC_INPUT_FLAGS` (`:6410`) are unchanged from the 26100 reading
except for the version-gated `FlipImmediateNoTearing` bit. ⚠ The REMAINING §2.5
shapes — `DXGKARG_CHECKMULTIPLANEOVERLAYSUPPORT3`,
`DXGKARG_SETVIDPNSOURCEADDRESSWITHMULTIPLANEOVERLAY3`,
`DXGK_MULTIPLANE_OVERLAY_ATTRIBUTES3`, the two caps args,
`DXGKARG_POSTMULTIPLANEOVERLAYPRESENT`,
`DXGKARG_VALIDATEUPDATEALLOCATIONPROPERTY`, `DXGKARG_CONTROLMODEBEHAVIOR` — are
still 26100 truth. Confirm each **at the point of use** as D3 is written, not in a
bulk pass; that is what keeps the citation next to the code that depends on it.

**4. `DXGK_MULTIPLANE_OVERLAY_PLANE3` has no `Enabled` field, and the doc
assumes one.**
`doc:2779-2782` requires, "For an **enabled** plane … `PlaneCount=1`,
`LayerIndex=0`, `ContextCount=1`", and `doc:2790` says "Disabled/zero-plane …
paths are explicit unbinds". The bindgen shape (verified) is
`{LayerIndex, PresentId, InputFlags, OutputFlags, MaxImmediateFlipLine,
ContextCount, ppContextData, DriverPrivateDataSize, pDriverPrivateData,
PlaneAttributes}` — **no `Enabled`**, unlike `DXGK_MULTIPLANE_OVERLAY_PLANE2`
which has one. The enable signal must come from a
`DXGK_PLANE_SPECIFIC_INPUT_FLAGS` bit or from `PlaneCount == 0`; the doc names
neither. *Conservative reading:* `PlaneCount == 0` ⇒ unbind that source;
`PlaneCount == 1` ⇒ require the WDK's `Enabled` input-flag bit set and **reject
every unknown input-flag bit** with `Supported=FALSE` / a counted refusal.

⭐ **RESOLVED 2026-08-11 against the shipping 28000 header — the conservative
reading is correct and now has a citation.**
`Include\10.0.28000.0\shared\d3dkmddi.h:6623` — `DXGK_MULTIPLANE_OVERLAY_PLANE3`
is `{LayerIndex, PresentId, InputFlags, OutputFlags, MaxImmediateFlipLine,
ContextCount, ppContextData, DriverPrivateDataSize, pDriverPrivateData,
PlaneAttributes}`, **no `Enabled`** — the 26100-derived shape in §2.5 holds at
28000. The enable signal is an INPUT-FLAG bit: `:6410`
`DXGK_PLANE_SPECIFIC_INPUT_FLAGS` = `Enabled:1 (0x1)`, `FlipImmediate:1 (0x2)`,
`FlipOnNextVSync:1 (0x4)`, `SharedPrimaryTransition:1 (0x8)`,
`IndependentFlipExclusive:1 (0x10)`, then **version-gated**
`FlipImmediateNoTearing:1 (0x20)` with `Reserved:26` at
`>= DXGKDDI_INTERFACE_VERSION_WDDM2_6`, else `Reserved:27`.
⇒ D3 must pin the legal mask as **`0x3F`** at our compile level (we build against
the 28000 default, i.e. the 2_6+ layout) with a `const _: () = assert!` tying it to
the bindgen bitfield, and refuse any bit outside it. ⛔ Do NOT hardcode `0x1F`: it
is the pre-2_6 mask and would refuse a legal `FlipImmediateNoTearing`.

**5. `CheckMPO3` on an allocation that is not (yet) a live primary.**
`doc:2768-2777` says `Supported=TRUE` only for "one layer-0, same-source,
same-adapter SDR RGB primary whose exact live `hAllocation` has the full output
extent", and "Every other configuration returns `STATUS_SUCCESS`,
`Supported=FALSE`". `doc:5085` explicitly lists "stale-allocation cases" among
the reject matrix. The doc never defines "live", and never says what to return
for a handle that resolves to no `AllocationContext` at all.
*Conservative reading:* an unresolvable/foreign/wrong-magic handle is
`STATUS_SUCCESS` + `Supported=FALSE` + a named counter — never a failing
NTSTATUS, because a failing NTSTATUS from this DDI is not in the doc's stated
return set. Note the tension: CLAUDE.md wants loud failure, the doc wants
`Supported=FALSE`; the counter is the reconciliation.

**6. ⭐ RESOLVED 2026-08-11 — no custom QEMU acknowledgement and no QEMU edit.**
`qemu-helios` is immutable for this retirement. Every real replacement sends a
nonzero `SET_SCANOUT_BLOB` with `VIRTIO_GPU_FLAG_FENCE` and a unique nonzero
`fence_id`. Only a successful response carrying the fence flag and the exact
ID is the transaction boundary: it atomically latches the candidate and permits
release of the prior KMD-owned backing. ETW still emits logically distinct latch
and reader-release events keyed to that same binding sequence/fence; neither is
reconstructed from Present timing. Error, timeout, missing flag, mismatched ID,
duplicate, or stale completion cannot alter a newer binding and retains every
backing whose ownership is uncertain, with a named refusal counter.

Explicit unbind is deliberately two-step because pinned QEMU's `SET(0)` arm
neither frees `g->dmabuf.primary[scanout]` nor calls `dpy_gl_release_dmabuf`.
First fenced-replace the real current binding with one permanent, zero-filled,
KMD-owned parking/black blob. The exact success of that **nonzero replacement**
releases the real prior backing and completes the logical unbind. A fenced
`SET(0)` may then disable scanout, but its completion never releases parking;
parking remains strongly retained until a later successful nonzero replacement
or transport reset. The parking blob is bounded adapter state, not the deleted
LINEAR presentation fallback, not an OS allocation guess, and not user-visible.

**7. The classic DDI's "nonblocking backend enqueue at interrupt level" is
satisfied by dormant D4's fixed interrupt queue.**
`doc:2785-2789` requires SetMPO3 to do "bounded validation/reference-taking and
nonblocking backend enqueue" at interrupt level, and `doc:5093-5094` requires
classic `SetVidPnSourceAddress` to be "entirely nonpageable and DIRQL-safe when
MMIO flip is advertised". The legacy `stage_scanout_bind` still takes the
virtio DISPATCH spinlock and is not used by D4. Owner-enabled StartDevice instead
reserves four DMA buffers and separately boxes one interrupt queue. The classic
callback performs only the reviewed immutable/atomic validation graph, moves
the exact candidate into a fixed slot, and tries one nonblocking atomic
exclusion gate shared with every normal queue mutator. It then publishes one
direct descriptor and MMIO-notifies the device without allocation, wait,
cleanup, diagnostics I/O, or a PASSIVE/DISPATCH lock. Contention refuses; it
never spins. The raised-half capability is private, non-cloneable, !Send/!Sync,
adapter-bound, rechecks current IRQL, and has one mint in the classic DDI. A
source gate maintains an exact call/macro allowlist (including the pinned
allocator-disabled virtio queue), audits the raised and synchronized transitive
bodies, and mutation-tests hidden helpers/macros, dependency drift, forged
capabilities, guard bypasses, pointer-lifetime ordering, and forbidden calls.
The gate also keeps PCI transport status access out of the synchronized callback:
`DxgkCbSynchronizeExecution` may be entered from `<= DISPATCH_LEVEL`, whereas
`DxgkCbReadDeviceSpace`/`DxgkCbWriteDeviceSpace` are PASSIVE-only. Physical
reset therefore has a separate `PassiveLevel`-typed, raw-queue-lifetime-pinned
phase that finishes before re-entering `virtio_lock`; a mutation that raises it
under that lock or into the DIRQL callback fails.
Completion and D0 events occur in the ordinary DPC continuation after exact
token/response provenance validation. This remains disabled while
`KMD_D2_OWNER_ENABLED == false`; it is source proof, not runtime validation or
activation approval.

**8. `DestroyAllocation` versus "retain the exact allocation reference until
replacement".**
`doc:167-173` and `doc:3122` require the KMD to retain the allocation bound to a
plane until a later accepted binding replaces it. WDDM does not let a miniport
refuse `DxgkDdiDestroyAllocation`, and the doc never states what happens when
dxgkrnl destroys an allocation that is still the current plane binding. The
current code handles this by cancelling the pending bind from
`retire_scanout_allocation_locked` (`adapter/scanout.rs:1114-1200`) — the exact
shortcut `doc:5700` says to replace. *Owner decision:* the retained
reference is to the **KMD-owned host backing object** (which §17.6 `doc:4381-4383`
says the KMD continues to own), not to the dxgkrnl handle: destroy unbinds the
plane through item 6's fenced parking replacement and defers the real backing's
`RESOURCE_UNREF` until that exact nonzero completion. The permanent parking
backing itself remains retained until a later nonzero replacement or reset.

**9. 27 timeline kinds must fit 12 ETW event IDs, and the doc supplies no
mapping.**
§12.3 defines exactly 12 event IDs (`doc:3282-3287`); `scanout_timeline.rs`
currently defines 27 `kind::*` values written from 33 call sites in 7 files.
§17.6 (`doc:4326-4327`) says only "modify … into bounded ETW event construction".
No mapping is given, and 12 IDs cannot express 27 kinds.
*Conservative reading:* keep only the sites whose meaning lands on IDs 1-3
(batch), 7-9 (plane), 10 (device reset/removal); delete the rest **with** the
mechanisms they instrumented (fast bind, snapshot, windowed BLT, refresh queue,
present stream are all being deleted anyway). Any surviving site with no legal
ID is an escalation, **not** a 13th event ID — §12.3's ID list is closed.

**10. `DxgkDdiControlEtwLogging` returns `void`, so "validates its `Flags` as
zero" has no failure channel.**
`doc:3255-3257` / `doc:4486-4487` require the callback to validate that the
currently-undefined `Flags` is zero, but the WDK signature is
`fn(Enable: BOOLEAN, Flags: ULONG, Level: UCHAR)` → `VOID` (verified).
*Conservative reading:* a nonzero `Flags` **clears** the enabled bit (fail
closed — no events) and increments a named refusal counter. There is no other
legal reaction.

**11. `DxgkDdiQueryDiagnosticTypesSupport` / `DxgkDdiControlDiagnosticReporting`
are omitted from the registration list.**
`doc:4482-4484` registers `ControlEtwLogging`, `CollectDbgInfo`,
`CollectDbgInfo2`, `CollectDiagnosticInfo` — but C42 (`doc:636`) also says
"WDDM 2.7+ drivers must support its black-screen diagnostic type", and both
sibling slots exist in the 3.2 table and are NULL today. If the OS gates
`CollectDiagnosticInfo` behind `QueryDiagnosticTypesSupport`, registering the
former alone yields a callback that is never invoked — a silent stub, which
CLAUDE.md rule 2 forbids. *Conservative reading:* the D9 audit must classify
both explicitly; if `CollectDiagnosticInfo` proves unreachable without the
query slot, implement the query slot and record the evidence. Note the argument
shape differs: `CollectDiagnosticInfo`'s first parameter is
`IN_CONST_PDEVICE_OBJECT`, not the miniport context handle.

**12. The doc neither authorizes nor forbids the service-key counter mirror.**
§12.3 permits "bounded per-object counters" and `doc:5739` permits "bounded
counters only as internal state, emit exact events through C42 ETW when
enabled". `scanout_trace.rs`'s `dump`/`reset` mirror those counters into the
driver service key via `diag::record_named_bytes` — user-readable, but not an
Escape, not a shared page, not a discovery table, and it is the mechanism
CLAUDE.md rule 2 *mandates* for every refusal path. §3 (`doc:371-379`) forbids a
"global file, named-object registry, adapter/process-global resource or
synchronization discovery table" — the service key is none of those.
*Conservative reading:* keep the mirror for surviving refusal counters; delete
it for every counter whose mechanism is deleted. Flag for owner confirmation,
because "no userspace cursor/read interface" (`doc:4326`) could be read more
broadly than intended.

**13. `MaxPlanes=1` is a design proposal, and the doc says so.**
`doc:2726-2734`: the numeric profile "is a design proposal, not a Microsoft
guarantee", and DWM may reject a Display-Core adapter that advertises it. There
is no fallback (`doc:2854`: "do not fall back to a 2.1 binary"). So D9's cold-DWM
admission gate can fail with **no remediation inside this lane** — the remedy
would be a different cap profile, which is a doc change. Plan for that outcome
rather than discovering it at the gate.

**14. `GetMultiPlaneOverlayCaps`/`GetPostCompositionCaps` are per-`VidPnSourceId`
and the doc gives no out-of-range rule.**
Both args carry a `VidPnSourceId`; Helios advertises exactly one source
(`vidpn.rs:NUM_VIDPN_SOURCES = 1`). §10.8 states the values but not the
behaviour for any other source id. *Conservative reading:* return
`STATUS_INVALID_PARAMETER` with a named counter for any id other than 0, and
zero the output first.

**15. `PlaneListCount` in the Present MPO arm has no stated bound.**
`doc:2802-2805` says "bounds `PlaneListCount`" without giving the bound;
`doc:5100` only says "bounds-checked". *Conservative reading:* bound it by the
advertised `MaxPlanes = 1` — accept exactly one enabled entry, reject anything
else with a counted refusal, and never read `pAllocationList` on this arm.

**16. `PostMultiPlaneOverlayPresent` is a mandated stub.**
`doc:2794-2797` / `doc:4497-4499` require registering it as an "always-success,
bounded diagnostic callback" that "the KMD never requests", while `doc:5101-5102`
expects it to return success if invoked anyway. CLAUDE.md rule 2 forbids silent
stubs. *Resolution (not a contradiction, but it must be written this way):* mark
it `// STUB: registered by doc §17.6 mandate; unreachable while no notification
flag is set` and give it a named hit counter so an unexpected invocation is
loud.

**17. §17.6's own `cpu_host_aperture.rs` entry is internally inconsistent.**
`doc:4282-4283` lists it under **Modify** with the text "— delete". `doc:4409-4413`
resolves it as a delete. Not this lane's file (core), but noted because the same
"Modify: … — delete" idiom is used for `adapter/read_ledger.rs` at `doc:4296`,
which **is** this lane's file. Treat both as DELETE.

---

*Section 6 contains 17 items.*

**11. ⭐ RESOLVED before D0 starts — the ETW kernel API is already bound, by
`wdk-sys`, not by this crate's bindgen.**
`kmd_render/build.rs:468-476` allowlists only `DXGK.*` / `Dxgk.*` / `D3DKMT_.*` /
`D3DDDI_.*` / `D3DKMDT_.*` / `KMT_.*`, so **nothing named `Etw*` is in
`crate::dxgk`** — the vendored `tmp/dxgk_bindings.rs` has zero matches, which reads
at first like a `build.rs` cross-lane request. It is not one. `wdk-sys`'s own
generated bindings already export everything D0 needs (verified on the VM in
`target/debug/build/wdk-sys-*/out/`):

* `ntddk.rs` — `EtwRegister`, `EtwUnregister`, `EtwProviderEnabled`,
  `EtwEventEnabled`, `EtwWrite`, `EtwWriteEx`, `EtwSetInformation`,
  `EtwActivityIdControl`.
* `types.rs` — `REGHANDLE` (`:31537`), `_EVENT_DATA_DESCRIPTOR` (`:31541`),
  `_EVENT_DESCRIPTOR` (`:31633`) and their aliases.

⇒ `diag_etw.rs` imports from `wdk_sys` / `wdk_sys::ntddk`, **not** from
`crate::dxgk`, and **D0 needs no `build.rs` edit and no cross-lane request** for
its bindings. (It still needs the §4 `ddi/mod.rs` + `lib.rs` wiring and item 1's
`base.rs` forwarders.)
