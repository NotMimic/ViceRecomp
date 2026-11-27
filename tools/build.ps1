# build.ps1: builds Victory from source\ and puts the .exe files in bin\. Started by BUILD.bat.
# The builds stay in target\ (beside source\): the next one only redoes what changed.
# Needs Rust (rustup, the toolchain of rust-toolchain.toml) and the Windows SDK / MSVC build tools. ASCII only.
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
function Say([string]$text, [string]$color = 'Gray') { Write-Host $text -ForegroundColor $color }

if (Get-Process victory -ErrorAction SilentlyContinue) { Say 'STOP: close the title first (victory.exe is running).' 'Red'; exit 1 }
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) { Say 'STOP: cargo (Rust) is not installed.' 'Red'; exit 1 }
$env:CARGO_TARGET_DIR = Join-Path $root 'target'
Push-Location (Join-Path $root 'source')
try {
    Say '=== Building the play version (release) ===' 'Cyan'
    & cargo build --locked --release -p victory
    if ($LASTEXITCODE -ne 0) { Say 'STOP: the release build failed.' 'Red'; exit 1 }
    Say '=== Building the diagnostic version (diag) ===' 'Cyan'
    & cargo build --locked --profile diag -p victory --features diagnostics
    if ($LASTEXITCODE -ne 0) { Say 'STOP: the diag build failed.' 'Red'; exit 1 }
} finally { Pop-Location }
foreach ($b in 'release', 'diag') {
    $from = Join-Path $root "target\$b\victory.exe"
    $to = Join-Path $root "bin\$b"
    New-Item -ItemType Directory -Force -Path $to | Out-Null
    Copy-Item $from (Join-Path $to 'victory.exe') -Force
    Say ("bin\{0}\victory.exe updated" -f $b) 'Green'
}
Say 'Run PREPARE.bat again: the launcher will remind you.' 'Yellow'
