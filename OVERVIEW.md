# OVERVIEW.md — Helios vGPU Architecture

## Project Goal

Build a **render-only WDDM 2.x graphics driver** for a Windows 11 VM running under KVM/QEMU that:

- Exposes a Vulkan-capable GPU adapter to the Windows graphics stack
- Intercepts DirectX calls at the DXVK/VKD3D layer (DX → Vulkan translation happens in the guest)
- Serializes Vulkan commands over the **virtio-gpu Venus protocol** to virglrenderer on the Linux host
- Executes on the host's physical GPU via the host Vulkan driver (RADV, ANV, etc.)
- Does NOT do display output (render-only adapter, like NVIDIA Optimus render-side)
- Does NOT use GPU passthrough — host retains full GPU access

---

## Driver Model & WDDM Targeting

Helios is a **render-only WDDM 2.0 display miniport** — a graphics-capable adapter that produces no display output (the "Optimus render-side" model). It registers via `DxgkInitialize` + `DRIVER_INITIALIZATION_DATA`, and is made render-only two ways: every VidPN/display DDI is left **NULL** (not stubbed), and `DxgkDdiStartDevice` / `QueryChildRelations` report **0** video-present sources and **0** children.

**Why not MCDM?** Microsoft's "render device without display" model is MCDM (`Class=ComputeAccelerator`, WDDM 2.6+, `MiscCaps.ComputeOnly`). We deliberately do **not** use it: MCDM is the *compute* driver model and risks not exposing the **3D-graphics** pipeline that is Helios's entire purpose (DXVK/VKD3D → Vulkan rendering). A WDDM 2.0 render-only graphics miniport keeps the graphics pipeline.

**WDDM version:** floor is **WDDM 2.0** (Win10 1507+, covers Win11 guests). 2.0 already provides everything the Venus host-replay path needs — **GPU virtual addressing (GpuMmu)** and **monitored fences**. (Both are WDDM 2.0 features, *not* 2.6; the WDDM 3.1+ native GPU fence object is out of scope.)

**The two version fields (easy to get wrong):**
- `DRIVER_INITIALIZATION_DATA.Version` → set to the WDK symbol `DXGKDDI_INTERFACE_VERSION` (the header's build-against alias), or `DXGKDDI_INTERFACE_VERSION_WDDM2_0` (= `0x5023`; **not** `0x7002`). Never hardcode a guessed hex.
- `DXGK_DRIVERCAPS.WDDMVersion` → leave **0** (reserved for any driver `>= WIN7`). The OS infers the level from `Version` + the registered DDIs, not this field.

**GPU-VA opt-in (mandatory for the GPU-VA path):** the `DXGKQAITYPE_DRIVERCAPS` handler must fill `DXGK_DRIVERCAPS.MemoryManagementCaps` (`DXGK_VIDMMCAPS`) with `VirtualAddressingSupported = 1` and `GpuMmuSupported = 1` (never both GpuMmu and IoMmu). Without it the `SubmitCommandVirtual` / `SetRootPageTable` path is never enabled.

### Helios phases ↔ Microsoft WDDM roadmap

| Helios phase | Microsoft roadmap / DDI flow |
|---|---|
| 0 Toolchain | Roadmap steps 1, 3, 4 (+ signing prereqs, step 8) |
| 1 Adapter enumeration | "Initializing Display Miniport drivers": DriverEntry → AddDevice → StartDevice → QueryAdapterInfo → QueryChildRelations |
| 2 VirtIO PCI + virtqueue | Vendor HW bring-up inside StartDevice (opaque to WDDM; no dedicated MS step) |
| 3 WDDM memory | CreateAllocation → BuildPagingBuffer; GpuMmu segments via `DXGKQAITYPE_QUERYSEGMENT4` |
| 4 Venus context + submit | GPU-VA path: `SubmitCommandVirtual` (no `Patch`); completion via InterruptRoutine → DxgkCbNotifyInterrupt → DpcRoutine |
| 5 Vulkan ICD | The paired user-mode driver (substitutes for the classic D3D UMD) |
| 6 DXVK / VKD3D | App-level validation (roadmap test/debug + WHLK) |

**STATUS (2026-06-04):** Phases 1–1.5 ✅ (driver loads; Code 37 cleared) and **Phase 2 ✅** (virtio-gpu bring-up via the `virtio-drivers` crate — `GET_DISPLAY_INFO` round-trips on real HW; the hand-rolled virtqueue in KMD.md is **superseded**). The device currently sits at **Code 43** (post-start GPU-VA gate). Phase 3 = clear Code 43 + the **`DxgkDdiEscape`** Venus spine; Phases 3 & 4 are being landed together as one KMD push, because the escape protocol (the real ICD↔KMD contract, in `helios_protocol::escape`) is independent of GPU VA and is the cheapest route to first host traffic. First Venus bring-up uses an interim `fence_id`→KEVENT model rather than the `DxgkCbNotifyInterrupt` monitored fence listed above. See the `phase3-kickoff` memory.

**DDIs still to register for a *loadable, render-capable* adapter (stubs OK in Phase 1, but the slots must be non-NULL so device creation succeeds):** `Render`/`RenderKm`, `Patch`, `OpenAllocation`/`CloseAllocation`, `DescribeAllocation`, `GetStandardAllocationDriverData`, `GetNodeMetadata`, `SetRootPageTable`/`GetRootPageTableSize` (GpuMmu), `CollectDbgInfo`, `ControlInterrupt`, `QueryCurrentFence`. CPU/GPU sync uses the WDDM 2.0 **monitored fence** (`DxgkDdiSignalMonitoredFence` / `DxgkCbSignalMonitoredFence`), not the WDDM 3.1+ native fence.

---

## System Diagram

```
┌─────────────────────────────────────────────────────────────────────┐
│  Windows 11 Guest (KVM VM)                                          │
│                                                                     │
│  ┌────────────┐  ┌──────────┐  ┌──────────────────────────────┐   │
│  │  D3D11 app │  │ D3D12 app│  │  Vulkan app                  │   │
│  └─────┬──────┘  └────┬─────┘  └──────────────┬───────────────┘   │
│        │              │                         │                   │
│  ┌─────▼──────────────▼─────┐                  │                   │
│  │   DXVK (d3d11.dll)       │                  │                   │
│  │   VKD3D-Proton (d3d12.dll│                  │                   │
│  └─────────────┬────────────┘                  │                   │
│                │ Vulkan calls                   │ Vulkan calls      │
│  ┌─────────────▼────────────────────────────────▼───────────────┐  │
│  │           Helios Vulkan ICD (helios_icd.dll)                  │  │
│  │                  Venus command encoder                        │  │
│  └────────────────────────────┬──────────────────────────────────┘  │
│                               │ D3DKMTEscape / DMA buffers          │
│  ┌────────────────────────────▼──────────────────────────────────┐  │
│  │        Helios KMD (helios_kmd.sys) — WDDM 2.x render-only    │  │
│  │        Kernel-Mode Display Miniport Driver                    │  │
│  └────────────────────────────┬──────────────────────────────────┘  │
│                               │ VirtIO PCI (virtqueues)             │
│                               │ VEN_1AF4 DEV_1050                   │
└───────────────────────────────┼─────────────────────────────────────┘
                                │ VirtIO transport (shared memory ring)
┌───────────────────────────────┼─────────────────────────────────────┐
│  Linux Host (KVM)             │                                     │
│                               ▼                                     │
│  ┌─────────────────────────────────────────────────────────────┐   │
│  │  QEMU 9.2+ virtio-gpu-gl device model                       │   │
│  │  (blob=on, hostmem=8G, venus=on)                            │   │
│  └──────────────────────────┬──────────────────────────────────┘   │
│                             │ virgl_renderer_* API                  │
│  ┌──────────────────────────▼──────────────────────────────────┐   │
│  │  virglrenderer (Venus context type)                         │   │
│  │  Venus command decoder + Vulkan replay                      │   │
│  └──────────────────────────┬──────────────────────────────────┘   │
│                             │ Vulkan API                            │
│  ┌──────────────────────────▼──────────────────────────────────┐   │
│  │  Host GPU Driver (RADV/ANV/NVIDIA)                          │   │
│  └─────────────────────────────────────────────────────────────┘   │
│  Physical GPU (AMD/Intel/NVIDIA) — also used by host               │
└─────────────────────────────────────────────────────────────────────┘
```

---

## Component Breakdown

### 1. Helios KMD (`kmd/`) — Kernel-Mode Driver

**Language:** Rust (no_std) using `windows-drivers-rs` / `wdk-sys`  
**Driver model:** WDM (direct DDI table registration via `DxgkInitialize`)  
**WDDM version:** 2.0 (floor). GPU VA (GpuMmu) and monitored fences are WDDM **2.0** features — no 2.6 bump needed. See "Driver Model & WDDM Targeting" above.  

The KMD is the only piece that runs in the Windows kernel. It:
- Presents a PCI device to the OS (virtio-gpu at VEN_1AF4&DEV_1050)
- Registers as a WDDM render-only adapter (zero video present sources/targets)
- Manages WDDM memory: segments, allocations, paging buffers
- Submits GPU command buffers (containing encoded Venus commands) through virtqueues
- Handles fences/synchronization between CPU and the host-side Venus renderer

**Key DDIs implemented (render path only):**

| DDI | Purpose |
|-----|---------|
| `DxgkDdiAddDevice` | Allocate adapter context |
| `DxgkDdiStartDevice` | Map PCI BARs, init virtqueues |
| `DxgkDdiQueryAdapterInfo` | Report caps, segments (render-only) |
| `DxgkDdiQueryChildRelations` | Return empty — no display outputs |
| `DxgkDdiCreateDevice` | Per-D3D-device state |
| `DxgkDdiCreateAllocation` | Allocate GPU resources |
| `DxgkDdiDestroyAllocation` | Free GPU resources |
| `DxgkDdiBuildPagingBuffer` | Back allocations with guest memory |
| `DxgkDdiSubmitCommandVirtual` | Queue Venus DMA buffers |
| `DxgkDdiInterruptRoutine` | Process fence completions |
| `DxgkDdiDpcRoutine` | Notify Dxgkrnl of completions |
| `DxgkDdiEscape` | ICD→KMD out-of-band commands |
| `DxgkDdiCreateContext` | GPU execution context |

**DDIs returning NULL/NOT_SUPPORTED (display path):**

All VidPn DDIs: `DxgkDdiIsSupportedVidPn`, `DxgkDdiRecommendFunctionalVidPn`, `DxgkDdiEnumVidPnCofuncModality`, `DxgkDdiSetVidPnSourceAddress`, `DxgkDdiUpdateActiveVidPnPresentPath`, etc.

### 2. Helios ICD (`icd/`) — Vulkan Installable Client Driver

**Language:** Rust (std available, this is a user-mode DLL)  
**Output:** `helios_icd.dll` + `helios_vulkan.json` (Vulkan loader manifest)  

The ICD sits between the Vulkan loader and the KMD. It:
- Exposes `vk_icdGetInstanceProcAddr` (Vulkan ICD entry point)
- Encodes Vulkan API calls into Venus binary protocol
- Submits Venus command buffers to the KMD via `D3DKMTEscape` + shared memory
- Deserializes Venus response/event stream for return values and completions

DXVK and VKD3D-Proton are not modified — they see a standard Vulkan ICD.

### 3. Host daemon (`host/`) — optional helper

For Venus, QEMU + virglrenderer handle everything natively. The host/ crate is for:
- Diagnostics (dumping Venus streams)
- Configuration helpers
- Future: custom transport bypassing QEMU's virgl path for lower latency

---

## Venus Protocol (the critical path)

Venus is a **Vulkan command serialization protocol** used to send Vulkan calls over the virtio-gpu transport. The guest (ICD) encodes API calls; the host (virglrenderer) decodes and replays them against the physical GPU.

```
Guest ICD                            Host virglrenderer
─────────────────                    ──────────────────
vkCreateBuffer(...)                  
  → vn_encode_vkCreateBuffer(...)    
  → VIRTIO_GPU_CMD_SUBMIT_3D         
    [cmd_buf = Venus bytes]    ──►   vn_decode_vkCreateBuffer(...)
                                       → vkCreateBuffer() on host GPU
                               ◄──   VkResult + handle returned via
                                       VIRTIO_GPU_RESP_OK_NODATA or
                                       response ring
```

**Spec / codegen source:** https://gitlab.freedesktop.org/virgl/venus-protocol  
**Host implementation:** https://gitlab.freedesktop.org/virgl/virglrenderer (src/venus/)  
**Reference guest (Linux):** https://gitlab.freedesktop.org/mesa/mesa (src/virtio/vulkan/)

The Venus wire format is auto-generated from `vk.xml`. You must use the same codegen to ensure compatibility with virglrenderer. See `TRANSPORT.md` for the full wire format.

---

## What Is NOT a Passthrough

This design specifically avoids GPU passthrough (VFIO/IOMMU). Here's the comparison:

| Property | Passthrough | Helios vGPU |
|----------|-------------|-------------|
| Host GPU access | ❌ Host loses GPU | ✅ Host keeps GPU |
| Multiple VMs | ❌ One VM only | ✅ Multiple VMs possible |
| Performance | ~95% native | ~40–60% native (target: 50%) |
| Driver complexity | Low (host driver does all work) | High (Venus stack) |
| Guest Vulkan | ✅ Native | ✅ Via Venus |
| Guest DirectX | ✅ Native | ✅ Via DXVK+Venus |

---

## Memory Model (WDDM 2.0 GPU Virtual Addressing)

WDDM 2.0 requires GPU virtual addressing (no patch-location-list approach). The memory model:

```
┌──────────────────────────────────────────┐
│  Guest physical memory (Windows RAM)     │
│  ┌───────────────────────────────────┐   │
│  │  Blob resource backing store      │   │  ← virtiogpu hostmem= region
│  │  (DMA-buf exported to KVM)        │   │     mapped into guest GPA
│  └───────────────────────────────────┘   │
│  ┌───────────────────────────────────┐   │
│  │  Venus command ring buffer        │   │  ← Shared memory for commands
│  └───────────────────────────────────┘   │
└──────────────────────────────────────────┘
```

- **Segment 0:** CPU-accessible aperture segment — backed by the virtio-gpu `hostmem=` blob region. Size: 512 MB initially.
- **Allocations:** Each Vulkan VkBuffer/VkImage = one virtio-gpu resource ID + backing pages
- **Command buffers:** Venus-encoded commands submitted via `VIRTIO_GPU_CMD_SUBMIT_3D` with the resource-backed command buffer

---

## Performance Architecture

To hit the 40–60% native GPU performance target:

1. **Zero-copy memory:** Use blob resources + `hostmem=8G` so the guest directly writes into host-visible memory without a memcpy through QEMU.
2. **Batching:** Accumulate Venus commands in a per-context ring buffer; flush on `vkQueueSubmit` boundary, not per-command.
3. **Async fence polling:** Use `VIRTIO_GPU_FLAG_FENCE` + monitored fence object (`DxgkCbSignalMonitoredFence`) to avoid spinning.
4. **Descriptor caching:** Venus supports pipeline/descriptor set handle caching; use it to avoid re-encoding static descriptors.
5. **NO QEMU FPS CAP:** QEMU limits virtio-gpu fence polling to 100fps by default. This only affects scanout, not 3D submit. Venus compute/render is not affected.

---

## Security Boundary

The KMD runs in kernel-mode and accepts command buffers from the ICD (user-mode). Since we are targeting a VM scenario (not production signing), the threat model is:
- **In scope:** Crashing the guest kernel must not affect the host
- **In scope:** Invalid Venus commands must not crash virglrenderer
- **Out of scope:** Malicious guest kernel exploiting the host (KVM's responsibility)

The KMD must validate:
- Command buffer bounds (pointer + length within the mapped segment)
- Resource IDs are valid before use
- Fence IDs are within range

---

## Key References

| Topic | URL |
|-------|-----|
| WDDM 2.0 features | https://learn.microsoft.com/en-us/windows-hardware/drivers/display/wddm-2-0-and-windows-10 |
| Render vs. display driver types | https://learn.microsoft.com/en-us/windows-hardware/drivers/display/render-only-and-display-only-devices |
| MCDM (compute model — NOT used, for reference) | https://learn.microsoft.com/en-us/windows-hardware/drivers/display/mcdm-implementation-guidelines |
| DXGK_DRIVERCAPS / DXGK_VIDMMCAPS | https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmddi/ns-d3dkmddi-_dxgk_drivercaps |
| DRIVER_INITIALIZATION_DATA | https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/dispmprt/ns-dispmprt-_driver_initialization_data |
| DxgkDdiQueryAdapterInfo | https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmddi/nc-d3dkmddi-dxgkddi_queryadapterinfo |
| DXGK_DRIVERCAPS | https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmddi/ns-d3dkmddi-_dxgk_drivercaps |
| GPU VA model (WDDM 2.0) | https://learn.microsoft.com/en-us/windows-hardware/drivers/display/gpu-virtual-memory-in-wddm-2-0 |
| Monitored fence | https://learn.microsoft.com/en-us/windows-hardware/drivers/display/monitored-fences |
| windows-drivers-rs | https://github.com/microsoft/windows-drivers-rs |
| Venus protocol repo | https://gitlab.freedesktop.org/virgl/venus-protocol |
| virglrenderer Venus src | https://gitlab.freedesktop.org/virgl/virglrenderer/-/tree/master/src/venus |
| Mesa Venus driver (Linux ref) | https://gitlab.freedesktop.org/mesa/mesa/-/tree/main/src/virtio/vulkan |
| virtio-win kvm drivers | https://github.com/virtio-win/kvm-guest-drivers-windows/tree/master/viogpu |
| Prior art: mvisor Win vGPU (virgl/GL, WDF) — transport reference | https://github.com/tenclass/mvisor-win-vgpu-driver |
| Prior art: virtio-gpu-win-icd | https://github.com/Keenuts/virtio-gpu-win-icd |
| Prior art: qemu-3dfx (ship-your-own-ICD model) | https://github.com/kjliew/qemu-3dfx |
| VirtIO spec 1.2 GPU section | https://docs.oasis-open.org/virtio/virtio/v1.2/virtio-v1.2.html#sec-gpu |
| QEMU virtio-gpu-gl | https://www.qemu.org/docs/master/system/devices/virtio-gpu.html |
