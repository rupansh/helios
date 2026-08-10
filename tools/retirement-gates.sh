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
        # Prefer a recognised result line; fall back to the last non-empty line
        # so a NEW gate's summary is never silently dropped just because nobody
        # added its pattern here. This display has already hidden two results.
        if [ "$QUIET" != "--quiet" ]; then
            local shown
            shown=$(printf '%s\n' "$out" \
                | grep -E 'test result:|^OK:|^all mirrors|^up to date' \
                | grep -v 'test result: ok\. 0 passed')
            [ -n "$shown" ] || shown=$(printf '%s\n' "$out" | grep -v '^[[:space:]]*$' | tail -1)
            [ -z "$shown" ] || printf '%s\n' "$shown" | sed 's/^/      /'
        fi
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

# ── K4-CONTRACT.md §8 rows 5 and 6.
#
# Both rows say their evidence is "a grep gate in tools/retirement-gates.sh".
# Neither existed until 2026-08-10, so two of the nine acceptance obligations
# were backed by a review agent's reading — which is the exact thing the header
# of this file says is not a gate. They are `python3`, not `grep`, for one
# reason: both rules turn on the difference between LIVE CODE and a COMMENT, and
# `grep` cannot see that difference. The retired symbols are argued to survive as
# tombstones (the argument for the deletion plus its successor), so a plain
# `grep -c` over the tree would fail on prose that is deliberately there, and the
# obvious `grep -v '^\s*//'` patch is wrong on block comments, on trailing
# comments, and on `//` inside a string.
#
# ⚠ WHAT THESE TWO GATES CAN AND CANNOT CATCH. They are textual. They read
# `kmd_render` — which does not build on Linux at all — so they are the ONLY
# mechanical statement about that crate available on this host, and they are
# weaker than a compiler by construction:
#   * CAN: a restamp reintroduced in `DxgkDdiOpenAllocation` or in anything it
#     hands the private pointer to; a callee whose signature takes the buffer as
#     `*mut`; a `*mut` cast of the buffer; an assignment into it; any of the
#     write intrinsics; a retired symbol back as a type, a call, or a string.
#   * CANNOT: a write two calls deep (the flow is followed exactly ONE level, by
#     name); a write through a pointer stashed in a struct at create time and
#     dereferenced at open; a write emitted by a macro; a write in a callee
#     defined outside `kmd_render/src`. The one-level limit is enforced rather
#     than assumed: a callee this gate cannot find or cannot prove `*const` is a
#     FAILURE, not a skip, so the blind spot has to be opened deliberately.
#   * Each gate also fails when it stops seeing anything (no private-buffer
#     reference in the DDI; no tombstone anywhere). A gate that passes because it
#     went blind is the failure mode this whole file exists to prevent.

# The shared prelude: classify every byte of a Rust file as code / comment /
# string. Nested block comments, raw strings with any hash count, byte strings,
# and the lifetime-vs-char-literal ambiguity are all handled — a scanner that
# mishandles `'a` swallows the rest of the file and reports a clean tree.
K4_RUST_MASK_PY=$(cat <<'PY'
import os, re, sys

def rust_kinds(src):
    n = len(src)
    kind = bytearray(b'c' * n)          # 'c' code, '#' (35) comment, 's' (115) literal
    ident = set('abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_')
    i = 0
    while i < n:
        c = src[i]
        if c == '/' and i + 1 < n and src[i + 1] == '/':
            j = src.find('\n', i)
            j = n if j < 0 else j
            for k in range(i, j):
                kind[k] = 35
            i = j
            continue
        if c == '/' and i + 1 < n and src[i + 1] == '*':
            depth, j = 1, i + 2
            while j < n and depth > 0:
                if src[j] == '/' and j + 1 < n and src[j + 1] == '*':
                    depth += 1
                    j += 2
                elif src[j] == '*' and j + 1 < n and src[j + 1] == '/':
                    depth -= 1
                    j += 2
                else:
                    j += 1
            for k in range(i, min(j, n)):
                kind[k] = 35
            i = j
            continue
        if c == 'r' and (i == 0 or src[i - 1] not in ident or src[i - 1] == 'b'):
            j = i + 1
            while j < n and src[j] == '#':
                j += 1
            if j < n and src[j] == '"':
                close = '"' + '#' * (j - i - 1)
                e = src.find(close, j + 1)
                e = n if e < 0 else e + len(close)
                for k in range(i, e):
                    kind[k] = 115
                i = e
                continue
        if c == '"':
            j = i + 1
            while j < n:
                if src[j] == '\\':
                    j += 2
                    continue
                if src[j] == '"':
                    j += 1
                    break
                j += 1
            for k in range(i, min(j, n)):
                kind[k] = 115
            i = j
            continue
        if c == "'":
            if i + 1 < n and src[i + 1] == '\\':
                j = i + 2
                while j < n and src[j] != "'":
                    j += 2 if src[j] == '\\' else 1
                j = min(j + 1, n)
            elif i + 2 < n and src[i + 2] == "'":
                j = i + 3
            else:
                i += 1                  # a lifetime, not a char literal
                continue
            for k in range(i, j):
                kind[k] = 115
            i = j
            continue
        i += 1
    return kind

def blank_comments(src, kind):
    return ''.join(' ' if kind[i] == 35 else src[i] for i in range(len(src)))

def rust_files(root):
    out = []
    for base, _d, names in os.walk(root):
        for name in names:
            if name.endswith('.rs'):
                out.append(os.path.join(base, name))
    return sorted(out)
PY
)

# §8 row 5 — "DxgkDdiOpenAllocation writes NO byte of the private buffer".
K4_OBLIGATION5_PY=$(cat <<'PY'
REPO = sys.argv[1]
ROOT = REPO + '/kmd_render/src'
PATH = ROOT + '/ddi/create_allocation.rs'
BUF = 'pPrivateDriverData'
DDI = 'dxgkddi_open_allocation'
WRITE_INTRINSICS = [
    'copy_nonoverlapping', 'write_bytes', 'write_volatile', 'write_unaligned',
    'ptr::write', 'copy_from_slice', 'clone_from_slice', 'from_raw_parts_mut',
    'write_open_identity',
]

codes = {}
for f in rust_files(ROOT):
    s = open(f).read()
    codes[f] = blank_comments(s, rust_kinds(s))
code = codes[PATH]

m = re.search(r'^pub unsafe extern "C" fn ' + DDI + r'\(', code, re.M)
if not m:
    sys.exit('cannot find ' + DDI + ' -- the DDI was renamed or moved; FIX THIS GATE, do not delete it')

def body_from(text, start):
    e = text.find('\n}\n', start)
    return text[start:(len(text) - 3 if e < 0 else e) + 2]

regions = [(PATH, DDI, m.start(), body_from(code, m.start()))]

# Follow the buffer exactly one level: every call in the DDI body whose argument
# list names the buffer POINTER (its length is not a buffer and is not followed).
seen = set()
for cm in re.finditer(r'(^|[^.\w])([A-Za-z_][A-Za-z0-9_]*)[ \t\n]*\(', regions[0][3]):
    name = cm.group(2)
    if name in ('if', 'while', 'for', 'match', 'return', 'unsafe', 'fn') or name in seen:
        continue
    region = regions[0][3]
    d, j = 0, cm.end() - 1
    while j < len(region):
        if region[j] == '(':
            d += 1
        elif region[j] == ')':
            d -= 1
            if d == 0:
                break
        j += 1
    if BUF not in region[cm.end():j]:
        continue
    seen.add(name)
    if name in WRITE_INTRINSICS:
        sys.exit('OBLIGATION 5 VIOLATED: the open path hands the private buffer straight to `' + name + '`')
    hit = None
    for f, c in codes.items():
        fm = re.search(r'^[a-z ]*fn ' + name + r'\(', c, re.M)
        if fm:
            hit = (f, c, fm)
            break
    if hit is None:
        sys.exit('the open path hands the private buffer to `' + name + '`, whose definition this gate cannot find under kmd_render/src. It cannot vouch for a callee it cannot read -- FIX THIS GATE (or bring the callee back in-tree), do not delete it')
    f, c, fm = hit
    sig = c[fm.start():c.find(')', fm.end())]
    if '*mut' in sig:
        sys.exit('OBLIGATION 5 VIOLATED: the open path passes the private buffer to `' + name + '`, which takes it as `*mut`: ' + ' '.join(sig.split()))
    regions.append((f, name, fm.start(), body_from(c, fm.start())))

bad = []
refs = 0
for f, name, start, text in regions:
    base = codes[f].count('\n', 0, start) + 1
    for off, ln in enumerate(text.split('\n')):
        n = base + off
        for w in WRITE_INTRINSICS:
            if w in ln:
                bad.append('%s:%d: write intrinsic `%s` in %s -- %s' % (f, n, w, name, ln.strip()))
        # In the DDI body `*mut` is LEGAL: DXGKARG_OPENALLOCATION's OUT fields
        # (hDeviceSpecificAllocation, Pitch, SubresourceOffset) are written
        # through one, and the DDI says so in its own SAFETY comment. So there it
        # is banned only on a line that also names the private buffer. Inside a
        # followed callee it is banned outright -- that callee took the buffer as
        # `*const`, so manufacturing a `*mut` is exactly the laundering step.
        if BUF in ln:
            refs += 1
        if '*mut' in ln and (name != DDI or BUF in ln):
            bad.append('%s:%d: `*mut` reachable from the private buffer in %s -- %s' % (f, n, name, ln.strip()))
        if BUF not in ln:
            continue
        a = re.search(r'[^=!<>+\-*/&|^]=[^=]', ln)
        if a and BUF in ln[:a.start() + 1] and 'let ' not in ln[:a.start()]:
            bad.append('%s:%d: assignment INTO the private buffer in %s -- %s' % (f, n, name, ln.strip()))

if refs == 0 or len(regions) < 2:
    sys.exit('the gate found no private-buffer flow in %s (refs=%d, followed=%d). Either the DDI stopped reading private data or the gate went blind -- FIX THIS GATE, do not delete it' % (DDI, refs, len(regions) - 1))
if bad:
    sys.exit('OBLIGATION 5 VIOLATED -- DxgkDdiOpenAllocation writes the allocation-private buffer:\n' + '\n'.join(bad))
print('OK: %s + %d const-only callee(s) [%s]: %d %s references, no write intrinsic, no *mut, no assignment' % (DDI, len(regions) - 1, ', '.join(r[1] for r in regions[1:]), refs, BUF))
PY
)

run_gate "K4 §8.5: DxgkDdiOpenAllocation writes no byte of the private buffer" \
    python3 -c "$K4_RUST_MASK_PY
$K4_OBLIGATION5_PY" "$REPO"

# §8 row 6 — the retired allocation-identity symbols are gone from live code.
# They are EXPECTED in comments: K4-CONTRACT §6 says the VidMm tracker has no
# successor and §5 says the resource_id gap is named rather than bridged, and
# both arguments are written at the sites the mechanisms used to occupy. A
# tombstone is the point; a declaration is the violation. A string literal counts
# as live code — a retired name in a diag/ETW tag is a mechanism a reader will
# go looking for.
K4_OBLIGATION6_PY=$(cat <<'PY'
REPO = sys.argv[1]
RETIRED = [
    'HeliosWddmOpenIdentity', 'HeliosWddmAllocPrivate', 'HeliosWddmAllocMeta',
    'GlobalVidMmTracker', 'VidMmTrackerTable', 'AdoptedUmdResource',
    'write_open_identity',
]
ROOTS = [REPO + '/kmd_render/src', REPO + '/kmd_logic/src']

live, tombstones, files = [], 0, 0
for root in ROOTS:
    for path in rust_files(root):
        src = open(path).read()
        kind = rust_kinds(src)
        lines = src.splitlines()
        hit = False
        for sym in RETIRED:
            for mm in re.finditer(r'\b' + sym + r'\b', src):
                hit = True
                k = kind[mm.start()]
                if k == 35:
                    tombstones += 1
                else:
                    n = src.count('\n', 0, mm.start()) + 1
                    where = 'a string literal' if k == 115 else 'LIVE CODE'
                    live.append('%s:%d: `%s` in %s -- %s' % (path, n, sym, where, lines[n - 1].strip()))
        files += 1 if hit else 0

if tombstones == 0:
    sys.exit('not one tombstone found for any of the %d retired symbols. They were argued to survive in comments, so zero mentions means either the arguments were deleted or this gate stopped seeing the tree -- FIX THIS GATE, do not delete it' % len(RETIRED))
if live:
    sys.exit('OBLIGATION 6 VIOLATED -- a retired allocation-identity symbol is back:\n' + '\n'.join(live))
print('OK: %d retired symbols, 0 live occurrences under kmd_render/src + kmd_logic/src; %d comment tombstones across %d files' % (len(RETIRED), tombstones, files))
PY
)

run_gate "K4 §8.6: the retired allocation-identity symbols survive only as tombstones" \
    python3 -c "$K4_RUST_MASK_PY
$K4_OBLIGATION6_PY" "$REPO"

# ── The one constant hand-mirrored across two REPOSITORIES.
#
# CLAUDE.md names VKD3D_HEAP_FLAG_HELIOS_VENUS_EXPORT the highest-risk
# divergence in the vkd3d fork: a private D3D12_HEAP_FLAGS bit whose value lives
# in vkd3d-proton-helios/libs/vkd3d/vkd3d_private.h and is re-declared by hand
# in umd12/src/forward12/resource12.rs, because the D3D12_HEAP_FLAGS word is the
# only channel between them and neither side can include the other's header.
# Nothing has ever checked that the two agree except a human reading both.
run_gate "VKD3D_HEAP_FLAG_HELIOS_VENUS_EXPORT agrees across the two repos" \
    python3 -c "
import re, sys
h = open('$REPO/vkd3d-proton-helios/libs/vkd3d/vkd3d_private.h').read()
r = open('$REPO/umd12/src/forward12/resource12.rs').read()
mh = re.search(r'#define\s+VKD3D_HEAP_FLAG_HELIOS_VENUS_EXPORT\s*\(\(D3D12_HEAP_FLAGS\)\(1u?\s*<<\s*(\d+)\)\)', h)
mr = re.search(r'const\s+HELIOS_HEAP_FLAG_VENUS_EXPORT\s*:\s*D3D12_HEAP_FLAGS\s*=\s*D3D12_HEAP_FLAGS\(1\s*<<\s*(\d+)\)', r)
if not mh: sys.exit('vkd3d side not found — the #define shape changed; fix this gate, do not delete it')
if not mr: sys.exit('umd12 side not found — the const shape changed; fix this gate, do not delete it')
if mh.group(1) != mr.group(1):
    sys.exit('MISMATCH: vkd3d 1<<%s vs umd12 1<<%s' % (mh.group(1), mr.group(1)))
print('both sides agree: 1 << %s' % mh.group(1))
"

printf '\n'
if [ ${#FAILED[@]} -eq 0 ]; then
    printf 'ALL LINUX-SIDE RETIREMENT GATES PASS\n'
    exit 0
fi
printf 'FAILED: %s\n' "${FAILED[*]}"
exit 1
