# Build and run an isolated, interactive Vulkan preflight. This does not prove
# native D3D12 conformance. FAIL is a completed diagnostic result, not a pass.
[CmdletBinding()]
param(
    [ValidateSet('Build','Schedule','Run')][string]$Mode = 'Build',
    [string]$BuildDir = 'C:\ProgramData\Helios\sparse-behavior-probe',
    [string]$ArchiveRoot = 'Z:\tmp\fl12-sparse-compat-20260908',
    [string]$InteractiveUser = 'Rupansh',
    [string]$ExpectedIcdSHA256 = ''
)
$executedSource = $MyInvocation.MyCommand.ScriptBlock.Ast.Extent.Text
$ErrorActionPreference = 'Stop'
if ($BuildDir -notmatch '^[Cc]:\\') { throw 'BuildDir must be on local C: disk.' }
$BuildDir = [IO.Path]::GetFullPath($BuildDir).TrimEnd('\')
$runner = Join-Path $BuildDir 'vulkan-sparse-behavior-probe.ps1'
$exe = Join-Path $BuildDir 'probe.exe'
$receiptPath = Join-Path $BuildDir 'build.json'
if ($Mode -eq 'Run') { Start-Transcript -Path "$BuildDir\scheduled.log" -Force | Out-Null }
function Artifact([string]$Path) {
    $f = Get-Item -LiteralPath $Path
    [ordered]@{ Path=$f.FullName; SHA256=(Get-FileHash -LiteralPath $f.FullName -Algorithm SHA256).Hash; Length=$f.Length }
}
if ($Mode -eq 'Build') {
    $sourceRoot = Split-Path $PSScriptRoot -Parent
    $inputs = @('tools\vulkan_sparse_behavior_probe.c','vkd3d-proton-helios\include\private\vkd3d_reserved_compat.h')
    New-Item -ItemType Directory -Force -Path $BuildDir | Out-Null
    Remove-Item -LiteralPath $receiptPath -ErrorAction SilentlyContinue
    foreach ($relative in $inputs) {
        $dest = Join-Path "$BuildDir\src" $relative
        New-Item -ItemType Directory -Force -Path (Split-Path $dest -Parent) | Out-Null
        [IO.File]::WriteAllBytes($dest,[IO.File]::ReadAllBytes((Join-Path $sourceRoot $relative)))
    }
    [IO.File]::WriteAllText($runner,$executedSource)
    $vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
    $vs = (& $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath | Select-Object -First 1)
    if (!$vs -or !$env:VULKAN_SDK) { throw 'MSVC and Vulkan SDK are required.' }
    $cmd = @"
@echo off
call "$vs\VC\Auxiliary\Build\vcvars64.bat" >nul
if errorlevel 1 exit /b 1
"C:\Program Files\LLVM\bin\clang-cl.exe" /nologo /TC /std:c11 /W4 /WX /clang:-Wno-missing-field-initializers /I"$env:VULKAN_SDK\Include" "$BuildDir\src\tools\vulkan_sparse_behavior_probe.c" /Fo"$BuildDir\probe.obj" /Fe"$exe" /link /LIBPATH:"$env:VULKAN_SDK\Lib" vulkan-1.lib
exit /b %errorlevel%
"@
    [IO.File]::WriteAllText("$BuildDir\build.cmd",$cmd)
    & cmd.exe /d /c "`"$BuildDir\build.cmd`" > `"$BuildDir\build.log`" 2>&1"
    if ($LASTEXITCODE) { Get-Content "$BuildDir\build.log"; throw "Probe compilation failed: $LASTEXITCODE" }
    $artifacts = @($inputs | ForEach-Object { Artifact (Join-Path "$BuildDir\src" $_) })
    $artifacts += @($runner,$exe,"$BuildDir\build.cmd","$BuildDir\build.log" | ForEach-Object { Artifact $_ })
    $receipt = [ordered]@{ UTC=[DateTime]::UtcNow.ToString('o'); Artifacts=$artifacts; VisualStudio=$vs; VulkanSDK=$env:VULKAN_SDK;
        Clang=(& 'C:\Program Files\LLVM\bin\clang-cl.exe' --version | Out-String); BuildSucceeded=$true }
    [IO.File]::WriteAllText($receiptPath,($receipt | ConvertTo-Json -Depth 8))
    Write-Output ($receipt | ConvertTo-Json -Depth 8)
    exit 0
}
if ($ExpectedIcdSHA256 -notmatch '\A[0-9a-fA-F]{64}\z') { throw 'Provide the exact expected loaded ICD SHA256.' }
$receipt = Get-Content -LiteralPath $receiptPath -Raw | ConvertFrom-Json
if (!$receipt.BuildSucceeded -or @($receipt.Artifacts).Count -ne 6) { throw 'Incomplete build receipt.' }
foreach ($artifact in $receipt.Artifacts) {
    if ((Get-FileHash -LiteralPath $artifact.Path -Algorithm SHA256).Hash -ne $artifact.SHA256) { throw "Changed build input/artifact: $($artifact.Path)" }
}
if ($Mode -eq 'Schedule') {
    $taskName = 'HeliosSparseBehaviorProbe'
    $old = Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
    if ($old -and $old.State -eq 'Running') { throw 'Sparse behavior probe is already running.' }
    $arguments = "-NoProfile -WindowStyle Hidden -ExecutionPolicy Bypass -File `"$runner`" -Mode Run -BuildDir `"$BuildDir`" -ArchiveRoot `"$ArchiveRoot`" -ExpectedIcdSHA256 $ExpectedIcdSHA256"
    $action = New-ScheduledTaskAction -Execute 'powershell.exe' -Argument $arguments
    $principal = New-ScheduledTaskPrincipal -UserId $InteractiveUser -LogonType Interactive -RunLevel Highest
    # Do not kill a timed-out GPU owner: its fence drain retains pending backing.
    $settings = New-ScheduledTaskSettingsSet -ExecutionTimeLimit ([TimeSpan]::Zero) -MultipleInstances IgnoreNew
    Register-ScheduledTask -TaskName $taskName -Action $action -Principal $principal -Settings $settings -Force | Out-Null
    Start-ScheduledTask -TaskName $taskName
    Write-Output "Started interactive $taskName. Read the completed result.json in the new archive."
    exit 0
}
$session = (Get-Process -Id $PID).SessionId
if ($session -eq 0) { throw 'GPU probing requires an interactive scheduled task.' }
if ($executedSource.TrimEnd() -ne [IO.File]::ReadAllText($runner).TrimEnd()) { throw 'Executing script differs from the built runner.' }
if (Get-Process | Where-Object { $_.ProcessName -match '^3DMark' }) { throw 'Do not run preflight during a benchmark.' }
foreach ($name in @('VKD3D_FEATURE_LEVEL')) {
    if ([Environment]::GetEnvironmentVariable($name)) { throw "Unexpected override: $name" }
}
$env:HELIOS_WSI_ASYNC_PRESENT = '1'; Remove-Item Env:\HELIOS_RETIRE_FEEDBACK -ErrorAction SilentlyContinue; $env:VK_LAYER_VALIDATE_SYNC = '1'
$runName = 'guest-behavior-' + (Get-Date -Format 'yyyyMMdd-HHmmss-fff') + '-' + [Guid]::NewGuid().ToString('N').Substring(0,8)
$runDir = Join-Path $BuildDir $runName
$archive = Join-Path $ArchiveRoot $runName
New-Item -ItemType Directory -Path $runDir | Out-Null
$result = [ordered]@{ UTC=[DateTime]::UtcNow.ToString('o'); Session=$session; PID=$PID; Completed=$false;
    Scope='Vulkan compatibility preflight only; no native D3D12, benchmark or visual acceptance';
    ExpectedIcdSHA256=$ExpectedIcdSHA256; Cases=@(); Build=$receipt; Error=$null;
    LoaderEnvironment=@{ VK_DRIVER_FILES=$env:VK_DRIVER_FILES; VK_ICD_FILENAMES=$env:VK_ICD_FILENAMES } }
$locks=@()
try {
    foreach ($artifact in $receipt.Artifacts) {
        $locks += [IO.File]::Open($artifact.Path,[IO.FileMode]::Open,[IO.FileAccess]::Read,[IO.FileShare]::Read)
        if ((Get-FileHash -LiteralPath $artifact.Path -Algorithm SHA256).Hash -ne $artifact.SHA256) { throw 'Build changed before execution.' }
    }
    Copy-Item -LiteralPath $exe -Destination "$runDir\probe.exe"
    foreach ($index in 0..6) {
        $log = "$runDir\case-$index.log"
        $start = [DateTime]::UtcNow
        & cmd.exe /d /c "`"$runDir\probe.exe`" --venus $index > `"$log`" 2>&1"
        $rc = $LASTEXITCODE
        $lines = @(Get-Content -LiteralPath $log)
        $modules = @($lines | Where-Object { $_ -match '^MODULE .*vulkan_virtio.*\.dll$' } | ForEach-Object { $_.Substring(7) } | Select-Object -Unique)
        if ($modules.Count -ne 1) { throw "Case $index did not identify one loaded Venus ICD." }
        $icd = Artifact $modules[0]
        if ($icd.SHA256 -ne $ExpectedIcdSHA256) { throw "Case $index loaded an unexpected ICD." }
        $final = @($lines | Where-Object { $_ -match '^RESULT exit=' })
        if ($final.Count -ne 1 -or $rc -notin @(0,1,77)) { throw "Case $index did not finish normally." }
        $cacheLines = @($lines | Where-Object { $_ -match '^CACHE .* status=' })
        if (!$cacheLines.Count) { throw "Case $index did not publish an identity-bound cache record." }
        $cachePath = ($cacheLines[-1] -replace '^CACHE (.*) status=.*$','$1')
        Copy-Item -LiteralPath $cachePath -Destination "$runDir\case-$index.bin"
        $result.Cases += [ordered]@{ Index=$index; ExitCode=$rc; Seconds=([DateTime]::UtcNow-$start).TotalSeconds;
            Result=([regex]::Match($final[0].ToString(),'^RESULT .*$').Value);
            Icd=$icd; Log=(Artifact $log); Cache=(Artifact "$runDir\case-$index.bin") }
    }
    $result.Completed = $true
} catch { $result.Error=$_.Exception.ToString() }
finally { foreach ($lock in $locks) { $lock.Dispose() } }
Copy-Item -LiteralPath $receiptPath -Destination "$runDir\build.json"
Copy-Item -LiteralPath "$BuildDir\src" -Destination "$runDir\src" -Recurse
Copy-Item -LiteralPath $runner -Destination "$runDir\vulkan-sparse-behavior-probe.ps1"
[IO.File]::WriteAllText("$runDir\archive-phase.txt",'Serializing result')
[IO.File]::WriteAllText("$runDir\result.json",($result | ConvertTo-Json -Depth 6))
[IO.File]::WriteAllText("$runDir\archive-phase.txt",'Copying archive')
New-Item -ItemType Directory -Force -Path $ArchiveRoot | Out-Null
Copy-Item -LiteralPath $runDir -Destination $archive -Recurse
foreach ($file in Get-ChildItem -LiteralPath $runDir -Recurse -File) {
    $dest = Join-Path $archive $file.FullName.Substring($runDir.Length+1)
    if ((Get-FileHash -LiteralPath $file.FullName).Hash -ne (Get-FileHash -LiteralPath $dest).Hash) { throw "Archive mismatch: $dest" }
}
Write-Output "$archive\result.json"
Stop-Transcript | Out-Null
if (!$result.Completed) { exit 1 }
exit 0
