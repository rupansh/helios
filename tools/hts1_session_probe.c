// hts1_session_probe.c — K5 acceptance on the target.
//
// Performs the exact D3DKMT sequence mesa unit A1 performs
// (`icd/mesa/src/virtio/vulkan/vn_helios_translation_session.c:539-784`), plus
// the refusals K5 owes, and checks each outcome against a stated expectation.
// It exists because A1 has NO CALLER in the ICD — `helios_translation_session_create`
// is compiled in and invoked by nothing — so nothing else on this machine can
// reach the kernel's HVC1/HTS1 path until mesa A3 lands. Without this probe K5
// would be "implemented but never exercised".
//
// State-neutral: every object it creates it destroys, and it renders nothing.
// Exit code 0 means every expectation held; 1 means at least one did not.
//
// Build (win11, WinLibs gcc):
//   gcc -O2 -o hts1_session_probe.exe hts1_session_probe.c
//       -I Z:\protocol\include -I Z:\icd\win-build\wdk-include -lgdi32

#include <stdio.h>
#include <string.h>
#include <windows.h>

#ifndef _NTDEF_
typedef LONG NTSTATUS, *PNTSTATUS;
#endif
#include <d3dkmthk.h>

#include "helios_native_render.h"
#include "helios_translation_session.h"

/* `(void)` does not suppress warn_unused_result, and every D3DKMT teardown call
 * is declared with it. Same macro, same reason, as A1's own teardown. */
#define IGNORE_STATUS(call)                                                    \
   do {                                                                        \
      NTSTATUS ignored_ = (call);                                              \
      (void)ignored_;                                                          \
   } while (0)

#define STATUS_SUCCESS_NT ((NTSTATUS)0x00000000L)
#define STATUS_PENDING_NT ((NTSTATUS)0x00000103L)

static int g_pass;
static int g_fail;

static void
check(int ok, const char *what, const char *detail)
{
   if (ok) {
      g_pass++;
      printf("  PASS  %s\n", what);
   } else {
      g_fail++;
      printf("  FAIL  %s -- %s\n", what, detail ? detail : "");
   }
}

static HeliosVulkanContextV1
hvc1_control(void)
{
   HeliosVulkanContextV1 r;
   memset(&r, 0, sizeof(r));
   r.magic = HELIOS_HVC1_MAGIC;
   r.abi_version = (uint16_t)HELIOS_HVC1_ABI_VERSION;
   r.struct_size = (uint16_t)HELIOS_HVC1_SIZE;
   r.package_generation = HELIOS_PACKAGE_GENERATION;
   r.capset = HELIOS_NATIVE_RENDER_CAPSET;
   r.mode = HELIOS_HVC1_MODE_FINITE_HNR2_RENDER;
   r.queue_family = HELIOS_HVC1_CONTROL_ORDINAL;
   r.queue_index = HELIOS_HVC1_CONTROL_ORDINAL;
   return r;
}

/* One D3DKMTCreateContext with the supplied private data. `out` receives the
 * whole call struct so the caller can read the returned ContextInfo mirror. */
static NTSTATUS
create_context(D3DKMT_HANDLE device, const void *pdd, UINT pdd_bytes,
               D3DKMT_CREATECONTEXT *out)
{
   memset(out, 0, sizeof(*out));
   out->hDevice = device;
   out->NodeOrdinal = HELIOS_HVC1_NODE_ORDINAL;
   out->EngineAffinity = HELIOS_HVC1_ENGINE_AFFINITY;
   out->Flags.Value = HELIOS_HVC1_CREATE_CONTEXT_FLAGS;
   out->ClientHint = D3DKMT_CLIENTHINT_VULKAN;
   out->pPrivateDriverData = (VOID *)pdd;
   out->PrivateDriverDataSize = pdd_bytes;
   return D3DKMTCreateContext(out);
}

static void
destroy_context(D3DKMT_HANDLE h)
{
   if (!h)
      return;
   D3DKMT_DESTROYCONTEXT dc;
   memset(&dc, 0, sizeof(dc));
   dc.hContext = h;
   IGNORE_STATUS(D3DKMTDestroyContext(&dc));
}

/* Helios's LEGACY (no-private-data) context profile, which K5 preserves
 * byte-for-byte and which no inbox driver matches: 256-KiB DMA buffer, 256-entry
 * lists. Used to identify the adapter INDEPENDENTLY of anything K5 changes — an
 * "is it Helios" test that keyed on K5's own behaviour would report "not Helios"
 * instead of failing when K5 was broken. Measured pre-K5: adapter 0 is
 * 262144/256/256, the other two are 4096/16/16. */
#define HELIOS_LEGACY_DMA_BUFFER_BYTES 262144u

/* Everything K5 owes on one adapter. Returns 1 if this is the Helios adapter. */
static int
probe_adapter(D3DKMT_HANDLE adapter, UINT index)
{
   D3DKMT_CREATEDEVICE cd;
   memset(&cd, 0, sizeof(cd));
   cd.hAdapter = adapter;
   if (D3DKMTCreateDevice(&cd) != STATUS_SUCCESS_NT)
      return 0;
   const D3DKMT_HANDLE device = cd.hDevice;

   /* ── Identify the adapter, before testing anything K5 owns. ─────────── */
   {
      D3DKMT_CREATECONTEXT legacy;
      NTSTATUS s0 = create_context(device, NULL, 0, &legacy);
      const int is_helios = s0 == STATUS_SUCCESS_NT &&
                            legacy.CommandBufferSize ==
                               HELIOS_LEGACY_DMA_BUFFER_BYTES;
      if (s0 == STATUS_SUCCESS_NT)
         destroy_context(legacy.hContext);
      if (!is_helios) {
         D3DKMT_DESTROYDEVICE dd;
         memset(&dd, 0, sizeof(dd));
         dd.hDevice = device;
         IGNORE_STATUS(D3DKMTDestroyDevice(&dd));
         printf("adapter %u: not Helios (legacy context status=0x%08x cmd=%u)\n",
                index, (unsigned)s0,
                s0 == STATUS_SUCCESS_NT ? legacy.CommandBufferSize : 0);
         return 0;
      }
   }

   /* ── A. the HVC1 control context ────────────────────────────────────── */
   HeliosVulkanContextV1 hvc1 = hvc1_control();
   D3DKMT_CREATECONTEXT control;
   NTSTATUS st = create_context(device, &hvc1, sizeof(hvc1), &control);
   if (st != STATUS_SUCCESS_NT) {
      char cw[96];
      snprintf(cw, sizeof(cw), "status=0x%08x", (unsigned)st);
      check(0, "A0: the HVC1 control context is admitted", cw);
      D3DKMT_DESTROYDEVICE dd;
      memset(&dd, 0, sizeof(dd));
      dd.hDevice = device;
      IGNORE_STATUS(D3DKMTDestroyDevice(&dd));
      return 1;
   }

   printf("adapter %u: Helios; HVC1 control context ADMITTED\n", index);
   g_pass++;
   printf("  cmd=%p/%u alloc=%p/%u patch=%p/%u\n", control.pCommandBuffer,
          control.CommandBufferSize, (void *)control.pAllocationList,
          control.AllocationListSize, (void *)control.pPatchLocationList,
          control.PatchLocationListSize);

   char why[192];
   snprintf(why, sizeof(why), "CommandBufferSize=%u, want >= %u",
            control.CommandBufferSize, (unsigned)HELIOS_HVC1_DMA_BUFFER_BYTES);
   check(control.pCommandBuffer != NULL &&
            control.CommandBufferSize >= HELIOS_HVC1_DMA_BUFFER_BYTES,
         "A1: the DMA buffer meets the advertised 256-KiB minimum", why);
   snprintf(why, sizeof(why), "AllocationListSize=%u, want >= %u",
            control.AllocationListSize,
            (unsigned)HELIOS_HVC1_ALLOCATION_LIST_ENTRIES);
   check(control.pAllocationList != NULL &&
            control.AllocationListSize >= HELIOS_HVC1_ALLOCATION_LIST_ENTRIES,
         "A2: the allocation list meets the 4096-entry minimum", why);
   snprintf(why, sizeof(why), "PatchLocationListSize=%u, want >= %u",
            control.PatchLocationListSize,
            (unsigned)HELIOS_HVC1_PATCH_LOCATION_ENTRIES);
   check(control.pPatchLocationList != NULL &&
            control.PatchLocationListSize >= HELIOS_HVC1_PATCH_LOCATION_ENTRIES,
         "A3: the patch list meets the 4096-entry minimum", why);

   /* ── B. a second control context on the same raw device ─────────────── */
   {
      HeliosVulkanContextV1 again = hvc1_control();
      D3DKMT_CREATECONTEXT second;
      NTSTATUS s2 = create_context(device, &again, sizeof(again), &second);
      snprintf(why, sizeof(why), "status=0x%08x (expected a refusal)",
               (unsigned)s2);
      check(s2 != STATUS_SUCCESS_NT,
            "B: a second HVC1 control context on one raw device is refused",
            why);
      if (s2 == STATUS_SUCCESS_NT)
         destroy_context(second.hContext);
   }

   /* ── C. an HVC1 whose mode is not the finite HNR2 Render mode ───────── */
   {
      HeliosVulkanContextV1 bad = hvc1_control();
      bad.mode = HELIOS_HVC1_MODE_FINITE_HNR2_RENDER + 7;
      D3DKMT_CREATECONTEXT ctx;
      NTSTATUS s3 = create_context(device, &bad, sizeof(bad), &ctx);
      snprintf(why, sizeof(why), "status=0x%08x (expected a refusal)",
               (unsigned)s3);
      check(s3 != STATUS_SUCCESS_NT, "C: an unsupported HVC1 mode is refused",
            why);
      if (s3 == STATUS_SUCCESS_NT)
         destroy_context(ctx.hContext);
   }

   /* ── D. an HVC1 queue context: K6's arm, refused until it exists ────── */
   {
      HeliosVulkanContextV1 queue = hvc1_control();
      queue.queue_family = 0;
      queue.queue_index = 0;
      D3DKMT_CREATECONTEXT ctx;
      NTSTATUS s4 = create_context(device, &queue, sizeof(queue), &ctx);
      snprintf(why, sizeof(why), "status=0x%08x (expected a refusal)",
               (unsigned)s4);
      check(s4 != STATUS_SUCCESS_NT,
            "D: an HVC1 QUEUE context is refused (K6 owns it)", why);
      if (s4 == STATUS_SUCCESS_NT)
         destroy_context(ctx.hContext);
   }

   /* ── E. 32 bytes that are not an HVC1 fall through to the legacy arm ── */
   {
      HeliosVulkanContextV1 wrong = hvc1_control();
      wrong.magic ^= 1u;
      D3DKMT_CREATECONTEXT ctx;
      NTSTATUS s5 = create_context(device, &wrong, sizeof(wrong), &ctx);
      snprintf(why, sizeof(why), "status=0x%08x allocList=%u", (unsigned)s5,
               s5 == STATUS_SUCCESS_NT ? ctx.AllocationListSize : 0);
      /* The legacy profile is the small GDI one; the point is that an
       * unrecognised record cannot take down a context the desktop needs. */
      check(s5 == STATUS_SUCCESS_NT &&
               ctx.AllocationListSize < HELIOS_HVC1_ALLOCATION_LIST_ENTRIES,
            "E: a 32-byte non-HVC1 record gets the LEGACY context, not a refusal",
            why);
      if (s5 == STATUS_SUCCESS_NT)
         destroy_context(ctx.hContext);
   }

   /* ── F. an ordinary context with no private data is unchanged ───────── */
   {
      D3DKMT_CREATECONTEXT ctx;
      NTSTATUS s6 = create_context(device, NULL, 0, &ctx);
      snprintf(why, sizeof(why), "status=0x%08x allocList=%u", (unsigned)s6,
               s6 == STATUS_SUCCESS_NT ? ctx.AllocationListSize : 0);
      check(s6 == STATUS_SUCCESS_NT &&
               ctx.AllocationListSize < HELIOS_HVC1_ALLOCATION_LIST_ENTRIES,
            "F: a no-private-data context still gets the legacy profile", why);
      if (s6 == STATUS_SUCCESS_NT)
         destroy_context(ctx.hContext);
   }

   /* ── G. an HQA1 naming a capability no session holds ────────────────── */
   {
      HeliosQueueAttachV1 hqa1;
      memset(&hqa1, 0, sizeof(hqa1));
      hqa1.magic = HELIOS_HQA1_MAGIC;
      hqa1.abi_version = (uint16_t)HELIOS_HQA1_ABI_VERSION;
      hqa1.struct_size = (uint16_t)HELIOS_HQA1_SIZE;
      hqa1.package_generation = HELIOS_PACKAGE_GENERATION;
      hqa1.session_generation = 0xDEADBEEFull;
      hqa1.capability_low = 0x1111111111111111ull;
      hqa1.capability_high = 0x2222222222222222ull;
      hqa1.endpoint_id = 1;
      hqa1.engine_class = HELIOS_ENGINE_CLASS_GRAPHICS;
      hqa1.context_generation = 1;
      hqa1.flags = HELIOS_HQA1_FLAG_D3D11_PHYSICAL;
      D3DKMT_CREATECONTEXT ctx;
      NTSTATUS s7 = create_context(device, &hqa1, sizeof(hqa1), &ctx);
      snprintf(why, sizeof(why), "status=0x%08x (expected a refusal)",
               (unsigned)s7);
      check(s7 != STATUS_SUCCESS_NT,
            "G: an HQA1 with a forged capability finds no session", why);
      if (s7 == STATUS_SUCCESS_NT)
         destroy_context(ctx.hContext);
   }

   /* ── H. the role-1 HVM1 reply pool, exactly as A1 creates it ────────── */
   D3DKMT_HANDLE pool = 0;
   D3DKMT_HANDLE paging_queue = 0;
   {
      HeliosVenusMemoryAllocationV1 hvm1;
      memset(&hvm1, 0, sizeof(hvm1));
      hvm1.magic = HELIOS_HVM1_MAGIC;
      hvm1.abi_version = (uint16_t)HELIOS_HVM1_ABI_VERSION;
      hvm1.struct_size = (uint16_t)HELIOS_HVM1_SIZE;
      hvm1.package_generation = HELIOS_PACKAGE_GENERATION;
      hvm1.byte_size = HELIOS_HVM1_REPLY_POOL_BYTES;
      hvm1.role = HELIOS_HVM1_ROLE_REPLY_POOL;
      hvm1.access = HELIOS_HVM1_ACCESS_CPU_READ | HELIOS_HVM1_ACCESS_HOST_WRITE;
      hvm1.cache_policy = HELIOS_HVM1_CACHE_WRITE_COMBINED;

      D3DDDI_ALLOCATIONINFO2 info;
      memset(&info, 0, sizeof(info));
      info.pSystemMem = NULL;
      info.pPrivateDriverData = &hvm1;
      info.PrivateDriverDataSize = sizeof(hvm1);

      D3DKMT_CREATEALLOCATION ca;
      memset(&ca, 0, sizeof(ca));
      ca.hDevice = device;
      ca.NumAllocations = 1;
      ca.pAllocationInfo2 = &info;
      NTSTATUS s8 = D3DKMTCreateAllocation2(&ca);
      snprintf(why, sizeof(why), "status=0x%08x", (unsigned)s8);
      check(s8 == STATUS_SUCCESS_NT,
            "H1: the 64-MiB role-1 HVM1 reply pool is created", why);
      if (s8 == STATUS_SUCCESS_NT) {
         pool = info.hAllocation;
         snprintf(why, sizeof(why),
                  "object_generation=%llu segment_page_shift=%u alignment=%llu",
                  (unsigned long long)hvm1.object_generation,
                  hvm1.segment_page_shift,
                  (unsigned long long)hvm1.allocation_alignment);
         check(hvm1.object_generation != 0 &&
                  hvm1.segment_page_shift == HELIOS_HVM1_SEGMENT_PAGE_SHIFT,
               "H2: the KMD wrote back a nonzero generation and the page shift",
               why);
      }
   }

   if (pool) {
      D3DKMT_CREATEPAGINGQUEUE pq;
      memset(&pq, 0, sizeof(pq));
      pq.hDevice = device;
      pq.Priority = D3DDDI_PAGINGQUEUE_PRIORITY_NORMAL;
      NTSTATUS s9 = D3DKMTCreatePagingQueue(&pq);
      if (s9 == STATUS_SUCCESS_NT) {
         paging_queue = pq.hPagingQueue;
         D3DDDI_MAKERESIDENT mr;
         memset(&mr, 0, sizeof(mr));
         mr.hPagingQueue = paging_queue;
         mr.NumAllocations = 1;
         mr.AllocationList = &pool;
         NTSTATUS s10 = D3DKMTMakeResident(&mr);
         snprintf(why, sizeof(why), "status=0x%08x", (unsigned)s10);
         check(s10 == STATUS_SUCCESS_NT || s10 == STATUS_PENDING_NT,
               "H3: the pool is made resident", why);
         if ((s10 == STATUS_SUCCESS_NT || s10 == STATUS_PENDING_NT) &&
             mr.PagingFenceValue) {
            const UINT64 v = mr.PagingFenceValue;
            D3DKMT_WAITFORSYNCHRONIZATIONOBJECTFROMCPU pw;
            memset(&pw, 0, sizeof(pw));
            pw.hDevice = device;
            pw.ObjectCount = 1;
            pw.ObjectHandleArray = &pq.hSyncObject;
            pw.FenceValueArray = &v;
            IGNORE_STATUS(D3DKMTWaitForSynchronizationObjectFromCpu(&pw));
         }

         D3DKMT_LOCK2 lk;
         memset(&lk, 0, sizeof(lk));
         lk.hDevice = device;
         lk.hAllocation = pool;
         NTSTATUS s11 = D3DKMTLock2(&lk);
         snprintf(why, sizeof(why), "status=0x%08x pData=%p", (unsigned)s11,
                  lk.pData);
         check(s11 == STATUS_SUCCESS_NT && lk.pData != NULL,
               "H4: the pool Lock2-maps to a CPU virtual address", why);
         if (s11 == STATUS_SUCCESS_NT && lk.pData) {
            /* Prove the mapping is real and writable end to end. WRITE_COMBINED,
             * so read back through the same pointer only. */
            volatile unsigned char *p = (volatile unsigned char *)lk.pData;
            p[0] = 0xA5;
            p[HELIOS_HVM1_REPLY_SLOT_BYTES - 1] = 0x5A;
            p[HELIOS_HVM1_REPLY_POOL_BYTES - 1] = 0xC3;
            snprintf(why, sizeof(why), "read back %02x %02x %02x", p[0],
                     p[HELIOS_HVM1_REPLY_SLOT_BYTES - 1],
                     p[HELIOS_HVM1_REPLY_POOL_BYTES - 1]);
            check(p[0] == 0xA5 && p[HELIOS_HVM1_REPLY_SLOT_BYTES - 1] == 0x5A &&
                     p[HELIOS_HVM1_REPLY_POOL_BYTES - 1] == 0xC3,
                  "H5: the first byte, the first slot boundary and the last "
                  "byte of the pool are all writable",
                  why);
            D3DKMT_UNLOCK2 ul;
            memset(&ul, 0, sizeof(ul));
            ul.hDevice = device;
            ul.hAllocation = pool;
            IGNORE_STATUS(D3DKMTUnlock2(&ul));
         }
      } else {
         snprintf(why, sizeof(why), "CreatePagingQueue status=0x%08x",
                  (unsigned)s9);
         check(0, "H3: the pool is made resident", why);
      }
   }

   /* ── teardown, in A1's order ────────────────────────────────────────── */
   if (pool) {
      D3DKMT_DESTROYALLOCATION2 da;
      memset(&da, 0, sizeof(da));
      da.hDevice = device;
      da.phAllocationList = &pool;
      da.AllocationCount = 1;
      IGNORE_STATUS(D3DKMTDestroyAllocation2(&da));
   }
   if (paging_queue) {
      D3DDDI_DESTROYPAGINGQUEUE dq;
      memset(&dq, 0, sizeof(dq));
      dq.hPagingQueue = paging_queue;
      IGNORE_STATUS(D3DKMTDestroyPagingQueue(&dq));
   }
   destroy_context(control.hContext);
   {
      D3DKMT_DESTROYDEVICE dd;
      memset(&dd, 0, sizeof(dd));
      dd.hDevice = device;
      IGNORE_STATUS(D3DKMTDestroyDevice(&dd));
   }
   return 1;
}

int
main(void)
{
   printf("hts1_session_probe: K5 acceptance (package generation 0x%llx)\n",
          (unsigned long long)HELIOS_PACKAGE_GENERATION);

   D3DKMT_ENUMADAPTERS2 ea;
   memset(&ea, 0, sizeof(ea));
   NTSTATUS st = D3DKMTEnumAdapters2(&ea);
   if (st != STATUS_SUCCESS_NT || ea.NumAdapters == 0) {
      printf("EnumAdapters2 st=0x%08x n=%u\n", (unsigned)st,
             (unsigned)ea.NumAdapters);
      return 2;
   }
   ea.pAdapters =
      (D3DKMT_ADAPTERINFO *)calloc(ea.NumAdapters, sizeof(D3DKMT_ADAPTERINFO));
   if (!ea.pAdapters)
      return 2;
   st = D3DKMTEnumAdapters2(&ea);
   if (st != STATUS_SUCCESS_NT) {
      printf("EnumAdapters2(2) st=0x%08x\n", (unsigned)st);
      return 2;
   }

   int found = 0;
   for (UINT i = 0; i < ea.NumAdapters; i++) {
      found += probe_adapter(ea.pAdapters[i].hAdapter, i);
      D3DKMT_CLOSEADAPTER ca;
      memset(&ca, 0, sizeof(ca));
      ca.hAdapter = ea.pAdapters[i].hAdapter;
      IGNORE_STATUS(D3DKMTCloseAdapter(&ca));
   }

   printf("\n%d checks passed, %d failed, %d Helios adapter(s) probed\n", g_pass,
          g_fail, found);
   if (found != 1) {
      printf("hts1_session_probe: INCONCLUSIVE - expected exactly one Helios "
             "adapter, found %d\n",
             found);
      return 2;
   }
   printf("hts1_session_probe: %s\n", g_fail ? "FAIL" : "PASS");
   return g_fail ? 1 : 0;
}
