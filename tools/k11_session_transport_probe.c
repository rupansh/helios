// k11_session_transport_probe.c — K11 target acceptance without Mesa A3/A4.
//
// This executable links the landed Mesa A1/A2 implementation directly. It
// therefore drives the same HVC1, role-1 K2a pool, HNR2 INIT, C51 wait, HVR1
// validation, and teardown code that a future vn_instance will use, while A3
// remains deliberately unstarted.
//
// It proves two simultaneous sessions in one process have distinct admission
// identities, exact HQA1 attach stays session-local, a second process cannot
// use an inherited session key, abrupt process exit is drained, and fresh plus
// repeated INIT/teardown remain usable. KMD counter deltas prove one real host
// context create/destroy and one real reply publication per successful INIT;
// the source/mutation gate proves those creates are owned by distinct exact
// SessionObjects and never expose the host ids in an ABI.
//
// Build on win11 (WinLibs gcc):
//   gcc -O2 -o C:\Users\Rupansh\k11_session_transport_probe.exe
//       Z:\tools\k11_session_transport_probe.c
//       Z:\icd\mesa\src\virtio\vulkan\vn_helios_native_kmt.c
//       -I Z:\protocol\include
//       -I Z:\icd\mesa\src\virtio\vulkan
//       -I Z:\icd\mesa\include
//       -I Z:\icd\win-build\wdk-include -ladvapi32 -lgdi32

#include <stdarg.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <wchar.h>
#include <windows.h>

#ifndef _NTDEF_
typedef LONG NTSTATUS, *PNTSTATUS;
#endif
#include <d3dkmthk.h>

#include "vn_helios_translation_session.h"

#define IGNORE_STATUS(call)                                                    \
   do {                                                                        \
      NTSTATUS ignored_ = (call);                                              \
      (void)ignored_;                                                          \
   } while (0)

#define STATUS_SUCCESS_NT ((NTSTATUS)0x00000000L)
#define REPEAT_SESSIONS 4u
#define VENUS_WIRE_VK_CREATE_INSTANCE_OPCODE 0u

static int g_pass;
static int g_fail;

/* A1/A2 route sparse diagnostics through this Mesa-owned symbol. The probe
 * supplies only the sink; no renderer or alternate transport is linked. */
void
vn_renderer_helios_diag_log(const char *fmt, ...)
{
   va_list ap;
   va_start(ap, fmt);
   fputs("  MESA  ", stderr);
   vfprintf(stderr, fmt, ap);
   fputc('\n', stderr);
   va_end(ap);
}

/* Compile A1 into this probe's translation unit instead of treating its
 * private session layout as another ABI. This keeps Mesa read-only while
 * letting the acceptance probe inspect the exact HVR1 bytes A1 consumed. */
#include "../icd/mesa/src/virtio/vulkan/vn_helios_translation_session.c"

static void
check(bool ok, const char *what, const char *detail)
{
   if (ok) {
      g_pass++;
      printf("  PASS  %s\n", what);
   } else {
      g_fail++;
      printf("  FAIL  %s -- %s\n", what, detail ? detail : "");
   }
}

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
destroy_context(D3DKMT_HANDLE context)
{
   if (context) {
      D3DKMT_DESTROYCONTEXT destroy = { .hContext = context };
      IGNORE_STATUS(D3DKMTDestroyContext(&destroy));
   }
}

static bool
find_helios_luid(LUID *out)
{
   D3DKMT_ENUMADAPTERS2 enumeration;
   memset(&enumeration, 0, sizeof(enumeration));
   if (D3DKMTEnumAdapters2(&enumeration) != STATUS_SUCCESS_NT ||
       !enumeration.NumAdapters)
      return false;
   enumeration.pAdapters = calloc(enumeration.NumAdapters,
                                  sizeof(*enumeration.pAdapters));
   if (!enumeration.pAdapters)
      return false;
   if (D3DKMTEnumAdapters2(&enumeration) != STATUS_SUCCESS_NT) {
      free(enumeration.pAdapters);
      return false;
   }

   unsigned matches = 0;
   for (UINT i = 0; i < enumeration.NumAdapters; i++) {
      /* Query the exact D3D11 UMD dxgkrnl serves for this adapter. A command
       * buffer size, enumeration index, marketing string, or successful legacy
       * context would only be a heuristic and could select another adapter. */
      D3DKMT_UMDFILENAMEINFO umd;
      memset(&umd, 0, sizeof(umd));
      umd.Version = KMTUMDVERSION_DX11;
      D3DKMT_QUERYADAPTERINFO query;
      memset(&query, 0, sizeof(query));
      query.hAdapter = enumeration.pAdapters[i].hAdapter;
      query.Type = KMTQAITYPE_UMDRIVERNAME;
      query.pPrivateDriverData = &umd;
      query.PrivateDriverDataSize = sizeof(umd);
      if (D3DKMTQueryAdapterInfo(&query) == STATUS_SUCCESS_NT &&
          wcsstr(umd.UmdFileName, L"helios_umd") != NULL) {
         if (matches == 0)
            *out = enumeration.pAdapters[i].AdapterLuid;
         matches++;
      }
      D3DKMT_CLOSEADAPTER close = {
         .hAdapter = enumeration.pAdapters[i].hAdapter,
      };
      IGNORE_STATUS(D3DKMTCloseAdapter(&close));
   }
   free(enumeration.pAdapters);
   return matches == 1;
}

struct session_identity {
   uint64_t generation;
   uint64_t capability_low;
   uint64_t capability_high;
   uint32_t endpoint_capacity;
};

static bool
session_identity(struct helios_translation_session *session,
                 struct session_identity *out)
{
   memset(out, 0, sizeof(*out));
   out->generation = helios_session_generation(session);
   helios_session_capability(session, &out->capability_low,
                             &out->capability_high);
   out->endpoint_capacity = helios_session_endpoint_capacity(session);
   return out->generation != 0 &&
          (out->capability_low != 0 || out->capability_high != 0) &&
          out->endpoint_capacity != 0;
}

static bool
session_host_init_evidence(struct helios_translation_session *session)
{
   /* INIT is the first transaction and therefore owns slot zero. A1 already
    * validated the HVR1 publication/generation/batch/chunk envelope before it
    * returned the session; this adds the operation-specific evidence that its
    * generic transaction helper deliberately leaves to the caller. */
   const uint8_t *slot = (const uint8_t *)session->pool_cpu +
                         session->slots[0].offset;
   HeliosVenusReplyV1 reply;
   memcpy(&reply, slot, sizeof(reply));
   return reply.magic == HELIOS_HVR1_MAGIC &&
          reply.version == HELIOS_HVR1_VERSION &&
          reply.header_size == HELIOS_HVR1_HEADER_SIZE &&
          reply.session_generation == session->session_generation &&
          reply.slot_generation == session->slots[0].generation &&
          reply.batch_token != 0 && reply.snapshot_generation != 0 &&
          reply.opcode == VENUS_WIRE_VK_CREATE_INSTANCE_OPCODE &&
          reply.status == 0 &&
          reply.total_bytes == sizeof(HeliosTranslationSessionReplyV1) &&
          reply.chunk_offset == 0 &&
          reply.chunk_bytes == sizeof(HeliosTranslationSessionReplyV1) &&
          reply.flags == HELIOS_HVR1_FLAG_FINAL;
}

static HeliosQueueAttachV1
hqa1(const struct session_identity *identity, uint64_t context_generation)
{
   HeliosQueueAttachV1 packet;
   memset(&packet, 0, sizeof(packet));
   packet.magic = HELIOS_HQA1_MAGIC;
   packet.abi_version = HELIOS_HQA1_ABI_VERSION;
   packet.struct_size = HELIOS_HQA1_SIZE;
   packet.package_generation = HELIOS_PACKAGE_GENERATION;
   packet.session_generation = identity->generation;
   packet.capability_low = identity->capability_low;
   packet.capability_high = identity->capability_high;
   packet.endpoint_id = 1;
   packet.engine_class = HELIOS_ENGINE_CLASS_GRAPHICS;
   packet.queue_family = 0;
   packet.queue_index = 0;
   packet.context_generation = context_generation;
   packet.flags = HELIOS_HQA1_FLAG_D3D11_PHYSICAL;
   return packet;
}

/* Build an ordinary outer device, make one HQA1 attempt, and tear it down.
 * The caller supplies the exact process-local session key or an intentionally
 * foreign pair. No search scalar, PID, name, or resource id enters the packet. */
static NTSTATUS
try_attach(LUID luid, const struct session_identity *identity,
           uint64_t context_generation)
{
   D3DKMT_OPENADAPTERFROMLUID open = { .AdapterLuid = luid };
   NTSTATUS status = D3DKMTOpenAdapterFromLuid(&open);
   if (status != STATUS_SUCCESS_NT)
      return status;
   D3DKMT_CREATEDEVICE device;
   memset(&device, 0, sizeof(device));
   device.hAdapter = open.hAdapter;
   status = D3DKMTCreateDevice(&device);
   if (status == STATUS_SUCCESS_NT) {
      HeliosQueueAttachV1 packet = hqa1(identity, context_generation);
      D3DKMT_CREATECONTEXT context;
      status = create_context(device.hDevice, &packet, sizeof(packet), &context);
      if (status == STATUS_SUCCESS_NT)
         destroy_context(context.hContext);
      D3DKMT_DESTROYDEVICE destroy = { .hDevice = device.hDevice };
      IGNORE_STATUS(D3DKMTDestroyDevice(&destroy));
   }
   D3DKMT_CLOSEADAPTER close = { .hAdapter = open.hAdapter };
   IGNORE_STATUS(D3DKMTCloseAdapter(&close));
   return status;
}

struct k11_counters {
   DWORD context_created;
   DWORD context_destroyed;
   DWORD init_ok;
   DWORD init_reject;
   DWORD host_reply_reject;
   DWORD reply_published;
   DWORD stale_transport;
   DWORD cleanup_reject;
};

static bool
read_counter(HKEY key, const char *name, DWORD *out)
{
   DWORD value = 0;
   DWORD bytes = sizeof(value);
   DWORD type = 0;
   if (RegQueryValueExA(key, name, NULL, &type, (BYTE *)&value, &bytes) !=
          ERROR_SUCCESS ||
       type != REG_DWORD || bytes != sizeof(value))
      return false;
   *out = value;
   return true;
}

static bool
read_counters(struct k11_counters *out, bool allow_missing_cleanup)
{
   HKEY key = NULL;
   if (RegOpenKeyExA(HKEY_LOCAL_MACHINE,
                     "SYSTEM\\CurrentControlSet\\Services\\helios_kmd_render",
                     0, KEY_QUERY_VALUE, &key) != ERROR_SUCCESS)
      return false;
   bool cleanup = read_counter(key, "K11CleanRej", &out->cleanup_reject);
   if (!cleanup && allow_missing_cleanup) {
      /* A newly deployed version has not necessarily destroyed a KMT device
       * yet, so this new diagnostic value may not have been materialized in
       * the service key. The in-memory counter starts at zero; the post-run
       * read below does not allow this exception. */
      out->cleanup_reject = 0;
      cleanup = true;
   }
   const bool complete =
      read_counter(key, "K11CtxNew", &out->context_created) &&
      read_counter(key, "K11CtxDel", &out->context_destroyed) &&
      read_counter(key, "K11InitOk", &out->init_ok) &&
      read_counter(key, "K11InitRej", &out->init_reject) &&
      read_counter(key, "K11ReplyRej", &out->host_reply_reject) &&
      read_counter(key, "K11ReplyOk", &out->reply_published) &&
      read_counter(key, "K11Stale", &out->stale_transport) && cleanup;
   RegCloseKey(key);
   return complete;
}

static DWORD
delta(DWORD before, DWORD after)
{
   return after - before;
}

/* The inherited anonymous pipe is deliberately the only cross-process carrier
 * for this negative test. The capability is never printed, persisted, placed
 * in an environment variable, or accepted by the driver outside HQA1. */
static int
child_mode(void)
{
   struct session_identity foreign;
   DWORD got = 0;
   HANDLE input = GetStdHandle(STD_INPUT_HANDLE);
   if (input == INVALID_HANDLE_VALUE ||
       !ReadFile(input, &foreign, sizeof(foreign), &got, NULL) ||
       got != sizeof(foreign))
      return 20;

   LUID luid;
   if (!find_helios_luid(&luid))
      return 21;
   if (try_attach(luid, &foreign, 1) == STATUS_SUCCESS_NT)
      return 22;

   struct helios_translation_session *own = NULL;
   if (helios_translation_session_create(luid, 3, &own) != VK_SUCCESS || !own)
      return 23;
   struct session_identity identity;
   if (!session_identity(own, &identity) || identity.endpoint_capacity > 3)
      return 24;

   /* Intentional abrupt exit: Windows/KMD process teardown must revoke and
    * drain the live host namespace and role-1 pool without this owner calling
    * either Mesa destroy function. */
   ExitProcess(0);
}

static bool
run_abrupt_child(const struct session_identity *foreign)
{
   SECURITY_ATTRIBUTES attributes = {
      .nLength = sizeof(attributes),
      .lpSecurityDescriptor = NULL,
      .bInheritHandle = TRUE,
   };
   HANDLE read_pipe = NULL, write_pipe = NULL;
   if (!CreatePipe(&read_pipe, &write_pipe, &attributes, 0))
      return false;
   SetHandleInformation(write_pipe, HANDLE_FLAG_INHERIT, 0);

   char executable[MAX_PATH];
   if (!GetModuleFileNameA(NULL, executable, sizeof(executable))) {
      CloseHandle(read_pipe);
      CloseHandle(write_pipe);
      return false;
   }
   char command[2 * MAX_PATH];
   snprintf(command, sizeof(command), "\"%s\" --child", executable);

   STARTUPINFOA startup;
   PROCESS_INFORMATION process;
   memset(&startup, 0, sizeof(startup));
   memset(&process, 0, sizeof(process));
   startup.cb = sizeof(startup);
   startup.dwFlags = STARTF_USESTDHANDLES;
   startup.hStdInput = read_pipe;
   startup.hStdOutput = GetStdHandle(STD_OUTPUT_HANDLE);
   startup.hStdError = GetStdHandle(STD_ERROR_HANDLE);
   const BOOL created = CreateProcessA(NULL, command, NULL, NULL, TRUE, 0, NULL,
                                       NULL, &startup, &process);
   CloseHandle(read_pipe);
   if (!created) {
      CloseHandle(write_pipe);
      return false;
   }
   DWORD written = 0;
   const BOOL sent = WriteFile(write_pipe, foreign, sizeof(*foreign), &written, NULL);
   CloseHandle(write_pipe);
   if (!sent || written != sizeof(*foreign)) {
      TerminateProcess(process.hProcess, 25);
   }
   const DWORD waited = WaitForSingleObject(process.hProcess, 120000);
   DWORD exit_code = 26;
   if (waited == WAIT_OBJECT_0)
      GetExitCodeProcess(process.hProcess, &exit_code);
   CloseHandle(process.hThread);
   CloseHandle(process.hProcess);
   return waited == WAIT_OBJECT_0 && exit_code == 0;
}

/* Runtime reset/stop acceptance helper. An owner-authorized PowerShell wrapper
 * starts this mode with redirected stdin, waits for K11_HOLD_READY, restarts
 * the adapter, and then writes one byte. The old capability must be dead and
 * Mesa teardown must return rather than waiting on vanished host work. The
 * wrapper then runs the ordinary probe again to prove a fresh namespace. */
static int
hold_reset_mode(void)
{
   LUID luid;
   if (!find_helios_luid(&luid))
      return 30;
   struct helios_translation_session *session = NULL;
   if (helios_translation_session_create(luid, 2, &session) != VK_SUCCESS ||
       !session || !session_host_init_evidence(session))
      return 31;
   struct session_identity identity;
   if (!session_identity(session, &identity)) {
      helios_translation_session_destroy(session);
      return 32;
   }

   puts("K11_HOLD_READY");
   fflush(stdout);
   char release = 0;
   DWORD got = 0;
   if (!ReadFile(GetStdHandle(STD_INPUT_HANDLE), &release, 1, &got, NULL) ||
       got != 1) {
      helios_translation_session_destroy(session);
      return 33;
   }

   const NTSTATUS stale = try_attach(luid, &identity, 1);
   helios_translation_session_destroy(session);
   if (stale == STATUS_SUCCESS_NT)
      return 34;
   puts("K11_HOLD_DRAINED");
   return 0;
}

int
main(int argc, char **argv)
{
   if (argc == 2 && strcmp(argv[1], "--child") == 0)
      return child_mode();
   if (argc == 2 && strcmp(argv[1], "--hold-reset") == 0)
      return hold_reset_mode();

   LUID luid;
   check(find_helios_luid(&luid), "A: exactly the Helios adapter is found",
         "the legacy 256-KiB context profile was not present");
   if (g_fail)
      return 2;

   struct k11_counters before, after;
   check(read_counters(&before, true), "B: K11 counter baseline is readable",
         "service key or counter values are absent");

   struct helios_translation_session *first = NULL, *second = NULL;
   const VkResult r1 = helios_translation_session_create(luid, 4, &first);
   const VkResult r2 = helios_translation_session_create(luid, 2, &second);
   check(r1 == VK_SUCCESS && r2 == VK_SUCCESS && first && second,
         "C: two vn_instance-equivalent sessions are live simultaneously",
         "one finite INIT or its actual HVR1 reply failed");
   if (!first || !second)
      goto done;

   check(session_host_init_evidence(first) &&
            session_host_init_evidence(second),
         "D: both INIT replies carry exact host vkCreateInstance evidence",
         "HVR1 opcode/status/generation/chunk bytes did not correlate");

   struct session_identity one, two;
   const bool one_ok = session_identity(first, &one);
   const bool two_ok = session_identity(second, &two);
   check(one_ok && one.endpoint_capacity <= 4 && two_ok &&
            two.endpoint_capacity <= 2,
         "E: each INIT returns nonzero bounded generation/capability/capacity",
         "a success field was zero or exceeded its request");
   check(one.generation != two.generation &&
            (one.capability_low != two.capability_low ||
             one.capability_high != two.capability_high),
         "F: simultaneous sessions have distinct generations and capabilities",
         "a session admission identity was reused");

   check(try_attach(luid, &one, 1) == STATUS_SUCCESS_NT,
         "G: exact HQA1 attaches through the session's direct endpoint",
         "the process/adapter/session/capability/endpoint tuple was refused");
   struct session_identity crossed = one;
   crossed.capability_low = two.capability_low;
   crossed.capability_high = two.capability_high;
   check(try_attach(luid, &crossed, 2) != STATUS_SUCCESS_NT,
         "H: a generation/capability pair spliced across sessions is refused",
         "attach discovered a session without its exact key");

   check(run_abrupt_child(&one),
         "I: another process cannot attach, then abrupt exit is drained",
         "the foreign attach succeeded, child INIT failed, or teardown stuck");
   check(try_attach(luid, &one, 2) == STATUS_SUCCESS_NT,
         "J: the parent session remains exact and live after child teardown",
         "foreign-process teardown disturbed the parent namespace");

   helios_translation_session_destroy(first);
   first = NULL;
   helios_translation_session_destroy(second);
   second = NULL;

   for (unsigned i = 0; i < REPEAT_SESSIONS; i++) {
      struct helios_translation_session *repeat = NULL;
      const VkResult result = helios_translation_session_create(luid, 1, &repeat);
      if (result != VK_SUCCESS || !repeat) {
         check(false, "K: repeated INIT/teardown stays reusable",
               "a fresh session failed after prior teardown");
         break;
      }
      helios_translation_session_destroy(repeat);
      if (i + 1 == REPEAT_SESSIONS)
         check(true, "K: repeated INIT/teardown stays reusable", NULL);
   }

   {
      struct helios_translation_session *fresh = NULL;
      const VkResult result = helios_translation_session_create(luid, 2, &fresh);
      check(result == VK_SUCCESS && fresh,
            "L: a fresh session succeeds after abrupt process exit",
            "a stale host namespace or role-1 binding survived");
      helios_translation_session_destroy(fresh);
   }

done:
   if (first)
      helios_translation_session_destroy(first);
   if (second)
      helios_translation_session_destroy(second);

   if (read_counters(&after, false)) {
      const DWORD expected = 2 + 1 + REPEAT_SESSIONS + 1;
      char detail[256];
      snprintf(detail, sizeof(detail),
               "ctx +%lu/-%lu init +%lu reply +%lu rejects +%lu/%lu stale +%lu cleanup +%lu",
               (unsigned long)delta(before.context_created, after.context_created),
               (unsigned long)delta(before.context_destroyed, after.context_destroyed),
               (unsigned long)delta(before.init_ok, after.init_ok),
               (unsigned long)delta(before.reply_published, after.reply_published),
               (unsigned long)delta(before.init_reject, after.init_reject),
               (unsigned long)delta(before.host_reply_reject, after.host_reply_reject),
               (unsigned long)delta(before.stale_transport, after.stale_transport),
               (unsigned long)delta(before.cleanup_reject, after.cleanup_reject));
      const DWORD created = delta(before.context_created, after.context_created);
      const DWORD destroyed = delta(before.context_destroyed, after.context_destroyed);
      const DWORD initialized = delta(before.init_ok, after.init_ok);
      const DWORD published = delta(before.reply_published, after.reply_published);
      check(created >= expected && created == destroyed &&
               created == initialized && initialized == published,
            "M: each successful INIT has one drained host namespace and HVR1",
            detail);
      check(delta(before.init_reject, after.init_reject) == 0 &&
               delta(before.host_reply_reject, after.host_reply_reject) == 0 &&
               delta(before.stale_transport, after.stale_transport) == 0 &&
               delta(before.cleanup_reject, after.cleanup_reject) == 0,
            "N: no host INIT/reply/stale/cleanup refusal moved", detail);
   } else {
      check(false, "M: K11 counter result is readable",
            "DestroyDevice did not publish the counter block");
   }

   printf("k11_session_transport_probe: %d/%d PASS\n", g_pass,
          g_pass + g_fail);
   return g_fail ? 1 : 0;
}
