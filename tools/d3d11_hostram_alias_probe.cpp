// Is the guest's CPU view of a staging texture the same guest RAM the host
// imports, and do the host GPU's writes land there?
//
// 2026-08-29. Every instrument that has been used on this question lives inside
// the stack under test: queries resolve before their work is flushed, the KMD
// backing-store sampler read one buffer N times, and a WARP control never
// touches DXVK. This probe answers it from OUTSIDE, with an observer that has
// no stake in the driver at all: QEMU's own view of guest physical memory.
//
//   QMP `xp/<n>xw <gpa>` reads guest RAM directly. The GPAs come from the
//   already-enabled `virtio_gpu_virgl_guest_blob_backing` trace, which prints
//   the first and last guest range of every role-1 blob the KMD hands the host.
//
// The staging texture is 1024x1024 B8G8R8A8 = exactly 4 MiB, the size of every
// role-1 pool in the trace, so the pool IS the texture and any page of it is
// data. Each dword is stamped 0xC0DE<low 16 bits of its own dword index>, so a
// dword read from the host names its own offset rather than merely matching.
//
// Three outcomes, all decisive:
//   host RAM shows 0xC0DExxxx, then the clear colour  -> memory aliases both
//        ways; the defect is in the guest's READ (caching / premature map).
//   host RAM shows 0xC0DExxxx, then still 0xC0DExxxx  -> the host's writes do
//        not reach the guest pages; its VkDeviceMemory is not our blob.
//   host RAM never shows 0xC0DExxxx                   -> the Lock2 CPU view is
//        not the memory the KMD exported. F16 confirmed.
//
// Long sleeps between phases are the sampling windows; the phase markers are
// flushed so the driver script can wait on them.
//
// Session 0, no window, no DWM, no scanout. Build on the VM with mingw:
//   g++ -O1 -o C:\Users\Rupansh\d3d11_hostram_alias_probe.exe ^
//       Z:\tools\d3d11_hostram_alias_probe.cpp -ld3d11 -ldxgi -ldxguid
#include <d3d11.h>
#include <dxgi1_2.h>
#include <windows.h>
#include <cstdio>
#include <cstdint>
#include <cwchar>

static const UINT W = 1024, H = 1024;

// Every dword names its own index, so a dword sampled from guest RAM proves
// both that it is ours and where in the texture it came from.
static inline uint32_t stamp(uint32_t index) {
  return 0xC0DE0000u | (index & 0xFFFFu);
}

// Magenta is exact in 8-bit UNORM, so the copied bytes are B=FF G=00 R=FF A=FF,
// i.e. the dword 0xFFFF00FF read back from BGRA memory.
static const float CLEAR[4] = {1.0f, 0.0f, 1.0f, 1.0f};
static const uint32_t CLEAR_DWORD = 0xFFFF00FFu;

static IDXGIAdapter1 *find_helios(IDXGIFactory1 *factory) {
  IDXGIAdapter1 *adapter = nullptr;
  for (UINT i = 0; factory->EnumAdapters1(i, &adapter) != DXGI_ERROR_NOT_FOUND; ++i) {
    DXGI_ADAPTER_DESC1 desc = {};
    adapter->GetDesc1(&desc);
    if (wcsstr(desc.Description, L"Helios")) return adapter;
    adapter->Release();
    adapter = nullptr;
  }
  return nullptr;
}

static void mark(const char *text) {
  printf("%s\n", text);
  fflush(stdout);
}

int main(int argc, char **argv) {
  const DWORD window_ms = argc > 1 ? (DWORD)strtoul(argv[1], nullptr, 10) : 25000;

  IDXGIFactory1 *factory = nullptr;
  if (FAILED(CreateDXGIFactory1(__uuidof(IDXGIFactory1), (void **)&factory))) {
    mark("FAIL CreateDXGIFactory1");
    return 1;
  }
  IDXGIAdapter1 *adapter = find_helios(factory);
  if (!adapter) {
    mark("FAIL no Helios adapter");
    return 1;
  }

  ID3D11Device *dev = nullptr;
  ID3D11DeviceContext *ctx = nullptr;
  D3D_FEATURE_LEVEL got = {};
  const D3D_FEATURE_LEVEL want[] = {D3D_FEATURE_LEVEL_11_0};
  HRESULT hr = D3D11CreateDevice(adapter, D3D_DRIVER_TYPE_UNKNOWN, nullptr, 0, want, 1,
                                 D3D11_SDK_VERSION, &dev, &got, &ctx);
  if (FAILED(hr)) {
    printf("FAIL D3D11CreateDevice 0x%08lx\n", (unsigned long)hr);
    return 1;
  }

  D3D11_TEXTURE2D_DESC sd = {};
  sd.Width = W; sd.Height = H; sd.MipLevels = 1; sd.ArraySize = 1;
  sd.Format = DXGI_FORMAT_B8G8R8A8_UNORM;
  sd.SampleDesc.Count = 1;
  sd.Usage = D3D11_USAGE_STAGING;
  sd.CPUAccessFlags = D3D11_CPU_ACCESS_READ | D3D11_CPU_ACCESS_WRITE;
  ID3D11Texture2D *staging = nullptr;
  hr = dev->CreateTexture2D(&sd, nullptr, &staging);
  if (FAILED(hr)) {
    printf("FAIL staging 0x%08lx\n", (unsigned long)hr);
    return 1;
  }

  D3D11_TEXTURE2D_DESC rd = sd;
  rd.Usage = D3D11_USAGE_DEFAULT;
  rd.CPUAccessFlags = 0;
  rd.BindFlags = D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE;
  ID3D11Texture2D *target = nullptr;
  hr = dev->CreateTexture2D(&rd, nullptr, &target);
  if (FAILED(hr)) {
    printf("FAIL target 0x%08lx\n", (unsigned long)hr);
    return 1;
  }
  ID3D11RenderTargetView *rtv = nullptr;
  hr = dev->CreateRenderTargetView(target, nullptr, &rtv);
  if (FAILED(hr)) {
    printf("FAIL rtv 0x%08lx\n", (unsigned long)hr);
    return 1;
  }

  // Stamp the whole 4 MiB through the mapped pointer.
  D3D11_MAPPED_SUBRESOURCE map = {};
  hr = ctx->Map(staging, 0, D3D11_MAP_WRITE, 0, &map);
  if (FAILED(hr)) {
    printf("FAIL map-write 0x%08lx\n", (unsigned long)hr);
    return 1;
  }
  printf("MAP ptr=%p pitch=%u\n", map.pData, (unsigned)map.RowPitch);
  // What KIND of memory did the driver hand the application? A view of a WDDM
  // allocation is a mapped section; MEM_PRIVATE committed heap is the UMD's own
  // process memory and cannot be anything the host also sees.
  MEMORY_BASIC_INFORMATION mbi = {};
  if (VirtualQuery(map.pData, &mbi, sizeof(mbi))) {
    printf("MAPKIND base=%p region=%llu state=0x%lx protect=0x%lx type=0x%lx%s\n",
           mbi.AllocationBase, (unsigned long long)mbi.RegionSize,
           (unsigned long)mbi.State, (unsigned long)mbi.Protect,
           (unsigned long)mbi.Type,
           mbi.Type == MEM_PRIVATE ? " PRIVATE(process heap)"
           : mbi.Type == MEM_MAPPED ? " MAPPED(section)"
           : mbi.Type == MEM_IMAGE ? " IMAGE" : " ?");
  }
  for (UINT y = 0; y < H; ++y) {
    uint32_t *row = (uint32_t *)((uint8_t *)map.pData + (size_t)y * map.RowPitch);
    for (UINT x = 0; x < W; ++x) row[x] = stamp(y * W + x);
  }
  ctx->Unmap(staging, 0);

  // A CPU-only round trip, so a later miss cannot be blamed on the write.
  hr = ctx->Map(staging, 0, D3D11_MAP_READ, 0, &map);
  if (FAILED(hr)) {
    printf("FAIL map-read-1 0x%08lx\n", (unsigned long)hr);
    return 1;
  }
  UINT good = 0;
  for (UINT y = 0; y < H; ++y) {
    const uint32_t *row = (const uint32_t *)((const uint8_t *)map.pData + (size_t)y * map.RowPitch);
    for (UINT x = 0; x < W; ++x) if (row[x] == stamp(y * W + x)) ++good;
  }
  ctx->Unmap(staging, 0);
  printf("P1 STAMPED cpu_readback=%u/%u\n", good, W * H);
  mark("P1 WINDOW OPEN");
  Sleep(window_ms);

  ctx->ClearRenderTargetView(rtv, CLEAR);
  ctx->CopyResource(staging, target);
  ctx->Flush();
  Sleep(3000);
  mark("P2 WINDOW OPEN");
  Sleep(window_ms);

  hr = ctx->Map(staging, 0, D3D11_MAP_READ, 0, &map);
  if (FAILED(hr)) {
    printf("FAIL map-read-2 0x%08lx\n", (unsigned long)hr);
    return 1;
  }
  UINT stamped = 0, cleared = 0, zero = 0, other = 0;
  uint32_t first_other = 0;
  for (UINT y = 0; y < H; ++y) {
    const uint32_t *row = (const uint32_t *)((const uint8_t *)map.pData + (size_t)y * map.RowPitch);
    for (UINT x = 0; x < W; ++x) {
      const uint32_t v = row[x];
      if (v == stamp(y * W + x)) ++stamped;
      else if (v == CLEAR_DWORD) ++cleared;
      else if (v == 0) ++zero;
      else { if (!other) first_other = v; ++other; }
    }
  }
  ctx->Unmap(staging, 0);
  printf("P3 AFTER_COPY stamped=%u cleared=%u zero=%u other=%u first_other=0x%08x\n",
         stamped, cleared, zero, other, first_other);
  mark("P3 DONE");
  Sleep(8000);
  return 0;
}
