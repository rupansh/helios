param(
    [string]$DllPath = "C:\Users\Rupansh\helios-mesa-build\src\virtio\vulkan\vulkan_virtio.dll",
    [string]$InstallDir = "C:\ProgramData\HeliosVulkan",
    [string]$ApiVersion = "1.4.352",
    [ValidateSet("Machine", "User")]
    [string]$Scope = "Machine",
    [switch]$NoRegistryCleanup,
    [switch]$NoSmoke
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Test-IsAdmin {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = [Security.Principal.WindowsPrincipal]::new($identity)
    return $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

function Resolve-ExistingPath([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "File not found: $Path"
    }
    return (Resolve-Path -LiteralPath $Path).Path
}

function Remove-StaleHeliosIcdValues([string]$RegPath) {
    if (-not (Test-Path -LiteralPath $RegPath)) {
        return
    }

    $item = Get-Item -LiteralPath $RegPath
    foreach ($name in $item.GetValueNames()) {
        $lower = $name.ToLowerInvariant()
        $isHelios =
            $lower.Contains("heliosvulkan") -or
            $lower.Contains("virtio_devenv_icd") -or
            $lower.Contains("vulkan_virtio.dll") -or
            $lower.Contains("helios-mesa-mingw")
        if ($isHelios) {
            Remove-ItemProperty -LiteralPath $RegPath -Name $name -ErrorAction SilentlyContinue
            Write-Output "Removed stale Vulkan ICD registry value: $name"
        }
    }
}

$sourceDll = Resolve-ExistingPath $DllPath
$installDirFull = [IO.Path]::GetFullPath($InstallDir)
$manifest = Join-Path $installDirFull "virtio_devenv_icd.x86_64.json"
$sourceHash = Get-FileHash -LiteralPath $sourceDll -Algorithm SHA256
$hashPrefix = $sourceHash.Hash.Substring(0, 12).ToLowerInvariant()
$destDll = Join-Path $installDirFull "vulkan_virtio-$hashPrefix.dll"
$stableDll = Join-Path $installDirFull "vulkan_virtio.dll"

if ($Scope -eq "Machine" -and -not (Test-IsAdmin)) {
    throw "Machine-scope install writes HKLM and requires an elevated PowerShell. Re-run as Administrator, or pass -Scope User."
}

New-Item -ItemType Directory -Force -Path $installDirFull | Out-Null
Copy-Item -LiteralPath $sourceDll -Destination $destDll -Force
try {
    Copy-Item -LiteralPath $sourceDll -Destination $stableDll -Force -ErrorAction Stop
} catch {
    Write-Output "Stable DLL path is in use; leaving it unchanged: $stableDll"
}

$json = [ordered]@{
    file_format_version = "1.0.1"
    ICD = [ordered]@{
        library_path = ($destDll -replace "\\", "/")
        library_arch = "64"
        api_version = $ApiVersion
    }
}
$json | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath $manifest -Encoding ascii

foreach ($oldDll in Get-ChildItem -LiteralPath $installDirFull -Filter "vulkan_virtio-*.dll" -ErrorAction SilentlyContinue) {
    if ($oldDll.FullName -eq $destDll) {
        continue
    }

    try {
        Remove-Item -LiteralPath $oldDll.FullName -Force -ErrorAction Stop
        Write-Output "Removed old ICD DLL: $($oldDll.FullName)"
    } catch {
        Write-Output "Old ICD DLL still in use, keeping: $($oldDll.FullName)"
    }
}

$cleanupRegPaths = @(
    "HKLM:\SOFTWARE\Khronos\Vulkan\Drivers",
    "HKCU:\SOFTWARE\Khronos\Vulkan\Drivers"
)
$installRegPaths = @()
if ($Scope -eq "Machine") {
    $installRegPaths += "HKLM:\SOFTWARE\Khronos\Vulkan\Drivers"
} else {
    $installRegPaths += "HKCU:\SOFTWARE\Khronos\Vulkan\Drivers"
}

if (-not $NoRegistryCleanup) {
    foreach ($regPath in $cleanupRegPaths) {
        Remove-StaleHeliosIcdValues $regPath
    }
}

foreach ($regPath in $installRegPaths) {
    New-Item -Path $regPath -Force | Out-Null
    New-ItemProperty -LiteralPath $regPath -Name $manifest -Value 0 -PropertyType DWord -Force | Out-Null
    Write-Output "Registered Vulkan ICD manifest: $manifest"
}

Write-Output "Installed ICD DLL: $destDll"
Write-Output "SHA256: $($sourceHash.Hash)"
Write-Output "Manifest:"
Get-Content -LiteralPath $manifest

if (-not $NoSmoke) {
    $vulkaninfo = Get-Command vulkaninfo.exe -ErrorAction SilentlyContinue
    if ($vulkaninfo) {
        Write-Output "Running vulkaninfo --summary with VK_DRIVER_FILES=$manifest"
        $env:VK_DRIVER_FILES = $manifest
        & $vulkaninfo.Source --summary
    } else {
        Write-Output "vulkaninfo.exe not found on PATH; skipping smoke test."
    }
}
