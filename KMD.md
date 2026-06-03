# KMD.md — Kernel-Mode Driver Implementation Guide

## Overview

The KMD (Kernel-Mode Driver) is a WDDM 2.x **render-only display miniport driver**. It lives in the Windows kernel, talks to the virtio-gpu PCI device, and exposes a GPU adapter to the Windows graphics stack.

**References:**
- WDDM initialization: https://learn.microsoft.com/en-us/windows-hardware/drivers/display/initializing-display-miniport-and-user-mode-display-drivers
- DDI reference index: https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/_display/
- DRIVER_INITIALIZATION_DATA: https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/dispmprt/ns-dispmprt-_driver_initialization_data
- Render-only adapter: https://learn.microsoft.com/en-us/windows-hardware/drivers/display/render-only-display-drivers

---

## ⚠️ Implementation Reality (read before the code below)

The code in this guide was written **before** implementation and is wrong in several load-bearing ways. The working, building source under `kmd/` is the ground truth — prefer it over the snippets below. Key corrections (all confirmed against the WDK 10.0.26100 bindings + a driver that compiles, links, and packages):

- **No display DDIs in `wdk-sys`.** It has no display `ApiSubset`, so `DRIVER_INITIALIZATION_DATA`, `DXGKRNL_INTERFACE`, every `DXGK*`/`DXGKARG_*`, and `DxgkInitialize` are **absent**. `build.rs` runs **custom bindgen** over `dispmprt.h`+`d3dkmddi.h` → `$OUT_DIR/dxgk_bindings.rs`, surfaced by `src/dxgk.rs` (which also `pub use wdk_sys::*;`). Use `use crate::dxgk::*;`, not `use wdk_sys::*`, for DDI types. `bindgen` must be **0.71** (match `wdk-build`, so `BuilderExt::wdk_default` applies).
- **Link `displib.lib`.** `DxgkInitialize` lives there; `wdk-build` doesn't link it. `build.rs` needs `println!("cargo:rustc-link-lib=static=displib");` or it's an unresolved external.
- **Panic handler:** do **not** define your own `#[panic_handler]` — just `extern crate wdk_panic;` (it supplies one; a second is a duplicate lang item). `wdk_panic::default_panic_handler` / `PanicHandler` are not real API.
- **DriverEntry:** `DxgkInitialize`'s 3rd arg is `*mut DRIVER_INITIALIZATION_DATA` → `let mut init = build_ddi_table(); DxgkInitialize(.., &mut init)`. Zero-init via `core::mem::zeroed()` (it has no `Default`).
- **Version fields:** `data.Version = DXGKDDI_INTERFACE_VERSION` (the symbol). The note's hex is wrong — WDDM2_0 is `0x5023`, not `0x7002`. `DXGK_DRIVERCAPS.WDDMVersion` must be **0** (reserved for `>= WIN7` drivers), not `KMT_DRIVERVERSION_WDDM_2_0`.
- **`DXGK_DRIVERCAPS`:** there is **no** `PresentationCaps.NoScanout` (render-only = report 0 sources, not a cap bit). `SupportNonVGA` is a plain `BOOLEAN` field. You **must** also fill `MemoryManagementCaps` with `VirtualAddressingSupported=1` + `GpuMmuSupported=1` to actually advertise GPU VA.
- **`DXGK_QUERYSEGMENTOUT4`:** count field is `NbSegment` (not `SegmentCount`); set `SegmentDescriptorStride` (u64); `pSegmentDescriptor` is `*mut u8` → cast to `*mut DXGK_SEGMENTDESCRIPTOR4`; flags via the bindgen union: `seg.Flags.__bindgen_anon_1.__bindgen_anon_1.set_CpuVisible(1)`/`set_Aperture(1)`.
- **bindgen enum-modules:** `DXGK_QUERYADAPTERINFOTYPE`, `POWER_ACTION`, `DXGK_WDDMVERSION`, … are modules — use `::Type` for the type and module constants for values (e.g. `_DXGK_QUERYADAPTERINFOTYPE::DXGKQAITYPE_DRIVERCAPS`). `DispatchIoRequest`'s 3rd arg is `PVIDEO_REQUEST_PACKET`.
- **Render-path DDIs:** for the runtime to create a device on the adapter, also register (stubs returning `STATUS_NOT_IMPLEMENTED` are fine, but the slots must be non-NULL): `Render`/`RenderKm`, `Patch`, `OpenAllocation`/`CloseAllocation`, `DescribeAllocation`, `GetStandardAllocationDriverData`, `GetNodeMetadata`, `SetRootPageTable`/`GetRootPageTableSize`, `CollectDbgInfo`, `ControlInterrupt`, `QueryCurrentFence`.
- **Build on local disk, never `Z:\`** (cargo/wdk IO fails on the 9p share — OS error 87, windows-drivers-rs#481): the `win` MCP `win_cargo` tool robocopy-mirrors to `C:\Users\Rupansh\helios-vgpu` and builds there. See TOOLCHAIN.md.
- **Driver model:** WDDM 2.0 **render-only graphics** miniport (NULL all VidPN DDIs, 0 sources) — *not* MCDM (`ComputeAccelerator`/`ComputeOnly`), which is the compute model and would risk the 3D-graphics path. See OVERVIEW.md "Driver Model & WDDM Targeting".

---

## Phase 1: DriverEntry and DDI Table

### 1.1 `src/lib.rs` — Entry Point

```rust
#![no_std]
#![allow(non_snake_case, non_camel_case_types, non_upper_case_globals)]

extern crate alloc;

use wdk_alloc::WdkAllocator;
use wdk_panic::PanicHandler;
use wdk_sys::{
    ntddk::*,
    *,
};

// Required: kernel allocator
#[global_allocator]
static ALLOCATOR: WdkAllocator = WdkAllocator;

// Required: panic handler (calls KeBugCheck, never returns)
#[panic_handler]
fn panic_handler(info: &core::panic::PanicInfo) -> ! {
    // SAFETY: KeBugCheck terminates the system; this is correct for kernel panics.
    unsafe { wdk_panic::default_panic_handler(info) }
}

mod adapter;
mod device;
mod interrupt;
mod memory;
mod scheduler;
mod virtio;
mod ddi;

use ddi::*;

/// DriverEntry: registered in the INF as the driver entrypoint.
/// Called by the OS when the driver is first loaded.
///
/// # Safety
/// Called by the OS with valid pointers. Standard WDM contract.
#[export_name = "DriverEntry"]
pub unsafe extern "system" fn driver_entry(
    driver_object: *mut DRIVER_OBJECT,
    registry_path: *const UNICODE_STRING,
) -> NTSTATUS {
    // SAFETY: Dxgkrnl guarantees driver_object and registry_path are valid.
    let status = unsafe {
        DxgkInitialize(driver_object, registry_path, &build_ddi_table())
    };
    status
}

fn build_ddi_table() -> DRIVER_INITIALIZATION_DATA {
    // Zero-initialize, then fill in what we support.
    // Render-only: display DDIs are left as NULL.
    let mut data = DRIVER_INITIALIZATION_DATA::default();

    // Required: WDDM version we're targeting (2.0 = Windows 10)
    // DXGKDDI_INTERFACE_VERSION_WDDM2_0 = 0x7002
    data.Version = DXGKDDI_INTERFACE_VERSION as u32;

    // PnP / power lifecycle
    data.DxgkDdiAddDevice             = Some(dxgkddi_add_device);
    data.DxgkDdiStartDevice           = Some(dxgkddi_start_device);
    data.DxgkDdiStopDevice            = Some(dxgkddi_stop_device);
    data.DxgkDdiRemoveDevice          = Some(dxgkddi_remove_device);
    data.DxgkDdiSetPowerState         = Some(dxgkddi_set_power_state);
    data.DxgkDdiDispatchIoRequest     = Some(dxgkddi_dispatch_io_request);
    data.DxgkDdiInterruptRoutine      = Some(dxgkddi_interrupt_routine);
    data.DxgkDdiDpcRoutine            = Some(dxgkddi_dpc_routine);

    // Adapter queries
    data.DxgkDdiQueryChildRelations   = Some(dxgkddi_query_child_relations);
    data.DxgkDdiQueryChildStatus      = Some(dxgkddi_query_child_status);
    data.DxgkDdiQueryDeviceDescriptor = Some(dxgkddi_query_device_descriptor);
    data.DxgkDdiQueryAdapterInfo      = Some(dxgkddi_query_adapter_info);

    // Device / context / allocation management
    data.DxgkDdiCreateDevice          = Some(dxgkddi_create_device);
    data.DxgkDdiDestroyDevice         = Some(dxgkddi_destroy_device);
    data.DxgkDdiCreateAllocation      = Some(dxgkddi_create_allocation);
    data.DxgkDdiDestroyAllocation     = Some(dxgkddi_destroy_allocation);
    data.DxgkDdiCreateContext         = Some(dxgkddi_create_context);
    data.DxgkDdiDestroyContext        = Some(dxgkddi_destroy_context);

    // Memory management / GPU VA
    data.DxgkDdiBuildPagingBuffer     = Some(dxgkddi_build_paging_buffer);

    // Command submission (WDDM 2.0 uses SubmitCommandVirtual)
    data.DxgkDdiSubmitCommandVirtual  = Some(dxgkddi_submit_command_virtual);
    data.DxgkDdiPreemptCommand        = Some(dxgkddi_preempt_command);
    data.DxgkDdiResetFromTimeout      = Some(dxgkddi_reset_from_timeout);
    data.DxgkDdiRestartFromTimeout    = Some(dxgkddi_restart_from_timeout);

    // Out-of-band ICD → KMD channel
    data.DxgkDdiEscape                = Some(dxgkddi_escape);

    // GPU Virtual Addressing (required for WDDM 2.0)
    data.DxgkDdiCreateProcess         = Some(dxgkddi_create_process);
    data.DxgkDdiDestroyProcess        = Some(dxgkddi_destroy_process);

    // Display DDIs — all NULL for render-only adapter
    // DxgkDdiSetVidPnSourceAddress      = None (default)
    // DxgkDdiRecommendFunctionalVidPn   = None
    // ... etc.

    data
}
```

**Note on `DXGKDDI_INTERFACE_VERSION`:** For WDDM 2.0 you want `DXGKDDI_INTERFACE_VERSION_WDDM2_0`. Check `d3dkmddi.h` in the WDK for the exact value (`0x7002`). For WDDM 2.6 (Windows 10 1903+), use `DXGKDDI_INTERFACE_VERSION_WDDM2_6` (`0x9003`). Set the lowest version that gives you the DDIs you need — start with 2.0.

---

## Phase 1: DxgkDdiAddDevice

Called when the OS finds a device matching the INF's hardware ID. Allocate the adapter context.

**Reference:** https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/dispmprt/nc-dispmprt-dxgkddi_add_device

```rust
// src/ddi/add_device.rs

use crate::adapter::AdapterContext;
use wdk_sys::*;

pub unsafe extern "C" fn dxgkddi_add_device(
    physical_device_object: *const DEVICE_OBJECT,
    miniport_device_context: *mut *mut core::ffi::c_void,
) -> NTSTATUS {
    // SAFETY: physical_device_object is a valid PDO from the PnP manager.
    //         miniport_device_context is a valid out-pointer.

    let ctx = match AdapterContext::new(physical_device_object) {
        Ok(c) => c,
        Err(e) => return e.into_ntstatus(),
    };

    // Box the context and leak it as a raw pointer.
    // We will reclaim it in DxgkDdiRemoveDevice.
    let raw = Box::into_raw(Box::new(ctx)) as *mut core::ffi::c_void;

    unsafe { *miniport_device_context = raw };
    STATUS_SUCCESS
}
```

```rust
// src/adapter.rs

use wdk_sys::*;

pub struct AdapterContext {
    /// The physical device object (PDO) for the virtio-gpu device
    pub pdo: *const DEVICE_OBJECT,
    /// Kernel interface callbacks (filled by DxgkDdiStartDevice)
    pub dxgkrnl: Option<DXGKRNL_INTERFACE>,
    /// VirtIO device state
    pub virtio: Option<crate::virtio::VirtioGpu>,
    /// Fence sequence counter
    pub fence_seq: u64,
}

// SAFETY: The kernel guarantees single-threaded access during init DDIs.
// For concurrent access, we'll add spinlocks later.
unsafe impl Send for AdapterContext {}
unsafe impl Sync for AdapterContext {}

impl AdapterContext {
    pub fn new(pdo: *const DEVICE_OBJECT) -> Result<Self, DriverError> {
        Ok(Self {
            pdo,
            dxgkrnl: None,
            virtio: None,
            fence_seq: 0,
        })
    }
}

pub enum DriverError {
    InsufficientResources,
    InvalidParameter,
    DeviceNotFound,
    IoError,
}

impl DriverError {
    pub fn into_ntstatus(self) -> NTSTATUS {
        match self {
            Self::InsufficientResources => STATUS_INSUFFICIENT_RESOURCES,
            Self::InvalidParameter      => STATUS_INVALID_PARAMETER,
            Self::DeviceNotFound        => STATUS_DEVICE_DOES_NOT_EXIST,
            Self::IoError               => STATUS_IO_DEVICE_ERROR,
        }
    }
}
```

---

## Phase 1: DxgkDdiStartDevice

The most critical initialization DDI. Maps hardware resources, saves the kernel interface.

**Reference:** https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/dispmprt/nc-dispmprt-dxgkddi_start_device

```rust
// src/ddi/start_device.rs

pub unsafe extern "C" fn dxgkddi_start_device(
    miniport_device_context: *mut core::ffi::c_void,
    dxgk_start_info: *const DXGK_START_INFO,
    dxgkrnl_interface: *const DXGKRNL_INTERFACE,
    number_of_video_present_sources: *mut u32,
    number_of_children: *mut u32,
) -> NTSTATUS {
    // SAFETY: All pointers guaranteed valid by Dxgkrnl contract.
    let adapter = unsafe { &mut *(miniport_device_context as *mut AdapterContext) };
    let dxgkrnl = unsafe { &*dxgkrnl_interface };

    // 1. Save Dxgkrnl callbacks — we need these for the entire driver lifetime.
    adapter.dxgkrnl = Some(unsafe { dxgkrnl.clone() });

    // 2. Get device info (translated resource list, registry path, etc.)
    let mut device_info = DXGK_DEVICE_INFO::default();
    let status = unsafe {
        (dxgkrnl.DxgkCbGetDeviceInformation)(
            dxgkrnl.DeviceHandle,
            &mut device_info,
        )
    };
    if status != STATUS_SUCCESS { return status; }

    // 3. Initialize VirtIO GPU device
    //    (maps PCI BARs, negotiates features, sets up virtqueues)
    let virtio = match crate::virtio::VirtioGpu::init(&device_info) {
        Ok(v) => v,
        Err(e) => return e.into_ntstatus(),
    };
    adapter.virtio = Some(virtio);

    // 4. Render-only: no video present sources, no children (no monitors)
    unsafe {
        *number_of_video_present_sources = 0;
        *number_of_children = 0;
    }

    STATUS_SUCCESS
}
```

---

## Phase 1: DxgkDdiQueryAdapterInfo

The most important capability DDI. Here we declare render-only support.

**Reference:** https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmddi/nc-d3dkmddi-dxgkddi_queryadapterinfo  
**DXGK_DRIVERCAPS:** https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmddi/ns-d3dkmddi-_dxgk_drivercaps

```rust
// src/ddi/query_adapter_info.rs

pub unsafe extern "C" fn dxgkddi_query_adapter_info(
    miniport_device_context: *mut core::ffi::c_void,
    query_adapter_info: *const DXGKARG_QUERYADAPTERINFO,
) -> NTSTATUS {
    let _adapter = unsafe { &mut *(miniport_device_context as *mut AdapterContext) };
    let args = unsafe { &*query_adapter_info };

    match args.Type {
        DXGKQAITYPE_DRIVERCAPS => query_driver_caps(args),
        DXGKQAITYPE_QUERYSEGMENT4 => query_segments(args),
        DXGKQAITYPE_GPUMMUCAPS => query_gpummu_caps(args),
        // Return not-supported for everything else; Dxgkrnl handles it.
        _ => STATUS_NOT_SUPPORTED,
    }
}

fn query_driver_caps(args: &DXGKARG_QUERYADAPTERINFO) -> NTSTATUS {
    if args.OutputDataSize < core::mem::size_of::<DXGK_DRIVERCAPS>() as u32 {
        return STATUS_BUFFER_TOO_SMALL;
    }
    // SAFETY: OutputData points to a DXGK_DRIVERCAPS buffer of sufficient size.
    let caps = unsafe { &mut *(args.pOutputData as *mut DXGK_DRIVERCAPS) };
    *caps = DXGK_DRIVERCAPS::default();

    // ── Render capabilities ──────────────────────────────────────────────────
    // PresentationCaps: we do NOT present (render-only)
    caps.PresentationCaps.NoScanout = 1;        // no display output
    caps.PresentationCaps.SupportKernelModeCommandBuffer = 0;

    // SchedulingCaps: enable GPU preemption at DMA buffer level
    caps.SchedulingCaps.MultiEngineAware = 0;   // one engine for now
    caps.SchedulingCaps.VSyncPowerSaveAware = 0;

    // FlipCaps: N/A for render-only (no flip/scanout)
    // caps.FlipCaps stays zeroed

    // GpuEngineTopology: one 3D engine
    caps.GpuEngineTopology.NbAsyncEngineCount = 0;

    // SupportNonVGA: yes, we're not a legacy VGA device
    caps.SupportNonVGA = 1;

    // SupportSmoothRotation: irrelevant for render-only
    caps.SupportSmoothRotation = 0;

    // SupportPerEngineTDR: report false for simplicity initially
    caps.SupportPerEngineTDR = 0;

    // WDDMVersion: must match the Version in DRIVER_INITIALIZATION_DATA
    // For WDDM 2.0: DXGKDDI_WDDMVersion = KMT_DRIVERVERSION_WDDM_2_0 = 2000
    caps.WDDMVersion = KMT_DRIVERVERSION_WDDM_2_0 as u32;

    // MaxAllocationListSlotId: max resource IDs
    caps.MaxAllocationListSlotId = 0xFFFF;

    // ApertureSegmentCommitLimit: max bytes we allow committed in aperture
    caps.ApertureSegmentCommitLimit = 512 * 1024 * 1024; // 512 MB

    // HighestAcceptableAddress: 64-bit addressing
    caps.HighestAcceptableAddress.QuadPart = !0i64;

    STATUS_SUCCESS
}

fn query_segments(args: &DXGKARG_QUERYADAPTERINFO) -> NTSTATUS {
    // WDDM 2.0 uses DXGK_QUERYSEGMENTOUT4
    // We report one aperture segment backed by the virtio-gpu hostmem blob.
    //
    // An aperture segment is CPU-accessible memory — the guest can write
    // into it, and the host virglrenderer reads from it.
    if args.OutputDataSize < core::mem::size_of::<DXGK_QUERYSEGMENTOUT4>() as u32 {
        return STATUS_BUFFER_TOO_SMALL;
    }

    // First call: Dxgkrnl sends pSegmentDescriptor = NULL to query count.
    // We return SegmentCount = 1, PagingBufferSegmentId = 1.
    let out = unsafe { &mut *(args.pOutputData as *mut DXGK_QUERYSEGMENTOUT4) };

    if args.pInputData.is_null()
        || (unsafe { &*(args.pInputData as *const DXGK_QUERYSEGMENTIN) }).AgpApertureBase == 0
    {
        // Second call: fill in the segment descriptor.
        // For now, use 512 MB aperture at a host-provided physical address.
        // The actual BAR address is filled in from the virtio-gpu hostmem region.
        out.SegmentCount = 1;
        out.PagingBufferSegmentId = 1;
        out.PagingBufferSize = 64 * 1024; // 64 KB paging buffer
        out.PagingBufferPrivateDataSize = 0;

        // pSegmentDescriptor is an array of DXGK_SEGMENTDESCRIPTOR4
        // Dxgkrnl allocates this; we just fill it.
        if !out.pSegmentDescriptor.is_null() {
            let seg = unsafe { &mut *out.pSegmentDescriptor };
            // CPU-accessible aperture (the hostmem= region)
            seg.Flags.CpuVisible = 1;
            seg.Flags.Aperture = 1;
            // Physical address from virtio-gpu BAR
            // (filled later after PCI init — placeholder)
            seg.BaseAddress.QuadPart = 0; // STUB: fill from VirtioGpu::hostmem_gpa
            seg.Size = 512 * 1024 * 1024; // 512 MB
            seg.CommitLimit = 512 * 1024 * 1024;
        }
    }
    STATUS_SUCCESS
}

fn query_gpummu_caps(args: &DXGKARG_QUERYADAPTERINFO) -> NTSTATUS {
    // GPU virtual addressing caps (WDDM 2.0 requirement)
    // https://learn.microsoft.com/en-us/windows-hardware/drivers/display/gpu-virtual-memory-in-wddm-2-0
    let caps = unsafe { &mut *(args.pOutputData as *mut DXGK_GPUMMUCAPS) };
    *caps = DXGK_GPUMMUCAPS::default();

    // 48-bit virtual address space (standard for x64)
    caps.VirtualAddressBitCount = 48;
    caps.PageTableLevelCount = 4;          // 4-level page table (PML4)
    caps.LargePageSupported = 0;           // start simple
    caps.DualPteSupported = 0;
    caps.AllowNonAlignedLargePageAddress = 0;

    STATUS_SUCCESS
}
```

---

## Phase 2: VirtIO PCI Initialization

### VirtIO PCI Device Layout

The virtio-gpu device presents as PCI vendor `0x1AF4`, device `0x1050`.

VirtIO devices use PCI capabilities to describe their config regions:

| Cap type | Meaning | BAR use |
|----------|---------|---------|
| 1 (COMMON_CFG) | Common config (feature bits, queue config) | BAR varies |
| 2 (NOTIFY_CFG) | Queue notification doorbells | BAR varies |
| 3 (ISR_CFG) | Interrupt status register | BAR varies |
| 4 (DEVICE_CFG) | Device-specific config (display info) | BAR varies |
| 8 (PCI_CFG) | Alternative config access | N/A |

**Reference:** VirtIO spec §4.1 https://docs.oasis-open.org/virtio/virtio/v1.2/virtio-v1.2.html#sec-virtio-over-pci-bus

### VirtIO Command Structures

Key structures from `virtio_gpu.h` (must match virglrenderer's definitions):

```rust
// src/virtio/gpu.rs

use bytemuck::{Pod, Zeroable};

/// VirtIO GPU control command header (prepended to every command)
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct VirtioGpuCtrlHdr {
    pub type_: u32,   // VIRTIO_GPU_CMD_* or VIRTIO_GPU_RESP_*
    pub flags: u32,
    pub fence_id: u64,
    pub ctx_id: u32,
    pub ring_idx: u8,
    pub padding: [u8; 3],
}

// Command types
pub const VIRTIO_GPU_CMD_GET_DISPLAY_INFO:  u32 = 0x0100;
pub const VIRTIO_GPU_CMD_RESOURCE_CREATE_2D: u32 = 0x0101;
pub const VIRTIO_GPU_CMD_RESOURCE_UNREF:    u32 = 0x0102;
pub const VIRTIO_GPU_CMD_SET_SCANOUT:       u32 = 0x0103;
pub const VIRTIO_GPU_CMD_RESOURCE_FLUSH:    u32 = 0x0104;
pub const VIRTIO_GPU_CMD_TRANSFER_TO_HOST_2D: u32 = 0x0105;
pub const VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING: u32 = 0x0106;
pub const VIRTIO_GPU_CMD_RESOURCE_DETACH_BACKING: u32 = 0x0107;
pub const VIRTIO_GPU_CMD_CTX_CREATE:        u32 = 0x0200;
pub const VIRTIO_GPU_CMD_CTX_DESTROY:       u32 = 0x0201;
pub const VIRTIO_GPU_CMD_CTX_ATTACH_RESOURCE: u32 = 0x0202;
pub const VIRTIO_GPU_CMD_CTX_DETACH_RESOURCE: u32 = 0x0203;
pub const VIRTIO_GPU_CMD_SUBMIT_3D:         u32 = 0x0204;
pub const VIRTIO_GPU_CMD_RESOURCE_CREATE_BLOB: u32 = 0x0208;
pub const VIRTIO_GPU_CMD_RESOURCE_MAP_BLOB:   u32 = 0x0209;
pub const VIRTIO_GPU_CMD_RESOURCE_UNMAP_BLOB: u32 = 0x020a;

// Response types
pub const VIRTIO_GPU_RESP_OK_NODATA:        u32 = 0x1100;
pub const VIRTIO_GPU_RESP_OK_DISPLAY_INFO:  u32 = 0x1101;
pub const VIRTIO_GPU_RESP_ERR_UNSPEC:       u32 = 0x1200;
pub const VIRTIO_GPU_RESP_ERR_OUT_OF_MEMORY: u32 = 0x1201;
pub const VIRTIO_GPU_RESP_ERR_INVALID_SCANOUT_ID: u32 = 0x1202;
pub const VIRTIO_GPU_RESP_ERR_INVALID_RESOURCE_ID: u32 = 0x1203;
pub const VIRTIO_GPU_RESP_ERR_INVALID_CONTEXT_ID: u32 = 0x1204;

// Flags
pub const VIRTIO_GPU_FLAG_FENCE: u32 = 1 << 0;

/// Context creation (for Venus: capset_id = VIRTIO_GPU_CAPSET_VENUS = 4)
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct VirtioGpuCtxCreate {
    pub hdr: VirtioGpuCtrlHdr,
    pub nlen: u32,
    pub context_init: u32,  // capset_id for Venus
    pub debug_name: [u8; 64],
}

pub const VIRTIO_GPU_CAPSET_VIRGL:  u32 = 1;
pub const VIRTIO_GPU_CAPSET_VIRGL2: u32 = 2;
pub const VIRTIO_GPU_CAPSET_VENUS:  u32 = 4;  // Vulkan via Venus

/// Submit 3D command (Venus commands go here)
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct VirtioGpuCmdSubmit {
    pub hdr: VirtioGpuCtrlHdr,
    pub size: u32,
    pub padding: u32,
    // Followed by `size` bytes of command data
}

/// Blob resource creation (zero-copy memory between guest and host)
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct VirtioGpuResourceCreateBlob {
    pub hdr: VirtioGpuCtrlHdr,
    pub resource_id: u32,
    pub blob_mem: u32,
    pub blob_flags: u32,
    pub nr_entries: u32,
    pub blob_id: u64,
    pub size: u64,
}

pub const VIRTIO_GPU_BLOB_MEM_GUEST:           u32 = 1;
pub const VIRTIO_GPU_BLOB_MEM_HOST3D:          u32 = 2;
pub const VIRTIO_GPU_BLOB_MEM_HOST3D_GUEST:    u32 = 3;
pub const VIRTIO_GPU_BLOB_FLAG_USE_MAPPABLE:   u32 = 1;
pub const VIRTIO_GPU_BLOB_FLAG_USE_SHAREABLE:  u32 = 2;
pub const VIRTIO_GPU_BLOB_FLAG_USE_CROSS_DEVICE: u32 = 4;
```

### Virtqueue Implementation

```rust
// src/virtio/queue.rs
// 
// A split virtqueue as per VirtIO spec §2.7.
// Reference: https://docs.oasis-open.org/virtio/virtio/v1.2/virtio-v1.2.html#sec-split-virtqueues

use wdk_sys::ntddk::*;
use wdk_sys::*;

const QUEUE_SIZE: usize = 256; // must be power of 2

/// Descriptor table entry (16 bytes each)
#[repr(C, align(16))]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct VirtqDesc {
    addr:  u64,   // guest physical address
    len:   u32,
    flags: u16,   // VIRTQ_DESC_F_*
    next:  u16,   // next descriptor index (if VIRTQ_DESC_F_NEXT)
}

const VIRTQ_DESC_F_NEXT:     u16 = 1;
const VIRTQ_DESC_F_WRITE:    u16 = 2; // device-writable (for responses)
const VIRTQ_DESC_F_INDIRECT: u16 = 4;

/// Available ring (driver → device)
#[repr(C)]
#[derive(Debug)]
struct VirtqAvail {
    flags: u16,
    idx:   u16,
    ring:  [u16; QUEUE_SIZE],
    used_event: u16,
}

/// Used ring entry
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct VirtqUsedElem {
    id:  u32,
    len: u32,
}

/// Used ring (device → driver)
#[repr(C)]
#[derive(Debug)]
struct VirtqUsed {
    flags: u16,
    idx:   u16,
    ring:  [VirtqUsedElem; QUEUE_SIZE],
    avail_event: u16,
}

pub struct Virtqueue {
    /// Descriptor table (QUEUE_SIZE entries × 16 bytes)
    desc_phys:  u64,
    desc_virt:  *mut VirtqDesc,
    /// Available ring
    avail_phys: u64,
    avail_virt: *mut VirtqAvail,
    /// Used ring
    used_phys:  u64,
    used_virt:  *mut VirtqUsed,
    /// Notification address (doorbell)
    notify_addr: *mut u16,
    notify_mult: u32,         // multiply queue_notify_off by this
    /// Free descriptor management
    free_head: u16,
    num_free: u16,
    /// Last seen used idx (for processing completions)
    last_used_idx: u16,
    /// Queue index (0 = ctrl, 1 = cursor)
    queue_idx: u16,
}

impl Virtqueue {
    /// Allocate and initialize the virtqueue.
    ///
    /// # Safety
    /// notify_addr must point to a valid MMIO doorbell register.
    pub unsafe fn new(
        queue_idx: u16,
        notify_addr: *mut u16,
        notify_mult: u32,
    ) -> Result<Self, super::VirtioError> {
        // Allocate contiguous physical memory for descriptor + avail + used rings.
        // Total: (16 * QUEUE_SIZE) + (6 + 2*QUEUE_SIZE) + (6 + 8*QUEUE_SIZE) bytes
        // Align to 4096 bytes.
        //
        // For simplicity, allocate one contigious MDL-pinned buffer.
        // TODO: In production, use MmAllocateContiguousMemory or separate MDLs.

        let desc_size  = core::mem::size_of::<VirtqDesc>() * QUEUE_SIZE;
        let avail_size = 6 + 2 * QUEUE_SIZE;
        let used_size  = 6 + 8 * QUEUE_SIZE;
        let total = desc_size + avail_size + used_size + 4096; // extra for alignment

        // SAFETY: PASSIVE_LEVEL, non-paged pool
        let virt = unsafe {
            ExAllocatePool2(POOL_FLAG_NON_PAGED, total as u64, u32::from_be_bytes(*b"VRTQ"))
        };
        if virt.is_null() {
            return Err(super::VirtioError::OutOfMemory);
        }
        // Zero-initialize
        unsafe { core::ptr::write_bytes(virt as *mut u8, 0, total); }

        // Get physical address
        let phys = unsafe { MmGetPhysicalAddress(virt) };

        let desc_virt  = virt as *mut VirtqDesc;
        let avail_virt = unsafe { (virt as *mut u8).add(desc_size) as *mut VirtqAvail };
        let used_virt  = unsafe { (virt as *mut u8).add(desc_size + avail_size) as *mut VirtqUsed };

        // Initialize free list (linked list via desc.next)
        for i in 0..(QUEUE_SIZE - 1) {
            unsafe { (*desc_virt.add(i)).next = (i + 1) as u16; }
        }
        unsafe { (*desc_virt.add(QUEUE_SIZE - 1)).next = 0; }

        Ok(Self {
            desc_phys: phys.QuadPart as u64,
            desc_virt,
            avail_phys: phys.QuadPart as u64 + desc_size as u64,
            avail_virt,
            used_phys: phys.QuadPart as u64 + desc_size as u64 + avail_size as u64,
            used_virt,
            notify_addr,
            notify_mult,
            free_head: 0,
            num_free: QUEUE_SIZE as u16,
            last_used_idx: 0,
            queue_idx,
        })
    }

    /// Allocate a descriptor chain for a scatter-gather I/O.
    /// `bufs`: list of (phys_addr, len, writable) tuples.
    pub fn add_buffer(
        &mut self,
        bufs: &[(u64, u32, bool)],
    ) -> Option<u16> {
        if bufs.len() > self.num_free as usize { return None; }

        let head = self.free_head;
        let mut idx = head;

        for (i, &(addr, len, writable)) in bufs.iter().enumerate() {
            let desc = unsafe { &mut *self.desc_virt.add(idx as usize) };
            desc.addr  = addr;
            desc.len   = len;
            desc.flags = if writable { VIRTQ_DESC_F_WRITE } else { 0 };
            if i + 1 < bufs.len() {
                desc.flags |= VIRTQ_DESC_F_NEXT;
                desc.next   = unsafe { (*self.desc_virt.add(idx as usize)).next };
                idx         = desc.next;
            }
        }

        self.free_head = unsafe { (*self.desc_virt.add(idx as usize)).next };
        self.num_free -= bufs.len() as u16;

        Some(head)
    }

    /// Make a descriptor chain available to the device.
    pub fn notify_head(&mut self, head: u16) {
        let avail = unsafe { &mut *self.avail_virt };
        let pos = avail.idx as usize & (QUEUE_SIZE - 1);
        avail.ring[pos] = head;
        // Memory barrier: ensure descriptor writes are visible before idx update
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        avail.idx = avail.idx.wrapping_add(1);
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        // Kick the device (write queue index to doorbell)
        unsafe { core::ptr::write_volatile(self.notify_addr, self.queue_idx); }
    }

    /// Process used ring, returning completed descriptor head indices.
    pub fn poll_used(&mut self, completed: &mut [u16]) -> usize {
        let used = unsafe { &*self.used_virt };
        let mut count = 0;
        while self.last_used_idx != used.idx && count < completed.len() {
            let pos = self.last_used_idx as usize & (QUEUE_SIZE - 1);
            completed[count] = used.ring[pos].id as u16;
            self.last_used_idx = self.last_used_idx.wrapping_add(1);
            count += 1;
        }
        count
    }

    /// Free a descriptor chain back to the free list.
    pub fn free_chain(&mut self, head: u16) {
        let mut idx = head;
        loop {
            let desc = unsafe { &mut *self.desc_virt.add(idx as usize) };
            let has_next = desc.flags & VIRTQ_DESC_F_NEXT != 0;
            let next = desc.next;
            desc.flags = 0;
            self.num_free += 1;
            if !has_next { break; }
            idx = next;
        }
        // Prepend to free list
        unsafe { (*self.desc_virt.add(idx as usize)).next = self.free_head; }
        self.free_head = head;
    }

    /// Physical address of the descriptor table (for device config)
    pub fn desc_phys(&self) -> u64 { self.desc_phys }
    pub fn avail_phys(&self) -> u64 { self.avail_phys }
    pub fn used_phys(&self) -> u64 { self.used_phys }
}
```

---

## Phase 3: DxgkDdiSubmitCommandVirtual

This is the hot path — called for every GPU command buffer submission.

**Reference:** https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmddi/nc-d3dkmddi-dxgkddi_submitcommandvirtual

```rust
// src/ddi/submit_command.rs

pub unsafe extern "C" fn dxgkddi_submit_command_virtual(
    miniport_device_context: *mut core::ffi::c_void,
    submit_command: *const DXGKARG_SUBMITCOMMANDVIRTUAL,
) -> NTSTATUS {
    let adapter = unsafe { &mut *(miniport_device_context as *mut AdapterContext) };
    let args = unsafe { &*submit_command };
    let virtio = adapter.virtio.as_mut().unwrap();

    // The command buffer is a GPU virtual address in the process's GPU VA space.
    // It contains Venus-encoded Vulkan commands assembled by the ICD.
    //
    // For WDDM 2.0 GPU VA model, we submit the GVA directly to the
    // GPU (in our case, via virtio-gpu CMD_SUBMIT_3D).
    //
    // We store a pending submission and fire the virtqueue.
    let fence_id = args.SubmissionFenceId;
    let ctx_id   = args.hContext as u32; // STUB: extract proper ctx ID

    // Allocate a VirtioGpuCmdSubmit header in the command ring
    let cmd = crate::virtio::gpu::VirtioGpuCmdSubmit {
        hdr: crate::virtio::gpu::VirtioGpuCtrlHdr {
            type_:    crate::virtio::gpu::VIRTIO_GPU_CMD_SUBMIT_3D,
            flags:    crate::virtio::gpu::VIRTIO_GPU_FLAG_FENCE,
            fence_id: fence_id,
            ctx_id:   ctx_id,
            ring_idx: 0,
            padding:  [0; 3],
        },
        size: args.DmaBufferSize,
        padding: 0,
    };

    // Submit to virtqueue:
    // [descriptor 0] = VirtioGpuCmdSubmit header (device-readable)
    // [descriptor 1] = DMA buffer content (device-readable)
    // [descriptor 2] = Response buffer (device-writable)
    let status = virtio.submit_3d_cmd(&cmd, args.DmaBufferGpuVirtualAddress, args.DmaBufferSize);
    if status != STATUS_SUCCESS { return status; }

    STATUS_SUCCESS
}
```

---

## Phase 3: Interrupt Handling

```rust
// src/ddi/interrupt.rs

/// ISR — runs at DIRQL. Must be fast. No allocations.
pub unsafe extern "C" fn dxgkddi_interrupt_routine(
    miniport_device_context: *mut core::ffi::c_void,
    message_number: u32,
) -> bool {
    let adapter = unsafe { &mut *(miniport_device_context as *mut AdapterContext) };
    let virtio = match adapter.virtio.as_mut() { Some(v) => v, None => return false };

    // Read ISR status register — this also clears the interrupt on legacy virtio
    let isr = virtio.read_isr();
    if isr == 0 { return false; } // not our interrupt

    // Check used ring for fence completions
    let mut completed = [0u16; 32];
    let n = virtio.ctrl_queue.poll_used(&mut completed);

    for &head in &completed[..n] {
        // Each completed descriptor corresponds to one CMD_SUBMIT_3D.
        // Extract fence ID from the response buffer.
        let fence_id = virtio.get_fence_for_descriptor(head);
        if let Some(fid) = fence_id {
            // Notify Dxgkrnl of fence completion.
            // DxgkCbNotifyInterrupt must be called at DIRQL from the ISR.
            let mut interrupt = DXGKARGCB_NOTIFY_INTERRUPT_DATA::default();
            interrupt.Flags.ValidMonitoredFenceValue = 1;
            interrupt.MonitoredFenceData.NodeOrdinal     = 0;
            interrupt.MonitoredFenceData.EngineOrdinal   = 0;
            interrupt.MonitoredFenceData.FenceValueCPUVirtualAddress = core::ptr::null_mut(); // STUB
            interrupt.MonitoredFenceData.CurrentFenceValue = fid;

            unsafe {
                (adapter.dxgkrnl.as_ref().unwrap().DxgkCbNotifyInterrupt)(
                    adapter.dxgkrnl.as_ref().unwrap().DeviceHandle,
                    &interrupt,
                );
            }
        }
        virtio.ctrl_queue.free_chain(head);
    }

    // Schedule DPC to call DxgkCbQueueDpc
    // SAFETY: DxgkCbNotifyInterrupt was just called above.
    unsafe {
        (adapter.dxgkrnl.as_ref().unwrap().DxgkCbQueueDpc)(
            adapter.dxgkrnl.as_ref().unwrap().DeviceHandle,
        );
    }

    true
}

/// DPC routine — runs at DISPATCH_LEVEL after ISR
pub unsafe extern "C" fn dxgkddi_dpc_routine(
    miniport_device_context: *mut core::ffi::c_void,
) {
    // Dxgkrnl calls this after we queued a DPC via DxgkCbQueueDpc.
    // We don't need to do anything here — Dxgkrnl handles the fence signaling
    // after we called DxgkCbNotifyInterrupt in the ISR.
    let _ = miniport_device_context;
}
```

---

## Phase 3: INF File

The INF tells Windows how to install the driver and which hardware ID to match.

```ini
; helios_kmd.inx
; Generated values: %HELIOS_DRIVER_NAME%, etc. are replaced by
; the wdk-build toolchain during package creation.

[Version]
Signature   = "$Windows NT$"
Class       = Display
ClassGUID   = {4D36E968-E325-11CE-BFC1-08002BE10318}
Provider    = %ProviderName%
DriverVer   = ; auto-stamped
CatalogFile = helios_kmd.cat

[DestinationDirs]
DefaultDestDir = 12  ; %WinDir%\System32\Drivers

[SourceDisksNames]
1 = %DiskName%,,,""

[SourceDisksFiles]
helios_kmd.sys = 1,,

; ──── Manufacturer / Models ────────────────────────────────────────────────

[Manufacturer]
%ProviderName% = Helios_Models,NTamd64

[Helios_Models.NTamd64]
%DeviceDesc% = Helios_Install, PCI\VEN_1AF4&DEV_1050

; ──── Installation ─────────────────────────────────────────────────────────

[Helios_Install]
CopyFiles = Helios_CopyFiles

[Helios_Install.Services]
AddService = helios_kmd, 0x00000002, Helios_ServiceInstall

[Helios_ServiceInstall]
ServiceType   = 1                   ; SERVICE_KERNEL_DRIVER
StartType     = 3                   ; SERVICE_DEMAND_START
ErrorControl  = 0                   ; SERVICE_ERROR_IGNORE
ServiceBinary = %12%\helios_kmd.sys
LoadOrderGroup = Video

[Helios_CopyFiles]
helios_kmd.sys

; ──── Software key ─────────────────────────────────────────────────────────

[Helios_Install.SoftwareSettings]
AddReg = Helios_SoftwareSettings_Reg

[Helios_SoftwareSettings_Reg]
HKR,, InstalledDisplayDrivers, %REG_MULTI_SZ%, "helios_icd"
HKR,, Version,                 %REG_DWORD%,    1

; ──── Strings ──────────────────────────────────────────────────────────────

[Strings]
ProviderName = "Helios Project"
DeviceDesc   = "Helios vGPU Render Adapter"
DiskName     = "Helios vGPU Driver Disk"
REG_DWORD    = 0x00010001
REG_MULTI_SZ = 0x00010000
```

---

## Troubleshooting

### BSOD: DRIVER_IRQL_NOT_LESS_OR_EQUAL
Paging in a non-paged function. Mark all ISR code as `#[link_section = ".text"]` (non-pageable), and anything called from DISPATCH_LEVEL.

### BSOD: SYSTEM_SERVICE_EXCEPTION in helios_kmd
Almost always a bad pointer dereference. Check that `miniport_device_context` is your `AdapterContext`, not something shifted. Add `DbgBreakPoint()` assertions in debug builds.

### Device shows as "Unknown Device" in Device Manager
The INF hardware ID doesn't match the actual PCI ID. Verify:
```powershell
Get-PnpDevice | Where-Object { $_.HardwareID -like "*1AF4*" }
```

### `DxgkInitialize` returns `STATUS_INVALID_PARAMETER`
The `DRIVER_INITIALIZATION_DATA` version doesn't match the DDIs you've filled in. Start with the lowest WDDM version and work up.

### Virtqueue stuck / no responses
1. Check that you're posting descriptors in the right order (readable before writable)
2. Verify the notification doorbell address — wrong BAR or wrong offset will silently drop notifications
3. Check `VIRGL_DEBUG=venus` on the host to see if commands arrive
