# HPS2 replacement on the existing WDDM surface

**Implementation status:** the bounded source cutover is implemented and built.
The investigation below records the starting point; the implementation and
runtime acceptance section at the end records the current contract. The .266
HE12 v2 execution repair and GPU feedback completion workaround are deployed
with stock virglrenderer. All four native ordering cases pass; the owner confirms
realtime Time Spy shadows are fixed and observed approximately 100 FPS in their
benchmark. General mixed-API and lifecycle acceptance remain open. See
[EXECUTION_SYNC.md](dx12/EXECUTION_SYNC.md) and the current task in ROADMAP.md.

Investigation, 2026-09-05, in `/home/rupansh/helios-vgpu-dx12`.
Inspected root `ba3083917af8a25c76c88eb6727014ae25c6e75c`,
DXVK `8148189e699a53997941852a4690bab656ba9419`,
Mesa `4468ef026359f27b6eeb8d38e45c8e48f0aff13d`, and
vkd3d `f3918d5e40a0e8201b223c001bd5589f667174aa`.
Rediscover these pointers before implementation. ROADMAP explicitly excludes
the abandoned /home/rupansh/helios-vgpu tree as an implementation reference.

## Decision and feasibility

**Use allocation-bound producer completion state in the KMD, its existing
read-only mapping/event infrastructure, and a small common ICD interface.**
Replace WSI's in-process producer discovery with an explicit dependency passed
through its existing helper interface. Keep WddmSurface::Wddm2_1GpuMmu.

This is the smallest credible replacement found for the actual HPS2 consumers.
It retains a private driver synchronization protocol. The KMD owns identity,
publication validation, completion and cleanup; user mode observes status and
requests waits. It needs no new WDDM submission executor, memory manager,
presenter, Vulkan layer or D3D11 DDI table.

**Planning estimate: approximately one concentrated implementation day for
the vertical source change, focused tests and builds.** A few hours is not a
credible estimate for a fully validated migration. Windows runtime acceptance,
fault testing and paired performance measurements may extend it. The estimate belongs to the original investigation; implementation and deployed
observations are recorded below. It is not a runtime acceptance claim.

The earlier queue-admission recommendation was too broad for the requested
budget. Real rendering currently reaches Venus before its outer WDDM metadata
packets. Making those packets control Vulkan dispatch, and proving D3D11
acquisition-notification coverage, is a separate subsystem project.

| Option | Fit for this task |
|---|---|
| Pagefile-backed named mapping or common DLL around HPS2 | Quick mechanical change; retains the resource-to-producer directory and its protocol problems |
| Allocation-bound KMD completion state, cached read-only status, event waits | Recommended bounded replacement; reuses live primitives and keeps per-draw work in user mode |
| Per-resource named fences alone | Still lacks the pending target, producer association and staged-image change notification |
| Runtime-controlled Vulkan dispatch and resource ownership throughout the stack | Larger work; not required to replace this file's current mechanisms |
| WDDM 3.2/native fences | Outside scope; would also change the display/DDI contract |

## Mechanisms that must survive removal

The file is C:\ProgramData\Helios\helios_present_sync_v2.bin: a mapped
32-byte header plus 4096 32-byte slots, **not disk I/O per frame**. Its values
describe announced work; the separately named fence proves completion.

| Responsibility | Current source seam | Replacement |
|---|---|---|
| Publish the dependency for the resource the consumer reads | umd/bridge/dxvk_bridge.cpp, publish_present_order | Validated allocation binding and registered stream/value dependency |
| Order sampled imports | dxvk_context.cpp, heliosEmitImportedWaits | Resource/epoch dependency retained with the command list and enforced before submission |
| Order imported copy sources | heliosPresentWaitBeforeRefresh, including alias-image copies | Wait for the captured source epoch, or consume an explicit WSI dependency |
| Detect stale private images | d3d11_context.cpp, HeliosGateStagedSrvFreshness | Cached announced epoch versus last staged epoch, using mapped loads |
| Stamp refreshed contents | refreshHeliosStagedImages | Stamp the same epoch whose dependency the recorded copy consumed |
| Find foreign producer fences | heliosProducerFence, PID/start/fence-name cache | KMD completion state |
| WSI source-copy dependency | wsi_win32_queue_present_vehicle | Unnamed, retained NT semaphore handle and exact value through the existing helper seam |
| Lifetime and cleanup | DxvkResourceAllocation destructor; WSI image destruction | Allocation/open references, command-list references, terminal status and device teardown |
| File/ACL setup | packaging/windows/Install-Helios.ps1; DXVK mapping code | Remove HPS2 setup after all active consumers have replacements |
| Diagnostics | noteGateFlush, gateFlushCount | Ordinary local counters |

There are **two live publisher call sites**, despite the broad consumer
fan-out: the D3D11 bridge and Mesa WSI. UMD12/vkd3d currently publish nothing
to HPS2. Neither live publisher sets kwaitOrdered=true. That optional skip
branch is not an active producer contract to recreate; stale mapped rows can
still contain historical values until old processes are gone.

The dedicated Present-buffer read/write handshake, scanout-read ledger,
snapshot retention and WSI image recycling are separate contracts. Preserve
them. A producer-ready epoch does not mean a consumer has finished reading.

## Existing implementation and OS contracts

1. **Read-only mapping and cleanup already exist.**
   kmd_render/src/ddi/blob_map.rs::map_nonpaged_page_to_user_readonly uses
   the SEH mapping shim with read-only/non-executable protection.
   escape_map_read_ledger in ddi/escape.rs maps once per owning device;
   mapping.rs and device teardown reclaim user mappings.
   adapter/read_ledger.rs keeps backing memory alive across StopDevice until
   its mappings are gone. Reuse the mechanics, keeping producer status
   separate from scanout-reader accounting and its 65-slot ABI.

2. **Exact asynchronous completion already exists.**
   VirtioGpu::register_present_stream assigns owner/context/generation-scoped
   cookies. Submitted/retired values and marker-boundary logic distinguish
   announced work from host completion.
   vn_queue.c::vn_signal_win32_external_semaphore tags the batch following
   the exact queue ring sequence. Reuse that boundary and retirement, never
   the adapter's newest wire fence.

3. **Allocation lookup and pinning fit WDDM 2.1.**
   Microsoft's [AcquireHandleData contract](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmddi/nc-d3dkmddi-dxgkcb_acquirehandledata)
   and [ReleaseHandleData contract](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmddi/nc-d3dkmddi-dxgkcb_releasehandledata)
   date to WDDM 2.0 and allow a KMD reference to prevent concurrent allocation
   destruction. Both require IRQL at most APC_LEVEL. Use this build's WDK
   types and verify callback availability.
   [GetHandleData](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmddi/nc-d3dkmddi-dxgkcb_gethandledata)
   distinguishes global allocation data from device-specific open data.
   A bare lookup pointer must not escape its protected lifetime.

4. **The UMD seams already have allocation identity.**
   UMD11 resource create/open paths call stamp_dxvk_resource_kmt_handles with
   the actual allocation handle. Bind before state.rs's existing helper can
   substitute its resource handle for a zero allocation handle; that fallback
   is not valid input to an allocation lookup. UMD12's AllocationIdentity carries
   h_allocation; present12.rs resolves the exact presented resource and queue.
   Mesa's NT-memory open path also receives a local allocation handle.
   Extend those seams; PID, geometry, Venus ID and the last active device are
   not substitutes.

5. **DX12 already has a worker insertion point.**
   vkd3d-proton-helios/libs/vkd3d/command.c implements
   d3d12_command_queue_enqueue_callback and
   VKD3D_SUBMISSION_QUEUE_USING_CALLBACK. Its existing swapchain uses this
   for ordered presentation and wait-before-signal handling. It is internal,
   not an exported Helios API: add a narrow bridge entry to enqueue the
   retained completion operation. Do not recursively drain the queue from
   that worker or mistake sample-only queue locking for flushing engine work.

Ordinary monitored fences already work on this adapter.
[Microsoft's native-fence documentation](https://learn.microsoft.com/en-us/windows-hardware/drivers/display/native-gpu-fence-objects)
distinguishes WDDM 2.x CPU-managed waits from WDDM 3.2 native GPU waits. This
proposal uses the existing completion/event route and needs neither native
fence capabilities nor a display-version bump.

## Bounded design contract

### Bind once to the actual allocation

Expose one versioned ICD interface for binding, publication, status sampling,
waiting and release. UMD11/UMD12 provide the exact local WDDM allocation
handle at their resource boundary. Resolve/pin it through dxgkrnl in the
caller's context and validate the Helios adapter and relevant device/open
association. Scope the returned binding to that caller's live device and the
underlying allocation incarnation.

Different opens of the same allocation must reach the same completion state.
Allocation, device-specific open, resource and Venus handles are different
types. Do not cast between them or use the copied OpenAllocationContext::present
metadata as a lifetime pin. Callback reference release requires a PASSIVE/APC
cleanup path, including when completion is observed at DISPATCH.

For staged imports the binding belongs to the external source, even when
DXVK allocates a private destination. Backing rotation must rotate its binding.
Do not recover a missing binding through a global resource-ID search.

### Announce work and complete it at exact host retirement

Each bound resource conceptually exposes an allocation generation, announced
epoch, completed epoch and live/error status. KMD-private state connects each
pending epoch to a validated producer stream and value.

User mode never writes these fields. A cached slot/index plus generation
permits direct loads without per-draw scans or Escapes. Snapshot retries are
bounded; contention is not an unpublished/ready result. Capacity exhaustion
is an explicit error. Size the producer status storage independently of the
existing 65-slot scanout ledger.

Publication accepts an already committed producer operation, including an
operation enqueued on an engine worker whose tagged host submission has yet
to arrive. It also handles completion arriving before publication. Bind
publication and pending waits to the exact stream generation.

Completion advances only through a proven completed prefix on that resource.
Out-of-order retirements must not turn max(retired) into false readiness.
Do not serialize unrelated resources behind this prefix. A later producer
must not inherit or reset another producer's timeline value namespace.

Allocation/device/transport teardown invalidates bindings and wakes waiters
with terminal status. An abandoned publication is failure, not success. Keep
status storage and dependencies alive until mappings, readers and in-flight
work release them; generation checks do not make freed pointers safe.

The mapped page is a **status view of live driver objects**. Mapping it grants
no publication or wait authority; control operations require the validated
binding. Shared driver status memory and a private ABI are explicit tradeoffs
to keep this change small.

### Preserve freshness and consumer execution boundaries

The staged-SRV gate checks the announced epoch, including draws with unchanged
bindings, and retains per-epoch flush deduplication. A refresh copies against
one captured epoch and stamps that epoch. Today's second lookup can stamp
a newer value than the copy waited for; do not carry that race forward.

Keep sampled-import dependencies attached to the command list until submission.
Enforce them on the submission worker before any relevant transfer, sparse or
graphics work reads the source. DxvkCommandList::submit is a local insertion
seam for retained resource/epoch waits. Do not move all waits onto the
application recording thread.

Sample status first. If pending, atomically check-and-register a referenced
event with the KMD, sleep in user mode with cancellation/device-loss handling,
then recheck status after wakeup. Reuse the existing fence-event pattern.
A raw wire-fence event cannot cover an announced stream value whose wire
submission does not exist yet: that predicate needs explicit handling.
Never park inside an Escape, hold the submission mutex while waiting, or
interpret timeout as permission to copy/sample.

### Add one thin DX12 producer hook

At the existing UMD12 present boundary, capture the exact resource binding,
engine queue, stream and value. Enqueue through the vkd3d worker callback seam,
after prior work and before later work. Retain the engine resource/queue and
callback data through execution and cleanup. Signal the common ICD stream
on the actual VkQueue; the existing HEPR correlation can name that stream/value.

Do not add producer-side GPU-idle waits or per-draw/per-resource-use tracking
to vkd3d. UMD12's current sample-only ECL fence is insufficient: the worker may
not have emitted the frame yet. Signal scope must cover the actual final
submission, including split-submit and prior-wait cases.

### Pass WSI dependencies explicitly

Pass the pre-present semaphore's **unnamed NT handle and exact value**, tied to
the source image, through the existing WSI-to-vehicle interface. Import/cache
it for that helper device and retain it through the copy. Record the dependency
for the actual source read. Specify duplication and close ownership; do not
borrow a handle past its lifetime.

**Do not trust the comment saying the caller already waited for the frame.**
wsi_common.c::wsi_helios_present_execute skips WaitForFences while
helios_vehicle_serving is true; the inline path has the same guard. Source
readiness cannot become an always-ready flag.

Preserve the copy-completion/recycle guard. helios_umd_get_present_result is
currently a retired stub returning -1; the live route uses
helios_umd_wait_last_present. Do not delete that route with HPS2 or claim the
named release-fence arm works. This extends the helper seam already in use.

## Performance and scope limits

The fast path replaces repeated 4096-slot lookup/seqlock work and producer
cache/name resolution with direct cached status loads. Pending reads can use
KMD completion directly, bypassing foreign named-fence import and its
user-mode completion relay.

Current Mesa's vn_queue_submission_fix_batch_semaphores CPU-waits for imported
Win32 sync before mirroring it into the host semaphore. Retaining a submission
worker wait therefore does not replace an existing native host GPU wait.

Publication introduces KMD work where HPS2 used a mapped write. Batch/piggyback
where the existing boundary permits, preserve DX11's folded present signal and
existing submission batching. No per-draw kernel query, added whole-queue drain,
unconditional staging copy or per-frame object creation. These give a credible
parity path, **not measured parity**.

The patch spans KMD/protocol, common ICD wrappers, DXVK consumer hooks, the two
existing publishers and the UMD12/vkd3d worker seam. Expect a focused
multi-repository change. Deleting implementations and the installer ACL block
is the final mechanical step.

This replaces producer discovery, readiness, freshness and cleanup. It does
not by itself repair every D3D12 application-fence/CPU-signal/shared-fence DDI
gap. Readiness also does not prove Vulkan external ownership or buffer reuse.
Preserve existing barriers and ownership handshakes. The vkd3d fork has no
generic external queue-family release/acquire implementation to assume: if the
target native-DX12 sharing path requires additional transfers, implement its
exact resource boundary or report that extra work as a blocker to claiming
mixed-API runtime correctness. Never suppress it with a ready bit.

## Implementation acceptance

- Put focused tests of the production state logic in kmd_logic: pending before
  submit, retire before publish, out-of-order completion, independent resources,
  stale bindings/generations, producer failure, reset, cancellation and waiter
  registration races. Keep protocol ABI/layout checks enabled.
- Build KMD, both UMDs, DXVK, Mesa and the affected vkd3d bridge. Linux cargo
  uses CARGO_TARGET_DIR=target/linux; Windows builds use local C:.
- Exercise mixed DX11/DX12 devices in one process, cross-process shared opens,
  delayed producer work, unchanged SRV bindings across frames, copy sources,
  rotation, resize and producer exit. Cover async and inline WSI, including
  the steady-state wait skip.
- Source searches must find no active HPS2 mapping/calls, producer PID/start/
  fence discovery or installer ACL setup. Preserve unrelated named fences.
- For runtime acceptance use compatible old/new stacks and interleaved
  workload runs. Compare publication CPU cost, submission/refresh counts,
  wait time, frame-time tails and visible changing frames. Include working
  DX11/Vulkan workloads and native DX12. Builds are not rendering/perf evidence.
- Cut over compatible artifacts together. Do not delete the file while an old
  process can still map it. Follow repository deployment/reboot authority;
  source implementation does not itself require either.

The first vertical implementation must establish exact allocation binding,
announced-to-retired completion, mapped freshness and a consumer wait together.
If that requires a new WDDM surface, scheduler executor or guessed identity,
report the concrete obstruction rather than expanding the project.

## Implementation and runtime acceptance

The source cutover keeps `WddmSurface::Wddm2_1GpuMmu`. It removes the DXVK and
Mesa HPS2 source/header pairs, their active callers, the foreign named-fence
cache, the optional unordered-read/kwait skip controls and installer file/ACL
setup. The old file is neither consulted nor deleted by this implementation.
The existing scanout reader and Present-buffer protocols remain separate.

### Allocation state and interface

`kmd_logic/src/producer_completion.rs` is the production state machine used by
`kmd_render/src/adapter/producer.rs`. `CreateAllocation` registers the exact
global allocation private pointer. `OpenAllocation` acquires the global private
data for the runtime's exact allocation handle and records its association with
the new open and `hKmdProcess`. Binding acquires the device-specific private data
and retains the generation already associated with that exact open. Each operation
uses one `AcquireHandleData`/`ReleaseHandleData` pair on the same PASSIVE thread,
with release outside driver locks. It never pairs independently resolved handles.
The production `Opens` registry and scoped-reference cleanup, including null private
data with a valid release token, are tested in `kmd_logic`.
No callback pointer is dereferenced or retained after that resolution. The returned binding
retains driver-owned status; it does not pin an allocation until DestroyDevice
and create a circular teardown dependency. Different opens reach one state.

**Runtime correction (.264):** `.263` crashed during DWM startup with
`0x113/0x26/1` in `GetHandleData`, called with an acquired reference outstanding.
The matching kernel dump identifies dxgkrnl's explicit rejection of a WDDM2
driver calling the WDDM1.x callback. The earlier null result during
`OpenAllocation` was this same callback restriction; the initial explanation
that the allocation handle was not yet published was incorrect. Both private-data
views now use the WDDM2 acquire/release callbacks. `.264 / oem48.inf` subsequently
booted with virtio-gpu, with no new bugcheck, no open/bind refusal and a fresh
visible desktop capture. DWM loaded the verified replacement UMD/ICD. This closes
the observed startup crash; the broader runtime acceptance packet remains pending.

The table reserves 8192 allocation slots, 8192 pending operations and 16384
resource/stream writer records; the KMD also reserves 32768 opens, 16384 bindings
and 1024 waits. Allocation, binding, stream and transport generations cannot
wrap into a live identity. Capacity exhaustion fails explicitly. Storage is
reserved at StartDevice; retirement and event wakeup allocate nothing.
The 512 KiB status view uses the existing read-only MDL and device-mapping
teardown path and survives transport Stop until adapter teardown.

`protocol/include/helios_producer.h` defines ABI v1, exported as
`helios_venus_producer_interface`. It includes stream registration, exact
allocation binding, publication, cached status, wait, retain/release and abort.
DXVK and vkd3d resolve that export from the module containing their live
device's `vkGetSemaphoreCounterValue` dispatch entry. This supports the
installer's content-hashed ICD filenames without choosing a module by name or
list order. An intercepting layer without the interface is refused explicitly.
The C control record is 96 bytes; status slots are 64 bytes. Rust layout asserts
and `python3 tools/sync-producer-abi.py --check` protect the three build-mirror
copies. The preexisting stream export remains for the separate Present-buffer
reader handshake; producer clients use the versioned interface.

Publication follows commitment of a retained signal operation. It records a
resource epoch independently of that stream's value namespace. Retirement
comes from the existing registered stream's exact tagged Venus ring boundary.
Each resource advances through its completed prefix, so another queue's later
epoch cannot hide an unfinished earlier epoch. Retirement before publication
is checked under the same transport serialization. Abort, stream closure,
allocation destruction and transport reset wake pending readers with failure;
they do not advance completed epochs. Releasing a consumer binding cancels only
its own waits. The binding's Vulkan device must outlive the binding.

Cached status uses bounded seqlock loads from read-only memory. A wait first
samples status, then atomically checks/registers a referenced event in the KMD,
waits in user mode and rechecks after wakeup. Timeout and cancellation include
contention on the reusable event mutex. They never authorize a read. The KMD
Escape itself does not sleep. Diagnostics are `PrInitF`, `PrGenF`, `PrRef`,
`PrPub` and `PrRet`; publication/retirement registry snapshots are rate limited.

### Engine and WSI boundaries

UMD11 stamps DXVK storage with the actual runtime allocation at create/open;
the former resource-handle substitution is removed. Storage rotation carries
the binding, including the external source of private staged images. The
unchanged-SRV gate reads cached generation/announced epochs. Each refresh
captures one dependency and stamps that same generation/epoch after recording
the copy. Imported-read dependencies live with the command list and are waited
by the submission worker before sparse, transfer or graphics submission, with
queue mutexes dropped. The worker checks shutdown/device loss between bounded
event waits; immediate-context/device teardown cancels these waits before
draining the workers. Folded producer signals, staging copies, external barriers,
snapshots and submission batching remain in place.

UMD12's Present calls `helios_vkd3d_enqueue_producer` with the exact engine
resource, allocation and queue. The existing callback FIFO orders an
ALL_COMMANDS signal after preceding work, including split submissions and
flushed queue waits. The resource has an internal reference through completion;
the queue's STOP/join lifecycle retains the queue through callbacks and fence
worker cleanup. No COM self-reference is released on the worker that would
make it join itself. HEPR now carries the registered context/cookie/value.
Handled failures that drop predecessor submissions, waits or initial transitions
mark the device removed, preventing a later empty signal from claiming success.
No sample-only ECL fence or GPU-idle wait is used by this hook.

Each present allocates a small retained operation payload for the existing
command/callback machinery. Semaphore, mapping and event objects are reused;
there is no per-frame NT/Vulkan synchronization-object creation. Measure this
payload and publication cost in the performance comparison.

WSI exports one unnamed NT semaphore handle per chain and passes it with the
exact pre-present value through `helios_umd_set_present_source_v2`. WSI owns
the handle until chain teardown; the helper duplicates it, caches an imported
Vulkan semaphore using exact kernel-object comparison, and retains the import
through the recorded copy. `clear_present_source_v2` ends the borrowed scope
and reports whether the DDI recorded a copy, including Present HRESULT failures.
`helios_umd_wait_last_present` remains the mandatory copy-completion guard.
Steady-state WSI still skips its frame-fence wait. GDI fallback and dropped or
resize-rejected frames wait their own source fence before reading/recycling.
A failed copy wait makes the chain terminal and retains the source allocation
through device teardown, including immediate swapchain destruction. This is
failure containment, not completion or a steady-state resource leak.

### Source/build checks

All checks below passed on the implementation checkout; none is runtime proof:

- `CARGO_TARGET_DIR=target/linux cargo test --manifest-path kmd_logic/Cargo.toml`:
  198 tests, including 20 new production producer-state/reference/wait tests. They cover
  publication before submission, retirement before publication, independent
  opens/writers, out-of-order completion, stale generations, capacity rejection,
  stream failure, allocation/reset teardown and event registration/cancellation
  orderings with exactly one reference transfer.
- `CARGO_TARGET_DIR=target/linux cargo test --manifest-path protocol/Cargo.toml`:
  14 tests plus ABI layout assertions.
- Windows release builds: `kmd_render`, `umd`, `umd12`, DXVK's static engine,
  Mesa `vulkan_virtio.dll`, and vkd3d static engines/DLLs. Cargo targets and build
  artifacts are on C:, with Linux targets under `target/linux`. Removing the
  stale generated Mesa WSI thin archive was necessary after deleting its old
  HPS2 object; rebuilding then linked successfully.
- Byte-exact ABI-copy check and root/nested `git diff --check`.

### Initial deployed runtime checks

The authorized `.264 / oem48.inf` deployment ran the complete installed 3DMark
definitions interactively on 2026-09-06, Fire Strike followed by Time Spy:

| Benchmark | Overall | Graphics | GT1 FPS | GT2 FPS | CPU / Physics | Combined |
|---|---:|---:|---:|---:|---:|---:|
| Fire Strike | 35669 | 57396 | 249.06 | 250.04 | 40451 | 8881 |
| Time Spy | 15934 | 16222 | 101.28 | 96.74 | 14482 | — |

Every workload in both result archives has successful status. Host VNC captures
show changing Fire Strike demo/GT2/Combined and Time Spy demo/GT1 scenes. Time
Spy loaded the verified native UMD12 and content-hashed ICD. The first Fire
Strike attempt was cancelled by display/focus loss coincident with the guest
capture task; its incomplete result is preserved and excluded. The successful
retry used a hidden task wrapper and host-only capture.

The four Time Spy processes recorded 21237 native presents in total, each with
`PresentProducerFailed=0`. Their logs still record `EclFenceNoDrain` and
`FenceBottomOfPipeUnproven`; passing this workload does not close the broader
DX12 submission/fence contracts.

After both benchmarks, the desktop remained visible on the same boot and DWM
process, with no new System bugcheck/shutdown event. KMD publication/retirement
snapshots advanced, and producer initialization/open/bind and IRQL failure
counters remained zero. Result archives, invocation records, module hashes,
captures and post-run health are under `tmp/hps2-264-runtime/`. These single
runs do not establish performance parity or replace the acceptance matrix below.

### Runtime acceptance packet — pending

Run only after a separately authorized compatible-stack installation. Record
root and nested revisions, dirty diffs, artifact hashes and actually loaded
modules for each arm. Do not mix an old UMD/ICD/KMD with this ABI. Keep the
shipping `UmdD3D12` default unchanged; native-DX12 activation is a separate
runtime step. Use interactive scheduled tasks, not session-0 launches.

| Case | Required stimulus and pass evidence |
|---|---|
| Mixed DX11/DX12 | Create DX11 and native UMD12 devices in one process; also run visible DX11 and DX12 applications together. Change distinct numbered/color frames; verify each window and desktop composition advance. Capture UMD12 `PresentProducerFailed`, KMD producer counters and exact frame content. |
| Cross-process sharing | Export a real shared allocation and open it twice in another process. Verify both opens observe one generation/epoch and an unrelated allocation remains independent. Change frame content repeatedly; close/reopen handles while retaining the allocation, then destroy/recreate and require a new generation. |
| Unchanged SRV binding | Bind the imported SRV once, publish at least 120 changing producer frames, and draw without SetShaderResources again. Compare staged pixels with the numbered producer frames; check recorded refresh epochs never exceed the consumed epoch. |
| Delayed and reordered work | Delay the actual vkd3d worker/queue work, including a queue wait before its signal and a split submission. Announced must advance while completed remains behind; the consumer must stay blocked until the registered boundary. Complete a later independent writer first and require the resource prefix to remain pending. `Umd12EclDelayUs` alone does not prove this boundary. |
| Rotation and resize | Alternate at least three buffers, resize repeatedly, minimize/restore and destroy/recreate the chain. Verify bindings follow backing rotation, frames change, old generations cannot satisfy new reads, and Present-buffer/scanout-reader protection still holds. |
| WSI | Exercise async WSI (`HELIOS_WSI_ASYNC_PRESENT=1`), FIFO and non-FIFO, delayed source signals, drops, occlusion, resize and forced helper-copy failure. Confirm the explicit handle/value reaches the copy, steady-state frame prep skips its fence, and no image recycles before the helper read completes. The owner excludes the inline path from this work. |
| Cancellation/teardown | Exit the producer while an epoch is pending; close a waiting consumer/device; cancel an event; stop/reset through a separately authorized lifecycle test. Waiters must terminate with failure, completed must not advance, and normal create/destroy cycles must not leak handles/mappings. Check the documented retained-allocation failure path separately. |

Existing `d3d11_shared_content_probe.cpp` and `d3d11_shared_draw_probe.cpp`
are useful sharing/readback controls; their single-frame, same-process results
do not cover the cross-process or unchanged-binding cases above.
`d3d12_clear_probe.cpp` and `d3d12_bridge_probe.cpp` provide native/engine
controls, not substitutes for native mixed-API acceptance. Require visibly
changing frames and correct content. Do not introduce a focus-stealing capture
during 3DMark; use the owner's visible observations and source/runtime ordering
evidence. Counter deltas and one frozen frame cannot pass this packet.

The owner initially reported an approximately 10% performance regression and
directed correctness work first. On .266 they confirm the shadows are fixed at
about 100 FPS and request investigation of a further 10–20% gain in both APIs.
Keep `HELIOS_WSI_ASYNC_PRESENT=1`; the inline path remains outside this work.
Use clean matching-settings before/after benchmarks per API, without a complex
interleaved campaign, and preserve visibly correct changing frames. The automated
74.26 FPS GT1 result used instrumentation and is a separate observation; settings
equivalence with the owner's benchmark is unproven. No further gain is claimed.

### Explicit remaining DX12 synchronization gaps

The bounded HPS2 hook supplies the presented resource's producer boundary.
The subsequent [HE12 v2 repair](dx12/EXECUTION_SYNC.md) replaces the general
sample-only ECL bridge and removes the above-watermark private-fence policy.
It adds runtime-context admission plus exact registered worker completion for
ECL and Present callbacks. This repair has completed independent review, is
deployed on .266, and passes the four native ordering cases described in
EXECUTION_SYNC.md. Those cases and the owner's Time Spy shadow acceptance do
not establish all runtime fence, resource-use or lifecycle contracts.
Nonzero monitored-fence GPU placements and direct D3D12 queue fence DDIs remain
explicit E_NOTIMPL paths pending their negotiated-contract analysis. These are
not the optional WDDM 3.2 native GPU fence feature, which is outside this
WDDM 2.1 repair and is not an acceptance blocker.

In particular, a completed producer epoch does not perform a Vulkan external
queue-family ownership transfer or release a consumer. The narrow vkd3d hook
does not record the general resource release/acquire protocol needed to prove
all DX12-to-DX11 shared-image uses. Adding that requires tracking the exact
resource layout/owner and recording matched barriers on both sides; a later
queue signal alone cannot supply it. Existing staging/external barriers and
Present-buffer/scanout-reader contracts are preserved. Mixed-API acceptance
must establish the concrete path's ownership proof; if absent, that path remains
blocked on this separate contract rather than being declared fixed by HPS2
retirement. No scheduler, memory-manager or presenter rewrite was introduced.
