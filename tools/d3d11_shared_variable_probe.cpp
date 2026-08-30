// d3d11_shared_variable_probe.cpp — is D3D11_RESOURCE_MISC_SHARED the ONLY
// variable behind the zero readback?
//
// `d3d11_shared_content_probe` clears a SHARED 256x256 BGRA render target and
// reads back zero, while `d3d11_roundtrip_split_probe` stage 4 clears a
// NON-shared 64x64 target and reads back the correct colour. Those two differ
// in size, format path and readback helper as well as in sharing, so neither
// isolates MISC_SHARED. This clears the SAME texture twice in ONE process,
// changing only MiscFlags, and reads both back the same way.
//
// Build: g++ -O1 -o d3d11_shared_variable_probe.exe d3d11_shared_variable_probe.cpp
//            -ld3d11 -ldxgi -ldxguid
#include <d3d11_4.h>
#include <dxgi1_6.h>
#include <cstdint>
#include <cstdio>

static const UINT W = 256, H = 256;
static const float CLEAR[4] = { 0.25f, 0.50f, 0.75f, 1.00f }; // -> 0x??407fbf

static IDXGIAdapter1 *find_helios(IDXGIFactory1 *f) {
  IDXGIAdapter1 *a = nullptr;
  for (UINT i = 0; f->EnumAdapters1(i, &a) != DXGI_ERROR_NOT_FOUND; ++i) {
    DXGI_ADAPTER_DESC1 d{};
    a->GetDesc1(&d);
    if (d.VendorId == 0x1af4) return a;
    a->Release();
  }
  return nullptr;
}

// Clear a render target created with `misc`, copy to staging, read the centre.
static void arm(ID3D11Device *dev, ID3D11DeviceContext *ctx, UINT misc,
                const char *label) {
  D3D11_TEXTURE2D_DESC td{};
  td.Width = W; td.Height = H; td.MipLevels = 1; td.ArraySize = 1;
  td.Format = DXGI_FORMAT_B8G8R8A8_UNORM;
  td.SampleDesc.Count = 1;
  td.Usage = D3D11_USAGE_DEFAULT;
  td.BindFlags = D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE;
  td.MiscFlags = misc;
  ID3D11Texture2D *tex = nullptr;
  HRESULT hr = dev->CreateTexture2D(&td, nullptr, &tex);
  if (FAILED(hr)) { printf("%-22s CreateTexture2D hr=0x%08lx\n", label, (unsigned long)hr); return; }

  ID3D11RenderTargetView *rtv = nullptr;
  hr = dev->CreateRenderTargetView(tex, nullptr, &rtv);
  if (FAILED(hr)) { printf("%-22s CreateRTV hr=0x%08lx\n", label, (unsigned long)hr); return; }
  ctx->ClearRenderTargetView(rtv, CLEAR);
  ctx->Flush();

  D3D11_TEXTURE2D_DESC sd = td;
  sd.MiscFlags = 0;
  sd.Usage = D3D11_USAGE_STAGING;
  sd.BindFlags = 0;
  sd.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
  ID3D11Texture2D *stg = nullptr;
  hr = dev->CreateTexture2D(&sd, nullptr, &stg);
  if (FAILED(hr)) { printf("%-22s staging hr=0x%08lx\n", label, (unsigned long)hr); return; }
  ctx->CopyResource(stg, tex);
  ctx->Flush();

  D3D11_MAPPED_SUBRESOURCE m{};
  hr = ctx->Map(stg, 0, D3D11_MAP_READ, 0, &m);
  if (FAILED(hr)) { printf("%-22s map hr=0x%08lx\n", label, (unsigned long)hr); return; }
  const uint8_t *row = (const uint8_t *)m.pData + (size_t)(H / 2) * m.RowPitch;
  const uint32_t px = *(const uint32_t *)(row + (W / 2) * 4);
  unsigned nonzero = 0;
  for (UINT y = 0; y < H; ++y) {
    const uint32_t *r = (const uint32_t *)((const uint8_t *)m.pData + (size_t)y * m.RowPitch);
    for (UINT x = 0; x < W; ++x) if (r[x]) ++nonzero;
  }
  ctx->Unmap(stg, 0);
  printf("%-22s misc=0x%02x centre=0x%08x nonzero=%u/%u  %s\n", label, misc, px,
         nonzero, W * H, (px & 0xFFFFFF) == 0x407FBF ? "PASS" : "FAIL");
  stg->Release(); rtv->Release(); tex->Release();
}

int main() {
  IDXGIFactory1 *f = nullptr;
  if (FAILED(CreateDXGIFactory1(__uuidof(IDXGIFactory1), (void **)&f))) return 1;
  IDXGIAdapter1 *a = find_helios(f);
  if (!a) { printf("no Helios adapter\n"); return 1; }
  ID3D11Device *dev = nullptr; ID3D11DeviceContext *ctx = nullptr;
  D3D_FEATURE_LEVEL got{};
  const D3D_FEATURE_LEVEL want[] = { D3D_FEATURE_LEVEL_11_0 };
  if (FAILED(D3D11CreateDevice(a, D3D_DRIVER_TYPE_UNKNOWN, nullptr, 0, want, 1,
                               D3D11_SDK_VERSION, &dev, &got, &ctx))) return 1;
  // Same device, same size, same format, same clear, same readback: the ONLY
  // difference across these three is MiscFlags.
  arm(dev, ctx, 0, "1 plain");
  arm(dev, ctx, D3D11_RESOURCE_MISC_SHARED, "2 SHARED");
  arm(dev, ctx, D3D11_RESOURCE_MISC_SHARED | D3D11_RESOURCE_MISC_SHARED_NTHANDLE,
      "3 SHARED_NTHANDLE");
  arm(dev, ctx, 0, "4 plain again");

  // ── 5: does OPENING the shared resource destroy the creator's content? ────
  // `d3d11_shared_content_probe` clears, then CreateSharedHandle +
  // OpenSharedResource1, and only then reads back — and reads zero. Arms 2/3
  // above show the clear itself survives on a shared texture, so the open is
  // the only remaining step between the two.
  D3D11_TEXTURE2D_DESC td{};
  td.Width = W; td.Height = H; td.MipLevels = 1; td.ArraySize = 1;
  td.Format = DXGI_FORMAT_B8G8R8A8_UNORM;
  td.SampleDesc.Count = 1;
  td.Usage = D3D11_USAGE_DEFAULT;
  td.BindFlags = D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE;
  td.MiscFlags = D3D11_RESOURCE_MISC_SHARED | D3D11_RESOURCE_MISC_SHARED_NTHANDLE;
  ID3D11Texture2D *tex = nullptr;
  if (FAILED(dev->CreateTexture2D(&td, nullptr, &tex))) { printf("5 create failed\n"); return 1; }
  ID3D11RenderTargetView *rtv = nullptr;
  dev->CreateRenderTargetView(tex, nullptr, &rtv);
  ctx->ClearRenderTargetView(rtv, CLEAR);
  ctx->Flush();

  auto read_centre = [&](const char *label) {
    D3D11_TEXTURE2D_DESC sd = td;
    sd.MiscFlags = 0; sd.Usage = D3D11_USAGE_STAGING;
    sd.BindFlags = 0; sd.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
    ID3D11Texture2D *stg = nullptr;
    if (FAILED(dev->CreateTexture2D(&sd, nullptr, &stg))) { printf("%-22s staging failed\n", label); return; }
    ctx->CopyResource(stg, tex);
    ctx->Flush();
    D3D11_MAPPED_SUBRESOURCE m{};
    if (FAILED(ctx->Map(stg, 0, D3D11_MAP_READ, 0, &m))) { printf("%-22s map failed\n", label); stg->Release(); return; }
    const uint8_t *row = (const uint8_t *)m.pData + (size_t)(H / 2) * m.RowPitch;
    const uint32_t px = *(const uint32_t *)(row + (W / 2) * 4);
    ctx->Unmap(stg, 0);
    stg->Release();
    printf("%-22s centre=0x%08x  %s\n", label, px,
           (px & 0xFFFFFF) == 0x407FBF ? "PASS" : "FAIL");
  };

  read_centre("5a before open");
  IDXGIResource1 *res1 = nullptr;
  HANDLE handle = nullptr;
  if (SUCCEEDED(tex->QueryInterface(__uuidof(IDXGIResource1), (void **)&res1))) {
    HRESULT hr = res1->CreateSharedHandle(nullptr,
      DXGI_SHARED_RESOURCE_READ | DXGI_SHARED_RESOURCE_WRITE, nullptr, &handle);
    printf("5b CreateSharedHandle hr=0x%08lx handle=%p\n", (unsigned long)hr, handle);
  }
  read_centre("5c after handle");
  if (handle) {
    ID3D11Device1 *dev1 = nullptr;
    if (SUCCEEDED(dev->QueryInterface(__uuidof(ID3D11Device1), (void **)&dev1))) {
      ID3D11Texture2D *opened = nullptr;
      HRESULT hr = dev1->OpenSharedResource1(handle, __uuidof(ID3D11Texture2D), (void **)&opened);
      printf("5d OpenSharedResource1 hr=0x%08lx\n", (unsigned long)hr);
    }
  }
  read_centre("5e after open");

  // Which mechanism? Re-clear the ORIGINAL after the open and read it back.
  //  * reads the colour  -> the memory is still ours; the open only DISCARDED
  //    contents (an UNDEFINED-layout transition on the opened image).
  //  * still reads zero  -> the open RE-MATERIALIZED the allocation and the
  //    original image's binding is dead. Different fix entirely.
  ctx->ClearRenderTargetView(rtv, CLEAR);
  ctx->Flush();
  read_centre("5f original re-clear");
  return 0;
}
