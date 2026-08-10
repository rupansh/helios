<#
.SYNOPSIS
  Syntax-check VK_LAYER_HELIOS_present without building Mesa.

.DESCRIPTION
  Runs clang-cl over `icd/mesa/src/vulkan/helios-present-layer/helios_present_layer.cpp`
  alone: no meson configure, no Mesa dependencies, no build directory.

  ⭐ THIS IS A SECOND COMPILER, NOT A SUBSTITUTE FOR THE BUILD. The layer IS in
  meson.build now (`-Dvulkan-layers=helios-present`), built by **mingw-w64
  g++**. This script uses **clang-cl**, and on 2026-08-10 the two disagreed
  about what was wrong in ways that matter:

    clang-cl found  17 errors of self-drift (a struct missing a member the
                    dispatch tables all supply, three gate constants used and
                    never defined, resize() on a type owning a std::mutex);
    mingw found     the DLL name_prefix mismatch — which is SILENT, the loader
                    just never finds the layer — and a dead function via
                    -Wunused-function.

  Neither compiler alone was sufficient. Keep both.

  It is also still the cheap one: seconds against a meson build, so it is what
  to run after every edit to the layer.

  ⚠ VM ONLY. The layer includes <windows.h>, <directx/d3d12.h> and
  <vulkan/vk_layer.h>; it cannot be checked from the Linux host, which is why it
  is not in tools/retirement-gates.sh.

.EXAMPLE
  win_exec: powershell -ExecutionPolicy Bypass -File Z:\tools\mesa-layer-syntax.ps1
#>
[CmdletBinding()]
param(
    # Emit the object too and assert the layer's eight loader entry points are
    # exported. clang warns -Wdll-attribute-on-redeclaration on every one of
    # them because the Vulkan headers declare them without dllexport; the
    # warning is cosmetic, but "cosmetic" is a claim, so this checks it.
    [switch]$CheckExports
)

# ⚠ NOT 'Stop'. With ErrorActionPreference=Stop, PowerShell turns ANY text a
# native command writes to stderr into a terminating NativeCommandError — and
# clang writes its warnings there, so a clean-but-warning compile killed this
# script at the first warning. Exit codes are checked explicitly below, which is
# the right signal for a compiler anyway.
$ErrorActionPreference = 'Continue'

$clang = 'C:\Program Files\LLVM\bin\clang-cl.exe'
$src   = 'Z:\icd\mesa\src\vulkan\helios-present-layer\helios_present_layer.cpp'
$log   = 'Z:\tmp\mesa_layer_syntax.log'

if (-not (Test-Path $clang)) { Write-Error "clang-cl not found at $clang"; exit 2 }
if (-not (Test-Path $src))   { Write-Error "layer source not found at $src";  exit 2 }

$includes = @(
    '/IZ:\icd\mesa\include',
    '/IZ:\icd\mesa\subprojects\DirectX-Headers-1.0\include',
    '/IZ:\icd\mesa\subprojects\DirectX-Headers-1.0\include\directx'
)

& $clang -fsyntax-only /std:c++17 /EHsc /D_CRT_SECURE_NO_WARNINGS @includes $src > $log 2>&1
$compileExit = $LASTEXITCODE

$errors   = @(Get-Content $log | Select-String ': error:'   -SimpleMatch).Count
$warnings = @(Get-Content $log | Select-String ': warning:' -SimpleMatch).Count
Write-Output "helios_present_layer.cpp: $errors error(s), $warnings warning(s)  (log: $log)"

if ($errors -gt 0) {
    Get-Content $log | Select-String ': error:' -SimpleMatch |
        Select-Object -First 20 | ForEach-Object { "  " + ($_ -replace '^clang-cl.exe : ', '') }
    exit 1
}

if ($CheckExports) {
    $obj = Join-Path $env:TEMP 'helios_layer_export_probe.obj'
    & $clang /c /std:c++17 /EHsc /D_CRT_SECURE_NO_WARNINGS /w @includes "/Fo$obj" $src 2>&1 | Out-Null
    if ($LASTEXITCODE -ne 0) { Write-Error "object build failed"; exit 1 }

    # The names the Vulkan loader resolves from a layer DLL by name. Missing any
    # of them means the layer silently never loads.
    $required = @(
        'vkNegotiateLoaderLayerInterfaceVersion',
        'vkGetInstanceProcAddr',
        'vkGetDeviceProcAddr',
        'vk_layerGetPhysicalDeviceProcAddr',
        'vkEnumerateInstanceLayerProperties',
        'vkEnumerateInstanceExtensionProperties',
        'vkEnumerateDeviceLayerProperties',
        'vkEnumerateDeviceExtensionProperties'
    )
    $directives = & 'C:\Program Files\LLVM\bin\llvm-readobj.exe' --coff-directives $obj 2>&1 | Out-String
    Remove-Item $obj -Force -ErrorAction SilentlyContinue

    $missing = $required | Where-Object { $directives -notmatch [regex]::Escape("/EXPORT:$_") }
    if ($missing) {
        Write-Output "MISSING /EXPORT directives: $($missing -join ', ')"
        Write-Output "The layer would build and then never load. A .def is the fix."
        exit 1
    }
    Write-Output "all $($required.Count) loader entry points carry /EXPORT directives"
}

if ($compileExit -ne 0) { exit 1 }
Write-Output "OK"
exit 0
