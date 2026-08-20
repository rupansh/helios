/*
 * helios_wddm.h — C mirror of the Helios D3D-side wire records.
 *
 * ⛔ SINGLE SOURCE OF TRUTH: protocol/src/wddm.rs.
 * This header is the hand-maintained C projection of that file for the QEMU
 * (`qemu-helios/`) and Mesa (`icd/mesa`) sides. Every constant, struct, and
 * assertion below exists in the Rust file first; if the two ever disagree, the
 * Rust file wins and this header is the bug. Change one, change both in the
 * same commit — the assertions here are what turn a layout drift into a compile
 * error rather than a live VM mis-resolving a GPUVA.
 *
 * Normative: docs/HELIOS_PRESENT_SYNC_RETIREMENT.md §10.3 (HWA2), §10.4
 * (HOB1/HOS1), §10.6 (HOC1). §17.1 states the mandate this file exists to
 * satisfy: "Generate/assert both Rust **and C** offsets and the D3D11-type-1
 * versus D3D12-type-2 validator."
 *
 * WHY C NEEDS THIS AT ALL: §10.4 makes the host a first-class HOB1 reader —
 * "QEMU/HPM1 reads and validates HOB1 through the current process page tables,
 * resolves every GPUVA operand, and executes it". Without these declarations
 * the host half of a kernel/host boundary would parse a 112-byte header, a
 * 40-byte use record and a 16-byte operand from a hand-written layout that
 * nothing binds to the Rust side.
 *
 * NO IDENTITY CROSSES THIS BOUNDARY: nothing here carries a host resource
 * token, raw virtio `resid`, PID, process handle, pointer, or KMT handle.
 * Identity is the exact WDDM allocation object plus a nonzero KMD-assigned
 * allocation *generation*; `allocation_generation` and the HOB1 generation
 * fields are anti-stale cross-checks and are never lookup keys.
 *
 * ⛔ WHAT THIS HEADER IS NOT: a validator. The rules — bounds, alignment,
 * region non-overlap, the CRC, the D3D11-type-1/D3D12-type-2 discrimination,
 * the unique-allocation closure, and the use↔operand agreement — live in
 * `protocol/src/wddm.rs` (`HeliosOuterBatchV1::validate`,
 * `validate_batch_record`). A host implementation must reproduce them; the
 * comments on each declaration below name the rule so it cannot be reproduced
 * from the field names alone.
 *
 * ⛔ HOST READ CONTRACT: the C65 pool extent a HOB1 lives in is a CPU-visible
 * write-combined guest allocation the guest keeps mapped. §10.6's "once
 * submitted, the extent is immutable" is a GUEST-side promise, so the host must
 * snapshot the record out of the process page tables and validate the snapshot
 * it will execute. Validating bytes the producer can still store to validates a
 * record that no longer exists.
 */

#ifndef HELIOS_WDDM_H
#define HELIOS_WDDM_H

#include <stddef.h>
#include <stdint.h>

#if defined(__cplusplus)
extern "C" {
#endif

#if defined(__cplusplus)
#define HELIOS_WDDM_STATIC_ASSERT(cond, msg) static_assert(cond, msg)
#define HELIOS_WDDM_ALIGNOF(type) alignof(type)
#else
#define HELIOS_WDDM_STATIC_ASSERT(cond, msg) _Static_assert(cond, msg)
#define HELIOS_WDDM_ALIGNOF(type) _Alignof(type)
#endif

/* ── The atomic package generation (§17.1, final bullet) ───────────────────
 *
 * ⛔ SINGLE SOURCE OF TRUTH: `protocol/src/lib.rs`
 * (`HELIOS_PACKAGE_GENERATION`). This is a hand-maintained C mirror, and the
 * Rust side carries a test pinning the exact literal. Change one, change all.
 *
 * Every record in this header carries the value in its `package_generation`
 * field, and a mismatch is fatal with no fallback and no wildcard: zero is
 * never "any" on either side (§10.2 admission table, §17.8 steps 5-6).
 *
 * Guarded so this header and helios_diagnostics.h can be included together.
 */
#ifndef HELIOS_PACKAGE_GENERATION
#define HELIOS_PACKAGE_GENERATION_TAG     0x48454C49u
#define HELIOS_PACKAGE_GENERATION_ORDINAL 3u
#define HELIOS_PACKAGE_GENERATION \
    ((((uint64_t)HELIOS_PACKAGE_GENERATION_TAG) << 32) | \
     (uint64_t)HELIOS_PACKAGE_GENERATION_ORDINAL)
#endif

HELIOS_WDDM_STATIC_ASSERT(HELIOS_PACKAGE_GENERATION == UINT64_C(0x48454C4900000003),
                          "package generation must equal protocol/src/lib.rs "
                          "HELIOS_PACKAGE_GENERATION");

/* ── OS sentinels this ABI reuses verbatim (§10.3) ─────────────────────────*/

/* D3DDDI_ID_UNINITIALIZED. C44: a D3D12 runtime primary preserves it rather
 * than being given a concrete VidPn source, and a non-primary uses it too. It
 * means "any source on this exact adapter" and is never a concrete identity. */
#define HELIOS_D3DDDI_ID_UNINITIALIZED 0xFFFFFFFFu

/* ------------------------------------------------------------------------ */
/* §10.3 — HWA2: the immutable create-time allocation descriptor            */
/* ------------------------------------------------------------------------ */

/* 'HWA2' little-endian. */
#define HELIOS_HWA2_MAGIC       0x32415748u
#define HELIOS_HWA2_ABI_VERSION 2u
#define HELIOS_HWA2_BYTES       168u

/* Allocation kind (§10.3, offset 64). */
#define HELIOS_HWA2_KIND_INVALID          0u
#define HELIOS_HWA2_KIND_BUFFER           1u
#define HELIOS_HWA2_KIND_IMAGE            2u
#define HELIOS_HWA2_KIND_STANDARD_PRIMARY 3u
#define HELIOS_HWA2_KIND_STANDARD_SHADOW  4u
#define HELIOS_HWA2_KIND_STANDARD_STAGING 5u
#define HELIOS_HWA2_KIND_PAGING_OBJECT    6u
#define HELIOS_HWA2_KIND_MAX              HELIOS_HWA2_KIND_PAGING_OBJECT

/* Flags (§10.3, offset 68) — eleven bits, and nothing else. */
#define HELIOS_HWA2_FLAG_PRIMARY               (1u << 0)
#define HELIOS_HWA2_FLAG_STEREO                (1u << 1)
#define HELIOS_HWA2_FLAG_SHARED                (1u << 2)
#define HELIOS_HWA2_FLAG_DISPLAYABLE           (1u << 3)
#define HELIOS_HWA2_FLAG_DIRECT_FLIP_COMPATIBLE (1u << 4)
#define HELIOS_HWA2_FLAG_D3D12_RUNTIME_PRIMARY (1u << 5)
#define HELIOS_HWA2_FLAG_PROTECTED             (1u << 6)
#define HELIOS_HWA2_FLAG_CROSS_ADAPTER         (1u << 7)
#define HELIOS_HWA2_FLAG_CPU_VISIBLE           (1u << 8)
#define HELIOS_HWA2_FLAG_RESOURCE_ASSOCIATED   (1u << 9)
#define HELIOS_HWA2_FLAG_STANDARD              (1u << 10)
#define HELIOS_HWA2_FLAG_MASK                  0x000007FFu

/* The two bits ONLY the KMD may set, and therefore the exact set a create-INPUT
 * descriptor must leave clear. §10.3: "KMD sets `D3D12_RUNTIME_PRIMARY` only
 * when the D3D12 create record, runtime `PRIMARY` flag, and required
 * `D3DDDI_ID_UNINITIALIZED` value agree … KMD sets `DIRECT_FLIP_COMPATIBLE`
 * only when the exact allocation is a non-protected, non-cross-adapter managed
 * primary in a swizzle/layout class the selected display backend implements",
 * and of both: "neither is inferred by an opener". A producer that pre-sets
 * either is refused by name (Rust `KmdOwnedFlagSetOnInput`), never silently
 * corrected — a silent correction makes the finished descriptor disagree with
 * the resource the producer believes it asked for. */
#define HELIOS_HWA2_FLAG_KMD_OWNED_MASK \
    (HELIOS_HWA2_FLAG_DIRECT_FLIP_COMPATIBLE | HELIOS_HWA2_FLAG_D3D12_RUNTIME_PRIMARY)

/* Bind flags (§10.3, offset 72) — the shared protocol vocabulary, never a raw
 * D3D11/D3D12 bit reinterpretation. */
#define HELIOS_HWA2_BIND_SHADER_RESOURCE  (1u << 0)
#define HELIOS_HWA2_BIND_RENDER_TARGET    (1u << 1)
#define HELIOS_HWA2_BIND_DEPTH_STENCIL    (1u << 2)
#define HELIOS_HWA2_BIND_UNORDERED_ACCESS (1u << 3)
#define HELIOS_HWA2_BIND_VERTEX_BUFFER    (1u << 4)
#define HELIOS_HWA2_BIND_INDEX_BUFFER     (1u << 5)
#define HELIOS_HWA2_BIND_CONSTANT_BUFFER  (1u << 6)
#define HELIOS_HWA2_BIND_STREAM_OUTPUT    (1u << 7)
#define HELIOS_HWA2_BIND_PRESENT          (1u << 8)
#define HELIOS_HWA2_BIND_VIDEO_DECODER    (1u << 9)
#define HELIOS_HWA2_BIND_VIDEO_ENCODER    (1u << 10)
#define HELIOS_HWA2_BIND_MASK             0x000007FFu

/* Misc flags (§10.3, offset 76). */
#define HELIOS_HWA2_MISC_GDI_COMPATIBLE   (1u << 0)
#define HELIOS_HWA2_MISC_TEXTURE_CUBE     (1u << 1)
#define HELIOS_HWA2_MISC_RESOURCE_CLAMP   (1u << 2)
#define HELIOS_HWA2_MISC_SHARED_NT_HANDLE (1u << 3)
#define HELIOS_HWA2_MISC_MASK             0x0000000Fu

/* Swizzle/layout class (§10.3, offset 88). Direct Flip requires the pair to be
 * equal AND the class to be one that is explicitly supported. */
#define HELIOS_HWA2_SWIZZLE_INVALID        0u
#define HELIOS_HWA2_SWIZZLE_LINEAR         1u
#define HELIOS_HWA2_SWIZZLE_OPAQUE_OPTIMAL 2u
#define HELIOS_HWA2_SWIZZLE_MAX            HELIOS_HWA2_SWIZZLE_OPAQUE_OPTIMAL

/* Memory class (§10.3, offset 92). No Vulkan memory-type index. */
#define HELIOS_HWA2_MEMORY_INVALID      0u
#define HELIOS_HWA2_MEMORY_DEVICE_LOCAL 1u
#define HELIOS_HWA2_MEMORY_SHARED       2u
#define HELIOS_HWA2_MEMORY_CPU_VISIBLE  3u
#define HELIOS_HWA2_MEMORY_MAX          HELIOS_HWA2_MEMORY_CPU_VISIBLE

#define HELIOS_HWA2_MAX_PLANES 4u

/*
 * Mirror of Rust `HeliosWddmPlaneRecordV2`.
 *
 * Records at or above `plane_count` are zero. A used record's range is
 * `offset + slice_pitch`, overflow-checked against the descriptor's
 * `byte_size` — never `row_pitch * height`, which over-counts a chroma plane.
 */
typedef struct HeliosWddmPlaneRecordV2 {
    uint64_t offset;      /* 0  */
    uint32_t row_pitch;   /* 8  */
    uint32_t slice_pitch; /* 12 */
} HeliosWddmPlaneRecordV2;

HELIOS_WDDM_STATIC_ASSERT(sizeof(HeliosWddmPlaneRecordV2) == 16,
                          "HeliosWddmPlaneRecordV2 must be the §10.3 16-byte plane record");
HELIOS_WDDM_STATIC_ASSERT(HELIOS_WDDM_ALIGNOF(HeliosWddmPlaneRecordV2) == 8, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosWddmPlaneRecordV2, offset) == 0, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosWddmPlaneRecordV2, row_pitch) == 8, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosWddmPlaneRecordV2, slice_pitch) == 12, "");

/*
 * Mirror of Rust `HeliosWddmAllocationDescV2` — §10.3's 168-byte table.
 *
 * The creator allocates the whole buffer, KMD writes it ONLY at create, and
 * every opener treats it as const. `DxgkDdiOpenAllocation` never writes it:
 * the retired `HeliosWddmOpenIdentity` restamped its first 48 bytes at open
 * time, so two openers of one allocation could disagree about what they had.
 *
 * ⛔ TWO CREATE STAGES, ONE LAYOUT. The buffer is `[in/out]` and the kernel
 * cannot invent texel dimensions, so the create is a request and a total
 * acceptance: user mode fills a complete create-INPUT record, KMD validates it
 * in full, and KMD then writes all 168 bytes back, echoing every field it
 * validated and stamping the ones only the kernel can know. The whole
 * difference between the two sides is `allocation_generation` (zero in, nonzero
 * out), `HELIOS_HWA2_FLAG_KMD_OWNED_MASK` (clear in, KMD-decided out), and the
 * one C44 cross-field rule those bits make stage-dependent: because
 * `HELIOS_HWA2_FLAG_D3D12_RUNTIME_PRIMARY` is KMD-owned, a D3D12 runtime
 * primary's INPUT is `FLAG_PRIMARY` + `HELIOS_D3DDDI_ID_UNINITIALIZED` + that
 * bit CLEAR, and is admitted; on OUTPUT the same primary+sentinel pair without
 * the bit is refused, so the two are still cross-validated in both directions on
 * the bytes every opener treats as const.
 * A refused input creates nothing. Rust `Hwa2Stage` /
 * `validate_create_input` / `validate_create_output`.
 *
 * Any malformed, unknown, TRUNCATED, mismatched-generation, or reserved-nonzero
 * descriptor makes create/open fail; it never selects a legacy parser. TRUNCATED
 * is a length gate on the `(pPrivateDriverData, PrivateDriverDataSize)` pair
 * itself — exactly 168 bytes, checked BEFORE the read — because a consumer that
 * reads 168 bytes out of a shorter buffer has already taken the out-of-bounds
 * read no later validation can undo (Rust `from_private_data`).
 */
typedef struct HeliosWddmAllocationDescV2 {
    uint32_t magic;                    /* 0   == HELIOS_HWA2_MAGIC */
    uint16_t abi_version;              /* 4   == HELIOS_HWA2_ABI_VERSION */
    uint16_t struct_size;              /* 6   == HELIOS_HWA2_BYTES */
    uint64_t package_generation;       /* 8   exact; zero is never a wildcard */
    uint64_t allocation_generation;    /* 16  zero IN, nonzero OUT; KMD assigns
                                        *     it; NEVER a lookup key */
    uint64_t byte_size;                /* 24  nonzero; bounds every plane */
    uint32_t width;                    /* 32  zero only for a non-image kind */
    uint32_t height;                   /* 36 */
    uint32_t depth_or_array_size;      /* 40 */
    uint32_t mip_levels;               /* 44 */
    uint32_t dxgi_format;              /* 48  carried verbatim: the D3DDDI
                                        *     translation is lossy */
    uint32_t d3d_ddi_format;           /* 52 */
    uint32_t sample_count;             /* 56 */
    uint32_t sample_quality;           /* 60 */
    uint32_t allocation_kind;          /* 64  HELIOS_HWA2_KIND_* */
    uint32_t flags;                    /* 68  HELIOS_HWA2_FLAG_*; on input every
                                        *     KMD_OWNED_MASK bit is clear */
    uint32_t bind_flags;               /* 72  HELIOS_HWA2_BIND_* */
    uint32_t misc_flags;               /* 76  HELIOS_HWA2_MISC_* */
    uint32_t vidpn_source;             /* 80  concrete, or the C44 sentinel; a
                                        *     PRIMARY carrying the sentinel is a
                                        *     D3D12 request on INPUT and the
                                        *     RUNTIME_PRIMARY bit on OUTPUT */
    uint32_t standard_allocation_type; /* 84  nonzero iff FLAG_STANDARD */
    uint32_t swizzle_class;            /* 88  HELIOS_HWA2_SWIZZLE_* */
    uint32_t memory_class;             /* 92  HELIOS_HWA2_MEMORY_* */
    uint32_t plane_count;              /* 96  0..=4 */
    uint32_t reserved;                 /* 100 zero */
    HeliosWddmPlaneRecordV2 planes[4]; /* 104 records >= plane_count are zero */
} HeliosWddmAllocationDescV2;

HELIOS_WDDM_STATIC_ASSERT(sizeof(HeliosWddmAllocationDescV2) == 168,
                          "HWA2 must be the §10.3 168-byte descriptor");
HELIOS_WDDM_STATIC_ASSERT(sizeof(HeliosWddmAllocationDescV2) == HELIOS_HWA2_BYTES,
                          "HWA2 size constant must match the struct");
HELIOS_WDDM_STATIC_ASSERT(HELIOS_WDDM_ALIGNOF(HeliosWddmAllocationDescV2) == 8, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosWddmAllocationDescV2, magic) == 0, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosWddmAllocationDescV2, abi_version) == 4, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosWddmAllocationDescV2, struct_size) == 6, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosWddmAllocationDescV2, package_generation) == 8, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosWddmAllocationDescV2, allocation_generation) == 16, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosWddmAllocationDescV2, byte_size) == 24, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosWddmAllocationDescV2, width) == 32, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosWddmAllocationDescV2, height) == 36, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosWddmAllocationDescV2, depth_or_array_size) == 40, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosWddmAllocationDescV2, mip_levels) == 44, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosWddmAllocationDescV2, dxgi_format) == 48, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosWddmAllocationDescV2, d3d_ddi_format) == 52, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosWddmAllocationDescV2, sample_count) == 56, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosWddmAllocationDescV2, sample_quality) == 60, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosWddmAllocationDescV2, allocation_kind) == 64, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosWddmAllocationDescV2, flags) == 68, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosWddmAllocationDescV2, bind_flags) == 72, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosWddmAllocationDescV2, misc_flags) == 76, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosWddmAllocationDescV2, vidpn_source) == 80, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosWddmAllocationDescV2, standard_allocation_type) == 84, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosWddmAllocationDescV2, swizzle_class) == 88, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosWddmAllocationDescV2, memory_class) == 92, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosWddmAllocationDescV2, plane_count) == 96, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosWddmAllocationDescV2, reserved) == 100, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosWddmAllocationDescV2, planes) == 104, "");
HELIOS_WDDM_STATIC_ASSERT(HELIOS_HWA2_FLAG_MASK ==
                              (HELIOS_HWA2_FLAG_PRIMARY | HELIOS_HWA2_FLAG_STEREO |
                               HELIOS_HWA2_FLAG_SHARED | HELIOS_HWA2_FLAG_DISPLAYABLE |
                               HELIOS_HWA2_FLAG_DIRECT_FLIP_COMPATIBLE |
                               HELIOS_HWA2_FLAG_D3D12_RUNTIME_PRIMARY |
                               HELIOS_HWA2_FLAG_PROTECTED | HELIOS_HWA2_FLAG_CROSS_ADAPTER |
                               HELIOS_HWA2_FLAG_CPU_VISIBLE |
                               HELIOS_HWA2_FLAG_RESOURCE_ASSOCIATED | HELIOS_HWA2_FLAG_STANDARD),
                          "the §10.3 flag mask is exactly its eleven bits");
HELIOS_WDDM_STATIC_ASSERT(HELIOS_HWA2_FLAG_KMD_OWNED_MASK == 0x00000030u,
                          "the KMD-owned pair is exactly DIRECT_FLIP_COMPATIBLE|D3D12_RUNTIME_PRIMARY");
HELIOS_WDDM_STATIC_ASSERT((HELIOS_HWA2_FLAG_KMD_OWNED_MASK & ~HELIOS_HWA2_FLAG_MASK) == 0u,
                          "the KMD-owned pair must be a subset of the defined flags");

/* ------------------------------------------------------------------------ */
/* §10.4 — HOB1: one complete contiguous translated outer command           */
/* ------------------------------------------------------------------------ */

/* 'HOB1' little-endian. */
#define HELIOS_HOB1_MAGIC        0x31424F48u
#define HELIOS_HOB1_ABI_VERSION  1u
#define HELIOS_HOB1_HEADER_BYTES 112u

/* Byte offset of the CRC field, folded in as ZERO when checksumming. */
#define HELIOS_HOB1_CRC_FIELD_OFFSET 80u

#define HELIOS_HOB1_USE_RECORD_BYTES     40u
#define HELIOS_HOB1_OPERAND_RECORD_BYTES 16u
#define HELIOS_HOB1_MAX_USE_RECORDS      4096u
#define HELIOS_HOB1_MAX_OPERAND_RECORDS  8192u
/* 15 MiB, header through payload. */
#define HELIOS_HOB1_MAX_BYTES            UINT64_C(15728640)
/* §10.4 says "aligned" without a number; 8 is the conservative reading — the
 * natural alignment of the u64-bearing 40-byte use record, a multiple of the
 * 16-byte operand's own requirement, and preserved by both strides. */
#define HELIOS_HOB1_OFFSET_ALIGNMENT     UINT64_C(8)
/* Required alignment of an operand's payload_offset: the Venus command stream
 * is encoded in 4-byte units, so every generated resource operand — of either
 * width — starts 4-byte aligned. Without this an operand could rewrite a
 * capability ordinal across two adjacent Venus command words, which is the
 * "arbitrary byte patching" §10.4 rejects. */
#define HELIOS_HOB1_OPERAND_ALIGN        4u

/* Batch arm (§10.4, offset 44) — EXACTLY one. The arm decides which identity
 * kind every use record must carry; a record never re-selects it. */
#define HELIOS_HOB1_FLAG_D3D11_PHYSICAL 1u
#define HELIOS_HOB1_FLAG_D3D12_VIRTUAL  2u

/* Use-record identity kind. Type 1 is a D3D11 allocation-list index with the
 * upper 32 address bits zero; type 2 is a D3D12 GPUVA. */
#define HELIOS_HOB1_IDENTITY_D3D11_ALLOCATION_INDEX 1u
#define HELIOS_HOB1_IDENTITY_D3D12_GPUVA            2u

/* Access bits. Only these three exist, and PRIMARY_WRITE implies WRITE. */
#define HELIOS_HOB1_ACCESS_READ          1u
#define HELIOS_HOB1_ACCESS_WRITE         2u
#define HELIOS_HOB1_ACCESS_PRIMARY_WRITE 4u
#define HELIOS_HOB1_ACCESS_MASK          7u

/* Operand kind. GENERATED_RESOURCE is the ONLY kind: "it identifies only a
 * generated resource operand whose payload bytes are zero; arbitrary patches
 * and raw renderer IDs are rejected" (§10.4). */
#define HELIOS_HOB1_OPERAND_KIND_INVALID            0u
#define HELIOS_HOB1_OPERAND_KIND_GENERATED_RESOURCE 1u
#define HELIOS_HOB1_OPERAND_KIND_MAX                HELIOS_HOB1_OPERAND_KIND_GENERATED_RESOURCE

#define HELIOS_HOB1_OPERAND_WIDTH_4 4u
#define HELIOS_HOB1_OPERAND_WIDTH_8 8u

/* CRC-64/ECMA-182 parameters for the HOB1 checksum: poly 0x42F0E1EBA9EA3693,
 * init 0, xorout 0, NOT reflected. ⚠ This is NOT CRC-64/XZ, which uses the same
 * polynomial reflected with init/xorout 0xFFFF...; the check value below is what
 * separates the two. It is a CORRUPTION check, never an identity. */
#define HELIOS_CRC64_ECMA182_POLY    UINT64_C(0x42F0E1EBA9EA3693)
#define HELIOS_CRC64_ECMA182_INIT    UINT64_C(0)
#define HELIOS_CRC64_ECMA182_XOROUT  UINT64_C(0)
/* CRC of the nine ASCII bytes "123456789". */
#define HELIOS_CRC64_ECMA182_CHECK   UINT64_C(0x6C40DF5F0B497347)

/*
 * Mirror of Rust `HeliosOuterBatchV1` — §10.4's 112-byte header.
 *
 * The header is followed, INSIDE THE SAME RECORD, by the bounded use table, the
 * typed-operand table, and the sealed Venus payload. All interior offsets are
 * from the HOB1 start; the three regions are non-overlapping, the payload is
 * last, and the payload ends exactly at `total_bytes` ("header through
 * payload").
 *
 * The generation fields are anti-stale cross-checks against the live KMD
 * context object, which remains the identity; no HOB1 record orders another
 * WDDM context.
 */
typedef struct HeliosOuterBatchV1 {
    uint32_t magic;               /* 0   == HELIOS_HOB1_MAGIC */
    uint16_t abi_version;         /* 4   == HELIOS_HOB1_ABI_VERSION */
    uint16_t header_size;         /* 6   == HELIOS_HOB1_HEADER_BYTES */
    uint64_t package_generation;  /* 8   exact */
    uint64_t session_generation;  /* 16  exact live HTS1 session */
    uint64_t context_generation;  /* 24  exact HQA1-attached context */
    uint64_t batch_id;            /* 32  nonzero, strictly increasing on THIS
                                   *     context only */
    uint32_t endpoint_id;         /* 40  nonzero */
    uint32_t flags;               /* 44  exactly one HELIOS_HOB1_FLAG_* arm */
    uint64_t total_bytes;         /* 48  nonzero, <= HELIOS_HOB1_MAX_BYTES, and
                                   *     <= the runtime-approved command buffer */
    uint32_t payload_offset;      /* 56  aligned, after both tables */
    uint32_t payload_bytes;       /* 60  nonzero: an empty payload is no batch */
    uint32_t use_offset;          /* 64  aligned, after the header; zero iff
                                   *     use_count is zero */
    uint32_t use_count;           /* 68  <= HELIOS_HOB1_MAX_USE_RECORDS */
    uint32_t operand_offset;      /* 72  aligned, non-overlapping; zero iff
                                   *     operand_count is zero */
    uint32_t operand_count;       /* 76  <= HELIOS_HOB1_MAX_OPERAND_RECORDS */
    uint64_t crc64;               /* 80  CRC-64/ECMA-182 of the whole record with
                                   *     THIS FIELD ZERO */
    uint8_t  reserved[24];        /* 88  zero */
} HeliosOuterBatchV1;

HELIOS_WDDM_STATIC_ASSERT(sizeof(HeliosOuterBatchV1) == 112,
                          "HOB1 must be the §10.4 112-byte header");
HELIOS_WDDM_STATIC_ASSERT(sizeof(HeliosOuterBatchV1) == HELIOS_HOB1_HEADER_BYTES,
                          "HOB1 header size constant must match the struct");
HELIOS_WDDM_STATIC_ASSERT(HELIOS_WDDM_ALIGNOF(HeliosOuterBatchV1) == 8, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosOuterBatchV1, magic) == 0, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosOuterBatchV1, abi_version) == 4, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosOuterBatchV1, header_size) == 6, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosOuterBatchV1, package_generation) == 8, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosOuterBatchV1, session_generation) == 16, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosOuterBatchV1, context_generation) == 24, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosOuterBatchV1, batch_id) == 32, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosOuterBatchV1, endpoint_id) == 40, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosOuterBatchV1, flags) == 44, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosOuterBatchV1, total_bytes) == 48, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosOuterBatchV1, payload_offset) == 56, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosOuterBatchV1, payload_bytes) == 60, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosOuterBatchV1, use_offset) == 64, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosOuterBatchV1, use_count) == 68, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosOuterBatchV1, operand_offset) == 72, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosOuterBatchV1, operand_count) == 76, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosOuterBatchV1, crc64) == 80, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosOuterBatchV1, crc64) == HELIOS_HOB1_CRC_FIELD_OFFSET,
                          "the CRC fold offset must be the CRC field's offset");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosOuterBatchV1, reserved) == 88, "");

/*
 * Mirror of Rust `HeliosOuterBatchUseV1` — §10.4's 40-byte use record.
 *
 * ⛔ HOST OBLIGATIONS the field names do not state, all enforced by
 * `validate_batch_record` in the Rust source and all of which the host must
 * reproduce:
 *   - the identity kind must be the one this batch's ARM requires;
 *   - a type-1 index must have its upper 32 address bits zero AND be BELOW the
 *     `D3DDDI_ALLOCATIONLIST` length for this Render — the consumer indexes a
 *     kernel array with a user-mode-supplied number;
 *   - a type-2 GPUVA must be nonzero and `address_or_index + byte_length` must
 *     not wrap;
 *   - `access_flags` is nonzero, inside HELIOS_HOB1_ACCESS_MASK, and never
 *     PRIMARY_WRITE without WRITE;
 *   - on the D3D11 arm no two records may name the same allocation ("the use
 *     table is the complete UNIQUE-allocation closure"); and
 *   - `{first_operand, operand_count}` runs tile the operand table in order, and
 *     each operand's `use_index` names the use whose run contains it. A use may
 *     own no operand, in which case `first_operand` is zero.
 */
typedef struct HeliosOuterBatchUseV1 {
    uint64_t address_or_index;               /* 0  type 1: index; type 2: GPUVA */
    uint64_t byte_length;                    /* 8  nonzero */
    uint64_t expected_allocation_generation; /* 16 the HWA2 generation the
                                              *    encoder saw; a mismatch is a
                                              *    stale batch, not a lookup
                                              *    miss */
    uint32_t access_flags;                   /* 24 HELIOS_HOB1_ACCESS_* */
    uint16_t identity_kind;                  /* 28 HELIOS_HOB1_IDENTITY_* */
    uint16_t operand_count;                  /* 30 */
    uint32_t first_operand;                  /* 32 */
    uint32_t reserved;                       /* 36 zero */
} HeliosOuterBatchUseV1;

HELIOS_WDDM_STATIC_ASSERT(sizeof(HeliosOuterBatchUseV1) == 40,
                          "HOB1 use record must be the §10.4 40-byte record");
HELIOS_WDDM_STATIC_ASSERT(sizeof(HeliosOuterBatchUseV1) == HELIOS_HOB1_USE_RECORD_BYTES,
                          "use-record size constant must match the struct");
HELIOS_WDDM_STATIC_ASSERT(HELIOS_WDDM_ALIGNOF(HeliosOuterBatchUseV1) == 8, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosOuterBatchUseV1, address_or_index) == 0, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosOuterBatchUseV1, byte_length) == 8, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosOuterBatchUseV1, expected_allocation_generation) == 16, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosOuterBatchUseV1, access_flags) == 24, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosOuterBatchUseV1, identity_kind) == 28, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosOuterBatchUseV1, operand_count) == 30, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosOuterBatchUseV1, first_operand) == 32, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosOuterBatchUseV1, reserved) == 36, "");

/*
 * Mirror of Rust `HeliosOuterBatchOperandV1` — §10.4's 16-byte typed operand.
 *
 * `payload_offset` is measured FROM THE HOB1 START and the whole operand must
 * lie inside `[payload_offset, +payload_bytes)`, so an operand can never name a
 * byte of the header or of either table. It must also be
 * HELIOS_HOB1_OPERAND_ALIGN-aligned.
 *
 * ⛔ The encoder writes ZERO into every host-resource-id operand and the
 * consumer rewrites it to a device-local capability ordinal: a host resource ID
 * never travels on the wire. That is a CHECKED rule, not a convention — the
 * `encoded_width` bytes at `payload_offset` must all be zero at validation
 * time, which is the only time validation can happen at all, because the CRC
 * covers the payload and a rewritten record no longer matches it. A nonzero
 * placeholder is how a raw renderer/host resource ID would reach the host at a
 * position the operand table blesses.
 */
typedef struct HeliosOuterBatchOperandV1 {
    uint32_t payload_offset; /* 0  from the HOB1 start */
    uint32_t use_index;      /* 4  into the use table */
    uint16_t operand_kind;   /* 8  HELIOS_HOB1_OPERAND_KIND_* */
    uint16_t encoded_width;  /* 10 4 or 8 */
    uint32_t reserved;       /* 12 zero */
} HeliosOuterBatchOperandV1;

HELIOS_WDDM_STATIC_ASSERT(sizeof(HeliosOuterBatchOperandV1) == 16,
                          "HOB1 operand must be the §10.4 16-byte record");
HELIOS_WDDM_STATIC_ASSERT(sizeof(HeliosOuterBatchOperandV1) == HELIOS_HOB1_OPERAND_RECORD_BYTES,
                          "operand-record size constant must match the struct");
HELIOS_WDDM_STATIC_ASSERT(HELIOS_WDDM_ALIGNOF(HeliosOuterBatchOperandV1) == 4, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosOuterBatchOperandV1, payload_offset) == 0, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosOuterBatchOperandV1, use_index) == 4, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosOuterBatchOperandV1, operand_kind) == 8, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosOuterBatchOperandV1, encoded_width) == 10, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosOuterBatchOperandV1, reserved) == 12, "");

/* ------------------------------------------------------------------------ */
/* §10.4 — HOS1: the fixed 64-byte D3D12 private-data descriptor            */
/* ------------------------------------------------------------------------ */

/* 'HOS1' little-endian. */
#define HELIOS_HOS1_MAGIC       0x31534F48u
#define HELIOS_HOS1_ABI_VERSION 1u
#define HELIOS_HOS1_BYTES       64u

/*
 * Mirror of Rust `HeliosOuterSubmitV1`.
 *
 * ⛔ METADATA, NOT WORK. D3D12 leaves the HOB1 at the exact submitted command
 * GPUVA and passes only this record; dxgkrnl copies the prefix into KMD private
 * data, and KMD validates it WITHOUT DEREFERENCING THE GPUVA. It "contains no
 * command bytes, resource identity, pointer, handle, GPUVA, or host token", and
 * it never carries `WrittenPrimaries` — that association stays in
 * `D3DDDICB_SUBMITCOMMAND` where C1 defines it.
 *
 * The eleven scalars account for all 64 bytes, so there is no byte array, no
 * trailing capacity, and no padding through which a producer could smuggle a
 * command, GPUVA, or handle. The scalar-sum assertion below is what keeps that
 * true in C as well.
 */
typedef struct HeliosOuterSubmitV1 {
    uint32_t magic;              /* 0  == HELIOS_HOS1_MAGIC */
    uint16_t abi_version;        /* 4  == HELIOS_HOS1_ABI_VERSION */
    uint16_t struct_size;        /* 6  == HELIOS_HOS1_BYTES */
    uint64_t package_generation; /* 8  exact */
    uint64_t session_generation; /* 16 exact */
    uint64_t context_generation; /* 24 exact attached VIRTUAL context */
    uint32_t endpoint_id;        /* 32 nonzero */
    uint32_t hob1_bytes;         /* 36 == CommandLength/DmaBufferSize; at least
                                  *    the 112-byte header and at most
                                  *    HELIOS_HOB1_MAX_BYTES */
    uint64_t batch_id;           /* 40 equals the HOB1's */
    uint64_t hob1_crc64;         /* 48 equals the HOB1's */
    uint64_t reserved;           /* 56 zero */
} HeliosOuterSubmitV1;

HELIOS_WDDM_STATIC_ASSERT(sizeof(HeliosOuterSubmitV1) == 64,
                          "HOS1 must be the §10.4 64-byte descriptor");
HELIOS_WDDM_STATIC_ASSERT(sizeof(HeliosOuterSubmitV1) == HELIOS_HOS1_BYTES,
                          "HOS1 size constant must match the struct");
HELIOS_WDDM_STATIC_ASSERT(HELIOS_WDDM_ALIGNOF(HeliosOuterSubmitV1) == 8, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosOuterSubmitV1, magic) == 0, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosOuterSubmitV1, abi_version) == 4, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosOuterSubmitV1, struct_size) == 6, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosOuterSubmitV1, package_generation) == 8, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosOuterSubmitV1, session_generation) == 16, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosOuterSubmitV1, context_generation) == 24, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosOuterSubmitV1, endpoint_id) == 32, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosOuterSubmitV1, hob1_bytes) == 36, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosOuterSubmitV1, batch_id) == 40, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosOuterSubmitV1, hob1_crc64) == 48, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosOuterSubmitV1, reserved) == 56, "");
HELIOS_WDDM_STATIC_ASSERT(sizeof(uint32_t) + sizeof(uint16_t) + sizeof(uint16_t) +
                                  sizeof(uint64_t) + sizeof(uint64_t) + sizeof(uint64_t) +
                                  sizeof(uint32_t) + sizeof(uint32_t) + sizeof(uint64_t) +
                                  sizeof(uint64_t) + sizeof(uint64_t) ==
                              sizeof(HeliosOuterSubmitV1),
                          "HOS1's declared scalars must account for all 64 bytes: no byte "
                          "array, no trailing capacity, no padding to smuggle work through");

/* ------------------------------------------------------------------------ */
/* §10.6 — HOC1: the C65 D3D12 command-buffer pool allocation record        */
/* ------------------------------------------------------------------------ */

/* 'HOC1' little-endian. */
#define HELIOS_HOC1_MAGIC       0x31434F48u
#define HELIOS_HOC1_ABI_VERSION 1u
#define HELIOS_HOC1_BYTES       64u

/* 64 MiB pool, 64-KiB extent alignment, at most 256 live extents. */
#define HELIOS_HOC1_POOL_BYTES        UINT64_C(67108864)
#define HELIOS_HOC1_EXTENT_ALIGNMENT  65536u
#define HELIOS_HOC1_MAX_LIVE_EXTENTS  256u

#define HELIOS_HOC1_ACCESS_CPU_WRITE          1u
#define HELIOS_HOC1_ACCESS_DEVICE_READ        2u
#define HELIOS_HOC1_ACCESS_REQUIRED           3u
#define HELIOS_HOC1_CACHE_WRITE_COMBINED      1u
#define HELIOS_HOC1_PHYSICAL_ADAPTER_MASK_NODE0 1u

/*
 * Mirror of Rust `HeliosOuterCommandAllocationV1`.
 *
 * ⛔ HOC1 is NEITHER A RENDERER/RESOURCE IDENTITY NOR A SHAREABLE OBJECT.
 * Nothing in it names a host backing, a `resid`, a Venus object, or anything
 * another process could open; KMD admits it only as a nonprimary, nonshared,
 * CPU-visible/WC allocation preferred in HLM1.
 *
 * Its only mutable field is `allocation_generation`: zero on input, and written
 * back nonzero by KMD at create. That is the one create-time write-back in this
 * ABI.
 */
typedef struct HeliosOuterCommandAllocationV1 {
    uint32_t magic;                 /* 0  == HELIOS_HOC1_MAGIC */
    uint16_t abi_version;           /* 4  == HELIOS_HOC1_ABI_VERSION */
    uint16_t struct_size;           /* 6  == HELIOS_HOC1_BYTES */
    uint64_t package_generation;    /* 8  exact */
    uint64_t allocation_generation; /* 16 zero in, nonzero out */
    uint64_t byte_size;             /* 24 == HELIOS_HOC1_POOL_BYTES */
    uint32_t extent_alignment;      /* 32 == HELIOS_HOC1_EXTENT_ALIGNMENT */
    uint32_t access;                /* 36 == HELIOS_HOC1_ACCESS_REQUIRED */
    uint32_t cache_policy;          /* 40 == HELIOS_HOC1_CACHE_WRITE_COMBINED */
    uint32_t physical_adapter_mask; /* 44 == ..._MASK_NODE0 */
    uint8_t  reserved[16];          /* 48 zero */
} HeliosOuterCommandAllocationV1;

HELIOS_WDDM_STATIC_ASSERT(sizeof(HeliosOuterCommandAllocationV1) == 64,
                          "HOC1 must be the §10.6 64-byte record");
HELIOS_WDDM_STATIC_ASSERT(sizeof(HeliosOuterCommandAllocationV1) == HELIOS_HOC1_BYTES,
                          "HOC1 size constant must match the struct");
HELIOS_WDDM_STATIC_ASSERT(HELIOS_WDDM_ALIGNOF(HeliosOuterCommandAllocationV1) == 8, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosOuterCommandAllocationV1, magic) == 0, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosOuterCommandAllocationV1, abi_version) == 4, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosOuterCommandAllocationV1, struct_size) == 6, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosOuterCommandAllocationV1, package_generation) == 8, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosOuterCommandAllocationV1, allocation_generation) == 16, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosOuterCommandAllocationV1, byte_size) == 24, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosOuterCommandAllocationV1, extent_alignment) == 32, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosOuterCommandAllocationV1, access) == 36, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosOuterCommandAllocationV1, cache_policy) == 40, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosOuterCommandAllocationV1, physical_adapter_mask) == 44, "");
HELIOS_WDDM_STATIC_ASSERT(offsetof(HeliosOuterCommandAllocationV1, reserved) == 48, "");

/* C65 arithmetic that must hold for the allocator to be expressible at all. */
HELIOS_WDDM_STATIC_ASSERT(HELIOS_HOC1_POOL_BYTES % HELIOS_HOC1_EXTENT_ALIGNMENT == 0, "");
HELIOS_WDDM_STATIC_ASSERT(HELIOS_HOB1_MAX_BYTES < HELIOS_HOC1_POOL_BYTES,
                          "a whole HOB1 must fit the C65 pool");
HELIOS_WDDM_STATIC_ASSERT((uint64_t)HELIOS_HOC1_MAX_LIVE_EXTENTS *
                                  (uint64_t)HELIOS_HOC1_EXTENT_ALIGNMENT <=
                              HELIOS_HOC1_POOL_BYTES,
                          "the live-extent limit is a bookkeeping bound, never the binding one");
HELIOS_WDDM_STATIC_ASSERT(HELIOS_HOC1_ACCESS_REQUIRED ==
                              (HELIOS_HOC1_ACCESS_CPU_WRITE | HELIOS_HOC1_ACCESS_DEVICE_READ),
                          "the required access is exactly CPU write plus device read");

#if defined(__cplusplus)
}
#endif

#endif /* HELIOS_WDDM_H */
