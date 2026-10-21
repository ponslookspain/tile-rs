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
. (Join-Path $PSScriptRoot 'vcenv.ps1')

$version = (Select-String -Path Cargo.toml -Pattern '^version\s*=\s*"([^"]+)"' |
    Select-Object -First 1).Matches[0].Groups[1].Value
if (-not $version) { throw 'Could not read the version from Cargo.toml' }
if ($version -notmatch '^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$') { throw 'Invalid package version' }

if (-not $SkipBuild) {
    cargo build --locked --release
    if ($LASTEXITCODE -ne 0) { throw 'cargo build --release failed' }
}
if (-not (Test-Path target\release\tile-rs.exe)) { throw 'target\release\tile-rs.exe not found' }

$name  = "tile-rs-v$version-windows-x64"
$stage = Join-Path 'release' $name
New-Item -ItemType Directory -Force -Path 'release' | Out-Null
$releaseRoot = (Resolve-Path 'release').Path
$stage = [IO.Path]::GetFullPath((Join-Path $releaseRoot $name))
if (-not $stage.StartsWith($releaseRoot + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
    throw 'Staging directory must be inside release/'
}
if (Test-Path $stage) { Remove-Item -LiteralPath $stage -Recurse -Force }
New-Item -ItemType Directory -Path $stage | Out-Null

Copy-Item target\release\tile-rs.exe $stage
Copy-Item README.md, README.ru.md, LICENSE, CHANGELOG.md $stage
$docsStage = Join-Path $stage 'docs'
New-Item -ItemType Directory -Path $docsStage | Out-Null
Copy-Item docs\CONFIGURATION.md, docs\DEVELOPMENT.md, docs\RELEASING.md, docs\WINDOW-MOTION.md $docsStage

# code-sign the exe before zipping (a no-op unless signing is configured)
& (Join-Path $PSScriptRoot 'sign.ps1') -Path (Join-Path $stage 'tile-rs.exe')
if ($LASTEXITCODE -ne 0) { throw 'code signing failed' }

$zip = "release\$name.zip"
Compress-Archive -Path "$stage\*" -DestinationPath $zip -Force
$hash = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLower()
"$hash  $name.zip" | Set-Content "$zip.sha256" -Encoding ascii
Remove-Item -LiteralPath $stage -Recurse -Force

Write-Host "Packed $zip"
Write-Host "SHA256 $hash"
