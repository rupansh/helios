// hnr2_native_probe.c — K6 acceptance on the target.
//
// Drives `DxgkDdiRender`'s HNR2 arm through the D3DKMT calls mesa unit A2 makes
// (`icd/mesa/src/virtio/vulkan/vn_helios_native_kmt.c:764` and `:906`), plus the
// refusals K6 owes, and checks each outcome against a stated expectation. It
// exists for the same reason `hts1_session_probe.c` does: A2's KMT half has no
// caller in the shipping ICD until mesa A3, so nothing else on this machine can
// reach the kernel's HNR2 path.
//
// ⛔ WHAT IT MUST NOT ASSERT, because K11 is absent by design:
//   - that `session_init` succeeds. It grants zero endpoints (there is no host
//     Venus context), `complete_init` refuses a zero grant, and the INIT Render
//     is therefore EXPECTED to be refused. `TsInitOk` stays 0; `TsInitRej` is
//     what moves. Grading it the other way reads a designed refusal as a K6
//     regression.
//   - that any HVR1 reply appears, that a SUBMIT_3D reached the host, or that
//     real GPU work ran. K6 stops at the host handoff.
//
// State-neutral: every object it creates it destroys, and it renders no pixels.
// Exit code 0 means every expectation held; 1 means at least one did not; 2 that
// exactly one Helios adapter was not found.
//
// Build (win11, WinLibs gcc):
//   gcc -O2 -o C:\Users\Rupansh\hnr2_native_probe.exe Z:\tools\hnr2_native_probe.c
//       -I Z:\protocol\include -I Z:\icd\win-build\wdk-include -lgdi32

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <windows.h>

#ifndef _NTDEF_
typedef LONG NTSTATUS, *PNTSTATUS;
#endif
#include <d3dkmthk.h>

#include "helios_native_render.h"
#include "helios_translation_session.h"

#define IGNORE_STATUS(call)                                                    \
   do {                                                                        \
      NTSTATUS ignored_ = (call);                                              \
      (void)ignored_;                                                          \
   } while (0)

#define STATUS_SUCCESS_NT ((NTSTATUS)0x00000000L)
#define STATUS_PENDING_NT ((NTSTATUS)0x00000103L)

/* Helios's LEGACY (no-private-data) context profile, which no inbox driver
 * matches. Identifying the adapter by something K6 does NOT own is what keeps a
 * broken K6 reporting FAIL instead of "not Helios". */
#define HELIOS_LEGACY_DMA_BUFFER_BYTES 262144u

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

/* ── context helpers ────────────────────────────────────────────────────── */

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

static HeliosVulkanContextV1
hvc1(uint32_t family, uint32_t index)
{
   HeliosVulkanContextV1 r;
   memset(&r, 0, sizeof(r));
   r.magic = HELIOS_HVC1_MAGIC;
   r.abi_version = (uint16_t)HELIOS_HVC1_ABI_VERSION;
   r.struct_size = (uint16_t)HELIOS_HVC1_SIZE;
   r.package_generation = HELIOS_PACKAGE_GENERATION;
   r.capset = HELIOS_NATIVE_RENDER_CAPSET;
   r.mode = HELIOS_HVC1_MODE_FINITE_HNR2_RENDER;
   r.queue_family = family;
   r.queue_index = index;
   return r;
}

/* ── HNR2 fragments ─────────────────────────────────────────────────────── */

/* One fragment header, written straight into the dxgkrnl-mapped command buffer.
 * The canonical layout `protocol/src/native_render.rs`'s `Hnr2CommandLayout`
 * fixes is header, then the (absent) tables, then the payload — so with no
 * tables the payload starts at exactly the header size. */
static void
fragment(HeliosNativeRenderV2 *h, uint64_t token, uint16_t index, uint16_t count,
         uint32_t chunk)
{
   memset(h, 0, sizeof(*h));
   h->magic = HELIOS_HNR2_MAGIC;
   h->abi_version = (uint16_t)HELIOS_HNR2_ABI_VERSION;
   h->header_size = (uint16_t)HELIOS_HNR2_HEADER_SIZE;
   h->package_generation = HELIOS_PACKAGE_GENERATION;
   h->batch_token = token;
   h->total_payload_bytes = (uint64_t)chunk * count;
   h->fragment_payload_offset = (uint64_t)chunk * index;
   h->fragment_payload_bytes = chunk;
   h->fragment_index = index;
   h->fragment_count = count;
   h->reply_allocation_list_index = HELIOS_HNR2_NO_REPLY_ALLOCATION_INDEX;
   if (index == 0)
      h->flags |= HELIOS_HNR2_FLAG_BEGIN;
   if (index + 1 == count)
      h->flags |= HELIOS_HNR2_FLAG_COMMIT;
}

/* Write `h` plus `chunk` zero payload bytes into the context's command buffer
 * and submit exactly that many bytes. Returns the Render status.
 *
 * ⚠ RE-ADOPTS THE RETURNED BUFFERS, like A2 does on every call including the
 * failure path (`helios_native_adopt`): dxgkrnl may hand back different
 * pointers, and using the stale ones is a use-after-free in user mode. */
static NTSTATUS
submit_fragment(D3DKMT_CREATECONTEXT *ctx, const HeliosNativeRenderV2 *h,
                uint32_t chunk, uint32_t command_length_override,
                uint32_t patch_location_count)
{
   const uint32_t command_length =
      command_length_override ? command_length_override
                              : (uint32_t)HELIOS_HNR2_HEADER_SIZE + chunk;
   memcpy(ctx->pCommandBuffer, h, sizeof(*h));
   if (chunk)
      memset((char *)ctx->pCommandBuffer + HELIOS_HNR2_HEADER_SIZE, 0, chunk);

   D3DKMT_RENDER render;
   memset(&render, 0, sizeof(render));
   render.hContext = ctx->hContext;
   render.CommandOffset = 0;
   render.CommandLength = command_length;
   render.AllocationCount = 0;
   render.PatchLocationCount = patch_location_count;
   render.NewCommandBufferSize = HELIOS_HVC1_DMA_BUFFER_BYTES;
   render.NewAllocationListSize = HELIOS_HVC1_ALLOCATION_LIST_ENTRIES;
   render.NewPatchLocationListSize = HELIOS_HVC1_PATCH_LOCATION_ENTRIES;
   const NTSTATUS st = D3DKMTRender(&render);
   if (render.pNewCommandBuffer) {
      ctx->pCommandBuffer = render.pNewCommandBuffer;
      ctx->CommandBufferSize = render.NewCommandBufferSize;
   }
   if (render.pNewAllocationList) {
      ctx->pAllocationList = render.pNewAllocationList;
      ctx->AllocationListSize = render.NewAllocationListSize;
   }
   if (render.pNewPatchLocationList) {
      ctx->pPatchLocationList = render.pNewPatchLocationList;
      ctx->PatchLocationListSize = render.NewPatchLocationListSize;
   }
   return st;
}

/* ── the probe ──────────────────────────────────────────────────────────── */

static int
probe_adapter(D3DKMT_HANDLE adapter, UINT index)
{
   D3DKMT_CREATEDEVICE cd;
   memset(&cd, 0, sizeof(cd));
   cd.hAdapter = adapter;
   if (D3DKMTCreateDevice(&cd) != STATUS_SUCCESS_NT)
      return 0;
   const D3DKMT_HANDLE device = cd.hDevice;
   char why[224];

   /* Identify the adapter before testing anything K6 owns. */
   {
      D3DKMT_CREATECONTEXT legacy;
      NTSTATUS s0 = create_context(device, NULL, 0, &legacy);
      const int is_helios =
         s0 == STATUS_SUCCESS_NT &&
         legacy.CommandBufferSize == HELIOS_LEGACY_DMA_BUFFER_BYTES;
      if (s0 == STATUS_SUCCESS_NT)
         destroy_context(legacy.hContext);
      if (!is_helios) {
         D3DKMT_DESTROYDEVICE dd;
         memset(&dd, 0, sizeof(dd));
         dd.hDevice = device;
         IGNORE_STATUS(D3DKMTDestroyDevice(&dd));
         return 0;
      }
   }
   printf("adapter %u: Helios\n", index);

   /* ── A. the control context, which the queue context needs ──────────── */
   HeliosVulkanContextV1 control_pdd =
      hvc1(HELIOS_HVC1_CONTROL_ORDINAL, HELIOS_HVC1_CONTROL_ORDINAL);
   D3DKMT_CREATECONTEXT control;
   NTSTATUS sa = create_context(device, &control_pdd, sizeof(control_pdd), &control);
   snprintf(why, sizeof(why), "status=0x%08x", (unsigned)sa);
   check(sa == STATUS_SUCCESS_NT, "A: the HVC1 control context is admitted", why);
   if (sa != STATUS_SUCCESS_NT) {
      D3DKMT_DESTROYDEVICE dd;
      memset(&dd, 0, sizeof(dd));
      dd.hDevice = device;
      IGNORE_STATUS(D3DKMTDestroyDevice(&dd));
      return 1;
   }

   /* ── B. the QUEUE context — K6's arm, and today's whole point ────────── */
   HeliosVulkanContextV1 queue_pdd = hvc1(0, 0);
   D3DKMT_CREATECONTEXT queue;
   NTSTATUS sb = create_context(device, &queue_pdd, sizeof(queue_pdd), &queue);
   snprintf(why, sizeof(why), "status=0x%08x (was STATUS_NOT_SUPPORTED before K6)",
            (unsigned)sb);
   check(sb == STATUS_SUCCESS_NT, "B: an HVC1 QUEUE context is admitted", why);
   if (sb != STATUS_SUCCESS_NT) {
      destroy_context(control.hContext);
      D3DKMT_DESTROYDEVICE dd;
      memset(&dd, 0, sizeof(dd));
      dd.hDevice = device;
      IGNORE_STATUS(D3DKMTDestroyDevice(&dd));
      return 1;
   }
   printf("  queue cmd=%p/%u alloc=%p/%u patch=%p/%u\n", queue.pCommandBuffer,
          queue.CommandBufferSize, (void *)queue.pAllocationList,
          queue.AllocationListSize, (void *)queue.pPatchLocationList,
          queue.PatchLocationListSize);

   /* The ICD's own adoption minima (`vn_helios_native_kmt.c:658`): passing here
    * is what says mesa A3 will adopt this context rather than refuse it. */
   snprintf(why, sizeof(why), "cmd=%u alloc=%u patch=%u", queue.CommandBufferSize,
            queue.AllocationListSize, queue.PatchLocationListSize);
   check(queue.pCommandBuffer != NULL && queue.pAllocationList != NULL &&
            queue.pPatchLocationList != NULL &&
            queue.CommandBufferSize >= HELIOS_HVC1_DMA_BUFFER_BYTES &&
            queue.AllocationListSize >= HELIOS_HVC1_ALLOCATION_LIST_ENTRIES &&
            queue.PatchLocationListSize >= HELIOS_HVC1_PATCH_LOCATION_ENTRIES,
         "C: the queue context meets the ICD's adoption minima", why);

   /* ── D. the zero-length capacity renegotiation ───────────────────────── */
   {
      D3DKMT_RENDER render;
      memset(&render, 0, sizeof(render));
      render.hContext = queue.hContext;
      render.CommandLength = 0;
      render.AllocationCount = 0;
      render.PatchLocationCount = 0;
      render.NewCommandBufferSize = HELIOS_HVC1_DMA_BUFFER_BYTES;
      render.NewAllocationListSize = HELIOS_HVC1_ALLOCATION_LIST_ENTRIES;
      render.NewPatchLocationListSize = HELIOS_HVC1_PATCH_LOCATION_ENTRIES;
      render.Flags.ResizeAllocationList = 1;
      render.Flags.ResizePatchLocationList = 1;
      NTSTATUS sd = D3DKMTRender(&render);
      if (render.pNewCommandBuffer) {
         queue.pCommandBuffer = render.pNewCommandBuffer;
         queue.CommandBufferSize = render.NewCommandBufferSize;
      }
      if (render.pNewAllocationList)
         queue.pAllocationList = render.pNewAllocationList;
      if (render.pNewPatchLocationList)
         queue.pPatchLocationList = render.pNewPatchLocationList;
      snprintf(why, sizeof(why), "status=0x%08x", (unsigned)sd);
      check(sd == STATUS_SUCCESS_NT,
            "D: the zero-length resize Render is a counted no-op, not a batch", why);
   }

   /* ── E. one complete single-fragment batch ───────────────────────────── */
   uint64_t token = 1;
   {
      HeliosNativeRenderV2 h;
      fragment(&h, token, 0, 1, 64);
      NTSTATUS se = submit_fragment(&queue, &h, 64, 0, 0);
      snprintf(why, sizeof(why), "status=0x%08x", (unsigned)se);
      check(se == STATUS_SUCCESS_NT, "E: a one-fragment BEGIN|COMMIT batch is admitted",
            why);
   }

   /* ── F. a three-fragment batch, in order ─────────────────────────────── */
   {
      int all_ok = 1;
      unsigned last = 0;
      token++;
      for (uint16_t i = 0; i < 3; i++) {
         HeliosNativeRenderV2 h;
         fragment(&h, token, i, 3, 128);
         NTSTATUS sf = submit_fragment(&queue, &h, 128, 0, 0);
         if (sf != STATUS_SUCCESS_NT) {
            all_ok = 0;
            last = (unsigned)sf;
            break;
         }
      }
      snprintf(why, sizeof(why), "status=0x%08x", last);
      check(all_ok, "F: a three-fragment batch assembles in order", why);
   }

   /* ── G. the refusals, each followed by "the machine is still alive" ──── */
   struct {
      const char *what;
      uint16_t index;
      uint16_t count;
      uint64_t token;
      uint32_t chunk;
      uint32_t command_length_override;
      uint32_t patch_location_count;
   } bad[] = {
      /* A token at or below the watermark. */
      {"G1: a non-increasing batch token is refused", 0, 1, 1, 64, 0, 0},
      /* An interior fragment with nothing open. */
      {"G2: an interior fragment with no open batch is refused", 1, 3, 900, 64, 0, 0},
      /* HNR2 requires PatchLocationListInSize == 0. */
      {"G3: a nonzero input patch-location count is refused", 0, 1, 901, 64, 0, 4},
      /* CommandLength beyond what the header describes. */
      {"G4: a CommandLength that disagrees with the header is refused", 0, 1, 902, 64,
       (uint32_t)HELIOS_HNR2_HEADER_SIZE + 64 + 8, 0},
   };
   for (size_t i = 0; i < sizeof(bad) / sizeof(bad[0]); i++) {
      HeliosNativeRenderV2 h;
      fragment(&h, bad[i].token, bad[i].index, bad[i].count, bad[i].chunk);
      NTSTATUS sg = submit_fragment(&queue, &h, bad[i].chunk,
                                    bad[i].command_length_override,
                                    bad[i].patch_location_count);
      snprintf(why, sizeof(why), "status=0x%08x (expected a refusal)", (unsigned)sg);
      check(sg != STATUS_SUCCESS_NT, bad[i].what, why);
   }
   {
      /* The point of the negatives is that the context SURVIVES them. */
      token = 1000;
      HeliosNativeRenderV2 h;
      fragment(&h, token, 0, 1, 64);
      NTSTATUS sh = submit_fragment(&queue, &h, 64, 0, 0);
      snprintf(why, sizeof(why), "status=0x%08x", (unsigned)sh);
      check(sh == STATUS_SUCCESS_NT,
            "G5: the context still accepts a legal batch after four refusals", why);
   }

   /* ── H. the role-1 reply pool, exactly as A1 creates it ──────────────── */
   D3DKMT_HANDLE pool = 0;
   D3DKMT_HANDLE paging_queue = 0;
   uint64_t pool_generation = 0;
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
      info.pPrivateDriverData = &hvm1;
      info.PrivateDriverDataSize = sizeof(hvm1);

      D3DKMT_CREATEALLOCATION ca;
      memset(&ca, 0, sizeof(ca));
      ca.hDevice = device;
      ca.NumAllocations = 1;
      ca.pAllocationInfo2 = &info;
      NTSTATUS sh1 = D3DKMTCreateAllocation2(&ca);
      /* ⛔ The generation comes back from the OPEN, not the create: dxgkrnl
       * discards a create-time KMD write into `pPrivateDriverData` entirely
       * (`FINDINGS.md` F11). It is the number `open_allocation_identity`
       * publishes, so it is exactly what the use record must repeat. */
      if (sh1 == STATUS_SUCCESS_NT) {
         pool = info.hAllocation;
         pool_generation = hvm1.object_generation;
      }
      snprintf(why, sizeof(why), "status=0x%08x generation=%llu", (unsigned)sh1,
               (unsigned long long)pool_generation);
      check(sh1 == STATUS_SUCCESS_NT && pool_generation != 0,
            "H1: the role-1 reply pool is created and its generation published", why);
   }
   if (pool) {
      D3DKMT_CREATEPAGINGQUEUE pq;
      memset(&pq, 0, sizeof(pq));
      pq.hDevice = device;
      pq.Priority = D3DDDI_PAGINGQUEUE_PRIORITY_NORMAL;
      if (D3DKMTCreatePagingQueue(&pq) == STATUS_SUCCESS_NT) {
         paging_queue = pq.hPagingQueue;
         D3DDDI_MAKERESIDENT mr;
         memset(&mr, 0, sizeof(mr));
         mr.hPagingQueue = paging_queue;
         mr.NumAllocations = 1;
         mr.AllocationList = &pool;
         NTSTATUS sh2 = D3DKMTMakeResident(&mr);
         if ((sh2 == STATUS_SUCCESS_NT || sh2 == STATUS_PENDING_NT) &&
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
         snprintf(why, sizeof(why), "status=0x%08x", (unsigned)sh2);
         check(sh2 == STATUS_SUCCESS_NT || sh2 == STATUS_PENDING_NT,
               "H2: the reply pool is resident", why);
      }
   }

   /* ── I. the finite HTS1 INIT — ⛔ EXPECTED TO BE REFUSED ──────────────
    *
    * This is the whole reply-slot lifecycle in one call: HAS_REPLY checks out
    * slot 0, `session_init` runs, and it REFUSES because it grants zero
    * endpoints — the host Venus context is K11's and does not exist. A SUCCESS
    * here would mean a session went Live with nothing behind it, which is the
    * finding, not the pass. What proves the arm ran is `TsInitRej` and
    * `TsSlotRel` moving while `TsInitOk` and `TsSlotStuck` stay 0. */
   if (pool && pool_generation) {
      HeliosTranslationSessionInitV1 init;
      memset(&init, 0, sizeof(init));
      init.magic = HELIOS_HTS1_INIT_MAGIC;
      init.abi_version = (uint16_t)HELIOS_HTS1_ABI_VERSION;
      init.struct_size = (uint16_t)HELIOS_HTS1_INIT_SIZE;
      init.package_generation = HELIOS_PACKAGE_GENERATION;
      init.capset = HELIOS_NATIVE_RENDER_CAPSET;
      init.requested_endpoint_capacity = 1;

      const uint32_t table = HELIOS_HNR2_USE_RECORD_SIZE;
      const uint32_t payload = (uint32_t)sizeof(init);
      HeliosNativeRenderV2 h;
      fragment(&h, 1, 0, 1, payload);
      h.use_record_offset = HELIOS_HNR2_HEADER_SIZE;
      h.use_record_count = 1;
      h.fragment_payload_offset = 0;
      h.flags |= HELIOS_HNR2_FLAG_HAS_REPLY;
      h.reply_allocation_list_index = 0;
      h.reply_offset = 0;
      h.reply_capacity_bytes = (uint64_t)HELIOS_HVR1_HEADER_SIZE + 4096u;
      h.reply_slot_generation = 1;

      HeliosNativeRenderUse use;
      memset(&use, 0, sizeof(use));
      use.allocation_list_index = 0;
      use.access_flags = HELIOS_HNR2_ACCESS_WRITE;
      use.expected_allocation_generation = pool_generation;

      char *cmd = (char *)control.pCommandBuffer;
      memcpy(cmd, &h, sizeof(h));
      memcpy(cmd + HELIOS_HNR2_HEADER_SIZE, &use, sizeof(use));
      memcpy(cmd + HELIOS_HNR2_HEADER_SIZE + table, &init, sizeof(init));

      memset(&control.pAllocationList[0], 0, sizeof(control.pAllocationList[0]));
      control.pAllocationList[0].hAllocation = pool;
      control.pAllocationList[0].WriteOperation = 1;

      D3DKMT_RENDER render;
      memset(&render, 0, sizeof(render));
      render.hContext = control.hContext;
      render.CommandOffset = 0;
      render.CommandLength = (uint32_t)HELIOS_HNR2_HEADER_SIZE + table + payload;
      render.AllocationCount = 1;
      render.PatchLocationCount = 0;
      render.NewCommandBufferSize = HELIOS_HVC1_DMA_BUFFER_BYTES;
      render.NewAllocationListSize = HELIOS_HVC1_ALLOCATION_LIST_ENTRIES;
      render.NewPatchLocationListSize = HELIOS_HVC1_PATCH_LOCATION_ENTRIES;
      NTSTATUS si = D3DKMTRender(&render);
      if (render.pNewCommandBuffer)
         control.pCommandBuffer = render.pNewCommandBuffer;
      if (render.pNewAllocationList)
         control.pAllocationList = render.pNewAllocationList;
      if (render.pNewPatchLocationList)
         control.pPatchLocationList = render.pNewPatchLocationList;
      snprintf(why, sizeof(why),
               "status=0x%08x -- SUCCESS would mean a session went Live with no host "
               "context behind it",
               (unsigned)si);
      check(si != STATUS_SUCCESS_NT,
            "I: the HTS1 INIT control Render is REFUSED (K11 grants no endpoints)",
            why);

      /* The slot must be reusable afterwards. A1 takes the FIRST idle slot every
       * time, so if the refusal above left slot 0 in flight this second attempt
       * dies at `ControlRenderSlotBusy` instead of reaching `session_init` — and
       * the session would be permanently dead.
       *
       * ⚠ THE STATUS CANNOT TELL THE TWO APART, and pretending otherwise would
       * be a check that passes either way: both refusals are
       * STATUS_INVALID_PARAMETER, because `DxgkDdiRender`'s documented return
       * set is narrow and every K6 reason lives in a counter. This assertion is
       * therefore only the cheap half — "the second attempt still refuses rather
       * than succeeding". THE DISCRIMINATOR IS THE COUNTER PAIR, read after the
       * run: `TsInitRej` must have moved by 2 (both attempts reached the INIT)
       * with `TsCtlRej` unmoved and `TsSlotStuck` 0. `TsInitRej == 1` with
       * `TsCtlRej` naming 0x0A09 is the stuck slot. */
      h.batch_token = 2;
      h.reply_slot_generation = 2;
      memcpy(control.pCommandBuffer, &h, sizeof(h));
      memcpy((char *)control.pCommandBuffer + HELIOS_HNR2_HEADER_SIZE, &use, sizeof(use));
      memcpy((char *)control.pCommandBuffer + HELIOS_HNR2_HEADER_SIZE + table, &init,
             sizeof(init));
      memset(&control.pAllocationList[0], 0, sizeof(control.pAllocationList[0]));
      control.pAllocationList[0].hAllocation = pool;
      control.pAllocationList[0].WriteOperation = 1;
      memset(&render, 0, sizeof(render));
      render.hContext = control.hContext;
      render.CommandLength = (uint32_t)HELIOS_HNR2_HEADER_SIZE + table + payload;
      render.AllocationCount = 1;
      render.NewCommandBufferSize = HELIOS_HVC1_DMA_BUFFER_BYTES;
      render.NewAllocationListSize = HELIOS_HVC1_ALLOCATION_LIST_ENTRIES;
      render.NewPatchLocationListSize = HELIOS_HVC1_PATCH_LOCATION_ENTRIES;
      NTSTATUS sj = D3DKMTRender(&render);
      snprintf(why, sizeof(why), "first=0x%08x second=0x%08x", (unsigned)si,
               (unsigned)sj);
      check(sj != STATUS_SUCCESS_NT,
            "J: a second INIT still refuses (the slot discriminator is TsInitRej==2 "
            "with TsSlotStuck==0)",
            why);
   }

   /* ── teardown, in A1's order ─────────────────────────────────────────── */
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
   destroy_context(queue.hContext);
   destroy_context(control.hContext);
   D3DKMT_DESTROYDEVICE dd;
   memset(&dd, 0, sizeof(dd));
   dd.hDevice = device;
   IGNORE_STATUS(D3DKMTDestroyDevice(&dd));
   return 1;
}

int
main(void)
{
   D3DKMT_ENUMADAPTERS2 ea;
   memset(&ea, 0, sizeof(ea));
   NTSTATUS st = D3DKMTEnumAdapters2(&ea);
   if (st != STATUS_SUCCESS_NT || ea.NumAdapters == 0) {
      printf("hnr2_native_probe: D3DKMTEnumAdapters2 count failed 0x%08x\n",
             (unsigned)st);
      return 2;
   }
   ea.pAdapters =
      (D3DKMT_ADAPTERINFO *)calloc(ea.NumAdapters, sizeof(D3DKMT_ADAPTERINFO));
   if (!ea.pAdapters)
      return 2;
   st = D3DKMTEnumAdapters2(&ea);
   if (st != STATUS_SUCCESS_NT) {
      printf("hnr2_native_probe: D3DKMTEnumAdapters2 fill failed 0x%08x\n",
             (unsigned)st);
      free(ea.pAdapters);
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
   free(ea.pAdapters);

   printf("\n%d checks passed, %d failed, %d Helios adapter(s) probed\n", g_pass,
          g_fail, found);
   if (found != 1) {
      printf("hnr2_native_probe: INCONCLUSIVE - expected exactly one Helios adapter, "
             "found %d\n",
             found);
      return 2;
   }
   printf("hnr2_native_probe: %s\n", g_fail ? "FAIL" : "PASS");
   return g_fail ? 1 : 0;
}
