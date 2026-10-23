<#
.SYNOPSIS
  Code-signs one or more files with signtool.exe, if signing is configured.
.DESCRIPTION
  Signing is optional. The certificates are supplied through environment variables (in CI they
  come from repository secrets, see docs/RELEASING.md):

    TILE_RS_SIGN_CERT_PFX_BASE64   base64 of a .pfx file           (with ...)
    TILE_RS_SIGN_CERT_PASSWORD     the .pfx password
    TILE_RS_SIGN_THUMBPRINT        SHA-1 thumbprint of a certificate already in the user store

  If none of these is set the script prints a note and does nothing (exit 0), so local builds and
  releases without a certificate keep working. Timestamps use the DigiCert RFC 3161 service.
.PARAMETER Path
  The file(s) to sign.
#>
param([Parameter(Mandatory)][string[]]$Path)

$ErrorActionPreference = 'Stop'
Set-Location (Split-Path -Parent $PSScriptRoot)

function Get-SignTool {
    $roots = @(
        "${env:ProgramFiles(x86)}\Windows Kits\10\bin",
        "$env:ProgramFiles\Windows Kits\10\bin"
    )
    foreach ($root in $roots) {
        if (-not $root -or -not (Test-Path $root)) { continue }
        $found = Get-ChildItem $root -Recurse -Filter signtool.exe -ErrorAction SilentlyContinue |
            Where-Object { $_.FullName -match '\\x64\\' } |
            Sort-Object FullName -Descending | Select-Object -First 1
        if ($found) { return $found.FullName }
    }
    return $null
}

$b64 = $env:TILE_RS_SIGN_CERT_PFX_BASE64
$password = $env:TILE_RS_SIGN_CERT_PASSWORD
$thumbprint = $env:TILE_RS_SIGN_THUMBPRINT

if (-not $b64 -and -not $thumbprint) {
    if ($env:TILE_RS_REQUIRE_SIGNING -eq '1') { throw 'Code signing is required for this build but no certificate is configured.' }
    Write-Host 'Code signing is not configured: skipping.' -ForegroundColor Yellow
    Write-Host 'Set TILE_RS_SIGN_CERT_PFX_BASE64 (and TILE_RS_SIGN_CERT_PASSWORD) or TILE_RS_SIGN_THUMBPRINT to sign.'
    exit 0
}

$signtool = Get-SignTool
if (-not $signtool) { throw 'signtool.exe was not found. Install the Windows SDK to sign.' }

$signArgs = @('sign', '/fd', 'SHA256', '/td', 'SHA256', '/tr', 'http://timestamp.digicert.com')
$pfx = $null
if ($b64) {
    $pfx = Join-Path ([System.IO.Path]::GetTempPath()) ("tile-rs-" + [guid]::NewGuid().ToString('N') + ".pfx")
    [System.IO.File]::WriteAllBytes($pfx, [Convert]::FromBase64String($b64))
    $signArgs += @('/f', $pfx)
    if ($password) { $signArgs += @('/p', $password) }
} else {
    $signArgs += @('/sha1', $thumbprint)
}

try {
    foreach ($file in $Path) {
        if (-not (Test-Path $file)) { throw "$file not found" }
        Write-Host "Signing $file"
        & $signtool @signArgs $file
        if ($LASTEXITCODE -ne 0) { throw "signtool failed for $file" }
    }
} finally {
    if ($pfx -and (Test-Path $pfx)) { Remove-Item $pfx -Force }
}
Write-Host 'Signing done.' -ForegroundColor Green
