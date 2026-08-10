#!/usr/bin/env bash
# A2's encoder gate: run the ICD's own HNR2 encoder against protocol/'s validator.
#
# Three steps, one exit code:
#   1. compile tools/hnr2_encode_probe.c together with the ICD source file
#      icd/mesa/src/virtio/vulkan/vn_helios_native_kmt.c (its encoder half is
#      platform-independent on purpose) -- with -Werror, because this is also the
#      only place that half is compiled by anything but mingw;
#   2. run it: it encodes a corpus AND asserts every refusal rule;
#   3. replay the corpus through protocol/tests/hnr2_encoder_gate.rs.
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
    echo "hnr2-encoder-gate: FAIL to compile the encoder + probe"
    exit 1
fi

if ! "$WORK/probe" "$WORK/corpus.bin"; then
    echo "hnr2-encoder-gate: FAIL the probe's own refusal assertions"
    exit 1
fi

OUT=$(HELIOS_HNR2_CORPUS="$WORK/corpus.bin" \
      cargo test --manifest-path "$REPO/protocol/Cargo.toml" \
      --test hnr2_encoder_gate -- --nocapture 2>&1)
RC=$?
if [ $RC -ne 0 ]; then
    printf '%s\n' "$OUT" | tail -30
    echo "hnr2-encoder-gate: FAIL the validator rejected the encoder's output"
    exit 1
fi

SUMMARY=$(printf '%s\n' "$OUT" | grep -E '^HNR2 corpus: [0-9]+ batches')
if [ -z "$SUMMARY" ]; then
    printf '%s\n' "$OUT" | tail -20
    echo "hnr2-encoder-gate: FAIL the corpus test did not run (it skipped)"
    exit 1
fi

BATCHES=$(printf '%s\n' "$SUMMARY" | sed -E 's/^HNR2 corpus: ([0-9]+) batches.*/\1/')
FRAGMENTS=$(printf '%s\n' "$SUMMARY" | sed -E 's/.* ([0-9]+) fragments validated/\1/')
# Floors, not exact counts: the corpus is meant to grow. They exist so a corpus
# that silently stopped being generated cannot report PASS.
if [ "$BATCHES" -lt 18 ] || [ "$FRAGMENTS" -lt 85 ]; then
    echo "hnr2-encoder-gate: FAIL corpus too small ($SUMMARY)"
    exit 1
fi

echo "$SUMMARY"
echo "hnr2-encoder-gate: PASS"
exit 0
