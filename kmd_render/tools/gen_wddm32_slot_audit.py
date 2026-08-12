#!/usr/bin/env python3
"""Generate the WDDM 3.2 `DRIVER_INITIALIZATION_DATA` slot-and-cap audit.

`docs/HELIOS_PRESENT_SYNC_RETIREMENT.md` section 17.6 (line 4460) and section
18.1 (line 4653) require:

    "The WDDM-3.2 uplift is an explicit slot-and-cap audit, not a version-constant
     change.  Generate the full WDK-28000 DRIVER_INITIALIZATION_DATA layout and
     classify every slot through 3.2 as implemented or disabled behind a truthful
     zero capability."

    "the generated WDK-28000 DRIVER_INITIALIZATION_DATA slot audit covers every
     field through WDDM 3.2 ... while each cap-disabled versioned family is
     provably unreachable"

`kmd_render` is a `panic = "abort"` `no_std` cdylib and cannot host a libtest
harness; `kmd_logic` deliberately has no `wdk-sys`/bindgen edge and cannot see
`DRIVER_INITIALIZATION_DATA` at all.  There is no third home, so the audit is
implemented as (a) this generator plus its checked-in classification table, and
(b) `const _: () = assert!(...)` blocks over `size_of!`/`offset_of!` inside
`kmd_render` — compile-time and legal in `no_std` — plus one runtime
verification of the built table inside `DriverEntry`.  This shape was approved
by the owner (task brief, unit K0) as the conservative reading of the
display lane's brief section 6 item 2.

Inputs
------
  --header   a WDK ``km/dispmprt.h``.  Default: the vendored copy at
             ``kmd_render/tools/wdk-28000/km/dispmprt.h`` (see its README).
  --classes  the checked-in classification TSV (one row per slot).

Outputs
-------
  kmd_render/src/ddi/wddm32_slot_audit.rs   generated Rust (checked in)
  docs/retirement/d9-wddm32-slot-audit.md   generated human-readable table

Failure modes, all hard errors (never a silent partial table):
  * a slot in the header with no classification row;
  * a classification row naming a slot the header does not declare;
  * a row order that does not match declaration order;
  * an unknown class name.

Regenerate with::

    python3 kmd_render/tools/gen_wddm32_slot_audit.py

and commit both outputs.  The generated Rust asserts the offsets it recorded
against the WDK the VM actually builds with, so a WDK whose
`DRIVER_INITIALIZATION_DATA` differs from the one audited here fails the build
instead of shipping a short struct to a longer-expecting dxgkrnl
(`STATUS_REVISION_MISMATCH`, the class `build.rs` documents at lines 28-34).
"""

from __future__ import annotations

import argparse
import os
import re
import sys

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

# ⭐ The VENDORED header, checked in beside this generator (2026-08-10).
#
# It used to default to `tmp/wdk-28000/Include/10.0.28000.0/km/dispmprt.h`,
# which `.gitignore` excludes — so `tools/retirement-gates.sh`'s staleness gate
# SKIPPED in any fresh clone while the suite still printed "ALL ... PASS", and
# the same commit produced a different gate result on two checkouts. Round 3 of
# the Phase-2 review demonstrated that with an intentionally stale audit file.
#
# `kmd_render/tools/wdk-28000/README.md` records the package, both SHA-256s and
# the re-extraction command. This generator resolves no `#include`, which is why
# one vendored file is sufficient input.
DEFAULT_HEADER = os.path.join(
    REPO, "kmd_render", "tools", "wdk-28000", "km", "dispmprt.h"
)
DEFAULT_CLASSES = os.path.join(REPO, "kmd_render", "tools", "wddm32_slot_classes.tsv")
DEFAULT_RS = os.path.join(REPO, "kmd_render", "src", "ddi", "wddm32_slot_audit.rs")
DEFAULT_MD = os.path.join(REPO, "docs", "retirement", "d9-wddm32-slot-audit.md")

# The four classes the verifier understands.  Two are terminal and two are
# transitional, and the transitional pair is what makes the audit armable at
# all: this table describes the driver that is BUILT, not the driver the
# retirement intends.
#
#   Implemented  must be non-NULL.
#   Disabled     must be NULL, and unreachable behind a truthful zero capability.
#   Pending      Pre-D9 transition: NULL and checked like `Disabled`.
#   Retiring     Pre-D9 transition: registered and checked like `Implemented`.
#
# The generator retains those two classes so an older milestone can be audited,
# but D9's activation gate requires both counts to be zero.
CLASSES = ("Implemented", "Disabled", "Pending", "Retiring")

# Every member of DRIVER_INITIALIZATION_DATA other than `Version` is a pointer,
# so the layout is `Version` (ULONG) + 4 bytes of padding + one 8-byte slot each.
# The generated Rust asserts this against the real bindgen struct rather than
# trusting it.
PTR = 8
FIRST_SLOT_OFFSET = 8

GUARD_RE = re.compile(
    r"#if \(DXGKDDI_INTERFACE_VERSION >= DXGKDDI_INTERFACE_VERSION_(\w+)\)"
)
MEMBER_RE = re.compile(r"^([A-Za-z_][\w*]*)\s+([A-Za-z_]\w*);$")


def parse_header(path: str) -> list[tuple[str, str, str]]:
    """Return [(slot_name, c_type, min_version_guard)] in declaration order."""
    with open(path, encoding="utf-8", errors="replace") as fh:
        text = fh.read()
    start = text.find("typedef struct _DRIVER_INITIALIZATION_DATA")
    if start < 0:
        raise SystemExit(f"{path}: no DRIVER_INITIALIZATION_DATA declaration")
    end = text.find("} DRIVER_INITIALIZATION_DATA", start)
    if end < 0:
        raise SystemExit(f"{path}: unterminated DRIVER_INITIALIZATION_DATA")
    body = text[start:end]

    slots: list[tuple[str, str, str]] = []
    guard = "BASE"
    depth_guard: list[str] = []
    for raw in body.split("\n"):
        line = raw.strip()
        m = GUARD_RE.match(line)
        if m:
            depth_guard.append(guard)
            guard = m.group(1)
            continue
        if line.startswith("#endif"):
            guard = depth_guard.pop() if depth_guard else "BASE"
            continue
        m = MEMBER_RE.match(line)
        if not m:
            continue
        ctype, name = m.group(1), m.group(2)
        if name == "Version":
            continue
        slots.append((name, ctype, guard))
    if not slots:
        raise SystemExit(f"{path}: parsed zero slots — the parser is out of date")
    return slots


def parse_classes(path: str) -> list[tuple[str, str, str]]:
    """Return [(slot_name, class, reason)] in file order, comments stripped."""
    rows: list[tuple[str, str, str]] = []
    with open(path, encoding="utf-8") as fh:
        for lineno, raw in enumerate(fh, 1):
            line = raw.rstrip("\n")
            if not line.strip() or line.lstrip().startswith("#"):
                continue
            parts = line.split("\t")
            if len(parts) != 3:
                raise SystemExit(
                    f"{path}:{lineno}: expected 3 tab-separated fields, got {len(parts)}"
                )
            name, cls, reason = (p.strip() for p in parts)
            if cls not in CLASSES:
                raise SystemExit(f"{path}:{lineno}: unknown class {cls!r}")
            if '"' in reason or "\\" in reason:
                raise SystemExit(f"{path}:{lineno}: reason must not contain \" or \\")
            rows.append((name, cls, reason))
    return rows


def reconcile(
    slots: list[tuple[str, str, str]], rows: list[tuple[str, str, str]]
) -> list[tuple[int, str, str, str, str, str]]:
    """Join header order with the classification; hard-fail on any divergence."""
    header_names = [s[0] for s in slots]
    class_names = [r[0] for r in rows]
    if header_names != class_names:
        missing = [n for n in header_names if n not in class_names]
        extra = [n for n in class_names if n not in header_names]
        msg = ["classification does not match the header slot list."]
        if missing:
            msg.append(f"  unclassified header slots: {missing}")
        if extra:
            msg.append(f"  classified slots absent from the header: {extra}")
        if not missing and not extra:
            msg.append("  same set, different order — the TSV must be in declaration order")
        raise SystemExit("\n".join(msg))
    return [
        (i, name, ctype, guard, cls, reason)
        for i, ((name, ctype, guard), (_, cls, reason)) in enumerate(zip(slots, rows))
    ]


RS_HEADER = '''//! GENERATED — do not edit by hand.
//!
//! The WDDM 3.2 `DRIVER_INITIALIZATION_DATA` slot-and-cap audit required by
//! `docs/HELIOS_PRESENT_SYNC_RETIREMENT.md` section 17.6:4460-4462 and graded by
//! section 18.1:4653-4656.
//!
//! Regenerate with `python3 kmd_render/tools/gen_wddm32_slot_audit.py`; the
//! classification lives in `kmd_render/tools/wddm32_slot_classes.tsv` and the
//! human-readable rendering in `docs/retirement/d9-wddm32-slot-audit.md`.
//!
//! # What this file proves, and how
//!
//! 1. **Layout.** One `const _: () = assert!(offset_of!(..) == ..)` per slot,
//!    plus one on `size_of::<DRIVER_INITIALIZATION_DATA>()`. These are evaluated
//!    against the WDK the build actually bindgens, so a WDK whose table differs
//!    from the audited one is a build failure rather than a short struct handed
//!    to a longer-expecting dxgkrnl (`STATUS_REVISION_MISMATCH`).
//! 2. **Classification.** [`SLOTS`] carries every slot through WDDM 3.2 as
//!    terminal `Implemented` or `Disabled`. The generator retains the two
//!    pre-D9 transition classes, but the D9 activation gate requires zero rows
//!    of either class.
//! 3. **Agreement.** [`verify`] walks the table `build_ddi_table()` actually
//!    produced and checks each slot's pointer word against its class. A
//!    disagreement fails `DriverEntry` — a registered slot that the audit calls
//!    unreachable, or a NULL slot the audit calls implemented, is a driver bug
//!    and must not load.
//!
//! `kmd_render` cannot host a libtest harness (`panic = "abort"` `no_std`
//! cdylib) and `kmd_logic` has no `wdk-sys` edge, so 1-3 are the only three
//! mechanisms available; see the generator's module docs.

use core::mem::{offset_of, size_of};

use crate::dxgk::DRIVER_INITIALIZATION_DATA;

/// How a slot is expected to appear in the built table.
#[allow(
    dead_code,
    reason = "generator supports pre-D9 transition tables; the D9 gate requires zero rows"
)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SlotClass {
    /// Registered and backed by a real implementation. Must be non-NULL.
    Implemented,
    /// Deliberately unregistered, and unreachable because the capability that
    /// would reach it is reported as zero/absent. Must be NULL.
    Disabled,
    /// Pre-D9 transition: unregistered and verified like [`Self::Disabled`].
    Pending,
    /// Pre-D9 transition: registered and verified like [`Self::Implemented`].
    Retiring,
}

/// One audited slot of `DRIVER_INITIALIZATION_DATA`.
pub(crate) struct SlotAudit {
    /// The WDK field name.
    pub(crate) name: &'static str,
    /// Byte offset inside `DRIVER_INITIALIZATION_DATA`, asserted below.
    pub(crate) offset: usize,
    /// The `DXGKDDI_INTERFACE_VERSION_*` guard the WDK declares it under.
    pub(crate) min_version: &'static str,
    /// Expected presence in the built table.
    pub(crate) class: SlotClass,
    /// Why. For `Disabled`, the truthful zero capability; for `Pending` and
    /// `Retiring`, the owning lane and the normative line.
    pub(crate) reason: &'static str,
}

/// Why [`verify`] refused the built table.
pub(crate) struct SlotAuditFailure {
    /// Index into [`SLOTS`].
    pub(crate) index: usize,
    /// The offending slot.
    pub(crate) name: &'static str,
    /// `true` when the slot was registered but classified unreachable;
    /// `false` when it was classified implemented but is NULL.
    pub(crate) registered: bool,
}
'''

RS_TAIL = '''
/// Compile-time proof that the audited struct is the struct we bindgen.
///
/// `DRIVER_INITIALIZATION_DATA` is `Version: ULONG` followed by
/// [`SLOT_COUNT`] pointers, so the size is fully determined by the slot count.
/// A WDK that adds, removes or retypes a slot changes this number and fails the
/// build here rather than at `DxgkInitialize`.
const _: () = assert!(
    size_of::<DRIVER_INITIALIZATION_DATA>() == EXPECTED_STRUCT_SIZE,
    "DRIVER_INITIALIZATION_DATA size differs from the audited WDDM 3.2 layout; \\
     regenerate kmd_render/src/ddi/wddm32_slot_audit.rs against the installed WDK"
);

/// Walk the table `build_ddi_table()` produced and check every slot against its
/// classification.
///
/// Reads the raw pointer word at each audited offset rather than the typed
/// field, so one loop covers all [`SLOT_COUNT`] slots including the WDK's
/// `PVOID`/`Reserved*` members. The offsets are the compile-time-asserted ones
/// above, so this cannot read outside the struct.
///
/// Returns the FIRST disagreement; `DriverEntry` reports it and refuses to load.
pub(crate) fn verify(data: &DRIVER_INITIALIZATION_DATA) -> Result<(), SlotAuditFailure> {
    let base = (data as *const DRIVER_INITIALIZATION_DATA).cast::<u8>();
    let mut i = 0;
    while i < SLOTS.len() {
        let slot = &SLOTS[i];
        // SAFETY: `slot.offset` is one of the offsets asserted at compile time
        // above to lie inside `DRIVER_INITIALIZATION_DATA`, and every member
        // after `Version` is a pointer-sized word. `read_unaligned` because the
        // caller's struct alignment is not this function's to assume.
        let word = unsafe { base.add(slot.offset).cast::<usize>().read_unaligned() };
        let registered = word != 0;
        // `Retiring` is verified like `Implemented` and `Pending` like
        // `Disabled`: the audit checks the table that is BUILT, and the two
        // transitional classes are the two directions it is still moving in.
        let expected = matches!(slot.class, SlotClass::Implemented | SlotClass::Retiring);
        if registered != expected {
            return Err(SlotAuditFailure {
                index: i,
                name: slot.name,
                registered,
            });
        }
        i += 1;
    }
    Ok(())
}
'''


def emit_rs(joined, header_path: str) -> str:
    out: list[str] = [RS_HEADER]
    n = len(joined)
    out.append("\n/// Number of audited slots (every member except `Version`).\n")
    out.append(f"pub(crate) const SLOT_COUNT: usize = {n};\n")
    out.append(
        "\n/// `Version` (ULONG + 4 bytes of padding) plus one pointer per slot.\n"
    )
    out.append(
        f"const EXPECTED_STRUCT_SIZE: usize = {FIRST_SLOT_OFFSET} + SLOT_COUNT * {PTR};\n"
    )
    out.append(
        f"\n/// The audited header: `{os.path.relpath(header_path, REPO)}`.\n"
        f"#[allow(dead_code, reason = \"provenance for the audit, read by humans\")]\n"
        f"pub(crate) const AUDITED_HEADER: &str = \"{os.path.relpath(header_path, REPO)}\";\n"
    )

    out.append("\n/// The classification, in WDK declaration order.\n")
    out.append(f"pub(crate) const SLOTS: [SlotAudit; SLOT_COUNT] = [\n")
    for i, name, _ctype, guard, cls, reason in joined:
        off = FIRST_SLOT_OFFSET + i * PTR
        out.append("    SlotAudit {\n")
        out.append(f'        name: "{name}",\n')
        out.append(f"        offset: {off},\n")
        out.append(f'        min_version: "{guard}",\n')
        out.append(f"        class: SlotClass::{cls},\n")
        out.append(f'        reason: "{reason}",\n')
        out.append("    },\n")
    out.append("];\n")

    out.append(
        "\n// One offset assertion per slot. Written out rather than looped so the\n"
        "// failing line names the slot that moved.\n"
    )
    for i, name, _ctype, _guard, _cls, _reason in joined:
        off = FIRST_SLOT_OFFSET + i * PTR
        out.append(
            f"const _: () = assert!(offset_of!(DRIVER_INITIALIZATION_DATA, {name}) == {off});\n"
        )

    out.append(RS_TAIL)
    return "".join(out)


def emit_md(joined, header_path: str) -> str:
    counts = {c: 0 for c in CLASSES}
    for _i, _n, _t, _g, cls, _r in joined:
        counts[cls] += 1
    lines = [
        "# WDDM 3.2 `DRIVER_INITIALIZATION_DATA` slot-and-cap audit",
        "",
        "GENERATED — do not edit by hand. Regenerate with",
        "`python3 kmd_render/tools/gen_wddm32_slot_audit.py`; edit the",
        "classification in `kmd_render/tools/wddm32_slot_classes.tsv`.",
        "",
        f"Audited header: `{os.path.relpath(header_path, REPO)}`",
        "",
        f"Slots: **{len(joined)}** (plus `Version`), struct size "
        f"**{FIRST_SLOT_OFFSET + len(joined) * PTR}** bytes.",
        "",
        f"* `Implemented` — {counts['Implemented']}",
        f"* `Disabled` — {counts['Disabled']} (unreachable behind a truthful zero capability)",
        f"* `Pending` — {counts['Pending']} (pre-D9 only; D9 requires zero)",
        f"* `Retiring` — {counts['Retiring']} (pre-D9 only; D9 requires zero)",
        "",
        "The machine-checked half of this table lives in",
        "`kmd_render/src/ddi/wddm32_slot_audit.rs`: compile-time `offset_of!`/",
        "`size_of!` assertions plus a `DriverEntry` walk of the built table that",
        "refuses to load if any slot disagrees with its class here.",
        "",
        "| # | Offset | Slot | Since | Class | Reason |",
        "|--:|-------:|------|-------|-------|--------|",
    ]
    for i, name, _ctype, guard, cls, reason in joined:
        off = FIRST_SLOT_OFFSET + i * PTR
        lines.append(f"| {i} | {off} | `{name}` | {guard} | {cls} | {reason} |")
    lines.append("")
    return "\n".join(lines)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--header", default=DEFAULT_HEADER)
    ap.add_argument("--classes", default=DEFAULT_CLASSES)
    ap.add_argument("--rs", default=DEFAULT_RS)
    ap.add_argument("--md", default=DEFAULT_MD)
    ap.add_argument(
        "--check",
        action="store_true",
        help="fail instead of writing when an output is stale",
    )
    args = ap.parse_args()

    slots = parse_header(args.header)
    rows = parse_classes(args.classes)
    joined = reconcile(slots, rows)

    rs = emit_rs(joined, args.header)
    md = emit_md(joined, args.header)

    stale = False
    for path, text in ((args.rs, rs), (args.md, md)):
        old = None
        if os.path.exists(path):
            with open(path, encoding="utf-8") as fh:
                old = fh.read()
        if old == text:
            continue
        stale = True
        if args.check:
            print(f"STALE: {path}", file=sys.stderr)
            continue
        with open(path, "w", encoding="utf-8") as fh:
            fh.write(text)
        print(f"wrote {path}")

    if args.check and stale:
        return 1
    if not stale:
        print("up to date")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
