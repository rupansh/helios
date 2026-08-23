// C++ surface of the Helios UMD <-> DXVK bridge. Included by both the
// cxx-generated glue and dxvk_bridge.cpp.
//
// cxx's generated glue manages `std::unique_ptr<HeliosDxvkDevice>` and therefore
// needs HeliosDxvkDevice to be a COMPLETE type here. We keep the DXVK headers out
// of this (and the glue) via pimpl: HeliosDxvkDevice is a thin complete shell
// holding a unique_ptr to an opaque Impl that owns the DXVK Rc<> objects. The
// destructor is declared here and defined out-of-line in dxvk_bridge.cpp, where
// Impl is complete.
#pragma once

#include <cstdint>
#include <memory>

// Owns the DXVK Rc<DxvkInstance/Adapter/Device>; defined in dxvk_bridge.cpp.
struct HeliosDxvkDeviceImpl;

struct HeliosDxvkDevice {
  HeliosDxvkDevice() noexcept;
  ~HeliosDxvkDevice();
  HeliosDxvkDevice(const HeliosDxvkDevice&) = delete;
  HeliosDxvkDevice& operator=(const HeliosDxvkDevice&) = delete;

  std::unique_ptr<HeliosDxvkDeviceImpl> impl;

  // Raw ID3D11Device* / ID3D11DeviceContext* (as size_t) for the DDI forwarders.
  std::size_t d3d11_device_ptr() const;
  std::size_t d3d11_context_ptr() const;
  std::size_t prepare_associated_texture2d(std::size_t desc_ptr) const;
  std::uint64_t associated_texture2d_preflight_bytes(
      std::size_t preflight_ptr) const;
  void discard_associated_texture2d_preflight(
      std::size_t preflight_ptr) const;
  // Package-private resource creation edge. Each call copies one validated
  // HRA1 record into the exact DXVK resource allocation graph before Vulkan
  // memory is allocated; the descriptor and initial-data pointers are only
  // borrowed for this synchronous call.
  std::size_t create_associated_buffer(
      std::size_t desc_ptr, std::size_t initial_data_ptr,
      std::uint64_t package_generation, std::uint64_t device_generation,
      std::uint64_t outer_allocation_token,
      std::uint64_t outer_allocation_bytes, std::size_t cpu_mapping,
      std::uint32_t association_flags) const;
  std::size_t create_associated_texture1d(
      std::size_t desc_ptr, std::size_t initial_data_ptr,
      std::uint64_t package_generation, std::uint64_t device_generation,
      std::uint64_t outer_allocation_token,
      std::uint64_t outer_allocation_bytes, std::size_t cpu_mapping,
      std::uint32_t association_flags) const;
  std::size_t create_associated_texture2d(
      std::size_t desc_ptr, std::size_t initial_data_ptr,
      std::uint64_t package_generation, std::uint64_t device_generation,
      std::uint64_t outer_allocation_token,
      std::uint64_t outer_allocation_bytes, std::size_t cpu_mapping,
      std::uint32_t association_flags,
      std::size_t preflight_ptr) const;
  std::size_t create_associated_texture3d(
      std::size_t desc_ptr, std::size_t initial_data_ptr,
      std::uint64_t package_generation, std::uint64_t device_generation,
      std::uint64_t outer_allocation_token,
      std::uint64_t outer_allocation_bytes, std::size_t cpu_mapping,
      std::uint32_t association_flags) const;
  // Opt-in queue-feed attribution. The timestamp is zero when tracing is off,
  // so the ordinary callback path performs no clock read or atomic update.
  std::uint64_t feed_trace_timestamp_ns() const noexcept;
  void feed_trace_present_callback(std::uint64_t duration_ns) const noexcept;
  // BUILD_2 recycle handoff. The deferred-context address is borrowed; the
  // command-list address transfers its one owned IC hCL reference on true.
  // The bridge gives it only to that exact deferred context's bounded cache;
  // the list's origin check rejects a cross-DC handoff, and false means the
  // caller still owns and releases the command-list reference.
  bool recycle_deferred_command_list(
      std::size_t deferred_context_ptr,
      std::size_t command_list_ptr) const noexcept;
  // Marks a freshly-created, UMD-private deferred context as eligible for
  // DXVK's DDI-only logical Finish(FALSE) reset. The borrowed pointer must
  // originate from this device's CreateDeferredContext call.
  bool enable_deferred_context_ddi_logical_reset(
      std::size_t deferred_context_ptr) const noexcept;
  // Shader creation wrappers. DXVK may throw dxvk::DxvkError while compiling
  // shader modules; these methods catch it and return 0 so exceptions never
  // cross the D3D UMD ABI.
  std::size_t create_vertex_shader(const std::uint8_t* code, std::size_t len) const;
  std::size_t create_pixel_shader(const std::uint8_t* code, std::size_t len) const;
  std::size_t create_geometry_shader(const std::uint8_t* code, std::size_t len) const;
  // Signature-carrying variant for the >=11.1 DDI, whose typed
  // D3D11_1DDIARG_SIGNATURE_ENTRY2 arrays supply the component types the raw
  // token stream lacks. `kind`: 0 = vertex, 1 = pixel, 2 = geometry.
  // `sig_words` = [n_in, n_out, then (sysval, register, mask, comptype,
  // stream) x n_in, then the same x n_out]; the container gets real
  // ISGN/OSGN chunks so dxbc-spv declares correctly-typed I/O.
  std::size_t create_shader_sig(
      std::uint32_t kind,
      const std::uint8_t* code,
      std::size_t len,
      const std::uint32_t* sig_words,
      std::size_t sig_words_len) const;
  // Signature-carrying tessellation shader create. `kind`: 0 = hull,
  // 1 = domain. `sig_words` = [n_in, n_out, n_patch, then entries for each
  // group as (sysval, register, mask, comptype, stream)]. D3D11 tessellation
  // DDI signatures do not carry component type/stream, so the Rust side passes
  // zeros there and the DXBC wrapper uses the same fallback rules as the 11.1
  // shader path.
  std::size_t create_tess_shader_sig(
      std::uint32_t kind,
      const std::uint8_t* code,
      std::size_t len,
      const std::uint32_t* sig_words,
      std::size_t sig_words_len) const;

  // Flip-model identity rotation (DXGI pfnRotateResourceIdentities): each
  // texture takes the DXVK storage (memory + VkImage + KMT handles) of the
  // NEXT one in the list, the last takes the first's. The swap executes on
  // the CS thread (ordered), no device drain.
  bool rotate_resource_backings(
      const std::size_t* d3d11_resource_ptrs,
      std::size_t count) const;

  // WDDM 2.1 ReleaseResource ordering. Flush the immediate context and wait
  // only until its work has reached the real vkQueueSubmit edge. The outer
  // submit hook closes the corresponding A5 scope before this returns; this is
  // not a GPU-completion wait and has no timeout, polling, or private timeline.
  bool flush_submitted() const;

  std::size_t create_hull_shader(const std::uint8_t* code, std::size_t len) const;
  std::size_t create_domain_shader(const std::uint8_t* code, std::size_t len) const;
  std::size_t create_compute_shader(const std::uint8_t* code, std::size_t len) const;
};

// Create a DXVK instance + logical device on the Helios venus adapter.
// Returns nullptr on failure. Matches the cxx bridge signature in src/bridge.rs.
std::unique_ptr<HeliosDxvkDevice> helios_dxvk_create_device(
    std::size_t   vk_instance,
    std::size_t   get_instance_proc_addr,
    std::size_t   icd_module_base,
    std::uint32_t luid_low,
    std::int32_t  luid_high,
    std::size_t   outer_context,
    std::size_t   outer_admit,
    std::size_t   outer_begin,
    std::size_t   outer_finish,
    std::size_t   outer_join,
    std::size_t   outer_allocate,
    std::size_t   outer_teardown_begin,
    std::size_t   outer_retire);
