/*
 * helios_translation_session.h — C mirror of the Helios HTS1 translation-session
 * and HQA1 outer-context-attach wire records.
 *
 * ⛔ SINGLE SOURCE OF TRUTH: protocol/src/translation_session.rs.
 * This header is the hand-maintained C projection of that file for the Mesa
 * (`icd/mesa`) side. Every constant, struct, and assertion below exists in the
 * Rust file first; if the two ever disagree, the Rust file wins and this header
 * is the bug. Change one, change both in the same commit — the assertions here
 * are what turn a layout drift into a compile error rather than a live VM
 * admitting a context into the wrong host namespace.
 *
 * Normative: docs/HELIOS_PRESENT_SYNC_RETIREMENT.md §10.4 — the HTS1 INIT
 * request/reply (lines 1188-1213) and the 72-byte HQA1 create-context table
 * (lines 1215-1241) — with §10.1 invariants 12/13/16 and §10.2's admission
 * table. §17.1 is the module mandate for the Rust side.
 *
 * WHY C NEEDS THIS AT ALL: the ICD is the PRODUCER of both records. It sends
 * INIT on its raw KMT device's HVC1 control context, validates the reply, and
 * exports the endpoint descriptor the outer UMD copies into HQA1
 * (docs/retirement/lane-mesa.md, units A1 and A5). §17.1 asks for generated C
 * declarations only for `physical_memory` and `diagnostics`, and for "both Rust
 * **and C**" offsets for `wddm`; it says nothing about HQA1/HTS1. The ICD's
 * answer today is `icd/mesa/src/virtio/vulkan/vn_renderer_helios.c:255-352` —
 * hand-declared structs guarded by `_Static_assert(sizeof(...) == N)` only, i.e.
 * SIZE and not offsets, across two repositories, with nothing binding them to
 * the Rust side. lane-mesa.md ambiguity A1 / CROSS-LANE REQUEST 1 makes this
 * header the mandate: the ICD declares no record of its own and asserts every
 * OFFSET, not just each size.
 *
 * NO IDENTITY CROSSES THIS BOUNDARY: HQA1 contains no pointer, no KMT handle, no
 * PID, no allocation/resource identity, no host object ID, and no
 * synchronization object. Every field is a generation counter, an unpredictable
 * admission nonce, a session-local endpoint ordinal, a class/flags scalar, or
 * reserved-zero. In particular `endpoint_id` is ⛔ NOT a host `INFO_RING_IDX`:
 * §10.7 line 1723 states that no host context or ring ID is ever returned to
 * user mode, so the ordinal is a session-local name the KMD maps internally to
 * that endpoint's unique nonzero ring.
 *
 * ⛔ CAPABILITIES ARE NONCES, NOT IDENTITY (§10.1 invariant 16). The 128-bit
 * capability is an unpredictable admission nonce, not a resource ID and not a
 * security boundary against code already executing in the same process. It never
 * names, indexes, or resolves anything: it is compared for equality against the
 * one session the raw device already owns, ONCE, at context creation, and after
 * that the live KMD context object is the identity. Because it is explicitly not
 * an in-process security boundary, plain equality is the correct comparison and
 * no constant-time primitive is implied. It is invalidated before
 * reset/removal wakeups, never persists across a package generation, is never
 * reused within one, and must never be copied into a refusal value, a counter,
 * or a diagnostic record. The all-zero pair is the ABSENT/INVALIDATED sentinel
 * and is never a usable capability.
 *
 * ⛔ HQC1 HAS NO WIRE PAYLOAD AND THEREFORE NO STRUCT HERE. It is an ordinary
 * unshared WDDM monitored fence created through `pfnCreateSynchronizationObject2Cb`
 * (§10.6, C61): an OS synchronization object, never bytes on this or any other
 * wire, and never exposed as an API fence.
 *
 * ⛔ WHAT THIS HEADER IS NOT: a validator. The rules — the "exactly one flag"
 * arm decode, the strictly-increasing context generation, the capability
 * comparison, the endpoint cross-checks that COMPARE AND REJECT but never look
 * up, the bounded-capacity admissions, and the three-way control-opcode
 * classification — live in `protocol/src/translation_session.rs`
 * (`HeliosQueueAttachV1::validate`, `HeliosTranslationSessionReplyV1::validate`,
 * `admit_*`, `admit_control_opcode`). A C implementation must reproduce them;
 * the comments on each declaration below name the rule so it cannot be
 * reconstructed from the field names alone.
 */

#ifndef HELIOS_TRANSLATION_SESSION_H
#define HELIOS_TRANSLATION_SESSION_H

#include <stddef.h>
#include <stdint.h>

/*
 * ⛔ Two constants below are ALIASES, exactly as they are in the Rust module,
 * and these includes are what makes them aliases rather than a second
 * hand-kept-equal copy:
 *
 *   - helios_wddm.h supplies HELIOS_HOB1_FLAG_D3D11_PHYSICAL /
 *     HELIOS_HOB1_FLAG_D3D12_VIRTUAL (HQA1's `flags` IS HOB1's outer-context
 *     arm, not merely equal to it) and HELIOS_HOB1_MAX_BYTES (a context-local
 *     batch IS one HOB1 record).
 *   - helios_native_render.h supplies HELIOS_HNR2_MAX_OUTSTANDING_SUBMISSIONS
 *     (§10.7 lines 1837-1840 state the 64-outstanding number once; the two lanes
 *     bound different pools with the same doc number).
 *
 * Two independent declarations of the same wire arm, hand-kept equal, is
 * precisely the drift class this ABI exists to prevent: an HQA1 that attaches a
 * physical context while its HOB1 records claim the virtual arm would pass both
 * validators separately.
 */
#include "helios_native_render.h"
#include "helios_wddm.h"

#if defined(__cplusplus)
extern "C" {
#endif

#if defined(__cplusplus)
#define HELIOS_TS_STATIC_ASSERT(cond, msg) static_assert(cond, msg)
#define HELIOS_TS_ALIGNOF(type) alignof(type)
#else
#define HELIOS_TS_STATIC_ASSERT(cond, msg) _Static_assert(cond, msg)
#define HELIOS_TS_ALIGNOF(type) _Alignof(type)
#endif

/* ── The atomic package generation (§17.1, final bullet) ───────────────────
 *
 * ⛔ SINGLE SOURCE OF TRUTH: `protocol/src/lib.rs`
 * (`HELIOS_PACKAGE_GENERATION`). This is a hand-maintained C mirror, and the
 * Rust side carries a test pinning the exact literal and naming this header.
 * Change one, change all.
 *
 * HQA1 and both HTS1 records carry it in their `package_generation` field, and a
 * mismatch is fatal with no fallback and no wildcard: zero is never "any" on
 * either side (§10.2 admission table, §17.8 steps 5-6).
 *
 * Guarded so this header, helios_wddm.h, helios_native_render.h and
 * helios_diagnostics.h can be included together — and normally it is already
 * defined by one of the two headers included above.
 */
#ifndef HELIOS_PACKAGE_GENERATION
#define HELIOS_PACKAGE_GENERATION_TAG     0x48454C49u
#define HELIOS_PACKAGE_GENERATION_ORDINAL 1u
#define HELIOS_PACKAGE_GENERATION \
    ((((uint64_t)HELIOS_PACKAGE_GENERATION_TAG) << 32) | \
     (uint64_t)HELIOS_PACKAGE_GENERATION_ORDINAL)
#endif

HELIOS_TS_STATIC_ASSERT(HELIOS_PACKAGE_GENERATION == UINT64_C(0x48454C4900000001),
                        "package generation must equal protocol/src/lib.rs "
                        "HELIOS_PACKAGE_GENERATION");

/* ------------------------------------------------------------------------ */
/* Magics, versions, sizes                                                   */
/* ------------------------------------------------------------------------ */

/* 'HQA1' little-endian (§10.4, table row `offset 0`). */
#define HELIOS_HQA1_MAGIC       0x31415148u
#define HELIOS_HQA1_ABI_VERSION 1u
/*
 * The create-context private data must be EXACTLY this many bytes: §10.4 line
 * 1218 calls HQA1 "the complete create-context private data", so a longer buffer
 * is as malformed as a shorter one.
 */
#define HELIOS_HQA1_SIZE        72u

/*
 * 'HTS1' / 'HTR1' little-endian.
 *
 * ⚠ §10.4 specifies the INIT request/reply CONTENTS (lines 1196-1201) but gives
 * no byte table and no magic for either, unlike HQA1/HOB1/HOS1/HVC1/HNR2/HVM1/
 * HVR1. The tags follow the corpus's own convention (ASCII, little endian) and
 * the request and reply get DISTINCT magics for a reason the retired HPS2 ABI
 * proved the hard way — its `HELIOS_PRESENT_REFRESH_MAGIC` shared a decoder with
 * its sibling record, and a shared magic lets one decoder arm accept the other
 * record's bytes.
 */
#define HELIOS_HTS1_INIT_MAGIC   0x31535448u
#define HELIOS_HTS1_REPLY_MAGIC  0x31525448u
#define HELIOS_HTS1_ABI_VERSION  1u
#define HELIOS_HTS1_INIT_SIZE    32u
#define HELIOS_HTS1_REPLY_SIZE   56u
/*
 * The endpoint descriptor is an array element of a versioned parent, never a
 * standalone message, so it carries no magic of its own.
 */
#define HELIOS_HTS1_ENDPOINT_SIZE 16u

/* ------------------------------------------------------------------------ */
/* HQA1 flags (§10.4, table row `offset 64`)                                 */
/* ------------------------------------------------------------------------ */

/*
 * UNIFIED WITH HOB1. HQA1's `offset 64` and HOB1's `offset 44` are the same
 * two-valued outer-context arm, and §10.4 puts the NUMBERS on the HOB1 row
 * ("exactly one of `D3D11_PHYSICAL=1`, `D3D12_VIRTUAL=2`", line 1267) while
 * giving HQA1 only bit positions. helios_wddm.h declares the values; these are
 * aliases of them, never a second declaration.
 *
 * bit 0 — the outer context is the D3D11 PHYSICAL Render context
 *         (`pfnCreateContextCb` + `pfnRenderCb`).
 * bit 1 — the outer context is the D3D12 VIRTUAL Submit context
 *         (`pfnCreateContextVirtualCb` + `pfnSubmitCommandCb`).
 *
 * "Exactly one set" is the rule. Any bit outside the mask is a hard reject
 * BEFORE the count, so a packet setting one known bit plus an unknown one can
 * never be admitted as that known kind — a future bit cannot be smuggled past an
 * old KMD as a no-op.
 */
#define HELIOS_HQA1_FLAG_D3D11_PHYSICAL HELIOS_HOB1_FLAG_D3D11_PHYSICAL
#define HELIOS_HQA1_FLAG_D3D12_VIRTUAL  HELIOS_HOB1_FLAG_D3D12_VIRTUAL
#define HELIOS_HQA1_FLAGS_MASK \
    (HELIOS_HQA1_FLAG_D3D11_PHYSICAL | HELIOS_HQA1_FLAG_D3D12_VIRTUAL)

/* ------------------------------------------------------------------------ */
/* Engine class (§10.4, table row `offset 44`)                               */
/* ------------------------------------------------------------------------ */

/*
 * The doc names the vocabulary — "exact graphics/compute/copy class admitted for
 * the outer context" — but not the encoding. These are dense 1-based values;
 * ZERO is reserved as "unset" so a zeroed buffer can never classify as a real
 * engine, and every unknown value is a hard reject, never a silent default.
 */
#define HELIOS_ENGINE_CLASS_GRAPHICS 1u
#define HELIOS_ENGINE_CLASS_COMPUTE  2u
#define HELIOS_ENGINE_CLASS_COPY     3u

/* ------------------------------------------------------------------------ */
/* Bounded limits (§10.2 line 990, §10.4 lines 1243-1251 and 1362-1366)      */
/* ------------------------------------------------------------------------ */

/*
 * ⛔ Exhausting any of these is a HARD FAILURE of session/device/context
 * creation — "bounded session/ring exhaustion fails before device exposure"
 * (line 990) and "host-dispatch-FIFO exhaustion, or session-capacity exhaustion
 * fails device creation or removes that device" (lines 1363-1365). Never a wait,
 * never a spill, never a retry loop.
 *
 * Where the doc states a number it is used verbatim; where it only says
 * "bounded" in prose the constant is still declared, because an unstated bound is
 * an unbounded implementation. Each such case is marked PROSE-ONLY below with
 * the reasoning the Rust module records.
 */

/*
 * PROSE-ONLY. Maximum live HTS1 sessions in one KMD `ProcessContext`. One
 * session exists per Mesa `vn_instance` (invariant 12) and a realistic process
 * holds a handful (DXVK, vkd3d, and any native Vulkan instance); 16 leaves an
 * order of magnitude of headroom while keeping the nonpaged list small and
 * fixed. The list is never searched on a submit path, so its size is a capacity
 * decision only.
 */
#define HELIOS_HTS1_MAX_SESSIONS_PER_PROCESS 16u

/*
 * HARD WIRE CEILING, not policy: the virtio-gpu control header carries
 * `ring_idx` as a single byte, so a host context has rings `0..=255` and ring 0
 * is the CPU/decode-only control ring (invariant 12).
 */
#define HELIOS_HTS1_MAX_RING_INDEX 255u

/*
 * PROSE-ONLY, with the hard ceiling above it. Each endpoint owns one unique,
 * non-recycled nonzero ring (invariant 12), so the capacity can never exceed
 * HELIOS_HTS1_MAX_RING_INDEX. A Vulkan physical device on this substrate exposes
 * single-digit queue counts across all families, so 64 is far above any real
 * device while staying well inside the ring namespace.
 */
#define HELIOS_HTS1_MAX_ENDPOINTS_PER_SESSION 64u

/*
 * DOC NUMBER, aliased. §10.7 lines 1837-1840: "one slot from a context-local
 * generation-checked pool capped at 64 outstanding submissions and 15 MiB
 * total"; restated in §16.2. That sentence is §10.7's, so
 * helios_native_render.h declares the number and this is an alias. The two lanes
 * bound DIFFERENT pools — this one counts unretired context-local batch IDs on
 * an outer HQA1-attached context, the native one counts checked-out HNR2 reply
 * slots on a raw HVC1 context — but they are the same doc number, and a reader
 * who changes one must change both.
 */
#define HELIOS_HTS1_MAX_OUTSTANDING_CONTEXT_BATCHES \
    HELIOS_HNR2_MAX_OUTSTANDING_SUBMISSIONS

/*
 * DOC NUMBER, aliased. §10.4 HOB1 table row `offset 48`: "nonzero, at most
 * 15 MiB". A context-local batch on an outer context IS one HOB1 record, so this
 * aliases helios_wddm.h's HELIOS_HOB1_MAX_BYTES — the constant the HOB1 header
 * validator actually enforces. Declaring the cap twice would let session
 * admission and the record validator disagree about which batches are legal.
 */
#define HELIOS_HTS1_MAX_CONTEXT_BATCH_BYTES HELIOS_HOB1_MAX_BYTES

/*
 * PROSE-ONLY. Depth of one physical endpoint's host-dispatch FIFO — the queue of
 * already-scheduler-eligible DMAs to which `DxgkDdiSubmitCommand` assigns
 * arrival-order serials under the short endpoint lock. §10.4 lines 1243-1248:
 * the FIFO is SHARED by outer contexts that vkd3d/DXVK mapped to the same
 * physical VkQueue, so it is sized at four times one context's outstanding cap:
 * a single context can never exhaust it alone, which keeps exhaustion a genuine
 * capacity signal rather than a self-inflicted one.
 */
#define HELIOS_HTS1_MAX_HOST_DISPATCH_FIFO_DEPTH \
    (4u * HELIOS_HTS1_MAX_OUTSTANDING_CONTEXT_BATCHES)

/* ------------------------------------------------------------------------ */
/* Control-opcode classes (§10.4, lines 1335-1366)                           */
/* ------------------------------------------------------------------------ */

/*
 * "Control requests are divided statically" into three classes. This ABI owns
 * the CLASS VOCABULARY and the carrier contract only. ⛔ The per-opcode
 * allowlist — which generated Venus opcode is in which class — is generated in
 * Mesa and in the KMD from the same schema and is deliberately NOT duplicated
 * here: "The allowlist and maximum input/reply size are part of the package
 * generation" (line 1362), and a second hand-maintained copy would be a mirror
 * that can drift.
 *
 *   1 PURE — bounded instance/device creation, Vulkan object creation or
 *     metadata that cannot dereference an outer D3D/resource allocation,
 *     capability and memory-requirement queries, descriptor-independent pipeline
 *     compilation, and their finite replies. May execute on HVC1/ring zero. Its
 *     only allocation access is the one checked-out slot of the session-owned
 *     HVM1 reply pool named writable in that Render.
 *   2 OUTER_ALLOCATION_BACKED — memory allocation/materialization, bind,
 *     map/cache ownership, transfer, query-result storage, destruction, or
 *     object materialization that can touch an outer allocation. Real only in
 *     the first actual outer batch whose allocation list/GPUVA names every exact
 *     allocation. ⛔ Ring zero never receives or resolves such an allocation.
 *   3 GPU_DEPENDENT — queue/device idle, fence status/wait, query `WAIT`, and
 *     teardown. The owning outer UMD must first submit pending batches, signal
 *     that context's private HQC1 value, and perform one event-backed CPU wait;
 *     only after that exact endpoint milestone completes may HVC1 fetch bounded
 *     result bytes. ⛔ Never encoded into an outer batch.
 */
#define HELIOS_CONTROL_CLASS_PURE                    1u
#define HELIOS_CONTROL_CLASS_OUTER_ALLOCATION_BACKED 2u
#define HELIOS_CONTROL_CLASS_GPU_DEPENDENT           3u

/* ------------------------------------------------------------------------ */
/* HTS1 — the endpoint descriptor                                            */
/* ------------------------------------------------------------------------ */

/*
 * Mirror of Rust `HeliosTranslationEndpointV1` — one physical lower-queue
 * endpoint of an HTS1 session, 16 bytes.
 *
 * The KMD returns the session's endpoints to the translator through the private
 * direct table (§14 lines 3994-4001: "physical endpoint descriptors only through
 * the private direct table"), and the UMD copies the selected one into HQA1.
 *
 * ⚠ `queue_family` and `queue_index` are DIAGNOSTIC CROSS-CHECK ONLY, NEVER
 * IDENTITY (§10.4 table rows `offset 48`/`offset 52`; the same rule appears for
 * HVC1 in §10.7 lines 1706-1707). The KMD compares them and rejects a mismatch —
 * it never looks an endpoint up by them, and no validator uses them to select
 * anything.
 */
typedef struct HeliosTranslationEndpointV1 {
    uint32_t endpoint_id;  /* 0  session-local ordinal, 1..=endpoint_capacity,
                            *    nonzero. ⛔ NOT a host INFO_RING_IDX */
    uint32_t engine_class; /* 4  HELIOS_ENGINE_CLASS_* */
    uint32_t queue_family; /* 8  diagnostic cross-check only */
    uint32_t queue_index;  /* 12 diagnostic cross-check only */
} HeliosTranslationEndpointV1;

HELIOS_TS_STATIC_ASSERT(sizeof(HeliosTranslationEndpointV1) == 16,
                        "the HTS1 endpoint descriptor must be 16 bytes");
HELIOS_TS_STATIC_ASSERT(sizeof(HeliosTranslationEndpointV1) == HELIOS_HTS1_ENDPOINT_SIZE,
                        "endpoint size constant must match the struct");
HELIOS_TS_STATIC_ASSERT(HELIOS_TS_ALIGNOF(HeliosTranslationEndpointV1) == 4, "");
HELIOS_TS_STATIC_ASSERT(offsetof(HeliosTranslationEndpointV1, endpoint_id) == 0, "");
HELIOS_TS_STATIC_ASSERT(offsetof(HeliosTranslationEndpointV1, engine_class) == 4, "");
HELIOS_TS_STATIC_ASSERT(offsetof(HeliosTranslationEndpointV1, queue_family) == 8, "");
HELIOS_TS_STATIC_ASSERT(offsetof(HeliosTranslationEndpointV1, queue_index) == 12, "");

/*
 * BOTH queue ordinals equal to `UINT32_MAX` is HVC1's reserved encoding for the
 * CONTROL context (§10.7 table rows `offset 24`/`offset 28`,
 * HELIOS_HVC1_CONTROL_ORDINAL in helios_native_render.h). An endpoint is always
 * a real physical lower VkQueue, so the control sentinel can never describe one
 * and a descriptor carrying it is refused.
 */

/* ------------------------------------------------------------------------ */
/* HTS1 — the finite INIT request                                            */
/* ------------------------------------------------------------------------ */

/*
 * Mirror of Rust `HeliosTranslationSessionInitV1`, 32 bytes.
 *
 * The finite `INIT` the raw KMT device's HVC1 control context sends before any
 * dependent outer context exists (§10.4 lines 1188-1203). It carries only what
 * INIT must confirm: the atomic package generation, the exact Venus capset the
 * caller was built against, and the endpoint capacity it asks the session to
 * admit. ⛔ It names NO allocation — the reply-pool slot it will be written into
 * is named by the enclosing HNR2 record's allocation list and reply descriptor,
 * not by these bytes.
 */
typedef struct HeliosTranslationSessionInitV1 {
    uint32_t magic;                       /* 0  == HELIOS_HTS1_INIT_MAGIC */
    uint16_t abi_version;                 /* 4  == HELIOS_HTS1_ABI_VERSION */
    uint16_t struct_size;                 /* 6  == HELIOS_HTS1_INIT_SIZE */
    uint64_t package_generation;          /* 8  exact; zero is never a wildcard */
    uint32_t capset;                      /* 16 == HELIOS_NATIVE_RENDER_CAPSET,
                                           *    matching HVC1's `capset` */
    uint32_t requested_endpoint_capacity; /* 20 1..=MAX_ENDPOINTS_PER_SESSION;
                                           *    the reply is authoritative and
                                           *    may be smaller */
    uint64_t reserved;                    /* 24 zero */
} HeliosTranslationSessionInitV1;

HELIOS_TS_STATIC_ASSERT(sizeof(HeliosTranslationSessionInitV1) == 32,
                        "the HTS1 INIT request must be 32 bytes");
HELIOS_TS_STATIC_ASSERT(sizeof(HeliosTranslationSessionInitV1) == HELIOS_HTS1_INIT_SIZE,
                        "INIT size constant must match the struct");
HELIOS_TS_STATIC_ASSERT(HELIOS_TS_ALIGNOF(HeliosTranslationSessionInitV1) == 8, "");
HELIOS_TS_STATIC_ASSERT(offsetof(HeliosTranslationSessionInitV1, magic) == 0, "");
HELIOS_TS_STATIC_ASSERT(offsetof(HeliosTranslationSessionInitV1, abi_version) == 4, "");
HELIOS_TS_STATIC_ASSERT(offsetof(HeliosTranslationSessionInitV1, struct_size) == 6, "");
HELIOS_TS_STATIC_ASSERT(offsetof(HeliosTranslationSessionInitV1, package_generation) == 8, "");
HELIOS_TS_STATIC_ASSERT(offsetof(HeliosTranslationSessionInitV1, capset) == 16, "");
HELIOS_TS_STATIC_ASSERT(
    offsetof(HeliosTranslationSessionInitV1, requested_endpoint_capacity) == 20, "");
HELIOS_TS_STATIC_ASSERT(offsetof(HeliosTranslationSessionInitV1, reserved) == 24, "");

/* ------------------------------------------------------------------------ */
/* HTS1 — the INIT reply                                                     */
/* ------------------------------------------------------------------------ */

/*
 * Mirror of Rust `HeliosTranslationSessionReplyV1`, 56 bytes.
 *
 * What a successful INIT returns "through the C51 event-backed reply" (§10.4
 * lines 1198-1201): a nonzero session generation, a CSPRNG-generated 128-bit
 * capability, the bounded endpoint capacity, and package/capset confirmation.
 *
 * It travels inside one checked-out slot of the session's role-1 HVM1 pool,
 * AFTER the HVR1 header that helios_native_render.h declares; HVR1's `status`
 * carries the operation's result, so this payload exists only on success and
 * carries no status of its own.
 *
 * ⛔ It carries no host context ID, no ring index, no allocation identity, and no
 * handle. A reply that grants MORE endpoints than the request asked for is a
 * hard reject rather than a silent windfall.
 */
typedef struct HeliosTranslationSessionReplyV1 {
    uint32_t magic;               /* 0  == HELIOS_HTS1_REPLY_MAGIC */
    uint16_t abi_version;         /* 4  == HELIOS_HTS1_ABI_VERSION */
    uint16_t struct_size;         /* 6  == HELIOS_HTS1_REPLY_SIZE */
    uint64_t package_generation;  /* 8  the exact generation the KMD is running */
    uint64_t session_generation;  /* 16 the new session's generation; nonzero */
    uint64_t capability_low;      /* 24 CSPRNG, KMD-generated. ⛔ never logged */
    uint64_t capability_high;     /* 32 CSPRNG, KMD-generated. ⛔ never logged */
    uint32_t capset;              /* 40 the exact capset the host context got */
    uint32_t endpoint_capacity;   /* 44 1..=MAX_ENDPOINTS_PER_SESSION, and never
                                   *    more than the request asked for */
    uint64_t reserved;            /* 48 zero */
} HeliosTranslationSessionReplyV1;

HELIOS_TS_STATIC_ASSERT(sizeof(HeliosTranslationSessionReplyV1) == 56,
                        "the HTS1 INIT reply must be 56 bytes");
HELIOS_TS_STATIC_ASSERT(sizeof(HeliosTranslationSessionReplyV1) == HELIOS_HTS1_REPLY_SIZE,
                        "reply size constant must match the struct");
HELIOS_TS_STATIC_ASSERT(HELIOS_TS_ALIGNOF(HeliosTranslationSessionReplyV1) == 8, "");
HELIOS_TS_STATIC_ASSERT(offsetof(HeliosTranslationSessionReplyV1, magic) == 0, "");
HELIOS_TS_STATIC_ASSERT(offsetof(HeliosTranslationSessionReplyV1, abi_version) == 4, "");
HELIOS_TS_STATIC_ASSERT(offsetof(HeliosTranslationSessionReplyV1, struct_size) == 6, "");
HELIOS_TS_STATIC_ASSERT(offsetof(HeliosTranslationSessionReplyV1, package_generation) == 8, "");
HELIOS_TS_STATIC_ASSERT(offsetof(HeliosTranslationSessionReplyV1, session_generation) == 16, "");
HELIOS_TS_STATIC_ASSERT(offsetof(HeliosTranslationSessionReplyV1, capability_low) == 24, "");
HELIOS_TS_STATIC_ASSERT(offsetof(HeliosTranslationSessionReplyV1, capability_high) == 32, "");
HELIOS_TS_STATIC_ASSERT(offsetof(HeliosTranslationSessionReplyV1, capset) == 40, "");
HELIOS_TS_STATIC_ASSERT(offsetof(HeliosTranslationSessionReplyV1, endpoint_capacity) == 44, "");
HELIOS_TS_STATIC_ASSERT(offsetof(HeliosTranslationSessionReplyV1, reserved) == 48, "");

/* ------------------------------------------------------------------------ */
/* §10.4 — HQA1: the complete create-context private driver data             */
/* ------------------------------------------------------------------------ */

/*
 * Mirror of Rust `HeliosQueueAttachV1` — §10.4's 72-byte table (lines 1215-1241).
 *
 * It attaches ONE outer D3D context to ONE existing HTS1 session and physical
 * lower-queue endpoint, and it is the COMPLETE create-context private driver
 * data the UMD passes to `pfnCreateContextCb` (D3D11) or
 * `pfnCreateContextVirtualCb` (D3D12).
 *
 * ⛔ IT IS READ EXACTLY ONCE. "KMD validates HQA1 once during
 * `DxgkDdiCreateContext`, rejects a zero or duplicate live context generation,
 * stores that generation in the new context, takes a strong direct reference to
 * that session/endpoint, and erases no capability into later DMA." After that
 * the live KMD context object IS the identity; HOB1/HOS1 repeat the stored
 * generation only as an anti-stale cross-check and no later lookup uses the
 * number (invariant 13).
 *
 * ⛔ `D3DDDICB_RENDER::pPrivateDriverData` stays reserved-zero: no Render,
 * Submit, Present, allocation-open, or display callback may attach a session or
 * reinterpret HQA1 (§10.4 lines 1327-1333).
 *
 * The KMD's validation order is fail-closed and structural first — shape, then
 * reserved/flags, then generations, then the capability, then the endpoint
 * cross-checks, and the monotonic context generation LAST — so a malformed
 * packet is rejected before its capability is even compared.
 */
typedef struct HeliosQueueAttachV1 {
    uint32_t magic;              /* 0  == HELIOS_HQA1_MAGIC */
    uint16_t abi_version;        /* 4  == HELIOS_HQA1_ABI_VERSION */
    uint16_t struct_size;        /* 6  == HELIOS_HQA1_SIZE */
    uint64_t package_generation; /* 8  exact atomic-package generation */
    uint64_t session_generation; /* 16 exact nonzero HTS1 generation */
    uint64_t capability_low;     /* 24 unpredictable KMD-returned nonce */
    uint64_t capability_high;    /* 32 unpredictable KMD-returned nonce */
    uint32_t endpoint_id;        /* 40 exact endpoint ordinal, nonzero.
                                  *    ⛔ NOT a ring index */
    uint32_t engine_class;       /* 44 HELIOS_ENGINE_CLASS_*, exactly the class
                                  *    of that endpoint */
    uint32_t queue_family;       /* 48 diagnostic cross-check only */
    uint32_t queue_index;        /* 52 diagnostic cross-check only */
    uint64_t context_generation; /* 56 UMD-chosen, nonzero, strictly increasing,
                                  *    never reused within this HTS1 session */
    uint32_t flags;              /* 64 HELIOS_HQA1_FLAG_*; exactly one set */
    uint32_t reserved;           /* 68 zero */
} HeliosQueueAttachV1;

HELIOS_TS_STATIC_ASSERT(sizeof(HeliosQueueAttachV1) == 72,
                        "HQA1 must be the §10.4 72-byte create-context record");
HELIOS_TS_STATIC_ASSERT(sizeof(HeliosQueueAttachV1) == HELIOS_HQA1_SIZE,
                        "HQA1 size constant must match the struct");
HELIOS_TS_STATIC_ASSERT(HELIOS_TS_ALIGNOF(HeliosQueueAttachV1) == 8, "");
HELIOS_TS_STATIC_ASSERT(offsetof(HeliosQueueAttachV1, magic) == 0, "");
HELIOS_TS_STATIC_ASSERT(offsetof(HeliosQueueAttachV1, abi_version) == 4, "");
HELIOS_TS_STATIC_ASSERT(offsetof(HeliosQueueAttachV1, struct_size) == 6, "");
HELIOS_TS_STATIC_ASSERT(offsetof(HeliosQueueAttachV1, package_generation) == 8, "");
HELIOS_TS_STATIC_ASSERT(offsetof(HeliosQueueAttachV1, session_generation) == 16, "");
HELIOS_TS_STATIC_ASSERT(offsetof(HeliosQueueAttachV1, capability_low) == 24, "");
HELIOS_TS_STATIC_ASSERT(offsetof(HeliosQueueAttachV1, capability_high) == 32, "");
HELIOS_TS_STATIC_ASSERT(offsetof(HeliosQueueAttachV1, endpoint_id) == 40, "");
HELIOS_TS_STATIC_ASSERT(offsetof(HeliosQueueAttachV1, engine_class) == 44, "");
HELIOS_TS_STATIC_ASSERT(offsetof(HeliosQueueAttachV1, queue_family) == 48, "");
HELIOS_TS_STATIC_ASSERT(offsetof(HeliosQueueAttachV1, queue_index) == 52, "");
HELIOS_TS_STATIC_ASSERT(offsetof(HeliosQueueAttachV1, context_generation) == 56, "");
HELIOS_TS_STATIC_ASSERT(offsetof(HeliosQueueAttachV1, flags) == 64, "");
HELIOS_TS_STATIC_ASSERT(offsetof(HeliosQueueAttachV1, reserved) == 68, "");

/* ── Vocabulary and bound relationships the Rust side asserts ──────────────*/

/* HQA1's flags agree numerically with HOB1's (§10.4, `offset 44`). */
HELIOS_TS_STATIC_ASSERT(HELIOS_HQA1_FLAG_D3D11_PHYSICAL == 1u, "");
HELIOS_TS_STATIC_ASSERT(HELIOS_HQA1_FLAG_D3D12_VIRTUAL == 2u, "");
HELIOS_TS_STATIC_ASSERT(HELIOS_HQA1_FLAGS_MASK == 3u, "");

/* Every endpoint consumes one unique nonzero host ring, and the virtio-gpu
 * header's `ring_idx` is one byte. */
HELIOS_TS_STATIC_ASSERT(HELIOS_HTS1_MAX_ENDPOINTS_PER_SESSION <= HELIOS_HTS1_MAX_RING_INDEX, "");
HELIOS_TS_STATIC_ASSERT(HELIOS_HTS1_MAX_ENDPOINTS_PER_SESSION > 0u, "");
HELIOS_TS_STATIC_ASSERT(HELIOS_HTS1_MAX_SESSIONS_PER_PROCESS > 0u, "");
HELIOS_TS_STATIC_ASSERT(HELIOS_HTS1_MAX_OUTSTANDING_CONTEXT_BATCHES == 64u, "");
HELIOS_TS_STATIC_ASSERT(HELIOS_HTS1_MAX_CONTEXT_BATCH_BYTES == UINT64_C(15728640), "");
/* A single context must not be able to exhaust the shared endpoint FIFO. */
HELIOS_TS_STATIC_ASSERT(HELIOS_HTS1_MAX_HOST_DISPATCH_FIFO_DEPTH >
                            HELIOS_HTS1_MAX_OUTSTANDING_CONTEXT_BATCHES, "");

/* Engine classes are dense, 1-based, and never zero; control classes likewise. */
HELIOS_TS_STATIC_ASSERT(HELIOS_ENGINE_CLASS_GRAPHICS == 1u, "");
HELIOS_TS_STATIC_ASSERT(HELIOS_ENGINE_CLASS_COMPUTE == 2u, "");
HELIOS_TS_STATIC_ASSERT(HELIOS_ENGINE_CLASS_COPY == 3u, "");
HELIOS_TS_STATIC_ASSERT(HELIOS_CONTROL_CLASS_PURE == 1u, "");
HELIOS_TS_STATIC_ASSERT(HELIOS_CONTROL_CLASS_OUTER_ALLOCATION_BACKED == 2u, "");
HELIOS_TS_STATIC_ASSERT(HELIOS_CONTROL_CLASS_GPU_DEPENDENT == 3u, "");

/* ── Every magic is four ASCII bytes read little-endian ────────────────────
 *
 * A transposed hex digit in a hand-copied literal would otherwise compile clean
 * here and fail only against the Rust side or a live host.
 */
HELIOS_TS_STATIC_ASSERT((HELIOS_HQA1_MAGIC & 0xFFu) == (unsigned char)'H', "");
HELIOS_TS_STATIC_ASSERT(((HELIOS_HQA1_MAGIC >> 8) & 0xFFu) == (unsigned char)'Q', "");
HELIOS_TS_STATIC_ASSERT(((HELIOS_HQA1_MAGIC >> 16) & 0xFFu) == (unsigned char)'A', "");
HELIOS_TS_STATIC_ASSERT(((HELIOS_HQA1_MAGIC >> 24) & 0xFFu) == (unsigned char)'1', "");
HELIOS_TS_STATIC_ASSERT((HELIOS_HTS1_INIT_MAGIC & 0xFFu) == (unsigned char)'H', "");
HELIOS_TS_STATIC_ASSERT(((HELIOS_HTS1_INIT_MAGIC >> 8) & 0xFFu) == (unsigned char)'T', "");
HELIOS_TS_STATIC_ASSERT(((HELIOS_HTS1_INIT_MAGIC >> 16) & 0xFFu) == (unsigned char)'S', "");
HELIOS_TS_STATIC_ASSERT(((HELIOS_HTS1_INIT_MAGIC >> 24) & 0xFFu) == (unsigned char)'1', "");
HELIOS_TS_STATIC_ASSERT((HELIOS_HTS1_REPLY_MAGIC & 0xFFu) == (unsigned char)'H', "");
HELIOS_TS_STATIC_ASSERT(((HELIOS_HTS1_REPLY_MAGIC >> 8) & 0xFFu) == (unsigned char)'T', "");
HELIOS_TS_STATIC_ASSERT(((HELIOS_HTS1_REPLY_MAGIC >> 16) & 0xFFu) == (unsigned char)'R', "");
HELIOS_TS_STATIC_ASSERT(((HELIOS_HTS1_REPLY_MAGIC >> 24) & 0xFFu) == (unsigned char)'1', "");
/* The three record magics must stay mutually distinct: a decoder that tries more
 * than one arm on the same bytes must never accept the wrong record. */
HELIOS_TS_STATIC_ASSERT(HELIOS_HQA1_MAGIC != HELIOS_HTS1_INIT_MAGIC, "");
HELIOS_TS_STATIC_ASSERT(HELIOS_HQA1_MAGIC != HELIOS_HTS1_REPLY_MAGIC, "");
HELIOS_TS_STATIC_ASSERT(HELIOS_HTS1_INIT_MAGIC != HELIOS_HTS1_REPLY_MAGIC, "");

#if defined(__cplusplus)
}
#endif

#endif /* HELIOS_TRANSLATION_SESSION_H */
