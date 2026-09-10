# Build locally first; schedule only after the integrated driver/harness review.
# Schedule and Run require the full SHA256 of the intended native UMD12 DLL.
[CmdletBinding()]
param(
    [ValidateSet('Build','Schedule','Run')][string]$Mode = 'Build',
    [string]$BuildDir = 'C:\ProgramData\Helios\indirect-probe',
    [string]$ArchiveRoot = 'Z:\tmp\fl12-indirect',
    [string]$InteractiveUser = 'Rupansh',
    [string]$ExpectedUmd12SHA256 = '',
    [switch]$MeasurePerformance,
    [switch]$InputAssembler
)
$executedRunnerSource = $MyInvocation.MyCommand.ScriptBlock.Ast.Extent.Text
$ErrorActionPreference = 'Stop'
if ($InputAssembler -and $MeasurePerformance) { throw 'IA performance mode is not implemented in this probe.' }
$caseKind = if ($InputAssembler) { 'ia' } else { 'roots' }
if ($BuildDir -notmatch '^[Cc]:\\') { throw 'BuildDir must be on local C: disk.' }
foreach ($path in @($BuildDir, $ArchiveRoot)) {
    if ($path -match '["\r\n]') { throw 'Invalid path for scheduled-task arguments.' }
}
$BuildDir = [IO.Path]::GetFullPath($BuildDir).TrimEnd('\')
$ArchiveRoot = [IO.Path]::GetFullPath($ArchiveRoot).TrimEnd('\')
if ($BuildDir.Length -le 2 -or $ArchiveRoot.Length -le 2) { throw 'Use a subdirectory, not a drive root.' }
$sourceRoot = Split-Path $PSScriptRoot -Parent
$localRunner = Join-Path $BuildDir 'd3d12-indirect-probe.ps1'
$provenancePath = Join-Path $BuildDir 'build-provenance.json'
$inputNames = @('d3d12_indirect_probe.cpp','d3d12_indirect_probe.hlsl',
    'd3d12_native_identity.h','d3d12-indirect-probe.ps1')
$artifactNames = @('indirect-ia-vs.dxil','indirect-ia-producer.dxil','indirect-ia-ps.dxil',
    'indirect-consumer-vs.dxil','indirect-producer.dxil','indirect-consumer-ps.dxil',
    'indirect-perf-vs.dxil','indirect-perf-producer.dxil','indirect-perf-ps.dxil',
    'd3d12_indirect_probe.exe','build.cmd','build.log')

function Get-Artifact([string]$Name) {
    $item = Get-Item -LiteralPath (Join-Path $BuildDir $Name)
    if ($item.PSIsContainer) { throw "Build input/artifact is not a file: $Name" }
    [ordered]@{ Name = $Name; Path = $item.FullName; Length = $item.Length
        SHA256 = (Get-FileHash -LiteralPath $item.FullName -Algorithm SHA256).Hash }
}
function Assert-Build {
    $build = Get-Content -LiteralPath $provenancePath -Raw | ConvertFrom-Json
    if ($build.Schema -ne 1 -or $build.Probe -ne 'native-indirect') { throw 'Invalid indirect build receipt.' }
    foreach ($category in @('Inputs','Artifacts')) {
        $names = if ($category -eq 'Inputs') { $inputNames } else { $artifactNames }
        $entries = @($build.$category)
        if ($entries.Count -ne $names.Count) { throw "Incomplete or extra $category in indirect build receipt." }
        foreach ($name in $names) {
            $records = @($entries | Where-Object { $_.Name -ceq $name })
            if ($records.Count -ne 1 -or $records[0].SHA256 -notmatch '\A[0-9a-fA-F]{64}\z') {
                throw "Missing, duplicate or invalid build record: $name"
            }
            $actual = Get-Artifact $name
            $record = $records[0]
            if ($actual.Path -ine $record.Path -or $actual.Length -ne $record.Length -or $actual.SHA256 -ine $record.SHA256) {
                throw "Build input/artifact changed since provenance capture: $name"
            }
        }
    }
    $runner = @($build.Inputs | Where-Object { $_.Name -eq 'd3d12-indirect-probe.ps1' })[0]
    # PowerShell parses the script before its first statement. Hash and decode
    # the same local bytes, then compare with the already parsed source: a
    # concurrent Build must not attribute an old in-memory grader to a new file.
    $runnerBytes = [IO.File]::ReadAllBytes($localRunner)
    $sha = [Security.Cryptography.SHA256]::Create()
    try { $runnerHash = [BitConverter]::ToString($sha.ComputeHash($runnerBytes)).Replace('-', '') }
    finally { $sha.Dispose() }
    $runnerSource = [Text.Encoding]::UTF8.GetString($runnerBytes).TrimStart([char]0xfeff)
    if ($runnerHash -ine $runner.SHA256 -or
            ![string]::Equals($executedRunnerSource, $runnerSource, [StringComparison]::Ordinal)) {
        throw 'Parsed runner differs from the hash-verified build runner; rebuild before scheduling.'
    }
    return $build
}
function Assert-NoOverrides {
    foreach ($name in @('VKD3D_FEATURE_LEVEL','VKD3D_SHADER_MODEL','VKD3D_SHADER_OVERRIDE',
            'D3D12SDKPath','D3D12SDKVersion')) {
        if ([Environment]::GetEnvironmentVariable($name)) { throw "Capability/runtime/shader override $name is forbidden." }
    }
    foreach ($name in @('d3d12.dll','D3D12Core.dll','dxgi.dll','helios_vkd3d.dll','d3d10warp.dll')) {
        if (Test-Path -LiteralPath (Join-Path $BuildDir $name)) { throw "App-local substitution found: $name" }
    }
}
function Get-NativeModules([string]$Text) {
    $modules = @($Text -split "`r?`n" | Where-Object { $_ -like 'MODULE,*' } | ForEach-Object {
        $parts = $_ -split ',',3
        if ($parts.Count -ne 3 -or ![IO.Path]::IsPathRooted($parts[2]) -or
                [IO.Path]::GetFileName($parts[2]) -ine $parts[1]) { throw "Malformed module evidence: $_" }
        $item = Get-Item -LiteralPath $parts[2]
        [ordered]@{ Name = $parts[1]; Path = $item.FullName; Length = $item.Length
            SHA256 = (Get-FileHash -LiteralPath $item.FullName -Algorithm SHA256).Hash
            FileVersion = $item.VersionInfo.FileVersion; ProductVersion = $item.VersionInfo.ProductVersion }
    })
    foreach ($name in @('d3d12.dll','D3D12Core.dll','dxgi.dll')) {
        $native = @($modules | Where-Object { $_.Name -ieq $name })
        if ($native.Count -ne 1 -or $native[0].Path -ine (Join-Path ([Environment]::SystemDirectory) $name)) {
            throw "Missing or substituted system runtime module: $name"
        }
    }
    $umd = @($modules | Where-Object { $_.Name -match '\Ahelios_umd12(?:_[0-9a-f]{16})?\.dll\z' })
    if ($umd.Count -ne 1 -or $umd[0].SHA256 -ine $ExpectedUmd12SHA256) {
        throw 'Exactly one logged Helios UMD12 with the expected full SHA256 is required.'
    }
    if ($umd[0].Name -match '\Ahelios_umd12_([0-9a-f]{16})\.dll\z') {
        if ($Matches[1] -ine $umd[0].SHA256.Substring(0,16)) { throw 'Hotplug UMD12 filename hash prefix does not match its bytes.' }
    }
    if (!@($modules | Where-Object { $_.Name -like 'vulkan_virtio*' }).Count) { throw 'Loaded Venus ICD evidence is missing.' }
    return $modules
}

function Assert-Measurement([string]$Text, [string]$WitnessPath) {
    $lines = @($Text -split "`r?`n")
    if (@($lines | Where-Object { $_ -ceq 'PASS,indirect-measurement,36 samples,36900 words' }).Count -ne 1) {
        throw 'Missing or duplicate measurement completion marker.'
    }
    $config = @($lines | Where-Object { $_.StartsWith('PERF_CONFIG,') })
    if ($config.Count -ne 1 -or $config[0] -notmatch '\APERF_CONFIG,max_commands=1024,width=1025,warm_samples=5,cpu_frequency=(\d+),gpu_frequency=(\d+)\z') {
        throw 'Invalid measurement configuration.'
    }
    $cpuFrequency = [UInt64]::Parse($Matches[1]); $gpuFrequency = [UInt64]::Parse($Matches[2])
    if (!$cpuFrequency -or !$gpuFrequency) { throw 'Zero measurement clock frequency.' }
    $samples = @($lines | Where-Object { $_.StartsWith('PERF,') })
    if ($samples.Count -ne 36) { throw 'Expected 36 measurement samples, including 30 warm samples.' }
    $witness = [IO.File]::ReadAllBytes($WitnessPath)
    if ($witness.Length -ne 36 * 1025 * 4) { throw 'Incomplete or extra pixel witness data.' }
    $ordinal = 0
    foreach ($count in @(1024,1,0)) { foreach ($arm in @('direct','indirect')) { foreach ($sample in -1..4) {
        $phase = if ($sample -ge 0) { 'warm' } elseif ($count -eq 1024) { 'first-use' } else { 'warmup' }
        $seed = 1009 + ($sample + 1) * 17
        $prefix = "PERF,ordinal=$ordinal,arm=$arm,count=$count,sample=$sample,phase=$phase,seed=$seed,"
        if (!$samples[$ordinal].StartsWith($prefix, [StringComparison]::Ordinal)) {
            throw "Missing, repeated or reordered measurement sample $ordinal."
        }
        $tail = $samples[$ordinal].Substring($prefix.Length)
        if ($tail -notmatch '\Arecord_ticks=(\d+),close_ticks=(\d+),submit_signal_ticks=(\d+),completion_wait_ticks=(\d+),gpu_begin=(\d+),gpu_end=(\d+),fence=(\d+)\z') {
            throw "Malformed measurement timing $ordinal."
        }
        $gpuBegin = [UInt64]::Parse($Matches[5]); $gpuEnd = [UInt64]::Parse($Matches[6])
        $fence = [UInt64]::Parse($Matches[7])
        if ($gpuEnd -le $gpuBegin -or $fence -ne $ordinal + 1) { throw "Invalid timestamp/fence for sample $ordinal." }
        foreach ($field in 1..4) { $null = [UInt64]::Parse($Matches[$field]) }
        for ($pixel = 0; $pixel -lt 1025; ++$pixel) {
            $value = [BitConverter]::ToUInt32($witness, ($ordinal * 1025 + $pixel) * 4)
            # Algebraic reference independent of the shader's separate VS/PS
            # fields: all inactive pixels stay zero, including count=0.
            $expected = if ($pixel -lt $count -or $pixel -eq 1024) { 23 * $seed + 41 * $pixel } else { 0 }
            if ($value -ne $expected) { throw "GPU pixel mismatch: sample=$ordinal pixel=$pixel actual=$value expected=$expected" }
        }
        ++$ordinal
    } } }
    return [ordered]@{ Samples = 36; WarmSamples = 30; ReadbackWords = 36900
        CPUFrequency = $cpuFrequency; GPUFrequency = $gpuFrequency
        ReadbackSHA256 = (Get-FileHash -LiteralPath $WitnessPath -Algorithm SHA256).Hash }
}

if ($Mode -eq 'Build') {
    New-Item -ItemType Directory -Force $BuildDir | Out-Null
    # Invalidate the previous success receipt before overwriting any input or
    # artifact. A compiler failure must leave no receipt authorizing mixed files.
    if (Test-Path -LiteralPath $provenancePath) { Remove-Item -LiteralPath $provenancePath -Force }
    $buildLocks = @()
    try {
        foreach ($name in $inputNames) {
            $source = Join-Path $PSScriptRoot $name
            $destination = Join-Path $BuildDir $name
            if ([IO.Path]::GetFullPath($source) -ine [IO.Path]::GetFullPath($destination)) {
                Copy-Item -LiteralPath $source -Destination $destination -Force
            }
            $buildLocks += [IO.File]::Open($destination, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
        }
        $vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
        $vs = & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
        if (!$vs) { throw 'MSVC installation not found.' }
        $dxcCommand = Get-Command dxc.exe -ErrorAction SilentlyContinue
        $dxc = if ($dxcCommand) { $dxcCommand.Source } else { $null }
        if (!$dxc -and $env:VULKAN_SDK) {
            $candidate = Join-Path $env:VULKAN_SDK 'Bin\dxc.exe'
            if (Test-Path -LiteralPath $candidate) { $dxc = $candidate }
        }
        if (!$dxc) { throw 'DXC was not found in PATH or VULKAN_SDK\Bin.' }
        $dxcVersion = & $dxc --version | Out-String
        if ($LASTEXITCODE) { throw "DXC version query failed: $LASTEXITCODE" }
        Push-Location $BuildDir
        try {
            & $dxc -T vs_6_0 -E vertex_main -Fo indirect-consumer-vs.dxil d3d12_indirect_probe.hlsl
            if ($LASTEXITCODE) { throw "DXC VS failed: $LASTEXITCODE" }
            & $dxc -T cs_6_0 -D PRODUCER=1 -E producer_main -Fo indirect-producer.dxil d3d12_indirect_probe.hlsl
            if ($LASTEXITCODE) { throw "DXC CS failed: $LASTEXITCODE" }
            & $dxc -T ps_6_0 -E pixel_main -Fo indirect-consumer-ps.dxil d3d12_indirect_probe.hlsl
            if ($LASTEXITCODE) { throw "DXC PS failed: $LASTEXITCODE" }
            & $dxc -T vs_6_0 -D MEASURE=1 -E vertex_main -Fo indirect-perf-vs.dxil d3d12_indirect_probe.hlsl
            if ($LASTEXITCODE) { throw "DXC perf VS failed: $LASTEXITCODE" }
            & $dxc -T cs_6_0 -D MEASURE=1 -D PRODUCER=1 -E producer_main -Fo indirect-perf-producer.dxil d3d12_indirect_probe.hlsl
            if ($LASTEXITCODE) { throw "DXC perf CS failed: $LASTEXITCODE" }
            & $dxc -T ps_6_0 -D MEASURE=1 -E pixel_main -Fo indirect-perf-ps.dxil d3d12_indirect_probe.hlsl
            if ($LASTEXITCODE) { throw "DXC perf PS failed: $LASTEXITCODE" }
            & $dxc -T vs_6_0 -D INPUT_ASSEMBLER=1 -E vertex_main -Fo indirect-ia-vs.dxil d3d12_indirect_probe.hlsl
            if ($LASTEXITCODE) { throw "DXC IA VS failed: $LASTEXITCODE" }
            & $dxc -T cs_6_0 -D INPUT_ASSEMBLER=1 -D PRODUCER=1 -E producer_main -Fo indirect-ia-producer.dxil d3d12_indirect_probe.hlsl
            if ($LASTEXITCODE) { throw "DXC IA CS failed: $LASTEXITCODE" }
            & $dxc -T ps_6_0 -D INPUT_ASSEMBLER=1 -E pixel_main -Fo indirect-ia-ps.dxil d3d12_indirect_probe.hlsl
            if ($LASTEXITCODE) { throw "DXC IA PS failed: $LASTEXITCODE" }
            $buildCommand = @"
@echo off
call "$vs\VC\Auxiliary\Build\vcvars64.bat"
if errorlevel 1 exit /b %errorlevel%
where cl
set WindowsSDKVersion
set VCToolsVersion
cl /nologo /Bv /W4 /EHsc /std:c++17 /O2 d3d12_indirect_probe.cpp /Fe:d3d12_indirect_probe.exe /link d3d12.lib dxgi.lib 2>&1
exit /b %errorlevel%
"@
            [IO.File]::WriteAllText((Join-Path $BuildDir 'build.cmd'), $buildCommand, [Text.Encoding]::ASCII)
            & cmd.exe /d /c (Join-Path $BuildDir 'build.cmd') *> (Join-Path $BuildDir 'build.log')
            if ($LASTEXITCODE) { Get-Content (Join-Path $BuildDir 'build.log'); throw "MSVC failed: $LASTEXITCODE" }
        } finally { Pop-Location }
        $provenance = [ordered]@{ Schema = 1; Probe = 'native-indirect'; UTC = [DateTime]::UtcNow.ToString('o')
            SourceRoot = $sourceRoot; LocalRunner = $localRunner; VisualStudio = $vs; BuildCommand = $buildCommand
            DxcPath = $dxc; DxcVersion = $dxcVersion; DxcHash = (Get-FileHash -LiteralPath $dxc -Algorithm SHA256).Hash
            Inputs = @($inputNames | ForEach-Object { Get-Artifact $_ })
            Artifacts = @($artifactNames | ForEach-Object { Get-Artifact $_ })
            Scope = 'Probe inputs and artifacts only; this receipt does not identify the deployed driver source.' }
        $pendingReceipt = Join-Path $BuildDir 'build-provenance.pending.json'
        $provenance | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $pendingReceipt -Encoding UTF8
        Move-Item -LiteralPath $pendingReceipt -Destination $provenancePath -Force
        $null = Assert-Build
    } catch {
        if (Test-Path -LiteralPath $provenancePath) { Remove-Item -LiteralPath $provenancePath -Force }
        throw
    } finally {
        foreach ($handle in $buildLocks) { $handle.Dispose() }
    }
    Write-Output "Built native indirect probe at $BuildDir"
    exit 0
}
if ($ExpectedUmd12SHA256 -notmatch '\A[0-9a-fA-F]{64}\z') { throw 'Schedule and Run require ExpectedUmd12SHA256 (64 hexadecimal digits).' }
$ExpectedUmd12SHA256 = $ExpectedUmd12SHA256.ToUpperInvariant()
if ($Mode -eq 'Schedule') {
    $null = Assert-Build
    Assert-NoOverrides
    $taskName = 'helios_indirect_acceptance'
    $existing = Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
    if ($existing -and $existing.State -eq 'Running') { throw "$taskName is already running." }
    $arguments = "-NoProfile -ExecutionPolicy Bypass -File `"$localRunner`" -Mode Run -BuildDir `"$BuildDir`" -ArchiveRoot `"$ArchiveRoot`" -ExpectedUmd12SHA256 $ExpectedUmd12SHA256"
    if ($MeasurePerformance) { $arguments += ' -MeasurePerformance' }
    if ($InputAssembler) { $arguments += ' -InputAssembler' }
    $action = New-ScheduledTaskAction -Execute "$env:SystemRoot\System32\WindowsPowerShell\v1.0\powershell.exe" -Argument $arguments -WorkingDirectory $BuildDir
    $principal = New-ScheduledTaskPrincipal -UserId $InteractiveUser -LogonType Interactive -RunLevel Highest
    $settings = New-ScheduledTaskSettingsSet -ExecutionTimeLimit ([TimeSpan]::Zero) -MultipleInstances IgnoreNew
    Register-ScheduledTask -TaskName $taskName -Action $action -Principal $principal -Settings $settings -Force | Out-Null
    Start-ScheduledTask -TaskName $taskName
    Write-Output "Started interactive $taskName. Inspect its fresh archived result.json; task start is not acceptance."
    exit 0
}
if ((Get-Process -Id $PID).SessionId -eq 0) { throw 'Run requires an interactive scheduled task.' }
$runName = 'run-' + (Get-Date -Format 'yyyyMMdd-HHmmss-fff') + '-' + [Guid]::NewGuid().ToString('N').Substring(0,8)
$runDir = Join-Path $BuildDir $runName
$archiveDir = Join-Path $ArchiveRoot $runName
New-Item -ItemType Directory $runDir | Out-Null
$env:HELIOS_WSI_ASYNC_PRESENT = '1'
Remove-Item Env:\HELIOS_RETIRE_FEEDBACK -ErrorAction SilentlyContinue
$env:VKD3D_SHADER_CACHE_PATH = '0'
$result = [ordered]@{ UTC = [DateTime]::UtcNow.ToString('o'); Session = (Get-Process -Id $PID).SessionId
    RunnerProcessId = $PID; ProcessId = 0; Completed = $false; ExitCode = 1; Errors = @()
    ExpectedUmd12SHA256 = $ExpectedUmd12SHA256; ArchivePath = $archiveDir
    MeasurePerformance = [bool]$MeasurePerformance
    Environment = [ordered]@{ HELIOS_WSI_ASYNC_PRESENT = $env:HELIOS_WSI_ASYNC_PRESENT
        VKD3D_SHADER_CACHE_PATH = $env:VKD3D_SHADER_CACHE_PATH
        VKD3D_FEATURE_LEVEL = $env:VKD3D_FEATURE_LEVEL; VKD3D_SHADER_MODEL = $env:VKD3D_SHADER_MODEL
        VKD3D_SHADER_OVERRIDE = $env:VKD3D_SHADER_OVERRIDE }
    Scope = 'Bounded native ExecuteIndirect root behavior; no feature-level, benchmark, visual or performance acceptance.' }
if ($MeasurePerformance) {
    $result.Scope = 'Equivalent-output direct/indirect overhead measurement, including all first-use/warmup samples and pixel witnesses; no optimization gain, general throughput or feature-level claim.'
}
if ($InputAssembler) { $result.Scope = 'Native GPU-produced root/VBV/IBV/indexed draw, counts, predication, barriers and closed-list replay; no feature-level or benchmark acceptance.' }
function Set-RunFailure([string]$Message) {
    $result.Completed = $false; $result.ExitCode = 1
    $result.Errors += $Message
    $result.Error = $result.Errors -join ' | '
}
$stdout = $null; $stderr = $null; $process = $null; $buildLocks = @()
try {
    Assert-NoOverrides
    # Hold the exact files read-only until execution and evidence capture end.
    # A simultaneous rebuild cannot invalidate its old receipt or overwrite any
    # of these inputs after verification but before the child consumes them.
    foreach ($name in @($inputNames + $artifactNames + 'build-provenance.json')) {
        $buildLocks += [IO.File]::Open((Join-Path $BuildDir $name), [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
    }
    $buildProvenance = Assert-Build
    $result.LocalRunner = Get-Artifact 'd3d12-indirect-probe.ps1'
    $result.BuildReceiptSHA256 = (Get-FileHash -LiteralPath $provenancePath -Algorithm SHA256).Hash
    $buildEvidence = Join-Path $runDir 'build'
    New-Item -ItemType Directory $buildEvidence | Out-Null
    foreach ($entry in @($buildProvenance.Inputs) + @($buildProvenance.Artifacts)) {
        $copy = Join-Path $buildEvidence $entry.Name
        Copy-Item -LiteralPath (Join-Path $BuildDir $entry.Name) -Destination $copy
        if ((Get-FileHash -LiteralPath $copy -Algorithm SHA256).Hash -ine $entry.SHA256) { throw "Build evidence copy hash mismatch: $($entry.Name)" }
    }
    Copy-Item -LiteralPath $provenancePath -Destination $buildEvidence
    if ((Get-FileHash -LiteralPath (Join-Path $buildEvidence 'build-provenance.json') -Algorithm SHA256).Hash -ine $result.BuildReceiptSHA256) {
        throw 'Build receipt evidence copy hash mismatch.'
    }
    $info = New-Object System.Diagnostics.ProcessStartInfo
    $info.FileName = Join-Path $BuildDir 'd3d12_indirect_probe.exe'
    $info.WorkingDirectory = $BuildDir
    if ($MeasurePerformance) { $info.Arguments = '--measure "' + (Join-Path $runDir 'performance-readbacks.bin') + '"' }
    if ($InputAssembler) { $info.Arguments = '--ia' }
    $result.Arguments = $info.Arguments
    $info.UseShellExecute = $false
    $info.RedirectStandardOutput = $true; $info.RedirectStandardError = $true
    $process = New-Object System.Diagnostics.Process
    $process.StartInfo = $info
    if (!$process.Start()) { throw 'Process start failed.' }
    $result.ProcessId = $process.Id
    $stdout = $process.StandardOutput.ReadToEndAsync(); $stderr = $process.StandardError.ReadToEndAsync()
    if (!$process.WaitForExit(540000)) {
        $result.TimedOut = $true
        throw 'Probe timed out; process termination requested, never retried.'
    }
    if (!$stdout.Wait(5000) -or !$stderr.Wait(5000)) { throw 'Child output capture timed out.' }
    $outText = $stdout.GetAwaiter().GetResult(); $errText = $stderr.GetAwaiter().GetResult()
    $result.ChildExitCode = $process.ExitCode
    $result.Modules = @(Get-NativeModules $errText)
    if ($MeasurePerformance) {
        $result.Measurement = Assert-Measurement $outText (Join-Path $runDir 'performance-readbacks.bin')
        if ($process.ExitCode -ne 0) { throw 'Native indirect measurement failed; inspect archived output.' }
    } else {
    if ($process.ExitCode -ne 0 -or !$outText.Contains("PASS,indirect-$caseKind,12 cases,48 words") -or
            !$outText.Contains('PASS,indirect-invalid-extent,close=80070057,reset=80070057')) {
        throw 'Native indirect acceptance failed; inspect archived output.'
    }
    $counts = @(3,1,0,7)
    if ($InputAssembler) {
        $lifetime = @($outText -split "`r?`n" | Where-Object { $_.StartsWith('LIFETIME,') })
        if ($lifetime.Count -ne 1 -or $lifetime[0] -cne
                'LIFETIME,negative_wait=258,completed_before=11,expected=12,list_reset=00000000,other_queue=1') {
            throw 'Missing or incorrect IA pending-list Reset / queue dependency witness.'
        }
    }
    foreach ($route in 0..2) { foreach ($replay in 0..3) {
        $line = "PASS,indirect-$caseKind,route=$route,replay=$replay,count=$($counts[$replay])"
        if (@($outText -split "`r?`n" | Where-Object { $_ -ceq $line }).Count -ne 1) {
            throw "Missing or repeated GPU readback case: $line"
        }
        if ($InputAssembler) {
            $query = @($outText -split "`r?`n" | Where-Object { $_.StartsWith("QUERY,route=$route,replay=$replay,") })
            if ($query.Count -ne 1 -or $query[0] -notmatch ',ia_vertices=(\d+),ia_primitives=(\d+),cs=(\d+),ps=(\d+)\z') {
                throw "Missing, duplicate or malformed IA query witness: route=$route replay=$replay"
            }
            $primitives = 1 + [Math]::Min($counts[$replay], 3)
            if ([UInt64]$Matches[1] -ne 3 * $primitives -or [UInt64]$Matches[2] -ne $primitives -or
                    [UInt64]$Matches[3] -ne 0 -or [UInt64]$Matches[4] -eq 0) {
                throw "Incorrect IA query continuation: $($query[0])"
            }
        }
    } }
    }
    $null = Assert-Build
} catch {
    Set-RunFailure $_.Exception.Message
} finally {
    # Terminate a failed/timed-out child without C++ unwinding. If termination
    # cannot be confirmed, record that fact and never block evidence collection
    # waiting indefinitely for its output pipes.
    if ($process -and $result.ProcessId -ne 0) {
        try {
            if (!$process.HasExited) {
                $process.Kill()
                if (!$process.WaitForExit(5000)) { throw 'Child termination could not be confirmed.' }
            }
            $result.ChildExitCode = $process.ExitCode
        } catch {
            $result.ChildMayStillBeRunning = $true
            Set-RunFailure $_.Exception.Message
        }
        foreach ($capture in @(@('stdout.txt',$stdout), @('stderr.txt',$stderr))) {
            try {
                if (!$capture[1] -or !$capture[1].Wait(5000)) { throw "Output capture unavailable: $($capture[0])" }
                [IO.File]::WriteAllText((Join-Path $runDir $capture[0]), $capture[1].GetAwaiter().GetResult())
            } catch { Set-RunFailure $_.Exception.Message }
        }
        foreach ($suffix in @('.log', '-vkd3d.log')) {
            try {
                $log = "C:\ProgramData\Helios\umd12-$($result.ProcessId)$suffix"
                Copy-Item -LiteralPath $log -Destination $runDir
            } catch { Set-RunFailure "Required per-PID log archive failed: $($_.Exception.Message)" }
        }
    }
    foreach ($handle in $buildLocks) { $handle.Dispose() }
    if ($process) { $process.Dispose() }
}
# Completion stays false while mandatory evidence is being archived. Archive
# copy/hash/final-result errors clear completion and return nonzero even if the
# child produced every GPU witness. Best-effort failed receipts are retained at
# both locations when an evidence filesystem itself has failed.
$archiveOwned = $false
try {
    $result.Evidence = @(Get-ChildItem -LiteralPath $runDir -File -Recurse | ForEach-Object {
        [ordered]@{ Path = $_.FullName.Substring($runDir.Length + 1); Length = $_.Length
            SHA256 = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash }
    })
    $result | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath (Join-Path $runDir 'result.json') -Encoding UTF8
    New-Item -ItemType Directory -Force $ArchiveRoot | Out-Null
    if (Test-Path -LiteralPath $archiveDir) { throw "Archive destination already exists: $archiveDir" }
    New-Item -ItemType Directory $archiveDir | Out-Null
    $archiveOwned = $true
    foreach ($item in Get-ChildItem -LiteralPath $runDir) {
        Copy-Item -LiteralPath $item.FullName -Destination $archiveDir -Recurse
    }
    foreach ($entry in $result.Evidence) {
        $archived = Get-Item -LiteralPath (Join-Path $archiveDir $entry.Path)
        if ($archived.Length -ne $entry.Length -or
                (Get-FileHash -LiteralPath $archived.FullName -Algorithm SHA256).Hash -ine $entry.SHA256) {
            throw "Archive evidence hash/size mismatch: $($entry.Path)"
        }
    }
    if ($result.Errors.Count -eq 0) { $result.Completed = $true; $result.ExitCode = 0 }
    $localResult = Join-Path $runDir 'result.json'
    $archivedResult = Join-Path $archiveDir 'result.json'
    $result | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath $localResult -Encoding UTF8
    Copy-Item -LiteralPath $localResult -Destination $archivedResult -Force
    if ((Get-FileHash -LiteralPath $localResult -Algorithm SHA256).Hash -ine
            (Get-FileHash -LiteralPath $archivedResult -Algorithm SHA256).Hash) { throw 'Archived final result hash mismatch.' }
} catch {
    Set-RunFailure "Mandatory evidence/archive failed: $($_.Exception.Message)"
    $failedReceiptDirectories = @($runDir)
    if ($archiveOwned) { $failedReceiptDirectories += $archiveDir }
    foreach ($directory in $failedReceiptDirectories) {
        try {
            if (Test-Path -LiteralPath $directory -PathType Container) {
                $result | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath (Join-Path $directory 'result.json') -Encoding UTF8
            }
        } catch { Write-Warning "Failed to persist failed result in $directory : $($_.Exception.Message)" }
    }
}
if (!$result.Completed) { throw $result.Error }
Write-Output "PASS: $archiveDir"
