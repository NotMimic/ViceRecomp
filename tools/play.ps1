# play.ps1: the Victory Grimkaaah launcher. Started by "Victory Grimkaaah.bat".
#   -Mode quality|fluidity|diagnostic   no menu
#   -Check                              check and show everything, without starting the title
#   -More @{VICTORY_X='1'}              extra settings for a test (written in the log)
# Every setting is in settings.ps1. ASCII only.
param(
    [ValidateSet('', 'quality', 'fluidity', 'diagnostic')][string]$Mode = '',
    [switch]$Check,
    [hashtable]$More = @{}
)
$ErrorActionPreference = 'Stop'
$tools = $PSScriptRoot
$root = Split-Path $tools -Parent
. (Join-Path $tools 'settings.ps1')

function Say([string]$text, [string]$color = 'Gray') { Write-Host $text -ForegroundColor $color }
function Warn([string]$text) { Write-Host "WARNING: $text" -ForegroundColor Yellow }
function Stop-Launcher([string]$text) { Write-Host "STOP: $text" -ForegroundColor Red; exit 1 }

Say ''
Say '=== VICTORY GRIMKAAAH ===' 'Cyan'

# ---- 1. Checks ----
$admin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if ($admin) { Stop-Launcher "start Victory Grimkaaah.bat from a normal window, not as administrator (the driver would keep its shader cache elsewhere)." }
$gameDir = Get-GameDir $root
if (-not $gameDir) { Stop-Launcher "no game_dir in $root\victory.toml (copy victory.example.toml to victory.toml and set it)." }
$game = Join-Path $gameDir $GameExe
if (-not (Test-Path $game)) { Stop-Launcher "the title is not there: $game" }
$exes = @{ release = Join-Path $root 'bin\release\victory.exe'; diag = Join-Path $root 'bin\diag\victory.exe' }
if (-not (Test-Path $exes.release)) { Stop-Launcher "no bin\release\victory.exe: run BUILD.bat first." }
if (Get-Process victory -ErrorAction SilentlyContinue) { Stop-Launcher "a victory.exe is already running: close the title first." }
$os = Get-CimInstance Win32_OperatingSystem
$limitGb = [math]::Round($os.TotalVirtualMemorySize / 1MB, 1)
$freeGb = [math]::Round($os.FreePhysicalMemory / 1MB, 1)
Say ("Title: {0}" -f $game)
# Victory Grimkaaah is made and tested for AMD Radeon cards only.
$cards = @(Get-CimInstance Win32_VideoController -ErrorAction SilentlyContinue)
$amd = @($cards | Where-Object { "$($_.AdapterCompatibility) $($_.Name)" -match 'AMD|Advanced Micro Devices|Radeon' })
Say ("Card: {0}" -f (($cards | ForEach-Object { $_.Name }) -join ', '))
if ($amd.Count -eq 0) { Warn "no AMD Radeon card found. Victory Grimkaaah is made and tested for AMD Radeon cards only (not NVIDIA or Intel)." }
Say ("Memory: {0} GB of free RAM, commit limit {1} GB" -f $freeGb, $limitGb)
if ($limitGb -lt 120) { Warn "commit limit under 120 GB: the title asks for about 86. Set the page file to 98304 MB (README.md)." }
$fingerprint = Get-PrepFingerprint $exes.release
$prepFile = Join-Path $tools 'prepared.txt'
$prepOk = (Test-Path $prepFile) -and ((Get-Content $prepFile -TotalCount 1) -eq $fingerprint)
if (-not $prepOk) { Warn "the preparation was not done for this build and these settings: run PREPARE.bat (otherwise 1 to 3 fps and an incomplete picture at first)." }

# ---- 2. The mode ----
$modes = [ordered]@{
    quality    = @{ Title = 'Quality: ray tracing, 10 to 27 fps'; Settings = 'quality'; Build = 'release' }
    fluidity   = @{ Title = 'Fluidity: no ray tracing, about 24 fps'; Settings = 'fluidity'; Build = 'release' }
    diagnostic = @{ Title = 'Diagnostic: as Quality, detailed log, a little slower'; Settings = 'quality'; Build = 'diag' }
}
if ($Check) {
    Say ''
    Say 'Dry run: nothing is started.' 'Cyan'
    Say ("Preparation up to date: {0}" -f $(if ($prepOk) { 'yes' } else { 'no' }))
    foreach ($m in $modes.Keys) {
        $chosen = Get-ModeSettings $modes[$m].Settings
        Say ''
        Say ("--- {0} ({1} settings, build {2}) ---" -f $modes[$m].Title, $chosen.Count, $modes[$m].Build) 'Cyan'
        foreach ($k in $chosen.Keys) { Say ("  {0}={1}" -f $k, $chosen[$k]) }
        Say ("  command: `"{0}`" run `"{1}`" --execute   (folder: {2})" -f $exes[$modes[$m].Build], $game, $root)
    }
    exit 0
}
if (-not $Mode) {
    Say ''
    Say '  1  Quality    : ray tracing, 10 to 27 fps'
    Say '  2  Fluidity   : no ray tracing, about 24 fps'
    Say '  3  Diagnostic : as Quality, detailed log, a little slower'
    Say '  4  Quit'
    $choice = Read-Host 'Your choice'
    switch ($choice.Trim()) {
        '1' { $Mode = 'quality' }
        '2' { $Mode = 'fluidity' }
        '3' { $Mode = 'diagnostic' }
        default { Say 'Nothing started.'; exit 0 }
    }
}
$m = $modes[$Mode]
$victory = $exes[$m.Build]
if (-not (Test-Path $victory)) { Stop-Launcher "no ${victory}: run BUILD.bat." }
if (-not $prepOk) {
    $go = Read-Host 'Preparation not up to date. Start anyway? (y/n)'
    if ($go.Trim().ToLower() -ne 'y') { Say 'Run PREPARE.bat, then Victory Grimkaaah.bat.'; exit 0 }
}

# ---- 3. The settings ----
$chosen = Get-ModeSettings $m.Settings
foreach ($k in $More.Keys) { $chosen[$k] = [string]$More[$k] }
Set-VictoryEnvironment $chosen

# ---- 4. The start ----
$logs = Join-Path $root 'logs'
New-Item -ItemType Directory -Force -Path $logs | Out-Null
$when = Get-Date -Format 'yyyyMMdd-HHmmss'
$log = Join-Path $logs "session-$when.txt"
$header = @("# VICTORY GRIMKAAAH, session of $(Get-Date -Format 'yyyy-MM-dd HH:mm:ss')", "# mode: $Mode ($($m.Title))", "# build: $($m.Build)", "# preparation up to date: $prepOk")
$header += $chosen.Keys | ForEach-Object { "# setting $_=$($chosen[$_])" }
[IO.File]::WriteAllText($log, (($header -join "`r`n") + "`r`n"), (New-Object Text.UTF8Encoding $false))
$started = Get-Date
Say ''
Say ("Starting: {0}" -f $m.Title) 'Green'
Say ("Log: {0}" -f $log)
# Optional, local only: a Discord status script beside this one.
$discord = Join-Path $tools 'discord-presence.ps1'
if (Test-Path $discord) {
    Start-Process powershell -WindowStyle Hidden -ArgumentList '-NoProfile', '-ExecutionPolicy', 'RemoteSigned', '-File', "`"$discord`"" | Out-Null
}
# cmd writes the title's output as it is (UTF-8) after the header.
$line = "`"`"$victory`" run `"$game`" --execute >> `"$log`" 2>&1`""
$p = Start-Process cmd.exe -ArgumentList '/d', '/c', $line -WorkingDirectory $root -NoNewWindow -PassThru

# During the session: the frame rate every 10 s, read at the end of the log.
function Get-LogTail([string]$path, [int]$bytes = 65536) {
    try {
        $f = [IO.File]::Open($path, 'Open', 'Read', 'ReadWrite')
        try {
            $from = [math]::Max(0, $f.Length - $bytes)
            $f.Seek($from, 'Begin') | Out-Null
            $buffer = New-Object byte[] ($f.Length - $from)
            $read = $f.Read($buffer, 0, $buffer.Length)
            return [Text.Encoding]::UTF8.GetString($buffer, 0, $read)
        } finally { $f.Dispose() }
    } catch { return '' }
}
Say 'The session runs. Close the title normally to finish (the report comes next).'
$last = ''
while (-not $p.HasExited) {
    Start-Sleep -Seconds 10
    $tail = Get-LogTail $log
    $found = [regex]::Matches($tail, '([0-9.]+) fps, ([0-9.]+) ms/frame[^\r\n]*?(\d+) draws/frame')
    if ($found.Count -gt 0) {
        $f = $found[$found.Count - 1]
        $text = "{0}  {1} fps, {2} ms a frame, {3} draws" -f (Get-Date -Format 'HH:mm:ss'), $f.Groups[1].Value, $f.Groups[2].Value, $f.Groups[3].Value
        if ($text -ne $last) { Say $text; $last = $text }
    }
}
$seconds = [int]((Get-Date) - $started).TotalSeconds
Say ''
Say ("Session over after {0} min {1} s." -f [int]($seconds / 60), ($seconds % 60)) 'Green'

# ---- 5. The report ----
$report = Join-Path $logs "report-$when.txt"
$python = Get-Command python -ErrorAction SilentlyContinue
if ($python) {
    $env:PYTHONIOENCODING = 'utf-8'
    & $python.Source (Join-Path $tools 'report.py') $log $report
} else {
    Warn "Python is not installed: no report (the log is here: $log)."
}
$dataDir = Join-Path $env:LOCALAPPDATA 'victory\data'
$crashes = Get-ChildItem -Path $dataDir -Recurse -Filter 'crashes.log' -ErrorAction SilentlyContinue | Where-Object { $_.LastWriteTime -gt $started } | Sort-Object LastWriteTime | Select-Object -Last 1
if ($crashes) {
    Warn "the session ended on a card hang or a crash. End of the report:"
    $text = [IO.File]::ReadAllText($crashes.FullName)
    $i = $text.LastIndexOf('unix time')
    if ($i -ge 0) { $text.Substring($i, [math]::Min(1200, $text.Length - $i)) -split "`n" | Select-Object -First 12 | ForEach-Object { Say "  $_" 'Yellow' } }
}
