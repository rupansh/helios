param(
    [Parameter(Mandatory)][string]$RepoRoot,
    [Parameter(Mandatory)][ValidateSet("umd", "umd12")][string]$Crate,
    [Parameter(Mandatory)][ValidateSet("x64", "x86")][string]$Architecture,
    [string]$Profile = "dev"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
. (Join-Path $PSScriptRoot "Initialize-HeliosBuild.ps1")
$RepoRoot = (Resolve-Path -LiteralPath $RepoRoot).Path
$drive = [IO.DriveInfo]::new([IO.Path]::GetPathRoot($RepoRoot))
if ($drive.DriveType -ne [IO.DriveType]::Fixed) {
    throw "UMD builds require a local Windows disk, never the shared Z: source tree: $RepoRoot"
}
Import-VisualStudioEnvironment -Architecture $Architecture
# Keep the selected LLVM toolchain first after VsDevCmd rewrites PATH.
if ($env:LIBCLANG_PATH) { $env:PATH = "$env:LIBCLANG_PATH;$env:PATH" }
if ($env:RUST_TOOLCHAIN) { $env:RUSTUP_TOOLCHAIN = $env:RUST_TOOLCHAIN }
if ($Architecture -eq "x86") {
    $engineName = if ($Crate -eq "umd") { "HELIOS_DXVK_BUILD" } else { "HELIOS_VKD3D_BUILD" }
    $engine = [Environment]::GetEnvironmentVariable("${engineName}_X86")
    if (-not $engine -or -not (Test-Path -LiteralPath $engine -PathType Container)) {
        throw "${engineName}_X86 must name a matching clang-cl x86 engine build."
    }
    [Environment]::SetEnvironmentVariable($engineName, $engine, "Process")
}
$crateRoot = Join-Path $RepoRoot $Crate
# Isolate the child from cargo-make's KMD target directory. Native artifacts use
# target/<profile>, x86 artifacts target/i686-pc-windows-msvc/<profile>.
$env:CARGO_TARGET_DIR = Join-Path $crateRoot "target"
$cargoArguments = @("build")
if ($Architecture -eq "x86") { $cargoArguments += @("--target", "i686-pc-windows-msvc") }
if ($Profile -ne "dev") { $cargoArguments += @("--profile", $Profile) }
Push-Location $crateRoot
try {
    & cargo.exe @cargoArguments
    if ($LASTEXITCODE -ne 0) { throw "$Crate $Architecture build failed with exit code $LASTEXITCODE." }
} finally { Pop-Location }
