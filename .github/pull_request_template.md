## What this changes

<!-- One change. What it does and why, in a few sentences. -->

## Switch

<!-- The VICTORY_... variable that turns it on. It must be off by default in the code. -->
`VICTORY_`

## Measured on my PC

| | Switch off | Switch on |
|---|---|---|
| Frame rate in the world (fps) | | |
| Median frame time (ms) | | |
| 90th percentile (ms) | | |
| Biggest freeze (ms) | | |
| Errors / card losses | | |

- PC (CPU, AMD card, RAM, driver):
- Mode (Quality, Fluidity, Diagnostic) and scene:
- How it was measured (`VICTORY_AB=10,...` in one session, or two sessions):
- Session reports attached: yes / no

## Checklist

- [ ] One change only, behind its own `VICTORY_...` switch, off by default in the code
- [ ] Measured on an AMD Radeon card, numbers above, reports attached
- [ ] No regression in frame rate, regularity, freezes, picture or stability
- [ ] `cargo test --locked --profile diag -p victory-gpu --features diagnostics --lib` passes
- [ ] `CHANGES.md` updated (and `tools\settings.ps1` if it should be on)
- [ ] No game files, no game names, no personal paths; English only
- [ ] Code is mine, or under a GPL-3.0-compatible license with its source named
