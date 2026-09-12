// tools/d3d12_caps_dump.cpp — the D3D12 caps baseline (GATES.md D12-G2 / D12-G9).
//
// Native runtime capability inventory, as `feature,field,value` CSV. Engine
// capability probes are separate: this executable requires the system runtime,
// exact Helios PCI adapter and loaded native UMD/ICD. Run interactively.
//
// That diff is the whole point. D3D12's tiered caps are the densest version of
// the advertise-only-what-is-backed hazard this project has faced (DECISIONS.md
// H4): the runtime cross-checks tiers against each other and against shader
// models, and `D3D12Core.dll`'s own strings say so
// ("Drivers that support raytracing must expose shader model 6.3.", ~12 distinct
// "Driver filled out an invalid value in D3D12DDI_D3D12_OPTIONS_DATA::<Tier>").
// A row that moves between G2 and G9 is a claim the DDI arm is making that the
// engine does not.
//
// ⛔ No VKD3D_FEATURE_LEVEL, no --feature-level, no VKD3D_SHADER_MODEL in a gate
// run: they raise advertised tiers without backing them.
//
// Build (VM, through vcvars64 — cl is not on PATH in a win_exec shell):
//   cl /nologo /EHsc /W4 tools\d3d12_caps_dump.cpp /Fe:caps.exe /link d3d12.lib dxgi.lib bcrypt.lib
// Run: caps.exe <expected-deployed-UMD12-SHA256>
// Device creation and cap queries are admission evidence, not GPU conformance.
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <d3d12.h>
#include <dxgi1_6.h>
#include <tlhelp32.h>
#include <bcrypt.h>
#include <stdio.h>
#include <string.h>
#include <wchar.h>
#include <initializer_list>
#include "d3d12_native_identity.h"

#pragma comment(lib, "d3d12.lib")
#pragma comment(lib, "dxgi.lib")
#pragma comment(lib, "bcrypt.lib")

static ID3D12Device *g_dev;

static bool file_sha256(const wchar_t *path, char (&hex)[65])
{
    HANDLE file = CreateFileW(path, GENERIC_READ, FILE_SHARE_READ | FILE_SHARE_DELETE,
            nullptr, OPEN_EXISTING, FILE_FLAG_SEQUENTIAL_SCAN, nullptr);
    if (file == INVALID_HANDLE_VALUE) return false;
    BCRYPT_ALG_HANDLE algorithm = nullptr;
    BCRYPT_HASH_HANDLE hash = nullptr;
    bool valid = BCryptOpenAlgorithmProvider(&algorithm, BCRYPT_SHA256_ALGORITHM, nullptr, 0) >= 0;
    if (valid) valid = BCryptCreateHash(algorithm, &hash, nullptr, 0, nullptr, 0, 0) >= 0;
    unsigned char buffer[65536];
    while (valid) {
        DWORD bytes = 0;
        if (!ReadFile(file, buffer, sizeof(buffer), &bytes, nullptr)) { valid = false; break; }
        if (!bytes) break;
        valid = BCryptHashData(hash, buffer, bytes, 0) >= 0;
    }
    unsigned char digest[32];
    if (valid) valid = BCryptFinishHash(hash, digest, sizeof(digest), 0) >= 0;
    if (hash) BCryptDestroyHash(hash);
    if (algorithm) BCryptCloseAlgorithmProvider(algorithm, 0);
    CloseHandle(file);
    if (!valid) return false;
    const char digits[] = "0123456789abcdef";
    for (size_t i = 0; i < sizeof(digest); ++i) {
        hex[2 * i] = digits[digest[i] >> 4];
        hex[2 * i + 1] = digits[digest[i] & 15];
    }
    hex[64] = 0;
    return true;
}

static bool native_modules(const char *expected_umd12_sha256)
{
    wchar_t system[MAX_PATH];
    if (!GetSystemDirectoryW(system, ARRAYSIZE(system))) return false;
    HANDLE snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPMODULE, GetCurrentProcessId());
    if (snapshot == INVALID_HANDLE_VALUE) return false;
    bool runtime = false, core = false, dxgi = false, umd = false, icd = false, valid = true;
    unsigned umd_count = 0;
    MODULEENTRY32W entry = {}; entry.dwSize = sizeof(entry);
    if (!Module32FirstW(snapshot, &entry)) valid = false;
    else do {
        const bool is_runtime = !_wcsicmp(entry.szModule, L"d3d12.dll");
        const bool is_core = !_wcsicmp(entry.szModule, L"D3D12Core.dll");
        const bool is_dxgi = !_wcsicmp(entry.szModule, L"dxgi.dll");
        const bool is_umd = helios_native_umd12_name(entry.szModule);
        const bool is_icd = !_wcsnicmp(entry.szModule, L"vulkan_virtio", 13);
        if (is_runtime || is_core || is_dxgi || is_umd || is_icd)
            fprintf(stderr, "MODULE,%ls,%ls\n", entry.szModule, entry.szExePath);
        if (is_runtime || is_core || is_dxgi) {
            wchar_t expected[MAX_PATH];
            if (swprintf_s(expected, L"%ls\\%ls", system, entry.szModule) < 0 ||
                _wcsicmp(entry.szExePath, expected)) valid = false;
        }
        runtime |= is_runtime; core |= is_core; dxgi |= is_dxgi;
        umd |= is_umd; icd |= is_icd;
        if (is_umd) {
            ++umd_count;
            char digest[65] = {};
            const bool hashed = file_sha256(entry.szExePath, digest);
            if (hashed) fprintf(stderr, "MODULE_SHA256,%ls,%s\n", entry.szModule, digest);
            if (!hashed || _stricmp(digest, expected_umd12_sha256)) valid = false;
            const size_t base_length = wcslen(L"helios_umd12");
            if (hashed && entry.szModule[base_length] == L'_')
                for (size_t i = 0; i < 16; ++i)
                    if (helios_native_ascii_lower(entry.szModule[base_length + 1 + i]) != digest[i])
                        valid = false;
        }
        if (!_wcsicmp(entry.szModule, L"helios_vkd3d.dll") ||
                !_wcsicmp(entry.szModule, L"d3d10warp.dll")) valid = false;
    } while (Module32NextW(snapshot, &entry));
    CloseHandle(snapshot);
    valid = valid && umd_count == 1;
    if (!(valid && runtime && core && dxgi && umd && icd))
        fprintf(stderr, "FAIL native runtime/UMD/ICD module identity\n");
    return valid && runtime && core && dxgi && umd && icd;
}

static void row(const char *feature, const char *field, long long value)
{
    printf("%s,%s,%lld\n", feature, field, value);
}

// A cap query that the driver refuses is itself a data point — H4's failure mode
// is a driver that does not answer, and D3D12Core says so in English
// ("Driver did not respond to D3D12DDICAPS_TYPE_D3D12_OPTIONS caps query.").
// Record the HRESULT rather than dropping the rows.
#define QUERY(feat, var)                                                              \
    HRESULT hr_##var = g_dev->CheckFeatureSupport(D3D12_FEATURE_##feat, &var, sizeof(var)); \
    if (FAILED(hr_##var)) { printf("%s,QUERY_FAILED,0x%08lx\n", #feat, (unsigned long)hr_##var); } \
    else

#define F(feat, var, member) row(#feat, #member, (long long)var.member)

int main(int argc, char **argv)
{
    if (argc != 2 || strlen(argv[1]) != 64) {
        fprintf(stderr, "Usage: caps.exe <expected-deployed-UMD12-SHA256>\n");
        return 1;
    }
    for (const char *p = argv[1]; *p; ++p) {
        if (!((*p >= '0' && *p <= '9') || (*p >= 'a' && *p <= 'f') || (*p >= 'A' && *p <= 'F'))) {
            fprintf(stderr, "FAIL expected UMD12 digest is not SHA256 hex\n"); return 1;
        }
    }
    DWORD session = 0;
    if (!ProcessIdToSessionId(GetCurrentProcessId(), &session) || !session) {
        fprintf(stderr, "FAIL run through an interactive scheduled task\n");
        return 1;
    }
    const wchar_t *overrides[] = {L"VKD3D_FEATURE_LEVEL", L"VKD3D_SHADER_MODEL"};
    for (auto name : overrides) if (GetEnvironmentVariableW(name, nullptr, 0)) {
        fprintf(stderr, "FAIL capability override present: %ls\n", name); return 1;
    }
    fprintf(stderr, "PROCESS,pid,%lu,session,%lu\n", GetCurrentProcessId(), session);
    // Never assume adapter 0 is Helios.
    IDXGIFactory1 *factory = nullptr;
    if (FAILED(CreateDXGIFactory1(__uuidof(IDXGIFactory1), (void **)&factory))) {
        fprintf(stderr, "CreateDXGIFactory1 failed\n");
        return 1;
    }
    IDXGIAdapter1 *adapter = nullptr, *chosen = nullptr;
    DXGI_ADAPTER_DESC1 chosen_desc = {};
    for (UINT i = 0; factory->EnumAdapters1(i, &adapter) != DXGI_ERROR_NOT_FOUND; ++i) {
        DXGI_ADAPTER_DESC1 d = {};
        if (FAILED(adapter->GetDesc1(&d))) { adapter->Release(); factory->Release(); return 1; }
        if (!chosen && d.VendorId == 0x1af4 && d.DeviceId == 0x1050 && !(d.Flags & DXGI_ADAPTER_FLAG_SOFTWARE)) {
            chosen = adapter; chosen_desc = d;   // keep the reference
            continue;
        }
        adapter->Release();
    }
    factory->Release();
    if (!chosen) { fprintf(stderr, "no virtio-gpu (VEN_1AF4) adapter\n"); return 1; }

    HRESULT hr = D3D12CreateDevice(chosen, D3D_FEATURE_LEVEL_11_0,
                                   __uuidof(ID3D12Device), (void **)&g_dev);
    if (FAILED(hr)) {
        fprintf(stderr, "D3D12CreateDevice failed hr=0x%08lx on %ls\n",
                (unsigned long)hr, chosen_desc.Description);
        return 1;
    }
    if (!native_modules(argv[1])) { g_dev->Release(); chosen->Release(); return 1; }

    printf("feature,field,value\n");
    for (const auto level : {D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
            D3D_FEATURE_LEVEL_12_0, D3D_FEATURE_LEVEL_12_1, D3D_FEATURE_LEVEL_12_2}) {
        ID3D12Device *created = nullptr;
        HRESULT create_hr = D3D12CreateDevice(chosen, level, __uuidof(ID3D12Device), (void **)&created);
        printf("CREATE_DEVICE,0x%04x,0x%08lx\n", unsigned(level), (unsigned long)create_hr);
        if (created) created->Release();
    }
    printf("ADAPTER,Description,0\n");          // the name goes to stderr; CSV stays numeric
    fprintf(stderr, "adapter: %ls  luid=%08lx:%08lx vendor=0x%04x device=0x%04x\n",
            chosen_desc.Description,
            (unsigned long)chosen_desc.AdapterLuid.HighPart,
            (unsigned long)chosen_desc.AdapterLuid.LowPart,
            chosen_desc.VendorId, chosen_desc.DeviceId);
    row("ADAPTER", "VendorId", chosen_desc.VendorId);
    row("ADAPTER", "DeviceId", chosen_desc.DeviceId);
    row("ADAPTER", "DedicatedVideoMemoryMiB", (long long)(chosen_desc.DedicatedVideoMemory >> 20));

    {
        D3D_FEATURE_LEVEL levels[] = { D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
                                       D3D_FEATURE_LEVEL_12_0, D3D_FEATURE_LEVEL_12_1,
                                       D3D_FEATURE_LEVEL_12_2 };
        D3D12_FEATURE_DATA_FEATURE_LEVELS v = {};
        v.NumFeatureLevels = ARRAYSIZE(levels);
        v.pFeatureLevelsRequested = levels;
        QUERY(FEATURE_LEVELS, v) F(FEATURE_LEVELS, v, MaxSupportedFeatureLevel);
    }
    {
        // The runtime clamps downward from what is asked; an SDK that knows a
        // newer model than the driver returns E_INVALIDARG instead, so walk down.
        const D3D_SHADER_MODEL ask[] = { (D3D_SHADER_MODEL)0x69, (D3D_SHADER_MODEL)0x68,
                                         (D3D_SHADER_MODEL)0x67, (D3D_SHADER_MODEL)0x66,
                                         (D3D_SHADER_MODEL)0x60 };
        for (int i = 0; i < ARRAYSIZE(ask); i++) {
            D3D12_FEATURE_DATA_SHADER_MODEL v = {};
            v.HighestShaderModel = ask[i];
            if (SUCCEEDED(g_dev->CheckFeatureSupport(D3D12_FEATURE_SHADER_MODEL, &v, sizeof(v)))) {
                row("SHADER_MODEL", "HighestShaderModel", v.HighestShaderModel);
                break;
            }
        }
    }
    {
        D3D12_FEATURE_DATA_ROOT_SIGNATURE v = {};
        v.HighestVersion = D3D_ROOT_SIGNATURE_VERSION_1_1;
        if (SUCCEEDED(g_dev->CheckFeatureSupport(D3D12_FEATURE_ROOT_SIGNATURE, &v, sizeof(v))))
            row("ROOT_SIGNATURE", "HighestVersion", v.HighestVersion);
    }
    {
        D3D12_FEATURE_DATA_D3D12_OPTIONS v = {};
        QUERY(D3D12_OPTIONS, v) {
            F(OPTIONS, v, DoublePrecisionFloatShaderOps);
            F(OPTIONS, v, OutputMergerLogicOp);
            F(OPTIONS, v, MinPrecisionSupport);
            F(OPTIONS, v, TiledResourcesTier);
            F(OPTIONS, v, ResourceBindingTier);
            F(OPTIONS, v, PSSpecifiedStencilRefSupported);
            F(OPTIONS, v, TypedUAVLoadAdditionalFormats);
            F(OPTIONS, v, ROVsSupported);
            F(OPTIONS, v, ConservativeRasterizationTier);
            F(OPTIONS, v, MaxGPUVirtualAddressBitsPerResource);
            F(OPTIONS, v, StandardSwizzle64KBSupported);
            F(OPTIONS, v, CrossNodeSharingTier);
            F(OPTIONS, v, CrossAdapterRowMajorTextureSupported);
            F(OPTIONS, v, VPAndRTArrayIndexFromAnyShaderFeedingRasterizerSupportedWithoutGSEmulation);
            F(OPTIONS, v, ResourceHeapTier);
        }
    }
    {
        D3D12_FEATURE_DATA_D3D12_OPTIONS1 v = {};
        QUERY(D3D12_OPTIONS1, v) {
            F(OPTIONS1, v, WaveOps);
            F(OPTIONS1, v, WaveLaneCountMin);
            F(OPTIONS1, v, WaveLaneCountMax);
            // ⚠ TotalLaneCount reads 1024 here and that number is KNOWN WRONG:
            // it is vkd3d's 32 * subgroupSize fallback (device.c:10226-10233),
            // because venus exposes neither VK_AMD_shader_core_properties nor
            // VK_NV_shader_sm_builtins. Record it; do not "fix" the CSV.
            F(OPTIONS1, v, TotalLaneCount);
            F(OPTIONS1, v, ExpandedComputeResourceStates);
            F(OPTIONS1, v, Int64ShaderOps);
        }
    }
    {
        D3D12_FEATURE_DATA_D3D12_OPTIONS2 v = {};
        QUERY(D3D12_OPTIONS2, v) {
            F(OPTIONS2, v, DepthBoundsTestSupported);
            F(OPTIONS2, v, ProgrammableSamplePositionsTier);
        }
    }
    {
        D3D12_FEATURE_DATA_D3D12_OPTIONS3 v = {};
        QUERY(D3D12_OPTIONS3, v) {
            F(OPTIONS3, v, CopyQueueTimestampQueriesSupported);
            F(OPTIONS3, v, CastingFullyTypedFormatSupported);
            F(OPTIONS3, v, WriteBufferImmediateSupportFlags);
            F(OPTIONS3, v, ViewInstancingTier);
            F(OPTIONS3, v, BarycentricsSupported);
        }
    }
    {
        D3D12_FEATURE_DATA_D3D12_OPTIONS4 v = {};
        QUERY(D3D12_OPTIONS4, v) {
            F(OPTIONS4, v, MSAA64KBAlignedTextureSupported);
            F(OPTIONS4, v, SharedResourceCompatibilityTier);
            F(OPTIONS4, v, Native16BitShaderOpsSupported);
        }
    }
    {
        D3D12_FEATURE_DATA_D3D12_OPTIONS5 v = {};
        QUERY(D3D12_OPTIONS5, v) {
            F(OPTIONS5, v, SRVOnlyTiledResourceTier3);
            F(OPTIONS5, v, RenderPassesTier);
            F(OPTIONS5, v, RaytracingTier);
        }
    }
    {
        D3D12_FEATURE_DATA_D3D12_OPTIONS6 v = {};
        QUERY(D3D12_OPTIONS6, v) {
            F(OPTIONS6, v, AdditionalShadingRatesSupported);
            F(OPTIONS6, v, PerPrimitiveShadingRateSupportedWithViewportIndexing);
            F(OPTIONS6, v, VariableShadingRateTier);
            F(OPTIONS6, v, ShadingRateImageTileSize);
            F(OPTIONS6, v, BackgroundProcessingSupported);
        }
    }
    {
        D3D12_FEATURE_DATA_D3D12_OPTIONS7 v = {};
        QUERY(D3D12_OPTIONS7, v) {
            F(OPTIONS7, v, MeshShaderTier);
            F(OPTIONS7, v, SamplerFeedbackTier);
        }
    }
    {
        D3D12_FEATURE_DATA_D3D12_OPTIONS8 v = {};
        QUERY(D3D12_OPTIONS8, v) F(OPTIONS8, v, UnalignedBlockTexturesSupported);
    }
    {
        D3D12_FEATURE_DATA_D3D12_OPTIONS9 v = {};
        QUERY(D3D12_OPTIONS9, v) {
            F(OPTIONS9, v, MeshShaderPipelineStatsSupported);
            F(OPTIONS9, v, MeshShaderSupportsFullRangeRenderTargetArrayIndex);
            F(OPTIONS9, v, AtomicInt64OnTypedResourceSupported);
            F(OPTIONS9, v, AtomicInt64OnGroupSharedSupported);
            F(OPTIONS9, v, DerivativesInMeshAndAmplificationShadersSupported);
            F(OPTIONS9, v, WaveMMATier);
        }
    }
    {
        D3D12_FEATURE_DATA_D3D12_OPTIONS10 v = {};
        QUERY(D3D12_OPTIONS10, v) {
            F(OPTIONS10, v, VariableRateShadingSumCombinerSupported);
            F(OPTIONS10, v, MeshShaderPerPrimitiveShadingRateSupported);
        }
    }
    {
        D3D12_FEATURE_DATA_D3D12_OPTIONS11 v = {};
        QUERY(D3D12_OPTIONS11, v) F(OPTIONS11, v, AtomicInt64OnDescriptorHeapResourceSupported);
    }
    {
        D3D12_FEATURE_DATA_D3D12_OPTIONS12 v = {};
        QUERY(D3D12_OPTIONS12, v) {
            F(OPTIONS12, v, MSPrimitivesPipelineStatisticIncludesCulledPrimitives);
            F(OPTIONS12, v, EnhancedBarriersSupported);
            F(OPTIONS12, v, RelaxedFormatCastingSupported);
        }
    }
    {
        D3D12_FEATURE_DATA_D3D12_OPTIONS13 v = {};
        QUERY(D3D12_OPTIONS13, v) {
            F(OPTIONS13, v, UnrestrictedBufferTextureCopyPitchSupported);
            F(OPTIONS13, v, UnrestrictedVertexElementAlignmentSupported);
            F(OPTIONS13, v, InvertedViewportHeightFlipsYSupported);
            F(OPTIONS13, v, InvertedViewportDepthFlipsZSupported);
            F(OPTIONS13, v, TextureCopyBetweenDimensionsSupported);
            F(OPTIONS13, v, AlphaBlendFactorSupported);
        }
    }
    {
        D3D12_FEATURE_DATA_D3D12_OPTIONS14 v = {};
        QUERY(D3D12_OPTIONS14, v) {
            F(OPTIONS14, v, AdvancedTextureOpsSupported);
            F(OPTIONS14, v, WriteableMSAATexturesSupported);
            F(OPTIONS14, v, IndependentFrontAndBackStencilRefMaskSupported);
        }
    }
    {
        D3D12_FEATURE_DATA_D3D12_OPTIONS15 v = {};
        QUERY(D3D12_OPTIONS15, v) {
            F(OPTIONS15, v, TriangleFanSupported);
            F(OPTIONS15, v, DynamicIndexBufferStripCutSupported);
        }
    }
    {
        D3D12_FEATURE_DATA_D3D12_OPTIONS16 v = {};
        QUERY(D3D12_OPTIONS16, v) {
            F(OPTIONS16, v, DynamicDepthBiasSupported);
            F(OPTIONS16, v, GPUUploadHeapSupported);
        }
    }
    {
        D3D12_FEATURE_DATA_D3D12_OPTIONS17 v = {};
        QUERY(D3D12_OPTIONS17, v) {
            F(OPTIONS17, v, NonNormalizedCoordinateSamplersSupported);
            F(OPTIONS17, v, ManualWriteTrackingResourceSupported);
        }
    }
    {
        D3D12_FEATURE_DATA_D3D12_OPTIONS18 v = {};
        QUERY(D3D12_OPTIONS18, v) F(OPTIONS18, v, RenderPassesValid);
    }
    {
        D3D12_FEATURE_DATA_D3D12_OPTIONS19 v = {};
        QUERY(D3D12_OPTIONS19, v) {
            F(OPTIONS19, v, MismatchingOutputDimensionsSupported);
            F(OPTIONS19, v, SupportedSampleCountsWithNoOutputs);
            F(OPTIONS19, v, PointSamplingAddressesNeverRoundUp);
            F(OPTIONS19, v, RasterizerDesc2Supported);
            F(OPTIONS19, v, NarrowQuadrilateralLinesSupported);
            F(OPTIONS19, v, AnisoFilterWithPointMipSupported);
            F(OPTIONS19, v, MaxSamplerDescriptorHeapSize);
            F(OPTIONS19, v, MaxSamplerDescriptorHeapSizeWithStaticSamplers);
            F(OPTIONS19, v, MaxViewDescriptorHeapSize);
            F(OPTIONS19, v, ComputeOnlyCustomHeapSupported);
        }
    }
    {
        D3D12_FEATURE_DATA_ARCHITECTURE1 v = {};
        QUERY(ARCHITECTURE1, v) {
            F(ARCHITECTURE1, v, TileBasedRenderer);
            F(ARCHITECTURE1, v, UMA);
            F(ARCHITECTURE1, v, CacheCoherentUMA);
            F(ARCHITECTURE1, v, IsolatedMMU);
        }
    }
    {
        D3D12_FEATURE_DATA_GPU_VIRTUAL_ADDRESS_SUPPORT v = {};
        QUERY(GPU_VIRTUAL_ADDRESS_SUPPORT, v) {
            F(GPUVA, v, MaxGPUVirtualAddressBitsPerResource);
            F(GPUVA, v, MaxGPUVirtualAddressBitsPerProcess);
        }
    }
    {
        D3D12_FEATURE_DATA_EXISTING_HEAPS v = {};
        QUERY(EXISTING_HEAPS, v) F(EXISTING_HEAPS, v, Supported);
    }
    {
        D3D12_FEATURE_DATA_SERIALIZATION v = {};
        QUERY(SERIALIZATION, v) F(SERIALIZATION, v, HeapSerializationTier);
    }
    {
        D3D12_FEATURE_DATA_CROSS_NODE v = {};
        QUERY(CROSS_NODE, v) {
            F(CROSS_NODE, v, SharingTier);
            F(CROSS_NODE, v, AtomicShaderInstructions);
        }
    }
    {
        D3D12_FEATURE_DATA_PREDICATION v = {};
        QUERY(PREDICATION, v) F(PREDICATION, v, Supported);
    }
    {
        D3D12_FEATURE_DATA_HARDWARE_COPY v = {};
        QUERY(HARDWARE_COPY, v) F(HARDWARE_COPY, v, Supported);
    }
    row("NODE", "Count", g_dev->GetNodeCount());
    for (int t = D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV; t < D3D12_DESCRIPTOR_HEAP_TYPE_NUM_TYPES; t++)
        row("DESCRIPTOR_STRIDE", t == 0 ? "CBV_SRV_UAV" : t == 1 ? "SAMPLER" : t == 2 ? "RTV" : "DSV",
            g_dev->GetDescriptorHandleIncrementSize((D3D12_DESCRIPTOR_HEAP_TYPE)t));

    g_dev->Release();
    chosen->Release();
    return 0;
}
