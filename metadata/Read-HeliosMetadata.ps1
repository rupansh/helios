# Build-time reader. Never source the .env files as executable shell code.
function Read-HeliosMetadata([Parameter(Mandatory)][string]$RepoRoot) {
    $values = @{}
    foreach ($relative in @("metadata\helios.env", "kmd_render\driver-version.env")) {
        foreach ($line in Get-Content -LiteralPath (Join-Path $RepoRoot $relative)) {
            $line = $line.Trim()
            if (-not $line -or $line.StartsWith("#")) { continue }
            if ($line -notmatch '^([A-Z0-9_]+)=([\x20-\x7e]+)$') {
                throw "Invalid metadata assignment in $relative"
            }
            $key = $Matches[1]
            $value = $Matches[2]
            if ($values.ContainsKey($key) -or $value.Contains('"') -or $value.Contains('\')) {
                throw "Invalid or duplicate metadata key: $key"
            }
            $values[$key] = $value
        }
    }
    foreach ($key in @("HELIOS_PRODUCT", "HELIOS_PUBLISHER", "HELIOS_KMD_ROLE",
        "HELIOS_UMD_ROLE", "HELIOS_UMD12_ROLE", "HELIOS_ADL_ROLE", "HELIOS_KMD_VERSION", "HELIOS_MONITOR_MODEL_YEAR")) {
        if (-not $values.ContainsKey($key)) { throw "Missing metadata key: $key" }
    }
    if ($values.HELIOS_PRODUCT.Length -gt 12) { throw "Product name exceeds the EDID limit (12 bytes)." }
    if ($values.HELIOS_KMD_VERSION -notmatch '^\d+\.\d+\.\d+\.\d+$') {
        throw "Driver version must have four numeric components."
    }
    foreach ($part in $values.HELIOS_KMD_VERSION.Split('.')) {
        if ([uint64]$part -gt 65535) { throw "Driver version components must fit 16 bits." }
    }
    if ($values.HELIOS_PUBLISHER.Length -gt 12) { throw "Publisher exceeds the EDID text limit (12 bytes)." }
    if ($values.HELIOS_MONITOR_MODEL_YEAR -notmatch '^\d{4}$' -or
        [int]$values.HELIOS_MONITOR_MODEL_YEAR -lt 1990 -or [int]$values.HELIOS_MONITOR_MODEL_YEAR -gt 2245) {
        throw "EDID model year must be between 1990 and 2245."
    }
    return $values
}

function Write-HeliosAdlVersionResource(
    [Parameter(Mandatory)][string]$RepoRoot,
    [Parameter(Mandatory)][string]$OutputPath
) {
    $metadata = Read-HeliosMetadata $RepoRoot
    $fields = @{
        COMMA = $metadata.HELIOS_KMD_VERSION.Replace('.', ',')
        DOTTED = $metadata.HELIOS_KMD_VERSION
        FILE_TYPE = "2"
        SUBTYPE = "0"
        PUBLISHER = $metadata.HELIOS_PUBLISHER
        PRODUCT = $metadata.HELIOS_PRODUCT
        DESCRIPTION = "$($metadata.HELIOS_PRODUCT) $($metadata.HELIOS_ADL_ROLE)"
        INTERNAL = "atiadlxx"
        FILENAME = "atiadlxx.dll"
    }
    $resource = Get-Content -LiteralPath (Join-Path $RepoRoot "metadata\version.rc.in") -Raw
    foreach ($key in $fields.Keys) { $resource = $resource.Replace("@$key@", $fields[$key]) }
    if ($resource -match '@[A-Z_]+@') { throw "Unexpanded metadata resource placeholder: $($Matches[0])" }
    Set-Content -LiteralPath $OutputPath -Value $resource -Encoding ascii
}
