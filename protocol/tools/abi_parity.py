#!/usr/bin/env python3
"""Mechanically compare the Rust wire-ABI layout assertions against their C twins.

`protocol/` is the single declaration site for every shared guest/host wire
record (CLAUDE.md / `docs/retirement/OWNERSHIP.md` section 4), and the C mirrors
in `protocol/include/` are hand-maintained.  A hand-maintained mirror needs a
mechanical comparison, not a careful reader: the 2026-08-10 `translator_dispatch`
repair found a mirror generated from an older revision, a "every validator has a
C twin" table where 9 of 12 twins did not exist, and two transposed offsets --
all of them in a file whose own `_Static_assert`s exist to catch exactly that.

What this checks, and what it deliberately does not
---------------------------------------------------
It compares four claim kinds that both languages state explicitly:

    Rust  assert!(size_of::<T>() == N)     C  _Static_assert(sizeof(T) == N)
    Rust  assert!(align_of::<T>() == N)    C  _Static_assert(_Alignof(T) == N)
    Rust  assert!(offset_of!(T, f) == N)   C  _Static_assert(offsetof(T, f) == N)
    Rust  pub const NAME: ty = V;          C  #define NAME V

and reports four defect classes:

  MISMATCH   both sides assert a LAYOUT claim and disagree on the number.  This
             is the ABI break: two compilers would lay the record out
             differently and the wire would silently corrupt.
  CONST-MIS  both sides declare a constant of the same name and disagree on its
             VALUE.  A layout can be identical while the two sides disagree
             about what a record MEANS.
  RUST-ONLY  a record Rust pins that C does not.  The C side is then free to
             drift, which is how a mirror gets generated from an older revision
             and nothing notices.
  C-ONLY     the mirror pins something Rust does not.  Usually a stale record
             the Rust side has deleted or renamed.

⛔ CONST-MIS was added 2026-08-10 because for one round this script built both
`consts` dictionaries and then used them ONLY to fold the right-hand side of a
layout assertion -- the values themselves were never compared.  Mutation-testing
every constant declared on both sides (XOR 0x10 into the C `#define`) put **173
of 174** through this script with exit 0, including every record magic
(`HELIOS_HWA2_MAGIC`, `HELIOS_HOB1_MAGIC`, `HELIOS_HOC1_MAGIC`,
`HELIOS_HOS1_MAGIC`), every ABI version (`HELIOS_HWA2_ABI_VERSION`,
`HELIOS_HNR2_ABI_VERSION`), and the `HELIOS_HWA2_KIND_*` / `_SWIZZLE_*` /
`_MEMORY_*` enums that decide which arm `classify_hwa2` takes.  The handful that
were caught were caught by `_Static_assert`s inside the headers -- that is the
C-mirror COMPILE gate doing its job, not this script.  Re-running the same
mutation test with the comparison and the expression folder in place: **241 of
241 caught, 0 silent**, and 247 of the 248 names declared on both sides have
their values compared.  The one that does not is the bitwise complement
described below, and it says so.

⚠ It does NOT prove the structs match.  Both sides asserting `sizeof == 40` says
nothing about field ORDER unless the offsets are also asserted, and a field
present on one side only is invisible here if the sizes still agree.  It is a
cheap gate over the claims that ARE written down, which is strictly more than a
human reading two files in different languages will do reliably.  The compiler
remains the authority on each side separately; this checks that the two
authorities were asked the same question.

⚠ And the bound worth recording: there is **no C consumer of
`protocol/include/*.h` in the tree today**.  `qemu-helios/` is at its
pre-retirement base (`docs/retirement/FINDINGS.md` F5 -- HPM1 is DECLINED) and
the ICD includes only `helios_wddm.h`.  So the constant comparison closes a hole
BEFORE a consumer exists; nothing observed today would have caught a divergence,
and nothing today would have suffered from one either.

Exit codes
----------
  0  every shared claim agrees, and every record is pinned on both sides
  1  at least one MISMATCH, CONST-MISMATCH, RUST-ONLY or C-ONLY

`--allow-one-sided` downgrades RUST-ONLY/C-ONLY to warnings, for the period
while a record is being migrated.  A MISMATCH and a CONST-MISMATCH are never
downgradable.
"""

from __future__ import annotations

import argparse
import ast
import os
import re
import sys

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
SRC = os.path.join(REPO, "protocol", "src")
INC = os.path.join(REPO, "protocol", "include")

# ---------------------------------------------------------------- Rust side

# `pub const NAME: ty = 123;`  -- the RHS of an assertion is often a named
# constant (`== HELIOS_HVC1_SIZE as usize`) rather than a literal.
RUST_CONST_RE = re.compile(
    r"^\s*pub const ([A-Z][A-Z0-9_]*)\s*:\s*[A-Za-z0-9_]+\s*=\s*([^;]+);", re.M
)

RUST_SIZE_RE = re.compile(
    r"assert!\(\s*(?:core::mem::)?size_of::<\s*([A-Za-z0-9_]+)\s*>\(\)\s*==\s*([^,)]+?)\s*[,)]"
)
RUST_ALIGN_RE = re.compile(
    r"assert!\(\s*(?:core::mem::)?align_of::<\s*([A-Za-z0-9_]+)\s*>\(\)\s*==\s*([^,)]+?)\s*[,)]"
)
RUST_OFFSET_RE = re.compile(
    r"assert!\(\s*(?:core::mem::)?offset_of!\(\s*([A-Za-z0-9_]+)\s*,\s*([A-Za-z0-9_]+)\s*\)\s*==\s*([^,)]+?)\s*[,)]"
)

# ------------------------------------------------------------------- C side

C_SIZE_RE = re.compile(r"STATIC_ASSERT\(\s*sizeof\(\s*([A-Za-z0-9_]+)\s*\)\s*==\s*([^,)]+?)\s*[,)]")
# ⚠ The mirrors do NOT write `_Alignof` at the assertion site. Each header
# defines its own `HELIOS_<AREA>_ALIGNOF(type)` that selects `alignof` or
# `_Alignof` by language version, and asserts through that. A parser that looks
# only for the bare keyword concludes the mirrors pin no alignment at all —
# which is how this script's first run produced 32 false "RUST-ONLY" findings
# against 32 assertions that were right there. Match both forms.
C_ALIGN_RE = re.compile(
    r"STATIC_ASSERT\(\s*(?:_Alignof|alignof|HELIOS_\w+_ALIGNOF)\(\s*([A-Za-z0-9_]+)\s*\)\s*==\s*([^,)]+?)\s*[,)]"
)
C_OFFSET_RE = re.compile(
    r"STATIC_ASSERT\(\s*offsetof\(\s*([A-Za-z0-9_]+)\s*,\s*([A-Za-z0-9_]+)\s*\)\s*==\s*([^,)]+?)\s*[,)]"
)
C_DEFINE_RE = re.compile(r"^\s*#define\s+([A-Z][A-Z0-9_]*)\s+(.+?)\s*$", re.M)


def _read_all(d: str, ext: str) -> dict[str, str]:
    out = {}
    for name in sorted(os.listdir(d)):
        if name.endswith(ext):
            with open(os.path.join(d, name), encoding="utf-8") as fh:
                out[name] = fh.read()
    return out


# ── Folding a right-hand side.
#
# ⛔ Deliberately ABSENT from the grammar: `~` (C) and `!` (Rust), and division.
# A bitwise complement's value depends on the integer WIDTH, which the two
# languages state in different places (`!HELIOS_ETW_FLAG_SUBKIND_MASK` on a Rust
# `u32` vs `((uint32_t)~HELIOS_ETW_FLAG_SUBKIND_MASK)`), and Python's integers are
# unbounded -- folding both in Python would compare two numbers that are not the
# claim either side made.  Those stay UNRESOLVED, which is the honest answer.
_FOLD_NODES = (
    ast.Expression, ast.BinOp, ast.UnaryOp, ast.Constant, ast.Name, ast.Load,
    ast.LShift, ast.RShift, ast.Add, ast.Sub, ast.Mult,
    ast.BitOr, ast.BitAnd, ast.BitXor, ast.USub, ast.UAdd,
)
_INT_MAX = {"u8": 0xFF, "u16": 0xFFFF, "u32": 0xFFFF_FFFF, "u64": 0xFFFF_FFFF_FFFF_FFFF,
            "usize": 0xFFFF_FFFF_FFFF_FFFF}


def _normalize(expr: str) -> str:
    """Rewrite the C and Rust spellings of an integer expression into one that
    Python's parser accepts, WITHOUT changing any value."""
    # A Rust `pub const` may wrap its `|`-chain over several lines. Python treats
    # a bare newline inside an expression as the end of it, so collapse first.
    e = re.sub(r"\s+", " ", expr).strip()
    e = re.sub(r"\bas\s+(usize|u\d+|i\d+)\b", "", e)
    # C fixed-width literal macros: UINT64_C(3) -> (3)
    e = re.sub(r"\b(?:U?INT(?:8|16|32|64|MAX|PTR)?_C)\s*\(", "(", e)
    # C casts this script knows are value-preserving for the widths in use here.
    e = re.sub(r"\(\s*(?:u?int(?:8|16|32|64)_t|size_t|unsigned(?:\s+(?:int|long|long\s+long))?)\s*\)",
               "", e)
    # Rust integer-type maxima, BEFORE the path collapse below eats `u32::`.
    e = re.sub(r"\b(u8|u16|u32|u64|usize)::MAX\b", lambda m: str(_INT_MAX[m.group(1)]), e)
    # Rust paths: `crate::wddm::NAME` -> `NAME`. `protocol/` is one crate with a
    # flat re-export, so the trailing segment is the whole identity here.
    e = re.sub(r"\b(?:[A-Za-z_]\w*::)+([A-Za-z_]\w*)\b", r"\1", e)
    # Integer suffixes anywhere, not only at the end: `1u << 0`, `0xFFFFFFFFu`.
    e = re.sub(r"\b(0[xX][0-9a-fA-F_]+|\d[\d_]*)[uUlL]+\b", r"\1", e)
    return e.strip()


def _fold(e: str, consts: dict[str, int]) -> int | None:
    """Evaluate `e` over the restricted grammar above, or None.

    `ast` with a node whitelist, never `eval` on the raw string: the input is a
    source file, and a gate that executes what it is auditing is not a gate.
    """
    try:
        tree = ast.parse(e, mode="eval")
    except SyntaxError:
        return None
    for node in ast.walk(tree):
        if not isinstance(node, _FOLD_NODES):
            return None
        if isinstance(node, ast.Constant) and not isinstance(node.value, int):
            return None
        if isinstance(node, ast.Name) and node.id not in consts:
            return None
        # A hostile or mistyped shift can allocate an arbitrarily large integer.
        if isinstance(node, ast.BinOp) and isinstance(node.op, (ast.LShift, ast.RShift)):
            if isinstance(node.right, ast.Constant) and node.right.value > 128:
                return None
    def ev(n):
        if isinstance(n, ast.Expression):
            return ev(n.body)
        if isinstance(n, ast.Constant):
            return n.value
        if isinstance(n, ast.Name):
            return consts[n.id]
        if isinstance(n, ast.UnaryOp):
            v = ev(n.operand)
            return -v if isinstance(n.op, ast.USub) else v
        a, b = ev(n.left), ev(n.right)
        return {ast.LShift: lambda: a << b, ast.RShift: lambda: a >> b,
                ast.Add: lambda: a + b, ast.Sub: lambda: a - b,
                ast.Mult: lambda: a * b, ast.BitOr: lambda: a | b,
                ast.BitAnd: lambda: a & b, ast.BitXor: lambda: a ^ b}[type(n.op)]()
    try:
        v = ev(tree)
    except (KeyError, TypeError, ValueError):
        return None
    return v if isinstance(v, int) else None


def _resolve(expr: str, consts: dict[str, int]) -> int | None:
    """Fold an assertion's right-hand side to an integer, or None if we cannot.

    Handles a literal, a named constant, the `as usize` / `u32` / `UL` suffixes
    both languages sprinkle on them, and -- since 2026-08-10 -- the small integer
    expressions the constant tables are actually written in (`1 << 3`,
    `(1u << 3)`, `UINT64_C(15728640)`, `15 * 1024 * 1024`, `A | B`,
    `crate::wddm::NAME`).  That last group is not a nicety: before it, 12 of the
    13 `HELIOS_HWA2_FLAG_*` bits and 11 of the 12 `HELIOS_HWA2_BIND_*` bits were
    declared on both sides and compared on neither, because one side writes
    `1 << 3` and the other `(1u << 3)`.

    Anything outside the grammar -- a `sizeof` of another type, a bitwise
    complement -- returns None and is reported as UNRESOLVED rather than silently
    skipped, because a claim this script cannot read is a claim it is not
    checking.
    """
    e = _normalize(expr)
    bare = e.strip("()").strip()
    if re.fullmatch(r"0[xX][0-9a-fA-F_]+", bare):
        return int(bare.replace("_", ""), 16)
    if re.fullmatch(r"[0-9_]+", bare):
        return int(bare.replace("_", ""))
    if bare in consts:
        return consts[bare]
    return _fold(e, consts)


def collect_rust() -> tuple[dict, list, dict, dict]:
    texts = _read_all(SRC, ".rs")
    consts: dict[str, int] = {}
    # `raw` keeps every declared name, resolvable or not, so that a name this
    # script cannot fold shows up as UNRESOLVED instead of vanishing.  A name
    # that vanishes is a name nobody knows is unchecked.
    raw: dict[str, str] = {}
    # two passes: a constant may be defined in terms of an earlier one
    for _ in range(3):
        for text in texts.values():
            for name, val in RUST_CONST_RE.findall(text):
                raw.setdefault(name, val.strip())
                v = _resolve(val, consts)
                if v is not None:
                    consts[name] = v
    claims: dict[tuple, tuple[int, str]] = {}
    unresolved: list[str] = []
    for fname, text in texts.items():
        for ty, val in RUST_SIZE_RE.findall(text):
            v = _resolve(val, consts)
            (claims.setdefault(("size", ty, ""), (v, fname)) if v is not None
             else unresolved.append(f"rust {fname}: size_of::<{ty}>() == {val.strip()}"))
        for ty, val in RUST_ALIGN_RE.findall(text):
            v = _resolve(val, consts)
            (claims.setdefault(("align", ty, ""), (v, fname)) if v is not None
             else unresolved.append(f"rust {fname}: align_of::<{ty}>() == {val.strip()}"))
        for ty, field, val in RUST_OFFSET_RE.findall(text):
            v = _resolve(val, consts)
            (claims.setdefault(("offset", ty, field), (v, fname)) if v is not None
             else unresolved.append(f"rust {fname}: offset_of!({ty}, {field}) == {val.strip()}"))
    return claims, unresolved, consts, raw


def collect_c() -> tuple[dict, list, dict, dict]:
    texts = _read_all(INC, ".h")
    consts: dict[str, int] = {}
    raw: dict[str, str] = {}
    # `C_DEFINE_RE` is line-anchored, so a `\`-continued macro would otherwise
    # arrive with an RHS of just `\`. Joining continuations first is what the
    # preprocessor does, and it is the difference between reading
    # HELIOS_PACKAGE_GENERATION / HELIOS_ETW_KEYWORD_ALL / the *_MASK unions and
    # filing them all as UNRESOLVED.
    joined = {k: re.sub(r"\\\n\s*", " ", v) for k, v in texts.items()}
    for _ in range(3):
        for text in joined.values():
            for name, val in C_DEFINE_RE.findall(text):
                if "(" in name:
                    continue
                raw.setdefault(name, val.strip())
                v = _resolve(val, consts)
                if v is not None:
                    consts[name] = v
    claims: dict[tuple, tuple[int, str]] = {}
    unresolved: list[str] = []
    for fname, text in texts.items():
        for ty, val in C_SIZE_RE.findall(text):
            v = _resolve(val, consts)
            (claims.setdefault(("size", ty, ""), (v, fname)) if v is not None
             else unresolved.append(f"c {fname}: sizeof({ty}) == {val.strip()}"))
        for ty, val in C_ALIGN_RE.findall(text):
            v = _resolve(val, consts)
            (claims.setdefault(("align", ty, ""), (v, fname)) if v is not None
             else unresolved.append(f"c {fname}: _Alignof({ty}) == {val.strip()}"))
        for ty, field, val in C_OFFSET_RE.findall(text):
            v = _resolve(val, consts)
            (claims.setdefault(("offset", ty, field), (v, fname)) if v is not None
             else unresolved.append(f"c {fname}: offsetof({ty}, {field}) == {val.strip()}"))
    return claims, unresolved, consts, raw


def describe(key: tuple) -> str:
    kind, ty, field = key
    return f"{kind} {ty}" + (f".{field}" if field else "")


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--allow-one-sided", action="store_true",
                    help="downgrade RUST-ONLY / C-ONLY to warnings (migration only)")
    ap.add_argument("-v", "--verbose", action="store_true")
    args = ap.parse_args()

    rust, rust_unres, rust_consts, rust_raw = collect_rust()
    c, c_unres, c_consts, c_raw = collect_c()

    # ── Claim kind 4: the VALUES of the constants both sides declare.
    #
    # `rust_consts` / `c_consts` were already being built to fold the right-hand
    # side of a layout assertion.  Comparing them costs nothing and is the only
    # check in this repo that a record MAGIC or an ABI VERSION means the same
    # thing in both languages -- see the ⛔ paragraph in the module docstring for
    # the mutation-test numbers that made this necessary.
    const_mismatches, const_unres = [], []
    shared_const_names = sorted(set(rust_raw) & set(c_raw))
    for name in shared_const_names:
        if name in rust_consts and name in c_consts:
            if rust_consts[name] != c_consts[name]:
                const_mismatches.append((name, rust_consts[name], c_consts[name]))
        else:
            side = "rust" if name not in rust_consts else "c"
            const_unres.append(
                f"const {name}: rust `{rust_raw[name]}` / c `{c_raw[name]}` "
                f"-- RHS unresolvable on the {side} side, so the VALUES were not compared")

    # Only types the C mirror knows about at all can be compared: `protocol/`
    # declares far more records than are mirrored, and that is by design --
    # a Rust-only record is a gap only if its TYPE is mirrored but a claim
    # about it is not.
    c_types = {ty for _k, ty, _f in c}
    rust_types = {ty for _k, ty, _f in rust}
    shared_types = c_types & rust_types

    mismatches, rust_only, c_only = [], [], []
    for key, (val, where) in sorted(rust.items()):
        if key[1] not in shared_types:
            continue
        if key in c:
            if c[key][0] != val:
                mismatches.append((key, val, where, c[key][0], c[key][1]))
        else:
            rust_only.append((key, val, where))
    for key, (val, where) in sorted(c.items()):
        if key[1] not in shared_types:
            continue
        if key not in rust:
            c_only.append((key, val, where))

    print(f"rust claims: {len(rust)}   c claims: {len(c)}")
    print(f"types mirrored on both sides: {len(shared_types)}")
    print(f"compared: {sum(1 for k in rust if k[1] in shared_types)} rust-side claims "
          f"over the mirrored types")
    print(f"constants declared on both sides: {len(shared_const_names)}   "
          f"values compared: {len(shared_const_names) - len(const_unres)}")

    if rust_unres or c_unres or const_unres:
        print(f"\nUNRESOLVED (claim present but this script could not fold its RHS): "
              f"{len(rust_unres) + len(c_unres) + len(const_unres)}")
        if args.verbose:
            for u in rust_unres + c_unres + const_unres:
                print(f"  {u}")

    # An UNRESOLVED claim is only harmless if the same record is pinned by some
    # OTHER claim that did resolve. Without this check the script can report
    # "OK" over a record whose only size assertion it could not read — which is
    # a silent hole exactly where a relational assertion (`== SOME_CONSTANT`)
    # is the only one a record has.
    unpinned = sorted(t for t in shared_types if ("size", t, "") not in rust)
    if unpinned:
        print(f"\nUNPINNED (mirrored type with no comparable size claim): {len(unpinned)}")
        for t in unpinned:
            print(f"  {t}")

    for key, rv, rw, cv, cw in mismatches:
        print(f"MISMATCH  {describe(key)}: rust {rv} ({rw}) != c {cv} ({cw})")
    for name, rv, cv in const_mismatches:
        print(f"CONST-MISMATCH  {name}: rust {rv} (0x{rv:X}) != c {cv} (0x{cv:X})")
    for key, val, where in rust_only:
        print(f"RUST-ONLY {describe(key)} == {val} ({where}) -- mirrored type, unmirrored claim")
    for key, val, where in c_only:
        print(f"C-ONLY    {describe(key)} == {val} ({where}) -- no Rust assertion")

    # A CONST-MISMATCH is never downgradable, for the same reason a layout
    # MISMATCH is not: both sides HAVE stated the claim and they disagree.
    bad = len(mismatches) + len(const_mismatches) + len(unpinned)
    if not args.allow_one_sided:
        bad += len(rust_only) + len(c_only)
    if bad == 0:
        print("\nOK: every shared layout claim and every shared constant value "
              "agrees on both sides")
        return 0
    print(f"\nFAIL: {len(mismatches)} mismatch, {len(const_mismatches)} const-mismatch, "
          f"{len(rust_only)} rust-only, {len(c_only)} c-only")
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
