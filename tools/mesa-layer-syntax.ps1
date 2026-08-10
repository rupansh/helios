<#
.SYNOPSIS
  Syntax-check VK_LAYER_HELIOS_present without building Mesa.

.DESCRIPTION
  `icd/mesa/src/vulkan/wsi/helios_present_layer.cpp` is not in `meson.build`
  yet, so nothing in any build touches it. It sat at 4269 lines for days in that
  state and the first compile produced 17 errors — the file had drifted from
  itself because nothing had ever checked it.

  This runs clang-cl over the translation unit alone. It needs no meson
  configure, no Mesa dependencies and no VM build directory, so it is cheap
  enough to run after every edit to the layer, which is the point: the reason
  the drift accumulated is that checking it used to require wiring it into a
  build first.

  ⚠ VM ONLY. The layer includes <windows.h>, <directx/d3d12.h> and
  <vulkan/vk_layer.h>; it cannot be checked from the Linux host, which is why it
  is not in tools/retirement-gates.sh.

  Retire this script once the layer is in meson.build and `win_meson` compiles
  it as a matter of course.

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
$src   = 'Z:\icd\mesa\src\vulkan\wsi\helios_present_layer.cpp'
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
