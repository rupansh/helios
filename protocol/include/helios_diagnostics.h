/*
 * helios_diagnostics.h — C mirror of the Helios graphics ETW schema.
 *
 * ⛔ SINGLE SOURCE OF TRUTH: protocol/src/diagnostics.rs.
 * This header is the hand-maintained C projection of that file for the QEMU
 * (`qemu-helios/`) and Mesa (`icd/mesa`) sides and for any C decoder of the
 * capture. Every constant, struct, and assertion below exists in the Rust file
 * first; if the two ever disagree, the Rust file wins and this header is the
 * bug. Change one, change both in the same commit — the assertions here are
 * what turn a layout drift into a compile error rather than a silent misparse.
 *
 * Normative: docs/HELIOS_PRESENT_SYNC_RETIREMENT.md §12.3 (ETW and
 * OS-diagnostic contract), with §10.8 supplying the plane latch/release events,
 * §17.6 the KMD EtwRegister/EtwWrite contract, and §17.1 the module mandate.
 *
 * WHAT THIS IS: a ONE-WAY, LOSSY schema. `kmd_render` registers one kernel
 * provider (HELIOS_ETW_PROVIDER_GUID) and writes one 72-byte payload per
 * event through a single `EtwWrite` data descriptor. Nothing travels back.
 *
 * WHAT THIS IS NOT: a control or query protocol. There is no verb, no request,
 * no reply, no mapped page, and no acknowledgement here. It replaces the
 * private `D3DKMTEscape` observability ABI (`QUERY_STATS`,
 * `QUERY_SCANOUT_TIMELINE`, the read ledger) that §17.1 deletes with no
 * fallback, and it is not a compatibility carrier for any of it.
 *
 * NO IDENTITY CROSSES THIS BOUNDARY: the payload carries no handle, pointer,
 * host/backing token, raw virtio `resid`, PID, or object name — only
 * generations, values, a status, bounded indices, and reserved zeros. The
 * static assertion on the payload's scalar sum below is what keeps it that way.
 *
 * ETW LOSS CHANGES NO BEHAVIOR: a dropped, gated-off, or rejected event must
 * never affect synchronization, recovery, lifetime, or package admission
 * (§12.3). Nothing declared here may be placed on a correctness path.
 */

#ifndef HELIOS_DIAGNOSTICS_H
#define HELIOS_DIAGNOSTICS_H

#include <stddef.h>
#include <stdint.h>

#if defined(__cplusplus)
extern "C" {
#endif

#if defined(__cplusplus)
#define HELIOS_DIAG_STATIC_ASSERT(cond, msg) static_assert(cond, msg)
#define HELIOS_DIAG_ALIGNOF(type) alignof(type)
#else
#define HELIOS_DIAG_STATIC_ASSERT(cond, msg) _Static_assert(cond, msg)
#define HELIOS_DIAG_ALIGNOF(type) _Alignof(type)
#endif

/* ── The atomic package generation (§17.1, final bullet) ───────────────────
 *
 * "Add one protocol/package generation constant shared by protocol, Mesa,
 * UMD11, UMD12, KMD, QEMU, and installer. Generation mismatch is fatal."
 *
 * ⛔ SINGLE SOURCE OF TRUTH: `protocol/src/lib.rs`
 * (`HELIOS_PACKAGE_GENERATION`). This is its hand-maintained C mirror, and the
 * Rust side carries a test that pins the exact literal below and names this
 * header. Change one, change both in the same commit.
 *
 * Layout: the high 32 bits are the constant ASCII tag 'H','E','L','I' read
 * big-endian, so a real generation always reads `48 45 4c 49 ..` in a hex dump
 * and can never be confused with zero, UINT64_MAX, or a small ordinal that some
 * other field might plausibly hold. The low 32 bits are the monotonic package
 * ordinal; `1` is the HPS2-retirement generation.
 *
 * For THIS header the constant is the value a decoder compares
 * `HeliosGraphicsEtwPayloadV1::package_generation` against — and only that.
 * ETW is a one-way lossy schema: a mismatch means the capture came from a
 * different package generation and must be reported as undecodable, never
 * reinterpreted, and never allowed to affect synchronization, recovery,
 * lifetime, or admission (§12.3). The fatal-mismatch behavior itself lives on
 * the control paths — KMD/UMD device creation, HPM1 negotiation, and installer
 * activation (§10.2 admission table, §17.8 steps 5-6).
 */
#define HELIOS_PACKAGE_GENERATION_TAG     0x48454C49u
#define HELIOS_PACKAGE_GENERATION_ORDINAL 3u
#define HELIOS_PACKAGE_GENERATION \
    ((((uint64_t)HELIOS_PACKAGE_GENERATION_TAG) << 32) | \
     (uint64_t)HELIOS_PACKAGE_GENERATION_ORDINAL)

HELIOS_DIAG_STATIC_ASSERT(HELIOS_PACKAGE_GENERATION == UINT64_C(0x48454C4900000003),
                          "package generation must equal protocol/src/lib.rs "
                          "HELIOS_PACKAGE_GENERATION");
HELIOS_DIAG_STATIC_ASSERT(HELIOS_PACKAGE_GENERATION != 0,
                          "a zero package generation is the uninitialized-buffer "
                          "reading every validator rejects");
HELIOS_DIAG_STATIC_ASSERT(HELIOS_PACKAGE_GENERATION != UINT64_MAX,
                          "an all-ones package generation must not pass either");

/* ── Provider identity (§12.3) ─────────────────────────────────────────────
 *
 * {6D9A1A95-2B6A-4DEF-BCF7-847B6F158B0E} — FIXED. The KMD's `EtwRegister`,
 * `tools/helios_etw_capture.ps1`, and every decoder hard-code this value; it
 * must never be regenerated.
 */
#define HELIOS_ETW_PROVIDER_GUID_DATA1 0x6d9a1a95u
#define HELIOS_ETW_PROVIDER_GUID_DATA2 0x2b6au
#define HELIOS_ETW_PROVIDER_GUID_DATA3 0x4defu
#define HELIOS_ETW_PROVIDER_GUID_DATA4_0 0xbcu
#define HELIOS_ETW_PROVIDER_GUID_DATA4_1 0xf7u
#define HELIOS_ETW_PROVIDER_GUID_DATA4_2 0x84u
#define HELIOS_ETW_PROVIDER_GUID_DATA4_3 0x7bu
#define HELIOS_ETW_PROVIDER_GUID_DATA4_4 0x6fu
#define HELIOS_ETW_PROVIDER_GUID_DATA4_5 0x15u
#define HELIOS_ETW_PROVIDER_GUID_DATA4_6 0x8bu
#define HELIOS_ETW_PROVIDER_GUID_DATA4_7 0x0eu

/*
 * Windows `GUID` layout (`Data1`/`Data2`/`Data3`/`Data4[8]`), declared here so
 * a consumer needs no Windows headers. Mirrors Rust `HeliosEtwGuid`.
 */
typedef struct helios_etw_guid {
    uint32_t data1;
    uint16_t data2;
    uint16_t data3;
    uint8_t data4[8];
} HeliosEtwGuid;

/* Initializer for a `HeliosEtwGuid` (and, field-compatible, for a `GUID`). */
#define HELIOS_ETW_PROVIDER_GUID_INIT                                          \
    {                                                                          \
        HELIOS_ETW_PROVIDER_GUID_DATA1, HELIOS_ETW_PROVIDER_GUID_DATA2,        \
            HELIOS_ETW_PROVIDER_GUID_DATA3,                                    \
        {                                                                      \
            HELIOS_ETW_PROVIDER_GUID_DATA4_0,                                  \
                HELIOS_ETW_PROVIDER_GUID_DATA4_1,                              \
                HELIOS_ETW_PROVIDER_GUID_DATA4_2,                              \
                HELIOS_ETW_PROVIDER_GUID_DATA4_3,                              \
                HELIOS_ETW_PROVIDER_GUID_DATA4_4,                              \
                HELIOS_ETW_PROVIDER_GUID_DATA4_5,                              \
                HELIOS_ETW_PROVIDER_GUID_DATA4_6,                              \
                HELIOS_ETW_PROVIDER_GUID_DATA4_7                               \
        }                                                                      \
    }

/*
 * The same GUID in the 16-byte binary encoding (Data1/Data2/Data3
 * little-endian, then Data4 in order) — what an ETW consumer compares against
 * a captured provider id. Mirrors Rust `HELIOS_ETW_PROVIDER_GUID_BYTES`; the
 * assertions below prove the two spellings agree byte for byte.
 */
#define HELIOS_ETW_PROVIDER_GUID_BYTES_INIT                                    \
    {                                                                          \
        0x95u, 0x1au, 0x9au, 0x6du, 0x6au, 0x2bu, 0xefu, 0x4du, 0xbcu, 0xf7u,  \
            0x84u, 0x7bu, 0x6fu, 0x15u, 0x8bu, 0x0eu                           \
    }

HELIOS_DIAG_STATIC_ASSERT(sizeof(HeliosEtwGuid) == 16,
                          "HeliosEtwGuid must be 16 bytes");
HELIOS_DIAG_STATIC_ASSERT(HELIOS_DIAG_ALIGNOF(HeliosEtwGuid) == 4,
                          "HeliosEtwGuid must be 4-byte aligned");
HELIOS_DIAG_STATIC_ASSERT(offsetof(HeliosEtwGuid, data1) == 0, "guid.data1@0");
HELIOS_DIAG_STATIC_ASSERT(offsetof(HeliosEtwGuid, data2) == 4, "guid.data2@4");
HELIOS_DIAG_STATIC_ASSERT(offsetof(HeliosEtwGuid, data3) == 6, "guid.data3@6");
HELIOS_DIAG_STATIC_ASSERT(offsetof(HeliosEtwGuid, data4) == 8, "guid.data4@8");
/* The binary spelling above must be the little-endian decomposition. */
HELIOS_DIAG_STATIC_ASSERT((HELIOS_ETW_PROVIDER_GUID_DATA1 & 0xffu) == 0x95u,
                          "guid byte 0");
HELIOS_DIAG_STATIC_ASSERT(((HELIOS_ETW_PROVIDER_GUID_DATA1 >> 8) & 0xffu) ==
                              0x1au,
                          "guid byte 1");
HELIOS_DIAG_STATIC_ASSERT(((HELIOS_ETW_PROVIDER_GUID_DATA1 >> 16) & 0xffu) ==
                              0x9au,
                          "guid byte 2");
HELIOS_DIAG_STATIC_ASSERT(((HELIOS_ETW_PROVIDER_GUID_DATA1 >> 24) & 0xffu) ==
                              0x6du,
                          "guid byte 3");
HELIOS_DIAG_STATIC_ASSERT((HELIOS_ETW_PROVIDER_GUID_DATA2 & 0xffu) == 0x6au,
                          "guid byte 4");
HELIOS_DIAG_STATIC_ASSERT(((HELIOS_ETW_PROVIDER_GUID_DATA2 >> 8) & 0xffu) ==
                              0x2bu,
                          "guid byte 5");
HELIOS_DIAG_STATIC_ASSERT((HELIOS_ETW_PROVIDER_GUID_DATA3 & 0xffu) == 0xefu,
                          "guid byte 6");
HELIOS_DIAG_STATIC_ASSERT(((HELIOS_ETW_PROVIDER_GUID_DATA3 >> 8) & 0xffu) ==
                              0x4du,
                          "guid byte 7");

/* ── Event descriptor constants (§12.3) ──────────────────────────────────── */

/* "Every event has ETW descriptor version 1". */
#define HELIOS_ETW_EVENT_VERSION 1u
/* Manifest-free provider: no channel is claimed. */
#define HELIOS_ETW_CHANNEL_NONE 0u
/*
 * `WINEVENT_OPCODE_INFO`. §12.3 defines no opcodes: an event ID naming two or
 * three alternatives discriminates them in the payload flags sub-kind, never
 * in the opcode.
 */
#define HELIOS_ETW_OPCODE_INFO 0u
/* §12.3 defines no task taxonomy; the event ID is the whole taxonomy. */
#define HELIOS_ETW_TASK_NONE 0u

/* `WINEVENT_LEVEL_*`, declared here so a consumer needs no Windows headers. */
#define HELIOS_ETW_LEVEL_LOG_ALWAYS 0u
#define HELIOS_ETW_LEVEL_CRITICAL 1u
#define HELIOS_ETW_LEVEL_ERROR 2u
#define HELIOS_ETW_LEVEL_WARNING 3u
#define HELIOS_ETW_LEVEL_INFORMATIONAL 4u
#define HELIOS_ETW_LEVEL_VERBOSE 5u

/*
 * The level of every Helios event. §12.3 assigns no per-event levels; one
 * uniform INFORMATIONAL level stays filterable by the maximum logging level
 * `DxgkDdiControlEtwLogging` sets (which LOG_ALWAYS would defeat) and assigns
 * no severity the doc never states.
 */
#define HELIOS_ETW_EVENT_LEVEL HELIOS_ETW_LEVEL_INFORMATIONAL

/* ── Keywords (§12.3) ──────────────────────────────────────────────────────
 *
 * "Keywords are bit 0 submission, bit 1 native synchronization, bit 2 display
 * lifetime, bit 3 device lifecycle, and bit 4 translation-session lifetime."
 *
 * ⚠ These belong to `EtwProviderEnabled`/the trace session ONLY.
 * `DxgkDdiControlEtwLogging`'s `Flags` "must be zero and is not treated as a
 * keyword mask" (§12.3) — see HELIOS_ETW_CONTROL_FLAGS_REQUIRED.
 */
#define HELIOS_ETW_KEYWORD_SUBMISSION (UINT64_C(1) << 0)
#define HELIOS_ETW_KEYWORD_NATIVE_SYNCHRONIZATION (UINT64_C(1) << 1)
#define HELIOS_ETW_KEYWORD_DISPLAY_LIFETIME (UINT64_C(1) << 2)
#define HELIOS_ETW_KEYWORD_DEVICE_LIFECYCLE (UINT64_C(1) << 3)
#define HELIOS_ETW_KEYWORD_TRANSLATION_SESSION_LIFETIME (UINT64_C(1) << 4)
#define HELIOS_ETW_KEYWORD_ALL                                                 \
    (HELIOS_ETW_KEYWORD_SUBMISSION | HELIOS_ETW_KEYWORD_NATIVE_SYNCHRONIZATION \
     | HELIOS_ETW_KEYWORD_DISPLAY_LIFETIME | HELIOS_ETW_KEYWORD_DEVICE_LIFECYCLE \
     | HELIOS_ETW_KEYWORD_TRANSLATION_SESSION_LIFETIME)

HELIOS_DIAG_STATIC_ASSERT(HELIOS_ETW_KEYWORD_ALL == UINT64_C(0x1f),
                          "five keyword classes, bits 0..4");

/*
 * The only legal value of `DXGKARG_CONTROLETWLOGGING::Flags` (§12.3: "its
 * currently undefined Flags must be zero and is not treated as a keyword
 * mask"). A nonzero value is a hard refusal, never a mask to honour.
 */
#define HELIOS_ETW_CONTROL_FLAGS_REQUIRED UINT64_C(0)

/* ── EtwWrite shape (§12.3) ───────────────────────────────────────────────── */

/* "each EtwWrite uses one data descriptor" — the whole event is the payload. */
#define HELIOS_ETW_DATA_DESCRIPTOR_COUNT 1u
/* The documented EtwWrite data-descriptor limit §12.3 measures against. */
#define HELIOS_ETW_MAX_DATA_DESCRIPTORS 128u

HELIOS_DIAG_STATIC_ASSERT(HELIOS_ETW_DATA_DESCRIPTOR_COUNT <
                              HELIOS_ETW_MAX_DATA_DESCRIPTORS,
                          "one descriptor, far below the documented limit");

/* ── Event IDs (§12.3) ────────────────────────────────────────────────────── */

#define HELIOS_ETW_EVENT_COUNT 12u
#define HELIOS_ETW_EVENT_ID_MIN 1u
#define HELIOS_ETW_EVENT_ID_MAX 12u

/*
 * The complete §12.3 event-ID set. Names are verbatim from the doc, including
 * the `...OrX` pairs: one ID covers both/all of the named transitions and the
 * payload flags sub-kind says which one it was.
 */
typedef enum helios_etw_event_id {
    /* Batch submitted (the batch that actually performs the writes). */
    HELIOS_ETW_EVENT_BATCH_SUBMIT = 1,
    /* Batch reached terminal host GPU completion. */
    HELIOS_ETW_EVENT_BATCH_COMPLETE = 2,
    /* Batch cancelled, or faulted. */
    HELIOS_ETW_EVENT_BATCH_CANCEL_OR_FAULT = 3,
    /* Core-0116 native fence object created, or opened. */
    HELIOS_ETW_EVENT_NATIVE_FENCE_CREATE_OR_OPEN = 4,
    /* Native-fence wait, or signal, issued. */
    HELIOS_ETW_EVENT_NATIVE_FENCE_WAIT_OR_SIGNAL = 5,
    /* Native fence closed, or reset. */
    HELIOS_ETW_EVENT_NATIVE_FENCE_CLOSE_OR_RESET = 6,
    /* §10.8 plane candidate validated and retained (pre-latch). */
    HELIOS_ETW_EVENT_PLANE_CANDIDATE = 7,
    /* Candidate latched as the current binding, or cancelled before latch. */
    HELIOS_ETW_EVENT_PLANE_LATCH_OR_CANCEL = 8,
    /* Backend acknowledged no reader uses a former binding, or explicit
     * plane/source unbind (§10.8). */
    HELIOS_ETW_EVENT_PLANE_READER_RELEASE_OR_UNBIND = 9,
    /* Device reset, or removal. */
    HELIOS_ETW_EVENT_DEVICE_RESET_OR_REMOVAL = 10,
    /* HTS1 session created, attached to, or drained. §12.3: events 11-12
     * "never serialize HQA1's capability or a host/KMT/resource handle". */
    HELIOS_ETW_EVENT_TRANSLATION_SESSION_CREATE_ATTACH_OR_DRAIN = 11,
    /* Progress of a translated synchronous operation. Same rule as ID 11. */
    HELIOS_ETW_EVENT_TRANSLATION_SYNCHRONOUS_PROGRESS = 12
} HeliosEtwEventId;

HELIOS_DIAG_STATIC_ASSERT(
    HELIOS_ETW_EVENT_TRANSLATION_SYNCHRONOUS_PROGRESS == HELIOS_ETW_EVENT_ID_MAX,
    "the last event ID is the maximum");
HELIOS_DIAG_STATIC_ASSERT(HELIOS_ETW_EVENT_ID_MAX == HELIOS_ETW_EVENT_COUNT,
                          "IDs are 1..COUNT with no gaps");

/* ── Event descriptor (mirrors `EVENT_DESCRIPTOR`, wdm.h) ─────────────────── */

typedef struct helios_etw_event_descriptor {
    uint16_t id;      /* HeliosEtwEventId */
    uint8_t version;  /* HELIOS_ETW_EVENT_VERSION */
    uint8_t channel;  /* HELIOS_ETW_CHANNEL_NONE */
    uint8_t level;    /* HELIOS_ETW_EVENT_LEVEL */
    uint8_t opcode;   /* HELIOS_ETW_OPCODE_INFO */
    uint16_t task;    /* HELIOS_ETW_TASK_NONE */
    uint64_t keyword; /* the event's single class keyword */
} HeliosEtwEventDescriptor;

HELIOS_DIAG_STATIC_ASSERT(sizeof(HeliosEtwEventDescriptor) == 16,
                          "EVENT_DESCRIPTOR is 16 bytes");
HELIOS_DIAG_STATIC_ASSERT(HELIOS_DIAG_ALIGNOF(HeliosEtwEventDescriptor) == 8,
                          "EVENT_DESCRIPTOR is 8-byte aligned");
HELIOS_DIAG_STATIC_ASSERT(offsetof(HeliosEtwEventDescriptor, id) == 0,
                          "descriptor.id@0");
HELIOS_DIAG_STATIC_ASSERT(offsetof(HeliosEtwEventDescriptor, version) == 2,
                          "descriptor.version@2");
HELIOS_DIAG_STATIC_ASSERT(offsetof(HeliosEtwEventDescriptor, channel) == 3,
                          "descriptor.channel@3");
HELIOS_DIAG_STATIC_ASSERT(offsetof(HeliosEtwEventDescriptor, level) == 4,
                          "descriptor.level@4");
HELIOS_DIAG_STATIC_ASSERT(offsetof(HeliosEtwEventDescriptor, opcode) == 5,
                          "descriptor.opcode@5");
HELIOS_DIAG_STATIC_ASSERT(offsetof(HeliosEtwEventDescriptor, task) == 6,
                          "descriptor.task@6");
HELIOS_DIAG_STATIC_ASSERT(offsetof(HeliosEtwEventDescriptor, keyword) == 8,
                          "descriptor.keyword@8");

/* ── Payload flags (§12.3 row 44) ──────────────────────────────────────────
 *
 * §12.3 delegates flag meaning to the ABI file without enumerating bits and
 * requires that "unknown flags are zero on emission". This revision therefore
 * defines EXACTLY ONE field — a two-bit sub-kind naming which alternative of
 * the event's own name occurred — and reserves everything else as zero.
 */
#define HELIOS_ETW_FLAG_SUBKIND_SHIFT 0u
#define HELIOS_ETW_FLAG_SUBKIND_MASK 0x00000003u
#define HELIOS_ETW_FLAGS_RESERVED_MASK ((uint32_t)~HELIOS_ETW_FLAG_SUBKIND_MASK)

HELIOS_DIAG_STATIC_ASSERT(HELIOS_ETW_FLAG_SUBKIND_SHIFT == 0u,
                          "the sub-kind occupies the low bits");
HELIOS_DIAG_STATIC_ASSERT(HELIOS_ETW_FLAGS_RESERVED_MASK ==
                              (uint32_t)0xfffffffcu,
                          "everything above the sub-kind is reserved zero");

/* Events whose name enumerates exactly one transition. Only legal value: 0. */
#define HELIOS_ETW_SUBKIND_SOLE 0u

/* BatchCancelOrFault */
#define HELIOS_ETW_SUBKIND_BATCH_CANCEL 0u
#define HELIOS_ETW_SUBKIND_BATCH_FAULT 1u
/* NativeFenceCreateOrOpen */
#define HELIOS_ETW_SUBKIND_NATIVE_FENCE_CREATE 0u
#define HELIOS_ETW_SUBKIND_NATIVE_FENCE_OPEN 1u
/* NativeFenceWaitOrSignal */
#define HELIOS_ETW_SUBKIND_NATIVE_FENCE_WAIT 0u
#define HELIOS_ETW_SUBKIND_NATIVE_FENCE_SIGNAL 1u
/* NativeFenceCloseOrReset */
#define HELIOS_ETW_SUBKIND_NATIVE_FENCE_CLOSE 0u
#define HELIOS_ETW_SUBKIND_NATIVE_FENCE_RESET 1u
/* PlaneLatchOrCancel */
#define HELIOS_ETW_SUBKIND_PLANE_LATCH 0u
#define HELIOS_ETW_SUBKIND_PLANE_CANCEL 1u
/* PlaneReaderReleaseOrUnbind */
#define HELIOS_ETW_SUBKIND_PLANE_READER_RELEASE 0u
#define HELIOS_ETW_SUBKIND_PLANE_UNBIND 1u
/* DeviceResetOrRemoval */
#define HELIOS_ETW_SUBKIND_DEVICE_RESET 0u
#define HELIOS_ETW_SUBKIND_DEVICE_REMOVAL 1u
/* TranslationSessionCreateAttachOrDrain */
#define HELIOS_ETW_SUBKIND_TRANSLATION_SESSION_CREATE 0u
#define HELIOS_ETW_SUBKIND_TRANSLATION_SESSION_ATTACH 1u
#define HELIOS_ETW_SUBKIND_TRANSLATION_SESSION_DRAIN 2u

HELIOS_DIAG_STATIC_ASSERT(HELIOS_ETW_SUBKIND_TRANSLATION_SESSION_DRAIN <=
                              HELIOS_ETW_FLAG_SUBKIND_MASK,
                          "the widest event's sub-kinds fit the field");

/* ── Payload sentinels (§12.3 rows 48 and 52) ─────────────────────────────── */

/*
 * `D3DDDI_ID_UNINITIALIZED` ((UINT)(~0), WDK d3dukmdt.h:1111) — the value
 * `vidpn_source` carries when the event has no VidPn source.
 */
#define HELIOS_ETW_VIDPN_SOURCE_UNINITIALIZED 0xffffffffu
/* UINT32_MAX — `node_or_plane` when the event has no node or plane index. */
#define HELIOS_ETW_NODE_OR_PLANE_NONE 0xffffffffu
/*
 * The largest legal node/plane index in this generation. §12.3 says "exact
 * bounded node/plane index" without stating the bound; the bound is the
 * selected profile's, and it is 0 on both axes — §10.8 fixes MaxPlanes=1 /
 * LayerIndex=0 for display and §10.8/§17.6 fix the one-node, one-engine
 * adapter for submission.
 */
#define HELIOS_ETW_MAX_NODE_OR_PLANE_INDEX 0u

/* ── The 72-byte payload (§12.3) ───────────────────────────────────────────
 *
 * | Offset | Size | Field | Rule |
 * |---:|---:|---|---|
 * | 0 | 8 | package generation | exact atomic-package generation |
 * | 8 | 8 | adapter generation | diagnostic generation, never an object lookup key |
 * | 16 | 8 | object/allocation generation | zero when not applicable; never a handle/token |
 * | 24 | 8 | context/plane generation | zero when not applicable; never a handle/token |
 * | 32 | 8 | sequence/value | submission fence, native value, or binding sequence by event ID |
 * | 40 | 4 | NTSTATUS/result | exact result, zero for an event without one |
 * | 44 | 4 | flags | event-ID-specific, with reserved bits zero |
 * | 48 | 4 | VidPn source | exact OS value or D3DDDI_ID_UNINITIALIZED |
 * | 52 | 4 | node/plane | exact bounded node/plane index or UINT32_MAX |
 * | 56 | 8 | auxiliary value 0 | documented per event ID |
 * | 64 | 8 | auxiliary value 1 | documented per event ID |
 */
#define HELIOS_ETW_PAYLOAD_V1_SIZE 72u

typedef struct helios_graphics_etw_payload_v1 {
    /* Exact atomic package generation of the emitting build. */
    uint64_t package_generation;
    /* Adapter diagnostic generation. NEVER an object lookup key. */
    uint64_t adapter_generation;
    /* Object/allocation generation, or 0 when not applicable. Never a
     * handle/token. */
    uint64_t object_generation;
    /* Context/plane generation, or 0 when not applicable. Never a
     * handle/token. */
    uint64_t context_generation;
    /* Submission fence, native monitored value, or binding sequence, selected
     * by event ID. A value, never a reference. */
    uint64_t sequence_value;
    /* Exact NTSTATUS (signed, like LONG), or 0 for an event with no result. */
    int32_t status;
    /* HELIOS_ETW_FLAG_SUBKIND_MASK holds the sub-kind; every reserved bit is
     * zero. */
    uint32_t flags;
    /* Exact OS VidPn source id, or HELIOS_ETW_VIDPN_SOURCE_UNINITIALIZED. */
    uint32_t vidpn_source;
    /* Exact bounded node/plane index, or HELIOS_ETW_NODE_OR_PLANE_NONE. */
    uint32_t node_or_plane;
    /*
     * Auxiliary value 0. §12.3 says it is "documented per event ID" and
     * delegates that documentation to the ABI file; THIS REVISION DOCUMENTS NO
     * INTERPRETATION FOR ANY EVENT ID, so it is reserved-zero on emission and
     * a decoder rejects a nonzero value. Giving it a meaning is an edit to
     * protocol/src/diagnostics.rs plus a HELIOS_ETW_EVENT_VERSION bump.
     */
    uint64_t aux0;
    /* Auxiliary value 1. Same reserved-zero rule as aux0. */
    uint64_t aux1;
} HeliosGraphicsEtwPayloadV1;

HELIOS_DIAG_STATIC_ASSERT(sizeof(HeliosGraphicsEtwPayloadV1) ==
                              HELIOS_ETW_PAYLOAD_V1_SIZE,
                          "the payload is 72 bytes");
HELIOS_DIAG_STATIC_ASSERT(sizeof(HeliosGraphicsEtwPayloadV1) == 72,
                          "the payload is 72 bytes");
HELIOS_DIAG_STATIC_ASSERT(HELIOS_DIAG_ALIGNOF(HeliosGraphicsEtwPayloadV1) == 8,
                          "the payload is align-8");
HELIOS_DIAG_STATIC_ASSERT(
    offsetof(HeliosGraphicsEtwPayloadV1, package_generation) == 0,
    "payload.package_generation@0");
HELIOS_DIAG_STATIC_ASSERT(
    offsetof(HeliosGraphicsEtwPayloadV1, adapter_generation) == 8,
    "payload.adapter_generation@8");
HELIOS_DIAG_STATIC_ASSERT(
    offsetof(HeliosGraphicsEtwPayloadV1, object_generation) == 16,
    "payload.object_generation@16");
HELIOS_DIAG_STATIC_ASSERT(
    offsetof(HeliosGraphicsEtwPayloadV1, context_generation) == 24,
    "payload.context_generation@24");
HELIOS_DIAG_STATIC_ASSERT(
    offsetof(HeliosGraphicsEtwPayloadV1, sequence_value) == 32,
    "payload.sequence_value@32");
HELIOS_DIAG_STATIC_ASSERT(offsetof(HeliosGraphicsEtwPayloadV1, status) == 40,
                          "payload.status@40");
HELIOS_DIAG_STATIC_ASSERT(offsetof(HeliosGraphicsEtwPayloadV1, flags) == 44,
                          "payload.flags@44");
HELIOS_DIAG_STATIC_ASSERT(
    offsetof(HeliosGraphicsEtwPayloadV1, vidpn_source) == 48,
    "payload.vidpn_source@48");
HELIOS_DIAG_STATIC_ASSERT(
    offsetof(HeliosGraphicsEtwPayloadV1, node_or_plane) == 52,
    "payload.node_or_plane@52");
HELIOS_DIAG_STATIC_ASSERT(offsetof(HeliosGraphicsEtwPayloadV1, aux0) == 56,
                          "payload.aux0@56");
HELIOS_DIAG_STATIC_ASSERT(offsetof(HeliosGraphicsEtwPayloadV1, aux1) == 64,
                          "payload.aux1@64");
/*
 * The 72 bytes are EXACTLY five u64 generations/values, four 32-bit scalars,
 * and two reserved u64. There is no room for a handle, pointer, backing token,
 * or raw `resid`, and widening any field to make room breaks this assertion.
 */
HELIOS_DIAG_STATIC_ASSERT(sizeof(HeliosGraphicsEtwPayloadV1) ==
                              5 * sizeof(uint64_t) + 4 * sizeof(uint32_t) +
                                  2 * sizeof(uint64_t),
                          "no padding and no hidden identity field");

/* ── Rejection reasons ─────────────────────────────────────────────────────
 *
 * Mirrors Rust `HeliosEtwReject::code()`. Every refusal is named so it can be
 * counted and printed; there is no anonymous failure in this schema.
 *
 * ⚠ A rejection is a decoding/self-check verdict about diagnostic bytes. §12.3
 * forbids it from gating a present, a fence, a lifetime, or package admission.
 */
typedef enum helios_etw_reject {
    HELIOS_ETW_REJECT_NONE = 0,
    HELIOS_ETW_REJECT_UNKNOWN_EVENT_ID = 1,
    HELIOS_ETW_REJECT_DESCRIPTOR_VERSION_MISMATCH = 2,
    HELIOS_ETW_REJECT_DESCRIPTOR_CHANNEL_MISMATCH = 3,
    HELIOS_ETW_REJECT_DESCRIPTOR_LEVEL_MISMATCH = 4,
    HELIOS_ETW_REJECT_DESCRIPTOR_OPCODE_MISMATCH = 5,
    HELIOS_ETW_REJECT_DESCRIPTOR_TASK_MISMATCH = 6,
    HELIOS_ETW_REJECT_DESCRIPTOR_KEYWORD_MISMATCH = 7,
    HELIOS_ETW_REJECT_PAYLOAD_LENGTH_MISMATCH = 8,
    HELIOS_ETW_REJECT_PAYLOAD_NOT_READABLE = 9,
    HELIOS_ETW_REJECT_PACKAGE_GENERATION_ZERO = 10,
    HELIOS_ETW_REJECT_PACKAGE_GENERATION_MISMATCH = 11,
    HELIOS_ETW_REJECT_EXPECTED_PACKAGE_GENERATION_ZERO = 12,
    HELIOS_ETW_REJECT_ADAPTER_GENERATION_ZERO = 13,
    HELIOS_ETW_REJECT_FLAGS_RESERVED_BITS_SET = 14,
    HELIOS_ETW_REJECT_FLAGS_SUBKIND_OUT_OF_RANGE = 15,
    HELIOS_ETW_REJECT_NODE_OR_PLANE_OUT_OF_RANGE = 16,
    HELIOS_ETW_REJECT_AUXILIARY_VALUE_0_NOT_ZERO = 17,
    HELIOS_ETW_REJECT_AUXILIARY_VALUE_1_NOT_ZERO = 18,
    HELIOS_ETW_REJECT_CONTROL_ETW_LOGGING_FLAGS_NOT_ZERO = 19
} HeliosEtwReject;

#define HELIOS_ETW_REJECT_CODE_MAX 19u

HELIOS_DIAG_STATIC_ASSERT(
    HELIOS_ETW_REJECT_CONTROL_ETW_LOGGING_FLAGS_NOT_ZERO ==
        (int)HELIOS_ETW_REJECT_CODE_MAX,
    "reject codes are 1..MAX with no gaps");

/* ── Per-event schema queries (mirror of the Rust accessors) ─────────────── */

/*
 * The event's class keyword (§12.3 five-class assignment). Returns 0 for an
 * unknown ID — an unknown event is dropped, never guessed at.
 */
static inline uint64_t helios_etw_event_keyword(HeliosEtwEventId event)
{
    switch (event) {
    case HELIOS_ETW_EVENT_BATCH_SUBMIT:
    case HELIOS_ETW_EVENT_BATCH_COMPLETE:
    case HELIOS_ETW_EVENT_BATCH_CANCEL_OR_FAULT:
        return HELIOS_ETW_KEYWORD_SUBMISSION;
    case HELIOS_ETW_EVENT_NATIVE_FENCE_CREATE_OR_OPEN:
    case HELIOS_ETW_EVENT_NATIVE_FENCE_WAIT_OR_SIGNAL:
    case HELIOS_ETW_EVENT_NATIVE_FENCE_CLOSE_OR_RESET:
        return HELIOS_ETW_KEYWORD_NATIVE_SYNCHRONIZATION;
    case HELIOS_ETW_EVENT_PLANE_CANDIDATE:
    case HELIOS_ETW_EVENT_PLANE_LATCH_OR_CANCEL:
    case HELIOS_ETW_EVENT_PLANE_READER_RELEASE_OR_UNBIND:
        return HELIOS_ETW_KEYWORD_DISPLAY_LIFETIME;
    case HELIOS_ETW_EVENT_DEVICE_RESET_OR_REMOVAL:
        return HELIOS_ETW_KEYWORD_DEVICE_LIFECYCLE;
    case HELIOS_ETW_EVENT_TRANSLATION_SESSION_CREATE_ATTACH_OR_DRAIN:
    case HELIOS_ETW_EVENT_TRANSLATION_SYNCHRONOUS_PROGRESS:
        return HELIOS_ETW_KEYWORD_TRANSLATION_SESSION_LIFETIME;
    }
    return 0;
}

/*
 * How many sub-kinds this event's own name enumerates: 1 for a single
 * transition, 2 for an `XOrY` pair, 3 for the translation-session event. Legal
 * sub-kind values are 0..count. Returns 0 for an unknown ID, which makes every
 * sub-kind of an unknown event illegal.
 */
static inline uint32_t helios_etw_event_subkind_count(HeliosEtwEventId event)
{
    switch (event) {
    case HELIOS_ETW_EVENT_BATCH_SUBMIT:
    case HELIOS_ETW_EVENT_BATCH_COMPLETE:
    case HELIOS_ETW_EVENT_PLANE_CANDIDATE:
    case HELIOS_ETW_EVENT_TRANSLATION_SYNCHRONOUS_PROGRESS:
        return 1u;
    case HELIOS_ETW_EVENT_BATCH_CANCEL_OR_FAULT:
    case HELIOS_ETW_EVENT_NATIVE_FENCE_CREATE_OR_OPEN:
    case HELIOS_ETW_EVENT_NATIVE_FENCE_WAIT_OR_SIGNAL:
    case HELIOS_ETW_EVENT_NATIVE_FENCE_CLOSE_OR_RESET:
    case HELIOS_ETW_EVENT_PLANE_LATCH_OR_CANCEL:
    case HELIOS_ETW_EVENT_PLANE_READER_RELEASE_OR_UNBIND:
    case HELIOS_ETW_EVENT_DEVICE_RESET_OR_REMOVAL:
        return 2u;
    case HELIOS_ETW_EVENT_TRANSLATION_SESSION_CREATE_ATTACH_OR_DRAIN:
        return 3u;
    }
    return 0;
}

/* The sub-kind encoded in a payload's flags word. */
static inline uint32_t helios_etw_payload_subkind(
    const HeliosGraphicsEtwPayloadV1 *payload)
{
    return (payload->flags & HELIOS_ETW_FLAG_SUBKIND_MASK) >>
           HELIOS_ETW_FLAG_SUBKIND_SHIFT;
}

/*
 * Structural validation of one payload against its event ID — the exact
 * mirror of Rust `validate_payload_v1`, including what it deliberately does
 * NOT check (object/context generation applicability, `status`,
 * `sequence_value`, and `vidpn_source`, none of which §12.3 makes checkable
 * per event). Returns HELIOS_ETW_REJECT_NONE on success.
 */
static inline HeliosEtwReject helios_etw_validate_payload_v1(
    HeliosEtwEventId event, const HeliosGraphicsEtwPayloadV1 *payload)
{
    if (payload->package_generation == 0) {
        return HELIOS_ETW_REJECT_PACKAGE_GENERATION_ZERO;
    }
    if (payload->adapter_generation == 0) {
        return HELIOS_ETW_REJECT_ADAPTER_GENERATION_ZERO;
    }
    if ((payload->flags & HELIOS_ETW_FLAGS_RESERVED_MASK) != 0) {
        return HELIOS_ETW_REJECT_FLAGS_RESERVED_BITS_SET;
    }
    if (helios_etw_payload_subkind(payload) >=
        helios_etw_event_subkind_count(event)) {
        return HELIOS_ETW_REJECT_FLAGS_SUBKIND_OUT_OF_RANGE;
    }
    if (payload->node_or_plane != HELIOS_ETW_NODE_OR_PLANE_NONE &&
        payload->node_or_plane > HELIOS_ETW_MAX_NODE_OR_PLANE_INDEX) {
        return HELIOS_ETW_REJECT_NODE_OR_PLANE_OUT_OF_RANGE;
    }
    if (payload->aux0 != 0) {
        return HELIOS_ETW_REJECT_AUXILIARY_VALUE_0_NOT_ZERO;
    }
    if (payload->aux1 != 0) {
        return HELIOS_ETW_REJECT_AUXILIARY_VALUE_1_NOT_ZERO;
    }
    return HELIOS_ETW_REJECT_NONE;
}

/*
 * Validate a captured descriptor against the fixed §12.3 table. Every field is
 * compared, not just the ID — §17.7's acceptance is "byte-exact decoding of
 * all section-12.3 IDs/flags/keywords". Mirrors Rust
 * `validate_descriptor_v1`; on success the event ID is stored through
 * `out_event` when that pointer is non-NULL.
 */
static inline HeliosEtwReject helios_etw_validate_descriptor_v1(
    const HeliosEtwEventDescriptor *descriptor, HeliosEtwEventId *out_event)
{
    HeliosEtwEventId event;

    if (descriptor->id < HELIOS_ETW_EVENT_ID_MIN ||
        descriptor->id > HELIOS_ETW_EVENT_ID_MAX) {
        return HELIOS_ETW_REJECT_UNKNOWN_EVENT_ID;
    }
    event = (HeliosEtwEventId)descriptor->id;
    if (descriptor->version != HELIOS_ETW_EVENT_VERSION) {
        return HELIOS_ETW_REJECT_DESCRIPTOR_VERSION_MISMATCH;
    }
    if (descriptor->channel != HELIOS_ETW_CHANNEL_NONE) {
        return HELIOS_ETW_REJECT_DESCRIPTOR_CHANNEL_MISMATCH;
    }
    if (descriptor->level != HELIOS_ETW_EVENT_LEVEL) {
        return HELIOS_ETW_REJECT_DESCRIPTOR_LEVEL_MISMATCH;
    }
    if (descriptor->opcode != HELIOS_ETW_OPCODE_INFO) {
        return HELIOS_ETW_REJECT_DESCRIPTOR_OPCODE_MISMATCH;
    }
    if (descriptor->task != HELIOS_ETW_TASK_NONE) {
        return HELIOS_ETW_REJECT_DESCRIPTOR_TASK_MISMATCH;
    }
    if (descriptor->keyword != helios_etw_event_keyword(event)) {
        return HELIOS_ETW_REJECT_DESCRIPTOR_KEYWORD_MISMATCH;
    }
    if (out_event != NULL) {
        *out_event = event;
    }
    return HELIOS_ETW_REJECT_NONE;
}

/*
 * Validate `DXGKARG_CONTROLETWLOGGING::Flags`. The parameter is 64-bit so a
 * nonzero high half can never be truncated away regardless of the WDK field's
 * width.
 */
static inline HeliosEtwReject helios_etw_validate_control_flags(uint64_t flags)
{
    if (flags != HELIOS_ETW_CONTROL_FLAGS_REQUIRED) {
        return HELIOS_ETW_REJECT_CONTROL_ETW_LOGGING_FLAGS_NOT_ZERO;
    }
    return HELIOS_ETW_REJECT_NONE;
}

#if defined(__cplusplus)
} /* extern "C" */
#endif

#endif /* HELIOS_DIAGNOSTICS_H */
