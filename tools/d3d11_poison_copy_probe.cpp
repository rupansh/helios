// Does the GPU write OUR memory, or memory we never map?
//
// 2026-08-29. d3d11_roundtrip_split_probe proves every GPU-routed read-back is
// exactly zero, but it cannot say WHY, because its destination staging texture
// is never written before the copy. "Reads zero" is therefore ambiguous between
// two very different defects:
//
//   (a) the GPU never wrote  -> we are reading our own untouched zero pages;
//   (b) the CPU maps a different buffer than the one the GPU wrote -> we are
//       reading somebody else's untouched zero pages.
//
// POISON the destination first and the ambiguity disappears. Fill it with
// 0xCDCDCDCD through Map(WRITE), confirm the poison reads back, and only then
// run the copy:
//
//   poison survives -> nothing wrote our memory        (a) or (b)
//   reads zero      -> something DID write our memory, with zeros. GPU->CPU
//                      visibility works and the HOST's view of the source was
//                      zero, i.e. CPU writes never reached the host.
//   reads pattern   -> the stage passes.
//
// The TIMESTAMP stage is the second, independent oracle: DXVK reads timestamps
// with vkGetQueryPoolResults (dxvk_gpu_query.cpp:96), a host CALL whose result
// comes back over the venus reply channel rather than through mapped memory. A
// moving timestamp is proof the host executed the command buffer that no map
// can give us.
//
// Session 0, no window, no DWM, no scanout. Build on the VM with mingw:
//   g++ -O1 -o C:\Users\Rupansh\d3d11_poison_copy_probe.exe ^
//       Z:\tools\d3d11_poison_copy_probe.cpp -ld3d11 -ldxgi -ldxguid
#include <d3d11.h>
#include <dxgi1_2.h>
#include <windows.h>
#include <cstdio>
#include <cstdint>
#include <cwchar>

static const UINT W = 64, H = 64;
static const uint32_t POISON = 0xCDCDCDCDu;

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

// Deterministic per-pixel pattern; never zero and never the poison value.
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
  if (FAILED(hr)) { printf("  CreateTexture2D usage=%d bind=0x%x cpu=0x%x hr=0x%08x\n",
                           (int)usage, bind, cpu, (unsigned)hr); return nullptr; }
  return t;
}

// Write `val` (or pat() when val==0) over every pixel of a mappable texture.
static bool write_tex(ID3D11DeviceContext *ctx, ID3D11Texture2D *t, uint32_t val, const char *tag) {
  D3D11_MAPPED_SUBRESOURCE m = {};
  HRESULT hr = ctx->Map(t, 0, D3D11_MAP_WRITE, 0, &m);
  if (FAILED(hr)) { printf("%-10s Map(WRITE) hr=0x%08x\n", tag, (unsigned)hr); return false; }
  for (UINT y = 0; y < H; ++y) {
    uint32_t *row = (uint32_t *)((uint8_t *)m.pData + (size_t)y * m.RowPitch);
    for (UINT x = 0; x < W; ++x) row[x] = val ? val : pat(x, y);
  }
  ctx->Unmap(t, 0);
  return true;
}

// The whole point of the probe: classify what came back, rather than asking a
// yes/no question whose "no" has three causes.
struct Census { UINT pattern, poison, zero, other; uint32_t first[4]; };

static bool census(ID3D11DeviceContext *ctx, ID3D11Texture2D *t, const char *tag, Census *out) {
  D3D11_MAPPED_SUBRESOURCE m = {};
  HRESULT hr = ctx->Map(t, 0, D3D11_MAP_READ, 0, &m);
  if (FAILED(hr)) { printf("%-10s Map(READ) hr=0x%08x FAIL\n", tag, (unsigned)hr); return false; }
  Census c = {};
  for (UINT y = 0; y < H; ++y) {
    const uint32_t *row = (const uint32_t *)((const uint8_t *)m.pData + (size_t)y * m.RowPitch);
    for (UINT x = 0; x < W; ++x) {
      const uint32_t v = row[x];
      if (v == pat(x, y))    ++c.pattern;
      else if (v == POISON)  ++c.poison;
      else if (v == 0)       ++c.zero;
      else                   ++c.other;
    }
  }
  for (int i = 0; i < 4; ++i) c.first[i] = ((const uint32_t *)m.pData)[i];
  ctx->Unmap(t, 0);

  const UINT total = W * H;
  const char *verdict =
      c.pattern == total ? "PASS  content arrived"
    : c.poison  == total ? "POISON INTACT  nothing wrote our memory"
    : c.zero    == total ? "ZEROED  our memory WAS written, with zeros"
    : c.other   == total ? "UNIFORM OTHER  see first[]"
    :                      "MIXED";
  printf("%-10s pat=%u poison=%u zero=%u other=%u  first=%08x %08x %08x %08x  %s\n",
         tag, c.pattern, c.poison, c.zero, c.other,
         c.first[0], c.first[1], c.first[2], c.first[3], verdict);
  if (out) *out = c;
  return c.pattern == total;
}

// Flush and block until the GPU says it is done, so a "poison intact" result
// cannot be a race. Returns the wait in ms; 2000 means it never signalled.
static DWORD settle(ID3D11Device *dev, ID3D11DeviceContext *ctx) {
  ID3D11Query *q = nullptr;
  D3D11_QUERY_DESC qd = {}; qd.Query = D3D11_QUERY_EVENT;
  if (FAILED(dev->CreateQuery(&qd, &q))) { ctx->Flush(); Sleep(500); return 0xffffffff; }
  ctx->End(q);
  ctx->Flush();
  BOOL done = FALSE;
  const DWORD t0 = GetTickCount();
  while (GetTickCount() - t0 < 2000) {
    if (ctx->GetData(q, &done, sizeof(done), 0) == S_OK) break;
    Sleep(1);
  }
  const DWORD spent = GetTickCount() - t0;
  q->Release();
  return spent;
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
  printf("D3D11CreateDevice hr=0x%08x fl=0x%x\n\n", (unsigned)hr, (unsigned)got);
  if (FAILED(hr)) return 1;

  int fails = 0;

  // A: the poison itself must survive a Map(WRITE)/Unmap/Map(READ) round trip,
  // or every later stage is measuring the poison rather than the copy.
  ID3D11Texture2D *ctl = mktex(dev, D3D11_USAGE_STAGING, 0,
                               D3D11_CPU_ACCESS_READ | D3D11_CPU_ACCESS_WRITE, nullptr);
  bool poison_works = false;
  if (ctl && write_tex(ctx, ctl, POISON, "A CONTROL")) {
    Census c = {};
    census(ctx, ctl, "A CONTROL", &c);
    poison_works = (c.poison == W * H);
    printf("           -> poison round-trips: %s\n\n", poison_works ? "YES" : "NO (later stages are void)");
  }
  if (ctl) ctl->Release();
  if (!poison_works) ++fails;

  // B: staging -> staging, no DEFAULT in the middle. The simplest possible GPU
  // round trip: both ends are CPU-visible and only the copy is on the GPU.
  {
    ID3D11Texture2D *src = mktex(dev, D3D11_USAGE_STAGING, 0, D3D11_CPU_ACCESS_WRITE, nullptr);
    ID3D11Texture2D *dst = mktex(dev, D3D11_USAGE_STAGING, 0,
                                 D3D11_CPU_ACCESS_READ | D3D11_CPU_ACCESS_WRITE, nullptr);
    if (src && dst && write_tex(ctx, src, 0, "B S2S.src") && write_tex(ctx, dst, POISON, "B S2S.dst")) {
      census(ctx, dst, "B pre", nullptr);
      ctx->CopyResource(dst, src);
      const DWORD ms = settle(dev, ctx);
      printf("B S2S      event settled in %lu ms\n", (unsigned long)ms);
      if (!census(ctx, dst, "B post", nullptr)) ++fails;
    } else ++fails;
    if (src) src->Release(); if (dst) dst->Release();
    printf("\n");
  }

  // C: staging -> DEFAULT -> staging, the shape stage 2 of the split probe uses.
  {
    ID3D11Texture2D *src = mktex(dev, D3D11_USAGE_STAGING, 0, D3D11_CPU_ACCESS_WRITE, nullptr);
    ID3D11Texture2D *mid = mktex(dev, D3D11_USAGE_DEFAULT, D3D11_BIND_SHADER_RESOURCE, 0, nullptr);
    ID3D11Texture2D *dst = mktex(dev, D3D11_USAGE_STAGING, 0,
                                 D3D11_CPU_ACCESS_READ | D3D11_CPU_ACCESS_WRITE, nullptr);
    if (src && mid && dst && write_tex(ctx, src, 0, "C SDS.src") && write_tex(ctx, dst, POISON, "C SDS.dst")) {
      ctx->CopyResource(mid, src);
      ctx->CopyResource(dst, mid);
      const DWORD ms = settle(dev, ctx);
      printf("C S2D2S    event settled in %lu ms\n", (unsigned long)ms);
      if (!census(ctx, dst, "C post", nullptr)) ++fails;
    } else ++fails;
    if (src) src->Release(); if (mid) mid->Release(); if (dst) dst->Release();
    printf("\n");
  }

  // D: a GPU-written render target -> poisoned staging, with a TIMESTAMP pair
  // around the work. The timestamps come back through vkGetQueryPoolResults, so
  // they say whether the host ran the command buffer even when the map does not.
  {
    ID3D11Texture2D *rt  = mktex(dev, D3D11_USAGE_DEFAULT, D3D11_BIND_RENDER_TARGET, 0, nullptr);
    ID3D11Texture2D *dst = mktex(dev, D3D11_USAGE_STAGING, 0,
                                 D3D11_CPU_ACCESS_READ | D3D11_CPU_ACCESS_WRITE, nullptr);
    ID3D11RenderTargetView *rtv = nullptr;
    if (rt && dst && SUCCEEDED(dev->CreateRenderTargetView(rt, nullptr, &rtv))
        && write_tex(ctx, dst, POISON, "D CLR.dst")) {
      ID3D11Query *dis = nullptr, *t0 = nullptr, *t1 = nullptr;
      D3D11_QUERY_DESC qd = {};
      qd.Query = D3D11_QUERY_TIMESTAMP_DISJOINT; dev->CreateQuery(&qd, &dis);
      qd.Query = D3D11_QUERY_TIMESTAMP;          dev->CreateQuery(&qd, &t0);
      qd.Query = D3D11_QUERY_TIMESTAMP;          dev->CreateQuery(&qd, &t1);

      if (dis) ctx->Begin(dis);
      if (t0) ctx->End(t0);
      const float col[4] = { 0.25f, 0.5f, 0.75f, 1.0f };  // BGRA bytes BF 80 40 FF
      ctx->ClearRenderTargetView(rtv, col);
      ctx->CopyResource(dst, rt);
      if (t1) ctx->End(t1);
      if (dis) ctx->End(dis);

      const DWORD ms = settle(dev, ctx);
      printf("D CLEAR    event settled in %lu ms\n", (unsigned long)ms);

      // Timestamps: the oracle that does not go through mapped memory.
      D3D11_QUERY_DATA_TIMESTAMP_DISJOINT dd = {};
      UINT64 a = 0, b = 0;
      const DWORD tw = GetTickCount();
      bool okd = false, oka = false, okb = false;
      while (GetTickCount() - tw < 2000) {
        if (!okd && dis) okd = ctx->GetData(dis, &dd, sizeof(dd), 0) == S_OK;
        if (!oka && t0)  oka = ctx->GetData(t0, &a, sizeof(a), 0) == S_OK;
        if (!okb && t1)  okb = ctx->GetData(t1, &b, sizeof(b), 0) == S_OK;
        if ((okd || !dis) && (oka || !t0) && (okb || !t1)) break;
        Sleep(1);
      }
      printf("D TIMESTAMP got(disjoint=%d,t0=%d,t1=%d) freq=%llu disjoint=%d t0=%llu t1=%llu delta=%lld  %s\n",
             (int)okd, (int)oka, (int)okb, (unsigned long long)dd.Frequency, (int)dd.Disjoint,
             (unsigned long long)a, (unsigned long long)b, (long long)(b - a),
             (oka && okb && b != a) ? "GPU EXECUTED (timestamps moved)"
                                    : "no timestamp evidence of execution");
      if (!census(ctx, dst, "D post", nullptr)) ++fails;

      if (dis) dis->Release(); if (t0) t0->Release(); if (t1) t1->Release();
      rtv->Release();
    } else ++fails;
    if (rt) rt->Release(); if (dst) dst->Release();
    printf("\n");
  }

  printf("TOTAL failures=%d\n", fails);
  ctx->Release(); dev->Release(); helios->Release(); factory->Release();
  return fails;
}
