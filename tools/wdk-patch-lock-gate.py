#!/usr/bin/env python3
"""Prove the windows-drivers-rs patch and lock are target-independent."""

from __future__ import annotations

import os
import sys
import tomllib
from dataclasses import dataclass


REPO_DEFAULT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
MANIFEST = "kmd_render/Cargo.toml"
LOCK = "kmd_render/Cargo.lock"
KMD_CHECK = "tools/kmd-check.ps1"
RETIREMENT = "tools/retirement-gates.sh"
REVISION = "6b040c03e56fbbebc10ed86136839774a82c3b59"
CRATES = ("wdk", "wdk-alloc", "wdk-build", "wdk-macros", "wdk-panic", "wdk-sys")
SOURCES = (MANIFEST, LOCK, KMD_CHECK, RETIREMENT)


def load_sources(repo: str) -> dict[str, str]:
    out: dict[str, str] = {}
    for path in SOURCES:
        with open(os.path.join(repo, path), encoding="utf-8", errors="replace") as stream:
            out[path] = stream.read()
    return out


def check_sources(s: dict[str, str]) -> list[str]:
    errors: list[str] = []
    try:
        manifest = tomllib.loads(s[MANIFEST])
        lock = tomllib.loads(s[LOCK])
    except tomllib.TOMLDecodeError as error:
        return [f"Cargo TOML does not parse: {error}"]

    patch = manifest.get("patch", {}).get("crates-io", {})
    if set(patch) != set(CRATES):
        errors.append(f"{MANIFEST}: unconditional crates.io patch set drifted: {sorted(patch)!r}")
    for crate in CRATES:
        spec = patch.get(crate, {})
        if spec.get("git") != "https://github.com/microsoft/windows-drivers-rs":
            errors.append(f"{MANIFEST}: {crate} patch is not the reviewed upstream repository")
        if spec.get("rev") != REVISION:
            errors.append(f"{MANIFEST}: {crate} patch is not pinned to {REVISION}")
    for target_name, target in manifest.get("target", {}).items():
        if "patch" in target:
            errors.append(f"{MANIFEST}: patch became target-conditional under {target_name}")

    packages = lock.get("package", [])
    for crate in CRATES:
        matches = [package for package in packages if package.get("name") == crate]
        if len(matches) != 1:
            errors.append(f"{LOCK}: expected one resolved {crate}, found {len(matches)}")
            continue
        source = matches[0].get("source", "")
        expected = (
            "git+https://github.com/microsoft/windows-drivers-rs"
            f"?rev={REVISION}#{REVISION}"
        )
        if source != expected:
            errors.append(f"{LOCK}: {crate} did not resolve through the unconditional patch")
        if "checksum" in matches[0]:
            errors.append(f"{LOCK}: patched git package {crate} retained a registry checksum")

    if "'--locked'" not in s[KMD_CHECK]:
        errors.append(f"{KMD_CHECK}: Windows KMD validation may rewrite Cargo.lock")
    gate_line = 'python3 "$REPO/tools/wdk-patch-lock-gate.py" "$REPO" --mutations'
    if s[RETIREMENT].count(gate_line) != 1:
        errors.append(f"{RETIREMENT}: WDK patch/lock gate must be integrated exactly once")
    return errors


@dataclass(frozen=True)
class Mutation:
    name: str
    path: str
    old: str
    new: str


def mutation_cases() -> tuple[Mutation, ...]:
    return (
        Mutation("drop one family patch", MANIFEST, "wdk-panic = { git", "wdk-panic-disabled = { git"),
        Mutation("move one patch revision", MANIFEST, REVISION, "0000000000000000000000000000000000000000"),
        Mutation("restore registry resolution", LOCK, "git+https://github.com/microsoft/windows-drivers-rs", "registry+https://github.com/rust-lang/crates.io-index"),
        Mutation("unlock Windows validation", KMD_CHECK, "'--locked', ", ""),
    )


def run_mutations(sources: dict[str, str]) -> None:
    for case in mutation_cases():
        source = sources[case.path]
        if case.old not in source:
            raise SystemExit(f"WDK patch mutation setup failed for {case.name}")
        mutated = dict(sources)
        mutated[case.path] = source.replace(case.old, case.new, 1)
        if not check_sources(mutated):
            raise SystemExit(f"WDK patch mutation was accepted: {case.name}")
    print(f"OK: {len(mutation_cases())} in-memory WDK patch/lock mutations rejected")


def main() -> None:
    repo = (
        os.path.abspath(sys.argv[1])
        if len(sys.argv) > 1 and not sys.argv[1].startswith("--")
        else REPO_DEFAULT
    )
    sources = load_sources(repo)
    errors = check_sources(sources)
    if errors:
        raise SystemExit("WDK patch/lock gate violated:\n" + "\n".join(errors))
    if "--mutations" in sys.argv[1:]:
        run_mutations(sources)
    print("OK: one unconditional windows-drivers-rs patch resolves the locked graph")


if __name__ == "__main__":
    main()
