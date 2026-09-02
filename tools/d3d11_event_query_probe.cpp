// D3D11_QUERY_EVENT semantics probe: after End(), poll GetData with and without an explicit Flush,
// and with DONOTFLUSH, logging every distinct HRESULT the runtime returns until S_OK or 2 s.
// A first-poll DXGI_ERROR_INVALID_CALL (0x887a0001) instead of S_FALSE is the defect this hunts.
#include <windows.h>
#include <d3d11.h>
#include <dxgi1_2.h>
#include <stdio.h>
#pragma comment(lib, "d3d11.lib")
#pragma comment(lib, "dxgi.lib")

static IDXGIAdapter1 *find_helios() {
  IDXGIFactory1 *f = nullptr;
  if (FAILED(CreateDXGIFactory1(__uuidof(IDXGIFactory1), (void **)&f))) return nullptr;
  IDXGIAdapter1 *a = nullptr;
  for (UINT i = 0; f->EnumAdapters1(i, &a) != DXGI_ERROR_NOT_FOUND; ++i) {
    DXGI_ADAPTER_DESC1 d = {}; a->GetDesc1(&d);
    if (d.VendorId == 0x1af4) { f->Release(); return a; }
    a->Release();
  }
  f->Release();
  return nullptr;
}

static void poll(ID3D11DeviceContext *ctx, ID3D11Query *q, UINT flags, const char *tag) {
  const DWORD t0 = GetTickCount();
  HRESULT last = 1; int polls = 0; DWORD ok_at = 0xFFFFFFFF;
  while (GetTickCount() - t0 < 2000) {
    BOOL done = FALSE;
    HRESULT hr = ctx->GetData(q, &done, sizeof(done), flags);
    ++polls;
    if (hr != last) { printf("  %-14s poll#%d t=%lums hr=0x%08x done=%d\n", tag, polls, (unsigned long)(GetTickCount() - t0), (unsigned)hr, (int)done); last = hr; }
    if (hr == S_OK) { ok_at = GetTickCount() - t0; break; }
    if (FAILED(hr)) break;
    Sleep(1);
  }
  printf("  %-14s => %s after %d polls (%lu ms)\n", tag, last == S_OK ? "SIGNALLED" : (FAILED(last) ? "ERROR" : "TIMEOUT"), polls, (unsigned long)(ok_at == 0xFFFFFFFF ? GetTickCount() - t0 : ok_at));
}

int main() {
  IDXGIAdapter1 *ad = find_helios();
  if (!ad) { printf("Helios adapter not found\n"); return 1; }
  ID3D11Device *dev = nullptr; ID3D11DeviceContext *ctx = nullptr; D3D_FEATURE_LEVEL fl;
  D3D_FEATURE_LEVEL want[] = { D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_10_0 };
  HRESULT hr = D3D11CreateDevice(ad, D3D_DRIVER_TYPE_UNKNOWN, nullptr, 0, want, 2, D3D11_SDK_VERSION, &dev, &fl, &ctx);
  printf("D3D11CreateDevice hr=0x%08x fl=0x%x\n", (unsigned)hr, (unsigned)fl);
  if (FAILED(hr)) return 1;
  // A little GPU work so the event has something to wait behind.
  D3D11_TEXTURE2D_DESC td = {}; td.Width = 256; td.Height = 256; td.MipLevels = 1; td.ArraySize = 1;
  td.Format = DXGI_FORMAT_B8G8R8A8_UNORM; td.SampleDesc.Count = 1; td.Usage = D3D11_USAGE_DEFAULT; td.BindFlags = D3D11_BIND_RENDER_TARGET;
  ID3D11Texture2D *rt = nullptr; ID3D11RenderTargetView *rtv = nullptr;
  if (SUCCEEDED(dev->CreateTexture2D(&td, nullptr, &rt))) dev->CreateRenderTargetView(rt, nullptr, &rtv);
  const float col[4] = { 0.f, 1.f, 0.f, 1.f };
  D3D11_QUERY_DESC qd = {}; qd.Query = D3D11_QUERY_EVENT;
  const char *variants[3] = { "flush", "noflush", "donotflush" };
  for (int round = 0; round < 2; ++round) {
    for (int v = 0; v < 3; ++v) {
      ID3D11Query *q = nullptr;
      if (FAILED(dev->CreateQuery(&qd, &q))) { printf("CreateQuery failed\n"); return 1; }
      if (rtv) ctx->ClearRenderTargetView(rtv, col);
      ctx->End(q);
      if (v == 0) ctx->Flush();
      printf("round %d variant %s:\n", round, variants[v]);
      poll(ctx, q, v == 2 ? D3D11_ASYNC_GETDATA_DONOTFLUSH : 0, variants[v]);
      if (v == 2) { // DONOTFLUSH may legitimately never complete; now flush and re-poll once
        ctx->Flush();
        poll(ctx, q, 0, "dnf+flush");
      }
      q->Release();
    }
  }
  printf("DONE\n");
  return 0;
}
