// Is DWM's COMPOSED desktop black? Read it through DXGI, not GDI.
//
// 2026-08-31: every "the desktop is black" datum so far came from GDI --
// Graphics.CopyFromScreen and PrintWindow(Progman). On a fully flip-composited
// WDDM path GDI can legitimately read black without the desktop being black,
// so those captures cannot separate "DWM composes nothing" from "GDI cannot
// see what DWM composed". Desktop Duplication returns DWM's own composition
// surface over the D3D path, which can.
//
// MUST run in SESSION 1 (an interactive-token scheduled task). From session 0
// DuplicateOutput returns E_ACCESSDENIED and says nothing about the desktop.
//
// Build on the VM (mingw is on PATH):
//   g++ -O1 -o C:\Users\Rupansh\desktop_duplication_probe.exe ^
//       Z:\tools\desktop_duplication_probe.cpp -ld3d11 -ldxgi -ldxguid
#include <d3d11.h>
#include <dxgi1_2.h>
#include <windows.h>
#include <cstdio>
#include <cstdint>
#include <cwchar>

static const char *kOut = "Z:\\tmp\\desktop_duplication.bmp";

static void write_bmp(const uint8_t *bgra, UINT w, UINT h, UINT pitch) {
  FILE *f = fopen(kOut, "wb");
  if (!f) { printf("bmp: fopen failed\n"); return; }
  const uint32_t pixels = w * h * 4u;
  const uint32_t off = 14u + 40u;
  uint8_t hdr[54] = {};
  hdr[0] = 'B'; hdr[1] = 'M';
  *(uint32_t *)(hdr + 2) = off + pixels;
  *(uint32_t *)(hdr + 10) = off;
  *(uint32_t *)(hdr + 14) = 40;
  *(int32_t *)(hdr + 18) = (int32_t)w;
  *(int32_t *)(hdr + 22) = -(int32_t)h;   // top-down
  *(uint16_t *)(hdr + 26) = 1;
  *(uint16_t *)(hdr + 28) = 32;
  *(uint32_t *)(hdr + 34) = pixels;
  fwrite(hdr, 1, sizeof(hdr), f);
  for (UINT y = 0; y < h; ++y)
    fwrite(bgra + (size_t)y * pitch, 1, (size_t)w * 4u, f);
  fclose(f);
  printf("bmp: wrote %s (%ux%u)\n", kOut, w, h);
}

int main() {
  IDXGIFactory1 *factory = nullptr;
  if (FAILED(CreateDXGIFactory1(IID_IDXGIFactory1, (void **)&factory))) {
    printf("no factory\n"); return 1;
  }

  // The output must be enumerated from the SAME adapter the device is on, or
  // DuplicateOutput returns DXGI_ERROR_UNSUPPORTED.
  IDXGIAdapter1 *adapter = nullptr;
  IDXGIOutput *output = nullptr;
  for (UINT i = 0; !output && factory->EnumAdapters1(i, &adapter) != DXGI_ERROR_NOT_FOUND; ++i) {
    DXGI_ADAPTER_DESC1 ad = {};
    adapter->GetDesc1(&ad);
    IDXGIOutput *o = nullptr;
    for (UINT j = 0; adapter->EnumOutputs(j, &o) != DXGI_ERROR_NOT_FOUND; ++j) {
      DXGI_OUTPUT_DESC od = {};
      o->GetDesc(&od);
      wprintf(L"adapter[%u] \"%s\" output[%u] \"%s\" attached=%d\n",
              i, ad.Description, j, od.DeviceName, (int)od.AttachedToDesktop);
      if (od.AttachedToDesktop && !output) { output = o; break; }
      o->Release();
    }
    if (output) break;
    adapter->Release();
    adapter = nullptr;
  }
  if (!output) { printf("no output attached to desktop\n"); return 1; }

  ID3D11Device *dev = nullptr; ID3D11DeviceContext *ctx = nullptr;
  HRESULT hr = D3D11CreateDevice(adapter, D3D_DRIVER_TYPE_UNKNOWN, nullptr, 0,
                                 nullptr, 0, D3D11_SDK_VERSION, &dev, nullptr, &ctx);
  printf("D3D11CreateDevice hr=0x%08x\n", (unsigned)hr);
  if (FAILED(hr)) return 1;

  IDXGIOutput1 *output1 = nullptr;
  if (FAILED(output->QueryInterface(IID_PPV_ARGS(&output1)))) {
    printf("no IDXGIOutput1\n"); return 1;
  }
  IDXGIOutputDuplication *dup = nullptr;
  hr = output1->DuplicateOutput(dev, &dup);
  printf("DuplicateOutput hr=0x%08x%s\n", (unsigned)hr,
         hr == E_ACCESSDENIED ? "  (E_ACCESSDENIED: not in session 1?)" : "");
  if (FAILED(hr)) return 1;

  // Nothing animates on an idle desktop, so force damage or AcquireNextFrame
  // just times out with no frame to read.
  POINT p = {}; GetCursorPos(&p);

  ID3D11Texture2D *frame = nullptr;
  for (int attempt = 0; attempt < 40 && !frame; ++attempt) {
    SetCursorPos(p.x + (attempt & 1 ? 3 : -3), p.y + (attempt & 2 ? 3 : -3));

    DXGI_OUTDUPL_FRAME_INFO info = {};
    IDXGIResource *res = nullptr;
    hr = dup->AcquireNextFrame(500, &info, &res);
    if (hr == DXGI_ERROR_WAIT_TIMEOUT) continue;
    if (FAILED(hr)) { printf("AcquireNextFrame hr=0x%08x\n", (unsigned)hr); return 1; }
    // A frame with no accumulated desktop update carries the previous image;
    // that is still DWM's composition, so it is fine to read either way.
    res->QueryInterface(IID_PPV_ARGS(&frame));
    res->Release();
    if (!frame) dup->ReleaseFrame();
  }
  SetCursorPos(p.x, p.y);
  if (!frame) { printf("no frame acquired in 40 attempts\n"); return 1; }

  D3D11_TEXTURE2D_DESC fd = {};
  frame->GetDesc(&fd);
  printf("frame %ux%u fmt=%u\n", fd.Width, fd.Height, (unsigned)fd.Format);

  D3D11_TEXTURE2D_DESC sd = fd;
  sd.Usage = D3D11_USAGE_STAGING;
  sd.BindFlags = 0;
  sd.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
  sd.MiscFlags = 0;
  ID3D11Texture2D *stage = nullptr;
  hr = dev->CreateTexture2D(&sd, nullptr, &stage);
  if (FAILED(hr)) { printf("staging hr=0x%08x\n", (unsigned)hr); return 1; }

  ctx->CopyResource(stage, frame);
  ctx->Flush();

  D3D11_MAPPED_SUBRESOURCE m = {};
  hr = ctx->Map(stage, 0, D3D11_MAP_READ, 0, &m);
  if (FAILED(hr)) { printf("Map hr=0x%08x\n", (unsigned)hr); return 1; }

  // "Is it black" is the whole question, so report the distribution, not a
  // verdict: a uniform non-black desktop and a real one are different states.
  size_t nonzero = 0, total = 0;
  uint32_t top[8] = {}; size_t topn[8] = {};
  for (UINT y = 0; y < fd.Height; y += 2) {
    const uint32_t *row = (const uint32_t *)((const uint8_t *)m.pData + (size_t)y * m.RowPitch);
    for (UINT x = 0; x < fd.Width; x += 2) {
      const uint32_t px = row[x] & 0x00ffffffu;
      ++total;
      if (px) ++nonzero;
      int slot = -1;
      for (int i = 0; i < 8; ++i) { if (topn[i] && top[i] == px) { slot = i; break; } }
      if (slot < 0) for (int i = 0; i < 8; ++i) { if (!topn[i]) { top[i] = px; slot = i; break; } }
      if (slot >= 0) ++topn[slot];
    }
  }
  printf("sampled=%zu nonzero=%zu (%.2f%%)\n", total, nonzero,
         total ? 100.0 * (double)nonzero / (double)total : 0.0);
  for (int i = 0; i < 8; ++i)
    if (topn[i]) printf("  colour 0x%06x  %zu\n", top[i], topn[i]);

  write_bmp((const uint8_t *)m.pData, fd.Width, fd.Height, m.RowPitch);
  ctx->Unmap(stage, 0);
  printf("VERDICT: %s\n", nonzero ? "DWM COMPOSES CONTENT" : "COMPOSED DESKTOP IS BLACK");
  return 0;
}
