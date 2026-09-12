# Reserved-resource compatibility backing

**Owner scope, 2026-09-12:** retain this fallback for general testing. Do not
spend further time on complete sparse semantics; make bounded improvements only
when justified by real workload failures. A working host-driver sparse path is
the practical route back to native backing, selected through the existing
behavior probe. A driver update alone is not proof that the path works, and
does not validate every remaining D3D tiled-resource obligation. The documented
semantic and memory-cost exceptions remain; no new emulation project is authorized.

On 2026-09-08 the owner authorized the upstream committed-backing fallback and
required the advertised-but-broken color4 case to be handled dynamically. This
is an explicit exception to complete sparse semantics. Native FL12_0/FL12_1
conformance remains unestablished, with other independent gates still open.

[D32_COPY.md](D32_COPY.md) records the subsequent bit-exact D32 MSAA path,
15,687 passing host assertions and the minimal Mesa maintenance8 backport.
The owner activated upstream system virglrenderer, and Mesa BF021927 now exposes
maintenance8. UMD12 6F29859B includes the raw-copy implementation and a native
DXBC immediate-buffer repair. Three native committed-resource/shader controls
pass 1,536 words; they do not validate CopyTiles. Current UMD12 `2AD1D25D`
subsequently admits FL12_1/tiled2/RT0, and fourteen bounded native tiled cases
pass. These include the initialized D32 array case after the authorized guest
reboot: 98,304 exact sample words in both directions with no debug errors.
Full mapping/alias conformance remains the explicit exception. The earlier
failed process teardown remains unresolved stability evidence; the passing
repetition does not explain that failure. FEATURE_LEVELS.md records the native
receipts and the independent mixed-sample rasterization gap. Exact provenance
is in D32_COPY.md; the older records below retain their original artifact scope.

The upstream helper is already in the fetched object
`35bdee1435c94f8c3548725fcb046595b263bd7e`, and was present in our base engine
before the strict-refusal candidate removed it. No upstream merge, dependency
move, renderer modification or launcher change was needed. The source snapshots
and starting dirty state are in `tmp/fl12-sparse-compat-20260908/baseline.json`,
`baseline-dirty.tar.gz` and `upstream-{resource,command}.c`.

## Selection and dynamic evidence

`libs/vkd3d/reserved_compat.h` isolates allocation policy and diagnostics. The
ordinary upstream fallback covers unsupported single-aspect 2D sparse
format/sample combinations. Additionally, advertised four-sample color images
require a matching successful behavior probe before selecting sparse backing.
An absent, stale, malformed, failed or inconclusive record selects committed
backing. No GPU, device-ID or driver-version allowlist controls this decision.
Other supported sample counts retain their existing sparse path.

`tools/vulkan_sparse_behavior_probe.c` is a separate executable with its own
Vulkan device. Each color format has three phases: a committed four-sample
control, first sparse tiles in two array layers, then final edge tiles in both
layers. Clear/resolve/readback checks every addressed pixel and distinguishes
the two layers. A failed interior prevents attempting the edge that previously
caused device loss. Formats are R8/R16/R32/RG32 UINT, RGBA8 UNORM/SRGB and BGRA8.
Other color formats remain unverified and use compatibility backing.

Only all three completed phases with exact readback and no observed validation
errors/timeout produce PASS. A completed sparse mismatch/device loss is FAIL;
setup/resource-pressure failure or failed committed control is inconclusive.
The executable atomically publishes UNKNOWN before GPU work. A fence timeout
keeps pending allocations alive while waiting for completion or device loss;
it never treats timeout as completion or calls DeviceWaitIdle. Do not kill a
pending owner to manufacture a completed result. Existing host-loss retirement
limits still apply. A finite probe pass certifies these cases, not all possible
sparse accesses or the complete D3D12 tiled contract.

The shared, versioned record/reader is
`include/private/vkd3d_reserved_compat.h`. Its key contains public device,
driver and pipeline UUIDs; API/driver versions; format/sample count; and guest
adapter LUID. Venus additionally requires maintenance7's underlying Vulkan
driver UUID/ID/version and a fingerprint of the loaded Windows Venus ICD bytes.
Mesa's synthesized public UUID/version alone cannot identify a host-driver or
dirty-ICD change. Incomplete identity selects compatibility backing. The checksum
and fingerprint detect corruption/changes; this cache is not an authentication
or capability-admission channel.

Windows records live in `%ProgramData%\Helios\sparse-probe-v2`; Linux host
diagnostic records use `$XDG_CACHE_HOME/helios/sparse-probe-v2`, or
`$HOME/.cache/helios/sparse-probe-v2`. Source, artifacts and complete results are
archived separately. The engine reads records once at device creation and does
no probe, readback or file I/O per resource/frame. Re-run after a stack change;
new devices consume the new result. Already-created devices keep their decision.
Missing/stale records always keep the conservative compatibility path available.
There is no automatic scan in the UMD or installer: the separate preflight must
be run to obtain a positive result after an identity change. The renderer binary
itself is not fingerprinted by this Vulkan query; its underlying driver identity
and the guest adapter epoch are available. A changed guest adapter identity
invalidates records, as observed during this controlled adapter restart; the key
does not independently attest to every renderer or host-environment change.

Build and run in the guest through win MCP:

```powershell
powershell -File Z:\tools\vulkan-sparse-behavior-probe.ps1 -Mode Build
powershell -File Z:\tools\vulkan-sparse-behavior-probe.ps1 -Mode Schedule -ExpectedIcdSHA256 <full-loaded-ICD-hash>
```

`Schedule` uses an interactive task; each case is a separate process/device.
The runner records the inherited Vulkan loader selection and checks the loaded
ICD's exact hash. It preserves async present/retire feedback, snapshots build
inputs, and verifies archive copies. A completed diagnosis containing FAIL
records is not a graphics pass. This Vulkan probe never substitutes for native
Microsoft D3D12 runtime validation.

## Behavior and deliberate gaps

| Operation | Compatibility behavior |
|---|---|
| CreateReservedResource | Allocates the whole image with ordinary committed-memory ownership. OOM and invalid descriptions remain errors. No sparse VkImage flags or page-table initialization are used. |
| GetResourceTiling | Synthetic standard tiles for every mip, without a packed tail. Geometry supports CopyTiles; it does not describe physical residency. Unrecognized geometry fails. |
| UpdateTileMappings | Mapping, unmapping, SKIP and tile-reuse effects are ignored. Full backing remains resident. NULL tiles do not acquire sparse zero/discard semantics. |
| CopyTileMappings | If either endpoint uses compatibility backing, mapping effects are ignored in both directions. It neither creates aliases nor unbinds a genuine sparse destination. |
| Heap/resource lifetime | The image owns committed backing. Mapping heaps do not supply its pixels. Native prepared operations retain their borrowed objects through admission/cancellation. |
| Native queue boundary | Arguments are validated, then the ordered operation uses the existing HE12 runtime admission and authenticated GPU completion stream even though there are no sparse binds. No synthetic CPU completion is added. |
| CopyTiles | Real pixel/sample copies use barriers, staging and ordered queue continuations. Color4/D16 behavior is separate from sparse alias semantics. D32 uses maintenance8 transfers, now exposed by the installed renderer/ICD. Native single-sample, color4 and initialized D32 array copies pass on2AD1. Missing maintenance8 and active non-virtualized scoped queries remain explicit refusals. Depth predication and broader native format/failure coverage remain unexercised. See D32_COPY.md. |
| Reporting | Current native candidate admits FL12_1/tiled2/RT0. Engine format queries include logical compatibility support at1/4 samples, reject2/8 and128-bit4x, and exclude multi-aspect depth/stencil. Device creation guards the required backing. This does not restore sparse mapping/alias semantics. No force-admission override is introduced. |

`VKD3D_RESOURCE_RESERVED_COMPAT` marks a logical tiled image while retaining
`COMMITTED|ALLOCATION`. It must never be treated as `VKD3D_RESOURCE_RESERVED` by
binding/destruction logic. A worker guard rejects any accidental nonzero sparse
bind involving committed backing. `ReservedCompatResources`,
`ReservedCompatUpdateMappings` and `ReservedCompatCopyMappings` are per-device
atomic diagnostic counts. Native mapping counts describe prepared attempts,
including attempts that may later be canceled. They do not certify GPU work.
Mapping messages are rate-limited at powers of two.

This fallback trades sparse memory savings and alias semantics for compatibility.
It can increase VRAM use substantially; no performance gain is claimed. IA VBV/IBV
ExecuteIndirect now has bounded native acceptance (INDIRECT_EMULATION.md).
Predicated single-sample CopyTiles is implemented as described below. Raw D32
MSAA sample copies now have bounded native acceptance. Broader scoped-query,
format and failure coverage, depth predication and full native tiled acceptance
remain open.
DXR/Port Royal and FL12_2/Speed Way remain separate milestones. WDDM2.1, stock
virglrenderer, producer epochs, consumer release, present protection and the
pending-allocator-reset/fence-worker question are unchanged.

## Preceding predicated single-sample implementation, UMD12 BE9D0EBE

`libs/vkd3d/tiled_copy_command.h` now handles predicated buffer and single-sample
image tile copies. `cs_tiled_copy_rows.comp` reads the existing GPU snapshot
made by SetPredication and conditionally moves raw bytes. For images, Vulkan
transfers stage one standard tile; the shader touches only its valid rows and
depth slices. A buffer-to-image copy first snapshots the destination, so a
false predicate writes back identical bytes. No sampled float conversion is
used: BC blocks and single-sample D16/D32 bits remain raw transfer data.

The scratch allocation is one allocator-owned 64-KB tile per image-copy call,
independent of tile count. Buffer copies use device addresses directly. DWORD
copies cover aligned interiors; byte accesses preserve misaligned offsets and
partial edge rows. Barriers protect sparse aliases, staging reuse and shader /
transfer / host visibility. Queue continuations supply compute or depth-copy
support when the public queue cannot issue the internal commands. There is no
new CPU readback, prefix wait or device-idle wait in this path. False predicates
still incur staging transfers; no performance improvement is claimed.

Physical conditional rendering and virtualized scoped query fragments pause
around the internal work. Application bindings are invalidated for restoration
before later draws/dispatches. The active non-virtualized scoped-query case
still fails recording explicitly, as does unavailable 8-bit storage/shader or
pipeline/scratch allocation failure. These failure branches have source/build
coverage, not native fault-injection acceptance. Packed mip CopyTiles and
multi-aspect images remain explicit invalid recordings. Raw D32 **MSAA** copies
still return E_NOTIMPL; single-sample bit preservation does not close that gap.

The deployed release UMD12 is
`BE9D0EBE5F3A021848429A8ED0F641CC908BB1809DF8EC9F3192B1BD44F343A6`.
Its 876-input production manifest is
`2d1989ae0f260231c5c88aeef0ade441f84f44794e4799cfbed32383148c00b0`.
Source, Linux/Windows build logs, seven static archives, DLL and deployment
receipt are in `tmp/fl12-predicated-tiles-20260908/`. The Windows mirrors match
all inputs; LLVM22.1.8, SDK1.4.350.0 and local C: build directories are retained.
Mechanical checks pass against the actual root base dcdb8b38, including UMD
clippy and 213 kmd_logic tests. The first invocation used the script's obsolete
3e750c0 default and flagged existing protocol documentation warnings; the
current-base comparison confirms those four warning sites are unchanged. The
script now defaults to HEAD for dirty work; committed changesets need an
explicit pre-change ref.

Host validation (`host-validation.json`) totals 12,151 passing assertions with
zero skips/failures or Vulkan validation errors. Predicated texture cases cover
2D arrays, 3D, BC1/BC3, integer color, D16/D32, edge holes, unaligned offsets and
all three engine queue classes (2,701 assertions). Buffer/query cases check a
GPU-only high-DWORD predicate snapshot after its source is overwritten, false
destination preservation, inherited compute bindings and overlapping query
counts (35 assertions). The remaining assertions are existing tile/MSAA
regressions; D32 MSAA cases check the refusal, not working copies.

Native `native-validation.json` records Session1 IA12/48-word/query/lifetime and
all four ordering cases on BE9D, with exact system runtime/UMD/ICD identities and
a zero-loss loader trace for both ordering processes. Adapter LUID is
`00000000:0711b78c`, Code0, unchanged .270/oem53.inf/WDDM2.1, UMD11 and ICD.
The new native `copy-tiles-predicated` case builds, but exits BLOCKED77 at tiled0
before any tile commands. Caps remain FL11_0/tiled0/RT0. This is host engine
acceptance plus native regression evidence; native tile behavior and Port Royal
remain unaccepted. Per-PID diagnostic files append across PID reuse: old UMD
module sections in the archived file are not evidence about the current child.

All three native regression controls completed on BE9D, with archived/exported
results, matching stock settings and changing VNC frames. Time Spy is
134.943024/95.854523 FPS, Fire Strike167.547668/196.592453 FPS (combined45.349815),
and Steel Nomad **Vulkan**90.496490 FPS. The exact comparison to9BDA, workload
versions/hashes and frames are in `benchmark-controls-be9d.json`. Time Spy GT2
and Fire Strike graphics are materially lower; performance cause/acceptance
and owner visual acceptance remain open. The brief GPU observation during the
later Vulkan run does not attribute the earlier DirectX slowdowns. No native
tiled or RT acceptance follows from these completed controls.

The owner subsequently confirms concurrent GPU use. Further benchmark runs may
check completion and rendered correctness, but their scores/FPS must not be used
for performance conclusions while that shared load remains.

The raw-D32 investigation in [D32_COPY.md](D32_COPY.md) now distinguishes shader
loads from depth export/attachment storage. The earlier round trip first fails on
the denormal bit pattern `0x00000001` (format40, byte8 at offset0) in
`tmp/fl12-sparse-compat-20260908/copy-tiles-msaa-initial.log`. The host and Venus
inventories in `tmp/fl12-audit-20260907/vulkan-capability-matrix-20260907.json`
both report `shaderDenormPreserveFloat32=false`; requesting that execution mode
is therefore not a supported shortcut. Raw transfers, loads and integer fragment
witnesses preserve the bits; fragment depth storage loses them. The new
maintenance8 path avoids that conversion and passes host raw-bit checks.
The [tiled-copy contract](https://microsoft.github.io/DirectX-Specs/d3d/archive/D3D11_3_FunctionalSpec.htm)
describes swizzling/deswizzling and per-sample order, not depth-value conversion.

## Validation records

The immutable pre-compatibility failure record remains
`tmp/fl12-msaa-20260908/evidence.json` (candidate `8C7471DA...`). Current work is
archived separately under `tmp/fl12-sparse-compat-20260908/`. Host engine tests,
guest Vulkan diagnostics and native runtime admission are distinct evidence.

The root is `master` at `dcdb8b38a0556d554b005e90132ff0979fcc28ba`, with dirty
engine `71ebda7ec9eee8f826a65844710f3ab8fa1c1dd7` and compiler
`cc75a0c98d34d7bcc03560527c799b52e48b4d1f`. No commits or pushes were made.
The 874 production inputs are frozen in `production-source.tar.gz` and
`source-manifest.json`, whose SHA256 is
`0223d596610fe81c050d285b773ac08eae6b954d21595c3cc4fac99ce7dadad8`.
Every input matched the Windows build mirrors. Final test/probe inputs are
separately frozen in `verified-test-and-probe-source.tar.gz` and
`verified-test-source-manifest.json`; earlier failed test revisions remain in
their original records.

Windows engine and release UMD12 builds pass with LLVM22.1.8, Vulkan SDK1.4.350.0
and bindgen0.72/layout assertions. A1 is clean, including 213 KMD logic tests and
197 documented added unsafe sites. DLL imports retain the static engine model:
OpenAdapter12 is exported, with no dxgi.dll or separate vkd3d DLL dependency.
`candidate-verification.json` binds sources and seven static archives to UMD12
`9BDA548C4577008F237978A1658D53005227B0C0E3E0F0EF3DBA02E60BE401A6`.
This DLL is hotplugged at
`C:\ProgramData\HeliosUmd\helios_umd12_9bda548c4577008f.dll`, through the normal
deployment script and adapter restart. `deployment-9bda.json` records destination
hash verification. KMD22.22.270.0/oem53.inf remains Code0/WDDM2.1; UMD11
`245D1BC3…` and loaded ICD `3349607B…` are unchanged. The script's historical S5
warning is stale: the package contains an older UMD12; 9BDA is a ProgramData
override and was not installed into DriverStore.

| Validation | Completed evidence and limits |
|---|---|
| CPU cache reader | 18 outcomes pass, covering corrupt, stale, truncated, partial and valid records. Synthetic PASS tests do not run against the live cache. |
| Host Vulkan preflight | `host-probe-verified-results.json`: all seven committed controls pass; all seven sparse interior checks fail with half the addressed pixels incorrect, zero validation errors and no timeout. Edge phase is deliberately unexecuted after that failure. |
| Guest Vulkan preflight | `guest-behavior-20260908-155006-338-8e60c9d4/result.json`: completed in session1 with the exact loaded ICD hash; same seven FAIL results, zero validation errors/timeouts. The earlier 15:27 archive belongs to the preceding adapter epoch. |
| Host committed mappings | `test_reserved_compat_mappings`: 34 assertions pass. Independent backing survives ignored alias/copy/unmap operations and early mapping-heap release; queried VkImage sparse requirement count is zero. |
| Host MSAA CopyTiles | 4,239 assertions pass for each of `test_copy_tiles_msaa` and `_array`; color/D16, edge tiles, offsets, predicates and queue classes. Raw D32 cases validate explicit refusal, not bit preservation. |
| Host regressions | 5,174 assertions across CopyTiles/byte-offset/array tests, 6,291,594 across six copy-queue tests, indirect roots138, state/predication1,570 and query continuation140. Zero failures/skips and Vulkan validation errors. Receipts: `host-tests-verified.json`, `host-regressions-verified.json`. The exact-name `test_copy_queue` attempt matched no test and contributes no evidence. |
| Native D3D12 regressions | `native-validation-9bda.json`: root12/48 words, indirect12/48 words, SO34 and four ordering cases/65,536 words each pass. System D3D12/Core, exact 9BDA UMD and ICD identities are recorded; ordering processes also have ETW loader evidence. No WARP/app-local engine substitution. |
| Native cache consumption | LUID changed to `00000000:06376d04` on restart: PID6016 reads seven UNKNOWN records; after fresh preflight, PID8932 reads seven FAIL records. This validates native initialization and identity invalidation, not execution of native tiled operations. |
| Native admission | FL11_0/tiled0/RT0 unchanged; runtime maximum remains FL12_2 with R8_0110 negotiation. Native CopyTiles returns BLOCKED77 at tiled0. Compatibility mapping admission, sparse aliases, full limits, failure injection and the passing real-GPU probe branch remain unexercised. |

Time Spy, Fire Strike and Steel Nomad Vulkan controls completed with matching
stock settings, archived/exported results, loaded artifact identities and changing
host-VNC frames. `benchmark-controls-9bda.json` records exact comparisons and the
first Steel Nomad capture miss/repeat; owner visual acceptance remains pending.
ROADMAP.md lists the FPS and scores. Existing pending-allocator-reset diagnostics recur in Time Spy;
the fence-worker lifetime question remains unresolved. No performance change
is inferred from functional tests or cache selection.
