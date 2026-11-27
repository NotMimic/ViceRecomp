# settings.ps1: THE table of Victory settings for this folder, and nothing else.
# Read by play.ps1 and prepare.ps1 (". .\settings.ps1"). ASCII only.
#
# Each setting: its name (without the VICTORY_ prefix), its value, the modes it
# applies to ('all', 'quality', 'fluidity'), whether it also matters for the
# preparation (shader translation, pipelines), what it does, and what was measured
# on the reference PC (AMD Radeon RX 7900 XTX, Ryzen 9 7950X3D, 32 GB RAM, Xbox
# Series X profile, 4K output). These settings are made for AMD Radeon cards only:
# they were not tested on NVIDIA or Intel cards.

# The title's executable, in the game_dir folder named in victory.toml.
$GameExe = 'game_scarlett_master.exe'
# The title's shader archive, in the same folder (used by PREPARE).
$GameShaders = 'shaders.rpf'

$Settings = @(
    # ---- Base ----
    [pscustomobject]@{ Name = 'TEXTURE_NO_WAIT'; Value = '1'; Modes = 'all'; Prep = $false
        About = "textures are read without waiting for the title's memory to hold still"; Measured = '' }
    [pscustomobject]@{ Name = 'RELAY'; Value = '2'; Modes = 'all'; Prep = $false
        About = "a thread of its own (the relay) takes the submissions the card has finished and tells the title"; Measured = '' }
    [pscustomobject]@{ Name = 'WIDE_BACK'; Value = '1'; Modes = 'all'; Prep = $false
        About = "what shaders write goes back to the title's memory on several threads"; Measured = '' }
    [pscustomobject]@{ Name = 'CARD_REHASH_RUNS'; Value = '1'; Modes = 'all'; Prep = $false
        About = "the compare passes on the card are grouped"; Measured = '' }
    [pscustomobject]@{ Name = 'JOIN_COPIES'; Value = '1'; Modes = 'all'; Prep = $false
        About = "buffer copies that follow each other become one copy"; Measured = '' }
    [pscustomobject]@{ Name = 'INDIRECT_WINDOW'; Value = '1'; Modes = 'all'; Prep = $false
        About = "indirect draws read their arguments through a window"; Measured = '' }
    [pscustomobject]@{ Name = 'RELAY_COPY'; Value = '1'; Modes = 'all'; Prep = $false
        About = "the relay also does the copies back"; Measured = '' }
    [pscustomobject]@{ Name = 'CARD_STEPS'; Value = '2'; Modes = 'all'; Prep = $false
        About = "fills and copies that would wait for the card are done on the card"; Measured = '' }
    [pscustomobject]@{ Name = 'INDIRECT_BATCH'; Value = '2'; Modes = 'all'; Prep = $false
        About = "indirect draw arguments are prepared in batches"; Measured = '' }
    [pscustomobject]@{ Name = 'MAP_THREADS'; Value = '1'; Modes = 'all'; Prep = $false
        About = "the title remaps its memory on one thread"; Measured = "8 threads measured worse on 2026-11-26 (A/B 600 s: median 45.2 against 43.6 ms)" }
    [pscustomobject]@{ Name = 'TITLE_PRIORITIES'; Value = '0'; Modes = 'all'; Prep = $false
        About = "the title's thread priorities are left as they are"; Measured = '' }
    [pscustomobject]@{ Name = 'CARD_AWAKE'; Value = '1'; Modes = 'all'; Prep = $false
        About = "the card is kept awake between two submissions"; Measured = '' }
    [pscustomobject]@{ Name = 'SHOW_AFTER'; Value = '1'; Modes = 'all'; Prep = $false
        About = "a frame is shown once its submissions are sent"; Measured = '' }
    [pscustomobject]@{ Name = 'AMBIENT'; Value = '0.8'; Modes = 'all'; Prep = $false
        About = "strength of the fill light while ray tracing is off"; Measured = '' }
    [pscustomobject]@{ Name = 'D3D12_DRED'; Value = 'light'; Modes = 'all'; Prep = $false
        About = "if the card hangs, Windows records where it was"; Measured = '' }
    [pscustomobject]@{ Name = 'THIN'; Value = '1'; Modes = 'all'; Prep = $false
        About = "the title's command lists are written down and translated on 8 threads"; Measured = '' }
    [pscustomobject]@{ Name = 'JOIN_PARTS'; Value = '8'; Modes = 'all'; Prep = $false
        About = "up to 8 submission parts are prepared together"; Measured = '' }
    [pscustomobject]@{ Name = 'UPLOAD_AHEAD'; Value = '1'; Modes = 'all'; Prep = $false
        About = "textures are sent to the card before the lists that read them"; Measured = '' }

    # ---- Renderer patches ----
    [pscustomobject]@{ Name = 'PORT_CONSTANTS'; Value = '1'; Modes = 'all'; Prep = $true
        About = "renderer patch 1: the title's graphics memory is read by the card in place"; Measured = "the 4 patches together: 39-44 ms a frame instead of 49-56 (2 sessions of 7 min, 2026-11-26)" }
    [pscustomobject]@{ Name = 'PORT_CBV'; Value = '1'; Modes = 'all'; Prep = $true
        About = "renderer patch 5: shaders read constant buffers in place (changes the shader translation)"; Measured = "see PORT_CONSTANTS" }
    [pscustomobject]@{ Name = 'PORT_GUARD'; Value = '1'; Modes = 'all'; Prep = $true
        About = "renderer patch 6: the lock of the memory guards is shared"; Measured = "see PORT_CONSTANTS" }
    [pscustomobject]@{ Name = 'PORT_JOURNAL'; Value = '1'; Modes = 'all'; Prep = $true
        About = "renderer patch 7: heap records are asked for at the title's submission"; Measured = "see PORT_CONSTANTS" }
    [pscustomobject]@{ Name = 'PORT_CONSTANTS_ORDER'; Value = '1'; Modes = 'all'; Prep = $false
        About = "with renderer patch 1: indirect draws whose arguments the card reads in place are prepared one at a time, right before they run (trees and palms no longer drop out for frames)"; Measured = "2026-11-27: trees no longer drop out, eyes correct; 12.7 fps, median 72 ms in the world" }

    # ---- Victory Grimkaaah's own settings and code ----
    [pscustomobject]@{ Name = 'UP_QUEUE'; Value = '1'; Modes = 'all'; Prep = $false
        About = "uploads to the card's memory go through a copy queue of their own"; Measured = "-3.5 ms a frame (A/B 600 s, 9 switches)" }
    [pscustomobject]@{ Name = 'REAPER'; Value = '1'; Modes = 'all'; Prep = $false
        About = "command lists back from the card are cleaned up on a thread of their own"; Measured = "-2.3 to -3.8 ms a frame (2 A/B runs of 8 and 10 min)" }
    [pscustomobject]@{ Name = 'TEXBACK_FREE'; Value = '1'; Modes = 'all'; Prep = $false
        About = "textures read back to the title no longer take the global remapping lock"; Measured = "the relay waited 173 ms for it in the timeline window" }
    [pscustomobject]@{ Name = 'SUBMIT_FRAMES'; Value = '2'; Modes = 'all'; Prep = $false
        About = "2 frames in flight: the title prepares the next frame while ours is sent (one more frame of latency)"; Measured = "the title no longer waits at present: 0 ms instead of 11-17" }
    [pscustomobject]@{ Name = 'SUBMIT_DEPTH'; Value = '128'; Modes = 'all'; Prep = $false
        About = "the submission queue holds two frames (about 37 submissions a frame)"; Measured = "with SUBMIT_FRAMES" }
    [pscustomobject]@{ Name = 'VSYNC'; Value = '1'; Modes = 'all'; Prep = $false
        About = "each frame waits for the screen's refresh: no tearing (FreeSync follows)"; Measured = "at 180 Hz: 5.6 ms of waiting at most" }
    [pscustomobject]@{ Name = 'FILL_FORWARD'; Value = '1'; Modes = 'all'; Prep = $false
        About = "a fill behind a title copy reads the copy's bytes instead of waiting for the card"; Measured = "with ray tracing: card wait 0 ms instead of 19-23, mean frame 58.6 instead of 62.7 ms, no more picture defects" }
    [pscustomobject]@{ Name = 'PLACED_ORDER'; Value = '1'; Modes = 'all'; Prep = $false
        About = "fills in front of a title copy are done on the card, in order"; Measured = "with FILL_FORWARD (alone: no effect)" }
    [pscustomobject]@{ Name = 'COPY_AHEAD'; Value = '1'; Modes = 'all'; Prep = $false
        About = "a title buffer's copy is made as soon as the title declares the buffer (while loading), not at the first frame that uses it"; Measured = "Quality, 2026-11-27: start freeze 3.8 s instead of 5.2; 13.9 fps instead of 12.5; 90th percentile 59 ms instead of 50 (to watch)" }

    # ---- By mode ----
    [pscustomobject]@{ Name = 'BVH_BUILDER'; Value = '1'; Modes = 'quality'; Prep = $false
        About = "the title's ray tracing: lit interiors, no dropped draws"; Measured = "busy scenes 10-16 fps instead of 24; lighter scenes 20-27 fps" }

    # ---- For the log and the report ----
    [pscustomobject]@{ Name = 'LOG'; Value = 'info'; Modes = 'all'; Prep = $true
        About = "the log level (read by the report)"; Measured = '' }
    [pscustomobject]@{ Name = 'NO_COLOR'; Value = '1'; Modes = 'all'; Prep = $true
        About = "a log without color codes"; Measured = ''; NoPrefix = $true }
)

# The variables that say where files are: never cleared.
$KeptPaths = @('VICTORY_CONFIG', 'VICTORY_GAME_DIR', 'VICTORY_CACHE_DIR', 'VICTORY_DATA_DIR')

# The environment variable of a setting.
function Get-VariableName($s) {
    if ($s.PSObject.Properties['NoPrefix'] -and $s.NoPrefix) { return $s.Name }
    return "VICTORY_$($s.Name)"
}

# The settings of a mode ('quality', 'fluidity'), in the table's order: name -> value.
function Get-ModeSettings([string]$Mode) {
    $chosen = [ordered]@{}
    foreach ($s in $Settings) {
        if ($s.Modes -eq 'all' -or $s.Modes -eq $Mode) { $chosen[(Get-VariableName $s)] = $s.Value }
    }
    return $chosen
}

# The settings of the preparation: name -> value.
function Get-PrepSettings {
    $chosen = [ordered]@{}
    foreach ($s in $Settings | Where-Object { $_.Prep }) { $chosen[(Get-VariableName $s)] = $s.Value }
    return $chosen
}

# Clears the inherited VICTORY_* variables (but the paths) and NO_COLOR, then sets $Chosen.
function Set-VictoryEnvironment($Chosen) {
    Get-ChildItem Env:VICTORY_* | Where-Object { $_.Name -notin $KeptPaths } | ForEach-Object { Remove-Item "Env:$($_.Name)" }
    Remove-Item Env:NO_COLOR -ErrorAction SilentlyContinue
    foreach ($k in $Chosen.Keys) { Set-Item "Env:$k" $Chosen[$k] }
}

# The fingerprint of a preparation: the build of victory.exe and the settings that matter.
function Get-PrepFingerprint([string]$Victory) {
    $exe = (Get-FileHash -Algorithm SHA256 -Path $Victory).Hash
    $p = Get-PrepSettings
    $text = ($p.Keys | Sort-Object | ForEach-Object { "$_=$($p[$_])" }) -join ';'
    return "$exe;$text"
}

# The title's folder, read from victory.toml (game_dir = '...').
function Get-GameDir([string]$Root) {
    $toml = Join-Path $Root 'victory.toml'
    if (-not (Test-Path $toml)) { return $null }
    foreach ($line in Get-Content $toml) {
        if ($line -match "^\s*game_dir\s*=\s*['""](.+)['""]\s*$") { return $Matches[1] }
    }
    return $null
}
