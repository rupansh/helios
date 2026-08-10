# Vendored WDK 10.0.28000 header

One file, checked in on purpose: `km/dispmprt.h`.

## Why it is here

`gen_wddm32_slot_audit.py` parses `DRIVER_INITIALIZATION_DATA` out of this header
to generate `kmd_render/src/ddi/wddm32_slot_audit.rs`, and
`tools/retirement-gates.sh` re-runs the generator with `--check` to prove the
committed `.rs` is not stale. That gate is the only thing standing between this
driver and the failure `docs/retirement/FINDINGS.md` F6 records: the audit file
says *"GENERATED — do not edit by hand"*, and following its own regenerate
instruction once reverted the arming fix and would have shipped a driver that
refuses to load at `DriverEntry` with `0xC0000182`.

**The gate runs on the Linux host, where no Windows kit is installed.** Until
2026-08-10 it read a `.gitignore`d copy under `tmp/wdk-28000/`, so it *skipped*
in any fresh clone — and the suite still printed "ALL … PASS". Round 3 of the
Phase-2 review demonstrated it: the same commit exits 1 with the tree present
and 0 without it. Checking in the one header the generator actually reads makes
the gate unconditional and gives it tracked ground truth that cannot silently
differ between two checkouts of the same commit.

⚠ **It is an input, not a source.** Do not edit it, do not patch it, and do not
read it as documentation of what Helios implements — `wddm32_slot_classes.tsv`
is where this project's own claims live. If it is ever updated, the update is a
re-extraction from the package below, never a hand edit.

## Provenance

| | |
|---|---|
| Package | `Microsoft.Windows.WDK.x86` **10.0.28000.2526** ([nuget.org](https://www.nuget.org/packages/Microsoft.Windows.WDK.x86/10.0.28000.2526)) |
| Package SHA-256 | `3432999540db204315247f8f904feebfd4a217af5529e3beea59884478f0daef` |
| Path inside it | `c/Include/10.0.28000.0/km/dispmprt.h` |
| File SHA-256 | `cf8bc6201dace5ca00fc581219e9490442f6e050987a92712c78f74ec5101f07` |
| Size | 167,159 bytes |

The package hash is the one `docs/HELIOS_PRESENT_SYNC_RETIREMENT.md` §1 pins,
and the file was verified byte-identical to the copy inside the kit **installed**
on the build VM at `C:\Program Files (x86)\Windows Kits\10\Include\10.0.28000.0\`.

Re-extract with:

```bash
curl -sSL -o /tmp/wdk28000.nupkg \
  https://www.nuget.org/api/v2/package/Microsoft.Windows.WDK.x86/10.0.28000.2526
sha256sum /tmp/wdk28000.nupkg     # must match the package hash above
bsdtar -xOf /tmp/wdk28000.nupkg 'c/Include/10.0.28000.0/km/dispmprt.h' \
  > kmd_render/tools/wdk-28000/km/dispmprt.h
```

## Scope of what is vendored

⛔ **Only this one header, and only because a Linux-side gate consumes it.**
Everything else the build needs comes from the kit **installed** on the VM
(`TOOLCHAIN.md` §2.1) — the D3D12 DDI bindings, the platform headers and the
libs are not vendored and must not be. The generator parses this file textually
and resolves no `#include`, which is why one file is sufficient.

Microsoft header, redistributed under the terms that accompany the package.
