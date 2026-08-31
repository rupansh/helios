/*
 * dmabuf_crosstype_alias_probe.c — does an NVIDIA dma-buf import into a
 * DIFFERENT memory type than the exporter's still alias the same pages?
 *
 * 2026-08-31. The guest chain for the black desktop is exonerated link by
 * link: DWM's composition batches name the flip-buffer allocations with WRITE
 * uses, the KMD patches the deferred import with the same resource id it scans
 * out, virglrenderer turns VkImportMemoryResourceInfoMESA into a dup-fd
 * VkImportMemoryFdInfoKHR, the host executes with zero refusals — and the
 * exported fd's pages still read zero. The one unvalidated link: the KMD
 * allocates the blob from the HOST_VISIBLE|HOST_COHERENT type, while the
 * ICD's deferred import requests the translated device-local index
 * (HAM2 renderer_type=1), and nothing checks that index against
 * vkGetMemoryFdPropertiesKHR's mask. This reproduces the exact shape on the
 * host GPU, outside the whole stack:
 *
 *   1. allocate 4587520 B from the HOST_VISIBLE|HOST_COHERENT type,
 *      exportable as DMA_BUF (like the KMD's allocate_memory_blob);
 *   2. vkGetMemoryFdPropertiesKHR on the exported fd — print the mask;
 *   3. import the fd into a second VkDeviceMemory at EACH memory type index
 *      (spec-legal or not — the ICD does not check either);
 *   4. for the interesting types (the exporter's own and the ICD's device-
 *      local translation): bind a 1280x800 BGRA OPTIMAL external image to the
 *      IMPORTED memory, vkCmdClearColorImage a distinct color, then read the
 *      ORIGINAL memory's mapping raw and count nonzero bytes.
 *
 * nonzero != 0 on the exporter-type arm but == 0 on the cross-type arm names
 * the divergence exactly. Build:
 *   gcc -O1 -o /tmp/dmabuf_crosstype_alias_probe \
 *       tools/dmabuf_crosstype_alias_probe.c -lvulkan
 */
#include <vulkan/vulkan.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#define CHECK(x) do { VkResult _r = (x); if (_r != VK_SUCCESS) { \
  printf("  %s -> %d\n", #x, _r); return; } } while (0)

static VkInstance inst;
static VkPhysicalDevice pdev;
static VkDevice dev;
static VkQueue queue;
static uint32_t qfam;
static VkPhysicalDeviceMemoryProperties memprops;
static PFN_vkGetMemoryFdKHR pGetFd;
static PFN_vkGetMemoryFdPropertiesKHR pGetFdProps;

static const VkDeviceSize SZ = 4587520;
static const uint32_t W = 1280, H = 800;

static uint32_t host_visible_type(void) {
  for (uint32_t i = 0; i < memprops.memoryTypeCount; i++) {
    VkMemoryPropertyFlags f = memprops.memoryTypes[i].propertyFlags;
    if ((f & VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT) &&
        (f & VK_MEMORY_PROPERTY_HOST_COHERENT_BIT))
      return i;
  }
  return UINT32_MAX;
}

/* image_ext: 0 = no VkExternalMemoryImageCreateInfo at all;
 * otherwise the handleTypes the image declares (may mismatch the import). */
static VkExternalMemoryHandleTypeFlags g_image_ext =
  VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT;

/* One arm: import `fd` at `type_index`, bind an OPTIMAL external image to the
 * IMPORT, clear it, read the ORIGINAL allocation's mapping. */
static void arm(int fd, uint32_t type_index, void *orig_map) {
  printf("ARM import type=%u flags=0x%x image_ext=0x%x:\n", type_index,
         memprops.memoryTypes[type_index].propertyFlags,
         (unsigned)g_image_ext);
  memset(orig_map, 0, SZ);   /* start from known-zero pages */

  int dupfd = dup(fd);
  VkImportMemoryFdInfoKHR imp = {
    .sType = VK_STRUCTURE_TYPE_IMPORT_MEMORY_FD_INFO_KHR,
    .handleType = VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT,
    .fd = dupfd,
  };
  VkMemoryAllocateInfo ai = {
    .sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,
    .pNext = &imp,
    .allocationSize = SZ,
    .memoryTypeIndex = type_index,
  };
  VkDeviceMemory mem2 = VK_NULL_HANDLE;
  VkResult r = vkAllocateMemory(dev, &ai, NULL, &mem2);
  printf("  import vkAllocateMemory -> %d\n", r);
  if (r != VK_SUCCESS) { close(dupfd); return; }

  VkExternalMemoryImageCreateInfo ext = {
    .sType = VK_STRUCTURE_TYPE_EXTERNAL_MEMORY_IMAGE_CREATE_INFO,
    .handleTypes = g_image_ext,
  };
  VkImageCreateInfo ici = {
    .sType = VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO,
    .pNext = g_image_ext ? &ext : NULL,
    .imageType = VK_IMAGE_TYPE_2D,
    .format = VK_FORMAT_B8G8R8A8_UNORM,
    .extent = { W, H, 1 },
    .mipLevels = 1, .arrayLayers = 1,
    .samples = VK_SAMPLE_COUNT_1_BIT,
    .tiling = VK_IMAGE_TILING_OPTIMAL,
    .usage = VK_IMAGE_USAGE_TRANSFER_SRC_BIT | VK_IMAGE_USAGE_TRANSFER_DST_BIT |
             VK_IMAGE_USAGE_SAMPLED_BIT | VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT,
    .sharingMode = VK_SHARING_MODE_EXCLUSIVE,
    .initialLayout = VK_IMAGE_LAYOUT_UNDEFINED,
  };
  VkImage img = VK_NULL_HANDLE;
  CHECK(vkCreateImage(dev, &ici, NULL, &img));
  VkMemoryRequirements req;
  vkGetImageMemoryRequirements(dev, img, &req);
  printf("  image reqs: size=%llu align=%llu typebits=0x%x (type %u %s)\n",
         (unsigned long long)req.size, (unsigned long long)req.alignment,
         req.memoryTypeBits, type_index,
         (req.memoryTypeBits >> type_index) & 1 ? "allowed" : "NOT-ALLOWED");
  if (req.size > SZ) { printf("  image needs %llu > %llu, skipping bind\n",
    (unsigned long long)req.size, (unsigned long long)SZ);
    vkDestroyImage(dev, img, NULL); vkFreeMemory(dev, mem2, NULL); return; }
  r = vkBindImageMemory(dev, img, mem2, 0);
  printf("  vkBindImageMemory -> %d\n", r);
  if (r != VK_SUCCESS) { vkDestroyImage(dev, img, NULL); vkFreeMemory(dev, mem2, NULL); return; }

  VkCommandPool pool; VkCommandBuffer cb;
  VkCommandPoolCreateInfo pci = { .sType = VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO,
                                  .queueFamilyIndex = qfam };
  CHECK(vkCreateCommandPool(dev, &pci, NULL, &pool));
  VkCommandBufferAllocateInfo cai = { .sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO,
    .commandPool = pool, .level = VK_COMMAND_BUFFER_LEVEL_PRIMARY, .commandBufferCount = 1 };
  CHECK(vkAllocateCommandBuffers(dev, &cai, &cb));
  VkCommandBufferBeginInfo bi = { .sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO };
  CHECK(vkBeginCommandBuffer(cb, &bi));
  VkImageMemoryBarrier bar = {
    .sType = VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER,
    .srcAccessMask = 0, .dstAccessMask = VK_ACCESS_TRANSFER_WRITE_BIT,
    .oldLayout = VK_IMAGE_LAYOUT_UNDEFINED,
    .newLayout = VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
    .srcQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED,
    .dstQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED,
    .image = img,
    .subresourceRange = { VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1 },
  };
  vkCmdPipelineBarrier(cb, VK_PIPELINE_STAGE_TOP_OF_PIPE_BIT,
                       VK_PIPELINE_STAGE_TRANSFER_BIT, 0, 0, NULL, 0, NULL, 1, &bar);
  VkClearColorValue col = { .float32 = { 0.5f, 0.25f, 0.75f, 1.0f } };
  VkImageSubresourceRange rng = { VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1 };
  vkCmdClearColorImage(cb, img, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, &col, 1, &rng);
  CHECK(vkEndCommandBuffer(cb));
  VkSubmitInfo si = { .sType = VK_STRUCTURE_TYPE_SUBMIT_INFO,
                      .commandBufferCount = 1, .pCommandBuffers = &cb };
  CHECK(vkQueueSubmit(queue, 1, &si, VK_NULL_HANDLE));
  CHECK(vkQueueWaitIdle(dev ? queue : queue));

  /* read the ORIGINAL memory's mapping raw */
  const unsigned char *p = (const unsigned char *)orig_map;
  size_t nz = 0; unsigned mx = 0; size_t first = SZ;
  for (size_t i = 0; i < SZ; i += 251) {  /* strided sample */
    if (p[i]) { nz++; if (p[i] > mx) mx = p[i]; if (first == SZ) first = i; }
  }
  printf("  RAW original mapping after GPU clear: strided_nonzero=%zu/%zu max=%u first_off=%zu  %s\n",
         nz, (size_t)(SZ / 251) + 1, mx, first,
         nz ? "ALIASED (writes visible)" : "ZERO (writes NOT visible)");

  vkDestroyCommandPool(dev, pool, NULL);
  vkDestroyImage(dev, img, NULL);
  vkFreeMemory(dev, mem2, NULL);
}

int main(void) {
  VkApplicationInfo app = { .sType = VK_STRUCTURE_TYPE_APPLICATION_INFO,
    .pApplicationName = "dmabuf_crosstype_alias_probe", .apiVersion = VK_API_VERSION_1_2 };
  VkInstanceCreateInfo ici = { .sType = VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO,
    .pApplicationInfo = &app };
  if (vkCreateInstance(&ici, NULL, &inst) != VK_SUCCESS) { printf("no instance\n"); return 1; }
  uint32_t n = 1;
  vkEnumeratePhysicalDevices(inst, &n, &pdev);
  VkPhysicalDeviceProperties props; vkGetPhysicalDeviceProperties(pdev, &props);
  printf("device: %s\n", props.deviceName);
  vkGetPhysicalDeviceMemoryProperties(pdev, &memprops);
  for (uint32_t i = 0; i < memprops.memoryTypeCount; i++)
    printf("  type %2u: flags=0x%04x heap=%u\n", i,
           memprops.memoryTypes[i].propertyFlags, memprops.memoryTypes[i].heapIndex);

  uint32_t nq = 0; vkGetPhysicalDeviceQueueFamilyProperties(pdev, &nq, NULL);
  VkQueueFamilyProperties qf[16]; if (nq > 16) nq = 16;
  vkGetPhysicalDeviceQueueFamilyProperties(pdev, &nq, qf);
  for (qfam = 0; qfam < nq; qfam++)
    if (qf[qfam].queueFlags & VK_QUEUE_GRAPHICS_BIT) break;

  float prio = 1.0f;
  VkDeviceQueueCreateInfo qci = { .sType = VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO,
    .queueFamilyIndex = qfam, .queueCount = 1, .pQueuePriorities = &prio };
  const char *exts[] = { "VK_KHR_external_memory_fd", "VK_EXT_external_memory_dma_buf" };
  VkDeviceCreateInfo dci = { .sType = VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO,
    .queueCreateInfoCount = 1, .pQueueCreateInfos = &qci,
    .enabledExtensionCount = 2, .ppEnabledExtensionNames = exts };
  if (vkCreateDevice(pdev, &dci, NULL, &dev) != VK_SUCCESS) { printf("no device\n"); return 1; }
  vkGetDeviceQueue(dev, qfam, 0, &queue);
  pGetFd = (PFN_vkGetMemoryFdKHR)vkGetDeviceProcAddr(dev, "vkGetMemoryFdKHR");
  pGetFdProps = (PFN_vkGetMemoryFdPropertiesKHR)vkGetDeviceProcAddr(dev, "vkGetMemoryFdPropertiesKHR");

  uint32_t hv = host_visible_type();
  printf("exporter: HOST_VISIBLE|HOST_COHERENT type = %u\n", hv);

  VkExportMemoryAllocateInfo exp = { .sType = VK_STRUCTURE_TYPE_EXPORT_MEMORY_ALLOCATE_INFO,
    .handleTypes = VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT };
  VkMemoryAllocateInfo ai = { .sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,
    .pNext = &exp, .allocationSize = SZ, .memoryTypeIndex = hv };
  VkDeviceMemory orig = VK_NULL_HANDLE;
  VkResult r = vkAllocateMemory(dev, &ai, NULL, &orig);
  printf("export vkAllocateMemory(type %u, DMA_BUF) -> %d\n", hv, r);
  if (r != VK_SUCCESS) return 1;
  void *map = NULL;
  if (vkMapMemory(dev, orig, 0, SZ, 0, &map) != VK_SUCCESS) { printf("map failed\n"); return 1; }

  VkMemoryGetFdInfoKHR gfi = { .sType = VK_STRUCTURE_TYPE_MEMORY_GET_FD_INFO_KHR,
    .memory = orig, .handleType = VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT };
  int fd = -1;
  r = pGetFd(dev, &gfi, &fd);
  printf("vkGetMemoryFdKHR -> %d fd=%d\n", r, fd);
  if (r != VK_SUCCESS) return 1;

  VkMemoryFdPropertiesKHR fdp = { .sType = VK_STRUCTURE_TYPE_MEMORY_FD_PROPERTIES_KHR };
  r = pGetFdProps(dev, VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT, fd, &fdp);
  printf("vkGetMemoryFdPropertiesKHR -> %d memoryTypeBits=0x%x\n", r, fdp.memoryTypeBits);

  /* The two arms that matter: the exporter's own type, and type 1 (the ICD's
   * translated device-local index in the failing HAM2 lines). Then any other
   * type in the fd mask, for completeness. */
  arm(fd, hv, map);
  if (hv != 1 && 1 < memprops.memoryTypeCount) arm(fd, 1, map);
  /* the live-stack suspicion: image declares OPAQUE_FD (venus advertisement)
   * while the memory import is DMA_BUF */
  g_image_ext = VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD_BIT;
  arm(fd, hv, map);
  arm(fd, 1, map);
  /* and: image declares NOTHING at all */
  g_image_ext = 0;
  arm(fd, hv, map);
  arm(fd, 1, map);
  printf("done\n");
  return 0;
}
