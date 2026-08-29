// Does the HOST GPU do any work when the guest draws hard?
//
// 2026-08-29. Every in-guest witness of host execution has turned out to be
// unsound: mapped memory cannot see the result, and the record-only query path
// resolves before its work is even flushed (PREFLUSH GetData returns S_OK on
// Helios, S_FALSE on WARP). So ask the one observer outside the whole stack --
// the host GPU's own utilisation counter, read with nvidia-smi pmon on the
// Linux side while this runs.
//
// The workload is deliberately gross: a 1024x1024 render target, DRAWS_PER_FRAME
// full-viewport triangles per iteration, for DURATION_MS. If the host is really
// rasterising this, virgl_render_server's sm% is unmissable. If the host GPU
// stays flat while the guest issues millions of vertices, the work is not
// reaching the GPU.
//
// A null result is weaker than a positive one, so the load is sized so that a
// null is still meaningful: at ~1e5 draws over 20 s covering 1M pixels each,
// anything executing would register.
//
//   g++ -O1 -o C:\Users\Rupansh\d3d11_gpu_load_probe.exe ^
//       Z:\tools\d3d11_gpu_load_probe.cpp -ld3d11 -ldxgi -ld3dcompiler -ldxguid
#include <d3d11.h>
#include <dxgi1_2.h>
#include <d3dcompiler.h>
#include <windows.h>
#include <cstdio>
#include <cstdint>
#include <cwchar>

static const UINT W = 1024, H = 1024;
static const int DRAWS_PER_FRAME = 64;
static const DWORD DURATION_MS = 20000;

static const char *kVS =
  "float4 main(uint id : SV_VertexID) : SV_Position {\n"
  "  float2 p = float2((id << 1) & 2, id & 2);\n"
  "  return float4(p * float2(2, -2) + float2(-1, 1), 0, 1);\n"
  "}\n";
// Deliberately expensive per pixel so the host cannot hide the work in noise.
static const char *kPS =
  "float4 main(float4 pos : SV_Position) : SV_Target {\n"
  "  float acc = 0;\n"
  "  [loop] for (int i = 0; i < 64; ++i)\n"
  "    acc += sin(pos.x * 0.01f + i) * cos(pos.y * 0.01f + i);\n"
  "  return float4(acc, 0.5, 0.25, 1);\n"
  "}\n";

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
  if (FAILED(D3D11CreateDevice(h, D3D_DRIVER_TYPE_UNKNOWN, nullptr, 0, nullptr, 0,
                               D3D11_SDK_VERSION, &dev, &got, &ctx))) {
    printf("D3D11CreateDevice failed\n"); return 1;
  }

  ID3DBlob *vsb = nullptr, *psb = nullptr, *err = nullptr;
  if (FAILED(D3DCompile(kVS, strlen(kVS), 0, 0, 0, "main", "vs_4_0", 0, 0, &vsb, &err))
   || FAILED(D3DCompile(kPS, strlen(kPS), 0, 0, 0, "main", "ps_4_0", 0, 0, &psb, &err))) {
    printf("shader compile failed: %s\n", err ? (char *)err->GetBufferPointer() : "");
    return 1;
  }
  ID3D11VertexShader *vs = nullptr; ID3D11PixelShader *ps = nullptr;
  dev->CreateVertexShader(vsb->GetBufferPointer(), vsb->GetBufferSize(), nullptr, &vs);
  dev->CreatePixelShader(psb->GetBufferPointer(), psb->GetBufferSize(), nullptr, &ps);
  if (!vs || !ps) { printf("shader creation failed\n"); return 1; }

  D3D11_TEXTURE2D_DESC td = {};
  td.Width = W; td.Height = H; td.MipLevels = 1; td.ArraySize = 1;
  td.Format = DXGI_FORMAT_B8G8R8A8_UNORM; td.SampleDesc.Count = 1;
  td.Usage = D3D11_USAGE_DEFAULT; td.BindFlags = D3D11_BIND_RENDER_TARGET;
  ID3D11Texture2D *rt = nullptr;
  if (FAILED(dev->CreateTexture2D(&td, nullptr, &rt))) { printf("rt failed\n"); return 1; }
  ID3D11RenderTargetView *rtv = nullptr;
  if (FAILED(dev->CreateRenderTargetView(rt, nullptr, &rtv))) { printf("rtv failed\n"); return 1; }

  D3D11_VIEWPORT vp = {};
  vp.Width = (FLOAT)W; vp.Height = (FLOAT)H; vp.MaxDepth = 1.0f;
  ctx->OMSetRenderTargets(1, &rtv, nullptr);
  ctx->RSSetViewports(1, &vp);
  ctx->IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
  ctx->IASetInputLayout(nullptr);
  ctx->VSSetShader(vs, nullptr, 0);
  ctx->PSSetShader(ps, nullptr, 0);

  printf("hammering %ux%u, %d draws/iter, for %lu ms -- watch nvidia-smi pmon now\n",
         W, H, DRAWS_PER_FRAME, (unsigned long)DURATION_MS);
  fflush(stdout);

  const float clear[4] = { 0, 0, 0, 1 };
  uint64_t frames = 0, draws = 0;
  const DWORD t0 = GetTickCount();
  while (GetTickCount() - t0 < DURATION_MS) {
    ctx->ClearRenderTargetView(rtv, clear);
    for (int d = 0; d < DRAWS_PER_FRAME; ++d) { ctx->Draw(3, 0); ++draws; }
    ctx->Flush();
    ++frames;
  }
  const DWORD ms = GetTickCount() - t0;

  printf("done: frames=%llu draws=%llu in %lu ms (%.0f draws/s, %.1f Gpix/s if executed)\n",
         (unsigned long long)frames, (unsigned long long)draws, (unsigned long)ms,
         draws * 1000.0 / (ms ? ms : 1),
         (double)draws * W * H / ((ms ? ms : 1) / 1000.0) / 1e9);

  rtv->Release(); rt->Release();
  ps->Release(); vs->Release(); psb->Release(); vsb->Release();
  ctx->Release(); dev->Release(); h->Release(); f->Release();
  return 0;
}
