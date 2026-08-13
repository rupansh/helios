// K2a shared-backing allocation, Lock2, role, repetition, and teardown probe.
//
// Build on win11:
//   gcc -O2 -o C:/Users/Rupansh/k2a_shared_backing_probe.exe
//       Z:/tools/k2a_shared_backing_probe.c -I Z:/protocol/include
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
#define HELIOS_LEGACY_DMA_BUFFER_BYTES 262144u
#define TEST_BYTES (4ull * 1024ull * 1024ull)
#define ALIAS_HOST_FIRST 0x1122334455667788ull
#define ALIAS_HOST_LAST 0x8877665544332211ull
#define ALIAS_GUEST_FIRST 0xA1B2C3D4E5F60718ull
#define ALIAS_GUEST_LAST 0x1827364554637281ull

#define IGNORE_STATUS(call)                                                    \
   do {                                                                        \
      NTSTATUS ignored_ = (call);                                              \
      (void)ignored_;                                                          \
   } while (0)

struct probe_device {
   D3DKMT_HANDLE adapter;
   D3DKMT_HANDLE device;
};

struct probe_allocation {
   D3DKMT_HANDLE resource;
   D3DKMT_HANDLE allocation;
   D3DKMT_HANDLE paging_queue;
   D3DKMT_HANDLE paging_fence;
   void *cpu;
   uint64_t bytes;
};

static int g_pass;
static int g_fail;

static void
check(int ok, const char *what, NTSTATUS status)
{
   if (ok) {
      g_pass++;
      printf("  PASS  %s\n", what);
   } else {
      g_fail++;
      printf("  FAIL  %s -- status=0x%08x\n", what, (unsigned)status);
   }
   fflush(stdout);
}

static NTSTATUS
create_legacy_context(D3DKMT_HANDLE device, D3DKMT_CREATECONTEXT *context)
{
   memset(context, 0, sizeof(*context));
   context->hDevice = device;
   context->ClientHint = D3DKMT_CLIENTHINT_VULKAN;
   return D3DKMTCreateContext(context);
}

static int
open_helios(struct probe_device *out)
{
   D3DKMT_ENUMADAPTERS2 enumeration;
   memset(&enumeration, 0, sizeof(enumeration));
   if (D3DKMTEnumAdapters2(&enumeration) != STATUS_SUCCESS_NT ||
       !enumeration.NumAdapters)
      return 0;

   enumeration.pAdapters = calloc(enumeration.NumAdapters,
                                  sizeof(*enumeration.pAdapters));
   if (!enumeration.pAdapters)
      return 0;
   if (D3DKMTEnumAdapters2(&enumeration) != STATUS_SUCCESS_NT) {
      free(enumeration.pAdapters);
      return 0;
   }

   int found = 0;
   for (UINT i = 0; i < enumeration.NumAdapters; i++) {
      D3DKMT_CREATEDEVICE create_device;
      memset(&create_device, 0, sizeof(create_device));
      create_device.hAdapter = enumeration.pAdapters[i].hAdapter;
      if (D3DKMTCreateDevice(&create_device) != STATUS_SUCCESS_NT) {
         D3DKMT_CLOSEADAPTER close = {
            .hAdapter = enumeration.pAdapters[i].hAdapter,
         };
         IGNORE_STATUS(D3DKMTCloseAdapter(&close));
         continue;
      }

      D3DKMT_CREATECONTEXT context;
      NTSTATUS status = create_legacy_context(create_device.hDevice, &context);
      int helios = status == STATUS_SUCCESS_NT &&
                   context.CommandBufferSize == HELIOS_LEGACY_DMA_BUFFER_BYTES;
      if (status == STATUS_SUCCESS_NT) {
         D3DKMT_DESTROYCONTEXT destroy = { .hContext = context.hContext };
         IGNORE_STATUS(D3DKMTDestroyContext(&destroy));
      }
      if (helios && !found) {
         out->adapter = enumeration.pAdapters[i].hAdapter;
         out->device = create_device.hDevice;
         found = 1;
      } else {
         D3DKMT_DESTROYDEVICE destroy = { .hDevice = create_device.hDevice };
         IGNORE_STATUS(D3DKMTDestroyDevice(&destroy));
         D3DKMT_CLOSEADAPTER close = {
            .hAdapter = enumeration.pAdapters[i].hAdapter,
         };
         IGNORE_STATUS(D3DKMTCloseAdapter(&close));
      }
   }
   free(enumeration.pAdapters);
   return found;
}

static void
close_helios(struct probe_device *device)
{
   if (device->device) {
      D3DKMT_DESTROYDEVICE destroy = { .hDevice = device->device };
      IGNORE_STATUS(D3DKMTDestroyDevice(&destroy));
      device->device = 0;
   }
   if (device->adapter) {
      D3DKMT_CLOSEADAPTER close = { .hAdapter = device->adapter };
      IGNORE_STATUS(D3DKMTCloseAdapter(&close));
      device->adapter = 0;
   }
}

static void
role_contract(uint32_t role, uint64_t *bytes, uint32_t *access,
              uint32_t *cache)
{
   *bytes = role == HELIOS_HVM1_ROLE_REPLY_POOL
               ? HELIOS_HVM1_REPLY_POOL_BYTES
               : TEST_BYTES;
   *cache = role == HELIOS_HVM1_ROLE_VULKAN_DEVICE_LOCAL
               ? HELIOS_HVM1_CACHE_NOT_CPU_VISIBLE
               : HELIOS_HVM1_CACHE_WRITE_COMBINED;
   switch (role) {
   case HELIOS_HVM1_ROLE_REPLY_POOL:
   case HELIOS_HVM1_ROLE_FEEDBACK:
      *access = HELIOS_HVM1_ACCESS_CPU_READ | HELIOS_HVM1_ACCESS_HOST_WRITE;
      break;
   case HELIOS_HVM1_ROLE_VULKAN_HOST_VISIBLE:
      *access = HELIOS_HVM1_ACCESS_CPU_READ | HELIOS_HVM1_ACCESS_CPU_WRITE |
                HELIOS_HVM1_ACCESS_HOST_READ | HELIOS_HVM1_ACCESS_HOST_WRITE;
      break;
   default:
      *access = HELIOS_HVM1_ACCESS_HOST_READ | HELIOS_HVM1_ACCESS_HOST_WRITE;
      break;
   }
}

static NTSTATUS
create_allocation(D3DKMT_HANDLE device, uint32_t role,
                  struct probe_allocation *out)
{
   uint64_t bytes;
   uint32_t access, cache;
   role_contract(role, &bytes, &access, &cache);

   HeliosVenusMemoryAllocationV1 hvm1;
   memset(&hvm1, 0, sizeof(hvm1));
   hvm1.magic = HELIOS_HVM1_MAGIC;
   hvm1.abi_version = (uint16_t)HELIOS_HVM1_ABI_VERSION;
   hvm1.struct_size = (uint16_t)HELIOS_HVM1_SIZE;
   hvm1.package_generation = HELIOS_PACKAGE_GENERATION;
   hvm1.byte_size = bytes;
   hvm1.role = role;
   hvm1.access = access;
   hvm1.cache_policy = cache;

   D3DDDI_ALLOCATIONINFO2 info;
   memset(&info, 0, sizeof(info));
   info.pSystemMem = NULL;
   info.pPrivateDriverData = &hvm1;
   info.PrivateDriverDataSize = sizeof(hvm1);

   D3DKMT_CREATEALLOCATION create;
   memset(&create, 0, sizeof(create));
   create.hDevice = device;
   create.NumAllocations = 1;
   create.pAllocationInfo2 = &info;
   create.Flags.CreateResource = 1;
   create.Flags.CreateShared = 1;
   create.Flags.NtSecuritySharing = 1;
   NTSTATUS status = D3DKMTCreateAllocation2(&create);
   if (status == STATUS_SUCCESS_NT) {
      out->resource = create.hResource;
      out->allocation = info.hAllocation;
      out->bytes = bytes;
   }
   return status;
}

static NTSTATUS
make_resident(D3DKMT_HANDLE device, struct probe_allocation *allocation)
{
   D3DKMT_CREATEPAGINGQUEUE create;
   memset(&create, 0, sizeof(create));
   create.hDevice = device;
   create.Priority = D3DDDI_PAGINGQUEUE_PRIORITY_NORMAL;
   NTSTATUS status = D3DKMTCreatePagingQueue(&create);
   if (status != STATUS_SUCCESS_NT)
      return status;
   allocation->paging_queue = create.hPagingQueue;
   allocation->paging_fence = create.hSyncObject;

   D3DDDI_MAKERESIDENT resident;
   memset(&resident, 0, sizeof(resident));
   resident.hPagingQueue = allocation->paging_queue;
   resident.NumAllocations = 1;
   resident.AllocationList = &allocation->allocation;
   status = D3DKMTMakeResident(&resident);
   if (status != STATUS_SUCCESS_NT && status != STATUS_PENDING_NT)
      return status;
   if (resident.PagingFenceValue) {
      const UINT64 value = resident.PagingFenceValue;
      D3DKMT_WAITFORSYNCHRONIZATIONOBJECTFROMCPU wait;
      memset(&wait, 0, sizeof(wait));
      wait.hDevice = device;
      wait.ObjectCount = 1;
      wait.ObjectHandleArray = &allocation->paging_fence;
      wait.FenceValueArray = &value;
      status = D3DKMTWaitForSynchronizationObjectFromCpu(&wait);
      if (status != STATUS_SUCCESS_NT)
         return status;
   }
   return STATUS_SUCCESS_NT;
}

static NTSTATUS
lock_allocation(D3DKMT_HANDLE device, struct probe_allocation *allocation)
{
   D3DKMT_LOCK2 lock;
   memset(&lock, 0, sizeof(lock));
   lock.hDevice = device;
   lock.hAllocation = allocation->allocation;
   NTSTATUS status = D3DKMTLock2(&lock);
   if (status == STATUS_SUCCESS_NT)
      allocation->cpu = lock.pData;
   return status;
}

static void
close_allocation(D3DKMT_HANDLE device, struct probe_allocation *allocation)
{
   if (allocation->cpu) {
      D3DKMT_UNLOCK2 unlock = {
         .hDevice = device,
         .hAllocation = allocation->allocation,
      };
      IGNORE_STATUS(D3DKMTUnlock2(&unlock));
      allocation->cpu = NULL;
   }
   if (allocation->resource) {
      D3DKMT_DESTROYALLOCATION2 destroy;
      memset(&destroy, 0, sizeof(destroy));
      destroy.hDevice = device;
      destroy.hResource = allocation->resource;
      IGNORE_STATUS(D3DKMTDestroyAllocation2(&destroy));
      allocation->resource = 0;
      allocation->allocation = 0;
   } else if (allocation->allocation) {
      D3DKMT_DESTROYALLOCATION2 destroy;
      memset(&destroy, 0, sizeof(destroy));
      destroy.hDevice = device;
      destroy.phAllocationList = &allocation->allocation;
      destroy.AllocationCount = 1;
      IGNORE_STATUS(D3DKMTDestroyAllocation2(&destroy));
      allocation->allocation = 0;
   }
   if (allocation->paging_queue) {
      D3DDDI_DESTROYPAGINGQUEUE destroy = {
         .hPagingQueue = allocation->paging_queue,
      };
      IGNORE_STATUS(D3DKMTDestroyPagingQueue(&destroy));
      allocation->paging_queue = 0;
   }
}

static int
exercise_role(struct probe_device *device, uint32_t role, const char *name)
{
   struct probe_allocation allocation;
   memset(&allocation, 0, sizeof(allocation));
   NTSTATUS status = create_allocation(device->device, role, &allocation);
   check(status == STATUS_SUCCESS_NT, name, status);
   if (status != STATUS_SUCCESS_NT)
      return 0;

   status = make_resident(device->device, &allocation);
   check(status == STATUS_SUCCESS_NT, "MakeResident completes", status);
   if (status == STATUS_SUCCESS_NT) {
      status = lock_allocation(device->device, &allocation);
      check(status == STATUS_SUCCESS_NT && allocation.cpu,
            "Lock2 returns the shared CPU view", status);
      if (status == STATUS_SUCCESS_NT && allocation.cpu) {
         volatile unsigned char *bytes = allocation.cpu;
         bytes[0] = (unsigned char)(0x40u + role);
         bytes[allocation.bytes - 1] = (unsigned char)(0x90u + role);
         MemoryBarrier();
         check(bytes[0] == (unsigned char)(0x40u + role) &&
                  bytes[allocation.bytes - 1] == (unsigned char)(0x90u + role),
               "first and last exact bytes are writable", STATUS_SUCCESS_NT);
      }
   }
   close_allocation(device->device, &allocation);
   return status == STATUS_SUCCESS_NT;
}

static int
run_child_exit(void)
{
   struct probe_device device = {0};
   struct probe_allocation allocation = {0};
   if (!open_helios(&device))
      return 2;
   NTSTATUS status = create_allocation(device.device,
                                       HELIOS_HVM1_ROLE_VULKAN_HOST_VISIBLE,
                                       &allocation);
   if (status != STATUS_SUCCESS_NT)
      return 3;
   status = make_resident(device.device, &allocation);
   if (status != STATUS_SUCCESS_NT)
      return 4;
   status = lock_allocation(device.device, &allocation);
   if (status != STATUS_SUCCESS_NT || !allocation.cpu)
      return 5;
   ((volatile unsigned char *)allocation.cpu)[0] = 0xCE;
   MemoryBarrier();
   ExitProcess(0);
}

static int
run_foreign_handle(const char *text)
{
   struct probe_device device = {0};
   if (!open_helios(&device))
      return 2;
   D3DKMT_HANDLE foreign = (D3DKMT_HANDLE)_strtoui64(text, NULL, 0);
   D3DKMT_LOCK2 lock = {
      .hDevice = device.device,
      .hAllocation = foreign,
   };
   NTSTATUS status = D3DKMTLock2(&lock);
   if (status == STATUS_SUCCESS_NT) {
      D3DKMT_UNLOCK2 unlock = {
         .hDevice = device.device,
         .hAllocation = foreign,
      };
      IGNORE_STATUS(D3DKMTUnlock2(&unlock));
   }
   close_helios(&device);
   return status == STATUS_SUCCESS_NT ? 1 : 0;
}

static int
run_alias(void)
{
   struct probe_device device = {0};
   struct probe_allocation allocation = {0};
   if (!open_helios(&device))
      return 2;
   NTSTATUS status = create_allocation(device.device,
                                       HELIOS_HVM1_ROLE_VULKAN_HOST_VISIBLE,
                                       &allocation);
   if (status != STATUS_SUCCESS_NT)
      return 3;
   status = make_resident(device.device, &allocation);
   if (status != STATUS_SUCCESS_NT)
      return 4;
   status = lock_allocation(device.device, &allocation);
   if (status != STATUS_SUCCESS_NT || !allocation.cpu)
      return 5;

   printf("K2A_ALIAS_READY bytes=%llu va=%p\n",
          (unsigned long long)allocation.bytes, allocation.cpu);
   fflush(stdout);
   Sleep(30000);

   volatile uint64_t *first = allocation.cpu;
   volatile uint64_t *last = (volatile uint64_t *)
      ((volatile unsigned char *)allocation.cpu + allocation.bytes - sizeof(uint64_t));
   MemoryBarrier();
   uint64_t first_value = *first;
   uint64_t last_value = *last;
   int host_ok = first_value == ALIAS_HOST_FIRST && last_value == ALIAS_HOST_LAST;
   printf("K2A_HOST_TO_GUEST %s first=%016llx last=%016llx\n",
          host_ok ? "PASS" : "FAIL", (unsigned long long)first_value,
          (unsigned long long)last_value);

   *first = ALIAS_GUEST_FIRST;
   *last = ALIAS_GUEST_LAST;
   MemoryBarrier();
   printf("K2A_GUEST_WRITTEN first=%016llx last=%016llx\n",
          (unsigned long long)ALIAS_GUEST_FIRST,
          (unsigned long long)ALIAS_GUEST_LAST);
   fflush(stdout);
   Sleep(30000);

   close_allocation(device.device, &allocation);
   close_helios(&device);
   return host_ok ? 0 : 1;
}

static void
test_process_teardown(struct probe_device *device)
{
   char executable[MAX_PATH];
   if (!GetModuleFileNameA(NULL, executable, sizeof(executable))) {
      check(0, "child process path resolves", (NTSTATUS)GetLastError());
      return;
   }
   char command[MAX_PATH + 32];
   snprintf(command, sizeof(command), "\"%s\" --child-exit", executable);
   STARTUPINFOA startup;
   PROCESS_INFORMATION process;
   memset(&startup, 0, sizeof(startup));
   memset(&process, 0, sizeof(process));
   startup.cb = sizeof(startup);
   BOOL created = CreateProcessA(NULL, command, NULL, NULL, FALSE, 0, NULL,
                                 NULL, &startup, &process);
   if (!created) {
      check(0, "process teardown child starts", (NTSTATUS)GetLastError());
      return;
   }
   WaitForSingleObject(process.hProcess, 30000);
   DWORD code = STILL_ACTIVE;
   GetExitCodeProcess(process.hProcess, &code);
   CloseHandle(process.hThread);
   CloseHandle(process.hProcess);
   check(code == 0, "process exit tears down an unclosed Lock2 allocation",
         (NTSTATUS)code);
   if (code == 0)
      exercise_role(device, HELIOS_HVM1_ROLE_VULKAN_HOST_VISIBLE,
                    "fresh allocation succeeds after process teardown");
}

static void
test_process_and_stale_handle(struct probe_device *device)
{
   struct probe_allocation allocation = {0};
   NTSTATUS status = create_allocation(device->device,
                                       HELIOS_HVM1_ROLE_VULKAN_HOST_VISIBLE,
                                       &allocation);
   if (status != STATUS_SUCCESS_NT) {
      check(0, "wrong-process test allocation creates", status);
      return;
   }
   D3DKMT_HANDLE handle = allocation.allocation;

   char executable[MAX_PATH];
   char command[MAX_PATH + 64];
   STARTUPINFOA startup = { .cb = sizeof(startup) };
   PROCESS_INFORMATION process = {0};
   BOOL have_path = GetModuleFileNameA(NULL, executable, sizeof(executable));
   if (have_path) {
      snprintf(command, sizeof(command), "\"%s\" --foreign-handle %u",
               executable, (unsigned)handle);
   }
   BOOL created = have_path &&
      CreateProcessA(NULL, command, NULL, NULL, FALSE, 0, NULL, NULL,
                     &startup, &process);
   if (created) {
      WaitForSingleObject(process.hProcess, 30000);
      DWORD code = STILL_ACTIVE;
      GetExitCodeProcess(process.hProcess, &code);
      CloseHandle(process.hThread);
      CloseHandle(process.hProcess);
      check(code == 0, "allocation handle is rejected in another process",
            (NTSTATUS)code);
   } else {
      check(0, "wrong-process child starts", (NTSTATUS)GetLastError());
   }

   close_allocation(device->device, &allocation);
   D3DKMT_LOCK2 stale = {
      .hDevice = device->device,
      .hAllocation = handle,
   };
   status = D3DKMTLock2(&stale);
   check(status != STATUS_SUCCESS_NT, "destroyed allocation handle stays stale",
         status);
   if (status == STATUS_SUCCESS_NT) {
      D3DKMT_UNLOCK2 unlock = {
         .hDevice = device->device,
         .hAllocation = handle,
      };
      IGNORE_STATUS(D3DKMTUnlock2(&unlock));
   }
}

int
main(int argc, char **argv)
{
   setvbuf(stdout, NULL, _IONBF, 0);
   if (argc == 2 && !strcmp(argv[1], "--child-exit"))
      return run_child_exit();
   if (argc == 2 && !strcmp(argv[1], "--alias"))
      return run_alias();
   if (argc == 3 && !strcmp(argv[1], "--foreign-handle"))
      return run_foreign_handle(argv[2]);

   struct probe_device device = {0};
   if (!open_helios(&device)) {
      printf("k2a_shared_backing_probe: INCONCLUSIVE - Helios adapter not found\n");
      return 2;
   }

   exercise_role(&device, HELIOS_HVM1_ROLE_REPLY_POOL,
                 "role 1 shared allocation creates");
   exercise_role(&device, HELIOS_HVM1_ROLE_VULKAN_HOST_VISIBLE,
                 "role 2 shared allocation creates");
   exercise_role(&device, HELIOS_HVM1_ROLE_FEEDBACK,
                 "role 3 shared allocation creates");

   struct probe_allocation role4 = {0};
   NTSTATUS status = create_allocation(device.device,
                                       HELIOS_HVM1_ROLE_VULKAN_DEVICE_LOCAL,
                                       &role4);
   check(status != STATUS_SUCCESS_NT,
         "role 4 is refused before a shared CPU view exists", status);
   close_allocation(device.device, &role4);

   for (unsigned i = 0; i < 8; i++) {
      char label[96];
      snprintf(label, sizeof(label), "repeat %u create/map/unmap/destroy", i + 1);
      exercise_role(&device, HELIOS_HVM1_ROLE_VULKAN_HOST_VISIBLE, label);
   }
   test_process_teardown(&device);
   test_process_and_stale_handle(&device);
   close_helios(&device);

   printf("k2a_shared_backing_probe: %d passed, %d failed -- %s\n",
          g_pass, g_fail, g_fail ? "FAIL" : "PASS");
   return g_fail ? 1 : 0;
}
