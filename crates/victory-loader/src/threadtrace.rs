//! Debugging aid (`VICTORY_TRACE_THREADS=1`): who touches other threads.
//!
//! A thread's registers change behind its back in two ways only: something
//! writes the context an exception or a queued procedure call saved for it,
//! or another thread sets its context. This records both kinds of event,
//! prints the calls as they happen and repeats the last ones, with the
//! faulting thread's previous exceptions, in a crash report:
//!
//! - the ntdll calls that suspend or resume a thread, read or set its
//!   context or queue a procedure call to it, each with the caller's stack;
//! - every exception each thread takes, whoever handles it.

use std::cell::Cell;
use std::ffi::CStr;
use std::fmt::Write as _;
use std::ptr::null_mut;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicPtr, AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::System::Diagnostics::Debug::{
    AddVectoredExceptionHandler, CONTEXT, EXCEPTION_POINTERS, FlushInstructionCache,
    RemoveVectoredExceptionHandler, RtlCaptureStackBackTrace,
};
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress};
use windows_sys::Win32::System::Memory::{
    MEM_COMMIT, MEM_RESERVE, PAGE_EXECUTE_READ, PAGE_EXECUTE_READWRITE,
    VirtualAlloc, VirtualProtect,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetCurrentThreadId, GetThreadId};

/// Environment variable that turns the trace on.
const ENV: &str = "VICTORY_TRACE_THREADS";

/// The hooked calls, in the order of `REAL`.
const NAMES: [&CStr; 7] = [
    c"NtSuspendThread",
    c"NtResumeThread",
    c"NtGetContextThread",
    c"NtSetContextThread",
    c"NtQueueApcThread",
    c"NtQueueApcThreadEx",
    c"NtQueueApcThreadEx2",
];
/// Each call's system call stub, copied before ours replaced it.
static REAL: [AtomicUsize; 7] = [const { AtomicUsize::new(0) }; 7];
/// How often each was called.
static COUNT: [AtomicU64; 7] = [const { AtomicU64::new(0) }; 7];

const FRAMES: usize = 6;
/// One recorded call. `time` is written last and is never zero.
struct Call {
    time: AtomicU64,
    /// Call index, then the target thread's id.
    what: AtomicU64,
    from: AtomicU64,
    detail: [AtomicU64; 2],
    frames: [AtomicU64; FRAMES],
}
const CALLS: usize = 256;
static RING: [Call; CALLS] = [const {
    Call {
        time: AtomicU64::new(0),
        what: AtomicU64::new(0),
        from: AtomicU64::new(0),
        detail: [const { AtomicU64::new(0) }; 2],
        frames: [const { AtomicU64::new(0) }; FRAMES],
    }
}; CALLS];
static NEXT: AtomicUsize = AtomicUsize::new(0);
static START: OnceLock<Instant> = OnceLock::new();

/// Microseconds since the trace began, never zero.
fn now() -> u64 {
    START.get().map_or(1, |s| s.elapsed().as_micros() as u64 + 1)
}

/// Records one call. Takes no lock and does not allocate: the caller may
/// have suspended a thread that holds either.
fn note(index: usize, thread: HANDLE, detail: [u64; 2]) {
    COUNT[index].fetch_add(1, Relaxed);
    let call = &RING[NEXT.fetch_add(1, Relaxed) % CALLS];
    call.time.store(0, Relaxed);
    // SAFETY: asks for the id of the thread the handle names; 0 if it may not.
    let target = unsafe { GetThreadId(thread) };
    call.what.store((index as u64) << 32 | u64::from(target), Relaxed);
    // SAFETY: no arguments.
    call.from.store(u64::from(unsafe { GetCurrentThreadId() }), Relaxed);
    call.detail[0].store(detail[0], Relaxed);
    call.detail[1].store(detail[1], Relaxed);
    let mut frames = [null_mut(); FRAMES];
    // SAFETY: writes at most `FRAMES` return addresses of this thread.
    let n = unsafe {
        RtlCaptureStackBackTrace(2, FRAMES as u32, frames.as_mut_ptr(), null_mut())
    };
    for (i, slot) in call.frames.iter().enumerate() {
        slot.store(if i < n as usize { frames[i] as u64 } else { 0 }, Relaxed);
    }
    call.time.store(now(), Relaxed);
}

/// One line for a recorded call, or nothing while it is being written.
fn describe(call: &Call) -> Option<String> {
    let time = call.time.load(Relaxed);
    if time == 0 {
        return None;
    }
    let what = call.what.load(Relaxed);
    let index = (what >> 32) as usize;
    let detail = [call.detail[0].load(Relaxed), call.detail[1].load(Relaxed)];
    let mut text = format!(
        "{:>10.3} ms {} on thread {} by thread {}",
        time as f64 / 1000.0,
        NAMES[index].to_string_lossy(),
        what as u32,
        call.from.load(Relaxed)
    );
    match index {
        2 => write!(text, " (flags {:#x})", detail[0]),
        3 => write!(text, " (flags {:#x}, rip {})", detail[0], crate::locate(detail[1] as usize)),
        4.. => write!(text, " (routine {}, flags {:#x})", crate::locate(detail[1] as usize), detail[0]),
        _ => Ok(()),
    }
    .ok();
    text.push_str(" from");
    for frame in &call.frames {
        match frame.load(Relaxed) {
            0 => break,
            pc => write!(text, " {}", crate::locate(pc as usize)).ok(),
        };
    }
    Some(text)
}

type Two = unsafe extern "system" fn(HANDLE, *mut core::ffi::c_void) -> i32;

fn real(index: usize) -> usize {
    REAL[index].load(Relaxed)
}

unsafe extern "system" fn suspend(thread: HANDLE, previous: *mut core::ffi::c_void) -> i32 {
    note(0, thread, [0, 0]);
    // SAFETY: the copied stub takes the arguments of the call it replaced.
    unsafe { std::mem::transmute::<usize, Two>(real(0))(thread, previous) }
}

unsafe extern "system" fn resume(thread: HANDLE, previous: *mut core::ffi::c_void) -> i32 {
    note(1, thread, [0, 0]);
    // SAFETY: as in `suspend`.
    unsafe { std::mem::transmute::<usize, Two>(real(1))(thread, previous) }
}

unsafe extern "system" fn get_context(thread: HANDLE, context: *mut core::ffi::c_void) -> i32 {
    // SAFETY: the caller's context, whose flags say what it asks for.
    let flags = unsafe { (*context.cast::<CONTEXT>()).ContextFlags };
    note(2, thread, [u64::from(flags), 0]);
    // SAFETY: as in `suspend`.
    unsafe { std::mem::transmute::<usize, Two>(real(2))(thread, context) }
}

unsafe extern "system" fn set_context(thread: HANDLE, context: *mut core::ffi::c_void) -> i32 {
    // SAFETY: the context the caller sets.
    let (flags, rip) = unsafe {
        let c = &*context.cast::<CONTEXT>();
        (c.ContextFlags, c.Rip)
    };
    note(3, thread, [u64::from(flags), rip]);
    // SAFETY: as in `suspend`.
    unsafe { std::mem::transmute::<usize, Two>(real(3))(thread, context) }
}

unsafe extern "system" fn queue(
    thread: HANDLE,
    routine: usize,
    a: usize,
    b: usize,
    c: usize,
) -> i32 {
    type Real = unsafe extern "system" fn(HANDLE, usize, usize, usize, usize) -> i32;
    note(4, thread, [0, routine as u64]);
    // SAFETY: as in `suspend`.
    unsafe { std::mem::transmute::<usize, Real>(real(4))(thread, routine, a, b, c) }
}

unsafe extern "system" fn queue_ex(
    thread: HANDLE,
    reserve: usize,
    routine: usize,
    a: usize,
    b: usize,
    c: usize,
) -> i32 {
    type Real = unsafe extern "system" fn(HANDLE, usize, usize, usize, usize, usize) -> i32;
    note(5, thread, [reserve as u64, routine as u64]);
    // SAFETY: as in `suspend`.
    unsafe { std::mem::transmute::<usize, Real>(real(5))(thread, reserve, routine, a, b, c) }
}

unsafe extern "system" fn queue_ex2(
    thread: HANDLE,
    reserve: usize,
    flags: usize,
    routine: usize,
    a: usize,
    b: usize,
    c: usize,
) -> i32 {
    type Real =
        unsafe extern "system" fn(HANDLE, usize, usize, usize, usize, usize, usize) -> i32;
    note(6, thread, [flags as u64, routine as u64]);
    // SAFETY: as in `suspend`.
    unsafe {
        std::mem::transmute::<usize, Real>(real(6))(thread, reserve, flags, routine, a, b, c)
    }
}

/// Makes ntdll's `name` jump to `to`, keeping a copy of its system call
/// stub at `copy`. Only a stub of the usual form (`mov r10, rcx; mov eax,
/// number`) is replaced.
///
/// # Safety
/// `copy` has 16 writable bytes that become executable; `to` takes the
/// call's arguments.
unsafe fn hook(index: usize, to: usize, copy: *mut u8) -> bool {
    // SAFETY: looks an export of the loaded ntdll up, reads the 8 bytes of
    // its stub the check needs, and rewrites the stub's first 12 bytes (it
    // is 24 long) while no other thread of ours runs it yet.
    unsafe {
        let ntdll = GetModuleHandleA(c"ntdll.dll".as_ptr().cast());
        let Some(at) = GetProcAddress(ntdll, NAMES[index].as_ptr().cast()) else {
            return false;
        };
        let at = at as usize as *mut u8;
        let head = std::slice::from_raw_parts(at, 8);
        if head[..4] != [0x4c, 0x8b, 0xd1, 0xb8] {
            return false;
        }
        std::ptr::copy_nonoverlapping(at, copy, 8);
        // syscall; ret
        std::ptr::copy_nonoverlapping([0x0f_u8, 0x05, 0xc3].as_ptr(), copy.add(8), 3);
        REAL[index].store(copy as usize, Relaxed);
        let mut old = 0;
        if VirtualProtect(at.cast(), 12, PAGE_EXECUTE_READWRITE, &mut old) == 0 {
            return false;
        }
        // mov rax, to; jmp rax
        at.write(0x48);
        at.add(1).write(0xb8);
        at.add(2).cast::<u64>().write_unaligned(to as u64);
        at.add(10).write(0xff);
        at.add(11).write(0xe0);
        VirtualProtect(at.cast(), 12, old, &mut old);
        FlushInstructionCache(GetCurrentProcess(), at.cast(), 12);
        true
    }
}

/// One exception a thread took.
#[derive(Clone, Copy)]
struct Seen {
    time: u64,
    code: u32,
    rip: usize,
    info: [usize; 2],
}
const SEEN: usize = 8;
thread_local! {
    /// The thread's last exceptions, oldest first, and how many it took.
    static HISTORY: Cell<([Seen; SEEN], u64)> = const {
        Cell::new(([Seen { time: 0, code: 0, rip: 0, info: [0; 2] }; SEEN], 0))
    };
}

unsafe extern "system" fn seen(info: *mut EXCEPTION_POINTERS) -> i32 {
    // SAFETY: the system passes the exception being dispatched.
    let record = unsafe { &*(*info).ExceptionRecord };
    let one = Seen {
        time: now(),
        code: record.ExceptionCode as u32,
        rip: record.ExceptionAddress as usize,
        info: [record.ExceptionInformation[0], record.ExceptionInformation[1]],
    };
    let _ = HISTORY.try_with(|h| {
        let (mut list, count) = h.get();
        list.copy_within(1.., 0);
        list[SEEN - 1] = one;
        h.set((list, count + 1));
    });
    0 // EXCEPTION_CONTINUE_SEARCH
}

/// The handler's registration, replaced to keep it ahead of later ones.
static HANDLER: AtomicPtr<core::ffi::c_void> = AtomicPtr::new(null_mut());

fn register_first() {
    // SAFETY: `seen` only reads the exception it is given.
    let new = unsafe { AddVectoredExceptionHandler(1, Some(seen)) };
    let old = HANDLER.swap(new, Relaxed);
    if !old.is_null() {
        // SAFETY: a registration of ours, removed once.
        unsafe { RemoveVectoredExceptionHandler(old) };
    }
}

/// Starts the trace if its switch is set; call before title code runs.
pub(crate) fn install() {
    if !std::env::var_os(ENV).is_some_and(|v| v == "1") {
        return;
    }
    START.get_or_init(Instant::now);
    let to: [usize; 7] = [
        suspend as *const () as usize,
        resume as *const () as usize,
        get_context as *const () as usize,
        set_context as *const () as usize,
        queue as *const () as usize,
        queue_ex as *const () as usize,
        queue_ex2 as *const () as usize,
    ];
    // SAFETY: a fresh page for the stub copies; each is whole before the
    // jump to it exists, and the page ends up read-only.
    let hooked: Vec<_> = unsafe {
        let page =
            VirtualAlloc(null_mut(), 4096, MEM_COMMIT | MEM_RESERVE, PAGE_EXECUTE_READWRITE)
                .cast::<u8>();
        assert!(!page.is_null(), "no page for the system call stubs");
        let names: Vec<_> = (0..NAMES.len())
            .filter(|&i| hook(i, to[i], page.add(16 * i)))
            .map(|i| NAMES[i].to_string_lossy())
            .collect();
        let mut old = 0;
        VirtualProtect(page.cast(), 4096, PAGE_EXECUTE_READ, &mut old);
        names
    };
    register_first();
    eprintln!("VIctory: thread trace on ({ENV}): {}", hooked.join(", "));
    let spawned = std::thread::Builder::new().name("thread trace".into()).spawn(|| {
        // Prints the calls as they are recorded, the first `LIVE` of them,
        // then only how many there were.
        const LIVE: usize = 400;
        let (mut printed, mut counted) = (0, [0; 7]);
        let mut last = Instant::now();
        loop {
            std::thread::sleep(Duration::from_millis(100));
            // Handlers registered since are ahead of ours again.
            register_first();
            let next = NEXT.load(Relaxed);
            while printed < next.min(LIVE) {
                if let Some(text) = describe(&RING[printed % CALLS]) {
                    eprintln!("VIctory: thread trace: {text}");
                }
                printed += 1;
            }
            let counts: [u64; 7] = std::array::from_fn(|i| COUNT[i].load(Relaxed));
            if counts != counted && last.elapsed() >= Duration::from_secs(5) {
                eprintln!("VIctory: thread trace: calls so far {}", totals(&counts));
                counted = counts;
                last = Instant::now();
            }
        }
    });
    if let Err(e) = spawned {
        eprintln!("VIctory: thread trace: no printing thread ({e})");
    }
}

fn totals(counts: &[u64; 7]) -> String {
    let each: Vec<_> = NAMES
        .iter()
        .zip(counts)
        .filter(|(_, n)| **n != 0)
        .map(|(name, n)| format!("{} {n}", name.to_string_lossy()))
        .collect();
    each.join(", ")
}

/// For a crash report on the faulting thread: its registers the report does
/// not print, its previous exceptions and the last recorded calls. Empty
/// when the trace is off.
pub(crate) fn report(context: &CONTEXT) -> Vec<String> {
    if START.get().is_none() {
        return Vec::new();
    }
    let now = now();
    // SAFETY: no arguments.
    let thread = unsafe { GetCurrentThreadId() };
    let mut lines = vec![format!(
        "  thread {thread} at {:.3} ms: rip {:#x} eflags {:#x} context flags {:#x} dr6 {:#x} dr7 {:#x}",
        now as f64 / 1000.0,
        context.Rip,
        context.EFlags,
        context.ContextFlags,
        context.Dr6,
        context.Dr7
    )];
    let _ = HISTORY.try_with(|h| {
        let (list, count) = h.get();
        lines.push(format!("  exceptions of this thread so far: {count}; the last ones:"));
        for s in list.iter().filter(|s| s.time != 0) {
            lines.push(format!(
                "    {:>10.3} ms before: {:#010x} at {} ({:#x} {:#x})",
                (now - s.time) as f64 / 1000.0,
                s.code,
                crate::locate(s.rip),
                s.info[0],
                s.info[1]
            ));
        }
    });
    let counts: [u64; 7] = std::array::from_fn(|i| COUNT[i].load(Relaxed));
    lines.push(format!("  calls on threads so far: {}; the last ones:", totals(&counts)));
    let next = NEXT.load(Relaxed);
    for i in next.saturating_sub(48)..next {
        if let Some(text) = describe(&RING[i % CALLS]) {
            lines.push(format!("    {text}"));
        }
    }
    lines
}
