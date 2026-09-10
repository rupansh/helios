# Native DGC compute statistics

The 2026-09-10 implementation repairs D3D12 pipeline statistics around native
`VK_EXT_device_generated_commands` compute execution. It does not restore the
removed indirect-command emulation. Ordinary, unqueried DGC still uses the same
native execution, preprocessing and batching paths.

## Contract and implementation

Microsoft defines `CSInvocations` as compute shader invocations. Pipeline
statistics scopes are supported on DIRECT lists, including their compute work;
COMPUTE lists do not support this query type. See the
[query data contract](https://learn.microsoft.com/en-us/windows/win32/api/d3d12/ns-d3d12-d3d12_query_data_pipeline_statistics)
and [command-list restrictions](https://learn.microsoft.com/en-us/windows/win32/direct3d12/queries).

Direct NVIDIA Vulkan and paired Venus both omit native DGC dispatches from the
physical pipeline query. The native Windows reproduction on UMD12 `5B8A411E…`
reports 4 instead of 6 invocations, then 3 instead of 4 on closed-list replay.
The larger DXBC/DXIL cases each have 32 failing statistics checks while their
shader atomic outputs are correct. This is a statistics defect, independently
of command execution or native feature-level admission.

The engine suspends hardware statistics around DGC compute on every driver,
avoiding double counts on implementations that count it correctly. An internal
GPU reduction reads the exact generated stream, count address and normalized
predicate used by native DGC. It multiplies each dispatch's XYZ group counts by
the compiled shader's numthreads product. Zero dimensions, suppressed work and
zero count contribute zero. Count values clamp to MaxCommandCount. Padded
argument layouts use the final DGC dispatch-token offset and stride.

At most 64 workgroups of 64 lanes produce partial statistics. Full-width byte
addressing and two-word unsigned arithmetic preserve 64-bit products/sums
without requiring shaderInt64 or 64-bit atomics. Scratch holds all eleven
statistics fields; every execution overwrites every field, including zero
cases. The existing virtual-query gather adds these buffer fragments alongside
Vulkan query-pool fragments. Overlapping scopes retain their own contributions
when an inner query is resolved. Query-index reuse and immutable closed-list
replay retain their existing rules.

The pass runs after native DGC, with physical compute queries stopped. Internal
compute is excluded from application statistics and from conditional rendering;
it explicitly reads the normalized predicate. GPU barriers publish patched
inputs, finish their reads before later writes, and publish scratch for gather.
Allocator-owned scratch stays live through existing authenticated completion.
There is no CPU argument readback, submission split or GPU-idle wait. Allocation
or missing shader-dimension failures invalidate recording; they do not return a
successful zero statistic. This does not repair the separate pending-allocator
Reset/fence-worker lifetime question or the host-loss callback boundary.

## Validation and provenance

Evidence: `tmp/fl12-query-contract-20260910/`. Root remains master at
`dcdb8b38a0556d554b005e90132ff0979fcc28ba`; engine base remains
`71ebda7ec9eee8f826a65844710f3ab8fa1c1dd7`, with uncommitted work. The source
manifest verifies 149 relevant mirrored Windows inputs. The frozen build receipt
contains the release UMD and seven static archives; the native executable's
receipt separately captures its actual compiler inputs. No work was pushed.

Release UMD12 SHA256:
`22C31F11911434E0A53114CCF04B3970DB4A0342422EB4892DAB26F41D00C017`.
It is hotplugged at
`C:\ProgramData\HeliosUmd\helios_umd12_22c31f11911434e0.dll` on unchanged
KMD22.22.271.0/oem54/Code0/WDDM2.1. UMD11 remains `57C84ED4…`, Mesa ICD
`43394BBD…`; the signed DriverStore UMD12 remains the older `41A7…` package.
Native runs record system D3D12/Core10.0.26100.9278 and DXGI10.0.26100.9444,
exact loaded UMD/ICD hashes, Helios PCI identity and an interactive session.
No WARP, app-local runtime or feature-level override is admitted.

- Direct NVIDIA, paired Venus and Intel each pass 2,166 checks across the two
  continuation and two expanded compute cases. Intel is the independent
  no-double-count control. Native Vulkan DGC invocation counters themselves
  remain unchanged and the old direct Vulkan probe still has its known failure.
- Expanded cases independently compare query results and guarded shader atomic
  output, with 24-thread workgroups, 4,101 commands, padded and packed streams,
  GPU-produced count/predicate data, count clamping, both predicate operations,
  high-only 64-bit predicates, zero dimensions and closed replay.
- The expanded NVIDIA DXBC case passes 933 checks with Vulkan core and
  synchronization validation: no VUID errors or synchronization hazards.
- Native Windows `query-after-20260910-010522-750` passes 2,218 checks across
  all four groups, including 56 expanded GPU-readback records. Those expanded
  cases check the native InfoQueue after authenticated completion; the older
  continuation fixtures enable the layer but do not grade its message queue.
- Linux/Windows engine and release UMD builds pass. A1 passes, including 211 KMD
  logic tests. No new KMD, ICD or renderer changes were needed for this repair.

The same deployed UMD also passes 23 inherited feature groups / 529,085 checks,
13 raster groups / 778,722 checks / 420 TIR readback records, and all four native
65,536-word ordering cases. The tiled suite has 18 PASS cases and one expected
tier-2 reserved-3D refusal; its aggregate exit77 is preserved as BLOCKED.
Native capability inventory `native-20260910-012531-831-a39d1f44ad024733b6d9ce875f778bad`
creates FL11_0/11_1/12_0/12_1 and refuses FL12_2, with tiled2/binding3/ROV1/
conservative3/SM6.0/RT0. These are bounded behavior and reporting checks, not
complete feature-level certification.

Minimum/maximum filtering now has actual pixel checks: R32_FLOAT bilinear and
mip reduction, exact texel centers with zero-weight neighbors, static and
descriptor samplers, DXBC and DXIL. See Microsoft's
[reduction-filter contract](https://learn.microsoft.com/en-us/windows/win32/direct3d11/tiled-resources-texture-sampling-features).
Native `filter-20260910-061417-607` passes 259 checks / 60 pixel readbacks with
the debug InfoQueue graded; NVIDIA, Intel and paired Venus each pass 249 checks.
The final test executable is `5D32003113D6C08C9A7DAF30CE676645E20D95E26DB5A5AD65ADC10EEABB222B`,
with 212 compiler inputs captured separately. No production change was needed
for filtering. The original filter fixture mixed a default DXBC vertex shader
with a DXIL pixel shader; native PSO creation correctly refused it. That failed
run is preserved and excluded. The corrected fixture uses a matching shader
model for both stages. Earlier Linux filter runs used the mixed fixture and
are superseded by the final same-model runs.

The first expanded fixture incorrectly used a COMPUTE list; its failed Linux
runs are preserved and excluded from acceptance. The first name-prefix test
selected no actual case and is also excluded. The corrected tests use exact
names and DIRECT lists. Initial compilation missed the generated shader include;
the subsequent completed builds include it.

Very large (>4 GiB) argument streams and >2^32 invocation totals have arithmetic
implementation coverage but no completed end-to-end workload in this receipt.
Native allocation-failure injection and host loss remain unexercised here.
Full FL12_1 conformance, the authorized sparse compatibility exception and native
DXR/Port Royal acceptance remain separate from this query repair. The old
TotalLaneCount1024 estimate remains an explicit reporting gap in FEATURE_LEVELS.md.

## Completed graphical controls

The same .271/22C31F11/57C84ED4/43394BBD stack completes all stock workloads,
run sequentially through interactive scheduled tasks. `controls/` under the
evidence directory contains the original `.3dmark-result` archives, XML exports,
copied definitions, CLI arguments, logs, module observations and independent
verification. Windows archive hashes were independently checked on Linux.
The settings within each archive match its preceding2AD1 stock control after
excluding adapter LUID and result UUID fields. No performance change is
attributed to this patch or to any intervening build.

| Control | Completed workloads | Graphics score | Measured FPS |
|---|---|---:|---|
| Time Spy | Demo, GT1, GT2, CPU | 23,071 | GT1 154.94; GT2 128.93; CPU 55.99 |
| Fire Strike | Demo, GT1, GT2, physics, combined | 59,806 | GT1 265.00; GT2 255.24; physics 129.95; combined 37.76 |
| Steel Nomad Vulkan | SteelNomadGt1VK | 9,384 | 93.84 |

Overall scores are Time Spy21,813 and Fire Strike35,060. These are single-run
measurements, not an optimization comparison. The installed CLI identifies
itself as2.32.8454, SystemInfo5.92.1497.0; actual x64 workload versions are
TimeSpy1.2.6.5, FireStrike1.1.0.0 and SteelNomad1.0.5.1. The separately inventoried
PortRoyal1.0.0.0 and SpeedWay1.1.1.2 have not run on this candidate.

All four Time Spy processes loaded the exact new UMD12 and system D3D12/Core;
all five Fire Strike processes loaded the expected DX11 UMD. Steel Nomad uses
the Vulkan workload and exact ICD; it loads system d3d12.dll but no D3D12Core
or UMD12. No workload uses WARP or an app-local D3D12/vkd3d replacement.
`frame-inspection.json` records viewed host-VNC scene pairs: Time Spy GT1
frame2932→7725, Fire Strike GT2 frame3941→6013 and Steel Nomad frame37→959.
No paintcap/focus-taking observer was used. These establish visible rendering
and motion; owner visual correctness acceptance remains pending. The .266
accepted shadows/~100FPS and the instrumented74.26FPS remain separate evidence.

The final guest receipt records unchanged boot, KMD22.22.271.0/oem54/Code0,
UmdD3D12=1, loaded DWM UMD11/ICD identities, LLVM22.1.8 and VulkanSDK1.4.350.0,
with no surviving benchmark/probe process. No guest reboot, owner QEMU restart,
commit or push was needed for this query repair. Final dirty-source snapshots
are separate from the frozen release and test build receipts.
