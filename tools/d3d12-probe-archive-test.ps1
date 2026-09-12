# Exercise the actual runners' archive blocks with synthetic child outcomes.
# This script never starts a graphics process or claims native GPU acceptance.
[CmdletBinding()]
param([string]$TestRoot = 'C:\ProgramData\Helios\probe-archive-tests')
$ErrorActionPreference = 'Stop'
if ($TestRoot -notmatch '^[Cc]:\\') { throw 'TestRoot must use local C: disk.' }
$testDir = Join-Path $TestRoot ((Get-Date -Format 'yyyyMMdd-HHmmss-fff') + '-' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $testDir -Force | Out-Null
Microsoft.PowerShell.Management\Copy-Item -LiteralPath $PSCommandPath -Destination $testDir
$testScriptHash = (Get-FileHash -LiteralPath $PSCommandPath -Algorithm SHA256).Hash

function Copy-Item {
    param([string]$LiteralPath, [string]$Destination, [switch]$Recurse, [switch]$Force)
    $leaf = [IO.Path]::GetFileName($LiteralPath)
    if ($leaf -eq 'result.json') {
        $receipt = Get-Content -LiteralPath $LiteralPath -Raw | ConvertFrom-Json
        if (!$script:FaultHit -and $script:FaultMode -eq 'final-result' -and $receipt.ExitCode -in @(0,77)) {
            $script:FaultHit = $true
            throw 'Injected final result copy failure.'
        }
    }
    Microsoft.PowerShell.Management\Copy-Item @PSBoundParameters
    $copied = if (Test-Path -LiteralPath $Destination -PathType Container) { Join-Path $Destination $leaf } else { $Destination }
    if (!$script:FaultHit -and $script:FaultMode -eq 'partial-copy' -and $leaf -eq 'result.json') {
        $script:FaultHit = $true
        $receipt = Get-Content -LiteralPath $copied -Raw | ConvertFrom-Json
        $script:PrematureSuccess = $receipt.Completed -or $receipt.ExitCode -in @(0,77)
        throw 'Injected partial archive failure after provisional result publication.'
    }
    if (!$script:FaultHit -and $script:FaultMode -eq 'corrupt-evidence' -and $leaf -eq 'z-evidence.bin') {
        $script:FaultHit = $true
        [IO.File]::AppendAllText($copied, 'corruption')
    }
}

$cases = @()
foreach ($probe in @('raytracing','tiled')) {
    $runner = Join-Path $PSScriptRoot "d3d12-$probe-probe.ps1"
    $source = Get-Content -LiteralPath $runner -Raw
    $startMarker = if ($probe -eq 'raytracing') { '# A child result is provisional until' } else { '# Publish a failure receipt first;' }
    $endMarker = if ($probe -eq 'raytracing') { 'if ($result.Blocked)' } else { 'Write-Output "Result:' }
    $start = $source.IndexOf($startMarker, [StringComparison]::Ordinal)
    $end = $source.IndexOf($endMarker, [StringComparison]::Ordinal)
    if ($start -lt 0 -or $end -le $start -or $source.LastIndexOf($startMarker, [StringComparison]::Ordinal) -ne $start) {
        throw "Cannot isolate the current $probe archive block; update the test boundary explicitly."
    }
    $archiveBlock = [ScriptBlock]::Create($source.Substring($start, $end - $start))
    Microsoft.PowerShell.Management\Copy-Item -LiteralPath $runner -Destination $testDir
    $runnerHash = (Get-FileHash -LiteralPath $runner -Algorithm SHA256).Hash
    foreach ($childExit in @(0,77)) {
        foreach ($fault in @('none','partial-copy','corrupt-evidence','final-result')) {
            $runName = "$probe-$childExit-$fault"
            $runDir = Join-Path $testDir "$runName-local"
            $ArchiveRoot = Join-Path $testDir "$runName-archive"
            New-Item -ItemType Directory -Path $runDir | Out-Null
            [IO.File]::WriteAllText((Join-Path $runDir 'a-evidence.bin'), 'synthetic evidence A')
            [IO.File]::WriteAllText((Join-Path $runDir 'z-evidence.bin'), 'synthetic evidence Z')
            $result = [ordered]@{ Completed = ($childExit -eq 0); Blocked = ($childExit -eq 77)
                ExitCode = $childExit; Scope = 'Synthetic archive unit test, not a GPU result.' }
            $script:FaultMode = $fault; $script:FaultHit = $false; $script:PrematureSuccess = $false
            & $archiveBlock
            if ($script:PrematureSuccess) { throw "Success was published before the evidence copy completed: $runName" }
            $expectedExit = if ($fault -eq 'none') { $childExit } else { 1 }
            if (($fault -ne 'none') -ne $script:FaultHit) { throw "Fault injection was not exercised: $runName" }
            $archiveName = if ($probe -eq 'raytracing') { [IO.Path]::GetFileName($runDir) } else { $runName }
            $archiveDir = Join-Path $ArchiveRoot $archiveName
            foreach ($directory in @($runDir,$archiveDir)) {
                $saved = Get-Content -LiteralPath (Join-Path $directory 'result.json') -Raw | ConvertFrom-Json
                if ($saved.ExitCode -ne $expectedExit -or $saved.Completed -ne ($expectedExit -eq 0)) {
                    throw "Incorrect receipt after $runName in $directory"
                }
                if ($probe -eq 'raytracing' -and $saved.Blocked -ne ($expectedExit -eq 77)) { throw "Stale BLOCKED receipt: $runName" }
                if ($fault -ne 'none' -and !$saved.ArchiveError) { throw "Missing archive failure diagnosis: $runName" }
                if ($fault -eq 'none') {
                    foreach ($entry in $saved.Evidence) {
                        $path = Join-Path $directory $entry.Path
                        if ((Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash -ine $entry.SHA256) { throw "Evidence mismatch: $path" }
                    }
                }
            }
            $cases += [ordered]@{ Probe = $probe; SyntheticChildExit = $childExit; Fault = $fault
                ExpectedExit = $expectedExit; RunnerSHA256 = $runnerHash; Passed = $true }
        }
    }
}
# A failed/terminating tiled client can leave its output pipe open. Exercise
# the actual bounded drain with completed, never-completing and faulted Tasks;
# partial output must survive, and the archived file must no longer be writable.
$runner = Join-Path $PSScriptRoot 'd3d12-tiled-probe.ps1'
$source = Get-Content -LiteralPath $runner -Raw
$tokens = $null; $errors = $null
$ast = [Management.Automation.Language.Parser]::ParseInput($source, [ref]$tokens, [ref]$errors)
if ($errors.Count) { throw 'Tiled runner parse errors.' }
$capture = @($ast.FindAll({ param($node)
    $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq 'Complete-ProbeCapture'
}, $false))
if ($capture.Count -ne 1) { throw 'Cannot isolate tiled capture cleanup.' }
. ([ScriptBlock]::Create($capture[0].Extent.Text))
$runnerHash = (Get-FileHash -LiteralPath $runner -Algorithm SHA256).Hash
foreach ($fault in @('none','open-pipe','failed-read')) {
    $path = Join-Path $testDir "tiled-output-$fault.txt"
    $file = [IO.FileStream]::new($path, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::Read, 1)
    $bytes = [Text.Encoding]::UTF8.GetBytes('Captured before teardown stalled.')
    $file.Write($bytes, 0, $bytes.Length)
    $pending = [Threading.Tasks.TaskCompletionSource[bool]]::new()
    if ($fault -eq 'none') { $pending.SetResult($true) }
    if ($fault -eq 'failed-read') { $pending.SetException([IO.IOException]::new('Injected pipe read failure.')) }
    $timer = [Diagnostics.Stopwatch]::StartNew()
    $errorText = Complete-ProbeCapture @([Threading.Tasks.Task]::FromResult($true), $pending.Task) @($file) 100
    $timer.Stop()
    if ($timer.ElapsedMilliseconds -gt 2000 -or [bool]$errorText -ne ($fault -ne 'none') -or
            $file.CanWrite -or [IO.File]::ReadAllText($path) -cne 'Captured before teardown stalled.') {
        throw "Tiled output cleanup lost evidence, failed to freeze it, or did not remain bounded: $fault"
    }
    $cases += [ordered]@{ Probe = 'tiled'; Fault = $fault; CaptureError = $errorText
        ElapsedMilliseconds = $timer.ElapsedMilliseconds; RunnerSHA256 = $runnerHash; Passed = $true }
}
# Exercise the DXR runner's actual cleanup and archive blocks with a process
# whose termination or output completion cannot be confirmed. The fake methods
# reject unbounded waits, without requiring a hung native driver or GPU work.
$runner = Join-Path $PSScriptRoot 'd3d12-raytracing-probe.ps1'
$source = Get-Content -LiteralPath $runner -Raw
$tokens = $null; $errors = $null
$ast = [Management.Automation.Language.Parser]::ParseInput($source, [ref]$tokens, [ref]$errors)
if ($errors.Count) { throw 'DXR runner parse errors.' }
$failure = @($ast.FindAll({ param($node)
    $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq 'Set-RunFailure'
}, $false))
$cleanup = @($ast.FindAll({ param($node)
    $node -is [Management.Automation.Language.TryStatementAst] -and $node.Finally -and
        $node.Finally.Extent.Text.Contains('# Kill is asynchronous')
}, $false))
if ($failure.Count -ne 1 -or $cleanup.Count -ne 1) { throw 'Cannot isolate DXR failure/cleanup code.' }
. ([ScriptBlock]::Create($failure[0].Extent.Text))
$cleanupText = $cleanup[0].Finally.Extent.Text
$cleanupBlock = [ScriptBlock]::Create($cleanupText.Substring(1, $cleanupText.Length - 2))
$start = $source.IndexOf('# A child result is provisional until', [StringComparison]::Ordinal)
$end = $source.IndexOf('if ($result.Blocked)', [StringComparison]::Ordinal)
if ($start -lt 0 -or $end -le $start) { throw 'Cannot isolate DXR archive code.' }
$archiveBlock = [ScriptBlock]::Create($source.Substring($start, $end - $start))
$runnerHash = (Get-FileHash -LiteralPath $runner -Algorithm SHA256).Hash
foreach ($fault in @('pending-termination','kill-failed','output-stuck','termination-completed')) {
    $runDir = Join-Path $testDir "raytracing-timeout-$fault-local"
    $ArchiveRoot = Join-Path $testDir "raytracing-timeout-$fault-archive"
    New-Item -ItemType Directory -Path $runDir | Out-Null
    $result = [ordered]@{ Completed = $false; Blocked = $false; ExitCode = 1; TimedOut = $true
        ProcessId = [int]::MaxValue; Errors = @('Synthetic child timeout')
        Scope = 'Synthetic timeout recovery; no process or GPU was started.' }
    $workloadPassed = $false
    $process = [pscustomobject]@{ HasExited = $false; ExitCode = -1; Fault = $fault
        KillCalls = 0; WaitCalls = 0; Disposed = $false }
    $process | Add-Member ScriptMethod Kill {
        $this.KillCalls++
        if ($this.Fault -eq 'kill-failed') { throw 'Injected Kill failure.' }
    }
    $process | Add-Member ScriptMethod WaitForExit {
        param([int]$Milliseconds = -1)
        if ($Milliseconds -le 0 -or $Milliseconds -gt 5000) { throw 'Unbounded termination wait.' }
        $this.WaitCalls++
        return $this.Fault -in @('output-stuck','termination-completed')
    }
    $process | Add-Member ScriptMethod Dispose { $this.Disposed = $true }
    $stdout = [pscustomobject]@{ Ready = ($fault -eq 'termination-completed'); WaitCalls = 0 }
    $stdout | Add-Member ScriptMethod Wait {
        param([int]$Milliseconds = -1)
        if ($Milliseconds -le 0 -or $Milliseconds -gt 5000) { throw 'Unbounded output wait.' }
        $this.WaitCalls++
        return $this.Ready
    }
    $stdout | Add-Member ScriptMethod GetAwaiter { return $this }
    $stdout | Add-Member ScriptMethod GetResult {
        if (!$this.Ready) { throw 'Unbounded output result retrieval.' }
        return 'Synthetic captured output.'
    }
    $stderr = $stdout
    $script:FaultMode = 'none'; $script:FaultHit = $false; $script:PrematureSuccess = $false
    & $cleanupBlock
    if (!$process.Disposed -or $process.KillCalls -ne 1 -or $stdout.WaitCalls -ne 2) {
        throw "Cleanup did not attempt bounded termination/output/disposal: $fault"
    }
    $mayRun = $fault -in @('pending-termination','kill-failed')
    if ([bool]$result.ChildMayStillBeRunning -ne $mayRun) { throw "Incorrect child liveness classification: $fault" }
    if ($process.WaitCalls -ne $(if ($fault -eq 'kill-failed') { 0 } else { 1 })) { throw "Missing bounded termination wait: $fault" }
    & $archiveBlock
    foreach ($directory in @($runDir, (Join-Path $ArchiveRoot ([IO.Path]::GetFileName($runDir))))) {
        $saved = Get-Content -LiteralPath (Join-Path $directory 'result.json') -Raw | ConvertFrom-Json
        if ($saved.ExitCode -ne 1 -or $saved.Completed -or $saved.Blocked -or !$saved.TimedOut -or
                [bool]$saved.ChildMayStillBeRunning -ne $mayRun) { throw "Failed timeout receipt: $fault" }
    }
    $cases += [ordered]@{ Probe = 'raytracing'; Fault = $fault; ExpectedExit = 1
        RunnerSHA256 = $runnerHash; Passed = $true }
}
foreach ($probe in @('stream-output','indirect')) {
    $runner = Join-Path $PSScriptRoot "d3d12-$probe-probe.ps1"
    $source = Get-Content -LiteralPath $runner -Raw
    $startMarker = '# Completion stays false while mandatory evidence'
    $endMarker = 'if (!$result.Completed)'
    $start = $source.IndexOf($startMarker, [StringComparison]::Ordinal)
    $end = $source.IndexOf($endMarker, [StringComparison]::Ordinal)
    if ($start -lt 0 -or $end -le $start -or $source.LastIndexOf($startMarker, [StringComparison]::Ordinal) -ne $start) {
        throw "Cannot isolate the current $probe archive block; update the test boundary explicitly."
    }
    $archiveBlock = [ScriptBlock]::Create($source.Substring($start, $end - $start))
    $tokens = $null; $errors = $null
    $ast = [Management.Automation.Language.Parser]::ParseInput($source, [ref]$tokens, [ref]$errors)
    if ($errors.Count) { throw "Runner parse errors: $probe" }
    $failure = @($ast.FindAll({ param($node)
        $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq 'Set-RunFailure'
    }, $false))
    if ($failure.Count -ne 1) { throw "Expected one Set-RunFailure in $probe" }
    . ([ScriptBlock]::Create($failure[0].Extent.Text))
    Microsoft.PowerShell.Management\Copy-Item -LiteralPath $runner -Destination $testDir
    $runnerHash = (Get-FileHash -LiteralPath $runner -Algorithm SHA256).Hash
    foreach ($fault in @('none','partial-copy','corrupt-evidence','final-result','failed-child')) {
        $runName = "$probe-$fault"
        $runDir = Join-Path $testDir "$runName-local"
        $ArchiveRoot = Join-Path $testDir "$runName-archive"
        $archiveDir = Join-Path $ArchiveRoot $runName
        New-Item -ItemType Directory -Path $runDir | Out-Null
        [IO.File]::WriteAllText((Join-Path $runDir 'a-evidence.bin'), 'synthetic evidence A')
        [IO.File]::WriteAllText((Join-Path $runDir 'z-evidence.bin'), 'synthetic evidence Z')
        $result = [ordered]@{ Completed = $false; ExitCode = 1
            Errors = @()
            Scope = 'Synthetic archive unit test, not a GPU result.' }
        if ($fault -eq 'failed-child') { $result.Errors = @('Synthetic child failure') }
        $script:FaultMode = $fault; $script:FaultHit = $false; $script:PrematureSuccess = $false
        & $archiveBlock
        if ($script:PrematureSuccess) { throw "Success published before evidence completion: $runName" }
        $expectedExit = if ($fault -eq 'none') { 0 } else { 1 }
        $injected = $fault -notin @('none','failed-child')
        if ($injected -ne $script:FaultHit) { throw "Fault injection was not exercised: $runName" }
        foreach ($directory in @($runDir,$archiveDir)) {
            $saved = Get-Content -LiteralPath (Join-Path $directory 'result.json') -Raw | ConvertFrom-Json
            if ($saved.ExitCode -ne $expectedExit -or $saved.Completed -ne ($expectedExit -eq 0)) {
                throw "Incorrect receipt after $runName in $directory"
            }
            if ($expectedExit -ne 0 -and !$saved.Errors.Count) { throw "Missing failure diagnosis: $runName" }
            if ($fault -in @('none','failed-child')) {
                foreach ($entry in $saved.Evidence) {
                    $path = Join-Path $directory $entry.Path
                    if ((Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash -ine $entry.SHA256) {
                        throw "Evidence mismatch: $path"
                    }
                }
            }
        }
        $cases += [ordered]@{ Probe = $probe; SyntheticChildExit = $(if ($fault -eq 'failed-child') { 1 } else { 0 })
            Fault = $fault; ExpectedExit = $expectedExit; RunnerSHA256 = $runnerHash; Passed = $true }
    }
}
[ordered]@{ UTC = [DateTime]::UtcNow.ToString('o'); TestScriptSHA256 = $testScriptHash
    Scope = 'Synthetic archive-stage unit tests only; no native runtime or GPU work.'
    Cases = $cases; Passed = $true } | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $testDir 'test-result.json') -Encoding UTF8
Write-Output "PASS: $($cases.Count) synthetic archive cases; $testDir"
