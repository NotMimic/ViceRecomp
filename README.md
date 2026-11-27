# ViceRecomp

**ViceRecomp** is a new type of the Victory compatibility layer: it runs an
Xbox Series X title natively on Windows, on an AMD Radeon RX 7900 XTX, from files
you supply yourself. It comes with its own launcher, its own tuned settings and my
own changes to the code, each one measured on my PC before being kept.

- **Version:** ViceRecomp 1.0 (2026-11-27)
- **Graphics card:** **AMD Radeon only** (made and tested on an RX 7900 XTX).
  This is **not** an NVIDIA or Intel build: every setting and every measurement
  here is for AMD, and nothing was tested on other cards.
- **Console profile:** Xbox Series X
- **Picture:** 4K output (3840x2160; the title draws at 2560x1440 and upscales),
  shown in a window, ray tracing on in Quality mode
- **Author:** Grimkaaah

> This repository contains no game files, code or assets of any title. You need
> your own copy of the title. It is not affiliated with or endorsed by the
> title's developer or publisher.

## my PC

| | |
|---|---|
| OS | Windows 11 Pro 64-bit |
| CPU | AMD Ryzen 9 7950X3D (16 cores, 32 threads) |
| GPU | AMD Radeon RX 7900 XTX, 24 GB |
| RAM | 32 GB, with a fixed 96 GB page file (98,304 MB) |
| Screen | 1920x1080, 180 Hz, FreeSync |

## Performance on my PC

**Ray tracing runs on AMD, with no crash.** Measured with the launcher's
end-of-session report, sessions of 3 to 9 minutes, 2026-11-26 and 2026-11-27.

| | Quality (ray tracing on) | Fluidity (ray tracing off) |
|---|---|---|
| Lighter scenes (1,000 to 1,600 draws a frame, cinematics) | **20 to 27 fps** (38 to 48 ms a frame) | 27 fps on average, up to 30 (the title's own cap) |
| Busy scenes in the world (3,000 to 5,500 draws a frame) | **10 to 16 fps**, 13.9 fps on average | **24.3 fps** on average |
| Frame time in the world | median 79 ms (best 5 s sample: 45 ms) | median 40 ms (best 5 s sample: 34 ms) |
| Regularity | median 38 ms between frames, 90 % under 59 ms | median 38 ms, 90 % under 54 ms |
| First frame of the world | 3.8 s freeze (5.2 s before my COPY_AHEAD change) | 4.2 s freeze |

| Both modes | |
|---|---|
| Crashes | **none**: 0 errors and 0 card losses over my last 10 sessions |
| Shaders | 4,270 from the cache, **0** translated while playing |
| Pipelines | about 2,160 made per session, **0** refused by the driver |
| Card memory | 15.5 to 21 GB used of 24 GB |
| PC memory shared with the card | 10.9 to 13.3 GB |
| Victory's process memory | 13 GB at the loading screen, 30 to 44 GB once in the world |
| Tearing | none (VSync on, FreeSync follows) |

## What I changed

Everything below was done for ViceRecomp. Each code change is behind its own `VICTORY_` switch,
was measured on my PC, and is only turned on in `tools\settings.ps1` when it
measured better. The full list, file by file, is in [CHANGES.md](CHANGES.md).

**The launcher, written from scratch**

- `ViceRecomp.bat`: a menu with three modes (Quality, Fluidity, Diagnostic),
  checks before starting (not administrator, files present, memory and page file,
  preparation up to date), one log per session in `logs\`, the frame rate on screen
  every 10 s, and a report at the end (frame rate, regularity, hitches, shaders and
  pipelines, errors, memory, most frequent warnings).
- `tools\settings.ps1`: **one** table with every setting, what it does and what it
  measured.
- `PREPARE.bat`: translates the shaders and makes the pipelines, **Quick** (the
  pipelines your sessions used, a few minutes) or **Full** (all of the title's
  pipelines, about 84,000, so that no area ever compiles one while you play).
- `BUILD.bat`: builds both versions (play and diagnostic) into `bin\`.

**Code changes, with what they measured**

| Switch | What it does | Measured on my PC |
|---|---|---|
| `VICTORY_COPY_AHEAD` | A title buffer's copy is made as soon as the title declares the buffer, while loading, instead of in the first frame that uses it | Freeze at the start of the world 5.2 s -> 3.8 s; 12.5 -> 13.9 fps (Quality) |
| `VICTORY_FILL_FORWARD` | A fill behind a title copy reads the copy's bytes instead of waiting for the card | With ray tracing: card wait 19-23 ms -> 0 ms; mean frame 62.7 -> 58.6 ms |
| `VICTORY_REAPER` | Command lists back from the card are cleaned up on a thread of their own | -2.3 to -3.8 ms a frame |
| `VICTORY_TEXBACK_FREE` | Textures read back to the title no longer take the global remapping lock | Removes a 173 ms wait seen in a timeline |
| `VICTORY_VSYNC` | Each frame waits for the screen's refresh | No more tearing |

**Settings I tuned**

| Setting | Measured on my PC |
|---|---|
| Two frames in flight (`SUBMIT_FRAMES=2`, `SUBMIT_DEPTH=128`) | The title waits 0 ms at present instead of 11-17 ms |
| Copy queue for uploads (`UP_QUEUE=1`) | -3.5 ms a frame |
| Renderer patches 1, 5, 6 and 7 on (`PORT_CONSTANTS`, `PORT_CBV`, `PORT_GUARD`, `PORT_JOURNAL`) | 39-44 ms a frame instead of 49-56 |
| Fills in order on the card (`PLACED_ORDER=1`) | Together with FILL_FORWARD |
| Memory remapping on one thread (`MAP_THREADS=1`) | 8 threads measured slower (45.2 against 43.6 ms) |
| Indirect draws read in place prepared one at a time (`PORT_CONSTANTS_ORDER=1`) | Trees no longer drop out, eyes correct; 12.7 fps, median 72 ms a frame in the world (2026-11-27) |

**Diagnostic tools** (in the diagnostic build)

- A line every second with the time between frames (median, 90th percentile,
  frames over 50 and 100 ms).
- A timeline recording that starts by itself at the first frame of a new scene
  (`VICTORY_TIMELINE_BINDS=<n>`), to see what a freeze is made of.
- A line every second with the buffer copies made, and how long before the title
  had declared them.
- The flicker measurement split by every switch that changes during a session.

**Tried, measured, and left off** (not better): making buffer copies outside the
buffer table's lock (`VICTORY_COPY_UNLOCKED`), a reserve of ready command lists
(`VICTORY_LIST_RESERVE`), and lifting the bound on pages ray tracing may bring in
(`VICTORY_FAULTS_ANY`).

## How to use it

**You need:** Windows 10 or 11 64-bit; a CPU with AVX2; an **AMD Radeon** card
with Direct3D 12, shader model 6.6, mesh shaders and ray tracing (made and tested
on an RX 7900 XTX 24 GB; NVIDIA and Intel cards are not supported, the launcher
warns if it finds no AMD card); 32 GB of RAM with a 96 GB page file (64 GB of
RAM is better); Rust (rustup, the toolchain of `source\rust-toolchain.toml`) and
the Visual Studio C++ build tools to build; Python 3 for the session report; your
own copy of the title.

1. Set the page file: System > Advanced system settings > Performance > Advanced >
   Virtual memory, custom size 98304 MB (initial and maximum), then restart.
2. Copy `victory.example.toml` to `victory.toml` and set `game_dir` to the folder
   of your copy of the title. If its executable or shader archive have other names,
   set them at the top of `tools\settings.ps1`.
3. Run `BUILD.bat` (first time and after any code change).
4. Run `PREPARE.bat` and pick Quick (or Full when you have time).
5. Run `ViceRecomp.bat` from a normal window (not as administrator) and pick
   a mode:
   - **1 Quality**: ray tracing on, 10 to 27 fps depending on the scene.
   - **2 Fluidity**: ray tracing off, about 24 fps.
   - **3 Diagnostic**: as Quality, with a detailed log, a little slower.

Close the title normally: the report is printed and saved in `logs\`.

## The folder

| | |
|---|---|
| `ViceRecomp.bat` | The launcher's menu |
| `PREPARE.bat`, `BUILD.bat` | Preparation and build |
| `tools\` | The launcher (`play.ps1`), the settings table (`settings.ps1`), `prepare.ps1`, `build.ps1`, the report (`report.py`) |
| `source\` | The source code |
| `bin\` | The built `victory.exe` (play and diagnostic), made by BUILD.bat |
| `logs\` | One log and one report per session |
| `victory.example.toml` | The configuration to copy to `victory.toml` |
| `CHANGES.md` | Every change of ViceRecomp, file by file |

## Known issues

- Trees: fixed on 2026-11-27 (`PORT_CONSTANTS_ORDER=1`); some far foliage may
  still flicker in places.
- Hitches of 0.3 to 0.6 s at camera cuts in cinematics, and a freeze of about 4 s
  at the first frame of the world.
- Memory: Victory uses 30 to 44 GB once in the world. With 32 GB of RAM the page
  file is required, and Windows pages memory out.
- With ray tracing on, busy scenes in the world stay between 10 and 16 fps.

## Contributing

Issues and pull requests are welcome if they follow [CONTRIBUTING.md](CONTRIBUTING.md):
AMD Radeon only, one change per pull request, behind its own `VICTORY_` switch,
measured with the launcher's session report, and never any game file.

## License

ViceRecomp is a modified version of the open-source Victory compatibility
layer and is distributed under the GNU General Public License v3.0 or later: see
[source/LICENSE](source/LICENSE). My changes are listed, with their dates, in
[CHANGES.md](CHANGES.md).
