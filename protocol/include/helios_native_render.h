/*
 * helios_native_render.h — C mirror of the Helios native-Vulkan legacy-KMT
 * wire records: HVC1, HNR2 (+ its use and patch records), HVM1, HVR1.
 *
 * ⛔ SINGLE SOURCE OF TRUTH: protocol/src/native_render.rs.
 * This header is the hand-maintained C projection of that file for the Mesa
 * (`icd/mesa`) side. Every constant, struct, and assertion below exists in the
 * Rust file first; if the two ever disagree, the Rust file wins and this header
 * is the bug. Change one, change both in the same commit — the assertions here
 * are what turn a layout drift into a compile error rather than a live VM
 * mis-parsing a Render fragment or a reply slot.
 *
 * Normative: docs/HELIOS_PRESENT_SYNC_RETIREMENT.md §10.7 — HVC1 (the
 * create-context table), HNR2 (the fragment header table plus the 24-byte use
 * and 16-byte typed-patch records), HVM1 (the allocation create/open record),
 * and HVR1 (the bounded reply header and its continuation request). §17.1 is the
 * module mandate for the Rust side.
 *
 * WHY C NEEDS THIS AT ALL: §17.1 asks for generated C declarations only for
 * `physical_memory` (QEMU) and `diagnostics`, and for "both Rust **and C**
 * offsets" for `wddm`. It says nothing about HVC1/HNR2/HVM1/HVR1 — which only
 * Mesa and the KMD consume — and the ICD's answer today is
 * `icd/mesa/src/virtio/vulkan/vn_renderer_helios.c:255-352`: hand-declared
 * structs guarded by `_Static_assert(sizeof(...) == N)` only, i.e. SIZE and not
 * offsets, across two repositories, with nothing binding them to the Rust side.
 * That is the same uncheckable hand-mirror class CLAUDE.md flags for
 * `VKD3D_HEAP_FLAG_HELIOS_VENUS_EXPORT`. docs/retirement/lane-mesa.md
 * (ambiguity A1, CROSS-LANE REQUEST 1) makes this header the mandate: the ICD
 * declares no record of its own and asserts every OFFSET, not just each size.
 *
 * NO IDENTITY CROSSES THIS BOUNDARY: nothing here carries a pointer, a KMT/NT
 * handle, a PID, a host object ID, a name, or a lookup key. ⛔⛔ In particular no
 * virtio-gpu `resource_id` (`resid`) appears anywhere in this ABI. Mesa stores
 * the opaque local HVM1 `object_generation` and repeats it in the use record's
 * `expected_allocation_generation`; that is a staleness check against the KMD's
 * own allocation object and resolves to NOTHING on the host. The generated Venus
 * encoders write ZERO into every host-resource-id operand, a patch record names
 * that operand's position and width, and the KMD rewrites it at COMMIT. A
 * nonzero host-resource-id operand arriving from user mode is not "already
 * resolved" — it is a rejected batch.
 *
 * ⛔ WHAT THIS HEADER DELIBERATELY OMITS — and why the omission is not an
 * oversight to be "fixed" later: the Rust module's `kernel_dma` submodule.
 * `Hnr2PhysicalCapability` (48 bytes) carries a physical address, a segment ID
 * and an HPM1 placement epoch, and `Hnr2KmdDmaPrivateV1` (64 bytes) is the
 * `DmaBufferPrivateDataSize` record dxgkrnl keeps opaque. §17.1: "this record
 * exists only in scheduler DMA and is never returned to user mode." Declaring
 * either in a Mesa-facing header would publish the guest-physical layout of the
 * HLM1 BAR window to user mode. Whoever needs them is writing kernel code and
 * must use the Rust module. `HELIOS_HVC1_DMA_PRIVATE_DATA_BYTES` below IS
 * mirrored, because it is an advertised `DXGK_CONTEXTINFO` value the ICD checks;
 * the record it sizes is not.
 *
 * ⛔ WHAT THIS HEADER IS NOT: a validator. The rules — fragment sequencing, the
 * COMMIT table bounds and non-overlap, the unique-allocation closure, the
 * `WriteOperation` agreement, the zero-placeholder operand check, the role
 * placement/access/cache contract, and the HVR1 chunk/continuation state machine
 * — live in `protocol/src/native_render.rs` (`HeliosNativeRenderV2::validate`,
 * `validate_commit_tables`, `HeliosVenusMemoryAllocationV1::validate`,
 * `HeliosVenusReplyV1::validate`). A C implementation must reproduce them; the
 * comments on each declaration below name the rule so it cannot be reconstructed
 * from the field names alone.
 *
 * ⛔ TWO SPELLINGS OF "ENGINE AFFINITY" LIVE HERE ON PURPOSE.
 * `HELIOS_HVC1_ENGINE_AFFINITY` is the zero-based context ordinal (0) and
 * `HELIOS_HVC1_FENCE_ENGINE_AFFINITY` is a BIT MASK over the one exposed node
 * (1 << 0). They are adjacent because confusing them is silent; never propagate
 * one value to the other's call site (docs/retirement/lane-mesa.md ambiguity A9).
 */

#ifndef HELIOS_NATIVE_RENDER_H
#define HELIOS_NATIVE_RENDER_H

#include <stddef.h>
#include <stdint.h>

#if defined(__cplusplus)
extern "C" {
#endif

#if defined(__cplusplus)
#define HELIOS_NR_STATIC_ASSERT(cond, msg) static_assert(cond, msg)
#define HELIOS_NR_ALIGNOF(type) alignof(type)
#else
#define HELIOS_NR_STATIC_ASSERT(cond, msg) _Static_assert(cond, msg)
#define HELIOS_NR_ALIGNOF(type) _Alignof(type)
#endif

/* ── The atomic package generation (§17.1, final bullet) ───────────────────
 *
 * ⛔ SINGLE SOURCE OF TRUTH: `protocol/src/lib.rs`
 * (`HELIOS_PACKAGE_GENERATION`). This is a hand-maintained C mirror, and the
 * Rust side carries a test pinning the exact literal and naming this header.
 * Change one, change all.
 *
 * HVC1, HNR2, HVM1, and HVR1 each carry it in their `package_generation` field,
 * and a mismatch is fatal with no fallback and no wildcard: zero is never "any"
 * on either side (§10.2 admission table, §17.8 steps 5-6).
 *
 * Guarded so this header, helios_wddm.h, helios_translation_session.h and
 * helios_diagnostics.h can be included together.
 */
#ifndef HELIOS_PACKAGE_GENERATION
#define HELIOS_PACKAGE_GENERATION_TAG     0x48454C49u
#define HELIOS_PACKAGE_GENERATION_ORDINAL 1u
#define HELIOS_PACKAGE_GENERATION \
    ((((uint64_t)HELIOS_PACKAGE_GENERATION_TAG) << 32) | \
     (uint64_t)HELIOS_PACKAGE_GENERATION_ORDINAL)
#endif

HELIOS_NR_STATIC_ASSERT(HELIOS_PACKAGE_GENERATION == UINT64_C(0x48454C4900000001),
                        "package generation must equal protocol/src/lib.rs "
                        "HELIOS_PACKAGE_GENERATION");

/* ── Package-wide admission ────────────────────────────────────────────────*/

/*
 * The exact Venus capset ID every HVC1 context must name (§10.7: "capset |
 * exact Venus capset ID").
 *
 * ⛔ The Rust side ALIASES `crate::virtio_gpu::VIRTIO_GPU_CAPSET_VENUS` rather
 * than re-declaring the number, so this lane can never drift from the
 * virtio-gpu wire constant the KMD actually sends in `CTX_CREATE`. There is no
 * C mirror of `virtio_gpu.rs`, so this one literal cannot be an alias here; the
 * Rust cross-check test in `native_render.rs` pins it and names this header,
 * which is what makes the copy checkable.
 */
#define HELIOS_NATIVE_RENDER_CAPSET 4u

/*
 * The CRC64 that `fragment_crc64` / `full_payload_crc64` carry is CRC-64/ECMA-182
 * and is declared exactly ONCE in the C mirrors, in helios_wddm.h
 * (`HELIOS_CRC64_ECMA182_POLY` / `_INIT` / `_XOROUT` / `_CHECK`) — the same
 * single-declaration rule the Rust side applies. Include that header to compute
 * one. It is a CORRUPTION DIAGNOSTIC and never validation authority: no
 * validator in this ABI consults it.
 *
 * The WDDM memory-segment IDs (`HELIOS_SEGMENT_ID_SYSTEM` / `_APERTURE` /
 * `_HLM1`) are likewise declared once, in
 * qemu-helios/include/hw/virtio/helios_physical_memory.h, which owns the whole
 * segment/page-number interpretation. This lane only names segments; it does not
 * define them, and neither does this header.
 */

/* ------------------------------------------------------------------------ */
/* §10.7 — HVC1: the legacy KMT control/queue create-context record          */
/* ------------------------------------------------------------------------ */

/* 'HVC1' little-endian. */
#define HELIOS_HVC1_MAGIC       0x31435648u
#define HELIOS_HVC1_ABI_VERSION 1u
#define HELIOS_HVC1_SIZE        32u

/*
 * The only admitted `mode`: finite HNR2 over legacy KMT Render. There is no
 * other mode in this package generation, so any other value is a hard reject
 * rather than a fallback.
 */
#define HELIOS_HVC1_MODE_FINITE_HNR2_RENDER 2u

/*
 * BOTH `queue_family` and `queue_index` equal to this value mean the control
 * context. The ordinals are copied diagnostics and never identity — the KMD
 * binds the context object itself, not these numbers. Exactly one of the two
 * carrying it is a refusal (`Hvc1Reject::QueueOrdinalMixed`), never a guess in
 * either direction.
 */
#define HELIOS_HVC1_CONTROL_ORDINAL 0xFFFFFFFFu

/* `D3DKMTCreateContext::NodeOrdinal`: this generation exposes one render node. */
#define HELIOS_HVC1_NODE_ORDINAL 0u
/* `D3DKMTCreateContext::EngineAffinity` — the ZERO-BASED ordinal (§10.7). */
#define HELIOS_HVC1_ENGINE_AFFINITY 0u
/*
 * `EngineAffinity` for the context's own monitored progress fence
 * (`D3DKMTCreateSynchronizationObject2`), and the same value §12.2 requires of
 * `D3DKMTOpenNativeFenceFromNtHandle`. A BIT MASK over the one exposed node —
 * not the ordinal above.
 */
#define HELIOS_HVC1_FENCE_ENGINE_AFFINITY (1u << 0)

/* `DXGK_CONTEXTINFO` minima the ICD validates on the returned context. */
#define HELIOS_HVC1_DMA_BUFFER_BYTES        (256u * 1024u)
#define HELIOS_HVC1_ALLOCATION_LIST_ENTRIES 4096u
/*
 * `PatchLocationListSize`. Distinct from HELIOS_HNR2_MAX_PATCH_RECORDS: this is
 * the OS patch-location capacity the KMD WRITES (one entry per use record, so at
 * most HELIOS_HNR2_MAX_USE_RECORDS), while HNR2's 8192 typed patch records live
 * inside the copied command and are never WDDM patch locations.
 */
#define HELIOS_HVC1_PATCH_LOCATION_ENTRIES  4096u
/*
 * `DmaBufferPrivateDataSize`. The 64-byte record it sizes is KMD-only and is
 * NOT declared in this header — see the omission note at the top of the file.
 */
#define HELIOS_HVC1_DMA_PRIVATE_DATA_BYTES  64u
/*
 * `DmaBufferSegmentSet` — zero, the documented contiguous paged-locked
 * DMA-buffer case. HVC1 never selects the aperture segment here.
 */
#define HELIOS_HVC1_DMA_BUFFER_SEGMENT_SET  0u

/*
 * The control context's host ring index. Its terminal point is renderer command
 * processing and reply publication only; ⛔ ring 0 completion is NEVER a
 * GPU-complete fact (invariant 12).
 */
#define HELIOS_HVC1_CONTROL_RING_INDEX 0u

/*
 * The only `DXGK_CREATECONTEXTFLAGS::Value` an HVC1 context may carry. §10.7
 * states the rule as "`VirtualAddressing=0` and all other unsupported context
 * flags are zero", and this generation supports none of them, so the admitted
 * word is exactly zero.
 */
#define HELIOS_HVC1_CREATE_CONTEXT_FLAGS 0u

/*
 * Mirror of Rust `HeliosVulkanContextV1` — §10.7's 32-byte create-context table.
 *
 * One raw KMT device per `vn_instance` creates exactly one CONTROL context (host
 * ring 0, HTS1 session owner, restricted to the pure-control opcode class) and
 * one QUEUE context per real lower `VkQueue`, each bound to a unique nonzero,
 * non-recycled `INFO_RING_IDX`. Neither the host context nor the ring index is
 * ever returned to user mode, which is why no field here can name one.
 */
typedef struct HeliosVulkanContextV1 {
    uint32_t magic;              /* 0  == HELIOS_HVC1_MAGIC */
    uint16_t abi_version;        /* 4  == HELIOS_HVC1_ABI_VERSION */
    uint16_t struct_size;        /* 6  == HELIOS_HVC1_SIZE */
    uint64_t package_generation; /* 8  exact; zero is never a wildcard */
    uint32_t capset;             /* 16 == HELIOS_NATIVE_RENDER_CAPSET */
    uint32_t mode;               /* 20 == HELIOS_HVC1_MODE_FINITE_HNR2_RENDER */
    uint32_t queue_family;       /* 24 copied diagnostic ordinal; never identity */
    uint32_t queue_index;        /* 28 copied diagnostic ordinal; never identity */
} HeliosVulkanContextV1;

HELIOS_NR_STATIC_ASSERT(sizeof(HeliosVulkanContextV1) == 32,
                        "HVC1 must be the §10.7 32-byte create-context record");
HELIOS_NR_STATIC_ASSERT(sizeof(HeliosVulkanContextV1) == HELIOS_HVC1_SIZE,
                        "HVC1 size constant must match the struct");
HELIOS_NR_STATIC_ASSERT(HELIOS_NR_ALIGNOF(HeliosVulkanContextV1) == 8, "");
HELIOS_NR_STATIC_ASSERT(offsetof(HeliosVulkanContextV1, magic) == 0, "");
HELIOS_NR_STATIC_ASSERT(offsetof(HeliosVulkanContextV1, abi_version) == 4, "");
HELIOS_NR_STATIC_ASSERT(offsetof(HeliosVulkanContextV1, struct_size) == 6, "");
HELIOS_NR_STATIC_ASSERT(offsetof(HeliosVulkanContextV1, package_generation) == 8, "");
HELIOS_NR_STATIC_ASSERT(offsetof(HeliosVulkanContextV1, capset) == 16, "");
HELIOS_NR_STATIC_ASSERT(offsetof(HeliosVulkanContextV1, mode) == 20, "");
HELIOS_NR_STATIC_ASSERT(offsetof(HeliosVulkanContextV1, queue_family) == 24, "");
HELIOS_NR_STATIC_ASSERT(offsetof(HeliosVulkanContextV1, queue_index) == 28, "");

/* ------------------------------------------------------------------------ */
/* §10.7 — HNR2: the finite Venus stream fragmentation ABI                   */
/* ------------------------------------------------------------------------ */

/* 'HNR2' little-endian. */
#define HELIOS_HNR2_MAGIC       0x32524E48u
#define HELIOS_HNR2_ABI_VERSION 2u
#define HELIOS_HNR2_HEADER_SIZE 112u

/* 15 MiB reassembled payload, at most 64 consecutive Render fragments. */
#define HELIOS_HNR2_MAX_PAYLOAD_BYTES UINT64_C(15728640)
#define HELIOS_HNR2_MAX_FRAGMENTS     64u

/*
 * Maximum COMMIT use records — also the maximum `AllocationCount` and the
 * maximum number of output `D3DDDI_PATCHLOCATIONLIST` entries (one per use).
 */
#define HELIOS_HNR2_MAX_USE_RECORDS   4096u
/* Maximum COMMIT typed-patch records — exact parsed resource operands. */
#define HELIOS_HNR2_MAX_PATCH_RECORDS 8192u
#define HELIOS_HNR2_USE_RECORD_SIZE   24u
#define HELIOS_HNR2_PATCH_RECORD_SIZE 16u

/*
 * KMD staging-pool caps per context: "capped at 64 outstanding submissions and
 * 15 MiB total". Exhaustion returns resource failure and NEVER waits, scans
 * another context, or spills to a global queue. The doc gives both numbers and
 * no arithmetic relating them, so both are enforced as written.
 */
#define HELIOS_HNR2_MAX_OUTSTANDING_SUBMISSIONS 64u
#define HELIOS_HNR2_SLOT_POOL_BYTES             UINT64_C(15728640)

/* Fragment flags — "only BEGIN=1, COMMIT=2, HAS_REPLY=4". */
#define HELIOS_HNR2_FLAG_BEGIN     1u
#define HELIOS_HNR2_FLAG_COMMIT    2u
#define HELIOS_HNR2_FLAG_HAS_REPLY 4u
#define HELIOS_HNR2_FLAG_MASK      7u

/* `reply_allocation_list_index` when the batch requests no reply. */
#define HELIOS_HNR2_NO_REPLY_ALLOCATION_INDEX 0xFFFFFFFFu
/*
 * Required alignment of `reply_offset` — HVR1's own 8-byte alignment, which is
 * what "exact aligned range" has to mean for a header containing `uint64_t`s.
 */
#define HELIOS_HNR2_REPLY_OFFSET_ALIGN UINT64_C(8)

/*
 * Use-record access bits. "Only READ=1 and WRITE=2 exist; unknown bits are
 * zero." WRITE must equal the `WriteOperation` bit of the matching
 * `D3DDDI_ALLOCATIONLIST` entry.
 */
#define HELIOS_HNR2_ACCESS_READ  1u
#define HELIOS_HNR2_ACCESS_WRITE 2u
#define HELIOS_HNR2_ACCESS_MASK  3u

/*
 * Patch-record operand kinds. INVALID exists so a ZEROED record is a reject
 * rather than an accidentally meaningful one. Both kinds name a generated
 * host-resource-id operand the encoder wrote as ZERO; the KMD rewrites it to a
 * DMA-local capability ordinal at COMMIT.
 */
#define HELIOS_HNR2_OPERAND_KIND_INVALID           0u
#define HELIOS_HNR2_OPERAND_KIND_HOST_RESOURCE_ID32 1u
#define HELIOS_HNR2_OPERAND_KIND_HOST_RESOURCE_ID64 2u
#define HELIOS_HNR2_OPERAND_WIDTH_32 4u
#define HELIOS_HNR2_OPERAND_WIDTH_64 8u
/*
 * Required alignment of a patch record's `payload_offset`: the Venus command
 * stream is encoded in 4-byte units, so every generated operand — of either
 * width — starts 4-byte aligned. "Arbitrary byte patching is rejected."
 */
#define HELIOS_HNR2_OPERAND_ALIGN 4u

/*
 * Mirror of Rust `HeliosNativeRenderV2` — §10.7's 112-byte fragment header.
 *
 * One batch is at most HELIOS_HNR2_MAX_PAYLOAD_BYTES split into at most
 * HELIOS_HNR2_MAX_FRAGMENTS consecutive Render calls; the first carries BEGIN,
 * the last COMMIT, and a one-fragment batch carries both. There is exactly one
 * incomplete batch per context, so no fragment lookup and no cross-context
 * assembler exists.
 *
 * ⛔ CANONICAL COMMAND-BUFFER LAYOUT. §10.7 gives explicit offsets for the use
 * and patch tables but never states where the fragment payload starts WITHIN the
 * command buffer — only that it "fits this returned command buffer after
 * metadata" and that the encoder must reserve the COMMIT metadata before
 * choosing every fragment payload length. The ABI therefore fixes ONE layout,
 * computed by Rust `Hnr2CommandLayout` and enforced in both directions, and a C
 * encoder must reproduce exactly it:
 *
 *   +0                                   HeliosNativeRenderV2   (112 bytes)
 *   +112                                 use records            (COMMIT only)
 *   +112 + uses*24                       patch records          (COMMIT only)
 *   +112 + uses*24 + patches*16          fragment payload
 *
 * Both table strides are multiples of 8, so the payload is always 8-aligned with
 * no padding and the regions cannot overlap by construction. An empty table has
 * offset ZERO — the header rule for "before COMMIT" — never a degenerate pointer
 * into the middle of the buffer.
 */
typedef struct HeliosNativeRenderV2 {
    uint32_t magic;                       /* 0   == HELIOS_HNR2_MAGIC */
    uint16_t abi_version;                 /* 4   == HELIOS_HNR2_ABI_VERSION */
    uint16_t header_size;                 /* 6   == HELIOS_HNR2_HEADER_SIZE */
    uint64_t package_generation;          /* 8   exact HVC1/package generation */
    uint64_t batch_token;                 /* 16  nonzero, strictly increasing on
                                           *     THIS context */
    uint64_t total_payload_bytes;         /* 24  nonzero, <= MAX_PAYLOAD_BYTES,
                                           *     constant across the batch */
    uint64_t fragment_payload_offset;     /* 32  within the REASSEMBLED payload:
                                           *     exactly prior offset + length,
                                           *     first is zero. NOT a
                                           *     command-buffer offset */
    uint32_t fragment_payload_bytes;      /* 40  fits the returned command buffer
                                           *     after the metadata */
    uint16_t fragment_index;              /* 44  zero based, exactly the expected
                                           *     next index */
    uint16_t fragment_count;              /* 46  1..=64, constant for the batch */
    uint32_t use_record_offset;           /* 48  zero before COMMIT */
    uint32_t use_record_count;            /* 52  zero before COMMIT; equals the
                                           *     COMMIT AllocationCount */
    uint32_t patch_record_offset;         /* 56  zero before COMMIT */
    uint32_t patch_record_count;          /* 60  zero before COMMIT */
    uint32_t reply_allocation_list_index; /* 64  NO_REPLY_ALLOCATION_INDEX, or a
                                           *     valid WRITABLE index */
    uint32_t flags;                       /* 68  HELIOS_HNR2_FLAG_* only */
    uint64_t reply_offset;                /* 72  zero without a reply; an exact
                                           *     aligned range in one slot */
    uint64_t reply_capacity_bytes;        /* 80  zero without a reply; exactly
                                           *     HVR1 header + maxChunkBytes */
    uint64_t fragment_crc64;              /* 88  corruption diagnostic ONLY */
    uint64_t full_payload_crc64;          /* 96  zero before COMMIT; diagnostic */
    uint64_t reply_slot_generation;       /* 104 zero without a reply; the exact
                                           *     nonzero checked-out slot
                                           *     generation with HAS_REPLY */
} HeliosNativeRenderV2;

HELIOS_NR_STATIC_ASSERT(sizeof(HeliosNativeRenderV2) == 112,
                        "HNR2 must be the §10.7 112-byte fragment header");
HELIOS_NR_STATIC_ASSERT(sizeof(HeliosNativeRenderV2) == HELIOS_HNR2_HEADER_SIZE,
                        "HNR2 header size constant must match the struct");
HELIOS_NR_STATIC_ASSERT(HELIOS_NR_ALIGNOF(HeliosNativeRenderV2) == 8, "");
HELIOS_NR_STATIC_ASSERT(offsetof(HeliosNativeRenderV2, magic) == 0, "");
HELIOS_NR_STATIC_ASSERT(offsetof(HeliosNativeRenderV2, abi_version) == 4, "");
HELIOS_NR_STATIC_ASSERT(offsetof(HeliosNativeRenderV2, header_size) == 6, "");
HELIOS_NR_STATIC_ASSERT(offsetof(HeliosNativeRenderV2, package_generation) == 8, "");
HELIOS_NR_STATIC_ASSERT(offsetof(HeliosNativeRenderV2, batch_token) == 16, "");
HELIOS_NR_STATIC_ASSERT(offsetof(HeliosNativeRenderV2, total_payload_bytes) == 24, "");
HELIOS_NR_STATIC_ASSERT(offsetof(HeliosNativeRenderV2, fragment_payload_offset) == 32, "");
HELIOS_NR_STATIC_ASSERT(offsetof(HeliosNativeRenderV2, fragment_payload_bytes) == 40, "");
HELIOS_NR_STATIC_ASSERT(offsetof(HeliosNativeRenderV2, fragment_index) == 44, "");
HELIOS_NR_STATIC_ASSERT(offsetof(HeliosNativeRenderV2, fragment_count) == 46, "");
HELIOS_NR_STATIC_ASSERT(offsetof(HeliosNativeRenderV2, use_record_offset) == 48, "");
HELIOS_NR_STATIC_ASSERT(offsetof(HeliosNativeRenderV2, use_record_count) == 52, "");
HELIOS_NR_STATIC_ASSERT(offsetof(HeliosNativeRenderV2, patch_record_offset) == 56, "");
HELIOS_NR_STATIC_ASSERT(offsetof(HeliosNativeRenderV2, patch_record_count) == 60, "");
HELIOS_NR_STATIC_ASSERT(offsetof(HeliosNativeRenderV2, reply_allocation_list_index) == 64, "");
HELIOS_NR_STATIC_ASSERT(offsetof(HeliosNativeRenderV2, flags) == 68, "");
HELIOS_NR_STATIC_ASSERT(offsetof(HeliosNativeRenderV2, reply_offset) == 72, "");
HELIOS_NR_STATIC_ASSERT(offsetof(HeliosNativeRenderV2, reply_capacity_bytes) == 80, "");
HELIOS_NR_STATIC_ASSERT(offsetof(HeliosNativeRenderV2, fragment_crc64) == 88, "");
HELIOS_NR_STATIC_ASSERT(offsetof(HeliosNativeRenderV2, full_payload_crc64) == 96, "");
HELIOS_NR_STATIC_ASSERT(offsetof(HeliosNativeRenderV2, reply_slot_generation) == 104, "");

/*
 * Mirror of Rust `HeliosNativeRenderUse` — §10.7's 24-byte COMMIT use record.
 *
 * ⛔ Every directly or transitively reachable allocation in the legacy batch
 * appears EXACTLY ONCE here and exactly once in the returned
 * `D3DDDI_ALLOCATIONLIST` with the same `WriteOperation`. Descriptor/object
 * reachability is expanded into that exact closure before COMMIT.
 */
typedef struct HeliosNativeRenderUse {
    uint32_t allocation_list_index;         /* 0  index into the COMMIT
                                             *    D3DDDI_ALLOCATIONLIST; not a
                                             *    handle, not an ID */
    uint32_t access_flags;                  /* 4  HELIOS_HNR2_ACCESS_* only */
    uint64_t expected_allocation_generation; /* 8 the opaque local HVM1
                                             *    object_generation Mesa stored:
                                             *    a staleness check, NOT a
                                             *    renderer identity */
    uint32_t first_patch;                   /* 16 */
    uint32_t patch_count;                   /* 20 */
} HeliosNativeRenderUse;

HELIOS_NR_STATIC_ASSERT(sizeof(HeliosNativeRenderUse) == 24,
                        "the HNR2 use record must be the §10.7 24-byte record");
HELIOS_NR_STATIC_ASSERT(sizeof(HeliosNativeRenderUse) == HELIOS_HNR2_USE_RECORD_SIZE,
                        "use-record size constant must match the struct");
HELIOS_NR_STATIC_ASSERT(HELIOS_NR_ALIGNOF(HeliosNativeRenderUse) == 8, "");
HELIOS_NR_STATIC_ASSERT(offsetof(HeliosNativeRenderUse, allocation_list_index) == 0, "");
HELIOS_NR_STATIC_ASSERT(offsetof(HeliosNativeRenderUse, access_flags) == 4, "");
HELIOS_NR_STATIC_ASSERT(offsetof(HeliosNativeRenderUse, expected_allocation_generation) == 8, "");
HELIOS_NR_STATIC_ASSERT(offsetof(HeliosNativeRenderUse, first_patch) == 16, "");
HELIOS_NR_STATIC_ASSERT(offsetof(HeliosNativeRenderUse, patch_count) == 20, "");

/*
 * Mirror of Rust `HeliosNativeRenderPatch` — §10.7's 16-byte typed-patch record.
 *
 * It names the byte position and encoded width of ONE generated host-resource-id
 * operand whose payload bytes the encoder wrote as ZERO. Arbitrary byte patching
 * is rejected: the offset/kind/width must identify such an operand in the
 * generated, fully parsed opcode schema, and `allocation_list_index` must equal
 * the owning use record's.
 */
typedef struct HeliosNativeRenderPatch {
    uint32_t payload_offset;        /* 0  within the REASSEMBLED Venus payload */
    uint32_t allocation_list_index; /* 4  == the owning use's index */
    uint16_t operand_kind;          /* 8  HELIOS_HNR2_OPERAND_KIND_* */
    uint16_t encoded_width;         /* 10 must match operand_kind */
    uint32_t reserved;              /* 12 zero */
} HeliosNativeRenderPatch;

HELIOS_NR_STATIC_ASSERT(sizeof(HeliosNativeRenderPatch) == 16,
                        "the HNR2 patch record must be the §10.7 16-byte record");
HELIOS_NR_STATIC_ASSERT(sizeof(HeliosNativeRenderPatch) == HELIOS_HNR2_PATCH_RECORD_SIZE,
                        "patch-record size constant must match the struct");
HELIOS_NR_STATIC_ASSERT(HELIOS_NR_ALIGNOF(HeliosNativeRenderPatch) == 4, "");
HELIOS_NR_STATIC_ASSERT(offsetof(HeliosNativeRenderPatch, payload_offset) == 0, "");
HELIOS_NR_STATIC_ASSERT(offsetof(HeliosNativeRenderPatch, allocation_list_index) == 4, "");
HELIOS_NR_STATIC_ASSERT(offsetof(HeliosNativeRenderPatch, operand_kind) == 8, "");
HELIOS_NR_STATIC_ASSERT(offsetof(HeliosNativeRenderPatch, encoded_width) == 10, "");
HELIOS_NR_STATIC_ASSERT(offsetof(HeliosNativeRenderPatch, reserved) == 12, "");

/* §10.7's own worst-case arithmetic, pinned so a bound cannot be changed here
 * without the aggregate being rechecked:
 *   15*1,048,576 + 64*112 + 4096*24 + 8192*16 = 15,965,184
 * below the 64*262,144 = 16,777,216-byte aggregate command-buffer capacity. */
HELIOS_NR_STATIC_ASSERT(HELIOS_HNR2_MAX_PAYLOAD_BYTES +
                                (uint64_t)HELIOS_HNR2_MAX_FRAGMENTS *
                                    (uint64_t)HELIOS_HNR2_HEADER_SIZE +
                                (uint64_t)HELIOS_HNR2_MAX_USE_RECORDS *
                                    (uint64_t)HELIOS_HNR2_USE_RECORD_SIZE +
                                (uint64_t)HELIOS_HNR2_MAX_PATCH_RECORDS *
                                    (uint64_t)HELIOS_HNR2_PATCH_RECORD_SIZE ==
                            UINT64_C(15965184),
                        "the §10.7 worst-case aggregate must stay 15,965,184 bytes");
HELIOS_NR_STATIC_ASSERT(UINT64_C(15965184) < (uint64_t)HELIOS_HNR2_MAX_FRAGMENTS *
                                                 (uint64_t)HELIOS_HVC1_DMA_BUFFER_BYTES,
                        "the worst case must fit the aggregate command-buffer capacity");
/* One output patch location per use record, and the context advertises exactly
 * that many. */
HELIOS_NR_STATIC_ASSERT(HELIOS_HVC1_PATCH_LOCATION_ENTRIES == HELIOS_HNR2_MAX_USE_RECORDS, "");
/* Both sides' COMMIT uniqueness bitmap is an array of 64-bit words. If the
 * maximum ever stopped being a multiple of 64 the array would silently be one
 * word short: legal high indices would then be reported out-of-range, which
 * fails closed but misclassifies. */
HELIOS_NR_STATIC_ASSERT(HELIOS_HNR2_MAX_USE_RECORDS % 64u == 0u, "");
HELIOS_NR_STATIC_ASSERT(HELIOS_HNR2_FLAG_MASK == (HELIOS_HNR2_FLAG_BEGIN |
                                                  HELIOS_HNR2_FLAG_COMMIT |
                                                  HELIOS_HNR2_FLAG_HAS_REPLY),
                        "the §10.7 fragment flag mask is exactly its three bits");
HELIOS_NR_STATIC_ASSERT(HELIOS_HNR2_ACCESS_MASK == (HELIOS_HNR2_ACCESS_READ |
                                                    HELIOS_HNR2_ACCESS_WRITE),
                        "only READ and WRITE exist");

/* ------------------------------------------------------------------------ */
/* §10.7 — HVM1: the ordinary WDDM allocation create/open record             */
/* ------------------------------------------------------------------------ */

/* 'HVM1' little-endian. */
#define HELIOS_HVM1_MAGIC       0x314D5648u
#define HELIOS_HVM1_ABI_VERSION 1u
#define HELIOS_HVM1_SIZE        64u

/*
 * The four exact storage roles — a CLOSED vocabulary, not an extension point.
 *   1  the HTS1 session's one reply/feedback pool: 64 MiB in four fixed 16-MiB
 *      slots, Lock2-mapped once after create and residency and held to teardown;
 *   2  an ordinary application `VkDeviceMemory` from the
 *      DEVICE_LOCAL|HOST_VISIBLE|HOST_COHERENT type: Lock2 at the Vulkan map
 *      boundary, held only for that map's lifetime;
 *   3  Venus feedback storage;
 *   4  an ordinary application `VkDeviceMemory` from the DEVICE_LOCAL-only type.
 *      ⛔ REJECTS MAP, has no CPU VA, and may NEVER be passed to Lock2.
 */
#define HELIOS_HVM1_ROLE_REPLY_POOL           1u
#define HELIOS_HVM1_ROLE_VULKAN_HOST_VISIBLE  2u
#define HELIOS_HVM1_ROLE_FEEDBACK             3u
#define HELIOS_HVM1_ROLE_VULKAN_DEVICE_LOCAL  4u

/* "only CPU_READ=1, CPU_WRITE=2, HOST_READ=4, HOST_WRITE=8" — an exact
 * role-compatible subset, never merely a subset of the mask. */
#define HELIOS_HVM1_ACCESS_CPU_READ   1u
#define HELIOS_HVM1_ACCESS_CPU_WRITE  2u
#define HELIOS_HVM1_ACCESS_HOST_READ  4u
#define HELIOS_HVM1_ACCESS_HOST_WRITE 8u
#define HELIOS_HVM1_ACCESS_MASK       15u

/* Cache policy: NOT_CPU_VISIBLE is role 4 only; WRITE_COMBINED is roles 1-3
 * only. Khronos defines uncached host memory as coherent, which is the whole
 * basis on which role 2 is advertised HOST_COHERENT without a flush protocol. */
#define HELIOS_HVM1_CACHE_NOT_CPU_VISIBLE 0u
#define HELIOS_HVM1_CACHE_WRITE_COMBINED  1u

/* The segment page shift the KMD returns in this generation. */
#define HELIOS_HVM1_SEGMENT_PAGE_SHIFT 12u

/* The role-1 pool: 64 MiB as exactly four 16-MiB slots. */
#define HELIOS_HVM1_REPLY_POOL_BYTES UINT64_C(67108864)
#define HELIOS_HVM1_REPLY_SLOT_BYTES UINT64_C(16777216)
#define HELIOS_HVM1_REPLY_SLOT_COUNT 4u

/*
 * Mirror of Rust `HeliosVenusMemoryAllocationV1` — §10.7's 64-byte record.
 *
 * It is the ONLY per-allocation private data on a `D3DKMTCreateAllocation2` in
 * this lane, which is zeroed and issued with `hResource=0`, one allocation,
 * `pSystemMem=NULL`, outer flags zero, priority NORMAL,
 * `VidPnSourceId=D3DDDI_ID_NOTAPPLICABLE`, and `CreateShared`,
 * `NtSecuritySharing`, `ExistingSysMem`, `ExistingKernelSysMem`,
 * `ExistingSection` and `PermanentSysMem` all zero.
 *
 * ⛔ THREE FIELDS ARE WRITE-BACK: `object_generation`, `segment_page_shift` and
 * `allocation_alignment` are ZERO on input and filled by the KMD. Validate as
 * create-INPUT before the call and as create-OUTPUT after it — the same struct,
 * two different exact contracts (Rust `Hvm1Stage`).
 *
 * ⛔ THE LENGTH IS A GATE, NOT AN EXPECTATION: the private-data buffer must be
 * exactly 64 bytes, checked BEFORE the record is read. A consumer that reads 64
 * bytes out of a shorter buffer has already taken the out-of-bounds read that no
 * later field validation can undo, and in the KMD that read is a kernel one.
 * Rust `HeliosVenusMemoryAllocationV1::from_private_data` is that gate and is
 * the only admitted entry point; do not cast the pointer.
 */
typedef struct HeliosVenusMemoryAllocationV1 {
    uint32_t magic;                /* 0  == HELIOS_HVM1_MAGIC */
    uint16_t abi_version;          /* 4  == HELIOS_HVM1_ABI_VERSION */
    uint16_t struct_size;          /* 6  == HELIOS_HVM1_SIZE */
    uint64_t package_generation;   /* 8  exact */
    uint64_t object_generation;    /* 16 zero in, nonzero out. ⭐ the opaque local
                                    *    allocation capability Mesa stores
                                    *    INSTEAD OF a virtio resource id */
    uint64_t byte_size;            /* 24 nonzero exact allocation/renderer view */
    uint32_t role;                 /* 32 HELIOS_HVM1_ROLE_* */
    uint32_t access;               /* 36 HELIOS_HVM1_ACCESS_*, role-compatible */
    uint32_t cache_policy;         /* 40 HELIOS_HVM1_CACHE_*, exact for the role */
    uint32_t segment_page_shift;   /* 44 zero in; the selected segment's shift out */
    uint64_t allocation_alignment; /* 48 zero in; the exact alignment out */
    uint64_t reserved;             /* 56 zero in BOTH directions */
} HeliosVenusMemoryAllocationV1;

HELIOS_NR_STATIC_ASSERT(sizeof(HeliosVenusMemoryAllocationV1) == 64,
                        "HVM1 must be the §10.7 64-byte allocation record");
HELIOS_NR_STATIC_ASSERT(sizeof(HeliosVenusMemoryAllocationV1) == HELIOS_HVM1_SIZE,
                        "HVM1 size constant must match the struct");
HELIOS_NR_STATIC_ASSERT(HELIOS_NR_ALIGNOF(HeliosVenusMemoryAllocationV1) == 8, "");
HELIOS_NR_STATIC_ASSERT(offsetof(HeliosVenusMemoryAllocationV1, magic) == 0, "");
HELIOS_NR_STATIC_ASSERT(offsetof(HeliosVenusMemoryAllocationV1, abi_version) == 4, "");
HELIOS_NR_STATIC_ASSERT(offsetof(HeliosVenusMemoryAllocationV1, struct_size) == 6, "");
HELIOS_NR_STATIC_ASSERT(offsetof(HeliosVenusMemoryAllocationV1, package_generation) == 8, "");
HELIOS_NR_STATIC_ASSERT(offsetof(HeliosVenusMemoryAllocationV1, object_generation) == 16, "");
HELIOS_NR_STATIC_ASSERT(offsetof(HeliosVenusMemoryAllocationV1, byte_size) == 24, "");
HELIOS_NR_STATIC_ASSERT(offsetof(HeliosVenusMemoryAllocationV1, role) == 32, "");
HELIOS_NR_STATIC_ASSERT(offsetof(HeliosVenusMemoryAllocationV1, access) == 36, "");
HELIOS_NR_STATIC_ASSERT(offsetof(HeliosVenusMemoryAllocationV1, cache_policy) == 40, "");
HELIOS_NR_STATIC_ASSERT(offsetof(HeliosVenusMemoryAllocationV1, segment_page_shift) == 44, "");
HELIOS_NR_STATIC_ASSERT(offsetof(HeliosVenusMemoryAllocationV1, allocation_alignment) == 48, "");
HELIOS_NR_STATIC_ASSERT(offsetof(HeliosVenusMemoryAllocationV1, reserved) == 56, "");
HELIOS_NR_STATIC_ASSERT(HELIOS_HVM1_ACCESS_MASK == (HELIOS_HVM1_ACCESS_CPU_READ |
                                                    HELIOS_HVM1_ACCESS_CPU_WRITE |
                                                    HELIOS_HVM1_ACCESS_HOST_READ |
                                                    HELIOS_HVM1_ACCESS_HOST_WRITE),
                        "the §10.7 access mask is exactly its four bits");
/* The role-1 pool is exactly four slots. */
HELIOS_NR_STATIC_ASSERT(HELIOS_HVM1_REPLY_SLOT_BYTES * (uint64_t)HELIOS_HVM1_REPLY_SLOT_COUNT ==
                            HELIOS_HVM1_REPLY_POOL_BYTES,
                        "the role-1 pool must be exactly its slots");

/* ------------------------------------------------------------------------ */
/* §10.7 — HVR1: the bounded reply header and its continuation request       */
/* ------------------------------------------------------------------------ */

/* 'HVR1' little-endian. */
#define HELIOS_HVR1_MAGIC       0x31525648u
#define HELIOS_HVR1_VERSION     1u
#define HELIOS_HVR1_HEADER_SIZE 80u

/*
 * Reply storage is FIXED AND BOUNDED, with no growth and no spill path: at most
 * 64 MiB per immutable snapshot, at most four snapshots and 256 MiB of snapshot
 * bytes live per HTS1 session, and at most 15 MiB published by one HNR2
 * transaction into one 16-MiB slot behind exactly one HVR1 header. The fifth
 * caller drops the slot/snapshot lock and event-waits for the oldest exact C51
 * owner — it never grows the pool. A result with no bounded rule is not
 * advertised at all, rather than truncated or streamed.
 */
#define HELIOS_HVR1_MAX_SNAPSHOT_BYTES      UINT64_C(67108864)
#define HELIOS_HVR1_MAX_LIVE_SNAPSHOTS      4u
#define HELIOS_HVR1_MAX_LIVE_SNAPSHOT_BYTES UINT64_C(268435456)
#define HELIOS_HVR1_MAX_CHUNK_BYTES         UINT64_C(15728640)

/* "exactly one of MORE=1 or FINAL=2". */
#define HELIOS_HVR1_FLAG_MORE  1u
#define HELIOS_HVR1_FLAG_FINAL 2u
#define HELIOS_HVR1_FLAG_MASK  3u

/*
 * Mirror of Rust `HeliosVenusReplyV1` — §10.7's 80-byte reply header, written by
 * the host at the start of the checked-out reply-slot range with the chunk
 * payload immediately after it at byte 80.
 *
 * ⛔ Mesa validates EVERY field here before decode. A continuation copies the
 * exact next bytes of the already-frozen snapshot: it never re-runs the
 * operation and never changes `status`.
 */
typedef struct HeliosVenusReplyV1 {
    uint32_t magic;               /* 0  == HELIOS_HVR1_MAGIC */
    uint16_t version;             /* 4  == HELIOS_HVR1_VERSION */
    uint16_t header_size;         /* 6  == HELIOS_HVR1_HEADER_SIZE */
    uint64_t package_generation;  /* 8  exact live package */
    uint64_t session_generation;  /* 16 exact HTS1 session */
    uint64_t slot_generation;     /* 24 == HNR2 offset 104 */
    uint64_t batch_token;         /* 32 == the requesting HNR2 batch token */
    uint64_t snapshot_generation; /* 40 exact nonzero retained snapshot */
    uint32_t opcode;              /* 48 exact generated reply opcode */
    int32_t  status;              /* 52 exact SIGNED Vulkan/decoder result;
                                   *    positive results such as VK_INCOMPLETE
                                   *    are legal outcomes of a bounded rule */
    uint64_t total_bytes;         /* 56 immutable logical-result size */
    uint64_t chunk_offset;        /* 64 exact expected next offset */
    uint32_t chunk_bytes;         /* 72 <= MAX_CHUNK_BYTES; payload at byte 80 */
    uint32_t flags;               /* 76 exactly one HELIOS_HVR1_FLAG_* */
} HeliosVenusReplyV1;

HELIOS_NR_STATIC_ASSERT(sizeof(HeliosVenusReplyV1) == 80,
                        "HVR1 must be the §10.7 80-byte reply header");
HELIOS_NR_STATIC_ASSERT(sizeof(HeliosVenusReplyV1) == HELIOS_HVR1_HEADER_SIZE,
                        "HVR1 header size constant must match the struct");
HELIOS_NR_STATIC_ASSERT(HELIOS_NR_ALIGNOF(HeliosVenusReplyV1) == 8, "");
HELIOS_NR_STATIC_ASSERT(offsetof(HeliosVenusReplyV1, magic) == 0, "");
HELIOS_NR_STATIC_ASSERT(offsetof(HeliosVenusReplyV1, version) == 4, "");
HELIOS_NR_STATIC_ASSERT(offsetof(HeliosVenusReplyV1, header_size) == 6, "");
HELIOS_NR_STATIC_ASSERT(offsetof(HeliosVenusReplyV1, package_generation) == 8, "");
HELIOS_NR_STATIC_ASSERT(offsetof(HeliosVenusReplyV1, session_generation) == 16, "");
HELIOS_NR_STATIC_ASSERT(offsetof(HeliosVenusReplyV1, slot_generation) == 24, "");
HELIOS_NR_STATIC_ASSERT(offsetof(HeliosVenusReplyV1, batch_token) == 32, "");
HELIOS_NR_STATIC_ASSERT(offsetof(HeliosVenusReplyV1, snapshot_generation) == 40, "");
HELIOS_NR_STATIC_ASSERT(offsetof(HeliosVenusReplyV1, opcode) == 48, "");
HELIOS_NR_STATIC_ASSERT(offsetof(HeliosVenusReplyV1, status) == 52, "");
HELIOS_NR_STATIC_ASSERT(offsetof(HeliosVenusReplyV1, total_bytes) == 56, "");
HELIOS_NR_STATIC_ASSERT(offsetof(HeliosVenusReplyV1, chunk_offset) == 64, "");
HELIOS_NR_STATIC_ASSERT(offsetof(HeliosVenusReplyV1, chunk_bytes) == 72, "");
HELIOS_NR_STATIC_ASSERT(offsetof(HeliosVenusReplyV1, flags) == 76, "");

/*
 * Mirror of Rust `HeliosVenusReplyContinuationV1` — the generated continuation
 * request `{packageGeneration, sessionGeneration, snapshotGeneration,
 * expectedOffset, maxChunkBytes}`.
 *
 * ⚠ §10.7 names these five fields but gives NO offset table, because the request
 * is a generated command carried inside an ordinary HNR2 Venus payload rather
 * than a header the KMD parses. This fixed 40-byte little-endian layout is the
 * ABI's definition of that tuple, with an explicit `reserved` so the record stays
 * padding-free like every other record here. It has no magic and no version for
 * the same reason: it is not a standalone message.
 */
typedef struct HeliosVenusReplyContinuationV1 {
    uint64_t package_generation;  /* 0  exact live package */
    uint64_t session_generation;  /* 8  exact HTS1 session */
    uint64_t snapshot_generation; /* 16 exact nonzero snapshot being continued */
    uint64_t expected_offset;     /* 24 exact next byte offset into it */
    uint32_t max_chunk_bytes;     /* 32 <= MAX_CHUNK_BYTES; equals the carrying
                                   *    HNR2's reply_capacity_bytes - 80 */
    uint32_t reserved;            /* 36 zero */
} HeliosVenusReplyContinuationV1;

HELIOS_NR_STATIC_ASSERT(sizeof(HeliosVenusReplyContinuationV1) == 40,
                        "the HVR1 continuation request must be 40 bytes");
HELIOS_NR_STATIC_ASSERT(HELIOS_NR_ALIGNOF(HeliosVenusReplyContinuationV1) == 8, "");
HELIOS_NR_STATIC_ASSERT(offsetof(HeliosVenusReplyContinuationV1, package_generation) == 0, "");
HELIOS_NR_STATIC_ASSERT(offsetof(HeliosVenusReplyContinuationV1, session_generation) == 8, "");
HELIOS_NR_STATIC_ASSERT(offsetof(HeliosVenusReplyContinuationV1, snapshot_generation) == 16, "");
HELIOS_NR_STATIC_ASSERT(offsetof(HeliosVenusReplyContinuationV1, expected_offset) == 24, "");
HELIOS_NR_STATIC_ASSERT(offsetof(HeliosVenusReplyContinuationV1, max_chunk_bytes) == 32, "");
HELIOS_NR_STATIC_ASSERT(offsetof(HeliosVenusReplyContinuationV1, reserved) == 36, "");

HELIOS_NR_STATIC_ASSERT(HELIOS_HVR1_FLAG_MASK == (HELIOS_HVR1_FLAG_MORE |
                                                  HELIOS_HVR1_FLAG_FINAL),
                        "exactly one of MORE or FINAL, and nothing else exists");
/* At most four immutable 64-MiB snapshots, 256 MiB total, per session. */
HELIOS_NR_STATIC_ASSERT(HELIOS_HVR1_MAX_SNAPSHOT_BYTES *
                                (uint64_t)HELIOS_HVR1_MAX_LIVE_SNAPSHOTS ==
                            HELIOS_HVR1_MAX_LIVE_SNAPSHOT_BYTES,
                        "the live-snapshot byte cap must be four whole snapshots");
/* One transaction: one header plus at most 15 MiB, never more than one HNR2
 * batch's worth of Venus payload, and always inside one 16-MiB slot. */
HELIOS_NR_STATIC_ASSERT(HELIOS_HVR1_MAX_CHUNK_BYTES == HELIOS_HNR2_MAX_PAYLOAD_BYTES, "");
HELIOS_NR_STATIC_ASSERT((uint64_t)HELIOS_HVR1_HEADER_SIZE + HELIOS_HVR1_MAX_CHUNK_BYTES <=
                            HELIOS_HVM1_REPLY_SLOT_BYTES,
                        "an HVR1 header plus its maximum chunk must fit one slot");

/* ── The closed vocabularies are dense and pinned ──────────────────────────
 *
 * Every value below is hand-copied from the Rust module, and a mistyped literal
 * in a `#define` is otherwise caught by NOTHING on this side: unlike a size or an
 * offset, a role or an operand kind has no struct to disagree with. Pinning each
 * one makes a typo a compile error here rather than a wrong `role` reaching
 * `DxgkDdiCreateAllocation`. `translation_session.rs` uses exactly this pattern
 * for its own engine/control-class vocabularies.
 */
HELIOS_NR_STATIC_ASSERT(HELIOS_HVC1_MODE_FINITE_HNR2_RENDER == 2u, "");
HELIOS_NR_STATIC_ASSERT(HELIOS_HVM1_ROLE_REPLY_POOL == 1u, "");
HELIOS_NR_STATIC_ASSERT(HELIOS_HVM1_ROLE_VULKAN_HOST_VISIBLE == 2u, "");
HELIOS_NR_STATIC_ASSERT(HELIOS_HVM1_ROLE_FEEDBACK == 3u, "");
HELIOS_NR_STATIC_ASSERT(HELIOS_HVM1_ROLE_VULKAN_DEVICE_LOCAL == 4u, "");
HELIOS_NR_STATIC_ASSERT(HELIOS_HVM1_CACHE_NOT_CPU_VISIBLE == 0u, "");
HELIOS_NR_STATIC_ASSERT(HELIOS_HVM1_CACHE_WRITE_COMBINED == 1u, "");
/* Zero is never a valid operand kind: a ZEROED patch record must be a reject,
 * not an accidentally meaningful record. Each kind's width is its own. */
HELIOS_NR_STATIC_ASSERT(HELIOS_HNR2_OPERAND_KIND_INVALID == 0u, "");
HELIOS_NR_STATIC_ASSERT(HELIOS_HNR2_OPERAND_KIND_HOST_RESOURCE_ID32 == 1u, "");
HELIOS_NR_STATIC_ASSERT(HELIOS_HNR2_OPERAND_KIND_HOST_RESOURCE_ID64 == 2u, "");
HELIOS_NR_STATIC_ASSERT(HELIOS_HNR2_OPERAND_WIDTH_32 == 4u, "");
HELIOS_NR_STATIC_ASSERT(HELIOS_HNR2_OPERAND_WIDTH_64 == 8u, "");
/* The two spellings of "engine affinity" must never converge (§10.7:1694-1695
 * vs 1905; §12.2:3197-3199). If a future edit made them equal, the reason the
 * two constants exist would be gone. */
HELIOS_NR_STATIC_ASSERT(HELIOS_HVC1_ENGINE_AFFINITY != HELIOS_HVC1_FENCE_ENGINE_AFFINITY,
                        "the zero-based ordinal and the node bit mask are different values");

/* ── Every magic is four ASCII bytes read little-endian ────────────────────
 *
 * A transposed hex digit in a hand-copied literal would otherwise compile clean
 * here and fail only against the Rust side or a live host. These are the same
 * assertions `native_render.rs` makes.
 */
HELIOS_NR_STATIC_ASSERT((HELIOS_HVC1_MAGIC & 0xFFu) == (unsigned char)'H', "");
HELIOS_NR_STATIC_ASSERT(((HELIOS_HVC1_MAGIC >> 8) & 0xFFu) == (unsigned char)'V', "");
HELIOS_NR_STATIC_ASSERT(((HELIOS_HVC1_MAGIC >> 16) & 0xFFu) == (unsigned char)'C', "");
HELIOS_NR_STATIC_ASSERT(((HELIOS_HVC1_MAGIC >> 24) & 0xFFu) == (unsigned char)'1', "");
HELIOS_NR_STATIC_ASSERT((HELIOS_HNR2_MAGIC & 0xFFu) == (unsigned char)'H', "");
HELIOS_NR_STATIC_ASSERT(((HELIOS_HNR2_MAGIC >> 8) & 0xFFu) == (unsigned char)'N', "");
HELIOS_NR_STATIC_ASSERT(((HELIOS_HNR2_MAGIC >> 16) & 0xFFu) == (unsigned char)'R', "");
HELIOS_NR_STATIC_ASSERT(((HELIOS_HNR2_MAGIC >> 24) & 0xFFu) == (unsigned char)'2', "");
HELIOS_NR_STATIC_ASSERT((HELIOS_HVM1_MAGIC & 0xFFu) == (unsigned char)'H', "");
HELIOS_NR_STATIC_ASSERT(((HELIOS_HVM1_MAGIC >> 8) & 0xFFu) == (unsigned char)'V', "");
HELIOS_NR_STATIC_ASSERT(((HELIOS_HVM1_MAGIC >> 16) & 0xFFu) == (unsigned char)'M', "");
HELIOS_NR_STATIC_ASSERT(((HELIOS_HVM1_MAGIC >> 24) & 0xFFu) == (unsigned char)'1', "");
HELIOS_NR_STATIC_ASSERT((HELIOS_HVR1_MAGIC & 0xFFu) == (unsigned char)'H', "");
HELIOS_NR_STATIC_ASSERT(((HELIOS_HVR1_MAGIC >> 8) & 0xFFu) == (unsigned char)'V', "");
HELIOS_NR_STATIC_ASSERT(((HELIOS_HVR1_MAGIC >> 16) & 0xFFu) == (unsigned char)'R', "");
HELIOS_NR_STATIC_ASSERT(((HELIOS_HVR1_MAGIC >> 24) & 0xFFu) == (unsigned char)'1', "");
/* The four record magics must stay mutually distinct: a decoder that tries more
 * than one arm on the same bytes must never accept the wrong record. */
HELIOS_NR_STATIC_ASSERT(HELIOS_HVC1_MAGIC != HELIOS_HNR2_MAGIC, "");
HELIOS_NR_STATIC_ASSERT(HELIOS_HVC1_MAGIC != HELIOS_HVM1_MAGIC, "");
HELIOS_NR_STATIC_ASSERT(HELIOS_HVC1_MAGIC != HELIOS_HVR1_MAGIC, "");
HELIOS_NR_STATIC_ASSERT(HELIOS_HNR2_MAGIC != HELIOS_HVM1_MAGIC, "");
HELIOS_NR_STATIC_ASSERT(HELIOS_HNR2_MAGIC != HELIOS_HVR1_MAGIC, "");
HELIOS_NR_STATIC_ASSERT(HELIOS_HVM1_MAGIC != HELIOS_HVR1_MAGIC, "");

#if defined(__cplusplus)
}
#endif

#endif /* HELIOS_NATIVE_RENDER_H */
