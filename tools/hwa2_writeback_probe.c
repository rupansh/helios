// hwa2_writeback_probe.c — does a KMD create-time private-data write reach the
// D3DKMT caller, and what does it depend on?
//
// The K5 acceptance probe (tools/hts1_session_probe.c, check H2) found that a
// role-1 HVM1 allocation created with `D3DKMTCreateAllocation2` comes back with
// `object_generation`, `segment_page_shift` and `allocation_alignment` all ZERO,
// even though the create is admitted — and the KMD's own `DxgkDdiOpenAllocation`
// then sees the same unstamped bytes (measured in the diag ring: create with
// `PrivateDriverDataSize=0x40`, admitted, then an open at `0x40` whose HVM1
// re-validation refuses, leaving `TsPoolBind` and `TsPoolRej` both at 0).
// Meanwhile a 168-byte HWA2 created by a UMD through `pfnAllocateCb` DOES reach
// open stamped: `OaHwa2Rej` has never fired while `OaNoRid` moves, and
// `read_open_descriptor` requires a NONZERO generation that `validate_create_input`
// forbids the producer from supplying.
//
// So the write-back works for one shape and not the other. This probe isolates
// the variable by issuing the SAME record type through four call shapes from one
// process, so size, record type, adapter and driver image are all held fixed and
// only the create-call shape moves.
//
// Build (win11, WinLibs gcc):
//   gcc -O2 -o hwa2_writeback_probe.exe hwa2_writeback_probe.c
//       -I Z:\protocol\include -I Z:\icd\win-build\wdk-include -lgdi32
//
// State-neutral: every object it creates it destroys, and it renders nothing.
// Exit 0 means every variant behaved; the OUTPUT is the measurement, not the
// exit code.

#include <stdio.h>
#include <string.h>
#include <windows.h>

#ifndef _NTDEF_
typedef LONG NTSTATUS, *PNTSTATUS;
#endif
#include <d3dkmthk.h>

#include "helios_native_render.h"
#include "helios_wddm.h"

#define IGNORE_STATUS(call)                                                    \
   do {                                                                        \
      NTSTATUS ignored_ = (call);                                              \
      (void)ignored_;                                                          \
   } while (0)

#define STATUS_SUCCESS_NT ((NTSTATUS)0x00000000L)

/* Helios's legacy (no-private-data) context profile: 256-KiB DMA buffer. No
 * inbox driver matches it, and it is independent of everything under test. */
#define HELIOS_LEGACY_DMA_BUFFER_BYTES 262144u

static int g_fail;

/* One ordinary non-image HWA2 buffer, the minimal record `validate_create_input`
 * admits: no dimensions, no planes, sentinel VidPn source, CPU-visible memory
 * class matched to the CPU_VISIBLE flag, and every KMD-owned bit clear. */
static HeliosWddmAllocationDescV2
hwa2_buffer(int resource_associated)
{
   HeliosWddmAllocationDescV2 d;
   memset(&d, 0, sizeof(d));
   d.magic = HELIOS_HWA2_MAGIC;
   d.abi_version = (uint16_t)HELIOS_HWA2_ABI_VERSION;
   d.struct_size = (uint16_t)HELIOS_HWA2_BYTES;
   d.package_generation = HELIOS_PACKAGE_GENERATION;
   d.allocation_generation = 0; /* zero in, nonzero out */
   d.byte_size = 65536;
   d.allocation_kind = HELIOS_HWA2_KIND_BUFFER;
   d.flags = HELIOS_HWA2_FLAG_CPU_VISIBLE |
             (resource_associated ? HELIOS_HWA2_FLAG_RESOURCE_ASSOCIATED : 0u);
   d.vidpn_source = HELIOS_D3DDDI_ID_UNINITIALIZED;
   d.swizzle_class = HELIOS_HWA2_SWIZZLE_LINEAR;
   d.memory_class = HELIOS_HWA2_MEMORY_CPU_VISIBLE;
   d.plane_count = 0;
   return d;
}

static HeliosVenusMemoryAllocationV1
hvm1_reply_pool(void)
{
   HeliosVenusMemoryAllocationV1 r;
   memset(&r, 0, sizeof(r));
   r.magic = HELIOS_HVM1_MAGIC;
   r.abi_version = (uint16_t)HELIOS_HVM1_ABI_VERSION;
   r.struct_size = (uint16_t)HELIOS_HVM1_SIZE;
   r.package_generation = HELIOS_PACKAGE_GENERATION;
   r.byte_size = HELIOS_HVM1_REPLY_POOL_BYTES;
   r.role = HELIOS_HVM1_ROLE_REPLY_POOL;
   r.access = HELIOS_HVM1_ACCESS_CPU_READ | HELIOS_HVM1_ACCESS_HOST_WRITE;
   r.cache_policy = HELIOS_HVM1_CACHE_WRITE_COMBINED;
   return r;
}

/* Create one allocation and report what came back in the caller's buffer.
 *
 * `create_resource` sets D3DKMT_CREATEALLOCATIONFLAGS::CreateResource;
 * `resource_private` additionally passes the same record as the RESOURCE-level
 * private data, which is what the D3D11 UMD does (umd/src/forward/resource.rs
 * sets both `alloc.pPrivateDriverData` and `allocation_info.pPrivateDriverData`).
 * `use_v1_thunk` calls D3DKMTCreateAllocation with the D3DDDI_ALLOCATIONINFO
 * (v1) array instead of D3DKMTCreateAllocation2 with the v2 array. */
static NTSTATUS
create_one(D3DKMT_HANDLE device, void *record, UINT record_bytes,
           int create_resource, int resource_private, int use_v1_thunk,
           D3DKMT_HANDLE *out_alloc, D3DKMT_HANDLE *out_resource)
{
   D3DDDI_ALLOCATIONINFO2 info2;
   D3DDDI_ALLOCATIONINFO info1;
   D3DKMT_CREATEALLOCATION ca;

   memset(&info2, 0, sizeof(info2));
   memset(&info1, 0, sizeof(info1));
   memset(&ca, 0, sizeof(ca));

   ca.hDevice = device;
   ca.NumAllocations = 1;
   ca.Flags.CreateResource = create_resource ? 1 : 0;
   if (resource_private) {
      ca.pPrivateDriverData = record;
      ca.PrivateDriverDataSize = record_bytes;
   }
   if (use_v1_thunk) {
      info1.pSystemMem = NULL;
      info1.pPrivateDriverData = record;
      info1.PrivateDriverDataSize = record_bytes;
      ca.pAllocationInfo = &info1;
   } else {
      info2.pSystemMem = NULL;
      info2.pPrivateDriverData = record;
      info2.PrivateDriverDataSize = record_bytes;
      ca.pAllocationInfo2 = &info2;
   }

   NTSTATUS st = use_v1_thunk ? D3DKMTCreateAllocation(&ca)
                              : D3DKMTCreateAllocation2(&ca);
   *out_alloc = st == STATUS_SUCCESS_NT
                   ? (use_v1_thunk ? info1.hAllocation : info2.hAllocation)
                   : 0;
   *out_resource = st == STATUS_SUCCESS_NT ? ca.hResource : 0;
   return st;
}

static void
destroy(D3DKMT_HANDLE device, D3DKMT_HANDLE alloc, D3DKMT_HANDLE resource)
{
   if (!alloc && !resource)
      return;
   D3DKMT_DESTROYALLOCATION2 da;
   memset(&da, 0, sizeof(da));
   da.hDevice = device;
   da.hResource = resource;
   if (!resource) {
      da.phAllocationList = &alloc;
      da.AllocationCount = 1;
   }
   IGNORE_STATUS(D3DKMTDestroyAllocation2(&da));
}

/* One HWA2 variant on its own device, so no variant can inherit another's
 * per-device state. */
static void
hwa2_variant(D3DKMT_HANDLE adapter, const char *name, int create_resource,
             int resource_private, int use_v1_thunk)
{
   D3DKMT_CREATEDEVICE cd;
   memset(&cd, 0, sizeof(cd));
   cd.hAdapter = adapter;
   if (D3DKMTCreateDevice(&cd) != STATUS_SUCCESS_NT) {
      printf("  %-34s CreateDevice FAILED\n", name);
      g_fail++;
      return;
   }

   HeliosWddmAllocationDescV2 d = hwa2_buffer(create_resource);
   D3DKMT_HANDLE alloc = 0, resource = 0;
   NTSTATUS st = create_one(cd.hDevice, &d, (UINT)sizeof(d), create_resource,
                            resource_private, use_v1_thunk, &alloc, &resource);
   if (st != STATUS_SUCCESS_NT) {
      printf("  %-34s status=0x%08x (create refused)\n", name, (unsigned)st);
      g_fail++;
   } else {
      printf("  %-34s alloc=0x%08x res=0x%08x  allocation_generation=%llu  %s\n",
             name, alloc, resource,
             (unsigned long long)d.allocation_generation,
             d.allocation_generation ? "STAMPED" : "*** ZERO ***");
      if (!d.allocation_generation)
         g_fail++;
   }
   destroy(cd.hDevice, alloc, resource);

   D3DKMT_DESTROYDEVICE dd;
   memset(&dd, 0, sizeof(dd));
   dd.hDevice = cd.hDevice;
   IGNORE_STATUS(D3DKMTDestroyDevice(&dd));
}

/* The HVM1 control arm, in the same process and the same run, so "HVM1 comes
 * back zero" and whatever the HWA2 arms do are one measurement rather than two. */
static void
hvm1_variant(D3DKMT_HANDLE adapter, const char *name, int create_resource,
             int resource_private, int use_v1_thunk)
{
   D3DKMT_CREATEDEVICE cd;
   memset(&cd, 0, sizeof(cd));
   cd.hAdapter = adapter;
   if (D3DKMTCreateDevice(&cd) != STATUS_SUCCESS_NT) {
      printf("  %-34s CreateDevice FAILED\n", name);
      g_fail++;
      return;
   }

   HeliosVenusMemoryAllocationV1 r = hvm1_reply_pool();
   D3DKMT_HANDLE alloc = 0, resource = 0;
   NTSTATUS st = create_one(cd.hDevice, &r, (UINT)sizeof(r), create_resource,
                            resource_private, use_v1_thunk, &alloc, &resource);
   if (st != STATUS_SUCCESS_NT) {
      printf("  %-34s status=0x%08x (create refused)\n", name, (unsigned)st);
   } else {
      printf("  %-34s alloc=0x%08x res=0x%08x  object_generation=%llu shift=%u "
             "align=%llu  %s\n",
             name, alloc, resource, (unsigned long long)r.object_generation,
             r.segment_page_shift, (unsigned long long)r.allocation_alignment,
             r.object_generation ? "STAMPED" : "*** ZERO ***");
      if (!r.object_generation)
         g_fail++;
   }
   destroy(cd.hDevice, alloc, resource);

   D3DKMT_DESTROYDEVICE dd;
   memset(&dd, 0, sizeof(dd));
   dd.hDevice = cd.hDevice;
   IGNORE_STATUS(D3DKMTDestroyDevice(&dd));
}

static int
is_helios(D3DKMT_HANDLE adapter)
{
   D3DKMT_CREATEDEVICE cd;
   memset(&cd, 0, sizeof(cd));
   cd.hAdapter = adapter;
   if (D3DKMTCreateDevice(&cd) != STATUS_SUCCESS_NT)
      return 0;
   D3DKMT_CREATECONTEXT ctx;
   memset(&ctx, 0, sizeof(ctx));
   ctx.hDevice = cd.hDevice;
   int helios = 0;
   if (D3DKMTCreateContext(&ctx) == STATUS_SUCCESS_NT) {
      helios = ctx.CommandBufferSize == HELIOS_LEGACY_DMA_BUFFER_BYTES;
      D3DKMT_DESTROYCONTEXT dc;
      memset(&dc, 0, sizeof(dc));
      dc.hContext = ctx.hContext;
      IGNORE_STATUS(D3DKMTDestroyContext(&dc));
   }
   D3DKMT_DESTROYDEVICE dd;
   memset(&dd, 0, sizeof(dd));
   dd.hDevice = cd.hDevice;
   IGNORE_STATUS(D3DKMTDestroyDevice(&dd));
   return helios;
}

int
main(void)
{
   printf("hwa2_writeback_probe: create-time private-data write-back, by call "
          "shape (package generation 0x%llx)\n",
          (unsigned long long)HELIOS_PACKAGE_GENERATION);

   D3DKMT_ENUMADAPTERS2 ea;
   memset(&ea, 0, sizeof(ea));
   if (D3DKMTEnumAdapters2(&ea) != STATUS_SUCCESS_NT || ea.NumAdapters == 0)
      return 2;
   ea.pAdapters =
      (D3DKMT_ADAPTERINFO *)calloc(ea.NumAdapters, sizeof(D3DKMT_ADAPTERINFO));
   if (!ea.pAdapters)
      return 2;
   if (D3DKMTEnumAdapters2(&ea) != STATUS_SUCCESS_NT)
      return 2;

   int found = 0;
   for (UINT i = 0; i < ea.NumAdapters; i++) {
      D3DKMT_HANDLE a = ea.pAdapters[i].hAdapter;
      if (is_helios(a)) {
         found++;
         printf("adapter %u is Helios\n\n", i);
         printf(" HWA2 (168 bytes, buffer kind):\n");
         hwa2_variant(a, "A bare / CreateAllocation2", 0, 0, 0);
         hwa2_variant(a, "B resource / CreateAllocation2", 1, 0, 0);
         hwa2_variant(a, "C resource+respriv / Create2", 1, 1, 0);
         hwa2_variant(a, "D bare / CreateAllocation(v1)", 0, 0, 1);
         hwa2_variant(a, "E resource+respriv / v1", 1, 1, 1);
         printf("\n HVM1 (64 bytes, role-1 reply pool):\n");
         hvm1_variant(a, "F bare / CreateAllocation2", 0, 0, 0);
         hvm1_variant(a, "G resource / CreateAllocation2", 1, 0, 0);
         hvm1_variant(a, "H bare / CreateAllocation(v1)", 0, 0, 1);
      }
      D3DKMT_CLOSEADAPTER ca;
      memset(&ca, 0, sizeof(ca));
      ca.hAdapter = a;
      IGNORE_STATUS(D3DKMTCloseAdapter(&ca));
   }

   if (found != 1) {
      printf("\nINCONCLUSIVE - expected exactly one Helios adapter, found %d\n",
             found);
      return 2;
   }
   printf("\n%d variant(s) did not come back stamped\n", g_fail);
   return 0;
}
