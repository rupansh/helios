/*
 * Copyright 2026 Helios
 * SPDX-License-Identifier: MIT
 *
 * A2's encoder gate, host half. Builds a corpus of HNR2 batches, encodes every
 * fragment with the ICD's OWN encoder (icd/mesa/src/virtio/vulkan/
 * vn_helios_native_kmt.c, whose upper half is deliberately platform-independent),
 * and writes them to a file that protocol/tests/hnr2_encoder_gate.rs replays
 * through `HeliosNativeRenderV2::validate` + `validate_commit_tables`.
 *
 * ⭐ Why this exists: the ICD has no test harness at all (lane-mesa.md §5.4) and
 * every fragment this encoder emits is validated by a Rust state machine in
 * another repository. "Both halves were written from the same doc" is not
 * evidence; running the producer against the consumer is.
 *
 * It also asserts the refusals directly -- a bad manifest must not encode.
 *
 * Build/run: tools/hnr2-encoder-gate.sh
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "vn_helios_native_kmt.h"

#define CORPUS_MAGIC   0x50524E48u /* 'HNRP' */
#define CORPUS_VERSION 1u

static FILE *out_file;
static unsigned corpus_batches;
static int failures;

static void
fail(const char *what)
{
   fprintf(stderr, "hnr2_encode_probe: FAIL %s\n", what);
   failures++;
}

static void
put(const void *bytes, size_t len)
{
   if (len && fwrite(bytes, 1, len, out_file) != len) {
      perror("fwrite");
      exit(2);
   }
}

static void
put_u16(uint16_t v)
{
   put(&v, sizeof(v));
}
static void
put_u32(uint32_t v)
{
   put(&v, sizeof(v));
}
static void
put_u64(uint64_t v)
{
   put(&v, sizeof(v));
}

/*
 * Encode one batch and append it to the corpus. `token` is the context-local
 * batch token the caller would have assigned.
 */
static void
emit(const char *name, const struct helios_hnr2_batch *b, uint64_t token)
{
   struct helios_hnr2_plan plan;
   enum helios_hnr2_encode_status st =
      helios_hnr2_plan_batch(b, HELIOS_HVC1_DMA_BUFFER_BYTES, &plan);
   if (st != HELIOS_HNR2_ENCODE_OK) {
      fprintf(stderr, "hnr2_encode_probe: FAIL plan(%s) = %s\n", name,
              helios_hnr2_encode_status_name(st));
      failures++;
      return;
   }

   put_u64(b->payload_bytes);
   put_u32(b->allocation_count);
   put_u32(b->patch_count);
   put_u32(plan.fragment_count);
   put_u32(b->has_reply ? 1u : 0u);
   put_u32(b->reply_allocation_index);
   put_u64(b->reply_offset);
   put_u64(b->reply_capacity_bytes);
   put_u64(b->reply_slot_generation);
   put_u64(token);
   put(b->payload, (size_t)b->payload_bytes);
   for (uint32_t i = 0; i < b->allocation_count; i++) {
      put_u32(b->allocations[i].handle);
      put_u32(b->allocations[i].access);
      put_u64(b->allocations[i].expected_generation);
   }
   for (uint32_t i = 0; i < b->patch_count; i++) {
      put_u32(b->patches[i].payload_offset);
      put_u32(b->patches[i].allocation_index);
      put_u16(b->patches[i].operand_kind);
      put_u16(0);
   }

   /* One command buffer of exactly the advertised floor -- the same size the
    * plan was computed against, and the smallest dxgkrnl may return. */
   static uint8_t command_buffer[HELIOS_HVC1_DMA_BUFFER_BYTES];
   for (uint16_t f = 0; f < plan.fragment_count; f++) {
      uint32_t length = 0;
      memset(command_buffer, 0xCD, sizeof(command_buffer));
      st = helios_hnr2_encode_fragment(b, &plan, f, token, command_buffer,
                                       sizeof(command_buffer), &length);
      if (st != HELIOS_HNR2_ENCODE_OK) {
         fprintf(stderr, "hnr2_encode_probe: FAIL encode(%s, %u) = %s\n", name,
                 f, helios_hnr2_encode_status_name(st));
         failures++;
         return;
      }
      put_u32(length);
      /* The exact D3DKMT_RENDER::AllocationCount the submit path would pass --
       * recorded so the Rust side validates against the real function, not
       * against its own idea of which fragment is the COMMIT. */
      put_u32(helios_hnr2_fragment_allocation_count(b, &plan, f));
      put(command_buffer, length);
   }
   corpus_batches++;
}

/* A batch whose plan must be REFUSED, and with which status. */
static void
expect_refusal(const char *name, const struct helios_hnr2_batch *b,
               enum helios_hnr2_encode_status want)
{
   struct helios_hnr2_plan plan;
   const enum helios_hnr2_encode_status got =
      helios_hnr2_plan_batch(b, HELIOS_HVC1_DMA_BUFFER_BYTES, &plan);
   if (got != want) {
      fprintf(stderr, "hnr2_encode_probe: FAIL %s: wanted %s, got %s\n", name,
              helios_hnr2_encode_status_name(want),
              helios_hnr2_encode_status_name(got));
      failures++;
   }
}

/* Deterministic filler: a payload of zeros would hide an offset slip. */
static void
fill(uint8_t *p, size_t len, uint32_t seed)
{
   uint32_t x = seed | 1u;
   for (size_t i = 0; i < len; i++) {
      x ^= x << 13;
      x ^= x >> 17;
      x ^= x << 5;
      p[i] = (uint8_t)x;
   }
}

int
main(int argc, char **argv)
{
   if (argc != 2) {
      fprintf(stderr, "usage: %s <corpus-file>\n", argv[0]);
      return 2;
   }

   if (!helios_crc64_self_test())
      fail("CRC-64/ECMA-182 check value");

   out_file = fopen(argv[1], "wb");
   if (!out_file) {
      perror(argv[1]);
      return 2;
   }
   put_u32(CORPUS_MAGIC);
   put_u32(CORPUS_VERSION);
   const long count_offset = ftell(out_file);
   put_u32(0); /* batch count, rewritten at the end */

   static uint8_t payload[16u * 1024u * 1024u];
   fill(payload, sizeof(payload), 0x9E3779B9u);

   const uint32_t floor = HELIOS_HVC1_DMA_BUFFER_BYTES;
   const uint32_t cap_nonfinal = floor - HELIOS_HNR2_HEADER_SIZE;

   struct helios_hnr2_allocation allocs[8];
   for (unsigned i = 0; i < 8; i++) {
      allocs[i].handle = 0x1000u + i;
      allocs[i].access = (i % 2) ? HELIOS_HNR2_ACCESS_READ
                                 : HELIOS_HNR2_ACCESS_WRITE;
      allocs[i].expected_generation = 0xA5A50000ull + i + 1;
   }

   struct helios_hnr2_batch b;
   uint64_t token = 0;

#define BASE(bytes)                                                            \
   do {                                                                        \
      memset(&b, 0, sizeof(b));                                                \
      b.payload = payload;                                                     \
      b.payload_bytes = (bytes);                                               \
   } while (0)

   /* 1. The smallest legal batch: one payload byte, no allocations. */
   BASE(1);
   emit("one-byte", &b, ++token);

   /* 2. A2's own fire-and-forget shape: several allocations, no reply. */
   BASE(4096);
   b.allocations = allocs;
   b.allocation_count = 4;
   emit("four-allocations", &b, ++token);

   /* 3. A1's control shape: one writable allocation carrying the reply, in
    *    every one of the four slots. */
   for (unsigned slot = 0; slot < HELIOS_HVM1_REPLY_SLOT_COUNT; slot++) {
      BASE(256);
      b.allocations = allocs; /* allocs[0] is WRITE */
      b.allocation_count = 1;
      b.has_reply = true;
      b.reply_allocation_index = 0;
      b.reply_offset = (uint64_t)slot * HELIOS_HVM1_REPLY_SLOT_BYTES;
      b.reply_capacity_bytes = HELIOS_HVR1_HEADER_SIZE + 4096;
      b.reply_slot_generation = 7 + slot;
      emit("control-reply", &b, ++token);
   }

   /* 4. Exactly one fragment's worth, and one byte past it: the boundary the
    *    greedy split is most likely to get wrong. */
   BASE(cap_nonfinal);
   emit("exactly-one-fragment-capacity", &b, ++token);
   BASE((uint64_t)cap_nonfinal + 1);
   emit("one-past-fragment-capacity", &b, ++token);
   BASE((uint64_t)cap_nonfinal * 2);
   emit("two-fragment-capacities", &b, ++token);

   /* 5. Several fragments with a COMMIT that also carries tables and a reply. */
   BASE(700u * 1024u);
   b.allocations = allocs;
   b.allocation_count = 2;
   b.has_reply = true;
   b.reply_allocation_index = 0;
   b.reply_offset = HELIOS_HVM1_REPLY_SLOT_BYTES;
   b.reply_capacity_bytes = HELIOS_HVR1_HEADER_SIZE + 65536;
   b.reply_slot_generation = 99;
   emit("multi-fragment-with-reply", &b, ++token);

   /* 6. Patches given OUT of allocation order and interleaved: the encoder must
    *    still emit contiguous per-use runs in use order. */
   {
      static struct helios_hnr2_patch_input patches[9];
      const uint32_t owners[9] = { 3, 0, 3, 1, 0, 2, 1, 3, 0 };
      for (unsigned i = 0; i < 9; i++) {
         patches[i].payload_offset = (uint32_t)(i * 8);
         patches[i].allocation_index = owners[i];
         patches[i].operand_kind = (i % 2)
                                      ? HELIOS_HNR2_OPERAND_KIND_HOST_RESOURCE_ID64
                                      : HELIOS_HNR2_OPERAND_KIND_HOST_RESOURCE_ID32;
      }
      BASE(1024);
      b.allocations = allocs;
      b.allocation_count = 4;
      b.patches = patches;
      b.patch_count = 9;
      emit("scrambled-patch-order", &b, ++token);
   }

   /* 6b. Patches into a MULTI-fragment payload: their offsets are into the
    *     reassembled payload, so an encoder that resolved them per fragment
    *     would place them at the wrong operand. */
   {
      static struct helios_hnr2_patch_input spread[6];
      const uint64_t bytes = (uint64_t)cap_nonfinal * 2 + 4096;
      for (unsigned i = 0; i < 6; i++) {
         spread[i].payload_offset = (uint32_t)((bytes / 7) * (i + 1) & ~3u);
         spread[i].allocation_index = i % 3;
         spread[i].operand_kind = HELIOS_HNR2_OPERAND_KIND_HOST_RESOURCE_ID64;
      }
      BASE(bytes);
      b.allocations = allocs;
      b.allocation_count = 3;
      b.patches = spread;
      b.patch_count = 6;
      emit("patches-across-fragments", &b, ++token);
   }

   /* 6c. A read-only manifest with no reply: WriteOperation must be 0 in both
    *     the use record and the allocation-list entry. */
   BASE(2048);
   b.allocations = &allocs[1]; /* READ */
   b.allocation_count = 1;
   emit("read-only-single", &b, ++token);

   /* 6d. A mixed manifest: patches on allocations 1 and 3 only, so 0 and 2 carry
    *     zero-length runs BETWEEN populated ones. A `first_patch` that was not
    *     advanced for an empty run still tiles [0,count) from the front and
    *     would pass a laxer check than validate_commit_tables'. */
   {
      static struct helios_hnr2_patch_input mixed[5];
      const uint32_t owners[5] = { 1, 3, 1, 3, 3 };
      for (unsigned i = 0; i < 5; i++) {
         mixed[i].payload_offset = (uint32_t)(i * 16);
         mixed[i].allocation_index = owners[i];
         mixed[i].operand_kind = HELIOS_HNR2_OPERAND_KIND_HOST_RESOURCE_ID32;
      }
      BASE(512);
      b.allocations = allocs;
      b.allocation_count = 4;
      b.patches = mixed;
      b.patch_count = 5;
      emit("empty-runs-between-populated", &b, ++token);
   }

   /* 6e. The clamp at EXACT equality: with one use record the COMMIT metadata is
    *     136 bytes, so a payload of exactly `cap_nonfinal` cannot fit one
    *     fragment, and the non-final take equals what is left -- the `>=` in the
    *     clamp, not the `>`. */
   BASE(cap_nonfinal);
   b.allocations = allocs;
   b.allocation_count = 1;
   emit("clamp-at-exact-equality", &b, ++token);

   /* 7. The metadata-heavy corner: the maximum tables leave only ~32 KiB for the
    *    COMMIT payload, so the last NON-final fragment must be clamped to leave
    *    the COMMIT at least one byte. */
   {
      static struct helios_hnr2_allocation many[HELIOS_HNR2_MAX_USE_RECORDS];
      static struct helios_hnr2_patch_input many_patches[HELIOS_HNR2_MAX_PATCH_RECORDS];
      for (uint32_t i = 0; i < HELIOS_HNR2_MAX_USE_RECORDS; i++) {
         many[i].handle = 0x2000u + i;
         many[i].access = HELIOS_HNR2_ACCESS_READ;
         many[i].expected_generation = 1 + i;
      }
      for (uint32_t i = 0; i < HELIOS_HNR2_MAX_PATCH_RECORDS; i++) {
         many_patches[i].payload_offset = (i % 1024u) * 4u;
         many_patches[i].allocation_index = i % HELIOS_HNR2_MAX_USE_RECORDS;
         many_patches[i].operand_kind = HELIOS_HNR2_OPERAND_KIND_HOST_RESOURCE_ID32;
      }

      BASE(8192);
      b.allocations = many;
      b.allocation_count = HELIOS_HNR2_MAX_USE_RECORDS;
      b.patches = many_patches;
      b.patch_count = HELIOS_HNR2_MAX_PATCH_RECORDS;
      emit("max-tables-small-payload", &b, ++token);

      /* Just past what one COMMIT can hold with those tables: two fragments,
       * the first clamped. */
      BASE(floor - 229488u + 1u);
      b.allocations = many;
      b.allocation_count = HELIOS_HNR2_MAX_USE_RECORDS;
      b.patches = many_patches;
      b.patch_count = HELIOS_HNR2_MAX_PATCH_RECORDS;
      emit("max-tables-clamped-split", &b, ++token);
   }

   /* 8. The batch bound itself: 15 MiB in the maximum 64 fragments. */
   BASE(HELIOS_HNR2_MAX_PAYLOAD_BYTES);
   b.allocations = allocs;
   b.allocation_count = 1;
   emit("max-payload", &b, ++token);

   /* ── refusals ──────────────────────────────────────────────────────────
    * Every one of these is a wire rule protocol/ enforces on the other side;
    * an encoder that emits them would be refused by the KMD instead. */
   BASE(0);
   expect_refusal("empty payload", &b, HELIOS_HNR2_ENCODE_NO_PAYLOAD);

   BASE(HELIOS_HNR2_MAX_PAYLOAD_BYTES + 1);
   expect_refusal("payload above the bound", &b,
                  HELIOS_HNR2_ENCODE_PAYLOAD_TOO_LARGE);

   {
      struct helios_hnr2_allocation bad = allocs[0];
      bad.expected_generation = 0;
      BASE(64);
      b.allocations = &bad;
      b.allocation_count = 1;
      expect_refusal("zero allocation generation", &b,
                     HELIOS_HNR2_ENCODE_BAD_ALLOCATION);

      bad = allocs[0];
      bad.access = 0;
      b.allocations = &bad;
      expect_refusal("no access bits", &b, HELIOS_HNR2_ENCODE_BAD_ALLOCATION);

      bad = allocs[0];
      bad.access = HELIOS_HNR2_ACCESS_MASK + 1;
      b.allocations = &bad;
      expect_refusal("unknown access bit", &b, HELIOS_HNR2_ENCODE_BAD_ALLOCATION);

      bad = allocs[0];
      bad.handle = 0;
      b.allocations = &bad;
      expect_refusal("null allocation handle", &b,
                     HELIOS_HNR2_ENCODE_BAD_ALLOCATION);
   }

   {
      struct helios_hnr2_patch_input p = {
         .payload_offset = 0,
         .allocation_index = 0,
         .operand_kind = HELIOS_HNR2_OPERAND_KIND_HOST_RESOURCE_ID32,
      };
      BASE(64);
      b.patches = &p;
      b.patch_count = 1;
      expect_refusal("patch with no use", &b,
                     HELIOS_HNR2_ENCODE_PATCH_WITHOUT_USE);

      b.allocations = allocs;
      b.allocation_count = 1;
      p.allocation_index = 1;
      expect_refusal("patch names a missing allocation", &b,
                     HELIOS_HNR2_ENCODE_BAD_PATCH);

      p.allocation_index = 0;
      p.operand_kind = HELIOS_HNR2_OPERAND_KIND_INVALID;
      expect_refusal("patch operand kind outside the vocabulary", &b,
                     HELIOS_HNR2_ENCODE_BAD_PATCH);

      p.operand_kind = HELIOS_HNR2_OPERAND_KIND_HOST_RESOURCE_ID32;
      p.payload_offset = 2;
      expect_refusal("misaligned patch offset", &b, HELIOS_HNR2_ENCODE_BAD_PATCH);

      p.payload_offset = 64;
      expect_refusal("patch operand past the payload", &b,
                     HELIOS_HNR2_ENCODE_BAD_PATCH);

      p.payload_offset = 60;
      p.operand_kind = HELIOS_HNR2_OPERAND_KIND_HOST_RESOURCE_ID64;
      expect_refusal("64-bit operand straddling the payload end", &b,
                     HELIOS_HNR2_ENCODE_BAD_PATCH);
   }

   {
      BASE(64);
      b.allocations = &allocs[1]; /* READ only */
      b.allocation_count = 1;
      b.has_reply = true;
      b.reply_allocation_index = 0;
      b.reply_capacity_bytes = HELIOS_HVR1_HEADER_SIZE + 16;
      b.reply_slot_generation = 1;
      expect_refusal("reply target is not writable", &b,
                     HELIOS_HNR2_ENCODE_BAD_REPLY);

      b.allocations = allocs; /* WRITE */
      b.reply_slot_generation = 0;
      expect_refusal("zero reply slot generation", &b,
                     HELIOS_HNR2_ENCODE_BAD_REPLY);

      b.reply_slot_generation = 1;
      b.reply_offset = 4; /* not 8-aligned */
      expect_refusal("misaligned reply offset", &b, HELIOS_HNR2_ENCODE_BAD_REPLY);

      b.reply_offset = HELIOS_HVM1_REPLY_SLOT_BYTES - 8;
      b.reply_capacity_bytes = HELIOS_HVR1_HEADER_SIZE + 4096;
      expect_refusal("reply range crosses a slot", &b,
                     HELIOS_HNR2_ENCODE_BAD_REPLY);

      b.reply_offset = 0;
      b.reply_capacity_bytes = HELIOS_HVR1_HEADER_SIZE - 1;
      expect_refusal("reply capacity below the HVR1 header", &b,
                     HELIOS_HNR2_ENCODE_BAD_REPLY);

      b.reply_capacity_bytes =
         HELIOS_HVR1_HEADER_SIZE + HELIOS_HVR1_MAX_CHUNK_BYTES + 1;
      expect_refusal("reply capacity above the chunk bound", &b,
                     HELIOS_HNR2_ENCODE_BAD_REPLY);

      BASE(64);
      b.reply_offset = 8; /* set without has_reply */
      expect_refusal("reply fields set without HAS_REPLY", &b,
                     HELIOS_HNR2_ENCODE_BAD_REPLY);
   }

   /* A command buffer below the advertised floor is refused, not adapted: the
    * whole plan is computed on that floor. */
   {
      BASE(64);
      struct helios_hnr2_plan plan;
      const enum helios_hnr2_encode_status got =
         helios_hnr2_plan_batch(&b, HELIOS_HVC1_DMA_BUFFER_BYTES - 1, &plan);
      if (got != HELIOS_HNR2_ENCODE_BUFFER_TOO_SMALL)
         fail("command buffer below the advertised floor");
   }

   /* 64 fragments is the hard bound: one more must refuse rather than truncate.
    * Payload 15 MiB fits in 61; force the count up with the maximum tables. */
   {
      static struct helios_hnr2_allocation many[HELIOS_HNR2_MAX_USE_RECORDS];
      for (uint32_t i = 0; i < HELIOS_HNR2_MAX_USE_RECORDS; i++) {
         many[i].handle = 0x3000u + i;
         many[i].access = HELIOS_HNR2_ACCESS_READ;
         many[i].expected_generation = 1 + i;
      }
      BASE(HELIOS_HNR2_MAX_PAYLOAD_BYTES);
      b.allocations = many;
      b.allocation_count = HELIOS_HNR2_MAX_USE_RECORDS;
      struct helios_hnr2_plan plan;
      const enum helios_hnr2_encode_status got =
         helios_hnr2_plan_batch(&b, HELIOS_HVC1_DMA_BUFFER_BYTES, &plan);
      /* 15 MiB of payload at 262,032 per fragment is 61 fragments even with the
       * COMMIT clamped, so this must PLAN, and the count must stay in bounds. */
      if (got != HELIOS_HNR2_ENCODE_OK)
         fail("max payload with a full use table should still plan");
      else if (plan.fragment_count > HELIOS_HNR2_MAX_FRAGMENTS)
         fail("fragment count above the §10.7 bound");
   }

   fseek(out_file, count_offset, SEEK_SET);
   put_u32(corpus_batches);
   fclose(out_file);

   printf("hnr2_encode_probe: %u batches encoded, %d failures\n", corpus_batches,
          failures);
   return failures ? 1 : 0;
}
