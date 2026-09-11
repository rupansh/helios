# DXR serialization and reference lifetimes

**Allocator follow-up, 2026-09-11:** UMD12 F6D00A83 preserves execution-owned
command-pool generations and recycles them after engine retirement. Native DXR,
conditional no-RT and ordering probes pass. [ALLOCATOR_LIFETIME.md](ALLOCATOR_LIFETIME.md)
contains the new contract and benchmark/lifetime receipt; older pending-reset
counts below remain observations of their explicitly identified builds.


## Conditional DXR support

Release UMD12 `465CBE13528F1BA7802200D1E620DD16D9B00F082ABFE22AA4227C2B180206CF`
fixes an unconditional RT requirement in the preceding 057934F9 native admission.
An otherwise eligible non-RT engine now retains native FL11_0..12_1 admission
and reports `RaytracingTier=NOT_SUPPORTED`. RT-capable engines report the
implemented RT1.0 tier. Shader models are capped at the engine's actual support
and the UMD's SM6.3 ceiling. Admission still refuses feature/shader overrides;
this is no capability override or RT emulation.

Adapter discovery and native device creation use the same static bridge, compare
engine capabilities and Vulkan device UUID, and release a refused device through
its existing owner. The bridge retains the discovery engine for transfer into
the first native device, or normal release on caps-only `CloseAdapter`. Permanent
process caching holds metadata only and does not cache a failed query. No queue synchronization, backing lifetime, renderer/KMD interface,
WDDM2.1 version, or async-WSI policy changed. The single-guest-adapter limitation
is unchanged. See [architecture](ARCHITECTURE.md#optional-adapter-capabilities).

| Requirement / optional feature | Host Vulkan | Venus / renderer | vkd3d | Helios / native runtime | Validation |
|---|---|---|---|---|---|
| Optional DXR1.0 | AS + RT pipeline features, required formats and shader-table limits | Mesa intersects its implemented passthrough set with renderer-advertised extensions; feature/property queries are conditional. Renderer starts from actual host enumeration. | Derives `options5.RaytracingTier`; no UMD-side extension-name heuristic | Native state-object/AS/ray callbacks retain engine-tier refusals; caps expose at most1.0 only with SM>=6.3 | Native positive state/AS/ray readback; RT-disabled API refusal and ordinary GPU readback |
| No RT support | RT extensions/features absent | `VN_DEBUG=no_ray_tracing` removes RT exposure at the ICD for the restricted-feature test | Also tested with engine-only extension suppression | Reports RT0 without rejecting an otherwise eligible FL12_1 device | Native tier/creation checks and four ordering cases; actual non-RT hardware remains untested |
| Shader models | Engine-required subgroup/float-controls features | Existing feature/property transport | Real `max_shader_model` | Explicit API-to-DDI release-token translation,5.1..min(engine,6.3) | Native6.3 in both modes; a lower-SM engine remains unexercised |

Source boundaries: `vn_physical_device_init_supported_extensions` and its
conditional feature/property chains in Mesa; `vkr_physical_device_init_extensions`
in the renderer; `d3d12_device_determine_ray_tracing_tier` in the engine;
`caps12::native_optional_caps`, `BridgeDevice12` and `raytracing::require_engine_dxr`
in the UMD. The last rejects unbacked AS/state/dispatch commands instead of
silently dropping them. Native creation still requires the existing base-feature
guards; this change does not claim support for every GPU regardless of those
requirements.

Evidence is under `tmp/dxr-conditional-20260911/`. `owned-source-manifest.json` and
`owned-build-windows/provenance.json` reconcile 63 mirrored driver/engine sources and
freeze the release DLL/static archives. The snapshot was collected after the
build and verified against its Windows local mirrors; it is not a pre-build
attestation. Engine code is committed locally as38ecb6f7286b8df5573cd619384cc4e6dc71aa26;
the frozen build was produced from bb46e7c6 plus those verified working-tree inputs.
The commit does not change the already captured build/version metadata;
compiler f4651bd0, Mesa2d4e910b, renderer2121d5d0 and protocolfe08e82c are unchanged.
LLVM/libclang22.1.8, VulkanSDK1.4.350.0 and bindgen0.72 remain. The subsequent
probe changes have their own independent build receipts and are not linked into
the DLL. Linux/Windows engine builds, Windows release UMD, and A1 pass, including
211 KMD logic tests. Deployment is a ProgramData override, verified by hash,
with a device disable/enable; .271/oem54/Code0 and UMD11/ICD remain unchanged.
It is not signed-package or hosted-CI validation.

Final465CBE13 acceptance, with System32 D3D12/Core10.0.26100.9278,
DXGI10.0.26100.9444 and exact UMD465CBE13 / ICD43394BBD identities:

| Configuration | Native caps / admission | GPU and refusal checks |
|---|---|---|
| Ordinary RT | PID5756: FL11_0..12_1 S_OK;12_2 unsupported; SM6.3 / RT1.0 | PID1908: eight DXR behavior groups and20 ray words pass; PID3584: all four ordering cases,65,536 words each |
| RT extensions suppressed in engine | PID3168: same FL admission / SM6.3, RT0 | PID9468: valid RT pipeline refused with E_INVALIDARG, then4,096 GPU readback words pass; PID11248: all four ordering cases pass with original deadlines |
| Only ray-query extension suppressed | PID2296: native RT1.0 remains available; DXR1.0 does not depend on ray query | Capability check only; no RT1.1 claim |
| RT exposure disabled in Venus ICD | PID2080: same FL admission / SM6.3, RT0; `VKD3D_DISABLE_EXTENSIONS` absent | PID8456: all four native ordering cases pass; `VN_DEBUG=no_ray_tracing` is process-local |

Receipts: `owned-native-caps/`, `owned-native-dxr/`, `owned-native-no-rt/`,
`owned-sync-rt/`, `owned-sync-no-rt/`, `venus-no-rt-caps/`, `venus-sync-no-rt/`.
The original10-second cross-process helper deadline is unchanged. These tests
close the discovered startup regression; they do not measure benchmark speed.
Repeated adapter/device creation, normal cleanup and cross-process shared-fence
signalling are exercised. Concurrent caps initialization, a changed physical
UUID, retry after failed discovery, lower-SM hardware and caps-only cleanup are
implemented but not specifically fault-injected/exercised by this suite.

The first candidate41BAC916 passed the positive fixture (PID2604), passes all eight behavior groups plus the
completion marker and all20 ray-result words. Its restricted-RT fixture,
PID10320, reports RT0, refuses the valid pipeline with E_INVALIDARG (0x80070057),
and passes all4,096 readback words. Both return0 with hash-verified module
identities and archived results under `verified-native-dxr/` and
`verified-native-no-rt/`. Earlier original-suite PID7040 also passed on this DLL.
The helper refuses to replace an already-running scheduled probe task.
However,41BAC916 failed the four-case ordering control: three cases passed,
then the cross-process helper exceeded the unchanged10-second startup deadline
during a second engine creation. That candidate discarded its discovery engine.
The465CBE13 candidate instead transfers that engine to the native device. The
failed `sync-rt/` receipt is retained; no timeout was increased and no queued
wait, completion or resource-lifetime check was removed.

The first candidate's native Windows cap probes PID11044 (normal) and PID5360 (RT extensions disabled)
load exactly 41BAC916/43394BBD and System32 D3D12/Core10.0.26100.9278,
DXGI10.0.26100.9444. Both admit FL11_0,11_1,12_0,12_1 and refuse12_2; both report
SM6.3, while their RT tiers are10 and0 respectively. The restricted process uses
`VKD3D_DISABLE_EXTENSIONS=VK_KHR_ray_tracing_pipeline,VK_KHR_acceleration_structure,VK_KHR_ray_query`:
these features are removed from engine discovery and device creation. This is a
missing-feature test on the current RT GPU, **not validation on actual non-RT
hardware**. No feature-level or shader-model override is set. Raw PID logs can
contain earlier PID-reuse records; module hashes and each run's API output are
the attribution evidence, not whole-file historical counter totals.

`tools/d3d12-raytracing-probe.ps1 -WithoutRaytracing` is the repeatable negative
mode. A valid minimal RT state object is created in the positive mode; the same
descriptor is refused in the negative mode, followed by a 4,096-word upload ->
default GPU buffer -> readback comparison and authenticated fence completion on
the same FL12_1 device. The existing positive suite retains cross-queue AS/ray
ordering, bundles, compaction/cloning/updating, serialization/relocation and
lifetime checks. Every graphical probe runs in an interactive scheduled task.
An earlier negative fixture failed before this boundary at local-root creation;
an intermediate positive fixture completed GPU checks but duplicated module
records and failed attribution. Neither failed receipt is treated as acceptance.

The owner-accepted Port Royal result below remains scoped to 057934F9. This
capability change has no new benchmark score, performance claim, or owner visual
acceptance. Full FL12_1/DXR compliance remains open: committed sparse fallback
mapping is not alias/residency compliance, tools visualization still refuses,
and pending allocator/fence-worker retirement remains unresolved.

The current engine's RT tier is also not proof of every DXR limit. Its
`d3d12_device_determine_ray_tracing_tier` deliberately admits recursion depths
below31, deferring failure until pipeline creation. The current NVIDIA host
reports depth31, stride4096 and dispatch limit2^30, but AS geometry/instance
limits2^24-1 and primitive limit2^29-1 (`host-vulkaninfo.txt`), whereas Microsoft's
[geometry-limit specification](https://microsoft.github.io/DirectX-Specs/d3d/Raytracing.html#geometry-limits)
states2^24 and2^29. The extreme-count boundary is unexercised and unresolved;
this is an additional conformance audit item, not evidence that Port Royal failed.
Native limit gating and meaningful limit tests still need to distinguish
vkd3d's compatibility reporting from the full Microsoft contract.


## Completed native Port Royal, 2026-09-11

The stock Port Royal collection completes on release UMD12
`057934F90BFBC8B50D1A50FAC1AECB53661658311F9DCAC762CA1289EC4F7F05`.
Demo PID 11064 and graphics-test PID 2668 run in session 1 on Helios LUID
`00000000:002572e3`; both workload statuses are 0. The graphics score is 12,337,
GT1 is 57.118721 FPS (export 57.12). Resolved graphics settings are 2560×1440,
ray-traced reflections, RT shadows enabled, medium reflection filtering and
16 temporal-AA samples. Demo rendering is 1280×800. The unmodified stock
`.3dmdef` SHA256 is `3D4EE2596D8295E6C5255E4B4EB282434C8ECB2CC8B71A7A930A797DB5A0351B`.
This is a single completed result, not a before/after performance claim.

Both processes load the exact 057934F9 native UMD, Mesa ICD 43394BBD and Microsoft
System32 D3D12/Core 10.0.26100.9278 / DXGI 10.0.26100.9444. WARP and app-local
vkd3d substitution are excluded by the recorded module lists. The wrapper uses
HELIOS_WSI_ASYNC_PRESENT=1, ordinary logging, no retired feedback workaround and
no feature/shader overrides. Host VNC captures 42 frames with 33 distinct hashes;
manual inspection of demo 13.png and graphics 35.png/39.png shows different
rendered scenes, including GT1 frame 2311→5011. **The owner visually accepted
this Port Royal run on 2026-09-11: "Looks correct"**, responding to demo 13.png
and graphics 35.png. Acceptance applies to the exact 057934F9 artifact and recorded
settings. A black transition capture 24.png is between the completed workloads,
not evidence of a frozen benchmark. The subsequent owner acknowledgment is
recorded in `owner-portroyal-visual-acceptance.json` under the evidence directory;
earlier receipts retain their original pending status.

Evidence under `tmp/dxr-native-admission-20260911/`:

- `controls/native-dxr-export-portroyal/`: invocation, resolved settings, loaded
  modules, driver logs, result archive and XML export; the wrapper hash-verifies
  copies from the guest's local C: output.
- `native-dxr-export-portroyal-review.json`: workload-status/settings/identity
  checks, scores, output and frame hashes. Result archive SHA256
  `e72bbbcac2ee7f8dc579073f19894360479c9529ab4aee4a5f399a11e4cd147c`;
  XML `6db9677677a40998a04d483a85895fc93446563ed886efe8553040d8555257b1`.
- `portroyal-export-frames/`: timestamped host captures; no focus-taking observer.
- `final-native-caps/` and `final-native-sync/`: the installed 057934F9 build
  admits FL11_0 through FL12_1, refuses FL12_2, reports SM6.3/RT1.0 and passes all four native
  ordering cases, each with 65,536 exact readback words and loaded identities.
- `guest-final.json`: .271/oem54/Code0, explicit UmdD3D12=1, unchanged UMD11/ICD,
  LLVM 22.1.8 and Vulkan SDK 1.4.350.0. No new signed package or hosted-CI validation.
  `kmd-artifact.json` records the running service and installed DriverStore image
  SHA256 `BEE454883A4800EA38ABE0C271DDF48EF0A1F087C6D7E75D2972F3473CC0738B`;
  it is an on-disk service-image hash, not an in-memory kernel dump.

Each Port Royal workload creates 29 RT state objects. Demo forwards 249,932 AS
builds and 32,534 ray dispatches; GT1 forwards 121,634 builds and 18,552 dispatches.
No native DXR refusal is recorded. Tools visualization is unexercised and remains
unsupported; this successful workload does not prove complete DXR conformance.
Other existing gaps remain, including sparse compatibility, arbitrary alias and
host-loss lifetimes, native RT1.1 and the estimated TotalLaneCount 1024. The logs
retain 91,993 demo / 41,715 GT1 pending-allocator-reset errors. Engine Reset returns
success without resetting when internal references remain; completion of this
benchmark does not prove those references are merely delayed worker bookkeeping.
The next focused lifetime probe must distinguish that case from pending GPU use.

The implementation is committed locally at root `c65b77e`, with engine admission
`54e759e1` and opt-in RT diagnostics `bb46e7c6`. Nothing new was pushed. Mesa
`2d4e910bd04`, DXIL compiler `f4651bd0`, renderer `2121d5d0` and protocol `fe08e82c`
remain. The deployment was built before those commits from source/patch inputs
frozen in `export-build-windows/`. `committed-build-reconciliation.json` checks
all 63 final input files: 62 are byte-identical to committed source; the remaining
file differs only by the corrected module comment. Export/trace input manifests
inherited their first inventory's UTC/repo-status metadata, while updating file
hashes; use the build-receipt UTC and the reconciliation for the final build.
Source commits do not imply a rebuild or a new deployed binary. The three
regression controls below complete on this same deployed stack; older scores
remain bound to their original builds.

## Completed regression controls, 2026-09-11

All stock workloads complete through interactive scheduled tasks, with status 0,
archived `.3dmark-result` files and XML exports. Resolved rendering settings match
the preceding completed 22C31F11 controls after excluding only run-result UUIDs,
output paths and adapter LUIDs. These are single completed runs; no performance
gain or regression is attributed to this DXR increment.

| Control | Completed workloads | Graphics score | Measured FPS |
|---|---|---:|---|
| Time Spy | Demo, GT1, GT2, CPU | 23,816 | GT1 164.54; GT2 130.06; CPU 55.68 |
| Fire Strike | Demo, GT1, GT2, physics, combined | 59,231 | GT1 254.75; GT2 260.37; physics 129.88; combined 45.39 |
| Steel Nomad Vulkan | SteelNomadGt1VK | 9,409 | 94.10 |

Overall scores are Time Spy 22,351 and Fire Strike 37,628. All four Time Spy
processes load the exact 057934F9 UMD12, 43394BBD ICD and Microsoft System32
D3D12/Core. All five Fire Strike processes load the unchanged 57C84ED4 DX11 UMD
and System32 D3D11. Steel Nomad loads the exact ICD and uses the Vulkan workload;
it does not load UMD12 or D3D12Core. All workload processes are in session 1;
WARP and app-local vkd3d substitution are excluded by the module receipts.

Under `tmp/dxr-native-admission-20260911/`, `controls/native-dxr-export-*`
contains the results, settings, identities and logs. Matching `*-review.json`
and `*-settings.json` files check completion, loaded implementation, output
hashes and rendering-setting equivalence. `controls-summary.json` aggregates
those receipts. The settings checker initially flagged differently named LUID
and result-UUID fields; the retained `*-settings-initial.json` files show those
non-rendering differences. No benchmark rerun or setting change was used to
resolve them.

`frame-inspection.json` records manually viewed host-VNC pairs: Port Royal GT1
frame 2311→5011, Time Spy GT1 frame 2188→3753, Fire Strike GT2 frame 7637→10211 and
Steel Nomad Vulkan frame 545→1192. These show changing scenes. The owner accepted
Port Royal as recorded above; visual acceptance of these three regression
controls remains pending. No paintcap or focus-taking observer ran. Existing
.266 shadow/~100FPS acceptance and the instrumented 74.26 FPS run remain separate.

`benchmark-artifacts.json` refreshes installed x64 workload hashes/versions:
Time Spy 1.2.6.5, Fire Strike 1.1.0.0, Steel Nomad 1.0.5.1, Port Royal 1.0.0.0 and
Speed Way 1.1.1.2 (not run). These are executable file versions, not inferred
benchmark-engine/catalog versions. The CLI is 2.32.8454. `guest-complete.json`
records unchanged .271/oem54/Code0, UmdD3D12=1, boot, UMD11/ICD and toolchain,
with no remaining benchmark/probe process or running task from this work. The
host capture processes have exited. No launcher restart was performed.

The native Time Spy logs still emit pending-allocator-reset diagnostics; both
that lifetime question and the broader ownership/consumer-release/host-loss
limits remain open. The source/build tests and successful benchmark completion
do not establish full FL12_0/12_1, DXR or WSI conformance.


## Public export associations, 2026-09-11

Release UMD12 `057934F90BFBC8B50D1A50FAC1AECB53661658311F9DCAC762CA1289EC4F7F05`
fixes the next Port Royal state-object boundary. Its RTPSO trace showed native
associations using internal symbols such as `\1?rayGen@@YAXXZ` while explicit
library exports gave vkd3d only the public name `rayGen`. The association missed;
vkd3d fell back to conflicting default local roots and failed to remap SRV1:0.

The frontend now retains explicit public export names, prefers an exact declared
mangled/public name for each runtime summary, and uses its former mangled-name
fallback for unfiltered libraries. This preserves aliases without collapsing
unfiltered overloaded functions to a common plain name. Implicit collection imports
and Add inherit independently owned name metadata; filtered/renamed imports retain
their new names. Allocation is fallible and all metadata is released with its
state object. No queue, shader-table GPU lifetime or execution policy changed.

The native probe now has a distinct raygen local root with an SRV at t0/space1,
a GPU address in its shader record, and an explicit `TraceRayGen` alias. The same
executable/shader fails on E21352DA at CreateStateObject with SRV1:0 unmapped, then
passes all seven groups and 20 ray-result words on 057934F9 (PID 8620/session 1).
The pipeline outlives its source collection and local roots. This is a reproduced
native regression; the earlier public-config poison experiment was not retained
as a test because it did not reproduce the separate payload overread.

Release/A1 pass, including 211 KMD logic tests. All seven static archives match
the E21352DA trace build. The engine's existing RT construction diagnostics are
now available in release only at explicit VKD3D_DEBUG=trace; default logging
remains unchanged. Linux/Windows engine builds pass. Known inputs and frozen
artifacts are in `export-source-manifest.json` / `export-build-windows`, native
before/after receipts in `export-before` / `export-after`, under
`tmp/dxr-native-admission-20260911/`. The register-space-only probe separately
passes on E76997FA and E21352DA; it did not reproduce the explicit-export defect.

Two trace attempts stopped before DXR at exclusive-fullscreen initialization;
they are not evidence about the binding failure. Authorized guest reboot at
2026-09-11 00:52:43 +05:30 restored benchmark entry, and the next traced run
reproduced the binding failure with exact system-runtime/Helios/ICD identities.
KMD .271/oem54/Code0/WDDM2.1, UMD11 and ICD remain. Port Royal subsequently
completes on 057934F9 with ordinary logging and receives owner visual acceptance,
as recorded above. Namespace OOM, overloaded-library and Add-specific
runtime tests remain separate from the covered aliased collection case.

## RT1.0 pipeline-config repair, 2026-09-11

The first admitted Port Royal run on AC1818B6 fails both workloads at native
CreateStateObject (workload status 10000, score 0, no exported result). VNC shows
loading screens, not a rendered benchmark sequence. Diagnostic UMD12
`82E16046D3EF422EB44AFE7712A4BF86412B927F1B693E17B47BC2121D341F7D`
records depth 1 with invalid trailing Flags 0x6c617645/0x56666472. The driver had
assumed DDI0110 always supplies the eight-byte RT1.1 `_0075` payload. For the
advertised RT1.0 contract it now reads only the four-byte `_0054` depth and
forwards API CONFIG instead of CONFIG1. No unknown flag is silently masked.
RT1.1 payload selection remains a separate obligation before raising that tier.

Release UMD12 `E76997FAAFDA50BDAF8B8AAB380E47D7F326AE7FAA54EE3A9B41C834DB55FE6F`
is deployed with the same KMD/ICD/UMD11 and seven unchanged static engine archives.
Windows release/A1 pass; the native DXR probe again passes all seven groups,
PID 6304/session 1 with exact E76997FA/43394BBD/system-runtime module identities.
The strengthened probe places a poison word after its public depth-only config;
it also passed on 82E16046, so it is not a reproduction of the observed Port Royal
DDI overread. Failed and diagnostic benchmark archives, input/build hashes and
native before/after receipts are separate directories under
`tmp/dxr-native-admission-20260911/`. Port Royal is being rerun on the repaired
build; no completed benchmark or performance claim follows from the probe.

## Native DXR admission and readback, 2026-09-11

Release UMD12 `AC1818B6CBFBDAD8AE80D610D431E067F04763CF040EA04852B89C418C786946`
reports RT1.0 and the gapless release shader-model list 5.1/6.0/6.1/6.2/6.3.
The static engine admission guard checks its Vulkan-derived SM>=6.3 and RT>=1.0
before publishing every native device, independent of the requested FL. Existing
feature/shader overrides remain refused. Optional SM features retain their own
caps; this does not enable mesh, VRS, native16-bit or sampler feedback.

Helios reuses the existing vkd3d DXR engine through the installed `misc.rs` /
`raytracing.rs` forwards. The native shader-table range stride now preserves all
64 bits and rejects out-of-range values instead of applying the unrelated
geometry-address stride truncation rule. Valid shader tables are exercised below;
the upper-bit negative case has not been independently exercised through the DDI.

The interactive native Windows probe, PID 8220/session 1, passes all seven behavior
groups and 20 ray-result words: triangle/AABB/callable hit/miss results,
collection/export/local-root retention, direct/compute ordering, explicit and
inherited-root bundles, compact/clone/update and source lifetimes, and serialized
BLAS/TLAS relocation with TLAS completion before BLAS recording. Serialized sizes
and reference-count queries agree with readback; a foreign driver identifier is
rejected. The log records 2 state objects, 3 prebuild queries, 4 AS builds, 3 postbuild
queries, 8 copies and 5 ray dispatches. These are native frontend results, distinct
from the earlier direct engine tests. Tools visualization remains explicitly
unsupported and no full DXR conformance claim follows; arbitrary alias/lifetime,
malformed inputs and host-loss behavior still need broader coverage.

The probe loads Microsoft System32 D3D12/Core 10.0.26100.9278, DXGI 10.0.26100.9444,
the exact AC1818B6 UMD12 and ICD 43394BBD on Helios1af4:1050; no WARP or app-local
engine substitute. Native creation separately admits FL11_0 through12_1, refuses
12_2, and reports SM6.3/RT1.0. All four ordinary ordering cases pass 65,536 words
each with the same loaded identities. KMD 22.22.271.0/oem54/Code0/WDDM2.1 and
UMD11 remain unchanged; this is a ProgramData deployment, not a signed package
or hosted-CI result. Async WSI remains enabled; no retired feedback workaround.

Linux engine, Windows static engine, release UMD and A1 pass (211 KMD logic tests).
Source/build/mirror receipts, all seven frozen archives, native results and hashes
are in `tmp/dxr-native-admission-20260911/`. Build source is root d831e6b plus the
caps/stride changes, engine 10efa8af plus its admission guard, Mesa 2d4e910bd04 and
DXIL f4651bd0. Compiler LLVM/libclang22.1.8, Vulkan SDK 1.4.350.0 and bindgen0.72
remain. The renderer file hash still matches 06ce3964; reading the privileged live
process mappings was unavailable in this run, so this is not fresh loaded-host
hash verification. The existing QEMU/renderer processes were not restarted.

Port Royal is now eligible for its first admitted native correctness run. Its
completion, changing rendered frames and owner visual acceptance remain separate
from these probe results. The tools-visualization experiment remains set aside;
no Port Royal trace has established it as a dependency. Dated sections below
retain their original artifacts and RT0 scope.

## AS build inputs and prebuild failures, 2026-09-10

Release UMD12 SHA256
`8F15F9DCE8BD7A53420469BA8805497D71309A83E9E341B300592D98C8602F20`
is installed at
`C:\ProgramData\HeliosUmd\helios_umd12_8f15f9dce8bd7a53.dll`.
`tmp/dxr-build-inputs-20260910/` contains the incremental patch, frozen source,
build/deployment receipts, raw tests and diagnostic attribution. Root master
remains dcdb8b38; engine HEAD remains 71ebda7e with accumulated uncommitted work.
No source was committed or pushed.

The engine now validates ordinary AS type/layout/flags, CPU descriptor-array
presence and DXR geometry/instance limits before allocating or choosing a union
arm. Conversion rejects null geometry pointers, unknown geometry flags and
aggregate primitive overflow. AABBCount is checked before its UINT64-to-UINT32
narrowing. Malformed builds fail command-list Close; a null outer build or
nonempty null postbuild array is rejected before dereference. Existing batch
rollback preserves earlier valid records. These are defensive engine refusals,
not proof that the native runtime forwards every malformed API call to the DDI.

Prebuild output is cleared before conversion/allocation can fail. All three
temporary arrays beyond the 16-entry stack are checked and freed on failure.
Update scratch is zero without ALLOW_UPDATE, including the existing hidden
rebuild-sizing policy; that application's policy branch was not separately
exercised. Both vertex and AABB strides use only their low 32 bits, as required
by the shared [DXR GPU address/stride structure](https://microsoft.github.io/DirectX-Specs/d3d/Raytracing.html#d3d12_gpu_virtual_address_and_stride).
Native DDI translation already masked them; the public engine API did not.
Prebuild accepts dummy nonzero GPU addresses, including unaligned/near-overflow
values, without dereferencing or validating their allocations. Actual build
address and GPU-content validation remain separate obligations.

| Engine test | Direct NVIDIA 610.57.04 | Paired Linux Mesa/Venus |
|---|---:|---:|
| Input rejection and valid array/pointer prebuild | 180 checks, pass | 180 checks, pass |
| Three individually injected prebuild allocation failures | 22 checks, pass | 22 checks, pass |
| High stride bits: build/update/copy/serialize/ray readback | 428 checks, pass | 428 checks, pass |
| Prior recording/copy/serialization/replay/failure fixtures | 33,859 checks, pass | 33,859 checks, pass |

Each stack completes 34,489 checks with zero failures/skips/todos. The final
test executable `852EEC30…` reproduces stale poisoned prebuild output with the
frozen old engine (17 checks, one failure), then safely stops before its unchecked
null/oversized cases. Candidate negative lists are never submitted. The glibc
LD_PRELOAD fixture is test-only: it fails each of three array allocations, checks
zero remaining sibling allocations and sizes, then verifies successful retry.
It skips explicitly without its hook; an initial skipped run is excluded. A
Meson compiler-variable typo and hidden hook exports were repaired before the
accepted builds/runs. Linux builds retain unrelated existing warnings; Windows
release UMD has zero Rust errors/warnings.

High-stride GPU validation retains 27 raw 07791 creation-view overlap diagnostics
with duplicate limit 1,000, and zero synchronization hazards. The current trace
and `copy-range-attribution.json` establish disjoint accessed ranges against the
fixture's 4,864-byte prebuild bound for all 27 pairs. The input-refusal fixture
has 17 raw 03703 scratch/view diagnostics on unsubmitted lists. Neither result
is called clean Vulkan validation; arbitrary alias/extent cases remain open.

Linux/Windows engine, release UMD and A1 pass, including 211 KMD logic tests.
The Windows receipt verifies 154 mirrored inputs and freezes all seven static
archives; LLVM 22.1.8, SDK 1.4.350.0, bindgen 0.72 and layout checks remain.
Adapter restart deployed only the ProgramData UMD12 increment: .271/oem54/Code0,
WDDM2.1, UMD11 57C84ED4… and ICD 43394BBD… remain. QEMU PID5244 and renderer
5277 still map the paired local binaries, library/server hashes 06ce3964… and
afed7176…. No guest or launcher restart occurred. Signed package/hosted CI
validation remains separate.

Interactive native caps PID2864 and ordering PIDs1744/8340 loaded exact 8F15F9DC
UMD12 and 43394BBD ICD with System32 D3D12/Core/DXGI; no WARP/app-local engine.
FL11_0 through 12_1 creation and all four 65,536-word ordering cases pass;
FL12_2 is refused and native caps remain FL12_1/tiled2/SM6.0/RT0. Host VNC shows
a healthy desktop. These native tests do not exercise the new RT code, which
remains unadmitted. No benchmark, owner DXR visual acceptance or performance
comparison was obtained.

Tools visualization was investigated but not implemented in this increment.
The current Vulkan/Venus AS surface exposes opaque copies/serialization, not
the DXR decoded-geometry representation. Keeping pointers to the application's
original buffers cannot satisfy release, update, copy and deserialization
lifetimes. The next work is an allocation-owned GPU representation and its
storage/query/copy/serialized-reference contract, before enabling native DXR
and exercising state objects, DXIL, shader tables and DispatchRays. The native
tools-size and tools-decode refusals remain; Port Royal is not ready.

## AS address and copy-range validation, 2026-09-10

That increment's ProgramData release UMD12 was
`2D90C57E34DF012734EDA1334C6E50A0D73EDA201D160041481F7C5811A9CA17`.
`tmp/dxr-copy-ranges-20260910/` records its source, build, deployment and tests.
Native admission remains FL12_1/tiled2/SM6.0/RT0. This work does not enable DXR
or establish Port Royal readiness.

The engine rejects malformed CPU-visible AS addresses before issuing Vulkan
commands: null/unaligned build destinations and update sources, null/unaligned
copy endpoints, self copies and null/unaligned postbuild sources. VA-view lookup
checks the address against the allocation extent before subtracting it. Invalid
copy modes are rejected before creating the destination view. Build failures
retain the existing transactional batch rollback and fail Close; this was
already implemented for placement failures, but zero addresses bypassed that
placement. GPU-only serialized contents still resolve at execution, and views
remain allocation-owned. No size/type cache, idle wait or new submission split
was added. The separate engine OMM surface retains its 128-byte source alignment;
the native ordinary-AS DDI still requires256 and does not admit OMM.

A new recording-rejection fixture checks13 cases, including a valid pending
build before each malformed operation. None is submitted. The corrected fixture
reproduces old-engine S_OK for a misaligned clone source, then the candidate
passes157 assertions, returns E_INVALIDARG for every case and leaves the device
healthy. The first fixture version had a null teardown-owner bug after observing
that S_OK; its crash is preserved and excluded. A missing IDL constant caused
one intermediate build failure and was repaired before passing builds.

The serialization fixture now also reuses three slots in one AS buffer, with a
256-byte base offset, through triangle BLAS -> AABB BLAS -> TLAS. For each of
seven source objects it clones to slot2, compacts backwards to slot0, clones
forwards to slot1, queries GPU current/compacted sizes, then serializes slot1.
Its existing restored-reference checks, closed-list replay, late BLAS recording
and later ray-hit/miss checks consume this output. This exercises21 additional
copies; the small and8,194-reference variants share code.

| Engine test | Direct NVIDIA610.57.04 | Paired Linux Mesa/Venus |
|---|---:|---:|
| Recording rejection |157 checks, pass|157 checks, pass|
| Packed copies + serialization/replay |428 checks, pass|428 checks, pass|
|8,194-reference variant |33,168 checks, pass|33,168 checks, pass|
| Five malformed GPU headers |106 checks, pass|106 checks, pass|

Each stack completes33,859 checks with zero failures/skips/todos. These are
engine/transport tests, not native Windows DXR acceptance.

### What the Vulkan diagnostics actually establish

[Vulkan copy valid usage](https://docs.vulkan.org/refpages/latest/refpages/source/VkCopyAccelerationStructureInfoKHR.html)
constrains the memory actually accessed. The1.4.357 validation layer instead
compares `VkAccelerationStructureCreateInfoKHR::size` in
`ValidateAccelStructsMemoryDoNotOverlap`; `GetSize()` returns that creation size.
It also constructs `range(offset, size)` with the size in the end-position field.
Our canonical views extend from an AS address to the allocation end, to support
future recording, GPU-only type/size changes and allocation-owned alias views.

The expanded small fixture retains27 raw07791 diagnostics, with duplicate
reporting increased to1,000 and no message filter. Every reported pair is in the
same backing buffer and its start offsets differ by at least4,864 bytes, the
fixture's maximum prebuild allocation bound. GPU-reported current/compacted
sizes for all seven copied objects also fit the4,864-byte slots. Original-copy
prebuild extents, packed-slot addresses and the complete attribution calculation
are retained in `copy-range-attribution.json`. These messages therefore do not
show an accessed-range violation in this fixture. They are not silently removed
or counted as clean Vulkan validation. The recording-only fixture also reports13
03703 scratch/AS creation-view overlaps; those are preserved separately and do
not invalidate the observed Close refusal. General arbitrary alias/reallocation
and actual out-of-bounds GPU-content cases still need their own evidence.

Do not shrink canonical views to the nearest other address to silence this
check: Vulkan's [AS aliasing rules](https://docs.vulkan.org/spec/latest/chapters/resources.html#resources-memory-aliasing)
require an alias to cover the original build-size requirement. A compacted
object's current size, or a CPU cache from a different recording, cannot provide
that guarantee.

The expanded test initially produced synchronization-layer complaints about
AS_COPY. Vulkan defines AS_BUILD to include AS copies, so those messages alone
are not proof that the old execution dependency was invalid. The candidate
also spells out AS_COPY when maintenance1 is enabled in legacy UAV/AS barriers
and the internal AS barrier. The final expanded synchronization-validation run
has zero synchronization hazards and retains the27 range diagnostics above.
No real-driver synchronization-failure or performance-gain claim follows from
this explicit stage mask.

Linux/Windows engine, release UMD12 and A1 pass, including211 KMD logic tests.
The Windows receipt verifies152 mirrored inputs and seven static archives;
LLVM22.1.8, VulkanSDK1.4.350.0, bindgen0.72 and layout assertions remain.
The owner had restarted QEMU before this work: current PID5244 and renderer5277
map the paired local renderer; its library/server hashes remain06ce3964… and
afed7176…. Guest boot is2026-09-10T21:27:40.5000000+05:30. This deployment only
restarted the adapter, leaving .271/oem54/Code0/WDDM2.1, UMD11 57C84ED4… and ICD
43394BBD… unchanged. The signed DriverStore UMD12 remains older.

Interactive native Windows validation on this exact2D90C57E build passes19
DGC-query/filter/ROV/conservative/TIR groups,781,199 assertions, with212 captured
compiler inputs and native executable
`D19903C36DD1F04E9F5F5814EDC3CABC7E077E2FE8B5FE9E401F00E3367D01D7`.
All four ordering cases pass65,536 words each, including cross-process shared
fences. Native inventory creates FL11_0 through FL12_1 and refuses FL12_2;
FL12_1/tiled2/binding3/ROV1/conservative3/SM6.0/RT0 remain. The new native DXR
probe, PID7356/session1, loads the exact UMD/ICD and exits BLOCKED77 before RT
commands. It does not validate the new engine address refusals through the DDI.
Loaded modules are system D3D12/Core10.0.26100.9278, DXGI10.0.26100.9444 and the
expected Helios UMD/ICD; WARP and app-local substitution are excluded. The final
guest receipt is Code0, same boot, no remaining graphical probes. Host VNC shows
a healthy desktop, not owner acceptance of a benchmark scene. No benchmark or
performance comparison was run on this build. Earlier22C31F11 control results
remain separately attributed. General sharing/consumer release, host-loss
callback, sparse compatibility and pending allocator Reset remain open.

Tools visualization remains an explicit unsupported boundary: Helios refuses
postbuild type1 and copy mode2, and the engine has no implementation of the
DXR decoded-geometry output. Vulkan's AS copy/serialization commands and the
current Venus protocol expose no corresponding general decoded-geometry mode.
Adding a Venus enum alone cannot make the proprietary host AS format readable.
The next DXR subsystem is a complete tools-visualization representation and its
build/update/copy/serialization lifetimes, followed by native state-object,
DXIL, shader-table and DispatchRays validation. RT0 remains the first native
Port Royal admission boundary. No renderer, KMD, ICD, launcher or feature-level
change, commit or push occurred in this increment.

## Execution-time serialization queries, 2026-09-10

Serialization postbuild queries now derive both fields from the AS contents at
execution. This covers the immediate Build postbuild form and the separate Emit
form, including a list recorded before its producer and replay after a GPU-only
type change. The engine no longer authorizes a TLAS-only pointer-count query or
invents a BLAS count from `rtas_kind` recorded earlier. That bookkeeping remains
for view/copy diagnostics; it is not query admission evidence.

The metadata continuation now distinguishes DESERIALIZE from SERIALIZATION_QUERY.
For each serialization query, the queue worker uses the existing exact timeline
dependency, unlocked waits and NVIDIA fence-proxy protocol to:

1. Query the current Vulkan serialization size and copy its eight bytes into
   host-coherent staging. Allocate device storage from that completed result,
   with checked space for the required 256-byte address alignment.
2. Serialize the AS into that storage, copy the public 56-byte header into
   staging, and wait for the copy. Validate its size and reference-count extent.
   The public KHR opacity-micromap block marker identifies a BLAS with no BLAS
   references; its block count must not be returned as a reference count.
3. Write the validated 16-byte DXR result on the GPU, bridge transfer access to
   subsequent UAV/read/copy consumers, and retire temporary objects only after
   authenticated completion. The queued suffix consumes the exact final value.

This is a cold compatibility path with three completion waits per query and
temporary storage proportional to the actual serialized size. It does not run
for ordinary builds without serialization postbuild requests or for ray dispatch.
No performance claim is made. There are no queue/device-idle waits, opaque AS
layout guesses, CPU type caches, feature overrides or changes to native DGC.
Recording failures fail Close; execution allocation/metadata failures remove the
device and stop the suffix. Uncertain completion quarantines temporary objects
and execution allocator references. Generic-engine scoped queries that cannot be split remain explicitly refused.
The final section records why that refusal is unreachable on the paired native
DGC surface; scoped metadata-operation coverage is still separate.

`tmp/dxr-postbuild-20260910/` contains exact source/build receipts and tests. On
both direct NVIDIA 610.57.04 and the paired Linux Mesa/Venus stack, the small
fixture passes 398 checks, the 8,194-reference fixture passes 33,138 and the five
malformed-deserialization cases pass 106: **33,642 checks per stack**, zero
failures/skips/todos/bugs. The query list is closed before the initial producer,
then observes TLAS -> BLAS -> TLAS at one VA. Each replay checks three adjacent
16-byte results at an 8-byte destination offset, untouched guards and agreement
with an independently serialized GPU header. Later ray dispatch still checks
the scene. Arbitrary resource alias/reallocation patterns, injected allocation
or host-loss failures, opacity micromaps and every query/predicate combination
remain unexercised.

Vulkan validation 1.4.357 and its synchronization-validation run each pass all 398
checks with no synchronization hazards, but retain six
`VUID-VkCopyAccelerationStructureInfoKHR-dst-07791` diagnostics from the fixture's
clone/compact views extending to the heap end. These still require accessed-range
analysis; neither run is clean Vulkan-validation acceptance. The previous three
generic-AS pointer-count diagnostics disappear because this path no longer uses
the TLAS-only query. The [1.4.357 layer source](https://github.com/KhronosGroup/Vulkan-ValidationLayers/blob/v1.4.357/layers/core_checks/cc_ray_tracing.cpp)
tests the creation type against TOP_LEVEL, although the
[Vulkan requirement](https://docs.vulkan.org/refpages/latest/refpages/source/vkCmdWriteAccelerationStructuresPropertiesKHR.html)
concerns the built type. The fetched main source accepts GENERIC at that check;
both source snapshots and their hashes are retained. No system layer was changed.

Linux engine/tests, Windows static engine and release UMD12 build; A1 passes
including 211 KMD logic tests. The deployed ProgramData UMD12 is
`5B8A411E148B96804776782353E68C46B82C4DBC444F352F67D8588FF65BADB7`;
`build-windows/provenance.json` verifies 145 mirrored source inputs and all seven
static archives. The new native probe also requires a serialization query after
TLAS restoration completes and before recording BLAS restores. Native RT remains
unadmitted; that new native assertion is not exercised while RT0 is reported.

After adapter restart, interactive native Windows validation loads that exact
UMD12, ICD `43394BBDEB29912BAE531C5AA5D016AD6EC1AA285FDC7F45297F48ACFD64DD36`
and the system D3D12/Core 10.0.26100.9278 and DXGI 10.0.26100.9444:

- All 13 native ROV/conservative/TIR groups pass 778,722 checks and 420 readback
  records. The unchanged native test executable remains `5750F3E6…`; the copied
  build receipt and 139 archived inputs are rechecked in `native-validation.json`.
- All four native ordering cases pass 65,536 words each: producer first,
  consumer first, CPU producer completion and cross-process shared fence.
  `native-sync/` retains per-process loaded module hashes and completed results.
- The revised DXR probe, PID 10600/session 1, admits native FL12_1 and exits
  BLOCKED77 at RaytracingTier0. Its new query assertion and other native DXR
  commands remain unreachable, not passed.

The adapter remains .271/oem54/Code0, with explicit `UmdD3D12=1` and the same
boot. UMD11 remains `57C84ED403DC8DEC476E8C8B7648EEEC137A207C5C5D3EDF00D2EB18C9010992`;
the signed DriverStore UMD12 remains the older 41A7 package artifact. The
ProgramData override is not a new signed package or hosted-CI result. Host VNC
shows a healthy desktop, not owner acceptance of a benchmark scene. QEMU and
render-server PIDs and on-disk renderer hashes are unchanged; fresh root-owned
`/proc` mapping inspection was unavailable without sudo authentication.
Optional RGBA32_FLOAT sample 16 and the 35 pending-allocator-reset diagnostics remain
separate limits. No benchmark or performance comparison was run.

Root remains master and all dependency heads are unchanged. No commit or push,
KMD/ICD/renderer change, guest reboot or launcher restart occurred. The existing
WDDM2.1, FL12_1/tiled2/RT0/SM6.0 surface and separate conformance/Port Royal
acceptance requirements remain in force.

## Contract and implementation

The [DXR serialization contract](https://microsoft.github.io/DirectX-Specs/d3d/Raytracing.html#d3d12_serialized_raytracing_acceleration_structure_header)
permits the destination BLAS contents to arrive after TLAS deserialization.
[Vulkan deserialization](https://docs.vulkan.org/refpages/latest/refpages/source/vkCmdCopyMemoryToAccelerationStructureKHR.html)
requires the referenced BLAS objects to have been created first. Creating those
objects while recording later BLAS operations only covers recordings that happen
before TLAS execution. It does not implement independent submissions.

`vkd3d-proton-helios/libs/vkd3d/rtas_deserialize.c` owns the metadata reader.
`acceleration_structure.c` and `command.c` place a mandatory command-stream split
before each DESERIALIZE and snapshot its source/destination GPU addresses into
the queued execution. No serialization bytes are read at recording, Close, or
ExecuteCommandLists admission. Ordinary execution batches allocate no metadata
array and retain their batching behavior.

At the boundary, the queue worker:

1. Submits a copy of the public 56-byte header, waiting on the preceding stream's
   exact timeline value. It waits for this copy's authenticated completion with
   the queue lock released. The existing NVIDIA fence-proxy tracking applies to
   these timeline values too.
2. Checks serialized size, deserialized size, pointer-table arithmetic, mapped
   source/destination extents and the Vulkan producer/version compatibility.
3. Copies only the validated reference table, in chunks of at most 64 KiB. It
   creates/reuses generic AS views for nonzero, aligned, backed BLAS addresses.
   Null entries are ignored; duplicate addresses reuse the VA view map. An AS
   view does not claim that BLAS contents have been initialized.
4. Submits the recorded restore and suffix with the exact metadata-copy timeline
   dependency, including when the suffix uses a fallback queue.

Header and table are separate reads so the driver never speculatively accesses
other allocations in a shared heap. The staging memory is host coherent;
transfer-to-host barriers and GPU completion precede each CPU read. One staging
buffer and command pool are reused within the operation and destroyed only after
completion. There is no queue/device-idle wait or CPU polling of a mapped GPU word.
This is a cold deserialization path, with one GPU readback for a BLAS and at least
two for a TLAS. No benchmark performance gain is claimed.

Every execution/replay reads current GPU data again. Application GPU-resource
lifetimes still cover the queued work; allocator references retain the recorded
commands. VA-map views remain allocation owned. Deserialization marks the
recorded destination type MUTATED: a formerly known TLAS/BLAS type cannot survive
a GPU-only overwrite and incorrectly authorize a type-specific query.

Recording allocation failures fail Close. Invalid GPU metadata removes the
engine device before the restore/suffix is submitted. A successful prefix stays
owned until its actual retirement. Failed submission or uncertain GPU completion
quarantines metadata objects and execution allocator references; it does not
fabricate a completion. The existing host-loss callback/teardown limitation is
not solved by quarantine. Mandatory splits with unvirtualized active queries
remain explicitly refused; the native DGC surface virtualizes its supported
statistics queries, but broader query combinations need their own validation.

## Evidence, 2026-09-09

`tmp/dxr-serialization-20260909/` contains the starting diff snapshots, exact
source copies and hashes, build logs, guest inventory and test outputs. Root
remains master at `dcdb8b38a0556d554b005e90132ff0979fcc28ba`; engine HEAD remains
`71ebda7ec9eee8f826a65844710f3ab8fa1c1dd7` with the accumulated dirty changes.
No dependency ref was moved, and nothing was committed or pushed.

| Test | Direct NVIDIA | Paired Linux Mesa/Venus + renderer | Behavior established |
|---|---:|---:|---|
| `test_raytracing_serialization` | 336 checks, pass | 336 checks, pass | Two replays of the same closed TLAS-only list, nine relocated references, 256-byte AS offsets, TLAS serialized/read back before any BLAS restore is recorded, later BLAS restore and ray-hit/miss output |
| `test_raytracing_serialization_large` | 33,076 checks, pass | 33,076 checks, pass | 8,194 references across two table chunks, the same ordering/replay/readback checks |
| `test_raytracing_serialization_rejection` | 106 checks, pass | 106 checks, pass | Five GPU-produced malformed headers: zero/overflow size, oversized destination, overflow pointer count and incompatible UUID; exact prefix marker survives, suffix marker is absent, removal is reported |

Each valid run has zero failures, skips, todos and bugs. The rejection fixture
checks Helios engine failure containment for invalid input; it is not a portable
D3D12 requirement for other drivers. These are engine/transport tests, not native
Windows DDI or owner-visible scene acceptance. The small and large fixtures share
code and must not be counted as independent implementations of the contract.
Null references, arbitrary alias/reallocation patterns, injected OOM/host loss,
and every query/predicate combination remain unexercised.

The initial host test failed because its replay list used the graphics family
with a compute queue. Its original run also exposed missing fence-proxy tracking
for the new timeline values. Both were repaired before the passing runs. The
first Linux Venus invocation selected the install manifest instead of the local
build manifest and skipped device creation; it is not a pass. Only the
`venus-devenv-*` outputs use the verified local ICD and execute these checks.

The final Vulkan-validation run completes all 336 checks but reports nine
existing fixture-path diagnostics: six AS clone/compact memory-range overlaps
(the VA views extend to the heap end) and three generic-AS/type diagnostics for
serialization pointer-count queries. No clean Vulkan-validation claim is made;
the accessed-range and built-type requirements must be reconciled with these
view and query implementations before RT admission. Successful readback does not
resolve a validation diagnostic.

Linux engine/tests, Windows clang-cl static engine and release UMD12 build.
A1 passes, including 211 KMD logic tests. LLVM/libclang22.1.8, VulkanSDK1.4.350.0,
bindgen0.72 and layout assertions remain intact. The release UMD12 is
`CBABC0B8E2CEC41AD5AA19E84C988BF093892A60734B47818EA98E87C9AEC4E2`;
`build-windows/provenance.json` verifies 145 mirrored source inputs and records
all seven static archives. The native probe now requires TLAS completion before
recording the BLAS restores; its build receipt pins the revised probe and grader.

The release UMD12 was hotplugged with an adapter restart, without a guest reboot
or launcher change. The adapter remains .271/oem54.inf/Code0 and WDDM2.1.
UMD11 remains `57C84ED403DC8DEC476E8C8B7648EEEC137A207C5C5D3EDF00D2EB18C9010992`;
the ICD remains `43394BBDEB29912BAE531C5AA5D016AD6EC1AA285FDC7F45297F48ACFD64DD36`.
This is a ProgramData override; the signed DriverStore UMD12 still carries 41A7.
UmdD3D12 is explicitly DWORD1. `guest-before.json` and `guest-after.json` record
the unchanged boot and system D3D12/Core10.0.26100.9278, DXGI10.0.26100.9444.

Interactive scheduled tasks using the native Windows runtime and exact Helios
adapter establish:

- All 13 ROV/conservative/TIR groups pass: 778,722 checks and 420 TIR readback
  records. The test executable is
  `5750F3E6E8F757D10CC217FDC20123707BCA648FC5B4A5F9A2DF38E8A204E864`.
  `native-inputs/receipt.json` records 13 translation units and 139 source,
  dependency, object and binary inputs; `native-validation.json` rechecks them.
  TIR positive GPU cases inspect InfoQueue; the ROV/conservative fixtures do not.
- All four native synchronization cases pass, each checking 65,536 words:
  producer first, consumer first, CPU producer completion and cross-process
  shared-fence ordering. `native-sync/` contains exact loaded module receipts.
- The revised native DXR probe admits FL12_1, loads the new UMD12 and unchanged
  ICD through the system runtime, then exits BLOCKED77 at RaytracingTier0.
  Its AS/state-object/DispatchRays paths are unreachable in this admitted native
  surface and have not been validated by this run.

The native tests retain the optional RGBA32_FLOAT sample16 restriction. They
also emit 34 pending-allocator-reset diagnostics; that pre-existing lifetime
question remains unresolved. Persisting/defaulted capability and refusal
counters are recorded, not presented as all zero. Host VNC captures show the
desktop before and after. No benchmark was run, no performance comparison was
made, and no later benchmark scene has owner visual acceptance. All test tasks
completed and the temporary Linux vtest server was stopped.

## Remaining admission boundary

Native RaytracingTier remains NOT_SUPPORTED and SM remains 6.0. A 2026-09-10
source recheck narrows the old unvirtualized-query claim: native FL12 admission
requires EXT DGC (`helios_vkd3d_validate_native_feature_level`), which makes
pipeline statistics inline in `d3d12_query_heap_create`. Occlusion and SO are
always inline; timestamps have no BeginQuery scope. Consequently the
`active_non_inline_running_queries` refusal is unreachable for valid scoped
queries on this paired native surface. It remains a real engine refusal without
DGC, and this source result does not validate AS metadata operations inside
virtualized query scopes. Tools-visualization copy/postbuild
formats, the remaining Vulkan AS copy-range diagnostics above, and native
state-object/DXIL/AS/DispatchRays behavior remain open. The complete native
FL12_0/FL12_1 contract and completed, owner-accepted Port Royal are separate
requirements. No force-admission flag or feature-level override was introduced.
WDDM2.1, async WSI, consumer release, exact admission/completion, and the existing
sharing/allocator-reset/host-loss acceptance limits are preserved.
