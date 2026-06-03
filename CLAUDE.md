# CLAUDE.md — Opus 4.6 Primary Implementor Instructions

## Project: KVM Windows Render-Only GPU Driver (Helios vGPU)

You are the **primary author** of this project. The human overseer has OS/driver/Rust expertise and will review your work, but you must drive all implementation decisions, write all code, and flag blockers proactively.

**Implementation reality (learned — authoritative over older prose in these docs):** The KMD is a **WDDM 2.0 render-only graphics** miniport (NOT MCDM/ComputeAccelerator). `wdk-sys` exposes no display DDIs, so `kmd/build.rs` custom-bindgens `dispmprt.h` + `d3dkmddi.h` into a `crate::dxgk` module and links `displib.lib` (for `DxgkInitialize`). Windows builds **cannot run on the `Z:\` share** (Rust IO fails with OS error 87 — windows-drivers-rs#481); build through the **`win` MCP server's `win_cargo`**, which mirrors `Z:\` → `C:\Users\Rupansh\helios-vgpu` and builds locally. See OVERVIEW.md "Driver Model & WDDM Targeting" and the errata banners in KMD.md / TOOLCHAIN.md.

---

## ⚠️ VERY IMPORTANT: `CARGO_TARGET_DIR`

The Linux host and the Windows VM (`win11`) **share the same source tree** (the Linux project dir is the VM's `Z:\` drive) but use different toolchains and produce incompatible artifacts. Set `CARGO_TARGET_DIR` per platform — and on Windows it MUST point at **local disk**, never the share:

- **Linux:** `CARGO_TARGET_DIR=target/linux` (native Linux fs).
- **Windows:** a **local C: path**, e.g. `CARGO_TARGET_DIR=C:\Users\Rupansh\helios-target\<crate>` — NOT `Z:\...`. Rust/cargo file IO **fails on the `Z:\` 9p/virtio share**: `OS error 87 (The parameter is incorrect)` on artifact copies, plus `could not canonicalize path Z:\`. Edit source on the share; keep the *target dir* on local C:.

Set this via the **`CARGO_TARGET_DIR` environment variable** on each cargo/`cargo make` invocation. Do **NOT** add `target-dir` to a committed `.cargo/config.toml` — that file is read on **both** platforms, so a Windows `C:` path would break Linux builds of shared crates (e.g. `protocol/`).

**Driving the VM:** prefer the **`win` MCP server** — `win_exec`, and `win_cargo` (which sets the local target dir + `LIBCLANG_PATH` for you) — over raw `ssh win`; it avoids cmd.exe quoting and stale-env issues. **coreutils are installed on `win11`**, so standard Unix tools (`ls`, `cp`, `mv`, `rm`, `cat`, …) work in `win_exec`. The source tree is at `Z:\`. See TOOLCHAIN.md §2.0.

---

## Your Role & Operating Rules

1. **Always read the relevant spec doc before writing any code** in that subsystem. The docs are the ground truth; do not guess at WDDM DDI signatures or Venus protocol fields.
2. **Never stub silently.** If a function is a stub, mark it `// STUB: reason` and log a `todo!()` or return a documented error code.
3. **Prefer explicit over clever.** Kernel code has zero tolerance for bugs. Prefer verbose, obviously correct code over clever abstractions.
4. **All unsafe blocks must have a `// SAFETY:` comment** explaining the invariant being upheld.
5. **One DDI function per commit message topic.** Keep changes scoped.
6. **Test at every milestone** using the test plan in each doc before proceeding.
7. When you encounter a WDDM DDI you haven't implemented yet, return `STATUS_NOT_IMPLEMENTED` (not a panic or crash).

---

## Repository Structure

```
helios-vgpu/
├── CLAUDE.md              ← You are here
├── OVERVIEW.md            ← Architecture (read first)
├── KMD.md                 ← Kernel-mode driver guide
├── ICD.md                 ← Vulkan ICD / UMD guide
├── TRANSPORT.md           ← VirtIO + Venus wire protocol
├── HOST.md                ← Host-side setup guide
├── TOOLCHAIN.md           ← Build environment setup
│
├── kmd/                   ← Kernel-Mode Driver (Rust, WDM)
│   ├── Cargo.toml
│   ├── build.rs
│   ├── helios_kmd.inx
│   └── src/
│       ├── lib.rs         ← DriverEntry, DDI table
│       ├── adapter.rs     ← Adapter init/teardown
│       ├── device.rs      ← Per-device context
│       ├── memory.rs      ← Segment/allocation management
│       ├── scheduler.rs   ← DMA buffer submission
│       ├── interrupt.rs   ← ISR/DPC handling
│       ├── virtio/
│       │   ├── mod.rs
│       │   ├── pci.rs     ← PCI capability scanning
│       │   ├── queue.rs   ← Virtqueue implementation
│       │   └── gpu.rs     ← virtio-gpu command structs
│       └── ddi/
│           ├── mod.rs
│           ├── add_device.rs
│           ├── start_device.rs
│           ├── query_adapter_info.rs
│           ├── create_allocation.rs
│           ├── build_paging_buffer.rs
│           ├── submit_command.rs
│           ├── patch.rs
│           └── interrupt.rs
│
├── icd/                   ← Vulkan ICD (Rust, user-mode DLL)
│   ├── Cargo.toml
│   ├── src/
│   │   ├── lib.rs         ← vk_icdGetInstanceProcAddr entry
│   │   ├── instance.rs
│   │   ├── device.rs
│   │   ├── venus/
│   │   │   ├── mod.rs
│   │   │   ├── encode.rs  ← Venus command serialization
│   │   │   ├── decode.rs  ← Response deserialization
│   │   │   └── ring.rs    ← Shared-memory command ring
│   │   └── transport.rs   ← KMD escape calls (D3DKMTEscape)
│   └── helios_vulkan.json ← ICD manifest
│
└── host/                  ← Host-side Rust daemon
    ├── Cargo.toml
    └── src/
        ├── main.rs
        ├── virgl.rs       ← virglrenderer bindings
        └── server.rs      ← Venus context management
```

---

## Implementation Order (Follow This Exactly)

### Phase 0: Toolchain (TOOLCHAIN.md)
- [ ] Set up Windows 11 dev VM with WDK 22H2, VS 2022, LLVM 17
- [ ] Verify `cargo wdk build` works on the empty KMD skeleton
- [ ] Set up Linux host with QEMU 9.2+, virglrenderer (Venus-enabled)

### Phase 1: KMD Skeleton — Adapter Enumeration
Goal: Driver loads, finds the virtio-gpu PCI device, kernel does not crash.

- [ ] `DriverEntry` → fill `DRIVER_INITIALIZATION_DATA`, call `DxgkInitialize`
- [ ] `DxgkDdiAddDevice` → allocate adapter context, return opaque handle
- [ ] `DxgkDdiStartDevice` → save `DXGKRNL_INTERFACE`, enumerate PCI BARs, map MMIO
- [ ] `DxgkDdiQueryAdapterInfo` → return `DXGK_DRIVERCAPS` with `RenderSupported=1`, zero display outputs
- [ ] `DxgkDdiQueryChildRelations` → return empty (render-only, no children)
- [ ] `DxgkDdiStopDevice` / `DxgkDdiRemoveDevice` → cleanup

**Test:** Device shows in Device Manager as "Helios vGPU Render Adapter". No BSOD.

### Phase 2: VirtIO PCI + Virtqueue
Goal: Can send `VIRTIO_GPU_CMD_GET_DISPLAY_INFO` and get a response.

- [ ] Scan PCI capability list for VirtIO capability structures (type 4 = common cfg, type 5 = notify, type 2 = ISR, type 1 = device cfg)
- [ ] Map all VirtIO BARs
- [ ] Initialize virtqueues (control queue = 0, cursor queue = 1)
- [ ] Implement virtqueue descriptor ring, available ring, used ring
- [ ] Implement `virtio_gpu_ctrl_hdr` and `VIRTIO_GPU_CMD_GET_DISPLAY_INFO`
- [ ] Wire up MSI-X interrupt → `DxgkCbNotifyInterrupt` → `DxgkCbQueueDpc`

**Test:** Send GET_DISPLAY_INFO from a test IOCTL, receive valid response in DebugView.

### Phase 3: WDDM Memory Management
Goal: `CreateAllocation` / `DestroyAllocation` work, segments are set up.

- [ ] `DxgkDdiQueryAdapterInfo(DXGKQAITYPE_QUERYSEGMENT4)` — report one CPU-visible aperture segment backed by host memory (hostmem blob region)
- [ ] `DxgkDdiCreateAllocation` — allocate virtio-gpu resource IDs
- [ ] `DxgkDdiDestroyAllocation`
- [ ] `DxgkDdiBuildPagingBuffer` — implement `DXGK_BUILDPAGINGBUFFER_OPERATION_TRANSFER` (map/unmap backing pages)
- [ ] `DxgkDdiSubmitCommandVirtual` — queue DMA buffers (for WDDM 2.0 GPU VA model)

**Test:** D3D12 device creation succeeds (via DXVK ICD stub).

### Phase 4: Venus Context + Command Submission
Goal: First real Vulkan command reaches virglrenderer on the host.

- [ ] Create Venus context via `VIRTIO_GPU_CMD_CTX_CREATE` with `VIRTIO_GPU_CAPSET_VENUS`
- [ ] Implement command ring buffer (see TRANSPORT.md §4)
- [ ] Implement `VN_CS_ENCODER` — Venus command serialization (start with `vkCreateInstance`)
- [ ] `DxgkDdiEscape` handler for ICD → KMD: submit Venus command buffer, poll fence

**Test:** `vkCreateInstance` from a test EXE reaches the host and virglrenderer logs it.

### Phase 5: Vulkan ICD (icd/)
Goal: `vulkaninfo` reports the Helios device.

- [ ] Implement Vulkan loader entrypoints (`vk_icdGetInstanceProcAddr`, negotiation)
- [ ] Implement instance/device creation dispatching to Venus encoder
- [ ] Implement `vkEnumeratePhysicalDevices` → forward to host, deserialize capabilities
- [ ] Implement `vkCreateDevice`, queues, command pools

**Test:** `vulkaninfo` shows one physical device. `vkcube` renders.

### Phase 6: DXVK + VKD3D Integration
Goal: D3D11/D3D12 apps render via DXVK → Helios ICD → Venus → host GPU.

- [ ] Install DXVK and configure it to use the Helios Vulkan ICD
- [ ] Run a D3D11 triangle app

**Test:** d3d11-triangle or dxvk-tests pass.

---

## Key Invariants to Never Violate

| Rule | Why |
|------|-----|
| Never call pageable code at IRQL > APC_LEVEL | BSOD `IRQL_NOT_LESS_OR_EQUAL` |
| Never allocate in ISR (DxgkDdiInterruptRoutine) | ISR runs at DIRQL |
| Always validate command buffer bounds in KMD | Security — guest can send malicious offsets |
| Virtqueue descriptors must not be freed while in-flight | Corruption |
| Venus commands must be flushed before signaling fence | Ordering |
| All WDDM 2.0+ drivers must use GPU VA, not physical addressing | Required by WDDM 2.0 |

---

## When You're Stuck

1. Check `KMD.md` — there's a troubleshooting section per phase.
2. Check the existing C reference: [kvm-guest-drivers-windows viogpu](https://github.com/virtio-win/kvm-guest-drivers-windows/tree/master/viogpu) — this is a display-only driver, but its virtio init code (`viogpu_queue.cpp`) is directly relevant.
3. For WDDM DDI signatures: always use the WDK headers directly, not memory. The canonical reference is `d3dkmddi.h`, `dispmprt.h` accessed via `wdk-sys`.
4. For Venus protocol: `venus-protocol/vk.xml` and `virglrenderer/src/venus/` are the ground truth.
5. Ask the overseer if you hit a fundamental architecture issue.

---

## Files Not to Touch

- `*.inx` files after initial creation — only modify with explicit instruction

---

## Code Style

```rust
// All kernel-mode code: no_std, no panics in release
// Use wdk-sys types directly for DDI structs
// Pattern for DDI implementations:
pub unsafe extern "C" fn dxgkddi_foo(
    miniport_context: *mut c_void,
    args: *mut DXGKARG_FOO,
) -> NTSTATUS {
    // SAFETY: Dxgkrnl guarantees miniport_context is our adapter context
    //         and args is a valid kernel-mode pointer.
    let adapter = &mut *(miniport_context as *mut AdapterContext);
    let args = &mut *args;
    
    match adapter.foo(args) {
        Ok(()) => STATUS_SUCCESS,
        Err(e) => e.into(),
    }
}
```
