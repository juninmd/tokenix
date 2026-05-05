# benchmark/bench.ps1
# Real tokenix benchmark -- measures actual token savings for file-read interception.
#
# Usage:
#   cd C:\path\to\tokenix
#   .\benchmark\bench.ps1
#
# Requirements: cargo in PATH. Ollama is NOT required for Phase 1.
# Optional Phase 2 requires: Ollama + nomic-embed-text + indexed repo.

$ErrorActionPreference = "Stop"

$RepoRoot = Split-Path -Parent $PSScriptRoot
$SamplesDir = Join-Path $RepoRoot "benchmark\samples"
Set-Location $RepoRoot

Write-Host ""
Write-Host "=== tokenix benchmark ===" -ForegroundColor Cyan
Write-Host ""
Write-Host "Building tokenix (release)..." -ForegroundColor Yellow
$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"
$null = cargo build --release
$Tokenix = Join-Path $RepoRoot "target\release\tokenix.exe"

if (-not (Test-Path $Tokenix)) {
    Write-Error "Build failed -- cannot find $Tokenix"
    exit 1
}
Write-Host ""

function Get-TokenCount {
    param([string]$Text)
    return [int](($Text.Length + 3) / 4)
}

Write-Host "Phase 1 -- File-read interception (outline mode)" -ForegroundColor White
Write-Host "Simulates an AI assistant reading source files."
Write-Host "tokenix intercepts reads on files >= 200 lines and returns a symbol outline."
Write-Host ""

$SrcFiles = Get-ChildItem -Path (Join-Path $RepoRoot "src") -Filter "*.rs" -File |
    Sort-Object FullName
$SampleFiles = Get-ChildItem -Path $SamplesDir -File |
    Where-Object { $_.Extension -in @(".rs", ".py", ".ts", ".go") } |
    Sort-Object FullName
$AllFiles = @($SrcFiles) + @($SampleFiles)

$Header = "{0,-42} {1,6} {2,10} {3,11} {4,8}" -f "File", "Lines", "RawTok", "HookTok", "Saved"
Write-Host $Header -ForegroundColor White
Write-Host ("-" * 80)

$TotalRaw = 0
$TotalHook = 0
$InterceptedCount = 0
$PassedCount = 0

foreach ($File in $AllFiles) {
    $Content = Get-Content $File.FullName -Raw -Encoding UTF8
    if ($null -eq $Content) { $Content = "" }
    $Lines = ($Content -split "`n").Count
    $RawTok = Get-TokenCount $Content
    $Rel = $File.FullName.Substring($RepoRoot.Length).TrimStart("\")

    if ($Lines -ge 200) {
        $HookOut = (& $Tokenix read $File.FullName 2>$null) -join "`n"
        $HookTok = Get-TokenCount $HookOut
        $SavedPct = [math]::Round((1 - $HookTok / [double]$RawTok) * 100, 1)
        $Row = "{0,-42} {1,6} {2,10} {3,11} {4,7}%  [intercepted]" -f $Rel, $Lines, $RawTok, $HookTok, $SavedPct
        Write-Host $Row -ForegroundColor Green
        $TotalRaw += $RawTok
        $TotalHook += $HookTok
        $InterceptedCount++
    } else {
        $Row = "{0,-42} {1,6} {2,10} {3,11} {4,8}  (pass)" -f $Rel, $Lines, $RawTok, "-", "-"
        Write-Host $Row -ForegroundColor DarkGray
        $PassedCount++
    }
}

Write-Host ("-" * 80)

if ($TotalRaw -gt 0) {
    $TotalSavedPct = [math]::Round((1 - $TotalHook / [double]$TotalRaw) * 100, 1)
    $TotalSavedTok = $TotalRaw - $TotalHook
    $TotalRow = "{0,-42} {1,6} {2,10} {3,11} {4,7}%" -f "TOTAL (intercepted files)", "", $TotalRaw, $TotalHook, $TotalSavedPct
    Write-Host $TotalRow -ForegroundColor Cyan

    Write-Host ""
    Write-Host "Summary:" -ForegroundColor White
    Write-Host ("  Files intercepted : " + $InterceptedCount)
    Write-Host ("  Files passed      : " + $PassedCount)
    Write-Host ("  Tokens consumed   : " + $TotalRaw + "  (without tokenix)")
    Write-Host ("  Tokens consumed   : " + $TotalHook + "  (with tokenix)")
    Write-Host ("  Tokens saved      : " + $TotalSavedTok) -ForegroundColor Green
    Write-Host ("  Reduction         : " + $TotalSavedPct + "%") -ForegroundColor Green
    $EstCost = [math]::Round($TotalSavedTok / 1000 * 0.003, 4)
    Write-Host ("  Est. cost saved   : " + [string]$EstCost + " USD  (at 3/M input tokens)")
}

Write-Host ""
Write-Host "Phase 2 -- Semantic search (requires Ollama + indexed repo)" -ForegroundColor White

$DbPath = Join-Path $RepoRoot ".tokenix\index.db"
if (-not (Test-Path $DbPath)) {
    Write-Host "  Skipped: no index found. Run: tokenix index ." -ForegroundColor Yellow
    Write-Host "  Ollama must be running: ollama serve"
    Write-Host "  Model must be pulled:   ollama pull nomic-embed-text"
} else {
    $DbAge = (Get-Date) - (Get-Item $DbPath).LastWriteTime
    if ($DbAge.TotalSeconds -gt 3600) {
        $AgeSecs = [int]$DbAge.TotalSeconds
        Write-Host ("  Warning: index is " + $AgeSecs + "s old. Re-run: tokenix index .") -ForegroundColor Yellow
    }

    Write-Host "  Simulating Grep interception for semantic queries..."
    Write-Host ""

    $Queries = @(
        "how does authentication work",
        "database connection and query execution",
        "error handling and validation",
        "user registration and password hashing"
    )

    foreach ($Q in $Queries) {
        Write-Host ("  Query: " + $Q)
        $Result = (& $Tokenix query $Q --budget 2000 2>$null) -join "`n"
        $Tok = Get-TokenCount $Result
        Write-Host ("  Returned: ~" + $Tok + " tokens")
        Write-Host ""
    }
}

Write-Host "=== benchmark complete ===" -ForegroundColor Cyan
Write-Host ""
