<#
.SYNOPSIS
  The same checks CI runs: formatting, clippy (warnings are errors), tests, release build.
  Run it before opening a pull request.
#>
$ErrorActionPreference = 'Stop'
Set-Location (Split-Path -Parent $PSScriptRoot)

function Step($title, [scriptblock]$cmd) {
    Write-Host "== $title" -ForegroundColor Cyan
    & $cmd
    if ($LASTEXITCODE -ne 0) { throw "$title failed" }
}

Step 'cargo fmt --check'  { cargo fmt --all -- --check }
Step 'cargo clippy'       { cargo clippy --locked --all-targets -- -D warnings }
Step 'cargo test'         { cargo test --locked --all-targets }
Step 'cargo build --release' { cargo build --locked --release }
Write-Host 'All checks passed.' -ForegroundColor Green
