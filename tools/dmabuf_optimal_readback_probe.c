/*
 * dmabuf_optimal_readback_probe.c — can qemu-helios' vulkan-readback OPTIMAL
 * arm actually read what a foreign-device OPTIMAL producer rendered?
 *
 * 2026-08-31. Two-reader divergence measured live on KMD 22.22.428.0: at the
 * SAME res_flush of the same primary (res 50), the KMD's raw mapping of the
 * blob counted 16384/64000 nonzero bytes while helios_scanout_read (which
 * samples the DisplaySurface AFTER helios_vulkan_readback_flush) reported
 * nonzero 0. The guest chain is fully witnessed (HBI1/A7 assoc/Nr2PImp*), and
 * the positive control — the KMD's LINEAR parking blob — only ever exercised
 * the vk-linear arm. This reproduces the vk-optimal consumer against a known
 * producer, all on the host GPU:
 *
 *   producer device P: 1280x800 B8G8R8A8 OPTIMAL image, usage 0x17 (SAMPLED|
 *   COLOR_ATTACHMENT|TRANSFER_SRC|TRANSFER_DST), dedicated exportable
 *   DEVICE_LOCAL memory, vkCmdClearColorImage(green) [+ arm: render-pass
 *   draw], final layout SHADER_READ_ONLY_OPTIMAL, dma-buf exported.
 *
 *   consumer device R: byte-for-byte the qemu-helios vulkan-readback consumer
 *   (ui/vulkan-readback.c): import dma-buf, OPTIMAL image usage 0x17 +
 *   MUTABLE_FORMAT, dedicated bind, acquire EXTERNAL->queue with
 *   oldLayout=SHADER_READ_ONLY_OPTIMAL, vkCmdCopyImageToBuffer, count.
 *
 *   raw check: import the same fd into a HOST_VISIBLE type on R and count
 *   nonzero bytes by CPU — the pages ground truth, same as the KMD probe.
 *
 * Arms vary one ingredient at a time:
 *   A producer image declares DMA_BUF external          (readback happy case)
 *   B producer image declares OPAQUE_FD but exports DMA_BUF (the venus shape:
 *     NVIDIA-bound venus rewrites DMA_BUF->OPAQUE_FD on the image)
 *   C producer image declares NO external info (the vkr "XXX force export"
 *     spec violation: external memory bound to non-external image)
 *   D producer leaves layout COLOR_ATTACHMENT_OPTIMAL (consumer still assumes
 *     SHADER_READ_ONLY — the real DWM frame never saw a readback barrier)
 *   E consumer uses oldLayout=GENERAL and no queue-family transfer
 *
 * Build (host):
 *   gcc -O1 -o /tmp/dmabuf_optimal_readback_probe \
 *       tools/dmabuf_optimal_readback_probe.c -lvulkan
 */
#include <vulkan/vulkan.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#define CHECK(x) do { VkResult _r = (x); if (_r != VK_SUCCESS) { \
  printf("  %s -> %d\n", #x, _r); return -1; } } while (0)

static const uint32_t W = 1280, H = 800;
static const uint32_t USAGE_017 =
    VK_IMAGE_USAGE_TRANSFER_SRC_BIT | VK_IMAGE_USAGE_TRANSFER_DST_BIT |
    VK_IMAGE_USAGE_SAMPLED_BIT | VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT;

typedef struct {
  VkInstance inst;
  VkPhysicalDevice pdev;
  VkDevice dev;
  VkQueue queue;
  uint32_t qfam;
  VkPhysicalDeviceMemoryProperties memprops;
  VkCommandPool pool;
  VkCommandBuffer cmd;
  VkFence fence;
} Ctx;

static int ctx_init(Ctx *c) {
  VkApplicationInfo app = { .sType = VK_STRUCTURE_TYPE_APPLICATION_INFO,
                            .apiVersion = VK_API_VERSION_1_2 };
  VkInstanceCreateInfo ii = { .sType = VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO,
                              .pApplicationInfo = &app };
  CHECK(vkCreateInstance(&ii, NULL, &c->inst));
  uint32_t n = 1;
  vkEnumeratePhysicalDevices(c->inst, &n, &c->pdev);
  if (!n) { printf("no phys dev\n"); return -1; }
  vkGetPhysicalDeviceMemoryProperties(c->pdev, &c->memprops);
  uint32_t qn = 0;
  vkGetPhysicalDeviceQueueFamilyProperties(c->pdev, &qn, NULL);
  VkQueueFamilyProperties *qf = calloc(qn, sizeof(*qf));
  vkGetPhysicalDeviceQueueFamilyProperties(c->pdev, &qn, qf);
  c->qfam = UINT32_MAX;
  for (uint32_t i = 0; i < qn; i++)
    if (qf[i].queueFlags & VK_QUEUE_GRAPHICS_BIT) { c->qfam = i; break; }
  free(qf);
  float prio = 1.0f;
  VkDeviceQueueCreateInfo qi = {
    .sType = VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO,
    .queueFamilyIndex = c->qfam, .queueCount = 1, .pQueuePriorities = &prio };
  const char *exts[] = {
    "VK_KHR_external_memory", "VK_KHR_external_memory_fd",
    "VK_EXT_external_memory_dma_buf", "VK_KHR_dedicated_allocation",
    "VK_KHR_get_memory_requirements2" };
  VkDeviceCreateInfo di = { .sType = VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO,
    .queueCreateInfoCount = 1, .pQueueCreateInfos = &qi,
    .enabledExtensionCount = 5, .ppEnabledExtensionNames = exts };
  CHECK(vkCreateDevice(c->pdev, &di, NULL, &c->dev));
  vkGetDeviceQueue(c->dev, c->qfam, 0, &c->queue);
  VkCommandPoolCreateInfo pi = {
    .sType = VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO,
    .flags = VK_COMMAND_POOL_CREATE_RESET_COMMAND_BUFFER_BIT,
    .queueFamilyIndex = c->qfam };
  CHECK(vkCreateCommandPool(c->dev, &pi, NULL, &c->pool));
  VkCommandBufferAllocateInfo ci = {
    .sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO,
    .commandPool = c->pool, .level = VK_COMMAND_BUFFER_LEVEL_PRIMARY,
    .commandBufferCount = 1 };
  CHECK(vkAllocateCommandBuffers(c->dev, &ci, &c->cmd));
  VkFenceCreateInfo fi = { .sType = VK_STRUCTURE_TYPE_FENCE_CREATE_INFO };
  CHECK(vkCreateFence(c->dev, &fi, NULL, &c->fence));
  return 0;
}

static int submit_wait(Ctx *c) {
  CHECK(vkEndCommandBuffer(c->cmd));
  VkSubmitInfo si = { .sType = VK_STRUCTURE_TYPE_SUBMIT_INFO,
    .commandBufferCount = 1, .pCommandBuffers = &c->cmd };
  CHECK(vkQueueSubmit(c->queue, 1, &si, c->fence));
  CHECK(vkWaitForFences(c->dev, 1, &c->fence, VK_TRUE, UINT64_MAX));
  CHECK(vkResetFences(c->dev, 1, &c->fence));
  CHECK(vkResetCommandBuffer(c->cmd, 0));
  return 0;
}

static int begin(Ctx *c) {
  VkCommandBufferBeginInfo bi = {
    .sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO,
    .flags = VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT };
  CHECK(vkBeginCommandBuffer(c->cmd, &bi));
  return 0;
}

static uint32_t mem_type(Ctx *c, uint32_t bits, VkMemoryPropertyFlags want) {
  for (uint32_t i = 0; i < c->memprops.memoryTypeCount; i++)
    if ((bits & (1u << i)) &&
        (c->memprops.memoryTypes[i].propertyFlags & want) == want)
      return i;
  return UINT32_MAX;
}

/* Producer: build image, write green, export. Returns fd or -1.
 * image_ext: handleTypes the image declares (0 = none).
 * final_layout: layout of the image when the producer is done.
 * dedicated/host_visible: the REAL venus blob is a NON-dedicated allocation
 * from a mappable type that the image binds afterward — the first probe run
 * proved the dedicated DEVICE_LOCAL shape reads back fine in every arm. */
static int g_dedicated = 1;
static int g_host_visible = 0;
static int g_attachment_write = 0;
static int producer(Ctx *p, VkExternalMemoryHandleTypeFlags image_ext,
                    VkImageLayout final_layout, VkDeviceSize *out_size) {
  VkExternalMemoryImageCreateInfo ext = {
    .sType = VK_STRUCTURE_TYPE_EXTERNAL_MEMORY_IMAGE_CREATE_INFO,
    .handleTypes = image_ext };
  VkImageCreateInfo ici = {
    .sType = VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO,
    .pNext = image_ext ? &ext : NULL,
    .imageType = VK_IMAGE_TYPE_2D,
    .format = VK_FORMAT_B8G8R8A8_UNORM,
    .extent = { W, H, 1 }, .mipLevels = 1, .arrayLayers = 1,
    .samples = VK_SAMPLE_COUNT_1_BIT,
    .tiling = VK_IMAGE_TILING_OPTIMAL,
    .usage = USAGE_017,
    .sharingMode = VK_SHARING_MODE_EXCLUSIVE,
    .initialLayout = VK_IMAGE_LAYOUT_UNDEFINED };
  VkImage img;
  CHECK(vkCreateImage(p->dev, &ici, NULL, &img));
  VkMemoryRequirements req;
  vkGetImageMemoryRequirements(p->dev, img, &req);
  *out_size = req.size;

  VkExportMemoryAllocateInfo exp = {
    .sType = VK_STRUCTURE_TYPE_EXPORT_MEMORY_ALLOCATE_INFO,
    .handleTypes = VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT };
  VkMemoryDedicatedAllocateInfo ded = {
    .sType = VK_STRUCTURE_TYPE_MEMORY_DEDICATED_ALLOCATE_INFO,
    .pNext = &exp, .image = img };
  uint32_t ti = mem_type(p, req.memoryTypeBits,
                         g_host_visible
                             ? (VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT |
                                VK_MEMORY_PROPERTY_HOST_COHERENT_BIT)
                             : VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT);
  if (ti == UINT32_MAX) { printf("  producer: no such memory type\n");
                          return -1; }
  printf("  producer mem type=%u flags=0x%x dedicated=%d\n", ti,
         p->memprops.memoryTypes[ti].propertyFlags, g_dedicated);
  VkMemoryAllocateInfo ai = { .sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,
    .pNext = g_dedicated ? (const void *)&ded : (const void *)&exp,
    .allocationSize = req.size, .memoryTypeIndex = ti };
  VkDeviceMemory mem;
  CHECK(vkAllocateMemory(p->dev, &ai, NULL, &mem));
  CHECK(vkBindImageMemory(p->dev, img, mem, 0));

  if (begin(p)) return -1;
  if (g_attachment_write) {
    /* Write through the ATTACHMENT path (render-pass loadOp clear), the way a
     * compositor's draws engage color compression — not the transfer engine. */
    VkAttachmentDescription att = {
      .format = VK_FORMAT_B8G8R8A8_UNORM,
      .samples = VK_SAMPLE_COUNT_1_BIT,
      .loadOp = VK_ATTACHMENT_LOAD_OP_CLEAR,
      .storeOp = VK_ATTACHMENT_STORE_OP_STORE,
      .stencilLoadOp = VK_ATTACHMENT_LOAD_OP_DONT_CARE,
      .stencilStoreOp = VK_ATTACHMENT_STORE_OP_DONT_CARE,
      .initialLayout = VK_IMAGE_LAYOUT_UNDEFINED,
      .finalLayout = final_layout };
    VkAttachmentReference ref = {
      .attachment = 0, .layout = VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL };
    VkSubpassDescription sub = {
      .pipelineBindPoint = VK_PIPELINE_BIND_POINT_GRAPHICS,
      .colorAttachmentCount = 1, .pColorAttachments = &ref };
    VkRenderPassCreateInfo rpi = {
      .sType = VK_STRUCTURE_TYPE_RENDER_PASS_CREATE_INFO,
      .attachmentCount = 1, .pAttachments = &att,
      .subpassCount = 1, .pSubpasses = &sub };
    VkRenderPass rp;
    CHECK(vkCreateRenderPass(p->dev, &rpi, NULL, &rp));
    VkImageViewCreateInfo vi = {
      .sType = VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO,
      .image = img, .viewType = VK_IMAGE_VIEW_TYPE_2D,
      .format = VK_FORMAT_B8G8R8A8_UNORM,
      .subresourceRange = { VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1 } };
    VkImageView view;
    CHECK(vkCreateImageView(p->dev, &vi, NULL, &view));
    VkFramebufferCreateInfo fbi = {
      .sType = VK_STRUCTURE_TYPE_FRAMEBUFFER_CREATE_INFO,
      .renderPass = rp, .attachmentCount = 1, .pAttachments = &view,
      .width = W, .height = H, .layers = 1 };
    VkFramebuffer fb;
    CHECK(vkCreateFramebuffer(p->dev, &fbi, NULL, &fb));
    VkClearValue clear = { .color = { .float32 = { 0.0f, 1.0f, 0.0f, 1.0f } } };
    VkRenderPassBeginInfo rbi = {
      .sType = VK_STRUCTURE_TYPE_RENDER_PASS_BEGIN_INFO,
      .renderPass = rp, .framebuffer = fb,
      .renderArea = { { 0, 0 }, { W, H } },
      .clearValueCount = 1, .pClearValues = &clear };
    vkCmdBeginRenderPass(p->cmd, &rbi, VK_SUBPASS_CONTENTS_INLINE);
    vkCmdEndRenderPass(p->cmd);
  } else {
    VkImageMemoryBarrier b = {
      .sType = VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER,
      .srcAccessMask = 0, .dstAccessMask = VK_ACCESS_TRANSFER_WRITE_BIT,
      .oldLayout = VK_IMAGE_LAYOUT_UNDEFINED,
      .newLayout = VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
      .srcQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED,
      .dstQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED,
      .image = img,
      .subresourceRange = { VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1 } };
    vkCmdPipelineBarrier(p->cmd, VK_PIPELINE_STAGE_TOP_OF_PIPE_BIT,
                         VK_PIPELINE_STAGE_TRANSFER_BIT, 0, 0, NULL, 0, NULL,
                         1, &b);
    VkClearColorValue green = { .float32 = { 0.0f, 1.0f, 0.0f, 1.0f } };
    VkImageSubresourceRange range = { VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1 };
    vkCmdClearColorImage(p->cmd, img, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
                         &green, 1, &range);
    VkImageMemoryBarrier b2 = {
      .sType = VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER,
      .srcAccessMask = VK_ACCESS_TRANSFER_WRITE_BIT, .dstAccessMask = 0,
      .oldLayout = VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
      .newLayout = final_layout,
      .srcQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED,
      .dstQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED,
      .image = img,
      .subresourceRange = { VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1 } };
    vkCmdPipelineBarrier(p->cmd, VK_PIPELINE_STAGE_TRANSFER_BIT,
                         VK_PIPELINE_STAGE_BOTTOM_OF_PIPE_BIT, 0, 0, NULL,
                         0, NULL, 1, &b2);
  }
  if (submit_wait(p)) return -1;

  PFN_vkGetMemoryFdKHR getfd =
      (PFN_vkGetMemoryFdKHR)vkGetDeviceProcAddr(p->dev, "vkGetMemoryFdKHR");
  VkMemoryGetFdInfoKHR gi = { .sType = VK_STRUCTURE_TYPE_MEMORY_GET_FD_INFO_KHR,
    .memory = mem,
    .handleType = VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT };
  int fd = -1;
  VkResult r = getfd(p->dev, &gi, &fd);
  if (r != VK_SUCCESS) { printf("  GetMemoryFd -> %d\n", r); return -1; }
  /* keep img+mem alive for the process lifetime: the fd references them */
  return fd;
}

/* Consumer: the vulkan-readback OPTIMAL arm, verbatim in shape. */
static int consumer_optimal(Ctx *c, int fd, VkDeviceSize dmabuf_size,
                            VkImageLayout old_layout, int use_external_qf,
                            uint32_t *nonzero, uint32_t *maxb,
                            uint32_t *green_px) {
  VkExternalMemoryImageCreateInfo ext = {
    .sType = VK_STRUCTURE_TYPE_EXTERNAL_MEMORY_IMAGE_CREATE_INFO,
    .handleTypes = VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT };
  VkImageCreateInfo ici = {
    .sType = VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO,
    .pNext = &ext,
    .flags = VK_IMAGE_CREATE_MUTABLE_FORMAT_BIT,
    .imageType = VK_IMAGE_TYPE_2D,
    .format = VK_FORMAT_B8G8R8A8_UNORM,
    .extent = { W, H, 1 }, .mipLevels = 1, .arrayLayers = 1,
    .samples = VK_SAMPLE_COUNT_1_BIT,
    .tiling = VK_IMAGE_TILING_OPTIMAL,
    .usage = USAGE_017,
    .sharingMode = VK_SHARING_MODE_EXCLUSIVE,
    .initialLayout = VK_IMAGE_LAYOUT_UNDEFINED };
  VkImage img;
  CHECK(vkCreateImage(c->dev, &ici, NULL, &img));
  VkMemoryRequirements req;
  vkGetImageMemoryRequirements(c->dev, img, &req);
  if (req.size != dmabuf_size)
    printf("  (shape: consumer req %llu vs fd %llu)\n",
           (unsigned long long)req.size, (unsigned long long)dmabuf_size);

  PFN_vkGetMemoryFdPropertiesKHR getprops =
      (PFN_vkGetMemoryFdPropertiesKHR)vkGetDeviceProcAddr(
          c->dev, "vkGetMemoryFdPropertiesKHR");
  VkMemoryFdPropertiesKHR props = {
    .sType = VK_STRUCTURE_TYPE_MEMORY_FD_PROPERTIES_KHR };
  CHECK(getprops(c->dev, VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT, fd,
                 &props));
  int dupfd = dup(fd);
  VkImportMemoryFdInfoKHR imp = {
    .sType = VK_STRUCTURE_TYPE_IMPORT_MEMORY_FD_INFO_KHR,
    .handleType = VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT,
    .fd = dupfd };
  VkMemoryDedicatedAllocateInfo ded = {
    .sType = VK_STRUCTURE_TYPE_MEMORY_DEDICATED_ALLOCATE_INFO,
    .pNext = &imp, .image = img };
  uint32_t ti = mem_type(c, req.memoryTypeBits & props.memoryTypeBits, 0);
  VkMemoryAllocateInfo ai = { .sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,
    .pNext = &ded, .allocationSize = req.size, .memoryTypeIndex = ti };
  VkDeviceMemory mem;
  CHECK(vkAllocateMemory(c->dev, &ai, NULL, &mem));
  CHECK(vkBindImageMemory(c->dev, img, mem, 0));

  VkBufferCreateInfo bci = { .sType = VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO,
    .size = (VkDeviceSize)W * H * 4,
    .usage = VK_BUFFER_USAGE_TRANSFER_DST_BIT,
    .sharingMode = VK_SHARING_MODE_EXCLUSIVE };
  VkBuffer staging;
  CHECK(vkCreateBuffer(c->dev, &bci, NULL, &staging));
  VkMemoryRequirements sreq;
  vkGetBufferMemoryRequirements(c->dev, staging, &sreq);
  uint32_t sti = mem_type(c, sreq.memoryTypeBits,
                          VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT |
                          VK_MEMORY_PROPERTY_HOST_COHERENT_BIT);
  VkMemoryAllocateInfo sai = { .sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,
    .allocationSize = sreq.size, .memoryTypeIndex = sti };
  VkDeviceMemory smem;
  CHECK(vkAllocateMemory(c->dev, &sai, NULL, &smem));
  CHECK(vkBindBufferMemory(c->dev, staging, smem, 0));
  uint8_t *map;
  CHECK(vkMapMemory(c->dev, smem, 0, VK_WHOLE_SIZE, 0, (void **)&map));
  memset(map, 0xAA, (size_t)W * H * 4);   /* poison: distinguish "copied zero"
                                             from "copy never landed" */

  if (begin(c)) return -1;
  VkImageMemoryBarrier acq = {
    .sType = VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER,
    .srcAccessMask = 0, .dstAccessMask = VK_ACCESS_TRANSFER_READ_BIT,
    .oldLayout = old_layout,
    .newLayout = VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,
    .srcQueueFamilyIndex = use_external_qf ? VK_QUEUE_FAMILY_EXTERNAL
                                           : VK_QUEUE_FAMILY_IGNORED,
    .dstQueueFamilyIndex = use_external_qf ? c->qfam
                                           : VK_QUEUE_FAMILY_IGNORED,
    .image = img,
    .subresourceRange = { VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1 } };
  vkCmdPipelineBarrier(c->cmd, VK_PIPELINE_STAGE_ALL_COMMANDS_BIT,
                       VK_PIPELINE_STAGE_TRANSFER_BIT, 0, 0, NULL, 0, NULL,
                       1, &acq);
  VkBufferImageCopy region = {
    .bufferOffset = 0, .bufferRowLength = W, .bufferImageHeight = H,
    .imageSubresource = { VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1 },
    .imageOffset = { 0, 0, 0 }, .imageExtent = { W, H, 1 } };
  vkCmdCopyImageToBuffer(c->cmd, img, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,
                         staging, 1, &region);
  VkBufferMemoryBarrier host = {
    .sType = VK_STRUCTURE_TYPE_BUFFER_MEMORY_BARRIER,
    .srcAccessMask = VK_ACCESS_TRANSFER_WRITE_BIT,
    .dstAccessMask = VK_ACCESS_HOST_READ_BIT,
    .srcQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED,
    .dstQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED,
    .buffer = staging, .offset = 0, .size = VK_WHOLE_SIZE };
  vkCmdPipelineBarrier(c->cmd, VK_PIPELINE_STAGE_TRANSFER_BIT,
                       VK_PIPELINE_STAGE_HOST_BIT, 0, 0, NULL, 1, &host,
                       0, NULL);
  if (submit_wait(c)) return -1;

  uint32_t nz = 0, mx = 0, green = 0;
  for (size_t px = 0; px < (size_t)W * H; px++) {
    const uint8_t *b = map + px * 4;
    for (int k = 0; k < 4; k++) {
      if (b[k]) nz++;
      if (b[k] > mx) mx = b[k];
    }
    /* B8G8R8A8: B=0 G=255 R=0 A=255 */
    if (b[0] == 0 && b[1] == 0xFF && b[2] == 0 && b[3] == 0xFF) green++;
  }
  *nonzero = nz; *maxb = mx; *green_px = green;
  return 0;
}

/* Raw pages: import into a HOST_VISIBLE type and count by CPU. */
static int raw_pages(Ctx *c, int fd, VkDeviceSize size,
                     uint32_t *nonzero, uint32_t *maxb) {
  PFN_vkGetMemoryFdPropertiesKHR getprops =
      (PFN_vkGetMemoryFdPropertiesKHR)vkGetDeviceProcAddr(
          c->dev, "vkGetMemoryFdPropertiesKHR");
  VkMemoryFdPropertiesKHR props = {
    .sType = VK_STRUCTURE_TYPE_MEMORY_FD_PROPERTIES_KHR };
  CHECK(getprops(c->dev, VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT, fd,
                 &props));
  uint32_t ti = mem_type(c, props.memoryTypeBits,
                         VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT |
                         VK_MEMORY_PROPERTY_HOST_COHERENT_BIT);
  if (ti == UINT32_MAX) { printf("  raw: no host-visible import type\n");
                          *nonzero = 0; *maxb = 0; return 0; }
  int dupfd = dup(fd);
  VkImportMemoryFdInfoKHR imp = {
    .sType = VK_STRUCTURE_TYPE_IMPORT_MEMORY_FD_INFO_KHR,
    .handleType = VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT,
    .fd = dupfd };
  VkMemoryAllocateInfo ai = { .sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,
    .pNext = &imp, .allocationSize = size, .memoryTypeIndex = ti };
  VkDeviceMemory mem;
  VkResult r = vkAllocateMemory(c->dev, &ai, NULL, &mem);
  if (r != VK_SUCCESS) { close(dupfd); printf("  raw import -> %d\n", r);
                         *nonzero = 0; *maxb = 0; return 0; }
  uint8_t *map;
  CHECK(vkMapMemory(c->dev, mem, 0, VK_WHOLE_SIZE, 0, (void **)&map));
  uint32_t nz = 0, mx = 0;
  /* sample the way the KMD probe does: stride across the whole extent */
  size_t step = size / 64000 ? size / 64000 : 1;
  uint32_t sampled = 0;
  for (size_t off = 0; off < size && sampled < 64000; off += step, sampled++) {
    if (map[off]) nz++;
    if (map[off] > mx) mx = map[off];
  }
  vkUnmapMemory(c->dev, mem);
  vkFreeMemory(c->dev, mem, NULL);
  *nonzero = nz; *maxb = mx;
  return 0;
}


/* Arm L/M: the REAL topology — nothing ever writes through the original.
 * The original memory (devlocal, memory-only allocate, exportable) belongs to
 * one device; a SECOND device imports the fd (dedicated to its own image, the
 * way the ICD's deferred import chains VkMemoryDedicatedAllocateInfo) and
 * CLEARS (L) or dynamic-render-clears (M) through that import; the readback
 * consumer then reads through a THIRD import. */
static int writer_through_import(Ctx *wr, int fd, VkDeviceSize size,
                                 int dynamic_rendering) {
  VkExternalMemoryImageCreateInfo ext = {
    .sType = VK_STRUCTURE_TYPE_EXTERNAL_MEMORY_IMAGE_CREATE_INFO,
    .handleTypes = VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT };
  VkImageCreateInfo ici = {
    .sType = VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO,
    .pNext = &ext,
    .imageType = VK_IMAGE_TYPE_2D,
    .format = VK_FORMAT_B8G8R8A8_UNORM,
    .extent = { W, H, 1 }, .mipLevels = 1, .arrayLayers = 1,
    .samples = VK_SAMPLE_COUNT_1_BIT,
    .tiling = VK_IMAGE_TILING_OPTIMAL,
    .usage = USAGE_017,
    .sharingMode = VK_SHARING_MODE_EXCLUSIVE,
    .initialLayout = VK_IMAGE_LAYOUT_UNDEFINED };
  VkImage img;
  CHECK(vkCreateImage(wr->dev, &ici, NULL, &img));
  VkMemoryRequirements req;
  vkGetImageMemoryRequirements(wr->dev, img, &req);
  if (req.size > size) { printf("  writer: req %llu > fd %llu\n",
      (unsigned long long)req.size, (unsigned long long)size); return -1; }
  PFN_vkGetMemoryFdPropertiesKHR getprops =
      (PFN_vkGetMemoryFdPropertiesKHR)vkGetDeviceProcAddr(
          wr->dev, "vkGetMemoryFdPropertiesKHR");
  VkMemoryFdPropertiesKHR props = {
    .sType = VK_STRUCTURE_TYPE_MEMORY_FD_PROPERTIES_KHR };
  CHECK(getprops(wr->dev, VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT, fd,
                 &props));
  int dupfd = dup(fd);
  VkImportMemoryFdInfoKHR imp = {
    .sType = VK_STRUCTURE_TYPE_IMPORT_MEMORY_FD_INFO_KHR,
    .handleType = VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT,
    .fd = dupfd };
  VkMemoryDedicatedAllocateInfo ded = {
    .sType = VK_STRUCTURE_TYPE_MEMORY_DEDICATED_ALLOCATE_INFO,
    .pNext = &imp, .image = img };
  uint32_t ti = mem_type(wr, req.memoryTypeBits & props.memoryTypeBits, 0);
  VkMemoryAllocateInfo ai = { .sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,
    .pNext = &ded, .allocationSize = size, .memoryTypeIndex = ti };
  VkDeviceMemory mem;
  CHECK(vkAllocateMemory(wr->dev, &ai, NULL, &mem));
  CHECK(vkBindImageMemory(wr->dev, img, mem, 0));

  if (begin(wr)) return -1;
  if (dynamic_rendering) {
    VkImageMemoryBarrier b = {
      .sType = VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER,
      .srcAccessMask = 0,
      .dstAccessMask = VK_ACCESS_COLOR_ATTACHMENT_WRITE_BIT,
      .oldLayout = VK_IMAGE_LAYOUT_UNDEFINED,
      .newLayout = VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
      .srcQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED,
      .dstQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED,
      .image = img,
      .subresourceRange = { VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1 } };
    vkCmdPipelineBarrier(wr->cmd, VK_PIPELINE_STAGE_TOP_OF_PIPE_BIT,
                         VK_PIPELINE_STAGE_COLOR_ATTACHMENT_OUTPUT_BIT, 0,
                         0, NULL, 0, NULL, 1, &b);
    VkImageViewCreateInfo vi = {
      .sType = VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO,
      .image = img, .viewType = VK_IMAGE_VIEW_TYPE_2D,
      .format = VK_FORMAT_B8G8R8A8_UNORM,
      .subresourceRange = { VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1 } };
    VkImageView view;
    CHECK(vkCreateImageView(wr->dev, &vi, NULL, &view));
    VkRenderingAttachmentInfo att = {
      .sType = VK_STRUCTURE_TYPE_RENDERING_ATTACHMENT_INFO,
      .imageView = view,
      .imageLayout = VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
      .loadOp = VK_ATTACHMENT_LOAD_OP_CLEAR,
      .storeOp = VK_ATTACHMENT_STORE_OP_STORE,
      .clearValue = { .color = { .float32 = { 0.0f, 1.0f, 0.0f, 1.0f } } } };
    VkRenderingInfo ri = {
      .sType = VK_STRUCTURE_TYPE_RENDERING_INFO,
      .renderArea = { { 0, 0 }, { W, H } },
      .layerCount = 1,
      .colorAttachmentCount = 1, .pColorAttachments = &att };
    PFN_vkCmdBeginRendering pBegin =
      (PFN_vkCmdBeginRendering)vkGetDeviceProcAddr(wr->dev,
                                                   "vkCmdBeginRendering");
    PFN_vkCmdEndRendering pEnd =
      (PFN_vkCmdEndRendering)vkGetDeviceProcAddr(wr->dev, "vkCmdEndRendering");
    if (!pBegin || !pEnd) { printf("  no dynamic rendering\n"); return -1; }
    pBegin(wr->cmd, &ri);
    pEnd(wr->cmd);
    VkImageMemoryBarrier b2 = b;
    b2.srcAccessMask = VK_ACCESS_COLOR_ATTACHMENT_WRITE_BIT;
    b2.dstAccessMask = 0;
    b2.oldLayout = VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL;
    b2.newLayout = VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL;
    vkCmdPipelineBarrier(wr->cmd, VK_PIPELINE_STAGE_COLOR_ATTACHMENT_OUTPUT_BIT,
                         VK_PIPELINE_STAGE_BOTTOM_OF_PIPE_BIT, 0, 0, NULL,
                         0, NULL, 1, &b2);
  } else {
    VkImageMemoryBarrier b = {
      .sType = VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER,
      .srcAccessMask = 0, .dstAccessMask = VK_ACCESS_TRANSFER_WRITE_BIT,
      .oldLayout = VK_IMAGE_LAYOUT_UNDEFINED,
      .newLayout = VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
      .srcQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED,
      .dstQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED,
      .image = img,
      .subresourceRange = { VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1 } };
    vkCmdPipelineBarrier(wr->cmd, VK_PIPELINE_STAGE_TOP_OF_PIPE_BIT,
                         VK_PIPELINE_STAGE_TRANSFER_BIT, 0, 0, NULL, 0, NULL,
                         1, &b);
    VkClearColorValue green = { .float32 = { 0.0f, 1.0f, 0.0f, 1.0f } };
    VkImageSubresourceRange range = { VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1 };
    vkCmdClearColorImage(wr->cmd, img, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
                         &green, 1, &range);
    VkImageMemoryBarrier b2 = b;
    b2.srcAccessMask = VK_ACCESS_TRANSFER_WRITE_BIT;
    b2.dstAccessMask = 0;
    b2.oldLayout = VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL;
    b2.newLayout = VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL;
    vkCmdPipelineBarrier(wr->cmd, VK_PIPELINE_STAGE_TRANSFER_BIT,
                         VK_PIPELINE_STAGE_BOTTOM_OF_PIPE_BIT, 0, 0, NULL,
                         0, NULL, 1, &b2);
  }
  if (submit_wait(wr)) return -1;
  return 0;
}

/* Memory-only original: allocate exportable devlocal WITHOUT any image, the
 * way the KMD's venus context backs a blob. */
static int allocate_original_fd(Ctx *p, VkDeviceSize size) {
  VkExportMemoryAllocateInfo exp = {
    .sType = VK_STRUCTURE_TYPE_EXPORT_MEMORY_ALLOCATE_INFO,
    .handleTypes = VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT };
  uint32_t ti = mem_type(p, ~0u, VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT);
  VkMemoryAllocateInfo ai = { .sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,
    .pNext = &exp, .allocationSize = size, .memoryTypeIndex = ti };
  VkDeviceMemory mem;
  CHECK(vkAllocateMemory(p->dev, &ai, NULL, &mem));
  PFN_vkGetMemoryFdKHR getfd =
      (PFN_vkGetMemoryFdKHR)vkGetDeviceProcAddr(p->dev, "vkGetMemoryFdKHR");
  VkMemoryGetFdInfoKHR gi = { .sType = VK_STRUCTURE_TYPE_MEMORY_GET_FD_INFO_KHR,
    .memory = mem,
    .handleType = VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT };
  int fd = -1;
  VkResult r = getfd(p->dev, &gi, &fd);
  if (r != VK_SUCCESS) { printf("  GetMemoryFd -> %d\n", r); return -1; }
  return fd;
}

static void run_arm(const char *name, Ctx *p, Ctx *c,
                    VkExternalMemoryHandleTypeFlags image_ext,
                    VkImageLayout final_layout,
                    VkImageLayout consumer_old, int external_qf) {
  printf("=== ARM %s ===\n", name);
  VkDeviceSize size = 0;
  int fd = producer(p, image_ext, final_layout, &size);
  if (fd < 0) { printf("  producer failed\n"); return; }
  printf("  producer req.size=%llu fd=%d\n", (unsigned long long)size, fd);
  uint32_t rnz = 0, rmx = 0;
  raw_pages(c, fd, size, &rnz, &rmx);
  printf("  RAW pages: nonzero %u/64000 max %u\n", rnz, rmx);
  uint32_t nz = 0, mx = 0, green = 0;
  if (consumer_optimal(c, fd, size, consumer_old, external_qf,
                       &nz, &mx, &green) == 0)
    printf("  READBACK: nonzero_bytes %u/%u max %u green_px %u/%u -> %s\n",
           nz, W * H * 4, mx, green, W * H,
           green == W * H ? "FULL GREEN" : (nz == 0 ? "ALL ZERO" : "PARTIAL"));
  close(fd);
}

int main(void) {
  Ctx p = {0}, c = {0};
  if (ctx_init(&p) || ctx_init(&c)) return 1;
  printf("two devices up (same GPU, separate instances)\n");

  run_arm("A dmabuf-ext, SHADER_RO, readback-exact", &p, &c,
          VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT,
          VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
          VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL, 1);
  run_arm("B opaque-declared image, dmabuf export", &p, &c,
          VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD_BIT,
          VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
          VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL, 1);
  run_arm("C non-external producer image", &p, &c,
          0,
          VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
          VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL, 1);
  run_arm("D producer left in COLOR_ATTACHMENT", &p, &c,
          VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT,
          VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
          VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL, 1);
  run_arm("E consumer GENERAL oldLayout, no qf transfer", &p, &c,
          VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT,
          VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
          VK_IMAGE_LAYOUT_GENERAL, 0);

  g_dedicated = 0; g_host_visible = 0;
  run_arm("F NON-dedicated DEVICE_LOCAL producer", &p, &c,
          VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT,
          VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
          VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL, 1);
  g_dedicated = 0; g_host_visible = 1;
  run_arm("G NON-dedicated HOST_VISIBLE producer (the venus blob shape)",
          &p, &c,
          VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT,
          VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
          VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL, 1);
  g_dedicated = 1; g_host_visible = 1;
  run_arm("H dedicated HOST_VISIBLE producer", &p, &c,
          VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT,
          VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
          VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL, 1);
  g_dedicated = 0; g_host_visible = 1;
  run_arm("I venus shape, non-external producer image", &p, &c,
          0,
          VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
          VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL, 1);
  g_dedicated = 0; g_host_visible = 0; g_attachment_write = 1;
  run_arm("J ATTACHMENT write, non-dedicated devlocal", &p, &c,
          VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT,
          VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
          VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL, 1);
  g_dedicated = 0; g_host_visible = 0; g_attachment_write = 1;
  run_arm("K ATTACHMENT write, non-external producer", &p, &c,
          0,
          VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
          VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL, 1);
  {
    printf("=== ARM L write-through-import (clear), read-through-import ===\n");
    VkDeviceSize size = 4587520;
    int fd = allocate_original_fd(&p, size);
    if (fd >= 0) {
      Ctx w = {0};
      if (!ctx_init(&w) && !writer_through_import(&w, fd, size, 0)) {
        uint32_t nz = 0, mx = 0, green = 0;
        if (consumer_optimal(&c, fd, size,
                             VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL, 1,
                             &nz, &mx, &green) == 0)
          printf("  READBACK: nonzero %u max %u green_px %u/%u -> %s\n",
                 nz, mx, green, W * H,
                 green == W * H ? "FULL GREEN"
                                : (nz == 0 ? "ALL ZERO" : "PARTIAL"));
      }
      close(fd);
    }
  }
  {
    printf("=== ARM M write-through-import (dynamic rendering), read-through-import ===\n");
    VkDeviceSize size = 4587520;
    int fd = allocate_original_fd(&p, size);
    if (fd >= 0) {
      Ctx w = {0};
      if (!ctx_init(&w) && !writer_through_import(&w, fd, size, 1)) {
        uint32_t nz = 0, mx = 0, green = 0;
        if (consumer_optimal(&c, fd, size,
                             VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL, 1,
                             &nz, &mx, &green) == 0)
          printf("  READBACK: nonzero %u max %u green_px %u/%u -> %s\n",
                 nz, mx, green, W * H,
                 green == W * H ? "FULL GREEN"
                                : (nz == 0 ? "ALL ZERO" : "PARTIAL"));
      }
      close(fd);
    }
  }
  return 0;
}
