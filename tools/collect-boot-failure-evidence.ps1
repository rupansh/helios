# Collect everything that distinguishes "the KMD bugchecked" from "Windows is in
# its dirty-shutdown repair loop", plus the Helios deploy state. Written for a
# guest booted WITHOUT the Helios GPU: nothing here touches the device, and every
# step degrades to a note instead of an error when its subject is absent.
#
#   win_exec: Invoke-Expression (Get-Content -Raw 'Z:\tools\collect-boot-failure-evidence.ps1')
#
# Writes into Z:\tmp\bootfail-<stamp>\ so the Linux side can read it directly.
# ⛔ Machine ExecutionPolicy is Restricted: `& script.ps1` runs and produces
# NOTHING, silently. Invoke-Expression is the only form that works here.

$ErrorActionPreference = 'Continue'
$stamp = (Get-Date -Format 'yyyyMMdd-HHmmss')
$out = "Z:\tmp\bootfail-$stamp"
New-Item -ItemType Directory -Force -Path $out | Out-Null
function Note([string]$name, [string]$text) {
    Set-Content -Path (Join-Path $out $name) -Value $text -Encoding UTF8
}

# 1. Bugcheck dumps. THE discriminator: a dump names the faulting driver, and no
#    dump at all means Windows never bugchecked, which points at the repair loop.
$dumps = @(Get-ChildItem 'C:\Windows\Minidump\*.dmp' -ErrorAction SilentlyContinue)
if ($dumps) {
    New-Item -ItemType Directory -Force -Path "$out\minidump" | Out-Null
    $dumps | Sort-Object LastWriteTime -Descending | Select-Object -First 8 |
        ForEach-Object { Copy-Item $_.FullName "$out\minidump\" -ErrorAction SilentlyContinue }
}
$memdmp = Get-Item 'C:\Windows\MEMORY.DMP' -ErrorAction SilentlyContinue
Note 'dumps.txt' (@(
    "minidumps: $($dumps.Count)"
    ($dumps | Sort-Object LastWriteTime -Descending | Select-Object -First 12 |
        ForEach-Object { "  {0}  {1} bytes  {2}" -f $_.Name, $_.Length, $_.LastWriteTime })
    "MEMORY.DMP: $(if ($memdmp) { "$($memdmp.Length) bytes  $($memdmp.LastWriteTime)" } else { 'absent' })"
) -join "`n")

# 2. Bugcheck + unexpected-shutdown events. Kernel-Power 41 without a 1001 is the
#    signature of a reset that Windows never saw coming — i.e. our QMP resets.
$ev = Get-WinEvent -FilterHashtable @{ LogName = 'System'; Id = 1001, 41, 6008, 1074 } -MaxEvents 60 -ErrorAction SilentlyContinue
Note 'events.txt' (($ev | ForEach-Object {
    "{0}  id={1,-5} {2}`n    {3}" -f $_.TimeCreated, $_.Id, $_.ProviderName, ($_.Message -replace "`r?`n", ' ')
}) -join "`n")

# 3. Boot history: how many boots, and did any complete.
Note 'boot.txt' (@(
    "LastBootUpTime: $((Get-CimInstance Win32_OperatingSystem).LastBootUpTime)"
    "LocalDateTime : $((Get-CimInstance Win32_OperatingSystem).LocalDateTime)"
    "BootupState   : $((Get-CimInstance Win32_ComputerSystem).BootupState)"
    "RecoveryEnabled/AutoReboot: $((Get-CimInstance Win32_OSRecoveryConfiguration | Select-Object -ExpandProperty AutoReboot))"
) -join "`n")

# 4. Helios deploy state. ConfigFlags=1 is CONFIGFLAG_DISABLED and Code-22s the
#    next boot; a timed-out `pnputil /disable-device` writes it and
#    `/enable-device` does NOT clear it.
$enum = 'HKLM:\SYSTEM\CurrentControlSet\Enum\PCI\VEN_1AF4&DEV_1050&SUBSYS_11001AF4&REV_01\4&27FF4EC&0&0017'
$svc = 'HKLM:\SYSTEM\CurrentControlSet\Services\helios_kmd_render'
$cls = 'HKLM:\SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}\0000'
Note 'deploy.txt' (@(
    "ConfigFlags   : $((Get-ItemProperty $enum -ErrorAction SilentlyContinue).ConfigFlags)"
    "service Start : $((Get-ItemProperty $svc -ErrorAction SilentlyContinue).Start)  (4 = disabled)"
    "service Image : $((Get-ItemProperty $svc -ErrorAction SilentlyContinue).ImagePath)"
    "UserModeDriverName:"
    (((Get-ItemProperty $cls -ErrorAction SilentlyContinue).UserModeDriverName) | ForEach-Object { "  $_" })
    "sys in DriverStore (newest 5):"
    (Get-ChildItem 'C:\Windows\System32\DriverStore\FileRepository' -Filter 'helios_kmd_render.inf_amd64_*' -Directory -ErrorAction SilentlyContinue |
        Sort-Object LastWriteTime -Descending | Select-Object -First 5 | ForEach-Object {
            $s = Join-Path $_.FullName 'helios_kmd_render.sys'
            "  {0}  sys={1}" -f $_.Name, (Get-Item $s -ErrorAction SilentlyContinue).VersionInfo.FileVersion
        })
    "deploy backups (newest 5):"
    (Get-ChildItem 'C:\ProgramData\HeliosDeployBackups' -Directory -ErrorAction SilentlyContinue |
        Sort-Object Name -Descending | Select-Object -First 5 | ForEach-Object { "  $($_.Name)" })
) -join "`n")

# 5. Device install history — the disable/enable/publish churn in plain text.
Copy-Item 'C:\Windows\INF\setupapi.dev.log' "$out\setupapi.dev.log" -ErrorAction SilentlyContinue

# 6. UMD logs. ⛔ These are APPENDED across boots and pids are reused, so an old
#    file is mostly other boots; the size/mtime here is what makes that visible.
$umd = @(Get-ChildItem 'C:\ProgramData\Helios\*.log' -ErrorAction SilentlyContinue)
Note 'umdlogs.txt' (($umd | Sort-Object LastWriteTime -Descending |
    ForEach-Object { "{0,-34} {1,10}  {2}" -f $_.Name, $_.Length, $_.LastWriteTime }) -join "`n")
if ($umd) {
    New-Item -ItemType Directory -Force -Path "$out\umd" | Out-Null
    $umd | Where-Object { $_.Length -lt 8MB } | Sort-Object LastWriteTime -Descending |
        Select-Object -First 6 | ForEach-Object { Copy-Item $_.FullName "$out\umd\" -ErrorAction SilentlyContinue }
}

# 7. The live KMD counters, derived from the source so fossils are excluded.
$names = @()
$src = Get-ChildItem 'Z:\kmd_render\src' -Recurse -Filter '*.rs' -ErrorAction SilentlyContinue
foreach ($f in $src) {
    foreach ($m in [regex]::Matches((Get-Content -Raw $f.FullName), 'b"([A-Za-z0-9_]{1,14})"')) {
        $names += $m.Groups[1].Value
    }
}
$names = $names | Sort-Object -Unique
$k = Get-ItemProperty $svc -ErrorAction SilentlyContinue
Note 'counters.txt' (($names | ForEach-Object {
    if ($k -and $null -ne $k.$_ -and $k.$_ -ne 0) { "{0}={1}" -f $_, $k.$_ }
}) -join "`n")

"collected: $out"
Get-ChildItem $out -Recurse | Select-Object -ExpandProperty FullName
