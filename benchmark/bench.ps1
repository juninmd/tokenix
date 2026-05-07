$ErrorActionPreference = "Stop"

$RepoRoot = Split-Path -Parent $PSScriptRoot
Set-Location $RepoRoot

$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"

Write-Host ""
Write-Host "=== tokenix benchmark wrapper ===" -ForegroundColor Cyan
Write-Host "Running the built-in real benchmark from the current checkout."
Write-Host ""

cargo run -- benchmark --refresh-index
