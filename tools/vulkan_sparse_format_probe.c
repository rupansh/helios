/* Read-only per-format sparse inventory. No VkDevice, memory binding or GPU
 * submission. Exit 0 means queries completed, not feature-level conformance.
 * Linux: cc tools/vulkan_sparse_format_probe.c -lvulkan -o <local executable>
 * Windows: clang-cl /TC <source> /I<VulkanSDK>\Include /link /LIBPATH:<VulkanSDK>\Lib vulkan-1.lib
 * Usage: <executable> --host | --venus. Selects the requested driver explicitly.
 */
#include <vulkan/vulkan.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <wchar.h>
#ifdef _WIN32
#include <windows.h>
#include <tlhelp32.h>
#endif

int main(int argc, char **argv)
{
    static const struct { const char *name; VkFormat format; VkImageUsageFlags attachment; } formats[] = {
        {"R8_UINT", VK_FORMAT_R8_UINT, VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT},
        {"R16_UINT", VK_FORMAT_R16_UINT, VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT},
        {"R32_UINT", VK_FORMAT_R32_UINT, VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT},
        {"RGBA8_UNORM", VK_FORMAT_R8G8B8A8_UNORM, VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT},
        {"D16_UNORM", VK_FORMAT_D16_UNORM, VK_IMAGE_USAGE_DEPTH_STENCIL_ATTACHMENT_BIT},
        {"D32_SFLOAT", VK_FORMAT_D32_SFLOAT, VK_IMAGE_USAGE_DEPTH_STENCIL_ATTACHMENT_BIT},
    };
    VkApplicationInfo app = {.sType = VK_STRUCTURE_TYPE_APPLICATION_INFO};
    VkInstanceCreateInfo create = {.sType = VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO};
    VkInstance instance;
    VkPhysicalDevice *devices, selected = VK_NULL_HANDLE;
    VkPhysicalDeviceDriverProperties driver = {.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_DRIVER_PROPERTIES};
    VkPhysicalDeviceIDProperties id = {.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_ID_PROPERTIES};
    VkPhysicalDeviceProperties2 properties = {.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_PROPERTIES_2};
    uint32_t count = 0, i, f, usage_index, sample;
    VkResult vr;
    int venus;

    if (argc != 2 || (strcmp(argv[1], "--venus") && strcmp(argv[1], "--host")))
    {
        fprintf(stderr, "Usage: %s --host|--venus\n", argv[0]);
        return 2;
    }
    venus = !strcmp(argv[1], "--venus");
    setvbuf(stdout, NULL, _IONBF, 0);
    app.pApplicationName = "Helios sparse format inventory";
    app.apiVersion = VK_API_VERSION_1_3;
    create.pApplicationInfo = &app;
    if ((vr = vkCreateInstance(&create, NULL, &instance)) != VK_SUCCESS)
    {
        fprintf(stderr, "vkCreateInstance %d\n", vr);
        return 1;
    }
    if ((vr = vkEnumeratePhysicalDevices(instance, &count, NULL)) != VK_SUCCESS || !count)
    {
        fprintf(stderr, "vkEnumeratePhysicalDevices %d count %u\n", vr, count);
        vkDestroyInstance(instance, NULL);
        return 1;
    }
    devices = calloc(count, sizeof(*devices));
    if (!devices) { vkDestroyInstance(instance, NULL); return 1; }
    if ((vr = vkEnumeratePhysicalDevices(instance, &count, devices)) != VK_SUCCESS)
    {
        fprintf(stderr, "vkEnumeratePhysicalDevices %d\n", vr);
        free(devices); vkDestroyInstance(instance, NULL); return 1;
    }
    properties.pNext = &driver;
    driver.pNext = &id;
    for (i = 0; i < count; i++)
    {
        vkGetPhysicalDeviceProperties2(devices[i], &properties);
        if ((venus && driver.driverID == VK_DRIVER_ID_MESA_VENUS) ||
                (!venus && driver.driverID == VK_DRIVER_ID_NVIDIA_PROPRIETARY))
        {
            selected = devices[i];
            break;
        }
    }
    free(devices);
    if (!selected)
    {
        fprintf(stderr, "Requested driver was not enumerated.\n");
        vkDestroyInstance(instance, NULL); return 77;
    }
    printf("QUERY_ONLY_NO_GPU_WORK\nDEVICE %s vendor=%04x device=%04x api=%u driver_id=%u driver=%s info=%s\n",
            properties.properties.deviceName, properties.properties.vendorID, properties.properties.deviceID,
            properties.properties.apiVersion, driver.driverID, driver.driverName, driver.driverInfo);
    printf("DEVICE_LUID valid=%u bytes=", id.deviceLUIDValid);
    for (i = 0; i < VK_LUID_SIZE; i++) printf("%02x", id.deviceLUID[i]);
    printf("\n");
    for (f = 0; f < sizeof(formats) / sizeof(formats[0]); f++)
    for (usage_index = 0; usage_index < 2; usage_index++)
    {
        VkPhysicalDeviceImageFormatInfo2 info = {.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_IMAGE_FORMAT_INFO_2};
        VkImageFormatProperties2 image = {.sType = VK_STRUCTURE_TYPE_IMAGE_FORMAT_PROPERTIES_2};
        info.type = VK_IMAGE_TYPE_2D;
        info.format = formats[f].format;
        info.tiling = VK_IMAGE_TILING_OPTIMAL;
        info.usage = VK_IMAGE_USAGE_TRANSFER_SRC_BIT | VK_IMAGE_USAGE_TRANSFER_DST_BIT |
                (usage_index ? formats[f].attachment : 0);
        info.flags = VK_IMAGE_CREATE_SPARSE_BINDING_BIT | VK_IMAGE_CREATE_SPARSE_RESIDENCY_BIT | VK_IMAGE_CREATE_SPARSE_ALIASED_BIT;
        vr = vkGetPhysicalDeviceImageFormatProperties2(selected, &info, &image);
        printf("FORMAT %s usage=%u image_result=%d sample_mask=%u\n", formats[f].name, info.usage, vr,
                vr == VK_SUCCESS ? image.imageFormatProperties.sampleCounts : 0);
        for (sample = 1; sample <= 4; sample *= 4)
        {
            VkPhysicalDeviceSparseImageFormatInfo2 sparse = {.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_SPARSE_IMAGE_FORMAT_INFO_2};
            if (vr != VK_SUCCESS || !(image.imageFormatProperties.sampleCounts & sample)) continue;
            sparse.format = info.format;
            sparse.type = info.type;
            sparse.samples = sample;
            sparse.usage = info.usage;
            sparse.tiling = info.tiling;
            count = 0;
            vkGetPhysicalDeviceSparseImageFormatProperties2(selected, &sparse, &count, NULL);
            printf("SPARSE %s usage=%u samples=%u property_count=%u\n", formats[f].name, info.usage, sample, count);
        }
    }
#ifdef _WIN32
    {
        HANDLE snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPMODULE, GetCurrentProcessId());
        MODULEENTRY32W module;
        memset(&module, 0, sizeof(module)); module.dwSize = sizeof(module);
        if (snapshot == INVALID_HANDLE_VALUE || !Module32FirstW(snapshot, &module))
        {
            fprintf(stderr, "Module inventory failed %lu\n", GetLastError());
            if (snapshot != INVALID_HANDLE_VALUE) CloseHandle(snapshot);
            vkDestroyInstance(instance, NULL); return 1;
        }
        do {
            if (wcsstr(module.szModule, L"vulkan") || wcsstr(module.szModule, L"virtio"))
                printf("LOADED_MODULE %ls\n", module.szExePath);
        } while (Module32NextW(snapshot, &module));
        CloseHandle(snapshot);
    }
#endif
    vkDestroyInstance(instance, NULL);
    return 0;
}
