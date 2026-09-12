[CmdletBinding()]
param(
    [string]$CommonScript = "",
    [string]$IcdScript = ""
)

# Real Registry-provider regression tests. Only a uniquely named HKCU subtree
# is changed; no adapter, machine-wide registration, or administrator is needed.
Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
# Windows PowerShell -File does not populate PSScriptRoot while evaluating
# parameter defaults; resolve repository-relative defaults after binding.
if (-not $CommonScript) { $CommonScript = Join-Path $PSScriptRoot "Helios-PackageCommon.ps1" }
if (-not $IcdScript) { $IcdScript = Join-Path $PSScriptRoot "..\..\tools\install-helios-icd.ps1" }
if (-not (Get-PSProvider -PSProvider Registry -ErrorAction SilentlyContinue)) {
    throw "These tests require the real Windows Registry provider."
}
. $CommonScript

function Assert-True([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw "Registry regression failed: $Message" }
}

function Assert-Snapshot([string]$Path, [string]$Name, $Expected) {
    $actual = Get-HeliosRegistrySnapshot $Path $Name
    Assert-True ($actual.exists -eq $Expected.exists) "$Name existence"
    if (-not $Expected.exists) { return }
    Assert-True ($actual.kind -ceq $Expected.kind) "$Name registry kind"
    $actualValues = @($actual.value)
    $expectedValues = @($Expected.value)
    Assert-True ($actualValues.Count -eq $expectedValues.Count) "$Name value count"
    for ($i = 0; $i -lt $expectedValues.Count; ++$i) {
        Assert-True ([string]::Equals([string]$actualValues[$i], [string]$expectedValues[$i], [StringComparison]::Ordinal)) "$Name value/slot $i"
    }
}

function Assert-UnrelatedState([string]$Path) {
    Assert-True ((Get-Item -LiteralPath $Path).GetValue("DriverVersion") -ceq "unrelated-version") "unrelated adapter value survived"
    $child = Join-Path $Path "UnrelatedChild"
    Assert-True (Test-Path -LiteralPath $child) "unrelated child key survived"
    Assert-True ((Get-Item -LiteralPath $child).GetValue("Keep") -ceq "child-value") "unrelated child value survived"
}

$relativeRoot = "Software\Helios.RegistryRegression." + [guid]::NewGuid().ToString("N")
$root = "HKCU:\$relativeRoot"
Assert-True (-not (Test-Path -LiteralPath $root)) "fixture root must be new"
try {
    # CreateSubKey deliberately initializes the fixture without New-Item -Force.
    $key = [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey("$relativeRoot\Adapter")
    try {
        $key.SetValue("DriverVersion", "unrelated-version", [Microsoft.Win32.RegistryValueKind]::String)
        $child = $key.CreateSubKey("UnrelatedChild")
        try { $child.SetValue("Keep", "child-value") } finally { $child.Dispose() }
        $original = @{
            UserModeDriverName = [string[]]@("native11", "native11", "native11", "native12")
            UserModeDriverNameWoW = [string[]]@("wow11", "wow11", "wow11", "wow12")
            InstalledDisplayDrivers = [string[]]@("native11", "native12", "wow11", "wow12")
        }
        foreach ($name in $original.Keys) { $key.SetValue($name, $original[$name], [Microsoft.Win32.RegistryValueKind]::MultiString) }
        $key.SetValue("OpenGLVersion", 2, [Microsoft.Win32.RegistryValueKind]::DWord)
        $key.SetValue("Binary", [byte[]]@(0, 1, 127, 255), [Microsoft.Win32.RegistryValueKind]::Binary)
        $key.SetValue("Expanded", '%SystemRoot%\unchanged', [Microsoft.Win32.RegistryValueKind]::ExpandString)
        $key.SetValue("Single", [string[]]@("only"), [Microsoft.Win32.RegistryValueKind]::MultiString)
        $key.SetValue("Empty", [string[]]@(), [Microsoft.Win32.RegistryValueKind]::MultiString)
    } finally { $key.Dispose() }
    $adapter = Join-Path $root "Adapter"
    $names = @("UserModeDriverName", "UserModeDriverNameWoW", "InstalledDisplayDrivers", "OpenGLVersion", "Binary", "Expanded", "Single", "Empty")
    $snapshots = @{}
    foreach ($name in $names) {
        # Match install-state.json and the actual failing rollback: writing an
        # OrderedDictionary and reading it back produces PSCustomObject/Object[].
        $snapshots[$name] = Get-HeliosRegistrySnapshot $adapter $name | ConvertTo-Json -Depth 5 | ConvertFrom-Json
        New-ItemProperty -LiteralPath $adapter -Name $name -Value "trial" -PropertyType String -Force | Out-Null
    }
    Assert-True ($snapshots.Expanded.value -ceq '%SystemRoot%\unchanged') "ExpandString snapshot remains unexpanded"
    $restored = @()
    foreach ($name in $names) {
        Restore-HeliosRegistrySnapshot $adapter $name $snapshots[$name]
        $restored += $name
        Assert-UnrelatedState $adapter
        foreach ($previous in $restored) { Assert-Snapshot $adapter $previous $snapshots[$previous] }
    }
    # Restoring the same serialized snapshots repeatedly must also be safe.
    foreach ($name in $names) { Restore-HeliosRegistrySnapshot $adapter $name $snapshots[$name] }
    Assert-UnrelatedState $adapter
    foreach ($name in $names) { Assert-Snapshot $adapter $name $snapshots[$name] }

    $absent = Get-HeliosRegistrySnapshot $adapter "Temporary" | ConvertTo-Json | ConvertFrom-Json
    New-ItemProperty -LiteralPath $adapter -Name "Temporary" -Value 7 -PropertyType DWord | Out-Null
    Restore-HeliosRegistrySnapshot $adapter "Temporary" $absent
    Restore-HeliosRegistrySnapshot $adapter "Temporary" $absent
    Assert-Snapshot $adapter "Temporary" $absent
    Assert-UnrelatedState $adapter

    $missing = Join-Path $root "Missing\Nested"
    Restore-HeliosRegistrySnapshot $missing "Absent" $absent
    Assert-True (-not (Test-Path -LiteralPath $missing)) "absent snapshot must not create a key"
    Restore-HeliosRegistrySnapshot $missing "UserModeDriverName" $snapshots.UserModeDriverName
    Assert-Snapshot $missing "UserModeDriverName" $snapshots.UserModeDriverName

    # Exercise the installer's actual key-creation guards and value writes,
    # redirected to HKCU fixtures. No mock replaces Registry-provider behavior.
    $tokens = $null; $parseErrors = $null
    $installer = [Management.Automation.Language.Parser]::ParseFile(
        (Join-Path $PSScriptRoot "Install-Helios.ps1"), [ref]$tokens, [ref]$parseErrors)
    Assert-True ($parseErrors.Count -eq 0) "installer parses"
    foreach ($variable in @("vulkanRegistry", "vulkanRegistryX86", "openClRegistry")) {
        $guard = @($installer.FindAll({ param($node)
            $node -is [Management.Automation.Language.CommandAst] -and
            $node.GetCommandName() -eq "Ensure-HeliosRegistryKey" -and
            $node.Extent.Text -ceq "Ensure-HeliosRegistryKey `$$variable"
        }, $true))
        $write = @($installer.FindAll({ param($node)
            $node -is [Management.Automation.Language.CommandAst] -and
            $node.GetCommandName() -eq "New-ItemProperty" -and
            $node.Extent.Text.StartsWith("New-ItemProperty -LiteralPath `$$variable ")
        }, $true))
        Assert-True ($guard.Count -eq 1 -and $write.Count -eq 1) "$variable has one guarded registration"
        $registration = [scriptblock]::Create($guard[0].Extent.Text + "`n" + $write[0].Extent.Text + " | Out-Null")
        $registryPath = Join-Path $root "$variable\MissingParent\Drivers"
        Set-Variable -Name $variable -Value $registryPath
        $vulkanManifestPath = "helios-native.json"; $vulkanManifestX86Path = "helios-wow.json"; $clvkPath = "clvk.dll"
        & $registration
        New-ItemProperty -LiteralPath $registryPath -Name "OtherVendor" -Value 77 -PropertyType DWord | Out-Null
        $child = [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey("$relativeRoot\$variable\MissingParent\Drivers\OtherVendorChild")
        try { $child.SetValue("Keep", "vendor-child") } finally { $child.Dispose() }
        & $registration
        Assert-True ((Get-Item -LiteralPath $registryPath).GetValue("OtherVendor") -eq 77) "$variable retains another vendor"
        Assert-True ((Get-Item -LiteralPath (Join-Path $registryPath "OtherVendorChild")).GetValue("Keep") -ceq "vendor-child") "$variable retains another vendor's child"
    }
    # The standalone ICD installer has no dependency on packaged helpers.
    # Execute its own actual initializer against a separate missing hierarchy.
    $icd = [Management.Automation.Language.Parser]::ParseFile($IcdScript, [ref]$tokens, [ref]$parseErrors)
    Assert-True ($parseErrors.Count -eq 0) "standalone ICD installer parses"
    $definition = $icd.Find({ param($node)
        $node -is [Management.Automation.Language.FunctionDefinitionAst] -and
        $node.Name -eq "Ensure-HeliosIcdRegistryKey"
    }, $true)
    Assert-True ($null -ne $definition) "standalone ICD initializer exists"
    Invoke-Expression $definition.Extent.Text
    $icdKey = Join-Path $root "Standalone\Missing\Drivers"
    Ensure-HeliosIcdRegistryKey $icdKey
    New-ItemProperty -LiteralPath $icdKey -Name "OtherVendor" -Value 88 -PropertyType DWord | Out-Null
    Ensure-HeliosIcdRegistryKey $icdKey
    Assert-True ((Get-Item -LiteralPath $icdKey).GetValue("OtherVendor") -eq 88) "standalone ICD initializer preserves existing values"
    Write-Host "PASS: real HKCU sequential/serialized/repeated snapshot restoration, unrelated values/subkeys, missing keys, and all three installer registrations."
} finally {
    # The randomized subtree is the only cleanup target, even on an assertion.
    if (Test-Path -LiteralPath $root) { Remove-Item -LiteralPath $root -Recurse -Force }
}
