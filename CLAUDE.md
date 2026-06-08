# CLAUDE.md — Opus 4.6 Primary Implementor Instructions

## Project: System-class KMDF virtio-gpu / Venus passthrough driver (Helios vGPU)

You are the **primary author** of this project. The human overseer has OS/driver/Rust expertise and will review your work, but you must drive all implementation decisions, write all code, and flag blockers proactively.

**Implementation reality (authoritative):** The KMD is a **System-class KMDF function driver** for the virtio-gpu PCI device (PCI\VEN_1AF4&DEV_1050), Setup class **System {4d36e97d-e325-11ce-bfc1-08002be10318}** — NOT a display/WDDM miniport. User mode reaches the KMD via **`DeviceIoControl` on a device interface** (`GUID_DEVINTERFACE_HELIOS`, discovered with SetupDiGetClassDevs + CreateFile). `kmd/build.rs` should use the KMDF/WDF path. The DOD-scoped `dispmprt.h`/`d3dkmddi.h` bindgen and `kmd/src/dxgk.rs` may remain in the repository as archived reference material, but they are not part of the active System-class build.

Windows builds **cannot run on the `Z:\` share** (Rust IO fails with OS error 87 — windows-drivers-rs#481); build through the **`win` MCP server's `win_cargo`**, which mirrors `Z:\` → `C:\Users\Rupansh\helios-vgpu` and builds locally. See `ARCH.md` (canonical) and `SYSTEM_CLASS_REFOCUS_2026_06_07.md`.

**Status (updated 2026-06-08):** The Venus stack renders end-to-end on the System-class KMDF + DeviceIoControl + Mesa Venus ICD path (`vulkaninfo`, device creation, mapped Venus memory, `vkCmdFillBuffer` readback, `vkcube`). The WDDM Display-Only Driver pivot is **archived**. Active display work is the Looking Glass IDD Vulkan sink experiment: the LG IDD driver can optionally mirror completed KVMFR BGRA frames into a persistent Helios Venus image and present it with `IOCTL_HELIOS_PRESENT_BLOB`. `LookingGlass\host` still builds on Windows with the MinGW/Ninja `win_looking_glass` path, but the IDD driver is a separate WDK/MSBuild build from a local NTFS mirror. Next validation is installing the rebuilt IDD, enabling `HKLM\SOFTWARE\LookingGlass\IDD\HeliosEnable`, and measuring the prototype copy path before replacing it with a true D3D12/Venus fast path.

---

## ⚠️ VERY IMPORTANT: `CARGO_TARGET_DIR`

The Linux host and the Windows VM (`win11`) **share the same source tree** (the Linux project dir is the VM's `Z:\` drive) but use different toolchains and produce incompatible artifacts. Set `CARGO_TARGET_DIR` per platform — and on Windows it MUST point at **local disk**, never the share:

- **Linux:** `CARGO_TARGET_DIR=target/linux` (native Linux fs).
- **Windows:** a **local C: path**, e.g. `CARGO_TARGET_DIR=C:\Users\Rupansh\helios-target\<crate>` — NOT `Z:\...`. Rust/cargo file IO **fails on the `Z:\` 9p/virtio share**: `OS error 87 (The parameter is incorrect)` on artifact copies, plus `could not canonicalize path Z:\`. Edit source on the share; keep the *target dir* on local C:.

Set this via the **`CARGO_TARGET_DIR` environment variable** on each cargo/`cargo make` invocation. Do **NOT** add `target-dir` to a committed `.cargo/config.toml` — that file is read on **both** platforms, so a Windows `C:` path would break Linux builds of shared crates (e.g. `protocol/`).

**Driving the VM:** prefer the **`win` MCP server** — `win_exec`, and `win_cargo` (which sets the local target dir + `LIBCLANG_PATH` for you) — over raw `ssh win`; it avoids cmd.exe quoting and stale-env issues. **coreutils are installed on `win11`**, so standard Unix tools (`ls`, `cp`, `mv`, `rm`, `cat`, …) work in `win_exec`. The source tree is at `Z:\`. See TOOLCHAIN.md §2.0.

---

## Your Role & Operating Rules

1. **Always read the relevant spec doc before writing any code** in that subsystem. The docs are the ground truth; do not guess at WDF/IOCTL signatures or Venus protocol fields.
2. **Never stub silently.** If a function is a stub, mark it `// STUB: reason` and log a `todo!()` or return a documented error code.
3. **Prefer explicit over clever.** Kernel code has zero tolerance for bugs. Prefer verbose, obviously correct code over clever abstractions.
4. **All unsafe blocks must have a `// SAFETY:` comment** explaining the invariant being upheld.
5. **One DDI function per commit message topic.** Keep changes scoped.
6. **Test at every milestone** using the test plan in each doc before proceeding.
7. When you encounter an IOCTL control code you don't recognize, return `STATUS_INVALID_DEVICE_REQUEST` (not a panic or crash).

---

## Repository Structure

```
helios-vgpu/
├── CLAUDE.md              ← You are here
├── ARCH.md                ← Canonical architecture (System-class KMDF + IOCTL + Venus) — read first
├── OVERVIEW.md            ← Architecture overview
├── KMD.md                 ← Kernel-mode driver guide
├── ICD.md                 ← Vulkan ICD guide
├── TRANSPORT.md           ← VirtIO + Venus wire protocol
├── HOST.md                ← Host-side setup guide
├── TOOLCHAIN.md           ← Build environment setup
│
├── kmd/                   ← Kernel-Mode Driver (Rust, KMDF/WDF)
│   ├── Cargo.toml
│   ├── build.rs
│   ├── helios_kmd.inx
│   └── src/
│       ├── lib.rs         ← DriverEntry, evt_device_add, IOCTL dispatch
│       ├── adapter.rs     ← WDF device-object context (virtio + fence table)
│       ├── ioctl.rs       ← EvtIoDeviceControl handlers (the IOCTL protocol)
│       ├── pnp.rs         ← evt_device_add / prepare_hardware / release_hardware
│       ├── interrupt.rs   ← WdfInterruptCreate + EvtInterruptIsr/EvtInterruptDpc
│       └── virtio/
│           ├── mod.rs
│           ├── pci.rs     ← PCI capability scanning
│           ├── queue.rs   ← Virtqueue implementation
│           └── gpu.rs     ← virtio-gpu command structs
│
├── icd/                   ← Vulkan ICD (Mesa venus port over IOCTL; supersedes the hand-written tree)
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
│   │   └── transport.rs   ← KMD calls via DeviceIoControl on GUID_DEVINTERFACE_HELIOS
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
- [ ] Set up Windows 11 dev VM with WDK (10.0.26100), VS 2022, LLVM 17
- [ ] Flip the driver model to **KMDF** (driver-type `KMDF`, KMDF 1.33); verify `cargo make` / `win_cargo` builds the KMDF skeleton
- [ ] Set up Linux host with QEMU 9.2+, virglrenderer (Venus-enabled)

### Phase 1: KMDF Skeleton — Device Interface
Goal: Driver loads, binds the virtio-gpu PCI FDO, exposes its device interface, kernel does not crash.

- [ ] `DriverEntry` → build `WDF_DRIVER_CONFIG { EvtDriverDeviceAdd: Some(evt_device_add), .. }`, call `WdfDriverCreate`
- [ ] `evt_device_add` → set PnP/power callbacks, `WDF_OBJECT_ATTRIBUTES_INIT_CONTEXT_TYPE(AdapterContext)`, `WdfDeviceCreate`, `WdfDeviceCreateDeviceInterface(&GUID_DEVINTERFACE_HELIOS)`, create the default parallel IO queue with `EvtIoDeviceControl`
- [ ] `evt_device_prepare_hardware` → map BARs via the translated CM resource list (`MmMapIoSpaceEx`); obtain `BUS_INTERFACE_STANDARD` via `WdfFdoQueryForInterface`
- [ ] `evt_device_release_hardware` → `set_virtio(None)`, `MmUnmapIoSpace` each BAR
- [ ] `AdapterContext` lives in the WDF device-object context (`WdfObjectGetTypedContext`): `virtio_lock` + `with_virtio`/`set_virtio` + fence table

**Test:** Device shows in Device Manager under System devices; `GUID_DEVINTERFACE_HELIOS` opens via SetupDi/CreateFile. No BSOD.

### Phase 2: VirtIO Transport under KMDF
Goal: Can send `VIRTIO_GPU_CMD_GET_DISPLAY_INFO` and get a response.

- [ ] Scan PCI capability list for VirtIO capability structures (type 4 = common cfg, type 5 = notify, type 2 = ISR, type 1 = device cfg, type 8 = shared-memory cfg)
- [ ] Map all VirtIO BARs from the CM resource list
- [ ] PCI config access via `KmdfConfigAccess` wrapping `BUS_INTERFACE_STANDARD` (`GetBusData`/`SetBusData`)
- [ ] Initialize virtqueues (control queue = 0, cursor queue = 1) — descriptor/available/used rings
- [ ] Implement `virtio_gpu_ctrl_hdr` and `VIRTIO_GPU_CMD_GET_DISPLAY_INFO`
- [ ] Wire up the interrupt: `WdfInterruptCreate` → `EvtInterruptIsr` (ack + `WdfInterruptQueueDpcForIsr`) → `EvtInterruptDpc` (pop used ring, signal per-fence KEVENT)

**Test:** Send GET_DISPLAY_INFO from a test IOCTL, receive valid response in DebugView.

### Phase 3: IOCTL Device Interface + Blob Management
Goal: User mode can create a Venus context and allocate/map host-visible blobs over `DeviceIoControl`.

- [ ] `evt_io_device_control` dispatch on the IOCTL control code (retrieve buffers via `WdfRequestRetrieveInputBuffer`/`OutputBuffer`; validate every guest-supplied size/offset against the WDF-reported in/out length)
- [ ] `IOCTL_HELIOS_CTX_CREATE` → `VIRTIO_GPU_CMD_CTX_CREATE` with `VIRTIO_GPU_CAPSET_VENUS`; `IOCTL_HELIOS_CTX_DESTROY`
- [ ] `IOCTL_HELIOS_ALLOC_BLOB` → allocate virtio-gpu blob resource IDs (returns `out_resource_id`)
- [ ] `IOCTL_HELIOS_MAP_BLOB` → record the SHARED_MEMORY_CFG(HOST_VISIBLE) BAR base, build an MDL over the mapped pages, `MmMapLockedPagesSpecifyCache(UserMode)`, return `out_user_va`

**Test:** A test EXE opens the device interface, creates a context, allocates a blob, and reads back a mapped user VA.

### Phase 4: Venus Submission + Fences
Goal: First real Vulkan command reaches virglrenderer on the host.

- [ ] `IOCTL_HELIOS_SUBMIT_VENUS` (METHOD_IN_DIRECT) → small buffered header (ctx_id/fence_id/buffer_size) + Venus blob via locked MDL (`WdfRequestRetrieveInputWdmMdl`); forward opaque Venus bytes to the host
- [ ] `IOCTL_HELIOS_WAIT_FENCE` → block on the `fence_id → KEVENT` table (timeout_ns); `EvtInterruptDpc` signals the event when the used ring reports completion
- [ ] Venus commands must be flushed before signaling the fence (ordering)

**Test:** `vkCreateInstance` from a test EXE reaches the host and virglrenderer logs it.

### Phase 5: Vulkan ICD (icd/)
Goal: `vulkaninfo` reports the Helios device.

> **PLAN (2026-06-04, user-approved — see the `mesa-venus-icd-port` memory):** do NOT hand-write the Vulkan ICD / Venus encoder. **Port Mesa's `venus` driver (`-Dvulkan-drivers=virtio`)** to Windows, swapping its Linux virtgpu-DRM `vn_renderer` backend for an IOCTL backend over `DeviceIoControl` on `GUID_DEVINTERFACE_HELIOS` (modeled on the clean non-DRM `vn_renderer_vtest.c` template). This reuses Mesa's mature, byte-correct Venus encoder (virglrenderer's decoder is also Mesa → wire-compatible) and follows the mvisor-win-vgpu-driver porting pattern. The win11 Mesa build toolchain is already installed. The Windows Vulkan loader enumerates it purely through `HKLM\SOFTWARE\Khronos\Vulkan\Drivers` registry JSON — precedent: SwiftShader and Mesa lavapipe enumerate via exactly this mechanism with no display adapter.

- [ ] Port Mesa venus → Windows Vulkan ICD with an IOCTL `vn_renderer` backend (see memory)
- [ ] Implement `vkEnumeratePhysicalDevices` → forward to host, deserialize capabilities
- [ ] Implement `vkCreateDevice`, queues, command pools

**Test:** `vulkaninfo` shows one physical device. `vkcube` renders.

### Phase 6: DXVK + VKD3D Integration
Goal: D3D11/D3D12 apps render via DXVK → Helios ICD → Venus → host GPU.

- [ ] Install DXVK and configure it to use the Helios Vulkan ICD
- [ ] Run a D3D11 triangle app

**Test:** d3d11-triangle or dxvk-tests pass.

### Phase 7: Presentation / Display Integration — archived, not active

The DOD + `SET_SCANOUT_BLOB` pivot is archived in `DISPLAY.md`, `PHASE7_DISPLAY_HANDOVER.md`, and
`CODE43_HANDOFF_FOR_CODEX.md`. Do not continue DOD VidPN/Code 43 work unless the owner explicitly asks for a
display-driver experiment.

Active replacement:

- Restore System-class KMDF + IOCTL setup.
- Benchmark offscreen Venus rendering and submit/fence latency.
- Fix the renderer path before display integration.
- Revisit presentation only after renderer performance is acceptable.

---

## Key Invariants to Never Violate

| Rule | Why |
|------|-----|
| Never call pageable code at IRQL > APC_LEVEL | BSOD `IRQL_NOT_LESS_OR_EQUAL` |
| Never allocate in ISR (EvtInterruptIsr) | ISR runs at DIRQL |
| Always validate IOCTL input buffer bounds in KMD | Security — guest can send malicious offsets |
| Virtqueue descriptors must not be freed while in-flight | Corruption |
| Venus commands must be flushed before signaling fence | Ordering |

---

## When You're Stuck

1. Check `KMD.md` — there's a troubleshooting section per phase.
2. Check the C reference for the **virtio init** path only: [kvm-guest-drivers-windows viogpu](https://github.com/virtio-win/kvm-guest-drivers-windows/tree/master/viogpu) — its virtio init code (`viogpu_queue.cpp`) is directly relevant; ignore its display/WDDM surface.
3. For the WDF/IOCTL surface: use the WDK headers via `wdk-sys` (`WdfRequestRetrieveInputBuffer`/`WdfRequestRetrieveInputWdmMdl`, `WdfDeviceCreateDeviceInterface`, `WdfInterruptCreate`) and `GUID_DEVINTERFACE_*` conventions. The [mvisor-win-vgpu-driver](https://github.com/Theelx/mvisor-win-vgpu-driver) is the reference for the System-class KMDF + IOCTL + Venus model.
4. For Venus protocol: `venus-protocol/vk.xml` and `virglrenderer/src/venus/` are the ground truth.
5. Ask the overseer if you hit a fundamental architecture issue.

---

## Files Not to Touch

- `*.inx` files after initial creation — only modify with explicit instruction. The active INF shape is
  System-class KMDF. The Display-class DOD INF shape is historical reference only.

---

## Code Style

```rust
// All kernel-mode code: no_std, no panics in release
// Use wdk-sys types directly for WDF/IOCTL structs
// Pattern for IOCTL handlers (dispatched from EvtIoDeviceControl):
unsafe fn handle_foo(
    adapter: &AdapterContext,
    request: WDFREQUEST,
) -> Result<usize, DriverError> {
    // SAFETY: WDF guarantees `request` is a valid kernel-mode handle for the
    //         lifetime of this callback; the input buffer length is validated
    //         against size_of::<HeliosFoo>() before any read.
    let in_buf = request_input_buffer::<HeliosFoo>(request)?;
    let args = pod_read_unaligned::<HeliosFoo>(in_buf);

    adapter.with_virtio(|v| v.foo(&args))?;
    Ok(0) // bytes_returned for WdfRequestCompleteWithInformation
}
```
