[CmdletBinding()]
param(
    [ValidateSet('Build','Schedule','Run')][string]$Mode = 'Build',
    [string]$BuildDir = 'C:\ProgramData\Helios\raytracing-probe',
    [string]$ArchiveRoot = 'Z:\tmp\fl12-raytracing',
    [string]$InteractiveUser = 'Rupansh',
    [string]$ExpectedUmd12SHA256 = '',
    [switch]$WithoutRaytracing
)
$executedRunnerSource = $MyInvocation.MyCommand.ScriptBlock.Ast.Extent.Text
$ErrorActionPreference = 'Stop'
if ($BuildDir -notmatch '^[Cc]:\\') { throw 'BuildDir must be on local C: disk.' }
$BuildDir = [IO.Path]::GetFullPath($BuildDir).TrimEnd('\')
$localRunner = Join-Path $BuildDir 'd3d12-raytracing-probe.ps1'
$receiptPath = Join-Path $BuildDir 'build-provenance.json'
$sourceNames = @('d3d12_raytracing_probe.cpp','d3d12_raytracing_probe.hlsl',
                 'd3d12_native_identity.h','d3d12-raytracing-probe.ps1')
$artifactNames = $sourceNames + @('d3d12_raytracing_probe.exe','raytracing.dxil','build.cmd','build.log','dxc.log','cl-path.txt')

function Get-Artifact([string]$Path) {
    $item = Get-Item -LiteralPath $Path -ErrorAction Stop
    [ordered]@{
        Name = $item.Name; Path = $item.FullName; Length = $item.Length
        SHA256 = (Get-FileHash -LiteralPath $item.FullName -Algorithm SHA256).Hash
        FileVersion = $item.VersionInfo.FileVersion; ProductVersion = $item.VersionInfo.ProductVersion
    }
}
function Assert-ExpectedDriver {
    if ($ExpectedUmd12SHA256 -notmatch '^[0-9a-fA-F]{64}$') {
        throw 'Schedule and Run require ExpectedUmd12SHA256: the full SHA256 of the intended native UMD build.'
    }
}
function Assert-Build([ref]$VerifiedReceiptBytes) {
    # Capture the exact receipt once. A later Build may replace the pathname
    # after the verified executable/shader have been copied into this run.
    $bytes = [IO.File]::ReadAllBytes($receiptPath)
    $receipt = [Text.Encoding]::UTF8.GetString($bytes).TrimStart([char]0xfeff) | ConvertFrom-Json
    if ($receipt.Schema -ne 1 -or !$receipt.BuildSucceeded -or @($receipt.Artifacts).Count -ne $artifactNames.Count) {
        throw 'Invalid or incomplete build receipt; run Build successfully first.'
    }
    foreach ($name in $artifactNames) {
        $entries = @($receipt.Artifacts | Where-Object { $_.Name -ceq $name })
        if ($entries.Count -ne 1) { throw "Build receipt must identify exactly one $name" }
        $entry = $entries[0]
        $path = Join-Path $BuildDir $name
        if ($entry.Path -ine $path -or $entry.SHA256 -notmatch '^[0-9a-fA-F]{64}$' -or
            (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash -ine $entry.SHA256) {
            throw "Build artifact changed since receipt capture: $name"
        }
    }
    $recordedRunner = @($receipt.Artifacts | Where-Object { $_.Name -ceq 'd3d12-raytracing-probe.ps1' })[0]
    # PowerShell parses the script before its first statement. Hash and decode
    # the same local bytes, then compare with the already parsed source: a
    # concurrent Build must not attribute an old in-memory grader to a new file.
    $runnerBytes = [IO.File]::ReadAllBytes($localRunner)
    $sha = [Security.Cryptography.SHA256]::Create()
    try { $runnerHash = [BitConverter]::ToString($sha.ComputeHash($runnerBytes)).Replace('-', '') }
    finally { $sha.Dispose() }
    $runnerSource = [Text.Encoding]::UTF8.GetString($runnerBytes).TrimStart([char]0xfeff)
    if ($runnerHash -ine $recordedRunner.SHA256 -or
            ![string]::Equals($executedRunnerSource, $runnerSource, [StringComparison]::Ordinal)) {
        throw 'Parsed runner differs from the hash-verified build runner; rebuild before scheduling.'
    }
    if ($null -ne $VerifiedReceiptBytes) { $VerifiedReceiptBytes.Value = $bytes }
    return $receipt
}
function Assert-NoOverrides {
    foreach ($name in @('VKD3D_FEATURE_LEVEL','VKD3D_SHADER_MODEL','VKD3D_SHADER_OVERRIDE','D3D12SDKPath','D3D12SDKVersion')) {
        if ([Environment]::GetEnvironmentVariable($name)) { throw "Capability/runtime/shader override $name is forbidden." }
    }
    foreach ($name in @('d3d12.dll','D3D12Core.dll','dxgi.dll','helios_vkd3d.dll','d3d10warp.dll')) {
        if (Test-Path -LiteralPath (Join-Path $BuildDir $name)) { throw "App-local substitution found: $name" }
    }
}

if ($Mode -eq 'Build') {
    # Invalidate the previous receipt before modifying any build input/output.
    # A failed rebuild must never leave permission to run a mixed artifact set.
    if (Test-Path -LiteralPath $receiptPath) { Remove-Item -LiteralPath $receiptPath -Force }
    New-Item -ItemType Directory -Force $BuildDir | Out-Null
    foreach ($name in $sourceNames) {
        $source = Join-Path $PSScriptRoot $name
        $destination = Join-Path $BuildDir $name
        if ([IO.Path]::GetFullPath($source) -ine $destination) {
            Copy-Item -LiteralPath $source -Destination $destination -Force
        }
    }
    $vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
    $vs = & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
    if (!$vs) { throw 'MSVC installation not found.' }
    $dxcCommand = Get-Command dxc.exe -ErrorAction SilentlyContinue
    $dxc = if ($dxcCommand) { $dxcCommand.Source } else { Join-Path $env:VULKAN_SDK 'Bin\dxc.exe' }
    if (!(Test-Path -LiteralPath $dxc)) { throw 'DXC not found in PATH or the installed Vulkan SDK.' }
    Push-Location $BuildDir
    try {
        & $dxc -T lib_6_3 -HV 2021 -Fo raytracing.dxil d3d12_raytracing_probe.hlsl *> dxc.log
        if ($LASTEXITCODE) { Get-Content dxc.log; throw "DXC library compilation failed: $LASTEXITCODE" }
        $build = "call `"$vs\VC\Auxiliary\Build\vcvars64.bat`"`r`nwhere cl > cl-path.txt`r`ncl /nologo /W4 /WX /EHsc /std:c++17 /O2 d3d12_raytracing_probe.cpp /Fe:d3d12_raytracing_probe.exe /link d3d12.lib dxgi.lib`r`nexit /b %errorlevel%`r`n"
        [IO.File]::WriteAllText("$BuildDir\build.cmd", $build, [Text.Encoding]::ASCII)
        & cmd.exe /d /c "$BuildDir\build.cmd" *> "$BuildDir\build.log"
        if ($LASTEXITCODE) { Get-Content "$BuildDir\build.log"; throw "MSVC failed: $LASTEXITCODE" }
        $cl = @(Get-Content -LiteralPath "$BuildDir\cl-path.txt")[0]
        $provenance = [ordered]@{
            Schema = 1; BuildSucceeded = $true; UTC = [DateTime]::UtcNow.ToString('o')
            SourceRoot = (Split-Path $PSScriptRoot -Parent); VisualStudio = $vs
            Dxc = (Get-Artifact $dxc); DxcVersion = (& $dxc --version | Out-String); Msvc = (Get-Artifact $cl)
            Artifacts = @($artifactNames | ForEach-Object { Get-Artifact (Join-Path $BuildDir $_) })
        }
        $provenance | ConvertTo-Json -Depth 8 | Set-Content "$receiptPath.new" -Encoding UTF8
        Move-Item -LiteralPath "$receiptPath.new" -Destination $receiptPath -Force
    } finally { Pop-Location }
    Write-Output "Built native DXR probe at $BuildDir"
    exit 0
}
if ($Mode -eq 'Schedule') {
    $previous = Get-ScheduledTask -TaskName 'helios_raytracing_acceptance' -ErrorAction SilentlyContinue
    if ($previous -and $previous.State -eq 'Running') { throw 'Previous DXR probe is still running; preserve its task and archive.' }
    Assert-ExpectedDriver
    $null = Assert-Build
    Assert-NoOverrides
    $arguments = "-NoProfile -ExecutionPolicy Bypass -File `"$localRunner`" -Mode Run -BuildDir `"$BuildDir`" -ArchiveRoot `"$ArchiveRoot`" -ExpectedUmd12SHA256 $ExpectedUmd12SHA256"
    if ($WithoutRaytracing) { $arguments += ' -WithoutRaytracing' }
    $action = New-ScheduledTaskAction -Execute 'powershell.exe' -Argument $arguments
    $principal = New-ScheduledTaskPrincipal -UserId $InteractiveUser -LogonType Interactive -RunLevel Highest
    $settings = New-ScheduledTaskSettingsSet -ExecutionTimeLimit (New-TimeSpan -Minutes 10)
    Register-ScheduledTask -TaskName 'helios_raytracing_acceptance' -Action $action -Principal $principal -Settings $settings -Force | Out-Null
    Start-ScheduledTask -TaskName 'helios_raytracing_acceptance'
    Write-Output 'Started interactive helios_raytracing_acceptance task.'
    exit 0
}

$runDir = Join-Path $BuildDir ('run-' + (Get-Date -Format 'yyyyMMdd-HHmmss-fff') + '-' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory $runDir | Out-Null
$env:HELIOS_WSI_ASYNC_PRESENT = '1'
Remove-Item Env:\HELIOS_RETIRE_FEEDBACK -ErrorAction SilentlyContinue
$env:VKD3D_SHADER_CACHE_PATH = '0'
# Restrict engine feature discovery, never force a feature level or shader model.
# This tests missing RT support on the current GPU, not another GPU model.
if ($WithoutRaytracing) {
    $env:VKD3D_DISABLE_EXTENSIONS = 'VK_KHR_ray_tracing_pipeline,VK_KHR_acceleration_structure,VK_KHR_ray_query'
} else { Remove-Item Env:\VKD3D_DISABLE_EXTENSIONS -ErrorAction SilentlyContinue }
$result = [ordered]@{
    UTC = [DateTime]::UtcNow.ToString('o'); Session = (Get-Process -Id $PID).SessionId
    Completed = $false; Blocked = $false; ExitCode = 1; ExpectedUmd12SHA256 = $ExpectedUmd12SHA256
    ProcessId = 0; Errors = @(); WithoutRaytracing = [bool]$WithoutRaytracing
    Environment = [ordered]@{ HELIOS_WSI_ASYNC_PRESENT = '1'; VKD3D_SHADER_CACHE_PATH = '0'; VKD3D_DISABLE_EXTENSIONS = $env:VKD3D_DISABLE_EXTENSIONS }
}
function Set-RunFailure([string]$Message) {
    $result.Completed = $false; $result.Blocked = $false; $result.ExitCode = 1
    $result.Errors += $Message
    $result.Error = $result.Errors -join ' | '
}
$process = $null; $stdout = $null; $stderr = $null; $workloadPassed = $false
try {
    if ($result.Session -eq 0) { throw 'Run requires an interactive scheduled task.' }
    Assert-ExpectedDriver
    if ([IO.Path]::GetFullPath($PSCommandPath) -ine $localRunner) { throw 'Run must use the copied local runner.' }
    $verifiedReceiptBytes = $null
    $receipt = Assert-Build ([ref]$verifiedReceiptBytes)
    Assert-NoOverrides
    # Snapshot and recheck the complete artifact set. Execute this run's local
    # copies so a subsequent Build cannot replace its executable or shader.
    foreach ($artifact in $receipt.Artifacts) {
        $destination = Join-Path $runDir $artifact.Name
        Copy-Item -LiteralPath $artifact.Path -Destination $destination
        if ((Get-FileHash -LiteralPath $destination -Algorithm SHA256).Hash -ine $artifact.SHA256) {
            throw "Run snapshot differs from build receipt: $($artifact.Name)"
        }
    }
    [IO.File]::WriteAllBytes((Join-Path $runDir 'build-provenance.json'), $verifiedReceiptBytes)
    $result.BuildReceipt = Get-Artifact (Join-Path $runDir 'build-provenance.json')
    $result.ExecutedArtifacts = @($receipt.Artifacts | ForEach-Object { Get-Artifact (Join-Path $runDir $_.Name) })
    $info = New-Object System.Diagnostics.ProcessStartInfo
    $info.FileName = "$runDir\d3d12_raytracing_probe.exe"
    $info.Arguments = "`"$runDir\raytracing.dxil`""
    if ($WithoutRaytracing) { $info.Arguments += ' --without-raytracing' }
    $info.WorkingDirectory = $runDir
    $info.UseShellExecute = $false; $info.RedirectStandardOutput = $true; $info.RedirectStandardError = $true
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
    $blocked = $process.ExitCode -eq 77 -and $errText.Contains('BLOCKED77,')
    $admitted = $outText -match '(?m)^CAP,NativeFL12_1Admission,00000000\r?$'
    $result.NativeFL12_1Admitted = $admitted
    $required = @('PASS,optional_rt_state_object_created', 'PASS,cross_queue_build_dispatch', 'PASS,bundle_pipeline_and_dispatch',
                  'PASS,bundle_inherited_root_dispatch', 'PASS,compact_clone_update_source_lifetime',
                  'PASS,tlas_restore_completed_before_blas_recording',
                  'PASS,serialization_query_after_tlas_restore',
                  'PASS,serialize_relocate_tlas_first_deserialize_lifetime', 'PASS,native_dxr_probe_completed')
    if ($WithoutRaytracing) {
        $required = @('CAP,RaytracingTier,0', 'PASS,no_rt_state_object_refused',
                      'PASS,no_rt_gpu_readback_4096_words', 'PASS,native_no_rt_probe_completed')
    }
    $workloadPassed = $process.ExitCode -eq 0 -and @($required | Where-Object { !$outText.Contains($_) }).Count -eq 0
    $result.Modules = @($errText -split "`r?`n" | Where-Object { $_ -like 'MODULE,*' } | ForEach-Object {
        $parts = $_ -split ',', 3
        if ($parts.Count -ne 3 -or !$parts[1] -or !$parts[2]) { throw 'Malformed loaded-module evidence.' }
        $module = Get-Artifact $parts[2]
        if ($module.Name -ine $parts[1]) { throw 'Loaded-module name/path evidence disagrees.' }
        $module
    })
    $umd = @($result.Modules | Where-Object { $_.Name -match '^helios_umd12(?:_[0-9a-fA-F]{16})?\.dll$' })
    if ($umd.Count -gt 1 -or (($admitted -or $workloadPassed) -and $umd.Count -ne 1)) {
        throw 'Admitted native DXR requires exactly one identified Helios UMD module.'
    }
    foreach ($module in $umd) {
        if ($module.SHA256 -ine $ExpectedUmd12SHA256) { throw 'Loaded native UMD SHA256 differs from the intended build.' }
        if ($module.Name -match '^helios_umd12_([0-9a-fA-F]{16})\.dll$' -and
            $Matches[1] -ine $module.SHA256.Substring(0, 16)) { throw 'Native UMD filename digest differs from its contents.' }
    }
    $result.UmdLoaded = $umd.Count -eq 1
    if ($blocked) {
        $result.Blocked = $true; $result.ExitCode = 77
        $result.BlockReason = ($errText -split "`r?`n" | Where-Object { $_ -like 'BLOCKED77,*' }) -join '; '
    } elseif (!$workloadPassed) {
        throw 'Native DXR probe failed; inspect archived output. No automatic retry.'
    } else {
        # Set completion only after every mandatory attribution operation passes.
        $result.Completed = $true; $result.ExitCode = 0
    }
} catch {
    Set-RunFailure $_.Exception.Message
} finally {
    # Kill is asynchronous and pending driver I/O can delay process exit. A
    # failed termination or undrained output must not block the failure receipt.
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
        $result.DriverLogs = @()
        foreach ($suffix in @('.log','-vkd3d.log')) {
            try {
                $log = "C:\ProgramData\Helios\umd12-$($result.ProcessId)$suffix"
                if (Test-Path -LiteralPath $log) {
                    Copy-Item -LiteralPath $log -Destination $runDir
                    $result.DriverLogs += Get-Artifact (Join-Path $runDir ([IO.Path]::GetFileName($log)))
                } elseif ($workloadPassed) { throw "Mandatory driver evidence missing: $log" }
            } catch { Set-RunFailure $_.Exception.Message }
        }
    }
    if ($process) {
        try { $process.Dispose() } catch { Set-RunFailure $_.Exception.Message }
    }
}
# A child result is provisional until every archived artifact matches its
# local evidence. In particular, a partial directory copy must never publish
# Completed=true or a successful BLOCKED classification before the evidence.
$verifiedCompleted = $result.Completed
$verifiedBlocked = $result.Blocked
$verifiedExitCode = $result.ExitCode
$result.Completed = $false; $result.Blocked = $false; $result.ExitCode = 1
$archiveDir = Join-Path $ArchiveRoot ([IO.Path]::GetFileName($runDir))
$result.ArchivePath = $archiveDir
$archiveOwned = $false
$localResult = Join-Path $runDir 'result.json'
try {
    $result.Evidence = @(Get-ChildItem -LiteralPath $runDir -File -Recurse | ForEach-Object {
        [ordered]@{ Path = $_.FullName.Substring($runDir.Length + 1); Length = $_.Length
            SHA256 = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash }
    })
    $result | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath $localResult -Encoding UTF8
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
    $result.Completed = $verifiedCompleted; $result.Blocked = $verifiedBlocked; $result.ExitCode = $verifiedExitCode
    $result | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath $localResult -Encoding UTF8
    $archivedResult = Join-Path $archiveDir 'result.json'
    Copy-Item -LiteralPath $localResult -Destination $archivedResult -Force
    if ((Get-FileHash -LiteralPath $localResult -Algorithm SHA256).Hash -ine
            (Get-FileHash -LiteralPath $archivedResult -Algorithm SHA256).Hash) { throw 'Archived final result hash mismatch.' }
} catch {
    $result.Completed = $false; $result.Blocked = $false; $result.ExitCode = 1
    $result.ArchiveError = $_.Exception.Message
    $failurePaths = @($localResult)
    if ($archiveOwned) { $failurePaths += (Join-Path $archiveDir 'result.json') }
    foreach ($path in $failurePaths) {
        try {
            $result | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath $path -Encoding UTF8
        } catch { Write-Warning "Failed to persist failure receipt at ${path}: $($_.Exception.Message)" }
    }
}
if ($result.Blocked) { Write-Output $result.BlockReason; exit 77 }
if (!$result.Completed) { Write-Output "FAIL: $runDir"; exit 1 }
Write-Output "PASS: $archiveDir"
exit 0
