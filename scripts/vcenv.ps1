<#
.SYNOPSIS
  Makes the MSVC linker (link.exe) available in the current PowerShell session.
.DESCRIPTION
  `cargo build` for the x86_64-pc-windows-msvc target needs the Visual Studio build tools
  environment. In a plain shell link.exe is missing (or, in Git Bash, shadowed by the coreutils
  `link`). Dot-source this script (. .\scripts\vcenv.ps1) before running cargo; it does nothing
  when the right link.exe is already on PATH.
#>
$ErrorActionPreference = 'Stop'

$existing = Get-Command link.exe -ErrorAction SilentlyContinue | Select-Object -First 1
if ($existing -and $existing.Source -match '\\VC\\') { return }

$vcvars = $null
$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
if (Test-Path $vswhere) {
    $root = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
    if ($root) {
        $candidate = Join-Path $root 'VC\Auxiliary\Build\vcvars64.bat'
        if (Test-Path $candidate) { $vcvars = $candidate }
    }
}
if (-not $vcvars) {
    $vcvars = Get-ChildItem @(
        "${env:ProgramFiles}\Microsoft Visual Studio",
        "${env:ProgramFiles(x86)}\Microsoft Visual Studio"
    ) -Recurse -Filter vcvars64.bat -ErrorAction SilentlyContinue |
        Sort-Object FullName -Descending | Select-Object -First 1 -ExpandProperty FullName
}
if (-not $vcvars) {
    throw 'Visual Studio Build Tools (C++ workload) not found: install them from https://aka.ms/vs/17/release/vs_BuildTools.exe'
}

# Run vcvars64.bat in cmd and copy the resulting environment into this session.
cmd /c "`"$vcvars`" >nul 2>&1 && set" | ForEach-Object {
    if ($_ -match '^([^=]+)=(.*)$') { Set-Item -Path "Env:$($Matches[1])" -Value $Matches[2] }
}
Write-Host "MSVC environment loaded from $vcvars" -ForegroundColor DarkGray
