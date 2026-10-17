<#
.SYNOPSIS
  Builds the release exe and packs it into release\tile-rs-vX.Y.Z-windows-x64.zip (+ .sha256).
.DESCRIPTION
  The version is read from Cargo.toml (the single source of truth).
  Used locally and by the release workflow (.github/workflows/release.yml).
.PARAMETER SkipBuild
  Pack the existing target\release\tile-rs.exe without building first.
#>
param([switch]$SkipBuild)

$ErrorActionPreference = 'Stop'
Set-Location (Split-Path -Parent $PSScriptRoot)

$version = (Select-String -Path Cargo.toml -Pattern '^version\s*=\s*"([^"]+)"' |
    Select-Object -First 1).Matches[0].Groups[1].Value
if (-not $version) { throw 'Could not read the version from Cargo.toml' }

if (-not $SkipBuild) {
    cargo build --release
    if ($LASTEXITCODE -ne 0) { throw 'cargo build --release failed' }
}
if (-not (Test-Path target\release\tile-rs.exe)) { throw 'target\release\tile-rs.exe not found' }

$name  = "tile-rs-v$version-windows-x64"
$stage = Join-Path 'release' $name
if (Test-Path release) { Remove-Item release -Recurse -Force }
New-Item -ItemType Directory -Path $stage | Out-Null

Copy-Item target\release\tile-rs.exe $stage
Copy-Item README.md, README.ru.md, LICENSE, CHANGELOG.md $stage
Copy-Item docs\CONFIGURATION.md $stage

$zip = "release\$name.zip"
Compress-Archive -Path "$stage\*" -DestinationPath $zip
$hash = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLower()
"$hash  $name.zip" | Set-Content "$zip.sha256" -Encoding ascii
Remove-Item $stage -Recurse -Force

Write-Host "Packed $zip"
Write-Host "SHA256 $hash"
