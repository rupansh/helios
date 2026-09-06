<#
.SYNOPSIS
Build or run the native runtime synchronization probe. Building never runs it.
.DESCRIPTION
Run can diagnose the installed stack. Acceptance of a replacement requires its
reviewed, matching driver set to be deployed. The Run mode requires an
interactive Windows session. See docs/dx12/EXECUTION_SYNC.md
for scheduled-task invocation and the additional visual acceptance obligations.
#>
[CmdletBinding()]
param(
    [ValidateSet('Build', 'Run')][string] $Mode = 'Build',
    [string] $BuildDir = 'C:\Users\Rupansh\helios-d3d12-sync',
    [string] $ResultDir = 'Z:\tmp\dx12-sync-acceptance',
    [ValidateRange(0, 4)][int] $Case = 0
)
$ErrorActionPreference = 'Stop'
$source = Join-Path $PSScriptRoot 'd3d12_sync_probe.cpp'
$exe = Join-Path $BuildDir 'd3d12_sync_probe.exe'
if ($Mode -eq 'Build') {
    if ([IO.Path]::GetPathRoot($BuildDir) -ne 'C:\') {
        throw 'BuildDir must be on local C: disk.'
    }
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    $vs = & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
    if (-not $vs) { throw 'MSVC C++ tools not found.' }
    $vcvars = Join-Path $vs 'VC\Auxiliary\Build\vcvars64.bat'
    New-Item -ItemType Directory -Force -Path $BuildDir | Out-Null
    Copy-Item $source (Join-Path $BuildDir 'd3d12_sync_probe.cpp') -Force
    $batch = Join-Path $BuildDir 'build.cmd'
    @"
@echo off
call "$vcvars"
if errorlevel 1 exit /b 1
cd /d "$BuildDir"
cl /nologo /W4 /WX /EHsc /std:c++17 /O2 d3d12_sync_probe.cpp /Fe:d3d12_sync_probe.exe /link d3d12.lib dxgi.lib
exit /b %errorlevel%
"@ | Set-Content -Encoding ASCII $batch
    & cmd.exe /d /c $batch
    if ($LASTEXITCODE -ne 0) { throw "Probe build failed: $LASTEXITCODE" }
    Get-FileHash -Algorithm SHA256 $source, $exe
    exit 0
}

if ([Diagnostics.Process]::GetCurrentProcess().SessionId -eq 0) {
    throw 'Run requires an interactive scheduled task; session 0 is not acceptance.'
}
if (-not (Test-Path $exe)) { throw "Build first: $exe" }
foreach ($name in @('d3d12.dll', 'd3d12core.dll', 'dxgi.dll', 'helios_vkd3d.dll')) {
    if (Test-Path (Join-Path $BuildDir $name)) { throw "App-local engine override is not native UMD acceptance: $name" }
}
$stamp = Get-Date -Format 'yyyyMMdd-HHmmss-fff'
$output = Join-Path $ResultDir $stamp
New-Item -ItemType Directory -Force -Path $output | Out-Null
$env:HELIOS_WSI_ASYNC_PRESENT = '1'
$record = [ordered]@{
    started = (Get-Date).ToString('o')
    session = [Diagnostics.Process]::GetCurrentProcess().SessionId
    executable = $exe
    sha256 = (Get-FileHash $exe -Algorithm SHA256).Hash
    source_sha256 = (Get-FileHash (Join-Path $BuildDir 'd3d12_sync_probe.cpp') -Algorithm SHA256).Hash
    case = $Case
    passed = $false
}
$process = New-Object Diagnostics.Process
$process.StartInfo.FileName = $exe
$process.StartInfo.WorkingDirectory = $BuildDir
$process.StartInfo.UseShellExecute = $false
$process.StartInfo.CreateNoWindow = $true
$process.StartInfo.RedirectStandardOutput = $true
$process.StartInfo.RedirectStandardError = $true
if ($Case) { $process.StartInfo.Arguments = "--case $Case" }
try {
    # Own the native process handle from creation to ExitCode. Start-Process
    # -PassThru can lose the handle for a fast-exiting child and return null.
    if (-not $process.Start()) { throw 'Probe process did not start.' }
    $record['pid'] = $process.Id
    # Drain both pipes concurrently so a full diagnostic log cannot deadlock.
    $stdout = $process.StandardOutput.ReadToEndAsync()
    $stderr = $process.StandardError.ReadToEndAsync()
    if (-not $process.WaitForExit(90000)) {
        & taskkill.exe /PID $process.Id /T /F | Out-Null
        $record['failure'] = 'Probe exceeded 90 seconds; no completion accepted.'
        if (-not $process.WaitForExit(5000)) { throw 'Probe tree did not exit after termination.' }
    }
    $record['exit_code'] = $process.ExitCode
    $log = $stdout.GetAwaiter().GetResult()
    $log | Set-Content -Encoding UTF8 (Join-Path $output 'stdout.txt')
    $stderr.GetAwaiter().GetResult() | Set-Content -Encoding UTF8 (Join-Path $output 'stderr.txt')
    $marker = if ($Case) { "PASS selected native runtime synchronization case $Case" } else { 'PASS all native runtime synchronization cases' }
    $record['passed'] = -not $record.Contains('failure') -and $process.ExitCode -eq 0 -and $log.Contains($marker)
} catch {
    $record['failure'] = $_.Exception.Message
} finally {
    $process.Dispose()
    $record['finished'] = (Get-Date).ToString('o')
    $record | ConvertTo-Json | Set-Content -Encoding UTF8 (Join-Path $output 'result.json')
}
Write-Output "Result: $output"
if (-not $record['passed']) { exit 1 }
