// Split the "everything reads back zero" defect into its four stages.
//
// 2026-08-28: helios_clear_test_321, d3d11_staging_readback_probe,
// d3d11_upload_integrity_probe (30/30) and d3d11_shared_content_probe all
// return an all-zero texture on Helios, by EVERY write method — initdata,
// UpdateSubresource and Map(WRITE_DISCARD) alike. That rules the black desktop
// out as a display defect but does not say which half is dead. These four
// stages do, and they need no window, no DWM and no scanout, so they run in
// session 0 in about two seconds:
//
//   1 CPU     Map(WRITE) -> Unmap -> Map(READ) on ONE staging texture.
//             FAIL here = Map is not a stable view; no GPU is involved.
//   2 UPLOAD  staging(write) -> CopyResource -> staging(read).
//             FAIL here with 1 passing = the copy engine loses CPU content.
//   3 INIT    DEFAULT created with D3D11_SUBRESOURCE_DATA -> copy -> read.
//   4 CLEAR   DEFAULT render target, ClearRenderTargetView -> copy -> read.
//             FAIL here alone = GPU writes are lost; everything else is fine.
//
// Build on the VM (mingw is on PATH; there is no cl.exe outside vcvars):
//   g++ -O1 -o C:\Users\Rupansh\d3d11_roundtrip_split_probe.exe ^
//       Z:\tools\d3d11_roundtrip_split_probe.cpp -ld3d11 -ldxgi -ldxguid
#include <d3d11.h>
#include <dxgi1_2.h>
#include <cstdio>
#include <cstdint>
#include <cwchar>

static const UINT W = 64, H = 64;

static IDXGIAdapter1 *find_helios(IDXGIFactory1 *factory) {
  IDXGIAdapter1 *adapter = nullptr;
  for (UINT i = 0; factory->EnumAdapters1(i, &adapter) != DXGI_ERROR_NOT_FOUND; ++i) {
    DXGI_ADAPTER_DESC1 desc = {};
    adapter->GetDesc1(&desc);
    wprintf(L"[%u] \"%s\" vendor=0x%04x\n", i, desc.Description, desc.VendorId);
    if (wcsstr(desc.Description, L"Helios")) return adapter;
    adapter->Release();
    adapter = nullptr;
  }
  return nullptr;
}

// Deterministic per-pixel pattern; never all-zero, so "read zero" cannot pass.
static uint32_t pat(UINT x, UINT y) { return 0xff000000u | (x << 8) | y | 0x00010000u; }

static ID3D11Texture2D *mktex(ID3D11Device *dev, D3D11_USAGE usage, UINT bind,
                              UINT cpu, const D3D11_SUBRESOURCE_DATA *init) {
  D3D11_TEXTURE2D_DESC d = {};
  d.Width = W; d.Height = H; d.MipLevels = 1; d.ArraySize = 1;
  d.Format = DXGI_FORMAT_B8G8R8A8_UNORM;
  d.SampleDesc.Count = 1;
  d.Usage = usage; d.BindFlags = bind; d.CPUAccessFlags = cpu;
  ID3D11Texture2D *t = nullptr;
  HRESULT hr = dev->CreateTexture2D(&d, init, &t);
  if (FAILED(hr)) { printf("  CreateTexture2D usage=%d bind=0x%x hr=0x%08x\n", (int)usage, bind, (unsigned)hr); return nullptr; }
  return t;
}

// Map(READ) `stage` and count how many of the W*H pixels match pat(), and how
// many bytes are zero. Both numbers are printed: "all zero" and "wrong values"
// are different diagnoses and have been confused before.
static bool verify(ID3D11DeviceContext *ctx, ID3D11Texture2D *stage, const char *tag) {
  D3D11_MAPPED_SUBRESOURCE m = {};
  HRESULT hr = ctx->Map(stage, 0, D3D11_MAP_READ, 0, &m);
  if (FAILED(hr)) { printf("%-7s Map(READ) hr=0x%08x FAIL\n", tag, (unsigned)hr); return false; }
  UINT good = 0, zero = 0;
  for (UINT y = 0; y < H; ++y) {
    const uint32_t *row = (const uint32_t *)((const uint8_t *)m.pData + (size_t)y * m.RowPitch);
    for (UINT x = 0; x < W; ++x) {
      if (row[x] == pat(x, y)) ++good;
      if (row[x] == 0) ++zero;
    }
  }
  ctx->Unmap(stage, 0);
  const UINT total = W * H;
  printf("%-7s match=%u/%u zero=%u/%u pitch=%u  %s\n", tag, good, total, zero, total,
         m.RowPitch, good == total ? "PASS" : "FAIL");
  return good == total;
}

static void fill(void *base, UINT pitch) {
  for (UINT y = 0; y < H; ++y) {
    uint32_t *row = (uint32_t *)((uint8_t *)base + (size_t)y * pitch);
    for (UINT x = 0; x < W; ++x) row[x] = pat(x, y);
  }
}

int main() {
  IDXGIFactory1 *factory = nullptr;
  if (FAILED(CreateDXGIFactory1(IID_IDXGIFactory1, (void **)&factory))) { printf("no factory\n"); return 1; }
  IDXGIAdapter1 *helios = find_helios(factory);
  if (!helios) { printf("Helios adapter not found\n"); return 1; }

  ID3D11Device *dev = nullptr; ID3D11DeviceContext *ctx = nullptr;
  D3D_FEATURE_LEVEL got = (D3D_FEATURE_LEVEL)0;
  HRESULT hr = D3D11CreateDevice(helios, D3D_DRIVER_TYPE_UNKNOWN, nullptr, 0,
                                 nullptr, 0, D3D11_SDK_VERSION, &dev, &got, &ctx);
  printf("D3D11CreateDevice hr=0x%08x fl=0x%x\n", (unsigned)hr, (unsigned)got);
  if (FAILED(hr)) return 1;

  int fails = 0;

  // 1 CPU: one staging texture, written and read through Map only.
  if (ID3D11Texture2D *s = mktex(dev, D3D11_USAGE_STAGING, 0,
                                 D3D11_CPU_ACCESS_READ | D3D11_CPU_ACCESS_WRITE, nullptr)) {
    D3D11_MAPPED_SUBRESOURCE m = {};
    hr = ctx->Map(s, 0, D3D11_MAP_WRITE, 0, &m);
    if (FAILED(hr)) { printf("1 CPU   Map(WRITE) hr=0x%08x FAIL\n", (unsigned)hr); ++fails; }
    else { fill(m.pData, m.RowPitch); ctx->Unmap(s, 0); if (!verify(ctx, s, "1 CPU")) ++fails; }
    s->Release();
  } else ++fails;

  // 2 UPLOAD: CPU-written staging -> DEFAULT -> staging.
  {
    ID3D11Texture2D *src = mktex(dev, D3D11_USAGE_STAGING, 0, D3D11_CPU_ACCESS_WRITE, nullptr);
    ID3D11Texture2D *mid = mktex(dev, D3D11_USAGE_DEFAULT, D3D11_BIND_SHADER_RESOURCE, 0, nullptr);
    ID3D11Texture2D *dst = mktex(dev, D3D11_USAGE_STAGING, 0, D3D11_CPU_ACCESS_READ, nullptr);
    if (src && mid && dst) {
      D3D11_MAPPED_SUBRESOURCE m = {};
      if (SUCCEEDED(ctx->Map(src, 0, D3D11_MAP_WRITE, 0, &m))) {
        fill(m.pData, m.RowPitch); ctx->Unmap(src, 0);
        ctx->CopyResource(mid, src);
        ctx->CopyResource(dst, mid);
        ctx->Flush();
        if (!verify(ctx, dst, "2 UPLOAD")) ++fails;
      } else { printf("2 UPLOAD Map(WRITE) failed\n"); ++fails; }
    } else ++fails;
    if (src) src->Release(); if (mid) mid->Release(); if (dst) dst->Release();
  }

  // 3 INIT: DEFAULT created with D3D11_SUBRESOURCE_DATA -> staging.
  {
    static uint32_t init[W * H];
    fill(init, W * 4);
    D3D11_SUBRESOURCE_DATA sd = {}; sd.pSysMem = init; sd.SysMemPitch = W * 4;
    ID3D11Texture2D *mid = mktex(dev, D3D11_USAGE_DEFAULT, D3D11_BIND_SHADER_RESOURCE, 0, &sd);
    ID3D11Texture2D *dst = mktex(dev, D3D11_USAGE_STAGING, 0, D3D11_CPU_ACCESS_READ, nullptr);
    if (mid && dst) {
      ctx->CopyResource(dst, mid); ctx->Flush();
      if (!verify(ctx, dst, "3 INIT")) ++fails;
    } else ++fails;
    if (mid) mid->Release(); if (dst) dst->Release();
  }

  // 4 CLEAR: GPU-written render target -> staging. The clear colour is the
  // pat() value of pixel (0,0) so one verify() serves every stage; every other
  // pixel must therefore mismatch, so this stage reports zero= instead.
  {
    ID3D11Texture2D *rt = mktex(dev, D3D11_USAGE_DEFAULT, D3D11_BIND_RENDER_TARGET, 0, nullptr);
    ID3D11Texture2D *dst = mktex(dev, D3D11_USAGE_STAGING, 0, D3D11_CPU_ACCESS_READ, nullptr);
    if (rt && dst) {
      ID3D11RenderTargetView *rtv = nullptr;
      if (SUCCEEDED(dev->CreateRenderTargetView(rt, nullptr, &rtv))) {
        const float col[4] = { 0.25f, 0.5f, 0.75f, 1.0f };   // BGRA 0x40 0x80 0xBF 0xFF
        ctx->ClearRenderTargetView(rtv, col);
        ctx->CopyResource(dst, rt);
        ctx->Flush();
        D3D11_MAPPED_SUBRESOURCE m = {};
        if (SUCCEEDED(ctx->Map(dst, 0, D3D11_MAP_READ, 0, &m))) {
          const uint32_t px = *(const uint32_t *)m.pData;
          UINT zero = 0;
          for (UINT y = 0; y < H; ++y) {
            const uint32_t *row = (const uint32_t *)((const uint8_t *)m.pData + (size_t)y * m.RowPitch);
            for (UINT x = 0; x < W; ++x) if (row[x] == 0) ++zero;
          }
          ctx->Unmap(dst, 0);
          printf("4 CLEAR px0=0x%08x zero=%u/%u  %s\n", px, zero, W * H,
                 zero == 0 && px != 0 ? "PASS" : "FAIL");
          if (!(zero == 0 && px != 0)) ++fails;
        } else { printf("4 CLEAR Map(READ) failed\n"); ++fails; }
        rtv->Release();
      } else ++fails;
    } else ++fails;
    if (rt) rt->Release(); if (dst) dst->Release();
  }

  printf("TOTAL failures=%d\n", fails);
  ctx->Release(); dev->Release(); helios->Release(); factory->Release();
  return fails;
}
