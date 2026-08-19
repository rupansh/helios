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
# A gate that did not run is not a gate that passed. The summary at the bottom
# reads this; nothing else may.
#
# ⭐ As of 2026-08-10 NOTHING APPENDS TO THIS — every gate in this file runs
# unconditionally, because the last conditional one (the slot audit, guarded on a
# `.gitignore`d header) had its input vendored into the repo instead. The array
# and its summary arm are kept deliberately: they are the shape any future
# conditional gate must use, and re-deriving them under pressure is how the
# original "SKIP reported as ALL PASS" defect got in. If you add a gate that can
# skip, append here — do not invent a second mechanism, and do not let the
# summary say "PASS".
SKIPPED=()

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
#
# ⚠ WHAT THIS GATE'S NAME PROMISES AND WHAT THE SCRIPT DOES. "ABI parity" is
# broader than the check, and for one round it was broader in a way that
# mattered: the script compared the size/align/offset ASSERTIONS and, although it
# built both constant dictionaries, never compared the constant VALUES. Mutation
# testing found 173 of 174 shared constants -- every record magic, every ABI
# version -- diverging with exit 0. It now compares four things: sizes, aligns,
# offsets, and the values of every constant declared on BOTH sides (247 of 248
# today; the one exception reports itself as UNRESOLVED). It still does NOT
# compare field ORDER beyond the offsets that are asserted, or types at all --
# the C-mirror COMPILE gate below is the other half, and neither replaces the
# other. `--verbose` lists everything the script could not fold.
run_gate "protocol Rust<->C ABI parity (sizes, aligns, offsets, constant values)" \
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
# instruction would have shipped a driver that refuses to load (FINDINGS.md F6).
#
# ⭐ UNCONDITIONAL since 2026-08-10, and the conditional that used to wrap it is
# gone rather than merely reported honestly. It read
# `[ -f "$REPO/tmp/wdk-28000/Include/10.0.28000.0/km/dispmprt.h" ]` against a
# path `.gitignore` excludes, so this gate SKIPPED in every fresh clone, every
# new worktree and every CI runner — and the summary printed "ALL … PASS" over
# it. Round 3 of the Phase-2 review demonstrated the consequence: with an
# intentionally stale `wddm32_slot_audit.rs`, the SAME commit exits 1 where the
# header happens to be staged and 0 where it is not.
#
# The fix is not a better skip message. The generator's single input —
# `km/dispmprt.h`, 167 KB, resolving no `#include` — is now VENDORED at
# `kmd_render/tools/wdk-28000/km/dispmprt.h` (owner decision; provenance, both
# SHA-256s and the re-extraction command are in that directory's README), so the
# gate has tracked ground truth that cannot differ between two checkouts of one
# commit, and there is nothing left to skip.
#
# ⚠ The "worse failure than the one being prevented" argument that justified the
# conditional was about *build machines* that had not staged the headers. It no
# longer applies here: this suite is Linux-side and its input is in the repo.
# `kmd_render/build.rs::verify_slot_audit_not_stale` keeps its own fallback chain
# and its own panic on the `STALE:` marker, so the driver build remains a second,
# independent enforcement point.
run_gate "WDDM 3.2 slot audit is not stale vs its generator" \
    python3 "$REPO/kmd_render/tools/gen_wddm32_slot_audit.py" --check

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
# ⚠ WHAT THESE TWO GATES CAN AND CANNOT CATCH — re-derived 2026-08-10 (round 3)
# from what the code below actually does. This block has now been wrong TWICE in
# the same way, and both times the wrongness was the same shape: it listed as
# CAUGHT the exact case that then defeated the gate. Round 2's version claimed "a
# callee whose signature takes the buffer as `*mut`" and "a `*mut` cast of the
# buffer" and caught neither. Round 3's critic then defeated the round-2 gate FOUR
# more ways, all of them with a CAN-list that said they were covered:
#   1. `*stamp |= 0x8000_0000` through a tracked alias — the assignment regex
#      excluded the character before `=`, so every COMPOUND operator was invisible.
#   2. `dst.copy_from_nonoverlapping(..)`, `dst.copy_from(..)`, `dst.replace(..)`
#      — methods were tested against a DENY-list that had `copy_to` but not
#      `copy_from`, and `.write(` but not `.replace(`.
#   3. `head.as_mut()` — not pointer arithmetic, so the alias chain stopped, and
#      two plain SAFE field writes through the `&mut` matched no rule at all.
#      4. a callee resolved by BARE IDENTIFIER with `create_allocation.rs`
#      searched first — and several top-level `fn` names under `kmd_render/src`
#      are ALREADY defined in two files (`round_up_page`, `invalidate_all`, the
#      helpers `e`/`f`), so no decoy needs planting. ⛔ Do not write a COUNT here;
#      it drifts with the tree. The gate below derives it itself and fails.
# ⇒ so read the list below as a description of code, not as a guarantee, and when
# you change a rule, re-derive this block in the same edit.
#
# The gates are textual; they read `kmd_render`, which does not build on Linux at
# all, so they are the ONLY mechanical statement about that crate available on
# this host, and they are weaker than a compiler by construction.
#
# §8.5 tracks ALIASES and states every rule over a STATEMENT (a span between `;`
# `{` `}`), not over a physical line — a method chain broken across lines put the
# buffer's name on one line and the write on another. `pPrivateDriverData` seeds a
# set of names; a `let`/assignment whose initialiser mentions a live alias and
# applies nothing to it but casts, `&`/`*` reborrows, the pointer methods in
# `PTR_METHODS` and the reference-minting methods in `REF_METHODS` binds ANOTHER
# name for the same pointer. A call is followed when ANY alias appears in its
# argument list, and the parameter that received it becomes the alias inside the
# callee.
#   * CAN: a restamp reintroduced in `DxgkDdiOpenAllocation` or in anything it
#     hands the private pointer to, INCLUDING after the pointer is rebound to one
#     or more locals AND including through a `&mut` minted by `as_mut` /
#     `as_uninit_mut`; a callee whose signature takes any alias as `*mut`; a
#     `*mut` anywhere in a statement that names an alias; an assignment through
#     any alias, PLAIN OR COMPOUND (`|=`, `&=`, `+=`, `<<=`, `>>=`, …); ANY method
#     call in a statement that names an alias whose name is not on the read-only
#     allow-list `SAFE_METHODS` — that test is INVERTED on purpose and is the only
#     rule here whose vocabulary is closed, so a method nobody has thought of
#     fails; any write intrinsic in the DDI or in a followed callee; a retired
#     symbol back as a type, a call, or a string (§8.6). Aliasing and
#     statement-scope are deliberately over-approximate — an alias stays live to
#     the end of its region regardless of branches and regardless of shadowing,
#     and a method is judged by the statement it sits in rather than by its
#     receiver — so the errors they can make are false ALARMS, not misses.
#   * HARD FAILURE, not a skip and not a miss: a callee this gate cannot find
#     under `kmd_render/src`, cannot parse, or whose bare name is AMBIGUOUS there.
#     `fn_pattern` anchors at column 0, so a callee defined inside an `impl` or a
#     nested `mod` is "cannot find" and stops the gate rather than passing it.
#   * CANNOT: a write two calls deep (the flow is followed exactly ONE level from
#     the DDI body; aliases ARE propagated inside a followed callee, but a call it
#     makes in turn is not followed); a pointer laundered through a struct field,
#     a slice, a `Vec`, a tuple/destructuring `let`, or a closure capture (only
#     plain `let x = …` / `let Some(x) = …` / `x = …` bindings carry an alias) —
#     and note that breaking the chain that way also detaches the method
#     allow-list, which has nothing to attach to once no alias is named; a write
#     through a pointer stashed in a struct at create time and dereferenced at
#     open; a write emitted by a macro.
#
# §8.6 reads every crate that can `use helios_protocol::…`, not just the KMD.
# The retired symbols are DECLARED in `protocol/src/wddm_legacy.rs` and re-exported
# by `pub use wddm_legacy::*`, so a root set of `kmd_render/src` + `kmd_logic/src`
# left `use helios_protocol::GlobalVidMmTracker;` in a UMD passing every gate in
# the repo. The quarantine module is now the ONE file where a live occurrence is a
# declaration rather than a resurrection; everywhere else it is a violation.
#   * CANNOT: a resurrection under a NEW name (the list is a name list); a
#     resurrection in C, C++, or the `.h` mirrors; a resurrection in `kmd/src`,
#     which is archived, in no build, and deliberately out of the root set.
#   * Each gate also fails when it stops seeing anything (no private-buffer
#     reference in the DDI; no tombstone anywhere; a root that does not exist).
#     A gate that passes because it went blind is the failure mode this whole
#     file exists to prevent.

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
#
# ⛔ AMENDED 2026-08-11, because the obligation's PREMISE was falsified on the
# target. §8.5 assumed the create-time write reaches the caller, so the open had
# nothing left to publish. It does not: dxgkrnl DISCARDS a KMD write into
# `DXGK_ALLOCATIONINFO::pPrivateDriverData` for any user-supplied buffer —
# `tools/hwa2_writeback_probe.c` on 22.22.266.0, 7 allocations across both
# thunks, both record types, bare and resource-associated, every one unstamped in
# the caller's buffer AND unstamped on arrival at the open DDI (5 × `0x0C02_00E6`
# in the diag ring) — while the same write at OPEN reaches user mode (probe H2
# went FAIL → PASS on 22.22.267.0, `OaHvm1Stamp=1`, `TsPoolBind=1`). The WDK
# annotates the two fields `in:` and `in/out:` respectively; the retirement read
# the first one as the second.
#
# So the rule is now: the open writes no byte of an HWA2 or HOC1 buffer — the
# no-restamp property §8.5 exists for, and the one the "two openers disagree"
# defect actually needed — and exactly ONE licensed write of an HVM1
# create-output, in `EXEMPT_CALLEE`, which is itself checked here rather than
# waved through. Adding a second name to that list is a contract change and must
# be argued in the commit that does it.
K4_OBLIGATION5_PY=$(cat <<'PY'
REPO = sys.argv[1]
ROOT = REPO + '/kmd_render/src'
PATH = ROOT + '/ddi/create_allocation.rs'
BUF = 'pPrivateDriverData'
DDI = 'dxgkddi_open_allocation'
# The ONE function licensed to write the private buffer, and the record type it
# is licensed for. Everything else in the flow stays under the original rules.
EXEMPT_CALLEE = 'stamp_open_hvm1'
EXEMPT_SIZE = 'HELIOS_HVM1_SIZE'
# The write intrinsics the exempt callee is allowed to use, and nothing else.
EXEMPT_WRITES = ('from_raw_parts_mut', 'copy_from_slice')
# Record vocabularies the exempt callee may not touch: naming one of these is how
# an HVM1-shaped exemption would be widened into an HWA2/HOC1 restamp.
EXEMPT_FORBIDDEN = ('HELIOS_HWA2_BYTES', 'HELIOS_HWA2_MAGIC', 'HELIOS_HOC1_SIZE',
                    'HELIOS_HOC1_MAGIC', 'HeliosWddmAllocationDescV2',
                    'HeliosOuterCommandAllocationV1')
# ── The two vocabularies, and why one is OPEN and the other is CLOSED.
#
# WRITE_INTRINSICS is the OPEN side: a substring net over free/path calls whose
# NAME says "write". It can only ever be incomplete, so nothing may depend on it
# alone; it survives because when it does fire it names the violation precisely.
WRITE_INTRINSICS = [
    'copy_nonoverlapping', 'write_bytes', 'write_volatile', 'write_unaligned',
    'ptr::write', '.write(', 'copy_from_slice', 'clone_from_slice',
    'from_raw_parts_mut', 'copy_to', 'write_open_identity',
]
# SAFE_METHODS is the CLOSED side, and the inversion is the point.
#
# ⛔ A completeness critic defeated the previous version three separate ways with
# method calls -- `dst.copy_from_nonoverlapping(..)`, `dst.copy_from(..)`,
# `dst.replace(..)` -- all passing for one reason: METHODS were tested against a
# deny-list. `copy_to` was on it and `copy_from` was not; `.write(` was on it and
# `.replace(` was not. A deny-list over a vocabulary the author does not control
# (every method `*mut T` has, now and in every future Rust release) cannot be
# completed, and every hole in it is silent. So the test is inverted: in any
# statement that names the private buffer, EVERY method call must be on this
# list. A method nobody has thought about fails the gate and has to be argued
# onto the list -- a false alarm, which is the error direction this gate is
# allowed to make.
#
# A binding whose initialiser applies ONLY PTR_METHODS/REF_METHODS to an alias is
# holding the very same pointer, so it is another name for the buffer. Anything
# else -- notably a function call -- is assumed to produce a NEW value, which is
# why `let desc = read_open_descriptor(buf, len)` does not make `desc` an alias
# and does not drag every later `Box::new(..desc..)` into the callee-following
# pass. The one exception is a followed callee that RETURNS a pointer or a
# reference: that return value can still be the buffer, so `ptr_ret` re-admits it.
PTR_METHODS = set(
    'add offset sub cast cast_mut cast_const wrapping_add wrapping_sub '
    'wrapping_offset byte_add byte_sub byte_offset as_ptr as_mut_ptr'.split())
# These mint a REFERENCE to the pointee rather than another pointer. `as_mut` was
# the third defeat: it is not pointer arithmetic, so the old alias test refused to
# call its result an alias, and two plain SAFE field writes through the resulting
# `&mut` then matched no rule at all -- `rec.allocation_generation = ..` (K4
# obligation 4's write-exactly-once field) and `rec.flags |= ..` (the word
# `hwa2_is_direct_scanout_primary` keys scan-out routing on), i.e. precisely the
# two fields round 2 was spent repairing.
REF_METHODS = set('as_mut as_ref as_uninit_mut as_uninit_ref'.split())
# Reads of the pointee, or of the pointer itself. Each is here because a live
# call site needs it, or because it is the read-only twin of a banned write.
READ_METHODS = set(
    'is_null is_aligned align_offset addr to_bits read read_volatile '
    'read_unaligned len is_empty'.split())
SAFE_METHODS = PTR_METHODS | REF_METHODS | READ_METHODS
# Not calls at all. `Some`/`Ok`/`Err` are enum constructors: they cannot write
# through what they wrap, and they are here because tracking `&mut` aliases makes
# `if let Some(rec) = rec` look like a call taking an alias -- which then
# hard-failed as an unfindable callee and reported the wrong violation for code
# that was violating something else two lines down.
NOT_CALLS = set('if while for match return unsafe fn else loop as Some Ok Err'.split())

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

def paren_end(t, i):
    d = 0
    while i < len(t):
        if t[i] == '(':
            d += 1
        elif t[i] == ')':
            d -= 1
            if d == 0:
                return i
        i += 1
    return len(t)

def split_args(t):
    out, d, cur = [], 0, ''
    for ch in t:
        if ch in '([{':
            d += 1
        elif ch in ')]}':
            d -= 1
        if ch == ',' and d == 0:
            out.append(cur)
            cur = ''
        else:
            cur += ch
    out.append(cur)
    return out

def named(t, aliases):
    return [a for a in aliases if re.search(r'\b' + re.escape(a) + r'\b', t)]

def calls_in(t):
    """(is_method, name, index of its open paren) for every call in `t`."""
    for cm in re.finditer(r'(\.\s*)?\b([A-Za-z_]\w*)\s*(?:::\s*<[^>]*>\s*)?\(', t):
        if cm.group(2) in NOT_CALLS:
            continue
        yield (cm.group(1) is not None, cm.group(2), cm.end() - 1)

def assign_ops(t):
    """Index of the `=` of every ASSIGNMENT in `t`, plain or COMPOUND.

    ⛔ The regex this replaced was `[^=!<>+\\-*/&|^]=[^=]`, whose leading class
    excluded the character before `=` for the WHOLE set of compound operators --
    so `|=`, `&=`, `+=`, `<<=` were all invisible and `*stamp |= 0x8000_0000`
    through a tracked alias passed the gate. Excluding a comparison is the
    requirement; excluding a compound assignment was the defect.
    """
    i = 0
    while i < len(t):
        if t[i] != '=':
            i += 1
            continue
        nxt = t[i + 1] if i + 1 < len(t) else ''
        prv = t[i - 1] if i else ''
        pp = t[i - 2] if i >= 2 else ''
        if nxt == '=' or prv == '=':        # ==
            i += 2 if nxt == '=' else 1
            continue
        if nxt == '>':                      # => fat arrow
            i += 2
            continue
        # `!=` `<=` `>=` are comparisons; `<<=` and `>>=` are ASSIGNMENTS, and the
        # only thing that tells them apart is the character before the operator.
        if prv in '!<>' and not (prv == pp and prv in '<>'):
            i += 1
            continue
        yield i
        i += 1

def ptr_preserving(init, live, ptr_ret):
    if not named(init, live):
        return False
    for is_method, name, _ in calls_in(init):
        if is_method and (name in PTR_METHODS or name in REF_METHODS):
            continue
        if (not is_method) and name in ptr_ret:
            continue
        return False
    return True

# Only plain `let x = ..;` / `let Some(x) = ..` / `x = ..;` bindings carry an
# alias. Destructuring, struct fields and captures are the documented blind spot.
BINDERS = [
    re.compile(r'\blet\s+(?:mut\s+)?([A-Za-z_]\w*)\s*(?::[^=;]*?)?=\s*([^;]*);', re.S),
    re.compile(r'\blet\s+(?:Some|Ok)\s*\(\s*(?:mut\s+)?([A-Za-z_]\w*)\s*\)\s*=\s*([^;]*?)\s*(?:;|else)', re.S),
    re.compile(r'(?m)^\s*([A-Za-z_]\w*)\s*=[^=]([^;]*);'),
]

def grow_aliases(text, seed, ptr_ret):
    """seed: {name: char offset from which it is live}. -1 == live everywhere."""
    aliases = dict(seed)
    changed = True
    while changed:
        changed = False
        for rx in BINDERS:
            for bm in rx.finditer(text):
                nm, init = bm.group(1), bm.group(2)
                if nm in aliases:
                    continue
                live = [a for a, o in aliases.items() if o <= bm.start(2)]
                if ptr_preserving(init, live, ptr_ret):
                    aliases[nm] = bm.start()
                    changed = True
    return aliases

def fn_pattern(name):
    return (r'^(?:pub(?:\s*\([^)]*\))?\s+)?(?:default\s+)?(?:const\s+)?(?:async\s+)?'
            r'(?:unsafe\s+)?(?:extern\s+"[^"]*"\s+)?fn\s+' + re.escape(name) + r'\s*[(<]')

def fn_header(c, start, name):
    """(parameter text, return-type text) of the fn declared at `start`."""
    i = c.find(name, start) + len(name)
    while i < len(c) and c[i].isspace():
        i += 1
    if i < len(c) and c[i] == '<':          # generic parameter list
        d = 0
        while i < len(c):
            if c[i] == '<':
                d += 1
            elif c[i] == '>':
                d -= 1
                if d == 0:
                    i += 1
                    break
            i += 1
        while i < len(c) and c[i].isspace():
            i += 1
    if i >= len(c) or c[i] != '(':
        return None
    close = paren_end(c, i)
    b = c.find('{', close)
    return c[i + 1:close], c[close + 1:b if b >= 0 else close + 1]

# ⛔ STATEMENTS, NOT LINES.
#
# Every rule below is stated over a statement-sized span, because a method chain
# broken across physical lines puts the buffer's name on one line and the write
# on another:
#     info.pPrivateDriverData
#         .cast::<u8>()
#         .copy_from(src, 8);
# A per-line rule sees an alias with no method and then a method with no alias.
# Splitting on braces as well as `;` is what keeps `unsafe { dst.copy_from(..) }`
# one span while stopping an `if` header from swallowing the block below it -- and
# it also closes the old rule's `let x = 0; alias.f |= 1;`-on-one-line hole, since
# the "`let ` earlier on the line" suppressor now only sees its own statement.
def statements(text):
    out, prev = [], 0
    for sm in re.finditer(r'[;{}]', text):
        out.append((prev, text[prev:sm.start()]))
        prev = sm.end()
    out.append((prev, text[prev:]))
    return out

ddi_text = body_from(code, m.start())
bad = []
exempt_seen = set()  # EXEMPT_CALLEE, if the flow actually reaches it
followed = {}       # callee name -> (file, name, start, body, [param aliases])
ptr_ret = set()     # followed callees that return a pointer/reference
aliases = {BUF: -1}

# Alias growth and one-level callee following are mutually recursive: a callee is
# followed because an alias reached it, and its return value can mint a new alias.
progress = True
while progress:
    progress = False
    aliases = grow_aliases(ddi_text, aliases, ptr_ret)
    for is_method, name, op in calls_in(ddi_text):
        if is_method or name in followed:
            continue
        argtext = ddi_text[op + 1:paren_end(ddi_text, op)]
        live = [a for a, o in aliases.items() if o <= op]
        # The buffer's LENGTH is not a buffer: `PrivateDriverDataSize` does not
        # match `\bpPrivateDriverData\b`, so a call taking only the length is not
        # followed.
        if not named(argtext, live):
            continue
        progress = True
        followed[name] = None
        if name in WRITE_INTRINSICS:
            bad.append('OBLIGATION 5: the open path hands the private buffer straight to `%s`' % name)
            continue
        # ⛔ A callee is resolved by BARE IDENTIFIER -- `calls_in` captures only the
        # trailing name, so `crate::virtio::venus::bringup::round_up_page(..)`
        # arrives here as `round_up_page`. The previous version took the FIRST
        # definition it found, searching create_allocation.rs first, and never
        # consulted the call's module path: a same-named function anywhere else
        # made it vet one function while another one wrote. That needs no decoy to
        # be planted: `round_up_page` is already defined in BOTH
        # ddi/create_allocation.rs and virtio/venus/bringup.rs, and
        # `invalidate_all` in BOTH adapter/allocation_object.rs and
        # ddi/native_fence.rs (plus the helpers `e`/`f`). So an ambiguous name is a
        # FAILURE, the same class as a name this gate cannot find at all -- and the
        # ambiguity is DERIVED here rather than asserted in a comment that drifts.
        defs = []
        for f in [PATH] + [x for x in codes if x != PATH]:
            for fm in re.finditer(fn_pattern(name), codes[f], re.M):
                defs.append((f, codes[f], fm))
        if not defs:
            sys.exit('the open path hands the private buffer to `' + name + '`, whose definition this gate cannot find under kmd_render/src. It cannot vouch for a callee it cannot read -- FIX THIS GATE (or bring the callee back in-tree), do not delete it')
        if len(defs) > 1:
            sys.exit('the open path hands the private buffer to `' + name + '`, and ' + str(len(defs))
                     + ' functions under kmd_render/src are called that: '
                     + ', '.join('%s:%d' % (d[0], d[1].count('\n', 0, d[2].start()) + 1) for d in defs)
                     + '. This gate resolves a callee by BARE NAME and cannot tell which one the call means, so vetting one of them would be a silent pass -- FIX THIS GATE (or rename one of them), do not delete it')
        f, c, fm = defs[0]
        if name == EXEMPT_CALLEE:
            # The exemption is not a pass: the callee is held to a stricter,
            # HVM1-shaped contract than the generic rules could express.
            exempt_seen.add(name)
            ebody = body_from(c, fm.start())
            ebase = c.count('\n', 0, fm.start()) + 1
            if not re.search(r'private_size\s+as\s+usize\s*!=\s*' + EXEMPT_SIZE + r'\s+as\s+usize',
                             ebody):
                bad.append('EXEMPTION: `%s` no longer guards on `private_size as usize != %s as usize`, so its write is no longer bounded by the record size' % (name, EXEMPT_SIZE))
            for est, estmt in statements(ebody):
                for w in WRITE_INTRINSICS:
                    if w not in estmt:
                        continue
                    en = ebase + ebody.count('\n', 0, est + estmt.find(w))
                    if w not in EXEMPT_WRITES:
                        bad.append('%s:%d: EXEMPTION: write intrinsic `%s` in `%s` is outside the licensed pair %s'
                                   % (f, en, w, name, '/'.join(EXEMPT_WRITES)))
                    elif w == 'from_raw_parts_mut' and EXEMPT_SIZE not in estmt:
                        bad.append('%s:%d: EXEMPTION: `from_raw_parts_mut` in `%s` is not bounded by %s -- %s'
                                   % (f, en, name, EXEMPT_SIZE, ' '.join(estmt.split())))
            for banned in EXEMPT_FORBIDDEN:
                if banned in ebody:
                    bad.append('EXEMPTION: `%s` names `%s`. The exemption is HVM1-only; an HWA2/HOC1 restamp is exactly what §8.5 still forbids' % (name, banned))
            continue
        hdr = fn_header(c, fm.start(), name)
        if hdr is None:
            sys.exit('cannot parse the signature of `' + name + '`, which receives the private buffer -- FIX THIS GATE, do not delete it')
        params, ret = hdr
        if '*mut' in params:
            bad.append('OBLIGATION 5: the open path passes the private buffer to `%s`, which takes it as `*mut`: %s' % (name, ' '.join(params.split())))
        # Positional match: the parameter that received an alias becomes the
        # alias inside the callee, so the rules below apply to it under its own
        # name there.
        pnames, plist = [], split_args(params)
        for idx, arg in enumerate(split_args(argtext)):
            if named(arg, live) and idx < len(plist):
                pm = re.match(r'\s*(?:mut\s+)?([A-Za-z_]\w*)\s*:', plist[idx])
                if pm:
                    pnames.append(pm.group(1))
        if re.search(r'\*\s*(?:mut|const)|&', ret):
            ptr_ret.add(name)
        followed[name] = (f, name, fm.start(), body_from(c, fm.start()), pnames)

regions = [(PATH, DDI, m.start(), ddi_text, aliases)]
for v in followed.values():
    if v:
        body = v[3]
        regions.append((v[0], v[1], v[2], body,
                        grow_aliases(body, dict((p, -1) for p in v[4]), ptr_ret)))

refs = 0
alias_names = set()
for f, name, start, text, alias_at in regions:
    base = codes[f].count('\n', 0, start) + 1
    lines = text.split('\n')
    offof = dict((a, 0 if o < 0 else o) for a, o in alias_at.items())
    alias_names |= set(a for a in alias_at if a != BUF)
    refs += len(re.findall(r'\b' + BUF + r'\b', text))

    def where(off, text=text, lines=lines, base=base):
        li = text.count('\n', 0, off)
        return base + li, lines[li].strip()

    for st, stmt in statements(text):
        # An alias is live over a statement if it becomes live anywhere at or
        # before that statement's END -- the same over-approximation the
        # line-based form had, so branches and shadowing can only add alarms.
        live = [a for a, o in offof.items() if o <= st + len(stmt)]
        for w in WRITE_INTRINSICS:
            k = stmt.find(w)
            if k >= 0:
                n, src = where(st + k)
                bad.append('%s:%d: write intrinsic `%s` in %s -- %s' % (f, n, w, name, src))
        hits = named(stmt, live)
        # In the DDI body `*mut` is LEGAL: DXGKARG_OPENALLOCATION's OUT fields
        # (hDeviceSpecificAllocation, Pitch, SubresourceOffset) are written
        # through one, and the DDI says so in its own SAFETY comment. So there it
        # is banned only in a statement that also names an ALIAS of the private
        # buffer -- which is what `let stamp = base as *mut u32;` is, and what the
        # literal-name form of this rule missed. Inside a followed callee it is
        # banned outright: that callee took the buffer as `*const`, so
        # manufacturing a `*mut` is exactly the laundering step.
        if '*mut' in stmt and (name != DDI or hits):
            n, src = where(st + stmt.find('*mut'))
            bad.append('%s:%d: `*mut` reachable from the private buffer in %s (via %s) -- %s'
                       % (f, n, name, ', '.join(hits) or 'the callee parameter', src))
        if not hits:
            continue
        for ai in assign_ops(stmt):
            if named(stmt[:ai], live) and 'let ' not in stmt[:ai]:
                n, src = where(st + ai)
                bad.append('%s:%d: assignment INTO the private buffer in %s -- %s' % (f, n, name, src))
        for is_method, mname, mop in calls_in(stmt):
            if is_method and mname not in SAFE_METHODS:
                n, src = where(st + mop)
                bad.append('%s:%d: method `.%s()` in a statement that names the private buffer in %s, and it is not on the read-only allow-list -- %s'
                           % (f, n, mname, name, src))

if refs == 0 or len(followed) < 2:
    sys.exit('the gate found no private-buffer flow in %s (refs=%d, followed=%d). Either the DDI stopped reading private data or the gate went blind -- FIX THIS GATE, do not delete it' % (DDI, refs, len(followed)))
if bad:
    sys.exit('OBLIGATION 5 VIOLATED -- DxgkDdiOpenAllocation writes the allocation-private buffer:\n' + '\n'.join(bad))
print('OK: %s + %d const-only callee(s) [%s]: %d %s reference(s), %d alias(es)%s -- no write intrinsic, no *mut, no assignment (plain or compound), no method outside the read-only allow-list.\n    Licensed HVM1 write: %s'
      % (DDI, len(regions) - 1, ', '.join(r[1] for r in regions[1:]), refs,
         BUF, len(alias_names),
         (' [' + ', '.join(sorted(alias_names)) + ']') if alias_names else '',
         ('%s, length-guarded on %s, no HWA2/HOC1 vocabulary' % (EXEMPT_CALLEE, EXEMPT_SIZE))
         if exempt_seen else 'none in this flow'))
PY
)

run_gate "K4 §8.5 (amended): open writes no byte of an HWA2/HOC1 buffer; the HVM1 stamp is the one licensed write" \
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
    'HELIOS_WDDM_ALLOC_KIND_TRACKING', 'write_open_identity',
]
# ⚠ EVERY crate that can `use helios_protocol::…`, not just the KMD. The retired
# symbols are `pub` in `protocol/src/wddm_legacy.rs` and re-exported wholesale by
# `pub use wddm_legacy::*` in `protocol/src/lib.rs`, so with the KMD-only root set
# this gate had, `use helios_protocol::GlobalVidMmTracker;` in umd12 was invisible
# to every gate in the repo -- it looked in the two places the symbols are NOT.
# `kmd/src` is deliberately absent: it is the archived System-class stack, in no
# build, and CLAUDE.md forbids resurrecting it, so a name there cannot come back.
ROOTS = [REPO + '/' + p + '/src' for p in
         ('protocol', 'kmd_render', 'kmd_logic', 'umd', 'umd_common', 'umd12')]
# The quarantine module is where these symbols LIVE. Its own header says the only
# legitimate edit is deleting a symbol once its last caller is gone, so a live
# occurrence HERE is a declaration awaiting that deletion; a live occurrence in
# any consumer is the resurrection this obligation forbids. That distinction is
# the whole reason the roots can be widened at all.
DECL = REPO + '/protocol/src/wddm_legacy.rs'

live, tombstones, files, declared = [], 0, 0, 0
missing = [r for r in ROOTS if not rust_files(r)]
if missing:
    sys.exit('these roots hold no .rs file at all: %s. A gate that reads nothing passes everything -- FIX THIS GATE, do not delete it' % ', '.join(missing))
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
                elif path == DECL:
                    declared += 1
                else:
                    n = src.count('\n', 0, mm.start()) + 1
                    where = 'a string literal' if k == 115 else 'LIVE CODE'
                    live.append('%s:%d: `%s` in %s -- %s' % (path, n, sym, where, lines[n - 1].strip()))
        files += 1 if hit else 0

if tombstones == 0:
    sys.exit('not one tombstone found for any of the %d retired symbols. They were argued to survive in comments, so zero mentions means either the arguments were deleted or this gate stopped seeing the tree -- FIX THIS GATE, do not delete it' % len(RETIRED))
if live:
    sys.exit('OBLIGATION 6 VIOLATED -- a retired allocation-identity symbol is back in a consumer:\n' + '\n'.join(live))
print('OK: %d retired symbols, 0 live occurrences across %d roots (%s); %d declaration(s) quarantined in protocol/src/wddm_legacy.rs; %d comment tombstones across %d files'
      % (len(RETIRED), len(ROOTS), ' '.join(r.replace(REPO + '/', '') for r in ROOTS),
         declared, tombstones, files))
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
#
# ⛔ AND FOR ONE ROUND, NOR DID THIS. It was two bare unanchored `re.search`
# calls with no comment awareness — in the one file that carries a 90-line
# comment/string/code classifier built for exactly this problem. A stale
# `/* Was: #define … 1u << 30 … */` above a live `1u << 31` made it print
# "both sides agree: 1 << 30" and exit 0 while the repositories disagreed
# (reproduced 2026-08-10). Both sides are masked first, both regexes are
# anchored to the start of a line, and TWO live definitions is a failure —
# because when there are two, this gate cannot know which one the compiler takes.
K4_VKD3D_FLAG_PY=$(cat <<'PY'
REPO = sys.argv[1]

def c_mask(src):
    """Blank C comments so a stale `/* Was: … */` cannot be read as live code.

    Block comments do NOT nest in C, so this is deliberately not `rust_kinds`.
    Newlines inside a blanked comment are preserved, which is what lets the
    `#define` regex below anchor with `re.M` and still report true line numbers.
    A string/char literal is copied through verbatim and never opens a comment,
    so `"/*"` in a message cannot swallow the rest of the header.
    """
    out, i, n = [], 0, len(src)
    while i < n:
        c = src[i]
        if c == '/' and i + 1 < n and src[i + 1] == '*':
            j = src.find('*/', i + 2)
            j = n if j < 0 else j + 2
            out.append(''.join('\n' if ch == '\n' else ' ' for ch in src[i:j]))
            i = j
            continue
        if c == '/' and i + 1 < n and src[i + 1] == '/':
            j = src.find('\n', i)
            j = n if j < 0 else j
            out.append(' ' * (j - i))
            i = j
            continue
        if c == '"' or c == "'":
            q, j = c, i + 1
            while j < n:
                if src[j] == '\\':
                    j += 2
                    continue
                if src[j] == q:
                    j += 1
                    break
                if src[j] == '\n':
                    break
                j += 1
            out.append(src[i:j])
            i = j
            continue
        out.append(c)
        i += 1
    return ''.join(out)

H = REPO + '/vkd3d-proton-helios/libs/vkd3d/vkd3d_private.h'
R = REPO + '/umd12/src/forward12/resource12.rs'
h = c_mask(open(H).read())
rs = open(R).read()
r = blank_comments(rs, rust_kinds(rs))
H_RE = re.compile(r'^\s*#\s*define\s+VKD3D_HEAP_FLAG_HELIOS_VENUS_EXPORT\s+'
                  r'\(\(D3D12_HEAP_FLAGS\)\(1u?\s*<<\s*(\d+)\)\)\s*$', re.M)
R_RE = re.compile(r'^\s*(?:pub(?:\s*\([^)]*\))?\s+)?const\s+HELIOS_HEAP_FLAG_VENUS_EXPORT\s*:\s*'
                  r'D3D12_HEAP_FLAGS\s*=\s*D3D12_HEAP_FLAGS\(1\s*<<\s*(\d+)\)\s*;', re.M)
mh = H_RE.findall(h)
mr = R_RE.findall(r)
if len(mh) == 0:
    sys.exit('vkd3d side not found -- the #define shape changed; fix this gate, do not delete it')
if len(mr) == 0:
    sys.exit('umd12 side not found -- the const shape changed; fix this gate, do not delete it')
if len(mh) > 1:
    sys.exit('vkd3d side has %d LIVE definitions of VKD3D_HEAP_FLAG_HELIOS_VENUS_EXPORT (%s) -- this gate cannot know which one the compiler takes; fix this gate, do not delete it' % (len(mh), ', '.join('1 << ' + v for v in mh)))
if len(mr) > 1:
    sys.exit('umd12 side has %d LIVE definitions of HELIOS_HEAP_FLAG_VENUS_EXPORT (%s) -- this gate cannot know which one the compiler takes; fix this gate, do not delete it' % (len(mr), ', '.join('1 << ' + v for v in mr)))
if mh[0] != mr[0]:
    sys.exit('MISMATCH: vkd3d 1<<%s vs umd12 1<<%s' % (mh[0], mr[0]))
print('both sides agree: 1 << %s (one live definition each, comments masked)' % mh[0])
PY
)

run_gate "VKD3D_HEAP_FLAG_HELIOS_VENUS_EXPORT agrees across the two repos" \
    python3 -c "$K4_RUST_MASK_PY
$K4_VKD3D_FLAG_PY" "$REPO"

# ── A2: the ICD's HNR2 encoder, run against protocol/'s validator.
#
# Every other gate here compares two texts. This one EXECUTES an ICD source file
# — `vn_helios_native_kmt.c`'s encoder half, which is platform-independent for
# exactly this reason — and feeds what it emits to `HeliosNativeRenderV2::validate`
# and `validate_commit_tables`, the code the KMD will run. Eight deliberate
# mutations (A1's old use-table offset, a zeroed allocation generation, ungrouped
# patch runs, an unclamped fragment split, a command-buffer-relative payload
# offset, a retained no-reply sentinel, a wrong CRC domain, one mistyped CRC
# table entry) were each caught by it before it was checked in.
run_gate "A2 HNR2 encoder validates against protocol (executed, not compared)" \
    bash "$REPO/tools/hnr2-encoder-gate.sh"

# K6's half of the same corpus. The encoder gate above proves the WIRE is right,
# with the per-context state machine hand-rolled as test locals; this one drives
# `helios_kmd_logic::native_render::RenderContext` — the state machine
# `kmd_render` will actually run, in the crate that only builds on the VM — and
# adds the three checks only the KMD side can make (staging admission, the
# one-slot-per-use patch plan, and the `WriteOperation` cross-check `protocol`
# structurally cannot make because it never sees a `DXGK_ALLOCATIONLIST`).
# Defeated 7 ways: dropping the open-batch store, dropping the per-entry
# write-operation check, counting typed operands instead of uses, a no-op staging
# retire, a token watermark that never advances, substituting the advertised
# command-buffer constant for the returned `DmaSize`, and making the assembler a
# process-global instead of per-context.
run_gate "K6 HNR2 decode: the ICD encoder's corpus replayed through the KMD assembler" \
    bash "$REPO/tools/hnr2-decode-gate.sh"

run_gate "K5 HTS1/HQA1: the C mirror's records replayed through the KMD session" \
    bash "$REPO/tools/hts1-attach-gate.sh"

# D9 replaces the dormant D2/D3 refusal gate with one package-level
# activation-coherence proof. The executable mutation pass feeds in-memory
# source overlays through the same gate checkers used for the real source.
run_gate "D9 WDDM 3.2 activation package is coherent and mutation-closed" \
    python3 "$REPO/tools/d9-wddm32-activation-gate.py" "$REPO"

run_gate "D9 WDDM 3.2 executable mutation suite" \
    python3 "$REPO/tools/d9-wddm32-activation-gate.py" "$REPO" --mutations

run_gate "K2a shared-backing CPU view is exact and lifetime-closed" \
    python3 "$REPO/tools/k2a-share-backing-store-gate.py" "$REPO"

run_gate "K2a shared-backing executable mutation suite" \
    python3 "$REPO/tools/k2a-share-backing-store-gate.py" "$REPO" --mutations

run_gate "K11 per-session stock-Venus transport is finite, lifetime-closed, and mutation-checked" \
    python3 "$REPO/tools/k11-session-transport-gate.py" "$REPO" --mutations

run_gate "K9 one-engine completion frontier is ordered, host-terminal, reset-closed, and mutation-checked" \
    python3 "$REPO/tools/k9-ordered-engine-gate.py" "$REPO" --mutations

run_gate "Post-K9 HVM1/Venus executor is bounded, direct-owned, host-terminal, and mutation-checked" \
    python3 "$REPO/tools/post-k9-executor-gate.py" "$REPO" --mutations

run_gate "Mesa A3 HVM1 renderer is escape-free, role-4-unmapped, exact-import, and mutation-checked" \
    python3 "$REPO/tools/mesa-a3-hvm-gate.py" "$REPO" --mutations

run_gate "Mesa A4 submit is mode-owned, bounded, exact-closure, same-context ordered, and mutation-checked" \
    python3 "$REPO/tools/mesa-a4-submit-gate.py" "$REPO" --mutations

run_gate "D4 classic/DMA DIRQL enqueue is capability-restricted and fixed-storage" \
    python3 "$REPO/tools/d4-dirql-gate.py" "$REPO"

run_gate "D5 MPO Present is bounded, exact-allocation, ordinary-packet, and lease-free" \
    python3 "$REPO/tools/d5-present-mpo-gate.py" "$REPO"

run_gate "K7 active native-fence surface is conjunctive, per-adapter, bounded, and lifecycle-closed" \
    python3 "$REPO/tools/k7-native-fence-gate.py" "$REPO"

# Each K7 mutation is an in-memory source overlay passed through the same gate
# checkers used above; unchanged D4/D5 ancestry is not re-parsed per case.
run_gate "K7 native-fence executable mutation suite" \
    python3 "$REPO/tools/k7-native-fence-gate.py" "$REPO" --mutations

printf '\n'
if [ ${#FAILED[@]} -eq 0 ]; then
    if [ ${#SKIPPED[@]} -eq 0 ]; then
        printf 'ALL LINUX-SIDE RETIREMENT GATES PASS\n'
    else
        # ⛔ NEVER "ALL … PASS" WHEN A GATE DID NOT RUN.
        #
        # It printed exactly that with 7 PASS + 1 SKIP, and it cost round 3 a
        # real confusion: two reviewers on the SAME commit disagreed about the
        # baseline (7 PASS + 1 SKIP vs 8 PASS) because one had `tmp/wdk-28000`
        # staged and the other did not. `.gitignore` excludes `/tmp/`, so a
        # fresh clone always takes the SKIP arm — and with an intentionally
        # stale `wddm32_slot_audit.rs` the same tree exits 1 with the headers
        # present and 0 without them.
        printf 'ALL RUN GATES PASS (%d SKIPPED: %s) -- this suite result is CONDITIONAL\n' \
            ${#SKIPPED[@]} "${SKIPPED[*]}"
    fi
    exit 0
fi
printf 'FAILED: %s\n' "${FAILED[*]}"
exit 1
