#!/usr/bin/env bash
# Print the KMD counter names that STILL HAVE A WRITER, as a PowerShell snippet
# that reads exactly those from the service key.
#
# The service key is append-only across every KMD version ever installed, so a
# `reg query` mixes live counters with fossils whose writers were deleted long
# ago. On 2026-08-24 the driver had 286 live names against 3,951 registry
# values, and a session spent a deploy cycle reasoning from `ScFlu`/`ScSet`,
# whose writers went away in 60a9988 (2026-08-21).
#
#   tools/kmd-live-counter-names.sh > /tmp/live.ps1
#   win_exec: Invoke-Expression (Get-Content -Raw 'Z:\tmp\live.ps1')
#
# (`& script.ps1` reads as empty: machine ExecutionPolicy is Restricted.)
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
names="$(grep -rhoE 'b"[A-Za-z0-9_]{1,14}"' "$root/kmd_render/src" | sed 's/^b"//; s/"$//' | sort -u)"
printf '$n=@('
first=1
while IFS= read -r n; do
  [ -n "$n" ] || continue
  [ "$first" = 1 ] || printf ','
  printf "'%s'" "$n"
  first=0
done <<< "$names"
printf ')\n'
cat <<'PS'
$k=Get-ItemProperty "HKLM:\SYSTEM\CurrentControlSet\Services\helios_kmd_render"
($n | ForEach-Object { if ($null -ne $k.$_ -and $k.$_ -ne 0) { "{0}={1}" -f $_,$k.$_ } }) -join " "
PS
