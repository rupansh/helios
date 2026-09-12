param(
    [Parameter(Mandatory)][ValidatePattern('^\d+\.\d+\.\d+\.\d+$')][string]$Version,
    [string]$InstallRoot = "C:\VulkanSDK\$Version"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

$required = @("Include\vulkan\vulkan.h", "Lib\vulkan-1.lib", "Bin\glslangValidator.exe")
$missing = @($required | Where-Object { -not (Test-Path -LiteralPath (Join-Path $InstallRoot $_) -PathType Leaf) })
if ($missing.Count -ne 0) {
    $installer = Join-Path ([IO.Path]::GetTempPath()) "vulkansdk-windows-X64-$Version.exe"
    $timer = [Diagnostics.Stopwatch]::StartNew()
    try {
        Invoke-WebRequest "https://sdk.lunarg.com/sdk/download/$Version/windows/vulkansdk-windows-X64-$Version.exe" -OutFile $installer
        # Recent installers contain multiple archives. 7-Zip extraction alone
        # can yield working shader tools without the Vulkan headers/library.
        # LunarG's copy-only install also works with a restored directory cache.
        & $installer --root $InstallRoot --accept-licenses --default-answer --confirm-command install copy_only=1
        if ($LASTEXITCODE -ne 0) { throw "Vulkan SDK installer failed with exit code $LASTEXITCODE." }
    } finally {
        Remove-Item -LiteralPath $installer -Force -ErrorAction SilentlyContinue
    }
    Write-Host "Vulkan SDK download and installation took $([math]::Round($timer.Elapsed.TotalSeconds, 1)) seconds."
}

foreach ($relativePath in $required) {
    if (-not (Test-Path -LiteralPath (Join-Path $InstallRoot $relativePath) -PathType Leaf)) {
        throw "Vulkan SDK $Version is missing $relativePath in $InstallRoot."
    }
}
$env:VULKAN_SDK = $InstallRoot
$env:VK_SDK_PATH = $InstallRoot
$bin = Join-Path $InstallRoot "Bin"
$env:PATH = "$bin;$env:PATH"
& (Join-Path $bin "glslangValidator.exe") --version
if ($LASTEXITCODE -ne 0) { throw "Vulkan SDK shader tool validation failed." }
if ($env:GITHUB_ENV) {
    "VULKAN_SDK=$InstallRoot" | Out-File -FilePath $env:GITHUB_ENV -Append -Encoding utf8
    "VK_SDK_PATH=$InstallRoot" | Out-File -FilePath $env:GITHUB_ENV -Append -Encoding utf8
}
if ($env:GITHUB_PATH) { $bin | Out-File -FilePath $env:GITHUB_PATH -Append -Encoding utf8 }
