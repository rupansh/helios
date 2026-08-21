param(
    [Parameter(Mandatory)][string]$RepoRoot,
    [Parameter(Mandatory)][string]$OutputDir,
    [string]$BuildRoot = "C:\helios-build"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
. (Join-Path $PSScriptRoot "Initialize-HeliosBuild.ps1")

$RepoRoot = (Resolve-Path -LiteralPath $RepoRoot).Path
Import-VisualStudioEnvironment
$clangCl = Assert-Command "clang-cl.exe"
$llvmLib = Assert-Command "llvm-lib.exe"
$llvmReadObj = Assert-Command "llvm-readobj.exe"
Assert-Command "meson.exe" | Out-Null
Assert-Command "ninja.exe" | Out-Null
Assert-Command "cargo.exe" | Out-Null
Assert-Command "cargo-make.exe" | Out-Null

$stampInf = Find-WindowsKitTool "stampinf.exe"
$inf2Cat = Find-WindowsKitTool "Inf2Cat.exe"
$kitBin = Split-Path -Parent $stampInf
$env:PATH = "$kitBin;$env:PATH"
$env:LIBCLANG_PATH = Split-Path -Parent $clangCl

$dxvkSource = Join-Path $RepoRoot "dxvk-helios"
$dxvkBuild = Join-Path $BuildRoot "dxvk"
$nativeFile = Join-Path $RepoRoot "ci\windows\clang-cl-native.ini"
$compatHeader = Join-Path $RepoRoot "umd\build-support\dxvk_c_compat.h"
New-Item -ItemType Directory -Force -Path $BuildRoot | Out-Null

if (Test-Path -LiteralPath $dxvkBuild) {
    Remove-Item -LiteralPath $dxvkBuild -Recurse -Force
}

# ⭐ `/D_ALLOW_COMPILER_AND_STL_VERSION_MISMATCH` WAS HERE AND IS GONE
# (2026-08-10), for the same reason it left umd/build.rs, umd12/build.rs and
# win-mcp's win_vkd3d in the same changeset: its only effect is to suppress the
# MSVC STL's own `error STL1000: Unexpected compiler version, expected Clang N
# or newer`, and every site that carried it recorded it as "a runtime-risk
# acknowledgement, not a fix". The fix is the compiler — LLVM_VERSION in
# .github/workflows/windows-stack.yml is 22.1.8, which clears both toolsets'
# floors (14.44 wants 19+, 14.51 wants 20+).
#
# ⛔ It had to go WITH that version bump, not after it. Keeping it would have
# left one job, one runner and one clang-cl stating opposite toolchain
# assumptions: this meson configure would say "the STL and the compiler may
# disagree" while the umd/build.rs that this same script later drives (through
# $env:HELIOS_CLANG_CL, via the `cargo make` below) says the opposite by having
# deleted the define. `umd` is the FIRST crate Cargo.make.toml's UMD loop
# builds, so that clang-cl bridge compile is where a stale floor surfaces.
#
# ⚠ Bound, stated because it cannot be closed from here: this DXVK configure has
# NOT been run under clang 22 — the workflow triggers only on push/PR to `wddm`
# and the retirement branch is `wddm-dx12`. The argument is narrow and holds
# without a run: removing a `-D` whose sole job is to gate an `#error` can only
# change the build by letting that `#error` fire, and clang 22 does not trip it.
# The nearest empirical support is vkd3d, rebuilt on the VM with an empty
# `-Dcpp_args=` for 215/215 clean targets; DXVK itself was not rebuilt.
# ⇒ If a future MSVC raises the bar again, RAISE CLANG. Do not restore this.
$dxvkCppArgs = @(
    "-Wno-deprecated-declarations"
    "-Wno-delete-non-abstract-non-virtual-dtor"
    "-Wno-unused-private-field"
    "-Wno-unused-lambda-capture"
    "-Wno-c++20-extensions"
    "-Wno-unused-const-variable"
) -join " "

& meson.exe setup $dxvkBuild $dxvkSource `
    --native-file $nativeFile `
    --buildtype release `
    -Db_vscrt=mt `
    "-Dcpp_args=$dxvkCppArgs" `
    "-Dc_args=/FI$compatHeader" `
    -Denable_d3d8=false `
    -Denable_d3d9=false `
    -Denable_d3d10=false `
    -Denable_d3d11=true `
    -Denable_dxgi=true
if ($LASTEXITCODE -ne 0) { throw "DXVK meson setup failed with exit code $LASTEXITCODE." }

& meson.exe compile -C $dxvkBuild
if ($LASTEXITCODE -ne 0) { throw "DXVK build failed with exit code $LASTEXITCODE." }

$env:HELIOS_DXVK_SRC = $dxvkSource
$env:HELIOS_DXVK_BUILD = $dxvkBuild
$env:HELIOS_CLANG_CL = $clangCl
$env:HELIOS_MSVC_LIB = $llvmLib
$env:HELIOS_WDK_INCLUDE = Find-WindowsKitInclude
$env:HELIOS_MSVC_INCLUDE = Join-Path $env:VCToolsInstallDir "include"

# rust-script repeats cargo-make's 64-character generated script names in its
# target paths. The normal runner profile makes those paths exceed link.exe's
# legacy MAX_PATH limit. wdk-build also force-installs its own rust-script
# version during the build, so an executable wrapper is not durable. Redirect
# the per-user cache root for child processes instead, then restore the shell
# folder immediately after cargo-make exits.
$shellFoldersKey = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Explorer\User Shell Folders"
$localAppDataName = "Local AppData"
$previousLocalAppData = Get-ItemPropertyValue -LiteralPath $shellFoldersKey -Name $localAppDataName
$previousLocalAppDataEnvironment = $env:LOCALAPPDATA
$shortLocalAppData = "C:\la"
New-Item -ItemType Directory -Force -Path $shortLocalAppData | Out-Null
Set-ItemProperty -LiteralPath $shellFoldersKey -Name $localAppDataName -Value $shortLocalAppData
$env:LOCALAPPDATA = $shortLocalAppData

$kmdRoot = Join-Path $RepoRoot "kmd_render"
Push-Location $kmdRoot
try {
    & cargo.exe make --profile release --makefile Cargo.make.toml
    if ($LASTEXITCODE -ne 0) { throw "Helios driver build failed with exit code $LASTEXITCODE." }
} finally {
    Pop-Location
    Set-ItemProperty -LiteralPath $shellFoldersKey -Name $localAppDataName -Value $previousLocalAppData
    $env:LOCALAPPDATA = $previousLocalAppDataEnvironment
}

$package = Join-Path $kmdRoot "target\release\helios_kmd_render_package"
$required = @("helios_kmd_render.inf", "helios_kmd_render.sys", "helios_umd.dll", "helios_umd12.dll")
foreach ($name in $required) {
    if (-not (Test-Path -LiteralPath (Join-Path $package $name) -PathType Leaf)) {
        throw "Driver package output is missing $name in $package."
    }
}

# A display UMD is loaded into arbitrary application processes. A dynamic MSVC
# or UCRT dependency binds through that application's DLL search order; CapCut,
# for example, supplies MSVCP140 14.28 to a UMD compiled against the 14.44 STL,
# which leaves driver-internal std::mutex objects ABI-incompatible and crashes
# its GPU process. Keep the shipped UMD self-contained and make CRT regressions
# a packaging failure rather than an application-specific runtime failure.
$umdDll = Join-Path $package "helios_umd.dll"
$umdImports = @(& $llvmReadObj --coff-imports $umdDll 2>&1)
if ($LASTEXITCODE -ne 0) {
    throw "Failed to inspect helios_umd.dll imports with llvm-readobj (exit $LASTEXITCODE)."
}
$dynamicCrtImports = @(
    $umdImports |
        Where-Object { $_ -match '(?i)(MSVCP\d+|VCRUNTIME\d+(?:_\d+)?|UCRTBASE|api-ms-win-crt-[^\s]+)\.dll' } |
        ForEach-Object { $_.Trim() } |
        Sort-Object -Unique
)
if ($dynamicCrtImports.Count -ne 0) {
    throw "helios_umd.dll imports an application-resolvable dynamic CRT: $($dynamicCrtImports -join '; ')"
}

# The vkd3d engine and its C++ shim deliberately share the dynamic MSVC CRT,
# but a WDDM UMD still cannot import the DXGI/D3D12 runtime above it or the
# Vulkan loader beside it. Keep that load-order boundary explicit here.
$umd12Dll = Join-Path $package "helios_umd12.dll"
$umd12Imports = @(& $llvmReadObj --coff-imports $umd12Dll 2>&1)
if ($LASTEXITCODE -ne 0) {
    throw "Failed to inspect helios_umd12.dll imports with llvm-readobj (exit $LASTEXITCODE)."
}
$forbiddenUmd12Imports = @(
    $umd12Imports |
        Where-Object { $_ -match '(?i)\b(?:dxgi|d3d12|vulkan-1)\.dll\b' } |
        ForEach-Object { $_.Trim() } |
        Sort-Object -Unique
)
if ($forbiddenUmd12Imports.Count -ne 0) {
    throw "helios_umd12.dll imports a forbidden upper-layer runtime: $($forbiddenUmd12Imports -join '; ')"
}

New-Item -ItemType Directory -Force -Path $OutputDir | Out-Null
Copy-Item -Path (Join-Path $package "*") -Destination $OutputDir -Recurse -Force

$umdPdb = Join-Path $RepoRoot "umd\target\release\helios_umd.pdb"
if (Test-Path -LiteralPath $umdPdb -PathType Leaf) {
    Copy-Item -LiteralPath $umdPdb -Destination $OutputDir -Force
}
$umd12Pdb = Join-Path $RepoRoot "umd12\target\release\helios_umd12.pdb"
if (Test-Path -LiteralPath $umd12Pdb -PathType Leaf) {
    Copy-Item -LiteralPath $umd12Pdb -Destination $OutputDir -Force
}
New-Item -ItemType Directory -Force -Path (Join-Path $OutputDir "licenses\dxvk") | Out-Null
Copy-Item -LiteralPath (Join-Path $dxvkSource "LICENSE") -Destination (Join-Path $OutputDir "licenses\dxvk\LICENSE") -Force

Write-Host "Driver artifact staged at $OutputDir"
