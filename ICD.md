# ICD.md — Vulkan ICD Implementation Guide

## Overview

The Helios ICD (`helios_icd.dll`) is a **Vulkan Installable Client Driver** — a DLL that the Vulkan loader (`vulkan-1.dll`) loads when an application calls `vkCreateInstance`. The ICD implements the Vulkan API by encoding calls into the Venus protocol and submitting them to the KMD.

DXVK (for D3D11) and VKD3D-Proton (for D3D12) will call into this ICD as if it were a native Vulkan driver.

**References:**
- Vulkan Loader Interface: https://github.com/KhronosGroup/Vulkan-Loader/blob/main/docs/LoaderDriverInterface.md
- Vulkan ICD spec: https://vulkan.lunarg.com/doc/sdk/latest/windows/loader_driver_interface.html
- ash (Vulkan Rust bindings): https://docs.rs/ash/latest/ash/

---

## 1. Vulkan ICD Entry Points

The Vulkan loader identifies ICDs via a JSON manifest file and loads the DLL, then calls the negotiation function.

### 1.1 ICD Manifest (`helios_vulkan.json`)

```json
{
    "file_format_version": "1.0.0",
    "ICD": {
        "library_path": ".\\helios_icd.dll",
        "api_version": "1.3.0"
    }
}
```

Register this manifest in the Windows registry:
```
HKLM\SOFTWARE\Khronos\Vulkan\Drivers\
  "C:\Windows\System32\helios_vulkan.json" = DWORD:0
```

Or via the INF's `AddReg` section (KMD handles this on install).

### 1.2 Required ICD Exports

```rust
// src/lib.rs — ICD entry points

// These functions MUST be exported with these exact names.
// The Vulkan loader calls them by name.

/// Vulkan loader negotiation (called first)
#[no_mangle]
pub extern "C" fn vk_icdNegotiateLoaderICDInterfaceVersion(
    p_supported_version: *mut u32,
) -> VkResult {
    // We support loader interface version 5 (Vulkan 1.1+)
    // https://github.com/KhronosGroup/Vulkan-Loader/blob/main/docs/LoaderDriverInterface.md#icd-interface-versions
    const HELIOS_ICD_INTERFACE_VERSION: u32 = 5;
    unsafe {
        if *p_supported_version > HELIOS_ICD_INTERFACE_VERSION {
            *p_supported_version = HELIOS_ICD_INTERFACE_VERSION;
        }
    }
    VkResult::SUCCESS
}

/// Get instance-level proc addresses
#[no_mangle]
pub extern "C" fn vk_icdGetInstanceProcAddr(
    instance: VkInstance,
    p_name: *const i8,
) -> PFN_vkVoidFunction {
    let name = unsafe { std::ffi::CStr::from_ptr(p_name) };
    dispatch::get_instance_proc_addr(instance, name)
}

/// Get physical device proc addresses (loader interface v4+)
#[no_mangle]
pub extern "C" fn vk_icdGetPhysicalDeviceProcAddr(
    instance: VkInstance,
    p_name: *const i8,
) -> PFN_vkVoidFunction {
    let name = unsafe { std::ffi::CStr::from_ptr(p_name) };
    dispatch::get_physical_device_proc_addr(instance, name)
}
```

### 1.3 Dispatch Table

```rust
// src/dispatch.rs

pub fn get_instance_proc_addr(
    _instance: VkInstance,
    name: &std::ffi::CStr,
) -> PFN_vkVoidFunction {
    match name.to_bytes() {
        b"vkCreateInstance"          => Some(unsafe { std::mem::transmute(instance::vk_create_instance as *const ()) }),
        b"vkDestroyInstance"         => Some(unsafe { std::mem::transmute(instance::vk_destroy_instance as *const ()) }),
        b"vkEnumeratePhysicalDevices"=> Some(unsafe { std::mem::transmute(instance::vk_enumerate_physical_devices as *const ()) }),
        b"vkGetPhysicalDeviceProperties" => Some(unsafe { std::mem::transmute(phys_device::vk_get_physical_device_properties as *const ()) }),
        b"vkGetPhysicalDeviceFeatures"   => Some(unsafe { std::mem::transmute(phys_device::vk_get_physical_device_features as *const ()) }),
        b"vkCreateDevice"            => Some(unsafe { std::mem::transmute(device::vk_create_device as *const ()) }),
        b"vkDestroyDevice"           => Some(unsafe { std::mem::transmute(device::vk_destroy_device as *const ()) }),
        b"vkGetDeviceProcAddr"       => Some(unsafe { std::mem::transmute(get_device_proc_addr as *const ()) }),
        // ... (all Vulkan functions we implement)
        _ => None,
    }
}
```

---

## 2. Instance Creation

### 2.1 `vkCreateInstance`

```rust
// src/instance.rs

pub extern "C" fn vk_create_instance(
    p_create_info: *const VkInstanceCreateInfo,
    _p_allocator: *const VkAllocationCallbacks,
    p_instance: *mut VkInstance,
) -> VkResult {
    let create_info = unsafe { &*p_create_info };

    // 1. Open the KMD device via D3DKMT
    let kmd = match KmdConnection::open() {
        Ok(k) => k,
        Err(_) => return VkResult::ERROR_INITIALIZATION_FAILED,
    };

    // 2. Create a Venus context in the KMD
    let ctx_id = kmd.create_venus_context()
        .map_err(|_| VkResult::ERROR_INITIALIZATION_FAILED)?;

    // 3. Encode and submit Venus vkCreateInstance
    let mut enc = VnEncoder::new();
    encode_vkCreateInstance(&mut enc, create_info);
    let cmd_buf = enc.finish();

    let fence_id = kmd.next_fence();
    kmd.submit_venus(ctx_id, fence_id, &cmd_buf)
        .map_err(|_| VkResult::ERROR_INITIALIZATION_FAILED)?;

    // 4. Wait for completion and read back the VkInstance handle
    let instance_handle = kmd.wait_and_read_handle(fence_id)
        .map_err(|_| VkResult::ERROR_INITIALIZATION_FAILED)?;

    // 5. Allocate our InstanceState, store the Venus handle
    let state = Box::new(InstanceState {
        kmd,
        ctx_id,
        venus_handle: instance_handle,
    });

    // 6. Return an opaque pointer as VkInstance
    // VkInstance is a non-dispatchable handle (u64 on 64-bit)
    unsafe { *p_instance = Box::into_raw(state) as VkInstance; }

    VkResult::SUCCESS
}
```

### 2.2 KMD Connection

```rust
// src/transport.rs — D3DKMT communication

// NOTE: the D3DKMT* thunks live under windows::Wdk (kernel-adjacent), NOT Win32.
// Enable the `Wdk_Graphics_Direct3D` cargo feature of the `windows` crate.
use windows::Wdk::Graphics::Direct3D::*;

pub struct KmdConnection {
    adapter_handle: D3DKMT_HANDLE,
    device_handle:  D3DKMT_HANDLE,
    fence_counter:  std::sync::atomic::AtomicU64,
}

impl KmdConnection {
    pub fn open() -> Result<Self, KmdError> {
        // 1. Enumerate adapters to find our VEN_1AF4&DEV_1050 adapter
        let adapter_handle = find_helios_adapter()?;

        // 2. Open a device on that adapter
        let mut open_adapter = D3DKMT_OPENADAPTERFROMLUID::default();
        // ... fill in adapter LUID from enumeration

        // 3. Create a D3DKMT device
        let mut create_device = D3DKMT_CREATEDEVICE::default();
        create_device.hAdapter = adapter_handle;
        unsafe { D3DKMTCreateDevice(&mut create_device) }?;

        Ok(Self {
            adapter_handle,
            device_handle: create_device.hDevice,
            fence_counter: std::sync::atomic::AtomicU64::new(1),
        })
    }

    pub fn submit_venus(
        &self,
        ctx_id: u32,
        fence_id: u64,
        venus_data: &[u8],
    ) -> Result<(), KmdError> {
        // Build escape buffer: header + payload + venus data
        let payload_size = core::mem::size_of::<HeliosEscapeSubmitVenus>() + venus_data.len();
        let mut escape_buf = vec![0u8; payload_size];

        let header = unsafe {
            &mut *(escape_buf.as_mut_ptr() as *mut HeliosEscapeSubmitVenus)
        };
        header.hdr.magic    = 0x48454C53;
        header.hdr.cmd_type = HELIOS_ESCAPE_SUBMIT_VENUS;
        header.hdr.version  = 1;
        header.hdr.size     = payload_size as u32;
        header.ctx_id       = ctx_id;
        header.fence_id     = fence_id;
        header.buffer_size  = venus_data.len() as u32;

        let data_offset = core::mem::size_of::<HeliosEscapeSubmitVenus>();
        escape_buf[data_offset..].copy_from_slice(venus_data);

        // Call D3DKMTEscape
        let mut escape = D3DKMT_ESCAPE::default();
        escape.hAdapter = self.adapter_handle;
        escape.hDevice  = self.device_handle;
        escape.Type     = D3DKMT_ESCAPETYPE::D3DKMT_ESCAPE_DRIVERPRIVATE;
        escape.pPrivateDriverData     = escape_buf.as_mut_ptr() as *mut _;
        escape.PrivateDriverDataSize  = escape_buf.len() as u32;

        unsafe { D3DKMTEscape(&escape) }?;
        Ok(())
    }

    pub fn next_fence(&self) -> u64 {
        self.fence_counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }

    pub fn wait_fence(&self, fence_id: u64, timeout_ns: u64) -> Result<(), KmdError> {
        // Use D3DKMTWaitForSynchronizationObjectFromCpu or
        // poll monitored fence value (mapped user-mode VA)
        // STUB: implement proper wait
        std::thread::sleep(std::time::Duration::from_micros(100));
        Ok(())
    }
}
```

---

## 3. Physical Device Enumeration

```rust
// src/phys_device.rs

pub extern "C" fn vk_enumerate_physical_devices(
    instance: VkInstance,
    p_physical_device_count: *mut u32,
    p_physical_devices: *mut VkPhysicalDevice,
) -> VkResult {
    let state = unsafe { &mut *(instance as *mut InstanceState) };

    // Query from host via Venus: vkEnumeratePhysicalDevices
    let mut enc = VnEncoder::new();
    encode_vkEnumeratePhysicalDevices(&mut enc, state.venus_handle, unsafe { *p_physical_device_count });
    let cmd_buf = enc.finish();

    let fence_id = state.kmd.next_fence();
    state.kmd.submit_venus(state.ctx_id, fence_id, &cmd_buf).ok()?;
    
    // Read response: [u32 count, u64 handles...]
    let response = state.kmd.wait_and_read_response(fence_id).ok()?;
    let count = u32::from_le_bytes(response[0..4].try_into().unwrap());

    if p_physical_devices.is_null() {
        unsafe { *p_physical_device_count = count; }
        return VkResult::SUCCESS;
    }

    let out_count = unsafe { *p_physical_device_count }.min(count);
    for i in 0..out_count as usize {
        let venus_phys = u64::from_le_bytes(response[4 + i*8..4 + i*8 + 8].try_into().unwrap());
        // Wrap Venus physical device handle in our PhysDevState
        let phys = Box::new(PhysDevState {
            venus_handle: venus_phys,
            instance_state: state as *mut _,
        });
        unsafe { *p_physical_devices.add(i) = Box::into_raw(phys) as VkPhysicalDevice; }
    }
    unsafe { *p_physical_device_count = out_count; }

    if out_count < count { VkResult::INCOMPLETE } else { VkResult::SUCCESS }
}
```

---

## 4. Implementation Order for Vulkan Functions

Implement in this order (each enables more test coverage):

### Tier 1: Minimum viable (`vulkaninfo` passes)
- [ ] `vkCreateInstance` / `vkDestroyInstance`
- [ ] `vkEnumeratePhysicalDevices`
- [ ] `vkGetPhysicalDeviceProperties` / `vkGetPhysicalDeviceProperties2`
- [ ] `vkGetPhysicalDeviceFeatures` / `vkGetPhysicalDeviceFeatures2`
- [ ] `vkGetPhysicalDeviceQueueFamilyProperties`
- [ ] `vkGetPhysicalDeviceMemoryProperties`
- [ ] `vkGetPhysicalDeviceFormatProperties`
- [ ] `vkEnumerateDeviceExtensionProperties`
- [ ] `vkEnumerateInstanceExtensionProperties`

### Tier 2: Device creation (`vkcube` passes)
- [ ] `vkCreateDevice` / `vkDestroyDevice`
- [ ] `vkGetDeviceQueue`
- [ ] `vkCreateCommandPool` / `vkDestroyCommandPool`
- [ ] `vkAllocateCommandBuffers` / `vkFreeCommandBuffers`
- [ ] `vkBeginCommandBuffer` / `vkEndCommandBuffer`
- [ ] `vkQueueSubmit` / `vkQueueWaitIdle`
- [ ] `vkCreateFence` / `vkDestroyFence` / `vkWaitForFences` / `vkResetFences`
- [ ] `vkCreateSemaphore` / `vkDestroySemaphore`

### Tier 3: Memory (`vkAllocateMemory` chain)
- [ ] `vkAllocateMemory` / `vkFreeMemory`
- [ ] `vkMapMemory` / `vkUnmapMemory`
- [ ] `vkFlushMappedMemoryRanges` / `vkInvalidateMappedMemoryRanges`
- [ ] `vkCreateBuffer` / `vkDestroyBuffer`
- [ ] `vkGetBufferMemoryRequirements`
- [ ] `vkBindBufferMemory`
- [ ] `vkCreateImage` / `vkDestroyImage`
- [ ] `vkGetImageMemoryRequirements`
- [ ] `vkBindImageMemory`

### Tier 4: Rendering (DXVK works)
- [ ] `vkCreateRenderPass` / `vkDestroyRenderPass`
- [ ] `vkCreateFramebuffer` / `vkDestroyFramebuffer`
- [ ] `vkCreateShaderModule` / `vkDestroyShaderModule`
- [ ] `vkCreateGraphicsPipelines` / `vkDestroyPipeline`
- [ ] `vkCreatePipelineLayout` / `vkDestroyPipelineLayout`
- [ ] `vkCreateDescriptorSetLayout` / `vkDestroyDescriptorSetLayout`
- [ ] `vkCreateDescriptorPool` / `vkDestroyDescriptorPool`
- [ ] `vkAllocateDescriptorSets` / `vkFreeDescriptorSets`
- [ ] `vkUpdateDescriptorSets`
- [ ] `vkCmdBeginRenderPass` / `vkCmdEndRenderPass`
- [ ] `vkCmdBindPipeline`
- [ ] `vkCmdBindVertexBuffers` / `vkCmdBindIndexBuffer`
- [ ] `vkCmdDrawIndexed` / `vkCmdDraw`
- [ ] `vkCmdCopyBuffer` / `vkCmdCopyImage`

### Tier 5: Swapchain (for DXVK Present)
For render-only adapter, the swapchain extension may not be needed — DXVK's Present path goes back through the software renderer / display driver. Confirm this with DXVK's source.

---

## 5. Extensions to Expose

Venus supports a large set of Vulkan extensions. Expose the ones DXVK requires:

**Minimum for DXVK 2.x:**
```
VK_KHR_swapchain (may be optional for render-only)
VK_KHR_maintenance1/2/3/4
VK_KHR_shader_draw_parameters
VK_EXT_vertex_attribute_divisor
VK_EXT_transform_feedback
VK_EXT_depth_clip_enable
VK_EXT_extended_dynamic_state
VK_KHR_timeline_semaphore
VK_KHR_synchronization2
VK_KHR_dynamic_rendering
VK_EXT_robustness2
VK_EXT_memory_budget
```

Query from the host via `vkEnumerateDeviceExtensionProperties` and pass through the ones in our allowlist.

---

## 6. Testing

```powershell
# Install the Helios ICD by placing helios_vulkan.json and helios_icd.dll
# and registering the manifest.

# Test 1: Loader finds the ICD
vulkaninfo 2>&1 | Select-String "Helios"

# Test 2: Enumerate physical device
$env:VK_LOADER_DEBUG = "all"
vulkaninfo

# Test 3: Render a frame
vkcube  # from VulkanSDK

# Test 4: DXVK smoke test
# Download a D3D11 demo, set DXVK_LOG_LEVEL=info
# Ensure DXVK picks up our ICD:
$env:DXVK_CONFIG_FILE = "dxvk.conf"
# dxvk.conf: d3d11.maxFeatureLevel = 11_1
```

---

## 7. ICD Loader Interface Reference

Full loader-ICD interface spec:
https://github.com/KhronosGroup/Vulkan-Loader/blob/main/docs/LoaderDriverInterface.md

Key interface version history:
| Version | Added capability |
|---------|-----------------|
| 0 | Initial (vk_icdGetInstanceProcAddr) |
| 1 | vk_icdGetInstanceProcAddr stability |
| 2 | Higher-performance `VkPhysicalDevice` dispatchable |
| 3 | `vk_icdNegotiateLoaderICDInterfaceVersion` |
| 4 | `vk_icdGetPhysicalDeviceProcAddr` |
| 5 | Loader calls `vk_icdGetInstanceProcAddr(NULL, "vkGetInstanceProcAddr")` |
| 6 | `vk_icdEnumerateAdapterPhysicalDevices` (DXGI adapter ordering) |

Target version 5 minimum.
