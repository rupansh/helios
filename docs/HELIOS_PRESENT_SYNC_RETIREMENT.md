# Helios present synchronization retirement

Status: **C57 REMAINS CLOSED; CORRECTIVE PASS 82 DEFINES IMMUTABLE REPLY
SNAPSHOTS, RESTORES THE RUNTIME PRIMARY ASSOCIATION, AND REMOVES THE 256-USE
SEMANTIC LIMIT; HOB1 RETIREMENT, CROSS-CONTEXT ORDER, AND VIRTUAL EXECUTION ARE
EXPLICIT; IMPLEMENTATION MAY BEGIN, BUT ACTIVATION/RETIREMENT REMAINS
UNAUTHORIZED PENDING HTS1/HLM1 AND RUNTIME GATES**

Static-analysis snapshot: **2026-08-09**

> ⚠ **This file is frozen. Where a measurement or owner decision contradicts it,
> THAT WINS.** Before implementation read the **SUPERSEDED CLAIMS INDEX** and
> `docs/retirement/FINDINGS.md`; they retire F1/F2/F5/F8 assumptions in the body.
> ⛔ **OWNER AMENDMENT 2026-08-11:** `qemu-helios` is immutable for retirement;
> every custom QEMU plane latch/release callback, record, and work unit is withdrawn.
> One exact successful fenced nonzero `SET_SCANOUT_BLOB` response is the latch and
> prior-release boundary. Explicit unbind first fenced-replaces with a permanent
> KMD parking/black blob, then may `SET(0)` while retaining parking through the
> next nonzero replacement/reset. Line numbers are load-bearing; preserve count.

Target baseline: **Windows 11 26H1 (build 28000) / WDDM 3.2 / D3D12 Core
DDI 0116 or later, traditional kernel submission**

This is the normative candidate architecture and blocker reference for the
later one-shot retirement. It is frozen as the implementation reference, but
cannot authorize activation until HTS1's raw/runtime process association,
HLM1's cold two-segment admission, and the other section-18 target gates pass.
The user waived further recursive clean-pass research to begin implementation.
C57 remains closed exactly as directed:
corrective passes 76-77 identify and precisely bound its selected lower-ICD
ABI carrier and mandatory target-conformance gate. Corrective passes 78-82 do
not reopen or replace that decision. Pass 78 closes the later, independent
problem of synchronous Venus calls that occur before a D3D runtime queue
exists; pass 79 closes cross-context dispatch and virtual-submit command-byte
ambiguities; pass 80 closes the command-buffer reuse lifetime; pass 81 makes
the sole control-reply storage exception exact and session-owned and adds the
missing HQA1 context-generation cross-check. Pass 82 replaces the
under-specified continuation with an immutable bounded snapshot, restores the
runtime's unmodified primary association, and distinguishes direct WDDM
allocation operands from descriptor/object reachability so no valid
257-resource operation is truncated.
This reference now authorizes disabled-by-default implementation work, but no
deployment, adapter activation, or HPS2 removal. It supersedes every earlier
proposal in this file that used
an independent Mesa
submission plus a WDDM "proxy": a proxy cannot truthfully claim that its
`WrittenPrimaries` were written by its commands. The selected design instead
collapses translated submission onto the real runtime-owned WDDM context.
Mesa records an immutable Venus batch; the outer D3D UMD submits that batch as
the actual WDDM command; and KMD reports that submission complete only after
the corresponding host work retires.

WDDM 2.1 cannot provide the complete selected topology because the D3D12 fence
DDI supplies an opaque `D3D12DDI_HFENCE` but no documented association to the
kernel synchronization object required by the exact-context WDDM wait/signal
callbacks. Released D3D12 Core DDI 0116 closes that gap for native fences: the
runtime passes the UMD an `HRTFENCE` plus `D3DKMT_CREATENATIVEFENCE` or
`D3DKMT_OPENNATIVEFENCEFROMNTHANDLE`, including the exact process-local
`hSyncObject` and mappings. The UMD then implements queue Wait/Signal by
calling the runtime's `pfnWaitForSynchronizationObjectFromGpuCb` and
`pfnSignalSynchronizationObjectFromGpuCb` on the exact runtime-created context.
That last arrow is a **selected Helios DDI implementation rule**, not a claim
that Windows performs an undocumented automatic `HFENCE` lowering after the
UMD callback returns. Its inputs, object applicability, and context-stream
ordering are established separately by C3-C8 and C29; the later implementation
must exercise the rule directly and fail the device on any callback failure.

The design deliberately does **not** use D3D12 HWQueues/HWS for correctness.
Microsoft explicitly assigns front-buffer validation and backbuffer
synchronization semantics to ordinary `pfnSubmitCommandCb` plus
`WrittenPrimaries`; no equivalent public statement was found for
`SubmitCommandToHwQueue`. The ordinary primary-bearing command therefore
contains the actual translated work, and stock DXGI/dxgkrnl—not a Helios
fence—orders it with DWM composition, direct/independent flip, and reuse.
Evidence is labelled as follows:

- **Documented fact** — stated by a cited Microsoft, Khronos, or upstream
  primary source.
- **Local-source conclusion** — proved from the commits recorded below.
- **Selected design decision** — a mandatory rule for the later coordinated
  implementation, derived from the documented contracts and local topology.
- **Rejected / unresolved hypothesis** — not an allowed implementation basis.

## 1. Repository and commit provenance

The root and every nested graphics repository were clean before this research
file was created. The recovery-era anchors happen to remain the checked-out
commits; they were verified rather than assumed.

| Scope | Branch | Exact commit | Initial worktree |
|---|---|---:|---|
| Root `/home/rupansh/helios-vgpu` | `wddm-dx12` (ahead of `origin/wddm-dx12` by 15) | `d1c820a4a11172e1230f41e5eacc74a28b9b9491` | clean |
| DXVK `dxvk-helios` | `master` (ahead of `origin/master` by 3) | `0c71456ab0dbd6d7a04711b680d371406438995f` | clean |
| Mesa/ICD `icd/mesa` | `main` (ahead of `origin/main` by 3) | `8559b66299a8f91fcde30edfdd23310195cc7ca6` | clean |
| vkd3d-proton `vkd3d-proton-helios` | `helios` (ahead of `helios/helios` by 4) | `f3918d5e40a0e8201b223c001bd5589f667174aa` | clean |
| QEMU `qemu-helios` | detached | `d4fde50ccb5ec51a635003fda441d0ea4bbb2818` | clean |
| Looking Glass `LookingGlass` | `master` | `0bb1015122cbfd7b56ad8b571984e3adf26e2bab` | clean |
| D3D11 UMD `umd/` | root-owned | root commit above | clean |
| D3D12 UMD `umd12/` | root-owned | root commit above | clean |
| protocol `protocol/` | root-owned | root commit above | clean |
| KMD `kmd_render/`, `kmd_logic/` | root-owned | root commit above | clean |

Current released-header evidence was independently checked on 2026-08-09:

| Microsoft package | SHA-256 | Header finding |
|---|---|---|
| [`Microsoft.Windows.WDK.x86` 10.0.28000.2526](https://www.nuget.org/packages/Microsoft.Windows.WDK.x86/10.0.28000.2526) | `3432999540db204315247f8f904feebfd4a217af5529e3beea59884478f0daef` | `c/Include/10.0.28000.0/um/d3d12umddi.h:14093-14123,14294-14342,14731-14750,14926-14975` declares Core 0112 native create and Core 0116 native open |
| [`Microsoft.Windows.WDK.x86` 10.0.26100.6584](https://www.nuget.org/packages/Microsoft.Windows.WDK.x86/10.0.26100.6584) | `dfa6cf4884fc7b88d59e2d40afad876bc6b3f4954bdf449d5c0ded9e3db22db1` | the same header ends at Core build 0110; it cannot supply the selected open association |

These packages were read from `/tmp` only and are not repository inputs or
task modifications. Microsoft identifies 28000.2526 as the current supported
26H1 WDK (C7). Runtime negotiation—not the header alone—is still mandatory.

`CLAUDE.md:9-21,42-45,83-104,141-159,206-219` establishes the active
GPU-MMU/WDDM 2.1 architecture and operational prohibitions. Relevant current
history and design constraints were read at `ROADMAP.md:238-249,2205-2256,
2670-2730,2858-2905,3090-3112,3187-3240,3970-4055,4104-4106,
4197-4201`. Generated `umd*/target/`, `umd_clean/`, `.claude/worktrees/`, and
archived documents are not live source and are not in the dependency closure
unless explicitly labelled below.

## 2. Executive decision and scope

**Decision: retire HPS2 only with an atomic Windows 11 26H1/build 28000,
WDDM 3.2, D3D12 Core DDI 0116-or-later architecture uplift.** The replacement
is not a new cross-process fence registry. It is a correction of where work is
submitted and who owns presentation:

1. **D3D11 and D3D12 translated work becomes actual WDDM work.** DXVK and
   vkd3d-proton continue translating through Vulkan/Mesa, but the private
   Helios Mesa device is record-only at queue-submit time. It seals the exact
   Venus command stream and resource references and returns them downward to
   the calling UMD. It never submits that work on a second KMT render context.
   The outer UMD puts the sealed batch in the real D3D runtime command buffer.
   D3D11 calls its normal runtime Render callback with exact allocation-list
   read/write flags. D3D12 writes complete HOB1 actual commands into the
   submitted GPUVA, sends only fixed HOS1 metadata through copied private data,
   and calls ordinary `pfnSubmitCommandCb` synchronously inside
   `pfnExecuteCommandLists`, on the exact runtime-created virtual context. The
   runtime supplies the tracked ordinary presentable allocations in
   `WrittenPrimaries` on the driver's behalf, as C1/C28 require. KMD/device
   execution walks C64's
   current process page tables, executes HOB1, and raises
   `DXGK_INTERRUPT_DMA_COMPLETED` for its exact
   `SubmissionFenceId` only after the host reports completion. The WDDM
   command is therefore the command that performs the writes; it is not a
   semaphore-only proxy.
2. **D3D12 queue fences use Core DDI 0116 native-fence identity and ordinary
   context callbacks.** At fence create/open, the UMD stores the exact
   `D3DKMT_HANDLE hSyncObject` returned inside the runtime-owned native-fence
   arguments against that `D3D12DDI_HFENCE`. `pfnWaitForFence` calls
   `pfnWaitForSynchronizationObjectFromGpuCb` with the queue's exact
   `D3DDDICB_CREATECONTEXTVIRTUAL::hContext`, so subsequent command buffers
   cannot run before `completed >= value`. `pfnSignalFence` first flushes the
   preceding actual batch and then calls
   `pfnSignalSynchronizationObjectFromGpuCb` on that same context/value. CPU
   signals/waits and shared opens remain runtime/KMT native-fence operations.
   vkd3d submits no independent Vulkan queue operation and maintains no shadow
   correctness timeline. Tile mapping uses the exact paging-context fence.
3. **Present is associated with the context that performed the rendering.**
   The DirectX Resource Heaps contract requires ordinary
   `pfnSubmitCommandCb` from the same ECL thread/context and says the runtime
   uses `WrittenPrimaries` plus the context/command-queue association for
   front-buffer validation and backbuffer synchronization with DWM/scanout.
   No Helios fence/value is sent to DWM: DXGI, dxgkrnl, the exact allocation,
   and normal runtime context state carry the handoff.
4. **Display-reader lifetime becomes exact per-plane display state.** On every
   OS flip/MPO/SetVidPnSourceAddress operation, KMD/QEMU retain the exact
   allocation/backing reference bound to each VidPn source/plane until a later
   accepted binding replaces it or the plane is disabled, and the display
   backend has released its old reader. Mode, power, reset, and adapter-stop
   paths explicitly unbind every affected plane. A PresentId or flip-complete
   indication is visibility/scheduling evidence, not by itself a reader-release
   fence. Composed presentation remains ordinary DWM/D3D11 work on exact WDDM
   allocations. This bounded plane object graph retires the Escape-mapped raw
   `resid` host-reader ledger and its polling/signaler thread.
5. **The version bump includes a truthful Display-Core/MPO3 surface.** Current
   source proves that changing Helios from WDDM 2.1 to 3.2 without that surface
   sends DWM into an unimplemented `E_NOTIMPL` path. The KMD advertises and
   implements one RGB primary plane, no overlay/transform/HDR features, the
   exact MPO Present union, and the MPO3 check/set/cap callbacks, binding only
   `ppContextData[].hAllocation`. The D3D11 UMD reports matching capabilities
   and its existing D3D11.1 `CheckDirectFlipSupport` returns true only for an
   exact compatible app/DWM primary pair; every unproved pair is declined
   before promotion. A shared KMD validator then consumes the exact
   OS-supplied allocation/source on both classic SetVidPn and MPO3 paths;
   unsupported MPO shapes return `Supported=FALSE`, while an impossible
   classic mismatch after UMD admission fails loudly before latch because
   Windows promises no seamless fallback there. Neither layer enters a guessed
   allocation path. D3D12 runtime primaries preserve their documented
   any-source sentinel rather than inventing a VidPn identity. The numeric
   one-plane profile is a design proposal and build-28000 cold-DWM admission is
   mandatory before retirement.
6. **Normal native Vulkan execution moves onto a documented KMT context.** Each
   real lower Vulkan queue owns one non-virtual `D3DKMTCreateContext` context
   with `ClientHint=VULKAN` and the command/allocation buffers returned by
   Dxgkrnl. One additional ordinary context carries decoder/CPU-only device
   control commands; it is never a GPU-completion surrogate.
   Every HNR2-executing context, including that control context, owns one
   unshared monitored progress fence. Each Mesa `vn_instance` owns one
   HTS1-scoped KMD/host Venus context/object namespace, even when several
   sessions belong to one KMD process; its raw control context uses
   CPU/decode ring zero and each real queue has one distinct nonzero host
   `INFO_RING_IDX` GPU timeline. Queue submission fragments a finite,
   pointer-free HNR2 Venus stream through `D3DKMTRender`; the final COMMIT names
   every accessed WDDM allocation in its exact allocation list. KMD
   `DxgkDdiRender` validates and reassembles the stream and replaces each
   resource operand with a DMA-local physical-capability ordinal derived only
   from those allocation-list entries. After VidMm Patch supplies final
   placement, QEMU resolves the capabilities through HPM1 and substitutes host
   resource IDs only in its private command copy before one real `SUBMIT_3D`.
   A queue `DxgkDdiSubmitCommand` completes only after its
   nonzero host/Venus GPU fence; a restricted control submission completes
   only after decoder processing and reply publication and never proves GPU
   work. Queue/device idle and teardown join every affected queue milestone
   before final control destruction. Shared
   Venus rings and their head/tail polling are deleted.

   Every ordinary native-Vulkan memory allocation—including device-local
   image/buffer memory, replies, feedback, and host-visible memory—is instead
   one unshared WDDM allocation whose KMD object owns a renderer view over its
   authoritative HPM1 bytes, never a second independent HOST3D byte copy.
   VidMm places that allocation through real WDDM paging DMA in HLM1, the
   package's fully CPU-visible linear memory segment, or its ordinary
   system/aperture representation. HPM1 makes every bind/copy/fill/discard
   authoritative in QEMU. `DxgkDdiRender` emits real output patch locations
   and `DxgkDdiPatch` writes the final OS-supplied segment/physical address into
   DMA-local capability slots. `D3DKMTLock2` obtains the direct HLM1 BAR view
   or VidMm's byte-identical system backing. The package does not advertise a
   CPU Host Aperture and therefore has no ambiguous implicit/null map callback.
   Neither Mesa nor protocol bytes receive a host resource ID. This lane never
   uses Escape, `pSystemMem`,
   `ExistingSysMem`, the incompatible GPUVA `D3DKMTSubmitCommand` model, a
   second D3D context, polling, or a global submission registry.
7. **Native Vulkan WSI moves above the ICD.** A mandatory Helios Vulkan WSI
   layer owns Win32 surface/swapchain/acquire/present entry points and creates
   a real D3D12/DXGI flip swapchain on the exact adapter. Per swapchain image
   it creates one shareable committed D3D12 texture and imports its NT handle
   into the lower Helios ICD as the VkImage returned to the application. It
   also creates two single-writer D3D12 fences, `Ready[i]` and `Release[i]`,
   and permanently imports them as Vulkan
   `VK_EXTERNAL_SEMAPHORE_HANDLE_TYPE_D3D12_FENCE_BIT` timeline semaphores.
   At Present, Vulkan waits the application's semaphores, releases the image
   in the package-defined Vulkan `GENERAL` / D3D12 `COMMON` boundary state and
   signals `Ready[i]=e`;
   the layer's D3D12 queue waits that value, copies the exact shared image into
   the real DXGI backbuffer, returns the D3D view of the shared image to
   `COMMON`, signals
   `Release[i]=e`, and calls ordinary DXGI Present. The next acquire waits
   `Release[i]=e` before signaling the application's acquire semaphore/fence.
   Handles are created/opened only at image lifetime, never per Present.
   Because the app-visible device is Vulkan 1.3, the layer also implements
   C45's complete singleton `LOCAL` device-group WSI surface and real
   swapchain-memory alias images; it does not reject those core-1.1
   interactions as optional. This first generation exposes only the exact
   copy-compatible SDR profile in
   section 10.7: BGRA8 UNORM, sRGB nonlinear, FIFO, one layer, equal nonzero
   Win32 client/backbuffer extents, and no scaling, conversion, resolve,
   tearing, protected content, HDR, timing, or exclusive-fullscreen extension.
8. **The WSI layer cannot be entered by a translator.** DXVK and vkd3d load the
   Helios ICD through a private direct-dispatch entry point rather than the
   Vulkan loader. Their instances are explicitly tagged record-only and have
   no Win32 WSI extensions. Thus the call graph is acyclic:

       native Vulkan app -> loader -> Helios WSI layer
         -> D3D12/DXGI -> UMD12 -> vkd3d -> direct Helios ICD (record-only)
         -> outer WDDM context
       native Vulkan app -> layer -> lower Helios ICD (normal app device)

   There is no `ICD -> DXGI/D3D` edge. The current
   `wsi_common_win32.cpp` D3D11/DComp vehicle is deleted, not adapted.
9. **Record-only translation has an explicit pre-queue control carrier.** One
   bounded `TranslationSession` (`HTS1`) is created for each Mesa
   `vn_instance`, never for an entire process and never per resource. The
   session owns one raw legacy-KMT `HVC1` control context and one distinct
   virgl/Venus host context, preserving the host decoder's one-`VkInstance`
   object namespace. Pure synchronous control requests needed before a D3D
   queue exists—instance/device/object creation, requirements, and bounded
   compile results—use finite `D3DKMTRender` messages on that control context
   and a C51 event-backed reply into the session's exact 64-MiB raw-device HVM1
   reply/feedback pool allocation. That allocation is the only WDDM allocation a
   pure-control command may write; it is never an outer D3D resource. Every
   outer D3D runtime context is attached
   once, at its documented create-context callback, with a fixed pointer-free
   `HQA1` packet carrying an unguessable session capability and the selected
   physical lower-queue endpoint. KMD requires the raw and runtime devices to
   have the identical `hKmdProcess`, adapter, package generation, and session
   generation, then stores a direct strong session/endpoint reference in the
   context; no submit-time capability lookup occurs. Render private data is
   reserved-zero and is never used for attachment. Allocation-backed Vulkan
   operations remain deferred into the first actual outer WDDM batch that
   names the exact allocation; the control context cannot borrow or discover
   an outer allocation. GPU-dependent synchronous calls first join the exact
   outer context's HQC1 milestone, which is ordered after the corresponding
   nonzero endpoint, and only then issue a bounded control fetch. Ring zero and
   the raw control context's C51 prove decoder/reply completion only, never
   outer GPU completion.

This decomposition replaces each HPS2 responsibility with its documented
owner:

| HPS2 responsibility | Selected owner/mechanism |
|---|---|
| Exact resource identity | WDDM allocation handle/GPUVA plus immutable create/open private data on D3D paths; one exact D3D12-resource NT handle per WSI image |
| Producer sync discovery | None on D3D paths; direct lifetime transfer of two WSI fence handles to the one consenting peer |
| Fence value publication | D3D12 Core-0116 native-fence object/value calls and normal WDDM context ordering; direct per-image epochs in the WSI layer |
| PID/process generation | Kernel/COM/Vulkan object lifetime; no PID key |
| Update/release/reclaim | Queue, allocation, swapchain, fence, and NT-handle destruction |
| Producer-to-consumer ordering | Actual outer queue completion before DXGI Present; Ready/Release only inside the native-WSI component |
| DWM cross-process handoff | Stock DXGI/WDDM allocation open, create-time private data, and runtime Present synchronization |
| Backbuffer/scanout reuse | DXGI/WDDM swapchain availability plus exact per-VidPn-source/MPO-plane allocation retention until replacement or unbind |
| Native Vulkan command/completion transport | One CPU/decode-only legacy KMT control context plus one per real lower queue; `D3DKMTRender` plus exact allocation list; ring-0 processing gates only control/reply completion, nonzero host-queue completion gates GPU scheduler work, and one explicit unshared monitored fence per HNR2 context supplies only that context's requested milestone |
| Native Vulkan memory/command/reply transport | Every ordinary device-local or host-visible `VkDeviceMemory` is one exact HVM1 WDDM allocation and KMD renderer view. HPM1 makes actual WDDM paging DMA authoritative for every placement; only host-visible roles expose Lock2 over the direct HLM1 BAR or byte-identical system view. No CPU Host Aperture, raw user-mode host ID, or second byte authority exists |
| Pre-queue translated control and namespace | One bounded HTS1 session and one raw HVC1 control context per Mesa `vn_instance`; finite event-backed pure-control replies into exact session-owned scratch before a D3D queue exists; one-time HQA1 create-context attachment of each exact outer context to its physical lower-queue endpoint; outer-allocation-backed work remains on that outer context |
| Crash/reset/DWM restart | dxgkrnl device/context teardown and normal DXGI/Vulkan error propagation |
| Security | OS resource sharing to DWM; unnamed same-process WSI handles only |
| Diagnostics | ETW/WDDM context/native-fence events, exact plane latch/release events, and bounded per-object counters |

The required version/features are all-or-nothing:

- Windows 11 26H1 OS build 28000 or later and a fully conformant WDDM 3.2
  KMD/UMD package;
- the complete active-display surface required by that reported level,
  including the exact one-primary-plane Display-Core/MPO3 contract in C41;
  changing only `WddmSurface`/version/caps is forbidden, and failure of the
  cold-DWM admission gate rejects the package;
- D3D12 Release 8 Core DDI 0116 or later negotiated at runtime;
- `D3D12DDICAPS_TYPE_0112_NATIVE_FENCE_SUPPORT` reported truthfully and
  `DXGK_FEATURE_NATIVE_FENCE` successfully enabled, including the complete
  KMD create/open/close/destroy/update/interrupt surface;
- traditional kernel submission through `pfnSubmitCommandCb`; the package
  must not select HWS/HWQueue submission for this architecture;
- every supported D3D12 fence reaches the UMD as `NATIVE` or `OPENED_NATIVE`;
  the legacy `MONITORED` branch and optional non-monitored-fence capability are
  refused rather than guessed;
- a lower Vulkan 1.3 device with `timelineSemaphore` and `synchronization2`
  explicitly enabled, the Win32 external-memory/external-semaphore device
  extensions enabled, and exact D3D12-resource/fence IMPORTABLE queries
  successful, plus C45's singleton device-group WSI and swapchain-memory
  alias contract, plus the complete C50-C67 Windows Vulkan lane: legacy KMT
  Render, per-context progress fences, finite HNR2 submission, direct-linear
  HLM1 storage with real HPM1 paging, one HTS1 session per `vn_instance`,
  HQA1 outer-context attach, and HQC1 synchronous progress. C57's
  D3D12-resource import carrier is
  published and selected with an explicit documentation-conflict gate: the lower ICD acquires the exact local allocation
  through `D3DKMTQueryResourceInfoFromNtHandle` plus
  `D3DKMTOpenResourceFromNtHandle` on its own KMT device, under a fail-closed
  admission check. The package also
  advertises no CPU Host Aperture callback or protocol; and
- the WSI layer, direct translator dispatch, record-only Mesa submit mode,
  D3D12/Vulkan external memory and native-fence support, KMD/QEMU batch
  completion, and exact display-plane lifetime are installed together.

WDDM 2.1 is not a compatibility target for the retirement. It already has the
ordinary submission and exact-context wait/signal callbacks, but its D3D12
fence DDI never gives this UMD the kernel synchronization-object identity those
callbacks require. Core 0116 plus WDDM 3.2 native fences supplies that missing
association. It does not transport a user fence to DWM; no such transport is
needed once the actual producer work is the normal primary-bearing WDDM
submission that DXGI presents.

The deployment is one shot. Old and new UMDs, ICD, WSI layer, KMD, host
protocol/QEMU, installer, and manifests may not interoperate. There is no dual
publish, old/new reader compatibility, HPS fallback, or feature-disable
fallback. If activation cannot guarantee the full feature set, the adapter
must not start the new package.


## 3. Hard constraints and non-goals

- No `D3DKMTEscape` for transport, discovery, metadata, synchronization,
  completion, lifetime, diagnostics, or fallback.
- No global file, named-object registry, adapter/process-global **resource or
  synchronization discovery table**, service, polling thread, sleeps,
  heuristic identity, or scan by PID, dimensions, HWND, timing, creation
  order, or queue order. The exact KMD `ProcessContext` may contain only a
  bounded list of HTS1 translation sessions for one-time context-creation
  admission; it contains no allocation/resource identity and is never queried
  by submit or Present after HQA1 has installed a direct context reference.
- No semaphore-only, ticket-only, or delayed-completion command may claim
  `WrittenPrimaries`. The WDDM command carrying that association must contain
  the actual translated work.
- No ICD call to DXGI, D3D11, or D3D12. The WSI layer is a separate module
  above the ICD. Translator instances bypass the Vulkan loader/layer entirely.
- No CPU wait on GPU completion in the steady-state render or Present path.
  API-defined acquire/destruction waits use events, never sleeps or polling.
- No assumption that DWM remains in composed mode. DXGI/WDDM owns composed,
  direct, independent, windowed, and fullscreen transitions.
- No cross-adapter inference. Every bridge validates the exact adapter LUID
  and Vulkan device/driver UUID; a mismatch fails.
- No version or feature fallback. Windows build 28000+, WDDM 3.2, Core DDI
  0116+, and native-fence negotiation are part of the package ABI.
- No implementation, build, deployment, guest change, registry change, adapter
  restart, reboot, commit, or push is part of this research task.
- Never delete the old mapped file while any legacy process can still map it.

Non-goals are preserving the current recursive DComp vehicle, preserving the
Escape-based graphics/present stream, maintaining WDDM 2.1 compatibility, or
inventing a custom DWM synchronization protocol. Broad native-Vulkan WSI
feature coverage is also not part of the retirement: the copy-only generation
advertises only the exact section-10.7 SDR/FIFO profile, while ordinary D3D
applications retain stock DXGI windowed/fullscreen behavior. The version
uplift is causal:
Core 0116 supplies the exact D3D12-fence-to-kernel-object association that 2.1
lacks, while ordinary WDDM submission remains the presentation domain.

## 4. Current HPS2 binary ABI and lifecycle

### 4.1 Wire format

**Local-source conclusion:** the two byte-compatible definitions are
`dxvk-helios/src/dxvk/dxvk_helios_present_sync.cpp:19-65` and
`icd/mesa/src/vulkan/wsi/wsi_helios_present_sync.c:22-51`.

| Offset | Bytes | Field | Meaning |
|---:|---:|---|---|
| 0 | 4 | `header.magic` | `0x32535048` (`HPS2`) |
| 4 | 4 | `header.slotCount` | exactly 4096 |
| 8 | 24 | `header.reserved[6]` | zero/unused |
| 32 + 32n + 0 | 4 | `slot.seq` | odd while writer owns slot; even when readable |
| 32 + 32n + 4 | 4 | `slot.resid` | Venus resource ID; zero means free |
| 32 + 32n + 8 | 4 | `slot.pid` | producer PID |
| 32 + 32n + 12 | 4 | `slot.fenceId` | named-fence ID; bit 30 is `kwaitOrdered` |
| 32 + 32n + 16 | 8 | `slot.value` | producer timeline value |
| 32 + 32n + 24 | 8 | `slot.producerStart` | process creation `FILETIME` |

The mapping is 131,104 bytes (`32 + 4096 * 32`). The state word is the aligned
64-bit pair `(resid << 32) | uint32(seq)`. There is **no hash function and no
probe sequence**: every operation linearly scans slots 0..4095. Writers CAS an
even state to odd, write the payload, then CAS odd to the next even sequence;
freeing is a second CAS that changes `resid` to zero. Readers accept a payload
only when the two state reads match and the sequence is even
(`dxvk_helios_present_sync.cpp:177-279,438-468`).

### 4.2 Mapping, path, and security

- Both implementations use `HELIOS_PRESENT_SYNC_PATH` when non-empty;
  otherwise they use
  `C:\ProgramData\Helios\helios_present_sync_v2.bin`
  (`dxvk_helios_present_sync.cpp:76-80`,
  `wsi_helios_present_sync.c:64-80`). An override receives no default ACL
  creation/repair. DXVK stores an arbitrary-length override in `std::string`;
  Mesa uses a `MAX_PATH` buffer and does not reject the
  `GetEnvironmentVariableA` required-size return, so an overlong override does
  not safely fall back and can simply disable its HPS path.
- The file is opened read/write with read/write/delete sharing, extended by
  `CreateFileMapping` and permanently mapped once per process
  (`dxvk_helios_present_sync.cpp:120-175`,
  `wsi_helios_present_sync.c:77-114`). Neither implementation unmaps it during
  normal process lifetime.
- DXVK creates/repairs a protected DACL granting `SY`/`BA` generic-all and
  Authenticated Users, Local Service, and the Window Manager group read/write
  (`dxvk_helios_present_sync.cpp:25-37,82-118`). Mesa does no ACL work. The
  installer independently creates the file and applies Modify rights to the
  same cross-principal users at `packaging/windows/Install-Helios.ps1:198-217`.
- The first initializer CASes magic from zero, then writes slot count. Other
  initializers call `Sleep(0)` up to 4096 times waiting for slot count
  (`dxvk_helios_present_sync.cpp:151-169`,
  `wsi_helios_present_sync.c:95-109`). Foreign magic or slot count disables the
  caller’s HPS path.

### 4.3 IDs, publication, lookup, release, and reclamation

- The D3D11 bridge allocates a DLL-local low-half ID and names its exported
  timeline `Global\HeliosPresentFence_<pid>_<processStart>_<id>`
  (`umd/bridge/dxvk_bridge.cpp:1188-1233`). The WSI writer allocates
  `0x80000000 | InterlockedIncrement(counter)` and uses the same naming scheme
  (`wsi_helios_present_sync.c:57-62`,
  `wsi_common_win32.cpp:998-1054`). Bit 30 in the stored DXVK ID advertises the
  historical `kwaitOrdered` property; lookup strips it. It is metadata, not a
  fence property (`dxvk_helios_present_sync.cpp:19-21,266-272,458-465`). No
  live `HeliosPresentSync::publish` call passes `true`, and the C WSI writer has
  no kwait parameter, so the bit is not currently produced intentionally.
  Neither 32-bit fence-ID counter checks wrap; after enough allocations the
  namespace/bit overlays can alias.
- Publish makes up to eight attempts. Each attempt scans the whole table for an
  existing `resid`, then a free slot, then a provably dead producer. Worst-case
  DXVK work is 98,304 slot visits plus liveness syscalls; Mesa has the same scan
  shape (`dxvk_helios_present_sync.cpp:330-399`,
  `wsi_helios_present_sync.c:247-313`). DXVK logs the first and each 512th full
  failure; Mesa silently returns false.
- Lookup scans all 4096 slots and retries a matching unstable slot up to eight
  times. No liveness check occurs on lookup. The consumer then constructs the
  global name and imports it, negative-caching failure for 256 calls
  (`dxvk_helios_present_sync.cpp:438-468`,
  `dxvk_context.cpp:9781-9826`).
- Release is owner-qualified by exact `(resid, current PID, process creation
  time, fence ID)`, clears the payload, then frees the `resid`
  (`dxvk_helios_present_sync.cpp:402-435`,
  `wsi_helios_present_sync.c:315-349`). DXVK ties this to the exact backing
  allocation destructor (`dxvk_memory.cpp:278-287`; marker storage at
  `dxvk_memory.h:609-640,728`). WSI releases before destroying each image
  (`wsi_common_win32.cpp:1591-1609`).
- `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)` plus process creation time
  protects PID reuse. Only `ERROR_INVALID_PARAMETER` proves death; access
  refusal preserves the slot (`dxvk_helios_present_sync.cpp:194-233`,
  `wsi_helios_present_sync.c:116-163`). DXVK additionally sweeps once at first
  map; Mesa only opportunistically reclaims during publish
  (`dxvk_helios_present_sync.cpp:281-325`). A crash leaves persistent state
  until a later writer can prove death.
- Slot sequence, fence-ID counters, and producer timeline values use wrapping
  integer increments without an exhaustion transition. The even/odd seqlock
  continues modulo 2^32 but has no formal ABA bound; fence IDs and 64-bit values
  can eventually reuse. These are current-protocol defects, not behavior the
  replacement preserves; the selected generation fails the device before
  queue sequence/value wrap.

### 4.4 Current happens-before claims and failure arms

The intended relation is:

```text
producer GPU writes
  -> producer timeline signal(value) recorded on its Vulkan queue
  -> publish(resid, producer-generation, fence-id, value)
  -> consumer lookup/import
  -> consumer GPU wait(value)
  -> consumer sample/copy
```

Publication occurs after the signal is recorded but before it necessarily
executes (`umd/bridge/dxvk_bridge.cpp:1301-1327`; WSI pre-present signal at
`wsi_common.c:2647-2665`, publish at
`wsi_common_win32.cpp:2124-2138`). This avoids advertising a never-recorded
value but depends on the global file to correlate the allocation to that value.

Failure is deliberately non-fatal in several consumer arms: a missing slot or
failed named-fence import skips ordering; the bounded refresh wait times out and
copies anyway (`dxvk_context.cpp:9628-9668`); a table-full publish drops the
update. These arms make HPS2 incapable of being the authoritative correctness
boundary.

## 5. Exhaustive direct and indirect dependency inventory

The normative `file:line`, reachability, role, identity, ordering, failure,
lifetime, and cost inventory is Appendix A. It is kept after the future-change
manifest so current behavior cannot be mistaken for the selected replacement.
Appendix A includes direct HPS2 code, indirect raw-resource-ID and named-fence
plumbing, the recursive vehicle, graphics Escapes, the D3D12 marker path, and
the `resid`-keyed host-reader ledger. The selected generation removes that
ledger and routes every real display read through exact OS flip status plus
bounded per-VidPn-source/MPO-plane allocation ownership as described in
sections 10.8 and 11.4.

## 6. Current producer/consumer sequence diagrams

Appendix B is the normative current-state sequence set. It covers Vulkan WSI
vehicle production, D3D11 producer/consumer operation, DX12/vkd3d production
to DWM's D3D11 composition path, and orderly/crash cleanup. The selected
replacement topology is separately shown in section 11.

## 7. Responsibility decomposition

HPS2 is not one primitive. The following is the complete responsibility map
after the final source and contract audit.

| Responsibility | Current writers / readers / owner | Current failure | Selected replacement and owner |
|---|---|---|---|
| Correlate an exact shared allocation | UMD/WSI publish a Venus `resid`; DXVK consumers scan HPS2 | raw ID reuse, absent ID, cross-device ambiguity | D3D: exact runtime WDDM allocation handle/GPUVA plus immutable KMD create-time private identity. Native WSI: exact per-image D3D12 NT resource handle imported once. |
| Discover producer synchronization | producer creates a globally named Vulkan timeline; consumer reconstructs a name | ACL/name/open failure silently drops ordering | No discovery on D3D paths. Native WSI directly transfers two unnamed fence handles while creating each image. |
| Publish the producer value | producer mutates a global slot; consumer reads it | seqlock retry, table full, stale value, O(4096) scan | D3D: no published custom value; actual WDDM submission/Present dependency. API fences use exact Core-0116 values. WSI epoch is image-local. |
| Defend against PID reuse | PID plus process start time | access denial and stale mapped entries | Kernel object, COM, and Vulkan object lifetime; no PID. |
| Update/release per resource | publish/release on image/backing destruction | crash omits release; reuse races | WDDM allocation/GPUVA and swapchain object lifetime. |
| Reclaim stale producers | publish-time process query and startup scan | global CPU work and uncertain liveness | dxgkrnl process/device/context teardown. |
| Order D3D producer work | independent Vulkan submit plus a custom timeline | outer runtime queue and lower queue are different causal streams | Record-only Mesa submit; one actual outer WDDM context submission. |
| Execute normal native Vulkan work | ICD serializes Venus commands through private Escape verbs and separately probes an otherwise unused KMT context | deleting Escape would leave no execution, allocation-reference, or completion lane | one CPU/decode-only legacy KMT control context plus one per real lower queue; returned command/allocation lists + `D3DKMTRender`; KMD ring-0 completion gates only control processing/reply, each nonzero queue fence gates its exact GPU scheduler submission, and every HNR2 context owns an explicit monitored fence for that context's promised completion class |
| Preserve D3D12 Queue Wait/Signal | current vkd3d Vulkan queue plus incomplete outer shadow | WDDM 2.1 HFENCE has no proven KMT-handle mapping; proxy may overtake | Core-0116 native `hSyncObject` plus exact-context runtime callbacks; no lower queue. |
| Associate primary writes | HPS2 resource ID and proxy proposals | a no-op proxy did not write the primary | Actual batch is the WDDM command; runtime supplies exact `WrittenPrimaries`. |
| Transfer to DWM | DWM-side DXVK scans HPS2 | no custom handle carrier; mode dependent | Stock DXGI/WDDM Present and resource-open path only. |
| CPU-to-GPU ordering | publication after enqueue; optional flush gates | publication can precede completion; CPU stalls | Runtime call order plus exact-context wait/signal and submission-fence completion; asynchronous dependencies. |
| GPU-to-GPU ordering | Vulkan timeline imported by another ICD device | custom name/handle and independent contexts | D3D: one queue. WSI: documented D3D12 fence/Vulkan D3D12-fence import pair. |
| `kwaitOrdered` provenance | bit 30 of custom fence ID | dormant/ambiguous mutable metadata | Deleted; WDDM queue and Present order are authoritative. |
| Backbuffer reuse | HPS value, vehicle release gate, DXGI behavior | timeout/copy-anyway and stale image | DXGI swapchain availability; WSI `Release[i]` before acquire. |
| Direct-scanout reader lifetime | Escape-mapped `resid` ledger and polling signaler | fixed capacity, polling, stale/raw identity | Exact per-plane KMD/QEMU binding and backend reader lease released only after replacement/unbind plus backend release. |
| Object ownership | named Vulkan semaphore plus mapping/cache | leaked names/mappings and crash residue | Queue/allocation/swapchain-owned kernel objects; WSI imports own references. |
| Cross-process access | ProgramData ACL and global named object | overbroad ACL; deployment skew | OS-controlled DXGI sharing to DWM; custom WSI handles remain unnamed and same-process. |
| Reset/removal/restart | slot reclamation and timeout fallback | false completion or stale slot | fail-closed device loss; dxgkrnl teardown; DXGI/Vulkan recreate. |
| Diagnostics | HPS counters and timeouts | several correctness failures continue | ETW, submission-fence/native-fence events, exact plane-lifetime events, bounded per-context/per-swapchain counters. |
| Scaling | global file/map/scan and shared locks | work grows with all processes/resources | per queue, per allocation, or per swapchain image only; no global hot object. |

Current writers are the D3D11 UMD bridge and native WSI producer; current
readers are DWM/application D3D11 DXVK contexts and the recursive WSI vehicle.
D3D12 currently has no HPS2 writer, which is a missing edge rather than proof
that it needs none. Appendix A lists each exact callsite.

## 8. Official Windows/WDDM and Vulkan contract findings

The following table is the normative authority ledger. “Documented” means the
linked Microsoft/Khronos contract or checked WDK declaration states it.
“Inference” is explicitly identified and is not silently promoted to a
Windows guarantee.

| ID | Finding and primary evidence | Version / restriction | Consequence |
|---|---|---|---|
| C1 | Ordinary `D3DDDICB_SUBMITCOMMAND` carries command GPUVA/length and `WrittenPrimaries`; the DirectX Resource Heaps contract says the runtime tracks presentable allocations for front-buffer validation/backbuffer synchronization, passes those allocation handles in `WrittenPrimaries` on behalf of the driver, and requires this callback synchronously inside `pfnExecuteCommandLists` on the context created for that command queue. A UMD adds handles only for the exceptional primaries it creates itself. [Microsoft structure](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dumddi/ns-d3dumddi-_d3dddicb_submitcommand), [Microsoft Resource Heaps](https://microsoft.github.io/DirectX-Specs/d3d/ResourceHeaps.html#submitcommandcb-cannot-pass-more-than-8-handles-in-writtenprimaries) | WDDM 2.0+ GPUVA path; at most eight primaries per submit | The HOB1 bytes at that exact GPUVA are the actual translated work, never a marker/proxy. The UMD preserves the runtime-visible resource/write association of the command lists and does not fabricate ordinary runtime-primary handles. KMD/device validation independently resolves HOB1 GPUVA operands; it does not claim access to the runtime-owned primary list absent from `DXGKARG_SUBMITCOMMANDVIRTUAL`. |
| C2 | KMD receives a normal virtual-context submission with a `SubmissionFenceId`; `DXGK_INTERRUPT_DMA_COMPLETED` reports the last completed submission fence. [Microsoft completion type](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmddi/ns-d3dkmddi-_dxgkargcb_notify_interrupt_data) | normal WDDM context scheduling | Helios reports completion only when the exact Venus/host batch has completed. A timer/rebase may trigger TDR/removal but may never forge completion. |
| C3 | D3D12 create-device gives the UMD `pKTCallbacks`, explicitly the runtime callbacks that invoke kernel; the WDDM 2.x table includes `pfnWaitForSynchronizationObjectFromGpuCb`, `pfnSignalSynchronizationObjectFromGpuCb`, `pfnCreateContextVirtualCb`, and `pfnSubmitCommandCb`. Released WDK 28000: `d3d12umddi.h:2665-2671`; `d3dumddi.h:4562-4581`. [Microsoft create-device argument](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dumddi/ns-d3dumddi-_d3dddiarg_createdevice) | WDDM 2.0+ callback table | The D3D12 UMD can issue documented WDDM waits/signals on its exact runtime context; it does not call direct KMT with an incompatible context-handle type. |
| C4 | Released D3D12 Core DDI 0112 adds native-fence capability/type selection and passes `D3DKMT_CREATENATIVEFENCE*` plus `D3D12DDI_HRTFENCE` to `pfnCreateFence`; the Core callback creates the exact native object for that runtime fence. Released WDK 28000 `d3d12umddi.h:14093-14123,14294-14342`. | D3D12 Release 8 build 0112 | For `NATIVE`, the UMD stores the returned process-local `hSyncObject` and mappings against its exact `HFENCE`; no cast, PID, or lookup is involved. |
| C5 | Core DDI 0116 adds `OPENED_NATIVE`, `D3DKMT_OPENNATIVEFENCEFROMNTHANDLE*`, and `pfnOpenNativeFenceCb(hRTFence,...)`; the open returns a new local `hSyncObject` and mappings for the opener. Released WDK 28000 `d3d12umddi.h:14731-14750,14926-14975`. [Microsoft open structure](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmthk/ns-d3dkmthk-d3dkmt_opennativefencefromnthandle) | D3D12 Release 8 build 0116 | Shared D3D12 fences have an exact per-process association. Handles still cross only through documented D3D12/KMT share/open operations, never allocation private bytes. |
| C6 | Native fences have KMD create/open/close/destroy DDIs, process-local mappings, reference-counted global lifetime, CPU/GPU waits/signals, and cross-process/default cross-adapter sharing. Microsoft says `D3DDDI_NATIVEFENCE_TYPE_DEFAULT` supports **all existing D3DKMT synchronization-object Wait/Signal operations from CPU and GPU**; `D3DKMT_CREATENATIVEFENCE::hSyncObject` is the returned process-local synchronization-object handle. [Microsoft native-fence contract](https://learn.microsoft.com/en-us/windows-hardware/drivers/display/native-gpu-fence-objects), [create operation](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmthk/nf-d3dkmthk-d3dkmtcreatenativefence), [create structure](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmthk/ns-d3dkmthk-d3dkmt_createnativefence), [mapping](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dukmdt/ns-d3dukmdt-d3dddi_nativefencemapping) | Windows 11 24H2 / WDDM 3.2 feature; OS negotiation mandatory; selected type is DEFAULT only | Core 0116 supplies the exact native object that the existing runtime synchronization callbacks can name. Require the native feature and 64-bit atomic support. Reset/removal destroys the epoch and mappings; never treat `UINT64_MAX` after removal as completion. |
| C7 | Microsoft lists WDK `10.0.28000.2526` as the current supported Windows 11 26H1 kit; WDK 26100 ends at Core build 0110, while the released 28000 header declares 0116. [Supported WDKs](https://learn.microsoft.com/en-us/windows-hardware/drivers/other-wdk-downloads), [WDK release notes](https://learn.microsoft.com/en-us/windows-hardware/drivers/wdk-release-notes), [Windows release information](https://learn.microsoft.com/en-us/windows/release-health/windows11-release-information) | Core 0116 must be negotiated, not inferred merely from OS version or Agility SDK | Selected deployment floor is build 28000 plus successful 0116 negotiation. 24H2/25H2 and Core <=0110 are rejected, not fallback targets. |
| C8 | `pfnWaitForSynchronizationObjectFromGpuCb` and `pfnSignalSynchronizationObjectFromGpuCb` are the WDDM-v2 replacements that Microsoft says support **all synchronization-object types**. Their structures carry exact runtime `HANDLE hContext`, `D3DKMT_HANDLE[]`, and values. Context Monitoring requires the UMD to flush pending commands before the wait; Dxgkrnl records the dependency, returns immediately, and does not schedule later context command buffers until it is satisfied. For a software signal packet the UMD flushes prior commands and queues the signal to that context. D3D12 separately requires workload A to finish before an API fence operation inserted between Execute calls A and B. [Microsoft wait callback](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dumddi/nc-d3dumddi-pfnd3dddi_waitforsynchronizationobjectfromgpucb), [wait arguments](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dumddi/ns-d3dumddi-d3dddicb_waitforsynchronizationobjectfromgpu), [signal callback](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dumddi/nc-d3dumddi-pfnd3dddi_signalsynchronizationobjectfromgpucb), [Context Monitoring](https://learn.microsoft.com/en-us/windows-hardware/drivers/display/context-monitoring), [D3D12 Execute ordering](https://learn.microsoft.com/en-us/windows/win32/api/d3d12/nf-d3d12-id3d12commandqueue-executecommandlists) | Windows 10+/WDDM 2.x callbacks; object supplied by C4/C5; selected queue is traditional kernel submission | **Selected design composition:** `pfnWaitForFence` flushes earlier actual work and issues the exact-context wait before later submits; `pfnSignalFence` flushes/submits all earlier actual work and then issues the exact-context signal. This is explicit UMD work, not an inferred runtime no-op or HWQueue broadcast. |
| C9 | Hardware flip queue queues future PresentIds and reports latch/cancel, but its public contract does not make PresentId completion a resource-reader-release fence. [Microsoft hardware flip queue](https://learn.microsoft.com/en-us/windows-hardware/drivers/display/hardware-flip-queue), [flip log entry](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmddi/ns-d3dkmddi-_dxgk_flipqueue_log_entry) | optional WDDM 3.0+/HWS surface, outside the selected normal-context retirement path | It may be implemented later as a display optimization, but is not a correctness dependency. Exact KMD/QEMU plane references own scanout lifetime. |
| C10 | Flip-model buffers are shared with DWM; independent flip can bypass DWM user-mode composition and later fall back to composition. [Microsoft flip model](https://learn.microsoft.com/en-us/windows/win32/direct3ddxgi/dxgi-flip-model), [performance/transition guidance](https://learn.microsoft.com/en-us/windows/win32/direct3ddxgi/for-best-performance--use-dxgi-flip-model) | DXGI 1.2+ | No design may rely on a permanent DWM consumer. Stock DXGI owns transitions. |
| C11 | WDDM OpenResource delivers the receiving UMD an exact KM resource plus resource/allocation private data; no synchronization-object or arbitrary NT-handle field exists. [Microsoft OpenResource argument](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dumddi/ns-d3dumddi-_d3dddiarg_openresource), [OpenResource callback](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dumddi/nc-d3dumddi-pfnd3dddi_openresource) | core WDDM resource open | Reconstruct the exact D3D12-created allocation in DWM's D3D11 UMD. Do not embed a process-local HANDLE in private bytes. |
| C12 | `D3DDDI_ALLOCATIONINFO::pPrivateDriverData` is explicitly `[in/out]`: KMD may return data in the create-time buffer. `D3DDDI_OPENALLOCATIONINFO` then gives the opener the private data passed to KMD when the resource was created. During `DxgkDdiOpenAllocation`, KMD may modify allocation private data only when `Flags.Create` is set; an ordinary open is read-only. [Microsoft create allocation info](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dukmdt/ns-d3dukmdt-_d3dddi_allocationinfo), [Microsoft UMD open info](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dukmdt/ns-d3dukmdt-_d3dddi_openallocationinfo), [Microsoft KMD open contract](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmddi/nc-d3dkmddi-dxgkddi_openallocationinfo) | same logical adapter; write only at creation | KMD returns one immutable, versioned descriptor at creation. The exact kernel allocation/resource remains the identity and KMD owns its host backing; private bytes contain no host token or independently usable identity. Receiving UMDs consume the descriptor only while paired with the exact `hKMResource`/`hAllocation`; dynamic synchronization remains WDDM queue state. Delete current ordinary-open restamping. |
| C13 | Resource Heaps states that the runtime tracks presentable allocations and uses normal synchronization for application rendering, DWM composition, and scanout; D3D12-created primaries must be created with `AllocateCb` and open as equivalent resources through the D3D11 path on the same adapter. [Microsoft DirectX Resource Heaps](https://microsoft.github.io/DirectX-Specs/d3d/ResourceHeaps.html) | D3D12/WDDM GPUVA | Preserve and harden the current D3D12 create-time allocation-private data -> D3D11 OpenResource path. Implement reverse `pfnOpenHeapAndResource` for full interop, not as a substitute carrier. |
| C14 | A D3D12 shared committed resource or shared heap is exported with `ID3D12Device::CreateSharedHandle`; the receiving component opens the exact NT handle. The only accepted `Access` is `GENERIC_ALL`; `pAttributes=NULL` selects the creator-token default descriptor and makes the handle non-inheritable. [Microsoft shared handles](https://learn.microsoft.com/en-us/windows/win32/api/d3d12/nf-d3d12-id3d12device-createsharedhandle), [shared heaps](https://learn.microsoft.com/en-us/windows/win32/direct3d12/shared-heaps) | exact adapter/device restrictions; cross-adapter is narrower | WSI image/fence handles are lifetime-scoped, unnamed, non-inheritable, same-process, and exact. Do not claim a narrower access mask than the API accepts. |
| C15 | Vulkan `D3D12_RESOURCE_BIT`/`D3D12_HEAP_BIT` imports D3D12 NT handles and requires compatible device and driver UUIDs; D3D12-resource imports are dedicated. [Khronos external memory types](https://docs.vulkan.org/refpages/latest/refpages/source/VkExternalMemoryHandleTypeFlagBits.html), [external memory features](https://docs.vulkan.org/refpages/latest/refpages/source/VkExternalMemoryFeatureFlagBits.html) | capability must be queried for exact format/tiling/usage | The ICD must genuinely advertise and implement the chosen committed-resource path. LUID alone is insufficient. |
| C16 | Vulkan `D3D12_FENCE_BIT` imports an NT shared D3D fence and owns a reference. Exact 64-bit submit values are supported; Khronos says timeline-semaphore imports should use `VkTimelineSemaphoreSubmitInfo` rather than the older binary-compatible `VkD3D12FenceSubmitInfoKHR`. [Khronos semaphore type](https://docs.vulkan.org/refpages/latest/refpages/source/VkExternalSemaphoreHandleTypeFlagBits.html), [D3D12-fence submit values](https://docs.vulkan.org/refpages/latest/refpages/source/VkD3D12FenceSubmitInfoKHR.html), [timeline values](https://docs.vulkan.org/refpages/latest/refpages/source/VkTimelineSemaphoreSubmitInfo.html) | matching device/driver UUID; imported object owns a reference | Use one Ready and one Release timeline per image, each single-writer. |
| C17 | D3D12 queue Wait is GPU-side and blocks following queue work until the fence reaches the value; queue Signal is a GPU-side signal. [Microsoft Wait](https://learn.microsoft.com/en-us/windows/win32/api/d3d12/nf-d3d12-id3d12commandqueue-wait), [Signal](https://learn.microsoft.com/en-us/windows/win32/api/d3d12/nf-d3d12-id3d12commandqueue-signal) | same device/adapter contract | WSI bridge remains asynchronous; no CPU GPU wait per Present. |
| C18 | Microsoft's multi-component interop contract uses inbound and outbound fence/value pairs and says that shape matches DWM/DXGI flip synchronization. [Microsoft D3D11On12 interop](https://learn.microsoft.com/en-us/windows/win32/direct3d12/direct3d-12-with-direct3d-11--direct-2d-and-gdi) | consenting components with explicit state contract | Ready/Release is valid only inside the layer/ICD/D3D12 component, not as an inferred DWM protocol. |
| C19 | Vulkan external ownership uses `VK_QUEUE_FAMILY_EXTERNAL`; matching physical device/group and driver are required. Release, semaphore/fence completion, then acquire establish the transfer. [Khronos external queue family](https://docs.vulkan.org/refpages/latest/refpages/source/VK_QUEUE_FAMILY_EXTERNAL.html), [external ownership](https://docs.vulkan.org/spec/latest/chapters/resources.html#resources-sharing-external) | explicit release/acquire barriers | Every WSI handoff uses a real image barrier and Ready/Release semaphore edge; an empty submit is insufficient. |
| C20 | Vulkan acquisition may return before the presentation engine has finished with an image; the supplied semaphore/fence signals when use may begin. It may also return `VK_NOT_READY` at timeout zero when no image is available, and a finite/infinite timeout may wait for availability. [Khronos WSI chapter](https://docs.vulkan.org/spec/latest/chapters/VK_KHR_surface/wsi.html) | timeout controls whether an image slot can be acquired; semaphore/fence validity rules still apply | This implementation uses the stricter bounded-reuse rule in C47: a previously presented slot becomes selectable only after its exact D3D `Release[i]=e` has completed. It may still return before the newly queued EXTERNAL-to-Vulkan acquire barrier signals the application's semaphore/fence. |
| C21 | A Vulkan layer can intercept and forward instance/device functions through the loader dispatch chain. [Khronos loader architecture](https://github.com/KhronosGroup/Vulkan-Loader/blob/main/docs/LoaderInterfaceArchitecture.md) | every intercepted object/lifetime call must be wrapped correctly | Native applications enter the WSI layer; translator instances use a private direct ICD proc-address path and never enter it. |
| C22 | `D3DKMTShareObjects` can group an allocation and sync object, and open-from-NT-handle can return both; however OpenResource has no field carrying that group handle. [Microsoft ShareObjects](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmthk/nf-d3dkmthk-d3dkmtshareobjects), [open group](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmthk/ns-d3dkmthk-_d3dkmt_openresourcefromnthandle) | Windows 8+ | Strong primitive, absent stock-DWM carrier; rejected. |
| C23 | Present-private-data query is query-only and its size can race; WDDM 2.1 sync tokens are present-batching DDIs, not a published fence/value transfer contract. [Microsoft query](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dumddi/nc-d3dumddi-pfnd3dddi_getresourcepresentprivatedriverdatacb), [WDDM 2.1 features](https://learn.microsoft.com/en-us/windows-hardware/drivers/display/wddm-2-1-features) | Windows 10/WDDM 2.1 | Neither is the retirement carrier. |
| C24 | `D3DKMTPresent` supports legacy blit/primary flip and warns that blit-present contexts must be fully drainable; redirected Present requires a runtime-owned PresentHistoryToken plus sync object/value. [Microsoft KMT Present](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmthk/nf-d3dkmthk-d3dkmtpresent), [redirected structure](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmthk/ns-d3dkmthk-_d3dkmt_present_redirected) | redirected surface is WDDM 2.3-era; public mint contract absent | It cannot replace modern windowed flip WSI. |
| C25 | `IDXGISwapChain::Present` carries only sync interval and flags, not an application fence handle/value. [Microsoft Present](https://learn.microsoft.com/en-us/windows/win32/api/dxgi/nf-dxgi-idxgiswapchain-present) | all flip modes | A direct user-created shared fence to DWM remains impossible without a documented carrier. |
| C26 | `IDXGISwapChain3::GetCurrentBackBufferIndex` returns the current real backbuffer index. D3D12 Present operations execute on the direct queue supplied when the swapchain was created, and the buffer presented must be in `PRESENT` state. [Microsoft current index](https://learn.microsoft.com/en-us/windows/win32/api/dxgi1_4/nf-dxgi1_4-idxgiswapchain3-getcurrentbackbufferindex), [Microsoft D3D12 swapchains](https://learn.microsoft.com/en-us/windows/win32/direct3d12/swap-chains) | Windows 10+ flip swapchain | The WSI layer queries `j` for every Present and copies `S[i]` to current `B[j]`; it never equates the two indices. |
| C27 | D3D11 `pfnRenderCb` submits the current command buffer with its allocation list; allocation entries preserve the exact allocation handle and `WriteOperation`. Present/Present1 requires outstanding render data to be flushed, and Present1 is called after application rendering and shared-resource ownership release. [Microsoft RenderCb](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dumddi/nc-d3dumddi-pfnd3dddi_rendercb), [allocation list](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dukmdt/ns-d3dukmdt-_d3dddi_allocationlist), [Present1](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dumddi/nc-d3dumddi-pfnd3dddi_present1) | RenderCb Vista+; Present1 Windows 8.1/WDDM 1.3+ | D3D11's actual translated batch and exact read/write allocation list are the documented producer boundary; a marker-only batch is not. |
| C28 | The D3D12 resource-heaps contract says the runtime tracks presentable allocations, passes their handles in `WrittenPrimaries` for front-buffer validation/backbuffer synchronization, and requires submission from the same thread during `pfnExecuteCommandLists` on a context created for that command queue. It also says runtime synchronization with DWM/scanout relies on detected backbuffer writes and the association between DXGK contexts and command queues, and directs the UMD not to merge command lists such that more than eight handles reach one callback. [Microsoft Resource Heaps](https://microsoft.github.io/DirectX-Specs/d3d/ResourceHeaps.html#submitcommandcb-cannot-pass-more-than-8-handles-in-writtenprimaries) | at most eight WrittenPrimaries per submit; runtime command-list association is authoritative | This is the primary-source DWM carrier: seal and submit exactly one complete actual HOB1 batch per runtime command-list association, synchronously inside ECL on that queue's exact ordinary context, and consume that association's runtime-supplied primary array unchanged. Several command lists are submitted separately in order; Helios never merges their primary sets, partitions/filters the runtime array, splits one association, invents a handle, or detaches a primary from its writing commands. An over-eight runtime association or command list that cannot fit one bounded HOB1 fails before SubmitCommandCb rather than fabricating a new association. |
| C29 | For non-user-mode-submission queues, native-fence waits/signals are software commands inside Dxgkrnl; the UMD/KMD native-log protocol is not used. [Microsoft native-fence kernel-queue rule](https://learn.microsoft.com/en-us/windows-hardware/drivers/display/native-gpu-fence-objects#native-fence-log-buffer-ddis) | WDDM 3.2 native fences; selected path is traditional kernel submission | This defines how a requested native operation executes after C8 inserts it. It does **not** say that Windows automatically translates D3D12 `pfnWaitForFence`/`pfnSignalFence` after a no-op UMD callback; Helios must make the C8 callback calls itself. |
| C30 | Vulkan queues are created only by the `VkDeviceQueueCreateInfo` array at `vkCreateDevice`; each count is bounded by the reported family count, equal family/flags pairs are unique, and all equal-family entries together fit that count. Protected queues are requested separately, while a queue passed to `vkQueuePresentKHR` must support presentation for the surface and own the presented image. Queues are otherwise externally synchronized. [Khronos device create](https://docs.vulkan.org/refpages/latest/refpages/source/VkDeviceCreateInfo.html), [queue create info](https://docs.vulkan.org/refpages/latest/refpages/source/VkDeviceQueueCreateInfo.html), [queue rules](https://docs.vulkan.org/spec/latest/chapters/devsandqueues.html), [Khronos Present](https://docs.vulkan.org/refpages/latest/refpages/source/vkQueuePresentKHR.html) | device lifetime | **Selected, no alternative:** when `VK_KHR_swapchain` is enabled, require at least one application-requested `flags=0` queue in the canonical present family and reserve one additional real, unprotected lower queue there at `vkCreateDevice`; require a true lower family count of at least two and expose one fewer queue. Protected-only device requests are not WSI capable. The private queue is never returned to the application. One layer-owned mutex serializes only the layer's short helper submissions on that private queue; no app-visible queue shares that lock or lower queue. Present validates the passed queue as an app-visible unprotected canonical queue before consuming any wait. |
| C31 | `DxgkDdiSetVidPnSourceAddress` associates the exact OS-supplied `hAllocation` with a VidPn source, requires KMD to program scanout from the supplied address, and says KMD should record the flipped-to address. It can run at DIRQL for MMIO flip and must be nonpageable. Microsoft also warns that rejecting `SharedPrimaryTransition` after the UMD admitted Direct Flip does not seamlessly fall back and makes presentation incorrect. Independent-flip-exclusive means the front buffer is accessed only by display hardware, not DWM. [Microsoft SetVidPnSourceAddress](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmddi/nc-d3dkmddi-dxgkddi_setvidpnsourceaddress), [argument structure](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmddi/ns-d3dkmddi-_dxgkarg_setvidpnsourceaddress), [operation flags](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmddi/ns-d3dkmddi-_dxgk_setvidpnsourceaddress_flags), released WDK 28000 `d3dkmddi.h:6489-6521` | per VidPn source/plane; classic direct/independent path | The same bounded KMD scanout validator used by MPO3 independently validates this exact allocation/source/current-mode binding before retaining a candidate. Static incompatibility must have returned false from `CheckDirectFlipSupport`; an impossible later mismatch fails loudly before latch and is never described as a seamless composition fallback. KMD/QEMU retain the accepted allocation until replacement/unbind plus backend release. |
| C32 | Vulkan defines implied external layouts for D3D11 texture handle types, but not for `D3D12_RESOURCE_BIT`; it says interaction with Vulkan layouts is generally left to the other API. D3D12 defines `COMMON` as the cross-engine boundary state. [Khronos external-layout rules](https://docs.vulkan.org/spec/latest/chapters/resources.html#resources-image-layouts), [Microsoft resource states](https://learn.microsoft.com/en-us/windows/win32/api/d3d12/ne-d3d12-d3d12_resource_states) | no portable implied-layout rule for D3D12 resource imports | **Selected package-private component contract:** the exact Helios imported image uses Vulkan `GENERAL` at the external boundary and D3D12 `COMMON` outside Vulkan. The ICD must advertise/test this exact pairing; it is not inferred as a generic Vulkan guarantee. |
| C33 | D3D11On12 shares the application's D3D12 command queue. `AcquireWrappedResources` enters D3D11 hazard ownership; `ReleaseWrappedResources` inserts the declared output-state barriers, and `Flush` submits the D3D11 work to that shared command queue. Microsoft recommends shared-queue interop when possible because it avoids redundant queues and sync primitives. [Microsoft acquire](https://learn.microsoft.com/en-us/windows/win32/api/d3d11on12/nf-d3d11on12-id3d11on12device-acquirewrappedresources), [release](https://learn.microsoft.com/en-us/windows/win32/api/d3d11on12/nf-d3d11on12-id3d11on12device-releasewrappedresources), [interop model](https://learn.microsoft.com/en-us/windows/win32/direct3d12/direct3d-12-with-direct3d-11--direct-2d-and-gdi) | consenting in-process device/queue and explicit input/output states | D3D11On12 work seals into the owning D3D12 queue's next actual ordinary-context submission. It may not create a D3D11 execution context or external Ready/Release pair. |
| C34 | `pfnUpdateGpuVirtualAddressCb` applies reserved-resource mapping operations on the paging context dedicated to an exact render context. It waits that context's monitored fence value and signals value+1 when the page-table update completes; the caller must wait a pending map's returned fence before GPU access. [Microsoft update structure](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dumddi/ns-d3dumddi-_d3dddicb_updategpuvirtualaddress), [Microsoft map callback](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dumddi/nc-d3dumddi-pfnd3dddi_mapgpuvirtualaddresscb) | Windows 10/WDDM 2.0+ GPUVA; exact context and reserved range | D3D12 Update/CopyTileMappings lower to WDDM paging operations and an exact first-use dependency. They do not authorize an internal Vulkan sparse queue. Every other translator-generated GPU command is folded into the next actual D3D queue submission or the advertised feature is refused. |
| C35 | Native-fence admission is multi-part: the OS feature must be enabled; KMD sets `DXGK_VIDSCHCAPS::NativeGpuFence`; `No64BitAtomics=0` means native 64-bit atomic updates visible to CPU; and `DXGKQAITYPE_NATIVE_FENCE_CAPS` returns mapping stride/range plus truthful `MapToGpuSystemProcess`. [Microsoft native-fence feature contract](https://learn.microsoft.com/en-us/windows-hardware/drivers/display/native-gpu-fence-objects), [scheduler caps](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmddi/ns-d3dkmddi-_dxgk_vidschcaps), [native caps](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmddi/ns-d3dkmddi-dxgk_native_fence_caps) | WDDM 3.2 capability surface; all reserved fields zero | KMD may report native support only when every native DDI, mapping mode, stride, address range, atomicity rule, and interrupt/update path is implemented. One missing gate prevents package admission; it does not select another fence type. |
| C36 | Fence Barriers in the Enhanced Barriers specification are explicitly a preview surface and do not appear in the released WDK 28000 `d3d12umddi.h`; that specification does not define ordinary command-queue `pfnWaitForFence`/`pfnSignalFence` lowering. [Microsoft DirectX-Specs preview](https://microsoft.github.io/DirectX-Specs/d3d/D3D12EnhancedBarriers.html#fence-barriers-preview) | unshipped preview, not a released package dependency | Rejected as a correctness mechanism. The selected design uses only released Core-0116 native-object identity and the established C8 context callbacks. |
| C37 | D3D12 `CopyResource` requires distinct resources of the same type and total size, identical dimensions or a permitted reinterpret copy, compatible DXGI formats, and identical multisample count/quality; it performs no stretch, blend, or color conversion. A flip-model `DXGI_SWAP_CHAIN_DESC1` restricts formats, requires sample count 1/quality 0, and requires 2-16 buffers. [Microsoft CopyResource](https://learn.microsoft.com/en-us/windows/win32/api/d3d12/nf-d3d12-id3d12graphicscommandlist-copyresource), [Microsoft swapchain descriptor](https://learn.microsoft.com/en-us/windows/win32/api/dxgi1_2/ns-dxgi1_2-dxgi_swap_chain_desc1) | copy-only bridge; D3D12 flip swapchain | `S[i]` and `B[j]` are exactly BGRA8 UNORM, 2D, one mip, one layer, sample count 1/quality 0, and equal nonzero extent. Any need for conversion, scaling, resolve, or tonemapping is unsupported, not a hidden shader/copy fallback. |
| C38 | Win32 Vulkan surfaces report `minImageExtent`, `maxImageExtent`, and `currentExtent` equal to the window size; `(0,0)` is legal while minimized but cannot create a base swapchain. FIFO is the only universally required present mode and is equivalent to swap interval 1. The selected format/color-space pair must have been enumerated, `COLOR_ATTACHMENT` must be in `supportedUsageFlags`, and create-time usage must be a subset. [Khronos Win32 WSI](https://docs.vulkan.org/spec/latest/chapters/VK_KHR_surface/wsi.html), [Khronos present modes](https://docs.vulkan.org/refpages/latest/refpages/source/VkPresentModeKHR.html) | base `VK_KHR_surface`/`VK_KHR_win32_surface`/`VK_KHR_swapchain`; no scaling extension | The layer advertises one exact FIFO SDR surface profile. It maps `VK_FORMAT_B8G8R8A8_UNORM`/`VK_COLOR_SPACE_SRGB_NONLINEAR_KHR` to `DXGI_FORMAT_B8G8R8A8_UNORM`/`DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709`, verifies the DXGI color space with `CheckColorSpaceSupport`, and sets it with `SetColorSpace1`. [Microsoft color space](https://learn.microsoft.com/en-us/windows/win32/api/dxgicommon/ne-dxgicommon-dxgi_color_space_type), [check](https://learn.microsoft.com/en-us/windows/win32/api/dxgi1_4/nf-dxgi1_4-idxgiswapchain3-checkcolorspacesupport), [set](https://learn.microsoft.com/en-us/windows/win32/api/dxgi1_4/nf-dxgi1_4-idxgiswapchain3-setcolorspace1) |
| C39 | `vkQueueSubmit2` is core in Vulkan 1.3 (or supplied by `VK_KHR_synchronization2`) and requires the `synchronization2` feature. Creating a timeline semaphore requires `timelineSemaphore`. Win32 D3D12-resource/fence import additionally requires the Win32 external-memory/semaphore device extensions and an IMPORTABLE result for the exact handle/format tuple. Every semaphore passed to `vkQueuePresentKHR` must be binary. [Khronos Submit2](https://docs.vulkan.org/refpages/latest/refpages/source/vkQueueSubmit2.html), [timeline feature](https://docs.vulkan.org/refpages/latest/refpages/source/VkPhysicalDeviceTimelineSemaphoreFeatures.html), [synchronization2 feature](https://docs.vulkan.org/refpages/latest/refpages/source/VkPhysicalDeviceSynchronization2Features.html), [Win32 external memory](https://docs.vulkan.org/refpages/latest/refpages/source/VK_KHR_external_memory_win32.html), [Win32 external semaphore](https://docs.vulkan.org/refpages/latest/refpages/source/VK_KHR_external_semaphore_win32.html), [Vulkan Present](https://docs.vulkan.org/refpages/latest/refpages/source/vkQueuePresentKHR.html#valid-usage) | selected lower-device floor is Vulkan 1.3; both features are still explicitly enabled | The layer clones and augments the downstream device-create request with `timelineSemaphore=VK_TRUE`, `synchronization2=VK_TRUE`, `VK_KHR_external_memory_win32`, and `VK_KHR_external_semaphore_win32`. It preserves the application's request and never mutates caller memory. Missing support makes this physical device/surface unsupported before any handle is exported. Present validates all waits as binary and every swapchain as a live layer-owned object before consuming any wait. |
| C40 | Sharing a committed D3D12 resource shares its implicit heap; shared heaps require `D3D12_HEAP_FLAG_SHARED`. `CreateFence` takes an exact initial value and fence flags; `D3D12_FENCE_FLAG_SHARED` is distinct from cross-adapter and non-monitored flags. [Microsoft shared heaps](https://learn.microsoft.com/en-us/windows/win32/direct3d12/shared-heaps), [committed resources](https://learn.microsoft.com/en-us/windows/win32/api/d3d12/nf-d3d12-id3d12device-createcommittedresource), [heap flags](https://learn.microsoft.com/en-us/windows/win32/api/d3d12/ne-d3d12-d3d12_heap_flags), [CreateFence](https://learn.microsoft.com/en-us/windows/win32/api/d3d12/nf-d3d12-id3d12device-createfence), [fence flags](https://learn.microsoft.com/en-us/windows/win32/api/d3d12/ne-d3d12-d3d12_fence_flags) | exact same-adapter, one-node WSI objects | `S[i]` is a DEFAULT committed texture with `D3D12_HEAP_FLAG_SHARED`, not DISPLAY or CROSS_ADAPTER. `Ready[i]` and `Release[i]` are each `CreateFence(0,D3D12_FENCE_FLAG_SHARED)`, never NON_MONITORED/CROSS_ADAPTER. Every resource/fence handle is unnamed, non-inheritable, `GENERIC_ALL`, and exported once per object lifetime. |
| C41 | The WDDM 2.1+ initialization surface contains `DxgkDdiCheckMultiPlaneOverlaySupport3` and `DxgkDdiSetVidPnSourceAddressWithMultiPlaneOverlay3`; WDDM 2.2 adds KMD MPO/post-composition capability slots. MPO3 check receives exact `hAllocation` values, while SetMPO3 receives per-plane context records containing exact `hContext`, `hAllocation`, and GPUVA. [Microsoft initialization table](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/dispmprt/ns-dispmprt-_driver_initialization_data), [MPO support](https://learn.microsoft.com/en-us/windows-hardware/drivers/display/multiplane-overlay-support), [MPO3 check](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmddi/nc-d3dkmddi-dxgkddi_checkmultiplaneoverlaysupport3), [MPO3 plane](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmddi/ns-d3dkmddi-_dxgk_multiplane_overlay_plane3), [SetMPO3](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmddi/nc-d3dkmddi-dxgkddi_setvidpnsourceaddresswithmultiplaneoverlay3), released WDK 28000 `d3dkmddi.h:6601-6719,6798-6865,6909-6943` | official fields/versions; the current Helios WDDM-3.2 `E_NOTIMPL` boundary is a local-source conclusion | A version-constant-only uplift is invalid. The selected KMD registers and implements a truthful one-primary-plane MPO3/Display-Core surface, consumes only the OS-supplied allocation/context, and returns unsupported for every unimplemented shape. The narrow numeric caps and their DWM sufficiency are a design proposal, not a Microsoft guarantee, so cold build-28000 DWM/MPO admission is a mandatory pre-retirement gate. |
| C42 | Display miniports have an OS-owned ETW control callback, `DxgkDdiControlEtwLogging`; it is an enable/disable notification, not provider registration. A kernel provider must separately call `EtwRegister`, retain the `REGHANDLE`, emit bounded system-space descriptors with `EtwProviderEnabled`/`EtwWrite`, and call `EtwUnregister` during drained unload. WDDM separately calls `DxgkDdiCollectDbgInfo` for driver debug reports, WDDM 3.2 adds `DxgkDdiCollectDbgInfo2` for richer TDR data, and WDDM 2.6+ exposes `DxgkDdiCollectDiagnosticInfo`; WDDM 2.7+ drivers must support its black-screen diagnostic type. [Microsoft ETW callback](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/dispmprt/nc-dispmprt-dxgkddi_control_etw_logging), [EtwRegister](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/wdm/nf-wdm-etwregister), [EtwWrite](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/wdm/nf-wdm-etwwrite), [CollectDbgInfo](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmddi/nc-d3dkmddi-dxgkddi_collectdbginfo), [WDDM 3.2 TDR debugging](https://learn.microsoft.com/en-us/windows-hardware/drivers/display/tdr-debuggability-improvements), [CollectDiagnosticInfo](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/dispmprt/nc-dispmprt-dxgkddi_collectdiagnosticinfo), released WDK table `tmp/dx12/sdk/dispmprt.h:2707,2727,2975-2981,3038` | ETW control/DbgInfo: Vista+; diagnostic slot: WDDM 2.6+; `CollectDbgInfo2`: WDDM 3.2 | Runtime observability uses the separately registered KMD provider, with emission gated by the OS callback. ETW loss is permitted and never changes recovery, ordering, or lifetime. Reset/TDR/black-screen state is copied only when the OS invokes the matching diagnostic DDI. No `QUERY_STATS`, `QUERY_SCANOUT_TIMELINE`, mapped page, private IOCTL, or `D3DKMTEscape` query survives. |
| C43 | DWM calls D3D11.1 `PFND3D11_1DDI_CHECKDIRECTFLIPSUPPORT` before attempting Direct Flip, after every mode change, and after recreating its swapchain. The callback receives the exact app and DWM swapchain resource handles plus the immediate-flip flag, and must return false unless their primary allocations are compatible, including stereo/MSAA/swizzle and linked-adapter configuration. [Microsoft D3D11.1 callback](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3d10umddi/nc-d3d10umddi-pfnd3d11_1ddi_checkdirectflipsupport), [Direct Flip DDI](https://learn.microsoft.com/en-us/windows-hardware/drivers/display/direct-flip-of-video-memory) | Windows 8/WDDM 1.2+; current Helios D3D11.1 table already exposes the slot at `umd/src/forward/tables.rs:267` and implements an always-false body at `forward/transfer.rs:369-382` | The UMD validates both live resource wrappers and their immutable allocation descriptors. Concrete D3D11 primary source IDs must match. A C44 D3D12 runtime-primary sentinel on either compatible resource is valid only on the same adapter/LUID and does not manufacture or compare a source ID. The pair must otherwise match the one-plane BGRA8/sample-1 profile and reject `IMMEDIATE`; missing data means false. The optional Escape route is forbidden. The later classic SetVidPn or MPO3 KMD path independently validates the actual OS-supplied source and binding. |
| C44 | For a D3D12 runtime primary, Resource Heaps requires `D3DDDI_ID_UNINITIALIZED` in `D3DDDI_ALLOCATIONINFO::VidPnSourceId`, says the runtime overwrites the field with that sentinel, and allows the primary on any VidPn source associated with the adapter LUID. It specifically requires D3D11 Direct/Independent-Flip support to handle all such sources. [Microsoft Resource Heaps primary creation](https://microsoft.github.io/DirectX-Specs/d3d/ResourceHeaps.html#support-primary-creation-through-the-new-d3d12-heap-ddis-without-existing-primary-description) | D3D12 runtime-managed primaries; same adapter LUID | HWA2 preserves the sentinel and marks the runtime-primary origin; it never invents a fixed source. Direct-Flip resource-pair admission is same-adapter/format/layout based, while the actual VidPn source comes only from the later OS SetVidPn/MPO3 call. |
| C45 | Vulkan 1.1 plus `VK_KHR_swapchain` includes the device-group WSI commands and structures; a singleton group's present mode must be `LOCAL`. The WSI contract also permits an application to create an image with `VkImageSwapchainCreateInfoKHR`, bind it to an exact swapchain image index with `VkBindImageMemorySwapchainInfoKHR`, use it anywhere a swapchain image is used, and later destroy it with `vkDestroyImage`; the alias does not change which memory the swapchain index presents. Both swapchain structures admit `VK_NULL_HANDLE`; in that branch they impose no swapchain-memory substitution and the ordinary image create/bind continues. The same external-memory payload may be imported repeatedly into distinct `VkDeviceMemory` objects, and layout transitions/writes on identical images aliasing the same external memory affect/define every identical alias. `D3D12_RESOURCE_BIT` must report `DEDICATED_ONLY`; its `allocationSize` is ignored and its external NT-handle `memoryTypeIndex` comes from `vkGetMemoryWin32HandlePropertiesKHR`. [Khronos device-group promotion](https://docs.vulkan.org/refpages/latest/refpages/source/VK_KHR_device_group.html), [singleton capabilities](https://docs.vulkan.org/refpages/latest/refpages/source/VkDeviceGroupPresentCapabilitiesKHR.html), [image swapchain create](https://docs.vulkan.org/refpages/latest/refpages/source/VkImageSwapchainCreateInfoKHR.html), [bind swapchain memory](https://docs.vulkan.org/refpages/latest/refpages/source/VkBindImageMemorySwapchainInfoKHR.html), [WSI alias contract](https://docs.vulkan.org/spec/latest/chapters/VK_KHR_surface/wsi.html), [Win32 repeated import](https://docs.vulkan.org/refpages/latest/refpages/source/VkImportMemoryWin32HandleInfoKHR.html), [external-memory features](https://docs.vulkan.org/refpages/latest/refpages/source/VkExternalMemoryFeatureFlagBits.html), [allocation import rules](https://docs.vulkan.org/refpages/latest/refpages/source/VkMemoryAllocateInfo.html), [external alias layout](https://docs.vulkan.org/spec/latest/chapters/resources.html#resources-memory-aliasing) | mandatory interaction for the selected Vulkan-1.3 device with `VK_KHR_swapchain`; selected group has one physical device and one node | The layer implements the complete singleton `LOCAL` WSI subset. Each non-null app-created alias is a real lower VkImage with the implied profile and a distinct dedicated `D3D12_RESOURCE_BIT` memory import of exact `S[index]`, reusing that slot's one retained unnamed NT resource handle. Null forms are consumed/stripped and forwarded as ordinary create/bind operations. No virtual swapchain is forwarded or mapped heuristically. |
| C46 | Instance/device extension enumeration is the contract by which applications discover enableable extensions; a layer can supply extensions, and enabled instance/device commands must be returned through the matching `vkGetInstanceProcAddr`/`vkGetDeviceProcAddr` dispatch path. [Khronos instance enumeration](https://docs.vulkan.org/refpages/latest/refpages/source/vkEnumerateInstanceExtensionProperties.html), [device enumeration](https://docs.vulkan.org/refpages/latest/refpages/source/vkEnumerateDeviceExtensionProperties.html), [instance proc address](https://docs.vulkan.org/refpages/latest/refpages/source/vkGetInstanceProcAddr.html), [device proc address](https://docs.vulkan.org/refpages/latest/refpages/source/vkGetDeviceProcAddr.html), [loader architecture](https://github.com/KhronosGroup/Vulkan-Loader/blob/main/docs/LoaderInterfaceArchitecture.md) | instance/device discovery and lifetime | The implicit layer, not the lower ICD, is the sole owner of `VK_KHR_surface`, `VK_KHR_win32_surface`, optional base-profile `VK_KHR_get_surface_capabilities2`, and `VK_KHR_swapchain`. It advertises, consumes at create, and returns every selected WSI entry point through both legal proc-address routes; stale lower names and entry points are filtered rather than leaked. |
| C47 | A D3D12 command allocator may be reset only after all GPU execution of lists recorded with it has finished; resetting while a list is executing is undefined. Vulkan likewise forbids reset of a pending command buffer. [Microsoft allocator reset](https://learn.microsoft.com/en-us/windows/win32/api/d3d12/nf-d3d12-id3d12commandallocator-reset), [Khronos command-buffer reset](https://docs.vulkan.org/refpages/latest/refpages/source/vkResetCommandBuffer.html) | per swapchain image/epoch | Each `S[i]` owns one D3D12 copy allocator/list and two Vulkan barrier command buffers. None is reset/re-recorded until `Release[i]=e` has actually completed; completion also proves the prior Ready barrier and D3D copy retired. A completed-value check is performed at Acquire and an exact fence event is armed only under backpressure—never polling, sleeping, or allocating an unbounded ring. |
| C48 | `vkDestroySwapchainKHR` requires the application to have completed outstanding operations on acquired images and destroys the swapchain's associated presentation-engine image handles. Images separately created with `VkImageSwapchainCreateInfoKHR` remain distinct application objects and must be destroyed with `vkDestroyImage`. [Khronos destroy-swapchain contract](https://docs.vulkan.org/refpages/latest/refpages/source/vkDestroySwapchainKHR.html), [Khronos WSI alias contract](https://docs.vulkan.org/spec/latest/chapters/VK_KHR_surface/wsi.html) | application-visible swapchain/alias lifetime | Parent destruction invalidates Acquire/Present and canonical returned swapchain-image handles, not a separately created alias. The layer relies on the application's Vulkan valid-usage precondition and can detect only later layer-visible misuse; it does not claim to prove retirement of arbitrary application command buffers. It drains its own presentation work and returns any externally owned aliased payload to Vulkan before destruction returns; exact backing/import references then keep each alias normally usable until its own GPU work and `vkDestroyImage` retire. |
| C49 | `D3DKMT_OPENNATIVEFENCEFROMNTHANDLE` requires the exact NT handle and KMT device plus `EngineAffinity`, synchronization flags, a 64-byte in/out private-driver-data buffer, and returns a process-local sync handle and native mapping. [Microsoft open-native-fence structure](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmthk/ns-d3dkmthk-d3dkmt_opennativefencefromnthandle), released WDK `icd/win-build/wdk-include/d3dkmthk.h:1719-1728` | Windows 11 24H2/WDDM 3.2 native-fence open | The selected one-node ICD open sets affinity bit 0, only `Shared|NtSecuritySharing`, every reserved/other flag and byte to zero, and validates the returned HNF1 payload before retaining the local handle/mapping. A zero/multi-bit affinity, cross-adapter flag, or mismatched payload fails import. |
| C50 | `D3DKMTCreateContext` returns an ICD command buffer, allocation list, and patch-location list; `D3DKMTRender` submits that current command buffer and returns the next buffers. Dxgkrnl then calls `DxgkDdiRender` to validate/translate it and uses the legacy physical `DxgkDdiSubmitCommand` path. By contrast, `D3DKMTSubmitCommand` is for GPU-virtual-addressing contexts that own their command pool and explicitly replaces Render; legacy patch-mode contexts must continue to use Render. [Microsoft create-context structure](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmthk/ns-d3dkmthk-_d3dkmt_createcontext), [KMT Render](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmthk/nf-d3dkmthk-d3dkmtrender), [KMD Render](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmddi/nc-d3dkmddi-dxgkddi_render), [submission sequence](https://learn.microsoft.com/en-us/windows-hardware/drivers/display/submitting-a-command-buffer), [GPUVA SubmitCommand](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmthk/nf-d3dkmthk-d3dkmtsubmitcommand) | Vista+ legacy patch model; selected native-ICD lane remains available on the WDDM-3.2 adapter as a non-virtual context | The normal native Vulkan ICD uses exactly the legacy `CreateContext`/`Render` model. It never calls `CreateContextVirtual`, `D3DKMTSubmitCommand`, a HWQueue submit, or both models for one context. The KMD reports that exact legacy submission complete only after its host/Venus work completes. |
| C51 | A monitored fence supports GPU and CPU signal/wait. `D3DKMTSignalSynchronizationObjectFromGpu` inserts a software signal on an exact KMT context stream; with `TopOfPipeline=FALSE` the signal occurs after preceding command buffers complete. `D3DKMTWaitForSynchronizationObjectFromCpu` can block or signal an asynchronous event. `NoSignalMaxValueOnTdr=TRUE` prevents reset from converting failure into maximum-value completion, and `NoGPUAccess=TRUE` selects packet-only operations. [Microsoft context monitoring](https://learn.microsoft.com/en-us/windows-hardware/drivers/display/context-monitoring), [KMT signal](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmthk/nf-d3dkmthk-d3dkmtsignalsynchronizationobjectfromgpu), [KMT CPU wait](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmthk/nf-d3dkmthk-d3dkmtwaitforsynchronizationobjectfromcpu), [sync flags](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dukmdt/ns-d3dukmdt-_d3dddi_synchronizationobject_flags) | WDDM 2.0+/Windows 10; unshared object local to one native ICD KMT context | Each normal-ICD context explicitly creates one unshared monitored progress fence at zero. A required CPU-visible completion queues a strictly increasing signal after the last Render on that same context and waits by one event/blocking KMT operation, never by sampling the CPU mapping. Reset/removal cancels the context and cannot satisfy the value. This is an explicit sync object, not an implicit scheduler event. |
| C52 | WDDM permits a memory segment to be directly CPU visible when it has linear access through a PCI aperture: `CpuVisible=1`, `CpuTranslatedAddress` is the BAR's bus-relative base, and an allocation's segment offset equals its BAR offset. An allocation in a fully CPU-accessible/resizable-BAR segment is guaranteed lockable. If VidMm evicts a still-mapped allocation, it preserves the CPU VA over system memory and the driver must preserve byte order through real paging DMA. `CacheCoherent` applies only to aperture segments, not memory segments. [Microsoft segment mapping](https://learn.microsoft.com/en-us/windows-hardware/drivers/display/mapping-virtual-addresses-to-a-memory-segment), [segment descriptor](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmddi/ns-d3dkmddi-_dxgk_segmentdescriptor4), [segment flags](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmddi/ns-d3dkmddi-_dxgk_segmentflags), [allocation usage](https://learn.microsoft.com/en-us/windows-hardware/drivers/display/allocation-usage-tracking) | documented since the WDDM memory-segment model; selected WDDM 3.2 | HLM1 is exactly `[Aperture id 1, CpuVisible linear memory id 2]`: id 1 remains the paging-buffer segment and id 2 has `Aperture=CacheCoherent=SupportsCpuHostAperture=SupportsCachedCpuHostAperture=0`, `CpuVisible=1`, and the exact full BAR base/size. Every ordinary native-Vulkan memory object is an HVM1 WDDM allocation with `AccessedPhysically=1` and real HPM1 paging. Host-visible roles additionally use allocation `CpuVisible=1,Cached=0`; device-local-only roles keep allocation `CpuVisible=0` and are never locked. The package does not advertise CPU Host Aperture, so no null/implicit map callback or HAP protocol exists. |
| C53 | `DxgkDdiRender` receives the command and input patch list in user address space and Microsoft requires `__try/__except` protection plus full validation before copying into kernel buffers. The allocation list has already been validated/copied and converted to KMD allocation handles, preserving index and `WriteOperation`. Render must pre-patch every currently resident (`SegmentId!=0`) reference and still emit every reference in its output patch list; `DxgkDdiPatch` receives the final allocation list with exact KMD allocation handle, segment ID, and physical address and may run repeatedly. A Patch error causes an OS bugcheck. [Microsoft KMD Render](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmddi/nc-d3dkmddi-dxgkddi_render), [Microsoft Patch](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmddi/nc-d3dkmddi-dxgkddi_patch), [GPU segments](https://learn.microsoft.com/en-us/windows-hardware/drivers/display/gpu-segments), released WDK 28000 `d3dkmddi.h:77-96,136-171,4358-4412` | Render and repeatable Patch at PASSIVE_LEVEL | Add purpose-limited protected command copying. HNR2's typed input records require zero WDDM input patches, but each allocation capability makes KMD emit one real output patch location into a DMA-local physical-capability slot. Set `NoPatchingRequired=0`. Render pre-patches from its validated kernel allocation list whenever resident. Patch is infallible for OS-valid state, idempotent, and side-effect-free: it writes only `{allocationGeneration,SegmentId,PhysicalAddress,byteRange,HPM1Epoch}` into that slot. Malicious/inconsistent user input is rejected in Render before an output list exists; an impossible later internal inconsistency is a driver correctness failure, not a recoverable Patch branch. Submit revalidates the immutable snapshot and may fail/remove the device; no Patch call maps QEMU memory or mutates lifetime. |
| C54 | `DmaBufferSegmentSet=0` requests a contiguous paged-locked write-combined DMA buffer; when nonzero, the set may contain only aperture segments, and a memory-segment bit causes context creation to fail. `AccessedPhysically` is truthful only when the render engine really uses allocation-list physical addresses; it makes memory-segment placement contiguous and maps a system-resident allocation into the ordinary aperture. `Cached=0` requests write-combined backing, while uncached Vulkan host memory is coherent but not host-cached. [Microsoft context info](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmddi/ns-d3dkmddi-_dxgk_contextinfo), [GPU segments](https://learn.microsoft.com/en-us/windows-hardware/drivers/display/gpu-segments), [allocation flags](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmddi/ns-d3dkmddi-_dxgk_allocationinfoflags), [Vulkan memory flags](https://docs.vulkan.org/refpages/latest/refpages/source/VkMemoryPropertyFlagBits.html) | context, allocation, mapping, and device lifetime | HVC1 normal-ICD contexts select `DmaBufferSegmentSet=0` and validate the returned paged-locked DMA buffers; existing D3D-runtime legacy contexts may retain aperture id 1. No context names HLM1 id 2. The ICD exposes a device-local HVM1 memory type for every supported ordinary image/buffer allocation and exposes `HOST_VISIBLE|HOST_COHERENT`, never `HOST_CACHED`, only for host-visible HVM1 roles after the target proves each Lock2 mapping is actually WC/uncached and the C51/DMA boundaries order CPU/device use. HNR2 consumes only the repeatedly patched current segment/physical address; HPM1 resolves either the current contiguous HLM1 binding or the exact ordinary-aperture PTE generation. |
| C55 | A finite Venus command stream is valid opaque `VIRTIO_GPU_CMD_SUBMIT_3D` input; QEMU submits it to the renderer, then a fenced command with `INFO_RING_IDX` creates a context/ring fence. Host completion callbacks are keyed by `(ctx_id,ring_idx,fence_id)`. Upstream Venus explicitly defines ring 0 as the context CPU timeline, signaled after renderer command processing, while a nonzero ring is bound to a physical VkQueue and signals after that queue executes. [Virtio GPU command header](https://docs.oasis-open.org/virtio/virtio/v1.2/virtio-v1.2.html), pinned Mesa `icd/mesa/src/virtio/vulkan/vn_renderer.h:83-95`, `vn_renderer_util.h:21-35`, QEMU `qemu-helios/hw/display/virtio-gpu-virgl.c:638-667,1222-1299` | one host Venus context per HTS1/`vn_instance`; CPU/decode-only ring 0 plus one nonzero non-recycled GPU timeline per physical lower VkQueue endpoint | HNR2 deletes `vn_ring`. BEGIN/DATA only stage bounded bytes; COMMIT repeats the complete exact WDDM allocation manifest, then KMD validates every typed operand and constructs one DMA-local physical capability per use. After VidMm's final Patch, QEMU validates and resolves each capability only through HPM1, substitutes renderer-private resource IDs in a host-private command copy, names the exact HVM1 reply destination, and emits one actual fenced `SUBMIT_3D`; neither the UMD stream nor shared storage carries a host resource ID. A control COMMIT is restricted to C60 pure-control opcodes and retires after reply publication plus ring-0 processing; it never proves GPU completion. A normal-HVC1 or HQA1-attached outer queue COMMIT retires only after its nonzero host fence. If several outer contexts share that endpoint, C62 serializes only scheduler-eligible DMA in actual KMD arrival order; no user-assigned cross-context sequence can delay an already submitted DMA behind work VidSch has not submitted. Queue/device idle and teardown join the latest C51/HQC1 milestones of every affected nonzero endpoint before final control destruction. Variable generated reply sizes use one exact bounded session-owned HVM1 pool and C60's finite continuation protocol; CPU waits use events, never shared-memory sampling. |
| C56 | VidMm manages a WDDM memory segment as physical pages and moves/fills/discards/maps contents through paging buffers. `DxgkDdiBuildPagingBuffer` must encode actual DMA; VidMm then calls `DxgkDdiPatch` before paging Submit even though it supplies no patch-location list, and Patch cannot change the buffer size. `DxgkDdiSubmitCommand` runs at `DISPATCH_LEVEL`, so it may enqueue but never block on QEMU. Released WDK 28000's WDDM-3.2 operations carry exact allocation/source/destination ADL or segment ranges. [Microsoft BuildPagingBuffer](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmddi/nc-d3dkmddi-dxgkddi_buildpagingbuffer), [Microsoft SubmitCommand](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmddi/nc-d3dkmddi-dxgkddi_submitcommand), [Microsoft Patch](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmddi/nc-d3dkmddi-dxgkddi_patch), released WDK 28000 `d3dkmddi.h:4947-5001` | selected WDDM 3.2 paging context and placement lifetime | HPM1 BuildPagingBuffer emits a bounded transaction naming exact allocation generation, source/destination runs, range, and next epoch. Mandatory paging Patch is infallible, side-effect-free, and no-size-change. Submit only enqueues; device/QEMU completion performs the bind/copy/fill/map, retains old placement until users drain, publishes the new epoch, and only then completes the paging SubmissionFenceId. `NotifyResidency2` audits state only. HNR2 Submit accepts a physical capability only if its current segment/range/epoch resolves through HPM1. |
| C57 | The three interfaces remain separate. Legacy `D3DKMT_OPENRESOURCE` consumes `hGlobalShare`; D3D12 runtime `pfnOpenHeapAndResource` supplies records only inside the D3D12 UMD; neither is the lower-ICD carrier. The NT-handle function page expressly pairs `D3DKMTOpenResourceFromNtHandle` with an NT handle acquired through `D3DKMTShareObjects` or, for a named object, `D3DKMTOpenNtHandleFromName`. The selected layer already holds one unnamed `CreateSharedHandle` result, so it skips the name lookup. The ICD first queries `NumAllocations` and buffer sizes, allocates a zero-initialized `D3DDDI_OPENALLOCATIONINFO2[NumAllocations]` plus the queried buffers, and passes those outputs to the open call; it never fabricates an allocation record. The released header calls the member an `_Field_size_(NumAllocations)` array of open-allocation structures, the enclosing thunk is `_Inout_`, and Microsoft's published dxgkrnl implementation copies a process-local allocation handle and per-allocation private-data pointer/size back into every element. The element also has an `[out] GpuVirtualAddress`, but the selected legacy Render/Patch lane does not require it to be nonzero. Learn's isolated sentence calling the array reserved is recorded as a documentation conflict; section 8.2 gives the full evidence and limitation. [Microsoft open function and handle provenance](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmthk/nf-d3dkmthk-d3dkmtopenresourcefromnthandle), [Microsoft named-handle helper](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmthk/nf-d3dkmthk-d3dkmtopennthandlefromname), [Microsoft open structure](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmthk/ns-d3dkmthk-_d3dkmt_openresourcefromnthandle), [Microsoft KMT query](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmthk/ns-d3dkmthk-_d3dkmt_queryresourceinfofromnthandle), [Microsoft allocation-info2](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dukmdt/ns-d3dukmdt-_d3dddi_openallocationinfo2), [Microsoft dxgkrnl implementation](https://github.com/microsoft/WSL2-Linux-Kernel/blob/linux-msft-wsl-6.6.y/drivers/hv/dxgkrnl/ioctl.c), [Microsoft destroy contract](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmthk/ns-d3dkmthk-_d3dkmt_destroyallocation2), released WDK `icd/win-build/wdk-include/d3dkmthk.h:1642-1685,1786-1797,5907`, `icd/win-build/wdk-include/d3dukmdt.h:479-486`, vendored dxgkrnl UAPI `icd/mesa/include/drm-uapi/d3dkmthk.h:1251-1262,1455-1478,1775-1778` | published carrier with one explicitly recorded Learn/header conflict; selected same-process `D3D12_RESOURCE_BIT` import | Query/open occurs once per slot, never per frame. Correctness retains the returned `hAllocation` and immutable per-allocation private data; a nonzero GPUVA is recorded and cross-checked but is not an HNR2 prerequisite. Failure status, count mismatch, zero `hAllocation`, or any private-data pointer/range outside the queried arena refuses the import. After canonical, alias, and GPU references drain, `D3DKMTDestroyAllocation2{hResource,phAllocationList=NULL,AllocationCount=0,Flags=0}` releases the resource and all associated allocations, then the retained NT handle closes. Named/OPAQUE objects, legacy global share, D3D/DXGI recursion, and raw host IDs remain forbidden by the selected profile. |
| C58 | `DxgkDdiCreateProcess` creates one KMD process object and `DXGKARG_CREATEDEVICE::hKmdProcess` identifies the corresponding object for every graphics device in that process. Both physical `D3DDDICB_CREATECONTEXT` and virtual `D3DDDICB_CREATECONTEXTVIRTUAL` explicitly carry bounded UMD-to-KMD context private data; KMD receives it in `DXGKARG_CREATECONTEXT`, while `D3DDDICB_RENDER::pPrivateDriverData` and its size are reserved-zero. [Microsoft process creation](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmddi/nc-d3dkmddi-dxgkddi_createprocess), [process argument](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmddi/ns-d3dkmddi-_dxgkarg_createprocess), [device argument](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmddi/ns-d3dkmddi-_dxgkarg_createdevice), [physical UMD context argument](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dumddi/ns-d3dumddi-_d3dddicb_createcontext), [virtual UMD context argument](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dumddi/ns-d3dumddi-_d3dddicb_createcontextvirtual), [KMD context argument](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmddi/ns-d3dkmddi-_dxgkarg_createcontext), [Render argument](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dumddi/ns-d3dumddi-_d3dddicb_render) | process and context creation lifetime; no resource identity is implied | HTS1 admission requires pointer-identical `hKmdProcess`, adapter/node, package generation, session generation, and an unpredictable 128-bit capability. HQA1 travels only in outer context-create PDD, physical for D3D11 and virtual for D3D12, and includes a UMD-chosen nonzero/nonreused context generation so later HOB1/HOS1 have an exact shared cross-check without a KMD-to-UMD output channel. After one validation, the KMD context retains a direct session/endpoint reference; the live context object remains identity, Render PDD remains zero, and submit/Present performs no session-table lookup. |
| C59 | A virgl Venus decoder context contains one `VkInstance`: adding another instance asserts/fails, while multiple `VkDevice` objects may exist beneath that instance. Queue `ring_idx` values are unique within that decoder context and ring zero is its CPU timeline. Current Mesa creates its renderer/session state per `vn_instance`. [upstream virgl context](https://cocalc.com/github/PojavLauncherTeam/virglrenderer/blob/master/src/venus/vkr_context.c), [upstream instance dispatch](https://android.googlesource.com/platform/external/virglrenderer/+/refs/heads/main/src/venus/vkr_instance.c), [upstream queue dispatch](https://android.googlesource.com/platform/external/virglrenderer/%2B/056b3873e41c015249499dbf9f761c8e9a78b720/src/venus/vkr_queue.c), pinned Mesa `icd/mesa/src/virtio/vulkan/vn_instance.c:177-184,309-342`, `vn_instance.h:102-126` | host decoder and frontend-instance lifetime | Never merge a process's translators into one host context. Create one bounded HTS1 session, raw HVC1 control context, host context, object namespace, and ring allocator per `vn_instance`. Multiple physical lower queues in that session get unique nonzero ring endpoints; logical D3D queues sharing one physical VkQueue attach to that same endpoint and its short FIFO. |
| C60 | Vulkan device/object creation and requirement/compile-result queries are synchronous API operations, while `vkQueueWaitIdle`, fence waits/status, and `vkGetQueryPoolResults(...WAIT...)` require exact prior GPU progress. In current Mesa these paths issue synchronous renderer calls before any D3D12 queue can exist (`vn_device.c:103-105,573-582`, `vn_buffer.c:262-300`, `vn_image.c:386-407`, `vn_pipeline.c:685-687,1801-1817`, `vn_query_pool.c:201-260`, `vn_queue.c:1494-1504,2796-2879`). D3D12 permits its actual submit callback only synchronously inside `pfnExecuteCommandLists` on the queue-created context. [Khronos device creation](https://docs.vulkan.org/refpages/latest/refpages/source/vkCreateDevice.html), [Khronos query results](https://docs.vulkan.org/refpages/latest/refpages/source/vkGetQueryPoolResults.html), [Khronos queue idle](https://docs.vulkan.org/refpages/latest/refpages/source/vkQueueWaitIdle.html), [Microsoft Resource Heaps](https://microsoft.github.io/DirectX-Specs/d3d/ResourceHeaps.html#submitcommandcb-cannot-pass-more-than-8-handles-in-writtenprimaries) | record-only translator before and after outer queue creation | Pure bounded control/reply commands use the raw HVC1 context and C51 event. Their only allocation access is a finite write into one exact slot of the HTS1-owned raw-device HVM1 reply/feedback pool listed writable in that HVC1 Render; they cannot name, read, write, bind, map, destroy, or infer an outer D3D/resource allocation. The pool is one 64-MiB allocation split into four 16-MiB slots, one outstanding owner per slot, with at most 15 MiB of payload plus one HVR1 header per HNR2 transaction. A logical result is produced once into an immutable session-owned snapshot of at most 64 MiB; at most four snapshots and 256 MiB of host snapshot bytes exist per session. Continuations name the exact snapshot generation and next offset, never re-execute the original operation, and preserve the entry point's partial-result rules. Results above that bound use only a generated legal partition/`VK_INCOMPLETE` rule or make the capability unavailable; no spill allocation exists. Any operation involving outer allocation-backed bytes is recorded/deferred and first materialized in an actual outer batch carrying that exact WDDM allocation. GPU-dependent synchronous operations first flush and join the exact attached outer context's HQC1 milestone, already ordered after its nonzero endpoint work, then issue only the bounded control fetch. Ring zero, raw-control C51 completion, and delayed folding never substitute for outer GPU completion. |
| C61 | Context Monitoring supplies unshared monitored fences plus exact-context GPU signals and event-backed CPU waits. `pfnCreateSynchronizationObject2Cb` is available to WDDM 1.2+ UMDs; the FromGpu/FromCpu callbacks are the Windows-10/WDDM-2 generation. A bottom-of-pipe signal is ordered after earlier command buffers, and `NoSignalMaxValueOnTdr` prevents reset from looking like completion. [Microsoft device callbacks](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dumddi/ns-d3dumddi-_d3dddi_devicecallbacks), [Microsoft context monitoring](https://learn.microsoft.com/en-us/windows-hardware/drivers/display/context-monitoring), [Microsoft GPU signal](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dumddi/nc-d3dumddi-pfnd3dddi_signalsynchronizationobjectfromgpucb), [Microsoft CPU wait](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dumddi/nc-d3dumddi-pfnd3dddi_waitforsynchronizationobjectfromcpucb) | selected outer UMD callback surface; CPU wait only for an API-required synchronous result or C65 bounded pool pressure | Every HQA1-attached outer context owns one private `HQC1` monitored progress fence. The UMD flushes/submits the actual batch and signals a strictly increasing value from that exact context with `TopOfPipeline=0`. It waits once by event only when C60 requires a synchronous result or C65 requires an exact extent to retire. D3D12 already has the WDDM-3.2 callback surface. The D3D11 UMD must therefore negotiate and implement a WDDM-2.1-or-later callback table for this package while retaining its selected physical Render path; the exact Render-plus-modern-callback combination is a mandatory target-runtime gate. Reset cancels the waiter and never publishes success. |
| C62 | `DxgkDdiSubmitCommand` is the KMD submission callback for a scheduler-selected DMA buffer and runs at `DISPATCH_LEVEL`; synchronization-object dependencies are recorded by Dxgkrnl and hold subsequent context work until satisfied. Independent D3D command queues have no implicit cross-queue CPU-call total order. [Microsoft SubmitCommand](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmddi/nc-d3dkmddi-dxgkddi_submitcommand), [Microsoft context monitoring](https://learn.microsoft.com/en-us/windows-hardware/drivers/display/context-monitoring), [D3D12 command queue synchronization](https://learn.microsoft.com/en-us/windows/win32/direct3d12/user-mode-heap-synchronization) | several HQA1-attached WDDM contexts mapped to one physical lower VkQueue endpoint | The endpoint lock is held only while assigning a KMD-owned host-dispatch serial and enqueuing an already scheduler-eligible DMA in actual SubmitCommand arrival order. It is never held across host/GPU completion and never waits for a UMD/CPU sequence from another WDDM context. Same-context order comes from VidSch; cross-context order comes only from documented native-fence waits/signals. This avoids a circular wait in which VidSch withholds context A while a submitted context B batch waits for A. |
| C63 | `DXGKARG_SUBMITCOMMANDVIRTUAL` gives KMD the exact submitted `DmaBufferVirtualAddress`, `DmaBufferSize`, context, and a kernel buffer containing the fixed UMD private-data prefix copied from `SubmitCommandCb`; it does not give KMD a CPU pointer to the variable GPUVA command bytes. [Microsoft virtual-submit structure](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmddi/ns-d3dkmddi-_dxgkarg_submitcommandvirtual), [Microsoft Resource Heaps](https://microsoft.github.io/DirectX-Specs/d3d/ResourceHeaps.html#submitcommandcb-cannot-pass-more-than-8-handles-in-writtenprimaries) | D3D12 virtual context and HPM1 process page-table lifetime | The complete bounded HOB1 actual command stays in the runtime-approved GPUVA buffer. A fixed HOS1 UMD-private descriptor only authenticates package/session/context generation, endpoint, context-local batch ID, length, and checksum; it contains no command/resource payload. At DISPATCH_LEVEL KMD validates HOS1 against the already attached context and nonblocking-enqueues `{process/page-table generation,GPUVA,size}`. QEMU/HPM1 reads the exact current mappings, validates the complete HOB1 and every operand, and performs the writes before completion. KMD never dereferences arbitrary user GPUVA or treats HOS1 as the primary-writing command. |
| C64 | `DxgkDdiSetRootPageTable` supplies a context's current root and is synchronized while that context is idle. `DXGK_BUILDPAGINGBUFFER_UPDATEPAGETABLE` supplies `hProcess`, the exact mapped allocation/offset, PTE entries, and `FirstPteVirtualAddress`; pages are allocation-sequential but may be physically noncontiguous. [Microsoft root-page-table callback](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmddi/nc-d3dkmddi-dxgkddi_setrootpagetable), [Microsoft update-page-table structure](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmddi/ns-d3dkmddi-_dxgk_buildpagingbuffer_updatepagetable) | HPM1 per-process address-space and D3D12 HOB1 execution | The selected package replaces the current decorative page-table handling with an authoritative generation-scoped device page-table service. Root changes, PTE update/copy, TLB flush, map/unmap, and paging completion update QEMU/HPM1 before a later virtual submit can read HOB1 or operands. A virtual submit names only its exact attached ProcessContext/address-space generation; stale roots, PTE epochs, unmapped ranges, cross-process mappings, and logically remapped ADLs are rejected. |
| C65 | A GPUVA-context UMD generates commands directly in user-managed command-buffer storage and manages that pool. `pfnAllocateCb` may create scratch/device-associated memory at any time and expressly associates an allocation with the device when `hResource=NULL`; the WDDM-2 callback table then supplies Lock2, reserve/map/free GPUVA, residency, and paging-fence operations. A bottom-of-pipe monitored-fence signal on the exact context occurs only after prior command buffers complete, and a CPU wait can use one asynchronous event instead of polling. [Microsoft SubmitCommand callback](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dumddi/nc-d3dumddi-pfnd3dddi_submitcommandcb), [Microsoft Allocate callback](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dumddi/nc-d3dumddi-pfnd3dddi_allocatecb), [Microsoft device callbacks](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dumddi/ns-d3dumddi-_d3dddi_devicecallbacks), [Microsoft context monitoring](https://learn.microsoft.com/en-us/windows-hardware/drivers/display/context-monitoring), [Microsoft GPU signal](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dumddi/nc-d3dumddi-pfnd3dddi_signalsynchronizationobjectfromgpucb), [Microsoft CPU wait](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dumddi/nc-d3dumddi-pfnd3dddi_waitforsynchronizationobjectfromcpucb) | D3D12 UMD-owned GPUVA command-buffer lifetime | Each D3D12 device creates exactly one nonshared HOC1 allocation by `pfnAllocateCb{hResource=NULL}` with `pSystemMem=NULL`, Lock2-maps it for its whole lifetime, reserves/maps its stable GPUVA, and makes it resident before use. It is a 64-MiB HLM1-preferred, CPU-visible/WC allocation with at most 256 live 64-KiB-aligned extents; HPM1 owns every local/system PTE transition. After filling an extent, the selected x64 UMD performs the architecture-required WC store drain, release-seals the immutable metadata, and only then calls SubmitCommandCb; an unexpected cache policy or failed publication probe rejects device admission. Every submitted extent is tagged with its owning outer context plus the strictly increasing HQC1 value signaled immediately after that submit. It cannot be overwritten, unmapped, UMD-evicted, unlocked, or freed until that exact value completes. Allocation checks completion once; if no suitable extent exists, it drops the pool lock and event-waits for only the oldest required owner/value, then retries. Reset/removal invalidates the whole pool without treating a value as complete. This bounds memory and descriptors without a per-submit CPU wait, polling loop, or unbounded spill allocation. |
| C66 | Variable synchronous Vulkan results must preserve the entry point's size-query, partial-result, availability, and side-effect rules. `vkGetPipelineCacheData` may return `VK_INCOMPLETE` when the supplied storage cannot hold all data; `vkGetQueryPoolResults` defines exact query ranges, stride, availability, `WAIT`, `NOT_READY`, and partial-result behavior. [Khronos pipeline-cache data](https://docs.vulkan.org/refpages/latest/refpages/source/vkGetPipelineCacheData.html), [Khronos query results](https://docs.vulkan.org/refpages/latest/refpages/source/vkGetQueryPoolResults.html) | generated synchronous reply schema and HTS1 session lifetime | HVR1 identifies one immutable result by package/session/slot/batch/snapshot generations, opcode, final status, total size, exact chunk offset/length, and `MORE`/`FINAL`. The host computes a result once, retains its source objects and at most 64 MiB of immutable bytes until final consume/cancel, and permits only the exact next offset. There are at most four live snapshots/256 MiB per session. A generated table classifies every reply-producing entry point as bounded, legally partial, or partitionable after one required GPU join; an unclassified advertised operation fails device qualification. Reset/device loss cancels snapshots and wakes waiters as failure rather than publishing partial success. |
| C67 | D3D12 deliberately separates binding from residency and object lifetime: applications make the entire possibly accessible set resident and keep referenced resources alive through GPU completion, while descriptors may encode resource GPUVAs and Tier 3 shaders may index a full million-entry heap. Legacy KMT Render instead supplies allocation/patch lists, and `D3DKMT_RENDER` expressly supports requesting and adopting larger next lists through `ResizeAllocationList`, `NewAllocationListSize`, `ResizePatchLocationList`, and `NewPatchLocationListSize`. [Microsoft Resource Binding](https://microsoft.github.io/DirectX-Specs/d3d/ResourceBinding.html), [Microsoft KMT Render](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmthk/ns-d3dkmthk-_d3dkmt_render), [Microsoft Render flags](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmthk/ns-d3dkmthk-_d3dkmt_renderflags), [Microsoft allocation list](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmddi/ns-d3dkmddi-_dxgk_allocationlist) | exact direct operands versus transitive descriptor/object reachability | HOB1/HNR2 carry at most 4096 unique direct WDDM allocation uses and 8192 typed occurrences; 256 remains only a HOC1 live-extent/backpressure count. D3D11 and normal native-Vulkan legacy contexts expose only a generated capability profile whose maximum indivisible allocation closure is at most 4096, return 4096 allocation/output-patch entries, and use the documented resize fields whenever a later returned list is smaller. Batches split only between complete operations and never omit/truncate a listed allocation. D3D12 Tier-3 descriptor contents are not falsely expanded into a per-submit million-entry table: the actual HOB1 names the descriptor heap/root/direct GPUVAs, C64 resolves them through the process page tables, and the HTS1-local Venus object/descriptor graph retains exact allocation generations and Vulkan objects under the API's residency/lifetime rules. The private record-only translator profile may expose the required Tier-3 features; the ordinary legacy native-Vulkan profile clamps any descriptor-indexing limit/feature that would violate the generated 4096-use proof. Neither path uses a raw host resource ID, process-global scan, or heuristic lookup. |

C52/C54's segment, allocation, Lock2, and physical-address rules and C56's
paging/Patch rules are documented Windows facts. HLM1/HPM1 are the selected
Helios hardware/device-model **design proposals** implementing those facts;
Microsoft does not specify QEMU. C55's context/ring fence behavior is an
inference verified from the pinned QEMU/Mesa/upstream source. Neither is left as
an architectural hypothesis: section 18 makes byte identity, arbitrary-page
placement/rebind, real paging completion, finite direct reply, and terminal fence ordering
release-blocking conformance gates. Failure rejects the atomic WDDM-3.2
generation; it does not select another transport.

### 8.1 Local-source conclusions checked at the recorded commits

- Current D3D11 UMD and DXVK ultimately submit translated work through a
  separate Vulkan ICD device; the current bridge and HPS2 calls are listed in
  Appendix A.
- Current D3D12 UMD/vkd3d likewise submits on a lower Vulkan queue and then
  emits WDDM marker/proxy work
  (`umd12/src/forward12/queue.rs:2589-2604,2700-2905,2931-3497`).
- Current KMD can defer a WDDM DMA-completed interrupt until a host fence, but
  its timeout/rebase arm can release the WDDM submission early
  (`kmd_render/src/virtio/gpu/mod.rs:5869-6210,6441-6567`). The former is a
  useful virtual-GPU execution precedent; the latter must be deleted.
- Current KMD advertises WDDM 2.1 and refuses HWQueues
  (`kmd_render/src/ddi/wddm_surface.rs:1-103`,
  `kmd_render/src/ddi/scheduler.rs:168-240`). The selected 3.2 uplift keeps
  traditional contexts/`SubmitCommandVirtual`; it does not revive the
  previously incomplete HWS path recorded at `ROADMAP.md:4083-4092`.
- Current UMD12 already creates an adopted WDDM allocation for every committed
  D3D12 resource and supplies the format/shape metadata that the KMD stamps at
  creation for DWM's D3D11 `pfnOpenResource`
  (`umd12/src/forward12/resource12.rs:90-133,1450-1540,1583-2040`). Its
  refused `pfnOpenHeapAndResource` at `:3308-3394` is the *reverse*
  D3D11-created-to-D3D12 open direction. It remains an interop-completeness
  item, but is not the missing D3D12-to-DWM carrier.
- Current KMD writes `HeliosWddmOpenIdentity` both at create and during ordinary
  `DxgkDdiOpenAllocation` (`kmd_render/src/ddi/create_allocation.rs:1612-1652,
  2456-2505,3025-3095`). C12 permits the create-time write-back but makes the
  ordinary-open mutation read-only. The selected ABI keeps only a create-time,
  generation-safe format/layout descriptor; KMD's allocation object owns the
  backing, and the later restamp is removed.
- Current ICD advertises only opaque Win32 memory/semaphores and rejects
  D3D12-specific imports
  (`icd/mesa/src/virtio/vulkan/vn_physical_device.c:1118-1139,1240-1258,
  3187-3214`; `vn_renderer_helios.c:3708-3712,4680-4694,4900-4910`).
- Current normal ICD creates a KMT device/context only as a probe and then
  creates/executes its Venus context through private Escape; it contains no
  live `D3DKMTRender` submission (`vn_renderer_helios.c:1711-1769,
  5158-5185`, plus the live Escape carrier at `:1399-1464`). Current KMD
  `DxgkDdiCreateContext` still labels Venus creation a stub and returns generic
  buffers (`kmd_render/src/device.rs:382-435`), while its physical submit path
  immediately acknowledges rather than executing an HNR2 host batch
  (`kmd_render/src/ddi/submit_command.rs:1096-1132`). C50-C56 are therefore an
  atomic new documented transport, not a claim that the current probe already
  supplies it.
- Current Mesa performs synchronous device, resource-requirement, pipeline,
  query, fence, and idle calls before or outside a translated queue submit
  (`vn_device.c:103-105,573-582`, `vn_buffer.c:262-300`,
  `vn_image.c:386-407`, `vn_pipeline.c:685-687,1801-1817`,
  `vn_query_pool.c:201-260`, `vn_queue.c:1494-1504,2796-2879`). Current UMD12
  creates the Vulkan/vkd3d device before its runtime queue context
  (`umd12/src/device12.rs:291-312`, `umd12/src/forward12/queue.rs:1208-1281`),
  and its context-create record currently supplies no HQA1 PDD. C58-C64 are
  therefore required transport and call-order changes, not an inference that
  record-only queue sealing already handles synchronous control.
- Current Win32 WSI vehicle deliberately creates D3D11/DXGI/DComp from inside
  the ICD and documents nested D3D11->UMD->DXVK->ICD teardown
  (`wsi_common_win32.cpp:228-260,480-505`). It violates the selected
  no-recursion topology and is removed.
- Helios DXVK D3D11On12 already imports the vkd3d device and exact queue
  (`dxvk-helios/src/d3d11/d3d11_main.cpp:354-426`), obtains the exact Vulkan
  resource from the D3D12 interop object (`d3d11_on_12.cpp:42-105`), and turns
  Acquire/Release into explicit layout transitions (`d3d11_on_12.cpp:109-147`).
  This is implementation precedent, not Windows authority; C33 supplies the
  contract. The selected record-only path preserves that shared-queue shape
  while moving its commands into the one outer D3D12 context batch.
- DXVK loads the Vulkan loader in
  `dxvk-helios/src/vulkan/vulkan_loader.cpp:12-48`; UMD/UMD12 deliberately do
  not link DXGI (`umd/build.rs:254-259`,
  `umd12/src/bridge12.rs:8-14`). The direct-ICD path is therefore a concrete
  required change, not an assumption.

### 8.2 Explicit limits and release-gated inferences

HLM1 is a documented WDDM segment shape, but it is **not** claimed to have
already passed on this target. Current source records that classic
`CpuVisible/CpuTranslatedAddress` experiments reached Code 43
(`kmd_render/src/ddi/gpummu.rs:79-95`,
`query_adapter_info.rs:973-1010`, `bar_segment.rs:64-99`). The checked archive
shows that the 22.22.45 trial was a three-segment layout and also set
`CacheCoherent` on the memory segment. A separate one-segment direct-BAR
"safe" probe omitted the aperture segment needed by the paging DMA pool
(`docs/archive/HANDOFF_BAR_SEGMENT_2026_07_05.md:9-18,59-72`; historical
commit `0c8f44bb9ddad49364348dd31321dc30b7bb762e`,
`kmd_render/src/ddi/query_adapter_info.rs:344-358,575-580`). Later knob examples also
combined mutually exclusive direct/HAP bits. Those are materially different
from, and therefore neither proof nor disproof of, the selected profile:
exactly two segments, aperture id 1 first for paging DMA and direct-linear
memory id 2 second, with id 2
`CacheCoherent=SupportsCpuHostAperture=0`, and a non-overlapping local GPU
range. No checked-in trace proves that exact profile. Consequently cold
AddAdapter, context, Lock2, paging, and visible-DWM admission in section 18 is
release-blocking. Failure leaves this architecture unqualified; it does not
restore HAP1 or any HPS2 fallback.

C57 is selected without collapsing three different interfaces. Legacy
`D3DKMT_OPENRESOURCE` consumes `hGlobalShare`; D3D12 runtime
`pfnOpenHeapAndResource` delivers records only inside the D3D12 UMD; neither is
the lower-ICD carrier. The lower ICD uses the NT-handle interface on its own KMT
device: query the count and buffer sizes, allocate and zero-initialize that many
`D3DDDI_OPENALLOCATIONINFO2` elements and the queried buffers, then call
`D3DKMTOpenResourceFromNtHandle`. The caller supplies storage, not fabricated
allocation records.

`D3DKMTOpenResourceFromNtHandle` expressly says its NT handle is typically
obtained through `D3DKMTShareObjects` or `D3DKMTOpenNtHandleFromName`. The name
helper is only a name-to-NT-handle precursor. This profile already holds an
unnamed NT handle, so it calls neither that helper nor legacy
`D3DKMTOpenResource` before query/open.

Microsoft's published sources conflict on the array direction, and this
reference does not hide that conflict. The Learn member page labels
`pOpenAllocationInfo2` input/reserved, and the shared element declaration labels
`hAllocation` and its private-data fields input. In the released header,
however, the enclosing thunk is `_Inout_`, the member is an
`_Field_size_(NumAllocations)` "Array of open allocation structs", and the
query exists to size that array and its aggregate private-data arena. Most
decisively, Microsoft's published dxgkrnl implementation of this exact
NT-handle ioctl copies a process-local allocation handle and per-allocation
private-data pointer/size back into each element. It dispatches
`LX_DXOPENRESOURCEFROMNTHANDLE` to `dxgkio_open_resource_nt` → `open_resource`
→ `assign_resource_handles`; the latter performs
`copy_to_user(&args->open_alloc_info[i], ...)` after setting `.allocation`,
`.priv_drv_data`, and `.priv_drv_data_size`. That implementation is the Linux
paravirtual dxgkrnl rather than Windows `dxgkrnl.sys`, so it is primary
Microsoft implementation evidence for the ABI direction, not a claim that the
Learn annotations are internally consistent.

The selected carrier therefore has a mandatory target-conformance gate. The
ICD refuses the import and leaves `D3D12_RESOURCE_BIT` unadvertised if open
fails, the returned count differs, `hAllocation` is zero, or the returned
private-data pointer/range is not wholly inside the queried arena. A nonzero
`GpuVirtualAddress` is recorded and cross-checked against any binding that uses
it; zero is legal because this legacy Render/Patch lane names allocations by
`hAllocation` and does not depend on GPUVA. This gate validates the selected
published ABI; it never falls back to a legacy global handle, a named/OPAQUE
object, D3D/DXGI recursion, or a host resource ID.

Current Helios Mesa (`icd/mesa/src/virtio/vulkan/vn_renderer_helios.c`) already
uses this query/zeroed-array/open shape. It must gain exact sizing, validation,
and teardown. Current DXVK/vkd3d retain only `hResource` because they do not need
the per-allocation outputs; their narrower use does not define this profile.

Core 0116 is proven by the released 28000 WDK declaration, and build 28000 is
the selected OS floor. The header alone does not promise that every runtime
will choose `NATIVE` for every fence: implementation admission must observe
successful 0116 negotiation, the native capability query, and only
`NATIVE`/`OPENED_NATIVE` create branches for the supported fence surface. A
`MONITORED` branch is a hard fence-creation failure, not a shadow-timeline or
HPS2 fallback. The package does not advertise optional non-monitored fences.

C3-C8 form the exact queue-order proof: runtime supplies `pKTCallbacks` and the
runtime context; Core 0116 supplies the local native synchronization handle;
the UMD calls the documented wait before later submits and flushes the actual
submit before the documented signal. The Microsoft pages retain the historical
member name `MonitoredFenceValueArray`, and the wait-structure page describes
its scalar alternative only for `D3DDDI_FENCE`; they do not add a separately
named native-value arm. This is a documentation tension, but not an unbound ABI
choice: WDK 28000's signal structure has exactly one value-bearing array at
that union offset; its wait structure has the same sole array carrier; the
callbacks explicitly support all object types; and DEFAULT native fences
explicitly support all existing D3DKMT GPU wait/signal operations. Therefore
the selected DEFAULT-native, `ObjectCount=1` operation supplies `&v` through
that array arm. Member spelling does not create another wire layout. This is a
**conclusion inferred from the cited released declarations and contracts**, not
a claim that an uncited native-specific callback exists.

The separate mapping from the D3D12 queue DDI to those callbacks is a Helios
implementation decision. `pfnWaitForFence` and `pfnSignalFence` are not no-ops,
and this reference does not claim that the runtime automatically emits a
kernel command after they return. Helios resolves their exact `HFENCE` through
Core 0116, calls the C8 callback itself, and returns the precise one-node
`PhysicalAdapterMask`. C27-C28 separately assign Present/DWM ordering to the
actual primary-bearing context submission. HWS, Present HWQueues, feature 41,
Fence Barriers, and hardware flip queue are expressly outside the correctness
chain because their released public contracts do not establish equivalent
`WrittenPrimaries`/DWM semantics or ordinary queue-fence lowering.

The public contracts also do not define DWM restart semantics for a custom
Vulkan layer. The layer relies only on ordinary DXGI device/swapchain error
reporting and must convert removal/reset/surface loss to Vulkan results and
recreate. Runtime validation is mandatory.

No claim is made that:

- native fences transport a user-created object to DWM;
- a Present private-data blob or sync token reaches DWM;
- a KMD-mutated private blob carries a process-local handle;
- a no-op/proxy command may name a primary written elsewhere;
- LUID equality proves Vulkan external-memory compatibility;
- a translator may call through the loader while the WSI layer is installed;
- a timeout, later fence value, or TDR means the intended host work completed.
- Core 0116 itself transfers a fence to DWM;
- HWQueue `WrittenPrimaries`, Present HWQueue arrays, or feature 41 are
  equivalent to the ordinary Resource Heaps contract;
- a legacy `MONITORED` D3D12 fence can be converted from `HFENCE` to a KMT
  handle by this UMD.
- linked-node `PhysicalAdapterMask` fan-out is implied by native-fence sharing;
  this generation exposes exactly one physical node per Helios adapter and
  refuses LDA/cross-adapter fence/resource capabilities.

## 9. Candidate matrix and rejected alternatives

| Candidate | Version / creator / owner | Exact association and transport | Ordering / lifetime / reset / security | Cost / recursion | Decision |
|---|---|---|---|---|---|
| **Actual translated batch on ordinary runtime context + Core-0116 native fences** | build 28000+, WDDM 3.2, Core 0116; one physical node per Helios adapter; runtime creates context/native objects; UMD records; KMD executes | exact runtime context, GPUVA/allocation, `WrittenPrimaries`; `HFENCE` is joined to local native `hSyncObject` at create/open | exact-context callback wait/signal; DMA completion only after host; stock DXGI Present; kernel lifetime | no extra per-Present discovery; one normal submission; no recursion | **Selected for D3D11/D3D12** |
| **Per-`vn_instance` HTS1 pre-queue control plus HQA1 outer-context attach** | one bounded KMD ProcessContext-owned session per translator instance; raw legacy KMT control device/context; outer D3D runtime contexts retain their normal owners | unpredictable session capability is checked once in create-context PDD against exact `hKmdProcess`/adapter/generation; direct context ref thereafter; no resource key | pure synchronous control uses finite HVC1/C51 reply into exact session-owned HVM1 scratch; outer-allocation-backed work remains on the exact outer allocation list; GPU-dependent sync joins exact nonzero endpoint first; session drains after all context/job refs | one finite control transaction per synchronous API need, no polling/global resource table/per-submit lookup/recursion | **Selected for record-only pre-queue control** |
| **Normal-ICD finite legacy KMT Render lane** | Vista+ KMT surface on selected WDDM-3.2 adapter; one HTS1/HVC1 control plus one nonvirtual HVC1 context per real lower Vulkan queue; one host Venus namespace per `vn_instance` session | exact returned command buffer; HNR2 COMMIT repeats exact allocation-list indices and typed resource patches; host ring is session endpoint-owned | fragments stage; COMMIT -> KMD Render -> physical SubmitCommand -> one fenced finite SUBMIT_3D; host/reply completion gates ordered DMA completion; C51 fence supplies CPU milestones | common batch one KMT Render; no shared ring, polling, Escape/IOCTL/loader recursion, raw host ID, or global registry | **Selected for native lower-ICD execution** |
| **HLM1 direct linear CPU-visible memory plus HPM1 paging and HVM1 all-memory allocation class** | documented CPU-visible memory segment; `TRANSFER2` family and full package selected at WDDM 3.2; initialized before segment enumeration | every ordinary device-local or host-visible native-Vulkan memory object is one exact HVM1 WDDM allocation; segment id 2 maps linearly to the complete BAR; HPM1 binds each exact current local/aperture placement to one KMD-owned renderer payload; only host-visible roles Lock2 the direct BAR or byte-identical system view; no CPU Host Aperture | real paging DMA retains source/destination until device completion and publishes one placement epoch; old renderer/BAR bindings drain before revoke; reset invalidates generations | O(page runs/bytes) only on placement change, O(1) Lock2/Unlock2 per active host mapping, zero per batch/Present; per-paging-context state and exact allocation-generation bindings, no scan/poll/global lock | **Selected physical-memory, all-memory allocation, and CPU-map service** |
| **Mandatory WSI layer + shared D3D12 image and Ready/Release fences + real DXGI backbuffer** | Vulkan/D3D12; layer owns per-image objects; exact BGRA8/sRGB/FIFO profile only | exact resource and fence NT handles opened once by same-process lower ICD | explicit external ownership, two single-writer timelines, ordinary DXGI Present; recreate on loss | one exact-format full-frame GPU copy, one bridge submit at present and one at acquire; acyclic only with direct translator ICD dispatch | **Selected for native Vulkan WSI** |
| **Exact current-plane binding on stock flip/MPO/SetVidPnSourceAddress path** | normal WDDM display path; OS/KMD/display host | exact plane/allocation supplied by OS | exact current-allocation ref survives until replacement/unbind and backend reader release | no polling; bounded by active planes | **Selected for direct/independent scanout lifetime** |
| WDDM 2.1 two-fence preflight/postflight proxy | outer UMD plus independent Mesa queue | local fence handles can cross same process | preflight may protect incoming use; postflight no-op did not perform primary write; Queue Wait/Signal mapping incomplete | four queue ops and two proxy submits per batch | Rejected: false primary completion |
| Delay a proxy's DMA completion until lower work | WDDM 2.1 KMD | private ticket can correlate queues locally | delayed completion is not proof that proxy commands wrote its `WrittenPrimaries`; residency set can be incomplete | serialization and ticket table | Rejected |
| User-created shared fence directly to DWM | D3D11.4/D3D12 | exact fence works after open; Present has no handle/value | good primitive, missing stock carrier | handle open per resource if carrier existed | Rejected |
| `ShareObjects` allocation+sync group | Windows 8+ | exact group NT handle; stock OpenResource never receives it | kernel lifetime/security after open | efficient lifetime open; transport gap | Rejected |
| Present private data | Windows 10/1809 | exact resource query but no documented producer/consumer equivalence | query size race; no GPU semantics | per-Present query | Rejected |
| WDDM 2.1 sync token | WDDM 2.1 | opaque resource token, no documented DWM transfer/value | present-batching purpose only | unknown | Rejected |
| D3D11On12 wrapped resource | consenting in-process devices | explicit wrapped resource and fence pair | correct within known peers | cannot alter stock DWM; recursion if invoked by ICD | Precedent only |
| Native fence alone without actual outer submission | WDDM 3.2/Core 0116 | exact create/open now exists, but no Present carrier to DWM | strong queue primitive; cannot legitimize a proxy | low wait cost | Rejected standalone; selected only for API queue fences |
| HWS/HWQueue actual-work path | WDDM 2.2+; feature-41 family later | HWQueue submit has `WrittenPrimaries` fields and fence-broadcast flags | public contract never assigns normal Resource-Heaps front/backbuffer/DWM semantics to that submit | potentially lower CPU overhead | Rejected for retirement correctness |
| Enhanced-Barrier Fence Barriers | developer-mode preview | command-list fence barriers have explicit HFENCE/value DDIs in the proposal | absent from released WDK 28000; no ordinary Queue Wait/Signal contract | potentially direct native instructions | Rejected: unshipped preview |
| KMT redirected Present | WDDM 2.3 | requires runtime-owned PHT/source/sync | no public PHT mint/lifecycle for ICD | potentially fast | Rejected |
| Legacy KMT BLT/flip | Vista+ | source allocation and context exact | windowed BLT is not modern flip; fullscreen flip requires primary/display mode | loses required topology/performance | Rejected |
| Allocation-local KMD hazard scheduler | any | exact only when all accesses are known | GPUVA submissions have no generic per-submit allocation list; cannot infer accesses | global hazard tracking | Rejected |
| `pSystemMem`/`ExistingSysMem` HNS1 | WDDM create allocation | UMD VA/backing exists, but KMD CreateAllocation receives no documented system VA and QEMU has no association | host and VidMm copies can diverge | superficially zero-copy | Rejected: missing KMD/host carrier |
| ShareBackingStoreWithKmd as Venus transport | WDDM 3.1 | exact UMD/KMD backing pointer | no documented QEMU/renderer mapping; shared-resource constraints; not generic host-visible Vulkan memory | would still need host copies/protocol | Rejected |
| MapApertureSegment2 staged command/reply | WDDM 3.0 IOMMU paging | transient BuildPagingBuffer CPU VA arrives after Render | no one-to-one semantic-batch association; pointer ends at Unmap | copies and paging callbacks per batch | Rejected |
| CPU Host Aperture/HAP1 physical-page interpretation | WDDM 2.0+ | public map input is allocation-relative page indices and permits null implicit allocations; it is not a global physical-page ID | no public null-implicit backing identity and current source also observes wrong-IRQL arrivals | would add map/unmap host transactions and page state | Rejected after adversarial pass 68; HLM1 does not advertise the callbacks |
| Stock HOST3D `vn_ring` and whole `RESOURCE_MAP_BLOB` | current Venus/virtio path | UMD receives raw resource ID and one contiguous mapping | head/status polling; whole mapping is not WDDM placement authority | low steady-state copy cost but forbidden polling/identity | Rejected; Windows path replaced by HNR2/HLM1/HPM1 |
| Implicit WSI layer while translators use loader | Vulkan loader | layer intercepts all callers | re-enters D3D UMD->translator->layer | fatal recursion/lock inversion | Rejected; direct translator ICD dispatch is mandatory |
| D3D12 shared intermediate without a validated D3D12-to-D3D11 OpenResource path | any | WSI bridge itself exact | current create/open groundwork exists, but a partial change that fails to preserve and validate it would strand DWM | incomplete | Rejected as a partial implementation |
| Global file/shared memory/service/named registry | ad hoc | global lookup | crash/PID/ACL/reclamation | scan, lock, polling | Forbidden |
| Escape/ticket/poll/sleep/heuristic identity | private | not a documented object association | ABA, timeout, mode transition | stalls and global contention | Forbidden |

## 10. Selected replacement architecture

Sections 10.1–10.9 are one protocol generation. None may ship separately.

### 10.1 Invariants

1. A D3D translated queue has exactly one OS-visible execution queue. No
   command from that logical queue is submitted on an independent Mesa KMT
   context.
2. A command carrying `WrittenPrimaries` contains the batch that actually
   performs those writes. A fence-only or delayed-completion packet may not
   carry the association.
3. Every ordinary WDDM `SubmissionFenceId` is completed only after the
   matching host Venus batch reaches terminal GPU completion. Decode, enqueue,
   QEMU acceptance, timeout, or a later batch is not completion.
4. Every supported D3D12 application fence is a Core-0116 native object.
   Queue Wait/Signal uses the exact runtime context plus exact local
   `hSyncObject`; no shadow HFENCE/timeline exists below it.
5. Present uses the exact source allocation after the actual primary-bearing
   context submission. `WrittenPrimaries`, the context/queue association, and
   ordinary WDDM resource open are the only app-to-DWM ordering route.
6. Native Vulkan never calls D3D/DXGI from the ICD. The separate WSI layer may
   call them because translator Vulkan dispatch bypasses the loader/layer.
7. Custom NT handles are unnamed, same-process, transferred at image creation,
   opened once, and closed after import. DWM never sees them.
8. Present/flip status and display-object lifetime are distinct. The exact
   allocation bound to each plane remains referenced until a later accepted
   binding replaces it or the plane is disabled and the backend releases it.
9. Reset/removal/cancel fails closed. No path continues after an ordering
   failure and no timeout is treated as success.
10. No adapter/process-global resource or synchronization discovery structure
    exists. One exact KMD `ProcessContext` may own a bounded list of HTS1
    sessions solely for create-context admission. Each admitted context stores
    a direct strong reference; submit, Present, allocation open, and display
    paths never search that list.
11. A normal native Vulkan queue has exactly one legacy KMT context. Its actual
    work is submitted with `D3DKMTRender`; it never mixes that context with
    `D3DKMTSubmitCommand`, a virtual context, HWQueue, Escape, or IOCTL work.
12. One HTS1 translation session—not a whole process and not an arbitrary KMT
    device—owns exactly one KMD/host Venus object namespace for one Mesa
    `vn_instance`. Its HVC1 control context uses host ring zero, whose terminal
    point is renderer command processing/reply publication only; every real
    physical lower queue endpoint owns one unique nonzero, non-recycled
    `INFO_RING_IDX` whose host fence represents that VkQueue's completion.
    Queue/device idle and teardown join the required nonzero queue timelines
    before final CPU-only control destruction. A host ring index is never
    treated as a WDDM engine or resource identity.
13. An outer D3D context attaches to an existing HTS1 session exactly once via
    fixed create-context `HQA1` PDD. The raw and runtime devices must resolve
    to the identical KMD process object and adapter generation. Render PDD is
    always zero; no attach, capability lookup, or endpoint discovery is
    permitted after context creation.
14. HVC1 control can execute only the allowlisted pure-control opcode class.
    Anything that creates, binds, maps, reads, writes, or destroys bytes owned
    by a WDDM allocation remains deferred until an actual outer batch names
    that exact allocation and is never resolved through ProcessContext state.
15. A translated synchronous operation whose result depends on GPU execution
    first joins the exact outer context's post-submit HQC1 milestone with one
    event-backed wait and only then performs its bounded ring-zero reply fetch.
    Native-loader queue milestones remain C51-owned. Ring-zero completion never
    satisfies a GPU wait, queue idle, device idle, query WAIT, or destruction
    dependency.
16. HTS1 capabilities are unpredictable admission nonces, not resource IDs or
    a security boundary against code already executing in the same process.
    They are invalidated before reset/removal wakeups, never persist across a
    process/package generation, and are never reused within that generation.

### 10.2 Version and feature admission

The package minimum is Windows 11 26H1/build 28000. KMD reports WDDM 3.2 only
after the complete native-fence, D3D virtual-context, and native-ICD legacy
Render-context DDI surfaces are implemented. Helios exposes one physical node and one Vulkan physical device
per logical adapter in this generation; every enumerated Vulkan physical-device
group used by the WSI layer therefore has `physicalDeviceCount=1`. Multiple
adapters/devices/processes remain independent, while LDA, multi-device Vulkan
groups, and cross-adapter resource/fence sharing are not advertised. The
D3D11 UMD explicitly uplifts from its current WDDM-1.3 table to a
WDDM-2.1-or-later callback table for C61's monitored-fence FromGpu/FromCpu
operations, but continues using its documented physical Render/Present path.
This is a selected package change, not an automatic consequence of the KMD or
D3D12 version. Full table size/caps, CreateContext/Render, and the C61 callbacks
must all pass the target runtime gate; missing support rejects the package.
Admission is conjunctive:

| Gate | Required result | Use |
|---|---|---|
| OS/runtime | build 28000+ and negotiated `D3D12DDI_SUPPORTED_0116` or later | native create **and open** association |
| Core cap | `D3D12DDICAPS_TYPE_0112_NATIVE_FENCE_SUPPORT::NativeGpuFenceSupported=TRUE` | runtime selects native D3D12 fences |
| KMD feature | `DXGK_FEATURE_NATIVE_FENCE` enabled, `DXGK_VIDSCHCAPS::NativeGpuFence=1`, `No64BitAtomics=0` | lifecycle plus native 64-bit CPU/GPU operations |
| native caps | `DXGKQAITYPE_NATIVE_FENCE_CAPS` accepted with truthful stride/range/`MapToGpuSystemProcess` | exact OS mappings and CMP behavior |
| fence branch | every supported create/open is `NATIVE` or `OPENED_NATIVE` | exact `HFENCE -> hSyncObject` mapping |
| topology | one physical node; no LDA/cross-adapter feature bits | exact `PhysicalAdapterMask`, no undocumented fan-out |
| queue model | traditional `pfnCreateContextVirtualCb` + `pfnSubmitCommandCb` | documented Resource-Heaps Present semantics |
| D3D11 callback/Render model | UMD negotiates WDDM 2.1+ full callback table; runtime accepts nonvirtual `pfnCreateContextCb` plus actual `pfnRenderCb`; `pfnCreateSynchronizationObject2Cb`, FromGpu signal, and FromCpu event wait are all non-null/functional; Acquire/ReleaseResource and every newly live table slot are implemented or truthfully refused as its contract permits | D3D11 actual primary-bearing Render plus HQC1 synchronous translated-control joins |
| translated pre-queue control | for every DXVK/vkd3d Mesa `vn_instance`, raw HVC1 INIT creates one distinct HTS1 host context before any dependent outer context; raw and runtime KMD devices expose pointer-identical `hKmdProcess`; every outer context-create HQA1 is accepted once and binds the intended physical lower-queue endpoint; two instances in one process remain distinct; bounded session/ring exhaustion fails before device exposure | synchronous Vulkan device/object control without an independent translated execution queue, merged host instances, or submit-time discovery |
| normal native ICD | before segment enumeration, exact `[aperture id 1,HLM1 id 2]` descriptors, BAR bounds, and C56/HPM1 paging pass cold AddAdapter; enumerate only C54's two supported ordinary memory types; prove device-local image/buffer create-bind-submit-destroy and host-visible local/system/relocation/Lock2 with zero CPU-host-aperture callback; then one control plus one `D3DKMTCreateContext` legacy patch context per exposed real lower queue; HVC1/HNR2 accepted; returned command/allocation/patch buffers nonzero; C53 output patching, C51 monitored fences, and C55 finite direct reply roundtrip succeed | complete no-Escape/no-poll native Vulkan execution and all ordinary Venus storage |
| D3D11 Direct Flip | existing D3D11.1 `pfnCheckDirectFlipSupport`; C43 exact-pair validation, `IMMEDIATE` refused | promotion only for the implemented one-primary resource pair; C61's selected table uplift does not broaden the admitted display profile |
| Vulkan WSI group | Vulkan 1.3, `VK_KHR_swapchain`, one physical device, C45 singleton `LOCAL` commands/structures and alias-image path | no omitted core-1.1 WSI interaction, split-instance bind, or multi-device mask |
| package generation | layer, translators, ICD, UMDs, KMD, protocol, host all match | prevents mixed HPS2/new binaries |

The package deliberately does not advertise HWS/HWQueues or optional
non-monitored fences for this protocol generation. A `MONITORED` D3D12 fence,
missing callback, missing cap, or older Core DDI yields a loud
adapter/device/fence-creation failure. The installer writes no feature registry
override. There is no package fallback to a WDDM-2.1 OS/KMD target and no HPS2
path in the new binaries; the D3D11 UMD's selected WDDM-2.1-or-later callback
ABI is exercised only as part of this WDDM-3.2 package.

### 10.3 Resource identity and allocation object model

#### D3D11

The D3D11 UMD creates and opens allocations through normal runtime callbacks.
For every DXVK/Mesa resource it retains:

- the exact runtime `HANDLE`/KM allocation delivered at create/open;
- immutable allocation private data sufficient to validate type, format,
  dimensions, plane layout, sharing, and host resource construction;
- the exact read/write use for each emitted batch.

KMD creates and owns the host/Venus backing as part of creating the exact WDDM
allocation. At create time it writes a versioned, immutable descriptor into the
`[in/out]` allocation-private buffer: package generation, allocation
generation, allocation kind, size, format, plane/layout, memory/sharing flags,
and reserved/version fields. It contains no host resource token, `resid`, PID,
process handle, synchronization object, mutable value, or independently usable
identity. Dxgkrnl associates the descriptor with the allocation and gives it
to a receiving UMD on OpenResource (C11-C12). KMD only validates/reads the bytes
on an ordinary open; its exact kernel allocation object remains authoritative.

The selected protocol layout is the following pointer-free, little-endian
`HeliosWddmAllocationDescV2` design proposal. It replaces both the current
`HeliosWddmAllocPrivate`/metadata trailer and mutable `HeliosWddmOpenIdentity`;
all C/Rust declarations have `sizeof==168`, `alignof==8`, and field-offset
assertions. The creator allocates the complete buffer, KMD writes it only on
create, and every opener treats it as const:

| Offset | Size | Field | Rule |
|---:|---:|---|---|
| 0 | 4 | magic | `0x32415748` (`HWA2`) |
| 4 | 2 | ABI version | `2` |
| 6 | 2 | structure size | `168` |
| 8 | 8 | package generation | exact atomic-package generation |
| 16 | 8 | allocation generation | nonzero KMD-assigned stale-validation/diagnostic generation; never an identity lookup key |
| 24 | 8 | byte size | exact backing extent, nonzero and bounds every plane |
| 32 | 4 | width | exact texel width; zero only for a non-image allocation kind |
| 36 | 4 | height | exact texel height; zero only for a non-image allocation kind |
| 40 | 4 | depth/array size | exact resource value, nonzero for images |
| 44 | 4 | mip levels | exact resource value, nonzero for images |
| 48 | 4 | DXGI format | exact `DXGI_FORMAT`; `UNKNOWN` only for a kind that has no DXGI image interpretation |
| 52 | 4 | D3D DDI format | exact `D3DDDIFORMAT` supplied/reported to the runtime |
| 56 | 4 | sample count | exact value, nonzero for images |
| 60 | 4 | sample quality | exact value |
| 64 | 4 | allocation kind | versioned protocol enum: buffer, image, standard primary/shadow/staging, or paging object |
| 68 | 4 | flags | bits: `PRIMARY`, `STEREO`, `SHARED`, `DISPLAYABLE`, `DIRECT_FLIP_COMPATIBLE`, `D3D12_RUNTIME_PRIMARY`, `PROTECTED`, `CROSS_ADAPTER`, `CPU_VISIBLE`, `RESOURCE_ASSOCIATED`, `STANDARD`; every other bit zero |
| 72 | 4 | bind flags | shared protocol vocabulary, never raw D3D11/D3D12 bit reinterpretation |
| 76 | 4 | misc flags | versioned protocol vocabulary; reserved bits zero |
| 80 | 4 | VidPn source | exact create-time `VidPnSourceId`; a conventional D3D11 primary has its concrete source, a C44 D3D12 runtime primary must preserve `D3DDDI_ID_UNINITIALIZED` (`0xffffffff`), and a non-primary uses the sentinel |
| 84 | 4 | standard-allocation type | exact OS enum for `STANDARD`, otherwise zero |
| 88 | 4 | swizzle/layout class | versioned KMD/UMD class; Direct Flip requires equality and an explicitly supported class |
| 92 | 4 | memory class | device-local/shared/CPU-visible protocol enum; no Vulkan memory-type index |
| 96 | 4 | plane count | `0..4`; Direct Flip requires exactly one |
| 100 | 4 | reserved | zero |
| 104 | 64 | four plane records | each is `{u64 offset,u32 rowPitch,u32 slicePitch}`; unused records zero; all ranges overflow-check inside byte size |

KMD sets `D3D12_RUNTIME_PRIMARY` only when the D3D12 create record, runtime
`PRIMARY` flag, and required `D3DDDI_ID_UNINITIALIZED` value agree; the bit and
sentinel are cross-validated and neither is inferred by an opener. KMD sets
`DIRECT_FLIP_COMPATIBLE` only when the exact allocation is a non-protected,
non-cross-adapter managed primary in a swizzle/layout class the selected
display backend implements. Those bits are necessary but not sufficient:
the D3D11 UMD must compare both live resource wrappers and every C43 field, and
KMD CheckMPO3 must later accept the actual display attributes. Any malformed,
unknown, truncated, mismatched-generation, or reserved-nonzero descriptor makes
create/open fail; it never selects a legacy parser.

The record-only Mesa backend receives an in-process wrapper for that exact UMD
allocation. Resource creation is an allocation-lifetime bridge operation: the
outer UMD calls its normal runtime allocation callback, KMD creates the backing,
and only then does the private Mesa object bind to the returned exact
allocation wrapper. The ICD does not call DXGI/D3D and no runtime callback is
made while a translator or Mesa lock is held. A host `resid` may remain solely
inside the KMD allocation object, but no UMD/ICD/batch/private descriptor can
name or supply it. The final D3D11 command contains every resource the sealed
Venus batch references and the correct read/write use. Batch references are
allocation-list indices on a legacy patch context or GPUVAs on a GPUVA context,
never raw IDs. Dxgkrnl resolves them for KMD; KMD maps each resolved allocation
object to its internally held host backing.

HTS1 control may create only an unbacked/deferred Venus object record for a
resource that will later own WDDM bytes. The first D3D11 batch that materializes
or binds it must list the exact allocation and write intent; KMD atomically
joins the deferred session object to that allocation for the duration of the
session/allocation references. No raw-control allocation list, ProcessContext
lookup, HQA1 field, or capability value can perform that join.

#### D3D12

The D3D12 runtime owns GPUVA/residency. The UMD creates/opens heaps and
resources through the normal D3D12 DDI, maps the WDDM allocations, and gives
vkd3d/Mesa exact GPUVAs and immutable allocation bindings. Committed resources
continue to call `pfnAllocateCb` during create, as the Resource Heaps contract
requires, and use the same create-time KMD-private descriptor described above.
The private Mesa backend may encode exact GPUVAs/resource-table indices but may
not create a parallel authoritative allocation. The actual ordinary-context batch
accesses only mapped, resident GPUVAs; KMD resolves those mappings to held
allocation/backing objects before host execution.

The D3D12-created-to-D3D11/DWM direction uses the creator's `pfnAllocateCb`
private bytes and the receiving D3D11 UMD's OpenResource path. The reverse
`pfnOpenHeapAndResource` direction is also implemented in the atomic uplift so
D3D11-created shared resources can be opened by D3D12, but it is not
misidentified as DWM's carrier. Neither direction carries a fence, value,
process-local handle, PID, or mutable ticket.

#### Native Vulkan WSI

Each virtual swapchain image is one shareable committed D3D12 texture created
on the exact DXGI adapter. The layer exports its NT resource handle and imports
it into the lower ICD with
`VK_EXTERNAL_MEMORY_HANDLE_TYPE_D3D12_RESOURCE_BIT`. Before creation the layer
requires all of the following for the exact section-10.7 BGRA8/FIFO profile,
tiling, usage union, and handle type:

- D3D12 committed-resource sharing support;
- Vulkan IMPORTABLE and compatible dedicated-only support;
- matching adapter LUID;
- matching Vulkan `deviceUUID` and `driverUUID`;
- a package-private layout/state contract proven for that exact import:
  Vulkan `GENERAL` while externally owned maps to D3D12 `COMMON`, and D3D12
  may transition only `COMMON -> COPY_SOURCE -> COMMON` between matching
  Ready/Release values (C32).

A mismatch fails `vkCreateSwapchainKHR`; there is no buffer-copy or CPU-copy
fallback. `S[i]` and the real DXGI `B[j]` must satisfy every C37
`CopyResource` restriction before either handle is exposed.

The import operation is exact, not an implied “external image” step. The
lower image is created with
`VkExternalMemoryImageCreateInfo{handleTypes=D3D12_RESOURCE_BIT}` and the C37
format/extent/usage. Its allocation chains
`VkImportMemoryWin32HandleInfoKHR{handleType=D3D12_RESOURCE_BIT,handle=H,
name=NULL}` to
`VkMemoryDedicatedAllocateInfo{image=S[i]}` after the exact
`VkExternalImageFormatProperties` query reports both IMPORTABLE and mandatory
DEDICATED_ONLY behavior. The layer calls D3D12
`CreateSharedHandle(S[i],NULL,GENERIC_ALL,NULL)` exactly once, obtains the
permitted `memoryTypeIndex` bits with
`vkGetMemoryWin32HandlePropertiesKHR`, performs the synchronous canonical
import, and retains the unnamed, non-inheritable `H[i]` privately for later
C45 alias imports. Every import through `H[i]` creates a distinct
`VkDeviceMemory`; its `allocationSize` field is ignored for
`D3D12_RESOURCE_BIT` and the implementation queries the size from Windows.
The next lower-ICD step is C57's selected published carrier. The ICD calls
`D3DKMTQueryResourceInfoFromNtHandle` and then
`D3DKMTOpenResourceFromNtHandle` on its own KMT device and reads, per
allocation, the process-local `hAllocation`, the per-allocation
`pPrivateDriverData`/`PrivateDriverDataSize` addressed inside the caller's
`pTotalPrivateDriverDataBuffer`, plus `[out] GpuVirtualAddress`. The local
allocation and immutable HWA2 record are what HNR2 requires; this legacy
Render/Patch lane does not require a nonzero GPUVA. Legacy `D3DKMTOpenResource`
remains a different global-share namespace and is still not a substitute, and
the D3D12 UMD's runtime-provided open-allocation array is still unreachable
from the ICD; neither is needed. The import is admitted only after the C57
admission check passes, and no canonical or alias `VkDeviceMemory` is exposed
if it fails.
The handle is never sent to another process or stored outside the layer's
per-swapchain image state, and is closed once the backing and every alias
import retire. Each imported Vulkan memory holds its own payload reference.
[Khronos Win32 memory import](https://docs.vulkan.org/refpages/latest/refpages/source/VkImportMemoryWin32HandleInfoKHR.html),
[Khronos allocation import rules](https://docs.vulkan.org/refpages/latest/refpages/source/VkMemoryAllocateInfo.html),
[Microsoft KMT resource query](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmthk/nf-d3dkmthk-d3dkmtqueryresourceinfofromnthandle),
[Microsoft KMT resource open](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmthk/nf-d3dkmthk-d3dkmtopenresourcefromnthandle).

Ready/Release use the parallel exact chain:
`VkSemaphoreTypeCreateInfo{TIMELINE,initialValue=0}` at creation followed by
`VkImportSemaphoreWin32HandleInfoKHR{handleType=D3D12_FENCE_BIT,flags=0,
name=NULL}` after the
external-semaphore query reports IMPORTABLE. The D3D12 side creates each fence
at zero with only `D3D12_FENCE_FLAG_SHARED` and exports it through
`CreateSharedHandle(F,NULL,GENERIC_ALL,NULL)`. Win32 semaphore import does not
transfer ownership of the NT handle, so the layer closes it immediately after
a successful permanent import. [Khronos Win32 semaphore import](https://docs.vulkan.org/refpages/latest/refpages/source/vkImportSemaphoreWin32HandleKHR.html).

### 10.4 Record-only translation and actual WDDM submission

A private, versioned in-process interface is added between the D3D UMD bridge,
DXVK/vkd3d, and Helios Mesa. It has no global discovery: the UMD receives the
function table directly while creating its translator instance.

#### HTS1 pre-queue control and outer-context attachment

Translator initialization is not deferred until the first D3D submit. For
each Mesa `vn_instance`, the direct ICD path opens the exact adapter, creates a
raw KMT device, and creates one legacy nonvirtual HVC1 control context. KMD
binds that raw device to the exact `hKmdProcess` supplied by Dxgkrnl and creates
a provisional, refcounted `TranslationSession`. Before the first finite HNR2
`INIT`, the same raw device creates, makes resident, and Lock2-maps exactly one
role-1 HVM1 reply-pool allocation: 64 MiB split into four 16-MiB slots. KMD
binds that allocation directly to the provisional session through the owning
raw-device reference; there is no process/session lookup at Render. INIT lists
the selected slot writable in its exact allocation list, creates one distinct
host virgl/Venus context and `VkInstance`, then
returns a nonzero session generation, a CSPRNG-generated 128-bit capability,
the bounded endpoint capacity, and package/capset confirmation through the C51
event-backed reply. No second `VkInstance` may be created in that host context.
Failure unlocks/destroys the pool, tears down the provisional session, and
fails translator device creation.

The KMD process object owns a bounded list of those sessions only while they
are live. The key is the unpredictable capability plus session generation; it
is consulted only by context creation and is not an allocation, host-object,
PID, or synchronization lookup namespace. A raw and runtime KMD device must
have pointer-identical `hKmdProcess`, adapter object, node, and package
generation. This equality is a mandatory observed target gate rather than an
inference from a PID. Multiple `vn_instance` objects in one process receive
different sessions, host contexts, capabilities, object namespaces, and ring
allocators.

Before the D3D UMD calls the runtime's context-create callback, its direct
bridge selects the translator's physical lower-queue endpoint and supplies the
following 72-byte pointer-free `HeliosQueueAttachV1` (`HQA1`) as the complete
create-context private data:

| Offset | Size | Field | Rule |
|---:|---:|---|---|
| 0 | 4 | magic | `0x31415148` (`HQA1`) |
| 4 | 2 | ABI version | `1` |
| 6 | 2 | structure size | `72` |
| 8 | 8 | package generation | exact atomic-package generation |
| 16 | 8 | session generation | exact nonzero HTS1 generation |
| 24 | 8 | capability low | unpredictable KMD-returned value |
| 32 | 8 | capability high | unpredictable KMD-returned value |
| 40 | 4 | endpoint ID | exact physical lower VkQueue endpoint, nonzero |
| 44 | 4 | engine class | exact graphics/compute/copy class admitted for the outer context |
| 48 | 4 | queue family | diagnostic cross-check only; never identity |
| 52 | 4 | queue index | diagnostic cross-check only; never identity |
| 56 | 8 | context generation | UMD-chosen, nonzero, monotonically increasing and never reused within this HTS1 session |
| 64 | 4 | flags | bit 0 D3D11 physical Render, bit 1 D3D12 virtual Submit; exactly one set |
| 68 | 4 | reserved | zero |

KMD validates HQA1 once during `DxgkDdiCreateContext`, rejects a zero or
duplicate live context generation, stores that generation in the new context,
takes a strong direct
reference to that session/endpoint, and erases no capability into later DMA.
HOB1/HOS1 repeat the stored value only as an anti-stale cross-check; the live
KMD context object remains the identity and no later lookup uses the numeric
generation. Each endpoint has a bounded host-dispatch FIFO shared only by outer contexts
that vkd3d/DXVK explicitly mapped to the same physical VkQueue. The translator
does not assign a cross-context execution sequence. When VidSch invokes KMD for
an already eligible DMA, KMD briefly takes the endpoint lock, assigns the next
host-dispatch serial in actual SubmitCommand arrival order, enqueues it, and
releases the lock before any host/GPU wait. Same-context order is the scheduler
stream; cross-context order exists only through explicit native-fence
dependencies. Ring IDs are unique within one session and are not recycled
before every endpoint job and context reference retires.

Every translated outer command uses one complete contiguous HOB1 record. The
112-byte header is followed by its bounded use table, typed-operand table, and
Venus payload; all offsets are from the HOB1 start and non-overlapping:

| Offset | Size | Field | Rule |
|---:|---:|---|---|
| 0 | 4 | magic | `0x31424f48` (`HOB1`) |
| 4 | 2 | ABI version | `1` |
| 6 | 2 | header size | `112` |
| 8 | 8 | package generation | exact atomic package generation |
| 16 | 8 | session generation | exact live HTS1 generation |
| 24 | 8 | context generation | exact HQA1-attached outer context |
| 32 | 8 | context-local batch ID | nonzero, strictly increasing only on this context |
| 40 | 4 | endpoint ID | exact direct endpoint reference |
| 44 | 4 | flags | exactly one of `D3D11_PHYSICAL=1`, `D3D12_VIRTUAL=2` |
| 48 | 8 | total bytes | header through payload, nonzero, at most 15 MiB, and no larger than the current runtime-approved command buffer |
| 56 | 4 | payload offset | aligned and after both tables |
| 60 | 4 | payload bytes | exact finite Venus command size |
| 64 | 4 | use-record offset | aligned, after header |
| 68 | 4 | use-record count | at most 4096 |
| 72 | 4 | operand-record offset | aligned, non-overlapping |
| 76 | 4 | operand-record count | at most 8192 |
| 80 | 8 | CRC64-ECMA | whole record with this field zero; corruption check, not identity |
| 88 | 24 | reserved | zero |

Each 40-byte use record is `{u64 addressOrIndex,u64 byteLength,
u64 expectedAllocationGeneration,u32 accessFlags,u16 identityKind,
u16 operandCount,u32 firstOperand,u32 reservedZero}`. `identityKind=1` means a
D3D11 allocation-list index with the upper 32 address bits zero;
`identityKind=2` means a D3D12 GPUVA. Access bits are only `READ=1`, `WRITE=2`,
and `PRIMARY_WRITE=4`; primary implies write. Each 16-byte typed operand is
`{u32 payloadOffset,u32 useIndex,u16 operandKind,u16 encodedWidth,
u32 reservedZero}`. It identifies only a generated resource operand whose
payload bytes are zero; arbitrary patches and raw renderer IDs are rejected.
D3D11 KMD converts type 1 to DMA-local physical capabilities during Render/
Patch, and the use table is the complete unique-allocation closure of every
operation in that physical batch. D3D12 QEMU/HPM1 resolves type 2 from the
exact submitted process page tables and requires the mapped KMD allocation
generation/range/access to match. A D3D12 descriptor table does not expand
into one use record per possibly indexed descriptor: the HOB1 names the exact
descriptor-heap/root/direct GPUVAs, while the HTS1-local generated Venus
object/descriptor graph retains exact allocation generations and object refs
under C67's explicit-residency and API-lifetime rules. Context-local Vulkan
handles are never mistaken for a host backing ID.
The record-only encoder splits only at a complete generated command/API
operation boundary before HOB1 sealing. The generated D3D11 profile proves
that one indivisible operation has at most 4096 unique allocation uses and
8192 direct operand occurrences; if that inequality fails, the associated cap
is not exposed. Every piece is independently complete,
fits its current outer command buffer, repeats its exact use/operand tables,
and is submitted in the owning context's order; no HOB1 is fragmented across
two outer WDDM submissions.

For D3D12 only, `pPrivateDriverData` contains this fixed 64-byte HOS1:

| Offset | Size | Field | Rule |
|---:|---:|---|---|
| 0 | 4 | magic | `0x31534f48` (`HOS1`) |
| 4 | 2 | ABI version | `1` |
| 6 | 2 | size | `64` |
| 8 | 8 | package generation | exact package generation |
| 16 | 8 | session generation | exact HTS1 generation |
| 24 | 8 | context generation | exact attached virtual context |
| 32 | 4 | endpoint ID | exact direct endpoint |
| 36 | 4 | HOB1 bytes | equals `CommandLength`/`DmaBufferSize` |
| 40 | 8 | context-local batch ID | equals HOB1 |
| 48 | 8 | HOB1 CRC64-ECMA | equals HOB1 |
| 56 | 8 | reserved | zero |

KMD reports enough `DmaBufferPrivateDataSize` for the 64-byte UMD prefix plus
its fixed KMD suffix. It accepts exactly `DmaBufferUmdPrivateDataSize=64` for
an HQA1 D3D12 context. HOS1 never carries `WrittenPrimaries`; the UMD/runtime
association stays in `D3DDDICB_SUBMITCOMMAND` where C1 defines it.

`D3DDDICB_RENDER::pPrivateDriverData` remains reserved-zero. No Render, Submit,
Present, allocation open, or display callback may attach a session, search the
ProcessContext list, or reinterpret HQA1. D3D11 must create/initialize HTS1
before its runtime physical context; D3D12 does so during vkd3d device creation,
before `pfnCreateCommandQueue` creates its runtime context. If either current
call order cannot meet that rule, device creation fails rather than attaching
late.

Control requests are divided statically:

- **pure control:** bounded instance/device creation, Vulkan object creation or
  metadata that cannot dereference an **outer D3D/resource allocation**,
  capability and memory-requirement queries, descriptor-independent pipeline
  compilation, and their finite replies may execute on HVC1/ring zero. The
  request may read only its copied finite HNR2 bytes and may write only the
  exact slot of the HTS1-owned raw-device HVM1 reply/feedback pool named
  writable in that Render's allocation list; this one session-scratch
  exception is not an outer
  allocation and cannot contain a host resource ID;
- **outer-allocation-backed:** memory allocation/materialization, bind,
  map/cache ownership, transfer, query-result storage, and destruction or
  object materialization that can touch an outer D3D/Vulkan resource
  allocation are represented by frontend/deferred handles and become real only
  in the first actual outer batch whose allocation list/GPUVA names every exact
  allocation; ring zero never receives or resolves such an allocation, and no
  deferred success is exposed unless its WDDM allocation/backing reservation
  has already succeeded;
- **GPU-dependent synchronous:** queue/device idle, fence status/wait, query
  `WAIT`, and teardown first force the owning outer UMD to submit pending
  batches, signal that context's private HQC1 value, and perform one event-backed
  CPU wait. Only after that exact endpoint milestone completes may HVC1 fetch
  bounded result bytes. A nonblocking status query returns the locally known
  not-ready state without a control round trip when the milestone has not
  completed.

The allowlist and maximum input/reply size are part of the package generation.
An opcode in the wrong class, an allocation operand in control, a missing
outer scope, host-dispatch-FIFO exhaustion, or session-capacity exhaustion fails
device creation or removes that device. There is no blocking host Venus
`vkQueueWaitIdle`, shared ring-head sample, or control-fence approximation.

A translator instance is created with
`HELIOS_TRANSLATOR_SUBMISSION_MODE_RECORD_ONLY`. Object/control calls obey the
HTS1 classification above; command recording and queue submit have this
contract:

1. validate all resources against the outer device's allocation/GPUVA wrappers;
2. serialize every translated command, barrier, translator-internal dependency,
   and referenced-resource access for one logical queue operation; D3D12 API
   Queue Wait/Signal and paging-queue operations are deliberately excluded and
   remain OS/runtime operations on the outer queue;
3. seal an immutable batch with a version, HTS1/session generation, endpoint
   ID, owning outer-context/queue generation, monotonically increasing
   **context-local** batch ID, byte length, checksum, and complete resource-use
   table; no field orders work against another WDDM context;
4. return the sealed bytes/resource table synchronously to the outer bridge;
5. perform **no queue-work** KMT Render/SubmitCommand/HWQueue call and signal no
   independent GPU timeline; the separately allowlisted HVC1 control carrier
   above is not an execution queue.

This contract covers every queue entry point, not only ordinary draw submits.
The private dispatcher rejects any `vkQueueSubmit`, `vkQueueSubmit2`,
`vkQueueBindSparse`, `vkQueuePresentKHR`, or queue-idle operation that arrives
without a live outer-operation scope. Translator upload/clear, initialization,
query, breadcrumb, sparse, fence-worker, swapchain, drain, and teardown work is
either recorded into the owning D3D11 flush/D3D12 ECL batch, lowered to the
documented WDDM paging operation in C34, or refused by the corresponding D3D
capability. No background worker may submit GPU work after the outer DDI has
returned. Destruction may CPU-wait for already submitted WDDM work when the API
requires it, but it cannot create a cleanup submission on a lower queue.

The outer UMD encodes the sealed batch as complete bounded
`HeliosOuterBatchV1` (`HOB1`) command bytes in its runtime-approved command
buffer. No user pointer or process-local handle is placed on the wire. D3D11
passes HOB1 through its normal Render command window/allocation list, so KMD
validates and converts it while generating physical DMA. D3D12 leaves HOB1 at
the exact submitted command GPUVA and passes only fixed 64-byte
`HeliosOuterSubmitV1` (`HOS1`) metadata in `pPrivateDriverData`; Dxgkrnl copies
that prefix into KMD private data. HOS1 contains only magic/version/size,
package/session/context generations, endpoint, context-local batch ID, HOB1
length/checksum, and reserved-zero fields. It contains no command bytes,
resource identity, pointer, handle, GPUVA, or host token. At virtual submit KMD
validates HOS1 against the already attached context and nonblocking-enqueues
the exact `{process/page-table generation,DmaBufferVirtualAddress,
DmaBufferSize}` hardware work. QEMU/HPM1 reads and validates HOB1 through the
current process page tables, resolves every GPUVA operand, and executes it.
D3D12 calls `pfnSubmitCommandCb` synchronously in the owning ECL callback;
neither KMD nor QEMU interprets HOS1 itself as work.

Per-queue order is the runtime call order. A bounded queue-local CPU mutex may
serialize simultaneous API callers only while appending/sealing command bytes;
it is released before runtime callbacks and never spans GPU completion.
Different queues/devices/adapters share no submission mutex.

### 10.5 D3D11 execution and Present

The selected D3D11 package negotiates a WDDM-2.1-or-later callback table, not
the current WDDM-1.3 table, while continuing to submit actual commands through
its physical Render context. During device creation it initializes its one
DXVK/Mesa HTS1 session first, selects the immediate context's physical lower
queue endpoint, passes HQA1 to `pfnCreateContextCb`, and creates one private
unshared HQC1 monitored progress fence through
`pfnCreateSynchronizationObject2Cb`. The table, full callback size/caps,
physical-context creation, Render, FromGpu signal, and FromCpu event wait must
all be observed together in the build-28000 gate; no missing callback is
emulated with polling or a raw KMT runtime-context cast.

For an immediate-context flush that has translated work:

1. DXVK closes the record-only Mesa batch.
2. UMD constructs one real D3D11 DMA command containing that batch.
3. UMD supplies every referenced WDDM allocation and exact read/write flag to
   the runtime Render callback.
4. KMD validates and forwards the host batch.
5. KMD reports `DXGK_INTERRUPT_DMA_COMPLETED` for its SubmissionFenceId only
   after host completion, in scheduler-lane order.
6. D3D11 Present/Present1 identifies the exact source allocation. The runtime
   releases it to composition/scanout only after the producing context reaches
   the associated completion.

The DWM process opens the source through normal OpenResource. Its D3D11
UMD/DXVK/Mesa stack follows the same actual-batch path for composition. There
is no HPS lookup on either side.

Present1's required rendering/ownership-release callback remains a normal
runtime callback. No extra callback, fence, or table scan is inserted.

### 10.6 D3D12 execution, fences, and Present

#### Queue creation

For every `D3D12DDI_HCOMMANDQUEUE`:

1. The vkd3d device's HTS1 session already exists from pre-queue control.
   vkd3d selects the exact physical lower VkQueue endpoint used by this logical
   command queue; multiple logical queues may share that endpoint, but the
   selected path does not carry vkd3d's CPU call order across WDDM contexts.
   The KMD endpoint FIFO serializes only scheduler-eligible DMA as C62 defines.
2. UMD fills HQA1 with that session and endpoint, calls the runtime's
   `pfnCreateContextVirtualCb`, and stores the returned runtime
   `HANDLE hContext` in the command-queue object. KMD validates HQA1 once and
   retains the direct session/endpoint reference.
3. UMD creates one private unshared HQC1 monitored progress fence for this
   outer context. It is never exposed as a D3D12 API fence.
4. vkd3d's queue object is bound one-to-one to this outer context in
   record-only mode and creates no lower execution context; its physical
   endpoint may be shared as described in step 1.
5. The adapter does not expose an HWS/HWQueue command-queue path in this
   protocol generation; all ECL work uses ordinary `pfnSubmitCommandCb`.

#### HOB1 pool and retirement

Each D3D12 device allocates one C65 command-buffer pool through the ordinary
runtime allocation/residency callbacks and maps it at stable GPUVAs. It calls
`pfnAllocateCb` with `hResource=NULL`, one zeroed legacy
`D3DDDI_ALLOCATIONINFO`, `pSystemMem=NULL`, and this 64-byte per-allocation
`HeliosOuterCommandAllocationV1` (`HOC1`) record; no resource-level private
data or runtime resource handle exists:

| Offset | Size | Field | Rule |
|---:|---:|---|---|
| 0 | 4 | magic | `0x31434f48` (`HOC1`) |
| 4 | 2 | ABI version | `1` |
| 6 | 2 | structure size | `64` |
| 8 | 8 | package generation | exact atomic package generation |
| 16 | 8 | allocation generation | zero on input; KMD returns nonzero |
| 24 | 8 | byte size | exactly 64 MiB |
| 32 | 4 | extent alignment | exactly 64 KiB |
| 36 | 4 | access | exactly `CPU_WRITE=1|DEVICE_READ=2` |
| 40 | 4 | cache policy | exactly `WRITE_COMBINED=1` |
| 44 | 4 | physical-adapter mask | exactly node bit 0 |
| 48 | 16 | reserved | zero |

KMD admits HOC1 only as a nonprimary, nonshared, CPU-visible/WC allocation
preferred in HLM1, with ordinary system placement supported, no HAP flags, and
real C64/HPM1 page-table handling; it is not an HVM1 renderer resource. UMD
retains its returned `hAllocation`, Lock2 VA, reserved GPUVA range, mapping,
and paging-fence state through device lifetime. It waits any initial
reserve/map/residency paging fence by one event before exposing the pool and
never calls Evict on it. Teardown reverses the order only after all extents
retire: free GPUVA mapping/range, Unlock2, then Deallocate.

The pool is exactly 64 MiB, every extent starts on a 64-KiB boundary, at most
256 extents may be live, and each HOB1 remains at most 15 MiB. A short device-
local allocator lock reserves an extent and records `{queue context,context
generation,batch ID}`; it is released before any runtime, translator, KMD, or
host call. After writing the complete HOB1 record, the x64 UMD executes the
required write-combining store drain (an `SFENCE` or the compiler/platform
primitive that emits the equivalent), then publishes the sealed state with a
release operation. SubmitCommandCb occurs only after that publication; QEMU
reads only after the resulting scheduler/device enqueue. No CPU store may
touch the extent afterward. Device qualification verifies that HOC1 Lock2
really returned the selected WC/uncached mapping and that the drain/submit
boundary exposes every byte; a cache-policy mismatch or torn-byte probe fails
the device. Once submitted, the extent is immutable.

After each successful `pfnSubmitCommandCb`, the UMD immediately inserts the
next strictly increasing HQC1 bottom-of-pipe signal on that same outer context
and records the value on the extent. The extent is reusable only after that
exact HQC1 value has completed. A new allocation performs at most one completed-
value check for candidate extents. If no suitable range exists, it releases the
allocator lock, arms one event wait for the oldest exact owner/value whose
retirement makes space, waits, and retries. It never loops on the mapped value,
holds the allocator lock while waiting, overwrites a pending range, or allocates
an unbounded spill buffer. Signal failure, reset, or removal poisons the device
and leaves all old-generation extents unavailable until teardown.

#### ExecuteCommandLists

For each runtime ECL:

1. on the same thread and before returning from `pfnExecuteCommandLists`, vkd3d
   resolves the exact ordered command lists and closes exactly one bounded
   record-only HOB1 batch for each runtime command-list association. It never
   merges two command lists into one submit and never splits one association;
   each complete batch must fit the HOB1/pool bounds (C28).
2. UMD makes all referenced WDDM heaps resident and resolves any paging-fence
   dependency before access.
3. UMD writes complete HOB1 bytes into the resident runtime-approved command
   GPUVA, fills fixed HOS1 in copied UMD private data, and calls
   `pfnSubmitCommandCb` with that actual command GPUVA/length and exact runtime
   context. HOS1 is metadata, not a proxy command or pointer-bearing payload.
4. The UMD preserves the exact runtime-visible resource and write association
   of those command lists. For ordinary runtime primaries, the runtime tracks
   the presentable allocations and supplies their handles in
   `WrittenPrimaries` on the driver's behalf. The UMD passes that command-list
   association's runtime-supplied array unchanged and adds only an exceptional
   primary it created itself when the contract requires it. It never filters,
   partitions, merges, or invents an ordinary primary and never passes more
   than eight. The
   submitted HOB1 must be the work represented by that same callback—never a
   redirected or omitted primary write. `DXGKARG_SUBMITCOMMANDVIRTUAL` does not
   carry `WrittenPrimaries` to KMD, so the document does not claim KMD
   revalidates an absent field. QEMU independently validates every HOB1
   GPUVA/resource operand (C1/C28).
5. After the submit callback succeeds, UMD inserts the C65 HQC1 signal and
   tags the immutable HOB1 extent with that exact retire value. A signal
   failure removes the device; the extent is never reclaimed speculatively.
6. KMD enqueues the GPUVA submit without dereferencing it; QEMU/HPM1 executes
   the exact HOB1 host work and KMD raises DMA-completed for the exact
   `SubmissionFenceId` only on exact host completion. Only then may the
   following HQC1 packet complete and make the extent reusable.

A command-list segment that references an unknown GPUVA, has a HOS1/HOB1
length/checksum/generation mismatch, redirects or omits a runtime-visible
presentable write, exceeds the callback's primary bound, or names an
allocation/page-table mapping that is not resident/current is rejected before
the UMD callback where locally knowable, by runtime callback validation, or by
the device before execution otherwise, and removes the device. It is never
truncated or reclassified.

Deferred outer-allocation-backed control records follow the same rule: their first
materialization or bind appears inside this actual ECL batch and names the
exact GPUVA/allocation already made resident by the runtime. A synchronous
pre-queue control reply can allocate decoder metadata, but cannot claim that a
D3D12 heap/resource has been bound before this outer submission.

#### Queue Wait/Signal

Core-0116 `pfnCreateFence` handles only `NATIVE` and `OPENED_NATIVE`:

1. For `NATIVE`, `pNativeFenceArgs` and `pfnCreateNativeFenceCb(hRTFence,...)`
   return the exact local `hSyncObject` and mappings. For `OPENED_NATIVE`,
   `pNativeFenceOpenArgs` and `pfnOpenNativeFenceCb(hRTFence,...)` return the
   opener's local equivalents. The UMD stores them in the exact `HFENCE`
   object; the raw NT handle is not retained after the documented open.
2. `pfnWaitForFence(Q,F,v)` sets `PhysicalAdapterMask` to Q's single exact
   physical-node bit, seals/submits any pending earlier actual batch, and then
   calls `pKTCallbacks->pfnWaitForSynchronizationObjectFromGpuCb` with
   `{hContext=Q.hContext,ObjectCount=1,
   ObjectHandleArray=&F.hSyncObject,MonitoredFenceValueArray=&v}` before any
   later batch is submitted. Microsoft specifies that subsequent context
   commands wait, so the call is CPU-asynchronous but queue-ordering.
3. `pfnSignalFence(Q,F,v)` sets the same exact mask, seals/submits all preceding
   actual work, then calls
   `pKTCallbacks->pfnSignalSynchronizationObjectFromGpuCb` with
   `{hContext=Q.hContext,ObjectCount=1,
   ObjectHandleArray=&F.hSyncObject,MonitoredFenceValueArray=&v}`. The array
   arm is the released-ABI conclusion in section 8.2: DEFAULT supports existing
   GPU operations, the callback supports all object types, and no other native
   signal-value arm exists.
4. CPU `ID3D12Fence::Signal`, CPU event waits, `CreateSharedHandle`, and shared
   open remain runtime/native-KMT operations on that same object. A second
   process obtains its own local mapping through `OPENED_NATIVE`.
5. vkd3d emits no Vulkan wait/signal and no engine-shadow fence. A callback
   failure removes the device; it is never treated as a completed value.

HQC1 is separate from every application `HFENCE`. When a synchronous internal
Vulkan result requires prior GPU completion, the owning UMD first seals and
submits all pending actual work, queues
`pfnSignalSynchronizationObjectFromGpuCb(Q.hContext,HQC1,++progress)` with
bottom-of-pipe semantics, and calls
`pfnWaitForSynchronizationObjectFromCpuCb` with one event for that value. The
event is canceled on reset/removal; only success permits the bounded HVC1
control fetch. This path is used only for API-defined synchronous query/idle/
destruction behavior, never for ordinary Execute, Present, acquire, or queue
Wait/Signal.

The package does not advertise optional non-monitored fences. If the runtime
supplies the legacy `MONITORED` branch, fence creation fails because the UMD
has no documented `HFENCE -> hSyncObject` association for it. This is an
admission constraint, not a fallback.

Tile mapping uses `pfnUpdateGpuVirtualAddressCb` on the exact command queue's
context and its documented paging fence (C34). Before a batch accesses the
mapped range, the outer context waits the returned paging-fence value through
the documented context/paging dependency.
The record-only path exposes no Vulkan sparse queue; initial sparse binding and
subsequent Update/CopyTileMappings are WDDM page-table operations. No Venus
worker drains or CPU-spins.

#### D3D11On12 on the same queue

The DXVK D3D11On12 object is a component of the vkd3d device, not a second
WDDM device. `CreateWrappedResource` retains the exact vkd3d resource and its
declared D3D12 input/output states. Acquire changes only component hazard
ownership. Release records the required output-state barriers. `Flush` seals
those D3D11 commands into the owning D3D12 command queue's next record-only
batch, which is submitted once on that queue's exact runtime context (C33).
Any D3D12 Queue Wait/Signal before or after the component work therefore uses
the exact-context callbacks above. No extra KMT context, lower Vulkan
queue, shared handle, or HPS lookup exists.

The implementation gate runs the established pattern
`D3D12 work -> AcquireWrappedResources -> D3D11 work ->
ReleaseWrappedResources -> Flush -> D3D12 work/Present` and proves the two
state transitions and all work appear in one runtime-context order. A wrapped resource
from a different device/adapter/queue is rejected.

#### Present

Present uses the ordinary D3D12 Present DDI for the exact source/destination
allocation(s) and truthful `AddedGpuWork`/multiplicity fields. Before Present:

1. every ECL that writes the backbuffer was an actual `pfnSubmitCommandCb`
   carrying that allocation in `WrittenPrimaries`;
2. KMD has not reported the associated submission fence complete until the
   corresponding host write completes;
3. any preceding API Wait and following Signal are in the same runtime context
   stream through the Core-0116 native object;
4. Resource Heaps' runtime tracking therefore supplies front-buffer
   validation/backbuffer synchronization and the normal DWM/scanout handoff.

No Present-HWQueue array, feature-41 assumption, custom fence, or user fence
crosses to DWM. DXGI/dxgkrnl can consume the exact allocation because the real
producer work is finally visible in the queue and primary association the
runtime already owns.

### 10.7 Native Vulkan KMT execution and WSI layer

#### Normal-ICD KMT execution carrier

The WSI layer cannot replace Escape unless the normal lower ICD can execute all
of its non-translator Vulkan work without Escape. C50 selects one documented
model: legacy patch-mode KMT Render. `ClientHint=VULKAN` is only a declared
client hint, not a grant of private semantics. The authoritative association is
the exact KMT context plus the bounded create-context private data that
Dxgkrnl passes to `DxgkDdiCreateContext`.

At `D3DKMTCreateDevice`, the KMD device object records the exact
`hKmdProcess`/adapter/package generation but creates no host object namespace.
The normal ICD uses one such raw KMT device per `vn_instance`. It first creates
one ordinary control context; that HVC1 context creates the provisional HTS1
session, and its first finite INIT creates exactly one host Venus context and
`VkInstance`. It then creates, for every real lower Vulkan queue, one queue
context on that same raw device by calling
`D3DKMTCreateContext` with the device's exact KMT handle, `NodeOrdinal=0`,
zero-based `EngineAffinity=0`,
`Flags.Value=0`, `ClientHint=D3DKMT_CLIENTHINT_VULKAN`, and this 32-byte,
pointer-free `HeliosVulkanContextV1` (`HVC1`) input:

| Offset | Size | Field | Rule |
|---:|---:|---|---|
| 0 | 4 | magic | `0x31435648` (`HVC1`) |
| 4 | 2 | ABI version | `1` |
| 6 | 2 | structure size | `32` |
| 8 | 8 | package generation | exact atomic-package generation |
| 16 | 4 | capset | exact Venus capset ID |
| 20 | 4 | mode | `2`, finite HNR2 over legacy KMT Render |
| 24 | 4 | Vulkan queue family | copied diagnostic ordinal; both ordinals `UINT32_MAX` mean control; never identity |
| 28 | 4 | Vulkan queue index | copied diagnostic ordinal; both ordinals `UINT32_MAX` mean control; never identity |

KMD accepts HVC1 only when `DXGK_CREATECONTEXTFLAGS::VirtualAddressing=0` and
all other unsupported context flags are zero. The raw KMD device permits
exactly one control context and one HTS1 session, assigns it host ring 0 after
INIT, and restricts that context's
generated opcode schema to C60 pure-control commands and synchronous replies
whose completion is defined by command processing. Its reply pool is an exact
raw-device HVM1 allocation owned by this session; it may not name an outer D3D
  allocation. It may not carry queue execution, an outer-allocation-backed operation,
a GPU-dependent destroy, or any opcode whose result claims prior/future GPU
completion. Each queue context receives a unique
nonzero `INFO_RING_IDX` bound to that real VkQueue and not recycled before
session destruction.
The returned context object retains the KMD device and session generations,
shared host-context reference, endpoint/ring index, and its own FIFO/fence
state; no host context or
ring ID is returned to user mode. It returns a legacy
`DXGK_CONTEXTINFO` with a 256-KiB DMA buffer, a 4096-entry allocation list, a
4096-entry patch-location capacity, and 64 bytes of KMD-only DMA private data,
`DmaBufferSegmentSet=0` (the documented contiguous paged-locked DMA-buffer
case), `Caps.NoPatchingRequired=0`, and every other cap/reserved field zero.
Context creation fails if Dxgkrnl does not return non-null command/allocation/
patch buffers of at least those advertised minima. The implementation never calls
`D3DKMTCreateContextVirtual` or `D3DKMTSubmitCommand` for this context.
The ordinary native-Vulkan profile advertises only descriptor/resource limits
whose generated worst-case indivisible allocation closure fits those 4096
entries. Before a later batch whose closure exceeds a smaller list actually
returned by Dxgkrnl, the ICD uses `ResizeAllocationList` and
`ResizePatchLocationList`, requests 4096 for the next Render, adopts all three
returned pointers/sizes even on failure, and records no over-capacity batch
until the requested capacity is present. This resize transition carries no
application work or primary association and is never a resource-identity
fallback.

Record-only translator instances use the same raw HVC1 control/session INIT
but create no raw HVC1 GPU queue context. Their runtime D3D contexts attach to
the pre-existing session through HQA1 as section 10.4 specifies. Normal native
Vulkan instances create HVC1 queue contexts directly. These are the only two
session endpoint attachment forms; neither shares a session across
`vn_instance` objects.

Each finite Venus submission is one HNR2 batch of at most 15 MiB, split into at
most 64 consecutive Render fragments. Every fragment starts with this 112-byte
`HeliosNativeRenderV2` header; the first has `BEGIN`, the last has `COMMIT`, and
a one-fragment batch has both. There is exactly one incomplete batch per
context, so no fragment lookup or cross-context assembler exists:

| Offset | Size | Field | Rule |
|---:|---:|---|---|
| 0 | 4 | magic | `0x32524e48` (`HNR2`) |
| 4 | 2 | ABI version | `2` |
| 6 | 2 | header size | `112` |
| 8 | 8 | package generation | exact HVC1/package generation |
| 16 | 8 | batch token | nonzero, strictly increasing for this context |
| 24 | 8 | total payload bytes | exact reassembled size, nonzero and at most 15 MiB |
| 32 | 8 | fragment payload offset | exactly the prior offset plus length; first is zero |
| 40 | 4 | fragment payload bytes | fits this returned command buffer after metadata |
| 44 | 2 | fragment index | zero based, exactly expected next index |
| 46 | 2 | fragment count | 1..64 and constant for the batch |
| 48 | 4 | use-record offset | zero before COMMIT; aligned/non-overlapping on COMMIT |
| 52 | 4 | use-record count | zero before COMMIT; equals COMMIT `AllocationCount`, at most 4096 |
| 56 | 4 | patch-record offset | zero before COMMIT; aligned/non-overlapping on COMMIT |
| 60 | 4 | patch-record count | zero before COMMIT; exact parsed resource operands, at most 8192 |
| 64 | 4 | reply allocation-list index | `UINT32_MAX` without reply; valid writable index with `HAS_REPLY` |
| 68 | 4 | flags | only `BEGIN=1`, `COMMIT=2`, `HAS_REPLY=4` |
| 72 | 8 | reply offset | zero without reply; exact aligned range on COMMIT |
| 80 | 8 | reply capacity bytes | zero without reply; with `HAS_REPLY`, exactly `80 + maxChunkBytes`, at most `80 + 15 MiB`, wholly inside one slot |
| 88 | 8 | fragment CRC64-ECMA | corruption diagnostic, never validation authority |
| 96 | 8 | full-payload CRC64-ECMA | zero before COMMIT; exact on COMMIT |
| 104 | 8 | reply slot generation | zero without reply; exact nonzero checked-out slot generation with `HAS_REPLY` |

Each 24-byte COMMIT use record is `{u32 allocationListIndex,u32 accessFlags,
u64 expectedAllocationGeneration,u32 firstPatch,u32 patchCount}`. Only
`READ=1` and `WRITE=2` exist; unknown bits are zero. Each 16-byte patch record
is `{u32 payloadOffset,
u32 allocationListIndex,u16 operandKind,u16 encodedWidth,u32 reserved}`.
Every direct WDDM-allocation-backed operand occurrence has exactly one patch
record, every index is in range, and every directly or transitively reachable
allocation in the legacy batch appears exactly once in the returned
`D3DDDI_ALLOCATIONLIST` with the same `WriteOperation`. Descriptor/object
reachability is expanded into that exact closure before COMMIT; the generated
ordinary-native profile makes any indivisible operation's closure at most
4096. Context-local Vulkan/
Venus object handles may remain context-local protocol operands; an allocation,
shared backing, host resource, or presentation object may not be named by a raw
host ID. The encoder writes zero into every host-resource-id operand. The patch
offset/type/width must identify such an operand in the generated, fully parsed
opcode schema; arbitrary byte patching is rejected. `HAS_REPLY` additionally
requires the KMD to prefix `vkSetReplyCommandStreamMESA` for the exact HVR1
header/payload capacity, allocation/range, and slot generation and set
`GENERATE_REPLY` on the following command inside the same finite host
submission.

The ICD zeroes and fills one `D3DKMT_RENDER` with the exact context,
`CommandOffset=0`, HNR2 `CommandLength`,
`PatchLocationCount=0`, all flags/history/broadcast fields zero, and reserved
`pPrivateDriverData=NULL`/size zero. A non-COMMIT fragment has
`AllocationCount=0`; COMMIT repeats the complete manifest as its exact
allocation list and is rejected if even one payload reference is absent. On
success the ICD adopts the returned next
command/allocation buffers and their actual sizes before recording again. A
batch larger than one command buffer is split into ordered HNR2 fragments,
without changing its Venus command boundaries. Across 64 advertised
256-KiB buffers, the worst-case aggregate is
`15*1,048,576 + 64*112 + 4096*24 + 8192*16 = 15,965,184` bytes, below the
`64*262,144 = 16,777,216`-byte capacity; the encoder must reserve the COMMIT
metadata before choosing every fragment payload length and observe each
returned buffer's actual size. A batch above the 15-MiB/64-fragment bound
returns the Vulkan operation's permitted host/device-memory
failure or loses the device when no such result is legal. Any failed Render
discards the incomplete assembler, loses the queue/context, and ignores
returned buffers rather than resubmitting through another transport.

At `PASSIVE_LEVEL`, `DxgkDdiRender` first validates `CommandLength` against the
advertised maximum and takes a pre-sized slot, then calls the C53
`render_user_copy.c` helper to probe/copy exactly `pCommand[0..CommandLength)`
under `__try/__except`. An exception returns `STATUS_INVALID_USER_BUFFER`,
releases the slot, and dispatches nothing. HNR2 requires
`PatchLocationListInSize=0`; its own typed operand records are inside the copied
command and are not WDDM patch locations. The converted KMD allocation list is
then range/access checked without dereferencing a user handle. KMD validates
every HNR2 range, fragment epoch/order, checksum, opcode, allocation use,
access bit, generation, and KMD allocation object. BEGIN/nonfinal fragments
append only copied bytes to the one context-owned assembler and generate a
small scheduler no-op; they neither list nor retain a resource. COMMIT supplies
the complete allocation list, re-parses the entire assembled stream, verifies
one-to-one typed capability coverage, prefixes an exact reply descriptor when
needed, and rewrites each zero resource operand only to a DMA-local capability
ordinal. It never substitutes a host resource ID at Render time.
It takes one slot from a context-local generation-checked pool capped at 64
outstanding submissions and 15 MiB total. Steady-state reuse allocates nothing;
exhaustion returns resource failure and never waits, scans another context, or
spills to a global queue.

For COMMIT, KMD emits one 48-byte DMA-local `HNR2PhysicalCapability` per use
record: `{u64 allocationGeneration,u32 segmentId,u32 accessFlags,
u64 physicalAddress,u64 allocationOffset,u64 byteLength,u64 hpmEpoch}`. At
Render, KMD pre-patches the current segment/physical address/HPM epoch whenever
the validated kernel allocation list reports `SegmentId!=0`; otherwise those
placement fields remain zero pending residency/Patch. KMD emits one corresponding
`D3DDDI_PATCHLOCATIONLIST` output entry with the exact `AllocationIndex`, a
driver-owned capability ordinal, zero `AllocationOffset`/`SplitOffset` and a
`PatchOffset` wholly inside that capability entry. Output count equals use
count and never exceeds 4096. A 64-byte pointer-free KMD DMA-private record
stores only device/context/slot generations, ring index, slot index, batch
token, lengths, checksum, and capability-table bounds. No raw host resource ID
or pointer enters DMA.

`DxgkDdiPatch` may be called repeatedly. For the OS-valid list/ranges generated
from Render's own output, it infallibly snapshots each exact
`DXGK_ALLOCATIONLIST::hDeviceSpecificAllocation`, access bit, `SegmentId`,
physical address, KMD allocation generation, byte range, and HPM1 placement
epoch into only that DMA-local capability record. It is idempotent and performs
no host call, mapping, allocation, refcount transfer, or other irreversible
action. User-controlled inconsistencies were already rejected by Render; an
impossible output-list/internal-state mismatch is a driver bug because returning
an error from Patch bugchecks Windows. Paging DMA is already self-contained at
BuildPagingBuffer and never depends on a Patch call. Any split submission
contains a self-sufficient, bounds-checked capability subset.

The scheduler invokes the matching legacy physical `DxgkDdiSubmitCommand`.
That callback validates the exact device/context/slot/generation and every
patched capability against the still-current HPM1 physical-page ownership and
epoch. A stale/unresident/mismatched capability removes the context/device; it
is never repaired by looking up a host ID. A fragment
no-op retires normally without a host command. A COMMIT transitions once from
staged to submitted and nonblocking-enqueues one HNR2/HPM1 private hardware
packet. QEMU revalidates the physical capability table, resolves each operand
through the exact HPM1 page owner, substitutes renderer-private resource IDs in
a host-only copy, and issues one `VIRTIO_GPU_CMD_SUBMIT_3D` on the device's one
host Venus context, with
`VIRTIO_GPU_FLAG_FENCE|VIRTIO_GPU_FLAG_INFO_RING_IDX` and the context's exact
ring index. It records the supplied `SubmissionFenceId` and host fence in the
slot. For ring 0, the host callback means only that the permitted control bytes
were processed and their exact reply was release-published; KMD may then retire
that CPU/decode-only WDDM submission, but records no GPU-complete fact. For a
nonzero queue ring, the callback is that VkQueue's terminal fence and KMD
raises `DXGK_INTERRUPT_DMA_COMPLETED` for the WDDM submission only after it.
Because this generation exposes one WDDM render node/engine, KMD
may execute host rings concurrently but retains early completions and reports
the engine's DMA-completed frontier in scheduler submission order. This is
bounded per-device engine state; it is not a global lookup, although a later
CPU-visible completion can experience head-of-line delay behind earlier work
on that engine. Host rejection,
timeout, reset, preemption failure, or a stale callback cancels/removes the
device and never advances a scheduler fence. Context reset/destruction drains
submitted refs and reclaims never-submitted staged slots by their direct
context ownership. Device teardown retires all ring indices before destroying
the one host object namespace; there is no adapter-global ticket lookup.

One explicit, unshared monitored fence per context supplies CPU-visible
progress. On a queue context it may cover a Vulkan fence/timeline CPU wait,
queue-idle, or teardown after that nonzero GPU timeline completes. On the
control context it may cover only ring-0 decoder/reply processing after the
restricted control Render; it never covers queue/device idle or GPU-dependent
destruction. The ICD creates each at context lifetime with
`D3DKMTCreateSynchronizationObject2`: type `D3DDDI_MONITORED_FENCE`, initial
value zero, `EngineAffinity=1u << 0`, `NoSignalMaxValueOnTdr=1`,
`NoGPUAccess=1`, `Shared=NtSecuritySharing=TopOfPipeline=NoSignal=NoWait=0`,
and every other/reserved flag zero. After the last relevant successful Render,
`D3DKMTSignalSynchronizationObjectFromGpu` inserts the next strictly increasing
value on that same `hContext`. Because `TopOfPipeline=0` and the preceding WDDM
submission is not complete until its promised ring-0 processing or nonzero
queue completion, the value cannot advance earlier than that context's stated
completion class. CPU waits use `D3DKMTWaitForSynchronizationObjectFromCpu`; infinite
waits block in KMT, while finite waits use an exact event and retain the armed
event object after timeout until signal or device loss. No code samples or
polls the returned CPU mapping. A fire-and-forget batch needs no extra progress
signal; KMD scheduler completion alone retires its internal slot.

`vkQueueWaitIdle` waits the latest C51 value on that queue's nonzero context.
`vkDeviceWaitIdle`, device reset, and orderly destruction snapshot every live
nonzero queue context, arm/wait each latest queue milestone without holding a
device/queue lock across KMT, and only after all succeed issue/drain any final
CPU-only ring-0 object destruction. A control C51 value is never substituted
for this join. Loss on any queue cancels the device; there is no polling,
host-ring scan, or adapter-global completion value.

Every ordinary native-Vulkan `VkDeviceMemory` allocation, plus Venus reply and
feedback storage, uses C52/C56/HLM1/HVM1. There is no command-ring allocation.
Each object is one ordinary, nonprimary, unshared WDDM allocation and one
KMD-owned renderer resource descriptor of the same page-rounded size. Its
bytes are never an independent private copy. HLM1 is the package's fully
CPU-visible linear local-memory segment; HPM1 is the actual paging/device
protocol that binds each current VidMm placement to the same renderer payload
and copies the bytes on every placement transition. Device-local-only HVM1
objects use that placement without exposing a CPU pointer; only mapped-memory,
reply, and feedback roles use Lock2.
The create/open record is this 64-byte pointer-free
`HeliosVenusMemoryAllocationV1` (`HVM1`):

| Offset | Size | Field | Rule |
|---:|---:|---|---|
| 0 | 4 | magic | `0x314d5648` (`HVM1`) |
| 4 | 2 | ABI version | `1` |
| 6 | 2 | structure size | `64` |
| 8 | 8 | package generation | exact atomic generation |
| 16 | 8 | object generation | zero on input; KMD writes nonzero diagnostic generation |
| 24 | 8 | byte size | nonzero exact allocation/renderer-view size |
| 32 | 4 | role | `1=REPLY_POOL`, `2=VULKAN_HOST_VISIBLE`, `3=FEEDBACK`, `4=VULKAN_DEVICE_LOCAL` |
| 36 | 4 | access | only `CPU_READ=1`, `CPU_WRITE=2`, `HOST_READ=4`, `HOST_WRITE=8`; exact role-compatible subset |
| 40 | 4 | cache policy | `0=NOT_CPU_VISIBLE` only for role 4; `1=WRITE_COMBINED` for roles 1-3 |
| 44 | 4 | segment page shift | zero on input; KMD returns the selected segment value, 12 in this generation |
| 48 | 8 | allocation alignment | zero on input; KMD returns exact alignment |
| 56 | 8 | reserved | zero |

It zeroes `D3DKMT_CREATEALLOCATION`/`D3DDDI_ALLOCATIONINFO2` and calls
`D3DKMTCreateAllocation2` with the exact KMT device, `hResource=0`, no runtime
or resource-level private bytes, one allocation, outer flags zero (therefore
unshared and `ExistingSection=0`), and no private runtime resource handle. The
allocation record has `pSystemMem=NULL`, HVM1 as its only per-allocation private
data, `VidPnSourceId=D3DDDI_ID_NOTAPPLICABLE`, allocation flags zero, priority
`D3DDDI_ALLOCATIONPRIORITY_NORMAL`, and every output/reserved field initially
zero. `CreateShared`, `NtSecuritySharing`, `ExistingSysMem`,
`ExistingKernelSysMem`, `ExistingSection`, and `PermanentSysMem` are all zero.

⛔ SUPERSEDED BY F5 — HPM1 IS DECLINED. Ignore "negotiate HPM1" in this
paragraph; the precondition dies with the protocol, and F2 measured that the
segment needs no negotiation. `DxgkDdiStartDevice` still reserves the complete
prefetchable 64-bit host-visible BAR range and allocates fixed nonpaged
placement state. The immutable segment table is exactly:

1. segment 1, the sole WDDM aperture segment and `PagingBufferSegmentId`; it
   remains the only nonzero `DmaBufferSegmentSet` choice for existing D3D
   runtime contexts, while HVC1 selects zero; and
2. segment 2, HLM1, a memory segment with `Aperture=0`, `CpuVisible=1`,
   `CacheCoherent=0`, `SupportsCpuHostAperture=0`,
   `SupportsCachedCpuHostAperture=0`, and
   `CpuTranslatedAddress=BAR guest-physical base`.

HLM1's segment offset is exactly the offset in that linear PCI aperture, as
the Windows contract requires. `BaseAddress`, `Size`, `CommitLimit`, page size,
`ApplicationTarget`, and `LocalBudgetGroup` match the one negotiated local
physical range. The KMD does not register `DxgkDdiMapCpuHostAperture` or
`DxgkDdiUnmapCpuHostAperture`, and no CPU-host-aperture union member, flag,
table, protocol feature, or implicit-allocation resolver exists. A query while
HPM1/BAR state is not `READY` fails adapter initialization; it never reports a
reduced segment and never initializes at first allocation.

Within that already-live segment service, KMD accepts HVM1 only as one of the
four exact storage roles above. It creates the renderer object/view as a
member of the KMD allocation object and returns exact size/alignment,
physical-adapter index 0, normal priority, HLM1 as the preferred read/write
segment, and the package's ordinary aperture as the documented
system-residency physical-address domain. All roles set only
`AccessedPhysically=1` and `ExplicitResidencyNotification=1`; roles 1-3 also
set `CpuVisible=1,Cached=0`, while role 4 sets `CpuVisible=0,Cached=0` and may
never be passed to Lock2. WDDM-3.2 Flags2 are
`DisablePartialResidency=1` and `RestrictedToSingleSegment=1`; every
unsupported/reserved flag is zero. `AccessedPhysically` is truthful because
the selected legacy physical Render engine actually dereferences the final
segment/physical-address capability patched from the allocation list; it is
not set merely to ask for contiguity. The Flags2 bits require whole allocation
residency in one segment at a time but are not claimed to pin an offset.
HPM1's physical resolver has two explicit, non-searchable domains: HLM1's
current contiguous BAR placement and the ordinary segment-1 aperture PTE table
programmed only by the corresponding documented paging map/unmap operations.
An HNR2 physical capability is accepted only when its named segment/offset
resolves through the current allocation generation/domain/epoch. A
system-aperture placement is never treated as a local page or recovered from
an old MDL snapshot.
The selected target reports no logical DMA remapping/IOMMU mode; all system-
memory ADLs consumed by HPM1 are physical page numbers. A future logical-ADL
profile needs its own proven translation and is not admitted by this package.

HPM1 also owns the authoritative per-ProcessContext GPU address spaces required
by C64. `DxgkDdiSetRootPageTable` records a new root plus monotonically
increasing address-space generation only while Windows has idled the target
context. `UPDATE_PAGE_TABLE`, `COPY_PAGE_TABLE_ENTRIES`, `FLUSH_TLB`, map, and
unmap paging packets carry the exact KMD `hProcess`-derived internal address-
space ID, current root generation, PTE virtual range, allocation generation/
offset when present, and every copied PTE/run needed by the device. User mode
never sees that ID. Paging completion is withheld until QEMU has atomically
published the new mapping/TLB epoch. A D3D12 virtual submit retains that exact
address-space generation through host completion; QEMU walks only those
current PTEs to read HOB1 and each type-2 GPUVA use. The current local
`SetRootPageTable` record-and-ignore and diagnostic-only UpdatePageTable paths
are deleted, not carried forward.

Each HTS1 session has exactly one role-1 reply/feedback pool: a 64-MiB HVM1
allocation divided into four fixed 16-MiB slots. It calls `D3DKMTLock2` once
after create/residency and retains the returned VA until pool teardown. Slot
metadata has `sessionGeneration`, `slotGeneration`, `ownerContext`,
`batchToken`, `replyOffset`, `replyBytes`, `C51Value`, and `state`; one short
session lock protects only
checkout/publication, never Render, host completion, decode, or an event wait.
A slot is not reusable until its matching C51 value completed, the CPU copied
or decoded the reply, and the slot generation was retired. When all four are
busy, the caller drops the lock and event-waits only for the oldest exact slot
needed, then retries. Reset poisons all slots and wakes device-lost; it never
marks a reply complete. A role-2 application memory
object calls it at the Vulkan map boundary and retains it only for that active
map lifetime. A role-4 application memory object rejects map and has no CPU VA.
Lock2 proves a stable CPU virtual view, not a fixed physical/BAR offset. While
resident in HLM1, VidMm maps `CpuTranslatedAddress + current segment offset`;
if VidMm evicts the allocation, Windows preserves the same CPU VA over the
system backing and HPM1's real paging DMA preserves identical byte order. Busy
prior uses are ordered by the exact C51 queue milestones and an event wait,
never by retry polling. An unresolvable backing fails before any pointer is
exposed; internal pool creation fails and an application map returns its legal
memory/device error. `vkUnmapMemory`, reset, or removal withdraws the
application pointer, and `D3DKMTUnlock2` occurs only after CPU and host/GPU uses
retire.

The selected x64 package exposes exactly two ordinary Vulkan memory types over
the one HLM1-budgeted heap: role 4 is `DEVICE_LOCAL` only; role 2 is
`DEVICE_LOCAL|HOST_VISIBLE|HOST_COHERENT`, never `HOST_CACHED`. Every ordinary
non-import `vkAllocateMemory` chooses exactly one role from its selected type;
`vkBindBufferMemory2`/`vkBindImageMemory2` stores the exact HVM1 allocation and
range in the bound object, and the HNR2 encoder derives every use-record from
that binding. Sparse, protected, lazily allocated, multi-instance, and any
other unimplemented memory type/heap/capability are not advertised. The
host-visible type is exposed only after admission proves that every role-2
Lock2 view is the `Cached=0` write-combined/uncached mapping and that CPU
publication before HNR2 Render plus C51/DMA completion before CPU read orders
the same bytes. Khronos defines uncached host memory as coherent. If the target
returns a cacheable mapping or fails the byte/order probe, native Vulkan
admission fails; this generation does not silently advertise a noncoherent or
copy fallback profile. C57 `D3D12_RESOURCE_BIT` imports are a separate
allocation class: they never masquerade as an ordinary HVM1 allocation, are
never exposed through an ordinary memory type, and are never locked.

The segment's byte authority is HPM1 and is maintained independently of CPU
mapping. `DxgkDdiBuildPagingBuffer` does not synchronously copy bytes, wait for
QEMU, or edit live QEMU state. For every reachable
`TRANSFER`/`TRANSFER2`, `FILL`/`FILL2`, `DISCARD_CONTENT`/`DISCARD_CONTENT2`,
ordinary `MAP_APERTURE_SEGMENT{,2}`/unmap, page-table, and residency operation,
it validates the exact allocation, segment/range, flags, page count, and
source/destination MDL or ADL, copies all needed page-run data into the supplied
paging DMA buffer, and returns actual hardware work. `MultipassOffset` is the
sole continuation state when the bounded buffer cannot carry every run.

The selected KMD/QEMU hardware packet is `HeliosPhysicalMemoryDmaV1` (`HPM1`).
Its fixed header contains magic/version/size, package and adapter generations,
operation, physical-adapter index, internal ProcessContext/address-space ID and
root generation (zero only for a non-address-space operation), allocation
generation (zero only for a documented null-allocation/page-table object), GPU
virtual/PTE range when applicable, byte range, source/destination segment IDs,
prior/new placement or TLB epochs, flags, run count, and reserved zero. A bounded run
contains `{u64 sourcePage,u64 destinationPage,u32 pageCount,u32 reservedZero}`;
page numbers are interpreted only in the named source/destination segment.
System-memory runs come from the physical ADL admitted by this non-IOMMU
profile. QEMU never accepts a UMD address, allocation handle, resource ID, or
unvalidated guest pointer in this packet.

BuildPagingBuffer writes every address/run needed by HPM1. VidMm then
**always** calls paging `DxgkDdiPatch`; with no patch-location list it is an
infallible, side-effect-free, no-size-change validation/final-address update of
the self-contained packet. Paging `DxgkDdiSubmitCommand` runs at
`DISPATCH_LEVEL`, snapshots the packet, and only nonblocking-enqueues it to the
device. No host round trip, wait, allocation, or pageable access occurs there.

The device/QEMU executes the paging DMA. Before publishing a local destination
it maps the exact KMD-owned renderer resource at
`BAR base + VidMm segment offset`, copies/fills the bytes, and records the new
allocation generation/placement epoch. Before publishing a segment-1 aperture
destination it installs the exact OS-supplied system-page runs in the HPM1
aperture table. Source placement, renderer mapping, CPU BAR aliases, and host
jobs retain references until the copy and any required QEMU address-space/RCU
transition complete; then the new epoch is atomically visible and obsolete
bindings are revoked. KMD raises the paging `SubmissionFenceId` completion only
after that terminal device acknowledgement. A local-to-local move rebinds only
at this boundary, a system-to-local move copies before the BAR bind becomes
current, and a local-to-system move copies out before local revocation.
`NotifyResidency2` is a postcondition audit, never the ordering edge.

HNR2's repeatedly patched capability is never a stored BAR offset. Submit
accepts local segment 2 only when its exact range is current in HPM1; it accepts
segment 1 only when the corresponding contiguous aperture range resolves to
the current exact system-page runs. Every old binding remains referenced until
queued host jobs drain. Unsupported paging/placement shapes fail the paging or
device before mutation and never leave an independent renderer copy divergent
from VidMm's bytes. Reset generation-invalidates queued paging/jobs and stale
acknowledgements, drains device/address-space transitions, then tears down BAR
bindings. There is no CPU-host-aperture callback, user BAR MDL, whole-resource
search/retry, polling, or raw user-mode resource ID.

Reply storage is fixed and bounded. Each logical result has one nonzero,
monotonic session-local snapshot generation. The host executes the original
operation once, retains every source-object ref, and freezes at most 64 MiB of
result bytes plus final status in an immutable snapshot. At most four snapshots
and 256 MiB of snapshot bytes are live per HTS1 session; the fifth caller drops
the slot/snapshot lock and event-waits for the oldest exact C51 owner. One HNR2
transaction may publish at most 15 MiB into one 16-MiB slot and begins with this
80-byte `HeliosVenusReplyV1` (`HVR1`):

| Offset | Size | Field | Rule |
|---:|---:|---|---|
| 0 | 4 | magic | `0x31525648` (`HVR1`) |
| 4 | 2 | version | `1` |
| 6 | 2 | header size | `80` |
| 8 | 8 | package generation | exact live package |
| 16 | 8 | session generation | exact HTS1 session |
| 24 | 8 | slot generation | equals HNR2 offset 104 |
| 32 | 8 | batch token | equals the requesting HNR2 token |
| 40 | 8 | snapshot generation | exact nonzero retained snapshot |
| 48 | 4 | opcode | exact generated reply opcode |
| 52 | 4 | status | exact signed Vulkan/decoder result |
| 56 | 8 | total bytes | immutable logical-result size, at most 64 MiB |
| 64 | 8 | chunk offset | exact expected next offset |
| 72 | 4 | chunk bytes | at most 15 MiB; payload follows at byte 80 |
| 76 | 4 | flags | exactly one of `MORE=1` or `FINAL=2` |

A generated continuation request carries
`{packageGeneration,sessionGeneration,snapshotGeneration,expectedOffset,
maxChunkBytes}` and may only copy the exact next bytes; it never recomputes the
operation or changes status. The final consume or explicit cancellation drops
the source refs and zeroes the slot. Results above 64 MiB use only a generated
entry-point rule: pipeline-cache data may return the Vulkan-defined partial
`VK_INCOMPLETE`, and query data may be partitioned into exact consecutive query
ranges after one GPU join. An entry point without a legal bounded rule is not
advertised/admitted. Reset, loss, or teardown invalidates snapshots, wakes all
waiters with failure, and never exposes a prefix as success.

HNR2 COMMIT injects `SetReplyCommandStreamMESA` and the reply-producing command
in the same finite direct dispatch. The host writes payload and HVR1, performs
the required release publication, and only then completes its context/ring
fence. C51 wakes the CPU; Mesa validates every HVR1 field before decode. No
`vkCreateRingMESA`, ring head/tail, `NotifyRing`, `WaitRingSeqno`,
`WaitVirtqueueSeqno`, synthetic fetch Render, recomputation, or shared-memory
sampling remains.

Teardown stops admission, drains/cancels every HNR2 use, unlocks each live CPU
mapping, destroys the WDDM allocation and its KMD-owned renderer view only after
all HPM1 placement/host references drain, then destroys contexts and the
device-owned Venus namespace. Adapter stop drains paging DMA and QEMU BAR/
aperture bindings before destroying HPM1 and the BAR region. No shared name,
PID, user mapping capability, or raw host resource ID survives.

[Microsoft KMT context](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmthk/ns-d3dkmthk-_d3dkmt_createcontext),
[KMT Render](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmthk/nf-d3dkmthk-d3dkmtrender),
[KMD Render arguments](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmddi/ns-d3dkmddi-_dxgkarg_render),
[KMD Render validation](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmddi/nc-d3dkmddi-dxgkddi_render),
[KMD context info](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmddi/ns-d3dkmddi-_dxgk_contextinfo),
[physical SubmitCommand](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmddi/nc-d3dkmddi-dxgkddi_submitcommand),
[context monitoring](https://learn.microsoft.com/en-us/windows-hardware/drivers/display/context-monitoring),
[BuildPagingBuffer](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmddi/nc-d3dkmddi-dxgkddi_buildpagingbuffer),
[GPU segments](https://learn.microsoft.com/en-us/windows-hardware/drivers/display/gpu-segments),
[mapping CPU virtual addresses](https://learn.microsoft.com/en-us/windows-hardware/drivers/display/mapping-virtual-addresses-to-a-memory-segment),
[segment flags](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmddi/ns-d3dkmddi-_dxgk_segmentflags),
[segment descriptor](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmddi/ns-d3dkmddi-_dxgk_segmentdescriptor4),
[physical allocation flags](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmddi/ns-d3dkmddi-_dxgk_allocationinfoflags_wddm2_0),
[allocation usage](https://learn.microsoft.com/en-us/windows-hardware/drivers/display/allocation-usage-tracking),
[Lock2](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmthk/nf-d3dkmthk-d3dkmtlock2),
and released WDK `icd/win-build/wdk-include/d3dkmthk.h:74-124,1479-1511,
5017-5109` plus released WDK 28000
`d3dkmddi.h:77-96,136-171,2790-2819,4358-4412,4947-5001`.

#### WSI layer and virtual surface ownership

The package installs one mandatory implicit layer,
`VK_LAYER_HELIOS_present`. The ICD itself stops advertising Win32 surface and
swapchain entry points to ordinary loader clients. The layer advertises only
the base WSI contract needed by this generation and fully virtualizes:

- Win32 surface create/destroy and support/capability/format/present-mode
  queries;
- swapchain create/destroy, image enumeration, acquire/acquire2, present;
- the Vulkan-1.1 singleton device-group Present queries, masks, modes, and
  swapchain/acquire/present structures in C45;
- swapchain-memory alias image create, bind, use, and destroy semantics in
  C45;
- base surface-status and `oldSwapchain` retirement behavior; and
- `VK_KHR_get_surface_capabilities2` only when it returns the same base
  profile and rejects/omits every unadvertised extension structure.

It does **not** advertise `VK_EXT_swapchain_colorspace`,
`VK_EXT_hdr_metadata`, `VK_KHR_incremental_present`, `VK_KHR_present_id`,
`VK_KHR_present_wait`, `VK_GOOGLE_display_timing`,
`VK_EXT_full_screen_exclusive`, `VK_EXT_surface_maintenance1`,
`VK_EXT_swapchain_maintenance1`, `VK_KHR_shared_presentable_image`, or either
FIFO-latest-ready extension. It also withholds
`VK_KHR_swapchain_mutable_format` and every other WSI adjunct that changes the
base profile below. It enumerates no mailbox, immediate,
FIFO-relaxed, shared, or latest-ready present mode. A create/query structure
that depends on an unadvertised extension is never accepted or approximated.

#### Extension discovery, enablement, and dispatch closure

The layer manifest and exported enumeration entry points advertise exactly
`VK_KHR_surface` and `VK_KHR_win32_surface` at instance scope, plus
`VK_KHR_get_surface_capabilities2` only for the base-compatible query path
defined below. At device scope it advertises `VK_KHR_swapchain` only for the
exact admitted Helios physical device. For both `pLayerName` naming this layer
and `pLayerName=NULL`, `vkEnumerateInstanceExtensionProperties` and
`vkEnumerateDeviceExtensionProperties` implement the two-call/
`VK_INCOMPLETE` contract, merge without duplicate names, preserve unrelated
lower extensions, and remove every stale lower Win32-surface/swapchain name.
The layer consumes its instance extensions at `vkCreateInstance` and its
device extension at `vkCreateDevice`; those names are removed from the copied
lower create arrays because the lower ICD deliberately exposes no WSI.

`vkGetInstanceProcAddr` returns the layer chain for instance creation/
destruction, both extension enumerations, `vkCreateWin32SurfaceKHR`,
`vkDestroySurfaceKHR`, all base surface support/capability/format/present-mode
queries, `vkGetPhysicalDeviceWin32PresentationSupportKHR`, the admitted
capabilities2/formats2 queries, physical-device-group enumeration,
`vkGetPhysicalDevicePresentRectanglesKHR`, both queue-family-properties query
forms, device creation, and every available device command below. The layer's
`vkGetDeviceProcAddr` returns its device chain for device destruction,
`vkCreateSwapchainKHR`, `vkDestroySwapchainKHR`,
`vkGetSwapchainImagesKHR`, both Acquire forms, `vkQueuePresentKHR`, both
device-group Present queries, `vkGetDeviceQueue{,2}`, `vkCreateImage`,
`vkDestroyImage`, and `vkBindImageMemory2` plus the `KHR` alias only if that
alias is advertised. It returns `NULL` where the Vulkan proc-address rules say
the extension/core command is unavailable or not enabled. Every other name is
forwarded through the captured next-layer dispatch table. A generated
entry-point manifest is compared byte-for-byte with these lists so an
application can neither bypass the virtual WSI nor pass a virtual handle to a
lower WSI function.

[Khronos instance extension enumeration](https://docs.vulkan.org/refpages/latest/refpages/source/vkEnumerateInstanceExtensionProperties.html),
[device extension enumeration](https://docs.vulkan.org/refpages/latest/refpages/source/vkEnumerateDeviceExtensionProperties.html),
[instance proc-address rules](https://docs.vulkan.org/refpages/latest/refpages/source/vkGetInstanceProcAddr.html),
[device proc-address rules](https://docs.vulkan.org/refpages/latest/refpages/source/vkGetDeviceProcAddr.html).

At intercepted `vkCreateDevice`, the layer first requires the selected lower
physical device to expose Vulkan 1.3. It copies and canonicalizes the caller's
device extension and feature chains, preserves all requested state, adds
`VK_KHR_external_memory_win32` and `VK_KHR_external_semaphore_win32`, and sets
`VkPhysicalDeviceTimelineSemaphoreFeatures::timelineSemaphore` and
`VkPhysicalDeviceSynchronization2Features::synchronization2` to `VK_TRUE`.
It never mutates application memory or inserts duplicate feature structures.
Failure of any C39 query/enablement makes surface support false and device/WSI
construction fail before an NT handle is created.

The helper queue has one selected construction, not a runtime alternative. If
the lower canonical unprotected family reports `q < 2`, Win32 presentation
support is false and `VK_KHR_swapchain` is not advertised. For a WSI-enabled
instance the two queue-family-properties entry points report `q-1` app-visible
queues for that one family and otherwise preserve the lower properties. At
device creation the layer preserves the caller's original queue table for
validation/lookup and first requires an original `flags=0`, nonzero-count
create record for the canonical family. A protected-only request, or a request
whose only unprotected queue belongs to another family, makes WSI-enabled
`vkCreateDevice` fail before creating any lower device or NT object. The layer
clones that exact record's compatible `pNext` policy, increases its lower count
by one, and appends priority `1.0f`; it never manufactures a new flags class
that the application did not request.

The caller's total plus this one queue is therefore at most the real `q`, and
the `(queueFamilyIndex,flags)` uniqueness rule remains true. An uncopyable
queue-create extension, inconsistent global priority, or capacity mismatch
fails `vkCreateDevice` before any WSI/NT object exists. After successful lower
creation the layer retrieves the appended queue at the private index and
never exposes that index through either app-facing `vkGetDeviceQueue` form.
The app-visible queue counts/indices remain exactly those in its original
create request. One device-local mutex covers only calls the layer itself
makes to this private helper queue. The helper is destroyed with the lower
device after every swapchain/helper submission retires.

[Khronos device queue table](https://docs.vulkan.org/refpages/latest/refpages/source/VkDeviceCreateInfo.html),
[queue count/priority rules](https://docs.vulkan.org/refpages/latest/refpages/source/VkDeviceQueueCreateInfo.html),
[queue retrieval](https://docs.vulkan.org/refpages/latest/refpages/source/vkGetDeviceQueue.html).

#### Singleton device-group and swapchain-memory-alias contract

The WSI device is one physical device, one logical-device member at index 0,
and one WDDM node. The layer accepts no linked display adapter, split-instance
binding, remote, sum, or local-multi-device presentation. This narrow topology
does not permit omission of Vulkan 1.1's `VK_KHR_swapchain` interactions:

- physical-device-group enumeration must yield a one-member group for the
  selected adapter. `vkCreateDevice` accepts an absent
  `VkDeviceGroupDeviceCreateInfo` or a one-member record naming that exact
  physical device; any larger/different group is not WSI-capable;
- `vkGetDeviceGroupPresentCapabilitiesKHR` returns `presentMask[0]=1`, every
  other mask zero, and exactly
  `VK_DEVICE_GROUP_PRESENT_MODE_LOCAL_BIT_KHR`;
- `vkGetDeviceGroupSurfacePresentModesKHR` returns exactly `LOCAL` for a live,
  supported layer surface and the ordinary surface-lost result after loss;
- `vkGetPhysicalDevicePresentRectanglesKHR` implements the two-call count/
  array contract and returns one non-overlapping rectangle covering the
  current client extent; the result is refreshed after move/resize/occlusion;
- absent `VkDeviceGroupSwapchainCreateInfoKHR` means `LOCAL`; when present it
  must contain exactly `LOCAL`. `VK_SWAPCHAIN_CREATE_SPLIT_INSTANCE_BIND_REGIONS_BIT_KHR`
  and every non-local mode are rejected rather than clamped;
- `vkAcquireNextImage2KHR` accepts only `deviceMask=1`, records that mask on
  the acquired slot, and otherwise uses the same state machine as
  `vkAcquireNextImageKHR` (whose singleton mask is also 1); and
- absent `VkDeviceGroupPresentInfoKHR` means LOCAL with implicit masks 1. When
  the structure is present, `mode` must still be exactly LOCAL and
  `swapchainCount` must be either zero (implicit masks 1) or exactly the outer
  Present count with every mask 1. Every effective mask must equal the mask
  recorded by the last Acquire. The layer validates this before consuming any
  semaphore.

[Khronos Vulkan-1.1 swapchain interactions](https://docs.vulkan.org/refpages/latest/refpages/source/VK_KHR_device_group.html),
[logical-device group create](https://docs.vulkan.org/refpages/latest/refpages/source/VkDeviceGroupDeviceCreateInfo.html),
[singleton capabilities](https://docs.vulkan.org/refpages/latest/refpages/source/VkDeviceGroupPresentCapabilitiesKHR.html),
[Present masks](https://docs.vulkan.org/refpages/latest/refpages/source/VkDeviceGroupPresentInfoKHR.html),
[Acquire mask](https://docs.vulkan.org/refpages/latest/refpages/source/VkAcquireNextImageInfoKHR.html),
[present rectangles](https://docs.vulkan.org/refpages/latest/refpages/source/vkGetPhysicalDevicePresentRectanglesKHR.html).

The same mandatory interaction allows application-created images backed by
swapchain memory. The layer implements it with real lower objects, never a
synthetic image handle or a process-global translation table:

1. If `VkImageSwapchainCreateInfoKHR::swapchain` is `VK_NULL_HANDLE`, the
   layer consumes and strips only that no-op structure, preserves every other
   valid copied `pNext` item, and performs an ordinary lower image creation.
   It creates no alias record or external import. Likewise, a
   `VkBindImageMemorySwapchainInfoKHR` with `swapchain=VK_NULL_HANDLE` is
   consumed/stripped and the ordinary lower bind is forwarded with the
   caller's `memory` and `memoryOffset`; alias-only null-memory/zero-offset
   rules do not apply. A mixed `vkBindImageMemory2` batch may contain ordinary,
   null-form, and non-null alias entries and preserves their input order and
   output statuses.
2. `vkCreateImage` with a non-null structure accepts a structure that names any
   still-valid layer swapchain, including a retired `oldSwapchain` that has
   not been destroyed. It enforces VUID 00995 against the immutable implied
   section-10.7 image parameters (`flags=0`, 2D, BGRA8, exact extent, one
   mip/layer/sample, optimal tiling, the exact effective swapchain usage and
   sharing-family fields, `UNDEFINED`). It consumes
   the virtual-swapchain structure instead of forwarding it. Other enabled,
   compatible image-create structures are copied to the lower call; a
   conflicting protected, disjoint, format-list, compression, DRM-modifier,
   or other shape fails rather than being dropped or approximated.
3. The layer creates a distinct real lower VkImage from that copied description
   with `VkExternalMemoryImageCreateInfo{handleTypes=D3D12_RESOURCE_BIT}`.
   The lower object is returned to the application and tracked in a
   device-scoped object record; ordinary image views and command-buffer uses
   therefore reach a real lower image directly. The ICD first tags it as an
   alias candidate for that swapchain generation and installs the exact
   canonical `S[i]` backing association only when bind supplies `imageIndex`.
4. For a non-null swapchain bind, `vkBindImageMemory2` requires the outer
   `memory=VK_NULL_HANDLE`, offset zero,
   the same live layer swapchain as image creation, and an in-range
   `VkBindImageMemorySwapchainInfoKHR::imageIndex` (VUIDs 01630, 01631, and
   01644). Because this profile never exposes deferred memory allocation, the
   index need not already have been acquired. An optional
   `VkBindImageMemoryDeviceGroupInfo` is accepted only with both counts zero or
   `deviceIndexCount=1,pDeviceIndices[0]=0` and no split regions. Other enabled
   non-conflicting bind output structures are preserved into the translated
   lower call.
5. The layer reuses that exact slot's private `H[i]`, calls
   `vkGetMemoryWin32HandlePropertiesKHR`, and chooses a compatible returned
   memory-type bit. It allocates one distinct lower VkDeviceMemory with
   `VkImportMemoryWin32HandleInfoKHR{D3D12_RESOURCE_BIT,H[i],name=NULL}` and
   `VkMemoryDedicatedAllocateInfo{image=lower_alias,buffer=NULL}`. The
   `allocationSize` member is not used as an identity or size assertion: the
   D3D12-resource import contract requires Windows to supply the real size.
   The memory is bound at offset zero only to this alias.
6. On success the record becomes exactly
   `lower_alias -> (layer swapchain backing, imageIndex) -> S[i]`. Vulkan's
   identical-external-image alias rules make writes and layout transitions
   through either image apply to the common payload; the normal explicit
   external ownership barriers remain mandatory. Present still selects
   `S[pImageIndices[k]]`; binding an alias never changes presentation identity.
7. Swapchain **retirement** through `oldSwapchain` leaves both canonical and
   alias images usable under the ordinary old-swapchain rules. Before
   **destruction**, the application satisfies C48 by completing its outstanding
   acquired-image operations. The layer drains each in-flight D3D Release and
   DXGI Present. For a slot last transferred to EXTERNAL ownership, it submits
   one final private-helper `EXTERNAL -> canonical-family` barrier from
   `GENERAL` to `GENERAL` and waits on an exact layer-owned Vulkan fence/event;
   a never-presented Vulkan-owned slot needs no invented transition. Only after
   those operations retire does it destroy the canonical swapchain image
   handles and disable Acquire/Present. A separately created lower alias and
   its distinct imported memory stay valid for ordinary image/view/command/
   queue use, with the resulting real layout and canonical-family ownership,
   until the app completes that use and calls `vkDestroyImage`. `S[i]`, `H[i]`,
   and the backing generation remain referenced while any alias exists, so
   parent destruction cannot cause use-after-free or payload substitution.

For a batched `vkBindImageMemory2`, the layer validates the complete input
array and creates every required dedicated import before issuing one lower
batch containing translated alias entries and unchanged ordinary entries. It
preserves each `VkBindMemoryStatus` output chain. If the lower batch fails, it
reports per-bind status when requested; otherwise the affected images remain
indeterminate exactly as the Vulkan contract requires. The layer retains each
possibly bound imported memory until that alias is destroyed and never frees a
payload merely because the aggregate call returned an error.
[Khronos batched bind failure semantics](https://docs.vulkan.org/refpages/latest/refpages/source/vkBindImageMemory2.html).

The exact external-image query must report IMPORTABLE and DEDICATED_ONLY for
the immutable C37 tuple before the layer advertises the surface. Repeated
imports of `H[i]` are expressly distinct VkDeviceMemory objects and do not
transfer ownership of that NT handle. There is one retained resource handle
per `S[i]`; each alias bind performs one memory-property query and one Vulkan
memory import but creates no new NT handle. Acquire and Present perform neither
operation, and no alias lookup exists outside the owning device/swapchain
objects.
[Khronos image-create contract](https://docs.vulkan.org/refpages/latest/refpages/source/VkImageSwapchainCreateInfoKHR.html),
[Khronos bind contract](https://docs.vulkan.org/refpages/latest/refpages/source/VkBindImageMemorySwapchainInfoKHR.html),
[WSI alias semantics](https://docs.vulkan.org/spec/latest/chapters/VK_KHR_surface/wsi.html),
[Win32 repeated import](https://docs.vulkan.org/refpages/latest/refpages/source/VkImportMemoryWin32HandleInfoKHR.html),
[mandatory dedicated import](https://docs.vulkan.org/refpages/latest/refpages/source/VkExternalMemoryFeatureFlagBits.html).

#### Normative copy-only surface profile

Every row is an admission condition. Failure means the surface reports no
support for that physical device/queue family or `vkCreateSwapchainKHR` fails
before any share handle is exposed; there is no conversion or compatibility
path.

| Vulkan-visible contract | Exact D3D12/DXGI realization | Admission/result |
|---|---|---|
| Physical device and present family | D3D12 device and `CreateSwapChainForHwnd` use the exact adapter LUID; lower ICD device/driver UUIDs match; Vulkan 1.3 plus C39 features/extensions are enabled; one canonical unprotected queue family owns external barriers | Any API, feature, extension, LUID, UUID, node, protected-mode, or family mismatch reports unsupported before handle export |
| Surface format | Sole pair `VK_FORMAT_B8G8R8A8_UNORM` + `VK_COLOR_SPACE_SRGB_NONLINEAR_KHR` | Both `S[i]` and `B[j]` are `DXGI_FORMAT_B8G8R8A8_UNORM`; `CheckColorSpaceSupport(DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709)` must include [`DXGI_SWAP_CHAIN_COLOR_SPACE_SUPPORT_FLAG_PRESENT`](https://learn.microsoft.com/en-us/windows/win32/api/dxgi1_4/ne-dxgi1_4-dxgi_swap_chain_color_space_support_flag), then `SetColorSpace1` must succeed (C38) |
| Extent and transform | `currentExtent == minImageExtent == maxImageExtent ==` current nonzero HWND client extent; `supportedTransforms=currentTransform=IDENTITY` | `S[i]` and `B[j]` have the identical width/height/depth required by C37; DXGI uses [`DXGI_SCALING_NONE`](https://learn.microsoft.com/en-us/windows/win32/api/dxgi1_2/ne-dxgi1_2-dxgi_scaling); no scale, rotate, crop, stretch, or letterbox |
| Images and layers | `minImageCount=2`, `maxImageCount=16`, `maxImageArrayLayers=1`; create uses `N=minImageCount` requested by the app, `2 <= N <= 16`, and `imageArrayLayers=1` | Allocate exactly `N` virtual `S` images and `N` real flip backbuffers; both are 2D, one mip, one layer, sample count 1, quality 0 |
| Usage and flags | `supportedUsageFlags = COLOR_ATTACHMENT | TRANSFER_SRC | TRANSFER_DST`; the effective usage (`imageUsage`, or enabled usage2 override) is a nonzero subset, `flags=0`, `preTransform=IDENTITY`, `compositeAlpha=OPAQUE` | Before advertising support, the union of all three usages must pass the exact Vulkan external-image-format import query. Canonical and alias images use the exact swapchain's effective usage. `S[i]` is a dedicated imported shared committed resource with `ALLOW_RENDER_TARGET`; `B[j]` has `DXGI_USAGE_RENDER_TARGET_OUTPUT`; alpha is [`DXGI_ALPHA_MODE_IGNORE`](https://learn.microsoft.com/en-us/windows/win32/api/dxgi1_2/ne-dxgi1_2-dxgi_alpha_mode) |
| Present mode | Sole `VK_PRESENT_MODE_FIFO_KHR` | [`DXGI_SWAP_EFFECT_FLIP_DISCARD`](https://learn.microsoft.com/en-us/windows/win32/api/dxgi/ne-dxgi-dxgi_swap_effect); `Present(1,0)`; no tearing flag or frame replacement (C38) |
| Window/fullscreen | Ordinary HWND window, including a borderless monitor-sized window | D3D applications retain stock DXGI windowed/exclusive behavior. Native Vulkan advertises no application-controlled exclusive-fullscreen extension; borderless fullscreen may transition among composed/direct/independent flip under stock DXGI (C10) |
| Copy operation | Full resource only | `S[i]` and current `B[j]` pass every C37 equality check, then one asynchronous `CopyResource`; no shader, blit, format reinterpretation, scaling, resolve, color conversion, alpha blend, HDR metadata, or tonemap |

When the Win32 client extent is `(0,0)`, surface queries report that exact
extent and swapchain creation fails until it becomes nonzero (C38). A resize
that changes either dimension makes the old swapchain incompatible and both
Acquire and Present return `VK_ERROR_OUT_OF_DATE_KHR`; this generation never
uses `VK_SUBOPTIMAL_KHR` to keep copying mismatched resources. Destroyed HWND
state returns `VK_ERROR_SURFACE_LOST_KHR`. DXGI device removal/reset maps to
`VK_ERROR_DEVICE_LOST`. The selected flip-model swapchain is not expected to
return `DXGI_STATUS_OCCLUDED`; if the target runtime does so contrary to the
flip-model status contract, the layer retires the swapchain as out of date
rather than inventing an occlusion protocol. [Microsoft Present results](https://learn.microsoft.com/en-us/windows/win32/api/dxgi/nf-dxgi-idxgiswapchain-present),
[Microsoft DXGI status](https://learn.microsoft.com/en-us/windows/win32/direct3ddxgi/dxgi-status),
[Khronos result meanings](https://docs.vulkan.org/refpages/latest/refpages/source/VkResult.html).

`oldSwapchain` remains a base requirement: create the new exact `S`/`B` set
first, atomically retire the old acquisition/present state only after success,
and drain the old presentation state under the teardown contract below. A
still-live old swapchain may continue to own alias images; its `S` backing and
private resource handles are destroyed only after those aliases and all GPU/
presentation references retire.
Windowed and borderless-fullscreen operation are supported; broader formats,
HDR, timing, tearing, scaling, stereoscopic arrays, protected presentation,
and application-controlled exclusive fullscreen require a future real D3D
rendering/conversion design and are non-goals of HPS2 retirement.

The layer creates a D3D12 device/queue and DXGI flip-model swapchain on the
exact adapter. D3D12 uses the selected actual ordinary-context translator
path. The swapchain owns its real backbuffer set `B[0..N)`. Each `S[i]` is
created with `CreateCommittedResource` using `D3D12_HEAP_TYPE_DEFAULT`, one
physical node (`CreationNodeMask=VisibleNodeMask=1`),
`D3D12_HEAP_FLAG_SHARED` only, `D3D12_TEXTURE_LAYOUT_UNKNOWN`, alignment zero,
the exact C37 texture description, `D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET`,
`pOptimizedClearValue=NULL`, and initial state `D3D12_RESOURCE_STATE_COMMON`.
`ALLOW_DISPLAY`,
`SHARED_CROSS_ADAPTER`, resource `ALLOW_CROSS_ADAPTER`, simultaneous access,
protected, not-resident, and not-zeroed flags are all zero. The runtime may
apply the implicit heap-tier denial flags required for a committed resource;
the layer never guesses them. For each virtual Vulkan image `i` in `S[0..N)`
the layer owns:

- exact-profile shared committed texture `S[i]`, returned to the app as a
  canonical lower-ICD VkImage only after the exact import below succeeds;
- one retained unnamed, non-inheritable resource handle `H[i]` and a bounded
  device-scoped list of real lower alias images/imported memories for that
  exact backing;
- single-writer D3D12 fences `Ready[i]` (Vulkan writes, D3D waits) and
  `Release[i]` (D3D writes, Vulkan waits);
- permanently imported Vulkan timeline semaphores for both fences;
- monotonically increasing epoch `e[i]`;
- one D3D12 copy command allocator and one copy command list;
- one resettable lower-Vulkan command pool with separate per-slot
  present-release and acquire barrier command buffers; and
- image state `NEVER_USED | AVAILABLE | ACQUIRED | PRESENT_QUEUED | D3D_COPY |
  DXGI_OWNED | RELEASE_QUEUED | ALIAS_ONLY | LOST`, together with the
  exact epoch whose Release completion permits allocator/command-buffer reuse.

The canonical import is completed before `vkGetSwapchainImagesKHR` can expose
the image. For each slot, the layer creates a real lower `VkImage` with the
immutable C37 image description and
`VkExternalMemoryImageCreateInfo{handleTypes=D3D12_RESOURCE_BIT}`; queries
`H[i]` with `vkGetMemoryWin32HandlePropertiesKHR`; obtains the image memory
requirements; selects a compatible returned memory-type bit; and allocates a
distinct `VkDeviceMemory` whose copied chain contains
`VkImportMemoryWin32HandleInfoKHR{D3D12_RESOURCE_BIT,H[i],name=NULL}` and
`VkMemoryDedicatedAllocateInfo{image=canonical_S_i,buffer=NULL}`. It binds
that memory at offset zero, installs the exact slot association, and only then
returns the canonical image handle. The requirement size supplies a valid
nonzero allocation field, but Windows remains authoritative because the
`D3D12_RESOURCE_BIT` import contract ignores `allocationSize`. The canonical
image and memory remain alive with `S[i]` until the slot's complete backing
lifetime retires. There is no implied, lazy, or first-Present import.
The lower allocation acquisition step is C57's selected published carrier, performed
once per slot on the ICD's own KMT device and never per frame. The ICD calls
`D3DKMTQueryResourceInfoFromNtHandle(hDevice=ICD device, hNtHandle=H[i])` to
obtain `NumAllocations`, `TotalPrivateDriverDataSize`, and
`ResourcePrivateDriverDataSize`; requires `NumAllocations==1` for the exact C37
committed profile; sizes one `D3DDDI_OPENALLOCATIONINFO2` element and the
`pTotalPrivateDriverDataBuffer` arena from those returns; and calls
`D3DKMTOpenResourceFromNtHandle` on the same device. The open returns the
process-local `hResource`, and in the array element the process-local
`hAllocation`, that allocation's `pPrivateDriverData`/`PrivateDriverDataSize`
addressed inside the supplied arena, plus `[out] GpuVirtualAddress`. The handle
and KMD-authored per-allocation private data are HNR2's identity and immutable
descriptor inputs; GPUVA is retained only when returned and is not a legacy
patch-list prerequisite. The per-allocation private data is the same create-time immutable descriptor the
D3D11 open path already consumes. The import is admitted only if the status is
success, `NumAllocations` still matches, `hAllocation` is nonzero, the private
data lies wholly inside the arena at the queried size, and any nonzero
`GpuVirtualAddress` agrees with a binding that uses it; any failure refuses the import and leaves the
`D3D12_RESOURCE_BIT` capability unadvertised, per section 10.9. The whole
import closes after every alias and GPU reference drains with
`D3DKMTDestroyAllocation2{hResource,phAllocationList=NULL,AllocationCount=0,
Flags=0}`, then the retained NT handle closes. No D3D or DXGI call, legacy
global share, named or OPAQUE object, or raw host ID participates.
[Khronos external-image create chain](https://docs.vulkan.org/refpages/latest/refpages/source/VkExternalMemoryImageCreateInfo.html),
[Khronos Win32 memory import](https://docs.vulkan.org/refpages/latest/refpages/source/VkImportMemoryWin32HandleInfoKHR.html),
[Microsoft KMT query](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmthk/ns-d3dkmthk-_d3dkmt_queryresourceinfofromnthandle),
[Microsoft KMT open](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmthk/ns-d3dkmthk-_d3dkmt_openresourcefromnthandle).

Each fence is `CreateFence(0,D3D12_FENCE_FLAG_SHARED)` under Core 0116; neither
`SHARED_CROSS_ADAPTER` nor `NON_MONITORED` is present. The layer creates one
unnamed, non-inheritable `GENERIC_ALL` NT handle, passes it synchronously to
`vkImportSemaphoreWin32HandleKHR`, and closes the transient handle. The Vulkan
object is created with
`VkSemaphoreTypeCreateInfo{semaphoreType=TIMELINE,initialValue=0}` and imported
with `VkImportSemaphoreWin32HandleInfoKHR{handleType=D3D12_FENCE_BIT,flags=0,
name=NULL}`. The NT
import retains its own object reference rather than taking handle ownership,
so closing the transient handle after successful import is mandatory. The ICD's
import path calls `D3DKMTOpenNativeFenceFromNtHandle` on its exact KMT device,
stores the returned local `hSyncObject`/mappings in that Vulkan semaphore, and
never calls D3D/DXGI. Its normal Vulkan KMT context implements the imported
timeline wait/signal through the documented KMT FromGpu operations on that
local object and exact context. Thus Ready crosses `ICD -> D3D12 UMD` and
Release crosses `D3D12 UMD -> ICD` by one documented native-fence NT handle
open per fence lifetime; no value or handle is rediscovered per frame.

The lower ICD tags each `S[i]` as the exact presentable image belonging to its
layer swapchain through a device/image-scoped private dispatch call. That tag
is stored on the image object; it is not a process-global lookup. It makes
`VK_IMAGE_LAYOUT_PRESENT_SRC_KHR` legal for these returned images even though
the lower ICD owns no Windows swapchain, and it lets the ICD validate the
layer's `PRESENT_SRC_KHR -> GENERAL -> VK_QUEUE_FAMILY_EXTERNAL` release and
the reciprocal `GENERAL -> PRESENT_SRC_KHR` acquire. Untagged images never get
this treatment.

The layer reports surface support only for a canonical unprotected queue family
that can execute the required image barriers and transfers. It creates `S[i]`
with the app-requested `VkSwapchainCreateInfoKHR::imageSharingMode` and family
list: exclusive uses that canonical present family; concurrent requires the
canonical family in the supplied list. A split exclusive graphics/present
configuration remains legal only when the application performs the ordinary
ownership transfer to the canonical present family before `vkQueuePresentKHR`.
External ownership remains explicit regardless of Vulkan sharing mode.

Acquire has no queue parameter, so the layer uses only the C30 queue reserved
at `vkCreateDevice`. It is a real lower queue in the canonical family, is never
returned through either app-facing queue getter, and is not shared with an
application queue. One device-local mutex serializes the layer's own calls to
that private queue; no app-visible queue dispatch wrapper or application queue
lock is part of this design. No mutex is held while waiting for completion or
across a D3D/DXGI call. If the real lower family cannot reserve this additional
queue, Win32 WSI is not advertised.

#### Acquire

An image is selectable only if it is `NEVER_USED`, or its preceding
`Release[i]=e` has actually completed. Acquire checks candidate slots with
`ID3D12Fence::GetCompletedValue`; a completed slot changes from
`RELEASE_QUEUED` to `AVAILABLE`. This check is an O(1) read of the exact
per-slot fence state, not a table scan outside this swapchain and not a KMT
handle open. For the selected slot:

1. validate its state, increment the nonwrapping target epoch, and reset that
   slot's D3D12 allocator/list and lower-Vulkan command pool only after the
   preceding Release completion (first use requires no reset dependency);
2. record the slot's acquire barrier command buffer and enqueue it on the
   private helper queue. It inserts the exact ICD-context Release wait when
   applicable, records `EXTERNAL -> canonical-family` ownership acquisition
   from `GENERAL` to `PRESENT_SRC_KHR`, and signals the binary semaphore and/or
   VkFence supplied by the application; and
3. mark the slot `ACQUIRED` and return its image index. Returning before the
   newly queued acquire barrier/app object completes remains legal; selection
   itself never occurs before the previous epoch's Release completed.

`vkAcquireNextImage2KHR` first requires `deviceMask=1`; the ordinary
`vkAcquireNextImageKHR` path has the same implicit singleton mask. The chosen
mask is stored with the acquisition so an explicit device-group Present can
be checked before its waits are consumed.

Timeout zero returns `VK_NOT_READY` when no slot is immediately selectable.
For a finite or infinite timeout, the layer chooses the oldest enqueued
`RELEASE_QUEUED` slot (the swapchain uses one D3D queue, so a later Release
cannot complete first), arms that slot's exact D3D12 fence event with
`SetEventOnCompletion`, and blocks for the caller's remaining timeout. It also
wakes on swapchain loss/destruction. On wake it revalidates state and the exact
value once; timeout returns `VK_TIMEOUT`, removal returns
`VK_ERROR_DEVICE_LOST`, and no loop polls or sleeps. If all slots are merely
application-held/never submitted, the same bounded state-change event path is
used until a Present, destruction, loss, or timeout makes progress possible.

#### Present

One intercepted `vkQueuePresentKHR` may contain several swapchains but only
one `pWaitSemaphores` array. Before consuming any wait, the layer validates
that the supplied queue is one of the application-visible `flags=0` handles
created from the original canonical-family queue record; the private helper,
a protected queue, a different-family queue, a foreign/lower queue, or an
unrequested index fails without consuming a semaphore or changing an image.
It then validates that every semaphore is binary, every swapchain is a live
layer-owned exact-profile object on this device, every image is acquired, and
every non-null `VkPresentInfoKHR::pNext` structure is the C45
`VkDeviceGroupPresentInfoKHR` singleton form. Absence has the same LOCAL/mask-1
meaning; any other structure, mode, count, or mask fails before submission.
An unknown/lower/mixed swapchain or unsupported extension structure is a layer
invariant failure and is never forwarded to lower WSI, because that would
consume shared waits twice. After this validation, the layer makes **one**
lower `vkQueueSubmit2` on the exact
lower form of the queue
passed to `vkQueuePresentKHR` (the canonical present family, externally
synchronized by the caller). That one submission:

1. waits each application binary semaphore in `pWaitSemaphores` exactly once;
2. records a `PRESENT_SRC_KHR -> GENERAL` and
   `canonical-family -> VK_QUEUE_FAMILY_EXTERNAL` release barrier in each
   presented slot's distinct release command buffer, and submits those
   per-slot buffers together; no command buffer belongs to two slots or is
   reset from another slot's Release result;
3. signals each presented image's distinct imported `Ready[i]=e` timeline
   value.

After that common release is enqueued, the layer schedules the following
chain independently for each `(swapchain,image)` in input order:

4. that swapchain's D3D12 queue GPU-waits `Ready[i]=e`;
5. the layer calls `IDXGISwapChain3::GetCurrentBackBufferIndex` and obtains the
   current real DXGI index `j`; it never infers `j` from `i`;
6. the layer records/executes the copy only through that slot's D3D12 command
   allocator/list. D3D12 interprets the external boundary as `COMMON`, transitions `S[i]` to
   `COPY_SOURCE` and `B[j]` to `COPY_DEST`, revalidates the immutable C37
   descriptions, executes one full-image `CopyResource`, then transitions
   `S[i]` back to `COMMON` and `B[j]` to `PRESENT`;
7. D3D12 signals `Release[i]=e`;
8. the layer calls ordinary DXGI `Present(1,0)` for `B[j]` and writes that
   swapchain's exact mapped Vulkan result to `pResults` when supplied.

The call returns the Vulkan aggregate result required for the per-swapchain
results. No binary wait is duplicated across swapchains. If a failure occurs
after the common lower release has transferred ownership and the layer cannot
produce the matching D3D Release/acquire path for every affected image, the
device is failed with `VK_ERROR_DEVICE_LOST`; it does not report an unaffected
state that no longer exists.

The slot becomes `RELEASE_QUEUED` after step 7. The next Acquire, not Present's
CPU return, waits/checks the exact Release completion and thereby gates both
reuse of `S[i]` and reset of its D3D12/Vulkan recording objects. The real DXGI
backbuffer is reused only when DXGI makes it current/available. A bounded pool
exists because there is exactly one allocator/list and two barrier command
buffers per swapchain slot; no list or command buffer is reset while pending.

Multiple swapchains have independent D3D queues or explicit queue-local
serialization; they share no image/fence values. Multiple application Present
queues in the canonical family remain externally synchronized by their Vulkan
callers; the layer submits release work on the exact application queue passed
to `vkQueuePresentKHR` and never uses the private helper there. The private
helper serves Acquire barriers only and has its one layer-owned mutex.

### 10.8 Display consumption and host-reader retirement

#### WDDM 3.2 Display-Core/MPO3 admission

Current local source records a hard Helios boundary: raising
`WddmSurface` above 2.1 makes build-28000 DWM enter the Display-Core/MPO3
surface, while the KMD registers no MPO3 table and its `DxgkDdiPresent`
explicitly rejects the `FlipWithMultiPlaneOverlay` union arm
(`kmd_render/src/ddi/wddm_surface.rs:1-33,55-64`,
`kmd_render/src/ddi/present_packet.rs:807-860`, `ROADMAP.md:4083-4092`).
Therefore the 3.2 uplift and HPS2 retirement include this display work; it is
not deferred behind successful native-fence tests.

The selected first-generation display profile is deliberately one primary,
not advertised multi-overlay hardware. Its KMD capabilities are the design
proposal `MaxPlanes=1`, `MaxRGBPlanes=1`, `MaxYUVPlanes=0`,
`OverlayCaps.Value=0`, `MaxStretchFactor=MaxShrinkFactor=1.0`; post-composition
caps likewise admit no scaling. The D3D11/DXGI UMD returns the same one-plane
truth instead of its current 16-plane, 16x stretch/shrink,
RGB/BILINEAR/SHARED/IMMEDIATE advertisement
(`umd/src/forward/present.rs:2125-2217`). Microsoft documents the fields and
that plane counts include DWM's primary, but does not promise that these
numeric values make every DWM version accept a Display-Core adapter. The cold
DWM gate below is therefore correctness admission, not an optional smoke test.

Promotion has an earlier UMD gate as well as a later KMD gate on **each** OS
display path. DWM calls the existing D3D11.1
`pfnCheckDirectFlipSupport` with the exact application and DWM swapchain
resource wrappers before Direct Flip and again after mode or DWM swapchain
changes (C43-C44). Helios initializes `*pSupported=FALSE` and sets it true only
when both wrappers are live on this same one-node adapter/LUID and both HWA2
descriptors prove: `PRIMARY|DISPLAYABLE|DIRECT_FLIP_COMPATIBLE`, BGRA8 UNORM,
equal nonzero extent, one plane, one mip/layer, sample count 1/quality 0, equal
implemented swizzle class, and none of `STEREO|PROTECTED|CROSS_ADAPTER`.
Concrete D3D11 primary IDs must be equal and non-sentinel. If either resource
is a C44 `D3D12_RUNTIME_PRIMARY`, its VidPn field must instead be the required
`D3DDDI_ID_UNINITIALIZED`; that sentinel means any source on this exact adapter
and is never replaced or compared as a concrete identity. Every other
origin/sentinel combination is rejected. `CheckDirectFlipFlags` must be zero
because the first generation does not support immediate flip. A missing field,
unknown flag, resource-generation mismatch, or unsupported pair leaves false;
the UMD never invokes its optional Escape callback to ask KMD.

This UMD call is only static resource-pair admission. The KMD implements one
nonpageable, bounded `validate_direct_scanout_binding` routine that consumes
only the exact OS-supplied allocation, VidPn source, current committed target
mode, operation flags, and actual plane attributes. The classic
`DxgkDdiSetVidPnSourceAddress` path invokes it directly on the structure's
`hAllocation` before retaining any candidate. `CheckMPO3` invokes it for each
proposed allocation/attribute tuple, and `SetMPO3` invokes it again on the
exact `ppContextData[].hAllocation` before retention. Thus MPO3 never stands
in for classic Direct/Independent Flip. Static incompatibility is rejected by
the UMD before promotion; because Microsoft says a later classic
`SharedPrimaryTransition` failure has no seamless composition fallback, a
KMD mismatch after admission is a loud invariant/device failure before latch,
not an advertised recovery arm.

`DxgkDdiCheckMultiPlaneOverlaySupport3` returns `STATUS_SUCCESS` with
`Supported=TRUE` only for one layer-0, same-source, same-adapter SDR RGB
primary whose exact live `hAllocation` has the full output extent, identity
rotation/flip, equal source/destination/clip rectangles, no alpha blend,
post-composition, HDR, stereo, scaling, or other advertised-off feature.
`PostCompositionCount` must be zero. Every other configuration returns
`STATUS_SUCCESS`, `Supported=FALSE`, and a zero/reserved-clean return record;
the OS remains free to use DWM composition. The KMD never identifies this
allocation from its geometry—the `hAllocation` in the check structure is the
identity.

`DxgkDdiSetVidPnSourceAddressWithMultiPlaneOverlay3` revalidates through the
same shared routine. For an enabled plane it requires `PlaneCount=1`, `LayerIndex=0`,
`ContextCount=1`, and one non-null `DXGK_PRIMARYCONTEXTDATA`; the exact scanout
identity is `ppPlanes[0]->ppContextData[0]->hAllocation`, paired with that
record's exact `hContext` and GPUVA. There is no `hAllocation` directly in
`DXGK_MULTIPLANE_OVERLAY_PLANE3`. The DDI retains the allocation/backing and a
preallocated candidate slot before returning success. It never blocks on a
host operation at interrupt level; only bounded validation/reference-taking
and nonblocking backend enqueue occur there. A genuine need for lower-IRQL
pre-work uses the documented `STATUS_RETRY`/`PrePresentNeeded` protocol, not a
sleep or poll. Disabled/zero-plane, source-visibility, mode, and power paths
are explicit unbinds and feed the same old-reader retirement state machine.

All special output behavior is off: no immediate-flip conversion,
`PostPresentNeeded`, Hsync completion, hardware flip queue, HDR, or
post-composition. The selected table registers
`DxgkDdiPostMultiPlaneOverlayPresent` as a bounded always-success diagnostic
callback, but the KMD never requests it;
if a future generation sets the matching notification flag, it must first add
the real operation. `PresentId` is logged for OS correlation only and never
releases a reader.

The normal KMD `DxgkDdiPresent` also implements, rather than reinterprets or
rejects, `DXGK_PRESENTFLAGS::FlipWithMultiPlaneOverlay`: it reads
`pPresentMultiPlaneOverlayInfo`, bounds `PlaneListCount`, validates every
`DXGK_PRESENTMULTIPLANEOVERLAYLIST::hDeviceSpecificAllocation`, and emits the
ordinary exact-allocation presentation packet. `pAllocationList` is never read
for that union arm. Actual display ownership starts only from the later
OS-supplied SetMPO3 plane binding, so this Present packet does not invent a
lease or equate its completion with scanout release.

Composed mode:

- DXGI shares the presented allocation with DWM.
- DWM opens it through the completed D3D12-to-D3D11 resource-open path.
- DWM's D3D11 composition read is a real WDDM batch with exact allocation list.
- normal WDDM resource scheduling orders the producer and consumer.

Direct/independent mode:

- DXGI/dxgkrnl reaches the ordinary flip/MPO/SetVidPnSourceAddress DDI only
  after the actual render-context dependency;
- classic SetVidPn and MPO3 independently run the shared exact-allocation,
  actual-source/current-mode/profile validator; KMD retains that exact backing
  before accepting a new plane binding;
- every ordinary replacement is a nonzero `SET_SCANOUT_BLOB` carrying
  `VIRTIO_GPU_FLAG_FENCE` and a unique nonzero fence id; only a successful reply
  echoing the exact flag/id is the latch-plus-prior-release boundary;
- KMD installs the new current and releases the prior backing at that boundary,
  while emitting logically distinct latch/release ETW events for the same fence;
- explicit unbind first fenced-replaces with a permanent KMD parking/black blob,
  then may `SET(0)` but retains parking until a later nonzero replacement/reset;
- a real current remains referenced until replacement; reuse is an OS/DXGI choice.

Transition/cancel:

- composition promotion/demotion uses ordinary DXGI state;
- a candidate canceled before latch releases only its candidate reference and
  never displaces the current plane;
- mode changes, source invisibility, power transitions, DWM restart, and teardown
  cancel candidates and run fenced parking replacement; uncertain completion
  retains the real backing, while transport reset is the terminal boundary (C31);
- the former `read_ledger`, Escape mapping, 65-slot page, event registration,
  10 ms signaler, DXVK scans, and snapshot fallback are deleted.

### 10.9 Failure policy

| Failure | Required result |
|---|---|
| Missing WDDM feature/version/package generation | adapter/device creation fails |
| Unknown batch version/checksum/resource/GPUVA | device removal; batch not submitted |
| Host submit failure | fail the WDDM submission/device |
| Host completion timeout | TDR/device removal; never advance progress |
| Submission-fence regression/out-of-order completion | device removal |
| Core DDI <0116, native feature/cap absent, `MONITORED` branch, or context mismatch | adapter/device/fence creation fails |
| WDDM-3.2 version/caps selected without the complete one-plane MPO3/Display-Core table, or cold DWM admission fails | adapter/package is rejected before HPS2 retirement; do not fall back to a 2.1 binary |
| D3D12-to-D3D11 OpenResource validation failure | resource/device open fails |
| Vulkan 1.3, timeline/synchronization2, Win32 external extension, external memory/fence capability, LUID, UUID, C37 description, format/color-space, usage, mode, or state mismatch | surface unsupported or Vulkan device/swapchain creation fails before handle export |
| Unknown/mixed WSI swapchain, non-binary Present wait, unsupported Present `pNext`, non-LOCAL device-group mode, or mask other than the last acquired singleton mask 1 | reject before consuming any wait; never forward to lower WSI |
| Invalid swapchain alias create/bind, external alias capability, memory type, or dedicated import | fail image create/bind without altering the swapchain image; never substitute ordinary memory or a fake lower swapchain |
| HWND client extent `(0,0)` or differs from an existing swapchain | creation fails while zero; existing Acquire/Present returns `VK_ERROR_OUT_OF_DATE_KHR` and recreates |
| HWND destroyed | `VK_ERROR_SURFACE_LOST_KHR`; retire swapchain |
| DXGI device removed/reset | `VK_ERROR_DEVICE_LOST`; destroy/recreate device-owned objects |
| DWM restart | ordinary DXGI status/recreation; no custom state to recover |
| process crash | OS closes devices/queues/handles; KMD cancels host batches/flips |
| flip candidate cancellation | release candidate once; leave current plane unchanged unless source is also disabled |
| WSI helper-queue failure | mark swapchain lost; never signal success speculatively |
| HNR2 fragment/order/manifest/input-operand/output-WDDM-patch/reply validation failure | reject in Render before output DMA, poison/cancel that context/device generation, emit no host command; Patch has no recoverable error arm, and an impossible post-Patch HPM mismatch fails Submit/removes the device; no alternate transport |
| HPM1 feature/startup/paging-DMA/ADL/placement/epoch/copy/fill/discard acknowledgement failure | fail adapter initialization or the exact paging submission/device; never advance its paging SubmissionFenceId, expose divergent bytes, or repair through a resource-ID lookup |
| HLM1 descriptor/BAR/linear-offset/Lock2/WC/coherency admission failure, or any CPU-host-aperture callback is observed | fail cold adapter/native-Vulkan qualification and do not activate the package; never restore HAP1, whole-blob, or Escape mapping |
| C57 carrier admission fails — query/open status, `NumAllocations` mismatch, any zero `hAllocation`, per-allocation private data outside or larger than the queried `pTotalPrivateDriverDataBuffer`, or a nonzero `GpuVirtualAddress` inconsistent with a binding that uses it | fail the import and do not advertise or expose the `D3D12_RESOURCE_BIT` Vulkan import; never fabricate identity from `hResource` alone, a legacy global share, an OPAQUE/named object, D3D/DXGI recursion, or a raw resource ID |
| HTS1 INIT fails, a second instance is requested in one host context, session/ring capacity is exhausted, or raw/runtime devices do not have the identical KMD process/adapter/package generation | fail translator device creation before any outer context/resource is exposed; never merge instances, fall back to PID, or create a host context per outer queue |
| HQA1 missing/malformed/stale, zero/reused context generation, capability/session/package/endpoint mismatch, late Render-time attach, or endpoint host-dispatch FIFO exhaustion | fail outer context/queue creation or remove that device; never scan ProcessContext during submit, wait for an absent cross-context sequence, or attach through reserved Render PDD |
| HOS1 size/field mismatch, HOB1 header/table/checksum/use mismatch, stale ProcessContext/root/PTE/TLB generation, unmapped or cross-process HOB1/operand GPUVA, or a HOB1 primary-write use inconsistent with the same command-list resource/use ledger | reject before host renderer execution and remove/fail the device; never claim that KMD can inspect runtime-owned `WrittenPrimaries`, CPU-dereference the submitted GPUVA at DISPATCH, execute HOS1 as work, repair through a host/resource lookup, or report SubmissionFenceId complete |
| HOB1 pool extent reused/unmapped/evicted by the UMD before its exact HQC1 value completes, pool metadata mismatch, extent-count/byte-cap exhaustion with no retiring owner, or the post-submit HQC1 signal fails | remove/fail the device and retain old-generation extents through teardown; never overwrite pending bytes, wait while holding the pool lock, spin on a completed value, allocate an unbounded spill extent, or treat reset as retirement |
| C60 opcode classification violation, raw control names anything except its exact session-owned HVM1 reply/feedback range, or an outer-allocation-backed deferred operation reaches host before an exact outer allocation use | reject/poison the session and submit no host command; never borrow an allocation through the process/session capability |
| HTS1 reply/HVR1 slot, batch, snapshot, offset, size, status, generation, or publication mismatch; a logical snapshot exceeds 64 MiB without a generated legal partition/partial-result rule; all four slots/snapshots are busy; or reset/cancel races a continuation | reject the malformed reply; for ordinary pressure drop the slot/snapshot lock and event-wait only for the oldest exact C51 owner, then retry. On schema/result mismatch fail device admission or lose the device; never recompute the original operation, grow a pool, exceed four snapshots/256 MiB, spill an allocation, poll, overwrite, or expose a prefix as success |
| A legacy batch exceeds 4096 unique reachable WDDM allocations or 8192 direct typed operands, or the current returned allocation/output-patch list is too small | split only before a complete generated operation; use the documented next-list resize path and adopt its returned pointers/sizes. The generated exposed-cap proof forbids an indivisible legal operation from exceeding the bound; never truncate, omit a transitive descriptor allocation, substitute a raw object ID, or split an indivisible operation |
| HQC1 creation/signal/event wait fails or reset occurs while a synchronous result waits | return device lost/failure; never use ring-zero completion or a later queue value as success; ordinary render/Present never enters this CPU-wait path |
| Any HNR2 context lacks its own C51 fence, a GPU opcode reaches ring 0, or idle/teardown substitutes a control value for all nonzero queues | fail device creation/operation; control may prove only decode/reply and no context may borrow another fence |
| finite direct SetReply/reply-fence admission probe fails | reject the atomic generation before deployment; never create `vn_ring` |
| epoch overflow | refuse further presents and recreate; no wrap |

## 11. Cross-UMD/DWM/DX12 FLIP topology

### 11.1 Components and object graph

```text
DX12 application process
  Mesa vn_instance -> HTS1 translation session
    raw HVC1 control context -> private host context/ring 0
  ID3D12CommandQueue Q
    -> Helios DX12 UMD
      -> vkd3d-proton
        -> Helios Mesa direct dispatch, record-only queue
      -> WDDM virtual context C(Q)
        HQA1 -> direct HTS1 physical-endpoint reference + private HQC1
        -> dxgkrnl -> Helios KMD -> Venus/QEMU host execution
      -> Core-0116 native fence objects F
         HFENCE/HRTFENCE <-> local hSyncObject/mappings
  DXGI swapchain allocation B
    -> DXGI Present of exact hAllocation(B) after C(Q)'s real write
      -> composed: DWM process opens B
         D3D11 UMD -> DXVK -> Mesa record-only -> DWM WDDM context
      -> direct/independent: stock flip/MPO DDI -> exact QEMU plane lease

Native Vulkan process
  each Mesa vn_instance -> one HTS1/KMD/host Venus context/object namespace
    raw KMT device bound to exact KMD ProcessContext
    control KMT context -> host ring 0 (CPU/decode completion only)
    app/private queue KMT contexts -> unique nonzero host ring each
    HNR2 COMMIT -> exact WDDM allocation list -> one finite SUBMIT_3D
    HLM1 id2 -> direct linear CPU-visible BAR memory
    HPM1 paging DMA -> exact current local/aperture placement <-> renderer view
    HVM1 device-local allocation -> HPM1 authoritative bytes; no CPU VA
    HVM1 host-visible allocation -> Lock2 direct BAR/system VA <-> same HPM1 bytes
  VK_LAYER_HELIOS_present
    shared S[i] + Ready[i] + Release[i]
    D3D12 context C(layer) -> query current j -> copy S[i] into B[j] -> Present
```

#### Translation-session creation and outer attach

```mermaid
sequenceDiagram
    participant T as DXVK/vkd3d + direct Mesa
    participant RK as raw KMT device/HVC1
    participant K as KMD ProcessContext
    participant H as Venus host context
    participant U as outer D3D UMD
    participant C as runtime D3D context

    T->>RK: CreateDevice + CreateContext(HVC1 control)
    RK->>K: provisional session on exact hKmdProcess
    T->>RK: finite INIT
    RK->>H: create one host context + one VkInstance
    H-->>RK: bounded reply
    RK-->>T: C51 event; {session generation, capability, endpoints}
    T-->>U: direct private session/physical-endpoint descriptor
    U->>C: CreateContext(HQA1)
    C->>K: validate same hKmdProcess/adapter/generation/capability once
    K-->>C: retain direct session/endpoint ref
    U->>C: create private HQC1 progress fence
    Note over C,K: later submit uses direct ref; no ProcessContext lookup
```

### 11.2 Composed D3D12 FLIP sequence

```mermaid
sequenceDiagram
    participant App as DX12 app
    participant U12 as DX12 UMD
    participant VKD as vkd3d + record-only Mesa
    participant C as WDDM context
    participant KMD as dxgkrnl/KMD
    participant Host as Venus/QEMU
    participant DXGI as DXGI Present
    participant DWM as DWM D3D11 UMD/DXVK
    participant ICD2 as record-only Mesa (DWM)

    App->>U12: ExecuteCommandLists(render B)
    U12->>VKD: translate and seal actual batch
    VKD-->>U12: bytes + exact GPUVA/resource uses
    U12->>C: SubmitCommandCb(actual batch; runtime supplies WrittenPrimaries=B)
    C->>KMD: schedule SubmissionFenceId p
    KMD->>Host: execute Venus batch
    App->>DXGI: Present(B)
    U12-->>DXGI: exact hAllocation(B), context association
    DXGI->>C: queue normal front/backbuffer dependency on p
    Host-->>KMD: exact completion
    KMD-->>C: DMA_COMPLETED(p)
    C-->>DXGI: producer dependency satisfied
    DXGI->>DWM: open/consume exact B
    DWM->>ICD2: translate composition read
    ICD2-->>DWM: sealed actual batch
    DWM->>KMD: RenderCb(allocation B read)
    KMD->>Host: execute composition
    Host-->>KMD: complete
    KMD-->>DWM: DMA completed
```

### 11.3 Normal native Vulkan execution and mapping sequence

```mermaid
sequenceDiagram
    participant App as Vulkan app/Mesa
    participant KMT as KMT legacy context
    participant VidMm as dxgkrnl/VidMm
    participant KMD as Helios KMD
    participant Q as QEMU/virglrenderer
    participant CPU as Lock2 CPU mapping

    KMD->>Q: StartDevice negotiates HPM1 and complete linear BAR range
    KMD-->>VidMm: expose aperture id1 + CpuVisible HLM1 id2 only when READY
    App->>KMT: CreateDevice + HVC1 control + finite HTS1 INIT
    KMT->>Q: create one host context/VkInstance; assign ring 0
    Q-->>App: C51 event-backed session reply
    App->>VidMm: CreateAllocation2(HVM1 device-local or host-visible, pSystemMem=NULL)
    VidMm->>KMD: CreateAllocation(exact object)
    KMD->>Q: create same-sized KMD-owned renderer view
    VidMm->>KMD: BuildPagingBuffer TRANSFER2(system ADL -> local pages)
    KMD-->>VidMm: actual HPM1 paging DMA + next epoch
    VidMm->>KMD: mandatory side-effect-free paging Patch; Submit
    KMD->>Q: enqueue HPM1 copy + exact BAR-offset renderer bind
    Q-->>KMD: paging transaction terminal
    KMD-->>VidMm: DMA_COMPLETED paging SubmissionFenceId
    App->>VidMm: Lock2(exact allocation)
    VidMm-->>CPU: direct CpuTranslatedAddress+offset VA (or byte-identical system VA)
    App->>KMT: Render HNR2 BEGIN/DATA (no allocations)
    KMT->>KMD: protected copy; stage; scheduler no-op
    App->>KMT: Render HNR2 COMMIT (complete allocation list)
    KMT->>KMD: Render emits output patches/capability slots
    VidMm->>KMD: Patch exact allocation SegmentId/PhysicalAddress/HPM epoch
    KMD->>Q: validate HPM1 capability; finite SUBMIT_3D(ctx, nonzero ring, host fence)
    Q->>CPU: write exact reply range when requested
    Q-->>KMD: matching context/ring fence terminal
    KMD-->>VidMm: DMA_COMPLETED exact ordered SubmissionFenceId
    VidMm-->>App: C51 event/progress signal when CPU result requested
    App->>CPU: validate token/opcode/size; decode reply
```

The first branch is adapter/segment lifetime. HVM1 Lock2/Unlock2 is an
allocation/map-lifetime operation and does not repeat for each Render or
Present. A non-reply batch omits the CPU/result steps. Placement change is an
actual paging DMA whose completion drains/rebinds QEMU state before VidMm or a
later Patch can expose the new epoch; no CPU-host-aperture map exists.

### 11.4 Native Vulkan WSI sequence

```mermaid
sequenceDiagram
    participant App as Vulkan app
    participant Layer as Helios WSI layer
    participant VICD as normal Helios ICD
    participant DQ as layer D3D12 context
    participant DXGI as DXGI
    participant DWM as DWM/scanout

    App->>Layer: AcquireNextImage(i, semaphore/fence)
    Layer->>VICD: wait Release[i]=old; EXTERNAL->Vulkan barrier; signal acquire
    VICD-->>App: acquire object signals
    App->>VICD: render S[i]
    App->>Layer: QueuePresent(i, app semaphores)
    Layer->>VICD: wait app sems; Vulkan->EXTERNAL barrier; signal Ready[i]=e
    Layer->>DQ: Wait Ready[i]=e
    Layer->>DXGI: GetCurrentBackBufferIndex() = j
    Layer->>DQ: barriers + CopyResource exact BGRA8 S[i] to B[j]
    Layer->>DQ: Signal Release[i]=e
    Layer->>DXGI: Present(B[j], 1, 0, exact DQ)
    DXGI->>DWM: normal flip/composition handoff
```

### 11.5 Direct/independent transition

```mermaid
sequenceDiagram
    participant DWM
    participant DXGI
    participant U as D3D11 UMD
    participant C as render context
    participant OS as dxgkrnl flip/MPO path
    participant KMD
    participant Host as QEMU scanout

    DWM->>DXGI: consider Direct/Independent Flip
    DXGI->>U: D3D11.1 CheckDirectFlipSupport(app,DWM pair)
    U-->>DXGI: true only for exact C43/C44 pair; D3D12 sentinel means this LUID, not a guessed source
    DXGI->>C: wait exact primary-bearing submission
    DXGI->>OS: Present exact allocation B
    OS->>KMD: classic SetVidPn(B,actual source) or MPO3(B,actual plane)
    KMD->>KMD: shared nonpageable exact-allocation/source/mode validator
    KMD->>Host: retain validated B and queue exact plane binding after render dependency
    Host-->>KMD: B latched; old reader A later released
    KMD->>Host: release A only after replacement + backend release
    Note over KMD,Host: retain current B until replacement/unbind + backend release
    Note over OS,KMD: promotion/demotion changes consumer, not producer identity
```

### 11.6 Required ten answers for composed DX12 FLIP

1. **Who creates the allocation?** DXGI/D3D12 runtime calls the DX12 UMD's
   resource/allocation DDIs; KMD creates the authoritative WDDM allocation and
   host backing.
2. **How does DWM obtain it?** Stock dxgkrnl/DXGI resource sharing and the
   receiving D3D11 UMD's OpenResource path with immutable private data.
3. **Who creates synchronization?** The runtime creates the WDDM context and
   Core-0116 native D3D12 fence objects. Present ordering itself is the normal
   primary-bearing context dependency; no Helios Present fence is created.
4. **How does it cross process/UMD boundaries?** It does not cross as a custom
   handle. The exact WDDM allocation plus runtime queue/Present association
   crosses through dxgkrnl.
5. **Which queue signals?** KMD completes C(Q)'s exact submission only after
   the actual host batch. API fence signals are exact-context runtime callback
   operations on the native `hSyncObject`.
6. **Which queue waits?** API waits are inserted on C(Q); Present/DWM resource
   use is scheduled after the primary-bearing producer submission completes.
7. **How are ownership/state transitions represented?** D3D barriers in the
   actual batch, WDDM WrittenPrimaries/resource scheduling, and DXGI Present.
8. **How is next reuse safe?** DXGI/dxgkrnl owns backbuffer availability. In
   direct scanout KMD/QEMU additionally retains the exact allocation currently
   bound to each plane until replacement/unbind plus backend release; no
   Present completion is interpreted as reader release.
9. **What changes in direct/independent flip?** DWM's composition batch is
   absent; stock OS flip/MPO state selects scanout and exact per-plane
   KMD/QEMU state owns the current display reference.
10. **Restart/reset/death?** Kernel object teardown cancels queues/flips; DXGI
    returns loss/reset; no PID slot or named object survives.

## 12. Exact object and operation contract

| Object | Create / owner | Transfer/open | Per-use signal/wait | Destruction |
|---|---|---|---|---|
| D3D11 context/DMA | runtime callback; UMD/KMD context | none | actual Render submission; DMA completion after host | runtime destroys context; KMD cancels batches |
| HTS1 translation session | one raw KMT HVC1 control context per Mesa `vn_instance`; KMD ProcessContext contains a bounded live-session list; before INIT the raw device creates one 64-MiB role-1 HVM1 pool with four 16-MiB slots; INIT creates one host context/VkInstance and returns generation/capability/endpoint bounds | HQA1 create-context PDD validates capability once against identical KMD process/adapter/package and installs a direct outer-context endpoint ref; no resource transfer or submit lookup | pure control uses finite HNR2/ring zero/C51 and only one exact checked-out reply-pool slot; outer-allocation-backed work uses the exact outer batch; GPU-dependent synchronous result joins exact HQC1/nonzero endpoint then fetches control reply; larger results use an immutable generation-checked snapshot and exact-next-offset continuation | `Active -> Draining -> Dead`; invalidate capability/wake device-lost first, cancel at most four snapshots/256 MiB and drain raw/outer contexts, endpoint jobs, slot owners, C51/HQC1 and host refs, unlock/destroy the pool, then destroy host context/session; process teardown cancels all |
| D3D12 virtual context/submission | runtime `pfnCreateContextVirtualCb`; one per API queue, HQA1-attached to an HTS1 physical endpoint; UMD writes complete HOB1 at the resident command GPUVA and fixed HOS1 in copied private data | runtime context plus direct KMD session/endpoint and exact ProcessContext/root/PTE generation only; HOS1 has no command/resource payload | actual `pfnSubmitCommandCb`; same-context scheduler order, C62 endpoint arrival-order serialization, C64 HPM1 GPUVA walk/validation, and DMA completion after exact host work | runtime destroys context only after HOB1 page-table/host refs drain; KMD cancels endpoint batches and drops session/address-space refs |
| D3D12 HOB1 GPUVA pool | one HOC1 `pfnAllocateCb{hResource=NULL,pSystemMem=NULL}` allocation per D3D12 device; UMD lifetime-Lock2 maps it, reserves/maps GPUVA, makes it resident, and divides 64 MiB into at most 256 live 64-KiB-aligned extents | no transfer; HOC1 is nonshared and each sealed extent is tagged with exact queue/context generation, batch ID, and post-submit HQC1 value | one completion check when allocating; one exact event wait only under pool pressure; no CPU wait in the ordinary path | extent remains immutable/mapped and is not UMD-evicted until its HQC1 value completes; after all extents retire, free GPUVA, Unlock2, and Deallocate; reset poisons all extents and teardown waits for contexts/waiters |
| D3D12 native fence | runtime + Core 0116; UMD stores `HFENCE`, `HRTFENCE`, local `hSyncObject`, mappings | native create or NT-handle open returns process-local identity/mappings | exact-context FromGpu wait/signal; CPU operations through runtime/KMT | native close/destroy after final process reference; recreate after reset |
| D3D allocation/backing | runtime create/open; KMD authoritative object | exact KM handle plus KMD create-time immutable private data delivered by stock OpenResource; ordinary open is read-only | allocation-list index or mapped GPUVA; Present exact allocation | close/destroy after all context/display/host-backing refs |
| Native Vulkan legacy KMT context/batch | normal ICD creates one HTS1 CPU/decode-only HVC1 control plus one nonvirtual HVC1 `D3DKMTCreateContext` per real lower queue; that session owns one Venus namespace, CPU ring 0, and one nonzero GPU endpoint per queue | HNR2 travels only in that context's Dxgkrnl command buffer; only COMMIT lists every exact WDDM allocation; Render emits one output patch location/capability slot per use | fragment no-ops stage bytes; COMMIT performs bounded Render validation/copy, repeatable Patch of exact physical capabilities, then one physical Submit and fenced direct SUBMIT_3D resolved through HPM1; control retires after restricted decode/reply, queue only after its nonzero GPU fence | queue/device idle and teardown join all affected nonzero C51 milestones before final control/session destruction; loss cancels generation/assembler; no virtual-submit, shared ring, Escape, or retry path |
| Outer translated progress HQC1 | D3D11/12 UMD creates one private unshared monitored fence for each HQA1-attached runtime context through its callback table | none; never exposed as API fence or session capability | after every actual outer submit, exact-context FromGpu signal of increasing value; values retire D3D12 HOB1 extents and also provide C60 synchronous query/idle/destruction milestones; FromCpu waits use one event only when needed | cancel armed wait on loss; destroy after HOB extents, context batches, and waiters drain, then drop session ref |
| HLM1/HPM1 local-memory service | KMD/QEMU negotiate the complete linear BAR and paging engine before QuerySegment4 exposes `[aperture id1,CpuVisible memory id2]`; each exact KMD allocation owns one renderer view, never an independent byte copy | BuildPagingBuffer emits actual DMA; mandatory paging Patch is side-effect-free/no-size-change; Submit only enqueues the exact allocation generation, segment/ADL runs, range, and next epoch | device/QEMU copies/fills/maps/binds, retains old placement and host refs, then publishes epoch and completes the paging SubmissionFenceId; HNR accepts only a current patched placement | reset/stop cancel transactions, drain address-space/host refs, revoke bindings, invalidate epochs, then destroy HPM/BAR state; no HAP, UMD ID, lookup scan, or CPU-synchronous paging mutation |
| Native Vulkan HVM1 storage | every ordinary non-import `VkDeviceMemory` creates one unshared allocation with `pSystemMem=NULL`, physical access, an exact device-local or host-visible role, and one KMD renderer view bound by HPM1 | device-local role has no CPU map; host-visible role uses `D3DKMTLock2` for the direct HLM1 BAR VA or VidMm's byte-identical system VA; no custom handle/ID/map callback | bound image/buffer records carry the exact allocation/range into HNR2; HPM1 paging preserves bytes; C51 orders host-visible CPU ownership and finite replies | unlock host-visible maps, retire HPM/host/GPU refs, destroy WDDM allocation/renderer view; no user MDL, name, file, HAP, or raw ID |
| Native Vulkan progress fence | ICD creates one unshared `D3DDDI_MONITORED_FENCE` for every HNR2-executing context, including control and every real lower queue | none; local KMT object/mappings stay in that ICD device | optional same-context FromGpu signal after the relevant Render; control value means only decode/reply completion, nonzero queue value means that queue's GPU completion; FromCPU event/block wait only when requested | join all required queue values for device idle/teardown, then drain control; destroy after armed waits/context work; reset/removal is failure and recreates it |
| Current display-plane binding | KMD; one exact object per active VidPn source/MPO plane | exact allocation from OS flip/MPO/SetVidPnSourceAddress or MPO3 `ppContextData[].hAllocation`, plus held KMD backing | new binding retained before acceptance; exact fenced nonzero replacement completion latches new and releases prior | explicit unbind first replaces with permanent parking, which survives optional `SET(0)` until later replacement/reset |
| WSI shared texture `S[i]` | layer D3D12 device; DEFAULT committed BGRA8 resource, SHARED heap only, ALLOW_RENDER_TARGET, initial COMMON | one retained `CreateSharedHandle(...,NULL,GENERIC_ALL,NULL)`; the lower ICD obtains the exact local allocation through C57's query/zeroed-array/open carrier on its own KMT device, so canonical and C45 alias Vulkan imports are available | Vulkan external release; D3D copy; external acquire | canonical image/memory live with `S[i]`; after every alias and GPU ref drains, `D3DKMTDestroyAllocation2{hResource,phAllocationList=NULL,AllocationCount=0,Flags=0}` closes the whole import, then `H[i]` closes |
| WSI alias image | app `vkCreateImage` with exact non-null still-valid layer swapchain; layer creates a real lower external VkImage | on bind, exact `imageIndex` selects `S[i]`; query `H[i]` memory types, make a distinct dedicated `D3D12_RESOURCE_BIT` VkDeviceMemory, bind at zero; null-swapchain forms instead forward as ordinary image/bind | identical-image alias layout/write semantics plus the same explicit swapchain/external ownership rules; Present still identifies original swapchain/index | old-swapchain retirement preserves use; parent destruction drains/reacquires external payload and removes only WSI/canonical handles; alias remains normal Vulkan image until its GPU refs and `vkDestroyImage` retire |
| WSI per-slot recording objects | layer; one D3D12 allocator/list and one lower-Vulkan resettable pool with acquire/release barrier buffers per `S[i]` | none | record/execute only for that slot/epoch | reset/re-record only after exact `Release[i]=e` completes; destroy after slot work cancels/retires |
| `Ready[i]` | layer `CreateFence(0,D3D12_FENCE_FLAG_SHARED)`; Vulkan is sole signaler | unnamed D3D12-fence NT handle imported permanently into a timeline semaphore initialized to zero, then transient handle closed | Vulkan signal e; D3D Queue Wait e | after all e complete/cancel and queue quiescence |
| `Release[i]` | layer `CreateFence(0,D3D12_FENCE_FLAG_SHARED)`; D3D is sole signaler | same exact permanent timeline import and lifetime process | D3D Queue Signal e; Vulkan wait e | same |
| DXGI backbuffer set `B[0..m)` | DXGI swapchain | OS/DWM normal sharing; current `j` queried for each Present | D3D copy to current `B[j]`, then Present | release all refs before ResizeBuffers/destroy |

### 12.1 Core-0116 native-fence contract

The 64-byte `D3DDDI_NATIVE_FENCE_PDD_SIZE` payload is object-associated KMD
private data, not a transport registry. The implementation uses this exact
little-endian, pointer-free `HeliosNativeFencePddV1` layout:

| Offset | Size | Field | Rule |
|---:|---:|---|---|
| 0 | 4 | magic | `0x31464e48` (`HNF1`) |
| 4 | 2 | ABI version | `1` |
| 6 | 2 | structure size | `64` |
| 8 | 8 | package generation | exact atomic-package generation |
| 16 | 8 | object generation | zero from UMD on create; nonzero KMD-assigned diagnostic/stale-validation generation on return |
| 24 | 4 | native type | documented `D3DDDI_NATIVEFENCE_TYPE` value |
| 28 | 4 | flags | bit 0 shared; cross-adapter and all other bits zero in this generation |
| 32 | 8 | adapter LUID | KMD writes the exact creating adapter LUID |
| 40 | 24 | reserved | zero and validated |

Neither `objectGeneration` nor any PDD field is independently usable as an
object key. KMD identity is exclusively the OS-delivered
`hGlobalNativeFence`/`hLocalNativeFence`; UMD identity is its live `HFENCE`
object plus the callback-returned local `hSyncObject`. No pointer, NT handle,
PID, GPUVA, host token, or allocation ID is serialized in PDD.

Creation/open/use/retirement is exact:

1. **Create.** Core 0116 calls UMD `pfnCreateFence_0116` with `FenceType=NATIVE`,
   one `HRTFENCE`, and `pNativeFenceArgs`. UMD initializes HNF1 and calls
   `pfnCreateNativeFenceCb(hRTDevice,hRTFence,pArgs)`. Dxgkrnl creates the
   kernel object and calls KMD `DxgkDdiCreateNativeFence`; KMD returns its
   global driver handle and validated PDD. The callback returns the local KMT
   `hSyncObject` and `D3DDDI_NATIVEFENCEMAPPING`; UMD stores them in that exact
   `HFENCE` private object.
2. **Share/open.** D3D12 `CreateSharedHandle` carries the kernel object under
   the caller's documented security descriptor. In another process/device,
   Core 0116 calls `pfnCreateFence_0116` with `OPENED_NATIVE` and
   `pNativeFenceOpenArgs`; UMD calls `pfnOpenNativeFenceCb`. Dxgkrnl locates the
   global object, creates process-local CPU/GPU mappings and local KMT handle,
   then calls KMD `DxgkDdiOpenNativeFence` with both exact driver handles. UMD
   validates HNF1/package/LUID/type and stores only the returned local state.
3. **GPU wait.** `pfnWaitForFence(Q,F,v)` sets Q's one exact physical-node bit,
   submits any earlier sealed work, and invokes
   `pfnWaitForSynchronizationObjectFromGpuCb` with
   `{hContext=Q.hContext,ObjectCount=1,ObjectHandleArray=&F.hSyncObject,
   MonitoredFenceValueArray=&v}` before any later SubmitCommand callback.
4. **GPU signal.** `pfnSignalFence(Q,F,v)` sets the same bit, first submits all
   earlier sealed work, then invokes
   `pfnSignalSynchronizationObjectFromGpuCb` with
   `{hContext=Q.hContext,ObjectCount=1,ObjectHandleArray=&F.hSyncObject,
   MonitoredFenceValueArray=&v}`. No `SignalAtSubmission`, broadcast context,
   direct GPUVA write, native-log entry, or lower Vulkan signal is used.
5. **CPU wait/signal.** The D3D12 runtime uses the native KMT CPU operations
   and event contract. UMD never polls `CurrentValueCpuVa`; diagnostics may
   sample it only off the correctness path.
6. **Close/destroy.** Queue/context references to `HFENCE` state drain before
   UMD private destruction. Dxgkrnl calls KMD close for each local object and
   destroy after the final global reference. KMD frees driver objects/mappings
   exactly once. Reset/removal invalidates every local mapping and generation;
   old values and `UINT64_MAX` are failure, never success.
7. **Multiple adapters/nodes.** Each Helios logical adapter exposes exactly one
   physical node and owns independent contexts, local handles, mappings, and
   values. `PhysicalAdapterMask` is that queue's one exact node bit. Multiple
   adapters and devices scale independently, but this generation rejects LDA,
   `SHARED_CROSS_ADAPTER`, cross-adapter resources, and any open that would
   convert a peer to a monitored fence. This is a declared capability boundary,
   not an identity or broadcast inference.

### 12.2 Native-WSI ICD-side fence contract

The normal Vulkan ICD owns a real KMT device/context, so it uses KMT handles
directly rather than a D3D runtime `HANDLE`. For each Ready or Release NT
handle it zero-initializes one
`D3DKMT_OPENNATIVEFENCEFROMNTHANDLE` and makes exactly one lifetime open with
[`D3DKMTOpenNativeFenceFromNtHandle`](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmthk/nf-d3dkmthk-d3dkmtopennativefencefromnthandle):

- `hNtHandle` is the exact unnamed handle supplied by the synchronous Vulkan
  import and `hDevice` is the importing ICD device's live KMT handle;
- `EngineAffinity=1u << 0`, matching the only physical adapter/node exposed by
  this device. Zero, multiple bits, overflow, or a bit outside the queried
  device topology is rejected before the call;
- `Flags` reproduces the creator's selected `Shared=1` and
  `NtSecuritySharing=1`; `CrossAdapter`, `TopOfPipeline`, `NoSignal`, `NoWait`,
  `NoSignalMaxValueOnTdr`, `NoGPUAccess`, `SignalByKmd`,
  `UnwaitCpuWaitersOnlyOnDestroy`, and every reserved/unused bit are zero;
- input `PrivateDriverData[64]` is HNF1 with this package generation, object
  generation zero, DEFAULT type, shared-only flags, exact importing adapter
  LUID, and zero reserved bytes; `Reserved[32]` is zero; and
- success is accepted only if `hSyncObject` is nonzero, all reserved bytes in
  `NativeFenceMapping` remain zero, the required mappings are nonzero/aligned,
  and returned HNF1 has the same package/type/flags/LUID plus a nonzero object
  generation matching the KMD's global fence object. Any mismatch closes the
  partial local object and fails semaphore import; it is never retried as a
  monitored or cross-adapter fence.

The ICD retains the returned process-local `hSyncObject`, native mapping, and
object generation in the imported semaphore object. The mapping is never used
for polling or identity. Queue use is:

- before the EXTERNAL-to-Vulkan acquire barrier, insert Release wait `e` with
  [`D3DKMTWaitForSynchronizationObjectFromGpu`](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmthk/nf-d3dkmthk-d3dkmtwaitforsynchronizationobjectfromgpu),
  `hContext=the submitting ICD context`, one local native handle, and one
  monitored value;
- after the Vulkan-to-EXTERNAL release barrier, insert Ready signal `e` with
  [`D3DKMTSignalSynchronizationObjectFromGpu`](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmthk/nf-d3dkmthk-d3dkmtsignalsynchronizationobjectfromgpu),
  `hContext=the submitting ICD context`, one local native handle, and
  `MonitoredFenceValueArray[0]=e`. Microsoft explicitly uses this non-2
  operation for the native DEFAULT fence's traditional-kernel-queue signal
  scenario; `FromGpu2` is not part of this selected contract;
- close the local sync object only after the imported semaphore and all queue
  references retire. Device removal invalidates the mapping/generation and
  loses the swapchain; no stale mapping is sampled or reopened.

[Microsoft open structure](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmthk/ns-d3dkmthk-d3dkmt_opennativefencefromnthandle),
released WDK `icd/win-build/wdk-include/d3dkmthk.h:1719-1728` and
`icd/win-build/wdk-include/d3dukmdt.h:1756-1809,1905-1930`.

Ready and Release are separate because each has exactly one writer. Each
image's epoch strictly increases and never wraps. No fence is shared between
images, queues, devices, or adapters, and no multiwriter total order is needed.

### 12.3 ETW and OS-diagnostic contract

The task-owned kernel provider GUID is
`{6D9A1A95-2B6A-4DEF-BCF7-847B6F158B0E}`. Driver initialization at
`PASSIVE_LEVEL` calls `EtwRegister` once and retains the returned `REGHANDLE`.
`DxgkDdiControlEtwLogging` changes only an atomic enabled bit and maximum
logging level; its currently undefined `Flags` must be zero and is not treated
as a keyword mask. It does not register the provider. An event is constructed
only when both that OS gate and `EtwProviderEnabled(REGHANDLE,level,keyword)`
accept it. Adapter stop first disables new event production,
waits for the bounded nonpaged event-site rundown, then driver unload calls
`EtwUnregister` exactly once. A failed registration disables ETW and records
that fact in the next OS-requested diagnostic snapshot; it never disables or
changes graphics ordering.

Every event has ETW descriptor version 1 and one 72-byte, align-8,
pointer-free `HeliosGraphicsEtwPayloadV1` data descriptor:

| Offset | Size | Field | Rule |
|---:|---:|---|---|
| 0 | 8 | package generation | exact atomic-package generation |
| 8 | 8 | adapter generation | diagnostic generation, never an object lookup key |
| 16 | 8 | object/allocation generation | zero when not applicable; never a handle/token |
| 24 | 8 | context/plane generation | zero when not applicable; never a handle/token |
| 32 | 8 | sequence/value | submission fence, native value, or binding sequence according to event ID |
| 40 | 4 | NTSTATUS/result | exact result, zero for an event without one |
| 44 | 4 | flags | event-ID-specific, with reserved bits zero |
| 48 | 4 | VidPn source | exact OS value or `D3DDDI_ID_UNINITIALIZED` |
| 52 | 4 | node/plane | exact bounded node/plane index or `UINT32_MAX` |
| 56 | 8 | auxiliary value 0 | documented per event ID |
| 64 | 8 | auxiliary value 1 | documented per event ID |

Event IDs are: 1 `BatchSubmit`, 2 `BatchComplete`, 3 `BatchCancelOrFault`,
4 `NativeFenceCreateOrOpen`, 5 `NativeFenceWaitOrSignal`, 6
`NativeFenceCloseOrReset`, 7 `PlaneCandidate`, 8 `PlaneLatchOrCancel`, 9
`PlaneReaderReleaseOrUnbind`, 10 `DeviceResetOrRemoval`, 11
`TranslationSessionCreateAttachOrDrain`, and 12
`TranslationSynchronousProgress`. Keywords are bit 0 submission, bit 1 native
synchronization, bit 2 display lifetime, bit 3 device lifecycle, and bit 4
translation-session lifetime. Events 11-12 serialize generations, endpoint,
sequence, and result state only; they never serialize HQA1's capability or a
host/KMT/resource handle. The ABI file defines each event's flag and
auxiliary-field interpretation; unknown flags are zero on emission and ignored
only by a newer decoder according to its event version.

`EtwProviderEnabled` is checked before payload construction. Event descriptors
and payloads used above `APC_LEVEL` are bounded nonpaged system-space objects;
each `EtwWrite` uses one data descriptor, far below the documented 128 limit,
and performs no allocation, wait, lock acquisition, host call, or object
lookup. ETW can drop events. No reader acknowledgement, event delivery, or
counter value participates in synchronization, recovery, lifetime, or package
admission. `DxgkDdiCollectDbgInfo{,2}` and
`DxgkDdiCollectDiagnosticInfo` independently copy only bounded snapshots when
dxgkrnl invokes them; no user caller can turn them into a query protocol.

No object name contains PID. No handle value is serialized. No operation opens a
handle per Present. Epochs are 64-bit, per image, strictly increasing, and never
written by both components.

WSI teardown relies on the application's C48 valid-usage precondition: every
pre-destruction use of a presentable image acquired from this swapchain,
including access through an alias while that slot was acquired, has completed.
The layer cannot inspect arbitrary recorded application command buffers and
does not claim otherwise. It stops new Acquire/Present, marks the swapchain
retiring, and waits/cancels only its own outstanding helper/D3D/DXGI work
through exact events. For every slot left in external ownership it waits the
exact `Release[i]=e`, submits the reciprocal EXTERNAL-to-canonical-family
ownership barrier on the private helper, and waits that helper milestone before
returning from destruction. It releases every DXGI backbuffer reference before
`ResizeBuffers` or DXGI destruction.

Destroying the Vulkan swapchain invalidates its Acquire/Present operations and
the canonical handles returned by `vkGetSwapchainImagesKHR`; it does **not**
destroy a separately app-created C45 image whose destruction contract is
`vkDestroyImage`. Each such object transitions to `ALIAS_ONLY`, loses its WSI
parent/index operations, and remains an ordinary lower Vulkan image with the
current Vulkan ownership/layout until its own GPU references and
`vkDestroyImage` retire. No new alias can be created or bound through the now-
invalid swapchain handle. The layer retains the exact `S[i]` payload, resource
handle, and distinct lower image/memory references needed by surviving aliases;
after the last alias and its GPU work retire it closes `H[i]`, destroys the
remaining import/backing, and drops the final slot/device references. The
canonical image, D3D resource/fences, per-slot recording objects, DXGI objects,
and helper reference may retire earlier only when their exact dependency counts
reach zero. Device loss skips successful-wait assumptions but still cancels and
releases CPU object references in dependency order; it never treats
`UINT64_MAX` as completed work.

## 13. Static proof of no recursive UMD/ICD path

### 13.1 Allowed call graph

```text
D3D11 app/DWM -> D3D11 runtime -> UMD -> DXVK -> direct ICD dispatch
             -> HTS1 pure control -> raw HVC1 KMT -> KMD/Venus -> reply
             -> record-only Mesa -> return actual batch -> UMD -> WDDM

D3D12 app/layer -> D3D12 runtime -> UMD12 -> vkd3d -> direct ICD dispatch
                -> HTS1 pure control -> raw HVC1 KMT -> KMD/Venus -> reply
                -> record-only Mesa -> return actual batch -> UMD12 -> WDDM

Vulkan app -> Vulkan loader -> Helios WSI layer -> lower normal ICD
           -> KMT legacy Render/sync -> dxgkrnl/KMD -> Venus host
           \-> D3D12/DXGI -> UMD12 path above (direct ICD, no loader)
```

There is no edge from Mesa/ICD to DXGI/D3D. There is no edge from either
translator to the Vulkan loader. Therefore the forbidden cycle

`ICD -> DXGI -> D3D UMD -> translator -> ICD`

cannot be formed.

### 13.2 Enforced dispatch proof

- DXVK's `vulkan_loader.cpp` loader search is disabled in Helios UMD builds.
- vkd3d's instance creation receives the same private direct-ICD proc table.
- every instance/device/queue proc-address request is resolved from that table;
  a pointer whose owning module is the Vulkan loader or WSI layer is rejected.
- translator instances carry a non-forgeable record-only tag and advertise no
  Win32 surface/swapchain extension.
- each translator `vn_instance` owns one separate raw HVC1/HTS1 control
  session. Its only KMT edge is the finite pure-control path, whose sole
  allocation access is the session's exact HVM1 reply/feedback range; it cannot
  submit outer-allocation-backed/GPU work. Every outer context attaches once through HQA1,
  after which KMD uses a direct session/endpoint reference. No process-session
  search is reachable from Render/Submit/Present.
- C45 alias create/bind consumes the virtual swapchain structure in the layer
  and reuses an already-created same-process resource handle only through lower
  Vulkan external-memory calls; it makes no D3D/DXGI call from the ICD and
  exposes no virtual swapchain to translator direct dispatch.
- every normal lower queue owns exactly one HVC1 legacy KMT context and submits
  HNR2 only through `D3DKMTRender`; its imported-fence operations name that
  same context. The control context and queue contexts share their HTS1
  session's one KMD/host Venus object namespace; ring zero is statically restricted to
  CPU/decode semantics, while only a nonzero queue context can establish GPU
  completion. User mode never receives the host object ID.
  The normal ICD has no D3D/DXGI import and no
  `D3DKMTCreateContextVirtual`, `D3DKMTSubmitCommand`, private Escape, IOCTL,
  global lookup, or service edge.
- the WSI layer refuses a device whose downstream D3D UMD reports a mismatched
  package generation/direct-dispatch state.
- static import-table tests reject `dxgi.dll`, `d3d11.dll`, and
  `d3d12.dll` imports from the ICD DLL and reject `vulkan-1.dll` imports
  from the translator-bearing UMD binaries.

### 13.3 Lock/reentrancy rules

The layer holds no swapchain/device mutex while invoking D3D/DXGI or the lower
ICD. It transitions state under a short per-image lock, takes an object
reference, releases the lock, performs the external call, then commits the
result under the lock. UMD callbacks similarly hold no translator/Mesa lock
while calling the runtime submission callback. Destroy uses an explicit
retiring state and reference count, not lock nesting. Native Render staging
uses only the owning context's bounded slot-pool lock; it releases that lock
before KMT, scheduler, or host calls, and a host callback addresses its already-
referenced context/slot directly rather than acquiring an adapter-global map.
ProcessContext's session-list lock is held only while creating, attaching, or
invalidating HTS1 and never across KMT, runtime, host, allocation, or GPU
waits. After HQA1, the outer context owns a direct reference. The per-endpoint
FIFO lock is held only to assign/insert/remove a bounded sequence record and is
released before scheduler/host work or an HQC1 event wait. A synchronous
control call holds no translator, endpoint, session-list, or UMD runtime lock
while waiting.
These rules remove reentrancy and lock inversion even on callbacks/error
teardown.

## 14. Lifetime, identity, reset, crash, and reuse proofs

| Case | Proof |
|---|---|
| Allocation reuse | A new WDDM allocation object/GPUVA mapping or NT resource handle is a new object. KMD owns a new backing reference for each allocation generation and retains it while any allocation/open/display reference survives. Create/open private data is only an immutable descriptor paired with that object; it contains no reusable backing token. A raw `resid` or HPS slot is never accepted as identity. |
| Simultaneous swapchains | each DXGI chain owns B[], S[], fence pairs, epochs, state, current-backbuffer query, and lock; no global lookup/value domain |
| Swapchain-memory aliases | each real lower alias is bound once to the exact `(swapchain backing,imageIndex)->S[i]` and owns a distinct dedicated imported memory reference. Present identity remains the original swapchain/index; retired/destroyed presentation state cannot free the backing until all alias and GPU references retire. |
| Multiple queues | each D3D API queue owns one runtime context/submission-fence stream and HQC1; contexts explicitly mapped to the same physical lower VkQueue share only that HTS1 endpoint's bounded KMD arrival-order dispatch FIFO/nonzero ring. They never wait on a UMD-assigned cross-context sequence. One D3D12-device HOB1 pool may contain extents owned by several queues, but its short allocator lock protects only free-range metadata; each extent retires against its exact owning HQC1 after the lock is dropped. Each normal Vulkan queue owns one distinct legacy KMT context, bounded slot pool, C51 object, and endpoint. Core-0116 native objects express explicit intercontext edges; no progress object shares a writer/value namespace |
| Multiple devices/processes | each Mesa `vn_instance` owns a distinct HTS1/host context even inside one process; the exact KMD ProcessContext contains only its bounded live-session list. Kernel handles and private objects remain device/process scoped; DWM opens resources through OS sharing and owns its own session |
| PID reuse | PID is absent from identity and lifetime |
| Process crash | OS closes device/queue/allocation handles; KMD cancels host batches and queued flips, releases refs once |
| DWM restart | app objects either continue under DXGI or receive reset/removal/status; recreated DWM opens exact allocations anew |
| Device removal/TDR | no submission completion/Release is forged; native `UINT64_MAX` removal state is failure; all generations change |
| Adapter reset | KMD first changes adapter/package-visible generation and marks every HTS1 session `Draining`, invalidates all HQA1 capabilities, and cancels C51/HQC1 event waiters as device-lost; UMD poisons every HOB1 pool extent without considering its retire value complete. KMD then invalidates queue, endpoint/ring, allocation/HPM1, host-batch, native-fence mapping, WSI, and flip generations together. Stale callbacks release only their held old-generation reference and cannot publish reply/completion or enter a replacement session |
| Window resize | layer/DXGI retires the old acquisition/present and `B[]` state before ResizeBuffers and creates a new generation; old `S[]` backing remains generation-qualified while any C45 alias or GPU reference survives |
| Fullscreen enter/leave | stock DXGI transition plus exact plane candidate/current teardown; no custom consumer assumption |
| Composed to independent and back | only the OS-selected consumer changes; producer completion is always the same primary-bearing runtime context |
| Occlusion | DXGI status/pacing controls; no polling or copy-anyway path |
| Cross-adapter request | LUID/UUID/feature mismatch fails; no implicit cross-adapter share |
| Fence value reuse | per-image Ready/Release epochs strictly increase and never wrap; each fence has one writer |
| Queue destruction with work | runtime/KMD queue ref and host batch ref persist until complete/cancel; destroy waits/cancels |
| Display reader | the exact current-plane allocation and backend lease survive until replacement/unbind plus backend release; no PresentId is treated as release |
| Late host callback | context, allocation, VidPn source/plane, and binding generations are validated; stale callback releases only its own held ref and cannot complete or replace a newer object |

Ownership references form this chain for a D3D batch:

`process -> device -> queue -> command buffer -> WDDM allocations -> KMD host
batch -> host completion -> exact SubmissionFenceId completion`.

For a normal native Vulkan batch the independent chain is:

`VkDevice/vn_instance -> HTS1 -> raw KMT device -> one KMD/host Venus namespace -> real lower VkQueue ->
nonvirtual KMT context/nonzero host ring -> HNR2 COMMIT plus exact allocation-
list references -> KMD context-local slot -> matching host-ring terminal/reply
completion -> ordered exact scheduler SubmissionFenceId -> optional same-
context progress-fence signal`.

For a translated instance/control and outer batch the chain is:

`KMD ProcessContext -> HTS1(raw HVC1 control + one host VkInstance) -> HQA1-
attached outer context/direct physical endpoint -> context-local sealed batch +
exact outer allocations -> host nonzero-ring completion -> WDDM SubmissionFenceId
-> optional private HQC1 signal/event -> bounded pure-control reply`.

For a direct flip the chain is:

`OS flip/MPO candidate -> exact allocation ref -> backend latch or cancel ->
VidPn source/MPO plane current binding -> backend reader lease -> later
replacement/unbind and backend release`.

No node is reclaimed merely because its creator process is unresponsive; kernel
teardown/cancel owns the transition.

## 15. Security and access-control model

- D3D app/DWM sharing uses dxgkrnl/DXGI's normal resource security and object
  ownership. Helios creates no DWM-accessible custom fence.
- WSI resource/fence handles are unnamed and stay inside one application
  process. For each resource/fence it calls `CreateSharedHandle` with the API's
  only accepted access mask, `GENERIC_ALL`, `pAttributes=NULL` (default
  creator-token security descriptor and non-inheritable handle), and
  `Name=NULL`. Fence handles are closed immediately after their permanent
  semaphore imports. The single resource handle `H[i]` is retained only in
  that swapchain image's private layer state so C45 aliases can import the same
  payload without creating new handles; it closes at backing/alias retirement.
  Every imported memory/semaphore owns its kernel payload reference (C14-C16,
  C45).
- A handle value never enters allocation private data, a command buffer,
  protocol message, log, environment variable, or file.
- HTS1's 128-bit capability is a CSPRNG admission nonce scoped to one exact KMD
  ProcessContext/adapter/package/session generation. It authorizes no resource
  and is not treated as protection against code already executing inside that
  process. It appears only in HQA1 create-context PDD, is zeroed from later DMA
  state, is never logged, persisted, or reused, and is invalidated before reset
  or process teardown wakes waiters.
- Normal native Vulkan execution is process/device/context scoped. HVC1/HNR2/
  HVM1 contain no pointer, kernel/NT handle, PID, host object ID, or reusable
  backing token. Dxgkrnl supplies the exact COMMIT allocation-list handles and
  final Patch segment/physical addresses;
  KMD fully parses the bounded opcode schema and validates every declared read/
  write/generation/patch before substituting only KMD-owned backing references.
  HNR2 DMA capabilities and HPM1 epochs exist only inside KMD/device-model
  hardware commands; Mesa cannot read or originate them. HPM1 placement comes
  only from OS paging inputs and is keyed by the live KMD allocation generation,
  not a UMD-provided resource ID. HLM1 exposes one linear BAR only through the
  ordinary WDDM segment/Lock2 contract; the package advertises no CPU Host
  Aperture callback or private page-map protocol. Venus allocations and progress
  fences are unshared and unnamed. The bounded physical-placement state is
  direct KMD/device state, not a cross-process discovery table or searchable
  resource registry.
- Each HTS1 control path may write only one checked-out range in its one
  unshared 64-MiB HVM1 reply-pool allocation. The four fixed slots are
  generation/token/C51-bound; a host range outside the exact slot, a stale
  owner, or a fifth concurrent result is rejected or event-backpressured. No
  reply allocation is grown, shared, named, or reachable from another session,
  and no reply carries an outer allocation capability. The corresponding host
  snapshot is immutable, session-local, generation-checked, capped at 64 MiB,
  and one of at most four/256 MiB; final consume, cancel, reset, or teardown
  zeroes/frees it after its source refs drain.
- D3D outer batches are equally bounded. HOB1 contains only a context-scoped
  allocation-list index or GPUVA plus expected live allocation generation;
  HOS1 contains only package/session/context generations, endpoint, batch ID,
  length, and checksum. Neither record carries a CPU pointer, kernel/NT handle,
  PID, renderer ID, or reusable discovery key. KMD never CPU-dereferences the
  submitted GPUVA at `DISPATCH_LEVEL`; HPM1/QEMU resolves it only through the
  exact retained ProcessContext/address-space generation and current OS-built
  page tables.
- HOC1 is one device-associated, nonshared allocation descriptor. It contains
  only package/allocation generation, fixed size/alignment/access/cache/node,
  and reserved zero; it carries no resource handle, CPU VA, GPUVA, process ID,
  session capability, or host token. The UMD retains the real callback-returned
  allocation/Lock2/GPUVA objects directly and never reconstructs them from
  HOC1. The CPU publishes HOB1 only through the selected x64 WC-drain plus
  release-seal boundary; QEMU never reads an extent before its scheduler/device
  enqueue and the UMD never modifies it afterward.
- Package generation and adapter/device UUID checks prevent another DLL/adapter
  from being treated as a peer.
- `C:\ProgramData\Helios` is no longer security-sensitive for Present
  synchronization. Installer removes its HPS ACL/setup only after the legacy
  quiescence boundary.
- No feature registry override is installed (C6).
- Protected resources are refused by the WSI bridge unless the exact protected
  cross-component contract is implemented; they never fall back to an
  unprotected copy.

## 16. Performance model

### 16.1 D3D steady state

| Scope | CPU / kernel / handle operations |
|---|---|
| D3D resource create/open | normal WDDM allocation create/open and GPUVA map/residency; no HPS map, slot scan, name, or fence open |
| Translator HTS1 lifetime | per Mesa `vn_instance`, one raw KMT device/control context, one host context/VkInstance, one bounded ProcessContext session entry/capability, one control C51 object, one fixed 64-MiB HVM1 reply pool with four slots, and at most four immutable host reply snapshots/256 MiB; per outer context, one HQA1 validation/direct ref and one HQC1 object; no per-batch lookup/open or reply spill |
| Synchronous translated control | pure control costs one finite HVC1 Render plus one bounded session-scratch reply and event; results above 15 MiB cost one additional finite HNR2/C51 transaction per exact-next-offset snapshot chunk. Snapshot creation/copy is O(result bytes), capped at 64 MiB each. A GPU-dependent result additionally costs one exact outer HQC1 signal and one event wait after pending actual work; outer-allocation-backed work adds no control submit and rides its actual outer batch |
| D3D11 batch | one record/seal pass proportional to command bytes/resource uses; one normal Render callback/kernel submission; one host completion interrupt/notification |
| D3D12 device/fence/pool lifetime | one Core-DDI negotiation; per fence one native create or open callback/KMT transition, process-local mappings, and final close/destroy; one fixed 64-MiB/256-live-extent HOB1 GPUVA pool per device; zero per-use handle opens or spill allocations |
| D3D12 batch | one record/seal pass into a resident HOB1 extent plus fixed 64-byte HOS1; one `pfnSubmitCommandCb` transition and one exact-context HQC1 retirement signal per actual batch; one completion check while allocating the next extent and an event wait only under bounded pool pressure; one HPM1 GPUVA/page-table walk and host-private operand substitution proportional to mapped pages/uses; one DMA-completed notification after host completion |
| D3D12 Queue Wait/Signal | one exact-context runtime callback and kernel synchronization operation per API call; zero custom handle opens and zero lower Vulkan queue submits |
| D3D Present | one normal Present transition; zero HPS scans/opens/publishes |
| DWM composition | one normal D3D11 batch on DWM's context; zero custom producer lookup |
| Direct/MPO3 plane update | one OS classic SetVidPn or MPO3 call, O(1) shared exact-allocation/source/current-mode validation and reference, one bounded nonblocking backend enqueue; no host wait, global scan, or allocation search at interrupt level |
| Direct-Flip eligibility | one O(1) D3D11 UMD comparison of two live wrappers/HWA2 descriptors when DWM probes, after mode change, or after DWM swapchain recreation; no kernel transition, Escape, handle open, global scan, or per-Present call |
| Diagnostics | ETW disabled: one `EtwProviderEnabled`/atomic-mask branch at each selected event site and no payload construction or trace-buffer write; ETW enabled: one fixed 72-byte nonpaged payload plus one-descriptor `EtwWrite` per context submit/retire/reject, native-fence transition, or plane candidate/latch/release event. Registration/unregistration occurs only at driver lifetime boundaries. OS diagnostic callbacks run only when invoked by dxgkrnl; there is no user query syscall, private handle, scan, poll, or per-Present Escape. |

CPU remains asynchronous with GPU completion. A queue-local CPU lock is held
only while appending/sealing one batch. The D3D12 device's HOB1 allocator has
one separate short free-range lock that is dropped before every callback and
event wait. The native-WSI layer has one
device-local mutex used only for calls it makes on its private, non-app-visible
Acquire helper queue. Application Present queues remain caller-externally-
synchronized and share neither that queue nor its lock. Different physical
endpoints, swapchains, devices, processes, and adapters use disjoint
streams/state. Logical queues explicitly mapped to one physical endpoint share
only that endpoint's bounded FIFO and short append lock; they share no
progress-value writer or CPU completion wait. KMD completion bookkeeping is
per runtime context/scheduler lane. There is no
adapter-global scan or lock on the Acquire/Present fast path.

### 16.2 Native Vulkan execution and WSI costs

Normal lower-ICD execution, independent of whether WSI is used, costs:

- once per adapter start, before segment enumeration: one HPM1 feature/
  generation negotiation, validation of the complete prefetchable linear BAR,
  and initialization of the bounded local-placement/paging state used by HLM1.
  Metadata is bounded by the reported segment/page and allocation limits; byte
  backing is bounded by the reported segment size. Failure rejects adapter
  initialization; none of this repeats per device, allocation, batch, Acquire,
  or Present;
- once per normal Vulkan `vn_instance`: one exact-adapter KMT device, one HTS1
  KMD/host Venus context/object namespace, one CPU/decode-only control KMT context on host
  ring 0,
  one exact 64-MiB/four-slot reply pool, one control-context monitored progress fence,
  and no shared command ring;
- once per real lower queue: one nonvirtual `D3DKMTCreateContext`, one unique
  nonzero host `INFO_RING_IDX`, one context-local assembler/pool capped at 64
  slots and 15 MiB, one 4096-entry allocation/output-patch list (using the
  documented next-list resize path if Dxgkrnl returns less), and one
  queue-context monitored progress fence;
- per WDDM placement/fill/discard transaction: one bounded
  `DxgkDdiBuildPagingBuffer` encode pass over the OS MDL/ADL/range, one paging
  mandatory infallible/no-size-change/side-effect-free paging Patch, one paging
  Submit,
  `ceil(runCount/maxRunsPerPacket)` HPM1 DMA packets when
  multipass is required, the required O(bytes) copy/fill (discard is O(page
  runs)), and one terminal QEMU acknowledgement plus DMA-completed interrupt.
  Source and destination placement/host refs are retained through completion. This is a
  VidMm residency/placement cost, not a Render, Acquire, or Present cost;
- per ordinary HVM1 allocation lifetime: one WDDM allocation/renderer-view
  creation and HPM1 placement binding. Device-local-only memory has no Lock2
  transition. Per internal pool or host-visible application map lifetime there
  is one ordinary `D3DKMTLock2`/`D3DKMTUnlock2`; Lock2 returns either the direct
  linear HLM1 BAR VA or VidMm's byte-identical system backing. There is no
  custom map callback/protocol and no map/open at Render, Acquire, or Present;
- per native Vulkan batch: validation proportional to
  `payloadBytes + 24*useCount + 16*patchCount`, with `useCount<=4096` and
  `patchCount<=8192`, `k` `D3DKMTRender` kernel
  transitions, `k` scheduler Patch/Submit passes, O(useCount) output physical-
  capability patches on COMMIT, exactly one HPM1 validation plus real fenced
  `SUBMIT_3D`, and one terminal host completion interrupt.
  Nonfinal fragment packets are small scheduler no-ops; the common <=256-KiB
  batch has `k=1`, while total payload is bounded at 15 MiB/64 fragments. The
  minimal WSI barrier batches below must each fit one fragment; and
- only when an API-visible CPU milestone/reply/idle/teardown requires it: one
  same-context KMT FromGpu progress signal after the last relevant Render and
  one blocking/event-based KMT CPU wait. Fire-and-forget work pays neither;
  a reply additionally checks out/poisons one exact-size HVM1 slot and validates
  its generation/token/opcode/size after the event. There is no progress
  polling, ring head/tail traffic, or per-batch handle creation. Queue idle
  pays one such signal/wait on that nonzero queue; device idle/teardown pays
  O(number of live queue contexts) snapshot/signal/waits, never one ring-0
  shortcut. Ring-0 CPU replies pay their own control signal/wait only when the
  caller needs a synchronous result;

Recording and slot-pool contention is per lower queue. The lock covers only a
bounded stage/adopt transition and is not held across KMT or host calls; queues,
devices, processes, and adapters share no native-Render pool or lock. HPM1 has
one per-paging-context transaction frontier plus exact per-allocation-generation
placement records. Build/Patch never waits on QEMU; Submit enqueues bounded
device work, and the old/new placement references drain through scheduler/device
completion. Disjoint allocation transactions can progress independently, and
no CPU-side scan, retry sleep, or polling worker exists. HPM1 is not on the
render/Present fast path except for O(useCount) validation of already-patched
capabilities. Distinct adapters are independent. The selected
single WDDM render engine
retains out-of-order host completions until its scheduler frontier advances, so
CPU-visible completion on one queue can wait behind earlier work from another;
host execution and CPU submission remain asynchronous. No global allocation
scan or serialized Present lock is introduced.

Per swapchain image, once:

- one shared committed D3D12 texture;
- one D3D12 resource NT-handle creation, one canonical Vulkan import, and one
  retained private handle close at final backing retirement;
- two D3D12 fences;
- two fence NT-handle creations and two permanent Vulkan imports;
- two transient fence-handle closes;
- one D3D12 copy command allocator/list;
- one lower-Vulkan resettable command pool and two minimal per-slot barrier
  command buffers; and
- one bounded state/Release epoch record. These recording objects are reset
  only after the exact prior Release completes.

Per application-created C45 alias image, once at create/bind and never at
Acquire/Present:

- one real lower `vkCreateImage` and a device-scoped O(1) alias record;
- one `vkGetMemoryWin32HandlePropertiesKHR` query on the already retained
  `H[i]` (normally one Vulkan-to-kernel query boundary);
- one distinct dedicated `vkAllocateMemory` D3D12-resource import and one
  `vkBindImageMemory2` (normally one import/open plus one bind boundary);
- zero new/duplicated NT handles; and
- one lower image/memory destruction at the application's alias lifetime end.

Alias count and memory objects are bounded only by the application's ordinary
Vulkan object/allocation limits, not by a Helios global table. Their bookkeeping
lock is device-local and held only for create/bind/destroy metadata updates;
no queue or D3D/DXGI call occurs while it is held.

Per device, once, the layer reserves one additional real lower queue in the
canonical family and one short helper mutex. The reported app-visible count is
one less than the true lower count; no application queue or dispatch wrapper
shares the helper queue. A lower family with fewer than two queues is not
Win32-WSI capable.

Per `VkPresentInfoKHR` containing `n` swapchain images:

- exactly one lower Vulkan submission on the presented queue, waiting each app
  binary semaphore once; it contains `n` external-release barriers and signals
  `n` distinct Ready timeline values. This minimal batch must fit one HNR2 and
  therefore adds exactly one normal-ICD `D3DKMTRender` kernel transition and
  one eventual host-completion notification for the whole PresentInfo;
- per image, the ICD issues one exact-context
  `D3DKMTSignalSynchronizationObjectFromGpu` syscall/queue operation for that
  Ready value after the release barrier; this is a kernel transition, not an
  additional handle open;
- per image: one D3D12 Queue Wait (one callback/kernel sync operation), one
  actual D3D12 copy batch (one `SubmitCommandCb` kernel transition) with four
  state transitions, one `CopyResource` of the admitted BGRA8 extent, one
  D3D12 Queue Signal (one callback/kernel sync operation), and one ordinary
  DXGI Present (one runtime/kernel Present boundary);
- zero handle opens/closes, object scans, CPU GPU waits, or global locks.

Per subsequent Acquire:

- at most `N` O(1) per-slot state/`GetCompletedValue` checks within that one
  swapchain; the common ready case finds a completed/never-used slot and has no
  blocking kernel wait;
- after exact prior Release completion, one D3D allocator/list reset and one
  Vulkan pool/two-buffer reset for the selected slot (CPU work only);
- one Vulkan private-helper submission/kernel queue boundary, one
  external-acquire barrier, and one binary semaphore/VkFence signal. This
  minimal helper batch must fit one HNR2 and adds exactly one normal-ICD
  `D3DKMTRender` transition and one eventual host-completion notification;
- for a reused slot, one exact-context
  `D3DKMTWaitForSynchronizationObjectFromGpu` syscall/queue operation for the
  already-completed Release value before that barrier (retained for exact
  queue/memory ordering, not CPU availability); and
- only under backpressure and when the API timeout permits blocking, one exact
  `SetEventOnCompletion` arm and one OS event wait on the oldest queued Release
  or state-change event. Timeout/loss is returned after one revalidation; there
  is no sleep, polling loop, or unbounded allocator/list growth.

For an admitted `W x H` BGRA8 image, the copy command logically reads
`4*W*H` bytes from `S` and writes `4*W*H` bytes to `B`: `8*W*H` bytes of
full-frame image traffic per Present. Cache, compression, tiling, and physical
bus traffic are hardware-dependent and are not claimed by this model. There is
no shader, format conversion, resolve, scale, or tonemap cost because those
profiles are not advertised. The full-frame copy is the explicit cost of
keeping DWM on its real DXGI backbuffer without an undocumented DWM carrier.
It removes HPS scans, named-fence open work, CPU timeouts, and recursive
teardown; no per-Present handle discovery or opening exists.

### 16.3 Removed costs

- DXVK lookup: up to 4096 slot visits per resource use;
- publish: up to eight full 4096-slot attempts and process-liveness queries;
- ProgramData file open/map and ACL repair;
- named semaphore reconstruction/open/cache;
- Escape per graphics/present operation;
- 10 ms read-ledger polling/signaler loop;
- global 65-slot reader page and scans;
- CPU `copy anyway` timeout paths;
- independent lower-queue submission and extra proxy submission.

## 17. One-shot implementation manifest

This is the minimum exact file manifest. A later implementation may add tests
or generated bindings, but may not omit a listed live owner.

### 17.1 Protocol and package generation

- Add `protocol/src/translation_session.rs` and export it from
  `protocol/src/lib.rs`: exact little-endian HQA1 plus HTS1 INIT/reply,
  endpoint descriptor, pure/deferred/GPU-dependent opcode classes, bounded
  session/ring/context-local-batch/host-dispatch-FIFO limits,
  generation/capability validation, and
  size/alignment/offset assertions. HQA1 contains no pointer, KMT handle, PID,
  allocation/resource identity, host object ID, or synchronization object.
  HQC1 is an OS sync object and has no private wire payload.
- Add `protocol/src/native_render.rs` and export it from `protocol/src/lib.rs`:
  exact little-endian HVC1/HNR2/HVM1, 24-byte use and 16-byte typed-patch
  records, fragment/access/operand/role enums, package/capset constants,
  size/alignment assertions, HVR1 plus the continuation-snapshot fields, and the
  15-MiB/64-fragment/4096-use/8192-patch bounds from
  section 10.7. These records are only the bounded normal-ICD-to-KMD Render and
  allocation ABI; they contain no pointer, handle, PID, host object ID, name,
  or lookup key. Define/assert the internal 48-byte
  `HNR2PhysicalCapability` and the exact one-output-patch-per-use rule; this
  record exists only in scheduler DMA and is never returned to user mode.
- Add `protocol/src/physical_memory.rs` and generated QEMU C declarations:
  exact HPM1 feature/generation and complete-HLM1-BAR admission fields,
  `HeliosPhysicalMemoryDmaV1`, paging operation enum, fixed header, 24-byte
  physical-page-run record, ADL/segment/page-size interpretation, prior/new
  placement/TLB epoch, internal address-space/root generation, GPUVA/PTE range,
  multipass bounds, terminal status, and all reserved-zero assertions.
  This is KMD/QEMU device DMA implementing VidMm paging; it contains no UMD
  allocation handle, pointer, PID, renderer ID, CPU-host-aperture opcode, or
  user-mode mapping token.
- `protocol/src/wddm.rs`: replace HVD/present-ticket and raw-`resid` *command
  identity* with the exact HOB1 header, 40-byte allocation-list/GPUVA use
  records, 16-byte typed operands, and fixed 64-byte D3D12 HOS1 private-data
  descriptor from section 10.4. Generate/assert both Rust and C offsets and the
  D3D11-type-1 versus D3D12-type-2 validator; no HOB1 record orders another
  context, and HOS1 carries no command/resource bytes. Define/assert C65's
  64-MiB pool, 64-KiB extent alignment, 256-live-extent limit, 15-MiB HOB1
  limit, immutable-seal state, and exact HQC1 retirement tuple. Define/assert
  the exact 64-byte HOC1 allocation record and its create-time generation
  write-back; HOC1 is neither a renderer/resource identity nor a shareable
  object. Replace
  `HeliosWddmOpenIdentity` with an
  immutable create-time allocation descriptor carrying package/allocation
  generation and format/layout flags, including the exact C44
  `D3D12_RUNTIME_PRIMARY`/`D3DDDI_ID_UNINITIALIZED` combination. It carries no
  host resource token; KMD's exact allocation object owns and resolves the host
  backing.
- Add `protocol/src/diagnostics.rs` plus the generated C bindings for the exact
  section-12.3 provider GUID, event descriptors, 72-byte payload, flags, and
  keyword constants. This is a one-way lossy ETW schema, not a control/query
  protocol and contains no handle, pointer, backing token, or raw `resid`.
- `protocol/src/escape.rs`: delete the file and its module export. Every current
  verb `0x0001..0x0011` is graphics, presentation, synchronization, allocation,
  or their private diagnostic query; none belongs to the new package. Do not
  retain `QUERY_STATS` or `QUERY_SCANOUT_TIMELINE` as an observability fallback.
- `protocol/src/ioctl.rs`: delete the legacy System-class graphics IOCTL ABI
  (`CTX_*`, `SUBMIT_VENUS`, blob/map/release, `WAIT_FENCE`, and
  `PRESENT_BLOB`) with the obsolete `kmd/` package. The installed package uses
  `kmd_render`; no IOCTL verb becomes a compatibility carrier for the new
  generation.
- Add one protocol/package generation constant shared by protocol, Mesa,
  UMD11, UMD12, KMD, QEMU, and installer. Generation mismatch is fatal.

### 17.2 DXVK repository

Delete:

- `dxvk-helios/src/dxvk/dxvk_helios_present_sync.h`
- `dxvk-helios/src/dxvk/dxvk_helios_present_sync.cpp`
- `dxvk-helios/src/dxvk/dxvk_helios_scanout_acquire.h`
- `dxvk-helios/src/dxvk/dxvk_helios_scanout_acquire.cpp`

Modify:

- `dxvk-helios/src/dxvk/meson.build`
- `dxvk-helios/src/dxvk/dxvk_context.cpp`,
  `dxvk_context.h`
- `dxvk-helios/src/dxvk/dxvk_memory.cpp`,
  `dxvk_memory.h`
- `dxvk-helios/src/dxvk/dxvk_device.cpp`, `dxvk_device.h`
- `dxvk-helios/src/dxvk/dxvk_image.cpp`, `dxvk_image.h`
- `dxvk-helios/src/dxvk/dxvk_options.cpp`, `dxvk_options.h`
- `dxvk-helios/src/d3d11/d3d11_context.cpp`
- `dxvk-helios/src/d3d11/d3d11_context_imm.cpp`
- `dxvk-helios/src/d3d11/d3d11_texture.cpp`
- `dxvk-helios/src/d3d11/d3d11_device.cpp`
- `dxvk-helios/src/d3d11/d3d11_on_12.cpp`,
  `d3d11_on_12.h`, `d3d11_main.cpp`
- `dxvk-helios/src/d3d9/d3d9_common_texture.cpp`
- `dxvk-helios/src/d3d9/meson.build`
- `dxvk-helios/src/wsi/win32/wsi_window_win32.cpp`
- `dxvk-helios/src/util/util_gdi.h`, `util_gdi.cpp`
- `dxvk-helios/src/util/util_shared_res.h`, `util_shared_res.cpp`
- `dxvk-helios/src/util/meson.build`
- `dxvk-helios/src/util/config/config.cpp`
- `dxvk-helios/src/vulkan/vulkan_loader.cpp`,
  `vulkan_loader.h`
- `dxvk-helios/src/dxvk/dxvk_instance.cpp`,
  `dxvk_instance.h`
- `dxvk-helios/src/dxvk/dxvk_queue.cpp`,
  `dxvk_queue.h`
- `dxvk-helios/src/dxvk/dxvk_cmdlist.cpp`,
  `dxvk_cmdlist.h`
- `dxvk-helios/src/dxvk/dxvk_sparse.cpp`, `dxvk_sparse.h`
- `dxvk-helios/src/dxvk/dxvk_presenter.cpp`, `dxvk_presenter.h`

Required result: no HPS/scanout lookup, no loader/layer dispatch in Helios UMD
builds, record-only submission, complete exact resource-use output, and no
lower KMT queue submission. The live `vkQueueSubmit2` at
`dxvk_cmdlist.cpp:96-104` becomes seal-and-return; `vkQueueBindSparse` at
`dxvk_sparse.cpp:528` lowers to the outer allocation/paging model or sparse
support is not advertised; and private D3D builds cannot reach
`vkQueuePresentKHR` at `dxvk_presenter.cpp:212`. D3D11On12 imports the exact
vkd3d queue and seals Acquire/Release barriers plus D3D11 commands into that
D3D12 runtime-context stream.

DXVK initializes exactly one direct-Mesa HTS1 session before the D3D11 UMD
creates its physical runtime context, exports the selected immediate physical
queue endpoint only through the direct bridge, and never merges two Mesa
instances. Its queue/device idle, query WAIT, fence status/wait, pipeline
compile result, and resource-requirement paths use the C60 classification:
pure control on HVC1 with only its exact session-owned reply/feedback range,
GPU-dependent result through the UMD's exact HQC1 join, and every
outer-allocation-backed create/bind/map/destroy folded into an actual
D3D11 outer batch. Add static coverage for every such synchronous callsite;
no call may fall through to a blocking shared Venus ring or raw lower GPU
queue.

The same atomic DXVK change removes every live
`D3DKMT_ESCAPE_UPDATE_RESOURCE_WINE` and
`D3DKMT_ESCAPE_SET_PRESENT_RECT_WINE` call/type plus the
`\\.\SharedGpuResource` open/set/get IOCTL implementation and all callers.
Fullscreen/window rectangles remain owned by stock Win32/DXGI window and
swapchain APIs; no private notification replaces the Escape. D3D11 shared
resources use only the selected ordinary WDDM create/share/open allocation
descriptor path. D3D9 sharing is admitted only if the later implementation
can express the same exact resource through documented ordinary WDDM
create/share/open data; otherwise that shared-resource create/open returns its
documented unsupported/error result before exporting a handle. It must never
fall back to Escape, the Wine device, a name, or inferred texture fields.

### 17.3 Mesa/ICD repository

Delete:

- `icd/mesa/src/vulkan/wsi/wsi_helios_present_sync.h`
- `icd/mesa/src/vulkan/wsi/wsi_helios_present_sync.c`

Modify:

- `icd/mesa/src/vulkan/wsi/meson.build`
- `icd/mesa/src/vulkan/wsi/wsi_common.c`,
  `wsi_common.h`, `wsi_common_private.h`
- `icd/mesa/src/vulkan/wsi/wsi_common_win32.cpp` — remove the entire
  D3D11/DXGI/DComp vehicle, HPS publish/release, named fences, GDI fallback
  selected by that vehicle, and recursive callbacks
- `icd/mesa/src/virtio/vulkan/vn_device.c`, `vn_device.h`
- `icd/mesa/src/virtio/vulkan/vn_buffer.c`, `vn_buffer.h`
- `icd/mesa/src/virtio/vulkan/vn_queue.c`, `vn_queue.h`
- `icd/mesa/src/virtio/vulkan/vn_device_memory.c`,
  `vn_device_memory.h`
- `icd/mesa/src/virtio/vulkan/vn_image.c`, `vn_image.h`
- `icd/mesa/src/virtio/vulkan/vn_pipeline.c`, `vn_pipeline.h`
- `icd/mesa/src/virtio/vulkan/vn_query_pool.c`, `vn_query_pool.h`
- `icd/mesa/src/virtio/vulkan/vn_physical_device.c`,
  `vn_physical_device.h`
- `icd/mesa/src/virtio/vulkan/vn_instance.c`, `vn_instance.h`
- `icd/mesa/src/virtio/vulkan/vn_icd.c`, `vn_icd.h`
- `icd/mesa/src/virtio/vulkan/vn_ring.c`, `vn_ring.h`
- `icd/mesa/src/virtio/vulkan/vn_wsi.c`, `vn_wsi.h`
- `icd/mesa/src/virtio/vulkan/vn_renderer_helios.c`,
  `vn_renderer.h`, `vn_renderer_internal.c`,
  `vn_renderer_internal.h`
- `icd/mesa/src/virtio/vulkan/meson.build`

Add:

- `icd/mesa/src/vulkan/wsi/helios_present_layer.cpp`
- `icd/mesa/src/vulkan/wsi/helios_present_layer.h`
- `icd/mesa/src/vulkan/wsi/helios_present_layer.def`
- `icd/mesa/src/vulkan/wsi/VkLayer_HELIOS_present.json.in`
- `icd/mesa/src/virtio/vulkan/vn_helios_record_submit.c`
- `icd/mesa/src/virtio/vulkan/vn_helios_record_submit.h`
- `icd/mesa/src/virtio/vulkan/vn_helios_direct_dispatch.c`
- `icd/mesa/src/virtio/vulkan/vn_helios_direct_dispatch.h`
- `icd/mesa/src/virtio/vulkan/vn_helios_translation_session.c`
- `icd/mesa/src/virtio/vulkan/vn_helios_translation_session.h`
- `icd/mesa/src/virtio/vulkan/vn_helios_native_kmt.c`
- `icd/mesa/src/virtio/vulkan/vn_helios_native_kmt.h`

Required result: direct private dispatch, record-only mode, D3D12-resource and
D3D12-fence external import with the exact C39 device-feature/extension
enablement, image/dedicated-memory and timeline-semaphore import chains,
IMPORTABLE capability/UUID checks, and the C40 object flags/initial values;
the exactly one private C30 WSI helper queue reserved in the copied
`VkDeviceQueueCreateInfo` table (never the Android-only emulated second queue
in `vn_physical_device.c:909-979`); the exact section-10.7 BGRA8/sRGB/FIFO copy-only
surface query and create profile; and the complete C45 singleton surface:
`vkGetDeviceGroupPresentCapabilitiesKHR`,
`vkGetDeviceGroupSurfacePresentModesKHR`,
`vkGetPhysicalDevicePresentRectanglesKHR`, `vkAcquireNextImage2KHR`, plus
`VkDeviceGroupDeviceCreateInfo`, `VkDeviceGroupSwapchainCreateInfoKHR`,
`VkDeviceGroupPresentInfoKHR`, and singleton bind-device structures. The layer
also owns C46 extension enumeration, instance/device create-chain consumption,
loader-chain dispatch, `vkGetInstanceProcAddr`/`vkGetDeviceProcAddr`, both
queue-family property forms, both queue getters, and a generated exact
entry-point manifest; lower WSI extension names/functions are filtered and no
virtual handle reaches an unwrapped lower entry point. Intercept `vkCreateImage`,
`vkBindImageMemory2`, and `vkDestroyImage` for
`VkImageSwapchainCreateInfoKHR`/`VkBindImageMemorySwapchainInfoKHR`: create a
real lower external image, import exact `S[index]` from the retained per-slot
handle into one distinct dedicated memory, bind it, and retain old-swapchain/
backing state until all alias/GPU refs retire. Consume/strip valid
`swapchain=VK_NULL_HANDLE` create/bind structures and forward the corresponding
ordinary lower operation without alias-only memory restrictions. Eagerly
perform the identical canonical image/query/dedicated-import/bind sequence for
every `S[i]` before exposing `vkGetSwapchainImagesKHR`; parent destruction
invalidates only Acquire/Present and canonical returned image handles, performs
the final external-to-Vulkan ownership return, and moves every separately
created surviving alias to `ALIAS_ONLY` while retaining its exact payload until
normal `vkDestroyImage`/GPU retirement. Reuse the existing upstream
singleton/alias structure at `wsi_common.c:2996-3080` and
`vn_image.c:559-622,717-760` where appropriate, but replace its assumption of
a real lower swapchain with the exact layer-owned D3D12-resource import; never
forward the virtual swapchain handle to the ICD.

For canonical and alias `D3D12_RESOURCE_BIT` import, rewrite the current live
paths in `vn_renderer_helios.c:3696-3864`,
`vn_device_memory.c:550-600,669-737`, and `vn_renderer.h:340-377` around C57's
one query/zeroed-output-array/open operation per imported resource. The caller
allocates the array and queried buffers but never manufactures an
`hAllocation`, private-data pointer, or driver descriptor. Retain the returned
resource/allocation record only after the C57 conformance checks pass; otherwise
leave the handle type unadvertised. Do not replace it with legacy
`D3DKMTOpenResource`, whose `hGlobalShare` namespace is different, and do not
route the ICD through D3D/DXGI. Delete the selected profile's named-object
(`D3DKMTOpenNtHandleFromName`), `OPAQUE_WIN32`, `identity.resource_id`, and
`vn_device_memory_import_resource_id` fallbacks. The name helper remains a
documented optional precursor for named objects, but no named object is created
or admitted by this package. Remove `tools/d3d11_kmt_shared_probe.cpp`; the
release gate exercises the selected import as conformance, not as ABI discovery.

The layer creates one D3D12 allocator/list and one lower-Vulkan command pool
with two barrier buffers per slot, and resets them only after exact Release
completion. Its private helper queue has one layer-owned mutex; no app-visible
queue is shared or globally locked. Keep the ICD free of D3D/DXGI imports. All withheld WSI
extensions, formats, usages, transforms, modes, and flags must fail at query or
creation rather than select conversion/scaling/tearing code. Record-only
dispatch must cover
`vn_QueueSubmit`, `vn_QueueSubmit2`, and `vn_QueueBindSparse`
(`vn_queue.c:2014,2174,2445`) and make the ordinary `vn_QueuePresentKHR`
(`vn_wsi.c:1098`) unavailable to translator instances; no auxiliary renderer
worker may bypass that policy.

For every record-only `vn_instance`,
`vn_helios_translation_session.c` creates one raw KMT device and HVC1 control
context, performs finite INIT, and returns the HTS1 generation/capability and
physical endpoint descriptors only through the private direct table to its
own translator/UMD. It implements the generated C60 allowlist across
`vn_device.c`, `vn_buffer.c`, `vn_image.c`, `vn_pipeline.c`,
`vn_query_pool.c`, and `vn_queue.c`: pure bounded calls use control; allocation-
backed calls create deferred records consumed by the exact outer batch; and
GPU-dependent calls invoke the owning outer-scope HQC1 join before a bounded
control fetch. A record-only instance creates no raw HVC1 GPU queue context.
Two instances in one process must create two sessions/host contexts; endpoint
and ring indices are unique only within their owning session.

The same files implement a distinct **normal-loader** execution mode. Replace
the current probe-only `D3DKMTCreateDevice`/`D3DKMTCreateContext` and private
Escape CTX/SUBMIT/MAP path in `vn_renderer_helios.c` with the section-10.7
legacy lane: one raw KMT device and one HTS1/host Venus namespace per
`vn_instance`, one HVC1 control context on
CPU/decode-only host ring 0, one HVC1 nonvirtual KMT context/nonzero GPU ring plus one unshared
monitored progress fence per HNR2-executing control/queue context; HNR2 finite-stream fragmentation
and exact COMMIT allocation-use/typed-operand construction in `vn_queue.c`;
`D3DKMTRender` buffer adoption/splitting/failure rules; a generated opcode
classifier that makes every GPU/queue-dependent command illegal on control;
same-context imported native-fence wait/signal placement; and explicit
per-nonzero-queue joins for queue/device idle and teardown. `vn_device.c` owns
device/queue/context/ring generation and drain order; a ring index is not
recycled before host device destruction. `vn_device_memory.c` and renderer
storage replace MAP_BLOB with ordinary unshared HVM1 allocations and
`D3DKMTCreateAllocation2` (`pSystemMem=NULL`). Expose only C54's one
device-local and one WC host-visible ordinary memory type. Every ordinary
non-import allocation selects the matching HVM1 role; buffer/image binding
records its exact allocation/offset/range, device-local memory never maps, and
only host-visible roles use `D3DKMTLock2`/`Unlock2`. Carry those exact bindings
into HNR2 COMMIT allocation references and the C53 output patch-list buffer.
The normal-loader physical-device profile clamps descriptor-indexing and every
related resource limit so the generated maximum allocation closure of one
indivisible operation is at most 4096 and direct typed occurrences at most
8192. It creates/requests 4096 allocation/output-patch entries, uses the
documented KMT next-list resize flags when a returned list is smaller, and
never advertises an unbounded binding feature that this legacy lane cannot
represent. The private record-only profile is separately allowed to expose
the Tier-3 features vkd3d needs because C67 executes it through the outer
D3D12 GPUVA/page-table/object-graph path.
Implement the C57 carrier exactly: `D3DKMTQueryResourceInfoFromNtHandle` then
`D3DKMTOpenResourceFromNtHandle` on the ICD's own KMT device, retaining the
returned `hAllocation` and per-allocation private driver data. Record and
cross-check `GpuVirtualAddress` when nonzero, but do not require it in the
legacy Render/Patch lane. Close the import with
`D3DKMTDestroyAllocation2{hResource,phAllocationList=NULL,AllocationCount=0,
Flags=0}`. The existing
`vn_renderer_helios.c` open path already has this shape; correct it to size
both arrays from the query, to treat a zeroed allocation array as a hard
import failure, and to gate `D3D12_RESOURCE_BIT` advertisement on that check.

The Windows normal-loader path in `vn_ring.c` is replaced by direct HNR2
dispatch: remove `vkCreateRingMESA`, shared head/tail/status storage,
`vkNotifyRingMESA`, `vkWaitRingSeqnoMESA`, `vkWaitVirtqueueSeqnoMESA`,
spin/sleep watchdogs, and every shared-ring reply path. Preserve generic
upstream ring code only for non-Helios platform builds. Refactor
`vn_renderer_shmem`/`vn_renderer_bo` so Windows code stores an opaque local
HVM1 allocation capability/generation rather than `res_id`; generated Venus
encoders write zero host-resource placeholders plus typed HNR2 operand records.
KMD Render creates one real WDDM output patch/capability slot per allocation use;
Mesa never supplies or observes the later physical address. Audit
`vn_device_memory.c`, `vn_wsi.c`, every generated shmem/resource descriptor,
and `vn_renderer_helios.c`: no actual virtio `resource_id` may reach user-mode
storage or protocol state.

Reply storage is exactly one 64-MiB HVM1 allocation per HTS1 session, four
16-MiB slots, one HVR1 header and at most 15 MiB of payload per transaction,
and at most four immutable 64-MiB host snapshots/256 MiB total. There is no
growth or spill path. Finite COMMIT contains
`SetReplyCommandStreamMESA + GENERATE_REPLY` and waits through C51 only when the
Vulkan call requires CPU output. Generated continuation requests carry the
exact snapshot generation/next offset and never recompute the result. Preserve
pipeline-cache `VK_INCOMPLETE`, query `WAIT`/`VK_NOT_READY`, and every other
generated decoder semantic. Every normal-
loader worker/queue entry uses its owning context; translator record-only mode
never creates one. Remove every private Escape call/import/ABI reference from
the shipped ICD. There is no `D3DKMTCreateContextVirtual`,
`D3DKMTSubmitCommand`, hidden IOCTL/service path, shared ring, polling, raw
host resource ID, or fallback between modes.

### 17.4 D3D11 UMD

Modify:

- `umd/bridge/dxvk_bridge.cpp`,
  `umd/bridge/dxvk_bridge.h`
- `umd/src/bridge.rs`
- `umd/src/forward.rs`
- `umd/src/forward/present.rs`
- `umd/src/forward/transfer.rs`, `umd/src/forward/tables.rs`
- `umd/src/forward/vehicle.rs` — delete vehicle exports/implementation
- `umd/src/forward/snapshot.rs` — delete the raw-`resid` snapshot transport
- `umd/src/vehicle_exports.rs` — delete
- `umd/src/forward/alloc.rs`, `umd/src/forward/resource.rs`,
  `umd/src/forward/state.rs`, `umd/src/forward/state_objects.rs`
- `umd/src/device_funcs.rs`
- `umd/src/scanout_acquire.rs` — delete module
- `umd/src/adapter.rs`, `umd/src/knobs.rs`
- `umd/src/lib.rs`
- `umd/build.rs`
- `umd/bridge/bridge_icd_exports.cpp`,
  `umd/bridge/bridge_icd_exports.h`
- `umd_common/bridge/bridge_icd_anchor.cpp`,
  `umd_common/bridge/bridge_icd_anchor.h`, and related common bridge code

Remove `helios_umd_set_present_source`,
`helios_umd_wait_last_present`, HPS publish/release, scanout ledger exports,
and every raw-`resid` lookup/discovery seam. Replace UMD-driven backing
adoption with KMD-created backing plus the versioned create/open allocation
descriptor. Add actual-batch command assembly, exact allocation references,
direct ICD table injection, and failure propagation. In
`umd/src/forward/present.rs` and the DXGI table setup, replace the current
16-plane/16x/BILINEAR/SHARED/IMMEDIATE MPO claims with the exact one-primary
profile in section 10.8; accept/forward only that profile and make every UMD
MPO capability and Present result consistent with the KMD's C41 surface. In
`forward/transfer.rs`, replace the current unconditional Direct-Flip refusal
with the C43-C44 fail-closed exact app/DWM resource-pair comparison: equal
concrete D3D11 sources or a cross-validated D3D12 runtime-primary sentinel on
the same adapter/LUID, plus the complete matching format/layout profile. Keep
`forward/tables.rs` wired to that callback. Do not compare or invent a source
ID for the sentinel branch. No validation may call `pfnEscapeCb`; unprovable
compatibility remains false, and this callback stays false until the KMD's
classic SetVidPn validator/lease path is in the same package.

`adapter.rs`, `device_funcs.rs`, `forward.rs`, the generated callback/table
bindings, and bridge initialization also perform the selected D3D11
WDDM-2.1+ uplift. They must advertise the complete negotiated table size and
implement/audit every newly live slot, including Acquire/ReleaseResource; the
physical Render/Present lane remains actual work. The bridge creates its DXVK
HTS1 session before `pfnCreateContextCb`, selects the immediate physical lower
queue endpoint, fills HQA1 as the complete context PDD, and creates one private
HQC1 monitored fence through the runtime callbacks. Add an exact direct-table
operation for C60 synchronous progress: flush/seal actual work, FromGpu-signal
HQC1 on that physical context, then one FromCpu event wait with no UMD/DXVK/
Mesa/session lock held. Missing modern callback, late session creation,
rejected HQA1, or inability to retain physical Render under the negotiated
table fails device creation; no WDDM-1.3, polling, direct-KMT-context cast, or
ring-zero fallback ships.

The D3D11 generator computes the complete transitive allocation closure for
each physical batch, including all six-stage descriptor/resource bindings and
translator-internal fixed uses. A generated compile-time inequality proves one
indivisible exposed operation fits 4096 unique uses/8192 typed occurrences;
the UMD requests 4096 runtime allocation/output-patch entries and splits only
between complete operations. No 256-entry legacy assumption, truncation, or
raw-object fallback remains.

### 17.5 vkd3d-proton and D3D12 UMD

Modify vkd3d:

- `vkd3d-proton-helios/libs/d3d12core/helios_entry.c`
- `vkd3d-proton-helios/libs/vkd3d/command.c`
- `vkd3d-proton-helios/libs/vkd3d/device.c`
- `vkd3d-proton-helios/libs/vkd3d/d3dkmt.c`
- `vkd3d-proton-helios/libs/vkd3d/shared_metadata.c`
- `vkd3d-proton-helios/libs/vkd3d/resource.c`
- `vkd3d-proton-helios/libs/vkd3d/heap.c`
- `vkd3d-proton-helios/libs/vkd3d/memory.c`
- `vkd3d-proton-helios/libs/vkd3d/vulkan_procs.h`
- `vkd3d-proton-helios/libs/vkd3d/vkd3d_private.h`
- `vkd3d-proton-helios/libs/vkd3d/meson.build`
- `vkd3d-proton-helios/include/private/vkd3d_d3dkmt.h`
- relevant public/private headers and build lists that declare the Helios
  create-device/queue interface

Modify UMD12:

- `umd12/bridge/vkd3d_bridge.cpp`,
  `umd12/bridge/vkd3d_bridge.h`
- `umd12/src/bridge12.rs`
- `umd12/src/forward12/identity12.rs` — delete the process-global
  engine-pointer/raw-Venus-ID registry; exact per-resource WDDM state replaces
  it
- `umd12/src/forward12/queue.rs`
- new `umd12/src/forward12/command_pool12.rs` — C65's fixed GPUVA allocation,
  extent allocator/seal metadata, per-owner HQC1 retirement, event-backed
  backpressure, poisoning, and teardown; export it from `forward12/mod.rs`
- `umd12/src/forward12/fence.rs` — replace engine-shadow correctness with
  Core-0116 `NATIVE`/`OPENED_NATIVE` state, `HRTFENCE`, local
  `hSyncObject`/mappings, and fail-closed destruction
- `umd12/src/forward12/resource12.rs` — preserve/correct the existing
  create-time `pfnAllocateCb` plus D3D12-to-D3D11 private-data path; implement
  `pfnOpenHeapAndResource` for the reverse D3D11-to-D3D12 direction
- `umd12/src/forward12/present12.rs` — ordinary Present with exact
  source/destination allocation and truthful fields; no Present-HWQueue path
- `umd12/src/forward12/misc.rs` — remove/refuse scheduling-group/HWS paths
- `umd12/src/caps12.rs` — negotiate Core 0116 and report only truthful native
  fence/version capabilities; expose one physical node and no LDA or
  cross-adapter resource/fence support
- `umd12/src/device12.rs` — retain `pKTCallbacks`, Core-0116 callbacks, and
  exact virtual-context handles
- `umd12/src/lib.rs`, `umd12/src/probe12.rs`, `umd12/build.rs`,
  `umd12/Cargo.toml` — remove/update stale vehicle and mutable-open-identity
  declarations while retaining the shared protocol dependency
- `umd12/bindgen/d3d12umddi_wrapper.h` and
  `umd12/bindgen/cached/d3d12umddi.rs` — regenerate from WDK 28000 and verify
  0112/0116 table sizes/offsets

Required result: one-to-one API queue/runtime virtual context, record-only
vkd3d/Mesa submit, actual batch to ordinary `pfnSubmitCommandCb`, Core-0116
native create/open association, exact-context runtime callback waits/signals,
both documented resource-open directions, and no lower execution queue. Audit
and eliminate every direct
lower-queue operation: ordinary/wait/signal/ECL/drain/interop submits in
`command.c:291,23670,23889-23899,24264-24272,24629,25612`; sparse binding in
`command.c:25099` and `resource.c:3796`; transfer clears in `memory.c:458`; and
the conditional Vulkan-swapchain submissions/present in
`swapchain.c:398,2375,2719,3112`. In record-only mode each becomes sealed outer
work, a C34 WDDM paging operation, or a capability refusal; the private Windows
UMD build must prove `swapchain.c`'s Vulkan WSI path unreachable.

During `vkd3d_create_device`, direct Mesa creates exactly one HTS1 session for
that `vn_instance` before any D3D12 command queue exists. `device.c` and
`command.c` expose a stable bounded physical-lower-queue endpoint for each
logical command queue. Several logical/API queues may map to one physical
endpoint, but no UMD sequence crosses their WDDM contexts and no duplicate host
ring index is allocated; C62's KMD endpoint lock serializes only
scheduler-eligible DMA in actual arrival order. In
`umd12/src/forward12/queue.rs`, replace the current legacy core
`pfnCreateContextCb` plus returned command/allocation/patch windows and
`pfnRenderCb` lane with the selected virtual-context path. Fill HQA1 **before**
the core `pfnCreateContextVirtualCb` call, validate attach success, retain the direct
session/endpoint ref in the queue object, and create one private HQC1 monitored
fence. Context creation requires the exact HOS1 UMD-prefix plus KMD-suffix
private-data size. ECL recording writes complete HOB1 to the resident command
GPUVA, fills exactly 64 HOS1 bytes, and preserves the same runtime-visible
resource/write use represented by the command lists before
`pfnSubmitCommandCb`. The runtime supplies ordinary runtime-primary
`WrittenPrimaries`; Helios emits one complete HOB1 per runtime command-list
association, consumes its supplied primary array unchanged, never merges two
associations, and adds only a UMD-created-primary exception.
Preserve C67's D3D12 binding model: HOB1 enumerates only direct
allocation/GPUVA operands, never one entry per potentially indexed Tier-3
descriptor. Descriptor creation/copy and Venus object creation maintain one
HTS1-local generation-checked object graph; the submitted command retains the
descriptor heap/version and Vulkan objects, while runtime/application
residency and lifetime plus C64 page tables govern the underlying allocations.
No submission scans a million-entry heap or substitutes a host resource ID.
Allocate/map one exact C65 GPUVA pool per D3D12 device through the runtime
callbacks. Track each sealed extent by queue/context generation, batch ID, and
post-submit HQC1 value; implement one completion check plus event-backed
oldest-owner backpressure, with no overwrite, UMD eviction, pool-lock wait, or
spill allocation. Poison the entire generation on signal/reset/removal and
drain every extent before pool/context teardown.
`bridge12.rs` exposes only bounded pure-control and HQC1-join callbacks
to the record-only Mesa instance. A GPU-dependent synchronous operation seals
pending ECL work, signals HQC1 from the exact virtual context, event-waits,
then permits a bounded HVC1 fetch; it cannot call a lower queue wait/idle.
Session, endpoint, HQA1, and HQC1 teardown is added to every queue/device/error
path. No host/session capability is stored in Render/Submit PDD or a resource.

Delete vkd3d's `D3DKMT_ESCAPE_UPDATE_RESOURCE_WINE` update in
`d3dkmt.c:145-195`, the `\\.\SharedGpuResource` IOCTL implementation in
`shared_metadata.c:24-65`, and its set/get/open callers and declarations in
`device.c:7682,7797` and `vkd3d_private.h:7166-7168`. Shared D3D12 resources
use only `CreateSharedHandle`/ordinary documented WDDM resource open plus the
immutable create-time HWA2 descriptor. Unsupported shapes fail before handle
export/open; no Wine metadata, Escape, filename/device, object name, or
descriptor inference remains as fallback.

### 17.6 KMD and logic

Delete:

- `kmd_render/src/ddi/escape.rs`
- `kmd_render/src/ddi/blob_map.rs`
- `kmd_render/src/seh_shim.c`

Modify:

- `kmd_render/src/ddi/wddm_surface.rs`
- `kmd_render/src/ddi/scheduler.rs`
- `kmd_render/src/ddi/query_adapter_info.rs`
- `kmd_render/src/ddi/bar_segment.rs`
- `kmd_render/src/ddi/segment_table.rs`
- `kmd_render/src/ddi/create_allocation.rs`
- `kmd_render/src/ddi/build_paging_buffer.rs`
- `kmd_render/src/ddi/cpu_host_aperture.rs` — delete; the selected segment does
  not advertise or register CPU Host Aperture callbacks
- `kmd_render/src/ddi/gpummu.rs`
- `kmd_render/src/ddi/submit_command.rs`
- `kmd_render/src/ddi/display.rs`
- `kmd_render/src/ddi/present_packet.rs`
- `kmd_render/src/ddi/interrupt.rs`
- `kmd_render/src/ddi/lifecycle.rs`
- `kmd_render/src/ddi/scanout_trace.rs`
- `kmd_render/src/ddi/scanout_timeline.rs`
- `kmd_render/src/adapter/mod.rs`
- `kmd_render/src/adapter/locks.rs`
- `kmd_render/src/adapter/scanout.rs`
- `kmd_render/src/adapter/segments.rs`
- `kmd_render/src/adapter/read_ledger.rs` — delete
- `kmd_render/src/device.rs`
- `kmd_render/src/diag.rs`
- `kmd_render/src/mapping.rs`
- `kmd_render/src/ddi/mod.rs`
- `kmd_render/build.rs`
- `kmd_render/Cargo.toml`
- `kmd_render/src/virtio/gpu/mod.rs`
- `kmd_render/src/virtio/gpu/resource_tables.rs`
- `kmd_render/src/virtio/pci_caps.rs`
- `kmd_render/src/virtio/ctrl.rs`
- `kmd_render/src/virtio/counters.rs`
- `kmd_render/src/virtio/venus/bringup.rs`
- `kmd_render/src/virtio/venus/ring.rs`
- `kmd_render/src/virtio/venus/protocol.rs`
- `kmd_render/src/virtio/venus/scanout.rs`
- `kmd_render/src/lib.rs`
- `kmd_render/src/irql.rs`
- `kmd_render/helios_kmd_render.inx`
- `kmd_logic/src/lib.rs` and exact batch/queue/flip tests

Add `kmd_render/src/ddi/native_render.rs`,
`kmd_render/src/ddi/native_fence.rs`,
`kmd_render/src/ddi/translation_session.rs`,
`kmd_render/src/ddi/native_linear_memory.rs`,
`kmd_render/src/ddi/native_physical_memory.rs`,
`kmd_render/src/adapter/physical_memory.rs`,
`kmd_render/src/ddi/mpo3.rs`, `kmd_render/src/ddi/direct_scanout.rs`, and
`kmd_render/src/ddi/diag_etw.rs`, plus
`kmd_render/src/render_user_copy.c`; modify
`kmd_render/src/ddi/scanout_timeline.rs` into bounded ETW event construction
with no userspace cursor/read interface, and modify `kmd_render/build.rs`,
`kmd_render/src/ddi/mod.rs`, and `kmd_render/src/lib.rs` for the complete WDDM
3.2 native-fence DDI table: query caps/feature admission, create/open,
close/destroy, current/monitored-value updates, and native-fence interrupt
reporting. Driver global/local handles reference bounded native-fence objects
directly; there is no hash table, scan, name, or process-global discovery
registry.

`device.rs` and `translation_session.rs` implement C58-C64 explicitly.
`DxgkDdiCreateProcess` allocates one bounded ProcessContext session list;
`DxgkDdiCreateDevice` records the exact supplied process-object pointer. HVC1
control creation makes a provisional per-`vn_instance` HTS1 session, finite
INIT creates exactly one host Venus context/VkInstance and returns a fresh
generation/capability, and HVC1 normal queue contexts on that raw device take
direct endpoint refs. D3D11/D3D12 context create parses HQA1 only from its
documented PDD, requires pointer-identical ProcessContext plus exact adapter/
package/session/endpoint, then stores a direct strong session/endpoint ref in
ContextContext. Render PDD stays reserved-zero and later submit/Present never
searches the session list. Each physical endpoint owns one bounded
host-dispatch FIFO/nonzero ring shared only by explicitly attached outer
contexts; at SubmitCommand it assigns arrival-order serials under a short lock,
never waits for an unsubmitted context, releases the lock before host/GPU
completion, and rejects capacity exhaustion. C60's generated pure-control
allowlist is enforced before host decode. The raw device creates the one exact
64-MiB role-1 HVM1 pool before INIT; control Render accepts only one checked-out
slot from that session-owned allocation, validates the complete continuation
tuple, and rejects any other allocation/range. Any outer-allocation operand or
GPU-dependent operation on control poisons the session. Standard monitored-fence DDIs/callbacks implement C51 and
outer HQC1; reset invalidates capabilities before waking waiters and final
session destruction waits for raw/outer context, endpoint, C51/HQC1, host job,
and reply references. Multiple sessions in one ProcessContext always create
different host contexts; no resource identity is stored in that list.

Independently implement HVC1 control/queue legacy nonvirtual context
create/destroy, bounded HNR2
fragment assembly and typed operand validation in `DxgkDdiRender`, one real
output WDDM patch/capability slot per use, repeatable side-effect-free
`DxgkDdiPatch` in `submit_command.rs`, the context-local pool, physical
`DxgkDdiSubmitCommand`, one HPM1-validated fenced finite
`SUBMIT_3D` per COMMIT, ring-0 CPU/decode opcode enforcement, nonzero
per-queue GPU timelines, ordered one-engine retirement, ring-0 processing/
reply-gated control completion, nonzero-host-fence-gated queue completion, and
explicit all-queue joins before device idle/reset/teardown for the normal
Vulkan ICD; it is never mixed with the D3D GPUVA path. Retain
traditional Create/DestroyContextVirtual and SubmitCommandVirtual for the D3D
runtime contexts. For an HQA1 D3D12 context, context creation reports a fixed
private-data buffer large enough for exactly 64 UMD HOS1 bytes plus the KMD
suffix. SubmitCommandVirtual accepts only that exact UMD prefix, validates it
against the direct session/context/endpoint reference, and nonblocking-enqueues
the exact process/page-table generation plus `DmaBufferVirtualAddress`/size;
it neither CPU-dereferences the GPUVA nor copies a variable command from HOS1.
The HPM1 device path walks the current authoritative process page tables,
reads/validates complete HOB1, resolves all type-2 GPUVA uses, executes one
fenced host submission, and only then permits truthful exact-host DMA
completion. Continue KMD-created
per-allocation host backing, immutable generation/layout descriptor write-back
at create, exact flip/MPO current-plane plus backend-reader lifetime, and
generation-safe teardown. `DxgkDdiOpenAllocation` only reads private data on
an ordinary open. Do not advertise HWS/HWQueue or hardware-flip-queue features
in this generation. Delete UMD-provided backing adoption, open-time
private-data restamping, timeout-rebase early completion, and every
reader-ledger/Escape ownership path.

Deleting the Escape mapper also removes `device.rs`'s user-MDL teardown,
`ddi/mod.rs`'s blob/Escape exports, `build.rs::compile_seh_shim`, and every
MAP_BLOB field. Retain the `cc` build dependency solely to compile the new C53
`render_user_copy.c` helper, and update its comment/build rule so no Escape/user-
mapping symbol survives. The helper only bounds-checks, probes, and copies the
`DxgkDdiRender` user command under `__try/__except`; it exposes no mapping or
long-lived address. Move only
the pure virtio cache-enum conversion still used by
`build_paging_buffer.rs:576` into `mapping.rs`; it performs no user mapping.
Register no `DxgkDdiEscape` implementation in the WDDM-3.2 initialization
table. Replace adapter-global Venus bring-up/ring state with one object
namespace per HTS1/`vn_instance` session; a KMD ProcessContext may own several
bounded sessions and no resource table. ⛔ SUPERSEDED BY F5: HPM1 IS DECLINED —
ignore "negotiate HPM1" and the `READY` service state here. Before exposing
either segment StartDevice still validates/reserves the complete host-visible
linear BAR and initializes fixed bounded placement/epoch state, guest-side, with
no host protocol. QuerySegment4 then exposes exactly aperture id 1 for paging DMA and
HLM1 memory id 2 with the flags/base/length in section 10.7. HVM1 allocation
creation owns a same-sized renderer view of authoritative HPM1 bytes and reports
only those exact local/system placement constraints. Delete
`cpu_host_aperture.rs`, its initialization/table registrations, both
Map/UnmapCpuHostAperture callbacks, all whole-allocation/page-array/raised-IRQL
logic, and the standard whole-blob mapper; no replacement map callback or HAP
vendor opcode exists.
`create_allocation.rs` also admits the distinct C65 HOC1 device-associated
allocation only from `pfnAllocateCb{hResource=NULL}`: exactly 64 MiB,
CPU-visible/WC, HLM1 preferred plus ordinary system placement, node 0,
nonprimary/nonshared, no renderer view, no HAP, and no `AccessedPhysically`
claim. It writes one allocation generation into HOC1 and lets C64/HPM1 map its
pages for virtual execution. Every other size/flag/cache/node/role combination
fails allocation.
`build_paging_buffer.rs` and `native_physical_memory.rs` must encode actual
bounded HPM1 paging DMA for every reachable transfer/transfer2, fill/fill2 and
discard operation; replace the current decorative `SetRootPageTable` and
diagnostic UpdatePageTable handling with C64's authoritative per-process root,
PTE-copy/update, TLB, local/aperture map/unmap, context-resource, and residency
packets; copy each PTE/MDL/ADL run before return; use only `MultipassOffset` for continuation; and complete the
paging SubmissionFenceId only after the QEMU copy/bind/fill/discard terminal
acknowledgement. `submit_command.rs` must distinguish paging from render DMA,
validate the final physical-page snapshot/epoch, and never execute paging work
inside BuildPagingBuffer or Patch. HNR2 parsing must validate every fragment/
range/opcode/use/operand/access/generation and generate the capability/output-
patch table. The pool limits, generation transitions, ordered host completion,
reset, and stale-callback rules are required behavior, not diagnostics.

`pci_caps.rs` must discover the exact guest-physical base and non-padded length
of the realized prefetchable 64-bit host-visible BAR. HPM1 negotiation returns
the same full capacity/page shift. StartDevice requires those values to equal
the later HLM1 descriptor and refuses a missing, short, overlapping,
overflowing, or mismatched capability. `adapter/segments.rs`, `bar_segment.rs`,
`segment_table.rs`, and `adapter/physical_memory.rs` own immutable segment/BAR
sizing, physical-placement ownership/epochs, and ordering;
`resource_tables.rs` owns only allocation renderer views/generations.
`query_adapter_info.rs` must describe id 1 as the sole aperture/paging-buffer
segment and HLM1 id 2, only after HPM1 is ready, as a memory segment with
`Aperture=CacheCoherent=SupportsCpuHostAperture=
SupportsCachedCpuHostAperture=0`, `CpuVisible=1`, and
`CpuTranslatedAddress=BAR base`; it must never put id 2 in
`DmaBufferSegmentSet`. `create_allocation.rs` returns every HVM1 role with
`AccessedPhysically=1` and `ExplicitResidencyNotification=1`; host-visible
roles additionally return `CpuVisible=1,Cached=0`, while device-local role 4
returns `CpuVisible=0,Cached=0`. All use Flags2
`DisablePartialResidency|RestrictedToSingleSegment`, and the exact selected
placement sets; no ExistingSysMem/PermanentSysMem/sharing flag. Qualification
must cold-start this precise two-segment profile, prove device-local
create/bind/use/relocate/destroy without any CPU mapping, prove host-visible
Lock2 returns the direct WC/uncached HLM1 mapping (or byte-identical VidMm
system mapping after eviction), and observe zero CPU-host-aperture callback
registration/invocation.

The WDDM-3.2 uplift is an explicit slot-and-cap audit, not a version-constant
change. Generate the full WDK-28000 `DRIVER_INITIALIZATION_DATA` layout and
classify every slot through 3.2 as implemented or disabled behind a truthful
zero capability. The selected active-display surface registers and implements:

- `DxgkDdiCheckMultiPlaneOverlaySupport3` and
  `DxgkDdiSetVidPnSourceAddressWithMultiPlaneOverlay3` with the exact
  one-primary validation/binding rules in section 10.8;
- `DxgkDdiSetVidPnSourceAddress` with the same shared
  `validate_direct_scanout_binding` routine, exact OS `hAllocation` and source,
  bounded nonpageable/DIRQL-safe candidate retention, and the identical
  candidate/current/backend-release state machine; a post-admission mismatch
  fails before latch and is never called a seamless fallback;
- `DxgkDdiGetMultiPlaneOverlayCaps` and
  `DxgkDdiGetPostCompositionCaps` with the one-plane/no-post-transform values;
- `DxgkDdiValidateUpdateAllocationProperty`, which validates the supplied
  live allocation and either performs only an implemented legal update or
  rejects it without mutating display identity;
- `DxgkDdiControlModeBehavior`, which marks every unsupported requested mode
  behavior `NotSatisfied` and never claims a hidden transform; and
- the existing `DxgkDdiUpdateMonitorLinkInfo`, revalidated under the 3.2 table.

The same generated table registers `DxgkDdiControlEtwLogging`,
`DxgkDdiCollectDbgInfo`, WDDM-3.2 `DxgkDdiCollectDbgInfo2`, and
`DxgkDdiCollectDiagnosticInfo`. At PASSIVE driver initialization,
`diag_etw.rs` calls `EtwRegister` for the section-12.3 GUID and retains its
`REGHANDLE`; the control callback changes only the atomic enabled bit/level,
validates its currently undefined `Flags` as zero, and provider keywords remain
owned by `EtwProviderEnabled`.
Each enabled event uses `EtwProviderEnabled` and one bounded nonpaged
`EtwWrite` payload. Stop/unload disables event sites, drains their rundown, and
calls `EtwUnregister`. The OS-requested diagnostic callbacks snapshot bounded
state into the supplied buffers; none opens a custom user handle or exposes a
callable private query. ETW loss changes no behavior. The generated
initialization table leaves `DxgkDdiEscape=NULL`; no Escape entry point is
present in the selected KMD.

Register `DxgkDdiPostMultiPlaneOverlayPresent` as an always-success, bounded
diagnostic callback, and never set the VSync-notification `PostPresentNeeded`,
plane `PostPresentNeeded`, or Hsync-completion flags. The generated table/flag
test enforces that invariant. `DxgkDdiPresent` gains the separately decoded and bounded
`pPresentMultiPlaneOverlayInfo` arm. Older MPO/2 slots, HWS, hardware flip
queue, post-composition transforms, protected display, CASO, LDA, and
cross-adapter scanout stay cap-disabled and may not become reachable merely
because their WDK fields exist.

### 17.7 Host/QEMU, packaging, and docs

Modify:

- `qemu-helios/hw/display/virtio-gpu-virgl.c`
- `qemu-helios/hw/display/virtio-gpu.c`
- `qemu-helios/hw/display/virtio-gpu-gl.c`
- `qemu-helios/include/hw/virtio/virtio-gpu.h`
- `qemu-helios/include/standard-headers/linux/virtio_gpu.h` plus a generated
  Helios vendor-protocol header for the negotiated HPM1 paging DMA and HLM1 BAR
  admission fields/opcodes
- `qemu-helios/hw/display/trace-events`
- `qemu-helios` is read-only for retirement; no display-listener callback,
  vendor plane record, trace event, or custom acknowledgement is added; KMD uses
  standard fenced `SET_SCANOUT_BLOB` plus its permanent parking/black blob
- `packaging/windows/Install-Helios.ps1`
- `packaging/windows/Install-Helios.cmd`
- `tools/read_ledger_dump.c` — delete
- `tools/scanout_timeline_dump.c` — delete; capture the C42 ETW provider with
  standard ETW tooling instead of querying a private KMD ring
- delete `tools/blob_capacity_probe.c`, `tools/blob_map_size_probe.c`,
  `tools/escape_owner_probe.c`, `tools/d3d11_shared_blob_truth_probe.cpp`,
  `tools/d3dkmt_alloc_probe.c`, `tools/d3dkmt_sync_probe.cpp`,
  `tools/vehicle_flipwait_probe.c`, `tools/vidmm_tracking_probe.c`,
  `tools/d3d11_kmt_shared_probe.cpp`, and `tools/vk_ring_fence_probe.cpp`;
  none has a compatibility or C57-admission role
- add `tools/native_fence_context_probe.cpp`,
  `tools/wddm_batch_completion_probe.cpp`,
  `tools/wddm_physical_paging_probe.cpp`,
  `tools/wddm_linear_bar_lock_probe.cpp`,
  `tools/finite_venus_reply_probe.cpp`,
  `tools/translation_session_attach_probe.cpp`,
  `tools/direct_flip_mpo_profile_probe.cpp`, and
  `tools/helios_etw_capture.ps1`. The ETW tool enables the fixed section-12.3
  provider GUID and decodes only its versioned event IDs/payload; it sends no
  KMD request or acknowledgement. The probes use only ordinary runtime/KMT
  objects, the OS display path, and C42 ETW; no new tool invokes a private
  Escape.
- `tools/window_burst_capture.ps1`
- `CONFORMANCE.md`
- `ROADMAP.md`, `DX12.md`, and `TRANSPORT.md`;
  `docs/dx12/PRESENT.md`, `docs/dx12/ARCHITECTURE.md`,
  `docs/dx12/DECISIONS.md`, `docs/dx12/KMD_IMPACT.md`,
  `docs/dx12/PENDING.md`, `docs/dx12/GATES.md`,
  `docs/dx12/SUBSTRATE.md`, `docs/dx12/PARALLEL.md`, and research snapshots
  `docs/dx12/research/R4-umd-template-and-split.md`,
  `R5-kmd-gap.md`, `R6-d3dkmt-surface.md`, `R7-present-swapchain.md`, and
  `R9-test-conformance.md`. The captured
  `docs/dx12/research/guest-vulkaninfo-full.txt` is labelled historical target
  evidence, not live behavior. Archive material remains immutable and is
  labelled historical rather than searched as live behavior.

QEMU remains unmodified. KMD requests standard fenced nonzero scanout replacement;
one exact successful response is both latch and prior-release boundary, with the
two ETW events emitted guest-side. For D3D12 virtual submits it consumes only
the KMD-originated process/address-space generation plus GPUVA/size descriptor,
walks C64's current HPM1 root/PTE/TLB state, reads complete HOB1, validates all
ranges/generations/typed operands, and substitutes renderer IDs only in a
host-private copy. HOS1 is never sent as a command. A stale/unmapped PTE,
cross-process page, checksum mismatch, or page-table generation change fails
before renderer dispatch and can never advance SubmissionFenceId. During adapter StartDevice it negotiates
HPM1, validates the complete linear host-visible BAR, and initializes bounded
physical-placement/renderer-view state before KMD exposes the segments. The PCI/VGA
transport realization in `virtio-gpu-pci.c:29-58` and
`virtio-vga.c:101-153` remains the source of the exact prefetchable 64-bit BAR
and `VIRTIO_GPU_SHM_ID_HOST_VISIBLE` capability; `virtio-gpu-gl.c:145-160`
owns the underlying linear BAR mapping/background and therefore must
participate in HLM1 initialization and drained teardown. HPM1 owns the bounded
placement/renderer-view state. No table may infer ownership from a user-mode
resource ID or resize beyond negotiated limits after StartDevice.

For each HPM1 paging packet QEMU validates package/adapter/allocation
generation, operation, segment/ADL page runs, range, and prior epoch; performs
the requested copy/fill/discard/resource-view bind; retains source, destination,
ordinary-aperture/local-placement and host-job refs; publishes the new
placement epoch only after bytes are
ready; and returns a terminal acknowledgement that alone permits KMD's paging
DMA completion. It accepts no logical-ADL profile in this generation and no
UMD address/resource ID. Local placement uses the exact HLM1 segment offset and
complete BAR bounds; system placement uses only the exact OS physical ADL/runs
encoded by paging DMA. Old local/ordinary-aperture mappings and host refs remain
until copy and queued users retire, after which QEMU performs any required
address-space/RCU revoke before acknowledging completion. Reset invalidates
adapter/backing generations and cannot acknowledge stale work. The old
whole-resource `RESOURCE_MAP_BLOB` placement, global overlap scan, retry/sleep,
CPU-host-aperture mapper, and asynchronous self-heal paths are not used for
HVM1 or paging.

The same host path accepts one finite HNR2 COMMIT plus its patched physical-
capability table, verifies every entry against HPM1, substitutes renderer IDs
only in a host-private copy, and issues one fenced `VIRTIO_GPU_CMD_SUBMIT_3D`.
It preserves one object namespace and exactly one `VkInstance` per HTS1
session/host context (not per process, outer queue, or arbitrary KMT device),
uses ring 0 only for CPU/decode control and unique nonzero GPU queue rings, and
reports the matching async `(ctx_id,ring_idx,fence_id)` completion only after
the timeline's actual contract is satisfied: command processing plus reply
publication for zero, VkQueue completion for nonzero. It rejects any generated
GPU opcode on zero and never reports a ring-0 event as device/queue idle.
Remove the 10-ms fence timer as correctness fallback. The
finite-reply admission probe must send `SetReplyCommandStreamMESA` plus a
harmless reply-producing command in one direct stream and validate its header,
token, size, and context fence; failure rejects this package generation and
never re-enables `vn_ring`.
QEMU tags every context/ring/fence callback with the owning session/package
generation and rejects duplicate nonzero ring bindings, a second instance in
one context, cross-session object use, stale KMD host-dispatch serial, or late
completion after reset. It exposes no session capability or renderer ID to
user mode; HQA1 admission remains wholly inside KMD.

Packaging installs the matching layer
DLL/JSON, ICD, both UMDs, KMD/INF, translator binaries, protocol and host
component as one generation. It removes creation/ACL management for the HPS
file and rejects incomplete/mismatched payloads.

### 17.8 Atomic activation and old-file retirement

1. Stop installing new legacy binaries.
2. Stage the complete signed guest generation without activating it, and stage
   the matching QEMU/display-backend generation on the host.
3. Verify hashes/generation of every staged payload before activation.
4. Shut the VM down completely. This terminates every guest process/DWM/UMD/
   ICD/KMD mapping **and** stops the old QEMU/display endpoint; a guest reboot
   or adapter restart alone is insufficient.
5. Start only the matching new QEMU/display endpoint, then boot once into the
   staged guest driver generation. Before KMD admits any allocation or command,
   the host and KMD mutually exchange and compare the exact protocol/package
   generation and required feature bitmap. Either mismatch fails adapter start
   without accepting work.
6. The guest installer verifies the active KMD, both UMDs, ICD, translators,
   WSI layer, manifests, and host-attested generation; no legacy DLL may load.
7. Confirm no legacy process can still map HPS2.
8. Only then remove
   `C:\ProgramData\Helios\helios_present_sync_v2.bin` and obsolete ACL/state
   setup. Deletion is an installer cleanup after quiescence, never startup
   behavior.
9. Never publish/read HPS2 from the new generation and never load a legacy
   binary as fallback.

## 18. Later implementation and runtime verification gates

### 18.1 Static/build gates

- every listed repository builds at the pinned implementation commits;
- WDK 28000 bindings expose Core 0116 exactly; generated Rust/C table sizes and
  offsets for `pfnCreateFence_0116`, `pfnCreateNativeFenceCb`,
  `pfnOpenNativeFenceCb`, `pKTCallbacks`, virtual contexts, and FromGpu
  wait/signal callbacks match the released headers;
- the generated WDK-28000 `DRIVER_INITIALIZATION_DATA` slot audit covers every
  field through WDDM 3.2; the C41 MPO3, capability, mode, allocation-update,
  monitor-link, and Present-union callbacks are non-null/consistent, while
  each cap-disabled versioned family is provably unreachable;
- imports show ICD has no D3D/DXGI and translators have no Vulkan-loader/layer
  dependency;
- a record-only translator cannot reach any lower `QueueSubmit{,2}`,
  `QueueBindSparse`, `QueuePresentKHR`, or queue-idle execution path; tests hit
  every callsite enumerated in sections 17.2, 17.3, and 17.5 and fail any call
  made without a live outer D3D operation scope;
- all HPS2 symbols/magic/path/fence-name/`present_fence_id`/`kwaitOrdered`
  searches are zero outside this historical reference and archived material;
- all `HELIOS_ESCAPE_*`, `QUERY_STATS`, `QUERY_SCANOUT_TIMELINE`,
  graphics/present/read-ledger private-Escape symbols, and private-Escape probe
  callsites are zero in the active package; `DxgkDdiEscape` is null in the KMD
  initialization table and no Escape object file is built;
- every **live outbound** `D3DKMTEscape` call/import is zero across shipped
  DXVK (including D3D9/D3D11/Win32 WSI), vkd3d, Mesa/Venus, UMD11/12, and tools;
  `D3DKMT_ESCAPE_UPDATE_RESOURCE_WINE`,
  `D3DKMT_ESCAPE_SET_PRESENT_RECT_WINE`, `\\.\SharedGpuResource`, and its
  OPEN/SET/GET private IOCTLs have zero active code/build/package hits. Released
  WDK declarations, this reference, archived documents, and the nonselected
  Gallium D3D10 UMD unsupported export are classified separately and cannot
  appear in an installed binary;
- `protocol/src/escape.rs`, `protocol/src/ioctl.rs`, KMD `ddi/escape.rs`,
  `ddi/blob_map.rs`, and `src/seh_shim.c` are absent from the build; no
  `compile_seh_shim`, user-MDL mapping field, or blob-map module/export remains.
  The sole C build is C53 `render_user_copy.c`; import/object scans prove it
  exports only the bounded Render-copy helper and no mapping/Escape symbol. The
  surviving pure cache-enum conversion has no user mapping or callable
  transport edge;
- HTS1 INIT/reply, HQA1, HOB1/HOS1, endpoint,
  HVC1/HNR2/HVM1/use/operand/output-patch/
  physical-capability and HPM1
  paging-DMA/BAR-admission sizes, offsets, endianness, flags, segment/ADL/
  physical-page rules, epochs, opcodes, and
  maxima are asserted identically in protocol, ICD, KMD, and QEMU as
  applicable. A normal-loader ICD creates only
  nonvirtual legacy KMT contexts and calls only `D3DKMTRender`; symbol/import
  and causal tests reject `D3DKMTCreateContextVirtual`,
  `D3DKMTSubmitCommand`, private IOCTL/service traffic, or a fallback transport
  in that mode. Each translator record-only instance creates exactly one raw
  HVC1 **control** session and no raw KMT GPU execution context; it can execute
  only the generated pure-control allowlist and can name only one checked-out
  slot of that session's exact 64-MiB role-1 HVM1 pool. Generated ABI tests
  assert the four-slot geometry, 15-MiB chunk ceiling, byte-exact HVR1,
  four-snapshot/64-MiB-each/256-MiB-session bounds, exact-next-offset
  continuation, C51-bound reuse/cancel, and absence of every reply-growth,
  recomputation, or spill path. Every outer D3D context has one
  byte-exact HQA1 create-context packet/direct session ref and one HQC1 object;
  Render PDD is zero and submit paths contain no capability lookup. Normal mode has no shared ring/head/tail, ring notify/wait,
  polling watchdog, or actual `resource_id` field in its user-mode renderer ABI;
- D3D11 bindings/table-size tests prove the selected WDDM-2.1+ callback table,
  physical CreateContext/Render path, FromGpu/FromCpu callbacks, and every
  newly live table slot coexist. A WDDM-1.3 advertisement, null callback, or
  unexpected virtual-only runtime path fails package qualification;
- HTS1 property tests create two `vn_instance` objects in one KMD process and
  prove two host contexts/capabilities/object namespaces, bounded capacity,
  no duplicate ring endpoint within either session, one-time HQA1 admission,
  direct context refs after attach, and no resource/session-table query from
  submit or Present. Wrong process/adapter/generation/capability/endpoint and
  all late Render-PDD attachments fail;
- the C42 ETW provider is enable/disable tested, emits bounded exact
  context/native-object/plane-generation events, and the OS-invoked
  `CollectDbgInfo{,2}`/`CollectDiagnosticInfo` callbacks return bounded valid
  snapshots without changing ordering or lifetime state;
- ETW tests prove one PASSIVE `EtwRegister`, a retained valid `REGHANDLE`,
  nonpaged one-descriptor version-1 payloads at elevated IRQL, loss-insensitive
  behavior, event-site rundown before one `EtwUnregister`, and byte-exact
  decoding of all section-12.3 IDs/flags/keywords;
- no raw `resid` or host backing token is accepted from a UMD/ICD, batch, or
  create/open private descriptor; KMD creates the renderer view, HPM1 placement
  comes only from actual WDDM paging DMA, and Render Submit resolves it only
  through the exact patched WDDM physical capability;
- protocol parsers prove bounds/checksum/generation/resource-table completeness,
  including strict HNR2 fragment order, one incomplete batch/context, COMMIT's
  exact one-to-one allocation/use/typed-operand/output-patch coverage, reply range/write access, and
  rejection of raw host objects or arbitrary byte patching. Every generated
  Venus resource-description path emits a typed operand rather than a host ID;
- HPM1 tests prove StartDevice physical-store/table readiness precedes
  QuerySegment4; actual transfer/transfer2/fill/fill2/discard paging DMA copies
  exact MDL/ADL runs, while SetRootPageTable plus update/copy-PTE/TLB operations
  publish exact process/address-space generations before virtual submit; uses
  MultipassOffset, then receives the mandatory
  infallible/no-size-change/side-effect-free paging Patch, and withholds the
  paging SubmissionFenceId until QEMU publishes the
  destination epoch. Local-local/system-local/local-system, partial/multipass,
  stale epoch, reset, failure, and concurrent Lock2/host-use tests prove source
  refs are never released early and no BuildPagingBuffer/Patch path mutates
  QEMU synchronously;
- HLM1 cold-admission tests prove the exact two-segment table: id 1 is the sole
  aperture/paging-buffer segment, id 2 is the complete linear memory BAR with
  `CpuVisible=1`, `CpuTranslatedAddress` equal to the BAR base, and
  `Aperture=CacheCoherent=SupportsCpuHostAperture=
  SupportsCachedCpuHostAperture=0`. `DmaBufferSegmentSet` never names id 2; the
  initialization table has no CPU-host-aperture callbacks, and any callback
  observation or Code 43 rejects the package;
- HVM1-specific tests additionally prove no `pSystemMem`/ExistingSysMem/
  sharing flag, the exact selected placement sets, and for every role
  `AccessedPhysically=1,ExplicitResidencyNotification=1` plus Flags2
  `DisablePartialResidency|RestrictedToSingleSegment`. Device-local role 4
  has `CpuVisible=0,Cached=0`, never reaches Lock2, and passes exact
  image/buffer allocate-bind-submit-relocate-destroy tests. Host-visible roles
  have `CpuVisible=1,Cached=0`; Lock2 returns an actual WC/uncached direct HLM1
  mapping or byte-identical VidMm system mapping. CPU publication/host
  completion ordering proves `HOST_VISIBLE|HOST_COHERENT` and never
  `HOST_CACHED`, otherwise native Vulkan admission fails;
- C57's carrier is selected from the published ABI and Microsoft implementation,
  so its target-conformance tests are mandatory acceptance
  tests rather than an ABI experiment. They validate the fail-closed admission
  check, wrong-adapter/format/count/generation rejection, and zero per-frame
  opens. The capability stays unadvertised on any admission-check failure;
- fuzz/property tests cover batch parsing, per-context submission completion,
  native-fence create/open/close/reset, plane candidate/current/backend-release,
  and WSI epoch state machines;
- D3D12 create-time private data round-trips every presentable format/plane
  layout into the D3D11 opener, ordinary `DxgkDdiOpenAllocation` never writes
  it, and reverse `pfnOpenHeapAndResource` interop also passes;
- HWA2 size/offset/flag/plane-bound fuzz tests pass in protocol, both UMDs, and
  KMD; the `D3D12_RUNTIME_PRIMARY` bit is accepted only with
  `PRIMARY` plus `D3DDDI_ID_UNINITIALIZED`. D3D11
  `CheckDirectFlipSupport` accepts equal concrete D3D11 sources or the exact
  same-adapter C44 sentinel branch, refuses immediate/stereo/protected/
  cross-adapter/mismatched formats or layouts, and is re-probed across DWM
  recreation and mode/fullscreen changes;
- Vulkan 1.3, timelineSemaphore, synchronization2, Win32 external extension,
  exact-format/resource and D3D12-fence IMPORTABLE tests pass; the copied
  device-create chain contains no duplicate feature structure and preserves
  every application request.

### 18.2 Queue and Present causal gates

For every normal-loader Vulkan `vn_instance` and each HNR2-executing context:

1. trace one raw KMT device and one distinct HTS1/host Venus object namespace,
   exactly one ordinary HVC1
   control context on host ring 0, exactly one HVC1 queue context with
   `VirtualAddressing=0` and one unique nonzero non-recycled host ring for each
   real queue, the declared legacy buffers/caps, and one distinct unshared
   monitored progress fence for the control context and every queue context;
2. send device initialization and a synchronous CPU reply-producing control
   command through ring 0; prove its optional milestone signals/waits only the
   control context's C51 object and means renderer processing/reply publication,
   not GPU completion. Hold real work on every nonzero queue, prove the ring-0
   value may retire without satisfying queue/device idle or GPU-dependent
   teardown, then release/join the exact newest C51 value on every affected
   nonzero queue before final control destruction. Suppressing any required
   object or injecting a GPU opcode on ring 0 fails closed;
3. submit both one-fragment and 64-fragment HNR2 through `D3DKMTRender`; prove
   only COMMIT emits output patches and one actual fenced `SUBMIT_3D`; invoke
   Patch twice with the same placement and prove byte-identical DMA/no host
   side effect, hold that host batch, and
   prove neither the exact scheduler `SubmissionFenceId` nor a following
   same-context progress value completes early;
4. release the host batch, prove reply publication precedes one ordered DMA-
   completed notification and then the optional progress signal; inject host
   rejection, timeout, reset, and a stale/wrong-ring callback and prove none
   advances either value;
5. verify COMMIT contains every referenced WDDM allocation exactly once with
   the correct read/write bit/generation and every encoded resource operand has
   one in-range typed operand plus one per-use output patch/capability slot;
   corrupt allocation/segment/physical-address/HPM epoch after Patch and prove
   Submit fails rather than repairing by host lookup; place each fragment across a guard/no-access page
   and corrupt order/range/checksum/generation/opcode/access/reply bounds,
   proving C53 returns the documented error without bugcheck, partial use, or
   host dispatch;
6. cross the 256-KiB/4096-use/8192-patch and 15-MiB/64-fragment boundaries,
   prove exact
   assembly or fail-closed API/device behavior, abort an incomplete batch, and
   saturate the 64-slot/15-MiB pool without global spill, scan, wait, or cross-
   queue assembler interference; separately submit an indivisible 257-allocation
   draw/dispatch, a D3D11 multi-stage draw above 256 uses, and Tier-3 dynamic
   descriptor indexing. Prove the first two carry the complete exact legacy
   closure, the Tier-3 path resolves the descriptor heap/object graph through
   C64/C67 without per-descriptor truncation, and queued object destruction or
   residency changes obey the API lifetime and HPM1 generation rules;
7. exercise two queue rings and two devices with out-of-order host completions:
   prove host execution remains independent, each context FIFO is legal, and
   the selected single WDDM engine holds later completions until its scheduler
   submission frontier advances rather than forging an out-of-order fence;
8. prove finite CPU milestone timeout retains its armed event/object until
   completion or loss, infinite waits block without polling, fire-and-forget
   batches create no progress signal, and source/binary scans contain no ring-
   head/status sampling or `resource_id` user ABI;
9. cold-start the exact `[aperture id1,HLM1 id2]` descriptor and prove id2's
   complete linear BAR bounds/offsets and flags, id1-only paging/DMA-buffer use,
   successful context creation, and zero CPU-host-aperture callback
   registration/invocation. Map an HVM1 allocation with Lock2 and prove its VA
   is actually WC/uncached, aliases the exact renderer bytes, and is withdrawn
   on unlock/reset/device death; any Code 43, cacheable mapping, split/short BAR,
   or callback observation fails package admission;
10. force local-to-system and system-to-local paging while unlocked and across a
   legal lock lifetime, plus a local-to-local relocation. Prove actual HPM1 DMA
   copies/fills/discards the exact OS runs, mandatory paging Patch is invoked and
   has no side effect/size change, destination epoch is invisible before device
   completion, old placement/host refs prevent reuse, and the Lock2 VA remains
   byte-identical while renderer/VidMm identity never diverges. Round-trip every
   generated reply-size family, including large pipeline-cache and query-result
   cases, and prove no fixed-slot truncation, polling, Escape, IOCTL, user MDL,
   file/name, HAP, or raw host-token operation; and
11. using C57's selected published carrier, import the same retained `S[i]` handle as
   the canonical image and two alias images. Prove exact local-allocation
   identity and lifetime: `NumAllocations` from the query matches the open,
   every returned `hAllocation` is nonzero, each per-allocation
   `pPrivateDriverData` lies inside the supplied
   `pTotalPrivateDriverDataBuffer` with the queried size, the returned
   any nonzero `GpuVirtualAddress` matches a binding that uses it, and a single
   `D3DKMTDestroyAllocation2` with `hResource`, a null allocation list, and
   zero flags releases the whole import. Then
   inject wrong LUID/node/format/layout/generation, multiple allocations,
   recycled handles, OPAQUE, and named-object paths, plus a deliberately
   zeroed allocation array, and prove each is refused with no VkImage or
   VkDeviceMemory exposed;

For every record-only DXVK/vkd3d Mesa `vn_instance`:

1. create its raw HVC1 control session before the dependent D3D runtime
   context; prove INIT creates exactly one host `VkInstance`, returns a fresh
   generation/capability, and a second translator instance in the same process
   creates a different session/host context rather than entering the first;
2. prove raw/runtime KMD devices have the identical ProcessContext and HQA1 is
   consumed only during outer context create. Inject wrong/stale package,
   process, adapter, session, capability, endpoint, flags, reserved bytes, and
   Render PDD; all fail without a host submit or resource association;
3. map two logical D3D queues to one physical endpoint and another queue to a
   second endpoint. Submit the first shared-endpoint queue while VidSch withholds
   the other, then reverse them: prove neither submitted batch waits for an
   absent UMD sequence, both enter the nonzero host ring in actual KMD arrival
   order, same-context FIFO and explicit native-fence dependencies remain
   correct, and the disjoint endpoint progresses independently. FIFO exhaustion
   and ring reuse before drain fail closed without CPU waiting;
4. exercise every generated C60 callsite. Pure create/requirements/compile
   requests return through one exact checked-out slot in the 64-MiB role-1
   HVM1 pool and finite ring-zero/C51 replies. Exercise four simultaneous slot
   owners, fifth-owner event backpressure, stale token/generation/range reject,
   15-MiB boundary, multi-chunk legal results, and an entry point with no legal
   continuation; prove no spill allocation, overwrite, polling, or false
   partial success. Outer-allocation-backed
   create/bind/map/destroy does not reach host until an actual outer batch
   names the exact allocation; query WAIT/queue idle/device idle first reaches
   the precise HQC1 value on every affected endpoint and only then performs a
   bounded control fetch. Hold the GPU batch and prove ring zero cannot release
   any GPU-dependent result;
5. reset/crash while INIT, HQA attach, endpoint work, HQC1 wait, and control
   reply are independently pending. Prove capability invalidation precedes
   device-lost wakeup, no later callback reaches the new generation, and host
   context/session destruction occurs only after every raw/outer/job/reference
   drains.

For each D3D11 and D3D12 queue:

1. hold one host batch before completion;
2. prove the exact WDDM `SubmissionFenceId` is not reported complete;
3. issue Present and prove DXGI/DWM/scanout does not consume the new pixels;
4. release the exact host batch;
5. prove DMA completion is reported once and Present then consumes it;
6. submit later batches out of host completion order and prove OS-visible
   submission completion remains in legal context order;
7. force timeout/reset and prove no success/completion is forged.
8. prove its context was HQA1-attached before first use and owns one private
   HQC1. Hold a synchronous internal query/idle request: pending actual work
   submits first, HQC1 signals only after that exact completion, one event wait
   returns, and ordinary render/Present never CPU-waits.

For D3D11 additionally, prove the runtime negotiated the full selected
WDDM-2.1+ table while continuing to call physical CreateContext/Render/Present;
exercise FromGpu signal/FromCpu event wait and all newly live slots, including
Acquire/ReleaseResource. Any WDDM-1.3 or incompatible callback/context shape
rejects the package before HPS2 retirement.

For D3D12 additionally:

- runtime negotiation proves Core 0116 and native-fence caps; every supported
  create/open trace is `NATIVE`/`OPENED_NATIVE`, while an injected legacy
  `MONITORED` branch fails loudly;
- ETW/PIX/driver counters prove every ECL batch uses the exact virtual context
  created for that command queue and ordinary `pfnSubmitCommandCb`; HOB1 is the
  complete command at the submitted GPUVA, exactly 64 HOS1 UMD-private bytes
  reach KMD, the runtime supplies the expected ordinary-primary
  `WrittenPrimaries`, and HOB1 preserves the same command-list resource/write
  association rather than redirecting the write;
- one ECL containing several runtime command-list associations produces one
  ordered HOB1 submit per association with the exact supplied primary array.
  Inject a merge whose union exceeds eight, a filtered/partitioned array, an
  over-eight single association, an invented handle, an omitted writer, or a
  command list exceeding one HOB1; each fails before SubmitCommandCb rather
  than creating a synthetic association;
- HOC1 creation traces prove one device-associated allocation only:
  `hResource=NULL`, `pSystemMem=NULL`, exact 64-MiB size/64-KiB alignment,
  CPU-visible/WC/node-0/nonshared flags, HLM1/system placement, one lifetime
  Lock2 mapping, one reserve/map GPUVA sequence, and no HVM1 renderer view or
  HAP callback. A wrong field, insufficient HLM1/budget, or initial paging-
  fence failure must fail device creation before an HOB1 GPUVA is exposed;
- fill every HOC1 cache line with adversarial patterns, omit/reorder the x64 WC
  drain in the negative harness, and vary the returned mapping cache policy.
  The selected path must observe byte-exact HOB1 only after drain/release seal;
  a torn pattern or non-WC mapping rejects device qualification;
- hold the HOB1 command pages unmapped, stale the process/root/PTE generation,
  map one operand through another process, mutate HOB1 after sealing, and vary
  HOS1 length/checksum/context/endpoint. Prove QEMU dispatch and
  SubmissionFenceId remain blocked/failed, KMD never CPU-dereferences the
  command GPUVA at DISPATCH, and a valid remapped submit completes only after
  HPM1 has published its exact page-table/TLB epoch;
- fill the C65 pool with 256 tiny live extents, four maximum-size live extents,
  and fragmented mixed-queue extents; prove an extent is never overwritten,
  unmapped, UMD-evicted, or freed before its exact owning HQC1 value, the pool
  lock is absent during callbacks/events, ordinary submissions never CPU-wait,
  pressure arms one exact event and then reuses only the released range, and
  signal failure/reset poisons rather than retires the generation;
- Queue Wait calls the exact-context wait before following work; Queue Signal
  flushes prior actual work then calls the exact-context signal;
- initial value, wait-before-signal, CPU signal, shared/opened fence,
  cross-process on the same adapter, two queues, two devices, two independent
  logical adapters, rejected LDA/cross-adapter flags, tile mapping, close races,
  and device removal match API/KMT behavior;
- `GetCompletedValue` after removal is treated as failure, not completion;
- Microsoft D3D12 command-queue fence HLK signal and multithreaded-wait tests,
  plus a held-host-batch causal test, pass on the target runtime.

HPS2 removal is prohibited until all HTS1/HQA1/HQC1, D3D11-WDDM2.1,
Core-0116/context/fence causal tests pass.

### 18.3 WSI gates

- native Vulkan app reaches the layer; DXVK/vkd3d direct dispatch never does;
- the lower device is Vulkan 1.3 and trace proves both required features and
  both Win32 external device extensions were enabled before WSI object create;
- device-group WSI queries return only `presentMask[0]=1`/LOCAL and one current
  full-client rectangle; explicit swapchain/acquire/present group structures
  accept only the singleton masks/mode, while split-instance and every
  multi-device mode fail before state or semaphore consumption;
- extension enumeration with both layer-name forms, instance/device create
  consumption, both proc-address routes, and the generated dispatch manifest
  expose every C45/C46 WSI entry point exactly once, filter every lower WSI
  name/function, and never leak a virtual handle to lower dispatch;
- the lower canonical family has at least two real unprotected queues; both
  queue-family property forms expose exactly one fewer, device creation adds
  the one private queue only by extending the application's original
  canonical-family `flags=0` record without violating duplicate/count/priority
  rules, both app queue getters hide its index, and no Android/emulated second
  queue or app-visible dispatch lock is used. A protected-only request fails
  WSI-enabled device creation before the lower device/NT objects; Present on a
  protected, private, foreign, different-family, or unrequested queue fails
  before consuming any wait;
- exactly one NT resource handle and two transient NT fence handles are created
  per swapchain image; the resource payload may be imported once per alias,
  while NT-handle creation and every handle/import operation remain zero per
  Present;
- object-create trace proves DEFAULT/SHARED-only committed resources, exact
  C37 descriptions, COMMON initial state, two zero-initialized SHARED-only
  fences, and the exact dedicated-memory/timeline import chains; injected
  DISPLAY/CROSS_ADAPTER/NON_MONITORED/missing-dedicated variants fail;
- canonical `S[i]` creation retains exactly one unnamed resource handle; alias
  create/bind tests (including a still-live retired `oldSwapchain`) consume the
  virtual swapchain pNext, create a real identical lower external image, query
  memory-type bits, make a distinct dedicated import of exact `S[index]`, and
  bind at offset zero. VUID-invalid params/index/memory/device-group shapes
  fail without changing the slot; binding does not require prior Acquire when
  deferred allocation is absent;
- canonical creation trace proves each `S[i]` gets its own real lower external
  VkImage, exact handle-property query, compatible dedicated imported memory,
  and offset-zero bind before `vkGetSwapchainImagesKHR` exposes it; there is no
  lazy/first-Present import;
- `VkImageSwapchainCreateInfoKHR` and
  `VkBindImageMemorySwapchainInfoKHR` with `swapchain=VK_NULL_HANDLE` are
  consumed/stripped and ordinary lower create/bind (including caller memory and
  offset) succeeds; mixed alias/null/ordinary bind batches preserve ordering
  and statuses;
- rendering and layout transitions through canonical and alias images produce
  identical pixels and layout state, while Present still selects the original
  swapchain/index. Destroy/recreate/device-loss tests retain backing through
  alias and in-flight use, then free it exactly once after `vkDestroyImage`;
- alias trace shows one query/import/bind per alias lifetime, zero new NT
  handles per alias, and zero alias handle/query/import operations per Acquire
  or Present;
- both Ready/Release D3D12 fence objects arrive as native create/open branches
  and the lower ICD obtains exact process-local mappings through documented
  native-fence open;
- native-fence open traces prove exact ICD `hDevice`, `EngineAffinity=1`, only
  Shared/NtSecuritySharing flags, zero reserved bytes, HNF1 input/output
  validation, nonzero local handle/mappings, lifetime retention, and rejection
  of wrong affinity/type/LUID/generation/cross-adapter/monitored variants;
- present release and acquire barriers are visible in Vulkan validation/trace;
- Ready/Release values are exact and single-writer under multiple app queues;
- a multi-swapchain Present waits each binary semaphore once; a timeline wait,
  unknown/mixed swapchain, unsupported `pNext`, non-LOCAL mode, or mask other
  than the last acquired singleton mask is rejected before any lower submit or
  wait consumption;
- timeout-zero Acquire returns NOT_READY without polling when unavailable;
- per-slot command-lifetime traces prove no D3D12 allocator/list or Vulkan
  command pool/buffer is reset/re-recorded until that slot's exact Release
  epoch completes. When all slots are busy, finite/infinite Acquire arms one
  exact fence/state event and blocks without polling; multi-swapchain Present
  submits distinct per-slot release buffers and bounded storage never grows;
- surface queries enumerate exactly the section-10.7 BGRA8/sRGB, FIFO,
  identity-transform, opaque-alpha, one-layer, unprotected usage profile;
- every admitted `S[i]`/`B[j]` pair has identical type, byte size, extent,
  format, mip/layer count, and sample count/quality; the debug layer reports no
  `CopyResource` or state error;
- `Present(1,0)` preserves FIFO order; windowed and borderless-fullscreen HWNDs
  survive composed/direct/independent transitions without changing the copy
  profile;
- resize and minimize return `VK_ERROR_OUT_OF_DATE_KHR`, destroyed HWND returns
  `VK_ERROR_SURFACE_LOST_KHR`, and device reset/removal returns
  `VK_ERROR_DEVICE_LOST`; successful recreation retires `oldSwapchain` only
  after the new exact object set exists;
- `vkDestroySwapchainKHR` tests rely on the application's acquired-image
  completion precondition, reject later canonical Acquire/Present use, drain
  layer-owned helper/D3D/DXGI work, perform the final exact external-to-Vulkan
  ownership return, and move separately created aliases to `ALIAS_ONLY`;
  post-destroy ordinary Vulkan use of those aliases remains valid and their
  exact payload/backing lives until their own GPU work and `vkDestroyImage`
  retire;
- negative query/create tests prove mailbox, immediate, FIFO-relaxed,
  latest-ready/shared modes, other formats/color spaces, scaling, stereo,
  protected presentation, HDR metadata, incremental regions, present ID/wait,
  timing, maintenance-mode switching, and application-controlled exclusive
  fullscreen are not advertised and cannot enter a fallback path;
- current DWM opens D3D12-created backbuffers through the D3D11 UMD;
- composed, direct, and independent transitions show correct current pixels;
- DWM restart, app crash, KMD reset, and adapter removal leak no objects and
  never reuse stale state;
- LUID/UUID/cross-adapter mismatch fails loudly.

### 18.4 Display/reader gates

- cold boot on build 28000 starts DWM with the WDDM-3.2 surface and never
  reaches the former `CDDisplaySwapChain`/`E_NOTIMPL` failure;
- UMD and KMD capability queries agree on exactly one RGB primary, no YUV,
  transform, scale, HDR, immediate, shared, or post-composition capability;
- `CheckMultiPlaneOverlaySupport3` accepts the exact one-primary profile and
  returns success/unsupported for injected second-plane, bad source/layer,
  scale, rotation, blend, YUV, HDR, post-composition, or stale-allocation
  cases; reserved fields remain zero;
- `SetVidPnSourceAddressWithMultiPlaneOverlay3` uses only
  `ppContextData[0]->hAllocation`, runs bounded at interrupt level, never waits
  or polls, never sets PostPresent/Hsync/HW-flip flags, and cleanly handles
  enabled, disabled, retry, cancellation, and source-unbind cases;
- classic `DxgkDdiSetVidPnSourceAddress` uses only its OS-supplied
  `hAllocation`/`VidPnSourceId`/committed-mode state, shares the exact bounded
  validator and candidate/current/backend-release state machine, is entirely
  nonpageable and DIRQL-safe when MMIO flip is advertised, and never assumes
  that MPO3 ran first;
- injected classic static incompatibility is rejected by the preceding UMD
  Direct-Flip gate and remains composed; an injected post-admission KMD
  invariant mismatch fails before latch and is reported as a loud failure,
  never as a seamless fallback from `SharedPrimaryTransition`;
- the `FlipWithMultiPlaneOverlay` Present union arm is bounds-checked and never
  read as `pAllocationList`; PostMPO, if invoked despite no request, returns
  success without changing lifetime state;
- composed, MPO, direct, independent, fullscreen, and mode-transition stock
  display paths resolve the exact OS-supplied allocation, never a `resid`;
- KMD retains each candidate before accepting it; cancellation before latch
  releases only the candidate and leaves the current binding;
- every real replacement uses a unique nonzero fenced `SET_SCANOUT_BLOB`, and
  only a successful response echoing the exact flag/id is terminal;
- that boundary installs the new current and releases the prior backing once;
- transitions unbind via fenced replacement with permanent parking, then may `SET(0)`;
  `SET(0)` never releases parking, retained until later replacement/reset;
- errors, timeouts, duplicate/stale/mismatched replies cannot affect a newer bind;
- no `read_ledger` page, event, polling thread, or Escape is created.

### 18.5 Performance and deployment gates

Measure adapter startup, paging, and Lock2 lifetime separately from frame
cadence: one HPM1/complete-linear-BAR negotiation per adapter; one real paging
DMA plus mandatory side-effect-free Patch and device completion per VidMm
placement transaction; and one ordinary Lock2/Unlock2 per active HVM1 CPU-map
lifetime. Prove no CPU-host-aperture callback/protocol, scan, polling, retry
sleep, CPU-side QEMU wait, or Present-path mapping work exists. Force local/
system pressure and relocation and prove bytes/generations remain exact.

Measure and assert per frame:

- D3D: one actual outer submission per chosen batching policy, zero lower
  execution submissions, zero custom handle opens, zero global scans or
  ProcessContext session lookups; HQA1/HQC1 are context-lifetime objects;
- translated control: zero ring-zero transaction for ordinary recorded GPU
  work. Pure synchronous API calls pay one bounded control transaction; only
  API-required GPU-dependent results additionally pay one exact HQC1 signal/
  event wait. Allocation-backed operations remain in the counted outer batch;
- normal native Vulkan: each HNR2 fragment produces one `D3DKMTRender` and
  physical scheduler packet, but only COMMIT produces one actual host
  submission/completion; its optional progress
  signal/wait exists only for an API-visible CPU milestone, never every batch;
- native WSI: two minimal Vulkan ownership submissions and therefore two HNR2
  Render transitions/host completions per present/reacquire cycle, one D3D copy
  submission, one ICD KMT Ready signal, one D3D Wait, one D3D Signal, one ICD
  KMT Release wait, one Present, zero handle opens;
- no CPU GPU wait except an API-requested CPU milestone, Acquire backpressure,
  teardown, or device-loss cancellation;
- queue-local lock hold time is bounded by bytes/resource entries sealed and
  the 64-slot context pool; no KMT/host call occurs under it;
- independent queues/processes scale without one adapter-global mutex.

Deployment test must reject every mixed generation. Visible rendering and
owner-observed liveness are acceptance requirements in addition to HLK, ETW,
hashes, counters, and device status.

## 19. Adversarial-review ledger

### 19.1 Corrective passes and rejected designs

| Pass | New material objection | Severity | Resolution |
|---:|---|---|---|
| 1 | “use shared fences” had no DWM handle/value carrier | fatal | rejected direct DWM fence |
| 2 | allocation+sync ShareObjects group had no stock OpenResource carrier | fatal | rejected group route |
| 3 | Present private bytes and sync tokens had no receiving-DWM semantics | fatal | rejected |
| 4 | D3D11/D3D12 proxy preflight/postflight did not make lower work the primary-bearing command | fatal | rejected all proxy variants |
| 5 | WDDM-2.1 D3D12 `HFENCE` could not be cast/mapped to the KMT sync handle required by FromGpu callbacks | fatal for 2.1 | rejected cast, engine shadow, and 2.1 retirement |
| 6 | one regular fence had two writers and unspecified total order | high | two-fence idea considered, later rejected with proxy |
| 7 | two regular fences imposed per-queue ping-pong and still left false primary completion | fatal | rejected WDDM 2.1 bridge |
| 8 | WDDM-3.2 native fences improved the primitive but the then-current Core DDI still did not expose D3D12 `HFENCE -> hSyncObject` | fatal at Core <=0110 | no selection until released Core 0116 evidence |
| 9 | fragmented/private-ticket batches lacked truthful residency/primary ownership | fatal | selected actual outer batch |
| 10 | HWQueue fence broadcasts exist, but public `SubmitCommandToHwQueue.WrittenPrimaries` docs never grant ordinary Resource-Heaps/DWM semantics | fatal | rejected HWS/HWQueue for correctness; retained normal `SubmitCommandCb` |
| 11 | WSI layer recursed through DXVK/vkd3d and current ICD lacked D3D12 imports | fatal | direct ICD dispatch, record-only mode, external support included atomically |
| 12 | WSI shared intermediate still depends on exact D3D12-created-to-D3D11/DWM opening | fatal | verified existing create-time `pfnAllocateCb`/D3D11 OpenResource direction; retained and corrected it; `pfnOpenHeapAndResource` identified as reverse direction |
| 13 | Acquire requires a queue and external ownership barrier; an empty submit was insufficient | high | layer-owned helper queue and reusable barrier command buffers |
| 14 | direct scanout reader lifetime still depended on Escape/polling | fatal | selected exact bounded candidate/current plane plus backend-reader ownership |
| 15 | feature 41 and Present-HWQueue semantics are too sparse to prove DWM correctness | fatal for that design | rejected both from selected chain |
| 16 | WSI incorrectly equated virtual image index with the current DXGI backbuffer | fatal | query `IDXGISwapChain3::GetCurrentBackBufferIndex` every Present and use distinct `S[i]`/`B[j]` domains |
| 17 | acquire helper would race app calls on an externally synchronized Vulkan queue | fatal | initial alternatives were identified; pass 42 later narrowed the selected result to one additional real private lower queue and removed shared-queue alternatives |
| 18 | current KMD mutates private data during ordinary OpenAllocation, and the draft tried to remove valid create-time transport | fatal | retain only documented `[in/out]` create-time descriptor transport; ordinary open is read-only; exact allocation object remains identity |
| 19 | UMD-provided/adopted host tokens left a forgeable raw-ID/ABA seam in allocation identity | fatal | KMD creates and owns backing for the exact WDDM allocation; private bytes describe layout only and batches name allocation-list entries/GPUVAs |
| 20 | translator upload, sparse, fence-worker, drain, or swapchain work could still submit outside the outer D3D causal stream | fatal | enumerate every direct Vulkan queue call; fold it into the owning outer batch, lower only tile mappings to C34 paging, or refuse the capability |
| 21 | WDK 26100/Core 0110 still had no native open association in D3D12 | fatal | raised OS/runtime floor to released WDK/build 28000 Core 0116 |
| 22 | Core 0116 mapped `HFENCE` to native object, but queue ordering still needed an exact context operation | fatal until proved | verified D3D12 `pKTCallbacks`, virtual `hContext`, and documented all-type FromGpu wait/signal callbacks; selected explicit callback lowering |
| 23 | legacy `MONITORED` and optional non-monitored fence branches remain unmapped | fatal if advertised | do not advertise non-monitored fences; reject any supported create/open not `NATIVE`/`OPENED_NATIVE` |
| 24 | hardware-flip PresentId completion is visibility, not external-reader release | fatal | PresentId removed from lifetime proof; exact fenced nonzero replacement completes latch+prior release, and unbind first replaces with permanent parking |
| 25 | direct ICD KMT Present cannot mint/open the DWM logical-surface/history state or own direct-flip transitions | fatal | rejected direct KMT WSI; selected layer-owned real DXGI swapchain plus actual D3D copy |
| 26 | system D3D11On12 selection is reserved/system-only and not guaranteed for DWM | fatal | rejected as DWM replacement; preserve normal D3D11 actual-context path |
| 27 | native WSI specified `D3DKMTSignalSynchronizationObjectFromGpu2`, whose public contract is monitored-fence-specific, for a DEFAULT native fence | fatal until corrected | use non-2 `D3DKMTSignalSynchronizationObjectFromGpu` with the exact ICD KMT context, one local object, and `MonitoredFenceValueArray[0]=e`, matching Microsoft's native kernel-queue scenario |
| 28 | a guest reboot/driver-load boundary did not stop a legacy QEMU/display endpoint and therefore could not prove atomic host/guest protocol activation | fatal | require full VM shutdown, matching host endpoint start, and mutual KMD/host generation-feature attestation before admitting work; remove HPS2 only afterward |
| 29 | native WSI advertised broad formats/modes/HDR/timing/fullscreen behavior while its sole `CopyResource` could only copy equal compatible resources | fatal for WSI correctness | constrain this generation to the normative BGRA8/sRGB/FIFO/equal-extent copy-only profile in C37-C38 and section 10.7; all broader surfaces are negatively queried/refused |
| 30 | WSI used `vkQueueSubmit2` and timeline semaphores without requiring/enabling synchronization2, timelineSemaphore, or the Win32 external extensions | fatal at device admission | require Vulkan 1.3, explicitly augment the copied lower-device feature/extension chain, and fail before handle export unless every C39 query succeeds |
| 31 | raising only Helios's WDDM version crosses the locally proven Display-Core/MPO3 boundary, while KMD rejected the MPO Present union and UMD advertised an impossible 16-plane profile | fatal for the 3.2 package | implement the exact one-primary C41/MPO3 table, Present union, truthful matching UMD/KMD caps, and exact allocation/backend lease atomically; cold build-28000 DWM admission is mandatory because Microsoft does not guarantee the proposed numeric profile |
| 32 | WSI object creation/import and mixed-Present validation were described only generically, leaving heap/fence flags, initial values, dedicated import, and binary-wait ownership ambiguous | fatal for exact handle/state transport | specify C40 object flags/values and the exact Vulkan image/memory/semaphore import chains; validate all layer objects and binary waits before one lower submit and never forward an unknown mixed Present |
| 33 | performance accounting omitted the ICD-side KMT signal per Present and KMT wait per Acquire required by the selected native-fence contract | high | count both exact-context kernel transitions explicitly in sections 16 and 18; they remain queue operations, not handle opens or CPU waits |
| 34 | the current `QUERY_STATS` and `QUERY_SCANOUT_TIMELINE` diagnostics still used Helios-private `D3DKMTEscape`, and the implementation manifest had not explicitly retired their tools/ABI | fatal against the no-Escape constraint | delete the entire private Escape ABI and every private-Escape probe in the new package; move live causal events to C42 ETW and OS-requested WDDM diagnostic DDIs, with explicit disabled/enabled cost |
| 35 | the one-plane MPO3 profile did not constrain the earlier D3D11 UMD Direct-Flip eligibility callback, so unsupported app/DWM resource pairs could reach promotion before KMD CheckMPO3 | fatal for composed/direct/independent transitions | define HWA2's exact immutable pair facts; make D3D11.1 `CheckDirectFlipSupport` fail closed for every non-C43 pair and immediate flip, then independently revalidate actual plane attributes in KMD CheckMPO3 |
| 36 | HWA2/C43 required equal non-sentinel VidPn IDs, contradicting the D3D12 runtime-primary rule and thereby rejecting all selected D3D12 Direct/Independent Flip | fatal for the mandatory DX12 topology | preserve and cross-validate C44's `D3DDDI_ID_UNINITIALIZED` runtime-primary marker, admit it only on the exact same adapter/LUID with the matching static profile, and take the actual source solely from the later OS display DDI |
| 37 | C43 treated CheckMPO3 as the later validator even though classic Direct/Independent Flip uses `DxgkDdiSetVidPnSourceAddress` and need not invoke MPO3 | fatal for classic promotion and exact reader lifetime | add one shared bounded/nonpageable KMD validator and candidate/current/backend lease state machine used independently by classic SetVidPn, CheckMPO3, and SetMPO3; the classic DDI uses its exact OS `hAllocation` and cannot advertise a seamless post-admission failure |
| 38 | `DxgkDdiControlEtwLogging` was named as if it registered the provider, leaving provider lifetime, elevated-IRQL payload safety, and the decoder ABI unspecified | high observability/lifetime gap | define the section-12.3 GUID/event ABI; `EtwRegister` at PASSIVE init, gate fixed nonpaged one-descriptor `EtwWrite` sites, drain them, and `EtwUnregister` once; event loss never affects graphics behavior |
| 39 | the WSI layer advertised Vulkan 1.3 plus `VK_KHR_swapchain` but omitted the mandatory Vulkan-1.1 singleton device-group interactions and rejected/failed to realize swapchain-memory alias images | fatal for advertised Vulkan API correctness | implement every C45 LOCAL query/mask/structure, consume alias create/bind pNext locally, create a real lower external image, make one distinct dedicated import from the exact retained `S[index]` handle, and hold old-swapchain/backing state through alias/GPU destruction with zero per-Present handle work |
| 40 | the first C45 text left a count-zero device-group Present mode ambiguous and did not define multi-image `vkBindImageMemory2` failure/partial-binding ownership | high API/lifetime gap | require LOCAL even when masks default from count zero; prevalidate and prepare the whole bind array, preserve `VkBindMemoryStatus`, issue one translated lower batch, and retain every potentially bound imported payload until its alias is destroyed after success, per-bind failure, or indeterminate aggregate failure |
| 41 | shipped DXVK/vkd3d still contained live Wine-private resource/rectangle Escapes and a compiled `\\.\SharedGpuResource` IOCTL metadata registry outside the original HPS symbol search | fatal against the no-Escape/no-side-channel constraints | add every callsite/header/build owner to the atomic manifest; delete UPDATE_RESOURCE/SET_PRESENT_RECT and OPEN/SET/GET metadata mechanisms with no fallback; route supported D3D11/12 sharing only through ordinary WDDM create/share/open and refuse unproven D3D9 sharing before handle export |
| 42 | Acquire's queue had been left as a choice among a shared app queue, internal-synchronization extension, or private queue, so lifetime, locking, and queue-count admission were not implementation-complete | fatal queue/reentrancy gap | select exactly one extra real lower queue reserved at `vkCreateDevice`, expose one fewer queue, hide the private index, require true capacity >=2, and use one layer-only mutex; no app queue or emulated second queue participates |
| 43 | the layer claimed WSI ownership without specifying extension enumeration, create-chain consumption, proc-address routing, and lower-name filtering | fatal API/reachability gap | define C46's complete instance/device discovery and generated dispatch closure; every virtual WSI command is layer-owned and lower WSI is neither advertised nor reachable |
| 44 | non-null-only swapchain alias handling rejected the valid `swapchain=VK_NULL_HANDLE` create/bind forms | high Vulkan API gap | consume/strip null forms and forward ordinary lower create/bind with caller memory/offset; support mixed bind batches without alias side effects |
| 45 | native WSI named the KMT native-fence open but omitted mandatory affinity, flags, private bytes, reserved fields, and returned-mapping validation | fatal exact-open/lifetime gap | define C49's one-node affinity, shared-only flags, HNF1 input/output, zero-reserved, mapping/object-generation validation and reset-time invalidation; reject every mismatch rather than retry another fence type |
| 46 | WSI reused D3D12 allocators/lists and Vulkan barrier buffers without a completion retirement rule, permitting reset while pending or unbounded growth | fatal GPU-lifetime/performance gap | allocate one bounded D3D allocator/list and one pool/two buffers per slot; make actual `Release[i]=e` completion the sole reset/re-record boundary and use one exact event only under Acquire backpressure |
| 47 | canonical `S[i]` was called an imported lower image but only the optional alias path had an exact image/query/dedicated-memory/bind sequence | fatal allocation-identity gap | eagerly perform the full canonical `D3D12_RESOURCE_BIT` create/query/dedicated-import/bind before exposing swapchain images; retain its exact memory/backing through slot lifetime |
| 48 | parent-swapchain destruction and Vulkan's outstanding-operation precondition were conflated with old-swapchain retirement, making alias usability and physical cleanup ambiguous | fatal use-after-free/API-lifetime gap | distinguish retirement from destruction; rely on the application's pre-destroy acquired-image completion, drain layer-owned work, return external ownership, invalidate only canonical WSI handles, and keep separately created aliases in `ALIAS_ONLY` until their own GPU work and `vkDestroyImage` retire |
| 49 | the first pass-48 correction still invalidated separately created alias images at parent destruction, contradicting their independent `vkDestroyImage` lifetime | fatal advertised-Vulkan lifetime gap | retain separately created aliases as normal `ALIAS_ONLY` Vulkan images after final ownership return; only canonical WSI handles/operations die with the swapchain |
| 50 | removing all private Escape left the normal loader-facing Vulkan ICD with no specified Windows submission path; its current KMT context was only a probe | fatal native-Vulkan execution/recursion gap | select exactly C50 legacy nonvirtual `D3DKMTCreateContext` + HVC1/HNR2 `D3DKMTRender`; KMD validates fragments and COMMIT's exact allocation manifest, submits one finite host batch on the matching physical scheduler context/ring, and completes only after host/reply terminal completion |
| 51 | helper injection could admit a device whose application requested only protected queues and Present did not prove the passed queue was the app's unprotected canonical queue | fatal queue/protection gap | require an original app-requested canonical-family `flags=0` queue before lower-device creation, extend only that record for the private helper, and reject every protected/private/foreign/wrong-family Present before consuming waits |
| 52 | compiled KMD Escape MAP_BLOB owners (`blob_map.rs`, `seh_shim.c`, build/module/device teardown) were absent from the one-shot manifest | fatal no-Escape/deployment gap | delete the private mapper, Escape DDI, user-MDL state, old shim, and build/module owners; replace storage with C52 WDDM allocations and leave the initialization Escape slot null |
| 53 | deleting the old SEH shim without replacing `DxgkDdiRender`'s mandatory protected access would let an invalid user command pointer bugcheck the new legacy lane | fatal security/stability gap | add C53's purpose-limited `render_user_copy.c` under `__try/__except`; copy only bounded command bytes into a preallocated KMD slot, then parse kernel storage and never expose a mapping |
| 54 | `pSystemMem`/`ExistingSysMem` gives the UMD a WDDM backing but gives KMD no documented system VA or QEMU/Venus association | fatal native-storage contract gap | reject HNS1 entirely; renderer bytes must be the actual WDDM placement resolved by HPM1, never an independent HOST3D authority |
| 55 | WDDM ShareBackingStoreWithKmd gives UMD/KMD one backing pointer but neither exposes the same bytes to QEMU nor satisfies generic host-visible Vulkan memory | fatal host-association gap | reject it as the Venus carrier; its intermediate HAP successor was also rejected at pass 68 |
| 56 | MapApertureSegment2 arrives in paging after Render and gives a transient BuildPagingBuffer CPU address, so it cannot be an HNR command/reply staging pointer | fatal causality/lifetime gap | reject HNS2 staging; command bytes travel in finite KMT Render, while HVM1 CPU access uses ordinary Lock2 over the selected HLM1/system mapping |
| 57 | the current mapper assumes one whole consecutive 4-KiB blob, ignores `pMemorySegmentPages`, and can acknowledge stale elevated-IRQL state | fatal exact-page/security gap | the attempted HAP1 repair was superseded, then rejected at pass 68; delete the mapper/callbacks instead of carrying any part into HLM1 |
| 58 | stock `vn_ring` exposes raw `resource_id`, polls head/status, and cannot make COMMIT's exact WDDM allocation list authoritative | fatal identity/polling gap | delete the Windows shared ring; HNR2 finite dispatch writes zero placeholders and typed allocation operands, KMD turns the converted allocation list into DMA-local physical capabilities, and QEMU alone resolves those capabilities to renderer-private IDs in a host-only copy after final Patch |
| 59 | a fixed reply mailbox cannot cover the hundreds of generated and caller-sized Venus reply paths | fatal Vulkan-output gap | exact generated `reply_size` chooses/grows an HVM1 slot; SetReply plus command share one finite COMMIT, host publishes reply metadata before fence, and C51 wakes the decoder without polling |
| 60 | one host Venus context per WDDM queue would split a VkDevice's object namespace, while ring index alone is not a WDDM engine | fatal object/queue topology gap | initially moved ownership to one KMD-device host context; pass 78 refines the correct scope to one HTS1 host context per `vn_instance`, because a process can contain several instances but a host decoder context accepts only one |
| 61 | the first HAP1 text treated CPU Host Aperture as an HVM1-only mapper, but Windows may invoke an advertised service immediately after segment enumeration with `hAllocation=NULL` for implicit/page-table storage and for other eligible segment allocations | fatal adapter-start/segment-contract gap | broadening HAP1 still failed the exact page-input contract; pass 68 rejects the service entirely and HLM1 advertises no callbacks |
| 62 | ring-0 control commands and replies had HVM1 pools but no explicitly owned C51 progress object | fatal control-completion gap | create one distinct unshared monitored progress fence for every HNR2-executing context, including control; its value can represent only that context's separately proven completion class and no context may borrow another fence |
| 63 | a nominal 16-MiB payload could not fit 64 256-KiB command buffers after per-fragment headers plus final use/patch metadata | fatal bounds/implementation gap | cap payload at 15 MiB, 64 fragments, 256 use records, and 4096 typed patches; assert the complete final COMMIT fits and test every boundary/failure arm |
| 64 | the first C62 fix still treated host ring 0 as if its terminal fence proved GPU work, although Venus defines zero as a CPU timeline that signals after renderer command processing | fatal queue-order/lifetime gap | restrict ring 0 to generated CPU/decode semantics and replies; only nonzero queue contexts/fences prove GPU completion, and queue/device idle, reset, and teardown join every affected nonzero C51 milestone before final control destruction |
| 65 | the first generic HAP1 design invented a synthetic backing for `hAllocation=NULL` and treated `pMemorySegmentPages` as global physical pages | fatal exact-identity/adapter-start gap | official input instead names allocation-relative page indices and gives no public substitute identity for the null implicit allocation; the attempted physical-page interpretation is rejected, not repaired |
| 66 | Lock2, MakeResident, residency notification, and WDDM-3.2 whole/single-segment flags do not promise a fixed physical BAR offset, while the current whole `RESOURCE_MAP_BLOB` path can diverge on relocation/eviction | fatal byte-authority/lifetime gap | select C56 actual HPM1 paging plus repeatedly patched current physical placement; Lock2's documented CPU VA may survive eviction, but no cached physical offset or lifetime blob mapping is authoritative |
| 67 | HNR2 originally declared `NoPatchingRequired=1` and zero output patches even though the selected renderer actually needs VidMm's final physical placement | fatal WDDM physical-addressing gap | set `NoPatchingRequired=0`; Render emits one output patch/capability slot per allocation use, repeatable Patch writes the exact final segment/physical address and HPM1 epoch without side effects, and Submit rejects any stale capability before QEMU resolution |
| 68 | HAP1's final text still inverted Microsoft's map contract: `hAllocation` is the mapped allocation and `pMemorySegmentPages` contains allocation-relative indices, while the null implicit case has no published backing resolver | fatal selected-carrier contradiction | reject HAP1 and every Map/UnmapCPUHostAperture callback/protocol. Select the separate documented direct-linear-memory profile: aperture id 1 remains the paging/DMA segment; HLM1 id 2 is a complete `CpuVisible` memory BAR with no HAP support. Cold AddAdapter/Lock2/DWM qualification is a hard admission gate, not assumed from current code |
| 69 | the lower ICD's `D3D12_RESOURCE_BIT` prose imported an NT handle without specifying how its KMT device obtained the exact local WDDM resource/allocation | fatal cross-UMD identity/lifetime gap | the attempted C57 Query/Open repair was later rejected by pass 75 because NT-handle open does not return the allocation record; named/OPAQUE/raw-ID paths remain forbidden |
| 70 | the performance/gate text called paging Patch optional although VidMm's BuildPagingBuffer contract says it calls Patch before paging Submit | high exact-DDI contradiction | make paging Patch mandatory, infallible, side-effect-free and no-size-change in C56, manifest, cost model, and causal gates |
| 71 | the all-or-nothing gate still named the rejected CPU Host Aperture lane and stopped at C55, omitting selected HPM1 paging and the then-proposed KMT resource open | fatal deployment contradiction | require C50-C56, name HLM1/HPM1 explicitly, require every CPU Host Aperture callback/protocol absent, and keep C57 as a separate hard blocker rather than pretending the proposed open is part of the complete lane |
| 72 | HVM1 covered only reply/feedback/host-visible storage, leaving ordinary device-local `VkDeviceMemory` without a WDDM allocation, renderer backing, HNR2 identity, residency, or destruction contract | fatal native-Vulkan completeness gap | make HVM1 the all-memory allocation ABI: every ordinary non-import allocation selects one of exactly two exposed memory types, owns one KMD renderer view and HPM1 placement, and supplies exact binding ranges to HNR2; device-local role never maps, host-visible role alone uses Lock2; refuse sparse/protected/lazy/multi-instance memory |
| 73 | local history records Code 43 for direct-BAR descriptors, so the abstract Microsoft segment contract alone could not be presented as target admission proof | fatal if treated as already qualified | identify both materially different rejected shapes: the three-segment/cache-coherent trial and the one-segment/no-paging-aperture probe explicitly recorded at historical commit `0c8f44bb9ddad49364348dd31321dc30b7bb762e`, `query_adapter_info.rs:344-358,575-580`. The selected two-segment aperture-first/direct-memory-second, non-cache-coherent, no-HAP shape remains untested; cold build-28000 AddAdapter, Lock2, paging, and visible-DWM admission is release-blocking, with no fallback |
| 74 | C57's released WDK header exposes `pOpenAllocationInfo2` but marks its nested allocation handle input, current DXVK/vkd3d retain only the separately returned resource, current Helios alone assumes a filled allocation record, and Microsoft Learn calls the array reserved | fatal unresolved cross-UMD target-ABI conflict | superseded by pass 75: this is not an empirical ABI admission question and a probe of reserved input cannot close it |
| 75 | the draft conflated legacy `D3DKMT_OPENRESOURCE` (`hGlobalShare`, legacy `pOpenAllocationInfo` documented `in/out`) with distinct `D3DKMT_OPENRESOURCEFROMNTHANDLE` (`hNtHandle`, `pOpenAllocationInfo2` input/reserved); it then proposed proving the nonexistent NT-handle allocation output by experiment | fatal interface/identity error | retract the C57 Query/Open allocation-return path and its probe gate. Keep the documented `hResource` output distinct from allocation identity, and keep D3D12 runtime `pfnOpenHeapAndResource` distinct from the lower ICD. Require a documented lower-ICD allocation carrier or redesign before retirement; no reserved-field observation, legacy global share, recursion, named/OPAQUE object, or raw ID may substitute |
| 76 | passes 74/75 blocked C57 solely on Learn's `pOpenAllocationInfo2` reserved/input sentence, despite the released array ABI and Microsoft's published implementation of the exact NT-handle ioctl | fatal over-blocking on a real documentation conflict | select query/zeroed-array/open as the carrier while preserving every namespace separation from pass 75. The released header sizes an array and the Microsoft dxgkrnl implementation copies `{allocation,priv_drv_data,priv_drv_data_size}` back per element. Record rather than erase the contradictory Learn/input annotations; require a fail-closed target-conformance gate and no fallback |
| 77 | pass 76 overstated the evidence, treated GPUVA as required by a legacy patch lane, and left a pass-75 manifest paragraph actively rejecting C57 immediately before the selected implementation paragraph | fatal evidence/lifetime precision and one-shot manifest contradiction | state the exact caller behavior: query sizes, allocate and zero the output storage, never custom-fill an allocation record, then open. Treat hAllocation plus immutable PDD as the required HNR2 outputs; accept zero GPUVA and cross-check only nonzero values. Explain `OpenNtHandleFromName` as an optional named-handle precursor that the unnamed profile skips. Close with resource-level `DestroyAllocation2` after all refs drain. Remove the stale rejection paragraph and retain the Learn/header conflict plus mandatory conformance gate |
| 78 | record-only translation had no synchronous Venus control/reply carrier before a D3D queue existed; a first process-wide host context proposal then violated virgl's one-`VkInstance`-per-context rule, and ring zero could not prove GPU-dependent queries/idle | fatal device-creation/object-namespace/ordering gap | select one bounded HTS1 raw HVC1 control session and distinct host context per Mesa `vn_instance`; attach each outer runtime context once through create-context HQA1 against exact `hKmdProcess`, then retain a direct physical-endpoint ref. Pure control uses finite C51 replies, allocation-backed operations stay deferred to the exact outer allocation batch, and GPU-dependent synchronous calls join that context's private HQC1 before control fetch. Uplift D3D11 to the WDDM-2.1+ callback surface needed for the same event-backed join; Render PDD, PID/resource lookup, merged instances, polling, and lower GPU queues remain forbidden |
| 79 | pass 78 still assigned a UMD cross-context endpoint sequence, which could circular-wait work VidSch had not made eligible; the virtual-submit lane also treated copied private data as if it carried variable command bytes and left page-table handling decorative | fatal cross-context scheduling and virtual-execution gap | remove every UMD cross-context execution sequence. C62 assigns a short KMD arrival-order serial only after a DMA is scheduler-eligible; same-context order comes from VidSch and cross-context order only from native fences. C63 keeps complete HOB1 work in the submitted GPUVA and uses fixed HOS1 solely as authenticated metadata; C64 makes the process root/PTE/TLB service authoritative before QEMU reads HOB1 or operands. The runtime—not the UMD—supplies ordinary runtime-primary `WrittenPrimaries` on the driver's behalf. |
| 80 | pass 79 left the UMD-managed HOB1 GPUVA pool without a retirement rule, allowing command bytes to be overwritten or unmapped while QEMU/host execution still referenced them, or forcing an unbounded allocation strategy | fatal command-buffer lifetime/performance gap | C65 defines one fixed 64-MiB/256-live-extent pool per D3D12 device. Every sealed extent receives the exact same-context HQC1 bottom-of-pipe value inserted immediately after its submit and remains immutable/mapped until that value completes. Allocation checks once and event-waits only under pressure after dropping the pool lock; reset/signal failure poisons instead of retires. No per-submit CPU wait, polling, overwrite, or spill allocation is permitted. |
| 81 | pure-control prose forbade every WDDM-byte access even though finite replies require one exact destination; reply size had an unbounded growth escape; HOB1/HOS1 named a shared context generation absent from HQA1; HOC1 WC writes lacked a publication edge; and an ECL touching more than eight primaries was merely refused rather than sliced with its writing work | fatal reply identity/lifetime, context ABI/cache publication, and primary-association gaps | Make the sole control exception one 64-MiB session-owned role-1 HVM1 allocation with four 16-MiB C51-bound slots, at most 15 MiB per HNR2 reply, generated legal continuations, and no spill. Extend HQA1 to 72 bytes with a UMD-chosen nonzero/nonreused context generation stored in the live KMD context and repeated only as an HOB1/HOS1 cross-check. Require the selected x64 WC store drain plus release seal before SubmitCommandCb and fail unexpected cache policy. Partition an ECL only at parsed translated-command boundaries; each actual submit carries at most eight runtime-supplied primary handles and the exact commands that write that subset. |
| 82 | pass 81's primary slicing assumed the UMD could partition/filter a runtime-maintained `WrittenPrimaries` association, but Microsoft documents no such operation | fatal primary-association overreach | retract slicing. Emit one complete actual HOB1 at the runtime-supported command-list association granularity, consume the runtime-supplied primary array unchanged, never merge or split/filter the association, and fail before SubmitCommandCb if that complete bounded unit cannot be represented |
| 83 | the first continuation carried only total/offset/length/status and could recompute a side-effecting or changing result, retain unbounded host bytes, or outlive its source objects | fatal reply consistency/lifetime gap | define byte-exact HVR1 and one immutable session-local snapshot generation per result, retain source refs, cap each snapshot at 64 MiB and each session at four/256 MiB, require exact-next-offset continuations, and cancel/wake failure on reset/teardown; only generated legal partition/partial-result rules exceed the bound |
| 84 | the one-shot Mesa manifest still mandated one-MiB reply pools that grew HVM1 allocations, contradicting the selected fixed four-slot/no-spill design | fatal deployment contradiction | replace it with exactly one 64-MiB/four-slot HVM1 pool, HVR1, bounded immutable snapshots, and no growth/recomputation/spill path |
| 85 | the 256-use/list limit could not encode a valid indivisible 257-resource operation, current D3D12 Tier 3 allows million-entry dynamic descriptor heaps, and D3D11 has more than 256 bound-resource slots | fatal allocation reachability/capability gap | reserve 256 only for HOC1 extent backpressure. Raise direct HOB1/HNR2 use/legacy allocation/output-patch capacity to 4096 and typed occurrences to 8192; dynamically adopt KMT next-list sizes; statically clamp the normal legacy Vulkan/D3D11 exposed profile so every indivisible closure fits. Keep D3D12 Tier-3 transitive descriptor access in the exact HTS1 object graph plus C64 GPUVA/page tables under the documented application residency/lifetime model, rather than enumerating or truncating descriptors |

### 19.2 Superseded complete pass A

| Perspective | Attack | Result / resolution |
|---|---|---|
| Contract/version | Is a 2.1 handle gap hidden under a 3.2 claim? | No. Build 28000/Core 0116 plus native feature/cap/branch admission is explicit; older/monitored branches fail. Ordinary submission remains a documented WDDM2+ surface. |
| Recursion/reentrancy | Can layer->D3D->translator re-enter layer? | No translator loader path exists; module/import/proc-owner gates enforce it. No layer lock spans external calls. |
| Cross-process/UMD transport | What custom handle reaches DWM? | None. Stock WDDM resource open only. WSI handles remain same-process. |
| Exact identity/reuse | Can PID/resid/handle value alias? | Identity is the live WDDM allocation/GPUVA or an imported kernel object. KMD alone owns its backing; create-time bytes are a paired immutable descriptor, never a token. |
| Queue/timeline correctness | Does any independent lower queue remain, or can a wait/signal overtake? | Record-only mode forbids lower execution. Wait is inserted on exact runtime `hContext` before later submit; signal follows flushed actual submit on the same context/native object. |
| Lifetime/crash/reset | Can timeout/stale callback advance progress? | No; fail-closed generation validation and no rebase-success arm. |
| Flip transitions | Does design require DWM or infer release from PresentId? | No; composed uses DWM, OS chooses direct/independent, and exact plane/backend leases survive until real release. |
| Multi-* scaling | Is there one global queue/lock/value? | No; per context/native object/image/swapchain/plane. Adapters are independent; WSI cross-adapter mismatches fail. |
| Security | Are global ACL/names retained? | No; same-process unnamed WSI handles and OS DWM sharing. |
| Performance | Is discovery/open performed per Present, or is a helper lock global? | No at the time; pass 42 later replaced the then-proposed app-queue lock with one private helper queue/mutex. Native WSI still pays its explicit barrier submits and copy. |
| Deployment | Can a legacy reader run with new writer? | reboot/load generation boundary and fail-fast checks prevent it. |
| Observability | Can a missing wait or reader release continue? | No; device/swapchain fails, and context/native-fence/plane-generation events identify the boundary. |

No new material gap was found in this pass at the time. Corrective passes
27-33 later exposed gaps outside its then-current attack detail, so this pass
does not count toward the final two-consecutive-pass requirement.

### 19.3 Superseded complete pass B

The same twelve attacks were rerun from the consumer toward the producer:

- DWM OpenResource reaches exact create-time D3D allocation data, not a private
  fence; KMD does not mutate it during ordinary open.
- DWM's composition batch is actual WDDM work and waits the producer's normal
  Present/resource dependency.
- direct scanout releases only after exact replacement/unbind and backend
  old-reader release, never merely through PresentId.
- native WSI returns a usable image only after Release plus the external
  acquire barrier; helper/app calls cannot race on the lower queue.
- every D3D producer path terminates at one real primary-bearing WDDM
  submission; API fence edges name the exact Core-0116 native object/context.
- destruction/reset removes all kernel/object references without PID recovery.
- no source path introduces Escape, global storage, polling, heuristic
  identity, loader recursion, or per-Present handle open.
- performance counts match section 16 and remain bounded per object.

No new material correctness gap, unsupported selected claim, recursion path, or
performance blocker was found at the time. Corrective passes 27-33 invalidate
its finality. Historical corrective and superseded clean passes remain because
they explain why superficially simpler designs and incomplete review scopes are
forbidden.

### 19.4 Superseded final adversarial result after pass 75

Root and the two independent contract/cross-UMD reviewers reran the twelve
perspectives through corrective pass 75. Root personally rechecked the two
distinct KMT structures, their handle namespaces and annotations, the D3D12
runtime UMD open DDI, current DXVK/vkd3d callsites, and Helios Mesa's stronger
return assumption. Pass 75 corrects a material root-review error; this is not a
clean-complete result:

- C57's former target-observation premise is invalid.
  `D3DKMTOpenResourceFromNtHandle` is not the legacy global-share
  `D3DKMTOpenResource` path and has no documented returned allocation/HWA2
  record. This is fatal to the selected native-WSI import path; a probe cannot
  promote reserved input into an ABI.
- HLM1's exact two-segment direct-linear profile remains an explicitly named
  runtime/package admission dependency. Its public segment and paging
  contracts are coherent, but the checked source contains only materially
  different failed descriptor experiments; the exact selected profile has not
  passed cold build-28000 AddAdapter, Lock2, paging, and visible-DWM tests.
- C57 is a static architecture blocker. HLM1 is a later runtime-admission gate.
  Neither may select HPS2, CPU Host Aperture, Escape, named/OPAQUE resource
  identity, a proxy submit, or any mixed-generation fallback on failure.

The ledger therefore contained **75 corrective/adversarial passes** at that
point. Pass 76 supersedes this subsection's first bullet.

### 19.5 Superseded final adversarial result after pass 77

Root and both independent reviewers reran the twelve perspectives after
Claude's C57 correction. The reviewers correctly found two pass-76
overstatements and one live contradiction: the shared element declaration does
label `hAllocation`/private data input; Microsoft's WSL dxgkrnl implementation
does not populate GPUVA; and the one-shot manifest still contained a pass-75
paragraph rejecting the selected carrier. Pass 77 corrects all three without
reintroducing a different interface:

- **C57 remains the selected carrier, with its evidence boundary explicit.**
  Legacy `D3DKMT_OPENRESOURCE` and D3D12 runtime `pfnOpenHeapAndResource` remain
  separate. The selected caller queries sizes, allocates and zeroes the output
  storage, then invokes `D3DKMTOpenResourceFromNtHandle` on its own KMT device;
  it never fabricates allocation records. Microsoft's published dxgkrnl
  implementation returns the local allocation and PDD through this exact
  NT-handle ioctl. Learn/header direction annotations conflict with that
  implementation, so the target conformance check is mandatory and failure
  leaves the Vulkan handle type unadvertised.
- **GPUVA is no longer overclaimed.** HNR2's nonvirtual Render/Patch lane needs
  the returned `hAllocation` and immutable PDD. A nonzero GPUVA is retained and
  cross-checked if present; zero is valid for C57 admission.
- **Handle acquisition is unambiguous.** `D3DKMTOpenNtHandleFromName` is a
  documented optional precursor for named resources. The selected layer already
  owns an unnamed NT handle and skips it. No caller custom-fills
  `pOpenAllocationInfo2`.
- **Lifetime is exact.** After canonical, alias, and GPU references drain,
  `D3DKMTDestroyAllocation2` names `hResource`, supplies no allocation list, and
  releases every associated allocation before the retained NT handle closes.
- **The manifest now has one answer.** The stale pass-75 rejection paragraph is
  gone; every selected section, failure arm, test, and file disposition names
  the same query/zeroed-array/open/destroy sequence.
- **HLM1 is unchanged.** Its exact two-segment direct-linear profile remains an
  explicit cold runtime/package admission dependency. No failure path selects
  HPS2, CPU Host Aperture, Escape, named/OPAQUE identity, a proxy, or mixed
  generations.

The ledger now contains **77 corrective/adversarial passes**. The number of
qualifying consecutive clean-complete passes after the final correction is
**zero**, because pass 77 changes normative text. Superseded passes do not
count. The next two passes must find no new material correction.

### 19.6 Current interim adversarial result after corrective pass 78

Pass 78 found that “record-only queue submit” did not cover synchronous Venus
device/object/query work before an outer D3D queue existed. It also rejected
the tempting process-wide repair: virgl's Venus context accepts one
`VkInstance`, while one Windows process may contain multiple DXVK/vkd3d Mesa
instances. The selected correction is now bounded and explicit:

- one HTS1 session/raw HVC1 control/host context/`VkInstance` exists per Mesa
  `vn_instance`, with multiple sessions contained only by the exact KMD
  ProcessContext;
- each D3D runtime context attaches once through the documented create-context
  HQA1 PDD after exact process/adapter/package/session/capability validation;
  KMD retains a direct physical-endpoint reference and never looks up a session
  during submit or Present;
- pure synchronous control uses finite ring-zero/C51 replies, allocation-backed
  operations remain deferred to the actual outer batch and exact allocation,
  and GPU-dependent synchronous results first join the exact outer HQC1/
  nonzero endpoint;
- ring zero remains CPU/decode only, Render PDD remains zero, multiple logical
  queues sharing a physical VkQueue share only its bounded KMD-arrival-order
  endpoint FIFO,
  and a second frontend instance never enters the first host context; and
- D3D11 explicitly uplifts to a WDDM-2.1-or-later callback table so the same
  monitored-fence FromGpu/FromCpu event join exists while actual rendering
  remains on the physical Render path. This combined runtime shape is an
  admission gate, not an inherited property of the KMD version.

C57 remains closed and unchanged by pass 78. HLM1 remains the separate cold
target-admission dependency. Because pass 78 changes the selected protocol,
manifest, D3D11 version, object graph, and test surface, the qualifying clean-
complete pass count resets to **zero** until two full attacks on this exact text
find no new material correction.

### 19.7 Current interim adversarial result after corrective pass 79

Pass 79 found two independent ordering/execution defects in the pass-78 text.
First, a UMD-assigned sequence shared across WDDM contexts could make an
already submitted batch wait for work that VidSch had not submitted, producing
a circular queue dependency. Second, `DxgkDdiSubmitCommandVirtual` receives an
exact GPUVA/size plus a fixed copied private-data buffer—not a CPU pointer to
the variable command stream—and the current local page-table callbacks were
not authoritative enough for the device to read that GPUVA safely. The
selected correction is now exact:

- C62 assigns a KMD-owned host-dispatch serial only when an already
  scheduler-eligible DMA reaches SubmitCommand. The endpoint lock covers only
  serial assignment and enqueue, never host/GPU completion or a missing value
  from another context. VidSch supplies same-context order; only documented
  native-fence waits/signals supply cross-context order.
- C63 defines complete HOB1 work in the runtime-approved command GPUVA and a
  fixed 64-byte HOS1 metadata prefix copied into KMD private data. KMD validates
  HOS1 and enqueues the exact ProcessContext/address-space generation,
  GPUVA, and size without dereferencing user GPUVA at `DISPATCH_LEVEL`.
- C64 replaces decorative root/PTE handling with an authoritative,
  generation-scoped HPM1 page-table/TLB service. QEMU reads HOB1 and every
  type-2 operand only through that exact current address space and reports the
  submission complete only after the resulting host work completes.
- Microsoft Resource Heaps assigns ordinary presentable-resource tracking and
  `WrittenPrimaries` population to the runtime on the driver's behalf. The UMD
  preserves the same command-list resource/write association and adds only an
  exceptional primary it created itself; KMD is not claimed to inspect a field
  absent from `DXGKARG_SUBMITCOMMANDVIRTUAL`.

C57 remains closed and unchanged by pass 79. HLM1 and HTS1 remain target
admission dependencies. Because pass 79 changes the normative scheduling,
command ABI, page-table implementation, manifest, failure rules, and tests,
the qualifying clean-complete pass count is **zero** until two full attacks on
this exact text find no new material correction.

### 19.8 Current interim adversarial result after corrective pass 80

Pass 80 found that a valid GPUVA submission also needs an exact UMD-side
command-buffer lifetime. The pass-79 text made HOB1 immutable in principle but
did not define when its GPUVA extent could be reused, unmapped, or freed. C65
now closes that edge with one bounded per-device pool:

- the pool is exactly 64 MiB, 64-KiB aligned, and admits at most 256 live
  extents; each HOB1 remains at most 15 MiB;
- after every successful actual submit, the UMD inserts the next same-context
  HQC1 bottom-of-pipe signal and records that value on the sealed extent;
- an extent remains immutable, mapped, and unavailable for UMD eviction or
  reuse until that exact value completes;
- ordinary allocation performs one completed-value check and never waits; only
  bounded pressure drops the pool lock, arms one event for the oldest exact
  owner/value needed to make space, waits, and retries; and
- signal failure, reset, or removal poisons all old-generation extents and
  cannot masquerade as retirement.

C57 remains closed and unchanged by pass 80. HLM1 and HTS1 remain target
admission dependencies. Because pass 80 changes the normative object graph,
queue work, cost model, manifest, lifetime, failure rules, and tests, the
qualifying clean-complete pass count is **zero** until two full attacks on this
exact text find no new material correction.

### 19.9 Superseded interim result after corrective pass 81

Pass 81 closed three bounded but material edges without changing C57 or the
selected submit topology:

- pure control may read only its copied HNR2 input and write only one exact
  checked-out slot in the HTS1 session's raw-device role-1 HVM1 reply pool. The
  pool is exactly 64 MiB/four 16-MiB slots; one transaction publishes at most
  15 MiB, larger legal results use generated finite continuations, slot reuse
  waits for the exact C51 owner, and no spill allocation exists;
- HQA1 is now a 72-byte create-context packet containing the UMD-chosen,
  nonzero, nonreused context generation that HOB1/HOS1 repeat. KMD stores it in
  the live context and treats the number only as an anti-stale cross-check, so
  no undocumented create-context output or later session lookup is required;
- HOC1 publication now requires the selected x64 WC store drain and release
  seal before SubmitCommandCb; any unexpected cache policy or torn-byte probe
  rejects device qualification; and
- an ECL that accumulates more than eight runtime primaries is sliced only at a
  parsed translated-command boundary. Each same-context actual submit carries
  at most eight handles selected from the runtime-supplied primary set and the
  exact commands that write them; an indivisible over-limit command fails
  before submission.

C57 remains closed and unchanged by pass 81. HLM1 and HTS1 remain target
admission dependencies. Because pass 81 changes normative ABI, storage,
ordering, cost, failure, manifest, and test text, the qualifying clean-complete
pass count is **zero** until two full independent attacks on this exact text
find no new material correction.

### 19.10 Implementation freeze after corrective pass 82

Pass 82 supersedes pass 81's primary-slicing paragraph and closes the three
confirmed blockers found in the final review:

- HVR1 and a four-snapshot/256-MiB session cap make continuation bytes
  immutable, exact-next-offset, source-ref-retaining, cancellable, and
  non-recomputed; the one-shot manifest now has no growable reply pool;
- every runtime `WrittenPrimaries` association is consumed unchanged by one
  complete actual HOB1. Helios does not partition a runtime-owned primary set;
  an unrepresentable association fails before SubmitCommandCb; and
- 256 is no longer a semantic resource limit. Legacy paths carry up to 4096
  exact unique allocations/8192 typed occurrences with documented list resize
  handling and capability clamping, while D3D12 Tier-3 transitive descriptors
  use C67's exact session object graph and C64 GPUVA/page tables.

At the user's direction, recursive static edge hunting stops here. This is the
frozen implementation reference: source implementation may begin in isolated,
disabled-by-default changes. It does **not** authorize installation, adapter
activation, HPS2 removal, or claiming correctness before section 18's target
runtime/HLK/visible-desktop gates pass. C57 remains closed and unchanged.

## 20. Remaining user decisions

None. The user's willingness to raise the WDDM version is taken as the
architectural choice to target Windows 11 26H1/build 28000, WDDM 3.2, Core DDI
0116 or later, and retire WDDM 2.1 **OS/KMD package** compatibility in the new
package. The selected D3D11 UMD separately negotiates a WDDM-2.1-or-later UMD
callback table as C61 requires; that does not make a WDDM-2.1 KMD/OS a target.

The former C57 **architecture dependency** is discharged. Passes 76-77 identify
and bound the published lower-ICD carrier — `D3DKMTQueryResourceInfoFromNtHandle`
followed by `D3DKMTOpenResourceFromNtHandle` on the ICD's own KMT device — and
record its exact identity, documentation conflict, conformance gate, and
lifetime rules in C57 and section 10.7. What
remains is not an architecture choice: HLM1's exact two-segment cold
AddAdapter/Lock2/paging/DWM admission and HTS1/HQA1/HQC1's exact raw/runtime
same-ProcessContext plus D3D11-physical-Render/modern-callback admission are
still unexecuted target-runtime gates.

Runtime/HLK gates in section 18 remain implementation acceptance tests, not
open architectural choices. Failure of Core-0116 negotiation, native branch
selection, exact-context ordering, stock Present ordering, or plane-reader
lifetime rejects the implementation and leaves the existing package untouched;
it never enables HPS2 or a mixed mode.

## 21. Final static-review checklist

- [x] Read `CLAUDE.md` and current `ROADMAP.md`.
- [x] Recorded root/nested commits, branches, and initial clean status.
- [x] Reconstructed HPS2 ABI, path, ACL, seqlock, probing, PID generation,
  fence naming/value, release/reclaim, failure, and cost.
- [x] Searched direct symbols, wire fields, raw identity, fence import/export,
  publish/lookup/wait/release, destruction, installer, diagnostics, KMD,
  D3D11On12, DXVK, vkd3d, and Mesa paths in both directions.
- [x] Distinguished live, conditional, retired, and documentation-only paths.
- [x] Decomposed every HPS2 responsibility.
- [x] Verified selected API/DDI names against Microsoft/Khronos primary sources
  and current WDK declarations; pass 75 explicitly separates legacy global-
  share open, NT-handle open, and D3D12 runtime UMD open and retracts the prior
  conflation.
- [x] Rejected direct DWM fence, ShareObjects group, mutable Present-private
  data as a sync mailbox, sync token,
  redirected Present, proxy, ticket, global, Escape, polling, and heuristic
  designs.
- [x] Proved actual D3D work, exact submission completion, Core-0116
  fence/context ordering, Present, DWM open, and exact display-reader lifetime
  form the required dependency chains.
- [x] Proved the complete native-WSI handle path. The Vulkan states, values,
  singleton device-group behavior, aliasing, and acquire/present ownership are
  specified, and passes 76-77 supply and precisely bound the previously missing
  imported-resource allocation acquisition and lifetime.
- [x] Proved no ICD-to-D3D/DXGI or translator-to-layer recursion.
- [x] Specified the pre-queue synchronous control path: one HTS1 host context
  per Mesa `vn_instance`, one-time HQA1 outer-context attachment, exact
  allocation deferral, HQC1 GPU-dependent joins, bounded lifetime, and no
  Render-PDD/process-resource lookup.
- [x] Made the D3D11 WDDM-2.1+ callback-table uplift explicit while preserving
  actual physical Render/Present and gating the combined runtime shape.
- [x] Covered composed/direct/independent, windowed/fullscreen, DWM restart,
  device reset/removal, process crash, PID/allocation reuse, simultaneous
  swapchains, queues, devices, processes, and adapters.
- [x] Made per-resource/per-Present handle, transition, copy, lock, and kernel
  costs explicit.
- [x] Defined one-shot deployment and safe post-quiescence HPS file removal.
- [x] Frozen static review after corrective pass 82 at the user's direction.
  The former two-additional-clean-pass research requirement is intentionally
  waived to begin implementation; this waiver does not authorize activation,
  HPS2 removal, or a correctness claim before the runtime gates pass.
- [ ] Later implementation/runtime/HLK gates in section 18—intentionally not
  executed in this research task.
- [x] `git diff --check` passes.
- [x] This task changed only this Markdown. The unrelated concurrent
  `umd/Cargo.lock` modification visible at final status was not touched or
  incorporated.
- [x] No implementation, build, deployment, VM, registry, adapter, reboot,
  commit, or push action occurred.

## 22. Final static-review result

**Result: IMPLEMENTATION MAY BEGIN; C57 is closed, but HPS2 retirement and
activation still await HTS1/HQA1/HQC1 and HLM1 target admission plus the
section-18 runtime/HLK/visible-desktop gates.**

The replacement responsibilities, exact object graph, queue ordering,
cross-UMD/DWM handoff, display-reader lifetime, recursion proof, performance
model, one-shot manifest, and failure policy are fully enumerated for the
build-28000/WDDM-3.2 candidate. Every checked live HPS2, private-Escape,
read-ledger, named-fence, raw-resource-ID, IOCTL, installer, diagnostic, and
host-reader dependency has an atomic disposition. The candidate does not
contain an allowed HPS2, proxy, global registry, polling, heuristic, or mixed
generation fallback.

The design is frozen for implementation, with deployment correctness still
conditional on its explicit target gates and no longer blocked by C57.
Corrective passes 76-77 select C57 from a released ABI plus Microsoft
implementation evidence while explicitly preserving the Learn/header direction
conflict: the
independent lower ICD acquires the exact local allocation record for a D3D12
`SHARED` resource NT handle by calling `D3DKMTQueryResourceInfoFromNtHandle`
and then `D3DKMTOpenResourceFromNtHandle` on its own KMT device, and reads the
returned per-allocation `hAllocation`, `pPrivateDriverData`/
`PrivateDriverDataSize`. A nonzero `[out] GpuVirtualAddress` is optional for the
selected legacy lane. Learn's isolated "reserved and should be set to zero"
sentence and the input annotations are a recorded documentation conflict with
Microsoft's published implementation, answered by a mandatory fail-closed
target-conformance gate rather than a fallback.

Corrective pass 78 closes the independent pre-queue control hole with
per-`vn_instance` HTS1 sessions, one-time create-context HQA1 attachment,
allocation-backed deferral, and exact HQC1/nonzero-endpoint joins. It does not
reopen C57 or add a lower translated GPU queue.

Corrective pass 79 removes the cross-context sequence deadlock and defines the
exact D3D12 virtual execution carrier: scheduler-eligible KMD arrival-order
enqueue, complete HOB1 work in GPUVA, fixed metadata-only HOS1, and
authoritative C64 process page tables. It also corrects `WrittenPrimaries`
ownership to match Microsoft Resource Heaps.

Corrective pass 80 adds C65's bounded HOB1 GPUVA-pool retirement. A sealed
extent survives until the exact post-submit HQC1 value completes; ordinary
submission stays CPU-asynchronous and bounded pressure uses one event rather
than polling or an unbounded spill pool.

Corrective pass 81 makes the pre-queue control exception finite and gives HQA1
the shared context-generation field consumed by HOB1/HOS1. Pass 82 supersedes
its primary-slicing proposal: HVR1 uses immutable bounded snapshots, runtime
`WrittenPrimaries` associations remain unchanged, and C67 removes the invalid
256-resource semantic limit without expanding Tier-3 descriptor heaps per
submit.

Two target-admission families still bar activation, and neither is C57. HLM1's
exact two-segment cold adapter/Lock2/paging/DWM qualification and HTS1/HQA1/
HQC1's same-ProcessContext plus D3D11 physical-Render/modern-callback
qualification remain unexecuted target gates. Components may now be implemented
in disabled-by-default, reviewable phases, but none may be activated as the
HPS2 replacement on this reference alone.

This research changed only this Markdown reference. It did not change source,
artifacts, packages, services, the registry, drivers, adapters, the VM, or the
host endpoint.

## Appendix A. Exhaustive current dependency inventory

Line references in this section are exact for the commits in section 1. Rows
combine the requested role, identity, ordering, lifetime, failure, and cost so
that every live callsite has one disposition.

### A.1 Direct HPS2 readers, writers, and lifecycle

| Repository and exact source | Status; process/component; role | Identity and synchronization | Timing / required happens-before | Failure, cleanup, and cost |
|---|---|---|---|---|
| DXVK `src/dxvk/dxvk_helios_present_sync.h:16-36`; `.cpp:19-65` | Live; any DXVK-backed D3D process; ABI owner | `resid` -> PID/start/fence/value/kwait | Shared contract for all calls below | Header only; duplicate ABI must match Mesa |
| DXVK `src/dxvk/meson.build:49` | Live build dependency | Compiles the C++ HPS2 implementation into DXVK | Build-time prerequisite for every DXVK reader/writer below | Remove with the implementation source in the atomic generation; no runtime cost by itself |
| DXVK `.cpp:76-175,281-325` | Live on first HPS use; mapper/security/reclaimer | File path, HPS2 magic, PID generation | Mapping precedes access; startup removes only proven-dead generations | Permanent mapping; file/open/map/ACL syscalls; one 4096-slot sweep |
| DXVK `.cpp:330` (`publish`) | Live D3D11 producer writer | Exact Venus `resid`; named exported Vulkan timeline/value | Signal is recorded before slot becomes visible | 3 full scans × 8 attempts; drop/log on failure |
| DXVK `.cpp:402` (`release`) | Live allocation lifecycle writer | Immutable `(resid,fenceId)` plus current PID/start | Release before backing/KMT/Vulkan teardown | Up to 8 full scans; crash leaves stale slot |
| DXVK `.cpp:438` (`lookup`) | Live consumer reader | Imported image’s `heliosResourceId` | Read stable tuple before opening/waiting | 4096 scan; missing/unstable means unordered |
| DXVK `.cpp:471,479,483` | Live helper/diagnostic | process creation time; gate-flush counter | Generation prevents PID ABA | process query once; atomic increments |
| UMD `bridge/dxvk_bridge.cpp:266-301` | Live per D3D11 device producer owner | One named timeline, ID/value, stream cookie | Device lifetime; lazy first-present creation | Mutex/CV; permanent failure latch |
| UMD `bridge/dxvk_bridge.cpp:1130` | Live `publish_present_order` producer | Exact backing `VkDeviceMemory` -> Venus `resid` | Derive exact consumed allocation; record GPU signal; publish | Resource lookup, mutex, HPS scan once/present; returns false loudly |
| UMD `bridge/dxvk_bridge.cpp:1188-1255` | Live producer discovery/ownership | Global named opaque-Win32 Vulkan timeline; present-stream cookie | Object created and private stream registered once/device | NULL DACL on fence; registration uses forbidden Escape indirectly |
| UMD `bridge/dxvk_bridge.cpp:1301-1340` | Live producer update | monotonically increasing `present_value`; HPS tuple | DXVK CS signal before slot publication; optional marker correlation | One mutex, signal record, full HPS scan/present |
| UMD `src/bridge.rs:594-623`; `bridge/dxvk_bridge.h:143-156` | Live FFI wrapper/declaration | Same resource pointer and optional `{ctx,value,cookie}` | Synchronous C++ call, outputs all-zero unless complete | No extra kernel call beyond callee |
| UMD `src/forward/present.rs:1370-1391` | Live conditional folded writer | Resource consumer will see (`dst` after BLT, otherwise `src`) | Signal/publish before existing `Flush` | Enabled by `present_batch_fold`; failed early try falls through |
| UMD `src/forward/present.rs:1403-1435` | Live ordinary fallback writer | Same exact consumed resource | After frame `Flush`, before ordering gate/Present | Always attempted for non-vehicle unless folded success |
| DXVK `src/dxvk/dxvk_memory.cpp:278-287`; `.h:617-640,728` | Live producer allocation lifecycle | Immutable allocation-local `(resid,fenceId)` | HPS release precedes allocation destruction/reuse | One release scan only for published allocations |
| DXVK `src/dxvk/dxvk_context.cpp:9682-9711` | Live DWM/IDD/D3D consumer discovery | Imported sharing metadata’s exact `heliosResourceId` | Sampling records resource for the current command list | Small per-list local dedupe scan |
| DXVK `src/dxvk/dxvk_context.cpp:198-204,9714-9778` | Live consumer GPU wait | HPS lookup; cached named timeline; latest value | Wait inserted before end-recording/current list samples | One global-table scan per touched resid/list; missing slot skips |
| DXVK `src/dxvk/dxvk_context.cpp:9781-9826` | Live consumer object-open/cache | `(pid,start,fenceId)` global name | Open once per generation; timeline wait values deduped | Kernel/name open on miss; failed import retried after 256 calls |
| DXVK `src/dxvk/dxvk_context.h:1765-1814,1837-1864,2439-2459,2783-2792` | Live consumer state/declarations and sampling seam | Exact producer-generation cache, imported `resid` set, waited values, counters, helper declarations | Every sampled shared image is recorded; list flush emits producer waits | Per-context hash maps/vectors grow with observed producers/resources; missing slots and timeout/skip diagnostics |
| DXVK `src/dxvk/dxvk_fence.h:23-43`; `.cpp:6-87` | Live producer and consumer named-fence plumbing | `OPAQUE_WIN32` timeline plus export/import name and security descriptor | Producer exports by name at fence creation; consumer imports same name on cache miss | External-property query and semaphore create; name branch skips ordinary KMT-handle bookkeeping |
| DXVK `src/dxvk/dxvk_context.cpp:577-595,694-707,9609-9679` | Live conditional IDD/DWM copy consumer | Imported `resid`; same timeline/value | Bounded CPU wait immediately before copy | Default 32 ms, WUDFHost profile 500 ms; timeout copies anyway |
| DXVK `src/d3d11/d3d11_context.cpp:3584-3648` | Live staged-SRV freshness reader | slot value vs allocation-local last-refresh value | Flush when newly published data outruns staged copy | HPS scan per bound staged SRV/draw gate; global atomic count |
| DXVK `src/dxvk/dxvk_context.cpp:10066-10145` | Live conditional staged refresh reader | `kwaitOrdered`, producer value, last refreshed value | Skip unretired kwait value; stamp copied value | Multiple HPS lookups; default skip option on |
| DXVK `src/dxvk/dxvk_options.cpp:22-24`; `.h:55-86`; `src/util/config/config.cpp:26-35` | Live configuration | wait timeout, staged probes, kwait skip | Evaluated on device/profile creation | Can disable CPU wait; does not repair missing ordering |
| Mesa WSI `src/vulkan/wsi/wsi_helios_present_sync.h:5-35`; `.c:22-51` | Live Win32 WSI writer ABI | Same HPS2 bytes; WSI high-half fence IDs | Must remain byte-compatible with DXVK | Separate implementation; no reader |
| Mesa WSI `.c:57-62,64-114,116-163` | Live WSI ID/mapping/generation | high-bit ID; same file/path/PID start | First vehicle chain creates timeline/map | No ACL repair/startup sweep; mapping permanent |
| Mesa WSI `.c:247-313,315-349` | Live vehicle publish/release writers | image Venus `resid`, WSI named timeline/value | Publish before vehicle Present; release before image destroy | Silent table-full failure; same worst-case scan |
| Mesa WSI `wsi_common.c:2647-2665`; `wsi_common_private.h:288-299` | Live vehicle producer timeline | swapchain timeline and `++next_value` | Signal appended after app waits in pre-present submit | One extra timeline signal/present |
| Mesa WSI `wsi_common_win32.cpp:998-1054` | Live vehicle producer object owner | named exported opaque-Win32 timeline | Created per vehicle chain before worker startup | Creation failure latches software fallback |
| Mesa `src/virtio/vulkan/vn_queue.c:4356-4425`; `vn_renderer.h:245-308`; `vn_renderer_helios.c:4708-4865` | Live named-fence export/import implementation beneath DXVK/WSI | Vulkan name -> NT object path -> `D3DKMTShareObjects` / name open -> local monitored fence | Producer keeps named NT handle live; consumer opens by name then closes transient handle after local KMT open | Per fence/name kernel operations; `GENERIC_ALL`; cross-principal DACL supplied by producer |
| Mesa WSI `wsi_common_win32.cpp:2100-2147` | Live vehicle producer/cross-DLL callsite | exact image memory -> resid; HPS value; TLS source | Publish, then `set_source`, then DXGI Present | Any identity/publish/arm failure latches vehicle failure |
| Mesa WSI `wsi_common_win32.cpp:1591-1609` | Live vehicle image lifecycle | image `resid`, chain fence ID | Release before `wsi_destroy_image` | One HPS scan/image destruction |
| Mesa WSI `meson.build:30-31` | Live build dependency | compiles HPS writer into Win32 WSI | Build-time | Remove source entry atomically |
| Installer `packaging/windows/Install-Helios.ps1:198-217` | Live package state owner | ProgramData file and cross-principal ACL | Install/repair before graphics processes | Creates persistent global state |

### A.2 Live indirect dependencies and retired lookalikes

| Exact source | Classification | Why it is in, or out of, the retirement closure |
|---|---|---|
| Protocol `src/wddm.rs:328-388` | Live, indirect identity ABI | Defines `HeliosWddmOpenIdentity`, including the current host resource token and typed VidMm association propagated to an opener. Replace it with an immutable package/allocation-generation plus format/layout descriptor. Remove the token: the exact WDDM allocation is the identity and KMD alone owns its backing. |
| KMD `kmd_render/src/ddi/create_allocation.rs:2456-2505,3025-3095` | Live, indirect; create write valid, ordinary-open write invalid | The first range writes the `[in/out]` Create allocation bytes, which C12 permits and OpenResource later delivers. Retain/revise it. The second restamps allocation/resource buffers on an ordinary open even though C12 makes that input read-only; delete those writes and validate/read only. |
| UMD `src/forward.rs:83`; `src/forward/alloc.rs:87-88`; `src/forward/state.rs:340-371`; `src/forward/resource.rs:200-585,1300-1329,1470-1507` | Live, indirect identity/adoption reader and transport | Currently creates DXVK/Venus memory first and passes its backing token through `pfnAllocateCb`. Remove that direction. The outer UMD allocates first, KMD creates/owns the backing, and DXVK/Mesa binds an in-process wrapper for the exact returned/opened allocation. Actual batches reference allocation-list indices or GPUVAs, never a backing token. |
| UMD12 `src/forward12/resource12.rs:90-133,1450-1540,1583-2040,3308-3394` | Live, indirect identity/adoption creator and reverse-open refusal | Committed-resource creation already calls `pfnAllocateCb`; change it from UMD-adopted to KMD-created backing. KMD descriptor write-back enables DWM's D3D11 OpenResource without carrying identity. The refused `pfnOpenHeapAndResource` is D3D11-created-to-D3D12 and must be implemented separately; it is not DWM's D3D12-open carrier. |
| KMD `kmd_render/src/ddi/wddm_surface.rs:1-33,55-64`; root `ROADMAP.md:4083-4092` | Live, indirect WDDM/display-surface admission boundary | The current selected surface is WDDM 2.1; local source records that raising it to 3.2 makes build-28000 DWM enter the unimplemented Display-Core/MPO3 path and fail `E_NOTIMPL`. The atomic uplift must implement and register the complete C41 surface before changing the single coupled WDDM level; a version-only change is forbidden. |
| UMD `src/forward/present.rs:1917,2125-2217,2219-2367` | Live, indirect contradictory MPO capability and Present surface | The D3D11/DXGI UMD advertises 16 planes, 16x stretch/shrink, and RGB/BILINEAR/SHARED/IMMEDIATE even though no matching KMD overlay path exists, then forwards MPO Present allocations. Replace both capability functions and Present admission with the exact one-primary/no-transform C41 profile in the same package as KMD MPO3. |
| UMD `src/forward/transfer.rs:369-382`; `src/forward/tables.rs:267` | Live D3D11.1 Direct-Flip admission slot | The callback is correctly wired but currently returns false for every app/DWM resource pair. Replace that blanket refusal only after HWA2 and the matching KMD classic SetVidPn path exist: accept equal concrete D3D11 sources or C44's exact same-adapter runtime-primary sentinel branch, reject immediate/unknown/mismatched pairs without Escape, and leave each KMD display DDI as the independent actual-source/binding validator. |
| KMD `src/ddi/display.rs:1246-1670`; `src/lib.rs:195`; WDK 28000 `d3dkmddi.h:6489-6521` | Live classic Direct/Independent-Flip binding DDI | It receives the exact OS `hAllocation`, `VidPnSourceId`, contexts, address, and flags and can run at DIRQL. Replace any address-only/current shortcut with the shared C31 exact-allocation validator and candidate/current/backend-release state machine. It must be nonpageable and cannot rely on CheckMPO3 having run. |
| KMD `kmd_render/src/ddi/present_packet.rs:807-860`; `src/ddi/display.rs:248-260` | Live, indirect MPO Present union refusal | The union is decoded correctly, but the selected current KMD rejects `FlipWithMultiPlaneOverlay` because it registers no MPO3 interface. Implement the bounded MPO arm without reading `pAllocationList`; later SetMPO3 binds only the OS-supplied `ppContextData[].hAllocation` and owns the display/backend lease. |
| Mesa `src/virtio/vulkan/vn_renderer_helios.c:3582-3949` | Live, indirect external-memory/adoption/open/share implementation | Currently creates a KMT allocation that adopts a BO `res_id`, opens resources from NT handles, and returns/imports an ID. In record-only D3D mode replace adoption with an allocation wrapper supplied by the owning outer UMD; no token crosses the boundary. Retain and extend the public Vulkan D3D12-resource/heap import path used by native WSI. |
| KMD `src/ddi/create_allocation.rs:1548-1618,2035-2140,2330-2525` | Live, indirect allocation/backing ownership | Parses create-time adoption data and selects `AdoptedUmdResource`. Delete adoption: KMD creates a fresh backing while creating the exact WDDM allocation, holds it in generation-qualified allocation state, and returns only the immutable layout descriptor. |
| DXVK `src/d3d11/d3d11_texture.cpp:85-92`; `src/dxvk/dxvk_image.cpp:634-645`; `src/dxvk/dxvk_memory.cpp:350-425` | Live, indirect identity receiver/reopen | Stamps a typed Venus ID, retains a HANDLE-punned fallback, and may reopen an NT/KMT resource. Replace standalone/punned identity with the exact UMD wrapper/`hAllocation` plus validated immutable open data; preserve explicit public resource sharing outside the internal path. |
| DXVK `src/d3d9/d3d9_common_texture.cpp:666-700`; `src/d3d11/d3d11_texture.cpp:933-969`; `src/util/util_gdi.h:176-204,516`; `src/d3d9/meson.build:26` | Live/conditional forbidden Wine Escape metadata path; D3D9 is compiled and D3D11 is live when its KMT-only guard is false | Calls `D3DKMTEscape(D3DKMT_ESCAPE_UPDATE_RESOURCE_WINE)` with texture metadata and then falls through to a second private metadata mechanism. Delete the calls/types. D3D11 uses the ordinary WDDM create/share/open descriptor; D3D9 exact sharing is implemented through that documented surface or refused before handle export, with no fallback. |
| DXVK `src/wsi/win32/wsi_window_win32.cpp:215-220,259-264,320-325` | Live Win32 window/fullscreen path; forbidden Escape/heuristic side channel | Sends HWND-punned context plus desktop/window rectangles through `D3DKMT_ESCAPE_SET_PRESENT_RECT_WINE` during enter/leave/update. Delete all three calls and the private enum. Stock Win32/DXGI owns window geometry and presentation transitions; no KMD metadata replacement exists. |
| DXVK `src/util/util_shared_res.cpp:11-44`; `src/util/meson.build:1-14`; callers `src/d3d9/d3d9_common_texture.cpp:675-700`, `src/d3d11/d3d11_texture.cpp:945-969`, `src/d3d11/d3d11_device.cpp:2629-2637` | Live compiled Wine-private file/device metadata registry | Opens `\\.\SharedGpuResource` and uses OPEN/SET/GET `DeviceIoControl` records keyed by a KMT handle. This is an ad hoc lookup/metadata side channel equivalent to the forbidden global registry. Delete implementation, declarations, build entry, and callers; never replace it with another IOCTL, file, or service. |
| vkd3d `libs/vkd3d/d3dkmt.c:145-195`; `include/private/vkd3d_d3dkmt.h:121`; `libs/vkd3d/shared_metadata.c:24-65`; `libs/vkd3d/device.c:7682,7797`; `libs/vkd3d/meson.build:112` | Live Windows shared-resource Escape plus compiled Wine-device fallback | After ordinary NT resource open it stamps a Wine-private descriptor by Escape; shared create/open separately sets/gets metadata through `\\.\SharedGpuResource`. Delete both mechanisms and declarations. D3D12/D3D11 interop uses the exact ordinary WDDM allocation-private create/open contract in sections 10.3/12. |
| Mesa Gallium `src/gallium/frontends/d3d10umd/D3DKMT.cpp:393-397`; `targets/d3d10umd/d3d10.def.in:28`; `meson.options:134-137`; `icd/win-build/README.md:26-32` | Nonselected stub export, not an outbound call | The function only logs unsupported and returns `STATUS_NOT_IMPLEMENTED`; the Helios ICD build sets `-Dgallium-drivers=` and the option defaults false. It is not shipped in the selected package. Keep it classified as a declaration/stub, and make packaging/import scans prove no selected binary exports or calls it. |
| Mesa `include/drm-uapi/d3dkmthk.h`; released/generated WDK headers and UMD bindgen files | Declaration-only ABI sources | Literal `D3DKMTEscape`/Escape structure names in headers are not outbound calls. Retain only headers needed for ordinary KMT/WDDM declarations; packaging and binary-import scans—not a source-name ban—must prove that no selected artifact imports or invokes Escape. Generated declarations never authorize a compatibility path. |
| Mesa `src/virtio/vulkan/vn_wsi.c:133-162,239-242`; WSI `wsi_common_win32.cpp:2105-2138` | Live, indirect native producer identity | Obtains the exact WSI image resource ID and consumes it for HPS publication/vehicle source correlation. Delete this native vehicle path. The new layer owns an exact D3D12 shared texture handle per virtual image and the lower ICD opens it once through public external-memory semantics. |
| Mesa WSI `wsi_common.c:2996-3080`; Venus `vn_image.c:559-622,717-760` | Live upstream base-contract precedent, not HPS2 transport | Already implements singleton LOCAL device-group query results and consumes `VkImageSwapchainCreateInfoKHR`/`VkBindImageMemorySwapchainInfoKHR` for an ordinary lower WSI swapchain. Preserve the Vulkan-visible behavior but route the new layer's virtual swapchain through C45's real lower external image plus distinct dedicated import of exact `S[index]`; never pass the virtual swapchain handle into this existing lower lookup. |
| DXVK `src/dxvk/dxvk_helios_scanout_acquire.h:15-186`; `.cpp:20-124,140-410`; `src/dxvk/meson.build:50`; `src/dxvk/dxvk_device.cpp:39,82`; `.h:11,164-165,804` | Live, indirect reverse host-reader synchronization owner | Resolves process-global UMD ledger exports and the loaded ICD's `helios_venus_memory_res_id`, owns generation-keyed private Vulkan gate fences, and runs an event-or-10 ms polling signaler. Device teardown CPU-signals every armed value before joining. Delete the source/build entry, object, resolver, thread, gates, counters, and shutdown seam. Normal WDDM queue completion plus bounded exact current-plane ownership replace the edge. |
| DXVK `src/dxvk/dxvk_context.cpp:9390-9405,9829-10001`; `.h:1765-1814,1837-1864,2439-2459,2783-2792`; `src/d3d11/d3d11_context_imm.cpp:1205-1240` | Live, indirect reverse host-reader consumer and prearm path | On every list end, scans touched scanout images, converts backing memory to raw `resid`, scans the ledger, arms/waits a private fence, and carries prearm generation state around WindowedBlt snapshot submission. Delete all state/calls. Translated writes are actual WDDM submissions; the current display allocation is held by its exact KMD/QEMU plane binding until replacement/unbind plus backend old-reader release. |
| Mesa `src/virtio/vulkan/vn_renderer_helios.c:708-760` | Live, indirect raw-identity export | `helios_venus_memory_res_id(VkDeviceMemory)` exposes `base_bo->res_id` to the loaded-module scan. Delete this internal identity export and its bridge enum/resolver; no host ID crosses KMD. |
| UMD `src/scanout_acquire.rs:1-203,206-330,333-597,600-744`; `src/adapter.rs:229-233,505-519`; `src/device_funcs.rs:965-971`; `src/lib.rs:39`; `src/knobs.rs:23,94,205,285-289`; `bridge/dxvk_bridge.cpp:1371-1386`; `.h:159-165`; `src/bridge.rs:198,627-629` | Live, indirect global discovery/lifetime/diagnostic layer | Captures a runtime adapter globally, probes/maps the ledger and registers an event by Escape per device, owns a process-global `Mutex<Vec<DeviceEntry>>`, exports lookup/snapshot functions by name, and hands the event into DXVK. Teardown unregisters, closes, and unmaps after joining. Snapshot/present capability gates also ride this probe. Delete the complete module, bridge methods, init/teardown, knobs, exports, and capability coupling. |
| Protocol `src/escape.rs:58-67,430-570,750-755,777-792` | Live, indirect shared-memory ABI | Defines Escape verbs `0x000E/0x000F`, the 65-slot `{resid,generation,issued,retired}` page, map/event operations, capability bits, sizes, and ABI tests. Remove the verbs, structures, flags, and tests atomically; do not retain a compatibility parser. |
| KMD `src/adapter/read_ledger.rs:1-330`; `src/adapter/mod.rs:27,34-36,492,1058,1266-1269,1622-1626`; `src/adapter/locks.rs:8-9`; `src/device.rs:352`; `src/ddi/lifecycle.rs:169`; `src/mapping.rs:44` | Live, indirect KMD global page/event/lifetime owner | Allocates one adapter page, owns 65 generation-qualified raw-ID slots plus a 16-entry event table and leaf locks, resets/reclaims mappings/events, and preserves global generation across transport reset. Delete all of it. Replacement state is bounded by live native objects, WDDM contexts, pending display candidates, and active VidPn/MPO planes, all with exact kernel object references. |
| KMD `src/ddi/escape.rs:372-376,751-954` | Live, indirect forbidden map/event transport | Dispatches probe/map/unmap and event register/unregister, including owner-keyed mappings and object references. Delete both handlers and all counters/diagnostics; an old verb is rejected, never translated or accepted as a probe. |
| KMD `src/adapter/scanout.rs:1025-1054,1181-1187`; `src/virtio/gpu/mod.rs:570-620,5421,5538,5584,5640,5750`; `src/virtio/ctrl.rs:1177-1182,1236-1242` | Live, indirect host-read issue/terminal/allocation-reclaim path | Issues raw-`resid` tickets before asynchronous direct flush, WindowedBlt/snapshot work, and related host reads; a `ScanoutFlushToken` retires on exact host response or Drop. Remove the custom snapshot/direct-read routes. Each display read originates from an exact OS flip/MPO/plane request; candidate, current binding, latch, and backend release retain exact allocation references. |
| KMD logic `kmd_logic/src/lib.rs:4079-4460` and following `scanout_read_ledger_tests` | Live executable model | Models the fixed global slot/generation/issued/retired/reclaim state and its capacity/reset tests. Replace those tests with per-context submission completion, Core-0116 native-fence lifecycle, exact plane candidate/current/backend-release, reset, mode-transition, destruction, and multi-queue/swapchain models. |
| Mesa `wsi_common_win32.cpp:229-266,268-352,850-887` | Live, indirect | Constructs the recursive `ICD -> DXGI/D3D11 -> UMD -> DXVK -> ICD` vehicle and resolves three UMD exports. It must be removed, not re-fenced. |
| Mesa `wsi_common_win32.cpp:1940-2057,2233-2255` | Live fallback machinery, partly unreachable | Imports a vehicle-release named timeline and otherwise performs a serial bounded wait. `helios_umd_get_present_result` is now a stub, so the named-release fast path cannot arm. Remove both. |
| UMD `src/forward/vehicle.rs:1-210`; `src/vehicle_exports.rs:56-83` | Live vehicle/TLS and retired result producer | `set_source` and `wait_last_present` are live; `get_present_result` always reports no result after the old Rust HPS producer was retired. |
| UMD `src/bridge.rs:1-17,414-428`; `src/forward/present.rs:1460-1464` | Documentation of retired code | Explicitly labels `present_sync_publish`, `present_sync_fence_id`, and kwait arm as retired. Do not resurrect them or count them as current writers. |
| UMD `bridge/bridge_icd_exports.cpp:308-329,534-546`; `.h:43-45` | Live, indirect | Resolves the private Mesa present-stream registration export used beside HPS publication. |
| Mesa `src/virtio/vulkan/vn_queue.c:1688-1795,3540-3546`; `vn_queue.h:286-289`; `vn_renderer.h:300-308` | Live, indirect | Tags an exported semaphore signal with a registered present stream and unregisters it at semaphore destruction. |
| Mesa `src/virtio/vulkan/vn_renderer_helios.c:1520-1611,2069-2209` | Live, indirect and forbidden | Registers present streams and submits explicit queue-boundary work through `D3DKMTEscape`; current completion barrier uses `vkWaitRingSeqnoMESA`. Remove the present-stream and Escape carrier. |
| Protocol `src/escape.rs:29,68-71,139-160,513-516,709-716,723` | Live, indirect wire/telemetry | Defines `SUBMIT_VENUS`, `PRESENT_STREAM`, capability, ABI, and counters. The new DMA ABI belongs in a non-Escape WDDM protocol module. |
| KMD `src/ddi/escape.rs:330-342,1125-1157,1195-1249` | Live, indirect and forbidden | Accepts stream register/unregister and Venus submit Escapes. These verbs cannot remain as fallback. |
| KMD `src/device.rs:91,175-185,360,410`; `src/ddi/display.rs:312-320,605,972` | Live, indirect | Stashes and consumes a present-stream marker around Present. Delete after real DMA work supplies scheduler ordering. |
| KMD `src/adapter/scanout.rs:243-260,403`; `src/virtio/ctrl.rs:1323-1419` | Live, indirect | Converts markers to stream boundaries and attaches them to asynchronous Escape submits. |
| KMD `src/virtio/gpu/mod.rs:1725-1950,2114-2143,3683-3821,4761-5177,5869-6220,6332-6574` | Live, indirect | Owns present-stream slots and adapter-global, cross-lane `wddm_pending` correlation/FIFO. Replace with exact per-runtime-context `SubmissionFenceId` retirement keyed by the real host batch plus bounded exact display candidate/current/backend-release state; no adapter-global cross-lane correlation. |
| KMD `src/virtio/counters.rs:432-493`; `src/ddi/interrupt.rs:124`; `src/ddi/scheduler.rs:122` | Live, indirect diagnostics/lifecycle | Counters and reset drains for present streams; replace with context DMA submit/retire/reject counters. |
| UMD12 `src/forward12/queue.rs:2589-2604,2700-2905,2931-3497`; `present12.rs:88-118,220-570`; `fence.rs:1-885` | Live adjacent path, no HPS writer | Current ECL/present marker logic correlates separate ICD work with a proxy, while fence code builds an engine shadow because Core <=0110 exposed no native object. Delete both correctness paths. Bind each API queue to its exact runtime virtual context, submit the sealed actual batch with ordinary `pfnSubmitCommandCb`/`WrittenPrimaries`, and map Core-0116 native `HFENCE` objects to exact-context callbacks. Present returns no HWQueue dependency. |
| vkd3d-proton `libs/vkd3d/command.c:24340-24670,25179-25364`; `libs/vkd3d/vkd3d_private.h:3727-3818` | Live adjacent producer | Queue admission and immutable worker state are the insertion points for ordered record-only batch sealing plus an exact resource-use manifest. They must stop executing a lower Vulkan queue. D3D12 Queue Wait/Signal exists solely on the bound outer context/native fence callbacks. |
| QEMU `hw/display/virtio-gpu-virgl.c:1217-1397` | Live host completion | Context/ring fences and asynchronous callback are the suitable completion source for WDDM DMA retirement. |
| `tools/read_ledger_dump.c:1-320`; `tools/window_burst_capture.ps1:246-296`; `CONFORMANCE.md:245` | Live diagnostic-only consumer/documentation | Maps and dumps the forbidden read-ledger Escape and invokes it during causal capture. Delete/replace the tool and columns; observe only context submission/native-fence and exact plane latch/release events through ETW and bounded driver counters, never a userspace map. |
| Protocol `src/escape.rs:41-46,72-81,577-717`; KMD `src/ddi/escape.rs:361-365,489-558,966-1100`; `src/ddi/scanout_timeline.rs:1-227`; `tools/scanout_timeline_dump.c:1-268`; `tools/blob_capacity_probe.c`, `blob_map_size_probe.c`, `escape_owner_probe.c` | Live private diagnostic Escape surface | `QUERY_STATS` copies transport/present counters and `QUERY_SCANOUT_TIMELINE` copies a fixed KMD ring to a userspace tool through `D3DKMTEscape`; several probes consume the same ABI. Diagnostics are part of the replacement constraints, so read-only does not legalize this side channel. Delete the protocol/query handlers and private probes; retain bounded counters only as internal state, emit exact events through C42 ETW when enabled, and copy reset/black-screen data only through OS-invoked WDDM diagnostic callbacks. |
| `tools/d3d11_shared_blob_truth_probe.cpp`, `d3dkmt_alloc_probe.c`, `d3dkmt_sync_probe.cpp`, `vehicle_flipwait_probe.c`, `vidmm_tracking_probe.c` | Live/reachable development probes for old private context/blob/fence Escapes | Delete or rewrite each against the new ordinary WDDM allocation/context/native-fence contract. None may ship or serve as a verification fallback while it calls a Helios-private Escape. |
| QEMU `ui/egl-headless.c:93-233`; `ui/trace-events:189-190`; `hw/display/trace-events:234` | Live diagnostics, not synchronization | `helios_scanout_*` tracepoints report host scanout identity/pixels and are passive output. They may remain for verification because no producer reads them and they do not discover, gate, signal, wait, or own lifetime. Their `res` field is a host diagnostic, never the replacement allocation identity. |
| `docs/dx12/PRESENT.md:660-694`, `docs/dx12/ARCHITECTURE.md:789`, `docs/dx12/research/R7-present-swapchain.md:473-485`, `ROADMAP.md:238-249,2239-2244,2701-2745` | Documentation-only | Historical/current descriptions must be updated in the one-shot implementation, but are not executable readers/writers. |
| ignored `umd_clean/`, `umd*/target/`, `.claude/worktrees/` | Generated/snapshot, not tracked | Search hits are stale copies or generated bridges. They are excluded from live reachability and must not be edited as source. Clean builds must regenerate without retired symbols. |

### A.3 Graphics-Escape and replacement-enabling closure

Literal HPS removal alone would leave the same ad hoc transport underneath it.
These live indirect paths are therefore in the atomic retirement closure.

| Exact source | Current status and role | Required disposition |
|---|---|---|
| Protocol `src/escape.rs:29-40,58-71,110-279,430-570,703-755,777-792` | Live graphics context/submit/blob/wait/present-stream/read-ledger ABI, compatibility notes, capability bits, shared page, and statistics | Remove active graphics verbs and compatibility shapes with their callers; no replacement/fallback Escape or shared page |
| Mesa `vn_renderer_helios.c:1399-1464,1663-1689,1780-2001,2271-2309,2482-2551` | Live `D3DKMTEscape` helper plus context, submit, blob, wait, release, and attach senders | Delete the helper and all graphics callers. Translator instances hand a sealed record-only batch directly back to their owning UMD; native Vulkan WSI uses public external-memory/fence imports and the layer-owned D3D queue. |
| Mesa `vn_renderer_helios.c:3991,4163,4200,4243-4266,4314-4496,4630,5040-5042,5171-5184` | Live callsites in renderer submit, retire, shmem/BO, sync, and device lifecycle | Every caller maps to the resource/queue-lifetime operation in sections 10 and 17; no retry/polling compatibility arm |
| KMD `src/ddi/escape.rs:321-361,1100-1264,1266-1531` | Live dispatch and implementations for the same graphics verbs | Delete dispatch/handlers/telemetry once KMT paths are complete; unknown old verbs fail, never translate |
| UMD `src/scanout_acquire.rs:1-744`; DXVK `dxvk_helios_scanout_acquire.{h,cpp}` and `dxvk_context.cpp:9829-10001`; KMD `src/ddi/escape.rs:372-376,751-954`; KMD `adapter/read_ledger.rs` | Live `resid` discovery, global shared page/event, polling thread, CPU-to-Vulkan gate, and KMD ticket transport for host-reader lifetime | Delete the entire route. Translated writes retire on the exact WDDM context submission. Every direct display read is an exact OS plane binding whose allocation/backend lease survives until replacement/unbind plus backend release. No Escape, raw ID, mapped page, event, thread, scan, PresentId inference, or timeout remains. |
| KMD `src/virtio/gpu/mod.rs:5869-6220,6331-6593` | Live adapter-global WDDM boundary queue, immediate/overflow/rebase paths, and completion drain | Replace with exact per-context `SubmissionFenceId` state. Only exact host completion raises DMA-completed; overflow, timeout, reset, or stale callback fails/cancels and never fabricates completion. |
| Protocol `src/wddm.rs:1-215`; Mesa `vn_renderer_helios.c:1001-1024,3342-3421,3582-3949,4667-4705`; KMD `src/ddi/create_allocation.rs:2035-2290`; KMD `src/ddi/submit_command.rs:1312-1754` | Live partial KMT allocation/sync/Render and KMD-created-backing groundwork, but current internal sharing still adopts/exports IDs and command completion correlates separate Escape work | Retain documented create/open and KMD-created backing. Replace marker/adoption/Escape semantics with versioned actual-batch bytes, exact allocation/GPUVA resource references, D3D11 Render or ordinary D3D12 `pfnSubmitCommandCb`, and true host-backed WDDM completion. |
| KMD `src/ddi/lifecycle.rs:95-102`; `src/ddi/query_adapter_info.rs:51-92`; `kmd_render/src/lib.rs:170` | Live StartDevice currently ignores `DXGK_START_INFO`; private adapter query is unsupported; a stub HWQueue callback is registered | Store exact adapter LUID; implement only documented WDDM-3.2 native-fence query/feature/DDIs; remove the HWQueue callback/cap; expose no private Escape query. |
| UMD12 `src/device12.rs:291-307`; vkd3d `libs/d3d12core/helios_entry.c:168-185` | Live zero-LUID / `VK_NULL_HANDLE` selection | Select the lower Vulkan physical device by the exact runtime adapter LUID plus public external-memory device/driver UUID compatibility; mismatch is fatal. |
| `packaging/windows/Install-Helios.ps1:114,295-307` versus legacy `kmd/helios_kmd.inx:53` | Live package explicitly installs `helios_kmd_render`; `kmd/` is the older System/IOCTL driver and is not selected by this package | Change active `kmd_render`; never revive legacy IOCTL graphics submission; archive stale documentation/build references explicitly |

### A.4 Residual literal and symbol-search closure

The final producer-to-consumer and consumer-to-producer searches found the
following non-HPS filenames whose state would otherwise be easy to miss. They
are explicit members of the one-shot closure or explicit retired lookalikes;
none is an alternative discovery or fallback channel.

| Exact source | Reachability and current role | Atomic disposition |
|---|---|---|
| `docs/archive/**` (including `REFACTOR_REVIEW.md`, `ARCH.md`, `DISPLAY.md`, `ICD.md`, `KMD.md`, `OVERVIEW.md`, the Gate/Phase handovers, and the historical WDDM/Venus design files); `docs/dx12/research/R3-vkd3d-internals.md` | Documentation-only residual literal hits. They describe earlier Escape, shared-file, IOCTL, or whole-blob designs and are neither built nor runtime-reachable. | Keep them explicitly historical or archive/remove them during the one-shot documentation update. They supply no authority for a compatibility branch and are excluded from live-source callsite counts. |
| Standard virtio declarations `protocol/src/virtio_gpu.rs`, Mesa `src/virtio/protocols/protocols/kumquat_gpu_protocol.rs`, and alternate QEMU module `hw/display/virtio-gpu-rutabaga.c` | Declaration/alternate-backend `RESOURCE_MAP_BLOB` literals, not HPS2 or `D3DKMTEscape` callsites. The selected Helios host endpoint is the separately inventoried virgl path. | Retain standards declarations needed by unrelated backends, but the selected HVM1/HPM1 byte-authority path may not call or fall back to whole-resource map-blob identity. Package/backend tests must prove the alternate rutabaga module is not selected for this Helios generation. |
| Development probe `icd/win-build/helios_vk_dev.c`; legacy unselected KMD `kmd/src/{pnp.rs,wdf.rs,virtio/gpu.rs}` | The probe contains MAP_BLOB diagnostics only. The old System-class KMD contains the previously inventoried IOCTL/MAP_BLOB implementation but is not the package selected by `Install-Helios.ps1`. | Rewrite/archive the probe and archive/remove the obsolete KMD package with its IOCTL protocol. Neither is an implementation source or fallback for the selected `kmd_render` package. |
| Protocol `protocol/src/ioctl.rs:1-14,54-130`; legacy KMD `kmd/src/ioctl.rs:23-25,83-90,145-423`; installer `packaging/windows/Install-Helios.ps1:114,295-307` | The IOCTL constants and old KMD handlers are live source in the legacy System-class package, but the active installer selects `helios_kmd_render`. They are not a live HPS2 reader/writer and are not the installed WDDM transport. | Archive/remove the obsolete `kmd/` graphics package and delete the protocol IOCTL ABI in the same source-generation change. Never reuse `SUBMIT_VENUS`, `WAIT_FENCE`, or `PRESENT_BLOB` as the new carrier. |
| Mesa `src/virtio/vulkan/vn_renderer_helios.c:95-106,531-538,1333-1397,5076-5079,5235` | Retired/unreachable IOCTL residue in the active ICD: the SetupDi/device-interface open is gone, `dev` remains `INVALID_HANDLE_VALUE`, and no live callsite invokes `helios_ioctl`; current graphics traffic uses the separately inventoried D3DKMT/Escape/KMT paths. | Delete constants, stats, dead helper, handle field/lock assumptions, and stale file banner. Do not retain a fail-clean IOCTL fallback. |
| UMD12 `umd12/bridge/vkd3d_bridge.cpp:306-525,669-780`; `umd12/src/bridge12.rs:280-480,630-720` | Live loaded-module discovery of thread/process Venus context, raw memory ID/allocation metadata/ownership exports, and sampled lower-queue wire fences. A missing/old export currently degrades to zero identity or zero boundary. | Delete raw-ID/export resolution, queue drain/sample, and graceful old-ICD branches. Replace the bridge surface with package-matched direct record-only dispatch and sealed batch/resource-use output; any generation mismatch fails device creation. |
| UMD12 `umd12/src/forward12/identity12.rs:1-123,162-240,297-418`; `resource12.rs:193,222,765-797,1618-2089,2956-2968,3884-3935` | Live process-global `OnceLock<Mutex<HashMap>>` keyed by an engine COM address and indexed by raw `venus_res_id`; it owns allocation/adoption correlation and collision recovery. | Delete the registry, raw-ID index, private `1 << 30` export/adopt flag, replacement-on-address-reuse behavior, and its counters. Store exact lifetime-bound WDDM allocation/resource/GPUVA associations in the owning DDI objects. |
| vkd3d-proton `libs/vkd3d/vkd3d_private.h:1088-1119` and the resource/heap users reached from the UMD12 row | Live private heap flag forces a Vulkan-export allocation solely so KMD can adopt its Venus resource ID. | Delete the private API bit and adoption direction. The KMD-created WDDM allocation/backing is wrapped by record-only translation; it is never rediscovered through a Vulkan memory ID. |
| UMD11 `umd/src/knobs.rs:78-130`; KMD `kmd_render/src/ddi/scanout_trace.rs:959-963,1050-1055`; `submit_command.rs:1470-1665` | Live kill switches, counters, present-stream marker, snapshot descriptor, raw resource ID, and fallback observability surrounding the HPS/read-ledger generation. | Remove vehicle/read-ledger/snapshot/present-stream gates and their counters with the mechanisms. Replace them with versioned actual-submit/native-fence/plane-latch/release diagnostics; no old knob may select an old path. |
| Mesa `src/virtio/vulkan/vn_renderer.h:245-308`; `vn_queue.c:1688-1795,3540-3546`; `vn_renderer_helios.c:711-722,753-920,1520-1611,2047-2263` | Live named sync, present-stream registration, raw memory identity/ownership exports, and lower-queue wire-fence sampling. These are indirect synchronization/identity owners even though their symbols do not contain `HPS2`. | Remove the private named/present-stream/raw-ID/wire-fence bridge. Retain only public external D3D12 memory/fence support for native WSI and private direct record-only translator dispatch. |
| Mesa `src/virtio/vulkan/vn_renderer.h:11-29,83-104,175-179`; `vn_renderer_util.h:21-35`; `vn_ring.c:298-344,701-730,887-958`; `vn_instance.c:177-184,309-342`; `vn_device.c:83-104,573-582,721-734`; `vn_buffer.c:262-300`; `vn_image.c:386-407`; `vn_pipeline.c:685-687,1801-1817`; `vn_query_pool.c:201-260`; `vn_queue.c:1494-1504,2796-2879`; `vn_renderer_helios.c:2005-2023,2120-2145` | Live normal-ICD execution and synchronous-control substrate: renderer shmem/BO objects expose raw `res_id`; indirect command and reply descriptions serialize it; reply completion samples a shared ring in a polling loop; ring zero is explicitly a CPU/decode timeline and current Helios rejects it as a GPU boundary, while each real queue owns a nonzero renderer timeline. Synchronous device/object/query/idle calls currently depend on that renderer path. | Replace the Windows paths with C50-C67 HVC1/HNR2/HVM1/HTS1/HQA1/HQC1/HOB1/HOS1/HPM1/HVR1: one session-owned Venus namespace per `vn_instance`, CPU/decode-only ring 0, one nonzero non-recycled GPU ring per physical endpoint, zero-placeholder typed operands, KMD output patches/final HPM1 physical capabilities, finite correctly typed host fences, immutable bounded reply snapshots, pure-control C51 replies, exact outer-context HQC1 joins for GPU-dependent translated calls, KMD-arrival-order endpoint FIFOs, authoritative virtual-address translation, exact descriptor/object reachability, and bounded immutable HOB1 retirement. Delete `vn_ring` and every user-visible host resource ID or shared head/status sample. |
| KMD `src/adapter/mod.rs:395-409`; `src/ddi/lifecycle.rs:216-233`; `src/device.rs:20-48,247-274,382-440`; `src/ddi/escape.rs:1195-1264`; `src/virtio/venus/bringup.rs:73-176`; `src/virtio/venus/ring.rs:571-740` | Live adapter-global Venus context/ring plus a stub `DxgkDdiCreateContext`; normal command submission still uses `SUBMIT_VENUS` Escape, HOST3D ring/reply mappings, raw resource IDs, and spin/sleep head waits. The KMD process/device/context objects do not yet own HTS1 sessions or validate HQA1. | Move the Venus namespace from adapter-global state to one refcounted HTS1 session per `vn_instance`; keep only a bounded creation-time session list in the exact ProcessContext, validate HQA1 once, and retain direct session/endpoint refs in outer contexts. Implement HVC1 legacy contexts, finite HNR2 Render/SubmitCommand completion, C51/HQC1 lifetime, and remove bring-up rings, polling, Escape submission, and adapter-global object identity. Ring 0 is retained only as a host control timeline, never as shared memory or a GPU-completion inference. |
| QEMU `hw/display/virtio-gpu-pci.c:29-58`, `virtio-vga.c:101-153`, `virtio-gpu-gl.c:145-160`; KMD `src/virtio/pci_caps.rs:15-35,101-140`; `src/adapter/segments.rs:36-58`; `src/ddi/segment_table.rs:41-86` | Live BAR/segment foundation: QEMU realizes the prefetchable 64-bit host-visible aperture; GL owns its background mapping; KMD discovers the base/length and reports the current segments. Current policy is blob-oriented and has no authoritative local physical-placement service. | Retain the complete hardware range; expose aperture id 1 only for paging/DMA and HLM1 memory id 2 as the direct linear CPU-visible BAR only after HPM1 admission. Fail cold adapter start on any capability/base/length/flag mismatch. GL/KMD/QEMU teardown drains paging, placements, and host jobs before destroying hostmem. |
| KMD `src/ddi/build_paging_buffer.rs:804-1229,1400-1475`; `src/ddi/submit_command.rs:1320-1877`; `src/ddi/cpu_host_aperture.rs:1-29,247-291,294-487`; `src/ddi/query_adapter_info.rs:750-777,835-885`; `src/virtio/ctrl.rs:861-911,1038-1119` | Live paging/mapping substrate CPU-copies/no-ops several paging classes, declares render physical Patch a no-op, has a whole-blob CPU-host-aperture mapper that ignores exact page semantics, and globally scans/retries/sleeps around one raw resource ID. | Implement C53 render prepatch/output/repeatable Patch and C56 self-contained HPM1 paging DMA with mandatory side-effect-free paging Patch, exact MDL/ADL/ranges/epochs, and completion only after host terminal acknowledgement. Delete `cpu_host_aperture.rs`, both callback registrations, whole-blob mapping, scan/sleep/raw-ID paths, and report the exact no-HAP HLM1 segment flags. |
| QEMU `hw/display/virtio-gpu-virgl.c:190-343,638-667,845-1004,1222-1299`; `include/standard-headers/linux/virtio_gpu.h:433-458` | Live host endpoint. Standard blob mapping installs one whole MemoryRegion and tears it down asynchronously; finite SUBMIT_3D already copies an exact stream; context completion is keyed by `(ctx_id,ring_idx,fence_id)`. There is no WDDM physical-placement/paging-epoch protocol. | Keep finite submit/context-fence precedent. Add HPM1 paging DMA, exact HLM1 BAR-offset binding and ordinary-aperture system-run handling, atomic epoch publication, address-space/RCU completion before paging acknowledgement, and generation-safe reset. HNR2 operands resolve only through patched HPM1 capabilities; HVM1 never uses standard whole-resource RESOURCE_MAP_BLOB. |
| `tools/vk_ring_fence_probe.cpp:1-26,58-70`; `icd/win-build/helios_vk_present.c:22,53,537-544`; Looking Glass `idd/LGIdd/CHeliosSink.cpp:26,567` and `host/platform/Windows/src/helios_sink.c:37,661` | Diagnostic or alternate direct-blob paths, not live HPS2 participants. Their comments and probes encode the retired `SUBMIT_VENUS`/`WAIT_FENCE`, global-name, or `PRESENT_BLOB` model and can mislead a later implementation audit. | Rewrite the verification probe for actual WDDM submission/Core-0116/native-fence causality. Delete or explicitly archive direct-blob helpers when the legacy IOCTL package is archived; none ships in the new generation. |

## Appendix B. Current producer/consumer sequences

### B.1 Vulkan WSI vehicle producer to D3D11 vehicle/DWM consumer

```mermaid
sequenceDiagram
    participant App as Vulkan app
    participant ICD1 as Mesa/Venus ICD #1
    participant HPS as HPS2 file
    participant DXGI as DXGI/D3D11 vehicle
    participant UMD as Helios D3D11 UMD + DXVK
    participant ICD2 as Mesa/Venus ICD #2
    participant KMD as dxgkrnl/KMD
    participant DWM
    App->>ICD1: vkQueuePresentKHR
    ICD1->>ICD1: pre-present submit signals named timeline value N
    ICD1->>HPS: publish(frame resid, pid/start, WSI fence id, N)
    ICD1->>UMD: helios_umd_set_present_source(resid, N, typed allocation)
    ICD1->>DXGI: vehicle swapchain Present
    DXGI->>UMD: pfnPresent / vehicle_present_prepare
    UMD->>ICD2: import resid and record copy
    ICD2->>HPS: lookup(resid)
    ICD2->>ICD2: import named timeline; wait N before copy
    UMD->>KMD: Present vehicle backbuffer
    KMD->>DWM: runtime-owned shared allocation / present token
    DWM->>UMD: open/sample through its D3D11 device as needed
    Note over ICD1,ICD2: Recursive ICD -> D3D -> UMD -> DXVK -> ICD path
```

The WSI image destruction path releases HPS. Its next-image recycle falls back
to `helios_umd_wait_last_present` because the old return-fence producer is
retired (`wsi_common_win32.cpp:2233-2255`, `vehicle.rs:134-176`).

### B.2 D3D11/DXVK producer and D3D11/DXVK consumer

```mermaid
sequenceDiagram
    participant P as Producer D3D11 process
    participant PU as Producer UMD/DXVK/ICD
    participant HPS as HPS2 file
    participant W as WDDM/DXGI
    participant C as DWM or IDD consumer
    participant CU as Consumer D3D11 UMD/DXVK/ICD
    P->>PU: render then Present
    PU->>PU: record named timeline signal N; Flush
    PU->>HPS: publish(exact consumed resid, pid/start, fence id, N)
    PU->>W: pfnPresentCb / Render marker
    W->>C: open/share presented allocation
    C->>CU: bind/sample or copy imported resource
    CU->>HPS: linear lookup(resid)
    CU->>CU: open/cache named producer timeline
    CU->>CU: GPU wait N before list sample
    alt staged copy path
      CU->>CU: bounded CPU wait N; timeout copies anyway
    end
```

### B.3 DX12/vkd3d producer to DWM D3D11 composition

```mermaid
sequenceDiagram
    participant A as DX12 app
    participant U12 as Helios DX12 UMD
    participant VKD as vkd3d-proton
    participant ICD as Mesa/Venus ICD
    participant K as dxgkrnl/KMD
    participant D as DWM
    participant U11 as DWM D3D11 UMD/DXVK/ICD
    A->>U12: ExecuteCommandLists / Present
    U12->>VKD: translated D3D12 work
    VKD->>ICD: vkQueueSubmit (separate ICD KMT/Escape stream)
    U12->>K: pfnRender marker + Present identity
    Note over U12,K: Marker samples/correlates a separate Venus boundary; it is not the actual work
    K->>D: runtime opens presented allocation after Present
    D->>U11: compose shared allocation
    U11->>U11: HPS lookup has no DX12 HPS writer for this resource
    Note over ICD,U11: Current tree has no complete producer-fence/value carrier across this path
```

This is the decisive current gap: UMD12/vkd3d has no live HPS2 writer. The
present-marker/present-stream machinery tries to delay WDDM completion behind a
separately submitted Venus stream, but it does not turn that work into the
runtime context’s DMA submission and still depends on forbidden Escapes.

### B.4 Resource destruction and stale-producer cleanup

```mermaid
sequenceDiagram
    participant R as Producer allocation/image
    participant H as HPS slot
    participant C as Consumer cache
    participant P as Later process/writer
    alt orderly destruction
      R->>H: release(resid, own pid/start/fence id)
      H->>H: odd epoch; clear payload; even epoch; resid=0
    else producer crash
      R--xH: mapping and populated slot persist
      C->>H: may read stale tuple
      C->>C: named-fence open fails; retry after 256 calls
      P->>P: OpenProcess + creation-time check
      P->>H: reclaim only if exact generation is proven dead
    end
```

### B.5 Current host-reader-to-producer-reuse guard

```mermaid
sequenceDiagram
    participant K as KMD scanout/read path
    participant H as QEMU/virgl host reader
    participant L as 65-slot KMD read ledger
    participant U as D3D11 UMD global registry
    participant X as DXVK signaler/context
    participant I as Mesa ICD
    K->>L: issue(raw resid) -> {slot,generation,issued}
    K->>H: enqueue asynchronous flush/read with ticket
    U->>K: MAP_READ_LEDGER + SCANOUT_EVENT Escapes
    U->>U: expose mapped lookup/snapshot functions by name
    X->>I: loaded-module helios_venus_memory_res_id(VkDeviceMemory)
    X->>U: lookup(resid); scan mapped slots
    alt issued > retired
      X->>X: create/arm private Vulkan timeline gate
      X->>X: queue producer wait(issued)
    end
    loop event or every 10 ms
      X->>U: snapshot all slots
    end
    H-->>K: exact success/error/cancel response
    K->>L: retire(ticket) exactly once; signal registered events
    X->>X: CPU-signal gate to retired value
    Note over K,X: Missing slot, export, gate, or capability can run the write ungated
```

This reverse edge is live and must not be mistaken for HPS2 proper merely
because its file name and magic differ. It has the same forbidden architectural
properties: a raw host ID, adapter-global shared table, Escape discovery and
lifetime registration, process-global lookup, polling, and a userspace fence
manufactured after the reader already exists. Section 10.9 replaces its
ordering responsibility inside KMD on the exact WDDM allocation; section 17
removes every displayed reader/writer in the same activation.

---

# SUPERSEDED CLAIMS INDEX

**Appended 2026-08-10. Read this before implementing any section above.**

This file is a static-analysis snapshot written without the ability to run
anything, so its "hard constraints" encode what its author could prove from
headers. Several have since been measured false, and one whole protocol has been
declined by the owner. The body above is **frozen and deliberately not edited**,
so this index is the only correction that travels with it.

⛔ **Why the index is appended rather than woven in.** Line numbers in this file
are load-bearing: `docs/retirement/lane-*.md` and `docs/retirement/K4-CONTRACT.md`
cite ranges into it. Inserting a 10-line banner at the top on 2026-08-10 shifted
every line after 8 by +10 and silently invalidated **every** citation in
`lane-kmd-core.md` §1 — discovered a session later, after an agent had already
read ten lines of the wrong text. **Any future edit to the body must preserve the
file's line count, or fix every citation in the same commit.** Appending here
shifts nothing.

## The index

| Claim in this file | Superseded by | What is true instead |
|---|---|---|
| Package minimum is **Windows 11 26H1 / build 28000**, Core DDI 0116 "no fallback" (header, §2:124-125, §18.1:4649) | **F1** | Core DDI **0116 negotiates on build 26100**. The 28000 figure was an artifact of what WDK 26100's `d3d12umddi.h` *declares*, not of the kernel. Measured on the inbox runtime, both arms. |
| HLM1's two-segment shape will fail `AddAdapter` with **Code 43** (the CLAUDE.md invariant this file inherited) | **F2** | It does **not**. `BarSegFlags=0x02` (`CpuVisible=1`, `SupportsCpuHostAperture=0`) starts `OK / CM_PROB_NONE` and `helios_paintcap` shows a fully composited live desktop. The KMD memory lane was never blocked by this. |
| ⛔ **HPM1 exists at all**: §10.7's paging-DMA protocol, the `HPM1` rows of the C52-C64 constraint table (`:646-650`), the QEMU paging executor in §17.7, and **every requirement that `DxgkDdiStartDevice` negotiate HPM1 before `QUERYSEGMENT4` may expose HLM1** (`:1975`, `:4403`) | **F5** (owner decision) | ⛔ **HPM1 is DECLINED, not deferred.** A new protocol inside a stack we do not own (QEMU) is permanent maintenance cost — ~4,500 lines concentrated in one upstream file, versus the scanout commits which are light and cherry-pickable. **The KMD does the work instead.** C63 is satisfied guest-side by bounds-checked subtraction against the §C65 command pool's own kernel mapping (*stricter* than HPM1 on C63's own invariant); C55's late-binding benefit is unrealized because nothing evicts today. ⇒ **No lane has a QEMU dependency. Do not add one.** The negotiation *precondition* on HLM1 dies with the protocol — and F2 independently shows the segment needs no negotiation. K2 and K3 are **rescoped guest-side, not blocked.** |
| §18.1's WDDM-3.2 slot audit is unproven | **F6** | Proven on the target in **both** halves, including the refusal path: flipping one row yields `CM_PROB_FAILED_DRIVER_ENTRY` with our own `0xC0000182`, and the breadcrumb decodes to the flipped row's index *and* direction. |
| §10.3's WSI chain is a design proposal | **F7** (+ addenda) | The **image** half runs end to end on the target: four D3D12 committed textures import per swapchain through `CreateSharedHandle` → `vkGetMemoryWin32HandlePropertiesKHR` → dedicated import → bind → the private tag call. |
| §10.3's **Ready/Release fences are imported from `ID3D12Fence`** (`:242`, C16 `:610`, C18 `:612`, C19 `:613`) | **F8** | ⛔ **Impossible, and not because of Helios.** An `ID3D12Fence` shared handle is refused by `D3DKMTOpenSyncObjectFromNtHandle2` on **every** adapter measured, including both Microsoft Basic Render Driver ones. The direction **reverses**: `ID3D12Device::OpenSharedHandle` *accepts* a D3DKMT monitored fence the ICD creates and exports, on Helios. So the ICD creates and exports; the D3D12 side opens. |
| "the native-fence DDI surface is complete" as an activation gate (`OWNERSHIP.md` §3) | **F9** | Four DDI slots are registered; the `DXGKQAITYPE_NATIVE_FENCE_CAPS` arm, `DXGK_FEATURE_NATIVE_FENCE` enablement, the `VIDSCHCAPS` bits and `DXGK_INTERRUPT_NATIVE_FENCE_SIGNALED` reporting are **absent** — nine symbols with no caller. Sequencing (they belong to K8/K9), but the gate is **not met**. |
| §10.3: the KMD "writes a versioned, immutable descriptor into the `[in/out]` buffer" — with **no statement of what a UMD may legally send** | **`K4-CONTRACT.md` §1** | HWA2 is a **two-stage** record like HVM1 and HOC1: `validate_create_input` / `validate_create_output`. The UMD supplies the geometry (which the kernel cannot invent) with `allocation_generation` and the two KMD-owned flag bits zero; the KMD validates in full and writes all 168 bytes. |
| §10.3 offset 24: `byte_size` is "the exact backing extent" | **`K4-CONTRACT.md` §1.3** | It is the **resource's** extent, UMD-supplied and echoed — *except* for allocations the KMD authored itself (`STANDARD`), where the KMD replaces its own pre-create estimate with the host's authoritative answer. Enforcing the estimate refused every OS shared primary; the pre-retirement code overwrote it and was right to. |
| §10.3: a host `resid` "may live solely inside the KMD allocation object" reads as a constraint already satisfied | **`K4-CONTRACT.md` §5** | It is a **mechanism change nobody has built**. HWA2 carries no host `resid` and no Vulkan memory-type index, and the ICD's import consumed both — so the import and export **refuse** until mesa unit **A3** (+K6) lands. Every consumer names A3 in its refusal. |
| `GlobalVidMmTracker` is "folded into HWA2's tracking-kind fields" (`protocol/src/wddm_legacy.rs`, since corrected) | **`K4-CONTRACT.md` §6** | HWA2 has **no** tracking kind, cookie, global-share field or tracker bit. The mechanism has **no successor**; it dies with UMD-backing adoption. |
| Residual display-lifetime phrases saying KMD/QEMU retain a plane lease until replacement/unbind plus backend release | **2026-08-11 owner amendment, lines 14-19** | QEMU is immutable and supplies no custom callback or acknowledgement. KMD owns candidate/current backing references; one exact successful standard fenced nonzero `SET_SCANOUT_BLOB` response latches the candidate and releases the prior backing. Explicit unbind first replaces with permanent KMD black parking; optional `SET(0)` does not release parking, which remains until later nonzero replacement or reset. |
## Standing reading rule

Two of this file's load-bearing assumptions were falsified by a single cheap
experiment each, and a third was withdrawn by the owner on maintenance grounds.
⇒ **Run the experiment before treating a claim here as a wall**, and prefer a
runtime check over a constant that encodes a guess about another lane's
schedule. `FINDINGS.md` is where the answers go.
