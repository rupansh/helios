<#
.SYNOPSIS
  Admit an already-deployed D9 package only after a cold boot and a visibly
  live WDDM 3.2 DWM desktop.

.DESCRIPTION
  This harness is read-only except for its JSON evidence file. It never installs
  a package, restarts an adapter, reboots Windows, or captures the desktop. A
  build, matching hash, PnP problem code 0, running dwm.exe, empty event log, or
  WDDM 3.2 string is supporting evidence only. The final admission predicate
  also requires the owner to attest that the current desktop is visibly live.

  WDK 28000 is the compile-time header/binding authority, not a guest-OS
  package minimum. FINDINGS.md F1 measured Core DDI 0116 negotiation on build
  26100. The caller therefore supplies the exact authorized target OS build;
  this harness records and checks it without restoring the superseded 28000
  minimum.

  Run this only after deployment and a cold boot have been separately authorized.

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File Z:\tools\d9-cold-dwm-admission.ps1 `
    -InstalledKmdPath C:\Windows\System32\drivers\helios_kmd_render.sys `
    -ExpectedKmdSha256 <64-hex-sha256> `
    -InstalledInfPath C:\Windows\System32\DriverStore\FileRepository\<package>\helios.inf `
    -ExpectedInfSha256 <64-hex-sha256> `
    -ExpectedDriverVersion 22.22.184.0 `
    -ExpectedOsBuild 26100 `
    -MinimumBootUtc 2026-08-13T10:00:00Z `
    -OwnerColdBootConfirmation COLD_BOOT_POWER_CYCLE `
    -OwnerVisibleDesktopConfirmation VISIBLE_DWM_DESKTOP `
    -OwnerVisibleDescription "Taskbar, cursor, and two repainted windows remained responsive"
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string] $InstalledKmdPath,

    [Parameter(Mandatory = $true)]
    [ValidatePattern('^[0-9A-Fa-f]{64}$')]
    [string] $ExpectedKmdSha256,

    [Parameter(Mandatory = $true)]
    [string] $InstalledInfPath,

    [Parameter(Mandatory = $true)]
    [ValidatePattern('^[0-9A-Fa-f]{64}$')]
    [string] $ExpectedInfSha256,

    [Parameter(Mandatory = $true)]
    [string] $ExpectedDriverVersion,

    [Parameter(Mandatory = $true)]
    [ValidatePattern('^[0-9]+$')]
    [string] $ExpectedOsBuild,

    [Parameter(Mandatory = $true)]
    [datetime] $MinimumBootUtc,

    [Parameter(Mandatory = $true)]
    [ValidateSet('COLD_BOOT_POWER_CYCLE')]
    [string] $OwnerColdBootConfirmation,

    [Parameter(Mandatory = $true)]
    [ValidateSet('VISIBLE_DWM_DESKTOP')]
    [string] $OwnerVisibleDesktopConfirmation,

    [Parameter(Mandatory = $true)]
    [ValidateLength(16, 512)]
    [string] $OwnerVisibleDescription,

    [int] $SessionId = (Get-Process -Id $PID).SessionId,

    [string] $EvidencePath = "$env:ProgramData\Helios\d9-cold-dwm-admission.json"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$failures = [System.Collections.Generic.List[string]]::new()
$checks = [ordered]@{}

function Add-D9Failure([string] $Message) {
    $failures.Add($Message)
}

function Get-D9Sha256([string] $Path) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        Add-D9Failure("file is absent: $Path")
        return $null
    }
    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToUpperInvariant()
}

$actualKmdHash = Get-D9Sha256 $InstalledKmdPath
$actualInfHash = Get-D9Sha256 $InstalledInfPath
$checks.installed_kmd_path = $InstalledKmdPath
$checks.installed_kmd_sha256 = $actualKmdHash
$checks.installed_inf_path = $InstalledInfPath
$checks.installed_inf_sha256 = $actualInfHash
if ($actualKmdHash -ne $ExpectedKmdSha256.ToUpperInvariant()) {
    Add-D9Failure('installed KMD SHA-256 does not match the authorized artifact')
}
if ($actualInfHash -ne $ExpectedInfSha256.ToUpperInvariant()) {
    Add-D9Failure('installed INF SHA-256 does not match the authorized package')
}

$os = Get-CimInstance Win32_OperatingSystem
$bootUtc = $os.LastBootUpTime.ToUniversalTime()
$minimumUtc = $MinimumBootUtc.ToUniversalTime()
$checks.boot_utc = $bootUtc.ToString('o')
$checks.minimum_boot_utc = $minimumUtc.ToString('o')
$checks.os_build = [string]$os.BuildNumber
$checks.expected_os_build = $ExpectedOsBuild
$checks.owner_cold_boot_confirmation = $OwnerColdBootConfirmation
if ($bootUtc -lt $minimumUtc) {
    Add-D9Failure('the running Windows instance predates the authorized cold-boot boundary')
}
if ([string]$os.BuildNumber -ne $ExpectedOsBuild) {
    Add-D9Failure("Windows build '$($os.BuildNumber)' is not the authorized target build '$ExpectedOsBuild'")
}
if ($OwnerColdBootConfirmation -ne 'COLD_BOOT_POWER_CYCLE') {
    Add-D9Failure('owner did not attest that this boot followed a cold power cycle')
}

$devices = @(Get-PnpDevice -Class Display -FriendlyName '*Helios*' -ErrorAction Stop)
if ($devices.Count -ne 1) {
    Add-D9Failure("expected exactly one Helios display adapter, found $($devices.Count)")
} else {
    $device = $devices[0]
    $problem = (Get-PnpDeviceProperty -InstanceId $device.InstanceId `
        -KeyName 'DEVPKEY_Device_ProblemCode').Data
    $driverVersion = [string](Get-PnpDeviceProperty -InstanceId $device.InstanceId `
        -KeyName 'DEVPKEY_Device_DriverVersion').Data
    $checks.device_instance_id = $device.InstanceId
    $checks.device_status = [string]$device.Status
    $checks.device_problem_code = [int]$problem
    $checks.driver_version = $driverVersion
    if ([int]$problem -ne 0 -or [string]$device.Status -ne 'OK') {
        Add-D9Failure("Helios PnP state is not clean: status=$($device.Status) problem=$problem")
    }
    if ($driverVersion -ne $ExpectedDriverVersion) {
        Add-D9Failure("installed driver version '$driverVersion' is not '$ExpectedDriverVersion'")
    }
}

$dwm = @(Get-Process dwm -ErrorAction SilentlyContinue | Where-Object {
    $_.SessionId -eq $SessionId
})
$checks.session_id = $SessionId
$checks.dwm_count = $dwm.Count
if ($dwm.Count -ne 1) {
    Add-D9Failure("expected one dwm.exe in session $SessionId, found $($dwm.Count)")
} else {
    $dwmStartUtc = $dwm[0].StartTime.ToUniversalTime()
    $checks.dwm_pid = $dwm[0].Id
    $checks.dwm_start_utc = $dwmStartUtc.ToString('o')
    if ($dwmStartUtc -lt $bootUtc) {
        Add-D9Failure('current-session DWM did not start in this boot instance')
    }
}

$dxdiagPath = Join-Path $env:TEMP ("helios-d9-dxdiag-{0}.txt" -f [guid]::NewGuid())
try {
    $dxdiag = Start-Process -FilePath "$env:SystemRoot\System32\dxdiag.exe" `
        -ArgumentList @('/whql:off', '/t', $dxdiagPath) -PassThru -WindowStyle Hidden
    if (-not $dxdiag.WaitForExit(120000)) {
        Stop-Process -Id $dxdiag.Id -Force -ErrorAction SilentlyContinue
        Add-D9Failure('dxdiag did not finish within 120 seconds')
    } elseif (-not (Test-Path -LiteralPath $dxdiagPath -PathType Leaf)) {
        Add-D9Failure('dxdiag produced no report')
    } else {
        $dxdiagText = Get-Content -LiteralPath $dxdiagPath -Raw
        $blocks = [regex]::Matches(
            $dxdiagText,
            '(?ms)^\s*Card name:\s*(?<name>[^\r\n]+)\r?\n(?<body>.*?)(?=^\s*Card name:|\z)'
        )
        $heliosBlocks = @($blocks | Where-Object { $_.Groups['name'].Value -match 'Helios' })
        $checks.dxdiag_helios_blocks = $heliosBlocks.Count
        if ($heliosBlocks.Count -ne 1) {
            Add-D9Failure("dxdiag expected one Helios display block, found $($heliosBlocks.Count)")
        } else {
            $modelMatch = [regex]::Match(
                $heliosBlocks[0].Groups['body'].Value,
                '(?mi)^\s*Driver Model:\s*(?<model>[^\r\n]+)'
            )
            $driverModel = if ($modelMatch.Success) {
                $modelMatch.Groups['model'].Value.Trim()
            } else {
                '<absent>'
            }
            $checks.dxdiag_driver_model = $driverModel
            if ($driverModel -ne 'WDDM 3.2') {
                Add-D9Failure("Helios dxdiag driver model is '$driverModel', not exact WDDM 3.2")
            }
        }
    }
} finally {
    Remove-Item -LiteralPath $dxdiagPath -Force -ErrorAction SilentlyContinue
}

$eventMatches = [System.Collections.Generic.List[object]]::new()
foreach ($logName in @('System', 'Application')) {
    $events = Get-WinEvent -FilterHashtable @{ LogName = $logName; StartTime = $os.LastBootUpTime } `
        -ErrorAction SilentlyContinue
    foreach ($event in $events) {
        $message = [string]$event.Message
        $oldFailure = $message -match 'CDDisplaySwapChain|E_NOTIMPL|0x80004001'
        $graphicsError = $event.Level -le 2 -and (
            [string]$event.ProviderName -match 'dwm|dxgkrnl|display|bugcheck' -or
            $message -match 'dwm|helios|dxgkrnl|display driver|LiveKernelEvent'
        )
        if ($oldFailure -or $graphicsError) {
            $eventMatches.Add([pscustomobject]@{
                log = $logName
                time_utc = $event.TimeCreated.ToUniversalTime().ToString('o')
                id = $event.Id
                provider = [string]$event.ProviderName
                message = ($message -replace "`r?`n", ' ').Substring(
                    0,
                    [Math]::Min(320, $message.Length)
                )
            })
        }
    }
}
$checks.rejected_event_count = $eventMatches.Count
if ($eventMatches.Count -ne 0) {
    Add-D9Failure("found $($eventMatches.Count) DWM/display failure event(s) since boot")
}

$checks.owner_visible_confirmation = $OwnerVisibleDesktopConfirmation
$checks.owner_visible_description = $OwnerVisibleDescription
$checks.owner_identity = [Security.Principal.WindowsIdentity]::GetCurrent().Name
if ($OwnerVisibleDesktopConfirmation -ne 'VISIBLE_DWM_DESKTOP') {
    Add-D9Failure('owner did not attest a visibly live DWM desktop')
}
if ([string]::IsNullOrWhiteSpace($OwnerVisibleDescription)) {
    Add-D9Failure('owner visible-desktop description is empty')
}

$passed = $failures.Count -eq 0
$evidence = [ordered]@{
    schema = 'helios-d9-cold-dwm-admission-v1'
    observed_utc = [datetime]::UtcNow.ToString('o')
    result = if ($passed) { 'ADMITTED' } else { 'REFUSED' }
    statement = if ($passed) {
        'WDDM 3.2 KMD surface admitted on this cold boot with owner-observed visible DWM startup.'
    } else {
        'WDDM 3.2 KMD surface is not admitted.'
    }
    caveat = 'This result does not establish full HPS2 retirement or production correctness.'
    checks = $checks
    rejected_events = @($eventMatches)
    failures = @($failures)
}

$evidenceDirectory = Split-Path -Parent $EvidencePath
if ($evidenceDirectory -and -not (Test-Path -LiteralPath $evidenceDirectory)) {
    New-Item -ItemType Directory -Path $evidenceDirectory -Force | Out-Null
}
$evidence | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath $EvidencePath -Encoding UTF8

Write-Host $evidence.statement
Write-Host "Evidence: $EvidencePath"
if (-not $passed) {
    $failures | ForEach-Object { Write-Host "REFUSED: $_" }
    exit 1
}
exit 0
