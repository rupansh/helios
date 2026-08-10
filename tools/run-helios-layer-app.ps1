<#
.SYNOPSIS
  Run a native Vulkan application through VK_LAYER_HELIOS_present, in the
  interactive desktop session, UNELEVATED, with the layer scoped to that one
  process.

.DESCRIPTION
  Two independent reasons this cannot be a plain win_exec/SSH invocation, both
  measured on this box on 2026-08-10:

  1. ⛔ THE VULKAN LOADER IGNORES ITS ENVIRONMENT VARIABLES IN AN ELEVATED
     PROCESS. win_exec/SSH lands elevated (verified: IsInRole(Administrator) =
     True). Under it, VK_LAYER_PATH, VK_ADD_IMPLICIT_LAYER_PATH,
     VK_INSTANCE_LAYERS, VK_LOADER_LAYERS_ENABLE and VK_DRIVER_FILES are all
     dropped by loader_secure_getenv. The failure is SILENT: with
     VK_LOADER_DEBUG=layer the loader prints every registry directory it
     searches and simply never mentions the env-var path at all. A layer staged
     this way looks exactly like a layer that failed to build.

     ⚠ The same trap sits in install-helios-icd.ps1's smoke test, which sets
     VK_DRIVER_FILES before running vulkaninfo. That has always been inert; it
     passes because the ICD is also registered in HKLM.

  2. SSH lands in session 0, which has no desktop. Anything that opens a window
     — which is the entire point of a WSI layer — needs the interactive
     session. This is the same schtasks rule the benchmarks follow.

  So: the app runs from a scheduled task, as the interactive user, at RunLevel
  Limited (unelevated), with the layer directory handed to it through
  VK_ADD_IMPLICIT_LAYER_PATH. Nothing machine-wide is registered, so dwm and
  dxvk-helios never see the layer (§2 item 8's acyclicity rule).

.EXAMPLE
  powershell -File tools\run-helios-layer-app.ps1 `
      -App C:\VulkanSDK\1.4.350.0\Bin\vkcube.exe -AppArgs '--c','120'
#>
param(
  [Parameter(Mandatory = $true)]
  [string]$App,
  [string[]]$AppArgs = @(),
  [string]$LayerDir = "C:\ProgramData\HeliosVulkanLayer",
  [string]$RunDir = "C:\Users\Rupansh\helios-layer-runs",
  [string]$TaskName = "helios_layer_app",
  [ValidateSet("off", "on", "trace")]
  [string]$LayerDebug = "on",
  # VK_LOADER_DEBUG value; "layer" is enough to prove insertion, "all" is loud.
  [string]$LoaderDebug = "layer",
  [int]$TimeoutSeconds = 120,
  # The control arm. Runs the identical task, in the identical session, at the
  # identical elevation, WITHOUT VK_ADD_IMPLICIT_LAYER_PATH. Anything the layer
  # is blamed for has to survive a comparison against this.
  [switch]$NoLayer,
  [switch]$KeepTask
)

$ErrorActionPreference = "Stop"

if (-not (Test-Path -LiteralPath $App -PathType Leaf)) { throw "App not found: $App" }
if (-not (Test-Path -LiteralPath $LayerDir -PathType Container)) {
  throw "Layer directory not found: $LayerDir  (run tools\install-helios-present-layer.ps1 first)"
}
$layerManifest = Join-Path $LayerDir "VkLayer_HELIOS_present.json"
if (-not (Test-Path -LiteralPath $layerManifest -PathType Leaf)) {
  throw "Layer manifest not found: $layerManifest  (run tools\install-helios-present-layer.ps1 first)"
}

$stamp = Get-Date -Format "yyyyMMdd-HHmmss"
$runDir = Join-Path $RunDir "$([IO.Path]::GetFileNameWithoutExtension($App))-$stamp"
New-Item -ItemType Directory -Force -Path $runDir | Out-Null
# The task runs as the interactive user; make sure it can write here even when
# this script created the tree from an elevated token.
$acl = Get-Acl $RunDir
$acl.SetAccessRule((New-Object Security.AccessControl.FileSystemAccessRule(
  "Users", "FullControl", "ContainerInherit,ObjectInherit", "None", "Allow")))
Set-Acl -Path $RunDir -AclObject $acl

$outPath = Join-Path $runDir "stdout.txt"
$errPath = Join-Path $runDir "stderr.txt"
$statusPath = Join-Path $runDir "status.txt"
$wrapper = Join-Path $runDir "run.ps1"

$argLiteral = if ($AppArgs.Count -gt 0) {
  "@(" + (($AppArgs | ForEach-Object { "'" + ($_ -replace "'", "''") + "'" }) -join ",") + ")"
} else {
  "@()"
}

# The wrapper is what actually runs in the session. It is written per run and
# left behind next to its output, so a run is reproducible by hand.
$wrapperText = @"
`$ErrorActionPreference = 'Continue'
# Scoped to this process tree only. Nothing is registered machine-wide.
$(if ($NoLayer) { "# CONTROL ARM: layer path deliberately not set." } else { "`$env:VK_ADD_IMPLICIT_LAYER_PATH = '$LayerDir'" })
`$env:VK_LOADER_DEBUG = '$LoaderDebug'
$(if ($LayerDebug -ne 'off') { "`$env:HELIOS_LAYER_DEBUG = '$LayerDebug'" } else { "Remove-Item Env:\HELIOS_LAYER_DEBUG -ErrorAction SilentlyContinue" })
`$id = [Security.Principal.WindowsIdentity]::GetCurrent()
`$elevated = ([Security.Principal.WindowsPrincipal]`$id).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
"user=`$(`$id.Name) elevated=`$elevated sessionId=`$((Get-Process -Id `$PID).SessionId)" |
    Set-Content -LiteralPath '$statusPath'
if (`$elevated) {
    # Loud: an elevated wrapper silently drops every VK_* variable above, so the
    # run would measure the machine-wide configuration instead of the layer.
    "ELEVATED_WRAPPER: the Vulkan loader will ignore VK_ADD_IMPLICIT_LAYER_PATH" |
        Add-Content -LiteralPath '$statusPath'
}
# A process that never starts — a missing runtime DLL is the usual cause, and
# mingw-built probes need libstdc++/libgcc unless linked static — otherwise
# leaves `$p null, and every later test on it reads exactly like a hang. Report
# it as what it is.
`$p = `$null
try {
    `$p = Start-Process -FilePath '$App' -ArgumentList $argLiteral -NoNewWindow -PassThru ``
            -RedirectStandardOutput '$outPath' -RedirectStandardError '$errPath'
} catch {
    "LAUNCH_FAILED: `$(`$_.Exception.Message)" | Add-Content -LiteralPath '$statusPath'
}
if (-not `$p) {
    "exitCode=(never started)" | Add-Content -LiteralPath '$statusPath'
    return
}
`$p | Wait-Process -Timeout $TimeoutSeconds -ErrorAction SilentlyContinue
if (-not `$p.HasExited) {
    "TIMEOUT after $TimeoutSeconds s; killing" | Add-Content -LiteralPath '$statusPath'
    `$p | Stop-Process -Force -ErrorAction SilentlyContinue
    `$p | Wait-Process -Timeout 10 -ErrorAction SilentlyContinue
}
"exitCode=`$(`$p.ExitCode)" | Add-Content -LiteralPath '$statusPath'
"@
Set-Content -LiteralPath $wrapper -Value $wrapperText -Encoding UTF8

$interactiveUser = (Get-CimInstance Win32_ComputerSystem).UserName
if (-not $interactiveUser) {
  throw "No interactive user is logged on; a WSI layer run needs a desktop session."
}

$action = New-ScheduledTaskAction -Execute "powershell.exe" `
  -Argument "-NoProfile -NonInteractive -ExecutionPolicy Bypass -File `"$wrapper`""
# RunLevel Limited is the load-bearing part: an elevated task reproduces the
# very trap this script exists to avoid.
$principal = New-ScheduledTaskPrincipal -UserId $interactiveUser -LogonType Interactive -RunLevel Limited
$settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries `
  -ExecutionTimeLimit ([TimeSpan]::FromSeconds($TimeoutSeconds + 120))
Register-ScheduledTask -TaskName $TaskName -Action $action -Principal $principal `
  -Settings $settings -Force | Out-Null

$arm = if ($NoLayer) { "CONTROL (no layer)" } else { "LAYER" }
Write-Host "Running $App $($AppArgs -join ' ')  (task $TaskName, user $interactiveUser, unelevated, arm=$arm)"
Start-ScheduledTask -TaskName $TaskName

$deadline = (Get-Date).AddSeconds($TimeoutSeconds + 60)
do {
  Start-Sleep -Milliseconds 500
  $state = (Get-ScheduledTask -TaskName $TaskName).State
} while ($state -ne "Ready" -and (Get-Date) -lt $deadline)

$info = Get-ScheduledTaskInfo -TaskName $TaskName
Write-Host "task state=$state lastTaskResult=0x$('{0:X8}' -f $info.LastTaskResult)"
if (-not $KeepTask) { Unregister-ScheduledTask -TaskName $TaskName -Confirm:$false }

Write-Host ""
Write-Host "=== status ==="
if (Test-Path -LiteralPath $statusPath) { Get-Content -LiteralPath $statusPath } else { Write-Host "(the wrapper never wrote a status file)" }
Write-Host ""
Write-Host "=== app stdout ($outPath) ==="
if (Test-Path -LiteralPath $outPath) { Get-Content -LiteralPath $outPath } else { Write-Host "(none)" }
Write-Host ""
Write-Host "=== app stderr ($errPath) ==="
if (Test-Path -LiteralPath $errPath) { Get-Content -LiteralPath $errPath } else { Write-Host "(none)" }
Write-Host ""
Write-Host "Run directory: $runDir"
