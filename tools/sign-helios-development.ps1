param(
    [Parameter(Mandatory)][string]$OutputDirectory,
    [string]$PackageDirectory = "",
    [string]$SignFile = ""
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
# A fresh powershell -NoProfile process does not load the Certificate provider
# merely because Get-ChildItem receives a Cert: path.
Import-Module Microsoft.PowerShell.Security -ErrorAction Stop
$repo = Split-Path -Parent $PSScriptRoot
. (Join-Path $repo "metadata\Read-HeliosMetadata.ps1")
$metadata = Read-HeliosMetadata $repo
$subject = "CN=$($metadata.HELIOS_PUBLISHER) $($metadata.HELIOS_PRODUCT) Development Test Signing"
$now = Get-Date
$certificate = Get-ChildItem Cert:\CurrentUser\My |
    Where-Object { $_.Subject -eq $subject -and $_.HasPrivateKey -and $_.NotBefore -le $now -and $_.NotAfter -gt $now.AddDays(30) } |
    Sort-Object NotAfter -Descending | Select-Object -First 1
if (-not $certificate) {
    $certificate = New-SelfSignedCertificate -Type CodeSigningCert -Subject $subject `
        -CertStoreLocation Cert:\CurrentUser\My -KeyAlgorithm RSA -KeyLength 3072 `
        -HashAlgorithm SHA256 -NotAfter $now.AddYears(3)
}
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
$certificateFile = Join-Path $OutputDirectory "helios-dev-test.cer"
Export-Certificate -Cert $certificate -FilePath $certificateFile -Force | Out-Null
if ($PackageDirectory) {
    New-Item -ItemType Directory -Force -Path $PackageDirectory | Out-Null
    Copy-Item -LiteralPath $certificateFile -Destination (Join-Path $PackageDirectory "helios-dev-test.cer") -Force
    # A reused output directory may contain the old WDK template certificate.
    Remove-Item -LiteralPath (Join-Path $PackageDirectory "WDRLocalTestCert.cer") -Force -ErrorAction SilentlyContinue
}
if ($SignFile) {
    if (-not (Test-Path -LiteralPath $SignFile -PathType Leaf)) { throw "Signing input is missing: $SignFile" }
    & signtool.exe sign /v /s My /sha1 $certificate.Thumbprint /fd SHA256 /t http://timestamp.digicert.com $SignFile
    if ($LASTEXITCODE -ne 0) { throw "signtool failed for $SignFile (exit $LASTEXITCODE)." }
}
Write-Host "Development signer: $subject [$($certificate.Thumbprint)]"
