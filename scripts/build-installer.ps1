<#
.SYNOPSIS
  Builds the release, copies it to dist\ and (if Inno Setup 6 is installed) builds the installer.
  Install Inno Setup from https://jrsoftware.org/isdl.php to get dist\tile-rs-<version>-setup.exe.
#>
$ErrorActionPreference = 'Stop'
Set-Location (Split-Path -Parent $PSScriptRoot)

cargo build --release
if ($LASTEXITCODE -ne 0) { throw 'cargo build --release failed' }

$version = (Select-String -Path Cargo.toml -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value
New-Item -ItemType Directory -Force dist | Out-Null
Copy-Item 'target\release\tile-rs.exe' 'dist\tile-rs.exe' -Force
Write-Host "dist\tile-rs.exe updated (v$version)"

# code-sign the exe before it is packed into the installer (a no-op unless signing is configured)
& (Join-Path $PSScriptRoot 'sign.ps1') -Path (Resolve-Path 'dist\tile-rs.exe').Path
if ($LASTEXITCODE -ne 0) { throw 'code signing failed' }

$iscc = @(
    (Join-Path ${env:ProgramFiles(x86)} 'Inno Setup 6\ISCC.exe'),
    (Join-Path $env:ProgramFiles 'Inno Setup 6\ISCC.exe'),
    (Join-Path $env:LOCALAPPDATA 'Programs\Inno Setup 6\ISCC.exe')
) | Where-Object { $_ -and (Test-Path $_) } | Select-Object -First 1

if (-not $iscc) {
    Write-Host 'Inno Setup 6 not found: the installer was not built.' -ForegroundColor Yellow
    Write-Host 'Install it from https://jrsoftware.org/isdl.php and run this script again.'
    exit 0
}

$icon = if (Test-Path 'assets\tile-rs.ico') { (Resolve-Path 'assets\tile-rs.ico').Path } else { '' }
& $iscc 'installer\tile-rs.iss' "/DAppVersion=$version" "/DBinPath=$((Resolve-Path 'dist\tile-rs.exe').Path)" "/DIconPath=$icon"
if ($LASTEXITCODE -ne 0) { throw 'ISCC failed' }

# code-sign the installer (a no-op unless signing is configured)
& (Join-Path $PSScriptRoot 'sign.ps1') -Path (Resolve-Path "dist\tile-rs-$version-setup.exe").Path
if ($LASTEXITCODE -ne 0) { throw 'code signing failed' }

Write-Host "Installer written to dist\tile-rs-$version-setup.exe" -ForegroundColor Green
