# Native command-pool generations

The native runtime may publish authenticated HE12 GPU completion before the
engine's independent fence worker drops an execution's allocator references.
Those references cover queued execution, submitted command buffers, scratch,
views and other allocator-owned backing. They are not themselves a GPU progress
counter. The original engine Reset kept the storage intact and returned S_OK
when references remained, repeatedly logging pending command lists. The accepted
057934F9 Port Royal run recorded 91,993 demo and 41,715 graphics-test diagnostics;
those counts did not prove that the application reset storage prematurely.

The public [allocator Reset contract](https://learn.microsoft.com/en-us/windows/win32/api/d3d12/nf-d3d12-id3d12commandallocator-reset)
requires completed GPU use before Reset and rejects an actively recording list.
The current WDK `PFND3D12DDI_RESET_COMMAND_POOL_0040` returns VOID. The native
frontend must propagate a failure through the device callback rather than
returning a fabricated successful engine reset.

## Ownership and implementation

Each DDI pool retains independent DIRECT, COMPUTE, COPY and BUNDLE allocator
classes. Recorders retain the class set through an Arc, independently of the
runtime's pool private block. A class mutex serializes lazy creation, generation
selection and Reset. Returned allocator references are owned; recorder locks
are released before taking class locks, and no class lock crosses a runtime
callback. Application/runtime serialization of recording against pool Reset
remains required.

The private engine `helios_vkd3d_try_reset_command_allocator` returns:

- S_OK after the ordinary Reset succeeds with no execution references.
- S_FALSE without changing storage when execution references remain.
- The actual failure for recording conflicts, allocation or Vulkan reset errors.

The caller owns a public reference. The acquire load of internal references
pairs with the existing atomic retirement; no Vulkan query, GPU-idle wait,
CPU completion shortcut or new completion publisher is added. BUNDLE allocators
have a different CPU-command representation and use their own ordinary Reset;
they must never be interpreted as Vulkan command-pool allocators.

On S_FALSE the frontend reserves ownership storage, resets a retired generation
whose execution references have gone away, or creates a replacement of the same
class. It exchanges the current generation and retains the previous one. New
recording therefore uses independent command storage. An allocation failure
before exchange leaves the selected generation intact and is reported through
`pfnSetErrorCb`; failure to report is counted. Pool/recorder destruction releases
public references, while the existing engine worker retains pending executions.
Uncertain host completion still quarantines references. No consumer-release or
external queue-family ownership rule changes.

Retired generations are a per-pool cache, reused when the worker releases them;
its size follows peak outstanding generations, without an arbitrary retention
cap that could force a premature release. Cached backing can remain until the
pool and its recorder owners are destroyed. If the worker cannot attest host
quiescence, this change does not make that backing reclaimable.

Lifecycle counters are `PoolGenerationReset`, `PoolGenerationRotated`,
`PoolGenerationCreated`, `PoolGenerationReused`, `PoolResetEngineFailed` and
`PoolResetErrorUnavailable`. Successful lifecycle counts are diagnostics, not
refusals despite their shared counter-report mechanism. A missing old warning
alone is insufficient evidence; rotation/reuse counts and GPU results matter.

Two related engine error paths are repaired: recycling-array allocation failure
returns E_OUTOFMEMORY before copying into unavailable memory, with an overflow
check on the requested count; failure to retain a recorded command buffer's
recycling metadata leaves its handle owned by the Vulkan pool and makes Close
return E_OUTOFMEMORY. That failed Close remains sticky across List Reset.
Freeing the buffer at Close would leave a recorded list referring to released
command storage. Release of an unfinished recording likewise preserves the
untracked handle until pool destruction. The final source fails the list
rather than relying solely on device-removal state inside the engine.

## Validation and provenance

Evidence root: `tmp/allocator-epochs-20260911/`. The candidate builds from root
54eb0c0340e308362389eaa85dd161517910793c and engine
38ecb6f7286b8df5573cd619384cc4e6dc71aa26 plus the recorded patches and source
manifest. `final-build-windows/provenance.json` reconciles 65 mirrored driver/engine
sources and archives all static libraries, import/export checks and build logs.
Windows uses local C: output, clang-cl/libclang22.1.8 and VulkanSDK1.4.350.0;
bindgen0.72 and layout assertions remain. Linux uses target/linux.

Final deployed UMD12 SHA256:
`F6D00A83BF18B93E365FACEAEF666CE21A34203244A5FD7839CEA2B217D69252`.
The ProgramData path is
`C:\ProgramData\HeliosUmd\helios_umd12_f6d00a83bf18b93e.dll`.
Device disable/enable completed with Code 0. KMD .271/oem54/WDDM2.1, UMD11
57C84ED4 and ICD 43394BBD are unchanged. This is a ProgramData update, not a new
signed package, hosted-CI result or guest reboot. Engine source is committed locally as
`5e8b2998cc350dac3d1b6ddb40232ef4e66e8074`; build metadata retains its actual
pre-commit base plus dirty source. The failed first Windows
compile was a missing C++ closing brace; its log is retained and no failed
artifact was deployed.

Linux engine builds, the Windows engine/release UMD and A1 pass, including 211
KMD logic tests. The engine ownership test checks a real unsignaled queue
dependency, S_FALSE with intact storage, authenticated retirement, 1,024 GPU
readback words, recording refusal, a synthetic count-overflow failure and the
separate bundle layout, plus synthetic pending-array reservation failure that
makes Close and subsequent List Reset return E_OUTOFMEMORY without removing
the device. It is supplementary engine validation, not native
Windows acceptance. Heap-allocation failure and host-loss injection remain
unexercised.

`tools/d3d12_allocator_probe.cpp` uses system D3D12 on the exact Helios hardware
adapter at FL12_1. Its scheduled-task runner verifies source/executable receipts,
loaded module paths and the intended UMD hash, and archives output/driver logs.
Baseline 465CBE13 and candidates E8A1551C and F6D00A83 pass 768 epochs across
DIRECT, COMPUTE and COPY, 64 epochs sharing an allocator across two queues, and a pending-list
Reset to different storage followed by release of the original public allocator.
Every epoch checks 4,096 words. The negative gate checks both unchanged readback
and absent completion. It never calls allocator Reset while GPU work is pending.
These small tests do not reproduce the old Reset warnings. The first candidate
records 832 successful resets with zero rotations and zero reset failures.

The candidate also passes the existing native DXR fixture (eight behavior groups,
20 ray words), no-RT valid-state-object refusal plus 4,096 GPU readback words, and
all four native ordering cases with 65,536 words each. Exact loaded system
runtime/UMD/ICD attribution is part of their acceptance receipts. Port Royal
rotation/recycling and owner-visible acceptance remain separately attributed
below; no performance improvement is claimed from these small tests.

## Intermediate Port Royal run

The E8A1551C candidate completed the stock demo and graphics test with workload
status 0, score 13,013 and 60.24752 graphics FPS. Its resolved settings exactly
match the previously accepted 057934F9 run. Both native workloads have identified
system runtime, E8A1551C UMD and 43394BBD ICD modules, without WARP or an app-local
engine. The archived result/export and review are under
`controls/allocator-epochs-portroyal/` and `allocator-epochs-portroyal-review.json`.

| Workload | Ordinary resets | Rotations | Reused generations | Additional generations created | Reset failures |
|---|---:|---:|---:|---:|---:|
| Demo 5272 |248337|97901|97578|323|0|
| Graphics 9740 |174172|43170|42997|173|0|

Created counts are totals across all pools, not a per-pool cache peak. The
initial live snapshot was incomplete; final destructor summaries supply these
counts. Both workloads have zero old pending-Reset errors and zero missing
reset-error callbacks. The first 50 VNC captures contain changing demo frames
but finish during graphics-test loading. This is completed execution evidence;
it lacks a captured changing graphics-test sequence and owner visual acceptance.
The later F6D00A83 repair changes OOM propagation and needs its own runtime receipt.
This is not a settings-equivalent, focused performance comparison establishing
a gain.

## Final native Port Royal and capability receipt

F6D00A83 completes the stock demo and graphics test, both status 0, with score
**12,163 / 56.31345 FPS**. Every resolved setting matches the accepted 057934F9
run: demo 1280x800, graphics 2560x1440, RT reflections and shadows enabled.
Demo PID 11076 and graphics PID 7276 each load system D3D12/Core 10.0.26100.9278,
DXGI 10.0.26100.9444, the exact F6D00A83 native UMD and ICD 43394BBD. WARP and
app-local engine/runtime substitution are absent. The installed application
is 3DMark 2.32.8454.0 with SystemInfo 5.92.1497.0; the PortRoyal workload executable
reports 1.0.0.0 and SHA256
`0E4B6B7896328157A3BE5281CF5A6E876242E4B9CF812186640B250B962ADA68`.

| Workload | Ordinary resets | Rotations | Reused generations | Additional generations created | Reset failures |
|---|---:|---:|---:|---:|---:|
| Demo 11076 |243718|102534|102211|323|0|
| Graphics 7276 |160104|42353|42180|173|0|

Both workloads have zero old pending-Reset errors and zero missing reset-error
callbacks. These are final per-process counters, not cache peaks. Full result,
XML export, module identities, stock definition and driver logs are archived in
`controls/allocator-epochs-final-portroyal/`; the review is
`allocator-epochs-final-portroyal-review.json`. The result SHA256 is
`39163374e6818ea7101cdb14002e33521c4beef0292702c9c4d61fcee516fff6`;
the XML SHA256 is
`091540792fa77b4e3bb6dab8f7f8da40d037d7cefb2e367434400dcff94808e7`.

Host VNC capture ran throughout completion and was stopped afterward. Its 91
captures include changing demo pixels and graphics-test frames 3434 and 5276
(57.png and 62.png), with advancing on-screen test time. Demo 20.png and GT 62.png
were accepted by the owner on 2026-09-12: "Looks correct". This acceptance applies
to the F6D00A83 final allocator build and its 12,163 result. The captures are
`tmp/allocator-epochs-20260911/portroyal-final-frames/20.png` and `62.png`.
Desktop capture and final inventory confirm a live desktop, Code 0, UmdD3D12=1
and no active probes or
benchmarks. Task definitions are restored to ordinary RT mode without rerunning.
No guest reboot, host renderer or VM-launcher change was made.

`final-native/`, `final-dxr/`, `final-no-rt/`, `final-sync/` and
`final-native-caps/` contain the final-build native receipts. Capability probes
8152 (normal) and 9356(RT extensions disabled) admit 11_0, 11_1, 12_0 and 12_1,
refuse 12_2 with 0x887a0004, report maximum 12_1 and SM6.3, and report RT1.0 versus
RT0 respectively. The no-RT state-object refusal/readback test also passes.
A physical non-RT GPU is still untested. The final source digest manifest matches root implementation
`1edb6366ab81e14248abf9e8b1176a19efdabc5e` and engine
`5e8b2998cc350dac3d1b6ddb40232ef4e66e8074`; the earlier intermediate benchmark is not
substituted for this final-build evidence.

The two candidates score 13,013 and 12,163; this variation is not attributed to
the OOM repair or presented as a measured speedup. No performance optimization
or baseline comparison is claimed. Remaining heap-allocation, concurrency and
host-loss fault injection are separate from the exercised ownership and sticky
error checks.

## Remaining acceptance

Full FL12_1/DXR compliance is not established by this allocator change. Committed
sparse fallback lacks mapping alias/residency semantics. DXR tools visualization,
extreme RT limits and other capability/behavior work remain open. General
DX12-to-DX11 external ownership and consumer release, host-loss error-bearing
completion, sharing/resize/rotation/teardown and async WSI stress retain their
existing limits. The owner accepted Port Royal separately on 057934F9 and this
F6D00A83 final allocator build. Time Spy, Fire Strike and Steel Nomad Vulkan
remain regression controls; their completed
057934F9 receipts do not constitute tests of this allocator increment. They
have not been rerun on F6D00A83.
