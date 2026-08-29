// Does DXGKDDI_SETALLOCATIONBACKINGSTORE require the allocation to be SHARED?
//
// 2026-08-29. The fix for the black desktop is to extend the K2a shared-backing
// contract from HVM1 role 1 to the UMD's HWA2 allocations, so that the pointer
// the application maps IS the memory the host imports. Microsoft's contract
// (learn.microsoft.com/.../sharing-backing-store-with-kmd) lists four
// properties, and one of them decides whether that is possible at all:
//
//     "The allocation must be created as shared."
//
// Ordinary D3D11 staging resources are NOT shared, and the UMD cannot make them
// so -- the runtime decides. If dxgkrnl enforces that line, the whole route is
// closed for exactly the allocations that need it, and the historical
// E_INVALIDARG on `HWA2_SHARE_BACKING_STORE_WITH_KMD = true` was that
// enforcement rather than the two other contract violations that were present
// at the same time (pSystemMem, and a second venus backing).
//
// So ask dxgkrnl directly, through the one path in this driver that already
// works: an HVM1 role-1 allocation. The KMD cannot tell CreateShared from a
// plain CreateResource -- both arrive as the single `Resource` bit -- so the two
// arms below are identical to the KMD and differ only in what dxgkrnl knows.
//
//   arm A  CreateResource=1 CreateShared=1  the known-good control
//   arm B  CreateResource=1 CreateShared=0  the question
//
// Each arm creates, makes resident, locks, and writes a per-arm 64-bit
// signature through the Lock2 pointer. Read the answer from OUTSIDE with the
// QEMU trace + QMP: a `virtio_gpu_virgl_guest_blob_backing` line for an arm
// means DxgkDdiSetAllocationBackingStore ran for it, and the signature visible
// at that line's `first` GPA means the pages alias. The KMD's `ShBkOk` counter
// is the in-guest corroboration.
//
// Build on win11:
//   gcc -O2 -o C:/Users/Rupansh/k2a_unshared_backing_probe.exe
//       Z:/tools/k2a_unshared_backing_probe.c -I Z:/protocol/include
//       -I Z:/icd/win-build/wdk-include -lgdi32

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <windows.h>

#ifndef _NTDEF_
typedef LONG NTSTATUS, *PNTSTATUS;
#endif
#include <d3dkmthk.h>

#include "helios_native_render.h"

#define STATUS_SUCCESS_NT ((NTSTATUS)0x00000000L)
#define STATUS_PENDING_NT ((NTSTATUS)0x00000103L)
static uint64_t g_test_bytes = 4ull * 1024ull * 1024ull;
#define TEST_BYTES g_test_bytes

#define IGNORE_STATUS(call)                                                    \
   do {                                                                        \
      NTSTATUS ignored_ = (call);                                              \
      (void)ignored_;                                                          \
   } while (0)

struct arm_result {
   const char *name;
   NTSTATUS create;
   NTSTATUS resident;
   NTSTATUS lock;
   void *cpu;
   uint64_t signature;
};

static D3DKMT_HANDLE g_adapter, g_device;

static NTSTATUS
open_device(void)
{
   D3DKMT_OPENADAPTERFROMDEVICENAME open;
   D3DKMT_ENUMADAPTERS2 enumerate;
   memset(&enumerate, 0, sizeof(enumerate));
   NTSTATUS status = D3DKMTEnumAdapters2(&enumerate);
   if (status != STATUS_SUCCESS_NT || !enumerate.NumAdapters)
      return status ? status : (NTSTATUS)0xC0000001L;
   D3DKMT_ADAPTERINFO *info =
      calloc(enumerate.NumAdapters, sizeof(D3DKMT_ADAPTERINFO));
   if (!info)
      return (NTSTATUS)0xC0000017L;
   enumerate.pAdapters = info;
   status = D3DKMTEnumAdapters2(&enumerate);
   if (status != STATUS_SUCCESS_NT) {
      free(info);
      return status;
   }
   (void)open;
   /* Pick the adapter whose driver answers our private-data contract: try each
    * until CreateDevice plus a role-1 create succeeds. Trying is cheaper and
    * more honest than matching on a description string. */
   for (UINT i = 0; i < enumerate.NumAdapters; ++i) {
      D3DKMT_CREATEDEVICE create;
      memset(&create, 0, sizeof(create));
      create.hAdapter = info[i].hAdapter;
      if (D3DKMTCreateDevice(&create) != STATUS_SUCCESS_NT)
         continue;
      g_adapter = info[i].hAdapter;
      g_device = create.hDevice;
      free(info);
      return STATUS_SUCCESS_NT;
   }
   free(info);
   return (NTSTATUS)0xC0000001L;
}

static void
run_arm(struct arm_result *arm, int create_shared, uint32_t role)
{
   HeliosVenusMemoryAllocationV1 hvm1;
   memset(&hvm1, 0, sizeof(hvm1));
   hvm1.magic = HELIOS_HVM1_MAGIC;
   hvm1.abi_version = (uint16_t)HELIOS_HVM1_ABI_VERSION;
   hvm1.struct_size = (uint16_t)HELIOS_HVM1_SIZE;
   hvm1.package_generation = HELIOS_PACKAGE_GENERATION;
   hvm1.byte_size = TEST_BYTES;
   hvm1.role = role;
   /* Role 4 is the split: the KMD marks it NOT cpu-visible, so it never asks
    * for shared backing. If unshared succeeds for role 4 and fails for role 1,
    * the refusal belongs to dxgkrnl's ShareBackingStoreWithKmd enforcement and
    * not to this driver's create-shape check. */
   const int cpu_visible = role != HELIOS_HVM1_ROLE_VULKAN_DEVICE_LOCAL;
   hvm1.access = cpu_visible
                    ? (HELIOS_HVM1_ACCESS_CPU_READ | HELIOS_HVM1_ACCESS_CPU_WRITE |
                       HELIOS_HVM1_ACCESS_HOST_READ | HELIOS_HVM1_ACCESS_HOST_WRITE)
                    : (HELIOS_HVM1_ACCESS_HOST_READ | HELIOS_HVM1_ACCESS_HOST_WRITE);
   hvm1.cache_policy = cpu_visible ? HELIOS_HVM1_CACHE_WRITE_COMBINED
                                   : HELIOS_HVM1_CACHE_NOT_CPU_VISIBLE;

   D3DDDI_ALLOCATIONINFO2 info;
   memset(&info, 0, sizeof(info));
   info.pSystemMem = NULL;
   info.pPrivateDriverData = &hvm1;
   info.PrivateDriverDataSize = sizeof(hvm1);

   D3DKMT_CREATEALLOCATION create;
   memset(&create, 0, sizeof(create));
   create.hDevice = g_device;
   create.NumAllocations = 1;
   create.pAllocationInfo2 = &info;
   create.Flags.CreateResource = 1;
   create.Flags.CreateShared = create_shared ? 1 : 0;
   create.Flags.NtSecuritySharing = create_shared ? 1 : 0;
   arm->create = D3DKMTCreateAllocation2(&create);
   if (arm->create != STATUS_SUCCESS_NT)
      return;

   D3DKMT_CREATEPAGINGQUEUE queue;
   memset(&queue, 0, sizeof(queue));
   queue.hDevice = g_device;
   queue.Priority = D3DDDI_PAGINGQUEUE_PRIORITY_NORMAL;
   arm->resident = D3DKMTCreatePagingQueue(&queue);
   if (arm->resident != STATUS_SUCCESS_NT)
      return;
   D3DDDI_MAKERESIDENT resident;
   memset(&resident, 0, sizeof(resident));
   resident.hPagingQueue = queue.hPagingQueue;
   resident.NumAllocations = 1;
   resident.AllocationList = &info.hAllocation;
   arm->resident = D3DKMTMakeResident(&resident);
   if (arm->resident == STATUS_PENDING_NT && resident.PagingFenceValue) {
      const UINT64 value = resident.PagingFenceValue;
      D3DKMT_WAITFORSYNCHRONIZATIONOBJECTFROMCPU wait;
      memset(&wait, 0, sizeof(wait));
      wait.hDevice = g_device;
      wait.ObjectCount = 1;
      wait.ObjectHandleArray = &queue.hSyncObject;
      wait.FenceValueArray = &value;
      arm->resident = D3DKMTWaitForSynchronizationObjectFromCpu(&wait);
   }
   if (arm->resident != STATUS_SUCCESS_NT)
      return;

   if (!cpu_visible) {
      arm->lock = (NTSTATUS)0xC0000002L; /* not attempted */
      return;
   }
   D3DKMT_LOCK2 lock;
   memset(&lock, 0, sizeof(lock));
   lock.hDevice = g_device;
   lock.hAllocation = info.hAllocation;
   arm->lock = D3DKMTLock2(&lock);
   if (arm->lock != STATUS_SUCCESS_NT)
      return;
   arm->cpu = lock.pData;

   /* First and last 8 bytes, so a host reader can find it at either end of the
    * traced range without knowing the interior page order. */
   volatile uint64_t *base = (volatile uint64_t *)lock.pData;
   base[0] = arm->signature;
   base[(TEST_BYTES / 8) - 1] = arm->signature;

   MEMORY_BASIC_INFORMATION mbi;
   memset(&mbi, 0, sizeof(mbi));
   if (VirtualQuery(lock.pData, &mbi, sizeof(mbi)))
      printf("  %s Lock2 ptr=%p type=0x%lx%s\n", arm->name, lock.pData,
             (unsigned long)mbi.Type,
             mbi.Type == MEM_PRIVATE ? " PRIVATE" :
             mbi.Type == MEM_MAPPED ? " MAPPED" : " ?");
}

int
main(void)
{
   NTSTATUS status = open_device();
   if (status != STATUS_SUCCESS_NT) {
      printf("FAIL open device 0x%08x\n", (unsigned)status);
      return 1;
   }
   printf("device ready\n");

   struct arm_result arms[4] = {
      { "A role1 SHARED  ", 0, 0, 0, NULL, 0x5AA5000000000001ull },
      { "B role1 UNSHARED", 0, 0, 0, NULL, 0x5AA5000000000002ull },
      { "C role4 SHARED  ", 0, 0, 0, NULL, 0x5AA5000000000003ull },
      { "D role4 UNSHARED", 0, 0, 0, NULL, 0x5AA5000000000004ull },
   };
   /* Size sweep on the one arm that works: how large a role-1 allocation can
    * the KMD's guest blob + the host's udmabuf coalescing actually carry?  That
    * bound is the bound on any fix that puts host-visible memory in guest RAM. */
   static const uint64_t sweep[] = { 1ull << 20, 4ull << 20, 8ull << 20,
                                     16ull << 20, 32ull << 20, 64ull << 20 };
   for (unsigned i = 0; i < sizeof(sweep) / sizeof(sweep[0]); ++i) {
      struct arm_result probe = { "  sweep", 0, 0, 0, NULL, 0x5AA5F00000000000ull + i };
      g_test_bytes = sweep[i];
      run_arm(&probe, 1, HELIOS_HVM1_ROLE_VULKAN_HOST_VISIBLE);
      printf("  sweep %4llu MiB create=0x%08x resident=0x%08x lock=0x%08x cpu=%p\n",
             (unsigned long long)(sweep[i] >> 20), (unsigned)probe.create,
             (unsigned)probe.resident, (unsigned)probe.lock, probe.cpu);
   }
   g_test_bytes = 4ull * 1024ull * 1024ull;

   run_arm(&arms[0], 1, HELIOS_HVM1_ROLE_VULKAN_HOST_VISIBLE);
   run_arm(&arms[1], 0, HELIOS_HVM1_ROLE_VULKAN_HOST_VISIBLE);
   run_arm(&arms[2], 1, HELIOS_HVM1_ROLE_VULKAN_DEVICE_LOCAL);
   run_arm(&arms[3], 0, HELIOS_HVM1_ROLE_VULKAN_DEVICE_LOCAL);

   for (int i = 0; i < 4; ++i) {
      printf("%s create=0x%08x resident=0x%08x lock=0x%08x cpu=%p sig=0x%016llx\n",
             arms[i].name, (unsigned)arms[i].create, (unsigned)arms[i].resident,
             (unsigned)arms[i].lock, arms[i].cpu,
             (unsigned long long)arms[i].signature);
   }
   printf("HOLDING 45s -- sample guest RAM now\n");
   fflush(stdout);
   Sleep(45000);
   printf("DONE\n");
   return 0;
}
