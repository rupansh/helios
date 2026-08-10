// vk_external_handle_probe.cpp — what external-memory/semaphore handle types
// does the Helios venus ICD actually support?
//
// WHY THIS EXISTS. VK_LAYER_HELIOS_present refuses to admit the Helios
// physical device with physdev_refused_no_external_image_import, "external
// image format query failed". That one counter covers three distinct
// conditions (missing Win32 external extensions, a failed query, a query that
// reports not-IMPORTABLE), and the failed-query arm itself cannot tell a NULL
// function pointer from VK_ERROR_FORMAT_NOT_SUPPORTED. Reading the layer says
// which line refused; only a measurement says why.
//
// docs/HELIOS_PRESENT_SYNC_RETIREMENT.md §10.3 is normative about the answer
// it needs: each swapchain image is a shareable committed D3D12 texture whose
// NT handle is imported with VK_EXTERNAL_MEMORY_HANDLE_TYPE_D3D12_RESOURCE_BIT,
// and Ready/Release are D3D12 fences imported with D3D12_FENCE_BIT. This probe
// asks the ICD, for the layer's exact BGRA8/OPTIMAL/usage-union tuple, which
// handle types it reports and with which features.
//
// It runs against the ICD directly (no layer, no window, no session): the
// point is the ICD's capability, not the layer's reaction to it.
//
// Build (VM, mingw g++):
//   g++ -O2 -o C:\Users\Rupansh\helios-probe\vk_external_handle_probe.exe \
//       Z:\tools\vk_external_handle_probe.cpp -lvulkan-1
//   (add -I "C:\VulkanSDK\<ver>\Include" -L "C:\VulkanSDK\<ver>\Lib")
//
// Exit code: 0 = the probe ran (read the table); 1 = could not even create an
// instance or find a physical device.

#define VK_USE_PLATFORM_WIN32_KHR
#include <vulkan/vulkan.h>
#include <stdio.h>
#include <string.h>

// The layer's copy-only profile (helios_present_layer.h), restated here so the
// probe measures the tuple the layer actually asks about.
#define HELIOS_WSI_FORMAT VK_FORMAT_B8G8R8A8_UNORM
#define HELIOS_WSI_SUPPORTED_USAGE                                           \
   (VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT | VK_IMAGE_USAGE_TRANSFER_SRC_BIT |  \
    VK_IMAGE_USAGE_TRANSFER_DST_BIT)

struct named_bit {
   const char *name;
   uint32_t bit;
};

static const named_bit mem_handle_types[] = {
   { "D3D12_RESOURCE", VK_EXTERNAL_MEMORY_HANDLE_TYPE_D3D12_RESOURCE_BIT },
   { "D3D12_HEAP", VK_EXTERNAL_MEMORY_HANDLE_TYPE_D3D12_HEAP_BIT },
   { "D3D11_TEXTURE", VK_EXTERNAL_MEMORY_HANDLE_TYPE_D3D11_TEXTURE_BIT },
   { "D3D11_TEXTURE_KMT", VK_EXTERNAL_MEMORY_HANDLE_TYPE_D3D11_TEXTURE_KMT_BIT },
   { "OPAQUE_WIN32", VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_WIN32_BIT },
   { "OPAQUE_WIN32_KMT", VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_WIN32_KMT_BIT },
};

static const named_bit sem_handle_types[] = {
   { "D3D12_FENCE", VK_EXTERNAL_SEMAPHORE_HANDLE_TYPE_D3D12_FENCE_BIT },
   { "OPAQUE_WIN32", VK_EXTERNAL_SEMAPHORE_HANDLE_TYPE_OPAQUE_WIN32_BIT },
   { "OPAQUE_WIN32_KMT", VK_EXTERNAL_SEMAPHORE_HANDLE_TYPE_OPAQUE_WIN32_KMT_BIT },
};

static const char *
result_name(VkResult r)
{
   switch (r) {
   case VK_SUCCESS: return "VK_SUCCESS";
   case VK_ERROR_OUT_OF_HOST_MEMORY: return "OUT_OF_HOST_MEMORY";
   case VK_ERROR_OUT_OF_DEVICE_MEMORY: return "OUT_OF_DEVICE_MEMORY";
   case VK_ERROR_FORMAT_NOT_SUPPORTED: return "FORMAT_NOT_SUPPORTED";
   case VK_ERROR_INITIALIZATION_FAILED: return "INITIALIZATION_FAILED";
   case VK_ERROR_EXTENSION_NOT_PRESENT: return "EXTENSION_NOT_PRESENT";
   case VK_ERROR_INCOMPATIBLE_DRIVER: return "INCOMPATIBLE_DRIVER";
   default: return "(other)";
   }
}

static void
print_mem_features(VkExternalMemoryFeatureFlags f)
{
   printf("%s%s%s",
          (f & VK_EXTERNAL_MEMORY_FEATURE_DEDICATED_ONLY_BIT) ? "DEDICATED_ONLY " : "",
          (f & VK_EXTERNAL_MEMORY_FEATURE_EXPORTABLE_BIT) ? "EXPORTABLE " : "",
          (f & VK_EXTERNAL_MEMORY_FEATURE_IMPORTABLE_BIT) ? "IMPORTABLE " : "");
   if (!f)
      printf("(none)");
}

int
main(void)
{
   VkApplicationInfo app = {};
   app.sType = VK_STRUCTURE_TYPE_APPLICATION_INFO;
   app.pApplicationName = "helios_vk_external_handle_probe";
   app.apiVersion = VK_API_VERSION_1_3;

   // Core 1.1 promotes the capability queries this probe uses, so no instance
   // extension is required beyond what the loader always provides.
   VkInstanceCreateInfo ici = {};
   ici.sType = VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO;
   ici.pApplicationInfo = &app;

   VkInstance inst = VK_NULL_HANDLE;
   VkResult r = vkCreateInstance(&ici, nullptr, &inst);
   if (r != VK_SUCCESS) {
      printf("FAIL vkCreateInstance -> %s (%d)\n", result_name(r), (int)r);
      return 1;
   }

   uint32_t n = 0;
   vkEnumeratePhysicalDevices(inst, &n, nullptr);
   if (!n) {
      printf("FAIL no physical devices\n");
      return 1;
   }
   VkPhysicalDevice phys[8];
   if (n > 8)
      n = 8;
   vkEnumeratePhysicalDevices(inst, &n, phys);

   auto get_image_props2 = (PFN_vkGetPhysicalDeviceImageFormatProperties2)
      vkGetInstanceProcAddr(inst, "vkGetPhysicalDeviceImageFormatProperties2");
   auto get_sem_props = (PFN_vkGetPhysicalDeviceExternalSemaphoreProperties)
      vkGetInstanceProcAddr(inst, "vkGetPhysicalDeviceExternalSemaphoreProperties");

   // Distinguishing a NULL entry point from a driver error is the entire point
   // of this probe, so it is reported before any query is attempted.
   printf("entry points: GetPhysicalDeviceImageFormatProperties2=%s "
          "GetPhysicalDeviceExternalSemaphoreProperties=%s\n\n",
          get_image_props2 ? "present" : "NULL",
          get_sem_props ? "present" : "NULL");
   if (!get_image_props2 || !get_sem_props) {
      printf("FAIL a core 1.1 capability entry point is missing\n");
      return 1;
   }

   for (uint32_t d = 0; d < n; d++) {
      VkPhysicalDeviceProperties props = {};
      vkGetPhysicalDeviceProperties(phys[d], &props);
      printf("=== GPU%u %s ===\n", d, props.deviceName);

      uint32_t en = 0;
      vkEnumerateDeviceExtensionProperties(phys[d], nullptr, &en, nullptr);
      VkExtensionProperties *exts = new VkExtensionProperties[en ? en : 1];
      if (en)
         vkEnumerateDeviceExtensionProperties(phys[d], nullptr, &en, exts);
      bool has_mem_win32 = false, has_sem_win32 = false, has_dedicated = false;
      for (uint32_t i = 0; i < en; i++) {
         if (!strcmp(exts[i].extensionName, VK_KHR_EXTERNAL_MEMORY_WIN32_EXTENSION_NAME))
            has_mem_win32 = true;
         if (!strcmp(exts[i].extensionName, VK_KHR_EXTERNAL_SEMAPHORE_WIN32_EXTENSION_NAME))
            has_sem_win32 = true;
         if (!strcmp(exts[i].extensionName, VK_KHR_DEDICATED_ALLOCATION_EXTENSION_NAME))
            has_dedicated = true;
      }
      delete[] exts;
      printf("device extensions: external_memory_win32=%d external_semaphore_win32=%d "
             "dedicated_allocation=%d\n",
             has_mem_win32, has_sem_win32, has_dedicated);

      printf("\n-- external IMAGE, B8G8R8A8_UNORM / 2D / OPTIMAL / "
             "COLOR_ATTACHMENT|TRANSFER_SRC|TRANSFER_DST --\n");
      for (const named_bit &h : mem_handle_types) {
         VkPhysicalDeviceExternalImageFormatInfo ext_info = {};
         ext_info.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_EXTERNAL_IMAGE_FORMAT_INFO;
         ext_info.handleType = (VkExternalMemoryHandleTypeFlagBits)h.bit;

         VkPhysicalDeviceImageFormatInfo2 fi = {};
         fi.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_IMAGE_FORMAT_INFO_2;
         fi.pNext = &ext_info;
         fi.format = HELIOS_WSI_FORMAT;
         fi.type = VK_IMAGE_TYPE_2D;
         fi.tiling = VK_IMAGE_TILING_OPTIMAL;
         fi.usage = HELIOS_WSI_SUPPORTED_USAGE;
         fi.flags = 0;

         VkExternalImageFormatProperties ep = {};
         ep.sType = VK_STRUCTURE_TYPE_EXTERNAL_IMAGE_FORMAT_PROPERTIES;
         VkImageFormatProperties2 fp = {};
         fp.sType = VK_STRUCTURE_TYPE_IMAGE_FORMAT_PROPERTIES_2;
         fp.pNext = &ep;

         VkResult qr = get_image_props2(phys[d], &fi, &fp);
         printf("  %-18s %-22s ", h.name, result_name(qr));
         if (qr == VK_SUCCESS) {
            print_mem_features(ep.externalMemoryProperties.externalMemoryFeatures);
            printf(" compatible=0x%x export=0x%x",
                   ep.externalMemoryProperties.compatibleHandleTypes,
                   ep.externalMemoryProperties.exportFromImportedHandleTypes);
         }
         printf("\n");
      }

      // The control arm for the table above: the same tuple with NO external
      // handle type. If this fails too, the refusal is about the format or
      // usage, not about external memory at all.
      {
         VkPhysicalDeviceImageFormatInfo2 fi = {};
         fi.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_IMAGE_FORMAT_INFO_2;
         fi.format = HELIOS_WSI_FORMAT;
         fi.type = VK_IMAGE_TYPE_2D;
         fi.tiling = VK_IMAGE_TILING_OPTIMAL;
         fi.usage = HELIOS_WSI_SUPPORTED_USAGE;
         VkImageFormatProperties2 fp = {};
         fp.sType = VK_STRUCTURE_TYPE_IMAGE_FORMAT_PROPERTIES_2;
         VkResult qr = get_image_props2(phys[d], &fi, &fp);
         printf("  %-18s %-22s (control: no external handle type)\n",
                "(none)", result_name(qr));
      }

      printf("\n-- external SEMAPHORE --\n");
      for (const named_bit &h : sem_handle_types) {
         VkPhysicalDeviceExternalSemaphoreInfo si = {};
         si.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_EXTERNAL_SEMAPHORE_INFO;
         si.handleType = (VkExternalSemaphoreHandleTypeFlagBits)h.bit;
         // §10.3 imports Ready/Release as TIMELINE semaphores.
         VkSemaphoreTypeCreateInfo st = {};
         st.sType = VK_STRUCTURE_TYPE_SEMAPHORE_TYPE_CREATE_INFO;
         st.semaphoreType = VK_SEMAPHORE_TYPE_TIMELINE;
         si.pNext = &st;

         VkExternalSemaphoreProperties sp = {};
         sp.sType = VK_STRUCTURE_TYPE_EXTERNAL_SEMAPHORE_PROPERTIES;
         get_sem_props(phys[d], &si, &sp);
         printf("  %-18s features=%s%s compatible=0x%x export=0x%x\n", h.name,
                (sp.externalSemaphoreFeatures & VK_EXTERNAL_SEMAPHORE_FEATURE_EXPORTABLE_BIT)
                   ? "EXPORTABLE " : "",
                (sp.externalSemaphoreFeatures & VK_EXTERNAL_SEMAPHORE_FEATURE_IMPORTABLE_BIT)
                   ? "IMPORTABLE " : "",
                sp.compatibleHandleTypes, sp.exportFromImportedHandleTypes);
      }
      printf("\n");
   }

   vkDestroyInstance(inst, nullptr);
   return 0;
}
