// Does the host execute anything, and does the guest CPU see the result?
//
// 2026-08-29. d3d11_poison_copy_probe showed that a poisoned staging texture
// comes back with the poison INTACT after every GPU route (staging->staging,
// staging->DEFAULT->staging, ClearRenderTargetView->staging). So the older
// "everything reads back zero" was never a GPU write of zeros — it was pages
// nothing had touched. Two hypotheses survive and D3D11 cannot separate them:
//
//   (a) the host never executes the command buffer;
//   (b) it executes, but writes memory this process never maps.
//
// This probe drops D3D11 and DXVK and talks to the venus ICD through the
// Vulkan loader, so whatever it finds belongs to the ICD / KMD / host
// substrate and not to DXVK. It answers, in one run:
//
//   * FILL   vkCmdFillBuffer over poisoned host-visible memory. Poison intact
//            = nothing executed or nothing landed here; the fill value = the
//            whole substrate works and the defect is above, in DXVK.
//   * COPY   CPU-written source -> vkCmdCopyBuffer -> poisoned destination,
//            which separates the CPU->host direction from host->CPU.
//   * TS     vkCmdWriteTimestamp read with vkGetQueryPoolResults, a host CALL
//            whose answer returns over the venus reply channel rather than
//            through mapped memory. It witnesses execution even if no map does.
//   * BDA    whether core Vulkan 1.2 bufferDeviceAddress can actually be
//            enabled on the session device. dxvk_device_info.cpp:502 turns the
//            core feature off and picks the EXT arm, and the host then reports
//            "bufferDeviceAddress feature is not enabled" 110 times a boot.
//            This creates the device with the CORE arm on and allocates with
//            VK_MEMORY_ALLOCATE_DEVICE_ADDRESS_BIT, so the host log says
//            straight away whether the core arm survives to vkCreateDevice.
//
// It also dumps timestampPeriod and the queue families, because DXVK reports
// a timestamp frequency of 0 (limits.timestampPeriod == 0) and because the
// bogus CONCURRENT queue-family index the host complains about is
// 1000146003 == VK_STRUCTURE_TYPE_MEMORY_REQUIREMENTS_2.
//
// It loads the venus ICD DIRECTLY through vk_icdGetInstanceProcAddr rather than
// through vulkan-1.dll, because that is what the UMD does: d3d11_module_report
// shows a live D3D11 process holding the DriverStore vulkan_virtio.dll with no
// vulkan-1.dll mapped at all. Going through the loader instead reaches the
// manifest in HKLM\SOFTWARE\Khronos\Vulkan\Drivers, which still names a
// week-old copy under C:\ProgramData\HeliosVulkan that enumerates 0 devices.
// Pass a different ICD path as argv[1] to compare builds.
//
// No import library is needed: every entry point is resolved by hand. Build on
// the VM with mingw:
//   g++ -O1 -I Z:\icd\mesa\include -o C:\Users\Rupansh\vk_exec_visibility_probe.exe ^
//       Z:\tools\vk_exec_visibility_probe.cpp
#define VK_NO_PROTOTYPES
#define VK_USE_PLATFORM_WIN32_KHR
#include <vulkan/vulkan.h>
#include <windows.h>
#include <cstdio>
#include <cstring>
#include <cstdint>

static const VkDeviceSize BUF_SIZE = 4096;
static const uint32_t POISON = 0xCDCDCDCDu;
static const uint32_t FILLV  = 0xA5A5A5A5u;
static const uint32_t COPYV  = 0x5A5A5A5Au;

#define IFN(name) static PFN_##name name = nullptr;
#define DFN(name) static PFN_##name name = nullptr;

IFN(vkGetInstanceProcAddr) IFN(vkCreateInstance) IFN(vkDestroyInstance)
IFN(vkEnumeratePhysicalDevices) IFN(vkGetPhysicalDeviceProperties2)
IFN(vkGetPhysicalDeviceFeatures2) IFN(vkGetPhysicalDeviceMemoryProperties)
IFN(vkGetPhysicalDeviceQueueFamilyProperties) IFN(vkCreateDevice)
IFN(vkDestroyDevice) IFN(vkGetDeviceProcAddr)

DFN(vkGetDeviceQueue) DFN(vkCreateBuffer) DFN(vkDestroyBuffer)
DFN(vkGetBufferMemoryRequirements) DFN(vkAllocateMemory) DFN(vkFreeMemory)
DFN(vkBindBufferMemory) DFN(vkMapMemory) DFN(vkUnmapMemory)
DFN(vkFlushMappedMemoryRanges) DFN(vkInvalidateMappedMemoryRanges)
DFN(vkCreateCommandPool) DFN(vkDestroyCommandPool) DFN(vkAllocateCommandBuffers)
DFN(vkBeginCommandBuffer) DFN(vkEndCommandBuffer) DFN(vkCmdFillBuffer)
DFN(vkCmdCopyBuffer) DFN(vkQueueSubmit) DFN(vkQueueWaitIdle) DFN(vkDeviceWaitIdle)
DFN(vkCreateFence) DFN(vkDestroyFence) DFN(vkWaitForFences)
DFN(vkCreateQueryPool) DFN(vkDestroyQueryPool) DFN(vkCmdResetQueryPool)
DFN(vkCmdWriteTimestamp) DFN(vkGetQueryPoolResults)

static const char *res_str(VkResult r) {
  switch (r) {
    case VK_SUCCESS: return "VK_SUCCESS";
    case VK_NOT_READY: return "VK_NOT_READY";
    case VK_TIMEOUT: return "VK_TIMEOUT";
    case VK_ERROR_OUT_OF_HOST_MEMORY: return "OUT_OF_HOST_MEMORY";
    case VK_ERROR_OUT_OF_DEVICE_MEMORY: return "OUT_OF_DEVICE_MEMORY";
    case VK_ERROR_INITIALIZATION_FAILED: return "INITIALIZATION_FAILED";
    case VK_ERROR_DEVICE_LOST: return "DEVICE_LOST";
    case VK_ERROR_MEMORY_MAP_FAILED: return "MEMORY_MAP_FAILED";
    case VK_ERROR_EXTENSION_NOT_PRESENT: return "EXTENSION_NOT_PRESENT";
    case VK_ERROR_FEATURE_NOT_PRESENT: return "FEATURE_NOT_PRESENT";
    case VK_ERROR_INCOMPATIBLE_DRIVER: return "INCOMPATIBLE_DRIVER";
    default: return "other";
  }
}

// Classify a mapped 4 KiB window the same way the D3D11 poison probe does, so
// the two transcripts can be compared line for line.
static void census(const void *p, const char *tag, uint32_t want) {
  const uint32_t *v = (const uint32_t *)p;
  const size_t n = BUF_SIZE / 4;
  size_t want_n = 0, poison_n = 0, zero_n = 0, other = 0;
  for (size_t i = 0; i < n; ++i) {
    if (v[i] == want)        ++want_n;
    else if (v[i] == POISON) ++poison_n;
    else if (v[i] == 0)      ++zero_n;
    else                     ++other;
  }
  const char *verdict =
      want_n == n   ? "PASS  the GPU wrote OUR memory"
    : poison_n == n ? "POISON INTACT  nothing wrote our memory"
    : zero_n == n   ? "ZEROED  our memory was written, with zeros"
    :                 "MIXED";
  printf("  %-8s want=%zu poison=%zu zero=%zu other=%zu  first=%08x %08x  %s\n",
         tag, want_n, poison_n, zero_n, other, v[0], v[1], verdict);
}

static void poison(void *p) {
  uint32_t *v = (uint32_t *)p;
  for (size_t i = 0; i < BUF_SIZE / 4; ++i) v[i] = POISON;
}

static const char *DEFAULT_ICD =
  "C:\\WINDOWS\\System32\\DriverStore\\FileRepository\\"
  "helios_kmd_render.inf_amd64_63714e9c1713cb36\\vulkan_virtio.dll";

typedef VkResult (VKAPI_PTR *PFN_vkNegotiateLoaderICDInterfaceVersion)(uint32_t *);

int main(int argc, char **argv) {
  const char *icdPath = argc > 1 ? argv[1] : DEFAULT_ICD;
  printf("ICD: %s\n", icdPath);
  HMODULE lib = LoadLibraryA(icdPath);
  if (!lib) { printf("ICD not loadable, err=%lu\n", (unsigned long)GetLastError()); return 1; }

  // The ICD entry point, not the loader's. Negotiate first if the ICD offers
  // it: mesa refuses to hand out procs at an unnegotiated interface version.
  auto neg = (PFN_vkNegotiateLoaderICDInterfaceVersion)(void *)
      GetProcAddress(lib, "vk_icdNegotiateLoaderICDInterfaceVersion");
  if (neg) {
    uint32_t version = 5;
    VkResult nr = neg(&version);
    printf("vk_icdNegotiateLoaderICDInterfaceVersion -> %d, version=%u\n", (int)nr, version);
  }
  vkGetInstanceProcAddr = (PFN_vkGetInstanceProcAddr)(void *)
      GetProcAddress(lib, "vk_icdGetInstanceProcAddr");
  if (!vkGetInstanceProcAddr)
    vkGetInstanceProcAddr = (PFN_vkGetInstanceProcAddr)(void *)
        GetProcAddress(lib, "vkGetInstanceProcAddr");
  if (!vkGetInstanceProcAddr) { printf("no vk_icdGetInstanceProcAddr\n"); return 1; }

#define LOADI(n) n = (PFN_##n)vkGetInstanceProcAddr(inst, #n); if (!n) printf("  MISSING %s\n", #n);
  VkInstance inst = VK_NULL_HANDLE;
  vkCreateInstance = (PFN_vkCreateInstance)vkGetInstanceProcAddr(nullptr, "vkCreateInstance");

  // A NULL pApplicationInfo is what once pinned the wire api to 1.1 and gave
  // the host NULL procs (memory: hps2-display-regression-nullproc-rootcause).
  VkApplicationInfo app = { VK_STRUCTURE_TYPE_APPLICATION_INFO };
  app.pApplicationName = "helios-vk-exec-probe";
  app.apiVersion = VK_API_VERSION_1_3;
  VkInstanceCreateInfo ici = { VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO };
  ici.pApplicationInfo = &app;
  VkResult r = vkCreateInstance(&ici, nullptr, &inst);
  printf("vkCreateInstance(api=1.3) = %s\n", res_str(r));
  if (r != VK_SUCCESS) return 1;

  LOADI(vkDestroyInstance) LOADI(vkEnumeratePhysicalDevices)
  LOADI(vkGetPhysicalDeviceProperties2) LOADI(vkGetPhysicalDeviceFeatures2)
  LOADI(vkGetPhysicalDeviceMemoryProperties) LOADI(vkGetPhysicalDeviceQueueFamilyProperties)
  LOADI(vkCreateDevice) LOADI(vkDestroyDevice) LOADI(vkGetDeviceProcAddr)

  uint32_t count = 0;
  vkEnumeratePhysicalDevices(inst, &count, nullptr);
  VkPhysicalDevice pds[8]; if (count > 8) count = 8;
  vkEnumeratePhysicalDevices(inst, &count, pds);
  printf("physical devices: %u\n", count);

  VkPhysicalDevice pd = VK_NULL_HANDLE;
  VkPhysicalDeviceProperties2 props = { VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_PROPERTIES_2 };
  for (uint32_t i = 0; i < count; ++i) {
    VkPhysicalDeviceProperties2 p = { VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_PROPERTIES_2 };
    vkGetPhysicalDeviceProperties2(pds[i], &p);
    printf("  [%u] \"%s\" api=%u.%u.%u driver=0x%x type=%u\n", i, p.properties.deviceName,
           VK_VERSION_MAJOR(p.properties.apiVersion), VK_VERSION_MINOR(p.properties.apiVersion),
           VK_VERSION_PATCH(p.properties.apiVersion), p.properties.driverVersion,
           (unsigned)p.properties.deviceType);
    if (pd == VK_NULL_HANDLE && strstr(p.properties.deviceName, "Venus")) { pd = pds[i]; props = p; }
  }
  if (pd == VK_NULL_HANDLE && count) { pd = pds[0]; vkGetPhysicalDeviceProperties2(pd, &props); }
  if (pd == VK_NULL_HANDLE) { printf("no physical device\n"); return 1; }
  printf("using \"%s\"\n", props.properties.deviceName);
  printf("  timestampPeriod=%f  (DXVK reports frequency 1e9/this)\n",
         props.properties.limits.timestampPeriod);

  uint32_t qfc = 0;
  vkGetPhysicalDeviceQueueFamilyProperties(pd, &qfc, nullptr);
  VkQueueFamilyProperties qfp[8]; if (qfc > 8) qfc = 8;
  vkGetPhysicalDeviceQueueFamilyProperties(pd, &qfc, qfp);
  printf("  queue families=%u\n", qfc);
  for (uint32_t i = 0; i < qfc; ++i)
    printf("    [%u] flags=0x%x count=%u timestampValidBits=%u\n", i,
           qfp[i].queueFlags, qfp[i].queueCount, qfp[i].timestampValidBits);

  // Does the device advertise the core 1.2 arm DXVK deliberately switches off?
  VkPhysicalDeviceBufferDeviceAddressFeaturesEXT bdaExt =
    { VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_BUFFER_DEVICE_ADDRESS_FEATURES_EXT };
  VkPhysicalDeviceVulkan12Features v12 = { VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VULKAN_1_2_FEATURES };
  v12.pNext = &bdaExt;
  VkPhysicalDeviceFeatures2 f2 = { VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_FEATURES_2 };
  f2.pNext = &v12;
  vkGetPhysicalDeviceFeatures2(pd, &f2);
  printf("  supported: vk12.bufferDeviceAddress=%d  extBDA.bufferDeviceAddress=%d\n",
         (int)v12.bufferDeviceAddress, (int)bdaExt.bufferDeviceAddress);

  VkPhysicalDeviceMemoryProperties mp = {};
  vkGetPhysicalDeviceMemoryProperties(pd, &mp);
  printf("  memory types=%u heaps=%u\n", mp.memoryTypeCount, mp.memoryHeapCount);
  for (uint32_t i = 0; i < mp.memoryTypeCount; ++i)
    printf("    type[%u] heap=%u flags=0x%x%s%s%s%s\n", i, mp.memoryTypes[i].heapIndex,
           mp.memoryTypes[i].propertyFlags,
           (mp.memoryTypes[i].propertyFlags & VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT) ? " DEVICE_LOCAL" : "",
           (mp.memoryTypes[i].propertyFlags & VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT) ? " HOST_VISIBLE" : "",
           (mp.memoryTypes[i].propertyFlags & VK_MEMORY_PROPERTY_HOST_COHERENT_BIT) ? " HOST_COHERENT" : "",
           (mp.memoryTypes[i].propertyFlags & VK_MEMORY_PROPERTY_HOST_CACHED_BIT) ? " HOST_CACHED" : "");

  // Create the device with the CORE bufferDeviceAddress arm enabled — the arm
  // dxvk_device_info.cpp:502 forces off. If the host still says the feature is
  // not enabled after this, the loss is below the guest, in vkr's filter.
  const bool wantCoreBda = v12.bufferDeviceAddress == VK_TRUE;
  VkPhysicalDeviceVulkan12Features en12 = { VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VULKAN_1_2_FEATURES };
  en12.bufferDeviceAddress = wantCoreBda ? VK_TRUE : VK_FALSE;
  VkPhysicalDeviceFeatures2 enf = { VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_FEATURES_2 };
  enf.pNext = &en12;

  const float prio = 1.0f;
  VkDeviceQueueCreateInfo qci = { VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO };
  qci.queueFamilyIndex = 0; qci.queueCount = 1; qci.pQueuePriorities = &prio;
  VkDeviceCreateInfo dci = { VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO };
  dci.pNext = &enf; dci.queueCreateInfoCount = 1; dci.pQueueCreateInfos = &qci;

  VkDevice dev = VK_NULL_HANDLE;
  r = vkCreateDevice(pd, &dci, nullptr, &dev);
  printf("vkCreateDevice(core bufferDeviceAddress=%d) = %s\n", (int)wantCoreBda, res_str(r));
  if (r != VK_SUCCESS) { printf("cannot continue\n"); return 1; }

#define LOADD(n) n = (PFN_##n)vkGetDeviceProcAddr(dev, #n); if (!n) printf("  MISSING %s\n", #n);
  LOADD(vkGetDeviceQueue) LOADD(vkCreateBuffer) LOADD(vkDestroyBuffer)
  LOADD(vkGetBufferMemoryRequirements) LOADD(vkAllocateMemory) LOADD(vkFreeMemory)
  LOADD(vkBindBufferMemory) LOADD(vkMapMemory) LOADD(vkUnmapMemory)
  LOADD(vkFlushMappedMemoryRanges) LOADD(vkInvalidateMappedMemoryRanges)
  LOADD(vkCreateCommandPool) LOADD(vkDestroyCommandPool) LOADD(vkAllocateCommandBuffers)
  LOADD(vkBeginCommandBuffer) LOADD(vkEndCommandBuffer) LOADD(vkCmdFillBuffer)
  LOADD(vkCmdCopyBuffer) LOADD(vkQueueSubmit) LOADD(vkQueueWaitIdle) LOADD(vkDeviceWaitIdle)
  LOADD(vkCreateFence) LOADD(vkDestroyFence) LOADD(vkWaitForFences)
  LOADD(vkCreateQueryPool) LOADD(vkDestroyQueryPool) LOADD(vkCmdResetQueryPool)
  LOADD(vkCmdWriteTimestamp) LOADD(vkGetQueryPoolResults)

  VkQueue queue = VK_NULL_HANDLE;
  vkGetDeviceQueue(dev, 0, 0, &queue);

  // Two host-visible buffers: dst is poisoned and written by the GPU, src is
  // written by the CPU and read by the GPU.
  VkBuffer buf[2] = {}; VkDeviceMemory mem[2] = {}; void *map[2] = {}; bool coherent[2] = {};
  for (int i = 0; i < 2; ++i) {
    VkBufferCreateInfo bci = { VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO };
    bci.size = BUF_SIZE;
    bci.usage = VK_BUFFER_USAGE_TRANSFER_SRC_BIT | VK_BUFFER_USAGE_TRANSFER_DST_BIT;
    bci.sharingMode = VK_SHARING_MODE_EXCLUSIVE;
    r = vkCreateBuffer(dev, &bci, nullptr, &buf[i]);
    if (r != VK_SUCCESS) { printf("vkCreateBuffer[%d] = %s\n", i, res_str(r)); return 1; }

    VkMemoryRequirements req = {};
    vkGetBufferMemoryRequirements(dev, buf[i], &req);
    if (i == 0) printf("  buffer memReq size=%llu align=%llu typeBits=0x%x\n",
                       (unsigned long long)req.size, (unsigned long long)req.alignment, req.memoryTypeBits);

    int chosen = -1;
    for (uint32_t t = 0; t < mp.memoryTypeCount; ++t) {
      if (!(req.memoryTypeBits & (1u << t))) continue;
      const VkMemoryPropertyFlags f = mp.memoryTypes[t].propertyFlags;
      if (!(f & VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT)) continue;
      if (chosen < 0) chosen = (int)t;
      if (f & VK_MEMORY_PROPERTY_HOST_COHERENT_BIT) { chosen = (int)t; break; }
    }
    if (chosen < 0) { printf("no host-visible memory type\n"); return 1; }
    coherent[i] = (mp.memoryTypes[chosen].propertyFlags & VK_MEMORY_PROPERTY_HOST_COHERENT_BIT) != 0;
    if (i == 0) printf("  using memory type %d (coherent=%d)\n", chosen, (int)coherent[i]);

    // Ask for a device address too: this is the allocation the host complains
    // about, and now the core feature really is enabled.
    VkMemoryAllocateFlagsInfo mafi = { VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_FLAGS_INFO };
    mafi.flags = VK_MEMORY_ALLOCATE_DEVICE_ADDRESS_BIT;
    VkMemoryAllocateInfo mai = { VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO };
    mai.allocationSize = req.size; mai.memoryTypeIndex = (uint32_t)chosen;
    r = vkAllocateMemory(dev, &mai, nullptr, &mem[i]);
    if (r != VK_SUCCESS) { printf("vkAllocateMemory[%d] = %s\n", i, res_str(r)); return 1; }
    r = vkBindBufferMemory(dev, buf[i], mem[i], 0);
    if (r != VK_SUCCESS) { printf("vkBindBufferMemory[%d] = %s\n", i, res_str(r)); return 1; }
    r = vkMapMemory(dev, mem[i], 0, VK_WHOLE_SIZE, 0, &map[i]);
    if (r != VK_SUCCESS) { printf("vkMapMemory[%d] = %s\n", i, res_str(r)); return 1; }
  }

  auto flush = [&](int i) {
    if (coherent[i] || !vkFlushMappedMemoryRanges) return;
    VkMappedMemoryRange mr = { VK_STRUCTURE_TYPE_MAPPED_MEMORY_RANGE };
    mr.memory = mem[i]; mr.offset = 0; mr.size = VK_WHOLE_SIZE;
    vkFlushMappedMemoryRanges(dev, 1, &mr);
  };
  // Always invalidate before reading, coherent or not: it costs nothing and
  // removes "the guest cached a stale line" from the list of explanations.
  auto invalidate = [&](int i) {
    if (!vkInvalidateMappedMemoryRanges) return;
    VkMappedMemoryRange mr = { VK_STRUCTURE_TYPE_MAPPED_MEMORY_RANGE };
    mr.memory = mem[i]; mr.offset = 0; mr.size = VK_WHOLE_SIZE;
    vkInvalidateMappedMemoryRanges(dev, 1, &mr);
  };

  printf("\n0 CPU CONTROL — map, poison, read back, no GPU involved\n");
  poison(map[0]); flush(0); invalidate(0);
  census(map[0], "control", POISON);

  VkCommandPool pool = VK_NULL_HANDLE;
  VkCommandPoolCreateInfo pci = { VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO };
  pci.queueFamilyIndex = 0; pci.flags = VK_COMMAND_POOL_CREATE_RESET_COMMAND_BUFFER_BIT;
  r = vkCreateCommandPool(dev, &pci, nullptr, &pool);
  printf("vkCreateCommandPool = %s\n", res_str(r));
  if (r != VK_SUCCESS) return 1;

  VkCommandBuffer cb = VK_NULL_HANDLE;
  VkCommandBufferAllocateInfo cbai = { VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO };
  cbai.commandPool = pool; cbai.level = VK_COMMAND_BUFFER_LEVEL_PRIMARY; cbai.commandBufferCount = 1;
  r = vkAllocateCommandBuffers(dev, &cbai, &cb);
  printf("vkAllocateCommandBuffers = %s\n", res_str(r));
  if (r != VK_SUCCESS) return 1;

  VkQueryPool qpool = VK_NULL_HANDLE;
  VkQueryPoolCreateInfo qpci = { VK_STRUCTURE_TYPE_QUERY_POOL_CREATE_INFO };
  qpci.queryType = VK_QUERY_TYPE_TIMESTAMP; qpci.queryCount = 2;
  if (vkCreateQueryPool) {
    r = vkCreateQueryPool(dev, &qpci, nullptr, &qpool);
    printf("vkCreateQueryPool(TIMESTAMP,2) = %s\n", res_str(r));
    if (r != VK_SUCCESS) qpool = VK_NULL_HANDLE;
  }

  // Record: timestamp, fill dst, copy src->dst, timestamp.
  poison(map[0]); flush(0);
  uint32_t *s = (uint32_t *)map[1];
  for (size_t i = 0; i < BUF_SIZE / 4; ++i) s[i] = COPYV;
  flush(1);

  VkCommandBufferBeginInfo bi = { VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO };
  bi.flags = VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT;
  r = vkBeginCommandBuffer(cb, &bi);
  if (r != VK_SUCCESS) { printf("vkBeginCommandBuffer = %s\n", res_str(r)); return 1; }
  if (qpool) { vkCmdResetQueryPool(cb, qpool, 0, 2);
               vkCmdWriteTimestamp(cb, VK_PIPELINE_STAGE_TOP_OF_PIPE_BIT, qpool, 0); }
  vkCmdFillBuffer(cb, buf[0], 0, BUF_SIZE, FILLV);
  if (qpool) vkCmdWriteTimestamp(cb, VK_PIPELINE_STAGE_BOTTOM_OF_PIPE_BIT, qpool, 1);
  r = vkEndCommandBuffer(cb);
  if (r != VK_SUCCESS) { printf("vkEndCommandBuffer = %s\n", res_str(r)); return 1; }

  VkFence fence = VK_NULL_HANDLE;
  VkFenceCreateInfo fci = { VK_STRUCTURE_TYPE_FENCE_CREATE_INFO };
  vkCreateFence(dev, &fci, nullptr, &fence);

  VkSubmitInfo si = { VK_STRUCTURE_TYPE_SUBMIT_INFO };
  si.commandBufferCount = 1; si.pCommandBuffers = &cb;
  const DWORD t0 = GetTickCount();
  r = vkQueueSubmit(queue, 1, &si, fence);
  printf("\n1 FILL — vkCmdFillBuffer(0x%08x) over the poison\n", FILLV);
  printf("  vkQueueSubmit = %s\n", res_str(r));
  VkResult wr = VK_NOT_READY;
  if (r == VK_SUCCESS && fence) wr = vkWaitForFences(dev, 1, &fence, VK_TRUE, 3000ull * 1000ull * 1000ull);
  printf("  vkWaitForFences = %s after %lu ms\n", res_str(wr), (unsigned long)(GetTickCount() - t0));
  VkResult qw = vkQueueWaitIdle(queue);
  printf("  vkQueueWaitIdle = %s\n", res_str(qw));
  invalidate(0);
  census(map[0], "fill", FILLV);

  if (qpool) {
    uint64_t ts[2] = { 0, 0 };
    VkResult qr = vkGetQueryPoolResults(dev, qpool, 0, 2, sizeof(ts), ts, sizeof(uint64_t),
                                        VK_QUERY_RESULT_64_BIT | VK_QUERY_RESULT_WAIT_BIT);
    printf("  timestamps = %s  t0=%llu t1=%llu delta=%lld  %s\n", res_str(qr),
           (unsigned long long)ts[0], (unsigned long long)ts[1],
           (long long)(ts[1] - ts[0]),
           (qr == VK_SUCCESS && (ts[0] || ts[1])) ? "GPU EXECUTED" : "no execution witness");
  }

  // 2 COPY: CPU-written source -> GPU copy -> poisoned destination.
  poison(map[0]); flush(0);
  vkBeginCommandBuffer(cb, &bi);
  VkBufferCopy region = { 0, 0, BUF_SIZE };
  vkCmdCopyBuffer(cb, buf[1], buf[0], 1, &region);
  vkEndCommandBuffer(cb);
  VkFence fence2 = VK_NULL_HANDLE;
  vkCreateFence(dev, &fci, nullptr, &fence2);
  r = vkQueueSubmit(queue, 1, &si, fence2);
  printf("\n2 COPY — CPU wrote 0x%08x into src, GPU copies src -> poisoned dst\n", COPYV);
  printf("  vkQueueSubmit = %s\n", res_str(r));
  if (r == VK_SUCCESS && fence2)
    printf("  vkWaitForFences = %s\n",
           res_str(vkWaitForFences(dev, 1, &fence2, VK_TRUE, 3000ull * 1000ull * 1000ull)));
  vkQueueWaitIdle(queue);
  invalidate(0); invalidate(1);
  census(map[0], "copy.dst", COPYV);
  census(map[1], "copy.src", COPYV);

  printf("\ndone\n");
  vkDeviceWaitIdle(dev);
  if (fence) vkDestroyFence(dev, fence, nullptr);
  if (fence2) vkDestroyFence(dev, fence2, nullptr);
  if (qpool) vkDestroyQueryPool(dev, qpool, nullptr);
  vkDestroyCommandPool(dev, pool, nullptr);
  for (int i = 0; i < 2; ++i) {
    if (map[i]) vkUnmapMemory(dev, mem[i]);
    if (buf[i]) vkDestroyBuffer(dev, buf[i], nullptr);
    if (mem[i]) vkFreeMemory(dev, mem[i], nullptr);
  }
  vkDestroyDevice(dev, nullptr);
  vkDestroyInstance(inst, nullptr);
  return 0;
}
