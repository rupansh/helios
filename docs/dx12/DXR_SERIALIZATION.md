# DXR serialization and reference lifetimes

The engine now prepares referenced Vulkan AS objects at the execution boundary
before deserializing an ordinary DXR acceleration structure. This closes the
TLAS-first, later-BLAS-recording gap in the host engine tests. Native DXR remains
unadmitted; this document does not establish a complete DXR tier or Port Royal
acceptance.

The subsequent AS input repair is deployed as UMD12 `8F15F9DC…`; the section
below records its engine and native regression evidence. The adapter admission
repair was deployed as UMD12 `898F75F9…`.
[FEATURE_LEVELS.md](FEATURE_LEVELS.md#adapter-admission-contract-2026-09-10)
records its direct DDI and native FL/ordering tests. That adapter increment kept
all seven static engine archives byte-identical to 2D90C57E; its AS implementation
and DXR cap did not change.
The dated results retain their original binary and scope. The current native
RT0 query confirms that Port Royal remains blocked.

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
