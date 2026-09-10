/* Native EXT DGC pipeline execution-set and preprocess ordering readback.
 * Build: glslangValidator -V tools/vulkan_dgc_probe.comp --vn dgc_probe_cs
 *          -o tools/vulkan_dgc_probe_shader.h
 *        cc -std=c11 -Wall -Wextra -Werror tools/vulkan_dgc_probe.c -lvulkan -o <probe>
 * Run: <probe> --venus|--host. Windows execution requires an interactive task.
 * This Vulkan test complements the native Windows D3D12 acceptance suite.
 */
#include <vulkan/vulkan.h>
#include <inttypes.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "vulkan_dgc_probe_shader.h"

#define REQUIRE(expr) do { if (!(expr)) { fprintf(stderr, "FAIL line %d: %s\n", __LINE__, #expr); exit(1); } } while (0)
#define CHECK(call) do { VkResult r_ = (call); if (r_ != VK_SUCCESS) { fprintf(stderr, "FAIL line %d: %s = %d\n", __LINE__, #call, r_); exit(1); } } while (0)
#define LOAD(name) PFN_vk##name name = (PFN_vk##name)vkGetDeviceProcAddr(device, "vk" #name); REQUIRE(name)

struct buffer { VkBuffer buffer; VkDeviceMemory memory; VkDeviceAddress address; };
static struct buffer make_buffer(VkDevice device, const VkPhysicalDeviceMemoryProperties *props,
        VkDeviceSize size, VkBufferUsageFlags2 usage, uint32_t allowed)
{
    struct buffer b = {0};
    VkBufferUsageFlags2CreateInfo usage2 = {.sType = VK_STRUCTURE_TYPE_BUFFER_USAGE_FLAGS_2_CREATE_INFO, .usage = usage};
    VkBufferCreateInfo info = {.sType = VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO, .pNext = &usage2,
        .size = size, .sharingMode = VK_SHARING_MODE_EXCLUSIVE};
    CHECK(vkCreateBuffer(device, &info, NULL, &b.buffer));
    VkMemoryRequirements req;
    vkGetBufferMemoryRequirements(device, b.buffer, &req);
    VkMemoryAllocateFlagsInfo flags = {.sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_FLAGS_INFO,
        .flags = VK_MEMORY_ALLOCATE_DEVICE_ADDRESS_BIT};
    VkMemoryAllocateInfo allocation = {.sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,
        .pNext = &flags, .allocationSize = req.size};
    VkMemoryPropertyFlags needed = VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | VK_MEMORY_PROPERTY_HOST_COHERENT_BIT;
    for (; allocation.memoryTypeIndex < props->memoryTypeCount; allocation.memoryTypeIndex++)
        if (((req.memoryTypeBits & allowed) & (1u << allocation.memoryTypeIndex)) &&
                (props->memoryTypes[allocation.memoryTypeIndex].propertyFlags & needed) == needed) break;
    REQUIRE(allocation.memoryTypeIndex < props->memoryTypeCount);
    CHECK(vkAllocateMemory(device, &allocation, NULL, &b.memory));
    CHECK(vkBindBufferMemory(device, b.buffer, b.memory, 0));
    VkBufferDeviceAddressInfo address = {.sType = VK_STRUCTURE_TYPE_BUFFER_DEVICE_ADDRESS_INFO, .buffer = b.buffer};
    b.address = vkGetBufferDeviceAddress(device, &address);
    REQUIRE(b.address);
    return b;
}

static void destroy_buffer(VkDevice device, struct buffer b)
{
    vkDestroyBuffer(device, b.buffer, NULL);
    vkFreeMemory(device, b.memory, NULL);
}

int main(int argc, char **argv)
{
    REQUIRE(argc == 2 && (!strcmp(argv[1], "--venus") || !strcmp(argv[1], "--host")));
    const int venus = !strcmp(argv[1], "--venus");
    VkApplicationInfo app = {.sType = VK_STRUCTURE_TYPE_APPLICATION_INFO,
        .pApplicationName = "Helios native DGC contract probe", .apiVersion = VK_API_VERSION_1_3};
    VkInstanceCreateInfo instance_info = {.sType = VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO, .pApplicationInfo = &app};
    VkInstance instance;
    CHECK(vkCreateInstance(&instance_info, NULL, &instance));
    uint32_t count = 0;
    CHECK(vkEnumeratePhysicalDevices(instance, &count, NULL));
    REQUIRE(count && count <= 16);
    VkPhysicalDevice devices[16], physical = VK_NULL_HANDLE;
    CHECK(vkEnumeratePhysicalDevices(instance, &count, devices));
    for (uint32_t i = 0; i < count; i++) {
        VkPhysicalDeviceDriverProperties driver = {.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_DRIVER_PROPERTIES};
        VkPhysicalDeviceProperties2 properties = {.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_PROPERTIES_2, .pNext = &driver};
        vkGetPhysicalDeviceProperties2(devices[i], &properties);
        if ((driver.driverID == VK_DRIVER_ID_MESA_VENUS) == venus &&
                properties.properties.deviceType == VK_PHYSICAL_DEVICE_TYPE_DISCRETE_GPU) {
            physical = devices[i];
            printf("DEVICE %s; driver=%s %s\n", properties.properties.deviceName, driver.driverName, driver.driverInfo);
            break;
        }
    }
    REQUIRE(physical);
    VkPhysicalDeviceDeviceGeneratedCommandsFeaturesEXT dgc = {.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_DEVICE_GENERATED_COMMANDS_FEATURES_EXT};
    VkPhysicalDeviceMaintenance5FeaturesKHR m5 = {.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_MAINTENANCE_5_FEATURES_KHR, .pNext = &dgc};
    VkPhysicalDeviceVulkan12Features v12 = {.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VULKAN_1_2_FEATURES, .pNext = &m5};
    VkPhysicalDeviceFeatures2 features = {.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_FEATURES_2, .pNext = &v12};
    vkGetPhysicalDeviceFeatures2(physical, &features);
    REQUIRE(dgc.deviceGeneratedCommands && m5.maintenance5 && v12.bufferDeviceAddress && features.features.pipelineStatisticsQuery);
    v12 = (VkPhysicalDeviceVulkan12Features){.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VULKAN_1_2_FEATURES,
        .pNext = &m5, .bufferDeviceAddress = VK_TRUE};
    dgc.dynamicGeneratedPipelineLayout = VK_FALSE;
    vkGetPhysicalDeviceQueueFamilyProperties(physical, &count, NULL);
    REQUIRE(count && count <= 32);
    VkQueueFamilyProperties families[32];
    vkGetPhysicalDeviceQueueFamilyProperties(physical, &count, families);
    uint32_t family = 0;
    for (; family < count; family++) if (families[family].queueFlags & VK_QUEUE_COMPUTE_BIT) break;
    REQUIRE(family < count);
    float priority = 1.0f;
    VkDeviceQueueCreateInfo qi = {.sType = VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO,
        .queueFamilyIndex = family, .queueCount = 1, .pQueuePriorities = &priority};
    const char *extensions[] = {VK_EXT_DEVICE_GENERATED_COMMANDS_EXTENSION_NAME, VK_KHR_MAINTENANCE_5_EXTENSION_NAME};
    VkPhysicalDeviceFeatures enabled = {.pipelineStatisticsQuery = VK_TRUE};
    VkDeviceCreateInfo di = {.sType = VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO, .pNext = &v12,
        .queueCreateInfoCount = 1, .pQueueCreateInfos = &qi, .enabledExtensionCount = 2,
        .ppEnabledExtensionNames = extensions, .pEnabledFeatures = &enabled};
    VkDevice device;
    CHECK(vkCreateDevice(physical, &di, NULL, &device));
    LOAD(CreateIndirectExecutionSetEXT); LOAD(UpdateIndirectExecutionSetPipelineEXT);
    LOAD(DestroyIndirectExecutionSetEXT); LOAD(CreateIndirectCommandsLayoutEXT);
    LOAD(DestroyIndirectCommandsLayoutEXT); LOAD(GetGeneratedCommandsMemoryRequirementsEXT);
    LOAD(CmdPreprocessGeneratedCommandsEXT); LOAD(CmdExecuteGeneratedCommandsEXT);
    VkQueue queue;
    vkGetDeviceQueue(device, family, 0, &queue);
    VkPhysicalDeviceMemoryProperties memory;
    vkGetPhysicalDeviceMemoryProperties(physical, &memory);
    struct buffer output = make_buffer(device, &memory, 256,
        VK_BUFFER_USAGE_2_STORAGE_BUFFER_BIT | VK_BUFFER_USAGE_2_SHADER_DEVICE_ADDRESS_BIT, UINT32_MAX);
    struct buffer arguments = make_buffer(device, &memory, 40,
        VK_BUFFER_USAGE_2_INDIRECT_BUFFER_BIT | VK_BUFFER_USAGE_2_SHADER_DEVICE_ADDRESS_BIT, UINT32_MAX);
    void *mapped;
    CHECK(vkMapMemory(device, output.memory, 0, VK_WHOLE_SIZE, 0, &mapped));
    memset(mapped, 0, 256);
    vkUnmapMemory(device, output.memory);
    const uint32_t stream[] = {0, 0, 1, 1, 1, 1, 1, 1, 1, 1};
    CHECK(vkMapMemory(device, arguments.memory, 0, VK_WHOLE_SIZE, 0, &mapped));
    memcpy(mapped, stream, sizeof(stream));
    vkUnmapMemory(device, arguments.memory);
    VkDescriptorSetLayoutBinding binding = {.binding = 0, .descriptorType = VK_DESCRIPTOR_TYPE_STORAGE_BUFFER,
        .descriptorCount = 1, .stageFlags = VK_SHADER_STAGE_COMPUTE_BIT};
    VkDescriptorSetLayoutCreateInfo sl = {.sType = VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO,
        .bindingCount = 1, .pBindings = &binding};
    VkDescriptorSetLayout set_layout;
    CHECK(vkCreateDescriptorSetLayout(device, &sl, NULL, &set_layout));
    VkPushConstantRange range = {VK_SHADER_STAGE_COMPUTE_BIT, 0, 8};
    VkPipelineLayoutCreateInfo pli = {.sType = VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO,
        .setLayoutCount = 1, .pSetLayouts = &set_layout, .pushConstantRangeCount = 1, .pPushConstantRanges = &range};
    VkPipelineLayout pipeline_layout;
    CHECK(vkCreatePipelineLayout(device, &pli, NULL, &pipeline_layout));
    VkDescriptorPoolSize pool_size = {VK_DESCRIPTOR_TYPE_STORAGE_BUFFER, 1};
    VkDescriptorPoolCreateInfo dpi = {.sType = VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO,
        .maxSets = 1, .poolSizeCount = 1, .pPoolSizes = &pool_size};
    VkDescriptorPool pool;
    CHECK(vkCreateDescriptorPool(device, &dpi, NULL, &pool));
    VkDescriptorSetAllocateInfo sai = {.sType = VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO,
        .descriptorPool = pool, .descriptorSetCount = 1, .pSetLayouts = &set_layout};
    VkDescriptorSet set;
    CHECK(vkAllocateDescriptorSets(device, &sai, &set));
    VkDescriptorBufferInfo dbi = {output.buffer, 0, 256};
    VkWriteDescriptorSet write = {.sType = VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET, .dstSet = set,
        .descriptorCount = 1, .descriptorType = VK_DESCRIPTOR_TYPE_STORAGE_BUFFER, .pBufferInfo = &dbi};
    vkUpdateDescriptorSets(device, 1, &write, 0, NULL);
    VkShaderModuleCreateInfo smi = {.sType = VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO,
        .codeSize = sizeof(dgc_probe_cs), .pCode = dgc_probe_cs};
    VkShaderModule shader;
    CHECK(vkCreateShaderModule(device, &smi, NULL, &shader));
    VkPipeline pipelines[2];
    for (uint32_t i = 0; i < 2; i++) {
        uint32_t tag = i + 1;
        VkSpecializationMapEntry entry = {0, 0, 4};
        VkSpecializationInfo spec = {1, &entry, sizeof(tag), &tag};
        VkPipelineCreateFlags2CreateInfo flags2 = {.sType = VK_STRUCTURE_TYPE_PIPELINE_CREATE_FLAGS_2_CREATE_INFO,
            .flags = VK_PIPELINE_CREATE_2_INDIRECT_BINDABLE_BIT_EXT};
        VkComputePipelineCreateInfo ci = {.sType = VK_STRUCTURE_TYPE_COMPUTE_PIPELINE_CREATE_INFO, .pNext = &flags2,
            .stage = {.sType = VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO, .stage = VK_SHADER_STAGE_COMPUTE_BIT,
                .module = shader, .pName = "main", .pSpecializationInfo = &spec}, .layout = pipeline_layout};
        CHECK(vkCreateComputePipelines(device, VK_NULL_HANDLE, 1, &ci, NULL, &pipelines[i]));
    }
    VkIndirectExecutionSetPipelineInfoEXT epi = {.sType = VK_STRUCTURE_TYPE_INDIRECT_EXECUTION_SET_PIPELINE_INFO_EXT,
        .initialPipeline = pipelines[0], .maxPipelineCount = 2};
    VkIndirectExecutionSetCreateInfoEXT eci = {.sType = VK_STRUCTURE_TYPE_INDIRECT_EXECUTION_SET_CREATE_INFO_EXT,
        .type = VK_INDIRECT_EXECUTION_SET_INFO_TYPE_PIPELINES_EXT, .info.pPipelineInfo = &epi};
    VkIndirectExecutionSetEXT execution;
    CHECK(CreateIndirectExecutionSetEXT(device, &eci, NULL, &execution));
    VkWriteIndirectExecutionSetPipelineEXT ew = {.sType = VK_STRUCTURE_TYPE_WRITE_INDIRECT_EXECUTION_SET_PIPELINE_EXT,
        .index = 1, .pipeline = pipelines[1]};
    UpdateIndirectExecutionSetPipelineEXT(device, execution, 1, &ew);
    VkIndirectCommandsExecutionSetTokenEXT et = {.type = VK_INDIRECT_EXECUTION_SET_INFO_TYPE_PIPELINES_EXT,
        .shaderStages = VK_SHADER_STAGE_COMPUTE_BIT};
    VkIndirectCommandsPushConstantTokenEXT pt = {.updateRange = {VK_SHADER_STAGE_COMPUTE_BIT, 0, 4}};
    VkIndirectCommandsLayoutTokenEXT tokens[] = {
        {.sType = VK_STRUCTURE_TYPE_INDIRECT_COMMANDS_LAYOUT_TOKEN_EXT, .type = VK_INDIRECT_COMMANDS_TOKEN_TYPE_EXECUTION_SET_EXT, .data.pExecutionSet = &et},
        {.sType = VK_STRUCTURE_TYPE_INDIRECT_COMMANDS_LAYOUT_TOKEN_EXT, .type = VK_INDIRECT_COMMANDS_TOKEN_TYPE_PUSH_CONSTANT_EXT, .data.pPushConstant = &pt, .offset = 4},
        {.sType = VK_STRUCTURE_TYPE_INDIRECT_COMMANDS_LAYOUT_TOKEN_EXT, .type = VK_INDIRECT_COMMANDS_TOKEN_TYPE_DISPATCH_EXT, .offset = 8},
    };
    VkIndirectCommandsLayoutCreateInfoEXT lci = {.sType = VK_STRUCTURE_TYPE_INDIRECT_COMMANDS_LAYOUT_CREATE_INFO_EXT,
        .flags = VK_INDIRECT_COMMANDS_LAYOUT_USAGE_EXPLICIT_PREPROCESS_BIT_EXT, .shaderStages = VK_SHADER_STAGE_COMPUTE_BIT,
        .indirectStride = 20, .pipelineLayout = pipeline_layout, .tokenCount = 3, .pTokens = tokens};
    VkIndirectCommandsLayoutEXT layout;
    CHECK(CreateIndirectCommandsLayoutEXT(device, &lci, NULL, &layout));
    VkGeneratedCommandsMemoryRequirementsInfoEXT mri = {.sType = VK_STRUCTURE_TYPE_GENERATED_COMMANDS_MEMORY_REQUIREMENTS_INFO_EXT,
        .indirectExecutionSet = execution, .indirectCommandsLayout = layout, .maxSequenceCount = 2, .maxDrawCount = 1};
    VkMemoryRequirements2 req = {.sType = VK_STRUCTURE_TYPE_MEMORY_REQUIREMENTS_2};
    GetGeneratedCommandsMemoryRequirementsEXT(device, &mri, &req);
    struct buffer preprocess = make_buffer(device, &memory, req.memoryRequirements.size,
        VK_BUFFER_USAGE_2_PREPROCESS_BUFFER_BIT_EXT | VK_BUFFER_USAGE_2_SHADER_DEVICE_ADDRESS_BIT, req.memoryRequirements.memoryTypeBits);
    REQUIRE(!(preprocess.address % req.memoryRequirements.alignment));
    VkCommandPoolCreateInfo cpi = {.sType = VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO, .queueFamilyIndex = family};
    VkCommandPool command_pool;
    CHECK(vkCreateCommandPool(device, &cpi, NULL, &command_pool));
    VkCommandBufferAllocateInfo cai = {.sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO,
        .commandPool = command_pool, .level = VK_COMMAND_BUFFER_LEVEL_PRIMARY, .commandBufferCount = 2};
    VkCommandBuffer commands[2];
    CHECK(vkAllocateCommandBuffers(device, &cai, commands));
    VkCommandBufferBeginInfo begin = {.sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO};
    VkQueryPoolCreateInfo query_info = {.sType = VK_STRUCTURE_TYPE_QUERY_POOL_CREATE_INFO,
        .queryType = VK_QUERY_TYPE_PIPELINE_STATISTICS, .queryCount = 2,
        .pipelineStatistics = VK_QUERY_PIPELINE_STATISTIC_COMPUTE_SHADER_INVOCATIONS_BIT};
    VkQueryPool query;
    CHECK(vkCreateQueryPool(device, &query_info, NULL, &query));
    CHECK(vkBeginCommandBuffer(commands[1], &begin));
    vkCmdBindPipeline(commands[1], VK_PIPELINE_BIND_POINT_COMPUTE, pipelines[0]);
    vkCmdBindDescriptorSets(commands[1], VK_PIPELINE_BIND_POINT_COMPUTE, pipeline_layout, 0, 1, &set, 0, NULL);
    uint32_t constants[2] = {0, 7};
    vkCmdPushConstants(commands[1], pipeline_layout, VK_SHADER_STAGE_COMPUTE_BIT, 0, 8, constants);
    VkGeneratedCommandsInfoEXT generated = {.sType = VK_STRUCTURE_TYPE_GENERATED_COMMANDS_INFO_EXT,
        .shaderStages = VK_SHADER_STAGE_COMPUTE_BIT, .indirectExecutionSet = execution,
        .indirectCommandsLayout = layout, .indirectAddress = arguments.address, .indirectAddressSize = sizeof(stream),
        .preprocessAddress = preprocess.address, .preprocessSize = req.memoryRequirements.size, .maxSequenceCount = 2, .maxDrawCount = 1};
    CHECK(vkBeginCommandBuffer(commands[0], &begin));
    CmdPreprocessGeneratedCommandsEXT(commands[0], &generated, commands[1]);
    CHECK(vkEndCommandBuffer(commands[0]));
    VkMemoryBarrier barrier = {.sType = VK_STRUCTURE_TYPE_MEMORY_BARRIER,
        .srcAccessMask = VK_ACCESS_COMMAND_PREPROCESS_WRITE_BIT_EXT, .dstAccessMask = VK_ACCESS_INDIRECT_COMMAND_READ_BIT};
    vkCmdPipelineBarrier(commands[1], VK_PIPELINE_STAGE_COMMAND_PREPROCESS_BIT_EXT,
        VK_PIPELINE_STAGE_DRAW_INDIRECT_BIT, 0, 1, &barrier, 0, NULL, 0, NULL);
    vkCmdResetQueryPool(commands[1], query, 0, 2);
    vkCmdBeginQuery(commands[1], query, 0, 0);
    CmdExecuteGeneratedCommandsEXT(commands[1], VK_TRUE, &generated);
    vkCmdEndQuery(commands[1], query, 0);
    /* DGC invalidates bind-point state. Rebind for an ordinary-dispatch
     * control using the same pipeline and query type on another output word. */
    vkCmdBindPipeline(commands[1], VK_PIPELINE_BIND_POINT_COMPUTE, pipelines[1]);
    vkCmdBindDescriptorSets(commands[1], VK_PIPELINE_BIND_POINT_COMPUTE, pipeline_layout, 0, 1, &set, 0, NULL);
    constants[0] = 2;
    vkCmdPushConstants(commands[1], pipeline_layout, VK_SHADER_STAGE_COMPUTE_BIT, 0, 8, constants);
    vkCmdBeginQuery(commands[1], query, 1, 0);
    vkCmdDispatch(commands[1], 1, 1, 1);
    vkCmdEndQuery(commands[1], query, 1);
    barrier.srcAccessMask = VK_ACCESS_SHADER_WRITE_BIT;
    barrier.dstAccessMask = VK_ACCESS_HOST_READ_BIT;
    vkCmdPipelineBarrier(commands[1], VK_PIPELINE_STAGE_COMPUTE_SHADER_BIT, VK_PIPELINE_STAGE_HOST_BIT,
        0, 1, &barrier, 0, NULL, 0, NULL);
    CHECK(vkEndCommandBuffer(commands[1]));
    VkFenceCreateInfo fi = {.sType = VK_STRUCTURE_TYPE_FENCE_CREATE_INFO};
    VkFence fence;
    CHECK(vkCreateFence(device, &fi, NULL, &fence));
    VkSubmitInfo submit = {.sType = VK_STRUCTURE_TYPE_SUBMIT_INFO, .commandBufferCount = 2, .pCommandBuffers = commands};
    CHECK(vkQueueSubmit(queue, 1, &submit, fence));
    VkResult wait;
    int timed_out = 0;
    while ((wait = vkWaitForFences(device, 1, &fence, VK_TRUE, 5000000000ull)) == VK_TIMEOUT) {
        timed_out = 1;
        fprintf(stderr, "FAIL: timeout; retaining submitted resources until completion or device loss\n");
    }
    CHECK(wait);
    uint64_t invocations[2] = {0};
    CHECK(vkGetQueryPoolResults(device, query, 0, 2, sizeof(invocations), invocations,
        sizeof(invocations[0]), VK_QUERY_RESULT_64_BIT | VK_QUERY_RESULT_WAIT_BIT));
    printf("QUERY DGC CSInvocations=%"PRIu64" (expected 2), ordinary=%"PRIu64" (expected 1)\n", invocations[0], invocations[1]);
    CHECK(vkMapMemory(device, output.memory, 0, VK_WHOLE_SIZE, 0, &mapped));
    uint32_t *words = mapped;
    int pixels = words[0] == 107 && words[1] == 207 && words[2] == 207;
    printf("READBACK %u %u %u (expected 107 207 207)\n", words[0], words[1], words[2]);
    for (uint32_t i = 3; i < 64; i++) pixels &= words[i] == 0;
    printf("READBACK_AND_GUARDS %s\n", pixels ? "PASS" : "FAIL");
    int good = !timed_out && invocations[0] == 2 && invocations[1] == 1 && pixels;
    vkUnmapMemory(device, output.memory);
    vkDestroyFence(device, fence, NULL);
    vkDestroyCommandPool(device, command_pool, NULL);
    vkDestroyQueryPool(device, query, NULL);
    destroy_buffer(device, preprocess);
    DestroyIndirectCommandsLayoutEXT(device, layout, NULL);
    DestroyIndirectExecutionSetEXT(device, execution, NULL);
    for (unsigned i = 0; i < 2; i++) vkDestroyPipeline(device, pipelines[i], NULL);
    vkDestroyShaderModule(device, shader, NULL);
    vkDestroyDescriptorPool(device, pool, NULL);
    vkDestroyPipelineLayout(device, pipeline_layout, NULL);
    vkDestroyDescriptorSetLayout(device, set_layout, NULL);
    destroy_buffer(device, arguments);
    destroy_buffer(device, output);
    vkDestroyDevice(device, NULL);
    vkDestroyInstance(instance, NULL);
    puts(good ? "PASS: execution-set create/update, pipeline selection, cross-command-buffer preprocess, queries, 64-word readback and teardown" : "FAIL: query counts, readback or timeout");
    return good ? 0 : 1;
}
