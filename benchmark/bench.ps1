# benchmark/bench.ps1
# tokenix benchmark — token savings + hook latency comparison.
#
# Usage:
#   cd <repo-root>
#   .\benchmark\bench.ps1
#
# Requirements: cargo in PATH. No Ollama needed.
# Phase 2 (semantic search) requires: tokenix index . run first.

$ErrorActionPreference = "Stop"

$RepoRoot = Split-Path -Parent $PSScriptRoot
$SamplesDir = Join-Path $RepoRoot "benchmark\samples"
Set-Location $RepoRoot

Write-Host ""
Write-Host "=== tokenix benchmark ===" -ForegroundColor Cyan
Write-Host ""
Write-Host "Building tokenix (release)..." -ForegroundColor Yellow
$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"
cargo build --release | Out-Null
$Tokenix = Join-Path $RepoRoot "target\release\tokenix.exe"

if (-not (Test-Path $Tokenix)) {
    Write-Error "Build failed -- cannot find $Tokenix"
    exit 1
}
Write-Host "  Built: $Tokenix" -ForegroundColor Green
Write-Host ""

function Get-TokenCount {
    param([string]$Text)
    return [int](($Text.Length + 3) / 4)
}

# ─── Phase 1: Token reduction ─────────────────────────────────────────────────

Write-Host "Phase 1 -- Token reduction (Read interception)" -ForegroundColor White
Write-Host "  tokenix intercepts reads on files >= 200 lines and returns a symbol outline."
Write-Host ""

$SrcFiles = Get-ChildItem -Path (Join-Path $RepoRoot "src") -Filter "*.rs" -File |
    Sort-Object FullName
$SampleFiles = Get-ChildItem -Path $SamplesDir -File |
    Where-Object { $_.Extension -in @(".rs", ".py", ".ts", ".go") } |
    Sort-Object FullName
$AllFiles = @($SrcFiles) + @($SampleFiles)

$Header = "{0,-44} {1,6} {2,10} {3,11} {4,8}" -f "File", "Lines", "Without", "With", "Saved"
Write-Host $Header -ForegroundColor White
Write-Host ("-" * 84)

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
        $Row = "{0,-44} {1,6} {2,10} tok {3,8} tok {4,7}%  [intercepted]" -f $Rel, $Lines, $RawTok, $HookTok, $SavedPct
        Write-Host $Row -ForegroundColor Green
        $TotalRaw += $RawTok
        $TotalHook += $HookTok
        $InterceptedCount++
    } else {
        $Row = "{0,-44} {1,6} {2,10} tok {3,11}  {4,8}  (pass-through)" -f $Rel, $Lines, $RawTok, "-", "-"
        Write-Host $Row -ForegroundColor DarkGray
        $PassedCount++
    }
}

Write-Host ("-" * 84)

if ($TotalRaw -gt 0) {
    $TotalSavedPct = [math]::Round((1 - $TotalHook / [double]$TotalRaw) * 100, 1)
    $TotalSavedTok = $TotalRaw - $TotalHook
    $TotalRow = "{0,-44} {1,6} {2,10} tok {3,8} tok {4,7}%" -f "TOTAL (intercepted files)", "", $TotalRaw, $TotalHook, $TotalSavedPct
    Write-Host $TotalRow -ForegroundColor Cyan
    Write-Host ""
    Write-Host "  Summary:" -ForegroundColor White
    Write-Host ("    Files intercepted : " + $InterceptedCount)
    Write-Host ("    Files passed      : " + $PassedCount)
    Write-Host ("    Tokens (no hook)  : " + $TotalRaw + " tokens")
    Write-Host ("    Tokens (w/ hook)  : " + $TotalHook + " tokens") -ForegroundColor Green
    Write-Host ("    Tokens saved      : " + $TotalSavedTok) -ForegroundColor Green
    Write-Host ("    Reduction         : " + $TotalSavedPct + "%") -ForegroundColor Green
    $EstCost = [math]::Round($TotalSavedTok / 1000000 * 3.00, 4)
    Write-Host ("    Est. cost saved   : `$" + [string]$EstCost + " USD  (at `$3.00/M input tokens)") -ForegroundColor Green
}

# ─── Phase 2: Hook latency ─────────────────────────────────────────────────────

Write-Host ""
Write-Host "Phase 2 -- Hook latency (wall clock, fastembed warm)" -ForegroundColor White
Write-Host "  Measures hook process startup + ONNX model init + intercept decision."
Write-Host ""

function Measure-HookMs {
    param([string]$HookInput, [int]$Runs = 3)
    $times = @()
    for ($i = 0; $i -lt $Runs; $i++) {
        $sw = [System.Diagnostics.Stopwatch]::StartNew()
        $psi = [System.Diagnostics.ProcessStartInfo]::new($Tokenix, "hook")
        $psi.RedirectStandardInput = $true
        $psi.RedirectStandardError = $true
        $psi.UseShellExecute = $false
        $psi.CreateNoWindow = $true
        $proc = [System.Diagnostics.Process]::Start($psi)
        $proc.StandardInput.WriteLine($HookInput)
        $proc.StandardInput.Close()
        $null = $proc.StandardError.ReadToEnd()
        $proc.WaitForExit()
        $sw.Stop()
        $times += $sw.ElapsedMilliseconds
    }
    return [int](($times | Measure-Object -Average).Average)
}

# Warm up the ONNX model (first subprocess call downloads nothing but initializes)
Write-Host "  Warming up ONNX model (first call may be slower)..." -ForegroundColor DarkGray
$psi = [System.Diagnostics.ProcessStartInfo]::new($Tokenix, "hook")
$psi.RedirectStandardInput = $true; $psi.RedirectStandardError = $true
$psi.UseShellExecute = $false; $psi.CreateNoWindow = $true
$wp = [System.Diagnostics.Process]::Start($psi)
$wp.StandardInput.WriteLine('{"tool_name":"Grep","tool_input":{"pattern":"how does the embedding model work here"}}')
$wp.StandardInput.Close(); $null = $wp.StandardError.ReadToEnd(); $wp.WaitForExit()

$Cases = @(
    @{ Label = "Read passthrough (small file, Cargo.toml)";
       Input = '{"tool_name":"Read","tool_input":{"file_path":"Cargo.toml"}}' },
    @{ Label = "Read intercept   (large file, src/main.rs)";
       Input = '{"tool_name":"Read","tool_input":{"file_path":"src/main.rs"}}' },
    @{ Label = "Grep passthrough (short pattern, 2 words)";
       Input = '{"tool_name":"Grep","tool_input":{"pattern":"embed query"}}' }
)

$LatHeader = "  {0,-50} {1,8}" -f "Scenario", "Avg (ms)"
Write-Host $LatHeader -ForegroundColor White
Write-Host ("  " + "-" * 60)

foreach ($Case in $Cases) {
    $ms = Measure-HookMs -HookInput $Case.Input -Runs 3
    $Row = "  {0,-50} {1,8}" -f $Case.Label, $ms
    Write-Host $Row
}

# Check if index exists for semantic grep
$GlobalTokenix = Join-Path $env:USERPROFILE ".tokenix"
$HasIndex = (Get-ChildItem -Path $GlobalTokenix -Filter "*.db" -ErrorAction SilentlyContinue | Measure-Object).Count -gt 0

if ($HasIndex) {
    $SemanticInput = '{"tool_name":"Grep","tool_input":{"pattern":"how does the embedding and semantic search work"}}'
    $ms = Measure-HookMs -HookInput $SemanticInput -Runs 3
    $Row = "  {0,-50} {1,8}" -f "Grep intercept   (semantic query, 7 words)", $ms
    Write-Host $Row -ForegroundColor Green
} else {
    Write-Host "  Grep intercept: skipped - run 'tokenix index .' first" -ForegroundColor Yellow
}

# ─── Phase 3: Semantic search ─────────────────────────────────────────────────

Write-Host ""
Write-Host "Phase 3 -- Semantic search quality" -ForegroundColor White

if (-not $HasIndex) {
    Write-Host "  Skipped: no index found. Run: tokenix index ." -ForegroundColor Yellow
} else {
    Write-Host "  Simulating semantic Grep interception..."
    Write-Host ""

    $Queries = @(
        "how does authentication work",
        "database connection and query execution",
        "error handling and validation",
        "embedding generation and vector search"
    )

    $QHeader = "  {0,-44} {1,8}" -f "Query", "Tokens"
    Write-Host $QHeader -ForegroundColor White
    Write-Host ("  " + "-" * 55)

    foreach ($Q in $Queries) {
        $Result = (& $Tokenix query $Q --budget 2000 2>$null) -join "`n"
        $Tok = Get-TokenCount $Result
        $Row = "  {0,-44} {1,8}" -f $Q, $Tok
        Write-Host $Row
    }
}

# ─── Summary table ─────────────────────────────────────────────────────────────

Write-Host ""
Write-Host "=== Summary ===" -ForegroundColor Cyan
Write-Host ""
Write-Host "  Token reduction"
Write-Host ("    Without tokenix : " + $TotalRaw + " tokens")
Write-Host ("    With tokenix    : " + $TotalHook + " tokens  (" + $TotalSavedPct + "% reduction)") -ForegroundColor Green
Write-Host ""
Write-Host "  Embedding backend"
Write-Host "    fastembed ONNX (nomic-embed-text-v1.5-Q, 130 MB)"
Write-Host "    No Ollama server required. Model auto-downloaded on first run."
Write-Host ""
Write-Host "=== benchmark complete ===" -ForegroundColor Cyan
Write-Host ""
