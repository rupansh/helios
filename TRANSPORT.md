# TRANSPORT.md — VirtIO-GPU + Venus Wire Protocol

## Overview

This document describes the exact wire format used to send Vulkan commands from the Windows guest (ICD) through the KMD and virtio-gpu device to virglrenderer on the Linux host.

The stack is:
```
ICD (user-mode)   →   KMD (kernel-mode)   →   virtio-gpu device   →   virglrenderer (host)
Venus command buffer   D3DKMTEscape / DMA   Virtqueue CMD_SUBMIT_3D   Venus decoder + Vulkan
```

---

## 1. Virtio-GPU Protocol

### 1.1 Device Identification

```
PCI Vendor ID: 0x1AF4 (Red Hat)
PCI Device ID: 0x1050 (VirtIO GPU)
PCI Subsystem: 0x0028 (GPU)
```

### 1.2 PCI Capability Scanning

VirtIO Modern (1.0+) uses PCI vendor capabilities (type 9) to describe config regions.

```
PCI Config Space → scan for capability type 0x09 (vendor-specific)
For each vendor cap:
  cfg_type = cap.data[0]
  bar      = cap.data[1]
  offset   = cap.data[4..8]  (u32 LE)
  length   = cap.data[8..12] (u32 LE)
```

| cfg_type | Name | What it maps |
|----------|------|-------------|
| 1 | VIRTIO_PCI_CAP_COMMON_CFG | VirtIO common config registers |
| 2 | VIRTIO_PCI_CAP_NOTIFY_CFG | Doorbell registers (one per queue) |
| 3 | VIRTIO_PCI_CAP_ISR_CFG | Interrupt status byte |
| 4 | VIRTIO_PCI_CAP_DEVICE_CFG | GPU-specific config |
| 5 | VIRTIO_PCI_CAP_PCI_CFG | Fallback config access |

### 1.3 Device Initialization Sequence

Follow exactly the VirtIO spec §3.1.1 "Driver Requirements: Device Initialization":

```
1. Reset device: CommonCfg.device_status = 0
2. Set ACKNOWLEDGE:  CommonCfg.device_status |= 1
3. Set DRIVER:       CommonCfg.device_status |= 2
4. Read device feature bits: CommonCfg.device_feature_select = 0..N
5. Negotiate features (write driver_feature):
   REQUIRED: VIRTIO_F_VERSION_1 (bit 32) — modern virtio
   REQUIRED: VIRTIO_GPU_F_VIRGL (bit 0) — 3D/virgl support
   REQUIRED: VIRTIO_GPU_F_EDID  (bit 1) — (even if we don't use it)
   REQUIRED: VIRTIO_GPU_F_RESOURCE_UUID (bit 2) — for Venus blob tracking
   REQUIRED: VIRTIO_GPU_F_RESOURCE_BLOB (bit 3) — zero-copy blobs
   REQUIRED: VIRTIO_GPU_F_CONTEXT_INIT (bit 4) — Venus context type
   REQUIRED: VIRTIO_F_RING_RESET (bit 40) — queue reset support
6. Set FEATURES_OK: CommonCfg.device_status |= 8
7. Verify FEATURES_OK is still set (device acknowledges features)
8. Configure queues (see 1.4)
9. Set DRIVER_OK: CommonCfg.device_status |= 4
```

**Rust constants:**
```rust
pub const VIRTIO_F_VERSION_1:              u64 = 1 << 32;
pub const VIRTIO_GPU_F_VIRGL:              u64 = 1 << 0;
pub const VIRTIO_GPU_F_EDID:               u64 = 1 << 1;
pub const VIRTIO_GPU_F_RESOURCE_UUID:      u64 = 1 << 2;
pub const VIRTIO_GPU_F_RESOURCE_BLOB:      u64 = 1 << 3;
pub const VIRTIO_GPU_F_CONTEXT_INIT:       u64 = 1 << 4;
pub const VIRTIO_F_RING_RESET:             u64 = 1 << 40;
```

### 1.4 Queue Configuration

VirtIO GPU has 2 queues:

| Index | Name | Purpose |
|-------|------|---------|
| 0 | controlq | All GPU commands (3D, resource mgmt, etc.) |
| 1 | cursorq | Cursor updates (we ignore this — render-only) |

For each queue, write to CommonCfg:
```
queue_select = <queue index>    (u16)
queue_size   = <desired size>   (u16, must be power-of-2 ≤ max)
queue_desc   = <desc phys addr> (u64)
queue_driver = <avail phys addr>(u64)
queue_device = <used phys addr> (u64)
queue_msix_vector = <MSI-X vector> (u16, for MSI-X interrupt routing)
queue_enable = 1                (u16)
```

---

## 2. Venus Command Protocol

### 2.1 What is Venus?

Venus is a protocol that **serializes Vulkan API calls into a binary byte stream** which can be submitted through the virtio-gpu 3D command path. The host virglrenderer decodes this stream and replays the Vulkan calls on the physical GPU.

**Key design principle:** Venus is NOT a bytecode/shader language. It serializes the Vulkan C API struct-by-struct, pointer-by-pointer into a flat binary buffer. The host sees the same data structures the guest app would have passed to the real driver.

### 2.2 Venus Protocol Sources

The protocol is defined by codegen from `vk.xml`:

- **Protocol definition (codegen):** https://gitlab.freedesktop.org/virgl/venus-protocol
- **Host decoder (virglrenderer):** https://gitlab.freedesktop.org/virgl/virglrenderer/-/tree/master/src/venus
- **Linux guest encoder (Mesa):** https://gitlab.freedesktop.org/mesa/mesa/-/tree/main/src/virtio/vulkan

You MUST use compatible encoding with virglrenderer's decoder. The source of truth is `venus-protocol/`. Clone it and run the codegen to get the actual field layouts.

### 2.3 Venus Command Ring Buffer

The Venus protocol uses a ring buffer in shared memory (a virtio-gpu blob resource) for submitting commands. This avoids the overhead of individual virtqueue operations per Vulkan call.

```
┌────────────────────────────────────┐  ← shared blob resource (hostmem)
│  Ring header (32 bytes)            │
│  ┌──────────────────────────────┐  │
│  │ shmem_size: u32              │  │
│  │ ring_size: u32               │  │
│  │ producer_index: u32          │  │  ← written by guest (ICD)
│  │ consumer_index: u32          │  │  ← written by host (virglrenderer)
│  │ status: u32                  │  │
│  └──────────────────────────────┘  │
│  Ring data (variable)              │
│  ┌──────────────────────────────┐  │
│  │ [Venus commands ...]         │  │
│  └──────────────────────────────┘  │
└────────────────────────────────────┘
```

The ring is a classic single-producer single-consumer ring:
- Guest writes Venus commands starting at `data + (producer_index % ring_size)`
- Guest advances `producer_index`
- Host reads from `data + (consumer_index % ring_size)`, advances `consumer_index`

### 2.4 Venus Command Encoding

Each Venus command has the form:
```
[VnCommandTypeId: u16][VnCommandFlags: u16][command-specific fields ...]
```

Command IDs map to Vulkan functions. Example for `vkCreateBuffer`:

```
Offset  Size  Field
──────  ────  ─────────────────────────────────────────────────────────
0       2     VN_CMD_vkCreateBuffer (= 0x0041, from codegen)
2       2     flags (0 = synchronous)
4       8     device: VkDevice (u64 opaque handle)
12      4     pCreateInfo ptr (inline or offset)
...           [VkBufferCreateInfo fields inlined]
...     8     pAllocator ptr (null = 0)
...     8     pBuffer ptr (= inline response area offset)
```

**Inlining rule:** All pointers in Vulkan structs are inlined (dereferenced and their content embedded) into the Venus command stream. There are no raw pointer values — only offsets or NULLs.

**Handles:** Vulkan object handles (VkDevice, VkBuffer, etc.) are represented as 64-bit integers in the Venus stream. These are **host-side opaque handles** assigned by virglrenderer, not the guest's pointers.

### 2.5 Venus Context Creation

Before sending any Venus commands, create a Venus context:

```rust
// Guest side: send VIRTIO_GPU_CMD_CTX_CREATE via the control virtqueue
let mut cmd = VirtioGpuCtxCreate::zeroed();
cmd.hdr.type_ = VIRTIO_GPU_CMD_CTX_CREATE;
cmd.hdr.ctx_id = ctx_id;     // guest-assigned context ID (1..=0xFFFFFFFE)
cmd.context_init = VIRTIO_GPU_CAPSET_VENUS;  // = 4
cmd.nlen = 0;  // no debug name

// Send via ctrl virtqueue, wait for VIRTIO_GPU_RESP_OK_NODATA
virtio.send_cmd_sync(&cmd, &mut response)?;
```

The `context_init` field with value 4 (VENUS) tells virglrenderer to create a Venus context type, not a VirGL OpenGL context.

### 2.6 Venus Ring Setup (after context creation)

```rust
// 1. Create a blob resource for the command ring
let ring_size: u64 = 1024 * 1024; // 1 MB ring

let mut create_blob = VirtioGpuResourceCreateBlob::zeroed();
create_blob.hdr.type_ = VIRTIO_GPU_CMD_RESOURCE_CREATE_BLOB;
create_blob.hdr.ctx_id = ctx_id;
create_blob.resource_id = new_resource_id();
create_blob.blob_mem    = VIRTIO_GPU_BLOB_MEM_HOST3D;
create_blob.blob_flags  = VIRTIO_GPU_BLOB_FLAG_USE_MAPPABLE;
create_blob.blob_id     = 0;  // host picks the blob
create_blob.size        = ring_size;
create_blob.nr_entries  = 0;

virtio.send_cmd_sync(&create_blob, &mut response)?;

// 2. Map the blob into guest GPA space
// (The KMD maps this into the aperture segment for the ICD to write to)
let mut map_blob = VirtioGpuResourceMapBlob::zeroed();
map_blob.hdr.type_    = VIRTIO_GPU_CMD_RESOURCE_MAP_BLOB;
map_blob.hdr.ctx_id   = ctx_id;
map_blob.resource_id  = create_blob.resource_id;
map_blob.offset       = 0;  // map from start

virtio.send_cmd_sync(&map_blob, &mut map_response)?;
// map_response contains the GPA where the blob is mapped in guest physical memory

// 3. Initialize the ring header
let ring_header_ptr = map_response.offset as *mut VenusRingHeader;
unsafe {
    (*ring_header_ptr).shmem_size   = ring_size as u32;
    (*ring_header_ptr).ring_size    = (ring_size - 32) as u32;
    (*ring_header_ptr).producer_index = 0;
    (*ring_header_ptr).consumer_index = 0;
    (*ring_header_ptr).status       = 0;
}
```

### 2.7 Submitting Venus Commands

Once the ring is set up, the ICD writes commands into the ring and submits via:

```rust
// Option A: Ring-based (preferred, lower overhead)
// Write Venus bytes directly to the ring buffer (shared memory).
// Then kick the device with VIRTIO_GPU_CMD_SUBMIT_3D, size=0
// to notify virglrenderer of new ring data.

// Option B: DMA buffer submit (for large/one-shot commands)
// Put Venus bytes in the DMA buffer, submit via CMD_SUBMIT_3D with size>0.
```

**For our driver, use Option B initially (simpler), then optimize to Option A.**

```rust
// Submitting a Venus command buffer via the KMD (from the ICD via D3DKMTEscape):

// ICD prepares a Venus-encoded buffer:
let mut buf = Vec::<u8>::new();
venus_encode_vkCreateInstance(&mut buf, &create_info);

// ICD calls D3DKMTEscape to hand the buffer to the KMD:
let escape_in = HeliosEscapeSubmitVenus {
    cmd_type: HELIOS_ESCAPE_SUBMIT_VENUS,
    ctx_id: ctx_id,
    buffer_ptr: buf.as_ptr() as u64,
    buffer_size: buf.len() as u32,
    fence_id: next_fence_id(),
};
D3DKMTEscape(&escape_params)?;

// KMD receives DxgkDdiEscape, builds VirtioGpuCmdSubmit, sends via ctrl virtqueue.
// virglrenderer decodes Venus bytes, calls vkCreateInstance on host GPU.
// Fence completion comes back via interrupt → DxgkCbNotifyInterrupt.
```

---

## 3. ICD ↔ KMD Communication

### 3.1 D3DKMTEscape Protocol

D3DKMTEscape is the standard WDDM mechanism for user-mode → kernel-mode out-of-band communication. The ICD uses this for:
- Submitting Venus command buffers
- Creating/destroying Venus contexts
- Memory mapping requests
- Fence wait queries

```rust
// Escape command codes (shared between ICD and KMD)
pub const HELIOS_ESCAPE_SUBMIT_VENUS:      u32 = 0x0001;
pub const HELIOS_ESCAPE_CTX_CREATE:        u32 = 0x0002;
pub const HELIOS_ESCAPE_CTX_DESTROY:       u32 = 0x0003;
pub const HELIOS_ESCAPE_ALLOC_BLOB:        u32 = 0x0004;
pub const HELIOS_ESCAPE_MAP_BLOB:          u32 = 0x0005;
pub const HELIOS_ESCAPE_WAIT_FENCE:        u32 = 0x0006;

/// Header for all escape commands
#[repr(C)]
pub struct HeliosEscapeHeader {
    pub magic:    u32,    // 0x48454C53 ('HELS')
    pub cmd_type: u32,
    pub version:  u32,    // = 1
    pub size:     u32,    // total escape buffer size
}

/// HELIOS_ESCAPE_SUBMIT_VENUS payload
#[repr(C)]
pub struct HeliosEscapeSubmitVenus {
    pub hdr:          HeliosEscapeHeader,
    pub ctx_id:       u32,
    pub fence_id:     u64,
    pub buffer_size:  u32,
    pub pad:          u32,
    // Followed by `buffer_size` bytes of Venus commands
}
```

### 3.2 KMD DxgkDdiEscape Handler

```rust
// src/ddi/escape.rs

pub unsafe extern "C" fn dxgkddi_escape(
    miniport_device_context: *mut core::ffi::c_void,
    escape: *const DXGKARG_ESCAPE,
) -> NTSTATUS {
    let adapter = unsafe { &mut *(miniport_device_context as *mut AdapterContext) };
    let args = unsafe { &*escape };

    // Validate: must be from a trusted process (PrivilegedEscape = false means user-mode)
    if args.pPrivateDriverData.is_null() || args.PrivateDriverDataSize < 16 {
        return STATUS_INVALID_PARAMETER;
    }

    let hdr = unsafe { &*(args.pPrivateDriverData as *const HeliosEscapeHeader) };

    // Validate magic
    if hdr.magic != 0x48454C53 { return STATUS_INVALID_PARAMETER; }
    if hdr.size as usize > args.PrivateDriverDataSize as usize { return STATUS_INVALID_PARAMETER; }

    match hdr.cmd_type {
        HELIOS_ESCAPE_SUBMIT_VENUS => escape_submit_venus(adapter, args),
        HELIOS_ESCAPE_CTX_CREATE   => escape_ctx_create(adapter, args),
        HELIOS_ESCAPE_CTX_DESTROY  => escape_ctx_destroy(adapter, args),
        HELIOS_ESCAPE_ALLOC_BLOB   => escape_alloc_blob(adapter, args),
        HELIOS_ESCAPE_MAP_BLOB     => escape_map_blob(adapter, args),
        _ => STATUS_NOT_SUPPORTED,
    }
}

fn escape_submit_venus(adapter: &mut AdapterContext, args: &DXGKARG_ESCAPE) -> NTSTATUS {
    let payload = unsafe { &*(args.pPrivateDriverData as *const HeliosEscapeSubmitVenus) };

    // Validate sizes
    let data_offset = core::mem::size_of::<HeliosEscapeSubmitVenus>();
    let expected_total = data_offset + payload.buffer_size as usize;
    if expected_total > args.PrivateDriverDataSize as usize {
        return STATUS_INVALID_PARAMETER;
    }

    // Pointer to Venus command bytes (in user-mode process memory, but
    // pPrivateDriverData is kernel-accessible during the escape call).
    let venus_data = unsafe {
        core::slice::from_raw_parts(
            (args.pPrivateDriverData as *const u8).add(data_offset),
            payload.buffer_size as usize,
        )
    };

    // Submit to virglrenderer via virtqueue
    let virtio = adapter.virtio.as_mut().unwrap();
    virtio.submit_venus_cmd(payload.ctx_id, payload.fence_id, venus_data)
}
```

---

## 4. Venus Command Encoding (Rust)

The Venus encoding is mechanically generated from `vk.xml` in the real Mesa implementation. For our Rust implementation, we will write the encoder manually for the subset of Vulkan we need, cross-referencing the virglrenderer decoder source.

### 4.1 Encoding Primitives

```rust
// src/icd/venus/encode.rs

pub struct VnEncoder {
    buf: Vec<u8>,
}

impl VnEncoder {
    pub fn new() -> Self { Self { buf: Vec::with_capacity(4096) } }

    pub fn write_u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    pub fn write_u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    pub fn write_i32(&mut self, v: i32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    pub fn write_handle(&mut self, h: u64) {
        // Venus handles are u64 (64-bit opaque)
        self.write_u64(h);
    }
    pub fn write_pointer_opt<T>(&mut self, ptr: Option<&T>, write_fn: impl Fn(&mut Self, &T)) {
        if let Some(v) = ptr {
            self.write_u32(1); // non-null
            write_fn(self, v);
        } else {
            self.write_u32(0); // null
        }
    }
    pub fn finish(self) -> Vec<u8> { self.buf }
}

// Venus command IDs (from virglrenderer/src/venus/vn_protocol_driver_info.h)
// These must EXACTLY match what virglrenderer expects.
// Generate from: venus-protocol repo's codegen output
pub const VN_CMD_vkCreateInstance:     u16 = 0x0000;
pub const VN_CMD_vkDestroyInstance:    u16 = 0x0001;
pub const VN_CMD_vkEnumeratePhysicalDevices: u16 = 0x0002;
pub const VN_CMD_vkCreateDevice:       u16 = 0x000B;
// ... (full list from venus-protocol codegen)

/// Encode a Venus command header
pub fn encode_cmd_header(enc: &mut VnEncoder, cmd_id: u16) {
    enc.write_u32(cmd_id as u32);  // type (upper 16 bits = 0 for standard cmds)
}

/// Example: encode vkCreateInstance
pub fn encode_vkCreateInstance(
    enc: &mut VnEncoder,
    p_create_info: &VkInstanceCreateInfo,
    // p_allocator: always NULL for Venus
) {
    encode_cmd_header(enc, VN_CMD_vkCreateInstance);
    encode_VkInstanceCreateInfo(enc, p_create_info);
    enc.write_u32(0); // pAllocator = NULL
    // Response area: Venus will write the VkInstance handle here
    // (handled via the response ring, not inline)
}

fn encode_VkInstanceCreateInfo(enc: &mut VnEncoder, info: &VkInstanceCreateInfo) {
    enc.write_u32(info.s_type as u32);
    enc.write_u32(0); // pNext = NULL (simplified)
    enc.write_u32(info.flags);
    // pApplicationInfo
    enc.write_pointer_opt(info.p_application_info.as_ref(), |enc, app| {
        enc.write_u32(app.s_type as u32);
        enc.write_u32(0); // pNext
        // pApplicationName: encode as length-prefixed string
        encode_string_opt(enc, app.p_application_name);
        enc.write_u32(app.application_version);
        encode_string_opt(enc, app.p_engine_name);
        enc.write_u32(app.engine_version);
        enc.write_u32(app.api_version);
    });
    // ppEnabledLayerNames
    enc.write_u32(info.enabled_layer_count);
    for i in 0..info.enabled_layer_count as usize {
        let name = unsafe { core::ffi::CStr::from_ptr(info.pp_enabled_layer_names.add(i).read()) };
        encode_string(enc, name.to_bytes());
    }
    // ppEnabledExtensionNames
    enc.write_u32(info.enabled_extension_count);
    for i in 0..info.enabled_extension_count as usize {
        let name = unsafe { core::ffi::CStr::from_ptr(info.pp_enabled_extension_names.add(i).read()) };
        encode_string(enc, name.to_bytes());
    }
}

fn encode_string(enc: &mut VnEncoder, s: &[u8]) {
    enc.write_u32(s.len() as u32 + 1);  // length including null
    enc.buf.extend_from_slice(s);
    enc.buf.push(0);  // null terminator
    // align to 4 bytes
    while enc.buf.len() % 4 != 0 { enc.buf.push(0); }
}

fn encode_string_opt(enc: &mut VnEncoder, ptr: *const i8) {
    if ptr.is_null() {
        enc.write_u32(0);
    } else {
        let s = unsafe { core::ffi::CStr::from_ptr(ptr) };
        encode_string(enc, s.to_bytes());
    }
}
```

**⚠️ CRITICAL:** The encoding above is a sketch. The actual Venus encoding is generated from `vk.xml` by the venus-protocol codegen. You MUST cross-reference `virglrenderer/src/venus/vn_protocol_driver_*.h` to get the exact field order and sizes. Any mismatch will result in virglrenderer silently producing garbage or crashing.

### 4.2 Getting the Actual Command Encoding

```bash
# On Linux (build machine):
git clone https://gitlab.freedesktop.org/virgl/venus-protocol.git
cd venus-protocol
# The headers in include/ show the exact encoding per-command
ls include/
# vn_protocol_driver_info.h  — command IDs
# vn_protocol_driver_*.h    — per-type encoders
```

For each Vulkan function you implement, look at:
- `include/vn_protocol_driver_<vulkan_type>.h` in venus-protocol
- `src/venus/vn_protocol_renderer_<type>.h` in virglrenderer (decoder side)

---

## 5. Synchronization and Fencing

### 5.1 Fence Flow

```
Guest ICD                  KMD                    virglrenderer
─────────                  ───                    ─────────────
Submit Venus cmd ─────►  DxgkDdiEscape()
                         submit via virtqueue ──► decode + execute Vulkan
                                                 write fence completion
                         ◄── interrupt            ◄── used ring entry
DxgkCbNotifyInterrupt
→ fence signaled
ICD unblocks ◄────────── monitored fence value updated
```

### 5.2 Fence Encoding in virtio-gpu

Every command with `VIRTIO_GPU_FLAG_FENCE` set causes virglrenderer to:
1. Process the command
2. Write a `VIRTIO_GPU_RESP_OK_NODATA` response with the same `fence_id`
3. Mark the descriptor as used in the used ring

The KMD checks the used ring in the ISR and calls `DxgkCbNotifyInterrupt` with the fence value.

### 5.3 Monitored Fence Setup (WDDM 2.0)

```rust
// When creating a GPU context, set up a monitored fence:
let mut create_sync = DXGKARG_CREATESYNCHRONIZATIONOBJECT2::default();
create_sync.Info.Type = DXGK_SYNCHRONIZATIONOBJECT_TYPE::MonitoredFence;
create_sync.Info.MonitoredFence.InitialFenceValue = 0;
// ... Dxgkrnl fills in FenceValueCPUVirtualAddress and FenceValueGPUVirtualAddress
```

---

## 6. Resource Management

### 6.1 Virtio-GPU Resource IDs

Every GPU buffer/image in the guest is backed by a virtio-gpu **resource ID** (u32). The lifecycle:

```
ICD: vkCreateBuffer()
  → ICD calls DxgkDdiCreateAllocation (via D3D runtime)
  → KMD assigns resource_id (monotonically increasing u32)
  → KMD sends VIRTIO_GPU_CMD_RESOURCE_CREATE_BLOB (for host memory)
  → KMD sends VIRTIO_GPU_CMD_CTX_ATTACH_RESOURCE (attach to Venus ctx)
  → Returns resource handle to ICD

ICD: vkDestroyBuffer()
  → ICD calls DxgkDdiDestroyAllocation
  → KMD sends VIRTIO_GPU_CMD_CTX_DETACH_RESOURCE
  → KMD sends VIRTIO_GPU_CMD_RESOURCE_UNREF
```

### 6.2 Blob Resource Memory

For Vulkan memory objects (`VkDeviceMemory`), we use blob resources with `VIRTIO_GPU_BLOB_MEM_HOST3D`:

```rust
let mut cmd = VirtioGpuResourceCreateBlob::zeroed();
cmd.hdr.type_   = VIRTIO_GPU_CMD_RESOURCE_CREATE_BLOB;
cmd.hdr.ctx_id  = ctx_id;
cmd.resource_id = new_resource_id();
cmd.blob_mem    = VIRTIO_GPU_BLOB_MEM_HOST3D;
cmd.blob_flags  = VIRTIO_GPU_BLOB_FLAG_USE_MAPPABLE
                | VIRTIO_GPU_BLOB_FLAG_USE_SHAREABLE;
cmd.blob_id     = 0;  // virglrenderer assigns (set via Venus VkDeviceMemory handle)
cmd.size        = allocation_size;
cmd.nr_entries  = 0;
```

The `blob_id` field links the blob resource to a Venus `VkDeviceMemory` object that was previously created via the Venus command stream. The mapping between Venus handles and blob IDs is managed by virglrenderer internally.

---

## 7. Reference Implementations & Reusable Patterns

The closest prior art is **[tenclass/mvisor-win-vgpu-driver](https://github.com/tenclass/mvisor-win-vgpu-driver)** (GPLv3, C) — a Windows guest GPU driver over a virtio-gpu-style device. It is **architecturally different** from Helios: a generic WDF/KMDF device exposing DRM-virtgpu `DeviceIoControl`, driving a Mesa **virgl OpenGL** ICD (its own `opengl32.dll`). It is OpenGL/virgl (not Vulkan/Venus) and bypasses WDDM entirely (no Dxgkrnl/D3DKMT). But the layer *beneath* the Helios KMD — the virtio-gpu 3D transport — is nearly identical, so it's a strong reference for Phases 2–4. Patterns worth adopting:

- **Two-descriptor `SUBMIT_3D`** (Phase 4): a small *mutable* `virtio_gpu_cmd_submit` header in descriptor 0, and the large command body passed **by physical address** in descriptor 1, pointing into a pre-reserved page-aligned slice of a contiguous pool. Avoids per-submit allocation; keeps the fence-flag header writable.
- **Bitmap page sub-allocator** (Phase 3): one big `MmAllocateContiguousMemory` region sub-allocated with an `RtlBitmap` (page-granular, last-free-index hint), instead of many per-allocation contiguous allocs.
- **Host-visible blob mapping** (Phase 3): `RESOURCE_CREATE_BLOB(HOST3D)` → `RESOURCE_MAP_BLOB` → read `virtio_gpu_resp_map_info { map_info, gpa, size }` → `MmMapIoSpaceEx(gpa, …)` using the **cache mode the host returns in `map_info`** (CACHED / WC / UNCACHED). That `map_info` byte is how the aperture segment should choose `PAGE_WRITECOMBINE` vs cached — the spec-sanctioned coherent-host-memory path.
- **Capset / `context_init` negotiation** (Phase 4): `GET_CAPSET_INFO`/`GET_CAPSET` + a supported-capset bitmask + `CTX_CREATE` with `context_init` = capset id. Identical in shape for Venus — use `VIRTIO_GPU_CAPSET_VENUS` and parse the Venus caps.
- **ISR/DPC split** (Phase 2): the ISR only reads ISR/MSI status and queues a DPC; the DPC drains the used ring, dispatches on `hdr.type`, frees command buffers, and signals fences — per-queue spinlock + MSI-X message→queue map. Enforces the invariant *free descriptors only on used-ring completion*.
- **Interim fences** (before WDDM monitored fences): a `fence_id → KEVENT` map signaled from the DPC + a blocking wait.

**Gotchas:**
- mvisor indexes queues **COMMAND=0 / CONTROL=1** — *opposite* of standard virtio-gpu (controlq=0). Helios uses the **standard** virtio-gpu layout (control queue = 0). Do not copy mvisor's ordering.
- mvisor invents a **custom** virtio device (`DEV_105B`, custom config struct); Helios uses the **standard** virtio-gpu device (`0x1050`) with standard feature/config negotiation.

**Other prior art:** [Keenuts/virtio-gpu-win-icd](https://github.com/Keenuts/virtio-gpu-win-icd), [kjliew/qemu-3dfx](https://github.com/kjliew/qemu-3dfx).

**Strategic fallback:** mvisor shows a *simpler* architecture that works — ship your own ICD over a private WDF device and skip WDDM (the qemu-3dfx model). Helios's WDDM + Vulkan-loader + DXVK/VKD3D path is more integrated and supports D3D11/12 (not just GL), which is the mandate — but the WDF+own-ICD approach is a proven fallback if the WDDM render path proves intractable.
