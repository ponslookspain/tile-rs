<#
.SYNOPSIS
  Writes the winget manifest of a published tile-rs release to packaging\winget\<version>\.
  The installer hash is read from the release's .sha256 asset, so run it after the release is
  published. Submitting the result to microsoft/winget-pkgs is a manual step: see docs/RELEASING.md.
.EXAMPLE
  ./scripts/winget-manifest.ps1 -Version 0.7.1
#>
param([Parameter(Mandatory = $true)][string]$Version)
$ErrorActionPreference = 'Stop'
if ($Version -notmatch '^\d+\.\d+\.\d+$') { throw "Version must look like 1.2.3, not '$Version'" }
$id = 'Ponslookspain.TileRs'
$base = "https://github.com/ponslookspain/tile-rs/releases/download/v$Version"
$installer = "tile-rs-$Version-setup.exe"
$content = (Invoke-WebRequest -UseBasicParsing "$base/$installer.sha256").Content
if ($content -is [byte[]]) { $content = [Text.Encoding]::ASCII.GetString($content) }
$line = $content.Trim()
if ($line -notmatch '^([0-9a-fA-F]{64})\s+(\S+)$' -or $Matches[2] -ne $installer) {
    throw "Unexpected checksum file for ${installer}: $line"
}
$hash = $Matches[1].ToUpperInvariant()
$directory = Join-Path (Split-Path -Parent $PSScriptRoot) "packaging\winget\$Version"
New-Item -ItemType Directory -Force $directory | Out-Null
$schema = 'https://aka.ms/winget-manifest'

function Write-Manifest($name, $text) {
    [IO.File]::WriteAllText((Join-Path $directory "$id.$name.yaml"), $text.Replace("`r`n", "`n"), [Text.UTF8Encoding]::new($false))
}

Write-Manifest 'yaml' @"
# yaml-language-server: `$schema=$schema.version.1.6.0.schema.json
PackageIdentifier: $id
PackageVersion: $Version
DefaultLocale: en-US
ManifestType: version
ManifestVersion: 1.6.0
"@

Write-Manifest 'installer' @"
# yaml-language-server: `$schema=$schema.installer.1.6.0.schema.json
PackageIdentifier: $id
PackageVersion: $Version
InstallerType: inno
Scope: user
MinimumOSVersion: 10.0.0.0
UpgradeBehavior: install
ProductCode: '{7B5E9F1C-2C4A-4E5B-9D3E-6C1F0A2B4D77}_is1'
Installers:
  - Architecture: x64
    InstallerUrl: $base/$installer
    InstallerSha256: $hash
ManifestType: installer
ManifestVersion: 1.6.0
"@

Write-Manifest 'locale.en-US' @"
# yaml-language-server: `$schema=$schema.defaultLocale.1.6.0.schema.json
PackageIdentifier: $id
PackageVersion: $Version
PackageLocale: en-US
Publisher: ponslookspain
PublisherUrl: https://github.com/ponslookspain
PublisherSupportUrl: https://github.com/ponslookspain/tile-rs/issues
PackageName: tile-rs
PackageUrl: https://github.com/ponslookspain/tile-rs
License: MIT
LicenseUrl: https://github.com/ponslookspain/tile-rs/blob/v$Version/LICENSE
ShortDescription: A small tiling window manager for Windows 10/11 with workspaces and a settings window.
Description: tile-rs arranges windows in a spiral, master-stack, columns or grid layout, with 9 workspaces per monitor, live mouse resizing, animations and a graphical settings window. It runs from the system tray.
Moniker: tile-rs
Tags:
  - tiling
  - window-manager
  - windows
  - workspaces
ReleaseNotesUrl: https://github.com/ponslookspain/tile-rs/releases/tag/v$Version
ManifestType: defaultLocale
ManifestVersion: 1.6.0
"@
Write-Host "Manifest written to $directory"
