# Changes in Victory Grimkaaah

Victory Grimkaaah is a modified version of the open-source Victory compatibility
layer (GNU GPL v3.0 or later, see [source/LICENSE](source/LICENSE)). This file lists
every change made for Victory Grimkaaah, with its date. Every code change is off by
default in the code and turned on, when it measured better, by `tools\settings.ps1`.

Victory Grimkaaah is made for **AMD Radeon cards only**: it was made and tested on
an AMD Radeon RX 7900 XTX, and not on NVIDIA or Intel cards.

Measurements are from the reference PC: Windows 11 Pro, Ryzen 9 7950X3D, 32 GB RAM
with a 96 GB page file, AMD Radeon RX 7900 XTX 24 GB, Xbox Series X profile, 4K output.

## Version 1.0 (2026-11-27)

### Launcher and tools (new, 2026-11-26 and 2026-11-27)

- `Victory Grimkaaah.bat` and `tools\play.ps1`: the launcher. A menu (Quality,
  Fluidity, Diagnostic), checks before starting, one log per session, the frame rate
  every 10 s, a report at the end and the end of the crash report if the session
  ended on one. `-Mode`, `-Check` (a dry run that shows every setting and the exact
  command) and `-More @{...}` (extra settings for a test, written in the log).
- `tools\settings.ps1`: the one table of settings (name, value, modes, whether it
  matters for the preparation, what it does, what it measured).
- `PREPARE.bat` and `tools\prepare.ps1`: shader translation, then the pipelines,
  Quick (`victory warm --used`) or Full (`victory warm`, every pipeline the title
  creates); a fingerprint of the build and the settings that matter, checked by the
  launcher.
- `BUILD.bat` and `tools\build.ps1`: the release and diagnostic builds into `bin\`.
- `tools\report.py`: the session report.

### Code (`source\crates\victory-gpu\src\`)

`host.rs`
- `VICTORY_REAPER`: a `victory-reaper` thread resets the command lists that came
  back from the card, off the submitting thread. -2.3 to -3.8 ms a frame (two A/B
  runs of 8 and 10 min). Adapted from mgrein's change a2cf68bb (GPL-3.0).
- `write_title_once`: one write into title memory through the kernel, for the
  texture read-back below.
- `frame_gaps_line`: a line every second with the time between frames (median, 90th
  percentile, longest, frames over 50 and 100 ms).
- `VICTORY_VSYNC`: presents wait for the screen's refresh (no tearing).
- `VICTORY_FILL_TRACE`, `VICTORY_COST_TRACE` (diagnostics): what holds a fill back,
  and what each copy costs.
- `execute_parts` calls `place::forward_fills` (`VICTORY_FILL_FORWARD`).
- The once-a-second diagnostic lines of `shadow::made_line` and `list::reserve_line`.

`texback.rs`
- `VICTORY_TEXBACK_FREE`: textures read back to the title are read and tiled
  without the global remapping lock; only the final check and write take it. Two
  tests: `tickets_tracked_without_the_mapping_lock_miss_no_invalidation`,
  `the_kernels_write_fails_on_pages_it_cannot_write`.

`host\place.rs`
- `VICTORY_FILL_FORWARD`: a fill behind a title copy done at its place reads the
  copy's bytes instead of waiting for the card. With ray tracing: card wait 0 ms
  instead of 19-23 ms, mean frame 58.6 instead of 62.7 ms, no more picture defects.
  Four tests (`forward_tests`).

`host\card_steps.rs`
- What holds back copies done at their place (`VICTORY_FILL_TRACE`); the count of
  forwarded fills in the card steps' line.

`shadow.rs`
- `VICTORY_COPY_AHEAD` (2026-11-27): the thread `victory-ahead` makes the copy of
  each title buffer as soon as the title declares it (`shadow::declared`, called by
  `resource.rs`), instead of in the first frame that uses it; every copy is then made
  aside (`made_aside`). Before it, the first frame of the world made 3,459 copies
  (1.1 GB, 2.0 s), 3,403 of them of buffers declared 10 s to a minute earlier.
  Quality: freeze 5.2 s -> 3.8 s, 12.5 -> 13.9 fps. Test:
  `a_copy_made_before_any_ask_is_the_one_asks_get`.
- `VICTORY_COPY_UNLOCKED` (2026-11-27, measured not better, off): copies made
  without the buffer table's lock. Test: `a_copy_made_aside_is_one_for_all_who_ask_at_once`.
- `made_line` (diagnostics): the copies made each second, by how long before the
  title had declared their buffer.
- `changed_runs`, `note_stale` (diagnostics).

`list.rs`
- `VICTORY_LIST_RESERVE=<n>` (2026-11-27, measured not better, off): a
  `victory-lists` thread keeps n command lists ready.

`memory.rs`
- `VICTORY_FAULTS_ANY` (2026-11-27, measured not better, off): pages ray tracing
  touches are brought in past the bound of 2,048 for a session.

`timeline\record.rs`, `timeline.rs`, `pipeline.rs` (diagnostics)
- `VICTORY_TIMELINE_BINDS=<n>`: a timeline recording starts by itself at the n-th
  pipeline a frame binds for the first time (the first frame of a new scene);
  buffers doubled so that a 5.7 s frame fits. Test:
  `a_recording_begins_at_the_asked_first_bind_of_a_frame_alone`.

`popwatch.rs`, `ab.rs` (diagnostics)
- The flicker measurement splits its counts by every switch of `VICTORY_AB`
  (`ab::bits`, `ab::switches`).

Comments that named the title were changed to "the title".

### Settings (`tools\settings.ps1`)

Turned on or tuned for Victory Grimkaaah, with what they measured: renderer patches
1, 5, 6 and 7 (39-44 ms a frame instead of 49-56), `UP_QUEUE` (-3.5 ms),
`SUBMIT_FRAMES=2` and `SUBMIT_DEPTH=128` (the title waits 0 ms at present instead of
11-17), `PLACED_ORDER` (with FILL_FORWARD), `MAP_THREADS=1` (8 threads were slower),
`BVH_BUILDER` in Quality mode (ray tracing).

`VICTORY_PORT_CONSTANTS_ORDER` (2026-11-27, `source\crates\victory-gpu\src\draw.rs` and
`descriptor.rs`): indirect draws whose arguments the card reads in place are prepared
one at a time, right before they run. Taken from avatarmanz's commits 631400f,
3e04ca0 and 15c3d0e (GPL-3.0-or-later), alone. Checked in a Quality session: trees
no longer drop out and the characters' eyes are correct; 12.7 fps and a median of
72 ms a frame in the world, 0 errors.

### Tried and removed

- A merge of newer outside changes (2026-11-27): the characters' eyes turned white
  and the trees still flickered; it was taken out the same day.

## Tests

`cargo test --profile diag -p victory-gpu --features diagnostics --lib`: 442 passed,
0 failed, 34 ignored on 2026-11-27. The timing test
`real_waits_are_recorded_with_who_ended_them` sometimes fails, with or without these
changes.
