//! Build script for the Helios D3D12 WDDM UMD.
//!
//! # Stage S4 — bindgen, the cxx bridge, and the engine link
//!
//! `ARCHITECTURE.md` §11 stages this crate. Through S3 this script did exactly
//! one thing, bindgen `d3d12umddi.h`. S4 adds the other two halves: it compiles
//! the cxx bridge (`bridge/vkd3d_bridge.cpp`) that wraps vkd3d-proton's
//! `ID3D12*` COM objects, and it links the prebuilt vkd3d engine archive into
//! `helios_umd12.dll`.
//!
//! ⛔ The link set is **measured, not guessed** (`D12-G1` static arm,
//! `tmp/dx12/gates/G1-static/RESULT.md:27-42`):
//!
//! ```text
//! C:\Users\Rupansh\vkd3d-build\libs\d3d12core\libhelios_d3d12_static.a
//! cargo:rustc-link-lib=dylib=gdi32
//! ```
//!
//! **One archive** — it is a union carrying every vkd3d / dxil-spirv /
//! dxbc-spirv object — plus `gdi32` for the `__imp_D3DKMT*` that
//! `libs/vkd3d/d3dkmt.c` imports and `vkd3d_dep` does not carry. That gate's
//! first attempt linked the archive *alone*, as `DECISIONS.md` D4 specified, and
//! got 19 unresolved externals; the set below is what actually resolved.
//! ⛔ **Never `dxgi`**, and nothing else either — not `advapi32`, not `ole32`,
//! not `user32`, not `version`. `umd/build.rs:247-250` states the DXGI rule for
//! D3D11 and the static engine is the first artifact that keeps it with zero
//! `dxgi` references at all.
//!
//! # Toolchain coherence (critical)
//!
//! vkd3d, the cxx shim, and the Rust crate must all use the MSVC C++ ABI with
//! the **dynamic** CRT (`/MD`). vkd3d is built with clang-cl under meson
//! (`vkd3d-proton-helios/meson.build:9` recognises `clang-cl` and pins
//! `cpp_std=c++17`, the same standard this shim compiles with); we compile the
//! shim with the *same* clang-cl so the objects link against one another and
//! against the Rust msvc target. `HELIOS_CLANG_CL` / `HELIOS_MSVC_LIB` /
//! `HELIOS_VKD3D_BUILD` override the baked-in defaults.
//!
//! # The bindgen deliverable
//!
//! The layout assertions. `layout_tests(true)` makes bindgen emit a
//! compile-time size/alignment/offset check per type, so **if this crate
//! compiles, the D3D12 DDI ABI is machine-checked against the SDK header**.
//! That is the whole point of the stage: `ARCHITECTURE.md` §12 rule 1 —
//! *never hand-transcribe a DDI ABI struct* — and R908 is what ignoring it
//! cost.

use std::env;
use std::path::{Path, PathBuf};

fn def(var: &str, default: &str) -> String {
    env::var(var).unwrap_or_else(|_| default.to_string())
}

/// Fail the build at the point the path is chosen, naming the env var that
/// overrides it.
///
/// Same shape as `umd/build.rs:63-70`, and the same reasoning: every default
/// here is an absolute path baked into this script. Without the check a wrong
/// path surfaces far from its cause — as a clang include error, a
/// missing-archive link error, or a "program not found" from `cc` — and none of
/// those name the variable that would fix it.
///
/// ⚠ This is the one sanctioned `panic!` in the crate (CLAUDE.md's no-panic rule
/// is about runtime data; a build script panicking on a missing toolchain path
/// is the `require_path` idiom and is correct).
fn require_path(env_var: &str, value: &str, dir: bool) {
    let path = Path::new(value);
    let ok = if dir { path.is_dir() } else { path.is_file() };
    if !ok {
        let kind = if dir { "directory" } else { "file" };
        panic!("helios_umd12: {env_var} {kind} not found: {value} (override with {env_var})");
    }
}

/// Pick the highest-versioned MSVC include directory (for the vcruntime/STL
/// headers the SDK headers transitively pull in).
///
/// NOTE: the sort is lexicographic over directory names, not semantic, so a
/// hypothetical `14.9.x` would outrank `14.44.x`. Only one toolset is installed
/// today; set `HELIOS_MSVC_INCLUDE` if that ever stops being true. Same
/// function, same caveat, as `umd/build.rs` — deliberately not shared, because
/// `umd_common` has no `build.rs` and must not acquire one (`DECISIONS.md` D3b:
/// a build script there would drag the WDK into a crate that must also build on
/// Linux).
fn find_msvc_include() -> String {
    if let Ok(v) = env::var("HELIOS_MSVC_INCLUDE") {
        return v;
    }
    let root = Path::new(r"C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Tools\MSVC");
    let mut versions: Vec<PathBuf> = std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join("include").is_dir())
        .collect();
    versions.sort();
    versions
        .last()
        .map(|p| p.join("include").to_string_lossy().into_owned())
        .unwrap_or_else(|| {
            panic!(
                "no MSVC toolset with an include/ directory under {}; set HELIOS_MSVC_INCLUDE \
                 to the vcruntime/STL include directory",
                root.display()
            )
        })
}

/// The **DDI** include root — where `d3d12umddi.h` and its WDK siblings come
/// from. Default: the **installed** Windows Kit 10.0.28000.0.
///
/// ⛔ **28000, not 26100, and that is the whole point of the retirement's U0.**
/// `HELIOS_PRESENT_SYNC_RETIREMENT.md` §10.2 requires a negotiated
/// `D3D12DDI_SUPPORTED_0116`, and §1 records why 26100 cannot serve it: its
/// `d3d12umddi.h` *"ends at Core build 0110"*. There is no
/// `D3D12DDI_DEVICE_FUNCS_CORE_0116`, no `PFND3D12DDI_CREATEFENCE_0116`, no
/// `pfnOpenNativeFenceCb` and no `D3D12DDICAPS_TYPE_0112_NATIVE_FENCE_SUPPORT`
/// in it at all.
///
/// ⭐ **CHANGED 2026-08-10 (owner decision): kit 28000 is INSTALLED on the VM,
/// and this no longer points into a gitignored tree in the repo.** It used to be
/// `C:\Users\Rupansh\helios-vgpu\tmp\wdk-28000\...`, staged by hand from a
/// NuGet package and carried into the build mirror by `win_cargo`'s robocopy.
/// Round 3 of the Phase-2 review made the cost of that concrete: the tree is
/// `.gitignore`d and untracked, no CI job or installer provisions it, and
/// `require_path` + `require_core_0116` **panic** without it — so
/// `helios_umd12.dll` could be built on exactly one computer, and the same tree
/// silently decided whether `tools/retirement-gates.sh` ran its slot-audit gate
/// or skipped it.
///
/// What is installed and how to reproduce it is `TOOLCHAIN.md`; the short
/// version is four NuGet packages at **10.0.28000.2526** — `Microsoft.Windows.
/// {WDK.x86, WDK.x64, SDK.CPP, SDK.CPP.x64}` — merged into the kit root, which
/// yields a kit whose `Include`/`Lib` subdirectory shape is identical to the
/// 26100 kit beside it. That completeness is the point: `TOOLCHAIN.md`'s
/// standing *"keep only complete kits"* rule exists because a partial kit can be
/// selected by MSBuild and then fail on a missing lib.
const WDK_DDI_INCLUDE_DEFAULT: &str =
    r"C:\Program Files (x86)\Windows Kits\10\Include\10.0.28000.0";

/// The **platform** include root — `windows.h`, the CRT, and every SDK header
/// the DDI graph reaches through.
///
/// ⭐ **Now the SAME kit as [`WDK_DDI_INCLUDE_DEFAULT`], and the split that used
/// to exist here is retired with its reason recorded.** While the DDI headers
/// came from a hand-staged *WDK* package this root had to be the installed
/// *SDK*, because that package shipped 11 `shared/` and 27 `um/` headers and so
/// could not resolve `d3dkmdt.h`, `d3dukmdt.h`, `d3dkmthk.h`, `dxmini.h`,
/// `dxgiddi.h`, `winapifamily.h` or `windows.h`. Worse, putting its `shared/`
/// on the include path ahead of the SDK's produced measured clang errors —
/// `d3dkmddi.h` referencing `D3DDDI_CREATEHWQUEUEFORUSERMODESUBMISSION_FLAGS`
/// and `D3DDDI_UMS_PDD_SIZE`, which nothing in that package declared.
///
/// The installed kit is complete, so both halves of that are gone. Measured on
/// the VM, 2026-08-10, after the install:
///
/// ```text
/// 28000 kit   um=1541 shared=255 km=224      (hand-staged tree: um=27 shared=11)
/// D3DDDI_CREATEHWQUEUEFORUSERMODESUBMISSION_FLAGS -> d3dkmddi.h, d3dkmthk.h, d3dukmdt.h
/// D3DDDI_UMS_PDD_SIZE                             -> d3dkmddi.h, d3dkmthk.h, d3dukmdt.h
/// ```
///
/// Both identifiers are now **declared**, by `shared\d3dukmdt.h`, and every
/// header this crate reaches lives in the same subdirectory it lives in under
/// 26100. ⇒ One kit, one include order, no mix.
///
/// ⚠ The two constants stay **separate** on purpose. They are the shape a future
/// split would need, `HELIOS_WDK_INCLUDE` / `HELIOS_SDK_INCLUDE` still override
/// them independently, and `require_core_0116` still fails the build if the DDI
/// root turns out not to be a 0116 one — so pointing this at 26100 by accident
/// is still caught rather than silently generating a 0110 binding.
const SDK_PLATFORM_INCLUDE_DEFAULT: &str =
    r"C:\Program Files (x86)\Windows Kits\10\Include\10.0.28000.0";

/// Symbols the generated bindings MUST contain for this crate to be able to
/// serve Core 0116 — `HELIOS_PRESENT_SYNC_RETIREMENT.md` §18.1's first build
/// gate, executed instead of trusted.
///
/// ⛔ **This exists because the failure it catches is SILENT.** Point
/// `HELIOS_WDK_INCLUDE` at the installed 26100 SDK — by typo, by a stale
/// environment, or by the staged tree not having reached the mirror — and
/// bindgen succeeds, emits a perfectly good 26100 generation, and the only
/// signal is `compare_or_refresh_cache`'s *warning*, which does not fail a
/// build. The crate then compiles against a cache that cannot name a single
/// 0116 type, and the first evidence is a runtime version mismatch on the
/// guest. §18.1 asks for *"WDK 28000 bindings expose Core 0116 exactly"*; this
/// is that sentence as a build step.
///
/// ⚠ Presence only. That the sizes and offsets match the released header is the
/// job of bindgen's own `layout_tests(true)` assertions, which are compile-time
/// `const` evaluations in the generated file itself, plus the per-arm table-size
/// assertions in `adapter12.rs`.
const CORE_0116_REQUIRED_SYMBOLS: &[&str] = &[
    // The version token halves `adapter12::ddi12_supported` composes from.
    "D3D12DDI_BUILD_VERSION_0116",
    // The three table shapes a 0116 negotiation selects.
    "D3D12DDI_DEVICE_FUNCS_CORE_0116",
    "D3D12DDI_COMMAND_LIST_FUNCS_3D_0114",
    "D3D12DDI_COMMAND_QUEUE_FUNCS_CORE_0001",
    // The corelayer callback arm 0116 selects, and the two callbacks that are
    // the entire reason the retirement wants this build.
    "D3D12DDI_CORELAYER_DEVICECALLBACKS_0116",
    "PFND3D12DDI_CREATENATIVEFENCE_CB_0112",
    "PFND3D12DDI_OPENNATIVEFENCE_CB_0116",
    // The fence DDI: §10.6's `NATIVE` / `OPENED_NATIVE` create.
    "PFND3D12DDI_CREATEFENCE_0116",
    "PFND3D12DDI_CALCPRIVATEFENCESIZE_0116",
    "D3D12DDIARG_CREATE_FENCE_0116",
    "D3D12DDI_FENCE_TYPE_0112",
    // The admission cap of §10.2's table.
    "D3D12DDICAPS_NATIVE_FENCE_SUPPORT_DATA_0112",
    // The KMT objects the two callbacks carry, from the SDK half of the mix —
    // listed here because a wrong platform root breaks them and nothing else.
    "_D3DKMT_CREATENATIVEFENCE",
    "_D3DKMT_OPENNATIVEFENCEFROMNTHANDLE",
    "_D3DDDI_NATIVEFENCEMAPPING",
    // §10.6 step 2's queue model, and the callback the whole submit spine uses.
    "_D3DDDICB_CREATECONTEXTVIRTUAL",
    "_D3DDDICB_SUBMITCOMMAND",
    // §12.1's 64-byte HNF1 payload length, so no consumer transcribes it.
    "D3DDDI_NATIVE_FENCE_PDD_SIZE",
];

/// Fail the build if the generated bindings cannot name Core 0116.
///
/// ⚠ Fatal, unlike [`compare_or_refresh_cache`]'s warning, and the asymmetry is
/// deliberate: a stale *cache* only degrades the Linux cross-check, while a
/// generation from the wrong SDK is a shipping DLL built against an ABI the
/// package has already declared insufficient.
fn require_core_0116(generated: &str) {
    let missing: Vec<&str> = CORE_0116_REQUIRED_SYMBOLS
        .iter()
        .copied()
        .filter(|sym| !generated.contains(sym))
        .collect();
    if missing.is_empty() {
        return;
    }
    panic!(
        "helios_umd12: the generated d3d12umddi bindings do not expose Core 0116. Missing: {}. \
         The DDI headers must come from WDK 28000 (staged at tmp/wdk-28000/, mirrored to \
         {WDK_DDI_INCLUDE_DEFAULT}); WDK 26100's d3d12umddi.h ends at Core build 0110. Override \
         with HELIOS_WDK_INCLUDE (DDI root) and HELIOS_SDK_INCLUDE (platform root).",
        missing.join(", "),
    );
}

fn generate_d3d12umddi_bindings() {
    let wdk_inc = def("HELIOS_WDK_INCLUDE", WDK_DDI_INCLUDE_DEFAULT);
    let sdk_inc = def("HELIOS_SDK_INCLUDE", SDK_PLATFORM_INCLUDE_DEFAULT);
    // Named at the point the path is chosen, so a missing staged WDK says
    // "HELIOS_WDK_INCLUDE directory not found" rather than surfacing 200 lines
    // later as `'d3d12umddi.h' file not found`.
    require_path("HELIOS_WDK_INCLUDE", &format!(r"{wdk_inc}\um"), true);
    require_path("HELIOS_SDK_INCLUDE", &format!(r"{sdk_inc}\um"), true);
    require_path("HELIOS_SDK_INCLUDE", &format!(r"{sdk_inc}\ucrt"), true);
    let msvc_inc = find_msvc_include();
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());

    let bindings = bindgen::Builder::default()
        .header("bindgen/d3d12umddi_wrapper.h")
        .clang_args([
            "-target".to_string(),
            "x86_64-pc-windows-msvc".to_string(),
            // ⛔ THE 28000 DDI HEADERS FIRST. clang searches `-I` in order, so
            // this is what decides that `d3d12umddi.h` is the Core-0116 one and
            // not the installed 26100 copy of the same file name. It still
            // matters even though both roots now default to the same kit: an
            // override that points `HELIOS_SDK_INCLUDE` at 26100 must not be
            // able to win the name `d3d12umddi.h`.
            //
            // ⭐ **The `shared`-exclusion argument that used to sit here is
            // RETIRED, and it is retired by measurement, not by preference.**
            // It read: `{wdk_inc}\shared` on the path makes `d3d10umddi.h`'s
            // `#include "d3dkmddi.h"` resolve to the 28000 KMD DDI header, which
            // needs a 28000 `d3dukmdt.h` the hand-staged WDK package did not
            // ship, so clang reported `unknown type name
            // 'D3DDDI_CREATEHWQUEUEFORUSERMODESUBMISSION_FLAGS'` and `use of
            // undeclared identifier 'D3DDDI_UMS_PDD_SIZE'`.
            //
            // That was a property of the *package*, not of the headers. With kit
            // 28000 INSTALLED (owner decision, 2026-08-10 — see
            // [`WDK_DDI_INCLUDE_DEFAULT`] and `TOOLCHAIN.md`) `shared/` holds 255
            // headers instead of 11, both identifiers are declared by
            // `shared\d3dukmdt.h`, and every header this crate reaches lives in
            // the same subdirectory it lives in under 26100. So `{sdk_inc}\shared`
            // below IS the 28000 `shared`, deliberately, and `d3dkmddi.h` now
            // resolves against its own generation rather than 26100's.
            //
            // ⚠ Still not on the path: `km\`. That is `kmd_render`'s input, not
            // this crate's, and nothing in the D3D12 UMD DDI graph includes it.
            // The two 0116 KMT objects (`D3DKMT_CREATENATIVEFENCE`,
            // `D3DKMT_OPENNATIVEFENCEFROMNTHANDLE`) are forward-declared opaque
            // inside `d3d12umddi.h` itself and defined by `shared\d3dkmthk.h`.
            // `require_core_0116` asserts all four names are present, so no
            // include-path change can silently lose them.
            format!(r"-I{wdk_inc}\um"),
            // Then the platform: the CRT/STL, then the SDK's own um/shared for
            // `windows.h`, `d3dkmthk.h`, `d3dkmdt.h`, `dxmini.h`, `dxgiddi.h`.
            format!("-I{msvc_inc}"),
            format!(r"-I{sdk_inc}\um"),
            format!(r"-I{sdk_inc}\shared"),
            format!(r"-I{sdk_inc}\ucrt"),
            format!(r"-I{sdk_inc}\winrt"),
        ])
        // The DDI surface. Mirrors `umd/build.rs`'s allowlist shape, retargeted:
        //
        //  * `D3D12DDI.*`          — every DDI struct, enum and arg type, which
        //                            includes the 43-enumerator
        //                            `D3D12DDICAPS_TYPE` (`d3d12umddi.h:94-150`)
        //                            and the three `D3D12DDI_FEATURE_*_106`
        //                            values that live in the SAME enum.
        //  * `PFND3D12DDI.*`       — the ~296 named function-pointer typedefs.
        //                            ⚠ 173 of them are absent from SDK 26100
        //                            despite appearing in DirectX-Specs
        //                            (memory `dx12-specs-mined-74th`); bindgen
        //                            generates what the header HAS, which is
        //                            precisely why this is generated and not
        //                            transcribed.
        //  * `D3DDDI.*` / `D3DKMT.*` — `D3D12DDIARG_CREATEDEVICE_0109` carries a
        //                            `CONST D3DDDI_DEVICECALLBACKS*`
        //                            (`d3d12umddi.h:13623`), the same 65-entry
        //                            table the D3D11 UMD drives, so the kernel
        //                            callback types must come along.
        //  * `DXGI.*`              — the DXGI DDI types the header references.
        //                            ⚠ `D12-G5` measured that this build never
        //                            requests `D3D12DDI_TABLE_TYPE_DXGI`
        //                            (`DDI_REFERENCE.md` §2.3), so these are
        //                            expected to stay unused. Generated anyway:
        //                            an allowlist that omits a type the header
        //                            reaches through produces an opaque blob,
        //                            which is exactly the ABI hole this stage
        //                            exists to close.
        .allowlist_type("D3D12DDI.*")
        .allowlist_type("PFND3D12DDI.*")
        .allowlist_type("D3D12_.*")
        .allowlist_type("D3DDDI.*")
        .allowlist_type("D3DKMT.*")
        .allowlist_type("DXGI_?DDI.*")
        .allowlist_var("D3D12DDI_.*")
        .allowlist_var("D3D12_.*")
        // ⭐ `D3DDDI_NATIVE_FENCE_PDD_SIZE` — 64, from the SDK's `d3dukmdt.h`
        // (`10.0.26100.0\shared\d3dukmdt.h:1905`). It is the length of the
        // object-associated KMD private data both `D3DKMT_CREATENATIVEFENCE`
        // and `D3DKMT_OPENNATIVEFENCEFROMNTHANDLE` carry, and therefore the
        // exact size of the retirement's `HeliosNativeFencePddV1`
        // (`HELIOS_PRESENT_SYNC_RETIREMENT.md` §12.1's 64-byte table).
        //
        // ⛔ Allowlisted **narrowly, and only this family**, so the fence lane
        // can assert its record against the released header instead of against
        // a literal `64`. A bare `D3DDDI_.*` would pull in hundreds of
        // unrelated `#define`s; §12 rule 1's point is that the ABI number comes
        // from the header, not that every number in the header comes along.
        .allowlist_var("D3DDDI_NATIVE_FENCE_.*")
        // ⛔ THE DELIVERABLE. Compile-time size/alignment/offset assertions for
        // every generated type. bindgen 0.70 emits them as
        //   const _: () = { ["Offset of field: X::y"][offset_of!(X, y) - N]; };
        // so a mismatch is an E0080 const-evaluation failure during an ordinary
        // `cargo build`, not a `#[test]` that has to be run.
        //
        // ⚠ NEVER drop this to shrink the generated file. `UNVERIFIED-2` names
        // the cost and the ONLY sanctioned mitigation: narrow the allowlist to
        // the implemented DDI versions. The assertions are the reason the crate
        // exists at this stage.
        .layout_tests(true)
        .derive_default(true)
        .generate_comments(false)
        .generate()
        .expect("bindgen failed to generate d3d12umddi bindings");

    let generated = out.join("d3d12umddi.rs");
    bindings
        .write_to_file(&generated)
        .expect("failed to write d3d12umddi.rs");

    // ⛔ §18.1's build gate, before anything consumes the file.
    require_core_0116(&std::fs::read_to_string(&generated).unwrap_or_default());

    println!("cargo:rerun-if-changed=bindgen/d3d12umddi_wrapper.h");
    println!("cargo:rerun-if-env-changed=HELIOS_WDK_INCLUDE");
    // These bindings are generated against these include paths, so changing any
    // selection must regenerate them.
    println!("cargo:rerun-if-env-changed=HELIOS_SDK_INCLUDE");
    println!("cargo:rerun-if-env-changed=HELIOS_MSVC_INCLUDE");
}

/// The committed copy of the generated bindings, used to TYPE-CHECK on a host
/// that has no WDK.
///
/// ⭐ This is what lets `cargo check --target x86_64-pc-windows-msvc` run on the
/// **Linux host**, which is the difference between eleven agents writing 214 DDI
/// handlers blind and eleven agents writing them against the real signatures
/// with the compiler answering (`PARALLEL.md` §7).
///
/// ⛔ **It is never used to build a shipping DLL.** On Windows the bindings are
/// regenerated from `d3d12umddi.h` every time and this file is only *compared*
/// against, so a stale cache is loud rather than silent. The SDK header stays
/// the single source of truth.
const CACHED_BINDINGS: &str = "bindgen/cached/d3d12umddi.rs";

/// Refresh the cache from a freshly generated file, and say so.
fn compare_or_refresh_cache(fresh: &Path) {
    let cached = Path::new(CACHED_BINDINGS);
    let fresh_text = std::fs::read_to_string(fresh).unwrap_or_default();
    let cached_text = std::fs::read_to_string(cached).unwrap_or_default();
    if fresh_text == cached_text {
        return;
    }
    // ⚠ Loud, not fatal: a WDK/SDK update legitimately changes the output, and
    // failing the Windows build would block the very machine that can fix it.
    // But it MUST be noticed, because until the cache is refreshed every
    // host-side `cargo check` is type-checking against a different ABI than the
    // one being shipped.
    println!(
        "cargo:warning=helios_umd12: {CACHED_BINDINGS} is STALE ({} bytes cached vs {} generated). \
         Host-side cross-checks are now against a different ABI than this build. Refresh it: \
         copy $OUT_DIR/d3d12umddi.rs over it and commit.",
        cached_text.len(),
        fresh_text.len()
    );
}

/// Compile the cxx bridge with clang-cl and link the measured engine set.
///
/// ⛔ **Only ever called when the BUILD HOST is Windows.** Everything in here
/// wants clang-cl, `llvm-lib` and the meson output tree; none of it exists on
/// the Linux host, and the host cross-check must return before reaching it.
/// See `main`.
fn build_vkd3d_bridge() {
    let clang_cl = def("HELIOS_CLANG_CL", r"C:\Program Files\LLVM\bin\clang-cl.exe");
    let archiver = def("HELIOS_MSVC_LIB", r"C:\Program Files\LLVM\bin\llvm-lib.exe");
    let vkd3d_build = def("HELIOS_VKD3D_BUILD", r"C:\Users\Rupansh\vkd3d-build");
    let mesa_build = def("HELIOS_MESA_BUILD", r"C:\Users\Rupansh\helios-mesa-build");

    // The module doc above calls the C++ ABI / CRT agreement critical, and a
    // build that declares no dependency on the compiler that decides it is
    // guarding heap corruption with prose. `cc` and `cxx-build` do not add a
    // rerun edge for a compiler supplied via `.compiler()`, so swapping
    // HELIOS_CLANG_CL or HELIOS_MSVC_LIB would leave the previously built
    // `helios_vkd3d_bridge.lib` — compiled against the previous MSVC STL — to be
    // relinked against a freshly built engine archive (which DOES have
    // rerun-if-changed), giving mismatched `std::string` / `std::mutex` layouts
    // across the cxx boundary inside one DLL. Declaring the identity as a build
    // input turns a changed *selection* into a rebuild. Verbatim reasoning from
    // `umd/build.rs:158-173`; the same hole exists in both drivers.
    //
    // ⚠ The remaining hole, stated honestly: this does NOT catch an in-place
    // LLVM upgrade — same toolchain paths, different compiler. A generated
    // toolchain fingerprint (resolved `clang-cl --version` + the MSVC include
    // dir, with `rerun-if-changed` on it) is the stronger fix and is still a
    // follow-up in both crates.
    println!("cargo:rerun-if-env-changed=HELIOS_CLANG_CL");
    println!("cargo:rerun-if-env-changed=HELIOS_MSVC_LIB");
    println!("cargo:rerun-if-env-changed=HELIOS_VKD3D_BUILD");

    require_path("HELIOS_CLANG_CL", &clang_cl, false);
    require_path("HELIOS_MSVC_LIB", &archiver, false);
    require_path("HELIOS_VKD3D_BUILD", &vkd3d_build, true);
    require_path("HELIOS_MESA_BUILD", &mesa_build, true);

    // ⭐ THE MEASURED LINK SET — one archive.
    // `tmp/dx12/gates/G1-static/RESULT.md:27-42`: `libhelios_d3d12_static.a` is
    // a *union* archive; meson hands it every object of `libvkd3d-proton`,
    // `libvkd3d_common`, `libvkd3d-shader`, `libdxil-spirv`,
    // `libdxbc_spv_module` and `libdxbc_spv`, plus `debug.c`,
    // `debug_control.c` and `helios_entry.c`. Adding the other six archives
    // `DECISIONS.md` D4 lists is harmless but redundant, so they are not listed.
    let archive = format!(r"{vkd3d_build}\libs\d3d12core\libhelios_d3d12_static.a");
    // Checked separately from the directory: the directory existing only means
    // meson was configured, not that the static target was ever built, and a
    // missing archive would otherwise surface as a wall of unresolved externals.
    // The env var named is the one that relocates it.
    require_path("HELIOS_VKD3D_BUILD", &archive, false);

    // --- Compile the cxx bridge shim with clang-cl (matches vkd3d's ABI) -----
    let mut build = cxx_build::bridge("src/bridge12.rs");
    build
        .file("bridge/vkd3d_bridge.cpp")
        .compiler(&clang_cl)
        .archiver(&archiver)
        .std("c++17")
        // cxx-build disables C++ exceptions by default; the shared
        // `bridge_guard` is a try/catch and will not compile without this.
        // ⛔ Enabling EH is NOT permission to define
        // `HELIOS_BRIDGE_ENGINE_CATCH` — vkd3d throws nothing.
        .flag("/EHsc")
        // ⚠ `"bridge"` comes FIRST deliberately: a same-named header in this
        // crate must win, so a future D3D12-only override of a shared header is
        // possible without editing `umd_common`. Same ordering rule as
        // `umd/build.rs:196-202`.
        .include("bridge")
        // The shared bridge headers (`DECISIONS.md` D3b, stage S1):
        // `bridge_common.h`, `bridge_util.h`, `bridge_guard.h`. One copy of the
        // source, compiled by this bridge and by `umd`'s — which is the whole
        // reason there is exactly one `bridge_guard` in the tree.
        .include("../umd_common/bridge")
        .include("../protocol/include")
        // ⛔ NO vkd3d include directory. `vkd3d-proton-helios/include/vkd3d.h`
        // drags in `vulkan.h` and vkd3d's own widl `D3D12_*` types, which then
        // collide with the SDK's. The `D12-G1` static arm proved the Windows SDK
        // `<d3d12.h>` plus this archive link is sufficient
        // (`tmp/dx12/gates/G1-static/RESULT.md`), so the bridge sees only SDK
        // headers.
        //
        // ⭐ `_ALLOW_COMPILER_AND_STL_VERSION_MISMATCH` WAS HERE AND IS GONE
        // (2026-08-10). It suppressed the MSVC STL's own `#error` when the
        // clang-cl version fell outside the STL's supported-compiler window,
        // and it was carried with an explicit warning that it was "a
        // runtime-risk acknowledgement, not a fix — the ABI still rests on the
        // objects agreeing, which nothing here can prove — and it is the FIRST
        // suspect if the engine misbehaves."
        //
        // The mismatch it acknowledged was real: MSVC 14.44's `<yvals_core.h>`
        // hard-asserts "expected Clang 19.0.0 or newer" and the installed
        // clang-cl was 17.0.6. VS 18 then landed MSVC 14.51, which raises the
        // bar to Clang 20 and whose `__msvc_doom_core.hpp` assumes
        // `defined(__clang__)` implies `__builtin_verbose_trap` (a Clang 19
        // builtin) — so the define stopped being sufficient and the build
        // failed outright.
        //
        // The fix was the root cause, not a wider suppression: clang-cl and
        // libclang moved 17.0.6 -> 22.1.8, which satisfies both toolsets. The
        // define is unnecessary, so it is deleted rather than left as a
        // now-false claim about the toolchain. Verified: vkd3d's 215-target
        // build and this bridge both compile with it absent.
        //
        // ⇒ If a future MSVC raises the bar again, RAISE CLANG. Re-adding this
        // define would restore an ABI risk nothing can measure.
        .define("NOMINMAX", None)
        .define("WIN32_LEAN_AND_MEAN", None)
        .define("_WIN32_WINNT", "0x0A00")
        .define("_CRT_SECURE_NO_WARNINGS", None);
    build.compile("helios_vkd3d_bridge");

    // --- Link the prebuilt vkd3d engine --------------------------------------
    // An MS-format COFF archive (meson archiver = llvm-lib) with a `.a` name, so
    // it goes in as a full path link arg rather than through Rust's
    // `static=NAME` -> `NAME.lib` resolution.
    println!("cargo:rustc-link-arg-cdylib={archive}");
    println!("cargo:rerun-if-changed={archive}");

    // Explicit A5 package edge; no loader or module search is permitted in the
    // translator-bearing D3D12 path.
    let mesa_import = format!(r"{mesa_build}\src\virtio\vulkan\vulkan_virtio.dll.a");
    require_path("HELIOS_MESA_BUILD", &mesa_import, false);
    println!("cargo:rustc-link-arg-cdylib={mesa_import}");
    println!("cargo:rerun-if-changed={mesa_import}");

    // ⛔ `gdi32` and NOTHING else. Measured, not assumed: the gate's link
    // attempt with the archive alone left 14 `__imp_D3DKMT*` unresolved
    // (`libs/vkd3d/d3dkmt.c`; `vkd3d_dep` does not carry `lib_gdi32`), and after
    // adding `gdi32` the probe linked and ran clean. Not `advapi32`, not
    // `ole32`, not `user32`, not `version`.
    //
    // ⛔ **Never `dxgi`.** A WDDM UMD sits *below* DXGI and implements the DXGI
    // DDI; taking a dependency on `dxgi.dll` from inside the driver that DXGI
    // is layered on is a load-order inversion. `umd/build.rs:247-250` states the
    // same rule for D3D11. The static engine keeps it with zero `dxgi`
    // references at all, and `dumpbin /IMPORTS` showing no `dxgi.dll` is the
    // `D12-G1` pass criterion — first on the probe `.exe`, now on this DLL.
    println!("cargo:rustc-link-lib=dylib=gdi32");

    for path in [
        // Shared, in `umd_common/bridge/`. ⚠ Cargo does not track the
        // `umd_common` rlib's non-Rust files, so without these three lines a
        // `bridge_guard.h` edit leaves a stale `helios_vkd3d_bridge.lib` linked
        // into the DLL — the edit appears to have no effect, which is worse than
        // a build failure. `umd/build.rs:258-266` carries the identical list.
        "../umd_common/bridge/bridge_common.h",
        "../umd_common/bridge/bridge_guard.h",
        "../umd_common/bridge/bridge_util.h",
        "bridge/vkd3d_bridge.cpp",
        "bridge/vkd3d_bridge.h",
        // The cxx bridge module itself: `cxx_build::bridge` regenerates the glue
        // from it, so an `extern "C++"` signature change must rebuild the shim.
        "src/bridge12.rs",
    ] {
        println!("cargo:rerun-if-changed={path}");
    }
    println!("cargo:rerun-if-env-changed=HELIOS_MESA_BUILD");
}

fn main() {
    println!("cargo:rerun-if-changed={CACHED_BINDINGS}");

    // ⚠ Two different questions, and conflating them is the bug this shape
    // avoids. `TARGET` is what we are compiling FOR; `cfg!(windows)` here is
    // what the BUILD SCRIPT is running ON. bindgen needs the WDK and the bridge
    // needs clang-cl + the meson tree, and all three live on the build host — so
    // the availability question is about the host, not the target.
    let target = env::var("TARGET").unwrap_or_default();
    if !target.contains("windows") {
        // Not even targeting Windows: nothing here is meaningful.
        // `src/lib.rs`'s `#[cfg(not(windows))] compile_error!` reports that, and
        // it keys off this same target.
        println!("cargo:warning=helios_umd12: skipping d3d12umddi bindgen on non-Windows target");
        return;
    }

    if !cfg!(windows) {
        // ⛔ THE HOST CROSS-CHECK, AND IT RETURNS BEFORE ANY S4 WORK.
        //
        // Cross-checking from a WDK-less host (the Linux side). Serve the cached
        // generation so `ddi12.rs`'s `include!` resolves and the whole DDI
        // surface type-checks. ⛔ `cargo check` only — this never links a DLL,
        // and `PARALLEL.md` §10 requires the integrator to re-check on the VM.
        //
        // ⭐ This branch is the inner loop for the eleven-agent, 214-slot DDI
        // fan-out (`PARALLEL.md` §7): the difference between agents writing
        // handlers blind and writing them against the real signatures with the
        // compiler answering. ⛔ Nothing from the cxx or link half may run here —
        // `build_vkd3d_bridge` is called only after this return, deliberately,
        // and moving either call site breaks the fan-out. `tools/umd12-host-check.sh`
        // supplies the two build-script overrides cxx needs on top of this.
        let cached = Path::new(CACHED_BINDINGS);
        if !cached.is_file() {
            panic!(
                "helios_umd12: cross-checking for {target} on a host with no WDK, and \
                 {CACHED_BINDINGS} is missing. Generate it on the VM (umd-check.ps1 -Crate umd12) \
                 and copy $OUT_DIR/d3d12umddi.rs there."
            );
        }
        let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join("d3d12umddi.rs");
        std::fs::copy(cached, &out).expect("failed to stage cached d3d12umddi.rs");
        println!(
            "cargo:warning=helios_umd12: HOST CROSS-CHECK — using {CACHED_BINDINGS}, not the SDK \
             header. Types are checked; nothing is linked and no ABI claim is made here."
        );
        return;
    }

    // The real path: the build host is Windows. Regenerate from the SDK header
    // (ground truth), then build and link the engine bridge.
    generate_d3d12umddi_bindings();
    let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join("d3d12umddi.rs");
    compare_or_refresh_cache(&out);

    build_vkd3d_bridge();
}
