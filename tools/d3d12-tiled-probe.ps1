# Build locally first; schedule against an exact deployed UMD identity.
# Examples through win MCP:
#   powershell -NoProfile -ExecutionPolicy Bypass -File Z:\tools\d3d12-tiled-probe.ps1 -Mode Build
#   powershell -NoProfile -ExecutionPolicy Bypass -File Z:\tools\d3d12-tiled-probe.ps1 -Mode Schedule -ExpectedUmd12SHA256 <full-deployed-UMD12-SHA256>
# A selected subset is explicit, e.g. -CaseSet 'mapping-signal,wait-remap'.
# Every case has a separate process/device. Overall exit 77 means BLOCKED;
# selected cases that did not execute or complete never count as a pass.
[CmdletBinding()]
param(
    [ValidateSet('Build','Schedule','Run')][string]$Mode = 'Build',
    [string]$BuildDir = 'C:\ProgramData\Helios\tiled-probe',
    [string]$ArchiveRoot = 'Z:\tmp\fl12-tiled',
    [string]$InteractiveUser = 'Rupansh',
    [string]$CaseSet = 'all',
    [string]$ExpectedUmd12SHA256 = ''
)
$executedRunnerSource = $MyInvocation.MyCommand.ScriptBlock.Ast.Extent.Text
$ErrorActionPreference = 'Stop'
if ($Mode -ne 'Build') {
    if ($ExpectedUmd12SHA256 -notmatch '\A[0-9a-fA-F]{64}\z') { throw 'Schedule/Run requires the full 64-hex ExpectedUmd12SHA256.' }
    $ExpectedUmd12SHA256 = $ExpectedUmd12SHA256.ToUpperInvariant()
}
$allCases = @('no-output-msaa','tiled-format-caps','tiling-buffer','tiling-2d','tiling-3d','mappings','copy-mappings',
    'copy-tiles-2d','copy-tiles-predicated','copy-tiles-msaa4x','copy-tiles-depth-msaa4x','committed-depth-copy','committed-depth-copy-immediates','immediate-constants','unmap-lifetime','mapping-signal','wait-remap',
    'invalid-counts','invalid-bounds')
$selectedCases = if ($CaseSet -eq 'all') { $allCases } else { @($CaseSet.Split(',') | ForEach-Object { $_.Trim() }) }
if (!$selectedCases.Count) { throw 'No cases selected.' }
foreach ($case in $selectedCases) {
    if ($allCases -notcontains $case) { throw "Unknown case: $case" }
}
if (@($selectedCases | Select-Object -Unique).Count -ne $selectedCases.Count) { throw 'Duplicate cases selected.' }
if ($BuildDir -notmatch '^[Cc]:\\') { throw 'BuildDir must be on local C: disk.' }
$BuildDir = [IO.Path]::GetFullPath($BuildDir).TrimEnd('\')
$localRunner = Join-Path $BuildDir 'd3d12-tiled-probe.ps1'
$executable = Join-Path $BuildDir 'd3d12_tiled_probe.exe'
$provenancePath = Join-Path $BuildDir 'build-provenance.json'
$requiredBuildFiles = @('d3d12_tiled_probe.cpp','d3d12-tiled-probe.ps1','d3d12_native_identity.h','d3d12_tiled_probe.exe')
$buildFileNames = $requiredBuildFiles + @('build.cmd','build.log')

function Get-Artifact([string]$Path) {
    $item = Get-Item -LiteralPath $Path -ErrorAction Stop
    [ordered]@{
        Path = $item.FullName; Length = $item.Length
        SHA256 = (Get-FileHash -LiteralPath $item.FullName -Algorithm SHA256).Hash
        FileVersion = $item.VersionInfo.FileVersion; ProductVersion = $item.VersionInfo.ProductVersion
        LastWriteUTC = $item.LastWriteTimeUtc.ToString('o')
    }
}
function Complete-ProbeCapture($Copies, $Files, [int]$TimeoutMilliseconds = 5000) {
    # A terminating driver client can have an exit status while an inherited
    # pipe is still open. Never call GetResult until BOTH drains have completed.
    # Stream to unbuffered files throughout the run so failure text is already
    # available when process teardown, rather than GPU work, is what is stuck.
    $captureError = $null
    try {
        if (![Threading.Tasks.Task]::WaitAll([Threading.Tasks.Task[]]$Copies, $TimeoutMilliseconds)) {
            $captureError = 'Probe output pipes did not close within the bounded drain; partial capture, no acceptance.'
        }
    } catch { $captureError = "Probe output capture failed: $($_.Exception.Message)" }
    finally {
        # Freeze the captured files before hashing/archiving. A still-pending
        # reader may subsequently fault on the closed destination; it cannot
        # append to a receipt's evidence. Do not wait for that reader again.
        foreach ($file in $Files) { if ($file) { $file.Dispose() } }
    }
    return $captureError
}
function Assert-Build {
    $build = Get-Content -LiteralPath $provenancePath -Raw | ConvertFrom-Json
    if ($build.Schema -ne 1 -or !$build.BuildSucceeded -or @($build.Artifacts).Count -ne $buildFileNames.Count) {
        throw 'Invalid or incomplete build receipt; run Build successfully first.'
    }
    foreach ($name in $buildFileNames) {
        $expectedPath = [IO.Path]::GetFullPath((Join-Path $BuildDir $name))
        $entries = @($build.Artifacts | Where-Object { $_.Path -eq $expectedPath })
        if ($entries.Count -ne 1 -or $entries[0].SHA256 -notmatch '\A[0-9a-fA-F]{64}\z') {
            throw "Build receipt lacks exactly one required local artifact hash: $expectedPath"
        }
    }
    foreach ($artifact in $build.Artifacts) {
        if ((Get-FileHash -LiteralPath $artifact.Path -Algorithm SHA256).Hash -ne $artifact.SHA256) {
            throw "Build artifact changed since provenance capture: $($artifact.Path)"
        }
    }
    $runner = @($build.Artifacts | Where-Object { $_.Path -ieq $localRunner })[0]
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
    foreach ($name in @('VKD3D_FEATURE_LEVEL','VKD3D_SHADER_MODEL','D3D12SDKPath','D3D12SDKVersion')) {
        if ([Environment]::GetEnvironmentVariable($name)) { throw "Capability/runtime override $name is forbidden." }
    }
    foreach ($name in @('d3d12.dll','D3D12Core.dll','dxgi.dll','helios_vkd3d.dll','d3d10warp.dll')) {
        if (Test-Path -LiteralPath (Join-Path $BuildDir $name)) { throw "App-local substitution found: $name" }
    }
}
function Get-RepositoryProvenance([string]$Repository) {
    $workTree = [IO.Path]::GetFullPath($Repository)
    $marker = Get-Item -Force -LiteralPath (Join-Path $workTree '.git')
    if ($marker.PSIsContainer) {
        $gitDirectory = $marker.FullName
    } else {
        $reference = (Get-Content -LiteralPath $marker.FullName -Raw).Trim()
        if ($reference -notmatch '^gitdir: (.+)$') { throw "Invalid gitdir file: $($marker.FullName)" }
        $gitDirectory = $Matches[1]
        if ($gitDirectory -match '^/(?!/)') { throw "Linux absolute gitdir cannot be resolved in this guest: $gitDirectory" }
        if (![IO.Path]::IsPathRooted($gitDirectory)) { $gitDirectory = Join-Path $workTree $gitDirectory }
        $gitDirectory = [IO.Path]::GetFullPath($gitDirectory)
    }
    if (!(Test-Path -LiteralPath $gitDirectory -PathType Container)) { throw "Git metadata missing: $gitDirectory" }
    # Forward slashes also preserve a drive root such as Z:/ without introducing
    # a trailing backslash before an argv quote. Never change global Git trust.
    $workTree = $workTree.Replace('\','/'); $gitDirectory = $gitDirectory.Replace('\','/')
    if ($workTree -match '["\r\n]' -or $gitDirectory -match '["\r\n]') { throw 'Invalid Git path for argv quoting.' }
    $git = (Get-Command git.exe -ErrorAction Stop).Source
    $captured = [ordered]@{}
    foreach ($operation in @('Head','Status')) {
        $tail = if ($operation -eq 'Head') { 'rev-parse --verify HEAD' } else { 'status --short --ignore-submodules=all' }
        $info = New-Object System.Diagnostics.ProcessStartInfo
        $info.FileName = $git; $info.WorkingDirectory = $workTree
        $info.UseShellExecute = $false; $info.RedirectStandardOutput = $true; $info.RedirectStandardError = $true
        # Explicit worktree overrides a stored Linux core.worktree, while the
        # resolved gitdir handles both ordinary repositories and submodules.
        $info.Arguments = "--no-optional-locks -c safe.directory= -c `"safe.directory=$workTree`" --git-dir=`"$gitDirectory`" --work-tree=`"$workTree`" $tail"
        $process = New-Object System.Diagnostics.Process; $process.StartInfo = $info
        try {
            if (!$process.Start()) { throw 'Git provenance process start failed.' }
            # Separate native streams bypass PowerShell 5's NativeCommandError
            # conversion; the actual Git exit code decides success or failure.
            $stdout = $process.StandardOutput.ReadToEndAsync(); $stderr = $process.StandardError.ReadToEndAsync()
            if (!$process.WaitForExit(30000)) {
                $process.Kill(); $null = $process.WaitForExit(5000)
                throw "Git $operation timed out for $workTree"
            }
            $outText = $stdout.GetAwaiter().GetResult(); $errText = $stderr.GetAwaiter().GetResult()
            if ($process.ExitCode -ne 0) { throw "Git $operation failed for $workTree (exit $($process.ExitCode)): $errText $outText" }
            $captured[$operation] = $outText.TrimEnd([char[]]"`r`n")
            $captured[$operation + 'Stderr'] = $errText
        } finally { $process.Dispose() }
    }
    if ($captured.Head -notmatch '^[0-9a-fA-F]{40}$') { throw "Invalid source HEAD for $workTree : $($captured.Head)" }
    [ordered]@{
        HEAD = $captured.Head; Status = $captured.Status; WorkTree = $workTree; GitDirectory = $gitDirectory
        GitExecutable = $git; HeadStderr = $captured.HeadStderr; StatusStderr = $captured.StatusStderr
        StatusScope = 'Own worktree only (--ignore-submodules=all); selected engine repositories are inventoried separately.'
    }
}

if ($Mode -eq 'Build') {
    New-Item -ItemType Directory -Force $BuildDir | Out-Null
    # A failed rebuild must never leave an old success receipt that can admit
    # partially replaced inputs or a stale executable to Schedule/Run.
    if (Test-Path -LiteralPath $provenancePath) { Remove-Item -LiteralPath $provenancePath -Force }
    $sourceRoot = Split-Path $PSScriptRoot -Parent
    foreach ($name in @('d3d12_tiled_probe.cpp','d3d12-tiled-probe.ps1','d3d12_native_identity.h')) {
        $source = Join-Path $PSScriptRoot $name
        $destination = Join-Path $BuildDir $name
        if ([IO.Path]::GetFullPath($source) -ne [IO.Path]::GetFullPath($destination)) {
            Copy-Item -LiteralPath $source -Destination $destination -Force
        }
    }
    $vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
    $vs = & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
    if (!$vs) { throw 'MSVC installation not found.' }
    $buildCommand = @"
@echo off
call "$vs\VC\Auxiliary\Build\vcvars64.bat"
if errorlevel 1 exit /b %errorlevel%
where cl
set WindowsSDKVersion
set VCToolsVersion
cl /nologo /Bv /W4 /EHsc /std:c++17 /O2 d3d12_tiled_probe.cpp /Fe:d3d12_tiled_probe.exe /link d3d12.lib dxgi.lib d3dcompiler.lib 2>&1
exit /b %errorlevel%
"@
    [IO.File]::WriteAllText((Join-Path $BuildDir 'build.cmd'), $buildCommand, [Text.Encoding]::ASCII)
    Push-Location $BuildDir
    try {
        & cmd.exe /d /c (Join-Path $BuildDir 'build.cmd') *> (Join-Path $BuildDir 'build.log')
        if ($LASTEXITCODE) { Get-Content (Join-Path $BuildDir 'build.log'); throw "MSVC failed: $LASTEXITCODE" }
    } finally { Pop-Location }
    $refs = [ordered]@{}
    foreach ($relative in @('.','vkd3d-proton-helios','icd\mesa','dxvk-helios')) {
        $refs[$relative] = Get-RepositoryProvenance (Join-Path $sourceRoot $relative)
    }
    $provenance = [ordered]@{
        Schema = 1; BuildSucceeded = $true
        UTC = [DateTime]::UtcNow.ToString('o'); SourceRoot = $sourceRoot; SourceRefs = $refs
        VisualStudio = $vs; BuildCommand = $buildCommand
        Artifacts = @($buildFileNames |
            ForEach-Object { Get-Artifact (Join-Path $BuildDir $_) })
        Scope = 'Native probe build only; driver source refs are inventory, not proof of the deployed binary source.'
    }
    $provenance | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath $provenancePath -Encoding UTF8
    Write-Output "Built native tiled probe: $executable"
    exit 0
}

$buildProvenance = Assert-Build
Assert-NoOverrides
if ($Mode -eq 'Schedule') {
    $taskName = 'helios_tiled_acceptance'
    $existing = Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
    if ($existing -and $existing.State -eq 'Running') { throw "$taskName is already running." }
    $caseArgument = $selectedCases -join ','
    $arguments = "-NoProfile -ExecutionPolicy Bypass -File `"$localRunner`" -Mode Run -BuildDir `"$BuildDir`" -ArchiveRoot `"$ArchiveRoot`" -CaseSet `"$caseArgument`" -ExpectedUmd12SHA256 $ExpectedUmd12SHA256"
    $action = New-ScheduledTaskAction -Execute "$env:SystemRoot\System32\WindowsPowerShell\v1.0\powershell.exe" -Argument $arguments -WorkingDirectory $BuildDir
    $principal = New-ScheduledTaskPrincipal -UserId $InteractiveUser -LogonType Interactive -RunLevel Highest
    $settings = New-ScheduledTaskSettingsSet -ExecutionTimeLimit (New-TimeSpan -Minutes 30) -MultipleInstances IgnoreNew
    Register-ScheduledTask -TaskName $taskName -Action $action -Principal $principal -Settings $settings -Force | Out-Null
    Start-ScheduledTask -TaskName $taskName
    Write-Output "Started interactive $taskName for $caseArgument. Inspect the fresh archived result.json; task start is not acceptance."
    exit 0
}

if ((Get-Process -Id $PID).SessionId -eq 0) { throw 'Run requires an interactive scheduled task.' }
$runName = 'run-' + (Get-Date -Format 'yyyyMMdd-HHmmss-fff') + '-' + [Guid]::NewGuid().ToString('N').Substring(0,8)
$runDir = Join-Path $BuildDir $runName
New-Item -ItemType Directory $runDir | Out-Null
$beforeEnvironment = [ordered]@{}
foreach ($name in @('HELIOS_WSI_ASYNC_PRESENT','HELIOS_RETIRE_FEEDBACK','VKD3D_CONFIG','VKD3D_FEATURE_LEVEL',
        'VKD3D_SHADER_MODEL','D3D12SDKPath','D3D12SDKVersion','VK_DRIVER_FILES','VK_ICD_FILENAMES')) {
    $beforeEnvironment[$name] = [Environment]::GetEnvironmentVariable($name)
}
$env:HELIOS_WSI_ASYNC_PRESENT = '1'
Remove-Item Env:\HELIOS_RETIRE_FEEDBACK -ErrorAction SilentlyContinue
$result = [ordered]@{
    UTC = [DateTime]::UtcNow.ToString('o'); Session = (Get-Process -Id $PID).SessionId
    RunnerProcess = $PID; SelectedCases = @($selectedCases); Completed = $false; ExitCode = 1
    ExpectedUmd12SHA256 = $ExpectedUmd12SHA256
    BeforeEnvironment = $beforeEnvironment; EffectiveInvariants = [ordered]@{ HELIOS_WSI_ASYNC_PRESENT = '1' }
    Cases = @(); Scope = 'Bounded native tiled behavior; no feature-level, benchmark, visual, or performance acceptance.'
}
$buildLocks = @()
try {
    # Hold one build for the complete case set. Re-reading a mutable receipt
    # between cases can accept different executables under the first receipt.
    foreach ($name in @($buildFileNames + 'build-provenance.json')) {
        $buildLocks += [IO.File]::Open((Join-Path $BuildDir $name), [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
    }
    $buildProvenance = Assert-Build
    $result.BuildReceipt = Get-Artifact $provenancePath
    $result.ExecutedExecutable = Get-Artifact $executable
    $buildEvidence = Join-Path $runDir 'build'
    New-Item -ItemType Directory $buildEvidence | Out-Null
    foreach ($artifact in $buildProvenance.Artifacts) {
        $copy = Join-Path $buildEvidence ([IO.Path]::GetFileName($artifact.Path))
        Copy-Item -LiteralPath $artifact.Path -Destination $copy
        if ((Get-FileHash -LiteralPath $copy -Algorithm SHA256).Hash -ine $artifact.SHA256) {
            throw "Build evidence copy hash mismatch: $($artifact.Path)"
        }
    }
    Copy-Item -LiteralPath $provenancePath -Destination $buildEvidence
    if ((Get-FileHash -LiteralPath (Join-Path $buildEvidence 'build-provenance.json') -Algorithm SHA256).Hash -ine $result.BuildReceipt.SHA256) {
        throw 'Build receipt evidence copy hash mismatch.'
    }
    $knob = Get-ItemProperty -Path 'HKLM:\SOFTWARE\Helios' -Name UmdD3D12 -ErrorAction SilentlyContinue
    $result.UmdD3D12 = if ($null -ne $knob) { [ordered]@{ State = 'Present'; Value = $knob.UmdD3D12 } } else { [ordered]@{ State = 'Absent'; Default = 'ON' } }
    $result.InstalledDisplayDrivers = @(Get-CimInstance Win32_PnPSignedDriver |
        Where-Object { $_.DeviceID -like 'PCI\VEN_1AF4&DEV_1050*' } |
        Select-Object DeviceID,DeviceName,DriverVersion,InfName,DriverDate,IsSigned,Signer)
    $failed = $false; $blocked = $false
    foreach ($case in $selectedCases) {
        if ($failed) {
            $result.Cases += [ordered]@{ Name = $case; Status = 'NOT_RUN'; Reason = 'Earlier case failed; no automatic retry.' }
            continue
        }
        $caseDir = Join-Path $runDir $case
        New-Item -ItemType Directory $caseDir | Out-Null
        $entry = [ordered]@{ Name = $case; StartUTC = [DateTime]::UtcNow.ToString('o'); Status = 'FAIL'; Completed = $false
            ExpectedUmd12SHA256 = $ExpectedUmd12SHA256; ExecutedExecutable = $result.ExecutedExecutable }
        $process = $null; $started = $false
        $captureFiles = @(); $captureCopies = @(); $processTerminated = $false
        try {
            Assert-NoOverrides
            $info = New-Object System.Diagnostics.ProcessStartInfo
            $info.FileName = $executable; $info.Arguments = "--case $case"; $info.WorkingDirectory = $BuildDir
            $info.UseShellExecute = $false; $info.RedirectStandardOutput = $true; $info.RedirectStandardError = $true
            $process = New-Object System.Diagnostics.Process; $process.StartInfo = $info
            if (!$process.Start()) { throw 'Probe process start failed.' }
            $started = $true
            $entry.ProcessId = $process.Id
            foreach ($stream in @(
                    [pscustomobject]@{ Name = 'stdout.txt'; Reader = $process.StandardOutput.BaseStream },
                    [pscustomobject]@{ Name = 'stderr.txt'; Reader = $process.StandardError.BaseStream })) {
                $file = [IO.FileStream]::new((Join-Path $caseDir $stream.Name), [IO.FileMode]::CreateNew,
                    [IO.FileAccess]::Write, [IO.FileShare]::Read, 1)
                $captureFiles += $file
                $captureCopies += $stream.Reader.CopyToAsync($file)
            }
            if (!$process.WaitForExit(90000)) {
                $entry.TimedOut = $true
                $process.Kill()
                if (!$process.WaitForExit(5000)) { throw 'Probe did not terminate after timeout; manual diagnosis required.' }
                throw 'Probe timed out; terminated without retry. Timeout is not GPU completion.'
            }
            $processTerminated = $true
            $entry.ExitCode = $process.ExitCode
        } catch {
            $entry.Error = $_.Exception.Message
        } finally {
            # Attempt both PID-matched logs before other result processing,
            # including failed or timed-out children. A missing/copy-failed
            # diagnostic must prevent a later PASS or BLOCKED classification.
            if ($entry.ProcessId) {
                $entry.DiagnosticLogs = @()
                foreach ($suffix in @('.log','-vkd3d.log')) {
                    $log = "C:\ProgramData\Helios\umd12-$($entry.ProcessId)$suffix"
                    try {
                        Copy-Item -LiteralPath $log -Destination $caseDir
                        $entry.DiagnosticLogs += [IO.Path]::GetFileName($log)
                    } catch {
                        $failure = "Diagnostic log copy failed for ${log}: $($_.Exception.Message)"
                        $entry.Error = if ($entry.Error) { "$($entry.Error); $failure" } else { $failure }
                        $entry.Status = 'FAIL'; $entry.Completed = $false
                    }
                }
            }
            $captureError = Complete-ProbeCapture $captureCopies $captureFiles
            $entry.OutputCaptureCompleted = !$captureError -and $captureCopies.Count -eq 2
            $entry.ProcessTerminationConfirmed = $processTerminated
            $entry.ChildMayStillBeRunning = $started -and !$processTerminated
            if (!$entry.OutputCaptureCompleted) {
                if (!$captureError) { $captureError = 'Both probe output streams were not captured.' }
                $entry.Error = if ($entry.Error) { "$($entry.Error); $captureError" } else { $captureError }
            }
            if ($process -and $started) {
                $outText = if (Test-Path -LiteralPath (Join-Path $caseDir 'stdout.txt')) {
                    [IO.File]::ReadAllText((Join-Path $caseDir 'stdout.txt')) } else { '' }
                $errText = if (Test-Path -LiteralPath (Join-Path $caseDir 'stderr.txt')) {
                    [IO.File]::ReadAllText((Join-Path $caseDir 'stderr.txt')) } else { '' }
                try {
                    $entry.Modules = @($errText -split "`r?`n" | Where-Object { $_ -like 'MODULE,*' } |
                        ForEach-Object {
                            $parts = $_ -split ',',3
                            $artifact = Get-Artifact $parts[2]
                            $artifact.Name = $parts[1]
                            $artifact
                        })
                    # Native identity is mandatory even when the admitted
                    # FL11_0 device later reports a missing tiled capability.
                    $umdModules = @($entry.Modules | Where-Object { $_.Name -match '\Ahelios_umd12(?:_[0-9a-f]{16})?\.dll\z' })
                    if ($umdModules.Count -ne 1) { throw 'Exactly one canonical logged native UMD12 module is required for PASS or BLOCKED.' }
                    $umd = $umdModules[0]
                    if ([IO.Path]::GetFileName($umd.Path) -ine $umd.Name) { throw 'Logged UMD12 name differs from its loaded path basename.' }
                    if ($umd.SHA256 -ine $ExpectedUmd12SHA256) { throw "Loaded UMD12 SHA256 differs from expected: $($umd.Path)" }
                    $nameMatch = [regex]::Match($umd.Name, '\Ahelios_umd12(?:_(?<suffix>[0-9a-f]{16}))?\.dll\z', [Text.RegularExpressions.RegexOptions]::IgnoreCase)
                    if ($nameMatch.Groups['suffix'].Success -and
                        $nameMatch.Groups['suffix'].Value -ine $umd.SHA256.Substring(0,16)) {
                        throw 'Loaded UMD12 filename hash suffix differs from its full SHA256 prefix.'
                    }
                    $entry.VerifiedUmd12 = $umd
                    if (!$entry.Error -and $processTerminated -and $entry.ExitCode -eq 0 -and
                        $outText -match "(?m)^PASS,tiled,$([regex]::Escape($case)),case-completed\r?$") {
                        $entry.Status = 'PASS'; $entry.Completed = $true
                    } elseif (!$entry.Error -and $processTerminated -and $entry.ExitCode -eq 77 -and
                        $outText -match "(?m)^BLOCKED,tiled,$([regex]::Escape($case)),") {
                        $entry.Status = 'BLOCKED'
                    } elseif (!$entry.Error) { $entry.Error = 'Exit code and required completion marker did not establish acceptance.' }
                } catch { $entry.Error = $_.Exception.Message; $entry.Status = 'FAIL'; $entry.Completed = $false }
                $process.Dispose()
            } elseif ($process -and !$started) {
                $process.Dispose()
            }
            $entry.EndUTC = [DateTime]::UtcNow.ToString('o')
            $entry | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath (Join-Path $caseDir 'case.json') -Encoding UTF8
            $result.Cases += $entry
        }
        Write-Output "$($entry.Status): $case"
        $failed = $entry.Status -eq 'FAIL'
        $blocked = $blocked -or $entry.Status -eq 'BLOCKED'
    }
    $result.Completed = !$failed -and !$blocked -and @($result.Cases | Where-Object { $_.Status -eq 'PASS' }).Count -eq $selectedCases.Count
    $result.ExitCode = if ($failed) { 1 } elseif ($blocked) { 77 } elseif ($result.Completed) { 0 } else { 1 }
} catch {
    $result.Error = $_.Exception.Message; $result.ExitCode = 1; $result.Completed = $false
} finally {
    foreach ($handle in $buildLocks) { $handle.Dispose() }
}
# Publish a failure receipt first; a partial archive must not leave a success
# result pointing at evidence that never arrived. PASS/BLOCKED follows the
# checked artifact copy and checked final result publication.
$verifiedCompleted = $result.Completed
$verifiedExitCode = $result.ExitCode
$result.Completed = $false; $result.ExitCode = 1
$result.EndUTC = [DateTime]::UtcNow.ToString('o')
$destination = Join-Path $ArchiveRoot $runName
$result.ArchivePath = $destination
$localResult = Join-Path $runDir 'result.json'
$archiveOwned = $false
try {
    $result.Evidence = @(Get-ChildItem -LiteralPath $runDir -File -Recurse | ForEach-Object {
        [ordered]@{ Path = $_.FullName.Substring($runDir.Length + 1); Length = $_.Length
            SHA256 = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash }
    })
    $result | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath $localResult -Encoding UTF8
    New-Item -ItemType Directory -Force $ArchiveRoot | Out-Null
    if (Test-Path -LiteralPath $destination) { throw 'Fresh archive name unexpectedly exists.' }
    New-Item -ItemType Directory $destination | Out-Null
    $archiveOwned = $true
    foreach ($item in Get-ChildItem -LiteralPath $runDir) {
        Copy-Item -LiteralPath $item.FullName -Destination $destination -Recurse
    }
    foreach ($entry in $result.Evidence) {
        $archived = Get-Item -LiteralPath (Join-Path $destination $entry.Path)
        if ($archived.Length -ne $entry.Length -or
                (Get-FileHash -LiteralPath $archived.FullName -Algorithm SHA256).Hash -ine $entry.SHA256) {
            throw "Archive evidence hash/size mismatch: $($entry.Path)"
        }
    }
    $result.Completed = $verifiedCompleted; $result.ExitCode = $verifiedExitCode
    $result | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath $localResult -Encoding UTF8
    $archivedResult = Join-Path $destination 'result.json'
    Copy-Item -LiteralPath $localResult -Destination $archivedResult -Force
    if ((Get-FileHash -LiteralPath $localResult -Algorithm SHA256).Hash -ine
            (Get-FileHash -LiteralPath $archivedResult -Algorithm SHA256).Hash) { throw 'Archived final result hash mismatch.' }
} catch {
    $result.Completed = $false; $result.ExitCode = 1
    $result.ArchiveError = $_.Exception.Message
    $failurePaths = @($localResult)
    if ($archiveOwned) { $failurePaths += (Join-Path $destination 'result.json') }
    foreach ($path in $failurePaths) {
        try {
            $result | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath $path -Encoding UTF8
        } catch { Write-Warning "Failed to persist failure receipt at ${path}: $($_.Exception.Message)" }
    }
}
Write-Output "Result: $(Join-Path $ArchiveRoot $runName) (exit $($result.ExitCode))"
exit $result.ExitCode
