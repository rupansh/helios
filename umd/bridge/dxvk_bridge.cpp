// Helios UMD <-> DXVK engine bridge implementation.
//
// Wraps DXVK's DxvkInstance/DxvkAdapter/DxvkDevice behind the opaque
// HeliosDxvkDevice. The DXVK engine references a frontend-provided
// `Logger::s_instance` global (normally defined in src/d3d11/d3d11_main.cpp,
// which we do not build) — we provide it here.

#define WIN32_LEAN_AND_MEAN
#define NOMINMAX
#include <windows.h>
#include <sddl.h>

#include <cstddef>
#include <cstdio>
#include <cstdlib>
#include <exception>
#include <share.h>

#include "dxvk_bridge.h"
#include "helios_resource_association.h"

// ⚠ These two resolve to `umd_common/bridge/`, not to this directory —
// `build.rs` adds it to the include path (`DECISIONS.md` D3b: one copy of the
// source, shared with the D3D12 bridge). `bridge_guard.h` is NOT included here:
// it needs `dxvk::DxvkError` for its engine arm, so it comes after the DXVK
// headers below.
#include "bridge_common.h"
#include "bridge_util.h"

#include "bridge_dxbc.h"

#include <algorithm>
#include <array>
#include <atomic>
#include <chrono>
#include <cstring>
#include <memory>
#include <mutex>
#include <optional>
#include <thread>
#include <type_traits>
#include <d3d11.h>
#include <dxgi.h>
#include <fstream>
#include <sstream>
#include <string>
#include <vector>

#include "dxvk_instance.h"
#include "dxvk_adapter.h"
#include "dxvk_device.h"
#include "../src/util/util_error.h"
#include "dxbc/dxbc_container.h"

// DXVK's full D3D11 COM implementation (built as libhelios_d3d11_static.a). We
// instantiate D3D11DXGIDevice from our DxvkDevice and forward the d3d10umddi DDI
// to ID3D11Device / ID3D11DeviceContext.
#include "d3d11_device.h"
#include "d3d11_context_def.h"
#include "d3d11_texture.h"
#include "d3d11_context_imm.h"
#include "dxvk_helios_feed_trace.h"

// ── the shared bridge_guard, with this bridge's one engine-specific arm ──────
//
// `dxvk::DxvkError` is not a `std::exception`, so without this arm every DXVK
// failure would log "unknown exception". The macro is the header's documented
// customization point and expands to the *identical* catch clause the guard
// carried before it moved (S1 is a move; behaviour change here is a defect).
// It must be defined AFTER `util_error.h` above, because it is expanded when
// `bridge_guard.h` is preprocessed.
//
// ⛔ Must not allocate — a `std::string` built inside a `std::bad_alloc`
// handler can throw again. That is also why `DxvkError::message()` (which
// returns `std::string`) is not called here.
#define HELIOS_BRIDGE_ENGINE_CATCH(what)                          \
  catch (const dxvk::DxvkError&) {                                \
    char msg[160];                                                \
    std::snprintf(msg, sizeof(msg), "%s: DxvkError", (what));     \
    ::helios_bridge::umd_log(msg);                                \
  }
#include "bridge_guard.h"

namespace dxbc_spv::dxbc {
  util::md5::Digest hashDxbcBinary(const void* data, size_t size);
}

namespace dxvk {
  // Frontend-provided global the DXVK engine links against. The string is the
  // log file name DXVK writes engine diagnostics to.
  Logger Logger::s_instance("helios_umd_dxvk.log");
}

namespace helios_bridge {
  // The rotate-sample instrument reads rows as std::uint32_t, so it is only
  // valid against a 32-bit-per-pixel format.
  bool is_32bpp_dxgi_format(DXGI_FORMAT format) {
    switch (format) {
    case DXGI_FORMAT_R8G8B8A8_TYPELESS:
    case DXGI_FORMAT_R8G8B8A8_UNORM:
    case DXGI_FORMAT_R8G8B8A8_UNORM_SRGB:
    case DXGI_FORMAT_R8G8B8A8_UINT:
    case DXGI_FORMAT_R8G8B8A8_SNORM:
    case DXGI_FORMAT_R8G8B8A8_SINT:
    case DXGI_FORMAT_B8G8R8A8_TYPELESS:
    case DXGI_FORMAT_B8G8R8A8_UNORM:
    case DXGI_FORMAT_B8G8R8A8_UNORM_SRGB:
    case DXGI_FORMAT_B8G8R8X8_TYPELESS:
    case DXGI_FORMAT_B8G8R8X8_UNORM:
    case DXGI_FORMAT_B8G8R8X8_UNORM_SRGB:
    case DXGI_FORMAT_R10G10B10A2_TYPELESS:
    case DXGI_FORMAT_R10G10B10A2_UNORM:
    case DXGI_FORMAT_R10G10B10A2_UINT:
    case DXGI_FORMAT_R11G11B10_FLOAT:
    case DXGI_FORMAT_R16G16_TYPELESS:
    case DXGI_FORMAT_R16G16_FLOAT:
    case DXGI_FORMAT_R16G16_UNORM:
    case DXGI_FORMAT_R16G16_UINT:
    case DXGI_FORMAT_R16G16_SNORM:
    case DXGI_FORMAT_R16G16_SINT:
    case DXGI_FORMAT_R32_TYPELESS:
    case DXGI_FORMAT_R32_FLOAT:
    case DXGI_FORMAT_R32_UINT:
    case DXGI_FORMAT_R32_SINT:
      return true;
    default:
      return false;
    }
  }

  // ⚠ `bridge_log_budget`, `PeriodicStat`, `qpc_elapsed_us` and `ComRelease<T>`
  // moved to `umd_common/bridge/{bridge_common,bridge_util}.h` at stage S1
  // (`DECISIONS.md` D3b) — same namespace, same signatures, so every use site
  // below is unchanged. They are engine-agnostic and the D3D12 bridge gets them
  // from the same file rather than a copy.

  // Minimal IDXGIAdapter the D3D11DXGIDevice constructor stores (it is not
  // queried during construction — the Dxvk objects are passed directly).
  class HeliosStubAdapter : public IDXGIAdapter {
    std::atomic<ULONG> m_ref{1};
  public:
    HRESULT STDMETHODCALLTYPE QueryInterface(REFIID riid, void** ppv) override {
      if (!ppv) return E_POINTER;
      if (riid == __uuidof(IUnknown) || riid == __uuidof(IDXGIObject) ||
          riid == __uuidof(IDXGIAdapter)) {
        *ppv = static_cast<IDXGIAdapter*>(this);
        AddRef();
        return S_OK;
      }
      *ppv = nullptr;
      return E_NOINTERFACE;
    }
    ULONG STDMETHODCALLTYPE AddRef() override { return ++m_ref; }
    ULONG STDMETHODCALLTYPE Release() override {
      ULONG r = --m_ref;
      if (!r) delete this;
      return r;
    }
    HRESULT STDMETHODCALLTYPE SetPrivateData(REFGUID, UINT, const void*) override { return E_NOTIMPL; }
    HRESULT STDMETHODCALLTYPE SetPrivateDataInterface(REFGUID, const IUnknown*) override { return E_NOTIMPL; }
    HRESULT STDMETHODCALLTYPE GetPrivateData(REFGUID, UINT*, void*) override { return E_NOTIMPL; }
    HRESULT STDMETHODCALLTYPE GetParent(REFIID, void** pp) override { if (pp) *pp = nullptr; return E_NOINTERFACE; }
    HRESULT STDMETHODCALLTYPE EnumOutputs(UINT, IDXGIOutput**) override { return DXGI_ERROR_NOT_FOUND; }
    HRESULT STDMETHODCALLTYPE GetDesc(DXGI_ADAPTER_DESC* d) override { if (d) std::memset(d, 0, sizeof(*d)); return S_OK; }
    HRESULT STDMETHODCALLTYPE CheckInterfaceSupport(REFGUID, LARGE_INTEGER*) override { return DXGI_ERROR_UNSUPPORTED; }
  };
}

namespace helios_bridge {
  // Per-process log path under C:\ProgramData\Helios (see umd_log_path() in
  // lib.rs). The restricted IddCx host process cannot write C:\Windows\Temp, so
  // its DXVK-bridge log lines vanished; ProgramData is standard-user writable and
  // the per-pid name keeps each process's file owned by that process.
  // Magic static, matching `shader_bytecode_dump_path()` in this same anonymous
  // namespace — the lazy `if (path[0] == 0)` form it used was an
  // unsynchronised write to shared storage, and the file already contradicted
  // itself on this point.
  const char* umd_log_file() {
    static const std::string path = [] {
      CreateDirectoryA("C:\\ProgramData\\Helios", nullptr);
      char buf[MAX_PATH] = {};
      _snprintf_s(buf, sizeof(buf), _TRUNCATE,
                  "C:\\ProgramData\\Helios\\umd-%lu.log",
                  (unsigned long)GetCurrentProcessId());
      return std::string(buf);
    }();
    return path.c_str();
  }

  void umd_log(const char* msg) {
    // _fsopen with _SH_DENYNO, NOT fopen_s: fopen_s opens in _SH_SECURE
    // (deny-sharing) mode, and the Rust side holds a persistent handle to the
    // same umd-<pid>.log since e88f2c6 — fopen_s then fails on EVERY call and
    // all bridge logging (incl. rotate-perf telemetry) silently vanishes
    // (found 18th session: DriverStore UMD had the strings, logs had no
    // [dxvk-bridge] lines).
    FILE* f = _fsopen(umd_log_file(), "a", _SH_DENYNO);
    if (f) {
      fprintf(f, "[dxvk-bridge] %s\n", msg);
      fclose(f);
    }
  }

  // ⚠ `bridge_guard` moved to `umd_common/bridge/bridge_guard.h` at stage S1
  // (`DECISIONS.md` D3b), together with the ~30 lines of comment recording why
  // it exists and why its compile-time assert is not optional (commit
  // `ead692e`, the truncation that crash-looped dwm and LogonUI at cold boot).
  // Read it there; the check that it is still the only one is
  // `grep -rnE '^[[:space:]]*static_assert\(' umd/bridge umd12/bridge
  // umd_common/bridge` -> exactly one hit, in that file. ⚠ The anchor is
  // load-bearing: without it this comment counts itself.
  //
  // The DXVK-specific arm stays here, as the header's one customization point.
  // `dxvk::DxvkError` is not a `std::exception`, so without this the generic
  // arms would not name it and every DXVK failure would log "unknown
  // exception". It expands to the identical catch clause the guard had before
  // the move -- S1 is a move, and a behaviour change here would be a defect.
  //
  // ⛔ Must not allocate: a `std::string` built inside a `std::bad_alloc`
  // handler can throw again. That is also why `DxvkError::message()` (returns
  // `std::string`) is not called.

}

using namespace helios_bridge;

struct HeliosDxvkDeviceImpl {
  dxvk::Rc<dxvk::DxvkInstance> instance;
  dxvk::Rc<dxvk::DxvkAdapter>  adapter;
  dxvk::Rc<dxvk::DxvkDevice>   device;
  ID3D11Device*        d3d11   = nullptr; // QI'd from D3D11DXGIDevice; holds it alive
  ID3D11DeviceContext* context = nullptr; // immediate context
  std::uint32_t venus_ctx_id = 0;

  ~HeliosDxvkDeviceImpl() {
    if (context) context->Release();
    if (d3d11) d3d11->Release();
  }
};

namespace {

  struct Texture2DPreflight {
    dxvk::D3D11Device* owner = nullptr;
    VkImage image = VK_NULL_HANDLE;
    VkMemoryRequirements requirements = { };

    ~Texture2DPreflight() {
      if (owner && image)
        owner->DiscardTexture2DHeliosPreflight(image);
    }
  };

  std::optional<HeliosResourceAssociationV1> make_resource_association(
      std::uint64_t package_generation,
      std::uint64_t device_generation,
      std::uint64_t outer_allocation_token,
      std::uint64_t outer_allocation_bytes,
      std::size_t cpu_mapping,
      std::uint32_t association_flags) {
    if (package_generation != HELIOS_PACKAGE_GENERATION
     || !device_generation
     || !outer_allocation_token
     || !outer_allocation_bytes
     || (association_flags & ~HELIOS_RESOURCE_ASSOCIATION_FLAG_MASK)
     || (!!cpu_mapping != !!(association_flags
          & HELIOS_RESOURCE_ASSOCIATION_FLAG_CPU_MAPPING)))
      return std::nullopt;

    HeliosResourceAssociationV1 association = { };
    association.s_type = HELIOS_RESOURCE_ASSOCIATION_STRUCTURE_TYPE;
    association.struct_bytes = HELIOS_RESOURCE_ASSOCIATION_BYTES;
    association.p_next = nullptr;
    association.abi_version = HELIOS_RESOURCE_ASSOCIATION_ABI_VERSION;
    association.reserved = 0;
    association.package_generation = package_generation;
    association.device_generation = device_generation;
    association.outer_allocation_token = outer_allocation_token;
    association.outer_allocation_bytes = outer_allocation_bytes;
    association.cpu_mapping = reinterpret_cast<void*>(cpu_mapping);
    association.association_flags = association_flags;
    association.reserved1 = 0;
    return association;
  }

  // The body every plain `create_*_shader` forwarder shares.
  //
  // Six bodies were identical apart from one COM interface type, one
  // `ID3D11Device` method and a two-letter dump tag. That is the shape where a
  // fix (a new dump, a changed refusal) lands in five of six and the sixth
  // behaves differently only under the workload that binds that stage.
  //
  // `Create` is a lambda rather than a pointer-to-member-function because the
  // six `ID3D11Device::Create*Shader` overloads have six different out-param
  // types; the lambda pins the pairing at each call site, where it is visible.
  template <typename Iface, typename Create>
  std::size_t create_shader_impl(const HeliosDxvkDeviceImpl* impl,
                                 const char* dump_tag,
                                 const char* name,
                                 const std::uint8_t* code,
                                 std::size_t len,
                                 Create create) {
    if (!impl || !impl->d3d11 || !code || !len)
      return 0;
    Iface* shader = nullptr;
    return bridge_guard(name, std::size_t(0), [&]() -> std::size_t {
      auto bytecode = prepare_shader_bytecode(code, len);
      if (!bytecode)
        return 0;
      dump_shader_bytecode(dump_tag, "raw", code, len);
      dump_shader_bytecode(dump_tag, "wrapped", bytecode.data(), bytecode.len());
      HRESULT hr = create(impl->d3d11, bytecode.data(), bytecode.len(), &shader);
      if (FAILED(hr)) {
        char msg[96];
        std::snprintf(msg, sizeof(msg), "%s returned failure", name);
        umd_log(msg);
        return 0;
      }
      return reinterpret_cast<std::size_t>(shader);
    });
  }

}

// Out-of-line ctor/dtor, defined where HeliosDxvkDeviceImpl is complete so the
// header (and the cxx glue) need no DXVK headers.
HeliosDxvkDevice::HeliosDxvkDevice() noexcept = default;
HeliosDxvkDevice::~HeliosDxvkDevice() = default;

std::size_t HeliosDxvkDevice::d3d11_device_ptr() const {
  return impl ? reinterpret_cast<std::size_t>(impl->d3d11) : 0;
}
std::size_t HeliosDxvkDevice::d3d11_context_ptr() const {
  return impl ? reinterpret_cast<std::size_t>(impl->context) : 0;
}

std::size_t HeliosDxvkDevice::prepare_associated_texture2d(
    std::size_t desc_ptr) const {
  return bridge_guard("prepare_associated_texture2d", std::size_t(0), [&]() {
    if (!impl || !impl->d3d11 || !desc_ptr)
      return std::size_t(0);
    auto preflight = std::make_unique<Texture2DPreflight>();
    preflight->owner = static_cast<dxvk::D3D11Device*>(impl->d3d11);
    HRESULT hr = preflight->owner->PrepareTexture2DHelios(
      reinterpret_cast<const D3D11_TEXTURE2D_DESC*>(desc_ptr),
      &preflight->requirements, &preflight->image);
    if (FAILED(hr) || !preflight->requirements.size || !preflight->image) {
      char msg[128];
      std::snprintf(msg, sizeof(msg),
        "texture2d preflight failed hr=0x%08lx",
        static_cast<unsigned long>(hr));
      umd_log(msg);
      return std::size_t(0);
    }
    return reinterpret_cast<std::size_t>(preflight.release());
  });
}

std::uint64_t HeliosDxvkDevice::associated_texture2d_preflight_bytes(
    std::size_t preflight_ptr) const {
  if (!impl || !impl->d3d11 || !preflight_ptr)
    return 0;
  const auto* preflight = reinterpret_cast<const Texture2DPreflight*>(preflight_ptr);
  return preflight->owner == static_cast<dxvk::D3D11Device*>(impl->d3d11)
    ? static_cast<std::uint64_t>(preflight->requirements.size)
    : 0;
}

void HeliosDxvkDevice::discard_associated_texture2d_preflight(
    std::size_t preflight_ptr) const {
  if (!preflight_ptr)
    return;
  auto preflight = std::unique_ptr<Texture2DPreflight>(
    reinterpret_cast<Texture2DPreflight*>(preflight_ptr));
  if (!impl || preflight->owner != static_cast<dxvk::D3D11Device*>(impl->d3d11))
    preflight.release();
}

std::size_t HeliosDxvkDevice::create_associated_buffer(
    std::size_t desc_ptr,
    std::size_t initial_data_ptr,
    std::uint64_t package_generation,
    std::uint64_t device_generation,
    std::uint64_t outer_allocation_token,
    std::uint64_t outer_allocation_bytes,
    std::size_t cpu_mapping,
    std::uint32_t association_flags) const {
  return bridge_guard("create_associated_buffer", std::size_t(0), [&]() {
    auto association = make_resource_association(package_generation,
      device_generation, outer_allocation_token, outer_allocation_bytes,
      cpu_mapping, association_flags);
    if (!impl || !impl->d3d11 || !desc_ptr || !association)
      return std::size_t(0);
    ID3D11Buffer* resource = nullptr;
    HRESULT hr = static_cast<dxvk::D3D11Device*>(impl->d3d11)->CreateBufferHelios(
      reinterpret_cast<const D3D11_BUFFER_DESC*>(desc_ptr),
      reinterpret_cast<const D3D11_SUBRESOURCE_DATA*>(initial_data_ptr),
      &*association, &resource);
    return SUCCEEDED(hr) ? reinterpret_cast<std::size_t>(resource) : 0;
  });
}

std::size_t HeliosDxvkDevice::create_associated_texture1d(
    std::size_t desc_ptr,
    std::size_t initial_data_ptr,
    std::uint64_t package_generation,
    std::uint64_t device_generation,
    std::uint64_t outer_allocation_token,
    std::uint64_t outer_allocation_bytes,
    std::size_t cpu_mapping,
    std::uint32_t association_flags) const {
  return bridge_guard("create_associated_texture1d", std::size_t(0), [&]() {
    auto association = make_resource_association(package_generation,
      device_generation, outer_allocation_token, outer_allocation_bytes,
      cpu_mapping, association_flags);
    if (!impl || !impl->d3d11 || !desc_ptr || !association)
      return std::size_t(0);
    dxvk::D3D11_HELIOS_CREATE_INFO create = { };
    create.ResourceAssociation = &*association;
    ID3D11Texture1D* resource = nullptr;
    HRESULT hr = static_cast<dxvk::D3D11Device*>(impl->d3d11)->CreateTexture1DHelios(
      reinterpret_cast<const D3D11_TEXTURE1D_DESC*>(desc_ptr),
      reinterpret_cast<const D3D11_SUBRESOURCE_DATA*>(initial_data_ptr),
      &create, &resource);
    return SUCCEEDED(hr) ? reinterpret_cast<std::size_t>(resource) : 0;
  });
}

std::size_t HeliosDxvkDevice::create_associated_texture2d(
    std::size_t desc_ptr,
    std::size_t initial_data_ptr,
    std::uint64_t package_generation,
    std::uint64_t device_generation,
    std::uint64_t outer_allocation_token,
    std::uint64_t outer_allocation_bytes,
    std::size_t cpu_mapping,
    std::uint32_t association_flags,
    std::size_t preflight_ptr) const {
  return bridge_guard("create_associated_texture2d", std::size_t(0), [&]() {
    auto preflight = std::unique_ptr<Texture2DPreflight>(
      reinterpret_cast<Texture2DPreflight*>(preflight_ptr));
    auto association = make_resource_association(package_generation,
      device_generation, outer_allocation_token, outer_allocation_bytes,
      cpu_mapping, association_flags);
    if (!impl || !impl->d3d11 || !desc_ptr || !association || !preflight
     || preflight->owner != static_cast<dxvk::D3D11Device*>(impl->d3d11)
     || !preflight->image || !preflight->requirements.size)
      return std::size_t(0);
    dxvk::D3D11_HELIOS_CREATE_INFO create = { };
    create.ResourceAssociation = &*association;
    create.PrecreatedImage = &preflight->image;
    ID3D11Texture2D* resource = nullptr;
    HRESULT hr = static_cast<dxvk::D3D11Device*>(impl->d3d11)->CreateTexture2DHelios(
      reinterpret_cast<const D3D11_TEXTURE2D_DESC*>(desc_ptr),
      reinterpret_cast<const D3D11_SUBRESOURCE_DATA*>(initial_data_ptr),
      &create, &resource);
    return SUCCEEDED(hr) ? reinterpret_cast<std::size_t>(resource) : 0;
  });
}

std::size_t HeliosDxvkDevice::create_associated_texture3d(
    std::size_t desc_ptr,
    std::size_t initial_data_ptr,
    std::uint64_t package_generation,
    std::uint64_t device_generation,
    std::uint64_t outer_allocation_token,
    std::uint64_t outer_allocation_bytes,
    std::size_t cpu_mapping,
    std::uint32_t association_flags) const {
  return bridge_guard("create_associated_texture3d", std::size_t(0), [&]() {
    auto association = make_resource_association(package_generation,
      device_generation, outer_allocation_token, outer_allocation_bytes,
      cpu_mapping, association_flags);
    if (!impl || !impl->d3d11 || !desc_ptr || !association)
      return std::size_t(0);
    dxvk::D3D11_HELIOS_CREATE_INFO create = { };
    create.ResourceAssociation = &*association;
    ID3D11Texture3D* resource = nullptr;
    HRESULT hr = static_cast<dxvk::D3D11Device*>(impl->d3d11)->CreateTexture3DHelios(
      reinterpret_cast<const D3D11_TEXTURE3D_DESC*>(desc_ptr),
      reinterpret_cast<const D3D11_SUBRESOURCE_DATA*>(initial_data_ptr),
      &create, &resource);
    return SUCCEEDED(hr) ? reinterpret_cast<std::size_t>(resource) : 0;
  });
}

std::uint64_t HeliosDxvkDevice::feed_trace_timestamp_ns() const noexcept {
  return dxvk::helios_feed::timestampNs();
}

void HeliosDxvkDevice::feed_trace_present_callback(
    std::uint64_t duration_ns) const noexcept {
  dxvk::helios_feed::umdPresentCallback(duration_ns);
}

bool HeliosDxvkDevice::recycle_deferred_command_list(
    std::size_t deferred_context_ptr,
    std::size_t command_list_ptr) const noexcept {
  return bridge_guard("recycle_deferred_command_list", false, [&]() -> bool {
    if (!impl || !impl->d3d11 || !deferred_context_ptr || !command_list_ptr)
      return false;

    // The UMD passes only its owned deferred COM context (created through this
    // bridge) and the owned command list its FinishCommandList just returned.
    // Do not probe them with GetType/GetDevice here: those methods AddRef and
    // Release the shared device on every handoff, defeating this hot-path
    // optimization. D3D11CommandList::IsReusableBy is the narrow contract
    // guard for a same-device but wrong-DC handoff.
    auto* context = reinterpret_cast<ID3D11DeviceContext*>(deferred_context_ptr);
    auto* commandList = reinterpret_cast<ID3D11CommandList*>(command_list_ptr);
    return static_cast<dxvk::D3D11DeferredContext*>(context)
      ->RecycleCommandList(static_cast<dxvk::D3D11CommandList*>(commandList));
  });
}

bool HeliosDxvkDevice::enable_deferred_context_ddi_logical_reset(
    std::size_t deferred_context_ptr) const noexcept {
  return bridge_guard("enable_deferred_context_ddi_logical_reset", false, [&]() -> bool {
    if (!impl || !impl->d3d11 || !deferred_context_ptr)
      return false;

    // This is called exactly once, immediately after this bridge created the
    // private deferred context. Do not QI or GetDevice on the hot DDI route.
    auto* context = reinterpret_cast<ID3D11DeviceContext*>(deferred_context_ptr);
    static_cast<dxvk::D3D11DeferredContext*>(context)
      ->EnableHeliosDdiLogicalReset();
    return true;
  });
}

std::size_t HeliosDxvkDevice::create_vertex_shader(const std::uint8_t* code, std::size_t len) const {
  return create_shader_impl<ID3D11VertexShader>(
      impl.get(), "vs", "CreateVertexShader", code, len,
      [](ID3D11Device* d, const void* bc, std::size_t n, ID3D11VertexShader** out) {
        return d->CreateVertexShader(bc, n, nullptr, out);
      });
}

std::size_t HeliosDxvkDevice::create_pixel_shader(const std::uint8_t* code, std::size_t len) const {
  return create_shader_impl<ID3D11PixelShader>(
      impl.get(), "ps", "CreatePixelShader", code, len,
      [](ID3D11Device* d, const void* bc, std::size_t n, ID3D11PixelShader** out) {
        return d->CreatePixelShader(bc, n, nullptr, out);
      });
}

std::size_t HeliosDxvkDevice::create_geometry_shader(const std::uint8_t* code, std::size_t len) const {
  return create_shader_impl<ID3D11GeometryShader>(
      impl.get(), "gs", "CreateGeometryShader", code, len,
      [](ID3D11Device* d, const void* bc, std::size_t n, ID3D11GeometryShader** out) {
        return d->CreateGeometryShader(bc, n, nullptr, out);
      });
}

std::size_t HeliosDxvkDevice::create_shader_sig(
    std::uint32_t kind,
    const std::uint8_t* code,
    std::size_t len,
    const std::uint32_t* sig_words,
    std::size_t sig_words_len) const {
  if (!impl || !impl->d3d11 || !code || !len || !sig_words || sig_words_len < 2)
    return 0;
  const std::uint32_t n_in = sig_words[0];
  const std::uint32_t n_out = sig_words[1];
  if (!signature_count_ok("create_shader_sig", n_in) ||
      !signature_count_ok("create_shader_sig", n_out))
    return 0;
  // Widen BEFORE adding: `n_in + n_out` was evaluated in std::uint32_t and only
  // then promoted, so a wrapped sum could satisfy the check the indexing below
  // depends on.
  if (sig_words_len != 2 + (std::size_t(n_in) + std::size_t(n_out)) * kSigEntryWords) {
    umd_log("create_shader_sig: signature word count mismatch");
    return 0;
  }
  const std::uint32_t* in_entries = sig_words + 2;
  const std::uint32_t* out_entries = in_entries + std::size_t(n_in) * kSigEntryWords;
  return bridge_guard("create_shader_sig", std::size_t(0), [&]() -> std::size_t {
      auto bytecode = prepare_shader_bytecode_with_sigs(
          code, len, in_entries, n_in, out_entries, n_out);
      if (!bytecode)
        return 0;
      const char* stage = kind == 0 ? "vs-sig" : kind == 1 ? "ps-sig" : "gs-sig";
      dump_shader_bytecode(stage, "raw", code, len);
      dump_shader_bytecode(stage, "wrapped", bytecode.data(), bytecode.len());
      HRESULT hr = E_FAIL;
      void* shader = nullptr;
      switch (kind) {
        case 0:
          hr = impl->d3d11->CreateVertexShader(bytecode.data(), bytecode.len(), nullptr,
                                               reinterpret_cast<ID3D11VertexShader**>(&shader));
          break;
        case 1:
          hr = impl->d3d11->CreatePixelShader(bytecode.data(), bytecode.len(), nullptr,
                                              reinterpret_cast<ID3D11PixelShader**>(&shader));
          break;
        case 2:
          hr = impl->d3d11->CreateGeometryShader(bytecode.data(), bytecode.len(), nullptr,
                                                 reinterpret_cast<ID3D11GeometryShader**>(&shader));
          break;
        default:
          umd_log("create_shader_sig: unknown shader kind");
          return 0;
      }
      if (FAILED(hr)) {
        umd_log("create_shader_sig: shader creation returned failure");
        return 0;
      }
      return reinterpret_cast<std::size_t>(shader);
  });
}

std::size_t HeliosDxvkDevice::create_tess_shader_sig(
    std::uint32_t kind,
    const std::uint8_t* code,
    std::size_t len,
    const std::uint32_t* sig_words,
    std::size_t sig_words_len) const {
  if (!impl || !impl->d3d11 || !code || !len || !sig_words || sig_words_len < 3)
    return 0;
  const std::uint32_t n_in = sig_words[0];
  const std::uint32_t n_out = sig_words[1];
  const std::uint32_t n_patch = sig_words[2];
  if (!signature_count_ok("create_tess_shader_sig", n_in) ||
      !signature_count_ok("create_tess_shader_sig", n_out) ||
      !signature_count_ok("create_tess_shader_sig", n_patch))
    return 0;
  if (sig_words_len !=
      3 + (std::size_t(n_in) + std::size_t(n_out) + std::size_t(n_patch)) * kSigEntryWords) {
    umd_log("create_tess_shader_sig: signature word count mismatch");
    return 0;
  }
  const std::uint32_t* in_entries = sig_words + 3;
  const std::uint32_t* out_entries = in_entries + std::size_t(n_in) * kSigEntryWords;
  const std::uint32_t* patch_entries = out_entries + std::size_t(n_out) * kSigEntryWords;
  return bridge_guard("create_tess_shader_sig", std::size_t(0), [&]() -> std::size_t {
      auto bytecode = prepare_shader_bytecode_with_tess_sigs(
          code, len, in_entries, n_in, out_entries, n_out, patch_entries, n_patch);
      if (!bytecode)
        return 0;
      const char* stage = kind == 0 ? "hs-sig" : "ds-sig";
      dump_shader_bytecode(stage, "raw", code, len);
      dump_shader_bytecode(stage, "wrapped", bytecode.data(), bytecode.len());
      HRESULT hr = E_FAIL;
      void* shader = nullptr;
      switch (kind) {
        case 0:
          hr = impl->d3d11->CreateHullShader(bytecode.data(), bytecode.len(), nullptr,
                                             reinterpret_cast<ID3D11HullShader**>(&shader));
          break;
        case 1:
          hr = impl->d3d11->CreateDomainShader(bytecode.data(), bytecode.len(), nullptr,
                                               reinterpret_cast<ID3D11DomainShader**>(&shader));
          break;
        default:
          umd_log("create_tess_shader_sig: unknown shader kind");
          return 0;
      }
      if (FAILED(hr)) {
        umd_log("create_tess_shader_sig: shader creation returned failure");
        return 0;
      }
      return reinterpret_cast<std::size_t>(shader);
  });
}

bool HeliosDxvkDevice::rotate_resource_backings(
    const std::size_t* d3d11_resource_ptrs,
    std::size_t count) const {
  if (!impl || !impl->d3d11 || !impl->context || !d3d11_resource_ptrs || count < 2)
    return false;
  return bridge_guard("rotate_resource_backings", false, [&]() -> bool {
      // Collect the DXVK images first; refuse the whole rotation if any entry
      // is not a storage-backed texture (a partial rotation would corrupt the
      // swapchain identity mapping). Rc refs: the swap executes later on the
      // CS thread and must not race resource destruction.
      std::vector<dxvk::Rc<dxvk::DxvkImage>> images;
      images.reserve(count);
      for (std::size_t i = 0; i < count; ++i) {
        auto* resource = reinterpret_cast<ID3D11Resource*>(d3d11_resource_ptrs[i]);
        auto* texture = resource ? dxvk::GetCommonTexture(resource) : nullptr;
        if (!texture || texture->GetImage() == nullptr || texture->GetImage()->storage() == nullptr) {
          umd_log("rotate_resource_backings: entry without image storage");
          return false;
        }
        images.push_back(texture->GetImage());
      }

      // CS-side identity rotation (18th session), mirroring upstream
      // D3D11SwapChain::RotateBackBuffers: swap the storages ON the CS thread
      // via DxvkContext::invalidateImage. No GPU drain is needed — every
      // already-recorded command holds its own storage ref and keeps targeting
      // the pre-rotation memory; the swap applies in CS order for everything
      // recorded after this DDI. The two rejected designs, for the record:
      //  - whole-device event-query drain (bring-up shim): 15-25 ms per
      //    present, dominated by Sleep(1) timer quantization;
      //  - per-image waitForResource on the present thread: WEDGES dwm — the
      //    bound backbuffer RTV is re-recorded into every new open cmdlist, so
      //    isInUse(Read) never clears for a bound render target (proven live
      //    with a dwm minidump, thread 1 parked in synchronizeUntil).
      LARGE_INTEGER qpcFreq, qpcT0;
      QueryPerformanceFrequency(&qpcFreq);
      QueryPerformanceCounter(&qpcT0);

      // InjectCsOrderedAfterPending dispatches the open recording chunk and
      // appends the swap on the ordered CS queue WITHOUT waiting for the CS
      // thread: the earlier SynchronizeCsThread variant blocked the present
      // thread behind the whole CS queue — up to 1.9 s per present during
      // login churn (rotate-perf) — the owner-visible "occasional dips".
      auto* immediateContext = static_cast<dxvk::D3D11ImmediateContext*>(impl->context);

      immediateContext->InjectCsOrderedAfterPending([
        cImages = std::move(images)
      ] (dxvk::DxvkContext* ctx) {
        auto first = cImages[0]->storage();

        for (std::size_t i = 0; i + 1 < cImages.size(); ++i) {
          ctx->invalidateImage(cImages[i], cImages[i + 1]->storage(),
            cImages[i + 1]->info().layout);
        }

        ctx->invalidateImage(cImages[cImages.size() - 1u],
          std::move(first), cImages[0]->info().layout);
      });

      // Drain-cost telemetry (measure-first, PSC WS2): same key and format as
      // the old whole-device drain so before/after numbers compare directly.
      // One log line per 32 rotations.
      {
        LARGE_INTEGER qpcT1;
        QueryPerformanceCounter(&qpcT1);
        static PeriodicStat s_drainStat(32u);
        if (const auto sample = s_drainStat.record(qpc_elapsed_us(qpcFreq, qpcT0, qpcT1))) {
          char msg[128];
          std::snprintf(msg, sizeof(msg),
                        "rotate-perf: n=%u drain_avg_us=%llu drain_max_us=%llu",
                        sample->n,
                        static_cast<unsigned long long>(sample->avg_us),
                        static_cast<unsigned long long>(sample->max_us));
          umd_log(msg);
        }
      }

      // Debug instrument (registry-gated, off by default): sample the ring
      // buffers — write-side ground truth for "does the composed frame carry
      // pixels". Records after the injected swap, so slots are POST-rotation
      // identities. HKLM\SOFTWARE\Helios!RotateSample (DWORD) = sample every
      // Nth rotation.
      static std::atomic<std::uint32_t> s_sampleEvery{~0u};
      static std::atomic<std::uint32_t> s_rotateCount{0};
      std::uint32_t sampleEvery = s_sampleEvery.load(std::memory_order_relaxed);
      if (sampleEvery == ~0u) {
        DWORD value = 0, size = sizeof(value);
        if (RegGetValueA(HKEY_LOCAL_MACHINE, "SOFTWARE\\Helios", "RotateSample",
                         RRF_RT_REG_DWORD, nullptr, &value, &size) != ERROR_SUCCESS)
          value = 0;
        sampleEvery = value;
        s_sampleEvery.store(sampleEvery, std::memory_order_relaxed);
      }
      if (sampleEvery && (s_rotateCount.fetch_add(1) % sampleEvery) == 0) {
        // Sample EVERY buffer in the ring, not just the presented one: a
        // nonzero count appearing in a slot other than [0] means content lands
        // in a buffer the present/rotation bookkeeping does not associate with
        // the presented allocation (ring misalignment), while all-zero across
        // the whole ring means the composition draws genuinely write nothing.
        for (std::size_t s = 0; s < count; ++s) {
          auto* res = reinterpret_cast<ID3D11Resource*>(d3d11_resource_ptrs[s]);
          ID3D11Texture2D* tex = nullptr;
          if (FAILED(res->QueryInterface(__uuidof(ID3D11Texture2D),
                                         reinterpret_cast<void**>(&tex)))) {
            char skipmsg[128];
            std::snprintf(skipmsg, sizeof(skipmsg),
                          "rotate-sample: slot=%zu/%zu SKIPPED (not a Texture2D)", s, count);
            umd_log(skipmsg);
            continue;
          }
          D3D11_TEXTURE2D_DESC td = {};
          tex->GetDesc(&td);
          // The old code forced MipLevels/ArraySize to 1 on the staging desc while
          // copying from the real one. With ArraySize > 1 the descriptions
          // mismatch, CopyResource is a silent no-op, and the tool reports
          // nonzero=0/N — the exact false conclusion ("the composition draws write
          // nothing") it exists to test for. And the rows below are read as
          // std::uint32_t, i.e. 32bpp is assumed: against a 16bpp ring the 4-byte
          // column stride reads past the last row, which is an OOB read, not
          // merely a wrong number. Skip and SAY SO instead.
          if (td.MipLevels != 1 || td.ArraySize != 1 || !is_32bpp_dxgi_format(td.Format)) {
            char skipmsg[192];
            std::snprintf(skipmsg, sizeof(skipmsg),
                          "rotate-sample: slot=%zu/%zu SKIPPED (mips=%u array=%u fmt=%u — "
                          "needs a single-subresource 32bpp texture)",
                          s, count, td.MipLevels, td.ArraySize,
                          static_cast<unsigned>(td.Format));
            umd_log(skipmsg);
            tex->Release();
            continue;
          }
          D3D11_TEXTURE2D_DESC sd = td;
          sd.BindFlags = 0;
          sd.MiscFlags = 0;
          sd.Usage = D3D11_USAGE_STAGING;
          sd.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
          ID3D11Texture2D* staging = nullptr;
          if (SUCCEEDED(impl->d3d11->CreateTexture2D(&sd, nullptr, &staging)) && staging) {
            impl->context->CopyResource(staging, tex);
            D3D11_MAPPED_SUBRESOURCE map = {};
            if (SUCCEEDED(impl->context->Map(staging, 0, D3D11_MAP_READ, 0, &map)) &&
                map.pData != nullptr) {
              const auto* base = static_cast<const std::uint8_t*>(map.pData);
              std::uint32_t nonzero = 0, samples = 0;
              for (UINT y = 0; y < td.Height; y += 64) {
                const auto* row = reinterpret_cast<const std::uint32_t*>(base + std::size_t(y) * map.RowPitch);
                for (UINT x = 0; x < td.Width; x += 64) {
                  ++samples;
                  nonzero += row[x] != 0;
                }
              }
              char msg[160];
              std::snprintf(msg, sizeof(msg),
                            "rotate-sample: slot=%zu/%zu %ux%u nonzero=%u/%u center=0x%08x",
                            s, count, td.Width, td.Height, nonzero, samples,
                            reinterpret_cast<const std::uint32_t*>(
                                base + std::size_t(td.Height / 2) * map.RowPitch)[td.Width / 2]);
              umd_log(msg);
              impl->context->Unmap(staging, 0);
            } else {
              char skipmsg[128];
              std::snprintf(skipmsg, sizeof(skipmsg),
                            "rotate-sample: slot=%zu/%zu SKIPPED (staging Map returned no data)",
                            s, count);
              umd_log(skipmsg);
            }
            staging->Release();
          } else {
            char skipmsg[128];
            std::snprintf(skipmsg, sizeof(skipmsg),
                          "rotate-sample: slot=%zu/%zu SKIPPED (staging CreateTexture2D failed)",
                          s, count);
            umd_log(skipmsg);
          }
          tex->Release();
        }
      }

      // The storage swap itself (resource[i] takes resource[i+1]'s, the last
      // takes the first's) executes in the injected CS command above.
      return true;
  });
}

bool HeliosDxvkDevice::flush_submitted() const {
  if (!impl || !impl->context || impl->device == nullptr)
    return false;
  return bridge_guard("flush_submitted", false, [&]() -> bool {
    auto* immediateContext = static_cast<dxvk::D3D11ImmediateContext*>(impl->context);
    immediateContext->Flush();
    immediateContext->SynchronizeCsThread(dxvk::DxvkCsThread::SynchronizeAll);
    impl->device->syncSubmissions();
    return true;
  });
}

std::size_t HeliosDxvkDevice::create_hull_shader(const std::uint8_t* code, std::size_t len) const {
  return create_shader_impl<ID3D11HullShader>(
      impl.get(), "hs", "CreateHullShader", code, len,
      [](ID3D11Device* d, const void* bc, std::size_t n, ID3D11HullShader** out) {
        return d->CreateHullShader(bc, n, nullptr, out);
      });
}

std::size_t HeliosDxvkDevice::create_domain_shader(const std::uint8_t* code, std::size_t len) const {
  return create_shader_impl<ID3D11DomainShader>(
      impl.get(), "ds", "CreateDomainShader", code, len,
      [](ID3D11Device* d, const void* bc, std::size_t n, ID3D11DomainShader** out) {
        return d->CreateDomainShader(bc, n, nullptr, out);
      });
}

std::size_t HeliosDxvkDevice::create_compute_shader(const std::uint8_t* code, std::size_t len) const {
  return create_shader_impl<ID3D11ComputeShader>(
      impl.get(), "cs", "CreateComputeShader", code, len,
      [](ID3D11Device* d, const void* bc, std::size_t n, ID3D11ComputeShader** out) {
        return d->CreateComputeShader(bc, n, nullptr, out);
      });
}

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
    std::size_t   outer_retire) {
  // R824: configuration delivered as a process-global side effect, whose
  // correctness used to be statement position -- these writes happened on EVERY
  // CreateDevice DDI, and one process (dwm) creates several D3D11 devices, so
  // the block was rewritten while earlier DxvkInstances were live and
  // _putenv_s is not safe against a concurrent getenv. The values are identical
  // on every call, so doing it once is behaviour-preserving; what goes away is
  // the repeat writes and the concurrent-write window.
  //
  // Static guarantee: none. std::call_once is a runtime construct and an
  // `EnvConfigured` token would be ceremony around one call site. _putenv_s
  // stays the mechanism because DXVK reads env; changing that is out of scope.
  static std::once_flag s_envOnce;
  std::call_once(s_envOnce, [] {
    // Force selection of the Helios venus device if other ICDs are present.
    _putenv_s("DXVK_FILTER_DEVICE_NAME", "Virtio-GPU Venus");
    // HELIOS_DXVK_KMT_SHARED is no longer forced here: the engine defaults it
    // ON (2026-08-05). Forcing it made a knob that could not be off in any
    // configuration this process ever produced, which hid the fact that the
    // engine's own default disagreed with every measurement taken.

    // Debug instrument (registry-gated, off by default): route DXVK's shader
    // dumping into every UMD-hosting process — session-0 services (dwm) cannot
    // be given process env vars any other way. HKLM\SOFTWARE\Helios!
    // ShaderDumpPath (REG_SZ) = target directory.
    char dumpPath[MAX_PATH] = {};
    DWORD size = sizeof(dumpPath);
    const bool haveDump =
        RegGetValueA(HKEY_LOCAL_MACHINE, "SOFTWARE\\Helios", "ShaderDumpPath",
                     RRF_RT_REG_SZ, nullptr, dumpPath, &size) == ERROR_SUCCESS &&
        dumpPath[0];
    if (haveDump)
      _putenv_s("DXVK_SHADER_DUMP_PATH", dumpPath);

    char msg[MAX_PATH + 128];
    std::snprintf(msg, sizeof(msg),
      "dxvk env configured once: DXVK_FILTER_DEVICE_NAME=Virtio-GPU Venus "
      "kmt-shared=default-on DXVK_SHADER_DUMP_PATH=%s",
      haveDump ? dumpPath : "(unset)");
    umd_log(msg);
  });

  return bridge_guard<std::unique_ptr<HeliosDxvkDevice>>(
      "helios_dxvk_create_device", nullptr,
      [&]() -> std::unique_ptr<HeliosDxvkDevice> {
      auto out = std::make_unique<HeliosDxvkDevice>();
      out->impl = std::make_unique<HeliosDxvkDeviceImpl>();
      auto& d = *out->impl;

      if (!vk_instance || !get_instance_proc_addr || !icd_module_base ||
          !outer_context || !outer_admit || !outer_begin || !outer_finish || !outer_join ||
          !outer_allocate || !outer_teardown_begin || !outer_retire ||
          (!luid_low && !luid_high)) {
        umd_log("REFUSING DXVK device: incomplete A5/provenance/LUID/outer edge");
        return nullptr;
      }

      dxvk::DxvkInstanceImportInfo import_info;
      import_info.loaderProc = reinterpret_cast<PFN_vkGetInstanceProcAddr>(
          get_instance_proc_addr);
      import_info.instance = reinterpret_cast<VkInstance>(vk_instance);
      import_info.recordOnlyDirect = true;
      import_info.expectedModule = reinterpret_cast<HMODULE>(icd_module_base);
      d.instance = new dxvk::DxvkInstance(import_info, dxvk::DxvkInstanceFlags());

      LUID luid;
      luid.LowPart  = luid_low;
      luid.HighPart = luid_high;
      d.adapter = d.instance->findAdapterByLuid(&luid);

      if (d.adapter == nullptr) {
        umd_log("REFUSING DXVK device: A5 instance has no exact LUID match");
        return nullptr;
      }

      dxvk::DxvkHeliosOuterOps outer_ops = { };
      outer_ops.context = reinterpret_cast<void*>(outer_context);
      outer_ops.begin = reinterpret_cast<dxvk::DxvkHeliosOuterBeginProc>(outer_begin);
      outer_ops.finish = reinterpret_cast<dxvk::DxvkHeliosOuterFinishProc>(outer_finish);
      outer_ops.join = reinterpret_cast<dxvk::DxvkHeliosOuterJoinProc>(outer_join);
      outer_ops.allocate = reinterpret_cast<dxvk::DxvkHeliosOuterAllocateProc>(outer_allocate);
      outer_ops.teardownBegin = reinterpret_cast<dxvk::DxvkHeliosOuterTeardownBeginProc>(outer_teardown_begin);
      outer_ops.retire = reinterpret_cast<dxvk::DxvkHeliosOuterRetireProc>(outer_retire);
      d.device = d.adapter->createDevice(outer_ops);
      if (d.device == nullptr) {
        umd_log("DxvkAdapter::createDevice returned null");
        return nullptr;
      }
      umd_log("DxvkDevice created from exact A5 record-only instance");

      // vkCreateDevice has now created and registered the exact A5 queues.
      // Admit the WDDM HQA1/HQC1 context before the D3D11 COM constructor can
      // allocate resources or submit through those queues. This synchronous
      // callback owns no lookup: outer_context is the same boxed object carried
      // by every other direct edge.
      using OuterAdmitProc = std::int32_t (*)(void*);
      auto admit = reinterpret_cast<OuterAdmitProc>(outer_admit);
      const std::int32_t admit_result =
          admit(reinterpret_cast<void*>(outer_context));
      if (admit_result < 0) {
        char msg[128];
        std::snprintf(msg, sizeof(msg),
          "REFUSING DXVK device: outer HQA1/HQC1 admission failed hr=0x%08x",
          static_cast<std::uint32_t>(admit_result));
        umd_log(msg);
        return nullptr;
      }

      // Instantiate DXVK's full D3D11 COM device from the DxvkDevice. The DDI
      // device-funcs forward to this ID3D11Device / its immediate context.
      // `new HeliosStubAdapter()` starts at refcount 1 and is only released AFTER
      // the D3D11DXGIDevice constructor returns — but that constructor builds the
      // D3D11 device and its immediate context and can throw dxvk::DxvkError, in
      // which case the catch below returns nullptr and the Release() never runs.
      // The guard makes the zero-refcount window exit through exactly one path.
      ComRelease<HeliosStubAdapter> stubAdapter(new HeliosStubAdapter());
      auto* dxgiDevice = new dxvk::D3D11DXGIDevice(
          stubAdapter.get(), nullptr, nullptr,
          d.instance, d.adapter, d.device,
          D3D_FEATURE_LEVEL_11_0, 0);
      stubAdapter.reset(); // dxgiDevice holds its own ref now

      HRESULT hr = dxgiDevice->QueryInterface(__uuidof(ID3D11Device),
                                              reinterpret_cast<void**>(&d.d3d11));
      if (FAILED(hr) || d.d3d11 == nullptr) {
        umd_log("QueryInterface(ID3D11Device) on D3D11DXGIDevice failed");
        // dxgiDevice has refcount 0 here (QI failed) — drop it.
        delete dxgiDevice;
        return nullptr;
      }
      // d.d3d11 now holds the one ref that keeps dxgiDevice alive.
      d.d3d11->GetImmediateContext(&d.context);
      umd_log("D3D11 COM device + immediate context created OK");
      return out;
  });
}
