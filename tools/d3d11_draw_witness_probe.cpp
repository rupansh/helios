// d3d11_draw_witness_probe.cpp — does a real pipeline DRAW land, on a single
// device, with NO sharing and NO display?
//
// 2026-08-31 fresh derivation. The roundtrip_split probe passes all four stages
// (CPU / upload-copy / init-copy / clear-copy) on the current guest-backed
// build, yet the desktop and a live-drawing d3d11_triangle are black. Every
// PASSING stage is a clear or a copy — none is a shader draw. DWM composition
// and the triangle both depend on VS->PS->RT rasterization (textured for DWM).
// This isolates that path from sharing and from the display:
//
//   A CLEAR   DEFAULT RT, ClearRenderTargetView(green) -> copy -> read.
//             Control: must PASS (same as roundtrip stage 4). Proves the
//             guest-backed staging readback works this process/boot.
//   B DRAW    DEFAULT RT, clear RED, then a fullscreen-triangle draw whose PS
//             returns constant GREEN -> copy -> read. Tests a solid draw.
//   C TEXDRAW DEFAULT RT, clear RED, fullscreen-triangle draw whose PS SAMPLES
//             a 4x4 GREEN SRV -> copy -> read. Tests the composition shape.
//
// No vertex buffer / input layout: the VS emits the fullscreen triangle from
// SV_VertexID, so IA vertex-format questions are removed. Draw(3,0).
//
// center pixel GREEN (0xff00ff00) = the op landed; RED (0xff0000ff) = the draw
// produced nothing and only the clear shows; 0 = neither (dead RT).
//
// Build on the VM (mingw on PATH):
//   g++ -O1 -o C:\Users\Rupansh\d3d11_draw_witness_probe.exe ^
//       Z:\tools\d3d11_draw_witness_probe.cpp -ld3d11 -ldxgi -ldxguid -ld3dcompiler
#include <d3d11.h>
#include <dxgi1_2.h>
#include <d3dcompiler.h>
#include <windows.h>
#include <cstdio>
#include <cstdint>
#include <cstring>
#include <cwchar>

static const UINT W = 64, H = 64;
static const uint32_t GREEN = 0xff00ff00u;  // BGRA: B=0 G=ff R=0 A=ff
static const uint32_t RED   = 0xff0000ffu;  // BGRA: B=0 G=0  R=ff A=ff

static IDXGIAdapter1 *find_helios(IDXGIFactory1 *f) {
  IDXGIAdapter1 *a = nullptr;
  for (UINT i = 0; f->EnumAdapters1(i, &a) != DXGI_ERROR_NOT_FOUND; ++i) {
    DXGI_ADAPTER_DESC1 d = {};
    a->GetDesc1(&d);
    if (wcsstr(d.Description, L"Helios")) return a;
    a->Release(); a = nullptr;
  }
  return nullptr;
}

static ID3DBlob *compile(const char *src, const char *entry, const char *tgt) {
  ID3DBlob *code = nullptr, *err = nullptr;
  HRESULT hr = D3DCompile(src, strlen(src), nullptr, nullptr, nullptr, entry, tgt,
                          0, 0, &code, &err);
  if (FAILED(hr)) {
    printf("  compile %s hr=0x%08x %s\n", entry, (unsigned)hr,
           err ? (const char *)err->GetBufferPointer() : "");
    if (err) err->Release();
    return nullptr;
  }
  if (err) err->Release();
  return code;
}

static ID3D11Texture2D *mktex(ID3D11Device *dev, D3D11_USAGE usage, UINT bind,
                              UINT cpu, const D3D11_SUBRESOURCE_DATA *init) {
  D3D11_TEXTURE2D_DESC d = {};
  d.Width = W; d.Height = H; d.MipLevels = 1; d.ArraySize = 1;
  d.Format = DXGI_FORMAT_B8G8R8A8_UNORM; d.SampleDesc.Count = 1;
  d.Usage = usage; d.BindFlags = bind; d.CPUAccessFlags = cpu;
  ID3D11Texture2D *t = nullptr;
  HRESULT hr = dev->CreateTexture2D(&d, init, &t);
  if (FAILED(hr)) { printf("  CreateTexture2D usage=%d bind=0x%x hr=0x%08x\n", (int)usage, bind, (unsigned)hr); return nullptr; }
  return t;
}

// Copy `rt` to a fresh staging, read center + corner, count nonzero pixels.
static void readback(ID3D11Device *dev, ID3D11DeviceContext *ctx,
                     ID3D11Texture2D *rt, const char *tag) {
  ID3D11Texture2D *st = mktex(dev, D3D11_USAGE_STAGING, 0, D3D11_CPU_ACCESS_READ, nullptr);
  if (!st) { printf("%-9s no staging\n", tag); return; }
  ctx->CopyResource(st, rt);
  ctx->Flush();
  Sleep(300);
  D3D11_MAPPED_SUBRESOURCE m = {};
  HRESULT hr = ctx->Map(st, 0, D3D11_MAP_READ, 0, &m);
  if (FAILED(hr)) { printf("%-9s Map hr=0x%08x\n", tag, (unsigned)hr); st->Release(); return; }
  const uint8_t *base = (const uint8_t *)m.pData;
  auto px = [&](UINT x, UINT y) {
    return *(const uint32_t *)(base + (size_t)y * m.RowPitch + (size_t)x * 4);
  };
  UINT nz = 0, green = 0, red = 0;
  for (UINT y = 0; y < H; ++y)
    for (UINT x = 0; x < W; ++x) {
      uint32_t p = px(x, y);
      if (p) ++nz;
      if (p == GREEN) ++green;
      if (p == RED) ++red;
    }
  uint32_t c = px(W / 2, H / 2), corner = px(0, 0);
  const char *verdict = (c == GREEN) ? "LANDED(green)" : (c == RED) ? "cleared-only(red)" : (c == 0) ? "DEAD(zero)" : "other";
  printf("%-9s center=0x%08x corner=0x%08x nz=%u/%u green=%u red=%u  %s\n",
         tag, c, corner, nz, W * H, green, red, verdict);
  ctx->Unmap(st, 0);
  st->Release();
}

int main() {
  IDXGIFactory1 *factory = nullptr;
  if (FAILED(CreateDXGIFactory1(IID_IDXGIFactory1, (void **)&factory))) { printf("no factory\n"); return 1; }
  IDXGIAdapter1 *helios = find_helios(factory);
  if (!helios) { printf("Helios adapter not found\n"); return 1; }

  ID3D11Device *dev = nullptr; ID3D11DeviceContext *ctx = nullptr;
  D3D_FEATURE_LEVEL got = (D3D_FEATURE_LEVEL)0;
  const D3D_FEATURE_LEVEL levels[] = {D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0};
  HRESULT hr = D3D11CreateDevice(helios, D3D_DRIVER_TYPE_UNKNOWN, nullptr, 0,
                                 levels, 2, D3D11_SDK_VERSION, &dev, &got, &ctx);
  printf("D3D11CreateDevice hr=0x%08x fl=0x%x\n", (unsigned)hr, (unsigned)got);
  if (FAILED(hr)) return 1;

  // Fullscreen-triangle VS from SV_VertexID (no vertex buffer).
  static const char *kVsFs =
      "struct VOut { float4 pos:SV_Position; float2 uv:TEXCOORD0; };"
      "VOut main(uint id:SV_VertexID){ VOut o;"
      "  float2 p = float2((id<<1)&2, id&2);"          // (0,0)(2,0)(0,2)
      "  o.uv = p;"
      "  o.pos = float4(p*float2(2,-2)+float2(-1,1), 0, 1);"
      "  return o; }";
  static const char *kPsGreen =
      "float4 main(float4 pos:SV_Position, float2 uv:TEXCOORD0):SV_Target {"
      "  return float4(0,1,0,1); }";
  static const char *kPsTex =
      "Texture2D t:register(t0); SamplerState s:register(s0);"
      "float4 main(float4 pos:SV_Position, float2 uv:TEXCOORD0):SV_Target {"
      "  return t.Sample(s, uv); }";

  ID3DBlob *vsb = compile(kVsFs, "main", "vs_5_0");
  ID3DBlob *psg = compile(kPsGreen, "main", "ps_5_0");
  ID3DBlob *pst = compile(kPsTex, "main", "ps_5_0");
  if (!vsb || !psg || !pst) return 1;

  ID3D11VertexShader *vs = nullptr; ID3D11PixelShader *psGreen = nullptr, *psTex = nullptr;
  dev->CreateVertexShader(vsb->GetBufferPointer(), vsb->GetBufferSize(), nullptr, &vs);
  dev->CreatePixelShader(psg->GetBufferPointer(), psg->GetBufferSize(), nullptr, &psGreen);
  dev->CreatePixelShader(pst->GetBufferPointer(), pst->GetBufferSize(), nullptr, &psTex);

  // A green 4x4 SRV texture (init data — the roundtrip INIT stage proves this).
  ID3D11ShaderResourceView *srv = nullptr;
  {
    uint32_t px[16]; for (int i = 0; i < 16; ++i) px[i] = GREEN;
    D3D11_TEXTURE2D_DESC d = {}; d.Width = 4; d.Height = 4; d.MipLevels = 1; d.ArraySize = 1;
    d.Format = DXGI_FORMAT_B8G8R8A8_UNORM; d.SampleDesc.Count = 1;
    d.Usage = D3D11_USAGE_DEFAULT; d.BindFlags = D3D11_BIND_SHADER_RESOURCE;
    D3D11_SUBRESOURCE_DATA sd = {}; sd.pSysMem = px; sd.SysMemPitch = 16;
    ID3D11Texture2D *tx = nullptr;
    if (SUCCEEDED(dev->CreateTexture2D(&d, &sd, &tx))) { dev->CreateShaderResourceView(tx, nullptr, &srv); tx->Release(); }
  }
  ID3D11SamplerState *samp = nullptr;
  { D3D11_SAMPLER_DESC s = {}; s.Filter = D3D11_FILTER_MIN_MAG_MIP_POINT;
    s.AddressU = s.AddressV = s.AddressW = D3D11_TEXTURE_ADDRESS_CLAMP; dev->CreateSamplerState(&s, &samp); }

  D3D11_VIEWPORT vp = {}; vp.Width = (float)W; vp.Height = (float)H; vp.MaxDepth = 1.0f;

  auto mkrt = [&](void) -> ID3D11Texture2D * {
    return mktex(dev, D3D11_USAGE_DEFAULT, D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE, 0, nullptr);
  };
  const float green4[4] = {0, 1, 0, 1};
  const float red4[4]   = {1, 0, 0, 1};

  // A CLEAR (control)
  if (ID3D11Texture2D *rt = mkrt()) {
    ID3D11RenderTargetView *rtv = nullptr; dev->CreateRenderTargetView(rt, nullptr, &rtv);
    ctx->ClearRenderTargetView(rtv, green4);
    ctx->Flush();
    readback(dev, ctx, rt, "A CLEAR");
    if (rtv) rtv->Release(); rt->Release();
  }

  // B DRAW (solid green PS over red clear)
  if (ID3D11Texture2D *rt = mkrt()) {
    ID3D11RenderTargetView *rtv = nullptr; dev->CreateRenderTargetView(rt, nullptr, &rtv);
    ctx->ClearRenderTargetView(rtv, red4);
    ctx->OMSetRenderTargets(1, &rtv, nullptr);
    ctx->RSSetViewports(1, &vp);
    ctx->IASetInputLayout(nullptr);
    ctx->IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
    ctx->VSSetShader(vs, nullptr, 0);
    ctx->PSSetShader(psGreen, nullptr, 0);
    ctx->Draw(3, 0);
    ctx->Flush();
    readback(dev, ctx, rt, "B DRAW");
    if (rtv) rtv->Release(); rt->Release();
  }

  // C TEXDRAW (sample green SRV over red clear)
  if (ID3D11Texture2D *rt = mkrt()) {
    ID3D11RenderTargetView *rtv = nullptr; dev->CreateRenderTargetView(rt, nullptr, &rtv);
    ctx->ClearRenderTargetView(rtv, red4);
    ctx->OMSetRenderTargets(1, &rtv, nullptr);
    ctx->RSSetViewports(1, &vp);
    ctx->IASetInputLayout(nullptr);
    ctx->IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
    ctx->VSSetShader(vs, nullptr, 0);
    ctx->PSSetShader(psTex, nullptr, 0);
    ctx->PSSetShaderResources(0, 1, &srv);
    ctx->PSSetSamplers(0, 1, &samp);
    ctx->Draw(3, 0);
    ctx->Flush();
    readback(dev, ctx, rt, "C TEXDRAW");
    if (rtv) rtv->Release(); rt->Release();
  }

  printf("done\n");
  return 0;
}
