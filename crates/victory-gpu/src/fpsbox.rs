//! The frame rate and the machine's load, drawn into the top left corner of
//! every frame shown.
//!
//! The window's caption has the frame rate, but a capture of the window's
//! picture (a stream) does not carry the caption. Here the numbers are part
//! of the picture: a small box of text copied onto the swap chain's back
//! buffer right after the title's frame, before it is presented. The
//! title's own frame is not touched, so saved frames and everything read
//! back from it are as before.
//!
//! The box, new once a second:
//!
//! ```text
//! DARKWORLDHM                the name, on a bar
//! 26.1 FPS  38 MS            the frames shown a second and a frame's time
//! WORST FRAME 62 MS          the longest wait between two frames that second
//! CPU 46%  GAME 10.6 CORES   all processors, all programs; this process
//! TOP THREAD 100%            this process's busiest thread, of one processor
//! GPU 37%  GAME 18%          the card's busiest engine: all programs; this one
//! VRAM 1.7/11.0 GB           the card's memory this process uses / its budget
//! RAM 9.3 GB  FREE 10.2 GB   this process's committed memory; the PC's free
//! (a graph)                  the last frames' times, a bar each
//! ```
//!
//! A thread near 100 % beside a card far from it is a frame held by that
//! thread; a card near 100 % holds it itself; neither busy is waiting (for
//! memory to cross between the two, mostly). The load comes from
//! `crate::stats`, the card's memory from the host.
//!
//! Colours say what holds the frame without reading the figures. The frame
//! rate is green from 28.5 frames a second (the console's 30), yellow from
//! 20, red below. A line turns red at its limit: a thread at 95 % of a
//! processor, the card at 95 %, the card's memory at 95 % of its budget,
//! under 2 GB of the PC's memory free. In the graph the full height is two
//! frames of the console (66.7 ms) and the grey line one (33.3 ms): a bar
//! is green up to 34 ms, yellow up to 50, red above.
//!
//! The text is drawn on the CPU with a font of its own (5 by 7 dots a
//! glyph) into one of four buffers the card copies from, taken in turn, so
//! a present still in the air reads what it was recorded with. On by
//! default. `VICTORY_FPS_COUNTER=fps` shows the name and the frame rate
//! alone; `=more` adds the draws a frame, the submitting thread's work a
//! frame and the PC's memory shared with the card; `=0` leaves the frame
//! as the title drew it. F3 goes round the four while a window of ours
//! has the focus: the whole box, with more, the frame rate alone, nothing.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU16, AtomicU32, AtomicUsize, Ordering};

use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::*;

/// The name at the head of every box.
const MARK: &str = "DARKWORLDHM";

/// What the presenter's meter measured over a second.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Second {
    /// Frames shown a second.
    pub(crate) fps: f64,
    /// The longest time between two frames shown, in milliseconds.
    pub(crate) worst_ms: f64,
    /// Draws a frame.
    pub(crate) draws: f64,
    /// The submitting thread's work a frame, in milliseconds.
    pub(crate) preparing_ms: f64,
}

/// The last [`Second`] as the box shows it: tenths of a frame a second,
/// tenths of a millisecond, draws, tenths of a millisecond.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Shown {
    tenths: u32,
    worst: u32,
    draws: u32,
    preparing: u32,
}

/// The last second, in [`Shown`]'s order; the first is `u32::MAX` before
/// the first second.
static SECOND: [AtomicU32; 4] = [AtomicU32::new(u32::MAX), AtomicU32::new(0), AtomicU32::new(0), AtomicU32::new(0)];

/// The second just measured.
pub(crate) fn second(s: Second) {
    if !s.fps.is_finite() || s.fps < 0.0 {
        return;
    }
    let tenths = |v: f64| if v.is_finite() && v > 0.0 { (v * 10.0).round().min(99_999.0) as u32 } else { 0 };
    SECOND[1].store(tenths(s.worst_ms), Ordering::Relaxed);
    SECOND[2].store(if s.draws.is_finite() && s.draws > 0.0 { s.draws.round().min(999_999.0) as u32 } else { 0 }, Ordering::Relaxed);
    SECOND[3].store(tenths(s.preparing_ms), Ordering::Relaxed);
    SECOND[0].store(tenths(s.fps), Ordering::Relaxed);
}

/// The times of the last frames shown, in tenths of a millisecond, written
/// in turn by the presenting thread.
struct Times {
    ring: [AtomicU16; 128],
    /// The frames shown so far.
    shown: AtomicUsize,
}

impl Times {
    const fn new() -> Times {
        Times { ring: [const { AtomicU16::new(0) }; 128], shown: AtomicUsize::new(0) }
    }

    /// A frame was shown `interval` after the one before it.
    fn push(&self, interval: std::time::Duration) {
        let tenths = (interval.as_secs_f64() * 10_000.0).round().clamp(1.0, f64::from(u16::MAX)) as u16;
        let at = self.shown.fetch_add(1, Ordering::Relaxed);
        self.ring[at % self.ring.len()].store(tenths, Ordering::Relaxed);
    }

    /// The times of the last `count` frames, the oldest first; 0 where
    /// there was no frame yet.
    fn last(&self, count: usize) -> Vec<u16> {
        let count = count.min(self.ring.len());
        let end = self.shown.load(Ordering::Relaxed);
        (0..count)
            .map(|i| {
                let back = count - i;
                if end < back { 0 } else { self.ring[(end - back) % self.ring.len()].load(Ordering::Relaxed) }
            })
            .collect()
    }
}

static TIMES: Times = Times::new();

/// A frame was shown `interval` after the one before it.
pub(crate) fn frame(interval: std::time::Duration) {
    TIMES.push(interval);
}

/// What is drawn: nothing, the name and the frame rate, the whole box, or
/// the whole box with the layer's own figures.
#[derive(Clone, Copy, Debug, PartialEq)]
#[repr(u8)]
enum Mode {
    Off,
    Rate,
    All,
    More,
}

impl Mode {
    /// The mode F3 turns this one into: the whole box, the box with the
    /// layer's figures, the frame rate alone, nothing, and round again.
    fn next(self) -> Mode {
        match self {
            Mode::All => Mode::More,
            Mode::More => Mode::Rate,
            Mode::Rate => Mode::Off,
            Mode::Off => Mode::All,
        }
    }
}

/// The mode now, as a [`Mode`]'s number; `u8::MAX` before the first frame.
static MODE: AtomicU8 = AtomicU8::new(u8::MAX);

/// The mode now: what `VICTORY_FPS_COUNTER` asked for, until F3 changed it.
fn mode() -> Mode {
    match MODE.load(Ordering::Relaxed) {
        0 => Mode::Off,
        1 => Mode::Rate,
        2 => Mode::All,
        3 => Mode::More,
        _ => {
            let asked = match std::env::var("VICTORY_FPS_COUNTER").as_deref() {
                Ok("0") => Mode::Off,
                Ok("fps") => Mode::Rate,
                Ok("more") => Mode::More,
                _ => Mode::All,
            };
            MODE.store(asked as u8, Ordering::Relaxed);
            asked
        }
    }
}

/// F3 went down since the last frame while a window of this process was
/// in front (so the key means nothing here while another program has it).
fn key_pressed() -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_F3};
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};
    static DOWN: AtomicBool = AtomicBool::new(false);
    // SAFETY: a query of one key's state.
    let down = unsafe { GetAsyncKeyState(i32::from(VK_F3.0)) } < 0;
    if DOWN.swap(down, Ordering::Relaxed) || !down {
        return false;
    }
    let mut process = 0u32;
    // SAFETY: plain queries; with no window in front the process stays 0.
    unsafe { GetWindowThreadProcessId(GetForegroundWindow(), Some(&mut process)) };
    process == std::process::id()
}

/// A glyph's seven rows, five dots each (bit 4 is the left one).
fn glyph(c: char) -> [u8; 7] {
    match c {
        '0' => [0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110],
        '1' => [0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
        '2' => [0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111],
        '3' => [0b11110, 0b00001, 0b00001, 0b01110, 0b00001, 0b00001, 0b11110],
        '4' => [0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010],
        '5' => [0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110],
        '6' => [0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110],
        '7' => [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000],
        '8' => [0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110],
        '9' => [0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100],
        '.' => [0, 0, 0, 0, 0, 0b01100, 0b01100],
        '%' => [0b11001, 0b11010, 0b00010, 0b00100, 0b01000, 0b01011, 0b10011],
        '/' => [0b00001, 0b00001, 0b00010, 0b00100, 0b01000, 0b10000, 0b10000],
        'A' => [0b01110, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001],
        'B' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10001, 0b10001, 0b11110],
        'C' => [0b01110, 0b10001, 0b10000, 0b10000, 0b10000, 0b10001, 0b01110],
        'D' => [0b11110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b11110],
        'E' => [0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b11111],
        'F' => [0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b10000],
        'G' => [0b01110, 0b10001, 0b10000, 0b10111, 0b10001, 0b10001, 0b01111],
        'H' => [0b10001, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001],
        'I' => [0b01110, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
        'K' => [0b10001, 0b10010, 0b10100, 0b11000, 0b10100, 0b10010, 0b10001],
        'L' => [0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b11111],
        'M' => [0b10001, 0b11011, 0b10101, 0b10101, 0b10001, 0b10001, 0b10001],
        'N' => [0b10001, 0b11001, 0b10101, 0b10011, 0b10001, 0b10001, 0b10001],
        'O' => [0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110],
        'P' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10000, 0b10000, 0b10000],
        'R' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10100, 0b10010, 0b10001],
        'S' => [0b01111, 0b10000, 0b10000, 0b01110, 0b00001, 0b00001, 0b11110],
        'T' => [0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100],
        'U' => [0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110],
        'V' => [0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01010, 0b00100],
        'W' => [0b10001, 0b10001, 0b10001, 0b10101, 0b10101, 0b11011, 0b10001],
        _ => [0; 7],
    }
}

/// The colours of the box.
#[derive(Clone, Copy, Debug, PartialEq)]
#[repr(u8)]
enum Ink {
    /// The box behind the text: nearly black.
    Box,
    /// The text: a bright yellow.
    Text,
    /// A figure at its goal: green.
    Good,
    /// A figure at its limit: red.
    Bad,
    /// The name's bar: a light blue.
    Mark,
    /// The graph's line of one console frame: grey.
    Dim,
}

/// Every [`Ink`], in the order of their numbers.
const INKS: [Ink; 6] = [Ink::Box, Ink::Text, Ink::Good, Ink::Bad, Ink::Mark, Ink::Dim];

impl Ink {
    fn rgb(self) -> (f32, f32, f32) {
        match self {
            Ink::Box => (0.03, 0.03, 0.03),
            Ink::Text => (1.0, 0.92, 0.16),
            Ink::Good => (0.30, 0.95, 0.35),
            Ink::Bad => (1.0, 0.30, 0.25),
            Ink::Mark => (0.20, 0.80, 1.0),
            Ink::Dim => (0.35, 0.35, 0.35),
        }
    }
}

/// A line of the box. A `bar` line is painted in its colour from side to
/// side, with its text in the box's.
#[derive(Clone, Debug, PartialEq)]
struct Line {
    text: String,
    ink: Ink,
    bar: bool,
}

impl Line {
    fn new(text: String, ink: Ink) -> Line {
        Line { text, ink, bar: false }
    }
}

/// The text for a frame rate in tenths.
fn text(tenths: u32) -> String {
    format!("{}.{} FPS", tenths / 10, tenths % 10)
}

/// The glyphs a line of the whole box has room for.
const COLUMNS: usize = 26;

/// The glyphs of the name and the frame rate alone.
const RATE_COLUMNS: usize = 11;

/// The lines of the box for `mode` (see the module's notes), each exactly
/// as long as the mode's box is wide: for the last second, the machine's
/// load if a look was taken, and the card's memory in bytes (its budget
/// and use, the shared memory's budget and use) if the host answered. A
/// line whose figures are not there yet is blank.
fn lines(mode: Mode, shown: Shown, stats: Option<crate::stats::Stats>, card: Option<[u64; 4]>) -> Vec<Line> {
    let gb = |mb: f64| mb / 1024.0;
    let blank = || Line::new(String::new(), Ink::Text);
    let tenths = shown.tenths;
    let rate = if tenths >= 285 {
        Ink::Good
    } else if tenths >= 200 {
        Ink::Text
    } else {
        Ink::Bad
    };
    let mut out = vec![Line { text: MARK.to_owned(), ink: Ink::Mark, bar: true }];
    if mode == Mode::Rate {
        out.push(Line::new(text(tenths), rate));
    } else {
        out.push(Line::new(if tenths > 0 { format!("{}  {:.0} MS", text(tenths), 10_000.0 / f64::from(tenths)) } else { text(tenths) }, rate));
        let worst = f64::from(shown.worst) / 10.0;
        out.push(Line::new(
            format!("WORST FRAME {worst:.0} MS"),
            if worst <= 40.0 {
                Ink::Good
            } else if worst <= 66.7 {
                Ink::Text
            } else {
                Ink::Bad
            },
        ));
        let limit = |at: bool| if at { Ink::Bad } else { Ink::Text };
        match stats {
            Some(s) => {
                out.push(Line::new(format!("CPU {:.0}%  GAME {:.1} CORES", s.cpu_all, s.cores), Ink::Text));
                out.push(Line::new(format!("TOP THREAD {:.0}%", s.top_thread), limit(s.top_thread >= 95.0)));
                out.push(match (s.gpu_all, s.gpu_ours) {
                    (Some(all), Some(ours)) => Line::new(format!("GPU {all:.0}%  GAME {ours:.0}%"), limit(all >= 95.0)),
                    _ => Line::new("GPU NOT KNOWN".to_owned(), Ink::Text),
                });
            }
            None => out.extend([blank(), blank(), blank()]),
        }
        out.push(match card {
            Some([budget, used, ..]) => Line::new(
                format!("VRAM {:.1}/{:.1} GB", gb((used >> 20) as f64), gb((budget >> 20) as f64)),
                limit(used >= budget / 100 * 95),
            ),
            None => blank(),
        });
        out.push(match stats {
            Some(s) => Line::new(
                format!("RAM {:.1} GB  FREE {:.1} GB", gb(f64::from(s.ram_ours_mb)), gb(f64::from(s.ram_free_mb))),
                limit(s.ram_free_mb < 2048),
            ),
            None => blank(),
        });
        if mode == Mode::More {
            out.push(Line::new(format!("DRAWS {}  PREP {:.0} MS", shown.draws, f64::from(shown.preparing) / 10.0), Ink::Text));
            out.push(match card {
                Some([.., shared]) => Line::new(format!("SHARED {:.1} GB", gb((shared >> 20) as f64)), Ink::Text),
                None => blank(),
            });
        }
    }
    let columns = if mode == Mode::Rate { RATE_COLUMNS } else { COLUMNS };
    for line in &mut out {
        line.text = fit(&line.text, columns);
    }
    out
}

/// `line` in exactly `columns` glyphs: cut, or filled with spaces.
fn fit(line: &str, columns: usize) -> String {
    let mut out: String = line.chars().take(columns).collect();
    out.extend(std::iter::repeat_n(' ', columns - out.chars().count()));
    out
}

/// Bytes a pixel of a swap chain format this layer presents in, or none.
fn pixel_bytes(format: DXGI_FORMAT) -> Option<usize> {
    match format {
        DXGI_FORMAT_R8G8B8A8_UNORM | DXGI_FORMAT_B8G8R8A8_UNORM | DXGI_FORMAT_R10G10B10A2_UNORM => Some(4),
        DXGI_FORMAT_R16G16B16A16_FLOAT => Some(8),
        _ => None,
    }
}

/// One pixel of `format` in the colour `ink`.
fn pixel(format: DXGI_FORMAT, ink: Ink, out: &mut [u8]) {
    let (r, g, b) = ink.rgb();
    let byte = |v: f32| (v * 255.0).round() as u8;
    match format {
        DXGI_FORMAT_R8G8B8A8_UNORM => out.copy_from_slice(&[byte(r), byte(g), byte(b), 255]),
        DXGI_FORMAT_B8G8R8A8_UNORM => out.copy_from_slice(&[byte(b), byte(g), byte(r), 255]),
        DXGI_FORMAT_R10G10B10A2_UNORM => {
            let ten = |v: f32| (v * 1023.0).round() as u32;
            out.copy_from_slice(&(ten(r) | ten(g) << 10 | ten(b) << 20 | 3 << 30).to_le_bytes());
        }
        _ => {
            for (i, v) in [r, g, b, 1.0].into_iter().enumerate() {
                out[i * 2..i * 2 + 2].copy_from_slice(&half(v).to_le_bytes());
            }
        }
    }
}

/// A value from 0 to 1 as a 16-bit float.
fn half(v: f32) -> u16 {
    let bits = v.clamp(0.0, 1.0).to_bits();
    if v <= 0.0 {
        return 0;
    }
    // Sign 0; the exponent moves from a bias of 127 to one of 15; the top
    // ten bits of the fraction stay. Values under 2^-14 (none of ours) are 0.
    let exponent = ((bits >> 23) & 0xFF) as i32 - 127 + 15;
    if exponent <= 0 {
        return 0;
    }
    ((exponent as u32) << 10 | (bits >> 13) & 0x3FF) as u16
}

/// Dots of box all round the text.
const PAD: u32 = 2;

/// Dots of the graph's height.
const GRAPH_DOTS: u32 = 16;

/// The frame time of a bar as high as the graph, in tenths of a
/// millisecond: two frames of the console.
const GRAPH_FULL: u32 = 667;

/// The bars the graph of a box `columns` glyphs wide has room for: two
/// dots a bar.
fn bars(columns: usize) -> usize {
    (columns * 6 - 1) / 2
}

/// The box in dots: its width and height and, row by row from the top,
/// each dot's [`Ink`]. A glyph takes six dots across with its gap and a
/// line nine down with its gap; [`PAD`] dots of box lie all round. With
/// `graph` (frame times in tenths of a millisecond, the oldest first, 0
/// for none) a graph of [`GRAPH_DOTS`] dots lies under the text.
fn bitmap(lines: &[Line], graph: Option<&[u16]>) -> (u32, u32, Vec<u8>) {
    let columns = lines.iter().map(|l| l.text.chars().count()).max().unwrap_or(0).max(1) as u32;
    let rows = lines.len().max(1) as u32;
    let width = 2 * PAD + columns * 6 - 1;
    let height = 2 * PAD + rows * 9 - 2 + if graph.is_some() { 2 + GRAPH_DOTS } else { 0 };
    let mut dots = vec![Ink::Box as u8; (width * height) as usize];
    let mut put = |x: u32, y: u32, ink: Ink| dots[(y * width + x) as usize] = ink as u8;
    for (i, line) in lines.iter().enumerate() {
        let top = PAD + i as u32 * 9;
        let ink = if line.bar {
            for y in top - 1..top + 8 {
                for x in 1..width - 1 {
                    put(x, y, line.ink);
                }
            }
            Ink::Box
        } else {
            line.ink
        };
        for (c, ch) in line.text.chars().enumerate() {
            for (row, bits) in glyph(ch).into_iter().enumerate() {
                for column in (0..5u32).filter(|column| bits >> (4 - column) & 1 != 0) {
                    put(PAD + c as u32 * 6 + column, top + row as u32, ink);
                }
            }
        }
    }
    if let Some(times) = graph {
        let top = PAD + rows * 9;
        // A frame of the console is a bar of half the height: the line
        // lies right above it, so a slower frame's bar crosses the line.
        for x in PAD..width - PAD {
            put(x, top + GRAPH_DOTS / 2 - 1, Ink::Dim);
        }
        for (i, &time) in times.iter().take(bars(columns as usize)).enumerate() {
            if time == 0 {
                continue;
            }
            let bar = (u32::from(time) * GRAPH_DOTS).div_ceil(GRAPH_FULL).clamp(1, GRAPH_DOTS);
            let ink = if time <= 340 {
                Ink::Good
            } else if time <= 500 {
                Ink::Text
            } else {
                Ink::Bad
            };
            for y in top + GRAPH_DOTS - bar..top + GRAPH_DOTS {
                put(PAD + 2 * i as u32, y, ink);
                put(PAD + 2 * i as u32 + 1, y, ink);
            }
        }
    }
    (width, height, dots)
}

/// A buffer the card copies a box from, with its footprint and where its
/// bytes are mapped.
struct Made {
    buffer: ID3D12Resource,
    data: *mut u8,
    width: u32,
    height: u32,
    pitch: u32,
}

// SAFETY: a free-threaded D3D12 object, its mapping (written under
// `PANEL`'s lock only) and plain numbers.
unsafe impl Send for Made {}

/// The buffers a box is drawn into, in turn.
const RING: usize = 4;

/// The box of one mode, as last drawn.
#[derive(Default)]
struct Panel {
    /// What the buffers were made for: the format, the scale and the mode.
    made_for: (i32, u32, u8),
    /// What `ring[at]` shows: the frame rate and the number of the look at
    /// the machine's load.
    shown: Option<(u32, u64)>,
    ring: Vec<Made>,
    at: usize,
}

/// A box for every format, scale and mode drawn so far. None is ever let
/// go: a present still in the air may read its buffers, and there are few
/// (a megabyte or two a buffer; a new one comes when the window changes
/// its size or F3 its mode).
static PANELS: Mutex<Vec<Panel>> = Mutex::new(Vec::new());

/// Copies the box onto `back`, a back buffer of `width` by `height` pixels
/// of `format` that `list` holds as a copy's destination. Nothing if the
/// box is off, no second was measured yet, the format is not one of ours
/// or the frame is too small for the box.
pub(crate) fn draw(host: &crate::host::Host, list: &ID3D12GraphicsCommandList, back: &ID3D12Resource, (width, height, format): (u32, u32, DXGI_FORMAT)) {
    let mut mode = mode();
    if key_pressed() {
        mode = mode.next();
        MODE.store(mode as u8, Ordering::Relaxed);
    }
    if mode == Mode::Off {
        return;
    }
    let [tenths, worst, draws, preparing] = [0, 1, 2, 3].map(|i| SECOND[i].load(Ordering::Relaxed));
    let Some(bytes) = pixel_bytes(format).filter(|_| tenths != u32::MAX) else {
        return;
    };
    // About a fortieth of the frame's height a glyph: 6 pixels a dot at
    // 2160 lines, 3 at 1080.
    let scale = (height / 360).clamp(2, 12);
    let whole = mode != Mode::Rate;
    let look = if whole {
        crate::stats::start();
        crate::stats::now()
    } else {
        None
    };
    let mut panels = PANELS.lock().unwrap_or_else(|e| e.into_inner());
    let made_for = (format.0, scale, mode as u8);
    let panel = match panels.iter().position(|p| p.made_for == made_for) {
        Some(at) => &mut panels[at],
        None => {
            panels.push(Panel { made_for, ..Default::default() });
            panels.last_mut().expect("just pushed")
        }
    };
    let key = (tenths, look.map_or(0, |(number, _)| number));
    if panel.shown != Some(key) {
        let card = if whole { host.video_memory() } else { None };
        let lines = lines(mode, Shown { tenths, worst, draws, preparing }, look.map(|(_, s)| s), card);
        let graph = whole.then(|| TIMES.last(bars(COLUMNS)));
        let (dots_wide, dots_high, dots) = bitmap(&lines, graph.as_deref());
        let (w, h) = (dots_wide * scale, dots_high * scale);
        let pitch = (w as usize * bytes).next_multiple_of(D3D12_TEXTURE_DATA_PITCH_ALIGNMENT as usize);
        // The next buffer in turn, made at its first use.
        let at = if panel.ring.len() < RING {
            let size = (pitch * h as usize) as u64;
            let Some((buffer, data)) = host.shared_buffer(size, crate::cardmem::Owner::PcOther)
            else {
                return;
            };
            panel.ring.push(Made { buffer, data, width: w, height: h, pitch: pitch as u32 });
            panel.ring.len() - 1
        } else {
            (panel.at + 1) % RING
        };
        let m = &panel.ring[at];
        if (m.width, m.height, m.pitch) != (w, h, pitch as u32) {
            // (Every box of one `made_for` has one size: not reached.)
            return;
        }
        // SAFETY: the buffer's own mapped bytes, `pitch * h` of them; the
        // card last read them at least `RING - 1` boxes ago.
        let out = unsafe { std::slice::from_raw_parts_mut(m.data, pitch * h as usize) };
        let mut colours = [[0u8; 8]; INKS.len()];
        for ink in INKS {
            pixel(format, ink, &mut colours[ink as usize][..bytes]);
        }
        // A row of dots is one row of pixels written `scale` times. The
        // row is put together here and not in the buffer: the buffer's
        // memory is for writing only.
        let scale = scale as usize;
        let mut row = vec![0u8; w as usize * bytes];
        for (y, dots) in dots.chunks_exact(dots_wide as usize).enumerate() {
            for (x, &ink) in dots.iter().enumerate() {
                for pixel in row[x * scale * bytes..(x + 1) * scale * bytes].chunks_exact_mut(bytes) {
                    pixel.copy_from_slice(&colours[ink as usize][..bytes]);
                }
            }
            for line in 0..scale {
                let to = (y * scale + line) * pitch;
                out[to..to + row.len()].copy_from_slice(&row);
            }
        }
        panel.at = at;
        panel.shown = Some(key);
    }
    let Some(m) = panel.ring.get(panel.at) else {
        return;
    };
    let margin = 4 * scale;
    if m.width + 2 * margin > width || m.height + 2 * margin > height {
        return;
    }
    // SAFETY: an open list; the back buffer is a copy's destination in it
    // and the box lies inside it; our buffer holds `pitch * height` bytes
    // and is kept for as long as the process runs.
    unsafe {
        let from = D3D12_TEXTURE_COPY_LOCATION {
            pResource: std::mem::transmute_copy(&m.buffer),
            Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
            Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                PlacedFootprint: D3D12_PLACED_SUBRESOURCE_FOOTPRINT {
                    Offset: 0,
                    Footprint: D3D12_SUBRESOURCE_FOOTPRINT { Format: format, Width: m.width, Height: m.height, Depth: 1, RowPitch: m.pitch },
                },
            },
        };
        let to = D3D12_TEXTURE_COPY_LOCATION {
            pResource: std::mem::transmute_copy(back),
            Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
            Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 { SubresourceIndex: 0 },
        };
        list.CopyTextureRegion(&to, margin, margin, 0, &from, None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stats() -> crate::stats::Stats {
        crate::stats::Stats {
            cpu_all: 100.0,
            cores: 28.0,
            top_thread: 100.0,
            gpu_all: Some(100.0),
            gpu_ours: Some(100.0),
            ram_ours_mb: 31 * 1024 + 512,
            ram_free_mb: 12 * 1024 + 100,
        }
    }

    #[test]
    fn the_text_is_the_rate_to_a_tenth() {
        assert_eq!(text(103), "10.3 FPS");
        assert_eq!(text(7), "0.7 FPS");
        assert_eq!(text(300), "30.0 FPS");
    }

    #[test]
    fn every_line_is_as_long_as_its_box_and_has_a_glyph_for_every_letter() {
        let longest = Shown { tenths: 99_999, worst: 99_999, draws: 999_999, preparing: 9_999 };
        let usual = Shown { tenths: 58, worst: 2_150, draws: 3_916, preparing: 313 };
        let card = Some([12_282 << 20, 11_900 << 20, 16_000 << 20, 10_444 << 20]);
        let texts = [
            (lines(Mode::All, longest, Some(stats()), card), 8, COLUMNS),
            (lines(Mode::More, usual, Some(crate::stats::Stats { gpu_all: None, gpu_ours: None, ..stats() }), card), 10, COLUMNS),
            (lines(Mode::All, Shown::default(), None, None), 8, COLUMNS),
            (lines(Mode::Rate, longest, None, None), 2, RATE_COLUMNS),
        ];
        let line = |box_: usize, at: usize| texts[box_].0[at].text.trim_end().to_owned();
        assert_eq!(line(0, 0), "DARKWORLDHM");
        assert_eq!(line(0, 1), "9999.9 FPS  0 MS");
        assert_eq!(line(0, 2), "WORST FRAME 10000 MS");
        assert_eq!(line(0, 3), "CPU 100%  GAME 28.0 CORES");
        assert_eq!(line(0, 6), "VRAM 11.6/12.0 GB");
        assert_eq!(line(0, 7), "RAM 31.5 GB  FREE 12.1 GB");
        assert_eq!(line(1, 1), "5.8 FPS  172 MS");
        assert_eq!(line(1, 2), "WORST FRAME 215 MS");
        assert_eq!(line(1, 4), "TOP THREAD 100%");
        assert_eq!(line(1, 5), "GPU NOT KNOWN");
        assert_eq!(line(1, 8), "DRAWS 3916  PREP 31 MS");
        assert_eq!(line(1, 9), "SHARED 10.2 GB");
        assert_eq!(line(2, 1), "0.0 FPS");
        assert_eq!(line(2, 3), "");
        assert_eq!(line(3, 0), "DARKWORLDHM");
        assert_eq!(line(3, 1), "9999.9 FPS");
        for (lines, count, columns) in &texts {
            assert_eq!(lines.len(), *count);
            assert!(lines[0].bar && lines[1..].iter().all(|l| !l.bar), "the name alone is on a bar");
            for line in lines {
                assert_eq!(line.text.chars().count(), *columns, "{line:?}");
                for c in line.text.chars().filter(|c| *c != ' ') {
                    assert!(glyph(c).iter().any(|&r| r != 0), "no glyph for {c:?} in {line:?}");
                }
            }
        }
    }

    #[test]
    fn a_line_at_its_limit_is_red_and_the_rate_at_its_goal_green() {
        let ink = |shown: Shown, stats, card, at: usize| lines(Mode::All, shown, Some(stats), card)[at].ink;
        let shown = |tenths, worst| Shown { tenths, worst, ..Default::default() };
        let calm = crate::stats::Stats { top_thread: 60.0, gpu_all: Some(70.0), ..stats() };
        let card = |used: u64| Some([10_000 << 20, used << 20, 0, 0]);
        // The frame rate and the worst frame.
        assert_eq!(ink(shown(300, 340), calm, None, 1), Ink::Good);
        assert_eq!(ink(shown(285, 400), calm, None, 2), Ink::Good);
        assert_eq!(ink(shown(245, 510), calm, None, 1), Ink::Text);
        assert_eq!(ink(shown(245, 510), calm, None, 2), Ink::Text);
        assert_eq!(ink(shown(199, 870), calm, None, 1), Ink::Bad);
        assert_eq!(ink(shown(199, 870), calm, None, 2), Ink::Bad);
        // The busiest thread, the card, its memory, the PC's memory.
        assert_eq!([4, 5, 6, 7].map(|at| ink(shown(300, 340), calm, card(9_000), at)), [Ink::Text; 4]);
        assert_eq!([4, 5].map(|at| ink(shown(300, 340), stats(), card(9_000), at)), [Ink::Bad; 2]);
        assert_eq!(ink(shown(300, 340), calm, card(9_500), 6), Ink::Bad);
        assert_eq!(ink(shown(300, 340), crate::stats::Stats { ram_free_mb: 2_047, ..calm }, None, 7), Ink::Bad);
    }

    #[test]
    fn the_box_holds_every_glyph_with_a_pad_and_the_name_on_its_bar() {
        let lines = [Line { text: fit(MARK, 11), ink: Ink::Mark, bar: true }, Line::new(fit("10.3 FPS", 11), Ink::Good)];
        let (w, h, dots) = bitmap(&lines, None);
        // Eleven glyphs of five dots and ten gaps of one; two lines of
        // seven dots and a gap of two; the pad all round.
        assert_eq!((w, h), (11 * 5 + 10 + 4, 2 * 7 + 2 + 4));
        assert_eq!(dots.len(), (w * h) as usize);
        let at = |x: u32, y: u32| dots[(y * w + x) as usize];
        // The outermost dots are box all round, the bar's too.
        assert!((0..w).all(|x| at(x, 0) == Ink::Box as u8 && at(x, h - 1) == Ink::Box as u8));
        assert!((0..h).all(|y| at(0, y) == Ink::Box as u8 && at(w - 1, y) == Ink::Box as u8));
        // The bar runs from side to side a dot above and below its glyphs,
        // and the name is cut out of it: the "D" has its stem in the box's
        // colour and its inside in the bar's.
        assert!((1..10).all(|y| at(1, y) == Ink::Mark as u8 && at(w - 2, y) == Ink::Mark as u8));
        assert_eq!((at(2, 2), at(4, 5)), (Ink::Box as u8, Ink::Mark as u8));
        // The second line, in its own colour: the "1" has dot column 2 of
        // its first row lit and column 0 not; a space lights nothing.
        assert_eq!((at(2 + 2, 2 + 9), at(2, 2 + 9)), (Ink::Good as u8, Ink::Box as u8));
        assert!((0..7).all(|y| (0..5).all(|x| at(2 + 4 * 6 + x, 2 + 9 + y) == Ink::Box as u8)));
    }

    #[test]
    fn the_graph_has_a_bar_for_every_frame_by_its_time() {
        let lines = [Line::new(fit("", COLUMNS), Ink::Text)];
        // No frame; a console frame; one just over it; a slow one; one
        // past the graph's height.
        let (w, h, dots) = bitmap(&lines, Some(&[0, 333, 420, 520, 5_000]));
        assert_eq!((w, h), (COLUMNS as u32 * 6 - 1 + 4, 7 + 4 + 2 + GRAPH_DOTS));
        assert_eq!(bars(COLUMNS), 77);
        let top = 2 + 9;
        let bar = |i: u32| {
            let column: Vec<u8> = (top..top + GRAPH_DOTS).map(|y| dots[(y * w + 2 + 2 * i) as usize]).collect();
            assert_eq!(column, (top..top + GRAPH_DOTS).map(|y| dots[(y * w + 3 + 2 * i) as usize]).collect::<Vec<u8>>(), "two dots a bar");
            column
        };
        let high = |column: &[u8], ink: Ink| column.iter().rev().take_while(|&&d| d == ink as u8).count() as u32;
        // The line of one console frame, where no bar covers it.
        assert_eq!(bar(0).iter().map(|&d| d == Ink::Dim as u8).collect::<Vec<_>>(), (0..GRAPH_DOTS).map(|y| y == 7).collect::<Vec<_>>());
        // A console frame stays right under the line.
        assert_eq!((high(&bar(1), Ink::Good), bar(1)[7]), (8, Ink::Dim as u8));
        // A slower frame crosses it.
        assert_eq!(high(&bar(2), Ink::Text), 11);
        assert_eq!(high(&bar(3), Ink::Bad), 13);
        assert_eq!(high(&bar(4), Ink::Bad), GRAPH_DOTS);
        // Nothing lies under the pad.
        assert!((0..w).all(|x| (top + GRAPH_DOTS..h).all(|y| dots[(y * w + x) as usize] == Ink::Box as u8)));
    }

    #[test]
    fn the_graph_gets_the_last_frames_the_oldest_first() {
        let times = Times::new();
        for ms in [20u64, 33, 50] {
            times.push(std::time::Duration::from_millis(ms));
        }
        assert_eq!(times.last(3), [200, 330, 500]);
        assert_eq!(times.last(5), [0, 0, 200, 330, 500]);
        for _ in 0..times.ring.len() {
            times.push(std::time::Duration::from_secs(60));
        }
        assert_eq!(times.last(2), [u16::MAX; 2], "a wait too long for the graph is its longest");
        assert_eq!(times.last(1_000).len(), times.ring.len());
    }

    #[test]
    fn the_key_goes_round_every_mode() {
        let mut mode = Mode::All;
        let round: Vec<Mode> = (0..4)
            .map(|_| {
                mode = mode.next();
                mode
            })
            .collect();
        assert_eq!(round, [Mode::More, Mode::Rate, Mode::Off, Mode::All]);
    }

    #[test]
    fn pixels_are_in_the_formats_the_layer_presents_in() {
        let mut four = [0u8; 4];
        pixel(DXGI_FORMAT_R8G8B8A8_UNORM, Ink::Text, &mut four);
        assert_eq!(four, [255, 235, 41, 255]);
        pixel(DXGI_FORMAT_B8G8R8A8_UNORM, Ink::Text, &mut four);
        assert_eq!(four, [41, 235, 255, 255]);
        pixel(DXGI_FORMAT_R10G10B10A2_UNORM, Ink::Text, &mut four);
        let v = u32::from_le_bytes(four);
        assert_eq!((v & 1023, v >> 10 & 1023, v >> 20 & 1023, v >> 30), (1023, 941, 164, 3));
        let mut eight = [0u8; 8];
        pixel(DXGI_FORMAT_R16G16B16A16_FLOAT, Ink::Text, &mut eight);
        assert_eq!(u16::from_le_bytes([eight[0], eight[1]]), 0x3C00, "1.0 as a half");
        assert_eq!(u16::from_le_bytes([eight[6], eight[7]]), 0x3C00);
        assert_eq!(half(0.5), 0x3800);
        assert_eq!(half(0.0), 0);
        assert_eq!(pixel_bytes(DXGI_FORMAT_R16G16B16A16_FLOAT), Some(8));
        assert_eq!(pixel_bytes(DXGI_FORMAT_D32_FLOAT), None);
        // Every colour has its number in the table the box is drawn from.
        assert!(INKS.iter().enumerate().all(|(i, &ink)| ink as usize == i));
    }
}
