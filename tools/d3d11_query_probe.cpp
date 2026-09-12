// Native Windows D3D11 query variants plus D3D10.1 legacy pipeline statistics.
// No private engine entry points. Run on Helios after building with the helper;
// build machines must NOT execute this GPU workload. Captures no desktop.
// The pipeline tests issue a real three-vertex draw and require nonzero counters.
#define WIN32_LEAN_AND_MEAN
#define NOMINMAX
#include <windows.h>
#include <d3d11.h>
#include <d3d10_1.h>
#include <dxgi1_2.h>
#include <d3dcompiler.h>
#include <wrl/client.h>
#include <array>
#include <algorithm>
#include <cstdio>
#include <cstring>
#include <cwchar>
#include <exception>
using Microsoft::WRL::ComPtr;

struct Failure {};
static void require(bool valid,const char* operation) {
    if (!valid) { std::printf("FAIL %s\n",operation); throw Failure{}; }
}
static void check(HRESULT hr,const char* operation) {
    if (hr != S_OK) {
        std::printf("FAIL %s hr=0x%08lx\n",operation,static_cast<unsigned long>(hr));
        throw Failure{};
    }
}
#define CHECK(call) check((call),#call)

struct Module {
    HMODULE value{};
    explicit Module(const wchar_t* name) {
        value=LoadLibraryExW(name,nullptr,LOAD_LIBRARY_SEARCH_SYSTEM32);
        require(value != nullptr,"load system runtime");
        wchar_t path[MAX_PATH]{};
        require(GetModuleFileNameW(value,path,MAX_PATH) != 0,"runtime path");
        std::printf("runtime %ls\n",path);
    }
    ~Module() { if (value) FreeLibrary(value); }
    template<class F> F function(const char* name) {
        const auto pointer=GetProcAddress(value,name);
        require(pointer != nullptr,name);
        return reinterpret_cast<F>(pointer);
    }
};

// The surrounding bytes detect writes beyond each API's own result size.
struct alignas(8) Result {
    std::array<unsigned char,104> bytes;
    Result() { bytes.fill(0xa5); }
    void* data() { return bytes.data()+8; }
    bool untouched() const { return std::all_of(bytes.begin(),bytes.end(),[](auto b){return b == 0xa5;}); }
    void canaries(UINT size) const {
        require(std::all_of(bytes.begin(),bytes.begin()+8,[](auto b){return b == 0xa5;}),"query prefix canary");
        require(std::all_of(bytes.begin()+8+size,bytes.end(),[](auto b){return b == 0xa5;}),"query suffix canary");
    }
    template<class T> T read() const { T value{}; std::memcpy(&value,bytes.data()+8,sizeof(value)); return value; }
};

template<class Poll,class Flush> static Result poll_query(UINT size,Poll poll,Flush flush,unsigned& pending) {
    require(size <= 88,"query result fits guarded storage");
    Result result;
    const ULONGLONG deadline=GetTickCount64()+10000;
    bool flushed=false;
    for (;;) {
        const HRESULT hr=poll(result.data(),size,D3D11_ASYNC_GETDATA_DONOTFLUSH);
        // Also reject a blocking GetData that eventually returns S_OK late.
        // An external task watchdog is still needed if the call never returns.
        require(GetTickCount64() < deadline,"query completion within 10 seconds");
        result.canaries(size);
        if (hr == S_OK) {
            require(!result.untouched(),"S_OK published query data");
            return result;
        }
        CHECK(hr == S_FALSE ? S_OK : hr);
        require(result.untouched(),"S_FALSE leaves the entire caller buffer unchanged");
        ++pending;
        if (!flushed) { flush(); flushed=true; }
        require(GetTickCount64() < deadline,"query completion within 10 seconds");
        Sleep(1);
    }
}

static ComPtr<ID3DBlob> compile_shader(Module& compiler) {
    const char* source=
        "float4 main(uint id:SV_VertexID):SV_Position {"
        "return float4(id==1?1.0:-1.0,id==2?1.0:-1.0,0.0,1.0);}";
    const auto compile=compiler.function<decltype(&D3DCompile)>("D3DCompile");
    ComPtr<ID3DBlob> code,errors;
    const HRESULT hr=compile(source,std::strlen(source),"query-probe",nullptr,nullptr,"main","vs_4_0",
        D3DCOMPILE_ENABLE_STRICTNESS,0,&code,&errors);
    if (errors) std::printf("shader diagnostics: %.*s\n",static_cast<int>(errors->GetBufferSize()),
        static_cast<const char*>(errors->GetBufferPointer()));
    CHECK(hr);
    require(code != nullptr,"compiled vertex shader");
    return code;
}

static void modern_queries(Module& runtime,IDXGIAdapter1* adapter,ID3DBlob* shader) {
    const auto create=runtime.function<decltype(&D3D11CreateDevice)>("D3D11CreateDevice");
    ComPtr<ID3D11Device> device;
    ComPtr<ID3D11DeviceContext> context;
    const D3D_FEATURE_LEVEL requested=D3D_FEATURE_LEVEL_11_0;
    D3D_FEATURE_LEVEL actual{};
    CHECK(create(adapter,D3D_DRIVER_TYPE_UNKNOWN,nullptr,0,&requested,1,D3D11_SDK_VERSION,&device,&actual,&context));
    require(actual == requested,"D3D11 feature level 11_0");
    ComPtr<ID3D11VertexShader> vs;
    CHECK(device->CreateVertexShader(shader->GetBufferPointer(),shader->GetBufferSize(),nullptr,&vs));
    context->VSSetShader(vs.Get(),nullptr,0);
    context->IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);

    constexpr UINT sizes[]={4,8,8,16,88,4,16,4,16,4,16,4,16,4,16,4};
    unsigned total_pending=0;
    for (int kind=0; kind<16; ++kind) {
        const D3D11_QUERY_DESC desc{static_cast<D3D11_QUERY>(kind),0};
        ComPtr<ID3D11Query> query;
        const bool predicate=kind == 5 || (kind >= 7 && (kind & 1));
        if (predicate) {
            ComPtr<ID3D11Predicate> p;
            CHECK(device->CreatePredicate(&desc,&p));
            CHECK(p.As(&query));
        } else CHECK(device->CreateQuery(&desc,&query));
        require(query->GetDataSize() == sizes[kind],"D3D11 query result size");
        if (kind != D3D11_QUERY_EVENT && kind != D3D11_QUERY_TIMESTAMP) context->Begin(query.Get());
        if (kind == D3D11_QUERY_PIPELINE_STATISTICS) context->Draw(3,0);
        context->End(query.Get());
        unsigned pending=0;
        const Result result=poll_query(sizes[kind],[&](void* data,UINT size,UINT flags){
            return context->GetData(query.Get(),data,size,flags);
        },[&]{context->Flush();},pending);
        // Also exercise the null/zero status-only path on a completed query.
        CHECK(context->GetData(query.Get(),nullptr,0,D3D11_ASYNC_GETDATA_DONOTFLUSH));
        if (kind == D3D11_QUERY_PIPELINE_STATISTICS) {
            const auto stats=result.read<D3D11_QUERY_DATA_PIPELINE_STATISTICS>();
            std::printf("D3D11 pipeline bytes=88 IA=%llu/%llu VS=%llu HS=%llu DS=%llu CS=%llu\n",
                stats.IAVertices,stats.IAPrimitives,stats.VSInvocations,stats.HSInvocations,stats.DSInvocations,stats.CSInvocations);
            require(stats.IAVertices == 3 && stats.IAPrimitives == 1 && stats.VSInvocations == 3,
                "D3D11 pipeline counts the real draw");
            require(stats.HSInvocations == 0 && stats.DSInvocations == 0 && stats.CSInvocations == 0,
                "unused D3D11 stages have zero invocations");
        } else if (kind == D3D11_QUERY_EVENT) require(result.read<BOOL>() == TRUE,"event completed TRUE");
        else if (kind == D3D11_QUERY_TIMESTAMP)
            require(result.read<UINT64>() != 0xa5a5a5a5a5a5a5a5ull,"timestamp was written");
        else if (kind == D3D11_QUERY_TIMESTAMP_DISJOINT)
            require(result.read<D3D11_QUERY_DATA_TIMESTAMP_DISJOINT>().Frequency != 0 &&
                result.read<D3D11_QUERY_DATA_TIMESTAMP_DISJOINT>().Frequency != 0xa5a5a5a5a5a5a5a5ull,
                "timestamp frequency was written and is nonzero");
        else if (kind == D3D11_QUERY_OCCLUSION)
            require(result.read<UINT64>() == 0,"empty occlusion result is zero across all 64 bits");
        else if (predicate) require(result.read<BOOL>() == FALSE,"empty predicate is FALSE");
        else if (kind >= 6 && !(kind & 1)) {
            const auto stats=result.read<D3D11_QUERY_DATA_SO_STATISTICS>();
            require(stats.NumPrimitivesWritten == 0 && stats.PrimitivesStorageNeeded == 0,"unused SO stream statistics are zero");
        }
        CHECK(device->GetDeviceRemovedReason());
        std::printf("PASS D3D11 query=%d bytes=%u pending_polls=%u\n",kind,sizes[kind],pending);
        total_pending+=pending;
    }
    context->ClearState(); context->Flush();
    vs.Reset(); context.Reset();
    const ULONG refs=device.Detach()->Release();
    std::printf("D3D11 device Release=%lu pending_polls=%u\n",refs,total_pending);
    require(refs == 0,"D3D11 final device reference count");
}

static void legacy_pipeline(Module& runtime,IDXGIAdapter1* adapter,ID3DBlob* shader) {
    const auto create=runtime.function<decltype(&D3D10CreateDevice1)>("D3D10CreateDevice1");
    ComPtr<ID3D10Device1> device;
    CHECK(create(adapter,D3D10_DRIVER_TYPE_HARDWARE,nullptr,0,D3D10_FEATURE_LEVEL_10_0,D3D10_1_SDK_VERSION,&device));
    ComPtr<ID3D10VertexShader> vs;
    CHECK(device->CreateVertexShader(shader->GetBufferPointer(),shader->GetBufferSize(),&vs));
    device->VSSetShader(vs.Get());
    device->IASetPrimitiveTopology(D3D10_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
    const D3D10_QUERY_DESC desc{D3D10_QUERY_PIPELINE_STATISTICS,0};
    ComPtr<ID3D10Query> query;
    CHECK(device->CreateQuery(&desc,&query));
    require(query->GetDataSize() == 64,"legacy D3D10 query result is 64 bytes");
    query->Begin(); device->Draw(3,0); query->End();
    unsigned pending=0;
    const Result result=poll_query(64,[&](void* data,UINT size,UINT flags){
        return query->GetData(data,size,flags);
    },[&]{device->Flush();},pending);
    CHECK(query->GetData(nullptr,0,D3D10_ASYNC_GETDATA_DONOTFLUSH));
    const auto stats=result.read<D3D10_QUERY_DATA_PIPELINE_STATISTICS>();
    std::printf("D3D10 pipeline bytes=64 IA=%llu/%llu VS=%llu pending_polls=%u\n",
        stats.IAVertices,stats.IAPrimitives,stats.VSInvocations,pending);
    require(stats.IAVertices == 3 && stats.IAPrimitives == 1 && stats.VSInvocations == 3,
        "legacy D3D10 pipeline counts the real draw");
    CHECK(device->GetDeviceRemovedReason());
    device->ClearState(); device->Flush();
    query.Reset(); vs.Reset();
    const ULONG refs=device.Detach()->Release();
    std::printf("D3D10 device Release=%lu\n",refs);
    require(refs == 0,"D3D10 final device reference count");
}

int main() {
    setvbuf(stdout,nullptr,_IONBF,0);
    try {
        std::printf("START native queries pid=%lu bits=%zu\n",GetCurrentProcessId(),sizeof(void*)*8);
        Module dxgi(L"dxgi.dll"),d3d11(L"d3d11.dll"),d3d10(L"d3d10_1.dll"),compiler(L"d3dcompiler_47.dll");
        const auto create_factory=dxgi.function<decltype(&CreateDXGIFactory1)>("CreateDXGIFactory1");
        ComPtr<IDXGIFactory1> factory;
        CHECK(create_factory(IID_PPV_ARGS(&factory)));
        ComPtr<IDXGIAdapter1> adapter;
        for (UINT index=0;;++index) {
            const HRESULT hr=factory->EnumAdapters1(index,&adapter);
            if (hr == DXGI_ERROR_NOT_FOUND) break;
            CHECK(hr);
            DXGI_ADAPTER_DESC1 desc{}; CHECK(adapter->GetDesc1(&desc));
            if (!(desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE) &&
                (std::wcsstr(desc.Description,L"Helios") || std::wcsstr(desc.Description,L"helios"))) {
                std::printf("adapter %ls LUID=%08lx:%08lx\n",desc.Description,
                    static_cast<unsigned long>(desc.AdapterLuid.HighPart),desc.AdapterLuid.LowPart);
                break;
            }
            adapter.Reset();
        }
        require(adapter != nullptr,"nonsoftware Helios adapter");
        const auto shader=compile_shader(compiler);
        modern_queries(d3d11,adapter.Get(),shader.Get());
        legacy_pipeline(d3d10,adapter.Get(),shader.Get());
        std::printf("PASS all 16 D3D11 query variants and legacy D3D10 pipeline statistics\n");
        return 0;
    } catch (const Failure&) { return 2; }
      catch (const std::exception& e) { std::printf("FAIL exception: %s\n",e.what()); return 2; }
}
