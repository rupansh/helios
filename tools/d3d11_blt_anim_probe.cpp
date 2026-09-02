// Animated windowed BLT-model probe: clears the back buffer to a different solid colour every
// frame (red, green, blue, ...) and presents at ~10 fps, so two captures taken seconds apart must
// differ if dwm keeps recomposing the window after each present. Args:
//   d3d11_blt_anim_probe.exe [discard|sequential] [shaderinput 0|1] [seconds]
// Logs to C:\Users\Rupansh\helios-probe\blt_anim.txt. Window title carries the frame counter.
#include <windows.h>
#include <d3d11.h>
#include <dxgi1_2.h>
#include <stdio.h>
#include <string.h>
static FILE* g_log;
static void L(const char* f, ...) { va_list a; va_start(a, f); char b[512]; vsnprintf(b, sizeof b, f, a); va_end(a); printf("%s\n", b); if (g_log) { fprintf(g_log, "%s\n", b); fflush(g_log); } }
static LRESULT CALLBACK wp(HWND h, UINT m, WPARAM w, LPARAM l) { if (m == WM_DESTROY) { PostQuitMessage(0); return 0; } return DefWindowProc(h, m, w, l); }
int main(int argc, char** argv) {
  g_log = fopen("C:\\Users\\Rupansh\\helios-probe\\blt_anim.txt", "w");
  const bool sequential = argc > 1 && !strcmp(argv[1], "sequential");
  const bool shaderInput = argc > 2 && atoi(argv[2]) != 0;
  const int seconds = argc > 3 ? atoi(argv[3]) : 25;
  L("blt_anim pid=%lu swap=%s shaderinput=%d seconds=%d", GetCurrentProcessId(), sequential ? "sequential" : "discard", (int)shaderInput, seconds);
  WNDCLASSA wc = {}; wc.lpfnWndProc = wp; wc.hInstance = GetModuleHandle(nullptr); wc.lpszClassName = "helios_anim"; wc.hbrBackground = (HBRUSH)(COLOR_WINDOW + 1); RegisterClassA(&wc);
  HWND hwnd = CreateWindowA("helios_anim", "helios-anim", WS_OVERLAPPEDWINDOW | WS_VISIBLE, 200, 150, 640, 480, nullptr, nullptr, wc.hInstance, nullptr);
  IDXGIFactory1* f = nullptr; CreateDXGIFactory1(__uuidof(IDXGIFactory1), (void**)&f);
  IDXGIAdapter1* ad = nullptr; IDXGIAdapter1* a = nullptr;
  for (UINT i = 0; f->EnumAdapters1(i, &a) != DXGI_ERROR_NOT_FOUND; ++i) { DXGI_ADAPTER_DESC1 d = {}; a->GetDesc1(&d); if (d.VendorId == 0x1af4 && !ad) ad = a; else a->Release(); }
  if (!ad) { L("no Helios adapter"); return 1; }
  DXGI_SWAP_CHAIN_DESC sd = {}; sd.BufferCount = 1; sd.BufferDesc.Width = 0; sd.BufferDesc.Height = 0; sd.BufferDesc.Format = DXGI_FORMAT_B8G8R8A8_UNORM;
  sd.BufferUsage = DXGI_USAGE_RENDER_TARGET_OUTPUT | (shaderInput ? DXGI_USAGE_SHADER_INPUT : 0); sd.OutputWindow = hwnd; sd.SampleDesc.Count = 1; sd.Windowed = TRUE;
  sd.SwapEffect = sequential ? DXGI_SWAP_EFFECT_SEQUENTIAL : DXGI_SWAP_EFFECT_DISCARD;
  ID3D11Device* dev = nullptr; ID3D11DeviceContext* ctx = nullptr; IDXGISwapChain* sc = nullptr; D3D_FEATURE_LEVEL fl;
  D3D_FEATURE_LEVEL want[] = { D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_10_0 };
  HRESULT hr = D3D11CreateDeviceAndSwapChain(ad, D3D_DRIVER_TYPE_UNKNOWN, nullptr, 0, want, 2, D3D11_SDK_VERSION, &sd, &sc, &dev, &fl, &ctx);
  L("D3D11CreateDeviceAndSwapChain hr=0x%08x fl=0x%x", (unsigned)hr, (unsigned)fl); if (FAILED(hr)) return 1;
  ID3D11Texture2D* bb = nullptr; sc->GetBuffer(0, __uuidof(ID3D11Texture2D), (void**)&bb); ID3D11RenderTargetView* rtv = nullptr; dev->CreateRenderTargetView(bb, nullptr, &rtv);
  const float cols[3][4] = { {1,0,0,1}, {0,1,0,1}, {0,0,1,1} };
  ULONGLONG t0 = GetTickCount64(); UINT frame = 0; MSG msg;
  while (GetTickCount64() - t0 < (ULONGLONG)seconds * 1000) {
    while (PeekMessage(&msg, nullptr, 0, 0, PM_REMOVE)) { TranslateMessage(&msg); DispatchMessage(&msg); }
    ctx->OMSetRenderTargets(1, &rtv, nullptr); ctx->ClearRenderTargetView(rtv, cols[frame % 3]);
    hr = sc->Present(0, 0);
    if (frame < 3 || (frame % 50) == 0) L("Present #%u colour=%s hr=0x%08x t=%lums", frame, frame % 3 == 0 ? "RED" : frame % 3 == 1 ? "GREEN" : "BLUE", (unsigned)hr, (unsigned long)(GetTickCount64() - t0));
    char title[64]; snprintf(title, sizeof title, "helios-anim f=%u %s", frame, frame % 3 == 0 ? "RED" : frame % 3 == 1 ? "GREEN" : "BLUE"); SetWindowTextA(hwnd, title);
    ++frame; Sleep(100);
  }
  L("done frames=%u", frame);
  return 0;
}
