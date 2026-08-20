#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#ifndef NOMINMAX
#define NOMINMAX
#endif
#include <windows.h>

#include <d3d12.h>

#include "vkd3d_bridge.h"
#include "helios_resource_association.h"
#include "bridge_common.h"
#include "bridge_guard.h"

#include <atomic>
#include <cstdio>
#include <cstdlib>
#include <mutex>
#include <share.h>

// These are ordinary symbols in the statically linked package engine. The
// selected Helios path performs no Vulkan loader or loaded-module search.
extern "C" HRESULT helios_vkd3d_create_device(
    void* vk_instance, void* get_instance_proc_addr, void* icd_module_base,
    LUID adapter_luid, void* outer_context,
    HRESULT (*outer_allocation_create)(void*, std::uint64_t, std::uint32_t,
                                       std::uint32_t, HeliosResourceAssociationV1*),
    HRESULT (*outer_allocation_teardown_begin)(void*, std::uint64_t,
                                               std::uint64_t),
    HRESULT (*outer_allocation_begin)(void*, std::uint64_t, std::uint64_t, void**),
    HRESULT (*outer_allocation_finish)(void*, void*, std::int32_t),
    HRESULT (*outer_allocation_retire)(void*, std::uint64_t, std::uint64_t,
                                       std::int32_t),
    REFIID iid, void** device);
extern "C" HRESULT helios_vkd3d_serialize_root_signature(
    const D3D12_ROOT_SIGNATURE_DESC* desc,
    D3D_ROOT_SIGNATURE_VERSION version, ID3DBlob** blob,
    ID3DBlob** error_blob);
extern "C" HRESULT helios_vkd3d_create_heap_associated(
    void* device, const D3D12_HEAP_DESC* desc,
    const void* allocation_pnext, std::uint64_t device_generation,
    std::uint64_t outer_allocation_token, void** heap);
extern "C" HRESULT helios_vkd3d_create_command_queue_associated(
    void* device, const D3D12_COMMAND_QUEUE_DESC* desc,
    void* callback_context, std::uintptr_t begin_address,
    std::uintptr_t finish_address, std::uintptr_t join_address, void** queue,
    std::uint32_t* out_vk_family_index, std::uint32_t* out_vk_queue_index);

namespace helios_bridge {

std::atomic<std::uint32_t> g_vkd3dCreateDeviceFailed{0};
std::atomic<std::uint32_t> g_vkd3dCreateDeviceNullOut{0};
std::atomic<std::uint32_t> g_vkd3dSerializeBadArg{0};
std::atomic<std::uint32_t> g_vkd3dAssociatedHeapRefused{0};
std::atomic<std::uint32_t> g_vkd3dAssociatedQueueRefused{0};

static const char* umd_log_file() {
  struct LogPath { char buf[MAX_PATH]; };
  static const LogPath path = [] {
    LogPath p{};
    CreateDirectoryA("C:\\ProgramData\\Helios", nullptr);
    _snprintf_s(p.buf, sizeof(p.buf), _TRUNCATE,
                "C:\\ProgramData\\Helios\\umd12-%lu.log",
                static_cast<unsigned long>(GetCurrentProcessId()));
    return p;
  }();
  return path.buf;
}

void umd_log(const char* msg) {
  FILE* f = _fsopen(umd_log_file(), "a", _SH_DENYNO);
  if (f) {
    std::fprintf(f, "[vkd3d-bridge] %s\n", msg);
    std::fclose(f);
  }
}

}  // namespace helios_bridge

using helios_bridge::umd_log;

struct HeliosVkd3dDeviceImpl {
  ID3D12Device* d3d12 = nullptr;

  ~HeliosVkd3dDeviceImpl() {
    if (d3d12) {
      d3d12->Release();
      d3d12 = nullptr;
    }
  }
};

HeliosVkd3dDevice::HeliosVkd3dDevice() noexcept = default;
HeliosVkd3dDevice::~HeliosVkd3dDevice() = default;

std::size_t HeliosVkd3dDevice::d3d12_device_ptr() const noexcept {
  return impl ? reinterpret_cast<std::size_t>(impl->d3d12) : 0;
}

std::size_t HeliosVkd3dDevice::create_associated_heap(
    std::size_t desc_ptr, std::uint64_t package_generation,
    std::uint64_t device_generation, std::uint64_t outer_allocation_token,
    std::uint64_t outer_allocation_bytes, std::size_t cpu_mapping,
    std::uint32_t association_flags) const noexcept {
  return helios_bridge::bridge_guard(
      "create_associated_heap", std::size_t{0}, [&]() -> std::size_t {
        const auto* desc = reinterpret_cast<const D3D12_HEAP_DESC*>(desc_ptr);
        const bool cpu_visible = desc &&
            (desc->Properties.CPUPageProperty == D3D12_CPU_PAGE_PROPERTY_WRITE_COMBINE ||
             desc->Properties.CPUPageProperty == D3D12_CPU_PAGE_PROPERTY_WRITE_BACK);
        const bool has_cpu_mapping = !!(association_flags &
            HELIOS_RESOURCE_ASSOCIATION_FLAG_CPU_MAPPING);
        if (!impl || !impl->d3d12 || !desc ||
            package_generation != HELIOS_PACKAGE_GENERATION ||
            !device_generation || !outer_allocation_token ||
            !outer_allocation_bytes || desc->SizeInBytes != outer_allocation_bytes ||
            (association_flags & ~HELIOS_RESOURCE_ASSOCIATION_FLAG_MASK) ||
            (!!cpu_mapping != has_cpu_mapping) ||
            (cpu_visible != has_cpu_mapping) ||
            (has_cpu_mapping && ((cpu_mapping & 4095u) ||
                outer_allocation_bytes > UINTPTR_MAX - cpu_mapping))) {
          helios_bridge::g_vkd3dAssociatedHeapRefused.fetch_add(
              1, std::memory_order_relaxed);
          return 0;
        }

        HeliosResourceAssociationV1 association{};
        association.s_type = HELIOS_RESOURCE_ASSOCIATION_STRUCTURE_TYPE;
        association.struct_bytes = HELIOS_RESOURCE_ASSOCIATION_BYTES;
        association.p_next = nullptr;
        association.abi_version = HELIOS_RESOURCE_ASSOCIATION_ABI_VERSION;
        association.package_generation = package_generation;
        association.device_generation = device_generation;
        association.outer_allocation_token = outer_allocation_token;
        association.outer_allocation_bytes = outer_allocation_bytes;
        association.cpu_mapping = reinterpret_cast<void*>(cpu_mapping);
        association.association_flags = association_flags;
        association.reserved1 = 0;

        void* heap = nullptr;
        const HRESULT hr = helios_vkd3d_create_heap_associated(
            impl->d3d12,
            desc,
            &association, device_generation, outer_allocation_token, &heap);
        if (FAILED(hr) || !heap) {
          helios_bridge::g_vkd3dAssociatedHeapRefused.fetch_add(
              1, std::memory_order_relaxed);
          return 0;
        }
        return reinterpret_cast<std::size_t>(heap);
      });
}

std::size_t HeliosVkd3dDevice::create_associated_queue(
    std::size_t desc_ptr, std::size_t callback_context,
    std::size_t begin_address, std::size_t finish_address,
    std::size_t join_address, std::uint32_t* out_vk_family_index,
    std::uint32_t* out_vk_queue_index) const noexcept {
  return helios_bridge::bridge_guard(
      "create_associated_queue", std::size_t{0}, [&]() -> std::size_t {
        if (out_vk_family_index) *out_vk_family_index = UINT32_MAX;
        if (out_vk_queue_index) *out_vk_queue_index = UINT32_MAX;
        if (!impl || !impl->d3d12 || !desc_ptr || !callback_context ||
            !begin_address || !finish_address || !join_address ||
            !out_vk_family_index || !out_vk_queue_index) {
          helios_bridge::g_vkd3dAssociatedQueueRefused.fetch_add(
              1, std::memory_order_relaxed);
          return 0;
        }

        ID3D12CommandQueue* queue = nullptr;
        const HRESULT hr = helios_vkd3d_create_command_queue_associated(
            impl->d3d12,
            reinterpret_cast<const D3D12_COMMAND_QUEUE_DESC*>(desc_ptr),
            reinterpret_cast<void*>(callback_context), begin_address,
            finish_address, join_address, reinterpret_cast<void**>(&queue),
            out_vk_family_index, out_vk_queue_index);
        if (FAILED(hr) || !queue) {
          helios_bridge::g_vkd3dAssociatedQueueRefused.fetch_add(
              1, std::memory_order_relaxed);
          if (queue) queue->Release();
          return 0;
        }
        return reinterpret_cast<std::size_t>(queue);
      });
}

std::unique_ptr<HeliosVkd3dDevice> helios_vkd3d_bridge_create_device(
    std::size_t vk_instance, std::size_t get_instance_proc_addr,
    std::size_t icd_module_base, std::uint32_t luid_low,
    std::int32_t luid_high, std::size_t outer_context,
    std::size_t outer_allocation_create,
    std::size_t outer_allocation_teardown_begin,
    std::size_t outer_allocation_begin,
    std::size_t outer_allocation_finish,
    std::size_t outer_allocation_retire) {
  static std::once_flag env_once;
  std::call_once(env_once, [] {
    if (!std::getenv("VKD3D_LOG_FILE")) {
      char path[MAX_PATH] = {};
      CreateDirectoryA("C:\\ProgramData\\Helios", nullptr);
      _snprintf_s(path, sizeof(path), _TRUNCATE,
                  "C:\\ProgramData\\Helios\\umd12-%lu-vkd3d.log",
                  static_cast<unsigned long>(GetCurrentProcessId()));
      _putenv_s("VKD3D_LOG_FILE", path);
    }
  });

  return helios_bridge::bridge_guard(
      "helios_vkd3d_bridge_create_device",
      std::unique_ptr<HeliosVkd3dDevice>{},
      [&]() -> std::unique_ptr<HeliosVkd3dDevice> {
        if (!vk_instance || !get_instance_proc_addr || !icd_module_base ||
            !outer_context || !outer_allocation_create ||
            !outer_allocation_teardown_begin || !outer_allocation_begin ||
            !outer_allocation_finish || !outer_allocation_retire ||
            (!luid_low && !luid_high)) {
          helios_bridge::g_vkd3dCreateDeviceFailed.fetch_add(
              1, std::memory_order_relaxed);
          return {};
        }

        LUID luid{luid_low, luid_high};
        ID3D12Device* device = nullptr;
        const HRESULT hr = helios_vkd3d_create_device(
            reinterpret_cast<void*>(vk_instance),
            reinterpret_cast<void*>(get_instance_proc_addr),
            reinterpret_cast<void*>(icd_module_base), luid,
            reinterpret_cast<void*>(outer_context),
            reinterpret_cast<HRESULT (*)(void*, std::uint64_t, std::uint32_t,
                                          std::uint32_t,
                                          HeliosResourceAssociationV1*)>(
                outer_allocation_create),
            reinterpret_cast<HRESULT (*)(void*, std::uint64_t, std::uint64_t)>(
                outer_allocation_teardown_begin),
            reinterpret_cast<HRESULT (*)(void*, std::uint64_t, std::uint64_t, void**)>(
                outer_allocation_begin),
            reinterpret_cast<HRESULT (*)(void*, void*, std::int32_t)>(
                outer_allocation_finish),
            reinterpret_cast<HRESULT (*)(void*, std::uint64_t, std::uint64_t, std::int32_t)>(
                outer_allocation_retire),
            __uuidof(ID3D12Device), reinterpret_cast<void**>(&device));
        if (FAILED(hr)) {
          helios_bridge::g_vkd3dCreateDeviceFailed.fetch_add(
              1, std::memory_order_relaxed);
          return {};
        }
        if (!device) {
          helios_bridge::g_vkd3dCreateDeviceNullOut.fetch_add(
              1, std::memory_order_relaxed);
          return {};
        }

        auto out = std::make_unique<HeliosVkd3dDevice>();
        out->impl = std::make_unique<HeliosVkd3dDeviceImpl>();
        out->impl->d3d12 = device;
        umd_log("ID3D12Device created from exact Mesa A5 instance");
        return out;
      });
}

std::int32_t helios_vkd3d_bridge_serialize_root_signature(
    std::size_t desc, std::uint32_t version,
    std::size_t* blob_out, std::size_t* err_out) noexcept {
  return helios_bridge::bridge_guard(
      "helios_vkd3d_bridge_serialize_root_signature",
      static_cast<std::int32_t>(0x80004005u), [&]() -> std::int32_t {
        if (!desc || !blob_out) {
          helios_bridge::g_vkd3dSerializeBadArg.fetch_add(
              1, std::memory_order_relaxed);
          return static_cast<std::int32_t>(0x80070057u);
        }
        *blob_out = 0;
        if (err_out) *err_out = 0;
        ID3DBlob* blob = nullptr;
        ID3DBlob* error = nullptr;
        const HRESULT hr = helios_vkd3d_serialize_root_signature(
            reinterpret_cast<const D3D12_ROOT_SIGNATURE_DESC*>(desc),
            static_cast<D3D_ROOT_SIGNATURE_VERSION>(version), &blob, &error);
        *blob_out = reinterpret_cast<std::size_t>(blob);
        if (err_out) {
          *err_out = reinterpret_cast<std::size_t>(error);
        } else if (error) {
          error->Release();
        }
        return static_cast<std::int32_t>(hr);
      });
}
