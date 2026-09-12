# Exercise the actual runners' build-verification functions with synthetic
# artifacts. No executable, graphics process or scheduled task is started.
[CmdletBinding()]
param([string]$TestRoot = 'C:\ProgramData\Helios\probe-provenance-tests')
$ErrorActionPreference = 'Stop'
if ($TestRoot -notmatch '^[Cc]:\\') { throw 'TestRoot must use local C: disk.' }
$testDir = Join-Path $TestRoot ((Get-Date -Format 'yyyyMMdd-HHmmss-fff') + '-' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $testDir -Force | Out-Null
Copy-Item -LiteralPath $PSCommandPath -Destination $testDir
$testScriptHash = (Get-FileHash -LiteralPath $PSCommandPath -Algorithm SHA256).Hash

function Invoke-ProvenanceCases([string]$Probe) {
    $runnerName = "d3d12-$Probe-probe.ps1"
    $sourcePath = Join-Path $PSScriptRoot $runnerName
    $source = [IO.File]::ReadAllText($sourcePath)
    $tokens = $null; $errors = $null
    $ast = [Management.Automation.Language.Parser]::ParseInput($source, [ref]$tokens, [ref]$errors)
    if ($errors.Count) { throw "Runner parse errors: $runnerName" }
    Copy-Item -LiteralPath $sourcePath -Destination $testDir
    $sourceHash = (Get-FileHash -LiteralPath $sourcePath -Algorithm SHA256).Hash
    $BuildDir = Join-Path $testDir $Probe
    New-Item -ItemType Directory -Path $BuildDir | Out-Null
    $localRunner = Join-Path $BuildDir $runnerName
    $provenancePath = Join-Path $BuildDir 'build-provenance.json'
    $receiptPath = $provenancePath
    # Use the runner's actual required-file declarations and functions. A
    # contract change must be exercised here rather than copied into a mock.
    $configurationNames = @('sourceNames','artifactNames','inputNames','requiredBuildFiles','buildFileNames')
    foreach ($statement in $ast.EndBlock.Statements) {
        if ($statement -is [Management.Automation.Language.AssignmentStatementAst] -and
                $statement.Left -is [Management.Automation.Language.VariableExpressionAst] -and
                $configurationNames -contains $statement.Left.VariablePath.UserPath) {
            . ([ScriptBlock]::Create($statement.Extent.Text))
        }
    }
    foreach ($name in @('Get-Artifact','Assert-Build')) {
        $definitions = @($ast.FindAll({ param($node)
            $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq $name
        }, $false))
        if ($definitions.Count -ne 1) { throw "Expected one $name definition in $runnerName" }
        . ([ScriptBlock]::Create($definitions[0].Extent.Text))
    }
    $fileNames = switch ($Probe) {
        'stream-output' { $inputNames + $artifactNames }
        'indirect' { $inputNames + $artifactNames }
        'tiled' { $buildFileNames }
        'raytracing' { $artifactNames }
    }
    foreach ($name in $fileNames) {
        $contents = if ($name -eq $runnerName) { $source } else { "Synthetic artifact: $name" }
        [IO.File]::WriteAllText((Join-Path $BuildDir $name), $contents, [Text.Encoding]::UTF8)
    }
    function Write-TestReceipt {
        $records = @($fileNames | ForEach-Object {
            $path = Join-Path $BuildDir $_
            [ordered]@{ Name = $_; Path = $path; Length = (Get-Item -LiteralPath $path).Length
                SHA256 = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash }
        })
        $receipt = [ordered]@{ Schema = 1; BuildSucceeded = $true; Probe = "native-$Probe"
            Marker = $script:ReceiptMarker; Artifacts = $records }
        if ($Probe -in @('stream-output','indirect')) {
            $receipt.Inputs = @($records | Where-Object { $inputNames -contains $_.Name })
            $receipt.Artifacts = @($records | Where-Object { $artifactNames -contains $_.Name })
        }
        $receipt | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $receiptPath -Encoding UTF8
    }
    $cases = @()
    $executedRunnerSource = $ast.Extent.Text
    $script:ReceiptMarker = 'A'
    Write-TestReceipt
    $null = Assert-Build
    $cases += [ordered]@{ Case = 'matching-parsed-A'; Passed = $true }

    # All disk artifacts and the receipt now consistently describe B. The
    # previously parsed A must still be rejected before any task is started.
    $sourceB = $source + "`n# Synthetic revision B changes the parsed source.`n"
    [IO.File]::WriteAllText($localRunner, $sourceB, [Text.Encoding]::UTF8)
    $script:ReceiptMarker = 'B'
    Write-TestReceipt
    $rejection = $null
    try { $null = Assert-Build } catch { $rejection = $_.Exception.Message }
    if ($rejection -notlike 'Parsed runner differs*') { throw "Stale parsed A was not specifically rejected: $runnerName ($rejection)" }
    $cases += [ordered]@{ Case = 'parsed-A-with-complete-build-B'; Passed = $true; Error = $rejection }

    $executedRunnerSource = $sourceB
    $null = Assert-Build
    $cases += [ordered]@{ Case = 'matching-parsed-B'; Passed = $true }

    if ($Probe -eq 'raytracing') {
        $before = [IO.File]::ReadAllBytes($receiptPath)
        $verifiedReceiptBytes = $null
        $verified = Assert-Build ([ref]$verifiedReceiptBytes)
        $script:ReceiptMarker = 'replacement-after-verification'
        Write-TestReceipt
        $runDir = Join-Path $BuildDir 'snapshot'
        New-Item -ItemType Directory -Path $runDir | Out-Null
        $writes = @($ast.FindAll({ param($node)
            $node -is [Management.Automation.Language.InvokeMemberExpressionAst] -and
                $node.Extent.Text -like '[[]IO.File[]]::WriteAllBytes*' -and
                $node.Extent.Text.Contains('$verifiedReceiptBytes')
        }, $true))
        if ($writes.Count -ne 1) { throw 'Expected one actual verified-receipt snapshot write.' }
        . ([ScriptBlock]::Create($writes[0].Extent.Text))
        $captured = [IO.File]::ReadAllBytes((Join-Path $runDir 'build-provenance.json'))
        if ([Convert]::ToBase64String($before) -cne [Convert]::ToBase64String($captured) -or
                $verified.Marker -ne 'B' -or
                (Get-Content -LiteralPath $receiptPath -Raw | ConvertFrom-Json).Marker -ne 'replacement-after-verification') {
            throw 'DXR archived a replaced receipt instead of the exact bytes it verified.'
        }
        $cases += [ordered]@{ Case = 'receipt-replaced-after-verification'; Passed = $true }
    }
    # A matching parsed source alone must not bypass the artifact digest.
    [IO.File]::AppendAllText($localRunner, "`n# Unreceipted modification.`n")
    $executedRunnerSource = [IO.File]::ReadAllText($localRunner)
    $rejection = $null
    try { $null = Assert-Build } catch { $rejection = $_.Exception.Message }
    if (!$rejection) { throw "Unreceipted runner modification was accepted: $runnerName" }
    $cases += [ordered]@{ Case = 'matching-source-with-invalid-digest'; Passed = $true; Error = $rejection }
    [ordered]@{ Probe = $Probe; RunnerSHA256 = $sourceHash; Cases = $cases }
}

$results = @('raytracing','stream-output','tiled','indirect' | ForEach-Object { Invoke-ProvenanceCases $_ })
$caseCount = @($results | ForEach-Object { $_.Cases }).Count
[ordered]@{ UTC = [DateTime]::UtcNow.ToString('o'); PowerShellVersion = $PSVersionTable.PSVersion.ToString()
    TestScriptSHA256 = $testScriptHash; Scope = 'Synthetic build-verification tests only; no task or GPU execution.'
    Probes = $results; Passed = $true } | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath (Join-Path $testDir 'test-result.json') -Encoding UTF8
Write-Output "PASS: $caseCount synthetic provenance cases; $testDir"
