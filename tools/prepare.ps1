# prepare.ps1: translates the title's shaders and makes the pipelines with our settings, before playing.
# Started by PREPARE.bat. To do again after BUILD.bat or a change of a setting marked Prep in
# settings.ps1 (the launcher says so). No game window. ASCII only.
#   -Mode quick   the pipelines earlier sessions used (about 4,400, a few minutes)
#   -Mode full    every pipeline the title creates (about 84,000, the whole title, long):
#                 an area never visited has no pipeline left to compile while playing.
#                 What is done already is not done again: it can be stopped and started again.
param([ValidateSet('', 'quick', 'full')][string]$Mode = '')
$ErrorActionPreference = 'Stop'
$tools = $PSScriptRoot
$root = Split-Path $tools -Parent
. (Join-Path $tools 'settings.ps1')
function Say([string]$text, [string]$color = 'Gray') { Write-Host $text -ForegroundColor $color }

if (Get-Process victory -ErrorAction SilentlyContinue) { Say 'STOP: close the title first (victory.exe is running).' 'Red'; exit 1 }
$victory = Join-Path $root 'bin\release\victory.exe'
if (-not (Test-Path $victory)) { Say 'STOP: no bin\release\victory.exe, run BUILD.bat.' 'Red'; exit 1 }
$gameDir = Get-GameDir $root
$shaders = if ($gameDir) { Join-Path $gameDir $GameShaders } else { '' }
if (-not $shaders -or -not (Test-Path $shaders)) { Say "STOP: no $GameShaders in the title's folder ($gameDir)." 'Red'; exit 1 }

if (-not $Mode) {
    Say ''
    Say '  1  Quick : the pipelines your sessions already used (a few minutes)'
    Say '  2  Full  : every pipeline of the whole title (long: start it when you are not playing)'
    Say '  3  Quit'
    $choice = Read-Host 'Your choice'
    switch ($choice.Trim()) {
        '1' { $Mode = 'quick' }
        '2' { $Mode = 'full' }
        default { Say 'Nothing started.'; exit 0 }
    }
}
$chosen = Get-PrepSettings
Set-VictoryEnvironment $chosen
Say '=== Preparation with these settings ===' 'Cyan'
foreach ($k in $chosen.Keys) { Say ("  {0}={1}" -f $k, $chosen[$k]) }
Push-Location $root
try {
    Say '=== 1/2 Shader translation ===' 'Cyan'
    & $victory shaders $shaders
    if ($LASTEXITCODE -ne 0) { Say 'STOP: the shader translation failed.' 'Red'; exit 1 }
    if ($Mode -eq 'full') {
        Say '=== 2/2 Every pipeline of the title (long; Ctrl+C stops it, the next run goes on from there) ===' 'Cyan'
        & $victory warm
    } else {
        Say '=== 2/2 Pipelines already used ===' 'Cyan'
        & $victory warm --used
    }
    if ($LASTEXITCODE -ne 0) { Say 'STOP: the pipeline preparation failed.' 'Red'; exit 1 }
} finally { Pop-Location }
Set-Content -Path (Join-Path $tools 'prepared.txt') -Value (Get-PrepFingerprint $victory) -Encoding ascii
Say 'Preparation done. You can start Victory Grimkaaah.bat.' 'Green'
