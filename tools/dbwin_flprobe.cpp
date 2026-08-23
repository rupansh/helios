// Capture the D3D11 debug layer's OutputDebugString while attempting an FL11_0
// create on the Helios adapter, to get d3d11.dll's exact rejection reason.
// Sets up the classic DBWIN_BUFFER shared-memory listener in a background
// thread, then calls D3D11CreateDevice(FL11_0, DEBUG). Run with
// HKLM\SOFTWARE\Helios!FeatureLevel11=1 (UMD claims 11_0).
//
// Build (WinLibs g++):
//   g++ -O2 -o C:\Users\Rupansh\dbwin_flprobe.exe Z:\tools\dbwin_flprobe.cpp \
//       -ld3d11 -ldxgi -ldxguid
#define INITGUID
#include <windows.h>
#include <dbghelp.h>
#include <d3d11.h>
#include <d3d11_4.h>
#include <dxgi.h>
#include <cstdio>
#include <cstring>
#include <cwchar>

struct DbwinBuffer { DWORD pid; char data[4096 - sizeof(DWORD)]; };

static volatile bool g_stop = false;
static HANDLE g_dataReady = nullptr;
static HANDLE g_bufferReady = nullptr;
static DbwinBuffer* g_buf = nullptr;
static volatile LONG g_dumped = 0;

static void capture_refused_process(DWORD pid) {
  if (InterlockedCompareExchange(&g_dumped, 1, 0) != 0)
    return;
  HANDLE process = OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ |
                                   PROCESS_SUSPEND_RESUME,
                               FALSE, pid);
  using NtSuspendProcessFn = LONG (WINAPI *)(HANDLE);
  using NtResumeProcessFn = LONG (WINAPI *)(HANDLE);
  HMODULE ntdll = GetModuleHandleW(L"ntdll.dll");
  auto suspend_process = reinterpret_cast<NtSuspendProcessFn>(
      GetProcAddress(ntdll, "NtSuspendProcess"));
  auto resume_process = reinterpret_cast<NtResumeProcessFn>(
      GetProcAddress(ntdll, "NtResumeProcess"));
  const bool suspended = process && suspend_process &&
                         suspend_process(process) >= 0;
  HANDLE dump = CreateFileW(L"C:\\Users\\Rupansh\\hnr2-refused-317-live.dmp",
                            GENERIC_WRITE, FILE_SHARE_READ, nullptr,
                            CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, nullptr);
  BOOL ok = FALSE;
  if (process && dump != INVALID_HANDLE_VALUE) {
    ok = MiniDumpWriteDump(
        process, pid, dump,
        static_cast<MINIDUMP_TYPE>(MiniDumpWithFullMemory |
                                   MiniDumpWithHandleData |
                                   MiniDumpWithThreadInfo),
        nullptr, nullptr, nullptr);
  }
  const DWORD error = ok ? ERROR_SUCCESS : GetLastError();
  printf("--- live refusal dump: pid=%lu ok=%d error=%lu ---\n",
         (unsigned long)pid, ok ? 1 : 0, (unsigned long)error);
  fflush(stdout);
  if (dump != INVALID_HANDLE_VALUE) CloseHandle(dump);
  if (suspended && resume_process) resume_process(process);
  if (process) CloseHandle(process);
}

static DWORD WINAPI listener(LPVOID) {
  SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL);
  while (!g_stop) {
    DWORD w = WaitForSingleObject(g_dataReady, 500);
    if (w == WAIT_TIMEOUT) continue;
    if (w != WAIT_OBJECT_0) break;
    // Copy out, NUL-terminate defensively.
    char line[4096];
    size_t n = 0;
    for (; n < sizeof(g_buf->data) - 1 && g_buf->data[n]; ++n) line[n] = g_buf->data[n];
    line[n] = 0;
    if (strstr(line, "D3DKMTRender fragment 0/1 len=264"))
      capture_refused_process(g_buf->pid);
    printf("[ODS pid=%lu] %s", (unsigned long)g_buf->pid, line);
    if (n == 0 || line[n ? n - 1 : 0] != '\n') printf("\n");
    fflush(stdout);
    SetEvent(g_bufferReady);
  }
  return 0;
}

int main(int argc, char** argv) {
  // DBWIN setup — become the OutputDebugString listener.
  g_bufferReady = CreateEventA(nullptr, FALSE, TRUE, "DBWIN_BUFFER_READY");
  g_dataReady = CreateEventA(nullptr, FALSE, FALSE, "DBWIN_DATA_READY");
  HANDLE map = CreateFileMappingA(INVALID_HANDLE_VALUE, nullptr, PAGE_READWRITE,
                                  0, sizeof(DbwinBuffer), "DBWIN_BUFFER");
  if (!g_bufferReady || !g_dataReady || !map) { printf("DBWIN setup failed (already listening?)\n"); }
  else {
    g_buf = (DbwinBuffer*)MapViewOfFile(map, FILE_MAP_READ | FILE_MAP_WRITE, 0, 0, sizeof(DbwinBuffer));
    SetEvent(g_bufferReady);
    CreateThread(nullptr, 0, listener, nullptr, 0, nullptr);
  }

  if (argc == 2 && std::strcmp(argv[1], "--listen") == 0) {
    printf("--- DBWIN listener only (12 seconds) ---\n");
    fflush(stdout);
    Sleep(12000);
    g_stop = true;
    SetEvent(g_dataReady);
    Sleep(200);
    return 0;
  }

  HMODULE currentIcd = LoadLibraryW(
    L"C:\\ProgramData\\HeliosUmd\\vulkan_virtio.dll");
  printf("--- preload current ICD: module=%p gle=%lu ---\n",
         currentIcd, (unsigned long)GetLastError());
  fflush(stdout);

  IDXGIFactory1* f = nullptr;
  if (FAILED(CreateDXGIFactory1(__uuidof(IDXGIFactory1), (void**)&f))) { printf("factory fail\n"); return 1; }
  IDXGIAdapter1* hel = nullptr; IDXGIAdapter1* a = nullptr;
  for (UINT i = 0; f->EnumAdapters1(i, &a) != DXGI_ERROR_NOT_FOUND; ++i) {
    DXGI_ADAPTER_DESC1 d{}; a->GetDesc1(&d);
    printf("adapter[%u] luid=%08lx:%08lx %ls\n", i,
           (unsigned long)d.AdapterLuid.HighPart,
           (unsigned long)d.AdapterLuid.LowPart, d.Description);
    if (wcsstr(d.Description, L"Helios")) {
      hel = a;
      break;
    }
    a->Release();
  }
  if (!hel) { printf("no Helios\n"); return 2; }

  printf("--- D3D11CreateDevice(FL11_0, DEBUG) ---\n"); fflush(stdout);
  const D3D_FEATURE_LEVEL l110[] = { D3D_FEATURE_LEVEL_11_0 };
  const bool removedEventDefault =
    argc == 2 && std::strcmp(argv[1], "--removed-event-default") == 0;
  ID3D11Device* dev = nullptr; ID3D11DeviceContext* ctx = nullptr;
  D3D_FEATURE_LEVEL got = (D3D_FEATURE_LEVEL)0;
  HRESULT hr = D3D11CreateDevice(removedEventDefault ? nullptr : hel,
                                 removedEventDefault ? D3D_DRIVER_TYPE_HARDWARE
                                                     : D3D_DRIVER_TYPE_UNKNOWN,
                                 nullptr,
                                 removedEventDefault ? D3D11_CREATE_DEVICE_BGRA_SUPPORT
                                                     : D3D11_CREATE_DEVICE_DEBUG,
                                 removedEventDefault ? nullptr : l110,
                                 removedEventDefault ? 0 : 1,
                                 D3D11_SDK_VERSION, &dev, &got, &ctx);
  Sleep(600); // let the listener flush any queued ODS lines
  printf("--- result: hr=0x%08x got=0x%04x ---\n", (unsigned)hr, (unsigned)got);
  fflush(stdout);

  if (argc == 2 &&
      (std::strcmp(argv[1], "--removed-event") == 0 || removedEventDefault)) {
    ID3D11Device4* dev4 = nullptr;
    HRESULT qiHr = dev ? dev->QueryInterface(__uuidof(ID3D11Device4),
                                              (void**)&dev4)
                       : E_FAIL;
    printf("--- QueryInterface(ID3D11Device4): hr=0x%08x dev4=%p ---\n",
           (unsigned)qiHr, dev4);
    HANDLE event = CreateEventW(nullptr, FALSE, FALSE, nullptr);
    DWORD cookie = 0;
    HRESULT registerHr = dev4 && event
      ? dev4->RegisterDeviceRemovedEvent(event, &cookie)
      : E_FAIL;
    printf("--- RegisterDeviceRemovedEvent: hr=0x%08x cookie=0x%08lx event=%p ---\n",
           (unsigned)registerHr, (unsigned long)cookie, event);
    fflush(stdout);
    if (SUCCEEDED(registerHr)) dev4->UnregisterDeviceRemoved(cookie);
    if (event) CloseHandle(event);
    if (dev4) dev4->Release();
    if (ctx) ctx->Release();
    if (dev) dev->Release();
    hel->Release(); f->Release();
    if (currentIcd) FreeLibrary(currentIcd);
    return SUCCEEDED(registerHr) ? 0 : 3;
  }

  ID3D11Buffer* buffer = nullptr;
  ID3D11Texture2D* texture = nullptr;
  if (SUCCEEDED(hr) && dev) {
    D3D11_BUFFER_DESC desc{};
    desc.ByteWidth = 64;
    desc.Usage = D3D11_USAGE_DEFAULT;
    desc.BindFlags = D3D11_BIND_CONSTANT_BUFFER;
    HRESULT bufferHr = dev->CreateBuffer(&desc, nullptr, &buffer);
    Sleep(600);
    printf("--- CreateBuffer(64, CONSTANT_BUFFER): hr=0x%08x ---\n",
           (unsigned)bufferHr);
    fflush(stdout);
  }

  if (SUCCEEDED(hr) && dev) {
    D3D11_TEXTURE2D_DESC desc{};
    desc.Width = 256;
    desc.Height = 80;
    desc.MipLevels = 1;
    desc.ArraySize = 1;
    desc.Format = DXGI_FORMAT_B8G8R8A8_UNORM;
    desc.SampleDesc.Count = 1;
    desc.Usage = D3D11_USAGE_DEFAULT;
    desc.BindFlags = D3D11_BIND_SHADER_RESOURCE;
    HRESULT textureHr = dev->CreateTexture2D(&desc, nullptr, &texture);
    Sleep(600);
    printf("--- CreateTexture2D(256x80, B8G8R8A8, SRV): hr=0x%08x ---\n",
           (unsigned)textureHr);
    fflush(stdout);
  }

  g_stop = true;
  Sleep(200);
  if (texture) texture->Release();
  if (buffer) buffer->Release();
  if (ctx) ctx->Release();
  if (dev) dev->Release();
  hel->Release(); f->Release();
  if (currentIcd) FreeLibrary(currentIcd);
  return 0;
}
