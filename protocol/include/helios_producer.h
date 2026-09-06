/* Canonical C ABI. Generated copies in engine/ICD trees are checked by
 * tools/sync-producer-abi.py. Version skew is an error; there is no fallback. */
#ifndef HELIOS_PRODUCER_H
#define HELIOS_PRODUCER_H
#include <stdint.h>
#include <stddef.h>

#define HELIOS_PRODUCER_ABI 1u
#define HELIOS_PRODUCER_ESCAPE 0x0013u
#define HP_MAP 1u
#define HP_BIND 2u
#define HP_PUBLISH 3u
#define HP_WAIT 4u
#define HP_CANCEL 5u
#define HP_RELEASE 6u
#define HP_ABORT 7u

struct helios_producer_control {
   uint32_t magic, command, header_version, header_size;
   uint32_t version, op;
   uint64_t binding, generation, epoch, stream_cookie, event, user_va;
   uint32_t allocation, ctx_id, value, slot, state, size;
};
struct helios_producer_status {
   uint64_t sequence, generation, announced, completed;
   uint32_t status, reserved[7];
};
struct helios_producer_snapshot {
   uint64_t generation, announced, completed;
   uint32_t status, reserved;
};

/* VkDevice is passed as uintptr_t, non-dispatchable VkSemaphore as uint64_t.
 * Results are VkResult. Bind requires an ALLOCATION handle, never a resource
 * handle. Each binding retains status until release; VkDevice must outlive it.
 * Wait does not hold the renderer submission mutex. Timeout/cancel are errors
 * to the dependent read. cancel_event is a caller-retained NT event or zero. */
struct helios_producer_api_v1 {
   uint32_t version, size;
   int32_t (*stream)(uintptr_t device, uint64_t semaphore, uint32_t *ctx_id, uint64_t *cookie);
   int32_t (*bind)(uintptr_t device, uint32_t allocation, void **binding);
   int32_t (*publish)(void *binding, uint64_t semaphore, uint64_t value, uint64_t *epoch);
   int32_t (*status)(void *binding, struct helios_producer_snapshot *snapshot);
   int32_t (*wait)(void *binding, uint64_t epoch, uint64_t timeout_ns, uintptr_t cancel_event);
   void (*retain)(void *binding);
   void (*release)(void *binding);
   void (*abort)(void *binding);
};
typedef int32_t (*helios_get_producer_api_fn)(uint32_t version, struct helios_producer_api_v1 *api);

#if defined(__cplusplus)
static_assert(sizeof(helios_producer_control) == 96);
static_assert(sizeof(helios_producer_status) == 64);
static_assert(offsetof(helios_producer_status, status) == 32);
#else
_Static_assert(sizeof(struct helios_producer_control) == 96, "producer control ABI");
_Static_assert(sizeof(struct helios_producer_status) == 64, "producer status ABI");
_Static_assert(offsetof(struct helios_producer_status, status) == 32, "producer status offset");
#endif
#endif
