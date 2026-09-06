# DX12 runtime admission and exact execution completion

**State:** .266/oem50.inf is deployed with the updated Mesa ICD after two
consecutive dry independent review rounds. Guest reboot and Code 0 are verified;
D3D12 remains enabled and the desktop is visible. All four native runtime ordering
cases pass on .266 (session 1, GPU readback witnesses). The owner confirms
realtime Time Spy shadows are fixed and observed approximately 100 FPS in their
benchmark. The separate instrumented GT1-only comparison completed at 74.26 FPS
versus 20.03 on .265 (3.71 times), with async WSI enabled; settings equivalence
with the owner's run is unproven. The implementation uses the shipped ICD feedback workaround with stock
virglrenderer; no launcher or host renderer change is required.
This is the HE12 v2 successor to the
sampled ECL bridge. It keeps `WddmSurface::Wddm2_1GpuMmu`, the existing software
scheduler, submission workers, Present ownership and scanout protection.
The native probe covers only its exercised ordering cases; the owner supplies
the shadow acceptance at recovered throughput. Broader acceptance remains below.

## Contract and ordering

There are two different edges. A runtime queue wait must prevent engine work
from starting early. A runtime queue signal must follow the actual engine work,
including split submissions and any engine fallback queue. A sampled Venus wire
fence could precede worker execution and supplied neither guarantee.

For each ECL, `forward12/queue.rs` performs the following on the entering DDI
thread, under the queue's context-operation mutex:

1. Create an unnamed, initially unsignaled manual-reset event. The engine
   prepares and retains the complete command batch, duplicates that handle,
   and reserves its stream value in the same FIFO commit as the work.
2. Submit the 24-byte `HeliosD3D12SubmitCmd` v2 through `pfnRenderCb` on the
   exact runtime context created for that queue. The command carries the
   registered stream handle, value and cookie; zero and v1 are refused.
3. Call `pfnSignalSynchronizationObject2Cb` on that context with
   `SignalAtSubmission | EnqueueCpuEvent`, `ObjectCount=0`, no broadcasts and
   the owned CPU event. Then release the caller's event handle.

The event is queued **after** Render and signals at submission, before packet
completion. Using completion here would deadlock: the worker would wait for the
same work it has yet to execute. Microsoft defines these flag bits and event
restrictions in [D3DDDICB_SIGNALFLAGS](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dukmdt/ns-d3dukmdt-_d3dddicb_signalflags)
and the exact context/event fields in
[D3DDDICB_SIGNALSYNCHRONIZATIONOBJECT2](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dumddi/ns-d3dumddi-_d3dddicb_signalsynchronizationobject2).
The public contract supports this ordering. The native runtime suite below now
exercises software fence waits/signals through the created contexts, including
independent GPU-write witnesses. See `DDI_REFERENCE.md` section 10.3.

The engine worker waits for admission before processing that ECL. It holds no
Vulkan queue lock while waiting, so a producer on another queue can satisfy a
wait-before-signal dependency. Its 100 ms event-wait interval only checks device
loss/cancellation; it never constitutes completion. After execution, it joins
the engine's external/serializing waiters and submits an `ALL_COMMANDS` signal
on the exact registered stream, alongside the existing engine timeline signal.
Existing command batching, split submits and allocator/fence-worker lifetimes
remain in force. There is no new caller-side worker drain or GPU-idle wait.

Present producer callbacks use their own event/HE12 pair before HEPR. This
covers `Queue::Wait -> Present` with no intervening ECL. The callback retains
the exact resource and allocation binding. Its publication epoch and stream
reservation share the queue FIFO lock with ECL reservations, so concurrent
publications cannot invert signal values. HEPR and its runtime present
descriptor retain their existing identity and ownership duties; inability to
submit that required identity is now an error.

## Kernel proof and lifetime

`DxgkDdiRender` authenticates the stream/cookie against the context's exact
`ContextContext -> DeviceContext -> hKmdProcess` chain. Each context binds one
generation-qualified stream, and each new Render must strictly advance its
value. No PID, resource geometry or private fence object supplies authority.

The KMD requests 104 private bytes per context. The existing Present prefix
occupies bytes 0..31 and the flip/snapshot record occupies 32..87. A separate
16-byte execution record occupies 88..103. Compile-time assertions prevent
overlap. A same-context batch merges to the largest exact stream value. A
different context/generation cannot inherit the previous record. Submit reads
the record without consuming it, preserving the proof for preemption replay.

`kmd_logic::execution_completion::{Record, Wait}` is production logic used by
the Render and retirement paths. Submission checks already-observed retirement
under the same lock that publishes pending waits. Later stream retirement
latches completion before registration teardown can erase the live slot.
Only the matching generation/value can complete the wait. Execution packets
stay in the retryable WDDM FIFO even if already ready, and explicitly request a
completion DPC so they do not depend on a future transport interrupt.

Legacy dead-stream discharge and timed head rebasing cannot discharge an
execution entry, including a packet also carrying Present copy obligations.
A FIFO overflow is terminal and retains outstanding obligations for reset;
locally detected missing transport/device loss never reports successful execution.
Host failures hidden behind an upstream retirement callback remain the gap
listed below. Preemption or reset discards the cancelled waiter without reporting
DMA completion.

Queue destruction removes the public queue slot, then destroys the runtime
context while keeping worker admission and the engine stream alive.
[`pfnDestroyContextCb`](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dumddi/nc-d3dumddi-pfnd3dddi_destroycontextcb)
drains queued work; cancelling an unsignaled admission before that drain would
discard valid work still awaiting runtime submission. After the callback returns,
engine cancellation precedes Release joining the workers. Context-destroy failure
also removes the engine device. Failed submission/admission removes the device and
aborts the corresponding producer publication. A failed submitted operation
does not release resources whose GPU use remains unproven.

## Fence authority and remaining gaps

**Scope clarification:** WDDM 3.2 native GPU fence objects are an optional,
separately advertised feature (`DXGK_VIDSCHCAPS::NativeGpuFence`). They are not
required for this WDDM 2.1 repair and their absence is not an acceptance blocker.
See [native GPU fence objects](https://learn.microsoft.com/en-us/windows-hardware/drivers/display/native-gpu-fence-objects).
Ordinary monitored fences already have GPU virtual addresses in WDDM 2.0;
Microsoft also documents a software signal path for engines that cannot write
those addresses. A nonzero fence GPU address therefore does not identify the
WDDM 3.2 feature. See [context monitoring](https://learn.microsoft.com/en-us/windows-hardware/drivers/display/context-monitoring).

The software WDDM runtime owns fence values, initial values, CPU signals,
rewinds, shared handles and queue waits. `D3D12DDIARG_CREATE_FENCE` supplies GPU
placements, not a KMT synchronization handle, CPU mapping or initial value. The
previous private `ID3D12Fence(0)` and guessed signal watermark were removed.
The UMD's opaque fence state now retains only its exact creating-device
association.

| Path | Current implementation and acceptance boundary |
|---|---|
| Zero-VA software runtime fences | Runtime owns values; HE12 admission/completion orders actual engine work. Native cross-queue/CPU/shared-fence cases pass on .265 with independent producer/consumer witnesses. Broader teardown and application acceptance remain open. |
| Nonzero monitored-fence GPU placements | Currently `FenceGpuVaRefused`, `E_NOTIMPL`. This is an unsupported older monitored-fence path, not a WDDM 3.2 requirement. Direct GPU access would require valid backing and fence writes; the documented software alternative requires exact runtime synchronization-object association. Neither may use a private shadow timeline as the application's fence. Whether this path is needed depends on the negotiated contract, not the presence of a GPU VA alone. |
| Direct D3D12 `pfnSignalFence` / `pfnWaitForFence` calls | These predate WDDM 3.2 native fences. Directional entry counters are retained; valid calls currently fail with `FenceNativeRefused` and `E_NOTIMPL` (the counter name is misleading but retained). These entry counters were zero in the deployed .264 Time Spy logs. The required software-context route must be validated; the 3.2 feature need not be implemented. |
| DX12 to DX11 shared-image ownership | Producer completion exists. General matching Vulkan external queue-family release/acquire and consumer-release tracking remain unimplemented on the vkd3d side. This needs exact layout/owner/resource-use tracking and matched barriers; a later queue signal cannot provide it. |
| Host device loss / render-server disconnection | Upstream `vkr_queue_thread` retires after any non-timeout wait result, including device loss, and the proxy forces retirement after server disconnection. The callback carries only a sequence number, not failure status. The guest cannot infer failure from that successful-looking retirement alone. A complete solution needs an error-bearing observation across this boundary. Reusing feedback for successful GPU completion does not by itself close this gap. |
| WSI | Keep `HELIOS_WSI_ASYNC_PRESENT=1`. The explicit unnamed NT semaphore/value dependency and separate copy/recycling guard remain unchanged. |

`Umd12EclSubmit`, `Umd12EclDrain` and `Umd12EclFence` registry switches are
retired. Existing inventory positions report fixed behavior and the appended
`ExecutionSyncVersion=2` distinguishes it. Retired sample/shadow counters stay
at their diagnostic indices with value zero. New counters distinguish ECL
worker commit, HE12 acceptance, admission-event queuing and refusal. None is
pixel or runtime-fence correctness evidence by itself.

## Time Spy versus Steel Nomad

The owner originally saw stale in-frame shadows in Time Spy and not Steel Nomad DX12.
UL documents Time Spy overlapping SSAO, light culling and unshadowed lighting
with shadow rendering, plus render-target heap aliasing. This makes queue
dependencies and aliasing relevant investigation targets.
[Time Spy engine](https://support.benchmarks.ul.com/support/solutions/articles/44002136148-time-spy-engine).
Steel Nomad also uses async compute, for its first volume-illumination pass,
and uses a different contact-shadow path.
[Steel Nomad engine](https://support.benchmarks.ul.com/support/solutions/articles/44002528067-steel-nomad-engine).
These differences are leads, not proof that a particular shadow pass caused
the observed defect. Whole-frame ordering and stale in-frame shadows need
separate visible acceptance.

## Exact GPU feedback completion (.266)

The existing retire worker carries the exact registered cookie, signal value,
context and KMD-returned wire fence from the tagged submit. After observing the
GPU-written feedback slot, it sends the 48-byte version-1 `STREAM_FEEDBACK`
escape (0x14). KMD requires the live device owner/context/cookie and exact
in-flight `AsyncVenus` receipt, including generation-qualified stream handle,
value and wire fence. An exact recorded successful response can acknowledge a
notification that raced wire retirement. A missing, cancelled, older or different
receipt never becomes completion merely because a higher watermark exists.

`execution_completion::Progress` has separate GPU-completed and wire-retired
values. The GPU edge advances allocation producer state and latches HE12 waits,
including publication/submission after observation. It never releases transport
storage, wire waiters, Present consumer claims, scanout leases or closing stream
slots. The real response retains those duties. Notification holds notify then
virtio locks; it requests the existing completion DPC after releasing both.
There is one notification per actual registered boundary on the retire worker,
with no additional draw-time query or renderer batch.

Only a fresh, initially-zero, permanent exported Win32 timeline can register.
A registered private stream refuses CPU signals and imports, with a named
refusal counter/log. Feedback notification is eligible only for the exact
queue submit containing the semaphore feedback command. A queue lacking
feedback support permanently revokes the backend feedback pointer before the
CPU counter-resync path becomes reachable. Neither `GetSemaphoreCounterValue`
nor its WDDM-folded result supplies this proof. Pointer load and dereference
hold `dev_mutex`, also held by detach before slot pool return. A sync reference
alone does not pin that slot. Detach, timeout, unsupported feedback or a rejected
notification leave KMD completion on the real wire path; none is success.

`stream_fb accepted/wire_retired/rejected` measures the new seam. The prior
`retire_fb fast/fallback/wire` counters measure local external-sync retirement.
Both are needed to establish that HE12 completion uses feedback in a live run.
The normal host renderer and VM launch command remain unchanged.

## Validation and runtime acceptance

The .265 slowdown now has host-side evidence. A render-aligned GT1 trace at
20.03 FPS shows direct/compute DMA medians of 10.19/10.58 ms. The async renderer
callback is enabled and QEMU dispatch follows it promptly. A standalone GPU
fill followed by the same empty queue marker reproduces an 8.060 ms average
work-submit / marker-submit / wait sequence with `SYNC_FD`-exportable fences
on NVIDIA 610.57.04, versus 0.329 ms for ordinary fences. This measures the
complete sequence, not `vkWaitForFences` alone. The private host patch proposal
was withdrawn at the owner's request; keep stock virglrenderer.

The existing solution is the
[archived WS2 feedback workaround](../archive/ROADMAP_HISTORY_THROUGH_2026-09-05.md)
(lines 3096–3127), `HELIOS_RETIRE_FEEDBACK`, already on by default. Current
`vn_renderer_helios.c::helios_sync_retire_thread` reads the exact semaphore's
GPU-written counter and advances its external sync; the captured .265 run
already reports `retire_fb fast=4607 fallback=0 wire=0`. On .265 KMD producer/HE12
progress still advanced only from the tagged AsyncVenus response. The .266
candidate implements the separate exact GPU completion edge described above.
213 production logic tests (including seven new feedback/receipt/race cases)
and 14 protocol tests pass. Windows Mesa and the normal signed .266 KMD package
build. The conservative startup unwind gate is 5248/17936 bytes, including
saved registers and return addresses; the prior 4936 figure counted stack
allocations only. Build receipts and the gate are in
`tmp/feedback-completion-baseline/`. Two independent rounds with different lens
compositions are dry. Deployment and boot receipts plus the passing native suite
are in `tmp/dx12-sync-266-runtime/20260906-155029-223/` and its parent directory.
The completed GT1-only run in `tmp/dx12-sync-266-perf/` reports 74.26 FPS versus
20.03 on .265, using the same definition/options with async WSI enabled. This run
used `HELIOS_PERF=1`, `--debug-log` and a four-second ETW slice. Render
PID 5556 loaded the new content-hashed ICD and packaged release UMD12. The final
reconstructed counters are `stream_fb accepted=49240 wire_retired=720 rejected=151`
and `retire_fb fast=50111 fallback=0 wire=0`. Refused notifications leave KMD
completion on the wire; this counter does not distinguish older removed receipts
from teardown/refusal and must not be described as zero or classified without
additional evidence. The adapter remains Code 0 after the benchmark. This is
one completed before/after benchmark comparison, not a broader performance claim.
The ETW artifact's mixed UTC offsets produce invalid durations in the earlier
parser; those durations are excluded from acceptance. The exported 3DMark XML
is the FPS source. The owner subsequently confirmed the realtime shadows are
fixed and observed about 100 FPS in their own benchmark; this is separate from
the instrumented GT1 measurement. Mixed sharing, unchanged bindings,
rotation/resize, teardown and WSI stress remain runtime acceptance work.
The prior 20 FPS visual check in `tmp/dx12-sync-265-perf/` was insufficient
because low throughput could conceal a timing-dependent race.

Source checks for this change: 206 `kmd_logic` tests, including eight execution
boundary cases; 14 protocol tests; UMD12 host Clippy with `-D warnings`; Windows
vkd3d, release UMD11/UMD12 and normal KMD package builds. The .265 normal KMD
stack gate measures 4936 bytes against a 17936-byte ceiling. The native probe
builds with MSVC `/W4 /WX`. Build logs, source snapshots and artifact hashes are
in `tmp/dx12-sync-20260906/`. Builds are not runtime acceptance.

`tools/d3d12_sync_probe.cpp` selects the Helios PCI adapter through the system
DXGI/D3D12 runtime and checks four direct/compute queue cases: signal before
wait, wait before signal, future CPU signal after a fence rewind, and a CPU
signal from another process opening the exact inherited unnamed shared-fence
handle. Every case requires 65536 exact readback words for its distinct epoch
in each of two independent producer/consumer witnesses.
Negative intervals must neither signal nor alter the readback sentinel through
the unsignaled wait. The producer is already queued for both CPU gate cases;
otherwise the negative interval would test only the consumer's ready fence.
Positive waits require an actual event and correct GPU bytes. A timeout is failure. The wrapper records binary/source
hashes, session, stdout/stderr and a JSON result.

Build without executing:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File Z:\tools\d3d12-sync-acceptance.ps1 -Mode Build
```

After independent review and deployment of the matching KMD plus explicit
release UMDs, register/run in the interactive guest session (never session 0):

```powershell
$action = New-ScheduledTaskAction -Execute 'powershell.exe' -Argument '-NoProfile -ExecutionPolicy Bypass -File Z:\tools\d3d12-sync-acceptance.ps1 -Mode Run'
$principal = New-ScheduledTaskPrincipal -UserId 'Rupansh' -LogonType Interactive -RunLevel Highest
$settings = New-ScheduledTaskSettingsSet -ExecutionTimeLimit (New-TimeSpan -Minutes 2)
Register-ScheduledTask -TaskName 'helios_d3d12_sync_acceptance' -Action $action -Principal $principal -Settings $settings -Force
Start-ScheduledTask -TaskName 'helios_d3d12_sync_acceptance'
```

Native execution acceptance passed on .265 in interactive session 1:
`tmp/dx12-sync-265-runtime/20260906-040507-991/result.json` records all four
cases, exit 0, with both producer and consumer witness checks. That process
loaded the .265 DriverStore UMD12 and logged `ExecutionSyncVersion=2`, exact
HE12 boundaries and queued admissions; direct fence/refusal counters remained
zero. Native runtime software-fence routing is therefore exercised, including
the cross-process CPU signal. This does not prove all resource-sharing or
presentation paths.

The owner has accepted realtime Time Spy shadows at recovered throughput.
Preserve that result during the next bounded DX11/DX12 performance investigation
in ROADMAP.md. Broader acceptance still requires:

- Queue/device teardown while waiting, producer loss, cancellation and normal
  completed teardown; no stuck worker, synthetic DMA completion or new crash.
- Native Time Spy GT1/GT2 coverage with visibly changing frames and correct
  moving shadows; the owner reports a shadow pass, without specifying individual
  subtests. Steel Nomad DX12 remains the reported unaffected rendering control.
  Collect runtime/ETW ordering evidence for a regression. Do not infer rendering
  correctness from scores or introduce a focus-stealing capture during 3DMark.
- Mixed DX11/DX12 and cross-process resource sharing, unchanged SRV bindings,
  staged-refresh epoch accuracy, buffer rotation, resize and async WSI with
  delayed source completion. Producer completion and consumer release require
  separate evidence; the external ownership gap above cannot be waived.

The independent reviewer resumed and completed the whole-change review. Its
confirmed defect was cancellation of valid, not-yet-admitted work during normal
queue destruction. The repaired drain-before-cancel ordering above was checked
by that reviewer; no further concrete defect survived the callback ABI/scope,
worker ordering, batching/replay, retirement and failure-path review. Windows
vkd3d and release UMD12 rebuilt after the repair, host Clippy passed, and the
matching .265 package was deployed and rebooted. Remaining runtime acceptance
is listed above.

Diagnosis on the deployed .264 stack (interactive session 1) now independently
reproduces an ordering failure: the case-1 completion event signals while the
readback contains `00000000`, where epoch 1 requires `3c6ef372` at word 0.
Evidence: `tmp/dx12-sync-264-diagnosis/20260906-035033-150/`. That wrapper recorded
a null exit code; its stderr contains the explicit readback failure, and the
wrapper now owns the native process handle through exit to preserve the code.
The strengthened CPU-gate case independently proves early execution: with its
future queue wait still unsignaled, the GPU overwrote the readback sentinel with
epoch-3 bytes (`78dde6e4` at word 0). It exited 1 in
`tmp/dx12-sync-264-diagnosis/20260906-040105-502/`. An earlier event/readback-only
version of that case passed because the deliberate 100 ms negative interval
allowed early GPU work to finish; it was insufficient admission evidence.
The .264 Time Spy `umd12-3252-vkd3d.log` also contains 2956 allocator resets
with command lists still awaiting execution. These support an early-completion
defect; they do not establish which shadow resource supplied the old pixels.
The owner originally reported frames from the first 1–10 frames flashing later (example
frame 799), plus black frames, in realtime only; individual image-tool frames
are clean. The owner is the visual oracle for the shadow repair. After a
2397x1517 scanout import failure, the existing QEMU display-size request and
Helios device restart restored actual VNC output at 1280x800; the screenshot is
`tmp/dx12-sync-265-runtime/desktop-restored-vnc.png`. No scanout source change or
arbitrary-resolution fix is claimed. The .266 recovery and owner shadow
acceptance remove the 20 FPS timing confounder. Further performance work must
retain the repaired ordering and the owner's visibly correct rendering.
