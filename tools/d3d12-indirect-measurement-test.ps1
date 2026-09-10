# Exercise the actual measurement grader with complete and corrupted synthetic
# receipts. This starts no executable, scheduled task or GPU workload.
[CmdletBinding()]
param([string]$TestRoot = 'C:\ProgramData\Helios\indirect-measurement-tests')
$ErrorActionPreference = 'Stop'
if ($TestRoot -notmatch '^[Cc]:\\') { throw 'TestRoot must use local C: disk.' }
$testDir = Join-Path $TestRoot ((Get-Date -Format 'yyyyMMdd-HHmmss-fff') + '-' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $testDir -Force | Out-Null
Copy-Item -LiteralPath $PSCommandPath -Destination $testDir
$runner = Join-Path $PSScriptRoot 'd3d12-indirect-probe.ps1'
$sourceBytes = [IO.File]::ReadAllBytes($runner)
$source = [Text.Encoding]::UTF8.GetString($sourceBytes).TrimStart([char]0xfeff)
[IO.File]::WriteAllBytes((Join-Path $testDir 'd3d12-indirect-probe.ps1'), $sourceBytes)
$tokens = $null; $errors = $null
$ast = [Management.Automation.Language.Parser]::ParseInput($source, [ref]$tokens, [ref]$errors)
if ($errors.Count) { throw 'Runner parse errors.' }
$definitions = @($ast.FindAll({ param($node)
    $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq 'Assert-Measurement'
}, $false))
if ($definitions.Count -ne 1) { throw 'Expected one actual measurement grader.' }
. ([ScriptBlock]::Create($definitions[0].Extent.Text))
$lines = New-Object 'Collections.Generic.List[string]'
$lines.Add('PERF_CONFIG,max_commands=1024,width=1025,warm_samples=5,cpu_frequency=10000000,gpu_frequency=1000000000')
$pixels = New-Object byte[] (36 * 1025 * 4)
$ordinal = 0
foreach ($count in @(1024,1,0)) { foreach ($arm in @('direct','indirect')) { foreach ($sample in -1..4) {
    $phase = if ($sample -ge 0) { 'warm' } elseif ($count -eq 1024) { 'first-use' } else { 'warmup' }
    $seed = 1009 + ($sample + 1) * 17
    $begin = 100000 + $ordinal * 1000
    $lines.Add("PERF,ordinal=$ordinal,arm=$arm,count=$count,sample=$sample,phase=$phase,seed=$seed,record_ticks=10,close_ticks=20,submit_signal_ticks=30,completion_wait_ticks=40,gpu_begin=$begin,gpu_end=$($begin+500),fence=$($ordinal+1)")
    for ($pixel = 0; $pixel -lt 1025; ++$pixel) {
        # Construct the separate VS/PS contributions, rather than copying the
        # grader's reduced 23*seed+41*pixel expression.
        [UInt32]$value = if ($pixel -lt $count -or $pixel -eq 1024) {
            2 * ($seed + 3 * $pixel) + $pixel + 3 * (7 * $seed + 11 * $pixel) + $pixel
        } else { 0 }
        [Array]::Copy([BitConverter]::GetBytes($value), 0, $pixels, ($ordinal * 1025 + $pixel) * 4, 4)
    }
    ++$ordinal
} } }
$lines.Add('PASS,indirect-measurement,36 samples,36900 words')
$validText = $lines -join "`n"
$cases = New-Object 'Collections.Generic.List[object]'
function Test-Receipt([string]$Name, [string]$Text, [byte[]]$Bytes, [string]$ExpectedError = '') {
    $directory = Join-Path $testDir $Name
    New-Item -ItemType Directory -Path $directory | Out-Null
    $path = Join-Path $directory 'performance-readbacks.bin'
    [IO.File]::WriteAllBytes($path, $Bytes)
    [IO.File]::WriteAllText((Join-Path $directory 'stdout.txt'), $Text)
    $errorMessage = $null
    try { $null = Assert-Measurement $Text $path } catch { $errorMessage = $_.Exception.Message }
    if ($ExpectedError) {
        if (!$errorMessage -or $errorMessage -notlike $ExpectedError) {
            throw "Expected specific rejection for $Name : $ExpectedError; actual: $errorMessage"
        }
    } elseif ($errorMessage) { throw "Unexpected rejection for $Name : $errorMessage" }
    $cases.Add([ordered]@{ Case = $Name; Passed = $true; Error = $errorMessage })
}
Test-Receipt 'complete' $validText $pixels
Test-Receipt 'missing-completion' ($lines[0..36] -join "`n") $pixels 'Missing or duplicate measurement completion marker.'
Test-Receipt 'duplicate-completion' ($validText + "`n" + $lines[37]) $pixels 'Missing or duplicate measurement completion marker.'
Test-Receipt 'missing-sample' (($lines[0..35] + $lines[37]) -join "`n") $pixels 'Expected 36 measurement samples*'
$duplicate = $lines.ToArray(); $duplicate[2] = $duplicate[1]
Test-Receipt 'duplicate-sample' ($duplicate -join "`n") $pixels 'Missing, repeated or reordered measurement sample 1.'
$reordered = $lines.ToArray(); $reordered[1] = $lines[2]; $reordered[2] = $lines[1]
Test-Receipt 'reordered-samples' ($reordered -join "`n") $pixels 'Missing, repeated or reordered measurement sample 0.'
Test-Receipt 'zero-clock' ($validText.Replace('cpu_frequency=10000000,', 'cpu_frequency=0,')) $pixels 'Zero measurement clock frequency.'
Test-Receipt 'invalid-config' ($validText.Replace('width=1025,', 'width=1024,')) $pixels 'Invalid measurement configuration.'
Test-Receipt 'wrong-seed' ($validText.Replace('seed=1009,', 'seed=1010,')) $pixels 'Missing, repeated or reordered measurement sample 0.'
Test-Receipt 'negative-ticks' ($validText.Replace('record_ticks=10,', 'record_ticks=-1,')) $pixels 'Malformed measurement timing 0.'
Test-Receipt 'equal-timestamps' ($validText.Replace('gpu_end=100500,', 'gpu_end=100000,')) $pixels 'Invalid timestamp/fence for sample 0.'
Test-Receipt 'wrong-fence' ($validText.Replace('fence=1' + "`n", 'fence=0' + "`n")) $pixels 'Invalid timestamp/fence for sample 0.'
Test-Receipt 'truncated-witness' $validText $pixels[0..($pixels.Length-2)] 'Incomplete or extra pixel witness data.'
Test-Receipt 'extra-witness' $validText ([byte[]]($pixels + [byte]0)) 'Incomplete or extra pixel witness data.'
$wrong = [byte[]]$pixels.Clone(); $wrong[0] = $wrong[0] -bxor 1
Test-Receipt 'wrong-active-pixel' $validText $wrong 'GPU pixel mismatch: sample=0 pixel=0 *'
$wrong = [byte[]]$pixels.Clone(); $wrong[(35 * 1025 + 1024) * 4] = 0
Test-Receipt 'wrong-zero-count-suffix' $validText $wrong 'GPU pixel mismatch: sample=35 pixel=1024 *'
$wrong = [byte[]]$pixels.Clone(); $wrong[35 * 1025 * 4] = 1
Test-Receipt 'nonzero-inactive-pixel' $validText $wrong 'GPU pixel mismatch: sample=35 pixel=0 *'
[ordered]@{ UTC = [DateTime]::UtcNow.ToString('o'); Passed = $true; Cases = $cases.ToArray()
    RunnerSHA256 = (Get-FileHash -LiteralPath (Join-Path $testDir 'd3d12-indirect-probe.ps1') -Algorithm SHA256).Hash
    TestSHA256 = (Get-FileHash -LiteralPath (Join-Path $testDir (Split-Path $PSCommandPath -Leaf)) -Algorithm SHA256).Hash
    Scope = 'Synthetic corruption tests of the actual measurement grader; no native runtime or GPU execution.'
} | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $testDir 'test-result.json') -Encoding UTF8
Write-Output "PASS: $($cases.Count) synthetic measurement cases; $testDir"
