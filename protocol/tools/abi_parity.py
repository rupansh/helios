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
It compares three claim kinds that both languages state explicitly:

    Rust  assert!(size_of::<T>() == N)     C  _Static_assert(sizeof(T) == N)
    Rust  assert!(align_of::<T>() == N)    C  _Static_assert(_Alignof(T) == N)
    Rust  assert!(offset_of!(T, f) == N)   C  _Static_assert(offsetof(T, f) == N)

and reports three defect classes:

  MISMATCH   both sides assert the claim and disagree on the number.  This is
             the ABI break: two compilers would lay the record out differently
             and the wire would silently corrupt.
  RUST-ONLY  a record Rust pins that C does not.  The C side is then free to
             drift, which is how a mirror gets generated from an older revision
             and nothing notices.
  C-ONLY     the mirror pins something Rust does not.  Usually a stale record
             the Rust side has deleted or renamed.

⚠ It does NOT prove the structs match.  Both sides asserting `sizeof == 40` says
nothing about field ORDER unless the offsets are also asserted, and a field
present on one side only is invisible here if the sizes still agree.  It is a
cheap gate over the claims that ARE written down, which is strictly more than a
human reading two files in different languages will do reliably.  The compiler
remains the authority on each side separately; this checks that the two
authorities were asked the same question.

Exit codes
----------
  0  every shared claim agrees, and every record is pinned on both sides
  1  at least one MISMATCH, RUST-ONLY or C-ONLY

`--allow-one-sided` downgrades RUST-ONLY/C-ONLY to warnings, for the period
while a record is being migrated.  A MISMATCH is never downgradable.
"""

from __future__ import annotations

import argparse
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


def _resolve(expr: str, consts: dict[str, int]) -> int | None:
    """Fold an assertion's right-hand side to an integer, or None if we cannot.

    Handles a literal, a named constant, and the `as usize` / `u32` / `UL`
    suffixes both languages sprinkle on them.  Anything else -- arithmetic, a
    sizeof of another type -- returns None and is reported as UNRESOLVED rather
    than silently skipped, because a claim this script cannot read is a claim
    it is not checking.
    """
    e = expr.strip()
    e = re.sub(r"\bas\s+(usize|u\d+|i\d+)\b", "", e).strip()
    e = re.sub(r"[uU][lL]*$|[lL][lL]*[uU]?$", "", e).strip()
    e = e.strip("()").strip()
    if re.fullmatch(r"0[xX][0-9a-fA-F_]+", e):
        return int(e.replace("_", ""), 16)
    if re.fullmatch(r"[0-9_]+", e):
        return int(e.replace("_", ""))
    if e in consts:
        return consts[e]
    return None


def collect_rust() -> tuple[dict, list]:
    texts = _read_all(SRC, ".rs")
    consts: dict[str, int] = {}
    # two passes: a constant may be defined in terms of an earlier one
    for _ in range(3):
        for text in texts.values():
            for name, val in RUST_CONST_RE.findall(text):
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
    return claims, unresolved


def collect_c() -> tuple[dict, list]:
    texts = _read_all(INC, ".h")
    consts: dict[str, int] = {}
    for _ in range(3):
        for text in texts.values():
            for name, val in C_DEFINE_RE.findall(text):
                if "(" in name:
                    continue
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
    return claims, unresolved


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

    rust, rust_unres = collect_rust()
    c, c_unres = collect_c()

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

    if rust_unres or c_unres:
        print(f"\nUNRESOLVED (claim present but this script could not fold its RHS): "
              f"{len(rust_unres) + len(c_unres)}")
        if args.verbose:
            for u in rust_unres + c_unres:
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
    for key, val, where in rust_only:
        print(f"RUST-ONLY {describe(key)} == {val} ({where}) -- mirrored type, unmirrored claim")
    for key, val, where in c_only:
        print(f"C-ONLY    {describe(key)} == {val} ({where}) -- no Rust assertion")

    bad = len(mismatches) + len(unpinned)
    if not args.allow_one_sided:
        bad += len(rust_only) + len(c_only)
    if bad == 0:
        print("\nOK: every shared layout claim agrees on both sides")
        return 0
    print(f"\nFAIL: {len(mismatches)} mismatch, {len(rust_only)} rust-only, "
          f"{len(c_only)} c-only")
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
