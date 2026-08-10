/*
 * K5's session gate, guest half: build HTS1 INIT and HQA1 records from
 * protocol/include/helios_translation_session.h — the same header the ICD
 * compiles — and emit them, each with the verdict the guest side expects, for
 * kmd_logic/tests/hts1_attach_gate.rs to replay through the KMD's own session
 * state machine.
 *
 * ⛔ It is a gate because the two halves are written in different languages
 * against different declarations of the same records. A field that moved would
 * pass the C `_Static_assert`s and the Rust `offset_of!` asserts separately and
 * still fail here, because these bytes are produced by one and consumed by the
 * other.
 *
 * Copyright 2026 Helios
 * SPDX-License-Identifier: MIT
 */

#include <stdint.h>
#include <stdio.h>
#include <string.h>

#include "helios_native_render.h"
#include "helios_translation_session.h"

/* The values the corpus is built around. The Rust side is told all of them, so a
 * skew between the two halves shows up as a refusal rather than as a silently
 * different scenario.
 *
 * ⛔ The package generation and the capset are the REAL constants, taken from the
 * headers. They were invented literals (0x2026... and 30) until a review pointed
 * out that a corpus admitting on values no component uses proves nothing about
 * the values every component does use — the two halves agreed with each other
 * and with nothing else. */
#define PKG_GENERATION HELIOS_PACKAGE_GENERATION
#define CAPSET HELIOS_NATIVE_RENDER_CAPSET
#define SESSION_GENERATION 0x00000000000000A7ull
#define CAP_LOW 0x0123456789ABCDEFull
#define CAP_HIGH 0xFEDCBA9876543210ull
#define ENDPOINT_CAPACITY 4u

/* Case kinds. */
#define CASE_INIT 1u
#define CASE_ATTACH 2u

/* Per-case flags. */
#define CASE_RESET 1u  /* rebuild a fresh live session before this case */
#define CASE_ADMIT 2u  /* the guest expects the KMD to admit it */

static FILE *out;
static uint32_t case_count;
static long count_slot;

static void
put_u32(uint32_t v)
{
   unsigned char b[4] = { (unsigned char)(v), (unsigned char)(v >> 8),
                          (unsigned char)(v >> 16), (unsigned char)(v >> 24) };
   fwrite(b, 1, 4, out);
}

static void
emit(uint32_t kind, uint32_t flags, const char *name, const void *bytes,
     uint32_t len)
{
   uint32_t name_len = (uint32_t)strlen(name);
   put_u32(kind);
   put_u32(flags);
   put_u32(name_len);
   fwrite(name, 1, name_len, out);
   put_u32(len);
   fwrite(bytes, 1, len, out);
   case_count++;
}

/* ── the well-formed records ─────────────────────────────────────────────── */

static HeliosTranslationSessionInitV1
good_init(void)
{
   HeliosTranslationSessionInitV1 r;
   memset(&r, 0, sizeof(r));
   r.magic = HELIOS_HTS1_INIT_MAGIC;
   r.abi_version = (uint16_t)HELIOS_HTS1_ABI_VERSION;
   r.struct_size = (uint16_t)HELIOS_HTS1_INIT_SIZE;
   r.package_generation = PKG_GENERATION;
   r.capset = CAPSET;
   r.requested_endpoint_capacity = ENDPOINT_CAPACITY;
   return r;
}

static HeliosQueueAttachV1
good_attach(uint32_t endpoint_id, uint64_t context_generation)
{
   HeliosQueueAttachV1 r;
   memset(&r, 0, sizeof(r));
   r.magic = HELIOS_HQA1_MAGIC;
   r.abi_version = (uint16_t)HELIOS_HQA1_ABI_VERSION;
   r.struct_size = (uint16_t)HELIOS_HQA1_SIZE;
   r.package_generation = PKG_GENERATION;
   r.session_generation = SESSION_GENERATION;
   r.capability_low = CAP_LOW;
   r.capability_high = CAP_HIGH;
   r.endpoint_id = endpoint_id;
   r.engine_class = HELIOS_ENGINE_CLASS_GRAPHICS;
   r.queue_family = 0;
   r.queue_index = 0;
   r.context_generation = context_generation;
   r.flags = HELIOS_HQA1_FLAG_D3D11_PHYSICAL;
   return r;
}

#define EMIT_INIT(flags, name, mutate)                                         \
   do {                                                                        \
      HeliosTranslationSessionInitV1 r = good_init();                          \
      mutate;                                                                  \
      emit(CASE_INIT, (flags), (name), &r, (uint32_t)sizeof(r));               \
   } while (0)

#define EMIT_ATTACH(flags, name, mutate)                                       \
   do {                                                                        \
      HeliosQueueAttachV1 r = good_attach(1, 10);                              \
      mutate;                                                                  \
      emit(CASE_ATTACH, (flags), (name), &r, (uint32_t)sizeof(r));             \
   } while (0)

int
main(int argc, char **argv)
{
   if (argc != 2) {
      fprintf(stderr, "usage: %s <corpus.bin>\n", argv[0]);
      return 2;
   }
   out = fopen(argv[1], "wb");
   if (!out) {
      perror("fopen");
      return 2;
   }

   fwrite("HTS1CORP", 1, 8, out);
   put_u32(1); /* corpus format version */
   put_u32(PKG_GENERATION & 0xFFFFFFFFu);
   put_u32((uint32_t)(PKG_GENERATION >> 32));
   put_u32(CAPSET);
   put_u32(SESSION_GENERATION & 0xFFFFFFFFu);
   put_u32((uint32_t)(SESSION_GENERATION >> 32));
   put_u32(CAP_LOW & 0xFFFFFFFFu);
   put_u32((uint32_t)(CAP_LOW >> 32));
   put_u32(CAP_HIGH & 0xFFFFFFFFu);
   put_u32((uint32_t)(CAP_HIGH >> 32));
   put_u32(ENDPOINT_CAPACITY);
   count_slot = ftell(out);
   put_u32(0); /* patched with case_count on the way out */

   /* ── HTS1 INIT ────────────────────────────────────────────────────────── */

   EMIT_INIT(CASE_RESET | CASE_ADMIT, "init/well-formed", (void)0);
   EMIT_INIT(CASE_RESET, "init/bad-magic", r.magic ^= 1u);
   EMIT_INIT(CASE_RESET, "init/bad-abi", r.abi_version = 2);
   EMIT_INIT(CASE_RESET, "init/bad-size", r.struct_size = 31);
   EMIT_INIT(CASE_RESET, "init/reserved-nonzero", r.reserved = 1);
   EMIT_INIT(CASE_RESET, "init/package-zero", r.package_generation = 0);
   EMIT_INIT(CASE_RESET, "init/package-foreign",
             r.package_generation = PKG_GENERATION + 1);
   EMIT_INIT(CASE_RESET, "init/capset-foreign", r.capset = CAPSET + 1);
   EMIT_INIT(CASE_RESET, "init/capacity-zero", r.requested_endpoint_capacity = 0);
   EMIT_INIT(CASE_RESET, "init/capacity-above-max",
             r.requested_endpoint_capacity = HELIOS_HTS1_MAX_ENDPOINTS_PER_SESSION + 1);

   /* ── HQA1 shape ───────────────────────────────────────────────────────── */

   EMIT_ATTACH(CASE_RESET | CASE_ADMIT, "attach/well-formed-d3d11", (void)0);
   EMIT_ATTACH(CASE_RESET | CASE_ADMIT, "attach/well-formed-d3d12",
               r.flags = HELIOS_HQA1_FLAG_D3D12_VIRTUAL);
   EMIT_ATTACH(CASE_RESET, "attach/bad-magic", r.magic ^= 1u);
   EMIT_ATTACH(CASE_RESET, "attach/bad-abi", r.abi_version = 2);
   EMIT_ATTACH(CASE_RESET, "attach/bad-size", r.struct_size = 71);
   EMIT_ATTACH(CASE_RESET, "attach/reserved-nonzero", r.reserved = 1);
   EMIT_ATTACH(CASE_RESET, "attach/flags-none", r.flags = 0);
   EMIT_ATTACH(CASE_RESET, "attach/flags-both", r.flags = HELIOS_HQA1_FLAGS_MASK);
   EMIT_ATTACH(CASE_RESET, "attach/flags-unknown-bit", r.flags |= 1u << 5);

   /* ── HQA1 generations and the capability ──────────────────────────────── */

   EMIT_ATTACH(CASE_RESET, "attach/package-foreign",
               r.package_generation = PKG_GENERATION + 1);
   EMIT_ATTACH(CASE_RESET, "attach/session-zero", r.session_generation = 0);
   EMIT_ATTACH(CASE_RESET, "attach/session-foreign",
               r.session_generation = SESSION_GENERATION + 1);
   EMIT_ATTACH(CASE_RESET, "attach/capability-absent",
               do { r.capability_low = 0; r.capability_high = 0; } while (0));
   EMIT_ATTACH(CASE_RESET, "attach/capability-low-flipped", r.capability_low ^= 1u);
   EMIT_ATTACH(CASE_RESET, "attach/capability-high-flipped", r.capability_high ^= 1u);
   EMIT_ATTACH(CASE_RESET, "attach/context-generation-zero", r.context_generation = 0);

   /* ── HQA1 endpoint cross-checks ───────────────────────────────────────── */

   EMIT_ATTACH(CASE_RESET, "attach/endpoint-zero", r.endpoint_id = 0);
   EMIT_ATTACH(CASE_RESET, "attach/endpoint-above-capacity",
               r.endpoint_id = ENDPOINT_CAPACITY + 1);
   EMIT_ATTACH(CASE_RESET, "attach/engine-class-zero", r.engine_class = 0);
   EMIT_ATTACH(CASE_RESET, "attach/engine-class-unknown", r.engine_class = 99);
   EMIT_ATTACH(CASE_RESET, "attach/control-sentinel-ordinals",
               do { r.queue_family = 0xFFFFFFFFu; r.queue_index = 0xFFFFFFFFu; } while (0));

   /* ── Sequenced cases: these depend on the case before them ────────────── */

   EMIT_ATTACH(CASE_RESET | CASE_ADMIT, "seq/declare-endpoint-1", (void)0);
   EMIT_ATTACH(0, "seq/same-endpoint-other-engine-class",
               do { r.context_generation = 11; r.engine_class = HELIOS_ENGINE_CLASS_COMPUTE; } while (0));
   EMIT_ATTACH(0, "seq/same-endpoint-other-queue-family",
               do { r.context_generation = 11; r.queue_family = 3; } while (0));
   EMIT_ATTACH(0, "seq/same-endpoint-other-queue-index",
               do { r.context_generation = 11; r.queue_index = 3; } while (0));
   EMIT_ATTACH(0, "seq/context-generation-repeated", (void)0);
   EMIT_ATTACH(0, "seq/context-generation-below-watermark",
               r.context_generation = 9);
   EMIT_ATTACH(CASE_ADMIT, "seq/context-generation-strictly-greater",
               r.context_generation = 11);
   EMIT_ATTACH(0, "seq/context-generation-repeats-the-new-watermark",
               r.context_generation = 11);
   EMIT_ATTACH(CASE_ADMIT, "seq/second-endpoint-declares-its-own-class",
               do {
                  r.endpoint_id = 2;
                  r.engine_class = HELIOS_ENGINE_CLASS_COMPUTE;
                  r.queue_family = 1;
                  r.context_generation = 12;
               } while (0));
   EMIT_ATTACH(0, "seq/first-endpoint-unchanged-by-the-second",
               do { r.engine_class = HELIOS_ENGINE_CLASS_COMPUTE; r.context_generation = 13; } while (0));

   fseek(out, count_slot, SEEK_SET);
   put_u32(case_count);
   fclose(out);
   printf("HTS1 corpus: %u cases\n", case_count);
   return 0;
}
