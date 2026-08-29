/* Can this GPU import a udmabuf (guest RAM) as VkDeviceMemory?
 *
 * That is the one question separating the two remaining fixes for the Helios
 * black desktop. Route (I) puts host-visible memory in guest pages and has the
 * host import them -- which is exactly this call. The guest reports the host's
 * vkAllocateMemory returning VK_ERROR_OUT_OF_DEVICE_MEMORY with no validation
 * message, so ask the host directly, outside the whole stack.
 *
 * Build: gcc -O2 -o udmabuf_import_probe udmabuf_import_probe.c -lvulkan
 */
#define _GNU_SOURCE
#include <fcntl.h>
#include <linux/udmabuf.h>
#include <stdio.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <unistd.h>
#include <vulkan/vulkan.h>

#define SIZE (4u << 20)

int main(void) {
   int memfd = memfd_create("probe", MFD_CLOEXEC | MFD_ALLOW_SEALING);
   if (memfd < 0) { perror("memfd_create"); return 1; }
   if (ftruncate(memfd, SIZE)) { perror("ftruncate"); return 1; }
   if (fcntl(memfd, F_ADD_SEALS, F_SEAL_SEAL | F_SEAL_SHRINK | F_SEAL_GROW)) {
      perror("F_ADD_SEALS"); return 1;
   }
   int dev_udmabuf = open("/dev/udmabuf", O_RDWR | O_CLOEXEC);
   if (dev_udmabuf < 0) { perror("/dev/udmabuf"); return 1; }
   struct udmabuf_create create = { .memfd = memfd, .flags = UDMABUF_FLAGS_CLOEXEC, .size = SIZE };
   int dmabuf = ioctl(dev_udmabuf, UDMABUF_CREATE, &create);
   if (dmabuf < 0) { perror("UDMABUF_CREATE"); return 1; }
   printf("udmabuf fd=%d size=%u\n", dmabuf, SIZE);

   const char *inst_exts[] = { "VK_KHR_get_physical_device_properties2",
                               "VK_KHR_external_memory_capabilities" };
   VkApplicationInfo app = { .sType = VK_STRUCTURE_TYPE_APPLICATION_INFO,
                             .apiVersion = VK_API_VERSION_1_2 };
   VkInstanceCreateInfo ici = { .sType = VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO,
                                .pApplicationInfo = &app,
                                .enabledExtensionCount = 2,
                                .ppEnabledExtensionNames = inst_exts };
   VkInstance inst;
   VkResult r = vkCreateInstance(&ici, NULL, &inst);
   if (r) { printf("vkCreateInstance=%d\n", r); return 1; }

   uint32_t n = 0;
   vkEnumeratePhysicalDevices(inst, &n, NULL);
   VkPhysicalDevice pds[8];
   if (n > 8) n = 8;
   vkEnumeratePhysicalDevices(inst, &n, pds);
   for (uint32_t i = 0; i < n; i++) {
      VkPhysicalDeviceProperties props;
      vkGetPhysicalDeviceProperties(pds[i], &props);
      printf("\n=== device %u: %s ===\n", i, props.deviceName);

      const char *dev_exts[] = { "VK_KHR_external_memory_fd",
                                 "VK_EXT_external_memory_dma_buf" };
      float pri = 1.0f;
      VkDeviceQueueCreateInfo q = { .sType = VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO,
                                    .queueCount = 1, .pQueuePriorities = &pri };
      VkDeviceCreateInfo dci = { .sType = VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO,
                                 .queueCreateInfoCount = 1, .pQueueCreateInfos = &q,
                                 .enabledExtensionCount = 2,
                                 .ppEnabledExtensionNames = dev_exts };
      VkDevice dev;
      r = vkCreateDevice(pds[i], &dci, NULL, &dev);
      if (r) { printf("  vkCreateDevice=%d (dma_buf ext likely absent)\n", r); continue; }

      PFN_vkGetMemoryFdPropertiesKHR getprops =
         (PFN_vkGetMemoryFdPropertiesKHR)vkGetDeviceProcAddr(dev, "vkGetMemoryFdPropertiesKHR");
      VkMemoryFdPropertiesKHR fdp = { .sType = VK_STRUCTURE_TYPE_MEMORY_FD_PROPERTIES_KHR };
      uint32_t mask = 0;
      if (getprops) {
         r = getprops(dev, VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT, dmabuf, &fdp);
         printf("  vkGetMemoryFdProperties(DMA_BUF) = %d, memoryTypeBits = 0x%08x\n",
                r, fdp.memoryTypeBits);
         mask = (r == VK_SUCCESS) ? fdp.memoryTypeBits : 0;
      } else {
         printf("  vkGetMemoryFdPropertiesKHR absent\n");
      }

      VkPhysicalDeviceMemoryProperties mp;
      vkGetPhysicalDeviceMemoryProperties(pds[i], &mp);
      int imported = 0;
      for (uint32_t t = 0; t < mp.memoryTypeCount; t++) {
         if (mask && !(mask & (1u << t))) continue;
         int fd2 = dup(dmabuf);
         VkImportMemoryFdInfoKHR imp = { .sType = VK_STRUCTURE_TYPE_IMPORT_MEMORY_FD_INFO_KHR,
                                         .handleType = VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT,
                                         .fd = fd2 };
         VkMemoryAllocateInfo mai = { .sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,
                                      .pNext = &imp, .allocationSize = SIZE,
                                      .memoryTypeIndex = t };
         VkDeviceMemory mem;
         r = vkAllocateMemory(dev, &mai, NULL, &mem);
         printf("  import type %u (flags 0x%02x heap %u) -> %d%s\n", t,
                mp.memoryTypes[t].propertyFlags, mp.memoryTypes[t].heapIndex, r,
                r == VK_SUCCESS ? "  IMPORTED" : "");
         if (r == VK_SUCCESS) {
            imported = 1;
            /* Importable is not usable. The whole point is to bind a buffer the
             * GPU copies into, so ask whether this type is even in a buffer's
             * memoryTypeBits, and whether the bind takes. */
            VkBufferCreateInfo bci = {
               .sType = VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO,
               .size = SIZE,
               .usage = VK_BUFFER_USAGE_TRANSFER_SRC_BIT |
                        VK_BUFFER_USAGE_TRANSFER_DST_BIT |
                        VK_BUFFER_USAGE_STORAGE_BUFFER_BIT,
               .sharingMode = VK_SHARING_MODE_EXCLUSIVE };
            VkBuffer buf;
            if (vkCreateBuffer(dev, &bci, NULL, &buf) == VK_SUCCESS) {
               VkMemoryRequirements req;
               vkGetBufferMemoryRequirements(dev, buf, &req);
               const int allowed = (req.memoryTypeBits & (1u << t)) != 0;
               VkResult br = vkBindBufferMemory(dev, buf, mem, 0);
               printf("      buffer memoryTypeBits=0x%08x type %u allowed=%d "
                      "bind=%d%s\n", req.memoryTypeBits, t, allowed, br,
                      br == VK_SUCCESS ? "  BINDABLE" : "");
               vkDestroyBuffer(dev, buf, NULL);
            }
            vkFreeMemory(dev, mem, NULL);
         }
         else close(fd2);
      }
      printf("  => udmabuf import %s on this device\n", imported ? "WORKS" : "FAILS");
      vkDestroyDevice(dev, NULL);
   }
   vkDestroyInstance(inst, NULL);
   return 0;
}
