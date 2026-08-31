// d3d11_xproc_shared_probe.cpp — does opening a shared resource from ANOTHER
// PROCESS orphan the creator, the way a second device in one process does?
//
// `d3d11_shared_variable_probe` arm 5 showed that `OpenSharedResource1` on a
// second device in the SAME process permanently orphans the creator's image: a
// fresh clear afterwards still reads zero. DWM's shape is different in one
// respect that matters — it opens across a process boundary, so the two devices
// have separate token spaces by construction rather than by accident. This runs
// the identical sequence with a real second process.
//
// parent: create shared+named RT, clear, read, spawn child, read, re-clear, read
// child : OpenSharedResourceByName, hold it, exit
//
// Build: g++ -O1 -o d3d11_xproc_shared_probe.exe d3d11_xproc_shared_probe.cpp
//            -ld3d11 -ldxgi -ldxguid
#include <windows.h>
#include <d3d11_4.h>
#include <dxgi1_6.h>
#include <cstdint>
#include <cstdio>
#include <cstring>

static const UINT W = 256, H = 256;
static const float CLEAR[4] = { 0.25f, 0.50f, 0.75f, 1.00f }; // -> 0x??407fbf
static const wchar_t *SHARE_NAME = L"helios_xproc_shared_probe";

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

static bool make_device(ID3D11Device **dev, ID3D11DeviceContext **ctx) {
  IDXGIFactory1 *f = nullptr;
  if (FAILED(CreateDXGIFactory1(__uuidof(IDXGIFactory1), (void **)&f))) return false;
  IDXGIAdapter1 *a = find_helios(f);
  if (!a) { printf("no Helios adapter\n"); return false; }
  D3D_FEATURE_LEVEL got{};
  const D3D_FEATURE_LEVEL want[] = { D3D_FEATURE_LEVEL_11_0 };
  return SUCCEEDED(D3D11CreateDevice(a, D3D_DRIVER_TYPE_UNKNOWN, nullptr, 0,
                                     want, 1, D3D11_SDK_VERSION, dev, &got, ctx));
}

static void read_centre(ID3D11Device *dev, ID3D11DeviceContext *ctx,
                        ID3D11Texture2D *tex, const char *label) {
  D3D11_TEXTURE2D_DESC sd{};
  tex->GetDesc(&sd);
  sd.MiscFlags = 0; sd.Usage = D3D11_USAGE_STAGING;
  sd.BindFlags = 0; sd.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
  ID3D11Texture2D *stg = nullptr;
  if (FAILED(dev->CreateTexture2D(&sd, nullptr, &stg))) { printf("%-28s staging failed\n", label); return; }
  ctx->CopyResource(stg, tex);
  ctx->Flush();
  D3D11_MAPPED_SUBRESOURCE m{};
  if (FAILED(ctx->Map(stg, 0, D3D11_MAP_READ, 0, &m))) { printf("%-28s map failed\n", label); stg->Release(); return; }
  const uint8_t *row = (const uint8_t *)m.pData + (size_t)(H / 2) * m.RowPitch;
  const uint32_t px = *(const uint32_t *)(row + (W / 2) * 4);
  ctx->Unmap(stg, 0); stg->Release();
  printf("%-28s centre=0x%08x  %s\n", label, px,
         (px & 0xFFFFFF) == 0x407FBF ? "PASS" : "FAIL");
  fflush(stdout);
}

int main(int argc, char **argv) {
  const bool child = argc > 1 && !strcmp(argv[1], "child");
  ID3D11Device *dev = nullptr; ID3D11DeviceContext *ctx = nullptr;
  if (!make_device(&dev, &ctx)) { printf("%s: device create failed\n", child ? "CHILD" : "PARENT"); return 1; }

  if (child) {
    ID3D11Device1 *dev1 = nullptr;
    if (FAILED(dev->QueryInterface(__uuidof(ID3D11Device1), (void **)&dev1))) return 2;
    ID3D11Texture2D *opened = nullptr;
    HRESULT hr = dev1->OpenSharedResourceByName(
      SHARE_NAME, DXGI_SHARED_RESOURCE_READ | DXGI_SHARED_RESOURCE_WRITE,
      __uuidof(ID3D11Texture2D), (void **)&opened);
    printf("CHILD OpenSharedResourceByName hr=0x%08lx opened=%p\n",
           (unsigned long)hr, (void *)opened);
    fflush(stdout);
    if (FAILED(hr) || !opened) return 3;
    // ⭐ Separate THE OPEN from THE OPENER'S FIRST USE. The child deliberately
    // does NOT touch the texture until after the parent has read at T+3s:
    //   * parent D passes -> the open alone is harmless, and the opener's first
    //     use is what discards (an UNDEFINED-layout transition).
    //   * parent D fails  -> the open itself re-materializes the memory.
    // Those two have completely different fixes.
    Sleep(5000);
    read_centre(dev, ctx, opened, "  CHILD read 1 (first touch, T+5)");
    Sleep(4000);          // spans the parent's re-clear at ~T+6s
    read_centre(dev, ctx, opened, "  CHILD read 2 (after parent re-clear)");
    return 0;
  }

  D3D11_TEXTURE2D_DESC td{};
  td.Width = W; td.Height = H; td.MipLevels = 1; td.ArraySize = 1;
  td.Format = DXGI_FORMAT_B8G8R8A8_UNORM;
  td.SampleDesc.Count = 1;
  td.Usage = D3D11_USAGE_DEFAULT;
  td.BindFlags = D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE;
  td.MiscFlags = D3D11_RESOURCE_MISC_SHARED | D3D11_RESOURCE_MISC_SHARED_NTHANDLE;
  ID3D11Texture2D *tex = nullptr;
  if (FAILED(dev->CreateTexture2D(&td, nullptr, &tex))) { printf("PARENT create failed\n"); return 1; }
  ID3D11RenderTargetView *rtv = nullptr;
  dev->CreateRenderTargetView(tex, nullptr, &rtv);

  ctx->ClearRenderTargetView(rtv, CLEAR);
  ctx->Flush();
  read_centre(dev, ctx, tex, "A parent, before share");

  IDXGIResource1 *res1 = nullptr;
  HANDLE handle = nullptr;
  if (SUCCEEDED(tex->QueryInterface(__uuidof(IDXGIResource1), (void **)&res1))) {
    HRESULT hr = res1->CreateSharedHandle(nullptr,
      DXGI_SHARED_RESOURCE_READ | DXGI_SHARED_RESOURCE_WRITE, SHARE_NAME, &handle);
    printf("B CreateSharedHandle(named) hr=0x%08lx handle=%p\n",
           (unsigned long)hr, handle);
    fflush(stdout);
  }
  read_centre(dev, ctx, tex, "C parent, after handle");

  wchar_t cmd[MAX_PATH * 2];
  wchar_t self[MAX_PATH];
  GetModuleFileNameW(nullptr, self, MAX_PATH);
  _snwprintf(cmd, MAX_PATH * 2, L"\"%s\" child", self);
  STARTUPINFOW si{}; si.cb = sizeof(si);
  PROCESS_INFORMATION pi{};
  if (!CreateProcessW(nullptr, cmd, nullptr, nullptr, TRUE, 0, nullptr, nullptr, &si, &pi)) {
    printf("CreateProcess failed %lu\n", GetLastError());
    return 1;
  }
  Sleep(3000);   // the child has OPENED but not yet touched it
  read_centre(dev, ctx, tex, "D parent, after xproc open (untouched)");
  ctx->ClearRenderTargetView(rtv, CLEAR);
  ctx->Flush();
  read_centre(dev, ctx, tex, "E parent, re-clear after open");
  WaitForSingleObject(pi.hProcess, 20000);
  return 0;
}
