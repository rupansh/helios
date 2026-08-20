#pragma once

#include <cstdint>
#include <memory>

// The vkd3d and Vulkan headers intentionally do not cross this seam. The
// implementation owns the one ID3D12Device reference created from Mesa A5's
// exact instance and direct GIPA.
struct HeliosVkd3dDeviceImpl;

struct HeliosVkd3dDevice {
  HeliosVkd3dDevice() noexcept;
  ~HeliosVkd3dDevice();
  HeliosVkd3dDevice(const HeliosVkd3dDevice&) = delete;
  HeliosVkd3dDevice& operator=(const HeliosVkd3dDevice&) = delete;

  std::unique_ptr<HeliosVkd3dDeviceImpl> impl;

  // BORROWED. The bridge retains the owning COM reference.
  std::size_t d3d12_device_ptr() const noexcept;

  // Package-private immutable creation edges. Returned COM references are
  // owned by the caller.
  std::size_t create_associated_heap(
      std::size_t desc_ptr, std::uint64_t package_generation,
      std::uint64_t device_generation, std::uint64_t outer_allocation_token,
      std::uint64_t outer_allocation_bytes, std::size_t cpu_mapping,
      std::uint32_t association_flags) const noexcept;
  std::size_t create_associated_queue(
      std::size_t desc_ptr, std::size_t callback_context,
      std::size_t begin_address, std::size_t finish_address,
      std::size_t join_address, std::uint32_t* out_vk_family_index,
      std::uint32_t* out_vk_queue_index) const noexcept;
};

std::unique_ptr<HeliosVkd3dDevice> helios_vkd3d_bridge_create_device(
    std::size_t vk_instance, std::size_t get_instance_proc_addr,
    std::size_t icd_module_base, std::uint32_t luid_low,
    std::int32_t luid_high, std::size_t outer_context,
    std::size_t outer_allocation_create,
    std::size_t outer_allocation_teardown_begin,
    std::size_t outer_allocation_begin,
    std::size_t outer_allocation_finish,
    std::size_t outer_allocation_retire);

std::int32_t helios_vkd3d_bridge_serialize_root_signature(
    std::size_t desc, std::uint32_t version,
    std::size_t* blob_out, std::size_t* err_out) noexcept;
