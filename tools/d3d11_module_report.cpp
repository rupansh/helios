// Which UMD and which venus ICD did THIS D3D11 device actually load?
//
// 2026-08-29. The deploy notes say helios_umd.dll and vulkan_virtio.dll are
// hash-verified in the DriverStore, but the registered Vulkan ICD manifest
// (HKLM\SOFTWARE\Khronos\Vulkan\Drivers) names a DIFFERENT, older file under
// C:\ProgramData\HeliosVulkan, and DXVK reaches Vulkan through vulkan-1.dll
// (dxvk-helios/src/vulkan/vulkan_loader.cpp:16) — i.e. through that manifest.
// A DriverStore hash that matches while the loaded module does not is the
// standing trap here (memory: umd-deploy-hazards-2026-08).
//
// So ask the process itself. Create the Helios D3D11 device, then walk our own
// module list and print the full path, size and timestamp of everything that
// matters. Whatever this prints is what the measurements were actually taken
// against.
//
//   g++ -O1 -o C:\Users\Rupansh\d3d11_module_report.exe ^
//       Z:\tools\d3d11_module_report.cpp -ld3d11 -ldxgi -ldxguid -lpsapi
#include <d3d11.h>
#include <dxgi1_2.h>
#include <windows.h>
#include <psapi.h>
#include <cstdio>
#include <cstring>
#include <cwchar>

static void report_modules(const char *when) {
  HMODULE mods[1024];
  DWORD needed = 0;
  if (!EnumProcessModules(GetCurrentProcess(), mods, sizeof(mods), &needed)) return;
  const size_t n = needed / sizeof(HMODULE);
  printf("--- loaded modules of interest (%s) ---\n", when);
  for (size_t i = 0; i < n; ++i) {
    char path[MAX_PATH] = {};
    if (!GetModuleFileNameA(mods[i], path, MAX_PATH)) continue;
    const char *base = strrchr(path, '\\');
    base = base ? base + 1 : path;
    if (!strstr(base, "vulkan") && !strstr(base, "helios") && !strstr(base, "Helios")
        && !strstr(base, "virtio") && !strstr(base, "dxvk"))
      continue;
    WIN32_FILE_ATTRIBUTE_DATA fad = {};
    unsigned long long size = 0;
    char stamp[64] = "?";
    if (GetFileAttributesExA(path, GetFileExInfoStandard, &fad)) {
      size = ((unsigned long long)fad.nFileSizeHigh << 32) | fad.nFileSizeLow;
      SYSTEMTIME st = {};
      FileTimeToSystemTime(&fad.ftLastWriteTime, &st);
      snprintf(stamp, sizeof(stamp), "%04u-%02u-%02u %02u:%02u",
               st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute);
    }
    printf("  %-13llu %s  %s\n", size, stamp, path);
  }
}

int main() {
  report_modules("before D3D11");

  IDXGIFactory1 *factory = nullptr;
  if (FAILED(CreateDXGIFactory1(IID_IDXGIFactory1, (void **)&factory))) { printf("no factory\n"); return 1; }
  IDXGIAdapter1 *helios = nullptr, *a = nullptr;
  for (UINT i = 0; factory->EnumAdapters1(i, &a) != DXGI_ERROR_NOT_FOUND; ++i) {
    DXGI_ADAPTER_DESC1 d = {}; a->GetDesc1(&d);
    if (wcsstr(d.Description, L"Helios")) { helios = a; break; }
    a->Release();
  }
  if (!helios) { printf("Helios adapter not found\n"); return 1; }

  ID3D11Device *dev = nullptr; ID3D11DeviceContext *ctx = nullptr;
  D3D_FEATURE_LEVEL got = (D3D_FEATURE_LEVEL)0;
  HRESULT hr = D3D11CreateDevice(helios, D3D_DRIVER_TYPE_UNKNOWN, nullptr, 0,
                                 nullptr, 0, D3D11_SDK_VERSION, &dev, &got, &ctx);
  printf("\nD3D11CreateDevice hr=0x%08x fl=0x%x pid=%lu\n\n",
         (unsigned)hr, (unsigned)got, (unsigned long)GetCurrentProcessId());

  report_modules("after D3D11");

  if (ctx) ctx->Release();
  if (dev) dev->Release();
  helios->Release(); factory->Release();
  return 0;
}
