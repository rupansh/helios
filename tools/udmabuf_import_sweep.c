/* WHICH udmabuf sizes will this GPU import as VkDeviceMemory?
 *
 * `udmabuf_import_probe.c` asked "can it at all" with one 4 MiB buffer and got
 * yes. That answer does not cover what the guest then measured: 205 of 206
 * imports refused OUT_OF_DEVICE_MEMORY in one boot, including a single-page
 * one, while a 4 MiB one succeeded. This probe takes ONE size per run and
 * reports whether the host imports it.
 *
 * ⛔ ONE SIZE PER PROCESS, DELIBERATELY. A refused import poisons the VkDevice:
 * every later import on it fails whatever its size. A loop inside one process
 * therefore reports the first refusal forever and reads as "nothing imports".
 * That is exactly how this was nearly misdiagnosed.
 *
 * Build: gcc -O2 -o /tmp/udmabuf_import_sweep tools/udmabuf_import_sweep.c -lvulkan
 * Run:   for s in 61440 65536 69632 983040 1044480 4194304; do
 *            /tmp/udmabuf_import_sweep $s; done
 *
 * Measured 2026-08-30, NVIDIA RTX PRO 6000 Blackwell:
 *      4096 REFUSED     61440 REFUSED     962560 REFUSED    1044480 REFUSED
 *     16384 REFUSED     65536 IMPORTED    983040 IMPORTED   4190208 REFUSED
 *     69632 REFUSED    131072 IMPORTED   1048576 IMPORTED   4194304 IMPORTED
 * => the host imports a udmabuf IFF its size is a multiple of 64 KiB.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <linux/udmabuf.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <unistd.h>
#include <vulkan/vulkan.h>

int main(int argc, char **argv)
{
   if (argc < 2) { fprintf(stderr, "usage: %s <bytes>\n", argv[0]); return 2; }
   const uint64_t bytes = strtoull(argv[1], NULL, 0);

   int ud = open("/dev/udmabuf", O_RDWR | O_CLOEXEC);
   if (ud < 0) { perror("/dev/udmabuf"); return 2; }
   int memfd = memfd_create("sweep", MFD_CLOEXEC | MFD_ALLOW_SEALING);
   if (memfd < 0 || ftruncate(memfd, bytes)) { perror("memfd"); return 2; }
   /* udmabuf requires F_SEAL_SHRINK; without it CREATE returns EINVAL. */
   fcntl(memfd, F_ADD_SEALS, F_SEAL_SHRINK);
   struct udmabuf_create c = { .memfd = memfd, .flags = UDMABUF_FLAGS_CLOEXEC, .size = bytes };
   int dmabuf = ioctl(ud, UDMABUF_CREATE, &c);
   if (dmabuf < 0) {
      printf("%10llu  UDMABUF_CREATE failed: %s\n",
             (unsigned long long)bytes, strerror(errno));
      return 1;
   }

   VkApplicationInfo ai = { .sType = VK_STRUCTURE_TYPE_APPLICATION_INFO,
                            .apiVersion = VK_API_VERSION_1_2 };
   VkInstanceCreateInfo ici = { .sType = VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO,
                                .pApplicationInfo = &ai };
   VkInstance inst;
   if (vkCreateInstance(&ici, NULL, &inst)) { printf("vkCreateInstance failed\n"); return 2; }
   uint32_t n = 0;
   vkEnumeratePhysicalDevices(inst, &n, NULL);
   VkPhysicalDevice *pds = calloc(n, sizeof(*pds));
   vkEnumeratePhysicalDevices(inst, &n, pds);
   VkPhysicalDevice pd = VK_NULL_HANDLE;
   for (uint32_t i = 0; i < n; i++) {
      VkPhysicalDeviceProperties p;
      vkGetPhysicalDeviceProperties(pds[i], &p);
      if (p.deviceType == VK_PHYSICAL_DEVICE_TYPE_DISCRETE_GPU) { pd = pds[i]; break; }
   }
   if (!pd) { printf("no discrete GPU\n"); return 2; }

   const char *ext[] = { "VK_KHR_external_memory_fd", "VK_EXT_external_memory_dma_buf" };
   float pri = 1.0f;
   VkDeviceQueueCreateInfo q = { .sType = VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO,
                                 .queueCount = 1, .pQueuePriorities = &pri };
   VkDeviceCreateInfo dci = { .sType = VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO,
                              .queueCreateInfoCount = 1, .pQueueCreateInfos = &q,
                              .enabledExtensionCount = 2, .ppEnabledExtensionNames = ext };
   VkDevice dev;
   if (vkCreateDevice(pd, &dci, NULL, &dev)) { printf("vkCreateDevice failed\n"); return 2; }

   /* Type 0 is what the KMD's "fewest property flags" rule picks, and the only
    * type NVIDIA admits for a dmabuf (mask 0x9, and type 3 refuses). */
   VkImportMemoryFdInfoKHR imp = { .sType = VK_STRUCTURE_TYPE_IMPORT_MEMORY_FD_INFO_KHR,
                                   .handleType = VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT,
                                   .fd = dmabuf };
   VkMemoryAllocateInfo mai = { .sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,
                                .pNext = &imp, .allocationSize = bytes, .memoryTypeIndex = 0 };
   VkDeviceMemory mem = VK_NULL_HANDLE;
   VkResult r = vkAllocateMemory(dev, &mai, NULL, &mem);
   printf("%10llu  %s64KiB  r=%2d  %s\n", (unsigned long long)bytes,
          bytes % (64u << 10) ? "non-" : "  x ", r, r ? "REFUSED" : "IMPORTED");
   return r ? 1 : 0;
}
