# Contributing to ViceRecomp

Thank you for helping. ViceRecomp moves forward by one rule: **a change is
kept only if it is measured better.** These rules keep issues and pull requests
useful and the project going in one direction.

## Scope

- **AMD Radeon cards only.** Changes for NVIDIA or Intel cards are out of scope.
- **Xbox Series X profile, 4K output.** Ray tracing in Quality mode.
- **No game files, no game names.** Never attach, link or paste files, shaders,
  textures or code of the title. Write "the title". Use only your own copy.
- **English only**, in code, comments, issues and pull requests.

## Issues

- Use a template: **Bug report** or **Performance report**. Blank issues are
  closed.
- One problem per issue. Search first: if it is already reported, add your
  report to that issue instead of opening a new one.
- Always attach the session report (`logs\report-<date>.txt`) and say which mode
  you played (Quality, Fluidity or Diagnostic).
- An issue without a report, or about another graphics card, may be closed.

## Pull requests

1. **One change per pull request.** Small and focused; no reformatting of code
   you did not change.
2. **Behind a switch.** Every new behavior is behind its own `VICTORY_...`
   environment variable, **off by default in the code**. It is turned on in
   `tools\settings.ps1` only when it measured better.
3. **Measured.** Give the numbers on your PC (CPU, AMD card, RAM, driver) with the
   switch off and on, in the same scene. Best: one Diagnostic session with
   `VICTORY_AB=10,VICTORY_YOUR_SWITCH`, which switches it every 10 s and logs each
   phase. Attach the session reports. A claim without numbers is not merged.
4. **No regression.** Frame rate, regularity (90th percentile), freezes, picture
   (no missing objects, wrong colors or black areas) and stability (no errors, no
   card loss) must not get worse.
5. **Tests pass:**
   `cargo test --locked --profile diag -p victory-gpu --features diagnostics --lib`
   (run from `source\`).
6. **Documented.** Add your change to `CHANGES.md` (what, which files, the
   switch, the numbers, the date) and its line to `tools\settings.ps1` if it is
   turned on.
7. **License.** Victory Grimkaaah is GPL-3.0-or-later. Only contribute code you
   wrote or code under a compatible license, and name its source in the commit
   message and in `CHANGES.md`.

Pull requests that do not follow the template, or that change many unrelated
things at once, are closed with a note on what to change.

## Review

The maintainer (Grimkaaah) reviews every pull request, may ask for a measurement
or a smaller change, and merges with a squash commit. `main` only changes through
reviewed pull requests.
