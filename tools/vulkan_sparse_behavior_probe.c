/* Isolated Vulkan preflight for reserved color4 compatibility selection.
 * Run on Windows only from an interactive scheduled task, never inside a UMD.
 * Usage: vulkan_sparse_behavior_probe --host|--venus <case 0..6>
 * Each process owns one disposable VkDevice and atomically publishes one case.
 * Exit 0=all phases pass, 1=observed sparse failure, 77=inconclusive.
 */
#define _CRT_SECURE_NO_WARNINGS
#include <vulkan/vulkan.h>
#include <errno.h>
#include <inttypes.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "../vkd3d-proton-helios/include/private/vkd3d_reserved_compat.h"
#ifdef _WIN32
#include <windows.h>
#include <tlhelp32.h>
#include <direct.h>
#include <process.h>
#define probe_mkdir(p) _mkdir(p)
#define probe_pid() _getpid()
#else
#include <sys/stat.h>
#include <unistd.h>
#define probe_mkdir(p) mkdir(p, 0700)
#define probe_pid() getpid()
#endif

static unsigned validation_errors;
static int timed_out;

static VKAPI_ATTR VkBool32 VKAPI_CALL debug_message(VkDebugUtilsMessageSeverityFlagBitsEXT severity,
        VkDebugUtilsMessageTypeFlagsEXT type, const VkDebugUtilsMessengerCallbackDataEXT *data, void *user)
{
    (void)type; (void)user;
    if (severity & VK_DEBUG_UTILS_MESSAGE_SEVERITY_ERROR_BIT_EXT) validation_errors++;
    fprintf(stderr, "VALIDATION %u %s\n", severity, data->pMessage);
    return VK_FALSE;
}

static int publish(struct vkd3d_sparse_probe_record *record)
{
    char path[VKD3D_SPARSE_PROBE_PATH_SIZE], temp[VKD3D_SPARSE_PROBE_PATH_SIZE + 40];
    FILE *file;
    size_t i;
    int good;
    if (!vkd3d_sparse_probe_path(path, sizeof(path), &record->key)) return 0;
    for (i = 1; path[i]; i++)
    {
        if (path[i] != '/' && path[i] != '\\') continue;
#ifdef _WIN32
        if (i == 2 && path[1] == ':') continue;
#endif
        memcpy(temp, path, i); temp[i] = 0;
        if (probe_mkdir(temp) && errno != EEXIST) return 0;
    }
    snprintf(temp, sizeof(temp), "%s.%u.tmp", path, (unsigned)probe_pid());
    record->version = VKD3D_SPARSE_PROBE_VERSION;
    record->size = sizeof(*record);
    record->checksum = vkd3d_sparse_probe_checksum(record);
    if (!(file = fopen(temp, "wb"))) return 0;
    good = fwrite(record, 1, sizeof(*record), file) == sizeof(*record);
    if (fclose(file)) good = 0;
#ifdef _WIN32
    if (good) good = !!MoveFileExA(temp, path, MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH);
#else
    if (good) good = !rename(temp, path);
#endif
    if (!good) remove(temp);
    else printf("CACHE %s status=%u phases=%u pixels=%u bad=%u checksum=%08x\n", path,
            record->status, record->completed_phases, record->checked_pixels, record->bad_pixels, record->checksum);
    return good;
}

/* A timeout is not completion. Keep every allocation alive until the submitted
 * work finishes or the private device reports loss; never destroy pending work
 * or call DeviceWaitIdle. The already-published UNKNOWN record selects fallback. */
static VkResult finish(VkDevice device, VkFence fence)
{
    VkResult vr;
    while ((vr = vkWaitForFences(device, 1, &fence, VK_TRUE, 5000000000ull)) == VK_TIMEOUT)
    {
        timed_out = 1;
        fprintf(stderr, "INCONCLUSIVE timeout; retaining pending resources until completion or device loss.\n");
    }
    return vr;
}

static VkResult allocate(VkDevice device, const VkPhysicalDeviceMemoryProperties *props,
        const VkMemoryRequirements *req, VkMemoryPropertyFlags flags, VkDeviceMemory *memory)
{
    VkMemoryAllocateInfo info = {.sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO};
    for (info.memoryTypeIndex = 0; info.memoryTypeIndex < props->memoryTypeCount; info.memoryTypeIndex++)
        if ((req->memoryTypeBits & (1u << info.memoryTypeIndex)) &&
                (props->memoryTypes[info.memoryTypeIndex].propertyFlags & flags) == flags)
            break;
    if (info.memoryTypeIndex == props->memoryTypeCount) return VK_ERROR_FEATURE_NOT_PRESENT;
    info.allocationSize = req->size;
    return vkAllocateMemory(device, &info, NULL, memory);
}

static void expected_color(unsigned index, unsigned layer, VkClearColorValue *clear, unsigned char *pixel)
{
    unsigned i, bytes = index == 0 ? 1 : index == 1 ? 2 : index == 3 ? 8 : 4;
    memset(clear, 0, sizeof(*clear));
    memset(pixel, 0, 8);
    if (index < 4)
    {
        clear->uint32[0] = layer ? 0x79 : 0x31;
        clear->uint32[1] = layer ? 0x24 : 0x63;
        for (i = 0; i < bytes; i++) pixel[i] = (unsigned char)(clear->uint32[i / 4] >> (8 * (i % 4)));
    }
    else
    {
        clear->float32[0] = layer ? 0.0f : 1.0f;
        clear->float32[1] = layer ? 1.0f : 0.0f;
        clear->float32[2] = layer ? 1.0f : 0.0f;
        clear->float32[3] = 1.0f;
        for (i = 0; i < 4; i++) pixel[i] = clear->float32[i] == 1.0f ? 255 : 0;
        if (index == 6) { unsigned char t = pixel[0]; pixel[0] = pixel[2]; pixel[2] = t; }
    }
}

/* phase 0: committed control; 1: sparse first tile in both layers;
 * 2: sparse final 1x1 edge in both layers. A failed interior skips risky edges. */
static VkResult run_phase(VkPhysicalDevice gpu, VkDevice device, VkQueue graphics, VkQueue sparse_queue,
        uint32_t family, unsigned index, unsigned phase, struct vkd3d_sparse_probe_record *record)
{
    const VkFormat format = (VkFormat)vkd3d_sparse_probe_formats[index];
    const unsigned bytes = index == 0 ? 1 : index == 1 ? 2 : index == 3 ? 8 : 4;
    VkFormat views[] = {format, bytes == 1 ? VK_FORMAT_R8_UINT : bytes == 2 ? VK_FORMAT_R16_UINT :
            bytes == 8 ? VK_FORMAT_R32G32_UINT : VK_FORMAT_R32_UINT};
    VkImageFormatListCreateInfo formats = {.sType = VK_STRUCTURE_TYPE_IMAGE_FORMAT_LIST_CREATE_INFO,
            .viewFormatCount = 2, .pViewFormats = views};
    VkImageCreateInfo info = {.sType = VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO, .pNext = &formats,
            .flags = VK_IMAGE_CREATE_MUTABLE_FORMAT_BIT | VK_IMAGE_CREATE_EXTENDED_USAGE_BIT,
            .imageType = VK_IMAGE_TYPE_2D, .format = format, .extent = {513, 513, 1},
            .mipLevels = 1, .arrayLayers = 2, .samples = VK_SAMPLE_COUNT_4_BIT, .tiling = VK_IMAGE_TILING_OPTIMAL,
            .usage = VK_IMAGE_USAGE_TRANSFER_SRC_BIT | VK_IMAGE_USAGE_TRANSFER_DST_BIT |
                    VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT | VK_IMAGE_USAGE_SAMPLED_BIT | VK_IMAGE_USAGE_STORAGE_BIT};
    VkPhysicalDeviceMemoryProperties memory_props;
    VkImage image = VK_NULL_HANDLE, resolved = VK_NULL_HANDLE;
    VkDeviceMemory image_memory = VK_NULL_HANDLE, resolved_memory = VK_NULL_HANDLE, buffer_memory = VK_NULL_HANDLE;
    VkDeviceMemory tile_memory[2] = {0}, metadata_memory[16] = {0};
    VkSparseImageMemoryRequirements requirements[16], *data_req = NULL;
    VkSparseMemoryBind metadata[16] = {0};
    VkSparseImageMemoryBind binds[2] = {0};
    VkSparseImageMemoryBindInfo image_bind = {0};
    VkSparseImageOpaqueMemoryBindInfo opaque_bind = {0};
    VkBindSparseInfo bind = {.sType = VK_STRUCTURE_TYPE_BIND_SPARSE_INFO};
    VkMemoryRequirements req, tile_req;
    VkBuffer buffer = VK_NULL_HANDLE;
    VkBufferCreateInfo buffer_info = {.sType = VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO,
            .usage = VK_BUFFER_USAGE_TRANSFER_DST_BIT};
    VkCommandPool pool = VK_NULL_HANDLE;
    VkCommandPoolCreateInfo pool_info = {.sType = VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO, .queueFamilyIndex = family};
    VkCommandBuffer cmd;
    VkCommandBufferAllocateInfo cmd_info = {.sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO,
            .level = VK_COMMAND_BUFFER_LEVEL_PRIMARY, .commandBufferCount = 1};
    VkCommandBufferBeginInfo begin = {.sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO};
    VkSubmitInfo submit = {.sType = VK_STRUCTURE_TYPE_SUBMIT_INFO, .commandBufferCount = 1, .pCommandBuffers = &cmd};
    VkFenceCreateInfo fence_info = {.sType = VK_STRUCTURE_TYPE_FENCE_CREATE_INFO};
    VkFence fence = VK_NULL_HANDLE;
    VkMemoryBarrier barrier = {.sType = VK_STRUCTURE_TYPE_MEMORY_BARRIER};
    VkMappedMemoryRange mapped = {.sType = VK_STRUCTURE_TYPE_MAPPED_MEMORY_RANGE, .size = VK_WHOLE_SIZE};
    VkImageMemoryBarrier images[2] = {{0}};
    VkExtent3D extent = {64, 64, 1};
    VkOffset3D offset = {0};
    unsigned char *pixels = NULL, expected[8];
    uint32_t count, i, layer, metadata_count = 0, x, y;
    uint32_t old_bad = record->bad_pixels;
    VkResult vr = VK_SUCCESS;
#define TRY(call) do { vr = (call); if (vr != VK_SUCCESS) { fprintf(stderr, "phase=%u line=%d result=%d\n", phase, __LINE__, vr); goto cleanup; } } while (0)
    vkGetPhysicalDeviceMemoryProperties(gpu, &memory_props);
    TRY(vkCreateFence(device, &fence_info, NULL, &fence));
    if (phase) info.flags |= VK_IMAGE_CREATE_SPARSE_BINDING_BIT | VK_IMAGE_CREATE_SPARSE_RESIDENCY_BIT | VK_IMAGE_CREATE_SPARSE_ALIASED_BIT;
    TRY(vkCreateImage(device, &info, NULL, &image));
    vkGetImageMemoryRequirements(device, image, &req);
    if (!phase)
    {
        TRY(allocate(device, &memory_props, &req, 0, &image_memory));
        TRY(vkBindImageMemory(device, image, image_memory, 0));
    }
    else
    {
        count = 0;
        vkGetImageSparseMemoryRequirements(device, image, &count, NULL);
        if (!count || count > 16) { vr = VK_ERROR_FEATURE_NOT_PRESENT; goto cleanup; }
        vkGetImageSparseMemoryRequirements(device, image, &count, requirements);
        for (i = 0; i < count; i++)
        {
            if (requirements[i].formatProperties.aspectMask == VK_IMAGE_ASPECT_COLOR_BIT) data_req = &requirements[i];
            else if (requirements[i].formatProperties.aspectMask == VK_IMAGE_ASPECT_METADATA_BIT)
            {
                unsigned layers = requirements[i].formatProperties.flags & VK_SPARSE_IMAGE_FORMAT_SINGLE_MIPTAIL_BIT ? 1 : 2;
                for (layer = 0; layer < layers; layer++)
                {
                    if (metadata_count == 16) { vr = VK_ERROR_FEATURE_NOT_PRESENT; goto cleanup; }
                    tile_req = req; tile_req.size = requirements[i].imageMipTailSize;
                    TRY(allocate(device, &memory_props, &tile_req, 0, &metadata_memory[metadata_count]));
                    metadata[metadata_count].resourceOffset = requirements[i].imageMipTailOffset + layer * requirements[i].imageMipTailStride;
                    metadata[metadata_count].size = tile_req.size;
                    metadata[metadata_count].memory = metadata_memory[metadata_count];
                    metadata[metadata_count].flags = VK_SPARSE_MEMORY_BIND_METADATA_BIT;
                    metadata_count++;
                }
            }
        }
        if (!data_req || !data_req->imageMipTailFirstLod ||
                !data_req->formatProperties.imageGranularity.width || !data_req->formatProperties.imageGranularity.height)
        { vr = VK_ERROR_FEATURE_NOT_PRESENT; goto cleanup; }
        extent = data_req->formatProperties.imageGranularity;
        if (extent.depth != 1 || (uint64_t)extent.width * extent.height * bytes * 4 != 65536)
        { vr = VK_ERROR_FEATURE_NOT_PRESENT; goto cleanup; }
        for (layer = 0; layer < 2; layer++)
        {
            binds[layer].subresource = (VkImageSubresource){VK_IMAGE_ASPECT_COLOR_BIT, 0, layer};
            binds[layer].extent = info.extent;
        }
        image_bind = (VkSparseImageMemoryBindInfo){image, 2, binds};
        bind.imageBindCount = 1; bind.pImageBinds = &image_bind;
        opaque_bind = (VkSparseImageOpaqueMemoryBindInfo){image, metadata_count, metadata};
        if (metadata_count) { bind.imageOpaqueBindCount = 1; bind.pImageOpaqueBinds = &opaque_bind; }
        TRY(vkQueueBindSparse(sparse_queue, 1, &bind, fence));
        TRY(finish(device, fence));
        TRY(vkResetFences(device, 1, &fence));
        bind.imageOpaqueBindCount = 0;
        if (phase == 2)
        {
            offset.x = ((info.extent.width - 1) / extent.width) * extent.width;
            offset.y = ((info.extent.height - 1) / extent.height) * extent.height;
            extent.width = info.extent.width - offset.x;
            extent.height = info.extent.height - offset.y;
        }
        for (layer = 0; layer < 2; layer++)
        {
            tile_req = req; tile_req.size = req.alignment;
            TRY(allocate(device, &memory_props, &tile_req, 0, &tile_memory[layer]));
            binds[layer].offset = offset;
            binds[layer].extent = extent;
            binds[layer].memory = tile_memory[layer];
        }
        printf("BIND phase=%u format=%u samples=4 layers=2 xy=%d,%d extent=%u,%u\n",
                phase, format, offset.x, offset.y, extent.width, extent.height);
        TRY(vkQueueBindSparse(sparse_queue, 1, &bind, fence));
        TRY(finish(device, fence));
        TRY(vkResetFences(device, 1, &fence));
    }
    info.flags = 0; info.pNext = NULL; info.samples = VK_SAMPLE_COUNT_1_BIT;
    info.usage = VK_IMAGE_USAGE_TRANSFER_SRC_BIT | VK_IMAGE_USAGE_TRANSFER_DST_BIT;
    TRY(vkCreateImage(device, &info, NULL, &resolved));
    vkGetImageMemoryRequirements(device, resolved, &req);
    TRY(allocate(device, &memory_props, &req, 0, &resolved_memory));
    TRY(vkBindImageMemory(device, resolved, resolved_memory, 0));
    buffer_info.size = (uint64_t)extent.width * extent.height * bytes * 2;
    TRY(vkCreateBuffer(device, &buffer_info, NULL, &buffer));
    vkGetBufferMemoryRequirements(device, buffer, &req);
    TRY(allocate(device, &memory_props, &req, VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT, &buffer_memory));
    TRY(vkBindBufferMemory(device, buffer, buffer_memory, 0));
    TRY(vkMapMemory(device, buffer_memory, 0, VK_WHOLE_SIZE, 0, (void **)&pixels));
    memset(pixels, 0xcd, (size_t)buffer_info.size);
    mapped.memory = buffer_memory;
    TRY(vkFlushMappedMemoryRanges(device, 1, &mapped));
    TRY(vkCreateCommandPool(device, &pool_info, NULL, &pool));
    cmd_info.commandPool = pool;
    TRY(vkAllocateCommandBuffers(device, &cmd_info, &cmd));
    TRY(vkBeginCommandBuffer(cmd, &begin));
    for (i = 0; i < 2; i++)
    {
        images[i].sType = VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER;
        images[i].dstAccessMask = VK_ACCESS_TRANSFER_WRITE_BIT;
        images[i].newLayout = VK_IMAGE_LAYOUT_GENERAL;
        images[i].srcQueueFamilyIndex = images[i].dstQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED;
        images[i].image = i ? resolved : image;
        images[i].subresourceRange = (VkImageSubresourceRange){VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 2};
    }
    vkCmdPipelineBarrier(cmd, VK_PIPELINE_STAGE_TOP_OF_PIPE_BIT, VK_PIPELINE_STAGE_TRANSFER_BIT, 0, 0, NULL, 0, NULL, 2, images);
    for (layer = 0; layer < 2; layer++)
    {
        VkClearColorValue clear;
        VkImageSubresourceRange range = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, layer, 1};
        expected_color(index, layer, &clear, expected);
        vkCmdClearColorImage(cmd, image, VK_IMAGE_LAYOUT_GENERAL, &clear, 1, &range);
    }
    barrier.srcAccessMask = VK_ACCESS_TRANSFER_WRITE_BIT; barrier.dstAccessMask = VK_ACCESS_TRANSFER_READ_BIT;
    vkCmdPipelineBarrier(cmd, VK_PIPELINE_STAGE_TRANSFER_BIT, VK_PIPELINE_STAGE_TRANSFER_BIT, 0, 1, &barrier, 0, NULL, 0, NULL);
    VkImageResolve resolve = {.srcSubresource = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 2}, .srcOffset = offset,
            .dstSubresource = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 2}, .dstOffset = offset, .extent = extent};
    vkCmdResolveImage(cmd, image, VK_IMAGE_LAYOUT_GENERAL, resolved, VK_IMAGE_LAYOUT_GENERAL, 1, &resolve);
    vkCmdPipelineBarrier(cmd, VK_PIPELINE_STAGE_TRANSFER_BIT, VK_PIPELINE_STAGE_TRANSFER_BIT, 0, 1, &barrier, 0, NULL, 0, NULL);
    VkBufferImageCopy copy = {.imageSubresource = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 2}, .imageOffset = offset, .imageExtent = extent};
    vkCmdCopyImageToBuffer(cmd, resolved, VK_IMAGE_LAYOUT_GENERAL, buffer, 1, &copy);
    barrier.dstAccessMask = VK_ACCESS_HOST_READ_BIT;
    vkCmdPipelineBarrier(cmd, VK_PIPELINE_STAGE_TRANSFER_BIT, VK_PIPELINE_STAGE_HOST_BIT, 0, 1, &barrier, 0, NULL, 0, NULL);
    TRY(vkEndCommandBuffer(cmd));
    TRY(vkQueueSubmit(graphics, 1, &submit, fence));
    TRY(finish(device, fence));
    TRY(vkInvalidateMappedMemoryRanges(device, 1, &mapped));
    for (layer = 0; layer < 2; layer++)
    {
        VkClearColorValue clear;
        expected_color(index, layer, &clear, expected);
        for (y = 0; y < extent.height; y++)
        for (x = 0; x < extent.width; x++)
        {
            size_t pos = ((size_t)layer * extent.height * extent.width + y * extent.width + x) * bytes;
            record->checked_pixels++;
            if (memcmp(pixels + pos, expected, bytes))
            {
                if (record->bad_pixels++ - old_bad < 4)
                    printf("MISMATCH phase=%u layer=%u xy=%u,%u byte0=%02x expected=%02x\n", phase, layer, x, y, pixels[pos], expected[0]);
            }
        }
    }
    printf("READBACK phase=%u bad=%u pixels=%u\n", phase, record->bad_pixels - old_bad, 2 * extent.width * extent.height);
cleanup:
    if (pixels) vkUnmapMemory(device, buffer_memory);
    if (pool) vkDestroyCommandPool(device, pool, NULL);
    if (fence) vkDestroyFence(device, fence, NULL);
    if (buffer) vkDestroyBuffer(device, buffer, NULL);
    if (buffer_memory) vkFreeMemory(device, buffer_memory, NULL);
    if (resolved) vkDestroyImage(device, resolved, NULL);
    if (resolved_memory) vkFreeMemory(device, resolved_memory, NULL);
    if (image) vkDestroyImage(device, image, NULL);
    if (image_memory) vkFreeMemory(device, image_memory, NULL);
    for (i = 0; i < 2; i++) if (tile_memory[i]) vkFreeMemory(device, tile_memory[i], NULL);
    for (i = 0; i < 16; i++) if (metadata_memory[i]) vkFreeMemory(device, metadata_memory[i], NULL);
    return vr;
#undef TRY
}

int main(int argc, char **argv)
{
    VkApplicationInfo app = {.sType = VK_STRUCTURE_TYPE_APPLICATION_INFO,
            .pApplicationName = "Helios sparse behavior preflight", .apiVersion = VK_API_VERSION_1_3};
    VkInstanceCreateInfo info = {.sType = VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO, .pApplicationInfo = &app};
    const char *layer_name = "VK_LAYER_KHRONOS_validation", *ext_name = VK_EXT_DEBUG_UTILS_EXTENSION_NAME;
    VkLayerProperties layers[64];
    VkExtensionProperties extensions[512];
    VkDebugUtilsMessengerCreateInfoEXT debug = {.sType = VK_STRUCTURE_TYPE_DEBUG_UTILS_MESSENGER_CREATE_INFO_EXT,
            .messageSeverity = VK_DEBUG_UTILS_MESSAGE_SEVERITY_WARNING_BIT_EXT | VK_DEBUG_UTILS_MESSAGE_SEVERITY_ERROR_BIT_EXT,
            .messageType = VK_DEBUG_UTILS_MESSAGE_TYPE_GENERAL_BIT_EXT | VK_DEBUG_UTILS_MESSAGE_TYPE_VALIDATION_BIT_EXT |
                    VK_DEBUG_UTILS_MESSAGE_TYPE_PERFORMANCE_BIT_EXT, .pfnUserCallback = debug_message};
    VkDebugUtilsMessengerEXT messenger = VK_NULL_HANDLE;
    VkPhysicalDeviceProperties2 props = {.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_PROPERTIES_2};
    VkPhysicalDeviceDriverProperties driver = {.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_DRIVER_PROPERTIES};
    VkPhysicalDeviceIDProperties id = {.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_ID_PROPERTIES};
    VkPhysicalDevice gpus[16], gpu = VK_NULL_HANDLE;
    VkPhysicalDeviceFeatures features;
    VkQueueFamilyProperties queues[64];
    VkInstance instance = VK_NULL_HANDLE;
    VkDevice device = VK_NULL_HANDLE;
    VkQueue graphics, sparse;
    float priority = 1.0f;
    VkDeviceQueueCreateInfo qinfo[2] = {{0}};
    VkDeviceCreateInfo create = {.sType = VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO};
    struct vkd3d_sparse_probe_record record = {0};
    uint32_t count = 64, i, matches = 0, graphics_family = UINT32_MAX, sparse_family = UINT32_MAX;
    int venus, index, result = 77, have_record = 0, maintenance7 = 0;
    unsigned phase;
    VkResult vr;
    setvbuf(stdout, NULL, _IONBF, 0);
    if (argc != 3 || (strcmp(argv[1], "--host") && strcmp(argv[1], "--venus")) ||
            strlen(argv[2]) != 1 || argv[2][0] < '0' || argv[2][0] > '6')
    { fprintf(stderr, "Usage: %s --host|--venus <case 0..6>\n", argv[0]); return 2; }
    venus = !strcmp(argv[1], "--venus"); index = argv[2][0] - '0';
    if (vkEnumerateInstanceLayerProperties(&count, layers) == VK_SUCCESS)
        for (i = 0; i < count; i++) if (!strcmp(layers[i].layerName, layer_name))
        { info.enabledLayerCount = 1; info.ppEnabledLayerNames = &layer_name;
          info.enabledExtensionCount = 1; info.ppEnabledExtensionNames = &ext_name; info.pNext = &debug; }
    if (vkCreateInstance(&info, NULL, &instance) != VK_SUCCESS) goto cleanup;
    if (info.enabledLayerCount)
        if (((PFN_vkCreateDebugUtilsMessengerEXT)vkGetInstanceProcAddr(instance, "vkCreateDebugUtilsMessengerEXT"))(
                instance, &debug, NULL, &messenger) != VK_SUCCESS) goto cleanup;
    count = 16;
    if (vkEnumeratePhysicalDevices(instance, &count, gpus) != VK_SUCCESS) goto cleanup;
    props.pNext = &driver; driver.pNext = &id;
    for (i = 0; i < count; i++)
    {
        vkGetPhysicalDeviceProperties2(gpus[i], &props);
        if ((venus && driver.driverID == VK_DRIVER_ID_MESA_VENUS) || (!venus &&
                driver.driverID != VK_DRIVER_ID_MESA_VENUS && (props.properties.deviceType == VK_PHYSICAL_DEVICE_TYPE_DISCRETE_GPU ||
                    props.properties.deviceType == VK_PHYSICAL_DEVICE_TYPE_INTEGRATED_GPU)))
        { gpu = gpus[i]; matches++; }
    }
    if (matches != 1) { fprintf(stderr, "Expected one matching GPU, got %u.\n", matches); goto cleanup; }
    vkGetPhysicalDeviceProperties2(gpu, &props);
    count = 512;
    if (vkEnumerateDeviceExtensionProperties(gpu, NULL, &count, extensions) != VK_SUCCESS) goto cleanup;
    for (i = 0; i < count; i++)
        if (!strcmp(extensions[i].extensionName, VK_KHR_MAINTENANCE_7_EXTENSION_NAME)) maintenance7 = 1;
    if (!vkd3d_sparse_probe_key_init(&record.key, vkGetPhysicalDeviceProperties2, gpu,
            vkd3d_sparse_probe_formats[index], maintenance7))
    { fprintf(stderr, "Incomplete stack identity; cannot publish a reusable result.\n"); goto cleanup; }
    printf("DEVICE %s driver_id=%u driver=%s version=%u api=%u format=%u samples=4 validation=%u\n",
            props.properties.deviceName, driver.driverID, driver.driverInfo, props.properties.driverVersion,
            props.properties.apiVersion, record.key.format, info.enabledLayerCount);
#ifdef _WIN32
    printf("PROCESS pid=%lu\n", (unsigned long)GetCurrentProcessId());
    HANDLE snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32, GetCurrentProcessId());
    MODULEENTRY32 module = {0}; module.dwSize = sizeof(module);
    if (snapshot != INVALID_HANDLE_VALUE)
    {
        if (Module32First(snapshot, &module)) do { printf("MODULE %s\n", module.szExePath); } while (Module32Next(snapshot, &module));
        CloseHandle(snapshot);
    }
#endif
    /* Invalidate a previous success before any work that can fail or time out. */
    if (!publish(&record)) { fprintf(stderr, "Cannot publish UNKNOWN cache record.\n"); goto cleanup; }
    have_record = 1;
    vkGetPhysicalDeviceFeatures(gpu, &features);
    if (!features.sparseBinding || !features.sparseResidencyImage2D || !features.sparseResidency4Samples ||
            !features.sparseResidencyAliased || !features.shaderStorageImageMultisample || !props.properties.sparseProperties.residencyNonResidentStrict)
        goto cleanup;
    count = 64; vkGetPhysicalDeviceQueueFamilyProperties(gpu, &count, queues);
    for (i = 0; i < count; i++)
    {
        if (queues[i].queueCount && (queues[i].queueFlags & VK_QUEUE_GRAPHICS_BIT) && graphics_family == UINT32_MAX) graphics_family = i;
        if (queues[i].queueCount && (queues[i].queueFlags & VK_QUEUE_SPARSE_BINDING_BIT) && sparse_family == UINT32_MAX) sparse_family = i;
    }
    if (graphics_family == UINT32_MAX || sparse_family == UINT32_MAX) goto cleanup;
    qinfo[0].sType = qinfo[1].sType = VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO;
    qinfo[0].queueFamilyIndex = graphics_family; qinfo[1].queueFamilyIndex = sparse_family;
    qinfo[0].queueCount = qinfo[1].queueCount = 1; qinfo[0].pQueuePriorities = qinfo[1].pQueuePriorities = &priority;
    create.queueCreateInfoCount = graphics_family == sparse_family ? 1 : 2; create.pQueueCreateInfos = qinfo;
    create.pEnabledFeatures = &features;
    if (vkCreateDevice(gpu, &create, NULL, &device) != VK_SUCCESS) goto cleanup;
    vkGetDeviceQueue(device, graphics_family, 0, &graphics); vkGetDeviceQueue(device, sparse_family, 0, &sparse);
    for (phase = 0; phase < 3; phase++)
    {
        vr = run_phase(gpu, device, graphics, sparse, graphics_family, index, phase, &record);
        if (validation_errors || timed_out || (phase == 0 && (vr != VK_SUCCESS || record.bad_pixels))) goto cleanup;
        if (vr != VK_SUCCESS || record.bad_pixels)
        {
            /* Resource pressure/setup failure is inconclusive, not a driver bug. */
            if (record.bad_pixels || vr == VK_ERROR_DEVICE_LOST || vr == VK_ERROR_FORMAT_NOT_SUPPORTED)
            { record.status = VKD3D_SPARSE_PROBE_FAIL; result = 1; }
            goto cleanup;
        }
        record.completed_phases |= 1u << phase;
    }
    record.status = VKD3D_SPARSE_PROBE_PASS; result = 0;
cleanup:
    if (device) vkDestroyDevice(device, NULL);
    if (messenger) ((PFN_vkDestroyDebugUtilsMessengerEXT)vkGetInstanceProcAddr(instance, "vkDestroyDebugUtilsMessengerEXT"))(instance, messenger, NULL);
    if (instance) vkDestroyInstance(instance, NULL);
    if (validation_errors || timed_out) { record.status = VKD3D_SPARSE_PROBE_UNKNOWN; result = 77; }
    if (have_record && !publish(&record)) { fprintf(stderr, "Failed to publish final result.\n"); result = 77; }
    printf("RESULT exit=%d status=%u phases=%u checked=%u bad=%u validation_errors=%u timeout=%d\n",
            result, record.status, record.completed_phases, record.checked_pixels, record.bad_pixels, validation_errors, timed_out);
    return result;
}
