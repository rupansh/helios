#!/usr/bin/env bash
# K6's decode gate: run the ICD's own HNR2 encoder against the KMD's own assembler.
#
# The sibling of tools/hnr2-encoder-gate.sh, and it shares the producer on
# purpose: the corpus is A2's real output, not a re-implementation. Where the
# encoder gate replays it through `protocol`'s validators, this replays it
# through `helios_kmd_logic::native_render::RenderContext` — the state machine
# `kmd_render` will run, which builds only on the VM and can therefore be tested
# nowhere else.
#
# Three steps, one exit code:
#   1. compile tools/hnr2_encode_probe.c together with the ICD source file
#      icd/mesa/src/virtio/vulkan/vn_helios_native_kmt.c, with -Werror;
#   2. run it to produce the corpus;
#   3. replay the corpus through kmd_logic/tests/hnr2_decode_gate.rs.
#
# ⛔ Step 3 SKIPS when HELIOS_HNR2_CORPUS is unset, so this script checks the
# counts the test prints. A gate that can pass on an empty corpus is not a gate.

set -u -o pipefail

cd "$(dirname "$0")/.." || exit 1
REPO=$PWD
WORK=$(mktemp -d) || exit 1
trap 'rm -rf "$WORK"' EXIT

CC=${CC:-cc}

if ! "$CC" -std=c11 -O1 -Wall -Wextra -Werror -Wno-unused-parameter \
     -I"$REPO/protocol/include" \
     -I"$REPO/icd/mesa/src/virtio/vulkan" \
     -o "$WORK/probe" \
     "$REPO/tools/hnr2_encode_probe.c" \
     "$REPO/icd/mesa/src/virtio/vulkan/vn_helios_native_kmt.c"; then
    echo "hnr2-decode-gate: FAIL to compile the encoder + probe"
    exit 1
fi

if ! "$WORK/probe" "$WORK/corpus.bin"; then
    echo "hnr2-decode-gate: FAIL the probe's own refusal assertions"
    exit 1
fi

OUT=$(HELIOS_HNR2_CORPUS="$WORK/corpus.bin" \
      CARGO_TARGET_DIR="$REPO/target/linux" \
      cargo test --manifest-path "$REPO/kmd_logic/Cargo.toml" \
      --test hnr2_decode_gate -- --nocapture 2>&1)
RC=$?
if [ $RC -ne 0 ]; then
    printf '%s\n' "$OUT" | tail -30
    echo "hnr2-decode-gate: FAIL the KMD assembler refused the encoder's output"
    exit 1
fi

SUMMARY=$(printf '%s\n' "$OUT" | grep -E '^HNR2 decode: [0-9]+ batches')
if [ -z "$SUMMARY" ]; then
    printf '%s\n' "$OUT" | tail -20
    echo "hnr2-decode-gate: FAIL the decode test did not run (it skipped)"
    exit 1
fi

BATCHES=$(printf '%s\n' "$SUMMARY" | sed -E 's/^HNR2 decode: ([0-9]+) batches.*/\1/')
FRAGMENTS=$(printf '%s\n' "$SUMMARY" | sed -E 's/.*, ([0-9]+) fragments,.*/\1/')
COMMITS=$(printf '%s\n' "$SUMMARY" | sed -E 's/.*, ([0-9]+) commits,.*/\1/')
SLOTS=$(printf '%s\n' "$SUMMARY" | sed -E 's/.*, ([0-9]+) output patch slots planned,.*/\1/')
MUTATIONS=$(printf '%s\n' "$SUMMARY" | sed -E 's/.*, ([0-9]+) write-bit mutations refused,.*/\1/')
SHORTBUF=$(printf '%s\n' "$SUMMARY" | sed -E 's/.*, ([0-9]+) short-DMA-buffer refusals,.*/\1/')
INTERLEAVED=$(printf '%s\n' "$SUMMARY" | sed -E 's/.* ([0-9]+) interleaved fragments/\1/')
# Floors, not exact counts: the corpus is meant to grow. They exist so a corpus
# that silently stopped being generated cannot report PASS. Kept in step with
# tools/hnr2-encoder-gate.sh, which shares the producer.
if [ "$BATCHES" -lt 18 ] || [ "$FRAGMENTS" -lt 85 ] || [ "$COMMITS" -lt 18 ] \
   || [ "$SLOTS" -lt 1 ] || [ "$MUTATIONS" -lt 1 ] || [ "$SHORTBUF" -lt 1 ] \
   || [ "$INTERLEAVED" -lt 4 ]; then
    echo "$SUMMARY"
    echo "hnr2-decode-gate: FAIL the corpus is too small to be a real corpus" \
         "(want >=18 batches, >=85 fragments, >=18 commits, >=1 patch slot," \
         ">=1 write-bit mutation, >=1 short-buffer refusal, >=4 interleaved fragments)"
    exit 1
fi

echo "$SUMMARY"
echo "hnr2-decode-gate: PASS"
