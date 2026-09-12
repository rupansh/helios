// Native D3D11 legacy DISCARD presentation: d3d11-msaa-present.exe [1|4 [unorm|srgb [seconds]]]
// Default: 4 samples, sRGB, 10 seconds, 1280x800. Run in the interactive desktop.
// Every frame is resolved/copied and checked against a CPU RGBA oracle before Present.
// Readback is NOT scanout evidence: capture the visible window from host VNC, never GDI.
// Midtone band: left = linear 0.5; right = alternating black/white SV_SampleIndex.
// For 4x, both resolve to UNORM128 / sRGB188; for 1x the right band is black.
// First-frame actual/expected BMPs are written in the working directory.
// --self-test exercises only the CPU oracle/grader and is safe on build machines.
#define WIN32_LEAN_AND_MEAN
#define NOMINMAX
#include <windows.h>
#include <d3d11.h>
#include <dxgi1_2.h>
#include <d3dcompiler.h>
#include <wrl/client.h>
#include <algorithm>
#include <array>
#include <cerrno>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <cwchar>
#include <exception>
#include <fstream>
#include <limits>
#include <vector>
using Microsoft::WRL::ComPtr;

struct Failure {};
static void require(bool value,const char* operation) {
    if (!value) { std::printf("FAIL %s Win32=%lu\n",operation,GetLastError()); throw Failure{}; }
}
static void check(HRESULT hr,const char* operation) {
    if (hr != S_OK) { std::printf("FAIL %s hr=0x%08lx\n",operation,static_cast<unsigned long>(hr)); throw Failure{}; }
}
#define CHECK(call) check((call),#call)
struct Module {
    HMODULE value{};
    explicit Module(const wchar_t* name) {
        value=LoadLibraryExW(name,nullptr,LOAD_LIBRARY_SEARCH_SYSTEM32);
        require(value != nullptr,"load system runtime");
        wchar_t path[MAX_PATH]{};
        const DWORD length=GetModuleFileNameW(value,path,MAX_PATH);
        require(length && length < MAX_PATH,"system runtime path");
        std::printf("runtime %ls\n",path);
    }
    ~Module() { if (value) FreeLibrary(value); }
    template<class T> T function(const char* name) {
        const auto result=GetProcAddress(value,name);
        require(result != nullptr,name);
        return reinterpret_cast<T>(result);
    }
};
struct Rgba { uint8_t r,g,b,a; };
static_assert(sizeof(Rgba) == 4,"RGBA8 layout");
static Rgba expected(UINT x,UINT y,UINT width,UINT height,UINT serial,UINT samples,bool srgb) {
    if (y >= height-16) {
        const uint8_t v=(serial & (1u << (x*16/width)))?255:0;
        return {v,v,v,255};
    }
    if (y >= height*3/4 && y < height*7/8) {
        const uint8_t v=x < width/2 || samples == 4 ? (srgb?188:128) : 0;
        return {v,v,v,255};
    }
    if (x/64 == y/64) return {255,255,255,255};
    if (y < height/2) return x < width/2 ? Rgba{255,0,0,255} : Rgba{0,255,0,255};
    return x < width/2 ? Rgba{0,0,255,255} : Rgba{255,255,0,255};
}
static uint64_t compare(const uint8_t* data,size_t pitch,UINT width,UINT height,UINT serial,UINT samples,bool srgb,bool report) {
    uint64_t mismatches=0;
    for (UINT y=0;y<height;++y) for (UINT x=0;x<width;++x) {
        const auto* got=data+static_cast<size_t>(y)*pitch+x*4;
        const Rgba want=expected(x,y,width,height,serial,samples,srgb);
        if (got[0]!=want.r || got[1]!=want.g || got[2]!=want.b || got[3]!=want.a) {
            if (!mismatches && report) std::printf("FAIL pixel %u,%u got=%u,%u,%u,%u expected=%u,%u,%u,%u\n",
                x,y,got[0],got[1],got[2],got[3],want.r,want.g,want.b,want.a);
            ++mismatches;
        }
    }
    return mismatches;
}
static std::vector<uint8_t> reference(UINT width,UINT height,UINT serial,UINT samples,bool srgb,size_t pitch) {
    std::vector<uint8_t> bytes(pitch*height,0xcc);
    for (UINT y=0;y<height;++y) for (UINT x=0;x<width;++x) {
        const Rgba value=expected(x,y,width,height,serial,samples,srgb);
        std::memcpy(bytes.data()+static_cast<size_t>(y)*pitch+x*4,&value,4);
    }
    return bytes;
}
static int self_test() {
    constexpr UINT dimensions[][2]={{16,32},{65,97},{1280,800}};
    unsigned cases=0;
    for (const auto& size:dimensions) for (UINT samples:{1u,4u}) for (bool srgb:{false,true}) {
        const UINT w=size[0],h=size[1],serial=0xa55a;
        const size_t pitch=static_cast<size_t>(w)*4+28;
        auto bytes=reference(w,h,serial,samples,srgb,pitch);
        require(compare(bytes.data(),pitch,w,h,serial,samples,srgb,true)==0,"CPU padded-row reference");
        require(compare(bytes.data(),pitch,w,h,serial^1,samples,srgb,false)>0,"CPU stale-frame rejection");
        for (UINT c=0;c<4;++c) {
            bytes[c]^=1;
            require(compare(bytes.data(),pitch,w,h,serial,samples,srgb,false)==1,"CPU single-channel rejection");
            bytes[c]^=1;
        }
        if (samples==4 && srgb) {
            // Incorrectly averaging encoded black/white samples yields128, not188.
            auto* pixel=bytes.data()+static_cast<size_t>(h*3/4)*pitch+(w*3/4)*4;
            pixel[0]=pixel[1]=pixel[2]=128;
            require(compare(bytes.data(),pitch,w,h,serial,samples,srgb,false)>0,"CPU wrong-gamma resolve rejection");
        }
        ++cases;
    }
    std::printf("PASS CPU oracle: %u size/sample/format cases, stale frames, all RGBA channels, wrong gamma\n",cases);
    return 0;
}
static void save_bmp(const char* name,const uint8_t* rgba,size_t pitch,UINT width,UINT height) {
    BITMAPFILEHEADER file{};
    BITMAPINFOHEADER info{};
    file.bfType=0x4d42;
    file.bfOffBits=sizeof(file)+sizeof(info);
    file.bfSize=file.bfOffBits+width*height*4;
    info.biSize=sizeof(info); info.biWidth=static_cast<LONG>(width); info.biHeight=-static_cast<LONG>(height);
    info.biPlanes=1; info.biBitCount=32; info.biCompression=BI_RGB; info.biSizeImage=width*height*4;
    std::ofstream stream(name,std::ios::binary);
    require(stream.is_open(),"open BMP reference");
    stream.write(reinterpret_cast<const char*>(&file),sizeof(file));
    stream.write(reinterpret_cast<const char*>(&info),sizeof(info));
    std::vector<uint8_t> row(static_cast<size_t>(width)*4);
    for (UINT y=0;y<height;++y) {
        const auto* input=rgba+static_cast<size_t>(y)*pitch;
        for (UINT x=0;x<width;++x) {
            row[x*4]=input[x*4+2]; row[x*4+1]=input[x*4+1]; row[x*4+2]=input[x*4]; row[x*4+3]=input[x*4+3];
        }
        stream.write(reinterpret_cast<const char*>(row.data()),static_cast<std::streamsize>(row.size()));
    }
    stream.close(); require(!stream.fail(),"write BMP reference");
    std::printf("reference %s\n",name);
}
static bool closed;
static LRESULT CALLBACK window_proc(HWND window,UINT message,WPARAM wparam,LPARAM lparam) {
    if (message==WM_CLOSE || (message==WM_KEYDOWN && wparam==VK_ESCAPE)) { closed=true; return 0; }
    if (message==WM_DESTROY) { PostQuitMessage(0); return 0; }
    return DefWindowProcW(window,message,wparam,lparam);
}
static void pump(HWND window) {
    MSG message{};
    while (PeekMessageW(&message,nullptr,0,0,PM_REMOVE)) {
        if (message.message==WM_QUIT) closed=true;
        else { TranslateMessage(&message); DispatchMessageW(&message); }
    }
    require(!closed && IsWindowVisible(window) && !IsIconic(window),"presentation window remains visible");
}
static void wait_gpu(ID3D11Device* device,ID3D11DeviceContext* context,ID3D11Query* event,HWND window,bool& pending) {
    pending=true; context->End(event); context->Flush();
    const ULONGLONG deadline=GetTickCount64()+5000;
    for (;;) {
        BOOL complete=FALSE;
        const HRESULT hr=context->GetData(event,&complete,sizeof(complete),D3D11_ASYNC_GETDATA_DONOTFLUSH);
        require(GetTickCount64()<deadline,"GPU completion within 5 seconds");
        CHECK(device->GetDeviceRemovedReason()); pump(window);
        if (hr==S_OK) { require(complete==TRUE,"event completion TRUE"); pending=false; return; }
        CHECK(hr==S_FALSE?S_OK:hr); Sleep(1);
    }
}
struct PendingGuard {
    bool& pending;
    ~PendingGuard() {
        if (pending && std::uncaught_exceptions()) {
            // Do not unwind GPU resource owners when completion is unknown.
            // The process exit is a failure; no synthetic completion is published.
            std::printf("FAIL GPU work may still be active; terminating without COM teardown\n");
            std::fflush(stdout); TerminateProcess(GetCurrentProcess(),2); std::_Exit(2);
        }
    }
};
static ComPtr<ID3DBlob> compile(Module& compiler,const char* source,const char* profile) {
    auto function=compiler.function<decltype(&D3DCompile)>("D3DCompile");
    ComPtr<ID3DBlob> code,errors;
    HRESULT hr=function(source,std::strlen(source),"msaa-present",nullptr,nullptr,"main",profile,D3DCOMPILE_ENABLE_STRICTNESS,0,&code,&errors);
    if (errors) std::printf("shader: %.*s\n",static_cast<int>(errors->GetBufferSize()),static_cast<const char*>(errors->GetBufferPointer()));
    CHECK(hr); require(code != nullptr,"shader bytecode"); return code;
}
static int run(UINT samples,bool srgb,UINT seconds,bool resize) {
    UINT width=1280,height=800;
    DWORD session=0; require(ProcessIdToSessionId(GetCurrentProcessId(),&session) && session!=0,"interactive desktop session");
    require(SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)!=nullptr,"physical window coordinates");
    const int screenWidth=GetSystemMetrics(SM_CXSCREEN),screenHeight=GetSystemMetrics(SM_CYSCREEN);
    require(screenWidth>=static_cast<int>(width) && screenHeight>=static_cast<int>(height),"1280x800 fits the primary screen");
    WNDCLASSW wc{}; wc.lpfnWndProc=window_proc; wc.hInstance=GetModuleHandleW(nullptr); wc.lpszClassName=L"HeliosMsaaPresent";
    require(RegisterClassW(&wc)!=0,"register window");
    wchar_t title[128]{}; swprintf_s(title,L"Helios D3D11 %ux %ls - native resolve/readback",samples,srgb?L"sRGB":L"UNORM");
    HWND window=CreateWindowExW(WS_EX_TOPMOST,wc.lpszClassName,title,WS_POPUP,
        (screenWidth-static_cast<int>(width))/2,(screenHeight-static_cast<int>(height))/2,width,height,nullptr,nullptr,wc.hInstance,nullptr);
    require(window!=nullptr,"create visible window"); ShowWindow(window,SW_SHOW); pump(window);
    RECT rectangle{}; require(GetClientRect(window,&rectangle) && rectangle.right==width && rectangle.bottom==height,"exact client size");
    POINT origin{}; require(ClientToScreen(window,&origin),"client origin");
    std::printf("START pid=%lu bits=%zu samples=%u format=%s seconds=%u window=%p client=%ld,%ld %ux%u\n",
        GetCurrentProcessId(),sizeof(void*)*8,samples,srgb?"sRGB":"UNORM",seconds,window,origin.x,origin.y,width,height);

    Module dxgi(L"dxgi.dll"),runtime(L"d3d11.dll"),compiler(L"d3dcompiler_47.dll");
    ComPtr<IDXGIFactory1> factory;
    CHECK(dxgi.function<decltype(&CreateDXGIFactory1)>("CreateDXGIFactory1")(IID_PPV_ARGS(&factory)));
    ComPtr<IDXGIAdapter1> adapter;
    for (UINT index=0;;++index) {
        HRESULT hr=factory->EnumAdapters1(index,&adapter);
        if (hr==DXGI_ERROR_NOT_FOUND) break;
        CHECK(hr); DXGI_ADAPTER_DESC1 desc{}; CHECK(adapter->GetDesc1(&desc));
        if (!(desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE) &&
            (std::wcsstr(desc.Description,L"Helios") || std::wcsstr(desc.Description,L"helios"))) {
            std::printf("adapter %ls LUID=%08lx:%08lx\n",desc.Description,static_cast<unsigned long>(desc.AdapterLuid.HighPart),desc.AdapterLuid.LowPart); break;
        }
        adapter.Reset();
    }
    require(adapter != nullptr,"nonsoftware Helios adapter");
    ComPtr<ID3D11Device> device; ComPtr<ID3D11DeviceContext> context;
    const D3D_FEATURE_LEVEL wanted=D3D_FEATURE_LEVEL_11_0; D3D_FEATURE_LEVEL actual{};
    CHECK(runtime.function<decltype(&D3D11CreateDevice)>("D3D11CreateDevice")(adapter.Get(),D3D_DRIVER_TYPE_UNKNOWN,nullptr,0,&wanted,1,D3D11_SDK_VERSION,&device,&actual,&context));
    require(actual==wanted,"feature level11_0 for sample-frequency shading");
    const DXGI_FORMAT format=srgb?DXGI_FORMAT_R8G8B8A8_UNORM_SRGB:DXGI_FORMAT_R8G8B8A8_UNORM;
    UINT quality=0,support=0; CHECK(device->CheckMultisampleQualityLevels(format,samples,&quality)); require(quality>0,"requested MSAA support");
    CHECK(device->CheckFormatSupport(format,&support));
    const UINT required=D3D11_FORMAT_SUPPORT_RENDER_TARGET | (samples>1?(D3D11_FORMAT_SUPPORT_MULTISAMPLE_RENDERTARGET|D3D11_FORMAT_SUPPORT_MULTISAMPLE_RESOLVE):0);
    require((support & required)==required,"render/resolve format support");
    DXGI_SWAP_CHAIN_DESC swapDesc{};
    swapDesc.BufferDesc.Width=width; swapDesc.BufferDesc.Height=height; swapDesc.BufferDesc.Format=format;
    swapDesc.SampleDesc.Count=samples; swapDesc.BufferUsage=DXGI_USAGE_RENDER_TARGET_OUTPUT;
    swapDesc.BufferCount=1; swapDesc.OutputWindow=window; swapDesc.Windowed=TRUE; swapDesc.SwapEffect=DXGI_SWAP_EFFECT_DISCARD;
    ComPtr<IDXGISwapChain> swap; CHECK(factory->CreateSwapChain(device.Get(),&swapDesc,&swap));
    CHECK(factory->MakeWindowAssociation(window,DXGI_MWA_NO_ALT_ENTER));
    ComPtr<ID3D11Texture2D> back,resolved,staging;
    ComPtr<ID3D11RenderTargetView> rtv;
    const auto createBuffers=[&]() {
        CHECK(swap->GetBuffer(0,IID_PPV_ARGS(&back)));
        D3D11_TEXTURE2D_DESC texture{}; back->GetDesc(&texture);
        require(texture.Width==width && texture.Height==height && texture.Format==format && texture.SampleDesc.Count==samples && texture.SampleDesc.Quality==0,"actual swapchain format/samples");
        CHECK(device->CreateRenderTargetView(back.Get(),nullptr,&rtv));
        texture.SampleDesc={1,0}; texture.Usage=D3D11_USAGE_DEFAULT; texture.BindFlags=0; texture.CPUAccessFlags=0; texture.MiscFlags=0;
        CHECK(device->CreateTexture2D(&texture,nullptr,&resolved));
        texture.Usage=D3D11_USAGE_STAGING; texture.CPUAccessFlags=D3D11_CPU_ACCESS_READ;
        CHECK(device->CreateTexture2D(&texture,nullptr,&staging));
        std::printf("backbuffer fmt=%u sample=%ux%u size=%ux%u\n",static_cast<UINT>(format),samples,0u,width,height);
    };
    createBuffers();
    const char* vsText="float4 main(uint id:SV_VertexID):SV_Position { return float4(id==2?3:-1,id==1?3:-1,0,1); }";
    const char* psText=
        "cbuffer C:register(b0){uint W,H,serial,pad;}"
        "float4 main(float4 pos:SV_Position,uint sample:SV_SampleIndex):SV_Target {"
        "uint x=(uint)pos.x,y=(uint)pos.y;"
        "if(y>=H-16){float v=(serial&(1u<<(x*16/W)))?1:0;return float4(v,v,v,1);}"
        "if(y>=H*3/4&&y<H*7/8){float v=x<W/2?0.5:((sample&1)?1:0);return float4(v,v,v,1);}"
        "if(x/64==y/64)return float4(1,1,1,1);"
        "if(y<H/2)return x<W/2?float4(1,0,0,1):float4(0,1,0,1);"
        "return x<W/2?float4(0,0,1,1):float4(1,1,0,1);}";
    const auto vsCode=compile(compiler,vsText,"vs_5_0"),psCode=compile(compiler,psText,"ps_5_0");
    ComPtr<ID3D11VertexShader> vs; ComPtr<ID3D11PixelShader> ps;
    CHECK(device->CreateVertexShader(vsCode->GetBufferPointer(),vsCode->GetBufferSize(),nullptr,&vs));
    CHECK(device->CreatePixelShader(psCode->GetBufferPointer(),psCode->GetBufferSize(),nullptr,&ps));
    D3D11_BUFFER_DESC cbDesc{}; cbDesc.ByteWidth=16; cbDesc.Usage=D3D11_USAGE_DEFAULT; cbDesc.BindFlags=D3D11_BIND_CONSTANT_BUFFER;
    ComPtr<ID3D11Buffer> cb; CHECK(device->CreateBuffer(&cbDesc,nullptr,&cb));
    D3D11_RASTERIZER_DESC raster{}; raster.FillMode=D3D11_FILL_SOLID; raster.CullMode=D3D11_CULL_NONE; raster.DepthClipEnable=TRUE; raster.MultisampleEnable=samples>1;
    ComPtr<ID3D11RasterizerState> rs; CHECK(device->CreateRasterizerState(&raster,&rs));
    D3D11_QUERY_DESC queryDesc{D3D11_QUERY_EVENT,0}; ComPtr<ID3D11Query> event; CHECK(device->CreateQuery(&queryDesc,&event));
    bool pending=false; PendingGuard guard{pending}; // Declared after all GPU owners; runs before their unwind.
    UINT geometry=0;
    UINT frames=0; uint64_t pixels=0;
    const ULONGLONG start=GetTickCount64(),finish=start+static_cast<ULONGLONG>(seconds)*1000;
    do {
        pump(window);
        const UINT nextGeometry=resize?static_cast<UINT>((GetTickCount64()-start)*12/(seconds*1000u)):0;
        if (nextGeometry>geometry && geometry<11) {
            // Retire API work and release every backbuffer reference before
            // ResizeBuffers. KMD presentation readers retire independently.
            pending=true; context->ClearState(); wait_gpu(device.Get(),context.Get(),event.Get(),window,pending);
            rtv.Reset(); staging.Reset(); resolved.Reset(); back.Reset();
            ++geometry; width=1280-geometry*16; height=800-geometry*8;
            require(SetWindowPos(window,nullptr,0,0,width,height,SWP_NOMOVE|SWP_NOZORDER),"resize visible window");
            CHECK(swap->ResizeBuffers(1,width,height,format,0));
            createBuffers();
            std::printf("RESIZE geometry=%u frame=%u client=%ld,%ld %ux%u\n",geometry,frames,origin.x,origin.y,width,height);
        }
        const D3D11_VIEWPORT viewport{0,0,static_cast<float>(width),static_cast<float>(height),0,1};
        pending=true;
        const UINT constants[4]={width,height,frames,0}; context->UpdateSubresource(cb.Get(),0,nullptr,constants,0,0);
        context->IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
        context->VSSetShader(vs.Get(),nullptr,0); context->PSSetShader(ps.Get(),nullptr,0);
        ID3D11Buffer* buffer=cb.Get(); context->PSSetConstantBuffers(0,1,&buffer);
        context->RSSetState(rs.Get()); context->RSSetViewports(1,&viewport);
        ID3D11RenderTargetView* target=rtv.Get(); context->OMSetRenderTargets(1,&target,nullptr);
        const FLOAT clear[4]={1,0,1,1}; context->ClearRenderTargetView(rtv.Get(),clear); context->Draw(3,0);
        context->OMSetRenderTargets(0,nullptr,nullptr);
        // Both typed resources and the resolve format are identical; resolving
        // directly to UNORM would not be a valid typed-sRGB ResolveSubresource.
        if (samples>1) context->ResolveSubresource(resolved.Get(),0,back.Get(),0,format);
        else context->CopyResource(resolved.Get(),back.Get());
        context->CopyResource(staging.Get(),resolved.Get());
        wait_gpu(device.Get(),context.Get(),event.Get(),window,pending);
        D3D11_MAPPED_SUBRESOURCE mapped{};
        const ULONGLONG mapDeadline=GetTickCount64()+5000;
        for (;;) {
            const HRESULT hr=context->Map(staging.Get(),0,D3D11_MAP_READ,D3D11_MAP_FLAG_DO_NOT_WAIT,&mapped);
            require(GetTickCount64()<mapDeadline,"staging map within 5 seconds");
            if (hr==S_OK) break;
            CHECK(hr==DXGI_ERROR_WAS_STILL_DRAWING?S_OK:hr); CHECK(device->GetDeviceRemovedReason()); pump(window); Sleep(1);
        }
        struct Unmap { ID3D11DeviceContext* context; ID3D11Texture2D* texture; ~Unmap(){context->Unmap(texture,0);} } unmap{context.Get(),staging.Get()};
        require(mapped.pData && mapped.RowPitch>=width*4 && mapped.RowPitch<=std::numeric_limits<size_t>::max()/height,"mapped texture bounds");
        if (!frames) {
            char file[96]{}; sprintf_s(file,"d3d11-msaa-%u-%s-actual.bmp",samples,srgb?"srgb":"unorm");
            save_bmp(file,static_cast<const uint8_t*>(mapped.pData),mapped.RowPitch,width,height);
            const auto wantedPixels=reference(width,height,frames,samples,srgb,static_cast<size_t>(width)*4);
            sprintf_s(file,"d3d11-msaa-%u-%s-expected.bmp",samples,srgb?"srgb":"unorm");
            save_bmp(file,wantedPixels.data(),static_cast<size_t>(width)*4,width,height);
        }
        require(compare(static_cast<const uint8_t*>(mapped.pData),mapped.RowPitch,width,height,frames,samples,srgb,true)==0,"exact resolved RGBA pixels");
        pixels+=static_cast<uint64_t>(width)*height;
        // Unmap must happen before further commands use the staging resource.
        // It remains scoped until this iteration ends; Present uses only back.
        pending=true; CHECK(swap->Present(1,0)); CHECK(device->GetDeviceRemovedReason()); pump(window);
        ++frames;
        if (frames==1 || frames%120==0) std::printf("frame=%u exact_pixels=%llu midpoint=%u sample_band=%u\n",frames,pixels,srgb?188u:128u,samples==4?(srgb?188u:128u):0u);
    } while (GetTickCount64()<finish);
    require(frames>=2,"multiple presented frames");
    require(!resize || geometry==11,"twelve distinct presented geometries");
    pump(window); pending=true; context->ClearState(); wait_gpu(device.Get(),context.Get(),event.Get(),window,pending);
    const ULONGLONG elapsed=GetTickCount64()-start;
    event.Reset(); rs.Reset(); cb.Reset(); ps.Reset(); vs.Reset(); rtv.Reset(); staging.Reset(); resolved.Reset(); back.Reset(); swap.Reset(); context.Reset();
    const ULONG refs=device.Detach()->Release(); require(refs==0,"final D3D11 device reference count");
    adapter.Reset(); factory.Reset();
    require(DestroyWindow(window),"destroy presentation window"); require(UnregisterClassW(wc.lpszClassName,wc.hInstance),"unregister window");
    std::printf("PASS native D3D11 %ux %s frames=%u exact_pixels=%llu elapsed_ms=%llu device_refs=%lu; host VNC still required for scanout\n",
        samples,srgb?"sRGB":"UNORM",frames,pixels,elapsed,refs);
    return 0;
}
int main(int argc,char** argv) {
    setvbuf(stdout,nullptr,_IONBF,0);
    try {
        if (argc==2 && std::strcmp(argv[1],"--self-test")==0) return self_test();
        require(argc<=5,"usage: d3d11-msaa-present.exe [1|4 [unorm|srgb [seconds [resize]]]]");
        UINT samples=4,seconds=10; bool srgb=true;
        if (argc>1) { require(std::strcmp(argv[1],"1")==0 || std::strcmp(argv[1],"4")==0,"sample count must be1 or4"); samples=static_cast<UINT>(argv[1][0]-'0'); }
        if (argc>2) { require(std::strcmp(argv[2],"unorm")==0 || std::strcmp(argv[2],"srgb")==0,"format must be unorm or srgb"); srgb=std::strcmp(argv[2],"srgb")==0; }
        if (argc>3) {
            char* end=nullptr; errno=0; const unsigned long value=std::strtoul(argv[3],&end,10);
            require(errno==0 && end!=argv[3] && *end=='\0' && value>=1 && value<=120,"seconds must be1..120"); seconds=static_cast<UINT>(value);
        }
        const bool resize=argc==5;
        require(!resize || std::strcmp(argv[4],"resize")==0,"last argument must be resize");
        require(!resize || seconds>=12,"resize run needs at least12 seconds");
        return run(samples,srgb,seconds,resize);
    } catch (const Failure&) { return 2; }
      catch (const std::exception& e) { std::printf("FAIL exception %s\n",e.what()); return 2; }
}
