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
# ⚠ WHAT THESE TWO GATES CAN AND CANNOT CATCH — re-derived 2026-08-10 from what
# the code below actually does, after a completeness critic DEFEATED the §8.5
# gate with a four-line patch. The block it replaced claimed both halves of that
# patch ("a callee whose signature takes the buffer as `*mut`", "a `*mut` cast of
# the buffer") and caught neither, because the old gate matched on the LITERAL
# field name: rebinding the pointer to a local erased every one of its rules. The
# gates are textual; they read `kmd_render`, which does not build on Linux at
# all, so they are the ONLY mechanical statement about that crate available on
# this host, and they are weaker than a compiler by construction.
#
# §8.5 now tracks ALIASES. `pPrivateDriverData` seeds a set of names; a `let` or
# an assignment whose initialiser mentions a live alias and applies nothing to it
# but casts, `&`/`*` reborrows and the pointer methods in `PTR_METHODS` binds
# ANOTHER name for the same pointer, and every rule below runs against the whole
# set. A call is followed when ANY alias appears in its argument list, and the
# parameter that received it becomes the alias inside the callee.
#   * CAN: a restamp reintroduced in `DxgkDdiOpenAllocation` or in anything it
#     hands the private pointer to, INCLUDING after the pointer is rebound to one
#     or more locals; a callee whose signature takes any alias as `*mut`; a `*mut`
#     cast of any alias; an assignment through any alias; any write intrinsic in
#     the DDI or in a followed callee; a retired symbol back as a type, a call, or
#     a string. Aliasing is deliberately over-approximate — an alias stays live to
#     the end of its region regardless of branches and regardless of shadowing —
#     so the errors it can make are false ALARMS, not misses.
#   * CANNOT: a write two calls deep (the flow is followed exactly ONE level from
#     the DDI body; aliases ARE propagated inside a followed callee, but a call it
#     makes in turn is not followed); a pointer laundered through a struct field,
#     a slice, a `Vec`, a tuple/destructuring `let`, or a closure capture (only
#     plain `let x = …` / `x = …` bindings carry an alias); a write through a
#     pointer stashed in a struct at create time and dereferenced at open; a write
#     emitted by a macro; a write in a callee defined outside `kmd_render/src`.
#     The one-level limit is enforced rather than assumed: a callee this gate
#     cannot find, cannot parse, or cannot prove `*const` is a FAILURE, not a
#     skip, so the blind spot has to be opened deliberately.
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
K4_OBLIGATION5_PY=$(cat <<'PY'
REPO = sys.argv[1]
ROOT = REPO + '/kmd_render/src'
PATH = ROOT + '/ddi/create_allocation.rs'
BUF = 'pPrivateDriverData'
DDI = 'dxgkddi_open_allocation'
WRITE_INTRINSICS = [
    'copy_nonoverlapping', 'write_bytes', 'write_volatile', 'write_unaligned',
    'ptr::write', '.write(', 'copy_from_slice', 'clone_from_slice',
    'from_raw_parts_mut', 'copy_to', 'write_open_identity',
]
# A binding whose initialiser applies ONLY these to an alias is holding the very
# same pointer, so it is another name for the buffer. Anything else -- notably a
# function call -- is assumed to produce a NEW value, which is why `let desc =
# read_open_descriptor(buf, len)` does not make `desc` an alias and does not drag
# every later `Box::new(..desc..)` into the callee-following pass. The one
# exception is a followed callee that RETURNS a pointer or a reference: that
# return value can still be the buffer, so `PTR_RET` re-admits it (below).
PTR_METHODS = set(
    'add offset sub cast cast_mut cast_const wrapping_add wrapping_sub '
    'wrapping_offset byte_add byte_sub byte_offset as_ptr as_mut_ptr'.split())
NOT_CALLS = set('if while for match return unsafe fn else loop as'.split())

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

def ptr_preserving(init, live, ptr_ret):
    if not named(init, live):
        return False
    for is_method, name, _ in calls_in(init):
        if is_method and name in PTR_METHODS:
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

ddi_text = body_from(code, m.start())
bad = []
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
        hit = None
        for f in [PATH] + [x for x in codes if x != PATH]:
            fm = re.search(fn_pattern(name), codes[f], re.M)
            if fm:
                hit = (f, codes[f], fm)
                break
        if hit is None:
            sys.exit('the open path hands the private buffer to `' + name + '`, whose definition this gate cannot find under kmd_render/src. It cannot vouch for a callee it cannot read -- FIX THIS GATE (or bring the callee back in-tree), do not delete it')
        f, c, fm = hit
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
    lineof = dict((a, 0 if o < 0 else base + text.count('\n', 0, o)) for a, o in alias_at.items())
    alias_names |= set(a for a in alias_at if a != BUF)
    for off, ln in enumerate(text.split('\n')):
        n = base + off
        live = [a for a, l in lineof.items() if l <= n]
        for w in WRITE_INTRINSICS:
            if w in ln:
                bad.append('%s:%d: write intrinsic `%s` in %s -- %s' % (f, n, w, name, ln.strip()))
        if re.search(r'\b' + BUF + r'\b', ln):
            refs += 1
        hits = named(ln, live)
        # In the DDI body `*mut` is LEGAL: DXGKARG_OPENALLOCATION's OUT fields
        # (hDeviceSpecificAllocation, Pitch, SubresourceOffset) are written
        # through one, and the DDI says so in its own SAFETY comment. So there it
        # is banned only on a line that also names an ALIAS of the private
        # buffer -- which is what `let stamp = base as *mut u32;` is, and what the
        # literal-name form of this rule missed. Inside a followed callee it is
        # banned outright: that callee took the buffer as `*const`, so
        # manufacturing a `*mut` is exactly the laundering step.
        if '*mut' in ln and (name != DDI or hits):
            bad.append('%s:%d: `*mut` reachable from the private buffer in %s (via %s) -- %s'
                       % (f, n, name, ', '.join(hits) or 'the callee parameter', ln.strip()))
        if not hits:
            continue
        a = re.search(r'[^=!<>+\-*/&|^]=[^=]', ln)
        if a and named(ln[:a.start() + 1], live) and 'let ' not in ln[:a.start()]:
            bad.append('%s:%d: assignment INTO the private buffer in %s -- %s' % (f, n, name, ln.strip()))

if refs == 0 or len(regions) < 2:
    sys.exit('the gate found no private-buffer flow in %s (refs=%d, followed=%d). Either the DDI stopped reading private data or the gate went blind -- FIX THIS GATE, do not delete it' % (DDI, refs, len(regions) - 1))
if bad:
    sys.exit('OBLIGATION 5 VIOLATED -- DxgkDdiOpenAllocation writes the allocation-private buffer:\n' + '\n'.join(bad))
print('OK: %s + %d const-only callee(s) [%s]: %d %s reference(s), %d alias(es)%s -- no write intrinsic, no *mut, no assignment'
      % (DDI, len(regions) - 1, ', '.join(r[1] for r in regions[1:]), refs,
         BUF, len(alias_names),
         (' [' + ', '.join(sorted(alias_names)) + ']') if alias_names else ''))
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
