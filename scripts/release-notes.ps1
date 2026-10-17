<#
.SYNOPSIS
  Writes the CHANGELOG.md section of a version to a file (used as GitHub release notes).
.PARAMETER Version
  e.g. 0.3.0 or 0.3.0-rc.1 (a pre-release falls back to the base version, then to [Unreleased]).
.PARAMETER Out
  Output file.
#>
param(
    [Parameter(Mandatory)] [string]$Version,
    [Parameter(Mandatory)] [string]$Out
)
$ErrorActionPreference = 'Stop'
Set-Location (Split-Path -Parent $PSScriptRoot)

$lines = Get-Content CHANGELOG.md

function Section($heading) {
    $start = -1
    for ($i = 0; $i -lt $lines.Count; $i++) {
        if ($lines[$i] -match ('^## \[' + [regex]::Escape($heading) + '\]')) { $start = $i + 1; break }
    }
    if ($start -lt 0) { return $null }
    $end = $lines.Count
    for ($i = $start; $i -lt $lines.Count; $i++) {
        if ($lines[$i] -match '^## \[') { $end = $i; break }
    }
    $text = ($lines[$start..($end - 1)] -join "`n").Trim()
    if ($text) { return $text } else { return $null }
}

$base = $Version -replace '-.*$', ''
$notes = Section $Version
if (-not $notes) { $notes = Section $base }
if (-not $notes) { $notes = Section 'Unreleased' }
if (-not $notes) { throw "No CHANGELOG.md section found for $Version" }

Set-Content -Path $Out -Value $notes -Encoding utf8
Write-Host "Release notes for $Version written to $Out"
