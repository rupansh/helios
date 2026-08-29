// Force DXVK's host-visible pools through teardown so the KMD can read them.
//
// 2026-08-29. The KMD (22.22.387.0) samples a role-1 HVM1 backing store at
// DestroyAllocation and reports Nr2BsCd, the count containing 0xCDCDCDCD --
// a value only this process writes, and only through the D3DKMTLock2 view.
// But d3d11_poison_copy_probe destroys about one role-1 allocation per run and
// it is always a venus shmem, so the staging pool is never sampled.
//
// Churn fixes that -- but churning TEXTURES does not: DXVK recycles one pool
// for all of them (measured: 240 textures moved Nr2BsMap by 2). It frees its
// memory chunks when the DEVICE goes away, so the loop has to be over devices.
// Each device teardown frees its pool, and that pool is a role-1 allocation
// reaching DestroyAllocation with our bytes still in it.
//
// Every staging texture is poisoned, and half of them additionally receive a
// GPU ClearRenderTargetView + CopyResource whose colour is CLEARV. So the
// KMD's counters answer the question D3D11 cannot:
//
//   Nr2BsCd > 0                  the poison reached the pages the host imported,
//                                and the GPU did NOT overwrite them
//                                => the host never executed the copy
//   Nr2BsCd == 0, Val == CLEARV  the GPU DID write those pages and the guest's
//                                own read of them is stale
//                                => a coherency defect, not an execution one
//
//   g++ -O1 -o C:\Users\Rupansh\d3d11_pool_churn_probe.exe ^
//       Z:\tools\d3d11_pool_churn_probe.cpp -ld3d11 -ldxgi -ldxguid
#include <d3d11.h>
#include <dxgi1_2.h>
#include <windows.h>
#include <cstdio>
#include <cstdint>
#include <cwchar>

// 512x512 BGRA = 1 MiB, so a handful fills a 4 MiB pool and the next one forces
// a fresh chunk. Small textures would all fit in one pool and never churn.
static const UINT W = 512, H = 512;
static const uint32_t POISON = 0xCDCDCDCDu;
// ClearRenderTargetView(0.25, 0.5, 0.75, 1.0) on B8G8R8A8_UNORM.
static const uint32_t CLEARV = 0xFF4080BFu;
static const int ROUNDS = 24;
static const int TEX_PER_DEVICE = 6;

static ID3D11Texture2D *mktex(ID3D11Device *dev, D3D11_USAGE usage, UINT bind, UINT cpu) {
  D3D11_TEXTURE2D_DESC d = {};
  d.Width = W; d.Height = H; d.MipLevels = 1; d.ArraySize = 1;
  d.Format = DXGI_FORMAT_B8G8R8A8_UNORM;
  d.SampleDesc.Count = 1;
  d.Usage = usage; d.BindFlags = bind; d.CPUAccessFlags = cpu;
  ID3D11Texture2D *t = nullptr;
  if (FAILED(dev->CreateTexture2D(&d, nullptr, &t))) return nullptr;
  return t;
}

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

  int devices = 0, poisoned = 0, cleared = 0, survived = 0, overwritten = 0, other = 0;
  for (int r = 0; r < ROUNDS; ++r) {
    ID3D11Device *dev = nullptr; ID3D11DeviceContext *ctx = nullptr;
    D3D_FEATURE_LEVEL got = (D3D_FEATURE_LEVEL)0;
    if (FAILED(D3D11CreateDevice(h, D3D_DRIVER_TYPE_UNKNOWN, nullptr, 0, nullptr, 0,
                                 D3D11_SDK_VERSION, &dev, &got, &ctx)))
      continue;
    ++devices;

    for (int t = 0; t < TEX_PER_DEVICE; ++t) {
      ID3D11Texture2D *stage = mktex(dev, D3D11_USAGE_STAGING, 0,
                                     D3D11_CPU_ACCESS_READ | D3D11_CPU_ACCESS_WRITE);
      if (!stage) continue;

      D3D11_MAPPED_SUBRESOURCE m = {};
      if (SUCCEEDED(ctx->Map(stage, 0, D3D11_MAP_WRITE, 0, &m))) {
        for (UINT y = 0; y < H; ++y) {
          uint32_t *row = (uint32_t *)((uint8_t *)m.pData + (size_t)y * m.RowPitch);
          for (UINT x = 0; x < W; ++x) row[x] = POISON;
        }
        ctx->Unmap(stage, 0);
        ++poisoned;
      }

      if (t & 1) {
        ID3D11Texture2D *rt = mktex(dev, D3D11_USAGE_DEFAULT, D3D11_BIND_RENDER_TARGET, 0);
        if (rt) {
          ID3D11RenderTargetView *rtv = nullptr;
          if (SUCCEEDED(dev->CreateRenderTargetView(rt, nullptr, &rtv))) {
            const float col[4] = { 0.25f, 0.5f, 0.75f, 1.0f };
            ctx->ClearRenderTargetView(rtv, col);
            ctx->CopyResource(stage, rt);
            ctx->Flush();
            ++cleared;
            rtv->Release();
          }
          rt->Release();
        }
        if (SUCCEEDED(ctx->Map(stage, 0, D3D11_MAP_READ, 0, &m))) {
          const uint32_t v = *(const uint32_t *)m.pData;
          if (v == POISON)      ++survived;
          else if (v == CLEARV) ++overwritten;
          else                  ++other;
          ctx->Unmap(stage, 0);
        }
      }
      stage->Release();
    }

    ctx->Release();
    dev->Release();
  }

  printf("devices=%d poisoned=%d cleared=%d\n", devices, poisoned, cleared);
  printf("guest view after GPU clear+copy: poison_survived=%d clear_value=%d other=%d\n",
         survived, overwritten, other);
  printf("now read Nr2BsCd / Nr2BsVal.\n");

  h->Release(); f->Release();
  return 0;
}
