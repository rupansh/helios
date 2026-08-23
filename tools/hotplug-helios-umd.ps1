param(
  # RELEASE by default, matching install-helios-kmd.ps1. Pass
  # -UmdDll ...\target\debug\helios_umd.dll for a deliberate debug deploy.
  [string]$UmdDll = "C:\Users\Rupansh\helios-vgpu\umd\target\release\helios_umd.dll",
  # The D3D12 UMD (`DECISIONS.md` D3 -- UserModeDriverName slot 3).
  #
  # ⛔ EMPTY BY DEFAULT, and that is deliberate. Passing it is an explicit,
  # opt-in act that rewrites slot 3 of a REG_MULTI_SZ dwm resolves at device
  # start. When empty this script's registry writes are bit-identical to what
  # they were before D3D12 existed, so a routine D3D11 deploy cannot acquire a
  # D3D12 path by accident.
  #
  # ⚠ Wiring slot 3 for real is stage S5 (`ARCHITECTURE.md` §11), which also
  # deletes `umd`'s own refusing OpenAdapter12 export and lands the `UmdD3D12`
  # kill switch in the same commit. Until then `helios_umd12.dll` refuses with
  # DXGI_ERROR_UNSUPPORTED, so registering it is inert -- but it is still a
  # change to what dwm resolves, so it must be asked for.
  [string]$Umd12Dll = "",
  # Both UMDs import the lower ICD by this unversioned module name. Keep the
  # exact matching build beside the content-addressed UMDs in ProgramData and
  # beside the package UMDs in the active DriverStore directory.
  [string]$VulkanDll = "C:\Users\Rupansh\helios-mesa-build\src\virtio\vulkan\vulkan_virtio.dll",
  [ValidateSet("ProgramData", "DriverStore", "PackageUpgrade")]
  [string]$Mode = "ProgramData",
  [string]$PackageDir = "C:\Users\Rupansh\helios-vgpu\kmd_render\target\debug\helios_kmd_render_package",
  [string]$ProgramDataDir = "C:\ProgramData\HeliosUmd",
  [string]$InstanceId = "",
  [string]$Probe = "C:\Users\Rupansh\helios-probe\d3d11_devicecreate_probe.exe",
  [switch]$KillUmdUsers,
  [switch]$RestartDevice,
  [switch]$ForceDriverStoreEdit,
  [switch]$NoProbe,
  [switch]$PlanOnly
)

. "$PSScriptRoot\helios-deploy-common.ps1"

function Stop-UmdUsers([string]$DllPath) {
  $paths = @()
  if ($DllPath) { $paths += $DllPath }
  if (Test-Path -LiteralPath $ProgramDataDir -PathType Container) {
    $paths += @(Get-ChildItem -LiteralPath $ProgramDataDir -Filter "helios_umd*.dll" -ErrorAction SilentlyContinue | ForEach-Object FullName)
  }

  $seen = @{}
  foreach ($path in ($paths | Sort-Object -Unique)) {
    foreach ($u in @(Get-HeliosFileUsers $path)) {
      if ($seen.ContainsKey($u.Id)) { continue }
      $seen[$u.Id] = $true
      if ($u.ProcessName -notin @("Idle", "System", "Registry", "smss", "csrss", "wininit", "services", "lsass", "fontdrvhost")) {
        Write-Host "Stopping UMD user $($u.ProcessName)[$($u.Id)] using $path"
        Stop-Process -Id $u.Id -Force -ErrorAction SilentlyContinue
      }
    }
  }
}

Assert-HeliosAdmin
Stop-LookingGlassHostService
$cleared = Clear-HeliosPendingRenames
if ($cleared -gt 0) { Write-Host "Removed $cleared stale Helios pending rename operation(s)." }

if (-not (Test-Path -LiteralPath $UmdDll -PathType Leaf)) { throw "UMD DLL not found: $UmdDll" }
$deployVulkan = $Mode -eq "ProgramData"
if ($deployVulkan -and -not (Test-Path -LiteralPath $VulkanDll -PathType Leaf)) {
  throw "UMD lower-ICD companion not found: $VulkanDll"
}
$deployUmd12 = -not [string]::IsNullOrWhiteSpace($Umd12Dll)
if ($deployUmd12) {
  if (-not (Test-Path -LiteralPath $Umd12Dll -PathType Leaf)) { throw "D3D12 UMD DLL not found: $Umd12Dll" }
  if ($Mode -ne "ProgramData") {
    # DriverStore/PackageUpgrade ship helios_umd12.dll through the INF, which is
    # stage S5 work and not this script's to fake.
    throw "-Umd12Dll is only supported in -Mode ProgramData; the DriverStore/package path ships the D3D12 UMD via the INF (ARCHITECTURE.md S5)"
  }
}
$id = Get-HeliosInstanceId $InstanceId
$srcHash = Get-HeliosFileHash $UmdDll
$src12Hash = if ($deployUmd12) { Get-HeliosFileHash $Umd12Dll } else { "" }
$vulkanHash = if ($deployVulkan) { Get-HeliosFileHash $VulkanDll } else { "" }
$classKey = Get-HeliosClassKey $id
$activeInf = Get-HeliosActiveInfName $id
$store = Get-HeliosActiveStoreDir $id $activeInf
$programDataDll = Join-Path $ProgramDataDir ("helios_umd_{0}.dll" -f $srcHash.Substring(0, 16).ToLowerInvariant())
$programData12Dll = if ($deployUmd12) {
  Join-Path $ProgramDataDir ("helios_umd12_{0}.dll" -f $src12Hash.Substring(0, 16).ToLowerInvariant())
} else { "" }
$programDataVulkanDll = Join-Path $ProgramDataDir "vulkan_virtio.dll"
$driverStoreVulkanDll = Join-Path $store "vulkan_virtio.dll"

Write-HeliosPlan "Helios UMD hotplug" @{
  Mode = $Mode
  Source = $UmdDll
  SourceHash = $srcHash
  Source12 = if ($deployUmd12) { $Umd12Dll } else { "(not deployed)" }
  Source12Hash = if ($deployUmd12) { $src12Hash } else { "(n/a)" }
  VulkanSource = if ($deployVulkan) { $VulkanDll } else { "(package-owned)" }
  VulkanSourceHash = if ($deployVulkan) { $vulkanHash } else { "(n/a)" }
  Instance = $id
  ClassKey = $classKey
  ActiveInf = $activeInf
  DriverStore = $store
  DriverStoreVulkan = $driverStoreVulkanDll
  ProgramDataDll = $programDataDll
  RestartDevice = [bool]$RestartDevice
  ForceDriverStoreEdit = [bool]$ForceDriverStoreEdit
}
if ($PlanOnly) { return }

if ($Mode -eq "PackageUpgrade") {
  $inf = Join-Path $PackageDir "helios_kmd_render.inf"
  if (-not (Test-Path -LiteralPath $inf -PathType Leaf)) { throw "Package INF not found: $inf" }
  Invoke-HeliosPnpUtil @("/add-driver", $inf, "/install") 120 | Out-Null
  if ($RestartDevice) {
    Invoke-HeliosPnpUtil @("/restart-device", $id) 90 | Out-Null
  } else {
    Write-Host "Package upgraded. Skipping adapter restart; reboot or pass -RestartDevice for a controlled test."
  }
} elseif ($Mode -eq "DriverStore") {
  if (-not $ForceDriverStoreEdit) {
    throw "-Mode DriverStore edits the active DriverStore package. Pass -ForceDriverStoreEdit only for an emergency debug override."
  }
  $dst = Join-Path $store "helios_umd.dll"
  if ($RestartDevice) { Invoke-HeliosPnpUtil @("/disable-device", $id, "/force") 90 | Out-Null }
  try {
    if ($KillUmdUsers) { Stop-UmdUsers $dst }
    $copy = Copy-HeliosFileVerified $UmdDll $dst 5 750
    Write-Host "Installed DriverStore UMD: $($copy.Destination)"
  } finally {
    if ($RestartDevice) { Invoke-HeliosPnpUtil @("/enable-device", $id) 90 | Out-Null }
  }
} else {
  New-Item -ItemType Directory -Force -Path $ProgramDataDir | Out-Null
  Grant-HeliosReadExecute $ProgramDataDir
  if ($RestartDevice) {
    Write-Host "Disabling Helios before replacing ProgramData UMD."
    Invoke-HeliosPnpUtil @("/disable-device", $id, "/force") 90 | Out-Null
  }
  try {
    if ($KillUmdUsers) {
      Stop-UmdUsers $programDataDll
      Stop-UmdUsers $programDataVulkanDll
    }
    $copy = Copy-HeliosFileVerified $UmdDll $programDataDll 10 1000
    $vulkanCopy = Copy-HeliosFileVerified $VulkanDll $programDataVulkanDll 10 1000 -DisplaceInUse
    Grant-HeliosReadExecute $ProgramDataDir
    $reapedVulkan = Remove-HeliosDisplacedCopies $programDataVulkanDll
    if ($reapedVulkan -gt 0) { Write-Host "Reaped $reapedVulkan displaced lower-ICD companion copy(ies)." }
    Write-Host "Installed ProgramData UMD companion: $($vulkanCopy.Destination)"

    if ($deployUmd12) {
      $copy12 = Copy-HeliosFileVerified $Umd12Dll $programData12Dll 10 1000
      Grant-HeliosReadExecute $ProgramDataDir
      Write-Host "Installed ProgramData D3D12 UMD: $($copy12.Destination)"

      # `DECISIONS.md` D3: UserModeDriverName is a REG_MULTI_SZ indexed by
      # KMTUMDVERSION (DX9=0, DX10=1, DX11=2, DX12=3). Slots 0-2 stay on
      # helios_umd.dll; slot 3 is the D3D12 UMD.
      #
      # ⛔ FOUR entries, never six. D3DKMTQueryAdapterInfo(KMTQAITYPE_UMDRIVERNAME)
      # returns STATUS_INVALID_PARAMETER for versions 4/5 on this adapter, so
      # DX12_WSA32/DX12_WSA64 must not be written.
      $umdPaths = @($programDataDll, $programDataDll, $programDataDll, $programData12Dll)
      # ⚠ InstalledDisplayDrivers is NOT index-parallel to UserModeDriverName --
      # it is a flat list of the DISTINCT package binaries, so it is TWO entries
      # here, not four (`DECISIONS.md` §6.1). The pre-D3D12 value was four copies
      # of "helios_umd", which is semantically wrong; it is corrected on this arm
      # only, so a D3D11-only deploy keeps its historical value bit-for-bit.
      $umdNames = @("helios_umd", "helios_umd12")
    } else {
      $umdPaths = @($programDataDll, $programDataDll, $programDataDll, $programDataDll)
      $umdNames = @("helios_umd", "helios_umd", "helios_umd", "helios_umd")
    }
    New-ItemProperty -LiteralPath $classKey -Name "UserModeDriverName" -PropertyType MultiString -Value $umdPaths -Force | Out-Null
    New-ItemProperty -LiteralPath $classKey -Name "InstalledDisplayDrivers" -PropertyType MultiString -Value $umdNames -Force | Out-Null
    Write-Host "Installed ProgramData UMD: $($copy.Destination)"
    # ALSO sync the active DriverStore package copy: at COLD BOOT dxgkrnl's
    # first UMD-path resolution loads the package's helios_umd.dll (before the
    # registry override takes effect for later device creates), so a stale
    # DriverStore copy means dwm's first — composition — device runs an old
    # UMD every boot (proven 2026-07-03: two different handler generations in
    # one dwm process, early devices on the stale DLL).
    $storeDll = Join-Path $store "helios_umd.dll"
    if (Test-Path -LiteralPath $storeDll -PathType Leaf) {
      & takeown.exe /F $storeDll | Out-Null
      & icacls.exe $storeDll /grant "Administrators:F" | Out-Null
      # The package copy is routinely MAPPED by long-lived shell processes
      # (observed 2026-07-27: ShellHost, SystemSettings, CrossDeviceResume), so
      # the plain Copy-Item this replaces hit a sharing violation and every
      # deploy of a session silently left the cold-boot copy stale — measured
      # that day: store SHA 56473A67 against a current F0C7A2E6, last written
      # eight hours and six deploys earlier. -DisplaceInUse renames the loaded
      # image aside so the new one lands at the real path.
      #
      # Verified by hash like every other copy in this script, and it THROWS on
      # failure instead of printing a red blob while the deploy reports success:
      # a stale package copy is exactly the cold-boot hazard the comment above
      # describes, so it must not be possible to miss it.
      $storeCopy = Copy-HeliosFileVerified $UmdDll $storeDll 5 750 -DisplaceInUse
      Write-Host "Synced DriverStore UMD: $($storeCopy.Destination)"
      $reaped = Remove-HeliosDisplacedCopies $storeDll
      if ($reaped -gt 0) { Write-Host "Reaped $reaped displaced DriverStore UMD copy(ies)." }
    } else {
      Write-Warning "DriverStore UMD not found at $storeDll - cold boots may load a stale UMD"
    }

    # The package UMD imports vulkan_virtio.dll by its unversioned name. During
    # the first UMD load after a cold boot, normal DLL search therefore resolves
    # the copy beside helios_umd.dll in DriverStore. Keeping only ProgramData in
    # sync can pair a current UMD with an older lower ICD before the registry
    # override is observed, which is not a coherent deploy.
    if (-not (Test-Path -LiteralPath $driverStoreVulkanDll -PathType Leaf)) {
      throw "DriverStore UMD companion not found at $driverStoreVulkanDll"
    }
    $oldStoreVulkanHash = Get-HeliosFileHash $driverStoreVulkanDll
    if ($oldStoreVulkanHash -ne $vulkanHash) {
      $backupDir = Join-Path "C:\ProgramData\HeliosDeployBackups" ((Get-Date -Format "yyyyMMdd-HHmmss-fff") + "-umd-companion")
      New-Item -ItemType Directory -Force -Path $backupDir | Out-Null
      $backupVulkanDll = Join-Path $backupDir "vulkan_virtio.dll"
      Copy-Item -LiteralPath $driverStoreVulkanDll -Destination $backupVulkanDll -Force
      $backupVulkanHash = Get-HeliosFileHash $backupVulkanDll
      if ($backupVulkanHash -ne $oldStoreVulkanHash) {
        throw "DriverStore UMD companion backup hash mismatch: source=$oldStoreVulkanHash backup=$backupVulkanHash"
      }
      Write-Host "Backed up DriverStore UMD companion: $backupVulkanDll hash $backupVulkanHash"
    }
    $storeVulkanCopy = Copy-HeliosFileVerified $VulkanDll $driverStoreVulkanDll 5 750 -DisplaceInUse
    Write-Host "Synced DriverStore UMD companion: $($storeVulkanCopy.Destination)"
    $reapedStoreVulkan = Remove-HeliosDisplacedCopies $driverStoreVulkanDll
    if ($reapedStoreVulkan -gt 0) { Write-Host "Reaped $reapedStoreVulkan displaced DriverStore UMD companion copy(ies)." }
  } finally {
    if ($RestartDevice) {
      Write-Host "Re-enabling Helios after ProgramData UMD replacement."
      Invoke-HeliosPnpUtil @("/enable-device", $id) 90 | Out-Null
      # Looking Glass IDD is retired for this target and must remain disabled.
      # A Helios restart therefore ends here; no ROOT\DISPLAY device is
      # restarted or used as a display-admission substitute.
    }
  }
}

Start-Sleep -Seconds 2
$state = Get-HeliosPnpState $id
$state | Format-List
$activeUmd = if ($Mode -eq "ProgramData") { $programDataDll } else { Join-Path (Get-HeliosActiveStoreDir $id (Get-HeliosActiveInfName $id)) "helios_umd.dll" }
$activeHash = Get-HeliosFileHash $activeUmd
Write-Host "Active UMD:  $activeUmd"
Write-Host "Active hash: $activeHash"
if ($activeHash -ne $srcHash) { throw "UMD hotplug failed: active hash $activeHash does not match source $srcHash" }

if ($deployVulkan) {
  $activeVulkanHash = Get-HeliosFileHash $programDataVulkanDll
  Write-Host "Active UMD companion:  $programDataVulkanDll"
  Write-Host "Active companion hash: $activeVulkanHash"
  if ($activeVulkanHash -ne $vulkanHash) {
    throw "UMD companion hotplug failed: active hash $activeVulkanHash does not match source $vulkanHash"
  }
  $driverStoreVulkanHash = Get-HeliosFileHash $driverStoreVulkanDll
  Write-Host "DriverStore UMD companion:  $driverStoreVulkanDll"
  Write-Host "DriverStore companion hash: $driverStoreVulkanHash"
  if ($driverStoreVulkanHash -ne $vulkanHash) {
    throw "DriverStore UMD companion hotplug failed: active hash $driverStoreVulkanHash does not match source $vulkanHash"
  }
}

if ($deployUmd12) {
  $active12Hash = Get-HeliosFileHash $programData12Dll
  Write-Host "Active D3D12 UMD:  $programData12Dll"
  Write-Host "Active D3D12 hash: $active12Hash"
  if ($active12Hash -ne $src12Hash) { throw "D3D12 UMD hotplug failed: active hash $active12Hash does not match source $src12Hash" }

  # Read the registry BACK and assert the shape, rather than trusting the write.
  # This is `GATES.md` D12-G6's pass criterion in miniature, and it exists
  # because "four entries" alone passes on the pre-split driver: every one of
  # the four used to point at helios_umd.dll, so only the VALUE of index 3
  # distinguishes a real D3D12 registration from the historical value.
  $names = @((Get-ItemProperty -LiteralPath $classKey).UserModeDriverName)
  if ($names.Count -ne 4) { throw "UserModeDriverName has $($names.Count) entries, want 4" }
  if ($names[3] -notmatch 'helios_umd12_[0-9a-f]{16}\.dll$') { throw "UserModeDriverName[3] is '$($names[3])', want the deployed helios_umd12 DLL" }
  if (@($names[0..2] | Where-Object { $_ -notmatch 'helios_umd_[0-9a-f]{16}\.dll$' }).Count -ne 0) { throw "UserModeDriverName[0..2] must all stay on helios_umd" }
  if (-not (Test-Path -LiteralPath $names[3] -PathType Leaf)) { throw "UserModeDriverName[3] path does not exist on disk: $($names[3])" }
  $installed = @((Get-ItemProperty -LiteralPath $classKey).InstalledDisplayDrivers)
  if ($installed.Count -ne 2) { throw "InstalledDisplayDrivers has $($installed.Count) entries, want exactly 2 (helios_umd,helios_umd12)" }
  Write-Host "UserModeDriverName[3]     -> $($names[3])"
  Write-Host "InstalledDisplayDrivers   -> $($installed -join ',')"

  # ⚠ COLD BOOT: dxgkrnl's first UMD-path resolution reads the DriverStore
  # package, not this registry override. The package ships helios_umd12.dll only
  # once the INF carries it, which is stage S5 -- so until then a cold boot has
  # NO D3D12 UMD and D3D12 device creation falls back to whatever slot 3 held
  # before. That is harmless while OpenAdapter12 refuses, and it is exactly why
  # this arm is opt-in.
  Write-Warning "D3D12 UMD is registered in the ProgramData override only; the DriverStore package does not carry helios_umd12.dll until the INF change (ARCHITECTURE.md S5). Cold boots will not see it."
}

if (-not $NoProbe -and (Test-Path -LiteralPath $Probe -PathType Leaf)) {
  Write-Host "Running D3D11 probe: $Probe"
  $probeResult = Invoke-HeliosProcess -FilePath $Probe -Arguments @() -TimeoutSeconds 20
  if ($probeResult.ExitCode -ne 0) {
    Write-Warning "Probe exit code $($probeResult.ExitCode). Check output above."
  }
} elseif (-not $NoProbe) {
  Write-Warning "Probe not found: $Probe"
}
