// Exercise the outer-allocation sizing path for D3D11 buffers.
//
// 2026-08-29. D3D11Device::...queryBufferMemoryRequirements (d3d11_device.cpp:353
// -> dxvk_device.cpp:129) is the only caller of DxvkDevice::
// queryBufferMemoryRequirements, and it runs for BUFFERS only — which is why the
// texture-only probes never triggered the corrupt CONCURRENT queue-family index
// the host reports 73 times a boot. Create buffers of every usage class so the
// path runs, then check /tmp/helios-qemu-stderr.log for
// "pQueueFamilyIndices[0] (1000146003)".
//
//   g++ -O1 -o C:\Users\Rupansh\d3d11_buffer_reqs_probe.exe ^
//       Z:\tools\d3d11_buffer_reqs_probe.cpp -ld3d11 -ldxgi -ldxguid
#include <d3d11.h>
#include <dxgi1_2.h>
#include <windows.h>
#include <cstdio>
#include <cwchar>
#include <initializer_list>

int main() {
  IDXGIFactory1 *f = nullptr;
  if (FAILED(CreateDXGIFactory1(IID_IDXGIFactory1, (void **)&f))) return 1;
  IDXGIAdapter1 *h = nullptr, *a = nullptr;
  for (UINT i = 0; f->EnumAdapters1(i, &a) != DXGI_ERROR_NOT_FOUND; ++i) {
    DXGI_ADAPTER_DESC1 d = {}; a->GetDesc1(&d);
    if (wcsstr(d.Description, L"Helios")) { h = a; break; }
    a->Release();
  }
  if (!h) { printf("no Helios adapter\n"); return 1; }

  ID3D11Device *dev = nullptr; ID3D11DeviceContext *ctx = nullptr;
  D3D_FEATURE_LEVEL got = (D3D_FEATURE_LEVEL)0;
  HRESULT hr = D3D11CreateDevice(h, D3D_DRIVER_TYPE_UNKNOWN, nullptr, 0, nullptr, 0,
                                 D3D11_SDK_VERSION, &dev, &got, &ctx);
  printf("D3D11CreateDevice hr=0x%08x\n", (unsigned)hr);
  if (FAILED(hr)) return 1;

  struct { const char *name; UINT bind; D3D11_USAGE usage; UINT cpu; } kinds[] = {
    { "VERTEX",   D3D11_BIND_VERTEX_BUFFER,   D3D11_USAGE_DEFAULT, 0 },
    { "INDEX",    D3D11_BIND_INDEX_BUFFER,    D3D11_USAGE_DEFAULT, 0 },
    { "CONSTANT", D3D11_BIND_CONSTANT_BUFFER, D3D11_USAGE_DYNAMIC, D3D11_CPU_ACCESS_WRITE },
    { "SRV",      D3D11_BIND_SHADER_RESOURCE, D3D11_USAGE_DEFAULT, 0 },
    { "UAV",      D3D11_BIND_UNORDERED_ACCESS,D3D11_USAGE_DEFAULT, 0 },
    { "STAGING",  0,                          D3D11_USAGE_STAGING,
      D3D11_CPU_ACCESS_READ | D3D11_CPU_ACCESS_WRITE },
  };
  int made = 0;
  for (auto &k : kinds) {
    for (UINT sz : { 256u, 4096u, 65536u, 1048576u }) {
      D3D11_BUFFER_DESC bd = {};
      bd.ByteWidth = sz; bd.Usage = k.usage; bd.BindFlags = k.bind;
      bd.CPUAccessFlags = k.cpu;
      if (k.bind == D3D11_BIND_SHADER_RESOURCE || k.bind == D3D11_BIND_UNORDERED_ACCESS) {
        bd.MiscFlags = D3D11_RESOURCE_MISC_BUFFER_STRUCTURED;
        bd.StructureByteStride = 16;
      }
      ID3D11Buffer *b = nullptr;
      HRESULT r = dev->CreateBuffer(&bd, nullptr, &b);
      if (SUCCEEDED(r)) { ++made; b->Release(); }
      else printf("  %-8s size=%-8u hr=0x%08x\n", k.name, sz, (unsigned)r);
    }
  }
  printf("buffers created OK: %d/%d\n", made, (int)(sizeof(kinds)/sizeof(kinds[0])) * 4);
  ctx->Release(); dev->Release(); h->Release(); f->Release();
  return 0;
}
