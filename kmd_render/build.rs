//! Build script for the Helios KMD.
//!
//! Two jobs:
//!   1. Generate Rust bindings for the WDDM display-miniport DDIs. `wdk-sys`
//!      only generates a fixed set of `ApiSubset`s (Base/Wdf/Gpio/Hid/...) and
//!      has NO display subset and no "add a header" API, so the entire
//!      `dispmprt.h` / `d3dkmddi.h` surface (DRIVER_INITIALIZATION_DATA,
//!      DXGKRNL_INTERFACE, every DXGKARG_*, DxgkInitialize, ...) is missing.
//!      We run our own bindgen over those headers, reusing the clang args /
//!      include paths that `wdk-build` computes via `Builder::wdk_default`.
//!   2. Configure downstream linking against the WDK (`configure_binary_build`).
//!
//! The generated file lands at `$OUT_DIR/dxgk_bindings.rs` and is pulled in by
//! `src/dxgk.rs`.
//!
//! NOTE (first-build tuning): bindgen pulls in every base type the display DDIs
//! transitively reference (DEVICE_OBJECT, LARGE_INTEGER, ...). We blocklist the
//! common ones and redirect them to `wdk_sys` via a `use` prelude so there is a
//! single canonical definition. The blocklist below is a starting set; the first
//! real build against the installed WDK will surface any remaining duplicate-
//! definition conflicts, which get added here. This is expected for layered
//! bindgen and is not a design problem.

use wdk_build::{BuilderExt, Config};

/// Headers that declare the WDDM render-path DDIs we implement.
///
/// `DXGKDDI_INTERFACE_VERSION` is left at the header default (WDDM 3.2 on the
/// 26100 WDK) so every generated struct matches the buffers 24H2 dxgkrnl hands a
/// native driver. We declare the same version in `DriverEntry` (consistent → no
/// BUFFER_TOO_SMALL). An earlier experiment pinned this to WDDM 2.0 (0x5023) to
/// shrink the cap surface, but 24H2 then rejects the UMD revision
/// (STATUS_REVISION_MISMATCH) — a 2.0 adapter is too old for the OS's user-mode
/// driver, so we stay OS-native.
const DXGK_HEADER_CONTENTS: &str = r#"
#include <ntddk.h>
#include <dispmprt.h>
#include <d3dkmddi.h>
"#;

/// Base types that `wdk-sys` already defines. We blocklist them here and import
/// `wdk_sys::*` so DDI code uses one canonical set instead of two incompatible
/// copies. Extend this list as the first build reports conflicts.
const BLOCKLISTED_BASE_TYPES: &[&str] = &[
    "_?DEVICE_OBJECT",
    "_?DRIVER_OBJECT",
    "_?UNICODE_STRING",
    "_?LARGE_INTEGER",
    "_?ULARGE_INTEGER",
    "_?LIST_ENTRY",
    "_?SINGLE_LIST_ENTRY",
    "_?PHYSICAL_ADDRESS",
    "_?GUID",
    "_?KEVENT",
    "_?DISPATCHER_HEADER",
    "_?IRP",
    "_?IO_STACK_LOCATION",
    "_?KDPC",
    "_?KSPIN_LOCK",
    "NTSTATUS",
];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    verify_slot_audit_not_stale();
    generate_dxgk_bindings()?;
    compile_version_resource()?;
    compile_seh_shim();

    // Emit the link configuration for a WDK binary (resolves ntoskrnl, etc.).
    Config::from_env_auto()?.configure_binary_build()?;

    // displib.lib provides DxgkInitialize — the WDDM display-miniport entry that
    // registers our DDI table. wdk-build links the base kernel libs (ntoskrnl,
    // hal, wmilib, ...) but not the display-miniport import lib, so add it here.
    // Its directory is already on the linker search path (km\<ver>\x64).
    println!("cargo:rustc-link-lib=static=displib");
    Ok(())
}

/// Fail the build if `src/ddi/wddm32_slot_audit.rs` has drifted from the
/// generator and classification table that produce it.
///
/// # Why this exists
///
/// The audit file's own header says "GENERATED — do not edit by hand" and names
/// the command to regenerate it. On 2026-08-10 that command **reverted the fix
/// that made the audit armable**: the `Retiring` class lived only in the
/// hand-edited `.rs`, so regenerating reclassified eight live slots to
/// `Disabled` and `verify()` would then have refused to load a correct driver
/// at `DriverEntry` with `0xC0000182`. A generated file that its generator
/// cannot reproduce is a trap aimed at whoever follows its instructions.
///
/// # Where `dispmprt.h` comes from, in order
///
/// ⭐ **Changed 2026-08-10 (owner decision): Windows Kit 10.0.28000.0 is now
/// INSTALLED on the build VM**, so the first choice is the installed kit and the
/// gitignored `tmp/wdk-28000/` tree is only a fallback. Round 3 of the Phase-2
/// review recorded why that ordering matters: while the staged tree was the
/// *only* source, an untracked directory silently decided whether this check ran
/// at all, and the same tree was a hard `require_path` in `umd12/build.rs` — so
/// `helios_umd12.dll` could be built on exactly one computer. The install is
/// four NuGet packages merged into the kit root; `TOOLCHAIN.md` has the recipe
/// and the pinned hashes.
///
/// 1. `HELIOS_WDK_KM_INCLUDE` — an explicit override, for a host that keeps its
///    kit somewhere else.
/// 2. the installed kit's `km\dispmprt.h`.
/// 3. `kmd_render/tools/wdk-28000/km/dispmprt.h` — the **vendored** copy, which
///    is in the repository and therefore always present. It is what
///    `tools/retirement-gates.sh` uses, because that gate runs on the **Linux**
///    host where no Windows kit is installed, and its provenance (package, both
///    SHA-256s, re-extraction command) is in that directory's README.
///
/// All three are the same package: the header was verified byte-identical
/// (SHA-256 `cf8bc620…`) across the installed kit, the previously-staged tree
/// and the vendored copy.
///
/// ⇒ Because (3) is tracked, the "no header" arm below is now **unreachable in a
/// normal checkout**. It is kept because a build from a partial export or a
/// stripped source drop is not this repository's business to prevent, and a
/// build script that panics on a missing *advisory* input would be exactly the
/// "worse failure than the one being prevented" this doc warns about.
///
/// # Why it is advisory when its inputs are absent
///
/// The check needs `python3` and one of the headers above. Hard-failing without
/// them would break the build on any machine that has neither, which is a worse
/// failure than the one being prevented. So: **present ⇒ enforced as a build
/// error; absent ⇒ a loud `cargo:warning`**. It is never silent, because a check
/// that can pass by being skipped is the kind of assurance this tree keeps
/// discovering is not real.
///
/// ⚠ On the win11 VM the check is advisory *in practice* for a second reason
/// that has nothing to do with headers: there is no working `python3` there —
/// Windows' App Execution Alias stub launches, prints "Python was not found",
/// and exits non-zero, which is what the marker protocol below exists to tell
/// apart from a real staleness verdict. The enforcing run is
/// `tools/retirement-gates.sh` on Linux.
fn verify_slot_audit_not_stale() {
    use std::path::Path;
    use std::path::PathBuf;
    use std::process::Command;

    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("kmd_render always has a parent directory")
        .to_path_buf();
    let generator = repo.join("kmd_render/tools/gen_wddm32_slot_audit.py");

    println!("cargo:rerun-if-changed=tools/gen_wddm32_slot_audit.py");
    println!("cargo:rerun-if-changed=tools/wddm32_slot_classes.tsv");
    println!("cargo:rerun-if-changed=src/ddi/wddm32_slot_audit.rs");
    println!("cargo:rerun-if-env-changed=HELIOS_WDK_KM_INCLUDE");

    let header = [
        std::env::var("HELIOS_WDK_KM_INCLUDE")
            .ok()
            .map(|d| PathBuf::from(d).join("dispmprt.h")),
        Some(PathBuf::from(
            r"C:\Program Files (x86)\Windows Kits\10\Include\10.0.28000.0\km\dispmprt.h",
        )),
        Some(repo.join("kmd_render/tools/wdk-28000/km/dispmprt.h")),
    ]
    .into_iter()
    .flatten()
    .find(|p| p.exists());

    let Some(header) = header else {
        println!(
            "cargo:warning=slot-audit staleness check SKIPPED (no WDK 28000 km/dispmprt.h: \
             tried HELIOS_WDK_KM_INCLUDE, the installed kit 10.0.28000.0, and the \
             vendored kmd_render/tools/wdk-28000/km/dispmprt.h -- the last is tracked, \
             so this means an incomplete checkout). Run tools/retirement-gates.sh."
        );
        return;
    };
    if !generator.exists() {
        println!(
            "cargo:warning=slot-audit staleness check SKIPPED (generator absent). \
             Run tools/retirement-gates.sh on the Linux host."
        );
        return;
    }

    // ⛔ Decide on the generator's OWN words, never on a bare exit status.
    //
    // The first version of this check treated "ran, exit != 0" as STALE, and it
    // broke the VM build on 2026-08-10 without anything being stale. Windows
    // ships an **App Execution Alias** stub named `python3.exe`: it launches
    // fine (so `Err(e)` never fires), prints "Python was not found; run without
    // arguments to install from the Microsoft Store", and exits non-zero. That
    // is "could not run the checker", which is the opposite of "the checker
    // says no" — and inferring one from the other blocks work for the wrong
    // reason, which is the mirror of a check that passes by being skipped.
    //
    // So require a POSITIVE marker from the generator in each direction, and
    // treat everything else as could-not-run.
    let (marker_ok, marker_stale) = ("up to date", "STALE:");
    // ⛔ `--header` is PASSED, not merely resolved. Before 2026-08-10 the
    // resolution above was an existence check and nothing more: the generator
    // fell back to its own hardcoded `tmp/wdk-28000/...` default, so this build
    // script could confirm one header exists and then check against a different
    // one. With kit 28000 installed the two are the same package (verified
    // byte-identical by SHA-256 at install time), but "the same today" is not a
    // property to build a checker on — that is exactly the class of assurance
    // this function's own doc comment warns about.
    match Command::new("python3")
        .arg(&generator)
        .arg("--check")
        .arg("--header")
        .arg(&header)
        .output()
    {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            if stderr.contains(marker_stale) {
                panic!(
                    "kmd_render/src/ddi/wddm32_slot_audit.rs is STALE with respect to \
                     gen_wddm32_slot_audit.py + wddm32_slot_classes.tsv.\n\
                     Regenerate it (python3 kmd_render/tools/gen_wddm32_slot_audit.py) \
                     and review the diff — a class that exists only in the .rs will be \
                     silently reverted, and the audit then refuses to load the driver.\n\
                     --- generator output ---\n{stdout}{stderr}"
                );
            }
            if !stdout.contains(marker_ok) {
                println!(
                    "cargo:warning=slot-audit staleness check SKIPPED: python3 produced \
                     neither marker (no working python3 on this machine?). Run \
                     tools/retirement-gates.sh on the Linux host."
                );
            }
        }
        Err(e) => println!(
            "cargo:warning=slot-audit staleness check SKIPPED (could not run python3: {e})"
        ),
    }
}

/// Compile the SEH shim for `MmMapLockedPagesSpecifyCache(UserMode)` (which
/// raises on failure — un-catchable from no_std Rust). Kernel-appropriate
/// flags: `/Zl` omits default-CRT lib records from the object (rustc drives
/// the kernel link; msvcrt must not be pulled in) and `/GS-` avoids
/// `__security_cookie` references. The `__C_specific_handler` reference the
/// `__try/__except` emits resolves from ntoskrnl.lib, already on the link.
fn compile_seh_shim() {
    cc::Build::new()
        .file("src/seh_shim.c")
        .flag("/Zl")
        .flag("/GS-")
        .compile("helios_seh_shim");
    println!("cargo:rerun-if-changed=src/seh_shim.c");
}

/// Single source of truth for the driver version, relative to the package root
/// (cargo runs a build script with the manifest directory as its cwd).
///
/// `Cargo.make.toml` reads the same file through cargo-make's **top-level**
/// `env_files` for the stampinf `-v` argument (see [`verify_version_wiring`]),
/// and `bump_kmd_version_at` in `tools/win-mcp/src/main.rs` reads and rewrites
/// it when bumping. There is no other version literal: the INF `DriverVer` and
/// the image `FILEVERSION` are rendered from this one value, because a mismatch
/// between them is `FAILED_ADD 0xc0000182` at install — visible only after a
/// reboot.
const DRIVER_VERSION_FILE: &str = "driver-version.env";

/// Parse `HELIOS_KMD_VERSION=a.b.c.d` out of [`DRIVER_VERSION_FILE`].
///
/// Every failure returns `Err`, which `main` propagates as a build failure with
/// a named cause. The alternative — defaulting, or emitting a resource from a
/// partially parsed version — is exactly the silent incoherence this file exists
/// to remove.
fn read_driver_version() -> Result<[u32; 4], Box<dyn std::error::Error>> {
    let text = std::fs::read_to_string(DRIVER_VERSION_FILE)
        .map_err(|e| format!("read {DRIVER_VERSION_FILE}: {e}"))?;
    let raw = text
        .lines()
        .map(str::trim)
        .find_map(|l| l.strip_prefix("HELIOS_KMD_VERSION="))
        .ok_or_else(|| format!("{DRIVER_VERSION_FILE}: no HELIOS_KMD_VERSION= line"))?
        .trim();

    let parts: Vec<&str> = raw.split('.').collect();
    if parts.len() != 4 {
        return Err(format!(
            "{DRIVER_VERSION_FILE}: HELIOS_KMD_VERSION={raw:?} has {} components, expected 4 (a.b.c.d)",
            parts.len()
        )
        .into());
    }
    let mut version = [0u32; 4];
    for (i, part) in parts.iter().enumerate() {
        version[i] = part.parse::<u32>().map_err(|e| {
            format!(
                "{DRIVER_VERSION_FILE}: HELIOS_KMD_VERSION={raw:?} component {i} ({part:?}): {e}"
            )
        })?;
    }
    Ok(version)
}

/// The cargo-make makefile, checked by [`verify_version_wiring`].
const DRIVER_MAKEFILE: &str = "Cargo.make.toml";

/// Fail the build if the INF's version stopped coming from [`DRIVER_VERSION_FILE`].
///
/// Inside this script the divergence class is already gone — one parse renders
/// both the comma and the dotted forms. The remaining risk is the *other*
/// consumer: `Cargo.make.toml`'s stampinf `-v`, which stamps the INF `DriverVer`,
/// and whose disagreement with the image `FILEVERSION` is `FAILED_ADD
/// 0xc0000182` at install — after a reboot.
///
/// This check deliberately lives here rather than in the `win_build_kmd` MCP
/// tool. A hand-run `cargo make --makefile Cargo.make.toml` is a documented
/// build path (TOOLCHAIN.md, BRINGUP_QUIRKS.md), and a gate that only runs
/// inside a compiled MCP server does not cover it. Building must not depend on
/// win-mcp.
fn verify_version_wiring() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed={DRIVER_MAKEFILE}");
    let make = std::fs::read_to_string(DRIVER_MAKEFILE)
        .map_err(|e| format!("read {DRIVER_MAKEFILE}: {e}"))?;

    // `env_files` is a TOP-LEVEL cargo-make key. Inside `[config]` it is silently
    // ignored, and stampinf then receives the literal "${HELIOS_KMD_VERSION}".
    // "Before the first table header" is what top-level means in TOML.
    let top_level = make
        .lines()
        .map(str::trim)
        .take_while(|l| !l.starts_with('['))
        .any(|l| l.starts_with("env_files") && l.contains(DRIVER_VERSION_FILE));
    if !top_level {
        return Err(format!(
            "{DRIVER_MAKEFILE}: no top-level `env_files` entry naming {DRIVER_VERSION_FILE}. \
             cargo-make ignores `env_files` inside [config], so stampinf would receive the \
             literal placeholder and the INF DriverVer would not match the image FILEVERSION \
             (FAILED_ADD 0xc0000182)."
        )
        .into());
    }

    if !make.contains(r#""-v", "${HELIOS_KMD_VERSION}""#) {
        return Err(format!(
            "{DRIVER_MAKEFILE}: the stampinf `-v` argument is not \"${{HELIOS_KMD_VERSION}}\". \
             The INF DriverVer must be rendered from {DRIVER_VERSION_FILE}, the same single \
             source this script renders FILEVERSION from — a literal there is \
             FAILED_ADD 0xc0000182 at install."
        )
        .into());
    }
    Ok(())
}

fn compile_version_resource() -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = std::path::PathBuf::from(std::env::var("OUT_DIR")?);
    let rc_path = out_dir.join("helios_kmd_render_version.rc");
    let res_path = out_dir.join("helios_kmd_render_version.res");

    // One parse renders both forms, so the numerics and the strings cannot
    // disagree with each other or with the stamped INF.
    let v = read_driver_version()?;
    verify_version_wiring()?;
    let comma = format!("{},{},{},{}", v[0], v[1], v[2], v[3]);
    let dotted = format!("{}.{}.{}.{}", v[0], v[1], v[2], v[3]);

    std::fs::write(
        &rc_path,
        format!(
            r#"1 VERSIONINFO
FILEVERSION {comma}
PRODUCTVERSION {comma}
FILEFLAGSMASK 0x3fL
FILEFLAGS 0
FILEOS 0x00040004L
FILETYPE 0x00000003L
FILESUBTYPE 0x00000004L
BEGIN
    BLOCK "StringFileInfo"
    BEGIN
        BLOCK "040904b0"
        BEGIN
            VALUE "CompanyName", "Helios Project\0"
            VALUE "FileDescription", "Helios vGPU WDDM render miniport\0"
            VALUE "FileVersion", "{dotted}\0"
            VALUE "InternalName", "helios_kmd_render.sys\0"
            VALUE "OriginalFilename", "helios_kmd_render.sys\0"
            VALUE "ProductName", "Helios vGPU\0"
            VALUE "ProductVersion", "{dotted}\0"
        END
    END
    BLOCK "VarFileInfo"
    BEGIN
        VALUE "Translation", 0x0409, 1200
    END
END
"#
        ),
    )?;

    let rc = find_windows_sdk_tool("rc.exe");
    let status = std::process::Command::new(&rc)
        .arg("/nologo")
        .arg(format!("/fo{}", res_path.display()))
        .arg(&rc_path)
        .status()?;
    if !status.success() {
        return Err(format!("{} failed with {status}", rc.display()).into());
    }

    println!("cargo:rerun-if-changed=build.rs");
    // Without this, editing the version alone would not regenerate the resource:
    // `rerun-if-changed=build.rs` only covers the script itself.
    println!("cargo:rerun-if-changed={DRIVER_VERSION_FILE}");
    println!("cargo:rustc-link-arg={}", res_path.display());
    Ok(())
}

fn find_windows_sdk_tool(name: &str) -> std::path::PathBuf {
    if let (Ok(sdk_dir), Ok(sdk_ver)) = (
        std::env::var("WindowsSdkDir"),
        std::env::var("WindowsSDKVersion"),
    ) {
        let candidate = std::path::Path::new(&sdk_dir)
            .join("bin")
            .join(sdk_ver.trim_end_matches('\\'))
            .join("x64")
            .join(name);
        if candidate.exists() {
            return candidate;
        }
    }

    let candidate =
        std::path::Path::new(r"C:\Program Files (x86)\Windows Kits\10\bin\10.0.26100.0\x64")
            .join(name);
    if candidate.exists() {
        return candidate;
    }

    name.into()
}

fn generate_dxgk_bindings() -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = std::env::var("OUT_DIR")?;
    let out_path = std::path::Path::new(&out_dir).join("dxgk_bindings.rs");

    // `wdk_default` seeds the builder with the correct target triple, kernel-mode
    // defines (_KERNEL_MODE, _AMD64_, ...) and WDK/SDK include paths.
    let mut builder = bindgen::Builder::wdk_default(Config::from_env_auto()?)?
        .header_contents("helios-dxgk-input.h", DXGK_HEADER_CONTENTS)
        // Generate only the display surface; base types come from wdk_sys.
        .allowlist_type("DXGK.*")
        .allowlist_type("_?DRIVER_INITIALIZATION_DATA")
        .allowlist_type("D3DKMT_.*")
        .allowlist_type("D3DDDI_.*")
        .allowlist_type("DXGK_.*")
        .allowlist_function("Dxgk.*")
        .allowlist_var("DXGK.*")
        .allowlist_var("D3DKMDT_.*")
        .allowlist_var("KMT_.*")
        // Re-export the base types from wdk_sys so blocklisted references resolve.
        // (The allow(...) lints are applied as an outer attribute on the `bindings`
        // module in src/dxgk.rs — an inner attribute here is illegal under include!.)
        .raw_line("pub use wdk_sys::*;");

    for ty in BLOCKLISTED_BASE_TYPES {
        builder = builder.blocklist_type(ty);
    }

    builder.generate()?.write_to_file(&out_path)?;

    Ok(())
}
