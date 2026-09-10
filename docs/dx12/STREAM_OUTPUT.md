# Native stream output

The 2026-09-07 owner-directed FL12_1 audit found that Helios discarded every
stream-output declaration while still creating its shader. Stream output is an
inherited D3D12 obligation. The implementation in this changeset closes the
native declaration/compiler path described below; the remaining valid-case
refusals mean that this is **not full native stream-output conformance** and
must not be used to promote the feature level.

## Contract and implementation

The native `_0026` geometry-shader-with-stream-output DDI owns the declaration,
strides and `RasterizedStream` after creation. The runtime's arrays are borrowed
only during that call. Shader destruction frees these owned arrays. PSO creation
passes a `STREAM_OUTPUT` subobject; vkd3d copies its entries, semantic names and
strides before returning, including deferred compilation paths.

The WDK binding defines `D3D12_SO_DDI_REGISTER_INDEX_DENOTING_GAP` as `0xffffffff`.
A gap retains its skipped component count and a null API semantic. A non-gap
DDI entry denotes physical register components, and can cross packed logical
semantics. Helios emits one entry per selected component using the private,
exact engine semantic `__HELIOS_DDI_SO_REGISTER`: `SemanticIndex` is the physical
register, `StartComponent` is the absolute component and `Stream` is unchanged.
The private `helios_vkd3d_create_stream_output_pipeline` factory explicitly sets
the per-PSO DDI origin; a semantic string alone cannot select this convention.
All public PSO decoders clear that origin. The engine owns it with the XFB
metadata, indirect variants preserve it, and the central compatibility hash
includes both values to invalidate pre-repair cached interpretations.
The engine validates this private coordinate convention. It does not match
against the fabricated OSG1 semantic names: DXIL's original metadata, which the
DDI does not provide, is the shader compiler's source of truth.

The compiler's added component callback carries both original DXIL semantics
and register/column/stream coordinates. Public semantic matching remains
available for the engine's developer harness. Each non-gap declaration must
match all requested components; an unmatched declaration fails PSO compilation.
The original stream-output callback and its public C struct retain their ABI.

Complete, contiguous, uniquely captured output variables receive XFB
decorations directly. Unique partial captures of user-defined outputs split the
original output into scalar variables at the same `Location` and `Component`;
these serve rasterization and capture together. DXIL's original vector/array
storage becomes private. Built-in partial capture, repeated capture and
clip/cull capture use separate scalar outputs, mirrored from `StoreOutput`.
Dynamic row writes select the appropriate fixed capture row. Captured scalar
values are 32-bit, before raster-only value adjustments. Gaps and stride padding
are not written.

A null GS program is retained as an SO-only object. The PSO has no geometry
bytecode, allowing the engine to choose domain-shader output when tessellation
is active, or vertex-shader output otherwise. The root signature must carry
`ALLOW_STREAM_OUTPUT`; vkd3d rejects a PSO that lacks it. Counts, pointers,
streams, slots, masks, component windows and stride alignment/limits are checked
before arrays are retained. The handle is cleared before SO validation or
allocation. Malformed declarations report `E_INVALIDARG` through the device
callback. Validated component counts determine fallible reservations for both
owned arrays; no push can grow them, and Vec ownership avoids an infallible
boxed-slice conversion. Reservation failure increments
`L6StreamOutputOutOfMemory` and reports `E_OUTOFMEMORY` through that callback.
This recovery path only updates atomic counters; it defers formatted diagnostic
summaries to normal later readouts, including when the error callback is absent.
No driver allocation precedes the OOM callback or the failed call's return.
Engine PSO failures return HRESULT.

SO target GPU addresses, buffer-filled-size locations, barriers, per-stream
statistics, queue ordering and resource lifetimes remain on the existing engine
command path. The integrator's SOSetTargets change validates mapped GPU address
ranges and reports an invalid command list rather than silently unbinding an
invalid VA. No CPU/GPU-idle synchronization is added.

A NULL SO-view array preserves the addressed slot count, and a size-zero view
unbinds that slot while ignoring both addresses. The declaration remains part
of the PSO: a declared but unbound target acts as a full buffer, stopping every
target of that stream. Other streams remain independent, and storage-needed
queries keep counting attempted output. A bound slot absent from the declaration
is ignored, including its counter. These are the inherited rules in
[D3D11.3 sections 14.5 and 14.6](https://microsoft.github.io/DirectX-Specs/d3d/archive/D3D11_3_FunctionalSpec.htm#14.6),
combined with [D3D12 NULL descriptors](https://microsoft.github.io/DirectX-Specs/d3d/ResourceBinding.html#null-descriptors).
NULL does not suppress the slot's shader declaration.

The engine keeps logical NULL bindings intact. At Vulkan recording it supplies
valid allocator-owned backing with an effective binding size of **zero**.
[vkCmdBindTransformFeedbackBuffersEXT](https://docs.vulkan.org/refpages/latest/refpages/source/vkCmdBindTransformFeedbackBuffersEXT.html)
permits that zero range while requiring a valid buffer and in-range offset.
The previous 16-byte capture range incorrectly admitted small-stride output
before overflowing. The zero range has no finite capture capacity. Venus
serializes the range unchanged; the stock renderer forwards it to Vulkan.
The existing counter-write-to-counter-read barrier remains in place for
render-pass pauses and resumption. Backing allocation failure invalidates the
list; active transform-feedback state is recorded only after Begin, so a failed
allocation cannot cause an unmatched End. No shader suppression, host renderer
change, or queue-idle wait implements this behavior.

[BufferFilledSize is a 32-bit quantity](https://learn.microsoft.com/en-us/windows/win32/direct3d12/stream-output-counters),
even though its GPU address is 64-bit. Target validation requires exactly four
in-range bytes and four-byte address alignment, matching Vulkan's counter
storage. A legal counter in the final four bytes of a heap is accepted; adjacent
sentinels must remain unchanged.

## Refused and unexercised behavior

- Nonzero `RasterizedStream` is refused with `E_NOTIMPL`. The live Venus device
  supports `transformFeedbackRasterizationStreamSelect`, but the engine's GS
  output-location offsets and PS signature matching do not yet select that
  stream coherently. Stream 0 and `NO_RASTERIZED_STREAM` remain implemented.
- A buffer whose declaration consists only of gaps is refused with `E_NOTIMPL`.
  Advancing its filled-size counter without a captured output requires an
  additional implementation; it must not appear to succeed without advancing.
- A partial built-in or repeated capture requiring more output components,
  locations or GS aggregate output components than the actual Vulkan limits is
  refused during shader compilation. A concrete valid example is a shader using
  its complete output budget while capturing only a component of `SV_Position`.
  Vulkan requires Position to remain a four-component built-in, so the current
  scalar capture consumes additional output interface space. User-defined
  unique partial capture no longer has this duplication problem.
- Scalar capture of non-32-bit types other than 16-bit values widened to their
  32-bit representation is refused during compilation. Legal D3D use and the
  native runtime's admission of those cases require separate validation.
- SO-owned arrays handle allocation failure, but shared shader-container
  construction and `Slot::store` still use infallible allocation. The local
  repair does not establish complete shader-creation OOM handling. Native
  allocation-failure injection remains unexercised.
- All 34 bounded native cases pass on candidate6344, including GPU readback.
  Tessellation passthrough, clip/cull, dynamic-row captures,
  nontrivial repeated captures and max-limit cases remain unexercised.

These are compiler/native contract limits, not evidence that the host GPU lacks
stream output. The fresh loaded-ICD Vulkan inventory under
`tmp/fl12-audit-20260907/fl12-vulkan-loaded-20260907-032445-083/` reports four
streams and buffers, a 512-byte per-buffer write window, 2048-byte maximum
stride, per-stream queries and rasterization stream selection. VS/DS/GS output
component limits are 128 and the GS aggregate limit is 1024. Native behavior is
a separate acceptance requirement.

## Validation and attribution

`tools/d3d12-stream-output-probe.ps1 -Mode Build` builds on local C: disk.
Build invalidates any previous successful receipt before changing its files.
Its explicit input/artifact manifest covers the C++ and HLSL, shared native
identity header, local copied PowerShell runner, executable, all three DXIL
shaders and compiler command/log. The receipt never hashes itself or sweeps in
an earlier run. Schedule and Run verify every manifest entry and the invoking
runner hash; Run keeps the build files open against writes/deletion until its
execution and evidence collection end. A failed rebuild cannot authorize a
mixture of old executable and new shaders. DXC resolves from PATH or the
installed Vulkan SDK, and its path, version and hash are recorded.

`-Mode Schedule -ExpectedUmd12SHA256 <64-hex-digits>` starts the hash-verified
local runner as an interactive scheduled task, passing the expected full UMD
hash. `-Mode Run` requires the same parameter and refuses session 0. The probe
requires the exact Helios PCI adapter, Microsoft System32 D3D12/D3D12Core/DXGI
and loaded Helios UMD12/Venus ICD identities. Exactly one logged native UMD
must match the expected full SHA256. The shared naming helper accepts only
`helios_umd12.dll` or the canonical `helios_umd12_<16-hex-digits>.dll`; a
suffixed filename must also match the DLL hash prefix. App-local runtime/engine
substitution and feature-level, shader-model and `VKD3D_SHADER_OVERRIDE`
replacement are rejected. Diagnostic runs preserve async present/retire
feedback and set and record `VKD3D_SHADER_CACHE_PATH=0`, so neither shader
replacement nor a cached pipeline bypasses the compiler under validation.

Archives contain the build inputs/artifacts and receipt, loaded-module hashes,
stdout/stderr and both per-PID UMD/engine logs. A child PASS is provisional:
missing logs, evidence/hash failures and archive failures all leave
`Completed=false` and return nonzero. Evidence copies and the final archived
result are hash-checked before the wrapper reports PASS. On an archive
filesystem failure it attempts to persist a failed result locally and at the
archive destination; failure to write that diagnostic is reported and cannot
produce a successful exit.

The runner captures its already parsed source from PowerShell's script AST.
Build verification compares that source with the exact local runner bytes
hashed against the receipt. Locking files after script entry cannot close the
earlier parse-to-entry replacement window; hashing only the current pathname
could attribute an older in-memory grader to a newer build. The guest
PowerShell 5.1 interpreter witness and synthetic provenance tests are recorded
in `FEATURE_LEVELS.md`; neither is a GPU result.

Thirty-four GPU-readback cases cover VS passthrough, packed/partial values, untouched
gaps and stride padding, two buffers, two GS streams, buffer overflow, filled
sizes, per-stream statistics, allocator/list resets, and simultaneous raster
and SO output. A missing-ALLOW_STREAM_OUTPUT PSO must return `E_INVALIDARG`
without removing the device. The original two-buffer NULL witness has room in
both targets: unbinding declared slot 0 stops both counters of the same stream
(160 and 80 bytes after the initial five vertices), while the extra draw reports
zero primitives written and one needed. Expecting the second same-stream buffer
to append was an incorrect test contract and has been removed.

Twenty-four additional cases use strides 4, 16 and 32, with initial unbound
state, a NULL array, a size-zero view with deliberately invalid ignored
addresses, and a real buffer with one vertex of capacity remaining. They cover
one and two streams, read counters before and after rebinding only slot 0, and
check both streams' written/needed queries and payload ordering. Each places
the second 32-bit counter at the end of a 4 MiB buffer-only heap and checks
neighboring sentinels. Every case marker is mandatory in the runner.
The PSO outlives execution, while its caller-owned
declaration and stride arrays leave scope immediately after creation.
After submission, Signal/event-registration/wait failures, device removal,
an invalid fence completion or an unexpected C++ exception terminate the child
without unwinding owners of pending GPU resources. A scope guard exits before
the caller's resource owners. Only the exact fence value and healthy device
authorize normal teardown; neither timeout nor process termination is a
completion witness. The wrapper also bounds child termination and output
collection rather than waiting indefinitely after a failed run. These repaired
failure paths are source-authored; native fault injection remains unexercised.

The Linux engine build uses `target/linux/vkd3d-so`. The earlier focused
`VKD3D_TEST_FILTER=stream_output` run with shader cache disabled exercises full,
partial/gapped/reordered, user-semantic scalarization and native-register-key
captures. The new NULL/range cases reproduced wrong small-stride writes before
the repair. The final full filter passed **9,246 assertions, zero failures and
zero skips**, including four-byte heap-boundary counters, per-stream overflow,
size-zero ignored addresses, and rebinding across query/copy boundaries. A
separate omitted-declaration witness preserves a nonzero ignored-slot counter
through bound, unbound and rebound states; it found no incorrect result, so no
declaration-mask compiler or counter-filtering change was made.

The indirect changeset's second review found that a public HLSL semantic could
legally spell `__HELIOS_DDI_SO_REGISTER7`. DXC assigns this fixture semantic
index7 at physical register0. A scratch ordinary-name control passed 34 host
assertions, while the exact marker failed PSO creation with E_INVALIDARG
(`so-semantic-witness/` in the audit directory). This motivated the explicit
factory origin above. The permanent marker fixture has DXIL SHA256
`ebebe021b62a76c22633473d4b8c3f1f77669c0866ae5cafb8d3812cd6fa9b22`,
compiled by DXC v1.9.2607 with `-T vs_6_0 -E main -Qstrip_reflect`.
Both public creation forms, each with cached-blob recreation and GPU capture,
now pass 36 assertions. Private physical capture passes 34, and the DXBC user
capture regression passes 33, with DGC/descriptor-buffer disabled and Vulkan
validation enabled. Logs are `indirect-round2-repaired-*.log` and
`indirect-round2-private-so-corrected.log`. The first private-test attempt failed
its symbol lookup (E_NOINTERFACE): RTLD_DEFAULT cannot see the locally loaded
engine. The corrected test resolves the module from the device's own vtable
and takes a reference with RTLD_NOLOAD. This was a test lookup failure, not an
engine PSO refusal. The private factory builds in both Windows and Linux.
The later full host stream-output filter passes 9,316 assertions with no
failures/skips or Vulkan validation errors (`indirect-round3-full-stream-output.log`).
Its native Windows execution remains unexercised.

The third indirect review found the new private factory bridge was an unguarded
`noexcept` call into a C engine that can reach throwing C++ compiler allocations.
The bridge now uses the shared `bridge_guard`, explicitly declares the C factory
`noexcept(false)` so /EHsc retains handling, and maps `std::bad_alloc` to
E_OUTOFMEMORY before logging. The Rust PSO failure branch counts that HRESULT
atomically before returning it. A Windows four-translation-unit witness
(C++ caller, C factory, implicit-C C++ wrapper, C++ throw site) terminates with
the implicit caller declaration and catches with the explicit one. Eight cases
using the actual extracted bridge function pass for success, HRESULT failure,
bad_alloc, standard/unknown exceptions and invalid arguments, including cleared
outputs and no OOM logging. Receipts are `exception-boundary-20260907-201017-101/`
and `so-guard-20260907-201838-265/` in the audit directory. Both use clang-cl
22.1.8, /EHsc, /O2 and /MD, matching the bridge compiler and exception model.
These are simulated boundary exceptions, not native allocation fault injection.
They do not establish compiler-wide cleanup or safe retry: unwinding skips
C-owned PSO/compiler cleanup, and later compiler allocation failures can leave
thread allocator state behind. Those existing construction limits remain open.

The fourth indirect review found that NULL/unbound SO backing allocation lost
the allocator's HRESULT and marked real OOM as E_INVALIDARG. The shared scratch
allocator now has an HRESULT form used by the new recording paths, with its old
boolean interface retained for unrelated callers. SO latches the actual error
before invalidation, so native Close can select its allocation-free OOM branch.
An extracted actual-helper test passes synthetic failures, first-error latching,
aligned reuse and memory-type filtering under ASan/UBSan; this does not inject
failure into a native driver or Vulkan allocation. The repaired engine again
passes all 9,316 host SO assertions with Vulkan validation
(`indirect-round4-repaired-stream_output.log`). The later native CB48 regression
is recorded below.

The fifth indirect review separately found that lazy indirect PSO compilation
could throw while holding its new source mutex; the private SO creation guard
is not on that recording path. The isolated indirect factory now catches inside
the lock/temporary-root ownership scope and returns an HRESULT before normal
release/unlock. Its actual extraction passes twelve simulated graphics/compute
cases and two cached lookups on Linux and Windows. This closes that local
containment obligation at source; it does not close the compiler-wide
allocation cleanup/safe-retry limits described above.

The sixth indirect round closes that local guard and finds a separate native
ExecuteIndirect diagnostic allocation after its void engine call. That return
now only increments an atomic counter so it can reach Close's latched-error
delivery. The sibling SO recording call has no allocating post-call diagnostics.
This is source-derived failure-path coverage, not native OOM injection or a
general sustained-OOM guarantee.

The host two-stream fixture is compiled from
`tests/shaders/pso/stream_output_two_streams.gs_6_0.hlsl` with official DXC
`v1.9.2607` (`1.9(1-0d3ee6b5)`) and `-T gs_6_0 -E main -Qstrip_reflect`.
The release archive SHA256 is
`55665c87824051ed4774ff3280a79ccbbb7d39243b9736ca5e98222134112d54`;
the emitted DXIL SHA256 is
`ebc7798a5feb3637f809e695a127597553007d16318fd92f1bc2c567104ac37b`.
Its four-vertex maximum includes the vertex emitted to the second stream.
An upstream PSO-only fixture that emits four vertices with a maximum of three
was unsuitable for this runtime test and remains unchanged.

These host tests do not enter the native Windows runtime or Helios UMD.
The expanded native C++ probe passes the MinGW C++17 syntax check with
`-Wall -Wextra -Werror` and the Windows MSVC/DXC build. Its initial native run
exposed an oracle error: SV_VertexID excludes StartVertexLocation, as specified
by [Microsoft](https://microsoft.github.io/hlsl-specs/proposals/0015-extended-command-info/).
The corrected probe adds an explicit VS-visible b0 draw tag before every draw,
retaining distinct iteration/phase payloads. Both positive and negative roots
have the same constant parameter and differ only in ALLOW_STREAM_OUTPUT.
Nonzero draw starts remain, but with no vertex buffer this does not validate
vertex-fetch offsets. Two independent reviewers closed this probe-only repair.

All **34 corrected native cases pass**, PID8440/session1, on exact UMD12
`6344CB095418C1D7B81FB77A3287A58979378DAA367DD2FEDECDA4747381217A`, system D3D12
runtime and Helios adapter/ICD. The immutable evidence is
`tmp/fl12-audit-20260907/native-so-tagged-6344cb09/run-20260907-092322-874-8c557f35/`.
All 15 evidence hashes and source inputs match; every readback follows its
submission's authenticated fence completion and healthy-device check. The driver
was not rebuilt for the oracle repair. Full SO conformance and owner visual
acceptance remain separate.
The subsequent CB48 candidate, after dry IR7/IR8 whole-change review, also
passes all 34 cases in session1, PID9156. Its evidence is
`tmp/fl12-audit-20260907/native-so-cb48d9db/run-20260907-222823-103-726931ae/`,
independently verified by `native-cb48-root-validation.json`. This run loads
exact UMD12 `CB48D9DB…`, the system runtime and unchanged Venus ICD, including
the revised private-origin and error-path source. It does not inject native
OOM or establish the remaining full SO limits.
The Meson/Ninja build also
compiled the concurrently integrated sparse/DXR changes; this is compile
coverage of those paths, not their runtime validation.

`L6StreamOutputCreates` counts accepted declarations, not PSO success or GPU
writes. `L6StreamOutputBadArg` counts rejected shader-create descriptions and
should be zero for valid workloads. `L6PsoEngineFailed` counts engine PSO
failures; the negative ALLOW_STREAM_OUTPUT case may be rejected by the runtime
before entering the UMD, so its counter movement cannot be assumed. Each
refused engine case returns a failure HRESULT and a specific engine log reason.
Command-list target/bounds failures use the existing engine invalid-list error
channel; benchmark correctness is never inferred from any of these counters.
