// Regression: a delayed producer must keep the vehicle source unavailable,
// then become acquirable after the helper copy completes. No queue/device-idle
// waits. Run in the interactive session with async WSI and the vehicle enabled.
// Build: x86_64-w64-mingw32-g++ -std=c++17 -O2 -static -Iicd/mesa/include
//        tools/vk_vehicle_completion_probe.cpp -luser32 -o <probe.exe>
// Usage: probe.exe <exact ICD DLL>. Require this PID's vehicle LIVE and
// copy-pending/completed diagnostics as well as PASS; GDI cannot pass the gate.
#define VK_USE_PLATFORM_WIN32_KHR
#define VK_NO_PROTOTYPES
#include <vulkan/vulkan.h>
#include <windows.h>
#include <algorithm>
#include <cstdio>
#include <cstdlib>
#include <vector>

#define REQUIRE(expr) do { if (!(expr)) { \
  std::fprintf(stderr, "FAIL line %d: %s\n", __LINE__, #expr); std::exit(1); } } while (0)
#define CHECK(expr) do { VkResult r = (expr); if (r != VK_SUCCESS) { \
  std::fprintf(stderr, "FAIL line %d: %s -> %d\n", __LINE__, #expr, r); std::exit(1); } } while (0)
#define INSTANCE_PROC(name) auto name = reinterpret_cast<PFN_##name>(get(instance, #name)); REQUIRE(name)
#define DEVICE_PROC(name) auto name = reinterpret_cast<PFN_##name>(vkGetDeviceProcAddr(device, #name)); REQUIRE(name)

static LRESULT CALLBACK window_proc(HWND h, UINT m, WPARAM w, LPARAM l) {
  return DefWindowProcA(h, m, w, l);
}

static void pump() {
  MSG message;
  while (PeekMessageA(&message, nullptr, 0, 0, PM_REMOVE)) {
    TranslateMessage(&message);
    DispatchMessageA(&message);
  }
}

int main(int argc, char** argv) {
  REQUIRE(argc == 2);
  DWORD session = 0;
  REQUIRE(ProcessIdToSessionId(GetCurrentProcessId(), &session) && session != 0);
  std::printf("vehicle completion probe pid=%lu session=%lu\n", GetCurrentProcessId(), session);
  std::fflush(stdout);
  HMODULE module = LoadLibraryA(argv[1]);
  REQUIRE(module);
  auto get = reinterpret_cast<PFN_vkGetInstanceProcAddr>(GetProcAddress(module, "vk_icdGetInstanceProcAddr"));
  REQUIRE(get);
  auto vkCreateInstance = reinterpret_cast<PFN_vkCreateInstance>(get(nullptr, "vkCreateInstance"));
  REQUIRE(vkCreateInstance);
  VkApplicationInfo app = { VK_STRUCTURE_TYPE_APPLICATION_INFO };
  app.pApplicationName = "helios vehicle completion probe";
  app.apiVersion = VK_API_VERSION_1_2;
  const char* instance_extensions[] = { "VK_KHR_surface", "VK_KHR_win32_surface" };
  VkInstanceCreateInfo ici = { VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO };
  ici.pApplicationInfo = &app;
  ici.enabledExtensionCount = 2;
  ici.ppEnabledExtensionNames = instance_extensions;
  VkInstance instance = VK_NULL_HANDLE;
  CHECK(vkCreateInstance(&ici, nullptr, &instance));
  INSTANCE_PROC(vkCreateWin32SurfaceKHR);
  INSTANCE_PROC(vkEnumeratePhysicalDevices);
  INSTANCE_PROC(vkGetPhysicalDeviceQueueFamilyProperties);
  INSTANCE_PROC(vkGetPhysicalDeviceSurfaceSupportKHR);
  INSTANCE_PROC(vkGetPhysicalDeviceSurfaceCapabilitiesKHR);
  INSTANCE_PROC(vkGetPhysicalDeviceSurfaceFormatsKHR);
  INSTANCE_PROC(vkGetPhysicalDeviceMemoryProperties);
  INSTANCE_PROC(vkCreateDevice);
  INSTANCE_PROC(vkGetDeviceProcAddr);
  INSTANCE_PROC(vkDestroySurfaceKHR);
  INSTANCE_PROC(vkDestroyInstance);

  WNDCLASSA wc = {};
  wc.lpfnWndProc = window_proc;
  wc.hInstance = GetModuleHandleA(nullptr);
  wc.lpszClassName = "HeliosVehicleCompletionProbe";
  REQUIRE(RegisterClassA(&wc));
  HWND hwnd = CreateWindowExA(0, wc.lpszClassName, "Helios copy completion", WS_POPUP | WS_VISIBLE,
    50, 50, 640, 480, nullptr, nullptr, wc.hInstance, nullptr);
  REQUIRE(hwnd);
  pump();
  VkWin32SurfaceCreateInfoKHR sci = { VK_STRUCTURE_TYPE_WIN32_SURFACE_CREATE_INFO_KHR };
  sci.hinstance = wc.hInstance;
  sci.hwnd = hwnd;
  VkSurfaceKHR surface = VK_NULL_HANDLE;
  CHECK(vkCreateWin32SurfaceKHR(instance, &sci, nullptr, &surface));
  uint32_t count = 0;
  CHECK(vkEnumeratePhysicalDevices(instance, &count, nullptr));
  REQUIRE(count);
  std::vector<VkPhysicalDevice> devices(count);
  CHECK(vkEnumeratePhysicalDevices(instance, &count, devices.data()));
  VkPhysicalDevice physical = VK_NULL_HANDLE;
  uint32_t family = UINT32_MAX;
  for (auto candidate : devices) {
    uint32_t n = 0;
    vkGetPhysicalDeviceQueueFamilyProperties(candidate, &n, nullptr);
    std::vector<VkQueueFamilyProperties> families(n);
    vkGetPhysicalDeviceQueueFamilyProperties(candidate, &n, families.data());
    for (uint32_t i = 0; i < n; ++i) {
      VkBool32 present = VK_FALSE;
      CHECK(vkGetPhysicalDeviceSurfaceSupportKHR(candidate, i, surface, &present));
      if (present && (families[i].queueFlags & VK_QUEUE_GRAPHICS_BIT)) {
        physical = candidate;
        family = i;
        break;
      }
    }
    if (physical) break;
  }
  REQUIRE(physical);
  float priority = 1.0f;
  VkDeviceQueueCreateInfo qci = { VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO };
  qci.queueFamilyIndex = family;
  qci.queueCount = 1;
  qci.pQueuePriorities = &priority;
  const char* device_extensions[] = { "VK_KHR_swapchain" };
  VkDeviceCreateInfo dci = { VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO };
  dci.queueCreateInfoCount = 1;
  dci.pQueueCreateInfos = &qci;
  dci.enabledExtensionCount = 1;
  dci.ppEnabledExtensionNames = device_extensions;
  VkDevice device = VK_NULL_HANDLE;
  CHECK(vkCreateDevice(physical, &dci, nullptr, &device));
  DEVICE_PROC(vkGetDeviceQueue);
  DEVICE_PROC(vkCreateSwapchainKHR); DEVICE_PROC(vkDestroySwapchainKHR);
  DEVICE_PROC(vkGetSwapchainImagesKHR); DEVICE_PROC(vkAcquireNextImageKHR);
  DEVICE_PROC(vkCreateSemaphore); DEVICE_PROC(vkDestroySemaphore);
  DEVICE_PROC(vkCreateFence); DEVICE_PROC(vkDestroyFence); DEVICE_PROC(vkResetFences); DEVICE_PROC(vkWaitForFences); DEVICE_PROC(vkGetFenceStatus);
  DEVICE_PROC(vkCreateBuffer); DEVICE_PROC(vkDestroyBuffer); DEVICE_PROC(vkGetBufferMemoryRequirements);
  DEVICE_PROC(vkAllocateMemory); DEVICE_PROC(vkFreeMemory); DEVICE_PROC(vkBindBufferMemory); DEVICE_PROC(vkCmdFillBuffer);
  DEVICE_PROC(vkCreateCommandPool); DEVICE_PROC(vkDestroyCommandPool);
  DEVICE_PROC(vkAllocateCommandBuffers); DEVICE_PROC(vkResetCommandBuffer);
  DEVICE_PROC(vkBeginCommandBuffer); DEVICE_PROC(vkEndCommandBuffer);
  DEVICE_PROC(vkCmdPipelineBarrier); DEVICE_PROC(vkCmdClearColorImage);
  DEVICE_PROC(vkQueueSubmit); DEVICE_PROC(vkQueuePresentKHR); DEVICE_PROC(vkDestroyDevice);
  VkQueue queue = VK_NULL_HANDLE;
  vkGetDeviceQueue(device, family, 0, &queue);
  VkSurfaceCapabilitiesKHR caps;
  CHECK(vkGetPhysicalDeviceSurfaceCapabilitiesKHR(physical, surface, &caps));
  REQUIRE(caps.supportedUsageFlags & VK_IMAGE_USAGE_TRANSFER_DST_BIT);
  CHECK(vkGetPhysicalDeviceSurfaceFormatsKHR(physical, surface, &count, nullptr));
  std::vector<VkSurfaceFormatKHR> formats(count);
  CHECK(vkGetPhysicalDeviceSurfaceFormatsKHR(physical, surface, &count, formats.data()));
  auto format = std::find_if(formats.begin(), formats.end(), [](auto f) { return f.format == VK_FORMAT_B8G8R8A8_UNORM; });
  REQUIRE(format != formats.end());
  VkSwapchainCreateInfoKHR ci = { VK_STRUCTURE_TYPE_SWAPCHAIN_CREATE_INFO_KHR };
  ci.surface = surface;
  ci.minImageCount = std::max(caps.minImageCount, 3u);
  REQUIRE(!caps.maxImageCount || ci.minImageCount <= caps.maxImageCount);
  ci.imageFormat = format->format;
  ci.imageColorSpace = format->colorSpace;
  ci.imageExtent = caps.currentExtent;
  ci.imageArrayLayers = 1;
  ci.imageUsage = VK_IMAGE_USAGE_TRANSFER_DST_BIT;
  ci.imageSharingMode = VK_SHARING_MODE_EXCLUSIVE;
  ci.preTransform = caps.currentTransform;
  ci.compositeAlpha = VK_COMPOSITE_ALPHA_OPAQUE_BIT_KHR;
  ci.presentMode = VK_PRESENT_MODE_FIFO_KHR;
  ci.clipped = VK_TRUE;
  VkSwapchainKHR chain = VK_NULL_HANDLE;
  CHECK(vkCreateSwapchainKHR(device, &ci, nullptr, &chain));
  CHECK(vkGetSwapchainImagesKHR(device, chain, &count, nullptr));
  std::vector<VkImage> images(count);
  CHECK(vkGetSwapchainImagesKHR(device, chain, &count, images.data()));
  std::vector<VkSemaphore> rendered(count);
  VkSemaphoreCreateInfo sem_ci = { VK_STRUCTURE_TYPE_SEMAPHORE_CREATE_INFO };
  VkSemaphore acquired;
  CHECK(vkCreateSemaphore(device, &sem_ci, nullptr, &acquired));
  for (auto& semaphore : rendered) CHECK(vkCreateSemaphore(device, &sem_ci, nullptr, &semaphore));
  // Finite GPU work, all submitted before Present's binary semaphore wait.
  // A future host-signaled timeline dependency would violate Present's
  // dependent-signal submission contract and could deadlock this probe.
  VkBufferCreateInfo buffer_ci = { VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO };
  buffer_ci.size = 256ull * 1024 * 1024;
  buffer_ci.usage = VK_BUFFER_USAGE_TRANSFER_DST_BIT;
  buffer_ci.sharingMode = VK_SHARING_MODE_EXCLUSIVE;
  VkBuffer work_buffer;
  CHECK(vkCreateBuffer(device, &buffer_ci, nullptr, &work_buffer));
  VkMemoryRequirements requirements;
  vkGetBufferMemoryRequirements(device, work_buffer, &requirements);
  VkPhysicalDeviceMemoryProperties memory_properties;
  vkGetPhysicalDeviceMemoryProperties(physical, &memory_properties);
  VkMemoryAllocateInfo memory_ci = { VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO };
  memory_ci.allocationSize = requirements.size;
  memory_ci.memoryTypeIndex = UINT32_MAX;
  for (uint32_t i = 0; i < memory_properties.memoryTypeCount; ++i) {
    if ((requirements.memoryTypeBits & (1u << i)) &&
        (memory_properties.memoryTypes[i].propertyFlags & VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT)) {
      memory_ci.memoryTypeIndex = i;
      break;
    }
  }
  REQUIRE(memory_ci.memoryTypeIndex != UINT32_MAX);
  VkDeviceMemory work_memory;
  CHECK(vkAllocateMemory(device, &memory_ci, nullptr, &work_memory));
  CHECK(vkBindBufferMemory(device, work_buffer, work_memory, 0));
  VkFenceCreateInfo fence_ci = { VK_STRUCTURE_TYPE_FENCE_CREATE_INFO };
  VkFence submitted, acquired_fence;
  CHECK(vkCreateFence(device, &fence_ci, nullptr, &submitted));
  CHECK(vkCreateFence(device, &fence_ci, nullptr, &acquired_fence));
  VkCommandPoolCreateInfo pool_ci = { VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO };
  pool_ci.queueFamilyIndex = family;
  pool_ci.flags = VK_COMMAND_POOL_CREATE_RESET_COMMAND_BUFFER_BIT;
  VkCommandPool pool;
  CHECK(vkCreateCommandPool(device, &pool_ci, nullptr, &pool));
  VkCommandBufferAllocateInfo alloc = { VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO };
  alloc.commandPool = pool;
  alloc.level = VK_COMMAND_BUFFER_LEVEL_PRIMARY;
  alloc.commandBufferCount = 1;
  VkCommandBuffer cmd;
  CHECK(vkAllocateCommandBuffers(device, &alloc, &cmd));

  auto present = [&](bool delayed, uint32_t frame) {
    uint32_t index = UINT32_MAX;
    CHECK(vkAcquireNextImageKHR(device, chain, 5000000000ull, acquired, VK_NULL_HANDLE, &index));
    CHECK(vkResetCommandBuffer(cmd, 0));
    VkCommandBufferBeginInfo begin = { VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO };
    CHECK(vkBeginCommandBuffer(cmd, &begin));
    if (delayed) {
      VkMemoryBarrier order = { VK_STRUCTURE_TYPE_MEMORY_BARRIER };
      order.srcAccessMask = order.dstAccessMask = VK_ACCESS_TRANSFER_WRITE_BIT;
      for (uint32_t i = 0; i < 2048; ++i) {
        vkCmdFillBuffer(cmd, work_buffer, 0, buffer_ci.size, i);
        vkCmdPipelineBarrier(cmd, VK_PIPELINE_STAGE_TRANSFER_BIT, VK_PIPELINE_STAGE_TRANSFER_BIT,
          0, 1, &order, 0, nullptr, 0, nullptr);
      }
    }
    VkImageMemoryBarrier barrier = { VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER };
    barrier.dstAccessMask = VK_ACCESS_TRANSFER_WRITE_BIT;
    barrier.oldLayout = VK_IMAGE_LAYOUT_UNDEFINED;
    barrier.newLayout = VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL;
    barrier.srcQueueFamilyIndex = barrier.dstQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED;
    barrier.image = images[index];
    barrier.subresourceRange = { VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1 };
    vkCmdPipelineBarrier(cmd, VK_PIPELINE_STAGE_TOP_OF_PIPE_BIT, VK_PIPELINE_STAGE_TRANSFER_BIT, 0, 0, nullptr, 0, nullptr, 1, &barrier);
    VkClearColorValue color = {};
    color.float32[0] = float(frame % 60) / 60;
    color.float32[1] = 0.4f;
    color.float32[3] = 1.0f;
    vkCmdClearColorImage(cmd, images[index], VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, &color, 1, &barrier.subresourceRange);
    barrier.srcAccessMask = VK_ACCESS_TRANSFER_WRITE_BIT;
    barrier.dstAccessMask = 0;
    barrier.oldLayout = VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL;
    barrier.newLayout = VK_IMAGE_LAYOUT_PRESENT_SRC_KHR;
    vkCmdPipelineBarrier(cmd, VK_PIPELINE_STAGE_TRANSFER_BIT, VK_PIPELINE_STAGE_BOTTOM_OF_PIPE_BIT, 0, 0, nullptr, 0, nullptr, 1, &barrier);
    CHECK(vkEndCommandBuffer(cmd));
    VkPipelineStageFlags stage = VK_PIPELINE_STAGE_TRANSFER_BIT;
    VkSubmitInfo submit = { VK_STRUCTURE_TYPE_SUBMIT_INFO };
    submit.waitSemaphoreCount = 1;
    submit.pWaitSemaphores = &acquired;
    submit.pWaitDstStageMask = &stage;
    submit.commandBufferCount = 1;
    submit.pCommandBuffers = &cmd;
    submit.signalSemaphoreCount = 1;
    submit.pSignalSemaphores = &rendered[index];
    CHECK(vkResetFences(device, 1, &submitted));
    CHECK(vkQueueSubmit(queue, 1, &submit, submitted));
    VkPresentInfoKHR pi = { VK_STRUCTURE_TYPE_PRESENT_INFO_KHR };
    pi.waitSemaphoreCount = 1;
    pi.pWaitSemaphores = &rendered[index];
    pi.swapchainCount = 1;
    pi.pSwapchains = &chain;
    pi.pImageIndices = &index;
    CHECK(vkQueuePresentKHR(queue, &pi));
    if (!delayed) CHECK(vkWaitForFences(device, 1, &submitted, VK_TRUE, 5000000000ull));
    pump();
    return index;
  };

  const auto warmup_end = GetTickCount64() + 10000;
  uint32_t frame = 0;
  do { present(false, frame++); Sleep(10); } while (GetTickCount64() < warmup_end);
  std::printf("warmup frames=%u; submitting finite delayed frame\n", frame);
  // This single chain starts its WSI producer at zero and increments once
  // per successful Present. Bind diagnostic acceptance to this frame only.
  std::printf("delayed_producer=%llu\n", static_cast<unsigned long long>(frame) + 1);
  std::fflush(stdout);
  const auto delayed_start = GetTickCount64();
  const uint32_t source = present(true, frame);
  std::vector<bool> held(count, false);
  for (uint32_t i = 1; i < count; ++i) {
    uint32_t index = UINT32_MAX;
    CHECK(vkAcquireNextImageKHR(device, chain, 5000000000ull, VK_NULL_HANDLE, acquired_fence, &index));
    CHECK(vkWaitForFences(device, 1, &acquired_fence, VK_TRUE, 5000000000ull));
    CHECK(vkResetFences(device, 1, &acquired_fence));
    REQUIRE(index != source && !held[index]);
    held[index] = true;
  }
  Sleep(80);
  const VkResult source_status = vkGetFenceStatus(device, submitted);
  REQUIRE(source_status == VK_SUCCESS || source_status == VK_NOT_READY);
  uint32_t index = UINT32_MAX;
  VkResult acquire_status = vkAcquireNextImageKHR(device, chain, 0, VK_NULL_HANDLE, acquired_fence, &index);
  REQUIRE(acquire_status == VK_NOT_READY || acquire_status == VK_SUCCESS);
  const bool initially_unavailable = acquire_status == VK_NOT_READY;
  if (initially_unavailable) {
    acquire_status = vkAcquireNextImageKHR(device, chain, 50000000, VK_NULL_HANDLE, acquired_fence, &index);
    REQUIRE(acquire_status == VK_TIMEOUT || acquire_status == VK_SUCCESS);
  }
  const bool timeout_proven = source_status == VK_NOT_READY && initially_unavailable && acquire_status == VK_TIMEOUT;
  if (acquire_status == VK_SUCCESS) {
    REQUIRE(index == source);
  }
  std::printf("%s: source wait observed for %llu ms\n",
    timeout_proven ? "PASS pending" : "INCONCLUSIVE pending (work completed during observation)",
    static_cast<unsigned long long>(GetTickCount64() - delayed_start));
  CHECK(vkWaitForFences(device, 1, &submitted, VK_TRUE, 5000000000ull));
  if (acquire_status != VK_SUCCESS)
    CHECK(vkAcquireNextImageKHR(device, chain, 5000000000ull, VK_NULL_HANDLE, acquired_fence, &index));
  CHECK(vkWaitForFences(device, 1, &acquired_fence, VK_TRUE, 5000000000ull));
  REQUIRE(index == source);
  std::puts("PASS completion: the retained source became acquirable");

  vkDestroySwapchainKHR(device, chain, nullptr);
  vkDestroyCommandPool(device, pool, nullptr);
  vkDestroyBuffer(device, work_buffer, nullptr);
  vkFreeMemory(device, work_memory, nullptr);
  vkDestroyFence(device, acquired_fence, nullptr);
  vkDestroyFence(device, submitted, nullptr);
  for (auto semaphore : rendered) vkDestroySemaphore(device, semaphore, nullptr);
  vkDestroySemaphore(device, acquired, nullptr);
  vkDestroyDevice(device, nullptr);
  vkDestroySurfaceKHR(instance, surface, nullptr);
  vkDestroyInstance(instance, nullptr);
  DestroyWindow(hwnd);
  FreeLibrary(module);
  std::puts(timeout_proven ? "PASS (require matching vehicle LIVE and pending/completed diagnostics)" : "INCONCLUSIVE");
  return timeout_proven ? 0 : 3;
}
