#!/usr/bin/env bash
# K5's session gate: build HTS1/HQA1 records with the C mirror the ICD compiles,
# and replay them through the KMD's own session state machine in Rust.
#
# Three steps, one exit code:
#   1. compile tools/hts1_attach_probe.c against protocol/include/ -- with
#      -Werror, which also evaluates every _Static_assert in the two headers it
#      pulls in;
#   2. run it: it writes a corpus of well-formed records and deliberate
#      mutations, each carrying the verdict the guest side expects;
#   3. replay the corpus through kmd_logic/tests/hts1_attach_gate.rs.
#
# ⛔ Step 3 SKIPS when HELIOS_HTS1_CORPUS is unset, so this script checks the
# counts the test prints. A gate that can pass on an empty corpus is not a gate.

set -u -o pipefail

cd "$(dirname "$0")/.." || exit 1
REPO=$PWD
WORK=$(mktemp -d) || exit 1
trap 'rm -rf "$WORK"' EXIT

CC=${CC:-cc}

if ! "$CC" -std=c11 -O1 -Wall -Wextra -Werror \
     -I"$REPO/protocol/include" \
     -o "$WORK/probe" \
     "$REPO/tools/hts1_attach_probe.c"; then
    echo "hts1-attach-gate: FAIL to compile the probe against protocol/include"
    exit 1
fi

if ! "$WORK/probe" "$WORK/corpus.bin"; then
    echo "hts1-attach-gate: FAIL the probe did not emit a corpus"
    exit 1
fi

OUT=$(HELIOS_HTS1_CORPUS="$WORK/corpus.bin" \
      cargo test --manifest-path "$REPO/kmd_logic/Cargo.toml" \
      --test hts1_attach_gate -- --nocapture 2>&1)
RC=$?
if [ $RC -ne 0 ]; then
    printf '%s\n' "$OUT" | tail -30
    echo "hts1-attach-gate: FAIL the KMD session disagreed with the guest"
    exit 1
fi

SUMMARY=$(printf '%s\n' "$OUT" | grep -E '^HTS1 corpus: [0-9]+ cases replayed')
if [ -z "$SUMMARY" ]; then
    printf '%s\n' "$OUT" | tail -20
    echo "hts1-attach-gate: FAIL the corpus test did not run (it skipped)"
    exit 1
fi

CASES=$(printf '%s\n' "$SUMMARY" | sed -E 's/^HTS1 corpus: ([0-9]+) cases.*/\1/')
ADMITTED=$(printf '%s\n' "$SUMMARY" | sed -E 's/.*, ([0-9]+) admitted.*/\1/')
REFUSED=$(printf '%s\n' "$SUMMARY" | sed -E 's/.*, ([0-9]+) refused.*/\1/')
# Floors, not exact counts: the corpus is meant to grow. They exist so a corpus
# that silently stopped being generated cannot report PASS.
if [ "$CASES" -lt 38 ] || [ "$ADMITTED" -lt 6 ] || [ "$REFUSED" -lt 30 ]; then
    echo "hts1-attach-gate: FAIL corpus too small ($SUMMARY)"
    exit 1
fi

echo "$SUMMARY"
echo "hts1-attach-gate: PASS"
exit 0
