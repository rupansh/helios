/* Isolate raw D32 transfer, sampling and fragment depth writes on committed
 * 16x1 images. This is a Vulkan boundary diagnostic, not native D3D12 evidence.
 * Windows execution must use an interactive scheduled task. No idle waits.
 * Generated shader headers come from the vulkan_depth_copy_probe shader folder.
 * Exit 0=all exact, 1=completed bit differences, 77=inconclusive/setup failure.
 */
#define _CRT_SECURE_NO_WARNINGS
#include <vulkan/vulkan.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#ifdef _WIN32
#include <windows.h>
#include <tlhelp32.h>
#endif
#include "depth_fullscreen.h"
#include "depth_write.h"
#include "depth_write_mixed.h"
#include "depth_read.h"
#include "depth_read_ms.h"
#include "depth_read_color.h"

#define COUNT(a) ((uint32_t)(sizeof(a) / sizeof((a)[0])))
#define TRY(call) do { VkResult result_ = (call); if (result_ != VK_SUCCESS) { \
    fprintf(stderr, "SETUP %s = %d line=%d\n", #call, result_, __LINE__); goto cleanup; } } while (0)
static unsigned validation_errors, differences, checked, completed_cases;
static int timed_out;
static const uint32_t patterns[] = {0, 0x80000000, 1, 0x007fffff, 0x00800000,
    0x3dcccccd, 0x3f800000, 0x40000000, 0xbf800000, 0x7f7fffff,
    0x7f800000, 0xff800000, 0x7fc12345, 0x7f812345, 0xffc54321, 0x80000001};

static VKAPI_ATTR VkBool32 VKAPI_CALL debug_message(VkDebugUtilsMessageSeverityFlagBitsEXT severity,
        VkDebugUtilsMessageTypeFlagsEXT type, const VkDebugUtilsMessengerCallbackDataEXT *data, void *user)
{
    (void)type; (void)user;
    if (severity & VK_DEBUG_UTILS_MESSAGE_SEVERITY_ERROR_BIT_EXT) validation_errors++;
    fprintf(stderr, "VALIDATION %u %s\n", severity, data->pMessage);
    return VK_FALSE;
}

static VkResult allocate(VkDevice device, const VkPhysicalDeviceMemoryProperties *properties,
        const VkMemoryRequirements *requirements, VkMemoryPropertyFlags flags, VkDeviceMemory *memory)
{
    VkMemoryAllocateInfo info = {.sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,
        .allocationSize = requirements->size};
    for (; info.memoryTypeIndex < properties->memoryTypeCount; info.memoryTypeIndex++)
        if ((requirements->memoryTypeBits & (1u << info.memoryTypeIndex)) &&
                (properties->memoryTypes[info.memoryTypeIndex].propertyFlags & flags) == flags)
            return vkAllocateMemory(device, &info, NULL, memory);
    return VK_ERROR_FEATURE_NOT_PRESENT;
}

static void barrier(VkCommandBuffer cmd, VkPipelineStageFlags src_stage, VkAccessFlags src_access,
        VkPipelineStageFlags dst_stage, VkAccessFlags dst_access)
{
    VkMemoryBarrier info = {.sType = VK_STRUCTURE_TYPE_MEMORY_BARRIER,
        .srcAccessMask = src_access, .dstAccessMask = dst_access};
    vkCmdPipelineBarrier(cmd, src_stage, dst_stage, 0, 1, &info, 0, NULL, 0, NULL);
}

static void check(const char *stage, unsigned samples, unsigned mixed,
        const uint32_t *words, unsigned count, unsigned stride)
{
    unsigned bad = 0;
    for (unsigned i = 0; i < count; i++)
    {
        uint32_t expected = patterns[i % COUNT(patterns)];
        checked++;
        if (words[i * stride] == expected) continue;
        bad++; differences++;
        printf("DIFF stage=%s samples=%u mixed=%u index=%u expected=%08x actual=%08x\n",
                stage, samples, mixed, i, expected, words[i * stride]);
    }
    printf("CHECK stage=%s samples=%u mixed=%u checked=%u bad=%u\n", stage, samples, mixed, count, bad);
}

static int run_case(VkPhysicalDevice gpu, VkDevice device, VkQueue queue, uint32_t family,
        unsigned samples, unsigned mixed)
{
    VkPhysicalDeviceMemoryProperties properties;
    VkMemoryRequirements requirements;
    VkImage images[2] = {0};
    VkImageView views[2] = {0};
    VkDeviceMemory memories[2] = {0}, buffer_memory = VK_NULL_HANDLE;
    VkBuffer buffer = VK_NULL_HANDLE;
    uint32_t *data = NULL;
    VkShaderModule shaders[4] = {0};
    VkPipeline pipelines[3] = {0};
    VkDescriptorSetLayout set_layout = VK_NULL_HANDLE;
    VkDescriptorPool descriptor_pool = VK_NULL_HANDLE;
    VkPipelineLayout pipeline_layout = VK_NULL_HANDLE;
    VkDescriptorSet sets[2];
    VkCommandPool command_pool = VK_NULL_HANDLE;
    VkCommandBuffer cmd;
    VkFence fence = VK_NULL_HANDLE;
    int completed = 0;
    unsigned i;
    VkFormat formats[2] = {VK_FORMAT_D32_SFLOAT, VK_FORMAT_R32G32_UINT};
    VkImageAspectFlags aspects[2] = {VK_IMAGE_ASPECT_DEPTH_BIT, VK_IMAGE_ASPECT_COLOR_BIT};
    VkBufferCreateInfo buffer_info = {.sType = VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO, .size = 4096,
        .usage = VK_BUFFER_USAGE_STORAGE_BUFFER_BIT | VK_BUFFER_USAGE_TRANSFER_SRC_BIT | VK_BUFFER_USAGE_TRANSFER_DST_BIT};
    VkImageCreateInfo image_info = {.sType = VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO,
        .imageType = VK_IMAGE_TYPE_2D, .extent = {16, 1, 1}, .mipLevels = 1, .arrayLayers = 1,
        .samples = (VkSampleCountFlagBits)samples, .tiling = VK_IMAGE_TILING_OPTIMAL};
    VkImageViewCreateInfo view_info = {.sType = VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO,
        .viewType = VK_IMAGE_VIEW_TYPE_2D, .subresourceRange = {.levelCount = 1, .layerCount = 1}};
    VkDescriptorSetLayoutBinding bindings[2] = {
        {0, VK_DESCRIPTOR_TYPE_STORAGE_BUFFER, 1, VK_SHADER_STAGE_FRAGMENT_BIT | VK_SHADER_STAGE_COMPUTE_BIT, NULL},
        {1, VK_DESCRIPTOR_TYPE_SAMPLED_IMAGE, 1, VK_SHADER_STAGE_COMPUTE_BIT, NULL}};
    VkDescriptorSetLayoutCreateInfo set_info = {.sType = VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO,
        .bindingCount = 2, .pBindings = bindings};
    VkPushConstantRange push_range = {VK_SHADER_STAGE_COMPUTE_BIT | VK_SHADER_STAGE_FRAGMENT_BIT, 0, 16};
    VkPipelineLayoutCreateInfo layout_info = {.sType = VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO,
        .setLayoutCount = 1, .pSetLayouts = &set_layout, .pushConstantRangeCount = 1, .pPushConstantRanges = &push_range};
    VkDescriptorPoolSize pool_sizes[2] = {{VK_DESCRIPTOR_TYPE_STORAGE_BUFFER, 2}, {VK_DESCRIPTOR_TYPE_SAMPLED_IMAGE, 2}};
    VkDescriptorPoolCreateInfo pool_info = {.sType = VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO,
        .maxSets = 2, .poolSizeCount = 2, .pPoolSizes = pool_sizes};
    VkDescriptorSetAllocateInfo set_allocate = {.sType = VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO,
        .descriptorSetCount = 1, .pSetLayouts = &set_layout};
    VkDescriptorBufferInfo buffer_descriptor = {.range = 4096};
    VkDescriptorImageInfo image_descriptor = {.imageLayout = VK_IMAGE_LAYOUT_GENERAL};
    VkWriteDescriptorSet writes[2] = {
        {.sType = VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET, .dstBinding = 0, .descriptorCount = 1,
            .descriptorType = VK_DESCRIPTOR_TYPE_STORAGE_BUFFER, .pBufferInfo = &buffer_descriptor},
        {.sType = VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET, .dstBinding = 1, .descriptorCount = 1,
            .descriptorType = VK_DESCRIPTOR_TYPE_SAMPLED_IMAGE, .pImageInfo = &image_descriptor}};
    const uint32_t *code[4] = {depth_fullscreen, mixed ? depth_write_mixed : depth_write,
        samples == 1 ? depth_read : depth_read_ms, depth_read_color};
    size_t sizes[4] = {sizeof(depth_fullscreen), mixed ? sizeof(depth_write_mixed) : sizeof(depth_write),
        samples == 1 ? sizeof(depth_read) : sizeof(depth_read_ms), sizeof(depth_read_color)};
    VkShaderModuleCreateInfo shader_info = {.sType = VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO};
    VkPipelineShaderStageCreateInfo stages[2] = {
        {.sType = VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO, .stage = VK_SHADER_STAGE_VERTEX_BIT, .pName = "main"},
        {.sType = VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO, .stage = VK_SHADER_STAGE_FRAGMENT_BIT, .pName = "main"}};
    VkPipelineVertexInputStateCreateInfo vertex = {.sType = VK_STRUCTURE_TYPE_PIPELINE_VERTEX_INPUT_STATE_CREATE_INFO};
    VkPipelineInputAssemblyStateCreateInfo assembly = {.sType = VK_STRUCTURE_TYPE_PIPELINE_INPUT_ASSEMBLY_STATE_CREATE_INFO,
        .topology = VK_PRIMITIVE_TOPOLOGY_TRIANGLE_LIST};
    VkViewport viewport = {0, 0, 16, 1, 0, 1};
    VkRect2D rect = {{0, 0}, {16, 1}};
    VkPipelineViewportStateCreateInfo viewport_info = {.sType = VK_STRUCTURE_TYPE_PIPELINE_VIEWPORT_STATE_CREATE_INFO,
        .viewportCount = 1, .pViewports = &viewport, .scissorCount = 1, .pScissors = &rect};
    VkPipelineRasterizationStateCreateInfo raster = {.sType = VK_STRUCTURE_TYPE_PIPELINE_RASTERIZATION_STATE_CREATE_INFO,
        .polygonMode = VK_POLYGON_MODE_FILL, .cullMode = VK_CULL_MODE_NONE, .lineWidth = 1};
    VkPipelineMultisampleStateCreateInfo multisample = {.sType = VK_STRUCTURE_TYPE_PIPELINE_MULTISAMPLE_STATE_CREATE_INFO,
        .rasterizationSamples = (VkSampleCountFlagBits)samples, .sampleShadingEnable = VK_TRUE, .minSampleShading = 1};
    VkPipelineDepthStencilStateCreateInfo depth = {.sType = VK_STRUCTURE_TYPE_PIPELINE_DEPTH_STENCIL_STATE_CREATE_INFO,
        .depthTestEnable = VK_TRUE, .depthWriteEnable = VK_TRUE, .depthCompareOp = VK_COMPARE_OP_ALWAYS, .maxDepthBounds = 1};
    VkPipelineColorBlendAttachmentState blend_attachment = {.colorWriteMask = VK_COLOR_COMPONENT_R_BIT | VK_COLOR_COMPONENT_G_BIT};
    VkPipelineColorBlendStateCreateInfo blend = {.sType = VK_STRUCTURE_TYPE_PIPELINE_COLOR_BLEND_STATE_CREATE_INFO,
        .attachmentCount = 1, .pAttachments = &blend_attachment};
    VkPipelineRenderingCreateInfo rendering_formats = {.sType = VK_STRUCTURE_TYPE_PIPELINE_RENDERING_CREATE_INFO,
        .colorAttachmentCount = 1, .pColorAttachmentFormats = &formats[1], .depthAttachmentFormat = VK_FORMAT_D32_SFLOAT};
    VkGraphicsPipelineCreateInfo graphics_info = {.sType = VK_STRUCTURE_TYPE_GRAPHICS_PIPELINE_CREATE_INFO,
        .pNext = &rendering_formats, .stageCount = 2, .pStages = stages, .pVertexInputState = &vertex,
        .pInputAssemblyState = &assembly, .pViewportState = &viewport_info, .pRasterizationState = &raster,
        .pMultisampleState = &multisample, .pDepthStencilState = &depth, .pColorBlendState = &blend};
    VkComputePipelineCreateInfo compute_info = {.sType = VK_STRUCTURE_TYPE_COMPUTE_PIPELINE_CREATE_INFO,
        .stage = {.sType = VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO, .stage = VK_SHADER_STAGE_COMPUTE_BIT, .pName = "main"}};
    VkCommandPoolCreateInfo command_pool_info = {.sType = VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO, .queueFamilyIndex = family};
    VkCommandBufferAllocateInfo command_info = {.sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO,
        .level = VK_COMMAND_BUFFER_LEVEL_PRIMARY, .commandBufferCount = 1};
    VkCommandBufferBeginInfo begin = {.sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO};
    VkImageMemoryBarrier image_barriers[2] = {{0}};
    VkBufferImageCopy copy = {.imageSubresource = {.aspectMask = VK_IMAGE_ASPECT_DEPTH_BIT, .layerCount = 1},
        .imageExtent = {16, 1, 1}};
    struct { uint32_t samples, offset, count, bytes; } args = {samples, 256, 16 * samples, 4};
    VkRenderingAttachmentInfo attachments[2] = {{0}};
    VkRenderingInfo rendering = {.sType = VK_STRUCTURE_TYPE_RENDERING_INFO, .renderArea = {{0, 0}, {16, 1}},
        .layerCount = 1, .colorAttachmentCount = 1, .pColorAttachments = &attachments[1], .pDepthAttachment = &attachments[0]};
    VkFenceCreateInfo fence_info = {.sType = VK_STRUCTURE_TYPE_FENCE_CREATE_INFO};
    VkSubmitInfo submit = {.sType = VK_STRUCTURE_TYPE_SUBMIT_INFO, .commandBufferCount = 1, .pCommandBuffers = &cmd};
    VkResult vr;

    printf("CASE samples=%u mixed=%u\n", samples, mixed);
    vkGetPhysicalDeviceMemoryProperties(gpu, &properties);
    TRY(vkCreateBuffer(device, &buffer_info, NULL, &buffer));
    vkGetBufferMemoryRequirements(device, buffer, &requirements);
    TRY(allocate(device, &properties, &requirements, VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | VK_MEMORY_PROPERTY_HOST_COHERENT_BIT, &buffer_memory));
    TRY(vkBindBufferMemory(device, buffer, buffer_memory, 0));
    TRY(vkMapMemory(device, buffer_memory, 0, VK_WHOLE_SIZE, 0, (void **)&data));
    for (i = 0; i < 1024; i++) data[i] = i < args.count ? patterns[i % COUNT(patterns)] : 0xdeadbeef;
    for (i = 0; i < 2; i++)
    {
        image_info.format = formats[i];
        image_info.usage = VK_IMAGE_USAGE_TRANSFER_SRC_BIT | VK_IMAGE_USAGE_TRANSFER_DST_BIT | VK_IMAGE_USAGE_SAMPLED_BIT |
                (i ? VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT : VK_IMAGE_USAGE_DEPTH_STENCIL_ATTACHMENT_BIT);
        TRY(vkCreateImage(device, &image_info, NULL, &images[i]));
        vkGetImageMemoryRequirements(device, images[i], &requirements);
        TRY(allocate(device, &properties, &requirements, VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT, &memories[i]));
        TRY(vkBindImageMemory(device, images[i], memories[i], 0));
        view_info.image = images[i]; view_info.format = formats[i]; view_info.subresourceRange.aspectMask = aspects[i];
        TRY(vkCreateImageView(device, &view_info, NULL, &views[i]));
    }
    TRY(vkCreateDescriptorSetLayout(device, &set_info, NULL, &set_layout));
    TRY(vkCreatePipelineLayout(device, &layout_info, NULL, &pipeline_layout));
    TRY(vkCreateDescriptorPool(device, &pool_info, NULL, &descriptor_pool));
    set_allocate.descriptorPool = descriptor_pool; buffer_descriptor.buffer = buffer;
    for (i = 0; i < 2; i++)
    {
        TRY(vkAllocateDescriptorSets(device, &set_allocate, &sets[i]));
        writes[0].dstSet = writes[1].dstSet = sets[i]; image_descriptor.imageView = views[i];
        vkUpdateDescriptorSets(device, 2, writes, 0, NULL);
    }
    for (i = 0; i < 4; i++)
    {
        shader_info.pCode = code[i]; shader_info.codeSize = sizes[i];
        TRY(vkCreateShaderModule(device, &shader_info, NULL, &shaders[i]));
    }
    stages[0].module = shaders[0]; stages[1].module = shaders[1]; graphics_info.layout = pipeline_layout;
    TRY(vkCreateGraphicsPipelines(device, VK_NULL_HANDLE, 1, &graphics_info, NULL, &pipelines[0]));
    compute_info.layout = pipeline_layout;
    for (i = 1; i < 3; i++)
    {
        if (i == 2 && samples == 1) continue;
        compute_info.stage.module = shaders[i + 1];
        TRY(vkCreateComputePipelines(device, VK_NULL_HANDLE, 1, &compute_info, NULL, &pipelines[i]));
    }
    TRY(vkCreateCommandPool(device, &command_pool_info, NULL, &command_pool));
    command_info.commandPool = command_pool;
    TRY(vkAllocateCommandBuffers(device, &command_info, &cmd));
    TRY(vkBeginCommandBuffer(cmd, &begin));
    barrier(cmd, VK_PIPELINE_STAGE_HOST_BIT, VK_ACCESS_HOST_WRITE_BIT, VK_PIPELINE_STAGE_ALL_COMMANDS_BIT,
            VK_ACCESS_MEMORY_READ_BIT | VK_ACCESS_MEMORY_WRITE_BIT);
    for (i = 0; i < 2; i++)
    {
        image_barriers[i].sType = VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER;
        image_barriers[i].newLayout = VK_IMAGE_LAYOUT_GENERAL;
        image_barriers[i].srcQueueFamilyIndex = image_barriers[i].dstQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED;
        image_barriers[i].dstAccessMask = VK_ACCESS_MEMORY_READ_BIT | VK_ACCESS_MEMORY_WRITE_BIT;
        image_barriers[i].image = images[i];
        image_barriers[i].subresourceRange = (VkImageSubresourceRange){aspects[i], 0, 1, 0, 1};
    }
    vkCmdPipelineBarrier(cmd, VK_PIPELINE_STAGE_TOP_OF_PIPE_BIT, VK_PIPELINE_STAGE_ALL_COMMANDS_BIT,
            0, 0, NULL, 0, NULL, 2, image_barriers);
    vkCmdBindDescriptorSets(cmd, VK_PIPELINE_BIND_POINT_COMPUTE, pipeline_layout, 0, 1, &sets[0], 0, NULL);
    vkCmdBindPipeline(cmd, VK_PIPELINE_BIND_POINT_COMPUTE, pipelines[1]);
    if (samples == 1)
    {
        vkCmdCopyBufferToImage(cmd, buffer, images[0], VK_IMAGE_LAYOUT_GENERAL, 1, &copy);
        barrier(cmd, VK_PIPELINE_STAGE_ALL_COMMANDS_BIT, VK_ACCESS_MEMORY_WRITE_BIT,
                VK_PIPELINE_STAGE_ALL_COMMANDS_BIT, VK_ACCESS_MEMORY_READ_BIT | VK_ACCESS_MEMORY_WRITE_BIT);
        copy.bufferOffset = 128 * sizeof(uint32_t);
        vkCmdCopyImageToBuffer(cmd, images[0], VK_IMAGE_LAYOUT_GENERAL, buffer, 1, &copy);
        barrier(cmd, VK_PIPELINE_STAGE_ALL_COMMANDS_BIT, VK_ACCESS_MEMORY_READ_BIT | VK_ACCESS_MEMORY_WRITE_BIT,
                VK_PIPELINE_STAGE_ALL_COMMANDS_BIT, VK_ACCESS_MEMORY_READ_BIT | VK_ACCESS_MEMORY_WRITE_BIT);
        vkCmdPushConstants(cmd, pipeline_layout, push_range.stageFlags, 0, sizeof(args), &args);
        vkCmdDispatch(cmd, 1, 1, 1);
        barrier(cmd, VK_PIPELINE_STAGE_ALL_COMMANDS_BIT, VK_ACCESS_MEMORY_READ_BIT | VK_ACCESS_MEMORY_WRITE_BIT,
                VK_PIPELINE_STAGE_ALL_COMMANDS_BIT, VK_ACCESS_MEMORY_READ_BIT | VK_ACCESS_MEMORY_WRITE_BIT);
    }
    for (i = 0; i < 2; i++)
    {
        attachments[i].sType = VK_STRUCTURE_TYPE_RENDERING_ATTACHMENT_INFO;
        attachments[i].imageView = views[i]; attachments[i].imageLayout = VK_IMAGE_LAYOUT_GENERAL;
        attachments[i].loadOp = VK_ATTACHMENT_LOAD_OP_DONT_CARE; attachments[i].storeOp = VK_ATTACHMENT_STORE_OP_STORE;
    }
    vkCmdBindDescriptorSets(cmd, VK_PIPELINE_BIND_POINT_GRAPHICS, pipeline_layout, 0, 1, &sets[0], 0, NULL);
    vkCmdBindPipeline(cmd, VK_PIPELINE_BIND_POINT_GRAPHICS, pipelines[0]);
    vkCmdPushConstants(cmd, pipeline_layout, push_range.stageFlags, 0, sizeof(args), &args);
    vkCmdBeginRendering(cmd, &rendering); vkCmdDraw(cmd, 3, 1, 0, 0); vkCmdEndRendering(cmd);
    barrier(cmd, VK_PIPELINE_STAGE_ALL_COMMANDS_BIT, VK_ACCESS_MEMORY_READ_BIT | VK_ACCESS_MEMORY_WRITE_BIT,
            VK_PIPELINE_STAGE_ALL_COMMANDS_BIT, VK_ACCESS_MEMORY_READ_BIT | VK_ACCESS_MEMORY_WRITE_BIT);
    args.offset = 384;
    vkCmdPushConstants(cmd, pipeline_layout, push_range.stageFlags, 0, sizeof(args), &args);
    vkCmdDispatch(cmd, 1, 1, 1);
    barrier(cmd, VK_PIPELINE_STAGE_ALL_COMMANDS_BIT, VK_ACCESS_MEMORY_READ_BIT | VK_ACCESS_MEMORY_WRITE_BIT,
            VK_PIPELINE_STAGE_ALL_COMMANDS_BIT, VK_ACCESS_MEMORY_READ_BIT | VK_ACCESS_MEMORY_WRITE_BIT);
    if (samples == 1)
    {
        copy.bufferOffset = 704 * sizeof(uint32_t);
        vkCmdCopyImageToBuffer(cmd, images[0], VK_IMAGE_LAYOUT_GENERAL, buffer, 1, &copy);
        copy.bufferOffset = 512 * sizeof(uint32_t); copy.imageSubresource.aspectMask = VK_IMAGE_ASPECT_COLOR_BIT;
        vkCmdCopyImageToBuffer(cmd, images[1], VK_IMAGE_LAYOUT_GENERAL, buffer, 1, &copy);
    }
    else
    {
        args.offset = 512;
        vkCmdBindPipeline(cmd, VK_PIPELINE_BIND_POINT_COMPUTE, pipelines[2]);
        vkCmdBindDescriptorSets(cmd, VK_PIPELINE_BIND_POINT_COMPUTE, pipeline_layout, 0, 1, &sets[1], 0, NULL);
        vkCmdPushConstants(cmd, pipeline_layout, push_range.stageFlags, 0, sizeof(args), &args);
        vkCmdDispatch(cmd, 1, 1, 1);
    }
    barrier(cmd, VK_PIPELINE_STAGE_ALL_COMMANDS_BIT, VK_ACCESS_MEMORY_WRITE_BIT, VK_PIPELINE_STAGE_HOST_BIT, VK_ACCESS_HOST_READ_BIT);
    TRY(vkEndCommandBuffer(cmd));
    TRY(vkCreateFence(device, &fence_info, NULL, &fence));
    TRY(vkQueueSubmit(queue, 1, &submit, fence));
    /* A timeout is not consumer release. Retain every pending object until
     * this private submission completes or the device reports loss. */
    while ((vr = vkWaitForFences(device, 1, &fence, VK_TRUE, 5000000000ull)) == VK_TIMEOUT)
    { timed_out = 1; fprintf(stderr, "INCONCLUSIVE timeout; retaining pending objects.\n"); }
    TRY(vr);
    if (samples == 1)
    {
        check("transfer-raw", samples, mixed, data + 128, args.count, 1);
        check("transfer-fetch", samples, mixed, data + 256, args.count, 1);
        check("fragment-raw", samples, mixed, data + 704, args.count, 1);
    }
    check("fragment-fetch", samples, mixed, data + 384, args.count, 1);
    check("fragment-input-witness", samples, mixed, data + 512, args.count, 2);
    check("fragment-bitcast-witness", samples, mixed, data + 513, args.count, 2);
    completed = 1; completed_cases++;
cleanup:
    vkDestroyFence(device, fence, NULL);
    vkDestroyCommandPool(device, command_pool, NULL);
    for (i = 0; i < COUNT(pipelines); i++) vkDestroyPipeline(device, pipelines[i], NULL);
    for (i = 0; i < COUNT(shaders); i++) vkDestroyShaderModule(device, shaders[i], NULL);
    vkDestroyDescriptorPool(device, descriptor_pool, NULL);
    vkDestroyPipelineLayout(device, pipeline_layout, NULL);
    vkDestroyDescriptorSetLayout(device, set_layout, NULL);
    for (i = 0; i < 2; i++)
    {
        vkDestroyImageView(device, views[i], NULL); vkDestroyImage(device, images[i], NULL);
        vkFreeMemory(device, memories[i], NULL);
    }
    if (data) vkUnmapMemory(device, buffer_memory);
    vkDestroyBuffer(device, buffer, NULL); vkFreeMemory(device, buffer_memory, NULL);
    return completed;
}

int main(int argc, char **argv)
{
    VkInstance instance = VK_NULL_HANDLE;
    VkDebugUtilsMessengerEXT messenger = VK_NULL_HANDLE;
    VkDevice device = VK_NULL_HANDLE;
    VkPhysicalDevice gpus[16], gpu = VK_NULL_HANDLE;
    VkExtensionProperties device_extensions[512];
    VkQueueFamilyProperties queues[64];
    VkQueue queue;
    uint32_t count, i, family = UINT32_MAX, matches = 0;
    int venus, result = 77;
    float priority = 1;
    const char *layer = "VK_LAYER_KHRONOS_validation", *debug_extension = VK_EXT_DEBUG_UTILS_EXTENSION_NAME;
    const char *extensions[2] = {VK_EXT_DEPTH_RANGE_UNRESTRICTED_EXTENSION_NAME, VK_EXT_DEPTH_CLAMP_ZERO_ONE_EXTENSION_NAME};
    VkApplicationInfo app = {.sType = VK_STRUCTURE_TYPE_APPLICATION_INFO, .pApplicationName = "depth-copy-boundary", .apiVersion = VK_API_VERSION_1_3};
    VkDebugUtilsMessengerCreateInfoEXT debug = {.sType = VK_STRUCTURE_TYPE_DEBUG_UTILS_MESSENGER_CREATE_INFO_EXT,
        .messageSeverity = VK_DEBUG_UTILS_MESSAGE_SEVERITY_ERROR_BIT_EXT | VK_DEBUG_UTILS_MESSAGE_SEVERITY_WARNING_BIT_EXT,
        .messageType = VK_DEBUG_UTILS_MESSAGE_TYPE_VALIDATION_BIT_EXT | VK_DEBUG_UTILS_MESSAGE_TYPE_PERFORMANCE_BIT_EXT,
        .pfnUserCallback = debug_message};
    VkInstanceCreateInfo info = {.sType = VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO, .pNext = &debug,
        .pApplicationInfo = &app, .enabledLayerCount = 1, .ppEnabledLayerNames = &layer,
        .enabledExtensionCount = 1, .ppEnabledExtensionNames = &debug_extension};
    VkPhysicalDeviceIDProperties id = {.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_ID_PROPERTIES};
    VkPhysicalDeviceFloatControlsProperties floats = {.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_FLOAT_CONTROLS_PROPERTIES, .pNext = &id};
    VkPhysicalDeviceDriverProperties driver = {.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_DRIVER_PROPERTIES, .pNext = &floats};
    VkPhysicalDeviceProperties2 properties = {.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_PROPERTIES_2, .pNext = &driver};
    VkPhysicalDeviceDepthClampZeroOneFeaturesEXT clamp = {.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_DEPTH_CLAMP_ZERO_ONE_FEATURES_EXT};
    VkPhysicalDeviceVulkan13Features features13 = {.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VULKAN_1_3_FEATURES, .pNext = &clamp};
    VkPhysicalDeviceFeatures2 features = {.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_FEATURES_2, .pNext = &features13};
    VkDeviceQueueCreateInfo queue_info = {.sType = VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO, .queueCount = 1, .pQueuePriorities = &priority};
    VkDeviceCreateInfo create = {.sType = VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO, .pNext = &features,
        .queueCreateInfoCount = 1, .pQueueCreateInfos = &queue_info, .enabledExtensionCount = 2, .ppEnabledExtensionNames = extensions};
    setvbuf(stdout, NULL, _IONBF, 0);
    if (argc != 2 || (strcmp(argv[1], "--host") && strcmp(argv[1], "--venus")))
    { fprintf(stderr, "Usage: %s --host|--venus\n", argv[0]); return 2; }
    venus = !strcmp(argv[1], "--venus");
#ifdef _WIN32
    DWORD session;
    if (!ProcessIdToSessionId(GetCurrentProcessId(), &session) || !session)
    { fprintf(stderr, "Interactive scheduled task required.\n"); return 77; }
    printf("PROCESS pid=%lu session=%lu\n", (unsigned long)GetCurrentProcessId(), (unsigned long)session);
#endif
    TRY(vkCreateInstance(&info, NULL, &instance));
    TRY(((PFN_vkCreateDebugUtilsMessengerEXT)vkGetInstanceProcAddr(instance, "vkCreateDebugUtilsMessengerEXT"))(
            instance, &debug, NULL, &messenger));
    count = COUNT(gpus); TRY(vkEnumeratePhysicalDevices(instance, &count, gpus));
    for (i = 0; i < count; i++)
    {
        vkGetPhysicalDeviceProperties2(gpus[i], &properties);
        if ((venus && driver.driverID == VK_DRIVER_ID_MESA_VENUS) || (!venus && driver.driverID != VK_DRIVER_ID_MESA_VENUS &&
                (properties.properties.deviceType == VK_PHYSICAL_DEVICE_TYPE_DISCRETE_GPU ||
                 properties.properties.deviceType == VK_PHYSICAL_DEVICE_TYPE_INTEGRATED_GPU)))
        { gpu = gpus[i]; matches++; }
    }
    if (matches != 1) { fprintf(stderr, "Expected one matching physical GPU, got %u.\n", matches); goto cleanup; }
    vkGetPhysicalDeviceProperties2(gpu, &properties); vkGetPhysicalDeviceFeatures2(gpu, &features);
    printf("DEVICE name=%s driver_id=%u driver=%s version=%u api=%u denorm32=%u signed_zero_inf_nan32=%u\n",
            properties.properties.deviceName, driver.driverID, driver.driverInfo, properties.properties.driverVersion,
            properties.properties.apiVersion, floats.shaderDenormPreserveFloat32, floats.shaderSignedZeroInfNanPreserveFloat32);
    printf("IDENTITY device_uuid="); for (i = 0; i < VK_UUID_SIZE; i++) printf("%02x", id.deviceUUID[i]);
    printf(" driver_uuid="); for (i = 0; i < VK_UUID_SIZE; i++) printf("%02x", id.driverUUID[i]);
    printf(" luid_valid=%u luid=", id.deviceLUIDValid); for (i = 0; i < VK_LUID_SIZE; i++) printf("%02x", id.deviceLUID[i]);
    printf("\n");
    count = COUNT(device_extensions);
    TRY(vkEnumerateDeviceExtensionProperties(gpu, NULL, &count, device_extensions));
    unsigned maintenance8 = 0;
    for (i = 0; i < count; i++)
        if (!strcmp(device_extensions[i].extensionName, VK_KHR_MAINTENANCE_8_EXTENSION_NAME)) maintenance8 = 1;
    printf("EXTENSION VK_KHR_maintenance8=%u\n", maintenance8);
    if (!features.features.sampleRateShading || !features13.dynamicRendering || !clamp.depthClampZeroOne)
    { fprintf(stderr, "Required sample shading/dynamic rendering/depth clamp feature missing.\n"); goto cleanup; }
    memset(&features.features, 0, sizeof(features.features)); features.features.sampleRateShading = VK_TRUE;
    features13 = (VkPhysicalDeviceVulkan13Features){.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VULKAN_1_3_FEATURES,
        .pNext = &clamp, .dynamicRendering = VK_TRUE};
    count = COUNT(queues); vkGetPhysicalDeviceQueueFamilyProperties(gpu, &count, queues);
    for (i = 0; i < count; i++) if ((queues[i].queueFlags & (VK_QUEUE_GRAPHICS_BIT | VK_QUEUE_COMPUTE_BIT)) ==
            (VK_QUEUE_GRAPHICS_BIT | VK_QUEUE_COMPUTE_BIT)) { family = i; break; }
    if (family == UINT32_MAX) { fprintf(stderr, "Graphics+compute family missing.\n"); goto cleanup; }
    queue_info.queueFamilyIndex = family; TRY(vkCreateDevice(gpu, &create, NULL, &device));
    vkGetDeviceQueue(device, family, 0, &queue);
#ifdef _WIN32
    HANDLE snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32, GetCurrentProcessId());
    MODULEENTRY32 module = {0}; module.dwSize = sizeof(module);
    if (snapshot != INVALID_HANDLE_VALUE)
    {
        if (Module32First(snapshot, &module)) do { printf("MODULE %s\n", module.szExePath); } while (Module32Next(snapshot, &module));
        CloseHandle(snapshot);
    }
#endif
    for (unsigned mixed = 0; mixed < 2; mixed++)
        for (unsigned samples = 1; samples <= 4; samples *= 4)
            if (!run_case(gpu, device, queue, family, samples, mixed)) goto cleanup;
    result = differences ? 1 : 0;
cleanup:
    if (device) vkDestroyDevice(device, NULL);
    if (messenger) ((PFN_vkDestroyDebugUtilsMessengerEXT)vkGetInstanceProcAddr(instance, "vkDestroyDebugUtilsMessengerEXT"))(instance, messenger, NULL);
    if (instance) vkDestroyInstance(instance, NULL);
    if (validation_errors || timed_out) result = 77;
    printf("RESULT exit=%d completed_cases=%u checked=%u differences=%u validation_errors=%u timeout=%d\n",
            result, completed_cases, checked, differences, validation_errors, timed_out);
    return result;
}
