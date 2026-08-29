// Does the host execute the command buffer at all?
//
// 2026-08-29. Everything measured so far says GPU-produced content never
// reaches the guest's CPU view: a poisoned staging texture survives
// staging->staging, staging->DEFAULT->staging and ClearRenderTargetView->
// staging alike. Two very different defects produce that, and no amount of
// reading mapped memory can separate them:
//
//   (a) the host never executes the work;
//   (b) it executes, and the guest's map is not that memory.
//
// An occlusion query separates them, because its result does NOT come back
// through mapped memory. DXVK reads occlusion results with
// vkGetQueryPoolResults (dxvk_gpu_query.cpp:96) — a host CALL whose answer
// returns over the venus reply channel. So:
//
//   samplesPassed == pixels drawn   the GPU rasterised our triangles. Work IS
//                                   executing; the defect is (b), memory.
//   samplesPassed == 0              nothing rasterised. The defect is (a),
//                                   execution, and every memory theory is moot.
//
// PIPELINE_STATISTICS is read the same way and is printed alongside as a
// second witness: it reports vertex/primitive/pixel-shader invocation counts,
// so a zero occlusion count with nonzero invocations would mean the raster
// stage specifically, not submission.
//
// A full-viewport quad is drawn with no vertex buffer (SV_VertexID), so the
// only resources involved are the render target and the query itself.
//
// It runs the SAME sequence on WARP first, as a positive control. A probe whose
// zero has never been shown to be capable of being nonzero is not evidence —
// twice this week an unvalidated instrument produced a confident wrong reading.
//
//   g++ -O1 -o C:\Users\Rupansh\d3d11_execution_witness_probe.exe ^
//       Z:\tools\d3d11_execution_witness_probe.cpp ^
//       -ld3d11 -ldxgi -ld3dcompiler -ldxguid
#include <d3d11.h>
#include <dxgi1_2.h>
#include <d3dcompiler.h>
#include <windows.h>
#include <cstdio>
#include <cstdint>
#include <cwchar>

static const UINT W = 64, H = 64;

static const char *kVS =
  "float4 main(uint id : SV_VertexID) : SV_Position {\n"
  "  float2 p = float2((id << 1) & 2, id & 2);\n"
  "  return float4(p * float2(2, -2) + float2(-1, 1), 0, 1);\n"
  "}\n";
static const char *kPS =
  "float4 main(float4 pos : SV_Position) : SV_Target { return float4(1, 0.5, 0.25, 1); }\n";

// Block until a query result is available, or give up. Returns false on
// timeout, which is itself a finding: an occlusion query that never resolves
// means the reply channel, not the raster stage.
static bool get_data(ID3D11DeviceContext *ctx, ID3D11Asynchronous *q,
                     void *out, UINT size, const char *tag) {
  const DWORD t0 = GetTickCount();
  while (GetTickCount() - t0 < 5000) {
    HRESULT hr = ctx->GetData(q, out, size, 0);
    if (hr == S_OK) return true;
    if (FAILED(hr)) { printf("  %s GetData hr=0x%08x\n", tag, (unsigned)hr); return false; }
    Sleep(1);
  }
  printf("  %s never resolved in 5000 ms\n", tag);
  return false;
}

// Returns samplesPassed, or UINT64_MAX if the run could not be set up.
static uint64_t run_once(IDXGIAdapter *adapter, D3D_DRIVER_TYPE type, const char *label) {
  printf("\n=== %s ===\n", label);
  ID3D11Device *dev = nullptr; ID3D11DeviceContext *ctx = nullptr;
  D3D_FEATURE_LEVEL got = (D3D_FEATURE_LEVEL)0;
  HRESULT hr = D3D11CreateDevice(adapter, type, nullptr, 0, nullptr, 0,
                                 D3D11_SDK_VERSION, &dev, &got, &ctx);
  printf("D3D11CreateDevice hr=0x%08x fl=0x%x\n", (unsigned)hr, (unsigned)got);
  if (FAILED(hr)) return UINT64_MAX;

  ID3DBlob *vsb = nullptr, *psb = nullptr, *err = nullptr;
  if (FAILED(D3DCompile(kVS, strlen(kVS), 0, 0, 0, "main", "vs_4_0", 0, 0, &vsb, &err))) {
    printf("vs compile failed: %s\n", err ? (char *)err->GetBufferPointer() : "");
    return UINT64_MAX;
  }
  if (FAILED(D3DCompile(kPS, strlen(kPS), 0, 0, 0, "main", "ps_4_0", 0, 0, &psb, &err))) {
    printf("ps compile failed: %s\n", err ? (char *)err->GetBufferPointer() : "");
    return UINT64_MAX;
  }
  ID3D11VertexShader *vs = nullptr; ID3D11PixelShader *ps = nullptr;
  dev->CreateVertexShader(vsb->GetBufferPointer(), vsb->GetBufferSize(), nullptr, &vs);
  dev->CreatePixelShader(psb->GetBufferPointer(), psb->GetBufferSize(), nullptr, &ps);
  if (!vs || !ps) { printf("shader creation failed\n"); return UINT64_MAX; }

  D3D11_TEXTURE2D_DESC td = {};
  td.Width = W; td.Height = H; td.MipLevels = 1; td.ArraySize = 1;
  td.Format = DXGI_FORMAT_B8G8R8A8_UNORM; td.SampleDesc.Count = 1;
  td.Usage = D3D11_USAGE_DEFAULT; td.BindFlags = D3D11_BIND_RENDER_TARGET;
  ID3D11Texture2D *rt = nullptr;
  if (FAILED(dev->CreateTexture2D(&td, nullptr, &rt))) { printf("rt failed\n"); return UINT64_MAX; }
  ID3D11RenderTargetView *rtv = nullptr;
  if (FAILED(dev->CreateRenderTargetView(rt, nullptr, &rtv))) { printf("rtv failed\n"); return UINT64_MAX; }

  ID3D11Query *occl = nullptr, *stats = nullptr;
  D3D11_QUERY_DESC qd = {};
  qd.Query = D3D11_QUERY_OCCLUSION;            dev->CreateQuery(&qd, &occl);
  qd.Query = D3D11_QUERY_PIPELINE_STATISTICS;  dev->CreateQuery(&qd, &stats);
  if (!occl) { printf("occlusion query unsupported\n"); return UINT64_MAX; }

  D3D11_VIEWPORT vp = {};
  vp.Width = (FLOAT)W; vp.Height = (FLOAT)H; vp.MaxDepth = 1.0f;
  const float clear[4] = { 0, 0, 0, 1 };

  ctx->OMSetRenderTargets(1, &rtv, nullptr);
  ctx->RSSetViewports(1, &vp);
  ctx->IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
  ctx->IASetInputLayout(nullptr);
  ctx->VSSetShader(vs, nullptr, 0);
  ctx->PSSetShader(ps, nullptr, 0);
  ctx->ClearRenderTargetView(rtv, clear);

  ctx->Begin(occl);
  if (stats) ctx->Begin(stats);
  ctx->Draw(3, 0);
  if (stats) ctx->End(stats);
  ctx->End(occl);
  ctx->Flush();

  uint64_t samples = 0;
  const bool ok = get_data(ctx, occl, &samples, sizeof(samples), "OCCLUSION");
  if (ok) {
    printf("  OCCLUSION samplesPassed=%llu / %u  %s\n", (unsigned long long)samples, W * H,
           samples == (uint64_t)W * H ? "rasterised every pixel"
           : samples > 0              ? "partial coverage"
                                      : "ZERO - nothing rasterised");
  }
  if (stats) {
    D3D11_QUERY_DATA_PIPELINE_STATISTICS s = {};
    if (get_data(ctx, stats, &s, sizeof(s), "PIPESTATS")) {
      printf("  PIPESTATS iaVertices=%llu iaPrimitives=%llu vsInvocations=%llu "
             "psInvocations=%llu\n",
             (unsigned long long)s.IAVertices, (unsigned long long)s.IAPrimitives,
             (unsigned long long)s.VSInvocations, (unsigned long long)s.PSInvocations);
    }
    stats->Release();
  }

  occl->Release(); rtv->Release(); rt->Release();
  ps->Release(); vs->Release(); psb->Release(); vsb->Release();
  ctx->Release(); dev->Release();
  return ok ? samples : UINT64_MAX;
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

  const uint64_t warp = run_once(nullptr, D3D_DRIVER_TYPE_WARP, "WARP (positive control)");
  const uint64_t helios = run_once(h, D3D_DRIVER_TYPE_UNKNOWN, "HELIOS");

  printf("\n--- verdict ---\n");
  if (warp != (uint64_t)W * H) {
    printf("INCONCLUSIVE: the control did not report %u either, so this probe\n"
           "does not measure what it claims. Fix the probe before reading Helios.\n", W * H);
  } else if (helios == (uint64_t)W * H) {
    printf("Helios EXECUTES and rasterises correctly; the defect is the guest's\n"
           "view of memory, not execution.\n");
  } else if (helios == UINT64_MAX) {
    printf("Helios could not complete the run; see its section above.\n");
  } else {
    printf("PROBE VALIDATED by the control, and Helios reports %llu of %u.\n"
           "Work is NOT executing on the host. Chase submission/execution;\n"
           "every memory-aliasing theory is downstream of this.\n",
           (unsigned long long)helios, W * H);
  }

  h->Release(); f->Release();
  return 0;
}
