//! Which title memory the title wrote since a host copy took it (decision
//! 0007, "Copies of title memory"). The runtime reserves title GPU memory
//! with write watching (`victory-hle` xmem), so the kernel keeps a dirty bit
//! per 4 KiB page; asking for a range's written pages also clears them. Host
//! copies of the same memory can overlap (nested buffers, textures in
//! buffers), so what one asks is kept here for the others: a journal of
//! each page's last write, numbered by the asking.
//!
//! Physical-page views (xmem's sections) cannot be write-watched. There a
//! copy leaves each 64 KiB block it took read-only, and our vectored handler
//! takes the first write to it: it notes the block as written and gives the
//! write access back. The runtime tells us when it maps or unmaps views
//! ([`title_memory_remapped`]), which resets their protection. Blocks found
//! wholly uncommitted are skipped while they remain so; one query can
//! confirm a whole region, including ordinary reservations between views.
//!
//! Writes into physical pages through another view are seen too: the
//! runtime tells us the views of the pages it wrote itself ([`writing`],
//! xmem's `write_physical` and DirectStorage's page destinations), and a
//! write by the title through any view of a guarded page faults like one
//! at the guarded address (the guard makes every view of the page
//! read-only, [`open`] gives them all back; not a page of more than
//! [`MANY_VIEWS`] views). A
//! system call writing into a guarded block fails (none is known to).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

/// The unit tracked and copied: 64 KiB, the console's page (the kernel's
/// 4 KiB pages fold into it). Finer units made the checks of the world's
/// big buffers cost more than the copies they saved.
pub(crate) const PAGE: usize = 64 << 10;
/// The kernel's page, the unit of its answers.
const KERNEL_PAGE: usize = 4096;

/// A title-memory range whose changed pages another subsystem needs to read.
pub(crate) struct Tracked {
    start: usize,
    end: usize,
    seen: Mutex<BTreeMap<usize, u64>>,
}

/// Tracks a runtime GPU-memory range, including physical-page views when
/// `views` is true. Other process memory is not tracked.
pub(crate) fn track(start: usize, len: usize, views: bool) -> Option<Tracked> {
    let end = start.checked_add(len)?;
    if len == 0 || writes(start, end, views).is_none() {
        return None;
    }
    Some(Tracked {
        start,
        end,
        seen: Mutex::default(),
    })
}

struct Journal {
    /// Numbers the askings; 0 is "never".
    asked: u64,
    /// By page number: the asking that last found it written.
    written: Stamps,
    /// Room for the kernel's answer.
    found: Vec<usize>,
    /// Page ranges asked ahead for a submission's refreshes ([`ask_ahead`])
    /// and the asking that did: `writes` answers for a range inside one
    /// from the journal, without another kernel call.
    ahead: Vec<(usize, usize)>,
    ahead_asked: u64,
    /// The ahead asking stands for a whole submission ([`ask_once`]): no
    /// other ahead asking is made, and `ahead` stays, until
    /// [`ask_once_end`].
    kept: bool,
    /// Room for [`ask_once`]'s ranges that are not view memory.
    alone: Vec<(usize, usize)>,
    /// With [`lean`]: rooms for the guards' answer to an asking (the pages
    /// written) and for their walk's list (the blocks not clean), kept
    /// between askings.
    pages: Vec<usize>,
    blocks: Vec<usize>,
    /// By page number: what our own writes stored since the page was last
    /// found written otherwise ([`wrote_own`]).
    own: BTreeMap<usize, OwnMark>,
}

static JOURNAL: Mutex<Journal> = Mutex::new(Journal {
    asked: 0,
    written: Stamps::new(),
    found: Vec::new(),
    ahead: Vec::new(),
    ahead_asked: 0,
    kept: false,
    alone: Vec::new(),
    pages: Vec::new(),
    blocks: Vec::new(),
    own: BTreeMap::new(),
});

/// Pages to a chunk of [`Stamps`]: 4 MiB of title addresses.
const CHUNK: usize = 64;

/// The journal's stamps: for each page, the asking that last found it
/// written, 0 for never. Kept by chunks of [`CHUNK`] pages, so that a
/// refresh reads its range's stamps a chunk at a time, one copy each. With
/// a tree entry per page ever written, the submitting thread copied every
/// entry of each range it refreshed into a list, some 22,500 times a frame
/// in a parked scene: the tree's walk and the copy were 5 % of its time.
struct Stamps(BTreeMap<usize, Box<[u64; CHUNK]>>);

impl Stamps {
    const fn new() -> Self {
        Self(BTreeMap::new())
    }

    fn chunk(&mut self, chunk: usize) -> &mut [u64; CHUNK] {
        self.0.entry(chunk).or_insert_with(|| Box::new([0; CHUNK]))
    }

    /// Notes `page` written at the asking `now`; its stamp before (0:
    /// never written).
    fn note(&mut self, page: usize, now: u64) -> u64 {
        std::mem::replace(&mut self.chunk(page / CHUNK)[page % CHUNK], now)
    }

    /// Notes `pages` written at the asking `now`. A chunk is looked up once
    /// for each run of pages in it (the kernel's answers come in order,
    /// sixteen of its pages to one of ours).
    fn note_all(&mut self, pages: impl IntoIterator<Item = usize>, now: u64) {
        let mut pages = pages.into_iter().peekable();
        while let Some(page) = pages.next() {
            let chunk = page / CHUNK;
            let stamps = self.chunk(chunk);
            stamps[page % CHUNK] = now;
            while let Some(page) = pages.next_if(|&p| p / CHUNK == chunk) {
                stamps[page % CHUNK] = now;
            }
        }
    }

    /// The stamps of pages `[first, last)` into `out` (emptied first), one
    /// per page in order.
    fn read_into(&self, first: usize, last: usize, out: &mut Vec<u64>) {
        out.clear();
        if first >= last {
            return;
        }
        out.resize(last - first, 0);
        // A range in one chunk (most refreshes) is looked up, not searched
        // for as a range ([`lean`]).
        if lean() && first / CHUNK == (last - 1) / CHUNK {
            let at = first / CHUNK * CHUNK;
            if let Some(stamps) = self.0.get(&(first / CHUNK)) {
                out.copy_from_slice(&stamps[first - at..last - at]);
            }
            return;
        }
        for (&chunk, stamps) in self.0.range(first / CHUNK..=(last - 1) / CHUNK) {
            let at = chunk * CHUNK;
            let (lo, hi) = (at.max(first), (at + CHUNK).min(last));
            out[lo - first..hi - first].copy_from_slice(&stamps[lo - at..hi - at]);
        }
    }
}

/// Ranges of one submission closer than this are asked about together:
/// walking the pages between them costs less than another kernel call (a
/// call that resets written pages costs 30-70 microseconds whatever the
/// range).
const AHEAD_GAP: usize = (8 << 20) / PAGE;
/// The most one asking covers (its answer needs 8 bytes per 4 KiB page).
const AHEAD_MAX: usize = (1 << 30) / PAGE;

/// Asks the kernel about many ranges `(start, end)` at once, before the
/// [`writes`] of each: ranges close together are one asking, and `writes`
/// then answers from the journal until [`ask_ahead_done`]. The world's
/// submissions refresh thousands of host copies (6,664 askings a frame,
/// 456 ms, in run 14). Ranges that are not write-watched, or lie alone, are
/// left to `writes`.
pub(crate) fn ask_ahead(ranges: &[(usize, usize)]) {
    let mut pieces: Vec<(usize, usize)> = ranges
        .iter()
        .filter(|r| r.0 < r.1)
        .map(|&(start, end)| (start / PAGE, end.div_ceil(PAGE)))
        .collect();
    if pieces.len() < 2 {
        return;
    }
    pieces.sort_unstable();
    let mut j = JOURNAL.lock().unwrap_or_else(|e| e.into_inner());
    let j = &mut *j;
    // The submission's one asking stands ([`ask_once`]): what it does not
    // hold is left to `writes`.
    if j.kept {
        return;
    }
    // View memory the guards hold is left to `writes`, which answers it
    // from the guards ([`known_view`]): the kernel's write watch can only
    // fail there. Asked here with the others, a run holding such a range
    // failed, was asked again by halves down to single ranges, failing
    // each time, and the range then went to `writes` all the same (after
    // triippywhale d0581a55). `VICTORY_VIEW_ASK=1` asks as before.
    #[cfg(feature = "diagnostics")]
    let all = pieces.len();
    // The guards are taken once for the whole list, under the journal's
    // lock as in `writes_into`: asked per range, the question was 2.7 % of
    // the submitting thread's time in a parked scene.
    if let Some(g) = view_guards() {
        pieces.retain(|&(first, _)| !view_in(&g, first * PAGE / BLOCK));
    }
    #[cfg(feature = "diagnostics")]
    AHEAD_VIEWS.fetch_add((all - pieces.len()) as u64, std::sync::atomic::Ordering::Relaxed);
    j.asked += 1;
    j.ahead_asked = j.asked;
    j.ahead.clear();
    let mut i = 0;
    while i < pieces.len() {
        let first = pieces[i].0;
        let mut last = pieces[i].1;
        let mut k = i + 1;
        while k < pieces.len()
            && pieces[k].0 <= last + AHEAD_GAP
            && last.max(pieces[k].1) - first <= AHEAD_MAX
        {
            last = last.max(pieces[k].1);
            k += 1;
        }
        ask_run(j, &pieces[i..k]);
        i = k;
    }
}

/// Since the last frame line: the kernel askings that took a millisecond
/// or more, their microseconds, and the longest asking's. An asking walks
/// page tables, some ten microseconds for most ranges; in run 75 the 2,644
/// askings of a frame took 219 ms in the lines where the pass over the
/// heap's views ran long, 25-50 ms in the others. These numbers say
/// whether a few askings wait for something or all of them are slow.
#[cfg(feature = "diagnostics")]
static SLOW: [std::sync::atomic::AtomicU64; 3] = [const { std::sync::atomic::AtomicU64::new(0) }; 3];

#[cfg(feature = "diagnostics")]
fn slow(us: u64) {
    use std::sync::atomic::Ordering::Relaxed;
    if us >= 1000 {
        SLOW[0].fetch_add(1, Relaxed);
        SLOW[1].fetch_add(us, Relaxed);
    }
    SLOW[2].fetch_max(us, Relaxed);
}

/// Since the last frame line: ranges of view memory the guards hold that a
/// submission's askings ahead ([`ask_ahead`], [`ask_once`]) left out. Each
/// was a kernel asking that failed, or several, before.
#[cfg(feature = "diagnostics")]
pub(crate) static AHEAD_VIEWS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// [`SLOW`], taken.
#[cfg(feature = "diagnostics")]
pub(crate) fn slow_asks() -> [u64; 3] {
    [0, 1, 2].map(|i| SLOW[i].swap(0, std::sync::atomic::Ordering::Relaxed))
}

/// The submission's refreshes are done: `writes` asks the kernel again.
pub(crate) fn ask_ahead_done() {
    let mut j = JOURNAL.lock().unwrap_or_else(|e| e.into_inner());
    if !j.kept {
        j.ahead.clear();
    }
}

/// Whether a submission asks the kernel once, before its first refresh,
/// about every range its lists refresh (`VICTORY_ASK_ONCE=1`; off by
/// default).
///
/// A submission's refreshes are done in groups, between the title's own
/// buffer copies (each copy flushes the refreshes recorded before it), and
/// every group asked the kernel again: 2,644 askings a frame, 70 a
/// submission. With the switch the journal answers all of them from one
/// asking made when the submission starts.
///
/// What changes: memory the title's other threads write while the
/// submission is being prepared is seen by the next submission's asking,
/// not by this one's later groups. What this submission's lists read was
/// written before they were submitted, so before the asking. Our own
/// writes into title memory while preparing are told to the journal
/// ([`wrote`]) or end the asking ([`ask_once_end`]).
pub(crate) fn once() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    // On by default since run 81 (with card buffers: 375-450 ms a frame
    // against 573-647, the picture unchanged over four minutes);
    // `VICTORY_ASK_ONCE=0` asks per range, as before.
    *ON.get_or_init(|| std::env::var_os("VICTORY_ASK_ONCE").is_none_or(|v| v != "0"))
}

/// With [`once`]: asks about `ranges` (everything a submission's lists
/// refresh) as [`ask_ahead`] does, and lets that asking stand until
/// [`ask_once_end`].
pub(crate) fn ask_once(ranges: &[(usize, usize)]) {
    if !once() {
        return;
    }
    ask_once_end();
    ask_ahead(ranges);
    let mut j = JOURNAL.lock().unwrap_or_else(|e| e.into_inner());
    let j = &mut *j;
    if j.ahead.is_empty() {
        // `ask_ahead` asked nothing (fewer than two ranges).
        j.asked += 1;
        j.ahead_asked = j.asked;
    }
    // `ask_ahead` leaves a range that lies alone to `writes`, which asks
    // once per flush: with one asking for the submission that was every
    // such range asked again at each of its 70 flushes (run 76: 2,300
    // askings a frame with the switch on, as without it). They are asked
    // here, once; what is not write-watched stays with `writes`.
    //
    // View memory the guards hold is not asked about: the asking can only
    // fail there, one failed kernel call for each such copy at every
    // submission, and `writes` answers it from the guards (after
    // triippywhale cc368c7c). The guards are taken once for the list, and
    // let go before the kernel is asked.
    let mut alone = std::mem::take(&mut j.alone);
    alone.clear();
    if let Some(g) = view_guards() {
        alone.extend(
            ranges
                .iter()
                .filter(|&&(start, end)| start < end && !view_in(&g, start / BLOCK))
                .map(|&(start, end)| (start / PAGE, end.div_ceil(PAGE))),
        );
    } else {
        alone.extend(ranges.iter().filter(|r| r.0 < r.1).map(|&(start, end)| (start / PAGE, end.div_ceil(PAGE))));
    }
    #[cfg(feature = "diagnostics")]
    AHEAD_VIEWS.fetch_add(
        (ranges.iter().filter(|r| r.0 < r.1).count() - alone.len()) as u64,
        std::sync::atomic::Ordering::Relaxed,
    );
    for &(first, last) in &alone {
        if !j.ahead.iter().any(|&(a, b)| a <= first && last <= b) && ask_kernel(j, first, last) {
            j.ahead.push((first, last));
        }
    }
    j.alone = alone;
    j.kept = !j.ahead.is_empty();
}

/// The submission's one asking stands no longer: its refreshes are done,
/// or title memory was written in a way the journal was not told.
pub(crate) fn ask_once_end() {
    let mut j = JOURNAL.lock().unwrap_or_else(|e| e.into_inner());
    if j.kept {
        j.kept = false;
        j.ahead.clear();
    }
}

/// We wrote title memory `[at, at + len)` while a submission's one asking
/// stands (the title's buffer copies, done at submission): its pages count
/// as written after that asking, so the refreshes that follow copy them.
pub(crate) fn wrote(at: usize, len: usize) {
    let mut j = JOURNAL.lock().unwrap_or_else(|e| e.into_inner());
    if !j.kept || len == 0 {
        return;
    }
    j.asked += 1;
    let now = j.asked;
    // The asking that stands moves with it: `asked` and `writes` give it as
    // "now", and a page noted written after "now" stays stale for every
    // copy that takes it (copied at `now`, written at `now + 1`): copied
    // again at every refresh, and a reason to settle flights
    // (fxceless/victory-fork#410).
    j.ahead_asked = now;
    j.written.note_all(at / PAGE..at.saturating_add(len).div_ceil(PAGE), now);
}

/// We wrote the 64 KiB page numbered `page` of title memory through a
/// view of our own (`crate::inplace::write_own`): no guard was opened and
/// no kernel write-watch bit set, so the journal is told here. The page,
/// and the same physical page at its other title addresses (`others`,
/// page numbers), count as written at a new asking, which every host copy
/// that holds them takes at its next refresh; with the [`STEP`]s the write
/// stored (`steps`), so that a copy that took a page since it was last
/// written otherwise compares only those ([`OwnMark`]). Returns the asking
/// that had last found `page` written (0: never) and the new one, for the
/// copy that wrote to note itself current.
pub(crate) fn wrote_own(page: usize, others: &[usize], steps: u16) -> (u64, u64) {
    let mut j = JOURNAL.lock().unwrap_or_else(|e| e.into_inner());
    let j = &mut *j;
    j.asked += 1;
    let now = j.asked;
    // As in [`wrote`]: an asking that stands for a submission, or for a
    // flush, moves with it, or a copy taking the page at the standing
    // asking would find it written "later" at every refresh.
    if j.kept || !j.ahead.is_empty() {
        j.ahead_asked = now;
    }
    let before = note_own(j, page, now, steps);
    for &other in others {
        if other != page {
            note_own(j, other, now, steps);
        }
    }
    (before, now)
}

/// Notes `page` written at the asking `now` by an own write of `steps`;
/// the page's stamp before.
fn note_own(j: &mut Journal, page: usize, now: u64, steps: u16) -> u64 {
    let before = j.written.note(page, now);
    let mark = own_noted(j.own.get(&page).copied(), before, now, steps);
    j.own.insert(page, mark);
    before
}

/// The steps of a page that an [`OwnMark`] keeps: 4 KiB, a refresh's
/// compare step.
pub(crate) const STEP: usize = 4 << 10;

/// What our own writes into a page stored since the page was last found
/// written otherwise ([`wrote_own`]). Every other write is stamped by the
/// kernel's write watch, the guards or [`wrote`] at an asking after it
/// happens, and our own writes neither set a write-watch bit nor open a
/// guard; so while the page's stamp is still the mark's last own write, a
/// copy that took the page at or after the mark's `whole` stamp can differ
/// from title memory only in the mark's steps.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct OwnMark {
    /// The stamp of the last own write: the mark speaks for the page only
    /// while the page's stamp is this one.
    pub(crate) last: u64,
    /// The page's stamp before the run's first own write: a copy that took
    /// the page before it must take the whole page.
    pub(crate) whole: u64,
    /// The [`STEP`]s the run's own writes stored, bit `i` for step `i`.
    pub(crate) steps: u16,
}

/// The [`STEP`]s of its page that `[at, at + len)` covers (the stretch lies
/// in one page, as every merged stretch does), bit `i` for step `i`.
pub(crate) fn steps_of(at: usize, len: usize) -> u16 {
    if len == 0 {
        return 0;
    }
    let first = at % PAGE / STEP;
    let last = (at % PAGE + len - 1).min(PAGE - 1) / STEP;
    (((1u32 << (last - first + 1)) - 1) << first) as u16
}

/// The mark of a page after an own write of `steps` at the stamp `now`,
/// the page's stamp having been `before`: the run goes on if `before` is
/// its last own write, and starts again after any other write.
fn own_noted(mark: Option<OwnMark>, before: u64, now: u64, steps: u16) -> OwnMark {
    match mark {
        Some(m) if m.last == before => OwnMark {
            last: now,
            whole: m.whole,
            steps: m.steps | steps,
        },
        _ => OwnMark {
            last: now,
            whole: before,
            steps,
        },
    }
}

/// The steps a copy must compare of a page stamped `stamp` that it took at
/// `taken` (before `stamp`): the mark's, if the stamp is the mark's last
/// own write and the copy took the page at or after the run began; `None`
/// for the whole page (written otherwise since the copy took it, or never
/// taken).
pub(crate) fn own_steps(mark: Option<OwnMark>, stamp: u64, taken: u64) -> Option<u16> {
    mark.filter(|m| taken != 0 && m.last == stamp && m.whole <= taken).map(|m| m.steps)
}

/// The [`OwnMark`]s of pages `[first, last)` that have one, in page order.
pub(crate) fn own_marks(first: usize, last: usize) -> Vec<(usize, OwnMark)> {
    let j = JOURNAL.lock().unwrap_or_else(|e| e.into_inner());
    j.own.range(first..last).map(|(&p, &m)| (p, m)).collect()
}

#[cfg(test)]
mod own_mark_tests {
    use super::{OwnMark, PAGE, STEP, own_marks, own_noted, own_steps, steps_of, wrote_own};

    #[test]
    fn steps_of_covers_the_steps_a_stretch_touches() {
        let page = 7 * PAGE;
        assert_eq!(steps_of(page, 0), 0);
        assert_eq!(steps_of(page, 1), 0b1);
        assert_eq!(steps_of(page + STEP - 1, 1), 0b1);
        assert_eq!(steps_of(page + STEP - 1, 2), 0b11);
        assert_eq!(steps_of(page + STEP, STEP), 0b10);
        assert_eq!(steps_of(page + 3 * STEP + 5, 2 * STEP), 0b111 << 3);
        assert_eq!(steps_of(page, PAGE), 0xFFFF);
        assert_eq!(steps_of(page + PAGE - 1, 1), 1 << 15);
        // Never past its page.
        assert_eq!(steps_of(page + PAGE - 1, 10), 1 << 15);
    }

    /// A mark's stamps and steps.
    fn parts(m: OwnMark) -> (u64, u64, u16) {
        (m.last, m.whole, m.steps)
    }

    #[test]
    fn a_run_of_own_writes_keeps_the_stamp_before_it() {
        // The first own write to a page last stamped 5.
        let m = own_noted(None, 5, 8, 0b1);
        assert_eq!(parts(m), (8, 5, 0b1));
        // The next finds the page at 8: the run goes on.
        let m = own_noted(Some(m), 8, 9, 0b100);
        assert_eq!(parts(m), (9, 5, 0b101));
        // Written otherwise at 10 since: a new run from 10.
        let m = own_noted(Some(m), 10, 12, 0b1000);
        assert_eq!(parts(m), (12, 10, 0b1000));
        // A page never written before.
        assert_eq!(parts(own_noted(None, 0, 3, 0b10)), (3, 0, 0b10));
    }

    #[test]
    fn only_a_copy_that_took_the_page_in_the_run_compares_steps() {
        // Written otherwise at 10, then by an own write at 12.
        let m = Some(own_noted(None, 10, 12, 0b1010));
        // Taken at the run's start or later: the steps.
        assert_eq!(own_steps(m, 12, 10), Some(0b1010));
        assert_eq!(own_steps(m, 12, 11), Some(0b1010));
        // Taken before the other write at 10: the whole page.
        assert_eq!(own_steps(m, 12, 9), None);
        // Stamped since by another write, never taken, or no mark: the whole page.
        assert_eq!(own_steps(m, 13, 11), None);
        assert_eq!(own_steps(m, 12, 0), None);
        assert_eq!(own_steps(None, 12, 11), None);
    }

    #[test]
    fn the_journal_marks_a_page_and_its_other_addresses() {
        // Pages no other test writes.
        let (page, other) = (0x7_1234_0000 / PAGE, 0x7_5678_0000 / PAGE);
        let (_, first) = wrote_own(page, &[other], steps_of(page * PAGE + 2 * STEP, 16));
        let (before, second) = wrote_own(page, &[other], steps_of(page * PAGE + 9 * STEP, STEP));
        assert_eq!(before, first);
        for p in [page, other] {
            let marks = own_marks(p, p + 1);
            assert_eq!(marks.len(), 1);
            assert_eq!(marks[0].0, p);
            assert_eq!(parts(marks[0].1), (second, 0, (1 << 9) | (1 << 2)));
        }
    }
}

/// One kernel asking for `pieces` (page ranges in order); by halves if it
/// fails, as a run across two reservations or into memory that is not
/// write-watched does. A piece alone is left to [`writes`].
fn ask_run(j: &mut Journal, pieces: &[(usize, usize)]) {
    if pieces.len() < 2 {
        return;
    }
    let first = pieces[0].0;
    let last = pieces.iter().map(|p| p.1).max().unwrap_or(first);
    if ask_kernel(j, first, last) {
        j.ahead.push((first, last));
        return;
    }
    let (a, b) = pieces.split_at(pieces.len() / 2);
    ask_run(j, a);
    ask_run(j, b);
}

/// Asks the kernel for the written pages of pages `[first, last)` and notes
/// them in the journal under the ahead asking; false (and nothing changed)
/// if the range is not write-watched memory of one reservation.
fn ask_kernel(j: &mut Journal, first: usize, last: usize) -> bool {
    use windows_sys::Win32::System::Memory::GetWriteWatch;
    const WRITE_WATCH_FLAG_RESET: u32 = 1;
    let mut count = (last - first) * PAGE / KERNEL_PAGE;
    j.found.resize(count.max(j.found.len()), 0);
    let mut granularity = 0;
    #[cfg(feature = "diagnostics")]
    let started = std::time::Instant::now();
    // SAFETY: the answer buffer holds `count` entries.
    let failed = unsafe {
        GetWriteWatch(
            WRITE_WATCH_FLAG_RESET,
            (first * PAGE) as *const _,
            (last - first) * PAGE,
            j.found.as_mut_ptr().cast(),
            &mut count,
            &mut granularity,
        )
    } != 0;
    if failed {
        return false;
    }
    #[cfg(feature = "diagnostics")]
    {
        use crate::copystat::{C, add};
        add(C::Watches, 1);
        let us = started.elapsed().as_micros() as u64;
        add(C::WatchUs, us);
        slow(us);
    }
    let now = j.ahead_asked;
    j.written.note_all(j.found[..count].iter().map(|&at| at / PAGE), now);
    true
}

/// What [`writes_into`] would say of `[start, end)` from the journal alone,
/// if the range lies in one asked ahead ([`ask_ahead`], [`ask_once`]): the
/// stamp of each page, with the number of the asking that stands (as
/// [`writes_into`] gives them). `None` if the kernel would have to be
/// asked.
pub(crate) fn asked(start: usize, end: usize) -> Option<(u64, Vec<u64>)> {
    let (first, last) = (start / PAGE, end.div_ceil(PAGE));
    let j = JOURNAL.lock().unwrap_or_else(|e| e.into_inner());
    j.ahead.iter().any(|&(a, b)| a <= first && last <= b).then(|| {
        let mut written = Vec::new();
        j.written.read_into(first, last, &mut written);
        (j.ahead_asked, written)
    })
}

/// The pages of `[start, end)` (whole pages) as last written, for pages
/// written since tracking began: `(page number, asking)` in page order, and
/// this asking's number, which is higher than any in the list but those the
/// kernel just reported. `None` if the memory is not write-watched (nor,
/// with `guarded`, view memory, which is then guarded).
pub(crate) fn writes(start: usize, end: usize, guarded: bool) -> Option<(u64, Vec<(usize, u64)>)> {
    thread_local! {
        // With [`lean`]: room for this thread's stamps, kept between
        // askings (the texture checks ask some hundreds of times a frame,
        // each list grown under the journal's lock).
        static ROOM: std::cell::Cell<Vec<u64>> = const { std::cell::Cell::new(Vec::new()) };
    }
    let mut written = if lean() { ROOM.take() } else { Vec::new() };
    let answer: Option<(u64, Vec<(usize, u64)>)> = writes_into(start, end, guarded, &mut written)
        .map(|now| (now, (start / PAGE..).zip(written.iter().copied()).filter(|&(_, at)| at != 0).collect()));
    if lean() {
        ROOM.set(written);
    }
    answer
}

/// [`writes`] with every page's stamp put into `written` (emptied first),
/// one per page of the range in order: the asking that last found the page
/// written, 0 if none did since tracking began. For the submitting thread's
/// refreshes, which ask some 20,000 times a frame and keep the room between
/// askings (a new list each time, grown entry by entry, was a tenth of that
/// thread's samples).
pub(crate) fn writes_into(start: usize, end: usize, guarded: bool, written: &mut Vec<u64>) -> Option<u64> {
    use windows_sys::Win32::System::Memory::GetWriteWatch;
    const WRITE_WATCH_FLAG_RESET: u32 = 1;
    written.clear();
    let (first, last) = (start / PAGE, end.div_ceil(PAGE));
    let mut j = JOURNAL.lock().unwrap_or_else(|e| e.into_inner());
    let j = &mut *j;
    // Asked ahead with the submission's other ranges: the journal has it.
    if j.ahead.iter().any(|&(a, b)| a <= first && last <= b) {
        j.written.read_into(first, last, written);
        return Some(j.ahead_asked);
    }
    if lean() {
        return asked_lean(j, first, last, guarded, written);
    }
    #[cfg(feature = "diagnostics")]
    let started = std::time::Instant::now();
    // View memory is not write-watched: where the guards know the range as
    // a view, the kernel asking would only fail, at the cost of the call.
    let view = guarded && known_view(first * PAGE / BLOCK);
    let mut count = 0;
    let failed = view || {
        count = (last - first) * PAGE / KERNEL_PAGE;
        j.found.resize(count.max(j.found.len()), 0);
        let mut granularity = 0;
        // SAFETY: the answer buffer holds `count` entries.
        let status = unsafe {
            GetWriteWatch(
                WRITE_WATCH_FLAG_RESET,
                (first * PAGE) as *const _,
                (last - first) * PAGE,
                j.found.as_mut_ptr().cast(),
                &mut count,
                &mut granularity,
            )
        };
        status != 0
    };
    #[cfg(feature = "diagnostics")]
    crate::copystat::add(
        if view {
            crate::copystat::C::WatchKnownView
        } else {
            crate::copystat::C::WatchFailed
        },
        failed as u64,
    );
    #[cfg(feature = "diagnostics")]
    if failed && !view && guarded {
        failed_asking(first * PAGE, last * PAGE);
    }
    let guarded = if failed {
        count = 0;
        if !guarded {
            return None;
        }
        #[cfg(feature = "diagnostics")]
        let guarding = std::time::Instant::now();
        let pages = guard(first * PAGE, last * PAGE, view);
        #[cfg(feature = "diagnostics")]
        crate::copystat::add(crate::copystat::C::GuardUs, guarding.elapsed().as_micros() as u64);
        pages?
    } else {
        Vec::new()
    };
    #[cfg(feature = "diagnostics")]
    {
        use crate::copystat::{C, add};
        add(C::Watches, 1);
        let us = started.elapsed().as_micros() as u64;
        add(C::WatchUs, us);
        slow(us);
    }
    j.asked += 1;
    let now = j.asked;
    j.written.note_all(j.found[..count].iter().map(|&at| at / PAGE), now);
    j.written.note_all(guarded, now);
    j.written.read_into(first, last, written);
    Some(now)
}

/// Whether an asking the journal cannot answer from what it holds pays its
/// fixed costs once (`VICTORY_ASK_LEAN`, on by default; `=0` as before):
/// the guards are taken once for the question whether they know the range
/// as view memory and for their answer (twice before, a lock and a walk
/// down their tree each time), the clock is read twice for the asking's
/// diagnostics (four times before), the guards' answer and their walk's
/// list of blocks go into rooms kept between askings (two lists grown from
/// empty and dropped before), and the journal's stamps and the guards'
/// clean blocks are looked up, not searched for as a range, where the
/// range lies in one chunk or one word. [`writes`] keeps its stamps in a
/// room of the asking thread.
///
/// In a parked scene the refreshes asked past what the
/// journal held some 2,200 times a frame, every one of view memory the
/// guards knew and 97 % of them all clean, so these costs were most of each
/// asking, paid on the submitting thread with the journal's lock held,
/// which the texture checks on the kept threads wait for. The pages
/// reported written, the guards' state and the protections asked are the
/// same either way.
fn lean() -> bool {
    static ON: std::sync::LazyLock<bool> =
        std::sync::LazyLock::new(|| std::env::var_os("VICTORY_ASK_LEAN").is_none_or(|v| v != "0"));
    *ON
}

/// Since the last frame-rate line, of the askings [`asked_lean`] answered:
/// the askings, those of view memory the guards answered under one take of
/// their lock, and the microseconds of those (diagnostics builds).
static LEAN: [std::sync::atomic::AtomicU64; 3] = [const { std::sync::atomic::AtomicU64::new(0) }; 3];

/// For the frame-rate line: [`LEAN`] per frame. Nothing with the switch off.
pub(crate) fn lean_line(frames: f64) {
    let [asked, views, us] = [0, 1, 2].map(|i| LEAN[i].swap(0, std::sync::atomic::Ordering::Relaxed));
    if asked == 0 {
        return;
    }
    tracing::info!(
        "write-watch askings past what the journal held per frame, their fixed costs paid once \
         (VICTORY_ASK_LEAN=0: as before): {:.0}, {:.0} of them of view memory the guards answered under one \
         take of their lock, {:.2} ms (timed in diagnostics builds)",
        asked as f64 / frames,
        views as f64 / frames,
        us as f64 / 1e3 / frames
    );
}

/// [`writes_into`] past what the journal holds, with [`lean`]'s cuts. The
/// question to the kernel or the guards, their answer and the journal's
/// notes are those of [`writes_into`] as before.
fn asked_lean(j: &mut Journal, first: usize, last: usize, guarded: bool, written: &mut Vec<u64>) -> Option<u64> {
    use std::sync::atomic::Ordering::Relaxed;
    use windows_sys::Win32::System::Memory::GetWriteWatch;
    const WRITE_WATCH_FLAG_RESET: u32 = 1;
    #[cfg(feature = "diagnostics")]
    let started = std::time::Instant::now();
    // Diagnostics: when the guards' work began, if they did any.
    #[cfg(feature = "diagnostics")]
    let mut guarding = None;
    let (start, end) = (first * PAGE, last * PAGE);
    let (mut pages, mut blocks) = (std::mem::take(&mut j.pages), std::mem::take(&mut j.blocks));
    pages.clear();
    let mut count = 0;
    // View memory is not write-watched: where the guards know the range as
    // a view, the kernel asking would only fail. The take of the guards
    // that asks them stays for their answer.
    let known = if guarded { view_guards().filter(|g| view_in(g, start / BLOCK)) } else { None };
    let view = known.is_some();
    let ok = match known {
        Some(mut g) => {
            #[cfg(feature = "diagnostics")]
            {
                crate::copystat::add(crate::copystat::C::WatchKnownView, 1);
                guarding = Some(started);
            }
            guard_asked(true);
            install();
            guard_in(&mut g, start / BLOCK, end.div_ceil(BLOCK), &mut pages, &mut blocks);
            true
        }
        None => {
            count = (last - first) * PAGE / KERNEL_PAGE;
            j.found.resize(count.max(j.found.len()), 0);
            let mut granularity = 0;
            // SAFETY: the answer buffer holds `count` entries.
            let failed = unsafe {
                GetWriteWatch(
                    WRITE_WATCH_FLAG_RESET,
                    start as *const _,
                    end - start,
                    j.found.as_mut_ptr().cast(),
                    &mut count,
                    &mut granularity,
                )
            } != 0;
            #[cfg(feature = "diagnostics")]
            crate::copystat::add(crate::copystat::C::WatchFailed, failed as u64);
            if failed {
                count = 0;
            }
            if !failed {
                true
            } else if guarded {
                #[cfg(feature = "diagnostics")]
                {
                    failed_asking(start, end);
                    guarding = Some(std::time::Instant::now());
                }
                guard_into(start, end, false, &mut pages, &mut blocks)
            } else {
                false
            }
        }
    };
    #[cfg(feature = "diagnostics")]
    {
        use crate::copystat::{C, add};
        let ended = std::time::Instant::now();
        if let Some(at) = guarding {
            add(C::GuardUs, ended.saturating_duration_since(at).as_micros() as u64);
        }
        if ok {
            let us = ended.saturating_duration_since(started).as_micros() as u64;
            add(C::Watches, 1);
            add(C::WatchUs, us);
            slow(us);
            if view {
                LEAN[2].fetch_add(us, Relaxed);
            }
        }
    }
    if !ok {
        j.pages = pages;
        j.blocks = blocks;
        return None;
    }
    LEAN[0].fetch_add(1, Relaxed);
    LEAN[1].fetch_add(u64::from(view), Relaxed);
    j.asked += 1;
    let now = j.asked;
    j.written.note_all(j.found[..count].iter().map(|&at| at / PAGE), now);
    j.written.note_all(pages.iter().copied(), now);
    j.written.read_into(first, last, written);
    j.pages = pages;
    j.blocks = blocks;
    Some(now)
}

/// Diagnostics: one in 2,048 of the askings the kernel's write watch
/// refused for memory the guards did not know (some 200 a frame in the
/// first rooms, each followed by a question to Windows about the memory):
/// what the memory is and what the guards hold of its first block.
#[cfg(feature = "diagnostics")]
fn failed_asking(start: usize, end: usize) {
    use windows_sys::Win32::System::Memory::{MEMORY_BASIC_INFORMATION, VirtualQuery};
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    if N.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % 2048 != 0 {
        return;
    }
    // SAFETY: plain data filled in by the query.
    let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: a valid out pointer of the right size.
    let n = unsafe { VirtualQuery(start as *const _, &mut info, size_of_val(&info)) };
    let (protected, clean, absent, viewed, aliased) = {
        let g = guards();
        let b = start / BLOCK;
        (g.protected.contains(&b), g.clean.contains(b), g.absent.contains(b), g.views.contains(&b), g.aliased.contains(&b))
    };
    tracing::info!(
        "a failed write-watch asking (one in 2048): {start:#x}+{:#x}; Windows says (answered {n}): base {:#x} region {:#x} state {:#x} type {:#x} protect {:#x} allocation protect {:#x}; \
         the guards of its first block: protected {protected} clean {clean} absent {absent} viewed {viewed} aliased {aliased}; is a view: {}",
        end - start,
        info.BaseAddress as usize,
        info.RegionSize,
        info.State,
        info.Type,
        info.Protect,
        info.AllocationProtect,
        is_view(start, end)
    );
}

impl Tracked {
    /// Takes changed pages of `[from, to)`, cut to the tracked range. A
    /// partial page stays changed so a later take can read its other bytes.
    pub(crate) fn take(&self, from: usize, to: usize) -> Vec<(usize, usize)> {
        let (from, to) = (from.max(self.start), to.min(self.end));
        if from >= to {
            return Vec::new();
        }
        let Some((_, pages)) = writes(from, to, true) else {
            return Vec::new();
        };
        let mut seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        let mut ranges: Vec<(usize, usize)> = Vec::new();
        for (page, write) in pages {
            if seen.get(&page).is_some_and(|&last| last >= write) {
                continue;
            }
            let (start, end) = ((page * PAGE).max(from), ((page + 1) * PAGE).min(to));
            if start >= end {
                continue;
            }
            // Keep partial pages dirty: the caller has not taken the rest.
            if (page * PAGE).max(self.start) >= from
                && ((page + 1) * PAGE).min(self.end) <= to
            {
                seen.insert(page, write);
            }
            match ranges.last_mut() {
                Some((_, last)) if *last == start => *last = end,
                _ => ranges.push((start, end)),
            }
        }
        ranges
    }
}

/// Guarded blocks of view memory.
const BLOCK: usize = PAGE;

/// A set of blocks, a bit each, 64 to a word, the words by their number.
/// The guards ask whether all of a range is clean some 2,300 times a
/// frame, over 140,000 blocks in all: counted one by one in a tree of
/// blocks, that was most of the 18 ms a frame of "guards" (run int-c).
struct Blocks(BTreeMap<usize, u64>);

impl Blocks {
    const fn new() -> Self {
        Self(BTreeMap::new())
    }

    /// The bits of `range`'s blocks in word `word`.
    fn mask(word: usize, range: &std::ops::Range<usize>) -> u64 {
        let low = word * 64;
        let (from, to) = (range.start.max(low), range.end.min(low + 64));
        if from >= to {
            return 0;
        }
        (u64::MAX >> (64 - (to - from))) << (from - low)
    }

    fn insert(&mut self, block: usize) {
        *self.0.entry(block / 64).or_insert(0) |= 1 << (block % 64);
    }

    fn remove(&mut self, block: usize) {
        if let Some(bits) = self.0.get_mut(&(block / 64)) {
            *bits &= !(1 << (block % 64));
            if *bits == 0 {
                self.0.remove(&(block / 64));
            }
        }
    }

    fn contains(&self, block: usize) -> bool {
        self.0.get(&(block / 64)).is_some_and(|bits| bits >> (block % 64) & 1 != 0)
    }

    /// Takes `bits` of word `word` out of the set.
    fn clear(&mut self, word: usize, bits: u64) {
        if let Some(held) = self.0.get_mut(&word) {
            *held &= !bits;
            if *held == 0 {
                self.0.remove(&word);
            }
        }
    }

    fn extend(&mut self, range: std::ops::Range<usize>) {
        if range.is_empty() {
            return;
        }
        for word in range.start / 64..=(range.end - 1) / 64 {
            *self.0.entry(word).or_insert(0) |= Self::mask(word, &range);
        }
    }

    /// Whether every block of `range` is in the set.
    fn holds(&self, range: std::ops::Range<usize>) -> bool {
        if range.is_empty() {
            return true;
        }
        let (first, last) = (range.start / 64, (range.end - 1) / 64);
        // A range in one word (most askings) is looked up, not searched for
        // as a range ([`lean`]).
        if first == last && lean() {
            let mask = Self::mask(first, &range);
            return self.0.get(&first).is_some_and(|&bits| bits & mask == mask);
        }
        let mut words = self.0.range(first..=last);
        (first..=last).all(|word| {
            let mask = Self::mask(word, &range);
            words.next().is_some_and(|(&at, &bits)| at == word && bits & mask == mask)
        })
    }

    /// The set's blocks in `range`, in order.
    fn range(&self, range: std::ops::Range<usize>) -> impl Iterator<Item = usize> + '_ {
        let first = range.start / 64;
        let last = (range.end.max(range.start + 1) - 1) / 64;
        self.0.range(first..=last).flat_map(move |(&word, &bits)| {
            let mut left = bits & Self::mask(word, &range);
            std::iter::from_fn(move || {
                (left != 0).then(|| {
                    let bit = left.trailing_zeros() as usize;
                    left &= left - 1;
                    word * 64 + bit
                })
            })
        })
    }

    /// [`Blocks::outside_into`] into a new list.
    #[cfg(test)]
    fn outside(&self, range: std::ops::Range<usize>) -> Vec<usize> {
        let mut out = Vec::new();
        self.outside_into(range, &mut out);
        out
    }

    /// The blocks of `range` not in the set, in order, into `out` (emptied
    /// first), read a word of blocks at a time. The list is the set as it
    /// is now: what the caller adds to the set or takes out of it
    /// afterwards does not change it.
    fn outside_into(&self, range: std::ops::Range<usize>, out: &mut Vec<usize>) {
        out.clear();
        if range.is_empty() {
            return;
        }
        let (first, last) = (range.start / 64, (range.end - 1) / 64);
        let mut words = self.0.range(first..=last).peekable();
        for word in first..=last {
            let held = words.next_if(|&(&at, _)| at == word).map_or(0, |(_, &bits)| bits);
            let mut left = !held & Self::mask(word, &range);
            while left != 0 {
                out.push(word * 64 + left.trailing_zeros() as usize);
                left &= left - 1;
            }
        }
    }
}

struct Guards {
    /// Blocks we made read-only.
    protected: BTreeSet<usize>,
    /// Of those, the ones not written since a copy took them.
    clean: Blocks,
    /// Blocks already copied as zeros. While they remain wholly uncommitted,
    /// the copy can skip hundreds of MB of failing kernel reads per world frame.
    /// With [`holes_level`] 2 those also in `clean` were found uncommitted
    /// and hold as holes; the others are due a look.
    absent: Blocks,
    /// Blocks we made read-only at some time, whose mapping the runtime
    /// has not changed since: `protected`, and the blocks a write took out
    /// of it. They are view memory still.
    views: BTreeSet<usize>,
    /// With [`holes_level`] 1 and 2: runs of uncommitted blocks as Windows
    /// described them in frame `holes_frame`, first block to end.
    holes: BTreeMap<usize, usize>,
    holes_frame: u64,
    /// With [`holes_level`] 2: the frame whose share of the holes was last
    /// made due ([`holes_due`]).
    due_frame: u64,
    /// With [`views_recheck`]: the frame whose share of the clean guarded
    /// blocks was last made due ([`views_due`]).
    views_frame: u64,
    /// Blocks whose physical page was guarded together with its other views
    /// (those views' blocks are in it too): a write to one opens them all.
    /// A write range can be huge (`fread`), so only these are looked up.
    aliased: BTreeSet<usize>,
}

static GUARDS: Mutex<Guards> = Mutex::new(Guards {
    protected: BTreeSet::new(),
    clean: Blocks::new(),
    absent: Blocks::new(),
    views: BTreeSet::new(),
    holes: BTreeMap::new(),
    holes_frame: 0,
    due_frame: 0,
    views_frame: 0,
    aliased: BTreeSet::new(),
});

fn guards() -> std::sync::MutexGuard<'static, Guards> {
    GUARDS.lock().unwrap_or_else(|e| e.into_inner())
}

/// Whether the guards know `block` as view memory, with no question to
/// Windows: guarded by our hand at some time and not remapped since, or
/// noted absent. The question ([`is_view`]) walks the page tables to the
/// end of the pages that share the first one's state, and a guarded view
/// is one long run of read-only pages: 0.6 ms for the title's 174 MB heap,
/// 16 times a frame, and most of the 68 ms a frame of "guards" (run 58).
/// A block the title wrote since it was guarded is writable again but a
/// view as before; asking about ranges that begin in one kept
/// `VirtualQuery` at 12% of the submitting thread's samples (run int-b).
/// `VICTORY_VIEW_ASK=1` (or `VICTORY_GUARD_ASK=1`) asks every time, as
/// before.
fn known_view(block: usize) -> bool {
    view_guards().is_some_and(|g| view_in(&g, block))
}

/// The guards, taken for [`view_in`]'s questions; `None` with
/// `VICTORY_VIEW_ASK=1` (or `VICTORY_GUARD_ASK=1`), where Windows is asked
/// every time.
fn view_guards() -> Option<std::sync::MutexGuard<'static, Guards>> {
    static ASK: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let ask = *ASK.get_or_init(|| {
        ["VICTORY_VIEW_ASK", "VICTORY_GUARD_ASK"]
            .iter()
            .any(|n| std::env::var_os(n).is_some_and(|v| v == "1"))
    });
    (!ask).then(guards)
}

/// [`known_view`] under a take of the guards the caller holds.
fn view_in(g: &Guards, block: usize) -> bool {
    g.views.contains(&block) || g.absent.contains(block)
}

/// Whether the blocks of a range the runtime maps or unmaps are noted as
/// view memory rather than forgotten ([`title_memory_remapped`];
/// `VICTORY_VIEW_KNOWN=1`, off by default). Blocks made read-only by our
/// hand are known as view memory without it, whether their guard stands
/// now or a write has opened it (`Guards::views`).
///
/// `VICTORY_VIEW_KNOWN=alt`: twenty seconds with, twenty without, in turn,
/// each change said in the log, for measuring it in one scene.
fn view_known() -> bool {
    /// 0 off, 1 on, 2 in turn.
    static MODE: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    match *MODE.get_or_init(|| match std::env::var("VICTORY_VIEW_KNOWN").as_deref() {
        Ok("1") => 1,
        Ok("alt") => 2,
        _ => 0,
    }) {
        0 => false,
        1 => true,
        _ => {
            static EPOCH: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
            let on = (EPOCH.get_or_init(std::time::Instant::now).elapsed().as_secs() / 20) % 2 == 1;
            static LAST: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(2);
            if LAST.swap(u8::from(on), std::sync::atomic::Ordering::Relaxed) != u8::from(on) {
                tracing::info!(
                    "ranges the runtime maps or unmaps are noted as view memory from now (VICTORY_VIEW_KNOWN=alt): {}",
                    if on { "ON" } else { "off, as usual" }
                );
            }
            on
        }
    }
}

/// Whether the block `page` is read-only by our hand and unwritten since a
/// copy took it (diagnostics of `VICTORY_BACK_OWN_CHECK`).
pub(crate) fn clean(page: usize) -> bool {
    guards().clean.contains(page)
}

/// For view memory: the pages of `[start, end)`'s blocks written since
/// they were last guarded (all of a block's pages), guarding them again
/// before the caller copies them. `None` if the memory is not a view
/// (`known`: [`known_view`] said it is one).
fn guard(start: usize, end: usize, known: bool) -> Option<Vec<usize>> {
    let mut written = Vec::new();
    guard_into(start, end, known, &mut written, &mut Vec::new()).then_some(written)
}

/// [`guard`] into the caller's rooms: the pages into `written` (emptied
/// first), `blocks` for the walk's list. False if the memory is not a view.
fn guard_into(start: usize, end: usize, known: bool, written: &mut Vec<usize>, blocks: &mut Vec<usize>) -> bool {
    written.clear();
    guard_asked(known);
    if !known && !is_view(start, end) {
        return false;
    }
    install();
    guard_in(&mut guards(), start / BLOCK, end.div_ceil(BLOCK), written, blocks);
    true
}

/// [`guard`]'s counts of an asking (`known`: with no question to Windows).
fn guard_asked(known: bool) {
    #[cfg(feature = "diagnostics")]
    {
        use crate::copystat::{C, add};
        add(C::GuardAsks, 1);
        add(C::GuardKnown, u64::from(known));
    }
    #[cfg(not(feature = "diagnostics"))]
    let _ = known;
}

/// [`guard`] of the blocks `[first, last)` of view memory, under the take
/// of the guards the caller holds: the pages written into `written`
/// (emptied first), `blocks` the room for the walk's list.
fn guard_in(g: &mut Guards, first: usize, last: usize, written: &mut Vec<usize>, blocks: &mut Vec<usize>) {
    written.clear();
    apply_changed(g);
    holes_due(g);
    views_due(g);
    // Nearly all of what is asked about is clean (a frame asks about some
    // 9 GB of view memory, 140,000 blocks): answered a word of blocks at
    // a time.
    if g.clean.holds(first..last) {
        // Nothing written, mapped or new since the range was last taken.
        #[cfg(feature = "diagnostics")]
        crate::copystat::add(crate::copystat::C::GuardClean, 1);
        return;
    }
    #[cfg(feature = "diagnostics")]
    crate::copystat::add(crate::copystat::C::GuardBlocks, (last - first) as u64);
    #[cfg(feature = "diagnostics")]
    let walking = std::time::Instant::now();
    if guard_words() {
        walk_words(g, first, last, blocks, written);
    } else {
        walk_blocks(g, first, last, written);
    }
    #[cfg(feature = "diagnostics")]
    {
        use std::sync::atomic::Ordering::Relaxed;
        WALKS[0].fetch_add(1, Relaxed);
        WALKS[3].fetch_add(walking.elapsed().as_micros() as u64, Relaxed);
    }
}

/// Whether [`guard`] walks a range that is not all clean a word of 64
/// blocks at a time ([`walk_words`]; `VICTORY_GUARD_WORDS`, on by default)
/// or block by block ([`walk_blocks`], `VICTORY_GUARD_WORDS=0`, as before).
///
/// In a parked scene some 70 askings a frame found a range of view memory
/// not all clean and walked some 41,000 blocks block by block, with the
/// journal's lock held; listing the clean blocks of each range alone was
/// 3.7 % of the samples taken in the submissions. A word of 64 clean
/// blocks is passed over at once.
fn guard_words() -> bool {
    static ON: std::sync::LazyLock<bool> =
        std::sync::LazyLock::new(|| std::env::var_os("VICTORY_GUARD_WORDS").is_none_or(|v| v != "0"));
    *ON
}

/// Since the last frame line, of [`guard`]'s walks over ranges that are
/// not all clean: the walks, the words of 64 blocks read whole
/// ([`walk_words`]; none block by block), the blocks looked at one by one,
/// and the microseconds the walks took.
#[cfg(feature = "diagnostics")]
static WALKS: [std::sync::atomic::AtomicU64; 4] = [const { std::sync::atomic::AtomicU64::new(0) }; 4];

/// [`WALKS`], taken.
#[cfg(feature = "diagnostics")]
pub(crate) fn guard_walks() -> [u64; 4] {
    [0, 1, 2, 3].map(|i| WALKS[i].swap(0, std::sync::atomic::Ordering::Relaxed))
}

/// [`guard`]'s walk over `[first, last)`, which is not all clean, block by
/// block: the pages of the blocks written since they were guarded put into
/// `written`, and those blocks guarded again.
fn walk_blocks(g: &mut Guards, first: usize, last: usize, written: &mut Vec<usize>) {
    #[cfg(feature = "diagnostics")]
    WALKS[2].fetch_add((last - first) as u64, std::sync::atomic::Ordering::Relaxed);
    let mut run: Option<usize> = None;
    // The range's clean blocks, in order: read off the set in one pass.
    // What the loop adds to the set meanwhile is the block it stands at
    // or ones before it.
    let clean: Vec<usize> = g.clean.range(first..last).collect();
    let mut next_clean = 0;
    let mut uncommitted_until = first;
    for block in first..=last {
        while next_clean < clean.len() && clean[next_clean] < block {
            next_clean += 1;
        }
        let is_clean = next_clean < clean.len() && clean[next_clean] == block;
        let mut open = block < last && !is_clean;
        if open && g.absent.contains(block) {
            // Ordinary commits need no runtime mapping notification. Reuse
            // this asking's query for the following blocks in the same region.
            if block >= uncommitted_until {
                uncommitted_until = hole_end(g, block);
            }
            open = block >= uncommitted_until;
            if open {
                g.absent.remove(block);
                if holes_level() == 2 {
                    untold(block);
                }
            } else if holes_level() == 2 {
                // A hole still: it holds until it is due again, or told
                // mapped or committed.
                g.clean.insert(block);
            }
        }
        if open {
            written.extend(block * BLOCK / PAGE..(block + 1) * BLOCK / PAGE);
            if g.protected.contains(&block) {
                // Written through the runtime, still read-only.
                g.clean.insert(block);
            } else {
                run.get_or_insert(block);
                continue;
            }
        }
        if let Some(from) = run.take() {
            protect(g, from, block);
        }
    }
}

/// [`walk_blocks`] a word of 64 blocks at a time: only the blocks that are
/// not clean when the walk begins are looked at ([`Blocks::outside_into`]), each
/// as [`walk_blocks`] looks at it, in the same order. A run of blocks to
/// guard again is guarded where [`walk_blocks`] guards it: at the first
/// block after it, clean or not, before the next block that is not clean
/// is looked at. The answer, the guards' state and the protections asked
/// are the same: [`walk_blocks`] also takes the clean blocks as they were
/// when it began (a guard asked for a run can make other views of its
/// pages, further in the range, read-only and clean). That list is made in
/// `blocks`, a room of the caller's.
fn walk_words(g: &mut Guards, first: usize, last: usize, blocks: &mut Vec<usize>, written: &mut Vec<usize>) {
    g.clean.outside_into(first..last, blocks);
    #[cfg(feature = "diagnostics")]
    {
        use std::sync::atomic::Ordering::Relaxed;
        WALKS[1].fetch_add(((last - 1) / 64 - first / 64 + 1) as u64, Relaxed);
        WALKS[2].fetch_add(blocks.len() as u64, Relaxed);
    }
    // The run of written blocks that are not read-only: its first block and
    // the block after its last.
    let mut run: Option<(usize, usize)> = None;
    let mut uncommitted_until = first;
    for &block in blocks.iter() {
        // Clean blocks lie between the run and this block: the run ends at
        // the first of them.
        if let Some((from, end)) = run
            && end < block
        {
            protect(g, from, end);
            run = None;
        }
        let mut open = true;
        if g.absent.contains(block) {
            if block >= uncommitted_until {
                uncommitted_until = hole_end(g, block);
            }
            open = block >= uncommitted_until;
            if open {
                g.absent.remove(block);
                if holes_level() == 2 {
                    untold(block);
                }
            } else if holes_level() == 2 {
                g.clean.insert(block);
            }
        }
        if open {
            written.extend(block * BLOCK / PAGE..(block + 1) * BLOCK / PAGE);
            if g.protected.contains(&block) {
                g.clean.insert(block);
            } else {
                run = Some((run.map_or(block, |(from, _)| from), block + 1));
                continue;
            }
        }
        if let Some((from, _)) = run.take() {
            protect(g, from, block);
        }
    }
    if let Some((from, end)) = run {
        protect(g, from, end);
    }
}

/// Whether `[start, end)` is view memory. The title's views lie in
/// placeholders, mapped and unmapped block by block as it streams, so a
/// range can begin in a block that is not mapped right now: the first
/// pages of the range that are there decide. A range with nothing there
/// counts as a view (its blocks are noted absent, and looked at again when
/// the runtime maps them).
///
/// Looking at the first address only took such a range for "not a view":
/// its copy was marked not watched for good and copied whole at every
/// refresh, around its unmapped pages (run 54: 300 such ranges, 570 MB
/// copied a frame for them, 255 copies a frame reading around holes).
/// `VICTORY_GUARD_HOLES=0` looks at the first address only, as before.
fn is_view(start: usize, end: usize) -> bool {
    use windows_sys::Win32::System::Memory::{
        MEM_COMMIT, MEM_MAPPED, MEMORY_BASIC_INFORMATION, VirtualQuery,
    };
    static HOLES: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let holes = *HOLES.get_or_init(|| std::env::var_os("VICTORY_GUARD_HOLES").is_none_or(|v| v != "0"));
    let mut at = start;
    loop {
        // SAFETY: plain data filled in by the query.
        let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: a valid out pointer of the right size.
        let n = unsafe { VirtualQuery(at as *const _, &mut info, size_of_val(&info)) };
        #[cfg(feature = "diagnostics")]
        crate::copystat::add(crate::copystat::C::GuardQueries, 1);
        if n == 0 {
            return false;
        }
        if info.Type == MEM_MAPPED {
            return true;
        }
        // Committed memory that is no view (the title's heaps, ours), or
        // the answer as it was before.
        if info.State == MEM_COMMIT || !holes {
            return false;
        }
        // A hole (a placeholder, or free): on to what follows it.
        let next = (info.BaseAddress as usize).saturating_add(info.RegionSize);
        if next <= at || next >= end {
            return true;
        }
        at = next;
    }
}

/// Makes blocks `[from, to)` read-only and notes them guarded and clean;
/// block by block if they span views, leaving out any that fail, and
/// noting those not committed (unmapped) absent. The same physical pages
/// at every other view of them are made read-only too: a write through
/// one of those is this memory written, and only a fault can say so.
/// `VICTORY_VIEW_GUARD=0` leaves the other views as they are, as before.
fn protect(g: &mut Guards, from: usize, to: usize) {
    protect_here(g, from, to);
    if !view_guard() {
        return;
    }
    for (base, bytes) in crate::inplace::views_between(from.saturating_mul(BLOCK), to.saturating_mul(BLOCK)) {
        let first = (base / BLOCK).max(from);
        let last = (base.saturating_add(bytes).div_ceil(BLOCK)).min(to);
        for block in first..last {
            let shown = crate::inplace::pages_of(block * BLOCK);
            // One view (nothing else to guard), or the page the title shows
            // wherever it has nothing (thousands of views): each view of
            // that one is guarded by itself.
            if shown.len() < 2 || shown.len() > MANY_VIEWS {
                continue;
            }
            #[cfg(feature = "diagnostics")]
            ALIASES[0].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            ALIASED_EVER.store(true, std::sync::atomic::Ordering::Relaxed);
            for alias in shown {
                let alias = alias / BLOCK;
                g.aliased.insert(alias);
                if alias != block {
                    protect_alias(g, alias);
                }
            }
        }
    }
}

/// Views of one page past which its views are not guarded together.
pub(crate) const MANY_VIEWS: usize = 64;

/// Whether all the views of a guarded page are guarded together
/// (`VICTORY_VIEW_GUARD`).
fn view_guard() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_VIEW_GUARD").is_none_or(|v| v != "0"))
}

/// Whether [`protect`] has ever guarded a block together with other views
/// of its pages. Until it has, [`open`] opens no block outside the range it
/// is given, so a write of ours that faults into the guards opens what
/// [`writing`] would have opened for it (`crate::host::copy_mapped`).
pub(crate) fn aliased_ever() -> bool {
    ALIASED_EVER.load(std::sync::atomic::Ordering::Relaxed)
}

static ALIASED_EVER: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Diagnostics: blocks guarded together with their other views, blocks
/// opened because another view of theirs was written, and blocks opened
/// because a view of theirs was mapped; since the start.
#[cfg(feature = "diagnostics")]
pub(crate) static ALIASES: [std::sync::atomic::AtomicU64; 3] = [const { std::sync::atomic::AtomicU64::new(0) }; 3];

/// Makes `block` read-only: another view of pages a copy has just taken
/// through its own view, so that a write through this one faults too. It is
/// not noted clean. The copies of title memory at this view have not taken
/// the pages: noted clean, the block would not be reported written at their
/// next check and they would keep the bytes from before the write (`guard`
/// reports a block that is read-only and not clean as written, then notes it
/// clean). A block that is clean stays so: had the pages been written since
/// its copies took them, `open` would have opened it.
fn protect_alias(g: &mut Guards, block: usize) {
    use windows_sys::Win32::System::Memory::{PAGE_READONLY, VirtualProtect};
    if g.protected.contains(&block) {
        return;
    }
    let mut old = 0;
    // SAFETY: title view memory; only its protection changes.
    if unsafe { VirtualProtect((block * BLOCK) as *const _, BLOCK, PAGE_READONLY, &mut old) } != 0 {
        g.protected.insert(block);
    }
}

/// [`protect`] for one range, without the other views of its pages.
fn protect_here(g: &mut Guards, from: usize, to: usize) {
    use windows_sys::Win32::System::Memory::{PAGE_READONLY, VirtualProtect};
    let set = |from: usize, to: usize| {
        #[cfg(feature = "diagnostics")]
        crate::copystat::add(crate::copystat::C::GuardProtects, 1);
        let mut old = 0;
        // SAFETY: title view memory; only its protection changes.
        unsafe {
            VirtualProtect(
                (from * BLOCK) as *const _,
                (to - from) * BLOCK,
                PAGE_READONLY,
                &mut old,
            ) != 0
        }
    };
    let note = |g: &mut Guards, from: usize, to: usize| {
        g.protected.extend(from..to);
        g.clean.extend(from..to);
        g.views.extend(from..to);
    };
    if set(from, to) {
        note(g, from, to);
        return;
    }
    #[cfg(feature = "diagnostics")]
    crate::copystat::add(crate::copystat::C::GuardProtectFails, 1);
    // Block by block; a block that is not committed answers for the whole
    // uncommitted run it begins (one question, no protection asked for the
    // rest: 77 runs a frame were refused whole in the world, with 10,264
    // questions to Windows and 1,120 protections a frame, run int-i-1).
    let mut block = from;
    while block < to {
        if set(block, block + 1) {
            note(g, block, block + 1);
            block += 1;
            continue;
        }
        #[cfg(feature = "diagnostics")]
        let error = std::io::Error::last_os_error();
        let end = uncommitted_end(block).min(to);
        if end > block {
            g.absent.extend(block..end);
            if holes_level() == 2 {
                // Nothing to copy there until it is told mapped or committed:
                // the asking's first look takes the range as clean.
                g.clean.extend(block..end);
            }
            block = end;
        } else {
            // Diagnostics: a committed block that will not be made
            // read-only is reported written at every asking and never
            // known to the guards (one in 1,024 said).
            #[cfg(feature = "diagnostics")]
            {
                static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                if N.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % 1024 == 0 {
                    tracing::info!("a block of title memory could not be made read-only (one in 1024): {:#x}: {error}", block * BLOCK);
                }
            }
            block += 1;
        }
    }
}

/// The end (exclusive) of the whole uncommitted blocks starting at `block`.
fn uncommitted_end(block: usize) -> usize {
    use windows_sys::Win32::System::Memory::{
        MEM_FREE, MEM_RESERVE, MEMORY_BASIC_INFORMATION, VirtualQuery,
    };
    // SAFETY: plain data filled in by the query.
    let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: a valid out pointer of the right size.
    let n = unsafe { VirtualQuery((block * BLOCK) as *const _, &mut info, size_of_val(&info)) };
    #[cfg(feature = "diagnostics")]
    crate::copystat::add(crate::copystat::C::GuardQueries, 1);
    if n != 0 && matches!(info.State, MEM_RESERVE | MEM_FREE) {
        (info.BaseAddress as usize).saturating_add(info.RegionSize) / BLOCK
    } else {
        block
    }
}

/// How long what Windows said of a run of uncommitted blocks holds
/// (`VICTORY_GUARD_HOLES_FRAME`): 0 (unset) every asking questions Windows
/// again; 1 for the rest of the frame; 2 until the runtime tells of a
/// mapping or a commit there.
///
/// The title's streamed buffers lie over placeholders with holes, and a
/// submission's askings walk them: 10,264 questions a frame in the world,
/// 7.5 ms of the submitting thread (run int-i-1), nearly all with the
/// answer of the asking before; with the title's small buffers on the card
/// they were the largest single cost of that thread (8.1 ms a frame of
/// `NtQueryVirtualMemory`, run int-m-kb).
///
/// shortcut (1): a block the title commits by an ordinary commit (no word
/// from the runtime) after the frame's first question about it is copied
/// as zeros until the next frame begins.
///
/// shortcut (2): a hole found holds, with no question and no walk of its
/// blocks (they count as clean), until the runtime tells of a mapping
/// ([`title_memory_remapped`]) or of a commit ([`title_pages_changed`]: the
/// title's `VirtualAlloc` and `XMemVirtualAlloc`, and our own allocations
/// for it), or until it is due a look again ([`holes_due`]). Memory
/// committed in a way the runtime does not tell of is copied as zeros
/// until then, and is logged when the look finds it. In the world: 443
/// questions a frame, none of the holes looked at again found committed
/// (run st-q-kv2), against 12,210 questions a frame (run st-o-kv).
fn holes_level() -> u8 {
    static LEVEL: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *LEVEL.get_or_init(|| {
        std::env::var("VICTORY_GUARD_HOLES_FRAME").ok().and_then(|v| v.trim().parse().ok()).filter(|&v| v <= 2).unwrap_or(0)
    })
}

/// Ranges told by [`title_pages_changed`] and not applied yet: the next
/// asking takes them, so the title's threads never wait for the guards.
static CHANGED: Mutex<Vec<(usize, usize)>> = Mutex::new(Vec::new());
static CHANGED_ANY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether the runtime should tell of ordinary commits and frees
/// ([`holes_level`] 2).
pub fn title_pages_told() -> bool {
    holes_level() == 2
}

/// The title's memory `[at, at + len)` was committed, decommitted or
/// released by an ordinary call (no mapping changed): with
/// [`holes_level`] 2, the blocks noted absent there are looked at again.
/// Nothing to do otherwise.
pub fn title_pages_changed(at: usize, len: usize) {
    if !title_pages_told() || len == 0 {
        return;
    }
    CHANGED.lock().unwrap_or_else(|e| e.into_inner()).push((at, len));
    CHANGED_ANY.store(true, std::sync::atomic::Ordering::Release);
}

/// Applies what [`title_pages_changed`] was told since the last asking.
fn apply_changed(g: &mut Guards) {
    if !CHANGED_ANY.swap(false, std::sync::atomic::Ordering::AcqRel) {
        return;
    }
    let changed = std::mem::take(&mut *CHANGED.lock().unwrap_or_else(|e| e.into_inner()));
    for (at, len) in changed {
        let absent: Vec<usize> = g.absent.range(at / BLOCK..at.saturating_add(len).div_ceil(BLOCK)).collect();
        for block in absent {
            g.absent.remove(block);
            g.clean.remove(block);
        }
    }
}

/// With [`holes_level`] 2: every how many frames a hole is looked at again
/// (`VICTORY_GUARD_HOLES_RECHECK`, 16 unset; 0 never).
fn holes_recheck() -> usize {
    static EVERY: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *EVERY.get_or_init(|| {
        std::env::var("VICTORY_GUARD_HOLES_RECHECK").ok().and_then(|v| v.trim().parse().ok()).unwrap_or(16)
    })
}

/// With [`holes_level`] 2, once a frame: a share of the holes (their words
/// of blocks by number, one share in [`holes_recheck`] each frame) is no
/// longer clean, so the askings of this frame and later question Windows
/// about them again, each once.
fn holes_due(g: &mut Guards) {
    if holes_level() != 2 {
        return;
    }
    let every = holes_recheck();
    let frame = crate::queue::presented();
    if every == 0 || g.due_frame == frame {
        return;
    }
    g.due_frame = frame;
    let turn = frame as usize % every;
    let Guards { absent, clean, .. } = g;
    for (&word, &bits) in &absent.0 {
        if word % every == turn {
            clean.clear(word, bits);
        }
    }
}

/// Every how many frames a guarded block is looked at again though no
/// write to it was seen (`VICTORY_VIEW_RECHECK`, in frames; unset or 0:
/// never).
///
/// A write through another view of a block's pages does not fault at the
/// guarded one. [`view_guard`] has every view guarded so that it does,
/// which cost 30 ms a frame in the world (run st-u-vg: 96-102 ms a frame
/// where st-u-wc without it made 68-73). Without it the copy at the
/// guarded view keeps its bytes for good. This bounds for how long: each
/// block is reported written once in so many frames, and its copies read
/// it again (with `VICTORY_REFRESH_COMPARE=1` they take only what
/// differs; without it they take the block whole).
fn views_recheck() -> usize {
    static EVERY: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *EVERY.get_or_init(|| std::env::var("VICTORY_VIEW_RECHECK").ok().and_then(|v| v.trim().parse().ok()).unwrap_or(0))
}

/// Diagnostics: clean blocks made due by [`views_due`] since the last
/// frame-rate line.
#[cfg(feature = "diagnostics")]
pub(crate) static DUE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// With [`views_recheck`], once a frame: a share of the clean guarded
/// blocks is no longer clean, so the next asking about them reports them
/// written. They stay read-only: `guard` notes them clean again with no
/// call to Windows.
fn views_due(g: &mut Guards) {
    let every = views_recheck();
    let frame = crate::queue::presented();
    if every == 0 || g.views_frame == frame {
        return;
    }
    g.views_frame = frame;
    for (word, bits) in due_bits(&g.clean, &g.absent, every, frame as usize % every) {
        #[cfg(feature = "diagnostics")]
        DUE.fetch_add(u64::from(bits.count_ones()), std::sync::atomic::Ordering::Relaxed);
        g.clean.clear(word, bits);
    }
}

/// The share of `clean` that [`views_due`] makes due at `turn` of `every`:
/// the words of blocks whose number is `turn` modulo `every`, with their
/// clean bits, without the holes (`absent`: those are [`holes_due`]'s,
/// and making one due questions Windows).
fn due_bits(clean: &Blocks, absent: &Blocks, every: usize, turn: usize) -> Vec<(usize, u64)> {
    clean
        .0
        .iter()
        .filter(|&(&word, _)| word % every == turn)
        .map(|(&word, &bits)| (word, bits & !absent.0.get(&word).copied().unwrap_or(0)))
        .filter(|&(_, bits)| bits != 0)
        .collect()
}

/// A hole that was due a look is committed or mapped, and the runtime had
/// not told ([`holes_level`] 2): counted, the first ones named with what
/// Windows says of them (which way of committing the runtime misses).
fn untold(block: usize) {
    use windows_sys::Win32::System::Memory::{MEMORY_BASIC_INFORMATION, VirtualQuery};
    static SEEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seen = SEEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    if seen > 48 && !seen.is_power_of_two() {
        return;
    }
    // SAFETY: plain data filled in by the query.
    let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: a valid out pointer of the right size.
    unsafe { VirtualQuery((block * BLOCK) as *const _, &mut info, size_of_val(&info)) };
    tracing::warn!(
        address = format_args!("{:#x}", block * BLOCK),
        state = format_args!("{:#x}", info.State),
        kind = format_args!("{:#x}", info.Type),
        protect = format_args!("{:#x}", info.Protect),
        allocation = format_args!("{:p}", info.AllocationBase),
        region = info.RegionSize,
        seen,
        "a hole was committed or mapped with no word from the runtime"
    );
}

/// [`uncommitted_end`] of an absent `block`, from this frame's answers
/// with [`holes_level`] 1 and 2.
fn hole_end(g: &mut Guards, block: usize) -> usize {
    if holes_level() == 0 {
        return uncommitted_end(block);
    }
    let frame = crate::queue::presented();
    if g.holes_frame != frame {
        g.holes.clear();
        g.holes_frame = frame;
    }
    if let Some((_, &end)) = g.holes.range(..=block).next_back()
        && block < end
    {
        return end;
    }
    let end = uncommitted_end(block);
    if end > block {
        g.holes.insert(block, end);
    }
    end
}

/// Gives write access back to the guarded blocks of `[start, end)` and
/// notes them written; true if any was guarded. The same physical pages at
/// every other view of them are opened too: the write that faulted (or is
/// ours, `crate::host::copy_mapped`) goes through one view, and every
/// guard over the pages must copy them again.
fn open(g: &mut Guards, start: usize, end: usize) -> bool {
    let any = open_here(g, start, end);
    if start >= end || g.aliased.is_empty() {
        return any;
    }
    let aliased: Vec<usize> = g.aliased.range(start / BLOCK..end.div_ceil(BLOCK)).copied().collect();
    for block in aliased {
        g.aliased.remove(&block);
        for alias in crate::inplace::pages_of(block * BLOCK) {
            if alias / BLOCK != block && open_here(g, alias, alias.saturating_add(BLOCK)) {
                #[cfg(feature = "diagnostics")]
                ALIASES[1].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            g.aliased.remove(&(alias / BLOCK));
        }
    }
    any
}

/// [`open`] for one range, without the other views of its pages.
fn open_here(g: &mut Guards, start: usize, end: usize) -> bool {
    use windows_sys::Win32::System::Memory::{PAGE_READWRITE, VirtualProtect};
    let blocks: Vec<usize> = g
        .protected
        .range(start / BLOCK..end.div_ceil(BLOCK))
        .copied()
        .collect();
    for &block in &blocks {
        g.protected.remove(&block);
        g.clean.remove(block);
        let mut old = 0;
        // SAFETY: a block we made read-only.
        unsafe { VirtualProtect((block * BLOCK) as *const _, BLOCK, PAGE_READWRITE, &mut old) };
    }
    !blocks.is_empty()
}

/// Before our own write into title memory `[at, at + len)` through the
/// kernel (which does not fault into our handler): opens its guarded blocks.
/// The runtime calls it (as `kernel_will_write`) before the title's kernel
/// reads into its own memory.
pub fn writing(at: usize, len: usize) {
    // The length is the title's (`fread`'s size times count, saturated).
    open(&mut guards(), at, at.saturating_add(len));
    // A placed texture there changes with no descriptor written.
    crate::descriptor::host::remapped(at, len);
}

/// What `VICTORY_PAGE_WRITES` asks to be done with a write the guards do
/// not see once its bytes are there ([`title_memory_written`]), as bits:
/// 1, the guarded blocks under it are opened and count as written (host
/// copies of buffers and textures read through tables take them again);
/// 2, the placed textures picked by index that lie under it are due again;
/// 4, both also for reads to plain addresses when they have finished.
/// Unset or `1`: bits 1 and 2. `guards`: bit 1 alone (the default until
/// the owner's session of 25 November). `0`: nothing, counted only (the
/// default until run 199). `slots`: bit 2 alone. `all`: everything.
/// (Bit 1: in every run 10,000 to 50,000 guarded blocks of host copies lay
/// under such writes and kept the bytes from before; runs 197 and 198
/// opened them, 9,766 and 10,796 blocks, with nothing worse for it. Bit 2:
/// the owner's 33-minute session had 64,113 slots uploaded before such a
/// write and left as they were; runs pw1 and ts40 (330 s each, bits 1 and
/// 2, slots due two frames on) had no fault. How a slot is made due:
/// `crate::descriptor::host::page_settle`.)
pub(crate) fn page_writes() -> u8 {
    static MODE: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *MODE.get_or_init(|| match std::env::var("VICTORY_PAGE_WRITES").as_deref().map(str::trim) {
        Ok("0") => 0,
        Ok("guards") => 1,
        Ok("slots") => 2,
        Ok("all") => 7,
        _ => 3,
    })
}

/// Since the start: writes into physical pages through the runtime's own
/// views that landed at addresses the title has mapped (one for each run
/// of addresses); their bytes; guarded blocks that lay under them (host
/// copies that would otherwise keep the bytes from before); reads to
/// plain addresses that finished.
pub(crate) static WRITTEN: [std::sync::atomic::AtomicU64; 4] = [const { std::sync::atomic::AtomicU64::new(0) }; 4];

/// The bytes of a write the guards do not see are there: title memory
/// `[at, at + len)`. `pages`: DirectStorage wrote them into physical pages
/// through a view of the runtime's own, and this is an address where the
/// title has those pages mapped (no title address faulted, nothing was
/// mapped or unmapped; the shortcut this file's header names). Otherwise a
/// read to a plain address has finished, of which [`writing`] was told
/// before it began.
///
/// Upstream's c75fdba does the same for its trackers (its `Writing` marks
/// the pages once more when dropped; page destinations are reported at
/// each address showing them). Here it is counted always and acted on as
/// [`page_writes`] says. Called without the runtime's page-table lock;
/// takes the guards' lock, then, alone, the lock of the ranges kept for
/// the texture pass.
pub fn title_memory_written(at: usize, len: usize, pages: bool) {
    use std::sync::atomic::Ordering::Relaxed;
    if len == 0 {
        return;
    }
    let mode = page_writes();
    let end = at.saturating_add(len);
    if pages {
        WRITTEN[0].fetch_add(1, Relaxed);
        WRITTEN[1].fetch_add(len as u64, Relaxed);
    } else {
        WRITTEN[3].fetch_add(1, Relaxed);
        if mode & 4 == 0 {
            return;
        }
    }
    {
        let mut g = guards();
        let guarded = g.protected.range(at / BLOCK..end.div_ceil(BLOCK)).count() as u64;
        if pages {
            WRITTEN[2].fetch_add(guarded, Relaxed);
        }
        if mode & 1 != 0 && guarded != 0 {
            open(&mut g, at, end);
        }
    }
    // A placed texture there changed with no descriptor written.
    crate::descriptor::host::page_written(at, len);
}

/// The runtime mapped a view of the physical bytes `[offset, offset + len)`
/// of `section` (`crate::inplace::title_memory_view`): the view is
/// writable, so the same bytes guarded at another view must be checked
/// again — and are, the pages noted written, which also has the next check
/// guard the new view (`protect`).
pub(crate) fn views_mapped(section: usize, offset: u64, len: usize) {
    if !view_guard() {
        return;
    }
    let ranges = crate::inplace::views_of_section(section, offset, len);
    if ranges.is_empty() {
        return;
    }
    let mut g = guards();
    for &(at, n) in &ranges {
        if open_here(&mut g, at, at.saturating_add(n)) {
            #[cfg(feature = "diagnostics")]
            ALIASES[2].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

/// Held shared by a read of title memory in place, and alone by the runtime
/// while it changes what title memory shows.
///
/// The runtime does not change a mapping in one step. To unmap one page of
/// a view it unmaps the whole view and maps what stays again
/// (`xmem.rs` `unmap`), and a view it maps is a reservation until the next
/// call commits it (`map`, then `commit`). For that moment pages the title
/// never gave up are "reserved, never committed", and a thread reading
/// them in place takes an access violation: run 71 in `Layout::detile`,
/// run 74 in the hash before it, both under `Staging::refresh_now`, which
/// had asked whether the texture was all mapped and been told yes. Asking
/// cannot help; not changing the mapping while such a read is under way
/// does.
static MAPPING: std::sync::RwLock<()> = std::sync::RwLock::new(());

/// What the runtime holds while it changes what title memory shows
/// ([`title_memory_remapping`]).
pub struct Remapping(#[allow(dead_code)] std::sync::RwLockWriteGuard<'static, ()>, std::time::Instant);

impl Drop for Remapping {
    fn drop(&mut self) {
        // From asking for the lock until it is let go (the frame log).
        crate::framelog::add(crate::framelog::T::RemapUs, self.1.elapsed().as_micros() as u64);
    }
}

/// The runtime is about to map, unmap or move physical pages of title
/// memory: until what it returns is dropped, no read of title memory in
/// place is under way and none begins ([`title_memory_reading`]). Taken
/// around the whole change, the unmapping and the mapping again.
pub fn title_memory_remapping() -> Remapping {
    let asked = std::time::Instant::now();
    let held = MAPPING.write().unwrap_or_else(|e| e.into_inner());
    crate::framelog::add(crate::framelog::T::Remaps, 1);
    crate::framelog::add(crate::framelog::T::RemapWaitUs, asked.elapsed().as_micros() as u64);
    Remapping(held, asked)
}

/// What a read of title memory in place holds ([`title_memory_reading`]).
pub struct Reading(#[allow(dead_code)] std::sync::RwLockReadGuard<'static, ()>);

/// For reading title memory in place: until what it returns is dropped,
/// the runtime changes no mapping, so memory found mapped stays mapped.
/// Not to be taken twice by one thread, nor held across anything that
/// maps title memory.
pub fn title_memory_reading() -> Reading {
    Reading(MAPPING.read().unwrap_or_else(|e| e.into_inner()))
}

/// [`title_memory_reading`] if it can be had at once: `None` while the
/// runtime changes a mapping (or waits to), for a caller that has another
/// way to read and would rather not wait.
pub fn title_memory_reading_now() -> Option<Reading> {
    match MAPPING.try_read() {
        Ok(guard) => Some(Reading(guard)),
        Err(std::sync::TryLockError::Poisoned(e)) => Some(Reading(e.into_inner())),
        Err(std::sync::TryLockError::WouldBlock) => None,
    }
}

/// The runtime mapped or unmapped title memory `[at, at + len)`: what it
/// shows is new and read-write, so it is no longer guarded (nor known
/// unmapped).
pub fn title_memory_remapped(at: usize, len: usize) {
    // A placed texture there arrives, goes or changes with no descriptor
    // written.
    crate::descriptor::host::remapped(at, len);
    // Textures already uploaded from there must be uploaded again, or the
    // next list that samples them reads their old contents.
    crate::texture::remapped(at, len);
    // The views the runtime told us of there are no longer what it shows.
    crate::inplace::views_gone(at, len);
    // And nothing stays in place over it.
    crate::inplace::title_memory_freeing(at, len);
    let mut g = guards();
    g.holes.clear();
    let range = at / BLOCK..at.saturating_add(len).div_ceil(BLOCK);
    let absent: Vec<usize> = g.absent.range(range.clone()).collect();
    for block in absent {
        g.absent.remove(block);
        g.clean.remove(block);
    }
    let blocks: Vec<usize> = g.views.range(range.clone()).copied().collect();
    for block in blocks {
        g.views.remove(&block);
        g.protected.remove(&block);
        g.clean.remove(block);
    }
    // Blocks made read-only as another view of a guarded page
    // (`protect_alias`) are in no list of views: left noted read-only,
    // whatever is mapped there next would be reported written once and
    // then never again, with no protection asked for it.
    let guarded: Vec<usize> = g.protected.range(range).copied().collect();
    for block in guarded {
        g.protected.remove(&block);
        g.clean.remove(block);
    }
    let aliased: Vec<usize> = g.aliased.range(at / BLOCK..at.saturating_add(len).div_ceil(BLOCK)).copied().collect();
    for block in aliased {
        g.aliased.remove(&block);
    }
    if view_known() {
        // The runtime calls this for two things only (`xmem`: `map` and
        // `unmap`): a range it has just mapped a section's pages into,
        // and a view it is about to make a placeholder again. Either way
        // what stands there is view memory or a hole in it, which is what
        // `is_view` would answer for a range that begins there; and the
        // write watch's asking can only fail there. The title remaps some
        // 83 times a frame, and each time the blocks were forgotten, so
        // that the next asking about each range went to the kernel twice
        // (run 235: 266 to 565 failed askings a frame with the blocks
        // remembered from their guards alone, no fewer than without).
        g.views.extend(at / BLOCK..at.saturating_add(len).div_ceil(BLOCK));
    }
}

/// Handles a write access violation at `at` if the block belongs to our guards
/// or became writable concurrently.
pub(crate) fn handle_write_fault(at: usize) -> bool {
    let mut g = guards();
    if open(&mut g, at, at + 1) {
        #[cfg(feature = "diagnostics")]
        crate::copystat::add(crate::copystat::C::GuardFaults, 1);
        crate::framelog::add(crate::framelog::T::GuardFaults, 1);
        return true;
    }
    writable(at)
}

/// Registers the handler that takes writes to guarded blocks, once, ahead
/// of the crash reporter's.
fn install() {
    use windows_sys::Win32::Foundation::{EXCEPTION_ACCESS_VIOLATION, NTSTATUS};
    use windows_sys::Win32::System::Diagnostics::Debug::{
        AddVectoredExceptionHandler, EXCEPTION_CONTINUE_EXECUTION, EXCEPTION_CONTINUE_SEARCH,
        EXCEPTION_POINTERS,
    };
    unsafe extern "system" fn on_write(info: *mut EXCEPTION_POINTERS) -> i32 {
        // SAFETY: the system passes the exception being dispatched.
        let r = unsafe { &*(*info).ExceptionRecord };
        const WRITE: usize = 1;
        if r.ExceptionCode != EXCEPTION_ACCESS_VIOLATION as NTSTATUS
            || r.NumberParameters < 2
            || r.ExceptionInformation[0] != WRITE
        {
            return EXCEPTION_CONTINUE_SEARCH;
        }
        let at = r.ExceptionInformation[1];
        // Held across both checks: another thread may have opened the block
        // since this one faulted, and a copy may have guarded it again since
        // (then it is ours again, not read-only for some other reason).
        if handle_write_fault(at) {
            EXCEPTION_CONTINUE_EXECUTION
        } else {
            EXCEPTION_CONTINUE_SEARCH
        }
    }
    static ONCE: std::sync::Once = std::sync::Once::new();
    // SAFETY: the handler only touches our guard state and protections.
    ONCE.call_once(|| unsafe {
        AddVectoredExceptionHandler(1, Some(on_write));
    });
}

/// Whether `at` is committed read-write memory now.
fn writable(at: usize) -> bool {
    use windows_sys::Win32::System::Memory::{
        MEM_COMMIT, MEMORY_BASIC_INFORMATION, PAGE_READWRITE, VirtualQuery,
    };
    // SAFETY: plain data filled in by the query.
    let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: a valid out pointer of the right size.
    let n = unsafe { VirtualQuery(at as *const _, &mut info, size_of_val(&info)) };
    n != 0 && info.State == MEM_COMMIT && info.Protect == PAGE_READWRITE
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::System::Memory::{
        MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE, VirtualAlloc, VirtualFree,
    };
    const MEM_WRITE_WATCH: u32 = 0x20_0000;

    /// The guards asked as [`writes_into`] asks them: with what
    /// [`known_view`] says of the range's first block.
    fn guard(start: usize, end: usize) -> Option<Vec<usize>> {
        super::guard(start, end, known_view(start / BLOCK))
    }

    #[test]
    fn written_pages_are_reported_to_every_asker() {
        // SAFETY: a fresh write-watched allocation, freed at the end.
        let p = unsafe {
            VirtualAlloc(
                std::ptr::null(),
                4 * PAGE,
                MEM_RESERVE | MEM_COMMIT | MEM_WRITE_WATCH,
                PAGE_READWRITE,
            )
        } as usize;
        assert_ne!(p, 0);
        let (start, end) = (p, p + 4 * PAGE);
        let (_, before) = writes(start, end, false).expect("watched");
        assert!(before.is_empty());
        // SAFETY: committed pages of ours.
        unsafe { ((p + PAGE + 8) as *mut u8).write(1) };
        let (now, after) = writes(start, end, false).expect("watched");
        assert_eq!(after, [(p / PAGE + 1, now)]);
        // Another asker after the kernel's bit was cleared still sees it.
        let (later, again) = writes(p + PAGE, p + 2 * PAGE, false).expect("watched");
        assert!(later > now);
        assert_eq!(again, [(p / PAGE + 1, now)]);
        let mut plain = vec![0u8; PAGE];
        assert!(
            writes(
                plain.as_mut_ptr() as usize,
                plain.as_ptr() as usize + PAGE,
                true
            )
            .is_none()
        );
        // SAFETY: our allocation.
        unsafe { VirtualFree(p as *mut _, 0, MEM_RELEASE) };
    }

    /// A kernel read into a guarded view block fails; opened first, it
    /// lands and the block counts as written.
    #[test]
    fn kernel_reads_into_guarded_blocks_after_opening() {
        use std::io::{Read, Seek, Write};
        use windows_sys::Win32::System::Memory::{
            CreateFileMappingW, FILE_MAP_WRITE, MapViewOfFile, UnmapViewOfFile,
        };
        // SAFETY: a pagefile section of two blocks, mapped read-write.
        let view = unsafe {
            let section = CreateFileMappingW(
                -1isize as _,
                std::ptr::null(),
                PAGE_READWRITE,
                0,
                (2 * BLOCK) as u32,
                std::ptr::null(),
            );
            assert!(!section.is_null());
            MapViewOfFile(section, FILE_MAP_WRITE, 0, 0, 2 * BLOCK).Value as usize
        };
        assert_ne!(view, 0);
        assert!(guard(view, view + 2 * BLOCK).is_some()); // now read-only
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(b"from the disk").unwrap();
        // `ReadFile` inside: the kernel writes the view.
        let read = |file: &mut std::fs::File| {
            file.rewind().unwrap();
            // SAFETY: 13 bytes of our view, nothing else refers to them.
            let into = unsafe { std::slice::from_raw_parts_mut((view + 100) as *mut u8, 13) };
            file.read(into).is_ok_and(|n| n == 13)
        };
        assert!(
            !read(&mut file),
            "the kernel does not fault into our handler"
        );
        writing(view + 100, 13);
        assert!(read(&mut file));
        // SAFETY: the bytes just read.
        let landed = unsafe { std::slice::from_raw_parts((view + 100) as *const u8, 13) };
        assert_eq!(landed, b"from the disk");
        assert_eq!(
            guard(view, view + 2 * BLOCK),
            Some((view / PAGE..view / PAGE + 1).collect())
        );
        // SAFETY: our view.
        unsafe {
            UnmapViewOfFile(
                windows_sys::Win32::System::Memory::MEMORY_MAPPED_VIEW_ADDRESS {
                    Value: view as *mut _,
                },
            )
        };
        title_memory_remapped(view, 2 * BLOCK);
    }

    #[test]
    fn writes_in_partly_committed_view_blocks_are_reported() {
        use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
        use windows_sys::Win32::System::Memory::{
            CreateFileMappingW, FILE_MAP_WRITE, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile,
            SEC_RESERVE, UnmapViewOfFile,
        };

        const SIZE: usize = 3 * BLOCK;
        struct View(MEMORY_MAPPED_VIEW_ADDRESS);
        impl Drop for View {
            fn drop(&mut self) {
                title_memory_remapped(self.0.Value as usize, SIZE);
                // SAFETY: our view is still mapped.
                unsafe { UnmapViewOfFile(self.0) };
            }
        }
        // SAFETY: a new reserved pagefile section. The view holds its own
        // reference after we close the handle, including on assertion failure.
        let view = unsafe {
            let section = CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                std::ptr::null(),
                PAGE_READWRITE | SEC_RESERVE,
                0,
                SIZE as u32,
                std::ptr::null(),
            );
            assert!(!section.is_null());
            let view = MapViewOfFile(section, FILE_MAP_WRITE, 0, 0, SIZE);
            CloseHandle(section);
            assert!(!view.Value.is_null());
            View(view)
        };
        let start = view.0.Value as usize;
        let commit = |at: usize, len| {
            // SAFETY: these pages belong to our reserved view.
            let committed = unsafe { VirtualAlloc(at as _, len, MEM_COMMIT, PAGE_READWRITE) };
            assert_eq!(committed as usize, at);
        };
        let early = start + BLOCK + 3 * KERNEL_PAGE;
        let late = start + 2 * BLOCK + 3 * KERNEL_PAGE;
        commit(start, BLOCK);
        commit(early, KERNEL_PAGE);
        let refresh = || guard(start, start + SIZE).unwrap();
        assert_eq!(refresh(), [start / PAGE, early / PAGE, late / PAGE]);
        // The last block is still entirely reserved.
        assert!(!refresh().contains(&(late / PAGE)));

        commit(late, KERNEL_PAGE); // Ordinary commit, without a runtime mapping notification.
        for at in [early, late] {
            assert!(writable(at));
            // SAFETY: each address is inside its committed read-write page.
            unsafe { (at as *mut u8).write_volatile(1) };
        }
        assert_eq!(refresh(), [early / PAGE, late / PAGE]);
    }

    /// A range of view memory whose first block is not mapped is a view
    /// all the same: its blocks are reported once, then the ones written
    /// or mapped since.
    #[test]
    fn a_range_that_begins_in_an_unmapped_block_is_guarded() {
        use windows_sys::Win32::System::Memory::{
            CreateFileMappingW, MEM_PRESERVE_PLACEHOLDER, MEM_REPLACE_PLACEHOLDER,
            MEM_RESERVE_PLACEHOLDER, MapViewOfFile3, PAGE_NOACCESS, UnmapViewOfFile2,
            VirtualAlloc2,
        };
        use windows_sys::Win32::System::Threading::GetCurrentProcess;
        // SAFETY: a fresh placeholder of three blocks and a pagefile
        // section of three; the middle block is mapped first.
        let (p, section) = unsafe {
            let p = VirtualAlloc2(
                GetCurrentProcess(),
                std::ptr::null(),
                3 * BLOCK,
                MEM_RESERVE | MEM_RESERVE_PLACEHOLDER,
                PAGE_NOACCESS,
                std::ptr::null_mut(),
                0,
            ) as usize;
            assert_ne!(p, 0);
            let section = CreateFileMappingW(
                -1isize as _,
                std::ptr::null(),
                PAGE_READWRITE,
                0,
                (3 * BLOCK) as u32,
                std::ptr::null(),
            );
            assert!(!section.is_null());
            (p, section)
        };
        let map = |block: usize| {
            // SAFETY: splits the placeholder at the block and maps the
            // section's block there.
            let view = unsafe {
                VirtualFree(
                    (p + block * BLOCK) as *mut _,
                    BLOCK,
                    MEM_RELEASE | MEM_PRESERVE_PLACEHOLDER,
                );
                MapViewOfFile3(
                    section,
                    GetCurrentProcess(),
                    (p + block * BLOCK) as *const _,
                    (block * BLOCK) as u64,
                    BLOCK,
                    MEM_REPLACE_PLACEHOLDER,
                    PAGE_READWRITE,
                    std::ptr::null_mut(),
                    0,
                )
            };
            assert_eq!(view.Value as usize, p + block * BLOCK);
            view
        };
        let pages = |block: usize| (p + block * BLOCK) / PAGE..(p + (block + 1) * BLOCK) / PAGE;
        let all: Vec<usize> = (0..3).flat_map(pages).collect();
        // Nothing mapped at all: a view with every block absent.
        assert!(is_view(p, p + 3 * BLOCK));
        let middle = map(1);
        title_memory_remapped(p + BLOCK, BLOCK);
        // The range begins in the placeholder, its middle block is a view.
        assert!(is_view(p, p + 3 * BLOCK));
        assert_eq!(guard(p, p + 3 * BLOCK), Some(all));
        assert_eq!(guard(p, p + 3 * BLOCK), Some(Vec::new()), "nothing written since");
        // SAFETY: inside the mapped middle block.
        unsafe { ((p + BLOCK + 8) as *mut u8).write_volatile(1) };
        assert_eq!(guard(p, p + 3 * BLOCK), Some(pages(1).collect()));
        let first = map(0);
        title_memory_remapped(p, BLOCK);
        assert_eq!(guard(p, p + 3 * BLOCK), Some(pages(0).collect()));
        assert_eq!(guard(p, p + 3 * BLOCK), Some(Vec::new()));
        // Memory of ours that is no view stays none.
        let mut local = [0u8; 16];
        let at = local.as_mut_ptr() as usize;
        assert!(!is_view(at, at + local.len()));
        // SAFETY: our views and placeholder.
        unsafe {
            UnmapViewOfFile2(GetCurrentProcess(), first, MEM_PRESERVE_PLACEHOLDER);
            UnmapViewOfFile2(GetCurrentProcess(), middle, MEM_PRESERVE_PLACEHOLDER);
            for block in 0..3 {
                VirtualFree((p + block * BLOCK) as *mut _, 0, MEM_RELEASE);
            }
            windows_sys::Win32::Foundation::CloseHandle(section);
        }
        title_memory_remapped(p, 3 * BLOCK);
    }

    /// Blocks of a placeholder between views are reported once, then not
    /// again (they cannot be guarded) until the runtime maps them.
    #[test]
    fn unmapped_blocks_are_reported_once_until_mapped() {
        use windows_sys::Win32::System::Memory::{
            CreateFileMappingW, MEM_PRESERVE_PLACEHOLDER, MEM_REPLACE_PLACEHOLDER,
            MEM_RESERVE_PLACEHOLDER, MapViewOfFile3, PAGE_NOACCESS, UnmapViewOfFile2,
            VirtualAlloc2,
        };
        use windows_sys::Win32::System::Threading::GetCurrentProcess;
        // SAFETY: a fresh placeholder of three blocks and a pagefile
        // section of two; block 0 shows the section's first block, as the
        // runtime maps physical pages.
        let (p, section) = unsafe {
            let p = VirtualAlloc2(
                GetCurrentProcess(),
                std::ptr::null(),
                3 * BLOCK,
                MEM_RESERVE | MEM_RESERVE_PLACEHOLDER,
                PAGE_NOACCESS,
                std::ptr::null_mut(),
                0,
            ) as usize;
            assert_ne!(p, 0);
            let section = CreateFileMappingW(
                -1isize as _,
                std::ptr::null(),
                PAGE_READWRITE,
                0,
                (2 * BLOCK) as u32,
                std::ptr::null(),
            );
            assert!(!section.is_null());
            (p, section)
        };
        let map = |block: usize| {
            // SAFETY: splits the placeholder at the block and maps the
            // section's block there.
            let view = unsafe {
                VirtualFree(
                    (p + block * BLOCK) as *mut _,
                    BLOCK,
                    MEM_RELEASE | MEM_PRESERVE_PLACEHOLDER,
                );
                MapViewOfFile3(
                    section,
                    GetCurrentProcess(),
                    (p + block * BLOCK) as *const _,
                    (block * BLOCK) as u64,
                    BLOCK,
                    MEM_REPLACE_PLACEHOLDER,
                    PAGE_READWRITE,
                    std::ptr::null_mut(),
                    0,
                )
            };
            assert_eq!(view.Value as usize, p + block * BLOCK);
            view
        };
        let first = map(0);
        let pages = |block: usize| (p + block * BLOCK) / PAGE..(p + (block + 1) * BLOCK) / PAGE;
        let all: Vec<usize> = (0..3).flat_map(pages).collect();
        assert_eq!(guard(p, p + 3 * BLOCK), Some(all));
        assert_eq!(
            guard(p, p + 3 * BLOCK),
            Some(Vec::new()),
            "nothing written since"
        );
        let second = map(1);
        title_memory_remapped(p + BLOCK, BLOCK);
        assert_eq!(guard(p, p + 3 * BLOCK), Some(pages(1).collect()));
        assert_eq!(guard(p, p + 3 * BLOCK), Some(Vec::new()));
        // SAFETY: our views and placeholder.
        unsafe {
            UnmapViewOfFile2(GetCurrentProcess(), first, MEM_PRESERVE_PLACEHOLDER);
            UnmapViewOfFile2(GetCurrentProcess(), second, MEM_PRESERVE_PLACEHOLDER);
            for block in 0..3 {
                VirtualFree((p + block * BLOCK) as *mut _, 0, MEM_RELEASE);
            }
            windows_sys::Win32::Foundation::CloseHandle(section);
        }
        title_memory_remapped(p, 3 * BLOCK);
    }

    /// A pagefile section mapped twice: two views of the same physical
    /// pages, the second left for the test; the section handle, the two
    /// addresses.
    fn two_views(bytes: usize) -> (usize, usize, usize) {
        use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
        use windows_sys::Win32::System::Memory::{
            CreateFileMappingW, FILE_MAP_WRITE, MapViewOfFile,
        };
        // SAFETY: a fresh pagefile section, mapped twice; the views keep
        // the section alive and the handle is closed by the caller.
        unsafe {
            let section = CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                std::ptr::null(),
                PAGE_READWRITE,
                0,
                bytes as u32,
                std::ptr::null(),
            );
            assert!(!section.is_null());
            let one = MapViewOfFile(section, FILE_MAP_WRITE, 0, 0, bytes).Value as usize;
            let two = MapViewOfFile(section, FILE_MAP_WRITE, 0, 0, bytes).Value as usize;
            assert_ne!(one, 0);
            assert_ne!(two, 0);
            crate::inplace::title_memory_view(one, bytes, section as usize, 0);
            crate::inplace::title_memory_view(two, bytes, section as usize, 0);
            (section as usize, one, two)
        }
    }

    /// The section of [`two_views`] is closed and its views unmapped; the
    /// guard is told they are gone first.
    fn drop_views(section: usize, views: &[usize], bytes: usize) {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Memory::{MEMORY_MAPPED_VIEW_ADDRESS, UnmapViewOfFile};
        for &view in views {
            title_memory_remapped(view, bytes);
            // SAFETY: a view of the section mapped by `two_views`.
            unsafe {
                UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS {
                    Value: view as *mut _,
                })
            };
        }
        // SAFETY: the section's last reference.
        unsafe { CloseHandle(section as *mut std::ffi::c_void) };
    }

    /// The title wrote the pages of a guarded texture through another view
    /// of them: the fault at that view is the guard's to see, and the
    /// guarded view's next check reports the pages written.
    #[test]
    fn a_write_through_another_view_is_reported_at_the_guarded_one() {
        const SIZE: usize = 2 * BLOCK;
        let (section, guarded, other) = two_views(SIZE);
        let pages = |block: usize| (guarded + block * BLOCK) / PAGE;
        // The first check takes it in (and every view of its pages).
        assert_eq!(guard(guarded, guarded + SIZE), Some(vec![pages(0), pages(1)]));
        assert_eq!(guard(guarded, guarded + SIZE), Some(Vec::new()), "nothing written yet");
        // Writing through the other view faults into the handler, which
        // opens that view and the guarded one: the next check sees it.
        // SAFETY: the second view, read-write once the handler has opened it.
        unsafe { ((other + BLOCK + 8) as *mut u8).write(1) };
        assert_eq!(guard(guarded, guarded + SIZE), Some(vec![pages(1)]));
        assert_eq!(guard(guarded, guarded + SIZE), Some(Vec::new()));
        drop_views(section, &[guarded, other], SIZE);
    }

    /// A view of guarded pages is mapped anew: it is writable, so the
    /// guarded view checks its pages again (and guards the new view).
    #[test]
    fn a_view_mapped_beside_a_guarded_one_makes_it_check_again() {
        const SIZE: usize = 2 * BLOCK;
        let (section, guarded, other) = two_views(SIZE);
        let pages = |block: usize| (guarded + block * BLOCK) / PAGE;
        assert_eq!(guard(guarded, guarded + SIZE), Some(vec![pages(0), pages(1)]));
        assert_eq!(guard(guarded, guarded + SIZE), Some(Vec::new()), "nothing written yet");
        title_memory_remapped(other, SIZE);
        crate::inplace::title_memory_view(other, SIZE, section, 0);
        assert_eq!(guard(guarded, guarded + SIZE), Some(vec![pages(0), pages(1)]));
        assert_eq!(guard(guarded, guarded + SIZE), Some(Vec::new()));
        drop_views(section, &[guarded, other], SIZE);
    }

    /// A turn's share of the clean blocks is the words of its number, with
    /// the holes left out: making a hole due would question Windows.
    #[test]
    fn a_turn_takes_its_words_of_clean_blocks_without_the_holes() {
        let (mut clean, mut absent) = (Blocks::new(), Blocks::new());
        // Words 0, 1 and 3 hold clean blocks; block 65 (word 1) is a hole.
        for block in [0, 5, 64, 65, 70, 192] {
            clean.insert(block);
        }
        absent.insert(65);
        assert_eq!(due_bits(&clean, &absent, 2, 0), vec![(0, 1 | 1 << 5)]);
        assert_eq!(due_bits(&clean, &absent, 2, 1), vec![(1, 1 | 1 << 6), (3, 1)]);
        // A word of holes alone has no share.
        absent.insert(64);
        absent.insert(70);
        assert_eq!(due_bits(&clean, &absent, 2, 1), vec![(3, 1)]);
        // Every turn of a round together takes each clean block once.
        let taken: u32 = (0..7).flat_map(|turn| due_bits(&clean, &Blocks::new(), 7, turn)).map(|(_, bits)| bits.count_ones()).sum();
        assert_eq!(taken, 6);
    }

    /// A write of the runtime's own, told at one view of the pages
    /// (`writing`): the other views' next checks report the pages written.
    #[test]
    fn a_write_told_at_one_view_is_reported_at_the_others() {
        const SIZE: usize = 2 * BLOCK;
        let (section, guarded, other) = two_views(SIZE);
        let pages = |block: usize| (guarded + block * BLOCK) / PAGE;
        assert_eq!(guard(guarded, guarded + SIZE), Some(vec![pages(0), pages(1)]));
        assert_eq!(guard(guarded, guarded + SIZE), Some(Vec::new()), "nothing written yet");
        writing(other + BLOCK, BLOCK);
        assert_eq!(guard(guarded, guarded + SIZE), Some(vec![pages(1)]));
        assert_eq!(guard(guarded, guarded + SIZE), Some(Vec::new()));
        // Half a page and the next: both blocks, not just the first's.
        writing(other + BLOCK / 2, BLOCK);
        assert_eq!(guard(guarded, guarded + SIZE), Some(vec![pages(0), pages(1)]));
        drop_views(section, &[guarded, other], SIZE);
    }

    /// Both views of the pages are checked, each for a copy of its own
    /// (the title shows one physical page at several addresses, and each
    /// address has its host copy): a write through either is reported at
    /// both, whichever is checked first. Guarding the pages for the first
    /// view's check must not note the second view clean, or its copy would
    /// keep the bytes from before the write.
    #[test]
    fn a_write_is_reported_at_every_checked_view() {
        const SIZE: usize = 2 * BLOCK;
        let (section, one, two) = two_views(SIZE);
        let pages = |view: usize, block: usize| (view + block * BLOCK) / PAGE;
        assert_eq!(guard(one, one + SIZE), Some(vec![pages(one, 0), pages(one, 1)]));
        assert_eq!(guard(two, two + SIZE), Some(vec![pages(two, 0), pages(two, 1)]), "the second view's first check");
        assert_eq!(guard(one, one + SIZE), Some(Vec::new()), "nothing written yet");
        assert_eq!(guard(two, two + SIZE), Some(Vec::new()), "nothing written yet");
        // SAFETY: the second view, read-write once the handler has opened it.
        unsafe { ((two + BLOCK + 8) as *mut u8).write(1) };
        assert_eq!(guard(one, one + SIZE), Some(vec![pages(one, 1)]));
        assert_eq!(guard(two, two + SIZE), Some(vec![pages(two, 1)]), "written through this view, checked second");
        // SAFETY: the first view, as above.
        unsafe { ((one + 8) as *mut u8).write(1) };
        assert_eq!(guard(two, two + SIZE), Some(vec![pages(two, 0)]));
        assert_eq!(guard(one, one + SIZE), Some(vec![pages(one, 0)]), "written through this view, checked second");
        assert_eq!(guard(one, one + SIZE), Some(Vec::new()));
        assert_eq!(guard(two, two + SIZE), Some(Vec::new()));
        drop_views(section, &[one, two], SIZE);
    }

    /// A page shown in more than `MANY_VIEWS` views is guarded at each by
    /// itself: the others stay writable, and nothing looks through them all.
    #[test]
    fn a_page_of_many_views_is_guarded_one_view_at_a_time() {
        let (section, one, two) = two_views(BLOCK);
        let filler = one + (1 << 45);
        for n in 0..MANY_VIEWS {
            crate::inplace::title_memory_view(filler + n * BLOCK, BLOCK, section, 0);
        }
        assert_eq!(guard(one, one + BLOCK), Some(vec![one / PAGE]));
        assert!(writable(two), "another view of it is left alone");
        // SAFETY: the guarded view, opened by the handler.
        unsafe { ((one + 8) as *mut u8).write(1) };
        assert_eq!(guard(one, one + BLOCK), Some(vec![one / PAGE]));
        title_memory_remapped(filler, MANY_VIEWS * BLOCK);
        drop_views(section, &[one, two], BLOCK);
    }

    /// A write or mapping that runs past the end of the address space (a
    /// title's `fread` with a huge count) covers up to the end; the sum used
    /// to overflow. Up there no test has guarded blocks.
    #[test]
    fn ranges_past_the_address_space_end_do_not_overflow() {
        let top = usize::MAX - 16;
        writing(top, PAGE);
        title_memory_remapped(top, usize::MAX);
    }
}

#[cfg(test)]
mod mapping_tests {
    use super::{title_memory_reading, title_memory_remapping};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, mpsc};
    use std::time::Duration;

    /// A change of mapping waits for a read in place that is under way, and
    /// a read in place waits for a change of mapping.
    #[test]
    fn reads_in_place_and_remappings_wait_for_each_other() {
        // A reader holds; the change does not begin until it lets go.
        let changed = Arc::new(AtomicBool::new(false));
        let reading = title_memory_reading();
        let (began, wait) = mpsc::channel();
        let change = {
            let changed = changed.clone();
            std::thread::spawn(move || {
                began.send(()).unwrap();
                let _changing = title_memory_remapping();
                changed.store(true, Ordering::SeqCst);
            })
        };
        wait.recv().unwrap();
        std::thread::sleep(Duration::from_millis(100));
        assert!(!changed.load(Ordering::SeqCst), "the mapping changed under a read in place");
        drop(reading);
        change.join().unwrap();
        assert!(changed.load(Ordering::SeqCst));
        // A change holds; the read does not begin until it is done.
        let read = Arc::new(AtomicBool::new(false));
        let changing = title_memory_remapping();
        let (began, wait) = mpsc::channel();
        let reader = {
            let read = read.clone();
            std::thread::spawn(move || {
                began.send(()).unwrap();
                let _reading = title_memory_reading();
                read.store(true, Ordering::SeqCst);
            })
        };
        wait.recv().unwrap();
        std::thread::sleep(Duration::from_millis(100));
        assert!(!read.load(Ordering::SeqCst), "a read in place began during a change of mapping");
        drop(changing);
        reader.join().unwrap();
        assert!(read.load(Ordering::SeqCst));
    }
}

#[cfg(test)]
mod blocks_tests {
    use super::Blocks;
    use std::collections::BTreeSet;

    /// The bit set answers as a tree of blocks does, across word ends:
    /// membership of whole ranges, the blocks of a range in order, and the
    /// blocks of a range not in the set in order.
    #[test]
    fn blocks_answer_as_a_tree_of_blocks() {
        let mut bits = Blocks::new();
        let mut tree = BTreeSet::new();
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = |most: usize| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 33) as usize % most
        };
        for step in 0..4000 {
            let (at, len) = (1000 + next(400), next(140));
            match step % 4 {
                0 => {
                    bits.extend(at..at + len);
                    tree.extend(at..at + len);
                }
                1 => {
                    bits.insert(at);
                    tree.insert(at);
                }
                _ => {
                    bits.remove(at);
                    tree.remove(&at);
                }
            }
            let (from, n) = (990 + next(420), next(200));
            let range = from..from + n;
            assert_eq!(
                bits.holds(range.clone()),
                tree.range(range.clone()).count() == n,
                "all of {range:?} at step {step}"
            );
            assert_eq!(
                bits.range(range.clone()).collect::<Vec<_>>(),
                tree.range(range.clone()).copied().collect::<Vec<_>>(),
                "the blocks of {range:?} at step {step}"
            );
            assert_eq!(
                bits.outside(range.clone()),
                range.clone().filter(|b| !tree.contains(b)).collect::<Vec<_>>(),
                "the blocks of {range:?} not held at step {step}"
            );
            let mut room = vec![usize::MAX; 3];
            bits.outside_into(range.clone(), &mut room);
            assert_eq!(
                room,
                range.clone().filter(|b| !tree.contains(b)).collect::<Vec<_>>(),
                "the blocks of {range:?} not held, into a room that held others, at step {step}"
            );
        }
    }
}

#[cfg(test)]
mod stamps_tests {
    use super::{CHUNK, Stamps};
    use std::collections::BTreeMap;

    /// `read_into` into a list that held something before.
    fn read(stamps: &Stamps, first: usize, last: usize) -> Vec<u64> {
        let mut out = vec![7; 3];
        stamps.read_into(first, last, &mut out);
        out
    }

    /// The chunks' edges: a run of pages across two chunks, the first and
    /// the last page of a chunk, pages never written between written ones,
    /// chunks never written.
    #[test]
    fn stamps_read_across_chunk_edges() {
        let mut s = Stamps::new();
        s.note_all(CHUNK - 2..CHUNK + 2, 3);
        assert_eq!(read(&s, CHUNK - 3, CHUNK + 3), [0, 3, 3, 3, 3, 0]);
        assert_eq!(s.note(2 * CHUNK, 4), 0);
        assert_eq!(s.note(3 * CHUNK - 1, 5), 0);
        assert_eq!(s.note(3 * CHUNK - 1, 6), 5);
        let got = read(&s, 2 * CHUNK - 1, 3 * CHUNK + 1);
        assert_eq!(got.len(), CHUNK + 2);
        assert_eq!((got[0], got[1], got[CHUNK], got[CHUNK + 1]), (0, 4, 6, 0));
        assert!(got[2..CHUNK].iter().all(|&at| at == 0));
        assert_eq!(read(&s, 10 * CHUNK + 5, 12 * CHUNK), vec![0; 2 * CHUNK - 5]);
        assert!(read(&s, 5, 5).is_empty());
        // Two runs in one chunk, the second before the first.
        s.note_all([5 * CHUNK + 9, 5 * CHUNK + 10, 5 * CHUNK + 1], 8);
        assert_eq!(read(&s, 5 * CHUNK, 5 * CHUNK + 11), [0, 8, 0, 0, 0, 0, 0, 0, 0, 8, 8]);
    }

    /// The stamps answer as a tree with an entry per page written does.
    #[test]
    fn stamps_answer_as_a_tree_of_pages() {
        let mut s = Stamps::new();
        let mut tree = BTreeMap::new();
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = |most: usize| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 33) as usize % most
        };
        for step in 1..3000u64 {
            let (at, len) = (next(6 * CHUNK), next(2 * CHUNK));
            if step % 3 == 0 {
                assert_eq!(s.note(at, step), tree.insert(at, step).unwrap_or(0), "page {at} at step {step}");
            } else {
                s.note_all(at..at + len, step);
                tree.extend((at..at + len).map(|page| (page, step)));
            }
            let (first, n) = (next(7 * CHUNK), next(3 * CHUNK));
            let want: Vec<u64> = (first..first + n).map(|page| tree.get(&page).copied().unwrap_or(0)).collect();
            assert_eq!(read(&s, first, first + n), want, "pages {first}+{n} at step {step}");
        }
    }
}
