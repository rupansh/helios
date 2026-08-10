/*
 * helios_translator_dispatch.h — C mirror of the Helios private direct-dispatch
 * translator ABI.
 *
 * ⛔ SINGLE SOURCE OF TRUTH: protocol/src/translator_dispatch.rs.
 * This header is the hand-maintained C projection of that file for Mesa
 * (`icd/mesa`, which IMPLEMENTS the interface) and for DXVK
 * (`dxvk-helios/`), vkd3d (`vkd3d-proton-helios/`), `umd/bridge` and
 * `umd12/bridge` (which CONSUME it). Every constant, struct, and assertion below
 * exists in the Rust file first; if the two ever disagree, the Rust file wins and
 * this header is the bug. Change one, change both in the same commit.
 *
 * Normative: docs/HELIOS_PRESENT_SYNC_RETIREMENT.md §2 item 8 (the acyclic call
 * graph), §10.4 (the record-only translator contract, the five-point seal
 * contract, and the queue entry points the private dispatcher must reject
 * without a live outer scope), §10.7 (the two session endpoint attachment
 * forms), §13 (the allowed call graph, the ENFORCED DISPATCH PROOF, and the
 * lock/reentrancy rules), §17.3/§17.4/§17.5 (who implements what).
 * docs/retirement/OWNERSHIP.md §4 records why it has exactly one home.
 *
 * WHY THIS FILE EXISTS: the reference mandates "a private, versioned in-process
 * interface … between the D3D UMD bridge, DXVK/vkd3d, and Helios Mesa" four
 * times and specifies it zero times. Left to the lanes, three repositories would
 * invent three incompatible ABIs. No consumer may declare its own copy of the
 * table, the entry-point name, or the submission-mode constant.
 *
 * ⛔ THIS IS NOT A WIRE FORMAT. Every other header in protocol/include describes
 * bytes that cross a kernel or machine boundary. These records never leave the
 * process: they contain function pointers and caller-owned buffer pointers by
 * construction. Do not memcpy one into WDDM private data, a Render command
 * buffer, an HNR2 payload, an HVM1 reply slot, or an ETW record.
 *
 * ⛔ NO IDENTITY CROSSES THIS INTERFACE. Nothing here carries a host resource
 * token or virtio `resid`, a PID, a KMT/NT/allocation handle, a GPUVA, an
 * allocation-list index, an allocation generation, or an HTS1 session
 * capability. An outer allocation appears only as an opaque UMD-assigned
 * `outer_allocation_token`, and the capability transits exactly once, sealed
 * inside the 72-byte HQA1 packet that `build_queue_attach` produces. The
 * prohibitions are kept by ABSENCE, which is why every record below has a
 * `_Static_assert` on its size: adding a field to smuggle one in breaks the
 * build.
 *
 * ⛔ NO ICD -> D3D/DXGI EDGE. The up half (HeliosTranslatorHostCallbacksV1)
 * terminates at the D3D UMD bridge and the WDDM runtime callback table. It is
 * not an edge to D3D or DXGI, which is what keeps §13.1's forbidden cycle
 * `ICD -> DXGI -> D3D UMD -> translator -> ICD` unformable. §13.2's static
 * import test (`dxgi.dll`/`d3d11.dll`/`d3d12.dll` rejected from the ICD DLL,
 * `vulkan-1.dll` rejected from the translator-bearing UMD binaries) is the
 * enforcement; the direction comment on every slot is the specification.
 *
 * ⛔ WHAT THIS HEADER IS NOT: an implementation. The behavioural rules — who may
 * block, what lock may be held, what a scope makes legal, what each refusal
 * means — are documented per slot in `protocol/src/translator_dispatch.rs` and
 * summarised here. A consumer that reads only the field names will get the ABI
 * right and the contract wrong.
 */

#ifndef HELIOS_TRANSLATOR_DISPATCH_H
#define HELIOS_TRANSLATOR_DISPATCH_H

#include <stddef.h>
#include <stdint.h>

/* For HELIOS_PACKAGE_GENERATION and the HOB1 vocabulary this interface reuses
 * verbatim: HELIOS_HOB1_ACCESS_*, HELIOS_HOB1_OPERAND_KIND_*,
 * HELIOS_HOB1_OPERAND_ALIGN, and the 4096/8192/15 MiB bounds. Reusing them is
 * deliberate — the sealed tables below become HOB1 tables, and a second
 * declaration of the same vocabulary is how two halves of one encoder drift. */
#include "helios_wddm.h"

/* For the HQA1/HTS1 vocabulary this interface's parameter records are drawn
 * from — HELIOS_ENGINE_CLASS_*, HELIOS_HQA1_FLAG_*,
 * HELIOS_HTS1_MAX_ENDPOINTS_PER_SESSION — and for the two record sizes this
 * header would otherwise hand-copy (HELIOS_HQA1_SIZE, HELIOS_HTS1_ENDPOINT_SIZE;
 * see the _Static_asserts on HELIOS_TRANSLATOR_HQA1_BYTES below).
 *
 * The alternative — forward-declaring the records and guarding each check on
 * `#if defined(...)` — was rejected: it makes a validator silently weaker in a
 * translation unit that includes one header and not the other, and a check that
 * quietly does less is the fail-OPEN direction. The records are still only ever
 * POINTED TO by this ABI; the include is for the vocabulary and the sizes. */
#include "helios_translation_session.h"

#if defined(__cplusplus)
extern "C" {
#endif

#if defined(__cplusplus)
#define HELIOS_TRANSLATOR_STATIC_ASSERT(cond, msg) static_assert(cond, msg)
#define HELIOS_TRANSLATOR_ALIGNOF(type)            alignof(type)
#else
#define HELIOS_TRANSLATOR_STATIC_ASSERT(cond, msg) _Static_assert(cond, msg)
#define HELIOS_TRANSLATOR_ALIGNOF(type)            _Alignof(type)
#endif

/* The package is x86-64 only, which is what lets one set of byte counts be
 * normative for Rust and C at once, and lets the calling convention be a
 * non-question (on x64 there is one). A 32-bit build would silently produce
 * 4-byte pointers and a table half the asserted size. */
#if defined(_WIN32) && !defined(_WIN64)
#error "helios_translator_dispatch.h: the Helios package is x86-64 only."
#endif
#define HELIOS_TRANSLATOR_CALL

/* Mesa marks the one exported entry point with this; consumers leave it empty
 * and resolve the symbol at run time. */
#if !defined(HELIOS_TRANSLATOR_DISPATCH_EXPORT)
#define HELIOS_TRANSLATOR_DISPATCH_EXPORT
#endif

/* ------------------------------------------------------------------------ */
/* The single entry point                                                    */
/* ------------------------------------------------------------------------ */

/*
 * The ONE exported symbol a translator resolves on the Helios ICD module, and
 * the ONLY sanctioned way a translator reaches the ICD (§2 item 8, §10.4
 * 1182-1184, §13.2 3355-3358):
 *
 *   - NOT through the Vulkan loader. DXVK's `vulkan_loader.cpp` search is
 *     disabled in Helios builds and vkd3d receives this same table.
 *   - NOT through the Helios WSI layer, which sits above the loader and has no
 *     edge to a translator instance.
 *   - NOT by GetProcAddress-by-name discovery of any other ICD export; §17.3
 *     deletes the ten name-resolved exports this replaces.
 *
 * Resolving it on a module the consumer already holds discovers nothing it did
 * not already have, which is what distinguishes it from the "global discovery"
 * §10.4 forbids.
 *
 *   HMODULE icd = ...;   // the module this process already loaded
 *   PFN_helios_icd_create_translator_v1 create =
 *       (PFN_helios_icd_create_translator_v1)
 *       GetProcAddress(icd, HELIOS_ICD_CREATE_TRANSLATOR_V1_NAME);
 */
#define HELIOS_ICD_CREATE_TRANSLATOR_V1_NAME "helios_icd_create_translator_v1"

/* The ABI version of this interface, independent of the package generation.
 * Both are checked: the package generation says "these binaries are one
 * package", this says "these binaries agree on the shape of this table". A
 * change to any declaration in this header bumps this AND the package
 * generation ordinal. */
#define HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION 1u

/*
 * §10.4 line 1368: "A translator instance is created with
 * HELIOS_TRANSLATOR_SUBMISSION_MODE_RECORD_ONLY." This is that constant, and
 * this is its only C declaration — DXVK and vkd3d use THIS symbol, never a
 * private copy.
 *
 * It means the five-point contract of §10.4 (1373-1385) in full, and in
 * particular point 5: the instance performs NO queue-work KMT
 * Render/SubmitCommand/HWQueue call and signals no independent GPU timeline.
 * §10.7 (1742-1747) adds the structural consequence: a record-only instance
 * creates no raw HVC1 GPU queue context; its outer D3D contexts attach to the
 * pre-existing HTS1 session through HQA1. That, and the normal native-Vulkan
 * form which never touches this ABI at all, are the only two session endpoint
 * attachment forms.
 */
#define HELIOS_TRANSLATOR_SUBMISSION_MODE_RECORD_ONLY 1u

/* Zero is never a wildcard and never a default: a zeroed create-info must be
 * refused, not silently promoted to record-only. RECORD_ONLY is the only value
 * this generation defines; every other value, including a future one, is
 * HELIOS_TRANSLATOR_STATUS_SUBMISSION_MODE. */
#define HELIOS_TRANSLATOR_SUBMISSION_MODE_INVALID 0u

/* ------------------------------------------------------------------------ */
/* Status codes — every named refusal this ABI can produce                   */
/* ------------------------------------------------------------------------ */

/*
 * Both halves return this. `int32_t` on the wire rather than the enum type,
 * because a foreign implementation returning a value this build does not know
 * must be DECODED AND REFUSED, never assumed: an unknown status is a package
 * mismatch, and it is a hard failure (fail translator device creation, or
 * remove the device if one exists), never a success, a retry, or a fallback.
 *
 * The values are explicit and pinned by a Rust test; a consumer that maps them
 * to strings may index by them.
 */
typedef int32_t HeliosTranslatorStatusCode;

enum HeliosTranslatorStatusValues {
    /* The only non-refusal. */
    HELIOS_TRANSLATOR_STATUS_OK = 0,

    /* Handshake — these fail translator device creation (§10.9 row 2860). */
    HELIOS_TRANSLATOR_STATUS_NULL_ARGUMENT = 1,
    /* `struct_bytes` is not exactly this build's sizeof. Not "at least": a short
     * record cannot be zero-extended and a long one cannot be truncated,
     * because either is a partial adopt. */
    HELIOS_TRANSLATOR_STATUS_STRUCT_BYTES = 2,
    HELIOS_TRANSLATOR_STATUS_ABI_VERSION = 3,
    /* §17.1: "Generation mismatch is fatal." Zero is not a wildcard. */
    HELIOS_TRANSLATOR_STATUS_PACKAGE_GENERATION = 4,
    HELIOS_TRANSLATOR_STATUS_SUBMISSION_MODE = 5,
    HELIOS_TRANSLATOR_STATUS_HOST_CALLBACKS = 6,
    HELIOS_TRANSLATOR_STATUS_ADAPTER_LUID = 7,
    HELIOS_TRANSLATOR_STATUS_ADAPTER_UNAVAILABLE = 8,
    /* HTS1 INIT failed (§10.4 1188-1203; §10.9 row 2860). */
    HELIOS_TRANSLATOR_STATUS_SESSION_INIT = 9,
    HELIOS_TRANSLATOR_STATUS_SESSION_CAPACITY = 10,
    /* The session is poisoned or lost. Every later call returns this; the UMD
     * must fail or remove the device. There is no un-poison and no retry
     * (§10.9 rows 2864-2865). */
    HELIOS_TRANSLATOR_STATUS_SESSION_POISONED = 11,

    /* Endpoints and outer contexts. */
    HELIOS_TRANSLATOR_STATUS_ENDPOINT_CAPACITY = 12,
    HELIOS_TRANSLATOR_STATUS_UNKNOWN_ENDPOINT = 13,
    HELIOS_TRANSLATOR_STATUS_ENGINE_CLASS = 14,
    /* §10.4 table row `offset 64`: exactly one context-kind flag set. */
    HELIOS_TRANSLATOR_STATUS_CONTEXT_FLAGS = 15,
    /* §10.4 (1233): nonzero, monotonically increasing, never reused. */
    HELIOS_TRANSLATOR_STATUS_CONTEXT_GENERATION = 16,
    HELIOS_TRANSLATOR_STATUS_UNKNOWN_CONTEXT = 17,
    /* Invariant 13: an outer D3D context attaches exactly once. */
    HELIOS_TRANSLATOR_STATUS_CONTEXT_ALREADY_ATTACHED = 18,

    /* Scopes (§10.4 1387-1396). */
    /* THE refusal §10.4 (1388-1390) demands: a queue entry point with no live
     * outer-operation scope. */
    HELIOS_TRANSLATOR_STATUS_NO_OUTER_SCOPE = 19,
    HELIOS_TRANSLATOR_STATUS_SCOPE_ALREADY_OPEN = 20,
    HELIOS_TRANSLATOR_STATUS_SCOPE_FOREIGN_THREAD = 21,
    HELIOS_TRANSLATOR_STATUS_SCOPE_NOT_SEALED = 22,
    HELIOS_TRANSLATOR_STATUS_SCOPE_ALREADY_SEALED = 23,
    HELIOS_TRANSLATOR_STATUS_SCOPE_STILL_LIVE = 24,
    HELIOS_TRANSLATOR_STATUS_BUFFER_TOO_SMALL = 25,
    /* 4096 uses / 8192 operands / 15 MiB. §10.4 (1297-1304) requires the split
     * before sealing, and §10.9 (2866) forbids truncating instead. */
    HELIOS_TRANSLATOR_STATUS_BATCH_BOUND_EXCEEDED = 26,
    /* A nonzero use offset on a D3D11 physical context: HOB1's identity_kind 1
     * arm is an allocation-list index with nowhere to put one. */
    HELIOS_TRANSLATOR_STATUS_D3D11_SUBRANGE_USE = 27,
    /* Refused outright rather than scope-gated: vkQueuePresentKHR on a
     * record-only instance (§17.3 3987-3989). */
    HELIOS_TRANSLATOR_STATUS_QUEUE_ENTRY_POINT_REFUSED = 28,
    HELIOS_TRANSLATOR_STATUS_FOREIGN_VULKAN_HANDLE = 29,
    /* The resolved procedure's owning module is the Vulkan loader or the WSI
     * layer (§13.2 3357-3358). */
    HELIOS_TRANSLATOR_STATUS_LOADER_PROVENANCE = 30,

    /* The up half. */
    HELIOS_TRANSLATOR_STATUS_REENTRANT_JOIN = 31,
    HELIOS_TRANSLATOR_STATUS_HOST_CALLBACK_FAILED = 32,
    /* §10.9 (2867): device lost/failure; never use ring-zero completion or a
     * later queue value as success. */
    HELIOS_TRANSLATOR_STATUS_DEVICE_LOST = 33,

    /* Field-level. */
    HELIOS_TRANSLATOR_STATUS_RESERVED_NON_ZERO = 34,
    HELIOS_TRANSLATOR_STATUS_DISPOSITION = 35,
    /* access_flags zero, outside HELIOS_HOB1_ACCESS_MASK, or PRIMARY_WRITE
     * without WRITE (§10.4 line 1283: "primary implies write"). */
    HELIOS_TRANSLATOR_STATUS_ACCESS_FLAGS = 36,
    /* operand_kind is not GENERATED_RESOURCE, or encoded_width is neither 4 nor
     * 8 (§10.4 1284-1286: "arbitrary patches and raw renderer IDs are
     * rejected"). */
    HELIOS_TRANSLATOR_STATUS_OPERAND_ENCODING = 37,
    /* HeliosSealedBatchV1::batch_id is zero (§10.4 table row `offset 32`). */
    HELIOS_TRANSLATOR_STATUS_BATCH_ID = 38,

    /* Distinct causes that used to borrow a neighbour's code.
     *
     * Each of the seven below replaces a refusal previously returned under a
     * code whose DOCUMENTED meaning was a different cause. The thesis of this
     * enum is that a code names exactly one cause: a gate or ETW record showing
     * BATCH_BOUND_EXCEEDED must not send someone hunting a size-split bug when
     * the defect is a four-byte alignment error. They are APPENDED, so a
     * consumer built against the previous header still decodes 0..=38
     * identically. */
    /* A record's session_generation is zero or is not the live HTS1 session's.
     * Distinct from SESSION_INIT, which means the session never came up. */
    HELIOS_TRANSLATOR_STATUS_SESSION_GENERATION = 39,
    /* A sealed use names an illegal byte range: zero byte_length, or
     * byte_offset + byte_length overflowing 64 bits. Distinct from
     * BUFFER_TOO_SMALL (a *destination* buffer) and from BATCH_BOUND_EXCEEDED
     * (a producer that failed to split). */
    HELIOS_TRANSLATOR_STATUS_SEALED_USE_RANGE = 40,
    /* A sealed operand's use_index is outside the batch's use table. An index
     * error, not a bound overrun. */
    HELIOS_TRANSLATOR_STATUS_OPERAND_USE_INDEX = 41,
    /* A progress value contradicts the record carrying it: a COMMITTED close
     * with no value, an ABANDONED one with a value, or a progress result whose
     * completed exceeds its last_submitted. A malformed argument, not a failed
     * callback (HOST_CALLBACK_FAILED) and not a reserved field. */
    HELIOS_TRANSLATOR_STATUS_PROGRESS_VALUE = 42,
    /* destroy_instance was called while an outer context is still attached.
     * §17.5 requires session/endpoint/HQA1/HQC1 teardown on every
     * queue/device/error path, so the bridge must detach every context first;
     * without this code that precondition would be prose that fails open. */
    HELIOS_TRANSLATOR_STATUS_CONTEXT_STILL_ATTACHED = 43,
    /* A HeliosSealedResourceUseV1::outer_allocation_token names no allocation
     * of the consumer's device. The token is opaque to the ICD, so only the
     * bridge can make this determination — and it must, because an unresolvable
     * token cannot become an allocation-list index or a GPUVA. */
    HELIOS_TRANSLATOR_STATUS_UNKNOWN_ALLOCATION_TOKEN = 44,
    /* An operand's encoded_width payload bytes are not zero. §10.4: an operand
     * "identifies only a generated resource operand whose payload bytes are
     * zero" — a nonzero placeholder is how a raw host resource id would reach
     * the host at a position the operand table blesses. */
    HELIOS_TRANSLATOR_STATUS_PAYLOAD_PLACEHOLDER_NON_ZERO = 45,

    /* The highest defined code. Bounds-check before indexing a name table. */
    HELIOS_TRANSLATOR_STATUS_MAX = 45
};

/* ------------------------------------------------------------------------ */
/* Opaque handles                                                            */
/* ------------------------------------------------------------------------ */

/* The ICD-owned translator instance: one Mesa `vn_instance`, one HTS1 session,
 * one host Venus namespace. §10.4 (1211-1213): multiple instances in one process
 * receive different sessions, host contexts, capabilities, object namespaces,
 * and ring allocators, so this handle is the instance's whole identity and
 * nothing about it is process-global. Hold it, compare it, pass it back; never
 * dereference it. */
typedef struct HeliosTranslatorInstance_T *HeliosTranslatorHandle;

/* The ICD-owned outer-operation scope. See `open_outer_scope`. */
typedef struct HeliosTranslatorScope_T *HeliosTranslatorScope;

/* Records this interface REFERENCES rather than restates: the ABI only ever
 * points at them. Their definitions come from helios_translation_session.h,
 * included above. */
struct HeliosQueueAttachV1;          /* HQA1, 72 bytes, §10.4 1220-1235 */
struct HeliosTranslationEndpointV1;  /* endpoint descriptor, 16 bytes */

/* Exact size of the `build_queue_attach` out buffer. */
#define HELIOS_TRANSLATOR_HQA1_BYTES 72u
/* Exact size of one `enumerate_endpoints` array element. */
#define HELIOS_TRANSLATOR_ENDPOINT_BYTES 16u

/* The two constants this header restates so a slot's parameter contract can be
 * asserted without spelling out a sibling record's name must EQUAL that record.
 * The Rust side pins them against sizeof; this is the C twin of that pin, and it
 * is why the include above is worth having: without it these two numbers are
 * hand-copied literals that nothing checks. */
HELIOS_TRANSLATOR_STATIC_ASSERT(HELIOS_TRANSLATOR_HQA1_BYTES == HELIOS_HQA1_SIZE, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(HELIOS_TRANSLATOR_HQA1_BYTES == sizeof(HeliosQueueAttachV1), "");
HELIOS_TRANSLATOR_STATIC_ASSERT(HELIOS_TRANSLATOR_ENDPOINT_BYTES == HELIOS_HTS1_ENDPOINT_SIZE, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(HELIOS_TRANSLATOR_ENDPOINT_BYTES ==
                                    sizeof(HeliosTranslationEndpointV1),
                                "");

/* ------------------------------------------------------------------------ */
/* Scope disposition and progress flags                                      */
/* ------------------------------------------------------------------------ */

/*
 * How a scope ended. The distinction is not bookkeeping: §10.4 (1346-1352) makes
 * an outer-allocation-backed operation real only in the actual outer batch that
 * names its allocation, and forbids exposing a deferred success before that.
 * COMMITTED is what tells the ICD the deferred operations in this batch have
 * happened; ABANDONED is what tells it they have not.
 *
 * Zero is refused, not defaulted: defaulting to COMMITTED would report
 * unrealised work as done, and defaulting to ABANDONED would silently drop a
 * submitted batch.
 */
#define HELIOS_TRANSLATOR_SCOPE_DISPOSITION_INVALID   0u
#define HELIOS_TRANSLATOR_SCOPE_DISPOSITION_COMMITTED 1u
#define HELIOS_TRANSLATOR_SCOPE_DISPOSITION_ABANDONED 2u

/* The outer device is lost or removed. The ICD converts it to
 * VK_ERROR_DEVICE_LOST and must never treat the reported progress values as
 * satisfied (§10.9 row 2867). */
#define HELIOS_TRANSLATOR_PROGRESS_FLAG_DEVICE_LOST 1u
#define HELIOS_TRANSLATOR_PROGRESS_FLAGS_MASK       1u

/* ------------------------------------------------------------------------ */
/* The up half: callbacks the D3D UMD bridge implements                      */
/* ------------------------------------------------------------------------ */

/*
 * A blocking C60 synchronous-progress join request.
 *
 * `required_progress_value`:
 *   - ZERO means "everything pending on this outer context": the bridge seals
 *     and submits whatever the translator has recorded, signals the next HQC1
 *     value from that exact context, and waits for it. This is the
 *     vkQueueWaitIdle / vkDeviceWaitIdle / teardown form (§17.4 4125-4128,
 *     §17.5 4239-4243).
 *   - NONZERO means "at least this value", where the value came from a
 *     HeliosOuterScopeCloseV1::progress_value the bridge reported for a batch
 *     this instance recorded. This is the fence-status / query-WAIT form.
 *
 * A value the bridge never issued is HOST_CALLBACK_FAILED; it is never waited
 * on speculatively, because a wait for a value that will never be signalled is
 * the hang §10.9 (2867) exists to prevent.
 */
typedef struct HeliosSyncProgressJoinV1 {
    uint32_t struct_bytes;             /* 0  == sizeof(HeliosSyncProgressJoinV1) */
    uint32_t abi_version;              /* 4  == HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION */
    uint64_t required_progress_value;  /* 8  see above; zero = everything pending */
    /* 16: anti-stale echo — the generation of the outer context that
     * `host_context_cookie` names, as HeliosOuterContextAttachV1 recorded it.
     *
     * ⛔ This field is why the up half is not the one place in the ABI where a
     * stale value is a use-after-free. The join is the only call in either
     * direction that makes one party dereference a pointer the other party
     * owns. Without it, a D3D12 app destroying a command queue on thread A
     * (bridge frees its queue object; detach_outer_context in flight) while
     * thread B sits inside vkWaitForFences hands the bridge a cookie it has
     * already freed and NO field to compare it against. §14's "Late host
     * callback" row requires exactly this: "context … generations are
     * validated; stale callback releases only its own held ref and cannot
     * complete or replace a newer object."
     *
     * The bridge compares it against the live generation of the context that
     * cookie belongs to and refuses a mismatch with
     * HELIOS_TRANSLATOR_STATUS_UNKNOWN_CONTEXT — a refusal, never a lookup that
     * finds the right context. Generations are never reused within a session
     * (§10.4), so a mismatch is always a stale caller, never an ambiguity. */
    uint64_t context_generation;
} HeliosSyncProgressJoinV1;

HELIOS_TRANSLATOR_STATIC_ASSERT(sizeof(HeliosSyncProgressJoinV1) == 24,
                                "HeliosSyncProgressJoinV1 must be 24 bytes");
HELIOS_TRANSLATOR_STATIC_ASSERT(HELIOS_TRANSLATOR_ALIGNOF(HeliosSyncProgressJoinV1) == 8, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosSyncProgressJoinV1, struct_bytes) == 0, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosSyncProgressJoinV1, abi_version) == 4, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosSyncProgressJoinV1, required_progress_value) == 8, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosSyncProgressJoinV1, context_generation) == 16, "");

/*
 * The outer context's progress, as the bridge knows it.
 *
 * ⛔ `completed_progress_value` is OUTER GPU COMPLETION and only that. Invariant
 * 15 and §10.9 (2868): ring-zero/control completion never satisfies a GPU wait,
 * a queue idle, a device idle, a query WAIT, or a destruction dependency, and no
 * context may borrow another's fence.
 */
typedef struct HeliosSyncProgressResultV1 {
    uint32_t struct_bytes;                    /* 0  == sizeof */
    uint32_t abi_version;                     /* 4 */
    uint64_t completed_progress_value;        /* 8  highest HQC1 value known complete */
    uint64_t last_submitted_progress_value;   /* 16 completed <= last_submitted always */
    uint32_t flags;                           /* 24 HELIOS_TRANSLATOR_PROGRESS_FLAG_* */
    uint32_t reserved;                        /* 28 zero */
} HeliosSyncProgressResultV1;

HELIOS_TRANSLATOR_STATIC_ASSERT(sizeof(HeliosSyncProgressResultV1) == 32,
                                "HeliosSyncProgressResultV1 must be 32 bytes");
HELIOS_TRANSLATOR_STATIC_ASSERT(HELIOS_TRANSLATOR_ALIGNOF(HeliosSyncProgressResultV1) == 8, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosSyncProgressResultV1, struct_bytes) == 0, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosSyncProgressResultV1, abi_version) == 4, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosSyncProgressResultV1, completed_progress_value) == 8, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosSyncProgressResultV1, last_submitted_progress_value) == 16, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosSyncProgressResultV1, flags) == 24, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosSyncProgressResultV1, reserved) == 28, "");

/* Direction: ICD -> UMD bridge. MAY BLOCK. */
typedef HeliosTranslatorStatusCode(HELIOS_TRANSLATOR_CALL *PFN_helios_translator_sync_progress_join)(
    void *host_context_cookie,
    const HeliosSyncProgressJoinV1 *request,
    HeliosSyncProgressResultV1 *out_result);

/* Direction: ICD -> UMD bridge. MUST NOT BLOCK.
 *
 * `context_generation` is the same anti-stale echo the join carries, for the
 * same reason: this call dereferences a cookie the bridge owns. It is passed as
 * a parameter rather than in a record because the query has no other input. */
typedef HeliosTranslatorStatusCode(HELIOS_TRANSLATOR_CALL *PFN_helios_translator_sync_progress_query)(
    void *host_context_cookie,
    uint64_t context_generation,
    HeliosSyncProgressResultV1 *out_result);

/*
 * The up half: the ONLY two things the ICD may ask the D3D UMD bridge to do.
 *
 * §17.5 (4239-4240): "bridge12.rs exposes only bounded pure-control and
 * HQC1-join callbacks to the record-only Mesa instance." Two slots, both about
 * the HQC1 milestone, and nothing else — no allocate, no submit-on-my-behalf, no
 * present, no resource open, no D3D or DXGI verb of any kind. A third slot is an
 * ABI-version bump and must be justified against §13.1's acyclicity proof,
 * because every up-slot is a potential edge back into D3D.
 *
 * Neither slot is optional. A record-only instance whose GPU-dependent
 * synchronous calls cannot join the outer milestone would have to approximate
 * one, and §10.4 (1366) forbids exactly that: "There is no blocking host Venus
 * vkQueueWaitIdle, shared ring-head sample, or control-fence approximation."
 */
typedef struct HeliosTranslatorHostCallbacksV1 {
    uint32_t struct_bytes;        /* 0  == sizeof(HeliosTranslatorHostCallbacksV1) */
    uint32_t abi_version;         /* 4  == HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION */
    uint64_t package_generation;  /* 8  == HELIOS_PACKAGE_GENERATION */

    /*
     * 16: MAY BLOCK. The ICD holds no translator, session, endpoint, scope, or
     * object-graph lock across this call; the bridge holds no UMD, DXVK/vkd3d,
     * Mesa, or session lock while waiting (§13.3; §17.4 4126-4128).
     *
     * The C60 GPU-dependent synchronous join (§10.4 1354-1360): seal-and-submit
     * pending work on the exact outer context named by `host_context_cookie`,
     * FromGpu-signal HQC1 from that same context, then one FromCpu event wait.
     * Only after that milestone completes may HVC1 fetch bounded result bytes.
     *
     * REENTRANCY: this is the ABI's one sanctioned reentrant edge. While the
     * ICD's Vulkan call is on the stack the bridge will call back DOWN into
     * open/seal/copy/close for the same outer context, because that is how
     * "submit pending batches" is expressed. It is safe because no lock is held
     * in either direction. A join nested inside a join is a cycle and must be
     * refused with HELIOS_TRANSLATOR_STATUS_REENTRANT_JOIN.
     *
     * THE CUT-AND-REOPEN PROTOCOL. The join almost always arrives with a scope
     * already open — the translator is recording inside the D3D11 flush or
     * D3D12 ECL that opened it — and neither obvious order works: opening a
     * second scope is SCOPE_ALREADY_OPEN, and sealing/closing the caller's
     * scope and stopping there leaves the bridge's own outer frame holding a
     * dead HeliosTranslatorScope. The bridge therefore performs exactly this,
     * in this order, on the calling thread:
     *
     *   1. scope live and recorded into: seal_outer_scope, copy_sealed_batch,
     *      encode HOB1, submit through the runtime callback, FromGpu-signal the
     *      next HQC1 value V from this exact outer context, then
     *      close_outer_scope with COMMITTED and V;
     *   2. scope live but nothing recorded: close it ABANDONED (no empty batch
     *      is ever sealed) and signal V from the context anyway — a signal on
     *      the OUTER context, never a cleanup submission on a lower queue;
     *   3. REOPEN: open_outer_scope on the same context, storing the new handle
     *      in place of the old one. The bridge is the sole owner of the scope
     *      handle, so no dangling handle ever exists;
     *   4. the one event-backed FromCpu wait for V (or for
     *      required_progress_value when nonzero), then fill the result.
     *
     * WHAT THE CUT INVALIDATES, AND WHAT THE ICD MUST RE-ACQUIRE. Step 1 is a
     * SEAL, and a seal is terminal: §10.4 (1354-1360) requires the join to
     * "force the owning outer UMD to submit pending batches", and this ABI has
     * exactly one way to say that. So a join does not suspend the caller's
     * scope — it ENDS it, and the scope the caller resumes into is a different
     * one with a different batch:
     *
     *   1. the scope handle is dead. The ICD must never cache a
     *      HeliosTranslatorScope across an up-call; its thread-current scope
     *      identity changes across a join by design and it re-reads it on
     *      return. This is the one place a scope handle is invalidated by
     *      something other than close_outer_scope on it directly;
     *   2. everything derived from the sealed batch dies with it — the
     *      context-local batch ID, every index into that batch's use and
     *      operand tables, and any partially built record. The reopened scope
     *      starts an empty batch at the next batch ID (gaps legal, reuse not);
     *   3. a join may therefore only be INITIATED AT A COMPLETE GENERATED
     *      COMMAND/API OPERATION BOUNDARY, exactly like the size split of §10.4
     *      (1297-1304) and for the same reason: the seal it performs cannot
     *      bisect a generated operation. A GPU-dependent synchronous Vulkan
     *      call is such a boundary by construction, which is why the triggering
     *      set is the fence/query/idle calls and nothing else. No refusal here
     *      can detect a violation — the ICD is the only party that knows where
     *      its operation boundaries are.
     */
    PFN_helios_translator_sync_progress_join sync_progress_join;

    /*
     * 24: MUST NOT BLOCK. No lock, no runtime callback, no event wait, and no
     * re-entry into the ICD — the bridge answers from state it already has.
     *
     * The nonblocking half of C60 (§10.4 1359-1360): "A nonblocking status query
     * returns the locally known not-ready state without a control round trip
     * when the milestone has not completed." vkGetFenceStatus, vkGetEventStatus,
     * and vkGetQueryPoolResults WITHOUT WAIT use this and must not be turned
     * into a join.
     */
    PFN_helios_translator_sync_progress_query sync_progress_query;
} HeliosTranslatorHostCallbacksV1;

HELIOS_TRANSLATOR_STATIC_ASSERT(sizeof(HeliosTranslatorHostCallbacksV1) == 32,
                                "the up-half table must be 32 bytes: two slots, no third");
HELIOS_TRANSLATOR_STATIC_ASSERT(HELIOS_TRANSLATOR_ALIGNOF(HeliosTranslatorHostCallbacksV1) == 8, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosTranslatorHostCallbacksV1, struct_bytes) == 0, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosTranslatorHostCallbacksV1, abi_version) == 4, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosTranslatorHostCallbacksV1, package_generation) == 8, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosTranslatorHostCallbacksV1, sync_progress_join) == 16, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosTranslatorHostCallbacksV1, sync_progress_query) == 24, "");

/* ------------------------------------------------------------------------ */
/* Create info and instance                                                  */
/* ------------------------------------------------------------------------ */

/*
 * Everything the ICD needs to create one record-only translator instance.
 *
 * It carries NO handle: not the adapter's, not a KMT device's, not a D3D
 * device's. The ICD opens the exact adapter itself from the LUID and creates its
 * own raw KMT device (§10.4 1189), which is what makes the "identical
 * hKmdProcess, adapter object, node, and package generation" gate at 1208-1210 a
 * mandatory observed target gate rather than an inference from a PID.
 *
 * The LUID is the OS adapter identity, not a handle: it names no object, confers
 * no access, and is already public to anything that can call
 * IDXGIAdapter::GetDesc. It is here because the alternative — letting the ICD
 * DISCOVER which adapter to open — is exactly what §3 (363-369) forbids.
 */
typedef struct HeliosTranslatorCreateInfoV1 {
    uint32_t struct_bytes;                 /* 0  == sizeof(HeliosTranslatorCreateInfoV1) */
    uint32_t abi_version;                  /* 4  == HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION */
    uint64_t package_generation;           /* 8  == HELIOS_PACKAGE_GENERATION */
    uint32_t submission_mode;              /* 16 == ..._SUBMISSION_MODE_RECORD_ONLY */
    uint32_t requested_endpoint_capacity;  /* 20 1..=session endpoint maximum */
    uint32_t adapter_luid_low;             /* 24 LUID::LowPart; {0,0} is refused */
    int32_t  adapter_luid_high;            /* 28 LUID::HighPart */
    /* 32: REQUIRED, never NULL. The pointer and the table it names must stay
     * valid until destroy_instance returns; the ICD may copy the table or retain
     * the pointer, so the bridge must treat it as retained. */
    const HeliosTranslatorHostCallbacksV1 *host_callbacks;
} HeliosTranslatorCreateInfoV1;

HELIOS_TRANSLATOR_STATIC_ASSERT(sizeof(HeliosTranslatorCreateInfoV1) == 40,
                                "HeliosTranslatorCreateInfoV1 must be 40 bytes");
HELIOS_TRANSLATOR_STATIC_ASSERT(HELIOS_TRANSLATOR_ALIGNOF(HeliosTranslatorCreateInfoV1) == 8, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosTranslatorCreateInfoV1, struct_bytes) == 0, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosTranslatorCreateInfoV1, abi_version) == 4, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosTranslatorCreateInfoV1, package_generation) == 8, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosTranslatorCreateInfoV1, submission_mode) == 16, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosTranslatorCreateInfoV1, requested_endpoint_capacity) == 20, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosTranslatorCreateInfoV1, adapter_luid_low) == 24, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosTranslatorCreateInfoV1, adapter_luid_high) == 28, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosTranslatorCreateInfoV1, host_callbacks) == 32, "");

/* Forward reference: the instance names its table. */
struct HeliosTranslatorDispatchV1;

/*
 * What the entry point returns: the instance handle, its const dispatch table,
 * the one VkInstance its HTS1 INIT created, and the two session facts the UMD
 * needs in order to encode HOB1/HOS1.
 *
 * The caller zero-initialises this and sets struct_bytes/abi_version before the
 * call; the ICD fills the rest. On any refusal the ICD leaves it zeroed apart
 * from those two fields — there is no partially created instance.
 */
typedef struct HeliosTranslatorInstanceV1 {
    uint32_t struct_bytes;       /* 0  == sizeof, set by the caller */
    uint32_t abi_version;        /* 4  set by the caller */
    HeliosTranslatorHandle handle; /* 8  NULL on any refusal */
    /* 16: ICD-owned, const, valid exactly as long as `handle`. Not copied out,
     * not freed by the caller, and NOT shared between instances — two
     * vn_instance objects in one process are two sessions and two tables (§10.4
     * 1211-1213). Run helios_translator_check_dispatch() on it before using a
     * single slot. */
    const struct HeliosTranslatorDispatchV1 *dispatch;
    /* 24: the one VkInstance this instance's HTS1 INIT created. Never NULL on
     * success.
     *
     * ⭐ THIS IS THE ONLY ROUTE BY WHICH A TRANSLATOR OBTAINS A VkInstance, and
     * it exists because there is no other legal one. §10.4's INIT paragraph
     * creates "one distinct host virgl/Venus context and VkInstance" and then
     * says "No second VkInstance may be created in that host context"; §10.9's
     * HTS1 row makes "a second instance is requested in one host context" a
     * FAIL TRANSLATOR DEVICE CREATION condition. A translator that resolved
     * vkCreateInstance through get_instance_proc_addr and minted its own would
     * be precisely that case, so that name is refused there (NULL, counted in
     * withheld_proc_addr_refused).
     *
     * Typed void* rather than VkInstance because this header must not include
     * vulkan.h; VkInstance is a dispatchable (pointer) handle, so the
     * representation is exact. The consumer casts it once, at the boundary.
     *
     * ⛔ It is NOT an identity the ICD looks anything up by: two translator
     * instances in one process have two VkInstances and two sessions, and the
     * ICD compares a presented handle against its own instance's rather than
     * searching a table (FOREIGN_VULKAN_HANDLE). */
    void *vk_instance;
    /* 32: the nonzero HTS1 session generation INIT returned. The UMD needs it
     * for HOB1 offset 16 and HOS1 offset 16. A generation counter, not a key:
     * no lookup uses it (invariant 10). */
    uint64_t session_generation;
    uint32_t endpoint_capacity;  /* 40 granted; <= requested, nonzero */
    /* 44: echoed RECORD_ONLY. A cross-check, not the tag — §13.2 (3359) requires
     * a non-forgeable record-only tag, which is ICD-internal state. The bridge
     * must refuse the instance if this is not the mode it asked for. */
    uint32_t submission_mode;
} HeliosTranslatorInstanceV1;

HELIOS_TRANSLATOR_STATIC_ASSERT(sizeof(HeliosTranslatorInstanceV1) == 48,
                                "HeliosTranslatorInstanceV1 must be 48 bytes");
HELIOS_TRANSLATOR_STATIC_ASSERT(HELIOS_TRANSLATOR_ALIGNOF(HeliosTranslatorInstanceV1) == 8, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosTranslatorInstanceV1, struct_bytes) == 0, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosTranslatorInstanceV1, abi_version) == 4, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosTranslatorInstanceV1, handle) == 8, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosTranslatorInstanceV1, dispatch) == 16, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosTranslatorInstanceV1, vk_instance) == 24, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosTranslatorInstanceV1, session_generation) == 32, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosTranslatorInstanceV1, endpoint_capacity) == 40, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosTranslatorInstanceV1, submission_mode) == 44, "");

/* ------------------------------------------------------------------------ */
/* Down-half parameter records                                               */
/* ------------------------------------------------------------------------ */

/*
 * What the bridge asks the ICD to seal into an HQA1 packet.
 *
 * §10.4 (1215-1218): "Before the D3D UMD calls the runtime's context-create
 * callback, its direct bridge selects the translator's physical lower-queue
 * endpoint and supplies the following 72-byte pointer-free HQA1 as the complete
 * create-context private data." The bridge selects; the ICD, which is the only
 * party holding the session generation and the admission capability, fills it.
 */
typedef struct HeliosQueueAttachRequestV1 {
    uint32_t struct_bytes;        /* 0  == sizeof */
    uint32_t abi_version;         /* 4 */
    /* 8: UMD-chosen; nonzero, monotonically increasing, never reused within this
     * HTS1 session (§10.4 table row `offset 56`). */
    uint64_t context_generation;
    /* 16: selected from enumerate_endpoints and nowhere else. Several logical
     * D3D12 queues may select the same endpoint (§10.6 step 1). */
    uint32_t endpoint_id;
    /* 20: HELIOS_ENGINE_CLASS_* — the engine class of the OUTER D3D context
     * this HQA1 is for: the D3D12 command-queue type, or the D3D11 node.
     *
     * ⭐ The bridge states it; the ICD checks it against the endpoint it
     * resolves. HQA1's `offset 44` row is "exact graphics/compute/copy class
     * admitted for the outer context", and the outer context is a bridge fact
     * that no other field of this request carries. Without it the ICD could
     * only copy the selected endpoint's own class into the packet, and the
     * KMD's later EngineClassMismatch check would compare that endpoint against
     * itself — a tautology, and HELIOS_TRANSLATOR_STATUS_ENGINE_CLASS would be
     * a declared refusal that nothing can trigger. With it, a D3D12 COPY queue
     * that selects a GRAPHICS endpoint is refused before any packet exists. */
    uint32_t engine_class;
    /* 24: exactly one HQA1 context-kind flag (D3D11 physical / D3D12 virtual). */
    uint32_t context_flags;
    uint32_t reserved;            /* 28 zero */
} HeliosQueueAttachRequestV1;

HELIOS_TRANSLATOR_STATIC_ASSERT(sizeof(HeliosQueueAttachRequestV1) == 32,
                                "HeliosQueueAttachRequestV1 must be 32 bytes");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosQueueAttachRequestV1, context_generation) == 8, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosQueueAttachRequestV1, struct_bytes) == 0, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosQueueAttachRequestV1, abi_version) == 4, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosQueueAttachRequestV1, endpoint_id) == 16, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosQueueAttachRequestV1, engine_class) == 20, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosQueueAttachRequestV1, context_flags) == 24, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosQueueAttachRequestV1, reserved) == 28, "");

/*
 * Confirmation that a runtime create-context succeeded and the KMD accepted the
 * HQA1, plus the per-context cookie the up half will be called with.
 */
typedef struct HeliosOuterContextAttachV1 {
    uint32_t struct_bytes;       /* 0  == sizeof */
    uint32_t abi_version;        /* 4 */
    uint64_t context_generation; /* 8  the generation whose packet was accepted */
    uint32_t endpoint_id;        /* 16 anti-stale cross-check */
    uint32_t context_flags;      /* 20 anti-stale cross-check */
    /* 24: the bridge's opaque per-outer-context cookie. The ICD stores it and
     * passes it, UNREAD, as the first argument of every up-call for this
     * context. It never dereferences it, never compares it to anything but
     * itself, and never copies it into a record. That it is a pointer is an
     * in-process detail, and it is why the up-calls need no handle namespace and
     * no lookup. */
    void *host_context_cookie;
} HeliosOuterContextAttachV1;

HELIOS_TRANSLATOR_STATIC_ASSERT(sizeof(HeliosOuterContextAttachV1) == 32,
                                "HeliosOuterContextAttachV1 must be 32 bytes");
HELIOS_TRANSLATOR_STATIC_ASSERT(HELIOS_TRANSLATOR_ALIGNOF(HeliosOuterContextAttachV1) == 8, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosOuterContextAttachV1, struct_bytes) == 0, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosOuterContextAttachV1, abi_version) == 4, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosOuterContextAttachV1, context_generation) == 8, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosOuterContextAttachV1, endpoint_id) == 16, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosOuterContextAttachV1, context_flags) == 20, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosOuterContextAttachV1, host_context_cookie) == 24, "");

/* Opening an outer-operation scope. */
typedef struct HeliosOuterScopeBeginV1 {
    uint32_t struct_bytes;       /* 0  == sizeof */
    uint32_t abi_version;        /* 4 */
    uint64_t context_generation; /* 8  the attached outer context */
    /* 16: anti-stale cross-check against the endpoint recorded at attach; a
     * mismatch is UNKNOWN_ENDPOINT, never a re-selection. The endpoint is fixed
     * at HQA1 time (invariant 13: "no attach, capability lookup, or endpoint
     * discovery is permitted after context creation"). */
    uint32_t endpoint_id;
    uint32_t reserved;           /* 20 zero */
} HeliosOuterScopeBeginV1;

HELIOS_TRANSLATOR_STATIC_ASSERT(sizeof(HeliosOuterScopeBeginV1) == 24,
                                "HeliosOuterScopeBeginV1 must be 24 bytes");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosOuterScopeBeginV1, context_generation) == 8, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosOuterScopeBeginV1, struct_bytes) == 0, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosOuterScopeBeginV1, abi_version) == 4, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosOuterScopeBeginV1, endpoint_id) == 16, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosOuterScopeBeginV1, reserved) == 20, "");

/* Closing an outer-operation scope, and reporting what the outer submission did. */
typedef struct HeliosOuterScopeCloseV1 {
    uint32_t struct_bytes;  /* 0  == sizeof */
    uint32_t abi_version;   /* 4 */
    uint32_t disposition;   /* 8  HELIOS_TRANSLATOR_SCOPE_DISPOSITION_*; zero refused */
    uint32_t reserved;      /* 12 zero */
    /* 16: on COMMITTED, the HQC1 value the bridge signalled from this exact
     * outer context AFTER the submission that carried this batch — the value
     * that completes when this batch's GPU work completes (§12, "Outer
     * translated progress HQC1"). Nonzero and strictly increasing on that
     * context. It is the correlation the ICD later passes back as
     * required_progress_value, which is how a fence status or a query WAIT names
     * THIS batch's completion rather than the whole context's backlog. On
     * ABANDONED: zero — there is no completion to name. */
    uint64_t progress_value;
} HeliosOuterScopeCloseV1;

HELIOS_TRANSLATOR_STATIC_ASSERT(sizeof(HeliosOuterScopeCloseV1) == 24,
                                "HeliosOuterScopeCloseV1 must be 24 bytes");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosOuterScopeCloseV1, disposition) == 8, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosOuterScopeCloseV1, struct_bytes) == 0, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosOuterScopeCloseV1, abi_version) == 4, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosOuterScopeCloseV1, reserved) == 12, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosOuterScopeCloseV1, progress_value) == 16, "");

/* ------------------------------------------------------------------------ */
/* The sealed batch                                                          */
/* ------------------------------------------------------------------------ */

/*
 * One entry of the sealed batch's complete resource-use table, in the TOKEN form
 * the ICD can produce.
 *
 * Same 40 bytes as HOB1's HeliosOuterBatchUseV1 (helios_wddm.h), with EVERY
 * FIELD AT THE OFFSET OF THE HOB1 FIELD IT STANDS IN FOR.
 *
 * ⚠ Same offsets, NOT the same fields. Three of the eight carry a different
 * meaning, and they are exactly the WDDM identity the ICD may not supply:
 *   0:  outer_allocation_token  stands where address_or_index does
 *   16: byte_offset             stands where expected_allocation_generation does
 *   28: reserved0               stands where identity_kind does
 * The other five — byte_length, access_flags, operand_count, first_operand,
 * reserved1 — are the same field with the same meaning.
 *
 * ⛔ SO THIS IS A FIELD-BY-FIELD CONVERSION, NEVER A memcpy PLUS A PATCH. Two
 * POD structs of the same size whose identity fields have different meanings
 * are exactly the shape that invites a block copy plus a patch, and a block copy
 * would leave identity_kind holding a reserved zero — an invalid arm — while the
 * encoder that "only patched the identity pair" would never notice. The encoder
 * WRITES identity_kind; it never inherits it.
 *
 * The UMD converts, because §10.4 (1398) makes the UMD the encoder:
 *   D3D11 (identity_kind 1): address_or_index = the allocation-list index the
 *                            UMD assigned to the token, upper 32 bits zero.
 *   D3D12 (identity_kind 2): address_or_index = the allocation's GPUVA base plus
 *                            byte_offset.
 *   both:                    expected_allocation_generation = the HWA2
 *                            generation the UMD observed.
 * The ICD never sees any of those. That is the point.
 */
typedef struct HeliosSealedResourceUseV1 {
    /* 0: UMD-assigned opaque token naming the outer WDDM allocation this use
     * touches. OPAQUE TO THE ICD: echoed, compared for equality when
     * deduplicating, never interpreted, indexed, or stored beyond the batch. Not
     * a handle, not a GPUVA, not a resid. Zero is refused.
     *
     * ⚠ How a token reaches the ICD is NOT part of this table: it arrives on the
     * resource-creation path the DXVK/vkd3d and UMD lanes own, alongside the
     * deferred frontend handles of §10.4 (1346-1352). This ABI defines only that
     * the sealed use table names allocations by token. */
    uint64_t outer_allocation_token;
    uint64_t byte_length;    /* 8  nonzero. HOB1 offset 8, unchanged. */
    /* 16: byte offset of the used range within that allocation. Sits where HOB1
     * keeps expected_allocation_generation, because both are the half of the
     * identity pair the other side does not have.
     *
     * MUST BE ZERO on a D3D11 physical context — HOB1's identity_kind 1 arm is
     * an allocation-list index with no room for a sub-allocation offset, so a
     * nonzero value there is HELIOS_TRANSLATOR_STATUS_D3D11_SUBRANGE_USE at
     * seal time. */
    uint64_t byte_offset;
    /* 24: HELIOS_HOB1_ACCESS_* — nonzero, within HELIOS_HOB1_ACCESS_MASK, never
     * PRIMARY_WRITE without WRITE. Copied into HOB1 unchanged. */
    uint32_t access_flags;
    /* 28: zero. Sits where HOB1 keeps identity_kind, which is WDDM identity the
     * ICD must never supply. */
    uint16_t reserved0;
    uint16_t operand_count;  /* 30 how many operands belong to this use */
    /* 32: {first_operand, operand_count} runs tile the operand table in order,
     * exactly as HOB1 requires. */
    uint32_t first_operand;
    uint32_t reserved1;      /* 36 zero */
} HeliosSealedResourceUseV1;

HELIOS_TRANSLATOR_STATIC_ASSERT(sizeof(HeliosSealedResourceUseV1) == 40,
                                "the sealed use record must be 40 bytes, HOB1's use-record size");
HELIOS_TRANSLATOR_STATIC_ASSERT(sizeof(HeliosSealedResourceUseV1) == HELIOS_HOB1_USE_RECORD_BYTES,
                                "sealed use and HOB1 use must stay the same size");
HELIOS_TRANSLATOR_STATIC_ASSERT(HELIOS_TRANSLATOR_ALIGNOF(HeliosSealedResourceUseV1) == 8, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosSealedResourceUseV1, outer_allocation_token) == 0, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosSealedResourceUseV1, byte_length) == 8, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosSealedResourceUseV1, byte_offset) == 16, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosSealedResourceUseV1, access_flags) == 24, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosSealedResourceUseV1, reserved0) == 28, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosSealedResourceUseV1, operand_count) == 30, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosSealedResourceUseV1, first_operand) == 32, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosSealedResourceUseV1, reserved1) == 36, "");

/*
 * One entry of the sealed batch's typed operand table, in PAYLOAD-RELATIVE form.
 *
 * The 16 bytes and the field roles are HOB1's HeliosOuterBatchOperandV1, with
 * one deliberate difference that earns the separate type: HOB1's payload_offset
 * is measured FROM THE HOB1 START, and the ICD does not know where in the record
 * the UMD will place the payload. So this carries the offset FROM THE PAYLOAD
 * START, and the UMD adds HOB1's payload_offset when it encodes. Reusing HOB1's
 * type with a different meaning for the same field name is exactly the drift
 * this corpus has been bitten by, so the field is renamed as well as re-based.
 *
 * ⛔ Like HOB1's operand, this identifies only a generated resource operand
 * whose payload bytes are ZERO (§10.4 1286). A nonzero placeholder is how a raw
 * host resource id would reach the host at a position the operand table blesses.
 */
typedef struct HeliosSealedOperandV1 {
    uint32_t payload_relative_offset; /* 0  from the PAYLOAD start; HELIOS_HOB1_OPERAND_ALIGN-aligned */
    uint32_t use_index;               /* 4  into the sealed use table */
    uint16_t operand_kind;            /* 8  HELIOS_HOB1_OPERAND_KIND_* */
    uint16_t encoded_width;           /* 10 4 or 8 */
    uint32_t reserved;                /* 12 zero */
} HeliosSealedOperandV1;

HELIOS_TRANSLATOR_STATIC_ASSERT(sizeof(HeliosSealedOperandV1) == 16,
                                "the sealed operand must be 16 bytes, HOB1's operand size");
HELIOS_TRANSLATOR_STATIC_ASSERT(sizeof(HeliosSealedOperandV1) == HELIOS_HOB1_OPERAND_RECORD_BYTES,
                                "sealed operand and HOB1 operand must stay the same size");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosSealedOperandV1, payload_relative_offset) == 0, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosSealedOperandV1, use_index) == 4, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosSealedOperandV1, operand_kind) == 8, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosSealedOperandV1, encoded_width) == 10, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosSealedOperandV1, reserved) == 12, "");

/*
 * The seal descriptor: everything §10.4 point 3 requires a sealed batch to be
 * stamped with, returned synchronously to the outer bridge (point 4).
 *
 * Point 3's list is "a version, HTS1/session generation, endpoint ID, owning
 * outer-context/queue generation, monotonically increasing CONTEXT-LOCAL batch
 * ID, byte length, checksum, and complete resource-use table". Every item is
 * here except one, deliberately:
 *
 * THE CHECKSUM. HOB1's CRC64 covers the whole assembled record with the CRC
 * field zeroed, and §10.4 (1398) makes the UMD the assembler. A checksum
 * computed by the ICD over a different byte image would be meaningless, so the
 * UMD computes the HOB1 CRC as it encodes. What the ICD provides is
 * `payload_crc64` over the payload bytes it produced, which catches a mis-copy
 * between copy_sealed_batch and the encoder — the one window the HOB1 CRC cannot
 * cover, because the HOB1 CRC is computed after it.
 *
 * Point 3 also ends "no field orders work against another WDDM context", and
 * nothing here can: batch_id is context-local, and the only cross-context order
 * that exists is an explicit native-fence dependency on the outer queues.
 */
typedef struct HeliosSealedBatchV1 {
    uint32_t struct_bytes;       /* 0  == sizeof, set by the caller */
    uint32_t abi_version;        /* 4 */
    uint64_t package_generation; /* 8  -> HOB1 offset 8 */
    uint64_t session_generation; /* 16 -> HOB1 offset 16 */
    uint64_t context_generation; /* 24 -> HOB1 offset 24 */
    /* 32: context-local, nonzero, strictly increasing on this context only.
     * -> HOB1 offset 32 and HOS1 offset 40. The ICD owns it because the ICD is
     * the sealer. An abandoned batch's ID is retired, never reissued: gaps are
     * legal, reuse is not. */
    uint64_t batch_id;
    uint64_t payload_bytes;      /* 40 exact finite Venus payload size, nonzero */
    /* 48: CRC64-ECMA-182 over the payload bytes only. A copy integrity
     * cross-check, not identity and not the HOB1 checksum. */
    uint64_t payload_crc64;
    uint32_t endpoint_id;        /* 56 -> HOB1 offset 40, HOS1 offset 32 */
    uint32_t context_flags;      /* 60 -> HOB1 offset 44; exactly one bit */
    uint32_t use_count;          /* 64 <= HELIOS_HOB1_MAX_USE_RECORDS */
    uint32_t operand_count;      /* 68 <= HELIOS_HOB1_MAX_OPERAND_RECORDS */
} HeliosSealedBatchV1;

HELIOS_TRANSLATOR_STATIC_ASSERT(sizeof(HeliosSealedBatchV1) == 72,
                                "HeliosSealedBatchV1 must be 72 bytes");
HELIOS_TRANSLATOR_STATIC_ASSERT(HELIOS_TRANSLATOR_ALIGNOF(HeliosSealedBatchV1) == 8, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosSealedBatchV1, struct_bytes) == 0, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosSealedBatchV1, abi_version) == 4, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosSealedBatchV1, package_generation) == 8, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosSealedBatchV1, session_generation) == 16, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosSealedBatchV1, context_generation) == 24, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosSealedBatchV1, batch_id) == 32, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosSealedBatchV1, payload_bytes) == 40, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosSealedBatchV1, payload_crc64) == 48, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosSealedBatchV1, endpoint_id) == 56, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosSealedBatchV1, context_flags) == 60, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosSealedBatchV1, use_count) == 64, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosSealedBatchV1, operand_count) == 68, "");

/*
 * Where the bridge wants the sealed batch's three arrays written.
 *
 * The bridge sizes its buffers from the HeliosSealedBatchV1 the seal returned,
 * so every capacity is known-exact before the call; a capacity below the sealed
 * size is BUFFER_TOO_SMALL and NOTHING is written. No partial copy, no resize
 * handshake.
 *
 * ⚠ The three destinations are caller-owned in-process memory. For D3D12 they
 * are typically inside the C65 HOB1 GPUVA pool extent the UMD reserved; for
 * D3D11 inside the runtime-approved command buffer. Neither is a fact the ICD
 * may rely on or learn: it writes bytes at pointers, and it is the UMD that
 * knows those bytes are about to become a WDDM command.
 */
typedef struct HeliosSealedBatchCopyV1 {
    uint32_t struct_bytes;                /* 0  == sizeof */
    uint32_t abi_version;                 /* 4 */
    void *payload;                        /* 8  non-NULL */
    uint64_t payload_capacity;            /* 16 >= payload_bytes */
    HeliosSealedResourceUseV1 *uses;      /* 24 non-NULL when use_capacity > 0 */
    uint32_t use_capacity;                /* 32 >= use_count */
    uint32_t reserved0;                   /* 36 zero */
    HeliosSealedOperandV1 *operands;      /* 40 */
    uint32_t operand_capacity;            /* 48 >= operand_count */
    uint32_t reserved1;                   /* 52 zero */
} HeliosSealedBatchCopyV1;

HELIOS_TRANSLATOR_STATIC_ASSERT(sizeof(HeliosSealedBatchCopyV1) == 56,
                                "HeliosSealedBatchCopyV1 must be 56 bytes");
HELIOS_TRANSLATOR_STATIC_ASSERT(HELIOS_TRANSLATOR_ALIGNOF(HeliosSealedBatchCopyV1) == 8, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosSealedBatchCopyV1, struct_bytes) == 0, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosSealedBatchCopyV1, abi_version) == 4, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosSealedBatchCopyV1, payload) == 8, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosSealedBatchCopyV1, payload_capacity) == 16, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosSealedBatchCopyV1, uses) == 24, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosSealedBatchCopyV1, use_capacity) == 32, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosSealedBatchCopyV1, reserved0) == 36, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosSealedBatchCopyV1, operands) == 40, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosSealedBatchCopyV1, operand_capacity) == 48, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosSealedBatchCopyV1, reserved1) == 52, "");

/* ------------------------------------------------------------------------ */
/* Refusal counters                                                          */
/* ------------------------------------------------------------------------ */

/*
 * Monotonic counters for every refusal class this ABI makes impossible.
 *
 * They exist so that "the private dispatcher rejects any vkQueueSubmit … without
 * a live outer-operation scope" (§10.4 1388-1390) is an OBSERVABLE property of a
 * running system rather than an assertion about source code. Every counter is
 * monotonic for the life of the instance and never resets.
 *
 * A nonzero value is a translator or bridge BUG, not a tolerated condition: each
 * increment already refused the operation and the corresponding Vulkan call
 * already failed. The bridge reads them at teardown and any acceptance gate
 * reads them mid-run; both must fail the run rather than average them away.
 */
typedef struct HeliosTranslatorRefusalCountersV1 {
    uint32_t struct_bytes;                     /* 0  == sizeof, set by the caller */
    uint32_t abi_version;                      /* 4  set by the caller */
    uint64_t queue_submit_without_scope;       /* 8  §10.4 line 1388 */
    uint64_t queue_submit2_without_scope;      /* 16 §10.4 line 1388 */
    uint64_t queue_bind_sparse_without_scope;  /* 24 §10.4 line 1389 */
    /* 32: vkQueuePresentKHR on a record-only instance. UNCONDITIONAL, not
     * scope-gated: §17.3 (3987-3989) makes it unavailable to translator
     * instances, and §2 item 8 gives them no Win32 WSI extension to reach it
     * with in the first place. */
    uint64_t queue_present_refused;
    uint64_t queue_wait_idle_without_scope;    /* 40 §10.4 line 1389 */
    /* 48: the doc's "queue-idle operation" is read to include the device-wide
     * form — it is a queue idle over every queue, and the fail-closed reading
     * refuses it on the same terms rather than leaving the wider operation less
     * gated than the narrower one. */
    uint64_t device_wait_idle_without_scope;
    /* 56: a proc-address request this instance refused because THE ICD ITSELF
     * determined the resolution would not be its own (§13.2 3357-3358).
     *
     * ⚠ The consumer-side half of that rule is not counted here and CANNOT be.
     * The comparison §13.2 describes — take icd_module_base, resolve the owning
     * module of each returned procedure, reject a mismatch — is performed by
     * DXVK/vkd3d/umd bridge, and this record is written only by the ICD: there
     * is no slot through which a consumer could increment a field of it.
     * Reading zero here is therefore NOT evidence that no provenance rejection
     * occurred, and a gate must not treat it as such. What keeps the consumer's
     * half loud is that it is FATAL, not counted: a provenance mismatch fails
     * translator device creation (HELIOS_TRANSLATOR_STATUS_LOADER_PROVENANCE). */
    uint64_t loader_provenance_rejected;
    uint64_t control_opcode_class_violation;   /* 64 §10.9 row 2864 */
    uint64_t deferred_use_without_outer_batch; /* 72 §10.9 row 2864 */
    uint64_t batch_bound_exceeded;             /* 80 §10.9 row 2866 */
    /* 88: get_instance_proc_addr was called with a vk_instance that is not this
     * instance's HeliosTranslatorInstanceV1::vk_instance (FOREIGN_VULKAN_HANDLE).
     * That slot returns a function pointer, not a status, so NULL is its only
     * refusal signal — and a NULL with no counter is exactly the silent layering
     * bypass §13.2 exists to make impossible. */
    uint64_t foreign_vulkan_handle_rejected;
    /* 96: a name a record-only instance may never vend was requested through
     * get_instance_proc_addr: vkCreateInstance (§10.4: "No second VkInstance may
     * be created in that host context"), or any Win32 surface/swapchain entry
     * point (§13.2: a translator instance "advertise[s] no Win32
     * surface/swapchain extension"). Also returned as NULL, counted for the same
     * reason as its neighbour above. */
    uint64_t withheld_proc_addr_refused;
    /* 104: a sync_progress_join was requested while one is already in progress
     * on this thread (REENTRANT_JOIN). The ICD is the detector — it is the only
     * party that initiates joins — so this has no consumer half. The join is the
     * ABI's one reentrant edge and this counter is the only bound on it, which
     * is why it must be observable rather than assumed. */
    uint64_t reentrant_join_refused;
} HeliosTranslatorRefusalCountersV1;

HELIOS_TRANSLATOR_STATIC_ASSERT(sizeof(HeliosTranslatorRefusalCountersV1) == 112,
                                "HeliosTranslatorRefusalCountersV1 must be 112 bytes");
HELIOS_TRANSLATOR_STATIC_ASSERT(HELIOS_TRANSLATOR_ALIGNOF(HeliosTranslatorRefusalCountersV1) == 8, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosTranslatorRefusalCountersV1, struct_bytes) == 0, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosTranslatorRefusalCountersV1, abi_version) == 4, "");
/* Every counter's offset, not a first-and-last pair: a consumer reading these
 * through this mirror indexes by offset, and an inserted field would otherwise
 * renumber every counter after it without breaking anything. */
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosTranslatorRefusalCountersV1, queue_submit_without_scope) == 8, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosTranslatorRefusalCountersV1, queue_submit2_without_scope) == 16, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosTranslatorRefusalCountersV1, queue_bind_sparse_without_scope) == 24, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosTranslatorRefusalCountersV1, queue_present_refused) == 32, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosTranslatorRefusalCountersV1, queue_wait_idle_without_scope) == 40, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosTranslatorRefusalCountersV1, device_wait_idle_without_scope) == 48, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosTranslatorRefusalCountersV1, loader_provenance_rejected) == 56, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosTranslatorRefusalCountersV1, control_opcode_class_violation) == 64, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosTranslatorRefusalCountersV1, deferred_use_without_outer_batch) == 72, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosTranslatorRefusalCountersV1, batch_bound_exceeded) == 80, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosTranslatorRefusalCountersV1, foreign_vulkan_handle_rejected) == 88, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosTranslatorRefusalCountersV1, withheld_proc_addr_refused) == 96, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosTranslatorRefusalCountersV1, reentrant_join_refused) == 104, "");

/* ------------------------------------------------------------------------ */
/* The down half: the ICD's dispatch table                                   */
/* ------------------------------------------------------------------------ */

/* A Vulkan PFN_vkVoidFunction, declared without depending on vulkan.h. */
typedef void(HELIOS_TRANSLATOR_CALL *PFN_helios_translator_void_function)(void);

/* `vk_instance` is typed void* because VkInstance is a dispatchable (pointer)
 * handle on x64 and this header must not include vulkan.h. */
typedef PFN_helios_translator_void_function(
    HELIOS_TRANSLATOR_CALL *PFN_helios_translator_get_instance_proc_addr)(void *vk_instance,
                                                                         const char *name);

/* `endpoint_bytes` must be exactly HELIOS_TRANSLATOR_ENDPOINT_BYTES: the array
 * is written with the ICD's stride, so a consumer built against a different
 * endpoint descriptor must be refused (STRUCT_BYTES) rather than handed a
 * mis-strided array. */
typedef HeliosTranslatorStatusCode(HELIOS_TRANSLATOR_CALL *PFN_helios_translator_enumerate_endpoints)(
    HeliosTranslatorHandle instance,
    uint32_t *endpoint_count,
    struct HeliosTranslationEndpointV1 *endpoints,
    uint32_t endpoint_bytes);

typedef HeliosTranslatorStatusCode(HELIOS_TRANSLATOR_CALL *PFN_helios_translator_build_queue_attach)(
    HeliosTranslatorHandle instance,
    const HeliosQueueAttachRequestV1 *request,
    struct HeliosQueueAttachV1 *out_hqa1,
    uint32_t out_hqa1_bytes);

typedef HeliosTranslatorStatusCode(HELIOS_TRANSLATOR_CALL *PFN_helios_translator_attach_outer_context)(
    HeliosTranslatorHandle instance,
    const HeliosOuterContextAttachV1 *attach);

typedef HeliosTranslatorStatusCode(HELIOS_TRANSLATOR_CALL *PFN_helios_translator_detach_outer_context)(
    HeliosTranslatorHandle instance,
    uint64_t context_generation);

typedef HeliosTranslatorStatusCode(HELIOS_TRANSLATOR_CALL *PFN_helios_translator_open_outer_scope)(
    HeliosTranslatorHandle instance,
    const HeliosOuterScopeBeginV1 *begin,
    HeliosTranslatorScope *out_scope);

typedef HeliosTranslatorStatusCode(HELIOS_TRANSLATOR_CALL *PFN_helios_translator_seal_outer_scope)(
    HeliosTranslatorScope scope,
    HeliosSealedBatchV1 *out_sealed);

typedef HeliosTranslatorStatusCode(HELIOS_TRANSLATOR_CALL *PFN_helios_translator_copy_sealed_batch)(
    HeliosTranslatorScope scope,
    const HeliosSealedBatchCopyV1 *destination);

typedef HeliosTranslatorStatusCode(HELIOS_TRANSLATOR_CALL *PFN_helios_translator_close_outer_scope)(
    HeliosTranslatorScope scope,
    const HeliosOuterScopeCloseV1 *close);

typedef HeliosTranslatorStatusCode(HELIOS_TRANSLATOR_CALL *PFN_helios_translator_query_refusal_counters)(
    HeliosTranslatorHandle instance,
    HeliosTranslatorRefusalCountersV1 *out_counters);

typedef HeliosTranslatorStatusCode(HELIOS_TRANSLATOR_CALL *PFN_helios_translator_destroy_instance)(
    HeliosTranslatorHandle instance);

/*
 * The private direct-dispatch table: everything a translator and its D3D UMD
 * bridge may ask the Helios ICD to do. ELEVEN slots, and no twelfth by accident:
 * `struct_bytes` is asserted equal to 112 on both sides, so adding a slot
 * without bumping the ABI version and both size constants breaks the build.
 *
 * SLOT CONTRACT SUMMARY (§13.3 governs the last two columns):
 *
 *   slot                     impl  called by            may block  lock rule
 *   get_instance_proc_addr   ICD   translator + bridge  no         none; concurrent
 *   enumerate_endpoints      ICD   bridge, pre-create   no         brief session lock
 *   build_queue_attach       ICD   bridge, pre-create   no         brief session lock
 *   attach_outer_context     ICD   bridge, post-create  no         brief session lock
 *   detach_outer_context     ICD   bridge, teardown     YES        none held; may up-call
 *   open_outer_scope         ICD   bridge, in DDI       no         none; refuses, never waits
 *   seal_outer_scope         ICD   bridge               no         queue-local mutex, inside
 *   copy_sealed_batch        ICD   bridge               no         none; sealed = immutable
 *   close_outer_scope        ICD   bridge, post-submit  no         brief context lock
 *   query_refusal_counters   ICD   bridge, gates        no         none; relaxed atomics
 *   destroy_instance         ICD   bridge, teardown     YES        none held; may up-call
 *
 * The two blocking slots block for the same reason and under the same rule:
 * §10.4 (1394-1396) allows destruction to CPU-wait for already submitted WDDM
 * work but forbids it from creating a cleanup submission on a lower queue, and
 * §13.3 requires that wait to hold no translator, endpoint, session-list, or UMD
 * runtime lock. Everything else is non-blocking by contract, which is what keeps
 * a D3D DDI entry point free of a GPU-completion wait.
 *
 * ORDERING:
 *
 *   helios_icd_create_translator_v1                    (once per vn_instance)
 *     [ ICD entry: helios_translator_check_create_info()
 *                  + helios_translator_check_host_callbacks() ]
 *     -> helios_translator_check_instance()            (mandatory, total)
 *     -> helios_translator_check_dispatch()            (mandatory, total)
 *     -> enumerate_endpoints                           (bridge selects one)
 *     -> build_queue_attach                            (per outer context)
 *        [ ICD: helios_translator_check_queue_attach_request() ]
 *        [ runtime pfnCreateContextCb / pfnCreateContextVirtualCb ]
 *     -> attach_outer_context                          (on success only)
 *        ... per outer operation:
 *          -> open_outer_scope
 *             [ translator records; queue entry points legal ONLY here ]
 *          -> seal_outer_scope     -> HeliosSealedBatchV1
 *             [ bridge: helios_translator_check_sealed_batch() ]
 *          -> copy_sealed_batch    -> payload + uses + operands
 *             [ bridge: helios_translator_check_sealed_use() per use,
 *                       helios_translator_check_sealed_operand() per operand,
 *                       helios_translator_check_operand_payload_zero() per
 *                       operand — the leakage check, on the copied bytes ]
 *             [ UMD encodes HOB1, submits via pfnRenderCb/pfnSubmitCommandCb,
 *               signals HQC1 ]
 *          -> close_outer_scope    (COMMITTED + HQC1 value, or ABANDONED)
 *             [ ICD: helios_translator_check_scope_close() ]
 *        ... and, from inside a GPU-dependent synchronous Vulkan call:
 *          -> sync_progress_join   (up)   [ ICD: helios_translator_check_join_result() ]
 *             [ the CUT: the bridge seals, submits, signals, CLOSES, and
 *               REOPENS the scope. The ICD's scope handle and everything
 *               derived from the sealed batch are dead across this call. ]
 *          -> sync_progress_query  (up)   [ ICD: helios_translator_check_query_result() ]
 *     -> detach_outer_context                          (per outer context)
 *     -> query_refusal_counters                        (must be all zero)
 *     -> destroy_instance
 *
 * WHAT IS NOT HERE, AND WHY: there is no submit, present, signal, or idle slot.
 * §10.4 point 5 ("perform no queue-work KMT Render/SubmitCommand/HWQueue call
 * and signal no independent GPU timeline") is therefore a property of the table
 * rather than a promise about its implementation. The queue entry points §10.4
 * (1388-1390) enumerates are Vulkan commands reached through
 * get_instance_proc_addr, and the live scope is what makes them legal.
 */
typedef struct HeliosTranslatorDispatchV1 {
    uint32_t struct_bytes;       /* 0  == sizeof(HeliosTranslatorDispatchV1) */
    uint32_t abi_version;        /* 4  == HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION */
    uint64_t package_generation; /* 8  == HELIOS_PACKAGE_GENERATION */

    /*
     * 16: the ICD module's base address, for the §13.2 provenance check.
     *
     * §13.2 (3357-3358): "every instance/device/queue proc-address request is
     * resolved from that table; a pointer whose owning module is the Vulkan
     * loader or WSI layer is rejected." The consumer performs that rejection, and
     * this is the datum that lets it: resolve the owning module of each returned
     * procedure (GetModuleHandleExW with
     * GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | ..._UNCHANGED_REFCOUNT) and
     * compare it with this value. A mismatch increments
     * loader_provenance_rejected and fails translator device creation.
     *
     * ⛔ COMPARE-ONLY. Never pass it to GetProcAddress, LoadLibrary, FreeLibrary,
     * or any other module operation. It discloses nothing: the consumer already
     * holds this module, because that is where it resolved the entry point.
     */
    const void *icd_module_base;

    /*
     * 24: the private replacement for the loader's vkGetInstanceProcAddr. Every
     * instance, device, and queue procedure the translator uses is resolved
     * through this slot and no other (§13.2 3357); vkGetDeviceProcAddr itself is
     * obtained through it. `vk_instance` must be the VkInstance this same
     * translator instance created — a handle from another instance, from a
     * loader-created device, or from the WSI layer is FOREIGN_VULKAN_HANDLE;
     * NULL resolves only the global commands the ICD chooses to expose.
     *
     * ⛔ The returned procedures are the ICD's own: never a loader trampoline,
     * never a layer's next-chain pointer, and never a Win32 WSI or swapchain
     * entry point — a record-only instance advertises no Win32
     * surface/swapchain extension at all (§13.2 3360), so vkQueuePresentKHR is
     * refused rather than dispatched.
     */
    PFN_helios_translator_get_instance_proc_addr get_instance_proc_addr;

    /*
     * 32: session/endpoint discovery, and the ONLY discovery this ABI has. The
     * bridge calls it BEFORE the runtime's create-context callback, because
     * §10.4 (1215-1217) requires the endpoint to be selected before HQA1 exists
     * and §10.6 steps 1-2 order it the same way for D3D12.
     *
     * Two-call form: with `endpoints == NULL`, *endpoint_count is set to the
     * session's endpoint count; otherwise *endpoint_count is the caller's element
     * capacity on entry and the number written on exit, and a capacity below the
     * count is ENDPOINT_CAPACITY with nothing written. `endpoints` is an array of
     * HeliosTranslationEndpointV1 (the record protocol/src/translation_session.rs
     * already owns): session-local ordinals, engine class, and two
     * diagnostic-only queue ordinals — explicitly NOT a host INFO_RING_IDX.
     *
     * ⛔ Returns descriptors. Never a capability, never a session generation to
     * be presented later, never a host object id.
     */
    PFN_helios_translator_enumerate_endpoints enumerate_endpoints;

    /*
     * 40: seals the complete 72-byte HQA1 create-context private data for one
     * outer context, from the endpoint and context generation the bridge chose.
     * The bridge passes those exact bytes to pfnCreateContextCb (D3D11) or
     * pfnCreateContextVirtualCb (D3D12) as the whole of pPrivateDriverData.
     * `out_hqa1_bytes` must be exactly HELIOS_TRANSLATOR_HQA1_BYTES.
     *
     * ⭐ THIS IS THE ONE PLACE A SESSION CAPABILITY CROSSES THE INTERFACE, AND IT
     * CROSSES SEALED INSIDE HQA1. §17.3 (3993-3995) requires the capability to
     * reach the UMD "only through the private direct table", and §10.4 requires
     * the UMD to deliver it in HQA1; handing the bridge the finished packet
     * satisfies both without ever giving it a bare nonce it could key a table
     * with, log, or copy into a Render/Submit private-data blob (which §17.5 line
     * 4244 forbids outright). Invariant 16 is preserved exactly: the capability
     * is an admission nonce compared once at context creation, and it never looks
     * anything up.
     */
    PFN_helios_translator_build_queue_attach build_queue_attach;

    /*
     * 48: called ONLY AFTER the runtime create-context callback returned success
     * — which per §10.4 (1237-1241) means the KMD validated the HQA1, accepted
     * the context generation, and took a strong direct session/endpoint
     * reference. Until this call the ICD has a sealed packet and no context.
     *
     * A generation the ICD never sealed a packet for is UNKNOWN_CONTEXT; a second
     * attach of the same generation is CONTEXT_ALREADY_ATTACHED, which is
     * invariant 13's "exactly once" made a refusal.
     *
     * If the runtime callback FAILED, the bridge calls detach_outer_context with
     * the same generation to retire the sealed packet. It must not silently drop
     * it: the generation must never be reused, and the ICD is the only party
     * tracking that.
     */
    PFN_helios_translator_attach_outer_context attach_outer_context;

    /*
     * 56: retires one outer context — on ordinary D3D queue/context destruction,
     * and on the failure path where create-context did not succeed (in which case
     * there is nothing to join and it cannot block). §17.5 (4243): "Session,
     * endpoint, HQA1, and HQC1 teardown is added to every queue/device/error
     * path." Refuses with SCOPE_STILL_LIVE if a scope is open on that context.
     * Never creates a cleanup submission on a lower queue (§10.4 1396).
     */
    PFN_helios_translator_detach_outer_context detach_outer_context;

    /*
     * 64: opens the outer-operation scope — the window in which the translator's
     * recording is legal. The bridge calls it inside the D3D DDI entry point that
     * owns the operation (the D3D11 flush, or the D3D12 ECL association) BEFORE
     * it invokes the translator, and closes it after the outer submission.
     *
     * This slot is the mechanism behind §10.4 (1387-1390). While a scope is live
     * on the calling thread, vkQueueSubmit, vkQueueSubmit2, vkQueueBindSparse and
     * queue-idle record into this batch; with no scope they are refused with
     * NO_OUTER_SCOPE and counted. The same rule closes §10.4's list of
     * translator-internal work (upload, clear, initialization, query, breadcrumb,
     * sparse, fence-worker, swapchain, drain, teardown): it is either inside
     * somebody's scope — hence "recorded into the owning D3D11 flush/D3D12 ECL
     * batch" — or it is refused. "No background worker may submit GPU work after
     * the outer DDI has returned" is exactly the statement that no worker holds a
     * scope.
     *
     * THE SCOPE IS THREAD-AFFINE: usable only from the thread that opened it
     * (SCOPE_FOREIGN_THREAD), and a second concurrent scope on one outer context
     * is SCOPE_ALREADY_OPEN rather than a wait. Both are the conservative
     * reading: D3D serialises its own per-context calls, and blocking here would
     * invent a lock §13.3 does not allow and that would be held across translator
     * code.
     */
    PFN_helios_translator_open_outer_scope open_outer_scope;

    /*
     * 72: performs points 1-3 of the §10.4 contract and reports the result —
     * validate every resource against the outer device's wrappers, serialise
     * every translated command, barrier, translator-internal dependency and
     * referenced-resource access for the one logical queue operation, then seal
     * an IMMUTABLE batch. The bounded queue-local mutex of §10.4 (1416-1418) may
     * be taken to serialise the final append/seal and is released before return:
     * "it is released before runtime callbacks and never spans GPU completion."
     *
     * After a successful seal the scope is SEALED: further recording is
     * SCOPE_ALREADY_SEALED, and the only legal continuations are
     * copy_sealed_batch and close_outer_scope.
     *
     * It splits nothing. §10.4 (1297-1304) requires the encoder to split "only at
     * a complete generated command/API operation boundary BEFORE HOB1 sealing",
     * so an over-bound batch is BATCH_BOUND_EXCEEDED here, not a truncation and
     * not a fragment.
     */
    PFN_helios_translator_seal_outer_scope seal_outer_scope;

    /*
     * 80: point 4 of the contract — "return the sealed bytes/resource table
     * synchronously to the outer bridge". Writes the Venus payload, the complete
     * resource-use table, and the typed operand table into the bridge's buffers.
     * Synchronous and total: either everything named by the seal is written, or
     * nothing is and the call refuses. It may be called more than once on a
     * sealed scope with different destinations; the bytes are identical every
     * time, which is what "immutable" means.
     */
    PFN_helios_translator_copy_sealed_batch copy_sealed_batch;

    /*
     * 88: ends the scope and tells the ICD what happened to the batch. On
     * COMMITTED the bridge has encoded HOB1, submitted it through the runtime
     * callback, and signalled the HQC1 value it reports; the ICD may now treat
     * this batch's deferred outer-allocation-backed operations as real (§10.4
     * 1346-1352) and may later name that value in a join. On ABANDONED it may
     * not, and the batch ID is retired. The scope handle is invalid on return in
     * both cases.
     */
    PFN_helios_translator_close_outer_scope close_outer_scope;

    /*
     * 96: reads HeliosTranslatorRefusalCountersV1. Every value must be zero on a
     * correct run. This is the observable form of the rules this table makes
     * impossible — see that type's comment.
     */
    PFN_helios_translator_query_refusal_counters query_refusal_counters;

    /*
     * 104: destroys the instance — the Mesa vn_instance, its HTS1 session, its
     * host Venus context, its HVC1 control context, its role-1 HVM1 reply pool,
     * and its raw KMT device. §12's HTS1 row spells the order out (Active ->
     * Draining -> Dead: invalidate the capability and wake device-lost first,
     * cancel snapshots, drain raw/outer contexts, endpoint jobs, slot owners,
     * C51/HQC1 and host refs, unlock/destroy the pool, then destroy host context
     * and session).
     *
     * Refuses with SCOPE_STILL_LIVE if any scope is open, and the bridge must
     * detach every outer context first. It may CPU-wait for already-submitted
     * WDDM work; it may not create a cleanup submission on a lower queue.
     *
     * After it returns, the handle, the table pointer, and every scope, endpoint
     * and cookie derived from them are dead. The capability is invalidated before
     * any waiter is woken and is never reused within this package generation
     * (invariant 16).
     */
    PFN_helios_translator_destroy_instance destroy_instance;
} HeliosTranslatorDispatchV1;

HELIOS_TRANSLATOR_STATIC_ASSERT(sizeof(HeliosTranslatorDispatchV1) == 112,
                                "the dispatch table must be 112 bytes: 24-byte header + 11 slots");
HELIOS_TRANSLATOR_STATIC_ASSERT(HELIOS_TRANSLATOR_ALIGNOF(HeliosTranslatorDispatchV1) == 8, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosTranslatorDispatchV1, struct_bytes) == 0, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosTranslatorDispatchV1, abi_version) == 4, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosTranslatorDispatchV1, package_generation) == 8, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosTranslatorDispatchV1, icd_module_base) == 16, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosTranslatorDispatchV1, get_instance_proc_addr) == 24, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosTranslatorDispatchV1, enumerate_endpoints) == 32, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosTranslatorDispatchV1, build_queue_attach) == 40, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosTranslatorDispatchV1, attach_outer_context) == 48, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosTranslatorDispatchV1, detach_outer_context) == 56, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosTranslatorDispatchV1, open_outer_scope) == 64, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosTranslatorDispatchV1, seal_outer_scope) == 72, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosTranslatorDispatchV1, copy_sealed_batch) == 80, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosTranslatorDispatchV1, close_outer_scope) == 88, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosTranslatorDispatchV1, query_refusal_counters) == 96, "");
HELIOS_TRANSLATOR_STATIC_ASSERT(offsetof(HeliosTranslatorDispatchV1, destroy_instance) == 104, "");

/* Pointers are 8 bytes and there are exactly eleven down slots and two up slots.
 * A twelfth or a third is an ABI-version bump, not an addition. */
HELIOS_TRANSLATOR_STATIC_ASSERT(sizeof(void *) == 8,
                                "the Helios package is x86-64 only");
HELIOS_TRANSLATOR_STATIC_ASSERT((sizeof(HeliosTranslatorDispatchV1) - 24) / 8 == 11, "");
HELIOS_TRANSLATOR_STATIC_ASSERT((sizeof(HeliosTranslatorHostCallbacksV1) - 16) / 8 == 2, "");

/* ------------------------------------------------------------------------ */
/* The exported entry point                                                  */
/* ------------------------------------------------------------------------ */

typedef HeliosTranslatorStatusCode(HELIOS_TRANSLATOR_CALL *PFN_helios_icd_create_translator_v1)(
    const HeliosTranslatorCreateInfoV1 *create_info,
    HeliosTranslatorInstanceV1 *out_instance);

/*
 * The one export. It both creates the record-only translator instance and
 * returns that instance's const dispatch table, which is what §10.4 means by
 * "the UMD receives the function table directly while creating its translator
 * instance".
 *
 * Mesa exports it from vn_helios_direct_dispatch.c (§17.3) and lists it in the
 * ICD's .def; every other private export named in vn_renderer_helios.c is
 * deleted by the same section. Consumers resolve it by
 * HELIOS_ICD_CREATE_TRANSLATOR_V1_NAME and never link against it.
 *
 * On any refusal no instance exists, no session was created, and *out_instance
 * keeps only the struct_bytes/abi_version the caller set. The refusal fails
 * translator DEVICE CREATION — there is no degraded mode (§10.9 row 2860).
 */
HELIOS_TRANSLATOR_DISPATCH_EXPORT HeliosTranslatorStatusCode HELIOS_TRANSLATOR_CALL
helios_icd_create_translator_v1(const HeliosTranslatorCreateInfoV1 *create_info,
                                HeliosTranslatorInstanceV1 *out_instance);

/* ------------------------------------------------------------------------ */
/* The total version/size checks, expressible from C                         */
/* ------------------------------------------------------------------------ */

/*
 * These are the C twins of HeliosTranslatorDispatchV1::validate and
 * HeliosTranslatorHostCallbacksV1::validate in
 * protocol/src/translator_dispatch.rs. Same checks, same order, same refusals.
 *
 * ⛔ TOTAL, NEVER PARTIAL. Exact size, exact ABI version, exact package
 * generation, every slot non-NULL. There is no "at least this size", no "use the
 * slots I recognise", no smaller-is-older tolerance, and no zero wildcard. A
 * mismatch fails translator device creation before any context, allocation,
 * endpoint, or resource is exposed.
 */
static inline HeliosTranslatorStatusCode
helios_translator_check_host_callbacks(const HeliosTranslatorHostCallbacksV1 *callbacks,
                                       uint64_t expected_package_generation)
{
    if (callbacks == NULL) {
        return HELIOS_TRANSLATOR_STATUS_HOST_CALLBACKS;
    }
    if (callbacks->struct_bytes != (uint32_t)sizeof(HeliosTranslatorHostCallbacksV1)) {
        return HELIOS_TRANSLATOR_STATUS_STRUCT_BYTES;
    }
    if (callbacks->abi_version != HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION) {
        return HELIOS_TRANSLATOR_STATUS_ABI_VERSION;
    }
    if (expected_package_generation == 0u ||
        callbacks->package_generation != expected_package_generation) {
        return HELIOS_TRANSLATOR_STATUS_PACKAGE_GENERATION;
    }
    if (callbacks->sync_progress_join == NULL || callbacks->sync_progress_query == NULL) {
        return HELIOS_TRANSLATOR_STATUS_HOST_CALLBACKS;
    }
    return HELIOS_TRANSLATOR_STATUS_OK;
}

static inline HeliosTranslatorStatusCode
helios_translator_check_dispatch(const HeliosTranslatorDispatchV1 *table,
                                 uint64_t expected_package_generation)
{
    if (table == NULL) {
        return HELIOS_TRANSLATOR_STATUS_NULL_ARGUMENT;
    }
    if (table->struct_bytes != (uint32_t)sizeof(HeliosTranslatorDispatchV1)) {
        return HELIOS_TRANSLATOR_STATUS_STRUCT_BYTES;
    }
    if (table->abi_version != HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION) {
        return HELIOS_TRANSLATOR_STATUS_ABI_VERSION;
    }
    if (expected_package_generation == 0u ||
        table->package_generation != expected_package_generation) {
        return HELIOS_TRANSLATOR_STATUS_PACKAGE_GENERATION;
    }
    /* Without the provenance anchor the §13.2 check is unperformable, and an
     * unperformable proof is not a proof. */
    if (table->icd_module_base == NULL) {
        return HELIOS_TRANSLATOR_STATUS_NULL_ARGUMENT;
    }
    /* Every slot, named individually: a loop over a cast pointer array would be
     * silently tolerant of a reordering. */
    if (table->get_instance_proc_addr == NULL ||
        table->enumerate_endpoints == NULL ||
        table->build_queue_attach == NULL ||
        table->attach_outer_context == NULL ||
        table->detach_outer_context == NULL ||
        table->open_outer_scope == NULL ||
        table->seal_outer_scope == NULL ||
        table->copy_sealed_batch == NULL ||
        table->close_outer_scope == NULL ||
        table->query_refusal_counters == NULL ||
        table->destroy_instance == NULL) {
        return HELIOS_TRANSLATOR_STATUS_NULL_ARGUMENT;
    }
    return HELIOS_TRANSLATOR_STATUS_OK;
}

/*
 * What the bridge checks on the instance the entry point filled in, before it
 * trusts a single field. The table it names must additionally pass
 * helios_translator_check_dispatch().
 */
static inline HeliosTranslatorStatusCode
helios_translator_check_instance(const HeliosTranslatorInstanceV1 *instance)
{
    if (instance == NULL) {
        return HELIOS_TRANSLATOR_STATUS_NULL_ARGUMENT;
    }
    if (instance->struct_bytes != (uint32_t)sizeof(HeliosTranslatorInstanceV1)) {
        return HELIOS_TRANSLATOR_STATUS_STRUCT_BYTES;
    }
    if (instance->abi_version != HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION) {
        return HELIOS_TRANSLATOR_STATUS_ABI_VERSION;
    }
    /* vk_instance is as required as the handle: §10.4's INIT creates the one
     * VkInstance and this field is the only legal route to it, so an instance
     * reported without one cannot be used for anything. */
    if (instance->handle == NULL || instance->dispatch == NULL ||
        instance->vk_instance == NULL) {
        return HELIOS_TRANSLATOR_STATUS_NULL_ARGUMENT;
    }
    /* §10.4 line 1199: INIT returns a NONZERO session generation. */
    if (instance->session_generation == 0u) {
        return HELIOS_TRANSLATOR_STATUS_SESSION_INIT;
    }
    if (instance->endpoint_capacity == 0u ||
        instance->endpoint_capacity > HELIOS_HTS1_MAX_ENDPOINTS_PER_SESSION) {
        return HELIOS_TRANSLATOR_STATUS_ENDPOINT_CAPACITY;
    }
    if (instance->submission_mode != HELIOS_TRANSLATOR_SUBMISSION_MODE_RECORD_ONLY) {
        return HELIOS_TRANSLATOR_STATUS_SUBMISSION_MODE;
    }
    return HELIOS_TRANSLATOR_STATUS_OK;
}

/*
 * The C twin of HeliosTranslatorCreateInfoV1::validate, run by the ICD on entry.
 *
 * It deliberately does NOT dereference host_callbacks: a pointer check and a
 * table check are separate refusals, and the ICD validates the table itself with
 * helios_translator_check_host_callbacks().
 */
static inline HeliosTranslatorStatusCode
helios_translator_check_create_info(const HeliosTranslatorCreateInfoV1 *create_info,
                                    uint64_t expected_package_generation)
{
    if (create_info == NULL) {
        return HELIOS_TRANSLATOR_STATUS_NULL_ARGUMENT;
    }
    if (create_info->struct_bytes != (uint32_t)sizeof(HeliosTranslatorCreateInfoV1)) {
        return HELIOS_TRANSLATOR_STATUS_STRUCT_BYTES;
    }
    if (create_info->abi_version != HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION) {
        return HELIOS_TRANSLATOR_STATUS_ABI_VERSION;
    }
    if (expected_package_generation == 0u ||
        create_info->package_generation != expected_package_generation) {
        return HELIOS_TRANSLATOR_STATUS_PACKAGE_GENERATION;
    }
    if (create_info->submission_mode != HELIOS_TRANSLATOR_SUBMISSION_MODE_RECORD_ONLY) {
        return HELIOS_TRANSLATOR_STATUS_SUBMISSION_MODE;
    }
    if (create_info->requested_endpoint_capacity == 0u ||
        create_info->requested_endpoint_capacity > HELIOS_HTS1_MAX_ENDPOINTS_PER_SESSION) {
        return HELIOS_TRANSLATOR_STATUS_ENDPOINT_CAPACITY;
    }
    /* {0,0} is not an adapter. A LUID names no object and confers no access, so
     * this is a shape check, not an authorisation one. */
    if (create_info->adapter_luid_low == 0u && create_info->adapter_luid_high == 0) {
        return HELIOS_TRANSLATOR_STATUS_ADAPTER_LUID;
    }
    if (create_info->host_callbacks == NULL) {
        return HELIOS_TRANSLATOR_STATUS_HOST_CALLBACKS;
    }
    return HELIOS_TRANSLATOR_STATUS_OK;
}

/*
 * The C twin of HeliosQueueAttachRequestV1::validate, run by the ICD before it
 * seals a single byte of HQA1.
 *
 * ⚠ EVERY CHECK HERE IS FIELD-LOCAL, and that is the point of the division.
 * Three fields are only fully checkable against session state the ICD holds:
 * context_generation must also be monotonic and unused within the session
 * (§10.4 1233), endpoint_id must name an endpoint OF THIS SESSION, and
 * engine_class must equal the resolved endpoint's class (§10.4 HQA1 row
 * `offset 44`). Each of those re-checks returns the SAME code as its field
 * check here, so a consumer never has to learn which layer refused it. Passing
 * this is necessary and not sufficient, by construction.
 */
static inline HeliosTranslatorStatusCode
helios_translator_check_queue_attach_request(const HeliosQueueAttachRequestV1 *request)
{
    if (request == NULL) {
        return HELIOS_TRANSLATOR_STATUS_NULL_ARGUMENT;
    }
    if (request->struct_bytes != (uint32_t)sizeof(HeliosQueueAttachRequestV1)) {
        return HELIOS_TRANSLATOR_STATUS_STRUCT_BYTES;
    }
    if (request->abi_version != HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION) {
        return HELIOS_TRANSLATOR_STATUS_ABI_VERSION;
    }
    if (request->reserved != 0u) {
        return HELIOS_TRANSLATOR_STATUS_RESERVED_NON_ZERO;
    }
    if (request->context_generation == 0u) {
        return HELIOS_TRANSLATOR_STATUS_CONTEXT_GENERATION;
    }
    if (request->endpoint_id == 0u) {
        return HELIOS_TRANSLATOR_STATUS_UNKNOWN_ENDPOINT;
    }
    if (request->engine_class != HELIOS_ENGINE_CLASS_GRAPHICS &&
        request->engine_class != HELIOS_ENGINE_CLASS_COMPUTE &&
        request->engine_class != HELIOS_ENGINE_CLASS_COPY) {
        return HELIOS_TRANSLATOR_STATUS_ENGINE_CLASS;
    }
    if (request->context_flags != HELIOS_HOB1_FLAG_D3D11_PHYSICAL &&
        request->context_flags != HELIOS_HOB1_FLAG_D3D12_VIRTUAL) {
        return HELIOS_TRANSLATOR_STATUS_CONTEXT_FLAGS;
    }
    return HELIOS_TRANSLATOR_STATUS_OK;
}

/*
 * The C twin of HeliosSealedBatchV1::validate: what the bridge checks on a seal
 * descriptor before it sizes a buffer or encodes a byte.
 *
 * The expected_* arguments come from the BRIDGE'S OWN STATE — the instance's
 * session generation, the context it opened the scope for, the endpoint and kind
 * it attached with — never from the descriptor. That is what makes this a check
 * rather than a lookup.
 */
static inline HeliosTranslatorStatusCode
helios_translator_check_sealed_batch(const HeliosSealedBatchV1 *batch,
                                     uint64_t expected_package_generation,
                                     uint64_t expected_session_generation,
                                     uint64_t expected_context_generation,
                                     uint32_t expected_endpoint_id,
                                     uint32_t expected_context_flags)
{
    uint64_t assembled;

    if (batch == NULL) {
        return HELIOS_TRANSLATOR_STATUS_NULL_ARGUMENT;
    }
    if (batch->struct_bytes != (uint32_t)sizeof(HeliosSealedBatchV1)) {
        return HELIOS_TRANSLATOR_STATUS_STRUCT_BYTES;
    }
    if (batch->abi_version != HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION) {
        return HELIOS_TRANSLATOR_STATUS_ABI_VERSION;
    }
    if (expected_package_generation == 0u ||
        batch->package_generation != expected_package_generation) {
        return HELIOS_TRANSLATOR_STATUS_PACKAGE_GENERATION;
    }
    if (expected_session_generation == 0u ||
        batch->session_generation != expected_session_generation) {
        return HELIOS_TRANSLATOR_STATUS_SESSION_GENERATION;
    }
    if (expected_context_generation == 0u ||
        batch->context_generation != expected_context_generation) {
        return HELIOS_TRANSLATOR_STATUS_CONTEXT_GENERATION;
    }
    if (batch->batch_id == 0u) {
        return HELIOS_TRANSLATOR_STATUS_BATCH_ID;
    }
    if (expected_endpoint_id == 0u || batch->endpoint_id != expected_endpoint_id) {
        return HELIOS_TRANSLATOR_STATUS_UNKNOWN_ENDPOINT;
    }
    if (batch->context_flags != expected_context_flags ||
        (batch->context_flags != HELIOS_HOB1_FLAG_D3D11_PHYSICAL &&
         batch->context_flags != HELIOS_HOB1_FLAG_D3D12_VIRTUAL)) {
        return HELIOS_TRANSLATOR_STATUS_CONTEXT_FLAGS;
    }
    /* No empty batch is ever sealed. */
    if (batch->payload_bytes == 0u) {
        return HELIOS_TRANSLATOR_STATUS_BATCH_BOUND_EXCEEDED;
    }
    if (batch->use_count > HELIOS_HOB1_MAX_USE_RECORDS ||
        batch->operand_count > HELIOS_HOB1_MAX_OPERAND_RECORDS) {
        return HELIOS_TRANSLATOR_STATUS_BATCH_BOUND_EXCEEDED;
    }
    /* ⛔ The 15 MiB cap is on the WHOLE HOB1 RECORD, not on the payload.
     *
     * HELIOS_HOB1_MAX_BYTES is HOB1's total_bytes limit — "header through
     * payload … at most 15 MiB". Bounding only payload_bytes against it lets a
     * seal pass here and become unencodable later: 15,728,600 payload bytes with
     * 4096 uses and 8192 operands is 16,023,624 assembled bytes, refused AFTER
     * the seal — at which point §10.4 has already made splitting impossible and
     * §10.9 forbids truncating instead.
     *
     * So the check is the minimum assembled size, computed exactly as the
     * encoder will lay it out. It is a necessary condition rather than the final
     * one — the encoder may add alignment padding and must re-check — which is
     * the fail-closed direction. The counts are already bounded above, so the
     * two products cannot overflow; only the payload add is checked. */
    assembled = (uint64_t)HELIOS_HOB1_HEADER_BYTES +
                (uint64_t)batch->use_count * (uint64_t)HELIOS_HOB1_USE_RECORD_BYTES +
                (uint64_t)batch->operand_count * (uint64_t)HELIOS_HOB1_OPERAND_RECORD_BYTES;
    if (batch->payload_bytes > UINT64_MAX - assembled) {
        return HELIOS_TRANSLATOR_STATUS_BATCH_BOUND_EXCEEDED;
    }
    assembled += batch->payload_bytes;
    if (assembled > HELIOS_HOB1_MAX_BYTES) {
        return HELIOS_TRANSLATOR_STATUS_BATCH_BOUND_EXCEEDED;
    }
    return HELIOS_TRANSLATOR_STATUS_OK;
}

/*
 * The C twin of HeliosSealedBatchCopyV1::validate, run by the ICD before
 * copy_sealed_batch writes a single byte.
 *
 * ⛔ THIS IS THE ONE CHECK IN THIS HEADER WHOSE ABSENCE IS A MEMORY ERROR rather
 * than a wrong answer. Every other record here is data the receiver interprets;
 * this one is three caller-owned buffers and their capacities, and the ICD is
 * about to WRITE into them. "Must be >= payload_bytes" as a comment is a rule
 * the ICD has to remember; as a function it is one it cannot forget.
 *
 * `batch` is the HeliosSealedBatchV1 the seal returned, which is where every
 * required size comes from — NEVER from the descriptor. Validate the batch first
 * (helios_translator_check_sealed_batch); this trusts its counts, which are
 * already bounded there.
 *
 * A short capacity is BUFFER_TOO_SMALL and NOTHING is written: §10.4 point 4
 * makes the copy synchronous and total, so there is no partial copy and no
 * resize handshake.
 */
static inline HeliosTranslatorStatusCode
helios_translator_check_sealed_batch_copy(const HeliosSealedBatchCopyV1 *dest,
                                          const HeliosSealedBatchV1 *batch)
{
    if (dest == NULL || batch == NULL) {
        return HELIOS_TRANSLATOR_STATUS_NULL_ARGUMENT;
    }
    if (dest->struct_bytes != (uint32_t)sizeof(HeliosSealedBatchCopyV1)) {
        return HELIOS_TRANSLATOR_STATUS_STRUCT_BYTES;
    }
    if (dest->abi_version != HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION) {
        return HELIOS_TRANSLATOR_STATUS_ABI_VERSION;
    }
    if (dest->reserved0 != 0u || dest->reserved1 != 0u) {
        return HELIOS_TRANSLATOR_STATUS_RESERVED_NON_ZERO;
    }
    /* The payload is never empty — a sealed batch has nonzero payload_bytes —
     * so its destination is unconditionally required. */
    if (dest->payload == NULL) {
        return HELIOS_TRANSLATOR_STATUS_NULL_ARGUMENT;
    }
    if (dest->payload_capacity < batch->payload_bytes) {
        return HELIOS_TRANSLATOR_STATUS_BUFFER_TOO_SMALL;
    }
    /* ⚠ The NULL check is conditioned on the BATCH'S COUNT, not on the caller's
     * capacity. A descriptor with {uses = NULL, use_capacity = 8} would
     * otherwise pass every size test and then be dereferenced; and a batch with
     * use_count == 0 is legal (a pure state-setting operation touches no
     * allocation), so an unconditional NULL check would refuse a correct
     * caller. */
    if (batch->use_count > 0u && dest->uses == NULL) {
        return HELIOS_TRANSLATOR_STATUS_NULL_ARGUMENT;
    }
    if (dest->use_capacity < batch->use_count) {
        return HELIOS_TRANSLATOR_STATUS_BUFFER_TOO_SMALL;
    }
    if (batch->operand_count > 0u && dest->operands == NULL) {
        return HELIOS_TRANSLATOR_STATUS_NULL_ARGUMENT;
    }
    if (dest->operand_capacity < batch->operand_count) {
        return HELIOS_TRANSLATOR_STATUS_BUFFER_TOO_SMALL;
    }
    return HELIOS_TRANSLATOR_STATUS_OK;
}

/*
 * The C twin of HeliosOuterContextAttachV1::validate, run by the ICD when the
 * bridge reports that the runtime accepted an HQA1.
 *
 * Field-local, on the same division as the queue-attach request: whether this
 * generation is one the ICD actually sealed a packet for, and whether it is
 * already attached, are session state and are answered with UNKNOWN_CONTEXT and
 * CONTEXT_ALREADY_ATTACHED.
 */
static inline HeliosTranslatorStatusCode
helios_translator_check_context_attach(const HeliosOuterContextAttachV1 *attach)
{
    if (attach == NULL) {
        return HELIOS_TRANSLATOR_STATUS_NULL_ARGUMENT;
    }
    if (attach->struct_bytes != (uint32_t)sizeof(HeliosOuterContextAttachV1)) {
        return HELIOS_TRANSLATOR_STATUS_STRUCT_BYTES;
    }
    if (attach->abi_version != HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION) {
        return HELIOS_TRANSLATOR_STATUS_ABI_VERSION;
    }
    if (attach->context_generation == 0u) {
        return HELIOS_TRANSLATOR_STATUS_CONTEXT_GENERATION;
    }
    if (attach->endpoint_id == 0u) {
        return HELIOS_TRANSLATOR_STATUS_UNKNOWN_ENDPOINT;
    }
    if (attach->context_flags != HELIOS_HOB1_FLAG_D3D11_PHYSICAL &&
        attach->context_flags != HELIOS_HOB1_FLAG_D3D12_VIRTUAL) {
        return HELIOS_TRANSLATOR_STATUS_CONTEXT_FLAGS;
    }
    /* The cookie is the only argument every later up-call carries. A NULL one
     * produces an up-call the bridge cannot resolve, and there is no later point
     * at which it becomes checkable. */
    if (attach->host_context_cookie == NULL) {
        return HELIOS_TRANSLATOR_STATUS_NULL_ARGUMENT;
    }
    return HELIOS_TRANSLATOR_STATUS_OK;
}

/* The C twin of HeliosOuterScopeBeginV1::validate, run by the ICD in
 * open_outer_scope. Field-local for the same reason as its neighbours: that the
 * named context is attached to THIS instance is session state, answered with
 * UNKNOWN_CONTEXT. */
static inline HeliosTranslatorStatusCode
helios_translator_check_scope_begin(const HeliosOuterScopeBeginV1 *begin)
{
    if (begin == NULL) {
        return HELIOS_TRANSLATOR_STATUS_NULL_ARGUMENT;
    }
    if (begin->struct_bytes != (uint32_t)sizeof(HeliosOuterScopeBeginV1)) {
        return HELIOS_TRANSLATOR_STATUS_STRUCT_BYTES;
    }
    if (begin->abi_version != HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION) {
        return HELIOS_TRANSLATOR_STATUS_ABI_VERSION;
    }
    if (begin->reserved != 0u) {
        return HELIOS_TRANSLATOR_STATUS_RESERVED_NON_ZERO;
    }
    if (begin->context_generation == 0u) {
        return HELIOS_TRANSLATOR_STATUS_CONTEXT_GENERATION;
    }
    if (begin->endpoint_id == 0u) {
        return HELIOS_TRANSLATOR_STATUS_UNKNOWN_ENDPOINT;
    }
    return HELIOS_TRANSLATOR_STATUS_OK;
}

/*
 * The C twin of HeliosSyncProgressJoinV1::validate, run by the UMD BRIDGE the
 * moment the up-call arrives and before it dereferences anything.
 *
 * `live_context_generation` is the generation of the context that
 * host_context_cookie belongs to, read from the bridge's own state. This is the
 * check context_generation exists for, and it is the one place in the ABI where
 * skipping a validator is a USE-AFTER-FREE rather than a wrong answer: without
 * it, a join in flight on thread B against a queue thread A has already
 * destroyed hands the bridge a freed cookie and nothing to compare it against.
 *
 * A mismatch is UNKNOWN_CONTEXT — a refusal, never a lookup that finds the right
 * context. Generations are never reused within a session (§10.4), so a mismatch
 * is always a stale caller and never an ambiguity.
 *
 * required_progress_value is deliberately unconstrained: zero is the legal
 * "everything pending" form, and whether a nonzero value is one this bridge ever
 * issued is bridge state (HOST_CALLBACK_FAILED).
 */
static inline HeliosTranslatorStatusCode
helios_translator_check_join_request(const HeliosSyncProgressJoinV1 *request,
                                     uint64_t live_context_generation)
{
    if (request == NULL) {
        return HELIOS_TRANSLATOR_STATUS_NULL_ARGUMENT;
    }
    if (request->struct_bytes != (uint32_t)sizeof(HeliosSyncProgressJoinV1)) {
        return HELIOS_TRANSLATOR_STATUS_STRUCT_BYTES;
    }
    if (request->abi_version != HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION) {
        return HELIOS_TRANSLATOR_STATUS_ABI_VERSION;
    }
    if (live_context_generation == 0u ||
        request->context_generation != live_context_generation) {
        return HELIOS_TRANSLATOR_STATUS_UNKNOWN_CONTEXT;
    }
    return HELIOS_TRANSLATOR_STATUS_OK;
}

/*
 * The C twin of HeliosSealedResourceUseV1::validate, run by the bridge as it
 * converts the entry into an HOB1 use record. `context_flags` is the owning
 * context's kind, which is what decides whether a nonzero byte_offset is
 * encodable at all.
 */
static inline HeliosTranslatorStatusCode
helios_translator_check_sealed_use(const HeliosSealedResourceUseV1 *use, uint32_t context_flags)
{
    uint32_t access;

    if (use == NULL) {
        return HELIOS_TRANSLATOR_STATUS_NULL_ARGUMENT;
    }
    if (use->reserved0 != 0u || use->reserved1 != 0u) {
        return HELIOS_TRANSLATOR_STATUS_RESERVED_NON_ZERO;
    }
    /* Zero is the "no allocation" reading of an uninitialised buffer, and an
     * allocation-naming record that names none is never legal. */
    if (use->outer_allocation_token == 0u) {
        return HELIOS_TRANSLATOR_STATUS_NULL_ARGUMENT;
    }
    /* A zero-length use and an overflowing range are one cause — an illegal byte
     * range — and neither is a destination buffer that came up short
     * (BUFFER_TOO_SMALL) nor a producer that failed to split. */
    if (use->byte_length == 0u || use->byte_offset > UINT64_MAX - use->byte_length) {
        return HELIOS_TRANSLATOR_STATUS_SEALED_USE_RANGE;
    }
    access = use->access_flags;
    if (access == 0u || (access & ~(uint32_t)HELIOS_HOB1_ACCESS_MASK) != 0u ||
        ((access & HELIOS_HOB1_ACCESS_PRIMARY_WRITE) != 0u &&
         (access & HELIOS_HOB1_ACCESS_WRITE) == 0u)) {
        return HELIOS_TRANSLATOR_STATUS_ACCESS_FLAGS;
    }
    if (context_flags == HELIOS_HOB1_FLAG_D3D11_PHYSICAL && use->byte_offset != 0u) {
        return HELIOS_TRANSLATOR_STATUS_D3D11_SUBRANGE_USE;
    }
    return HELIOS_TRANSLATOR_STATUS_OK;
}

/*
 * The C twin of HeliosSealedOperandV1::validate, against the payload the operand
 * must lie inside.
 *
 * Every refusal names its own cause: a misaligned or out-of-payload offset is
 * OPERAND_ENCODING (the operand's ENCODING is illegal) and an out-of-range
 * use_index is OPERAND_USE_INDEX. Neither is BATCH_BOUND_EXCEEDED, whose
 * documented meaning and matching counter are "4096 uses / 8192 operands /
 * 15 MiB, a producer that failed to split".
 */
static inline HeliosTranslatorStatusCode
helios_translator_check_sealed_operand(const HeliosSealedOperandV1 *operand,
                                       uint64_t payload_bytes,
                                       uint32_t use_count)
{
    uint64_t end;

    if (operand == NULL) {
        return HELIOS_TRANSLATOR_STATUS_NULL_ARGUMENT;
    }
    if (operand->reserved != 0u) {
        return HELIOS_TRANSLATOR_STATUS_RESERVED_NON_ZERO;
    }
    if (operand->operand_kind != HELIOS_HOB1_OPERAND_KIND_GENERATED_RESOURCE) {
        return HELIOS_TRANSLATOR_STATUS_OPERAND_ENCODING;
    }
    if (operand->encoded_width != 4u && operand->encoded_width != 8u) {
        return HELIOS_TRANSLATOR_STATUS_OPERAND_ENCODING;
    }
    if (operand->use_index >= use_count) {
        return HELIOS_TRANSLATOR_STATUS_OPERAND_USE_INDEX;
    }
    if ((operand->payload_relative_offset % HELIOS_HOB1_OPERAND_ALIGN) != 0u) {
        return HELIOS_TRANSLATOR_STATUS_OPERAND_ENCODING;
    }
    end = (uint64_t)operand->payload_relative_offset + (uint64_t)operand->encoded_width;
    if (end > payload_bytes) {
        return HELIOS_TRANSLATOR_STATUS_OPERAND_ENCODING;
    }
    return HELIOS_TRANSLATOR_STATUS_OK;
}

/*
 * The C twin of HeliosSealedOperandV1::validate_payload_zero: the §10.4
 * placeholder rule checked AGAINST THE BYTES rather than asserted.
 *
 * ⛔ This is the leakage check the whole interface exists to keep. A nonzero
 * placeholder at an offset the operand table blesses is precisely how a raw host
 * resource id would reach the host, and §17.3's audit ("no actual virtio
 * resource_id may reach user-mode storage or protocol state") is only
 * satisfiable if somebody looks.
 *
 * `payload` is the Venus payload the seal produced, exactly `payload_bytes`
 * long. Call helios_translator_check_sealed_operand() first; this re-derives the
 * bounds rather than trusting them, so it is total on its own — a check that
 * cannot run is not a check, so an out-of-range operand is OPERAND_ENCODING and
 * never a silent skip.
 */
static inline HeliosTranslatorStatusCode
helios_translator_check_operand_payload_zero(const HeliosSealedOperandV1 *operand,
                                             const uint8_t *payload,
                                             uint64_t payload_bytes)
{
    uint64_t start;
    uint64_t width;
    uint64_t i;

    if (operand == NULL || payload == NULL) {
        return HELIOS_TRANSLATOR_STATUS_NULL_ARGUMENT;
    }
    start = (uint64_t)operand->payload_relative_offset;
    width = (uint64_t)operand->encoded_width;
    if (start > payload_bytes || width > payload_bytes - start) {
        return HELIOS_TRANSLATOR_STATUS_OPERAND_ENCODING;
    }
    for (i = 0u; i < width; ++i) {
        if (payload[start + i] != 0u) {
            return HELIOS_TRANSLATOR_STATUS_PAYLOAD_PLACEHOLDER_NON_ZERO;
        }
    }
    return HELIOS_TRANSLATOR_STATUS_OK;
}

/*
 * The C twin of HeliosOuterScopeCloseV1::validate, run by the ICD.
 *
 * On success `*out_disposition` receives the decoded
 * HELIOS_TRANSLATOR_SCOPE_DISPOSITION_* value; on any refusal it is untouched.
 * Zero and every undefined value are DISPOSITION, never a default: defaulting a
 * zeroed field to COMMITTED would report unrealised deferred work as done, and
 * defaulting it to ABANDONED would silently drop a submitted batch.
 */
static inline HeliosTranslatorStatusCode
helios_translator_check_scope_close(const HeliosOuterScopeCloseV1 *close,
                                    uint32_t *out_disposition)
{
    if (close == NULL || out_disposition == NULL) {
        return HELIOS_TRANSLATOR_STATUS_NULL_ARGUMENT;
    }
    if (close->struct_bytes != (uint32_t)sizeof(HeliosOuterScopeCloseV1)) {
        return HELIOS_TRANSLATOR_STATUS_STRUCT_BYTES;
    }
    if (close->abi_version != HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION) {
        return HELIOS_TRANSLATOR_STATUS_ABI_VERSION;
    }
    if (close->reserved != 0u) {
        return HELIOS_TRANSLATOR_STATUS_RESERVED_NON_ZERO;
    }
    /* A committed batch must name the HQC1 value that completes it; without one
     * no later join can wait for exactly this batch. An abandoned batch has no
     * completion to name, and reporting one would arm a wait for a value that
     * will never be signalled. Both arms are one cause: a progress value that
     * contradicts the disposition beside it. */
    if (close->disposition == HELIOS_TRANSLATOR_SCOPE_DISPOSITION_COMMITTED) {
        if (close->progress_value == 0u) {
            return HELIOS_TRANSLATOR_STATUS_PROGRESS_VALUE;
        }
    } else if (close->disposition == HELIOS_TRANSLATOR_SCOPE_DISPOSITION_ABANDONED) {
        if (close->progress_value != 0u) {
            return HELIOS_TRANSLATOR_STATUS_PROGRESS_VALUE;
        }
    } else {
        return HELIOS_TRANSLATOR_STATUS_DISPOSITION;
    }
    *out_disposition = close->disposition;
    return HELIOS_TRANSLATOR_STATUS_OK;
}

/* The field checks both up-calls share: shape, reserved, flag mask, and the one
 * invariant the record itself states (completed <= last_submitted). Split out
 * because the two up-calls' INTERPRETATIONS are opposites, and merging them is
 * how a nonblocking query gets turned into a failure. */
static inline HeliosTranslatorStatusCode
helios_translator_check_progress_common(const HeliosSyncProgressResultV1 *result)
{
    if (result == NULL) {
        return HELIOS_TRANSLATOR_STATUS_NULL_ARGUMENT;
    }
    if (result->struct_bytes != (uint32_t)sizeof(HeliosSyncProgressResultV1)) {
        return HELIOS_TRANSLATOR_STATUS_STRUCT_BYTES;
    }
    if (result->abi_version != HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION) {
        return HELIOS_TRANSLATOR_STATUS_ABI_VERSION;
    }
    if (result->reserved != 0u) {
        return HELIOS_TRANSLATOR_STATUS_RESERVED_NON_ZERO;
    }
    /* An undefined flag bit is a reserved bit: same cause, same code. */
    if ((result->flags & ~(uint32_t)HELIOS_TRANSLATOR_PROGRESS_FLAGS_MASK) != 0u) {
        return HELIOS_TRANSLATOR_STATUS_RESERVED_NON_ZERO;
    }
    if ((result->flags & HELIOS_TRANSLATOR_PROGRESS_FLAG_DEVICE_LOST) != 0u) {
        return HELIOS_TRANSLATOR_STATUS_DEVICE_LOST;
    }
    /* Not a failed callback — a self-contradictory one. Completion cannot outrun
     * submission on a context that has one writer. */
    if (result->completed_progress_value > result->last_submitted_progress_value) {
        return HELIOS_TRANSLATOR_STATUS_PROGRESS_VALUE;
    }
    return HELIOS_TRANSLATOR_STATUS_OK;
}

/*
 * The C twin of HeliosSyncProgressResultV1::validate_join — the check for a
 * BLOCKING sync_progress_join.
 *
 * `required` is the value the ICD asked for (zero for the "everything pending"
 * form). A join that returns without having reached the value it was asked for
 * is a FAILED join, never a partial success: §10.9's HQC1 row forbids treating a
 * later or lesser value as satisfaction.
 *
 * ⛔ NOT FOR A QUERY RESULT. helios_translator_check_query_result() is that one,
 * and the difference is not stylistic: the tail of this function treats
 * completed < last_submitted as a failure, which is exactly the answer a
 * nonblocking query returns when work is outstanding.
 */
static inline HeliosTranslatorStatusCode
helios_translator_check_join_result(const HeliosSyncProgressResultV1 *result, uint64_t required)
{
    HeliosTranslatorStatusCode status = helios_translator_check_progress_common(result);
    if (status != HELIOS_TRANSLATOR_STATUS_OK) {
        return status;
    }
    if (required != 0u && result->completed_progress_value < required) {
        return HELIOS_TRANSLATOR_STATUS_HOST_CALLBACK_FAILED;
    }
    /* The "everything pending" form must have joined everything. */
    if (required == 0u &&
        result->completed_progress_value < result->last_submitted_progress_value) {
        return HELIOS_TRANSLATOR_STATUS_HOST_CALLBACK_FAILED;
    }
    return HELIOS_TRANSLATOR_STATUS_OK;
}

/*
 * The C twin of HeliosSyncProgressResultV1::validate_query — the check for a
 * NONBLOCKING sync_progress_query.
 *
 * It is the shared field checks and nothing else, because there is no value the
 * query was obliged to reach. §10.4: "A nonblocking status query returns the
 * locally known not-ready state without a control round trip when the milestone
 * has not completed" — so completed < last_submitted is the NORMAL, CORRECT
 * answer here and must never be a refusal. vkGetFenceStatus, vkGetEventStatus
 * and a non-WAIT vkGetQueryPoolResults all live on this path; making them fail
 * whenever work is outstanding would invert the required behaviour, and mapping
 * that failure to a callback failure would remove the device.
 *
 * The caller decides readiness by comparing completed_progress_value against the
 * value it cares about; this function does not, because "not ready" is a result
 * and not an error.
 */
static inline HeliosTranslatorStatusCode
helios_translator_check_query_result(const HeliosSyncProgressResultV1 *result)
{
    return helios_translator_check_progress_common(result);
}

/*
 * The C twin of HeliosTranslatorRefusalCountersV1::validate — the shape check
 * the ICD owes `out_counters` BEFORE it writes a byte of it, and the consumer
 * owes it after query_refusal_counters returns.
 *
 * query_refusal_counters takes a `*mut` into the CONSUMER's storage and the two
 * parties are separately compiled binaries (the ICD is Mesa; the consumer is
 * DXVK/vkd3d or a umd bridge). Every other record here re-checks struct_bytes
 * and abi_version on entry even though helios_translator_check_create_info()
 * already refused a mismatched abi_version at create time — the redundancy is
 * the point, because what it catches is a record whose SIZE changed without the
 * VERSION being bumped, which is exactly what create-time negotiation cannot
 * see.
 *
 * This record was the one exception; its sibling out-parameter
 * HeliosSyncProgressResultV1 has the identical shape and has always been
 * checked. No cross-field invariant exists to test: all thirteen fields are
 * independent monotonic counters, and a counter is never "too large".
 */
static inline HeliosTranslatorStatusCode
helios_translator_check_refusal_counters(const HeliosTranslatorRefusalCountersV1 *counters)
{
    if (counters == NULL) {
        return HELIOS_TRANSLATOR_STATUS_NULL_ARGUMENT;
    }
    if (counters->struct_bytes != (uint32_t)sizeof(HeliosTranslatorRefusalCountersV1)) {
        return HELIOS_TRANSLATOR_STATUS_STRUCT_BYTES;
    }
    if (counters->abi_version != HELIOS_TRANSLATOR_DISPATCH_ABI_VERSION) {
        return HELIOS_TRANSLATOR_STATUS_ABI_VERSION;
    }
    return HELIOS_TRANSLATOR_STATUS_OK;
}

#if defined(__cplusplus)
}
#endif

#endif /* HELIOS_TRANSLATOR_DISPATCH_H */
