# Direct DDI tests, separate from native-runtime admission/GPU tests.
[CmdletBinding()]
param(
    [ValidateSet('Build','Schedule','Run')][string]$Mode='Build',
    [string]$BuildDir='C:\ProgramData\Helios\adapter-probe',
    [string]$ArchiveRoot='Z:\tmp\fl12-adapter',
    [string]$Umd12Path='',
    [string]$ExpectedUmd12SHA256=''
)
$executedSource=$MyInvocation.MyCommand.ScriptBlock.Ast.Extent.Text
$ErrorActionPreference='Stop'
if ($BuildDir -notmatch '^[Cc]:\\' -or $BuildDir -match '["\r\n]' -or $ArchiveRoot -match '["\r\n]') {
    throw 'Use a local C: build directory and paths without quotes or newlines.'
}
$BuildDir=[IO.Path]::GetFullPath($BuildDir).TrimEnd('\')
$ArchiveRoot=[IO.Path]::GetFullPath($ArchiveRoot).TrimEnd('\')
$names=@('d3d12_adapter_probe.cpp','d3d12-adapter-probe.ps1','d3d12umddi.h','build.cmd','build.log','d3d12_adapter_probe.exe')
function Artifact([string]$path) {
    $item=Get-Item -LiteralPath $path
    [ordered]@{Name=$item.Name;Path=$item.FullName;Length=$item.Length;SHA256=(Get-FileHash -LiteralPath $path).Hash}
}
if ($Mode -eq 'Build') {
    New-Item -ItemType Directory -Path $BuildDir -Force | Out-Null
    foreach ($name in @('d3d12_adapter_probe.cpp','d3d12-adapter-probe.ps1')) {
        Copy-Item -LiteralPath (Join-Path $PSScriptRoot $name) -Destination $BuildDir
    }
    $sdk='C:\Program Files (x86)\Windows Kits\10\Include\10.0.26100.0\um\d3d12umddi.h'
    Copy-Item -LiteralPath $sdk -Destination $BuildDir
    $vswhere='C:\Program Files (x86)\Microsoft Visual Studio\Installer\vswhere.exe'
    $vs=& $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
    if (!$vs) {throw 'MSVC toolchain missing.'}
    $command=@"
@echo off
call "$vs\VC\Auxiliary\Build\vcvarsall.bat" x64 10.0.26100.0 >nul
if errorlevel 1 exit /b %errorlevel%
set WindowsSDKVersion
set VCToolsVersion
cl /nologo /Bv /W4 /WX /EHsc /std:c++17 /O2 /I. d3d12_adapter_probe.cpp /Fe:d3d12_adapter_probe.exe 2>&1
exit /b %errorlevel%
"@
    [IO.File]::WriteAllText((Join-Path $BuildDir 'build.cmd'),$command,[Text.Encoding]::ASCII)
    Push-Location $BuildDir
    try { & cmd.exe /d /c build.cmd *> build.log; $code=$LASTEXITCODE }
    finally {Pop-Location}
    if ($code) {Get-Content (Join-Path $BuildDir 'build.log') -Tail 25;throw "Compiler failed: $code"}
    [ordered]@{Schema=1;Scope='Direct DDI contract';UTC=[DateTime]::UtcNow.ToString('o');SDKHeader=$sdk;
        Files=@($names | ForEach-Object {Artifact (Join-Path $BuildDir $_)})} |
        ConvertTo-Json -Depth 6 | Set-Content (Join-Path $BuildDir 'build.json') -Encoding UTF8
    Write-Output "Built $BuildDir\d3d12_adapter_probe.exe"
    exit 0
}
if ($ExpectedUmd12SHA256 -notmatch '\A[0-9a-fA-F]{64}\z' -or $Umd12Path -notmatch '^[Cc]:\\' -or $Umd12Path -match '["\r\n]') {
    throw 'An exact local UMD12 path and full SHA256 are required.'
}
foreach ($name in @('VKD3D_FEATURE_LEVEL','VKD3D_SHADER_MODEL','VKD3D_SHADER_OVERRIDE','D3D12SDKPath','D3D12SDKVersion')) {
    if ([Environment]::GetEnvironmentVariable($name)) {throw "Forbidden override: $name"}
}
$receipt=Get-Content (Join-Path $BuildDir 'build.json') -Raw | ConvertFrom-Json
if ($receipt.Schema -ne 1 -or $receipt.Files.Count -ne $names.Count) {throw 'Invalid build receipt.'}
foreach ($name in $names) {
    $entries=@($receipt.Files | Where-Object {$_.Name -ceq $name})
    if ($entries.Count -ne 1 -or (Get-FileHash (Join-Path $BuildDir $name)).Hash -ine $entries[0].SHA256) {
        throw "Stale build: $name"
    }
}
if ((Get-FileHash $Umd12Path).Hash -ine $ExpectedUmd12SHA256) {throw 'UMD12 hash mismatch.'}
if ($Mode -eq 'Schedule') {
    if (Get-Process | Where-Object {$_.ProcessName -match '3DMark|d3d12.*probe|helios-native-features|paintcap'}) {
        throw 'Preserving active graphical workload.'
    }
    $runner=Join-Path $BuildDir 'd3d12-adapter-probe.ps1'
    $args='-NoProfile -WindowStyle Hidden -ExecutionPolicy Bypass -File "'+$runner+'" -Mode Run -BuildDir "'+$BuildDir+'" -ArchiveRoot "'+$ArchiveRoot+'" -Umd12Path "'+$Umd12Path+'" -ExpectedUmd12SHA256 '+$ExpectedUmd12SHA256
    $action=New-ScheduledTaskAction -Execute powershell.exe -Argument $args -WorkingDirectory $BuildDir
    $principal=New-ScheduledTaskPrincipal -UserId Rupansh -LogonType Interactive -RunLevel Highest
    Register-ScheduledTask -TaskName HeliosAdapterContract -Action $action -Principal $principal -Settings (New-ScheduledTaskSettingsSet -ExecutionTimeLimit (New-TimeSpan -Minutes 3)) -Force | Out-Null
    Start-ScheduledTask -TaskName HeliosAdapterContract
    Write-Output 'Scheduled direct DDI probe; task start is not a pass.'
    exit 0
}
if ((Get-Process -Id $PID).SessionId -eq 0) {throw 'Interactive session required.'}
if (![string]::Equals($executedSource,[IO.File]::ReadAllText((Join-Path $BuildDir 'd3d12-adapter-probe.ps1')),[StringComparison]::Ordinal)) {
    throw 'Parsed runner differs from the verified build runner.'
}
$out=Join-Path $BuildDir ('run-'+(Get-Date -Format yyyyMMdd-HHmmss-fff)+'-'+[guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory $out | Out-Null
$result=[ordered]@{UTC=[DateTime]::UtcNow.ToString('o');Scope='Direct DDI contract; no native D3D12 runtime or GPU execution';
    Session=(Get-Process -Id $PID).SessionId;UMD12=(Artifact $Umd12Path);Completed=$false;Passed=$false;ExitCode=1}
$locks=@();$process=$null
try {
    foreach ($name in $names + @('build.json')) {
        $path=Join-Path $BuildDir $name
        $locks += [IO.File]::Open($path,[IO.FileMode]::Open,[IO.FileAccess]::Read,[IO.FileShare]::Read)
        Copy-Item -LiteralPath $path -Destination $out
    }
    # Lock the tested module through process exit; the installed path is never changed.
    $locks += [IO.File]::Open($Umd12Path,[IO.FileMode]::Open,[IO.FileAccess]::Read,[IO.FileShare]::Read)
    if ((Get-FileHash $Umd12Path).Hash -ine $ExpectedUmd12SHA256) {throw 'UMD changed before launch.'}
    foreach ($entry in $receipt.Files) {
        if ((Get-FileHash (Join-Path $out $entry.Name)).Hash -ine $entry.SHA256) {throw 'Copied build input changed.'}
    }
    $env:HELIOS_WSI_ASYNC_PRESENT='1'
    Remove-Item Env:\HELIOS_RETIRE_FEEDBACK -ErrorAction SilentlyContinue
    $info=New-Object Diagnostics.ProcessStartInfo
    $info.FileName=Join-Path $out 'd3d12_adapter_probe.exe';$info.Arguments='"'+$Umd12Path+'"'
    $info.WorkingDirectory=$out;$info.UseShellExecute=$false;$info.CreateNoWindow=$true
    $info.RedirectStandardOutput=$true;$info.RedirectStandardError=$true
    $process=New-Object Diagnostics.Process;$process.StartInfo=$info
    if (!$process.Start()) {throw 'Probe did not start.'}
    $result.ProcessId=$process.Id
    $stdout=$process.StandardOutput.ReadToEndAsync();$stderr=$process.StandardError.ReadToEndAsync()
    if (!$process.WaitForExit(60000)) {throw 'Probe timeout; preserve child for diagnosis.'}
    $text=$stdout.GetAwaiter().GetResult()
    [IO.File]::WriteAllText((Join-Path $out 'stdout.txt'),$text)
    [IO.File]::WriteAllText((Join-Path $out 'stderr.txt'),$stderr.GetAwaiter().GetResult())
    $result.ExitCode=$process.ExitCode
    $summaries=@($text -split "`r?`n" | Where-Object {$_ -match '^SUMMARY,checks=[1-9][0-9]*,failures=[0-9]+$'})
    $modules=@($text -split "`r?`n" | Where-Object {$_ -like 'MODULE,*'})
    if ($modules.Count -ne 1 -or $modules[0].Substring(7) -ine $Umd12Path) {throw 'Loaded UMD path mismatch.'}
    $result.Completed=$summaries.Count -eq 1
    $result.Summary=$summaries
    $result.Passed=$result.Completed -and $process.ExitCode -eq 0 -and $summaries[0] -match ',failures=0$'
    Copy-Item "C:\ProgramData\Helios\umd12-$($process.Id).log" $out
} catch {$result.Error=$_.Exception.Message;$result.Passed=$false;$result.ExitCode=1}
finally {if($process) {$process.Dispose()};foreach ($lock in $locks) {$lock.Dispose()}}
$result.EndUTC=[DateTime]::UtcNow.ToString('o')
$result | ConvertTo-Json -Depth 6 | Set-Content (Join-Path $out 'result.json') -Encoding UTF8
New-Item -ItemType Directory -Force $ArchiveRoot | Out-Null
$archive=Join-Path $ArchiveRoot (Split-Path $out -Leaf)
Copy-Item $out $archive -Recurse
foreach ($file in Get-ChildItem $out -File) {
    if ((Get-FileHash $file.FullName).Hash -ine (Get-FileHash (Join-Path $archive $file.Name)).Hash) {throw 'Archive differs.'}
}
Write-Output "$archive; completed=$($result.Completed); passed=$($result.Passed)"
if (!$result.Passed) {exit 1}
