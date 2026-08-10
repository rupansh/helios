#!/usr/bin/env bash
# The HPS2 retirement's Linux-side mechanical gates, as ONE exit code.
#
# docs/dx12/METHOD.md section 3 criterion 3: "Every mechanical check passes as an
# exit code, not as a human's reading. A grep check has twice counted its own
# documentation here — a check that is not an exit code is an opinion."
#
# Two of the checks below were run by hand during the 2026-08-10 translator
# dispatch repair, found real defects, and were then not checked in. The next
# session re-derived one of them from scratch. That is the whole reason this
# file exists: a gate that lives in a transcript is not a gate.
#
# Everything here runs on the LINUX host with no VM and no WDK. The VM-only
# lanes (kmd_render, umd, umd12, icd/mesa) cannot be gated from here — see
# docs/retirement/OWNERSHIP.md section 6.
#
# Usage:  tools/retirement-gates.sh [--quiet]
# Exit:   0 all gates pass, 1 otherwise (the failing gate names itself)

set -u -o pipefail

cd "$(dirname "$0")/.." || exit 1
REPO=$PWD
QUIET=${1:-}
FAILED=()

run_gate() {
    local name=$1; shift
    local out rc
    out=$("$@" 2>&1); rc=$?
    if [ $rc -eq 0 ]; then
        printf 'PASS  %s\n' "$name"
        # Show the lines that carry the RESULT, not the last lines of output.
        # `tail -3` on a cargo run shows the doc-test block, which reads
        # "0 passed" while 139 unit tests passed above it — a display that
        # invites exactly the misreading this file exists to prevent.
        # ⚠ The doc-test exclusion must anchor on "ok. 0 passed", NOT on
        # "0 passed": `140 passed` CONTAINS `0 passed` as a substring, so the
        # unanchored form silently hid the real result line for any count
        # ending in zero. It read correctly at 139 and 189 and went blind at
        # 140 — a filter that is right for the counts you happen to have is
        # the same class of defect as a gate that is not an exit code.
        [ "$QUIET" = "--quiet" ] || printf '%s\n' "$out" \
            | grep -E 'test result:|^OK:|^all mirrors|^up to date' \
            | grep -v 'test result: ok\. 0 passed' \
            | sed 's/^/      /'
    else
        printf 'FAIL  %s  (exit %d)\n' "$name" $rc
        printf '%s\n' "$out" | sed 's/^/      /' | tail -25
        FAILED+=("$name")
    fi
}

# ── protocol: the wire ABI is the one thing every other lane consumes.
run_gate "protocol unit tests" \
    env CARGO_TARGET_DIR="$REPO/protocol/target/linux" \
    cargo test --quiet --manifest-path "$REPO/protocol/Cargo.toml"

# The C mirrors are hand-maintained. A hand-maintained mirror needs a
# mechanical comparison, not a careful reader.
run_gate "protocol Rust<->C ABI parity" \
    python3 "$REPO/protocol/tools/abi_parity.py"

# The mirrors must also still compile as C. `abi_parity.py` reads text; this
# makes the compiler evaluate every _Static_assert in them.
run_gate "protocol C mirrors compile (all _Static_asserts evaluated)" \
    bash -c 'printf "%s\n" \
        "#include \"helios_diagnostics.h\"" \
        "#include \"helios_native_render.h\"" \
        "#include \"helios_translation_session.h\"" \
        "#include \"helios_translator_dispatch.h\"" \
        "#include \"helios_wddm.h\"" \
        "int main(void){return 0;}" \
      | gcc -I '"$REPO"'/protocol/include -fsyntax-only -x c - && echo "all mirrors compile"'

# ── kmd_render: the KMD's testable pure logic (kmd_render itself is a
# panic=abort no_std cdylib and cannot host a libtest harness).
run_gate "kmd_logic unit tests" \
    env CARGO_TARGET_DIR="$REPO/kmd_logic/target/linux" \
    cargo test --quiet --manifest-path "$REPO/kmd_logic/Cargo.toml"

# The slot audit's generated Rust must still be what its generator emits.
# On 2026-08-10 it was not, and following the file's own "regenerate with"
# instruction would have shipped a driver that refuses to load.
if [ -f "$REPO/tmp/wdk-28000/Include/10.0.28000.0/km/dispmprt.h" ]; then
    run_gate "WDDM 3.2 slot audit is not stale vs its generator" \
        python3 "$REPO/kmd_render/tools/gen_wddm32_slot_audit.py" --check
else
    printf 'SKIP  WDDM 3.2 slot audit staleness (tmp/wdk-28000 headers absent)\n'
    printf '      This gate needs the staged WDK 28000 headers; see FINDINGS.md F1.\n'
fi

printf '\n'
if [ ${#FAILED[@]} -eq 0 ]; then
    printf 'ALL LINUX-SIDE RETIREMENT GATES PASS\n'
    exit 0
fi
printf 'FAILED: %s\n' "${FAILED[*]}"
exit 1
