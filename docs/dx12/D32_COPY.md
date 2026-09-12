# Raw D32 multisample copies

## Current admission and validation

The current2AD1 UMD admits native FL12_1/tiled2 after the no-output sampling
repair; see [FEATURE_LEVELS.md](FEATURE_LEVELS.md#native-fl12_1-admission-candidate-2026-09-09)
for exact source/build/runtime provenance. Native color tile cases pass. The
D32 array probe needed full-tile dimensions and RT/DS metadata initialization:
its98,304 matching words alone did not pass the native debug layer. A clear
variant then exited1 during stalled process teardown. After the authorized
guest reboot, the same production2AD1 build passes the corrected initialized
case:98,304 raw sample words with no native debug errors, both directions,
independent integer producer/consumer and byte-offset1 tile layout. Evidence is
`tmp/fl12-maintenance8-native-20260908/native-depth-streamed-2ad1/run-20260909-004446-397-0e8a1987/`.
The earlier teardown cause remains unresolved. The runner now streams output
and bounds final draining, so such failures cannot hide behind an endless EOF
wait. This does not establish native predicated depth-copy or full sparse
mapping/alias conformance.

## Preceding raw-copy validation, 2026-09-08

The owner restarted QEMU with the normal launcher after installing upstream
system virglrenderer `cf6c62da`. QEMU PID1786532 maps system library SHA256
`923746b955cac2d845871da5ee166a36e7653e63c893e36599061a690f122619`; server
PID1786563 and its workers execute
`8af7e191718094aad273b8e4ee5940b66ae88dfeefa4316033cf2cffe769b873`.
The QEMU executable is
`f811fa17238840a5edb0bf3e511ba6f6ec8f0cd4603a4d12d4c925243737a726`.
No launcher modification or isolated-library override was needed.

Guest Mesa BF021927 (full hash below) is deployed and now exposes maintenance8.
The independent depth-export diagnostic still has the same 72/576 differences,
zero validation errors and no timeout. That diagnostic deliberately uses the
lossy fragment-depth route; it is not a failing raw-copy implementation test.
UMD12 81DE8971 was deployed first. Its native committed R32_UINT→D32→R32_UINT
MSAA control passed 512 raw comparisons. The same control using shader immediate
constants instead of an uploaded buffer failed 32 comparisons: signaling NaN
`7f812345` became `7fc12345` in the integer producer, before any depth copy.
Both incoming DXIL and emitted SPIR-V preserved the literal. This locates the
remaining loss after emitted float-typed SPIR-V, not in the D32 transfer.

The native runtime's DXBC converter represents immediate buffers as float32
arrays in DXIL address space5. These are raw register words: see Microsoft's
[ICB/register contract](https://microsoft.github.io/DirectX-Specs/d3d/archive/D3D11_3_FunctionalSpec.htm#7.5.1%20Immediate%20Constant%20Buffer)
and [converter implementation](https://github.com/microsoft/DirectXShaderCompiler/blob/main/projects/dxilconv/lib/DxbcConverter/DxbcConverter.cpp).
The dxil-spirv repair recognizes that address space, uses integer backing and
constants, restores float users with bitcasts, and pads/clamps ICB accesses for
mandatory zero OOB reads. Ordinary address-space0 float LUTs retain their old
representation. Constant/dynamic GEPs, zero-index aliases and array/scalar
pointer bitcasts preserve backing type, storage class and bounds. This adds no
CPU readback, GPU wait or per-resource allocation; one zero word per ICB supplies
the OOB value. It does not claim arbitrary new DXIL pointer support.

That raw-copy release UMD12 SHA256 is
`6F29859B2585912A241800628E2C66CA59E8079494661B3FFC707B3931ED96B5`, installed at
`C:\ProgramData\HeliosUmd\helios_umd12_6f29859b2585912a.dll` after adapter restart.
Its 877-input manifest is
`64fc0a76321b336425a3b6c21b72a6b93da60e3c935dc15339ef488169afb38c`; four compiler
inputs differ from the original D32 build. All 877 match the Windows mirror.
The immutable archive retains the UMD and seven static archives, import/export
checks, LLVM22.1.8/VulkanSDK1.4.350.0 and Windows check/release logs.

`tools/d3d12_tiled_probe.cpp` now includes three independent native controls.
The buffer-input and immediate-input committed MSAA cases each pass 512 words;
the ICB compute case passes another 512, including raw values, finite float
arithmetic, literal use and OOB/wrapping indices. Its actual DXBC ICB declaration
is checked before applying the OOB oracle. The preceding 81DE compute case had
10 mismatches, all the signaling NaN. The controls use the Microsoft debug layer,
own-queue fences, GPU readback, exact native identities and device-health checks.
All three pass on 6F29 with no debug errors. **They do not call CopyTiles or prove
tiled-resource conformance.** Native IA12/48-word/12-query/lifetime, root12/48-word,
SO34 and four ordering cases also pass. Loader traces lose zero events/buffers.

Ten compiler fixtures cover direct/dynamic/constant/OOB/constexpr GEPs, pointer
aliases and an ordinary private-array control; all emitted SPIR-V validates.
They are compiler checks, not native-runtime admission tests. All 44 existing
LLVM built-in shaders compile/validate and match their earlier output using the
same DXC. An attempted upstream golden comparison stopped at
`glitched-integer-width.comp` because the current DXC output differs; it is not
reported as an upstream golden-suite pass. A1 is clean, including 213 KMD logic
tests; Linux engine and Windows engine/check/release builds pass.

Evidence is in `tmp/fl12-maintenance8-native-20260908/`, particularly
`native-icb-fixed/run-20260908-225814-779-d05e4815/`, `compiler-fixtures/`,
`compiler-regression.json`, `icb-build/build-windows/` and
`fl12-sync-loader-6f29/`. Native runtime/Core remain10.0.26100.8972, DXGI
10.0.26100.9168; current LUID is `00000000:00d4a77b`. KMD22.22.270.0/oem53.inf,
Code0, WDDM2.1, UMD11 and explicit `UmdD3D12=1` are unchanged. Native caps remain
FL11_0/tiled0/RT0. Port Royal, full feature-level conformance and owner visual
acceptance remain open. The owner has stopped other GPU workloads, so new
completed benchmarks can measure performance; earlier shared-load results
remain excluded from performance attribution.

## Contract and implementation

`CopyTiles` moves raw sample data in sample-index order, preserving denormals,
signed zero, infinities and NaN payloads. The [D3D tiled-copy contract](https://microsoft.github.io/DirectX-Specs/d3d/archive/D3D11_3_FunctionalSpec.htm)
defines swizzling/deswizzling, not numeric conversion.

`vkd3d-proton-helios/libs/vkd3d/tiled_copy_depth.h` implements a separate raw
D32 MSAA path using `VK_KHR_maintenance8` and `VK_EXT_depth_range_unrestricted`.
[Maintenance8](https://docs.vulkan.org/refpages/latest/refpages/source/VK_KHR_maintenance8.html)
permits matching color/depth copies, including R32_UINT and D32_SFLOAT. Both
images have the same sample count. Multisample depth transfers require a
graphics-capable Vulkan queue; existing ordered continuations retain the
application's direct, compute or copy queue ordering.

Each recording owns one R32_UINT multisample staging image of one tile's
dimensions, with Vulkan's actual memory requirements and a dedicated allocation.
Its size is independent of tile count. Image, memory and view belong to the
command allocator, survive command-list Reset/replay, and retire with its
submitted buffers. Failed setup unwinds unsubmitted objects and records the
HRESULT. No CPU readback, GPU-idle wait or new completion mechanism is added.

Writes use the existing byte-offset-aware integer compute shader, then transfer
raw samples from staging to depth. Reads transfer depth to integer staging first.
Predicated writes snapshot the original destination into staging before the
conditional patch, preserving its bits when false. Predicated reads preserve
false destinations and edge padding. Barriers order staging reuse, aliased tiles,
shader accesses, transfers and host readback. Internal work suspends application
render/query fragments and physical conditional rendering; affected state is
invalidated and predication restored afterward.

Unvirtualized scoped queries remain an explicit refusal. Without maintenance8
or unrestricted depth copies, raw D32 MSAA CopyTiles records E_NOTIMPL. Ordinary
committed D32 rendering remains available. The old D32 fragment-depth pipeline
is unreachable from CopyTiles. The owner-authorized committed reserved fallback
still ignores mapping/unmapping/aliasing; this does not close that separate gap.

## Depth-export diagnosis before renderer activation, 2026-09-08

`tools/vulkan_depth_copy_probe.c` and its GLSL stages separate raw transfer,
sampling, fragment input/bitcast witnesses and depth storage. Images are 16x1,
with sixteen patterns and sample counts 1/4. Pure D32 bitcasts and the shared
D16/D32 conditional selection produce identical results:

| Stage | Host NVIDIA 610.57.04 | Guest installed ICD 3349607B | Guest staged ICD BF021927 |
|---|---|---|---|
| D32 buffer/image raw transfer | Exact | Exact | Exact |
| Sampling transfer-initialized D32 | Exact | Exact | Exact |
| Integer color witnesses of fragment input/bitcast | Exact | Exact | Exact |
| Fragment depth storage and subsequent sampling | Denormals flush; NaNs become `ff800000` | Same | Same |
| `VK_KHR_maintenance8` exposed | Yes | No | No; withheld by old renderer |

Each run completes four cases / 576 comparisons, with 72 differences and zero
validation errors/timeouts. These are completed **failing raw-bit diagnostics**.
Raw transfer readback locates loss after the integer witness, in depth export/
attachment storage; it does not distinguish those two hardware stages.
`shaderDenormPreserveFloat32=false` is separately queried on both stacks.

Windows probes run through interactive scheduled tasks in Session1 and record
loaded ICD identities. The staged ICD is selected with a process-local Vulkan
manifest and a normal interactive token; system registration is unchanged.
An elevated attempt ignored that override and loaded the installed ICD. The
exact hash gate rejected its receipt; it is excluded from staged-ICD evidence.
The probe's Schedule mode now uses a normal token. This is Vulkan boundary
evidence, not native D3D12 conformance. No app-local D3D12 runtime or WARP is
substituted. An earlier wrapper serialization failure happened after the GPU
process exited; that partial archive is also retained. Corrected runs archive
completely, including all 22 source/build artifacts.

## Original D32 source and build provenance

Root remains dirty `master` at `dcdb8b38a0556d554b005e90132ff0979fcc28ba`,
engine at `71ebda7ec9eee8f826a65844710f3ab8fa1c1dd7` with prior changes intact.
The 877-input UMD/engine manifest is
`666abc35e14d3fc3eb59c6b2d62bea325658b9a8e4724216c24e5c8520396bd5`;
all inputs match the Windows build mirror. Release UMD12 SHA256 is
`81DE897168BA45CB22B2B600C38DC91CBD655332628BDF987D96BDC55634EB08`.

Mesa remains at `a04516a702dff81d3a2e44019cdd79abf3fb7423` plus eight files.
Its Windows O2/debugoptimized build SHA256 is
`BF0219279E958E18BD76170FE8D2BB1AB3937BCB5ED98B5EDF2B2C2312966767`.
Receipts preserve the existing MinGW configuration. UMD/engine retain LLVM22.1.8,
VulkanSDK1.4.350.0, local C: targets and bindgen0.72. The embedded Mesa git string
is not used as source provenance.

Mesa feature forwarding is backported from upstream
`6d8f1495dfc8d12e279a5927ebdbbecf1cf5677b`. Latest Mesa was fetched/inspected
at `d253ffa22c4f8436f9a9abf976ec429bc6c02168`; that support is distinct from
this checkout and the installed ICD. The protocol generator remains
`70991d4c7e4e5a7bfa2fbb8a6e77e4eac350145d`, with only the maintenance8
extension-list entry from upstream `e2544a38cfd466a89604fd8065fa4adc6fb9c1aa`.

Regeneration first reproduced the original unmodified headers. Applying only
the generated maintenance8 delta preserves all four Helios protocol customizations.
Feature serialization and access-flags3 chains are included; no command IDs are
added. Encoder, renderer protocol and renderer device support all gate exposure.
To reproduce, check out the generator revision in an isolated directory, generate
`OLD` with `python vn_protocol.py --outdir OLD`, add `'VK_KHR_maintenance8'` after
maintenance7 in the extension list, generate `NEW`, then apply only the header
diff. Keep Helios amendments and update the banner. Exact patches are archived.
Do not import maintenance9-11 as part of this backport.

The previously installed stock virglrenderer 1.3.0-2 lacked maintenance8 forwarding/protocol
support. A clean, **unmodified upstream** renderer at
`cf6c62da2a1384b194f463e6221371962fe99575`, with protocol1.1.3
`ca19b6358d7cc491bc3e4de76f04c6700876a8fa`, is built at
`target/linux/virglrenderer-maintenance8-install`. Upstream version metadata
still says1.3.0; the revision identifies the newer source. Library SHA256 is
`fe3fd6a1994aad8b7cf4101155f4e4838db19071f517645b6e55d5215642e459`;
server SHA256 is `4e407fe607833429d8447fba2df9333a29ce2e9f7db25321dc56d70b25394a21`.
All installed virgl public API symbols remain. Unit tests were not built because
Check is absent. No renderer VM acceptance follows from this build.

## Validation and activation

Host tests disable DGC/descriptor-buffer/descriptor-heap extensions and enable
Vulkan synchronization validation:

| Test | Assertions | Result |
|---|---|---|
| MSAA copies, raw D32 and closed-list replay | 4,356 | Pass |
| Array/edge MSAA copies, raw D32 and replay | 4,356 | Pass |
| maintenance8 disabled: refusals and other MSAA controls | 4,239 | Pass; not D32 copy support |
| Predicated single-sample tiles | 2,701 | Pass |
| Predicated buffer/query isolation | 35 | Pass |

All 15,687 assertions pass with zero skips/failures or Vulkan validation errors.
A1 is clean, including 213 kmd_logic tests. Linux engine, Windows engine/release
UMD and Mesa builds pass; existing compiler warnings are retained in records.
Broader inherited/format/ROV/conservative/DXR acceptance and the pending allocator
Reset/fence-worker lifetime question remain open.

Evidence is in `tmp/fl12-d32-copy-20260908/`: source manifests/patches, immutable
Windows builds, stock renderer receipt, host logs and completed guest archives.
`validation.json` grades the final installed-ICD run
`guest-final/guest-depth-20260908-201441-923-06c87c92` and staged-ICD run
`staged-icd-final/guest-depth-20260908-201459-972-ebf77587`, and records the
excluded elevated attempt. The final probe executable SHA256 is
`50E8A04EFB4E29C13A3E90C7941AEF8D1540A74729A4873D2B07DBA9D05E9D0D`.
The staged Mesa probe on the old renderer proves that updating Mesa alone does
not expose maintenance8. Native admission remains FL11_0/tiled0/RT0; Port Royal
has not run against these staged builds. Installed BE9D/3349607B, .270/oem53.inf,
WDDM2.1 and native architecture are unchanged. These runs predate the owner's
system renderer update below. No guest driver deployment, launcher restart,
commit or push occurred.

`final-guest-state.json` confirms Code0, `UmdD3D12=1`, unchanged DWM-loaded
UMD11/ICD and no remaining benchmark/probe processes; both diagnostic tasks are
Ready. `final-desktop.png` is a host-VNC capture of the baseline
desktop. It does not establish owner visual acceptance of the staged candidate.

The owner subsequently installed system package
`virglrenderer-git virglrenderer_1.3.0_121_gcf6c62da-1`, then restarted the normal
launcher. The current activation and native results are recorded at the top.
`owner-system-renderer.json` describes the intermediate state with the old
processes still mapped; it is not the current running identity.

The earlier isolated renderer wrapper and `owner-restart-command.sh` remain
unused historical preparation. They are unnecessary with the owner's system
replacement. No Linux host Mesa update is required: virglrenderer uses the
NVIDIA Vulkan driver, which already exposed maintenance8. The guest Mesa update
is now deployed. Remaining native tiled/inherited/format/ROV/conservative/DXR
obligations must be addressed before increasing admission.
