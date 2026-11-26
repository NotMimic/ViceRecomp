//! The card's memory by owner (`VICTORY_CARD_MEMORY=1`, diagnostics builds,
//! where the memory line is): every resource and heap the layer makes is
//! entered under the kind that names its purpose, with the bytes the device
//! says its description takes (`GetResourceAllocationInfo`; a heap by its
//! size), and leaves the account when the Direct3D object is destroyed,
//! wherever its last reference was dropped. The memory line then says, per
//! kind, how many are alive and their bytes, against what the system counts
//! in use.
//!
//! How an object leaves: a small COM object (an `IUnknown`, nothing more) is
//! stored in the object's private data (`SetPrivateDataInterface`). The
//! runtime holds a reference to it until the object is destroyed, and its
//! last release takes the entry out. No owner's code has to say when it lets
//! go, so objects held by lists, slots and other threads leave when the last
//! of them does. Off, entering is one load of a flag.
//!
//! What it cannot see: pipeline states and their shader code, descriptor
//! heaps, command allocators and lists, query heaps, and the driver's own
//! allocations. What the system counts beyond the sum holds them.

use std::ffi::c_void;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use windows::Win32::Graphics::Direct3D12::{ID3D12Device, ID3D12Heap, ID3D12Object, ID3D12Resource};
use windows::Win32::Graphics::Dxgi::IDXGISwapChain;
use windows::core::{GUID, HRESULT, IUnknown, Interface};

/// What a resource or heap the layer makes is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Owner {
    /// Host copies of textures the title fills.
    TitleTextures,
    /// Host copies of textures the host renders to or writes.
    HostTargets,
    /// Host copies given tiles level by level: no memory of their own
    /// (their tiles are in [`Owner::Tiles`]).
    Reserved,
    /// The heaps those tiles come from.
    Tiles,
    /// Twins of depth textures: colour twins, depth planes, their buffers.
    DepthTwins,
    /// Copies of title buffers in the card's memory.
    CardCopies,
    /// Card sides of windows of title memory outside buffers.
    Windows,
    /// Twins of list scratch in the card's memory.
    ScratchTwins,
    /// Hashes of the pieces of card copies, for the compare passes.
    Hashes,
    /// Arguments, budgets and scratch of indirect draws and dispatches.
    Indirect,
    /// List constants filled on the card.
    CardConstants,
    /// Landing buffers of title bytes on their way up (the copy queue).
    Landing,
    /// Buffers that carry bytes between textures placed in the same memory.
    Aliases,
    /// Images of the stand-in for indirect light.
    Ambient,
    /// The swap chain's buffers.
    SwapChain,
    /// Buffers of the in-place probe.
    Probes,
    /// Host copies of title memory.
    PcCopies,
    /// Results of the compare passes and the pieces copied back.
    PcCompare,
    /// Texture staging.
    Staging,
    /// List scratch.
    Scratch,
    /// Tables of views past the title's slots.
    ViewTables,
    /// Read backs of copies and textures.
    ReadBacks,
    /// Capture, meter and timestamp buffers.
    Meters,
    /// Other buffers in the PC's memory.
    PcOther,
}

/// Where a kind's bytes are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pool {
    /// The card's own memory (the system's local group).
    Card,
    /// The PC's memory shared with the card (the non-local group).
    Pc,
    /// None of its own: counted among the card's kinds, with no bytes.
    NoBytes,
}

const COUNT: usize = Owner::PcOther as usize + 1;

/// Each kind's name in the line and its memory, in [`Owner`]'s order.
const KINDS: [(&str, Pool); COUNT] = [
    ("textures the title fills", Pool::Card),
    ("textures the host renders to or writes", Pool::Card),
    ("textures given tiles level by level (no bytes of their own)", Pool::NoBytes),
    ("heaps of those tiles", Pool::Card),
    ("twins of depth textures", Pool::Card),
    ("card copies of title buffers", Pool::Card),
    ("windows on the card", Pool::Card),
    ("list scratch twins", Pool::Card),
    ("hashes of the compare passes", Pool::Card),
    ("indirect-draw buffers", Pool::Card),
    ("list constants on the card", Pool::Card),
    ("landing buffers", Pool::Card),
    ("buffers between textures in the same memory", Pool::Card),
    ("images of the stand-in for indirect light", Pool::Card),
    ("the swap chain's buffers", Pool::Card),
    ("probe buffers", Pool::Card),
    ("host copies of title memory", Pool::Pc),
    ("results of the compare passes and copies back", Pool::Pc),
    ("texture staging", Pool::Pc),
    ("list scratch", Pool::Pc),
    ("view tables", Pool::Pc),
    ("read backs", Pool::Pc),
    ("capture, meter and timestamp buffers", Pool::Pc),
    ("other buffers", Pool::Pc),
];

impl Owner {
    fn pool(self) -> Pool {
        KINDS[self as usize].1
    }
}

/// Objects alive and their bytes, per kind.
pub(crate) struct Account {
    live: [[AtomicU64; 2]; COUNT],
}

impl Account {
    pub(crate) const fn new() -> Account {
        Account {
            live: [const { [AtomicU64::new(0), AtomicU64::new(0)] }; COUNT],
        }
    }

    fn add(&self, kind: Owner, bytes: u64) {
        let [count, held] = &self.live[kind as usize];
        count.fetch_add(1, Ordering::Relaxed);
        held.fetch_add(bytes, Ordering::Relaxed);
    }

    fn sub(&self, kind: Owner, bytes: u64) {
        let [count, held] = &self.live[kind as usize];
        count.fetch_sub(1, Ordering::Relaxed);
        held.fetch_sub(bytes, Ordering::Relaxed);
    }

    /// Count and bytes alive now, per kind in [`Owner`]'s order.
    #[cfg_attr(not(feature = "diagnostics"), allow(dead_code))]
    fn now(&self) -> [(u64, u64); COUNT] {
        std::array::from_fn(|i| (self.live[i][0].load(Ordering::Relaxed), self.live[i][1].load(Ordering::Relaxed)))
    }
}

static ACCOUNT: Account = Account::new();

/// The private data key the trackers are stored under.
const ENTERED: GUID = GUID::from_u128(0x6c1e3b52_9a47_4d0f_b8e2_57c90a1d4f36);

/// Whether the account is kept (`VICTORY_CARD_MEMORY=1`).
pub(crate) fn on() -> bool {
    #[cfg(feature = "diagnostics")]
    {
        static ON: std::sync::LazyLock<bool> =
            std::sync::LazyLock::new(|| std::env::var_os("VICTORY_CARD_MEMORY").is_some_and(|v| v == "1"));
        *ON
    }
    #[cfg(not(feature = "diagnostics"))]
    {
        false
    }
}

/// Enters `resource`, just made, as `kind`, with the bytes the device says
/// its description takes (none for a kind without memory of its own).
pub(crate) fn enter(resource: &ID3D12Resource, kind: Owner) {
    if !on() {
        return;
    }
    let bytes = if kind.pool() == Pool::NoBytes { 0 } else { allocated(resource) };
    attach(resource, kind, bytes);
}

/// Enters `heap`, just made, as `kind`, with its size.
pub(crate) fn enter_heap(heap: &ID3D12Heap, kind: Owner) {
    if !on() {
        return;
    }
    // SAFETY: a plain query on a live heap.
    let bytes = unsafe { heap.GetDesc() }.SizeInBytes;
    attach(heap, kind, bytes);
}

/// Enters the first `buffers` buffers of `swap_chain`, once it is made or
/// its buffers were made again.
pub(crate) fn enter_swap_chain(swap_chain: &IDXGISwapChain, buffers: u32) {
    if !on() {
        return;
    }
    for i in 0..buffers {
        // SAFETY: a live swap chain; an index below its count of buffers.
        if let Ok(buffer) = unsafe { swap_chain.GetBuffer::<ID3D12Resource>(i) } {
            enter(&buffer, Owner::SwapChain);
        }
    }
}

/// The bytes the device allocates for `resource`'s description (its
/// alignment included), or 0 if it does not say.
fn allocated(resource: &ID3D12Resource) -> u64 {
    let mut device: Option<ID3D12Device> = None;
    // SAFETY: plain queries on a live resource and its device.
    unsafe {
        if resource.GetDevice(&mut device).is_err() {
            return 0;
        }
        let Some(device) = device else {
            return 0;
        };
        let size = device.GetResourceAllocationInfo(0, &[resource.GetDesc()]).SizeInBytes;
        if size == u64::MAX { 0 } else { size }
    }
}

/// Adds `bytes` of `kind` to the account until `object` is destroyed.
fn attach(object: &ID3D12Object, kind: Owner, bytes: u64) {
    let tracker = tracker(&ACCOUNT, kind, bytes);
    // SAFETY: a live object; the runtime takes a reference of its own to
    // the tracker and lets it go when the object is destroyed. If this
    // fails, dropping our reference below takes the entry out again.
    if let Err(e) = unsafe { object.SetPrivateDataInterface(&ENTERED, &tracker) } {
        static SAID: AtomicBool = AtomicBool::new(false);
        if !SAID.swap(true, Ordering::Relaxed) {
            tracing::warn!(error = %e, "an object could not be entered in the account of the card's memory: such objects are left out");
        }
    }
}

/// The COM object stored in an entered object's private data.
#[repr(C)]
struct Tracker {
    /// First, as COM wants: the `IUnknown` table.
    vtable: &'static Vtable,
    refs: AtomicU32,
    account: &'static Account,
    kind: Owner,
    bytes: u64,
}

/// `IUnknown`'s table: `QueryInterface`, `AddRef`, `Release`.
#[repr(C)]
struct Vtable {
    query_interface: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
}

static VTABLE: Vtable = Vtable {
    query_interface,
    add_ref,
    release,
};

const E_NOINTERFACE: HRESULT = HRESULT(0x8000_4002_u32 as i32);
const E_POINTER: HRESULT = HRESULT(0x8000_4003_u32 as i32);

/// A tracker holding one reference: `bytes` of `kind` are in `account`
/// until its last reference goes.
fn tracker(account: &'static Account, kind: Owner, bytes: u64) -> IUnknown {
    account.add(kind, bytes);
    let raw = Box::into_raw(Box::new(Tracker {
        vtable: &VTABLE,
        refs: AtomicU32::new(1),
        account,
        kind,
        bytes,
    }));
    // SAFETY: a tracker starts with its `IUnknown` table, and the one
    // reference it holds is handed over here.
    unsafe { IUnknown::from_raw(raw.cast()) }
}

/// `IUnknown::QueryInterface`: a tracker is an `IUnknown` and nothing else.
unsafe extern "system" fn query_interface(this: *mut c_void, iid: *const GUID, out: *mut *mut c_void) -> HRESULT {
    if out.is_null() {
        return E_POINTER;
    }
    // SAFETY: COM's rules: `this` is a live tracker, `out` is writable and
    // `iid`, if not null, names an interface.
    unsafe {
        if !iid.is_null() && *iid == IUnknown::IID {
            add_ref(this);
            *out = this;
            return HRESULT(0);
        }
        *out = std::ptr::null_mut();
    }
    E_NOINTERFACE
}

/// `IUnknown::AddRef`.
unsafe extern "system" fn add_ref(this: *mut c_void) -> u32 {
    // SAFETY: COM's rules: `this` is a live tracker.
    unsafe { (*this.cast::<Tracker>()).refs.fetch_add(1, Ordering::Relaxed) + 1 }
}

/// `IUnknown::Release`: the last takes the entry out and frees the tracker.
unsafe extern "system" fn release(this: *mut c_void) -> u32 {
    let tracker = this.cast::<Tracker>();
    // SAFETY: COM's rules: `this` is a live tracker until its last
    // reference goes, here.
    let left = unsafe { (*tracker).refs.fetch_sub(1, Ordering::Release) } - 1;
    if left == 0 {
        std::sync::atomic::fence(Ordering::Acquire);
        // SAFETY: the last reference: nothing else names the tracker, which
        // `tracker` made with `Box::into_raw`.
        let tracker = unsafe { Box::from_raw(tracker) };
        tracker.account.sub(tracker.kind, tracker.bytes);
    }
    left
}

/// The memory line's account: the kinds alive, then what the system counts
/// in use in the card's own memory (`card`) and in the PC's memory shared
/// with the card (`shared`), in bytes.
#[cfg_attr(not(feature = "diagnostics"), allow(dead_code))]
pub(crate) fn line(card: u64, shared: u64) -> String {
    format_line(&ACCOUNT.now(), card, shared)
}

/// [`line`] for the counts and bytes `live`, per kind in [`Owner`]'s order:
/// each memory's kinds that have objects alive (count, MB), their sum, the
/// system's figure and what it counts beyond the sum.
#[cfg_attr(not(feature = "diagnostics"), allow(dead_code))]
fn format_line(live: &[(u64, u64); COUNT], card: u64, shared: u64) -> String {
    let mb = |bytes: u64| bytes as f64 / (1 << 20) as f64;
    let mut text = String::from("the card's memory by owner:");
    let groups = [
        ("in the card's own memory (local)", Pool::Card, card),
        ("in the PC's memory shared with the card (non-local)", Pool::Pc, shared),
    ];
    for (title, pool, system) in groups {
        let _ = write!(text, " {title}: ");
        let mut sum = 0;
        for (&(count, bytes), &(name, of)) in live.iter().zip(&KINDS) {
            let here = of == pool || (of == Pool::NoBytes && pool == Pool::Card);
            if count == 0 || !here {
                continue;
            }
            sum += bytes;
            let _ = write!(text, "{name} {count}, {:.0} MB; ", mb(bytes));
        }
        let _ = write!(
            text,
            "entered {:.0} MB of {:.0} MB in use, {:.0} MB not entered;",
            mb(sum),
            mb(system),
            mb(system) - mb(sum)
        );
    }
    text.push_str(
        " not seen: pipeline states and their shader code, descriptor heaps, command allocators and lists, \
         query heaps, the driver's own allocations",
    );
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An entry stays while any reference to its tracker is alive, whoever
    /// holds it, and leaves with the last; a tracker answers only as an
    /// `IUnknown`.
    #[test]
    fn an_entry_leaves_with_the_last_reference() {
        static ACCOUNT: Account = Account::new();
        let at = Owner::CardCopies as usize;
        let first = tracker(&ACCOUNT, Owner::CardCopies, 3 << 20);
        let second = tracker(&ACCOUNT, Owner::CardCopies, 1 << 16);
        assert_eq!(ACCOUNT.now()[at], (2, (3 << 20) + (1 << 16)));
        let held = first.clone();
        let asked = first.cast::<IUnknown>().expect("a tracker is an IUnknown");
        assert!(first.cast::<ID3D12Object>().is_err());
        drop(first);
        drop(held);
        assert_eq!(ACCOUNT.now()[at], (2, (3 << 20) + (1 << 16)));
        drop(asked);
        assert_eq!(ACCOUNT.now()[at], (1, 1 << 16));
        drop(second);
        assert!(ACCOUNT.now().iter().all(|&n| n == (0, 0)));
    }

    /// The line names each kind alive with its count and megabytes under
    /// its memory, sums each memory and says what the system counts beyond
    /// the sum; kinds with nothing alive are left out.
    #[test]
    fn the_line_sums_each_memory_against_the_system() {
        let mut live = [(0, 0); COUNT];
        live[Owner::TitleTextures as usize] = (2, 3 << 20);
        live[Owner::Reserved as usize] = (5, 0);
        live[Owner::Tiles as usize] = (1, 64 << 20);
        live[Owner::ReadBacks as usize] = (1, 1 << 20);
        let line = format_line(&live, 100 << 20, 4 << 20);
        assert!(
            line.starts_with("the card's memory by owner: in the card's own memory (local): textures the title fills 2, 3 MB; "),
            "{line}"
        );
        assert!(
            line.contains(
                "textures given tiles level by level (no bytes of their own) 5, 0 MB; heaps of those tiles 1, 64 MB; \
                 entered 67 MB of 100 MB in use, 33 MB not entered;"
            ),
            "{line}"
        );
        assert!(
            line.contains(
                " in the PC's memory shared with the card (non-local): read backs 1, 1 MB; entered 1 MB of 4 MB in use, \
                 3 MB not entered; not seen: "
            ),
            "{line}"
        );
        assert!(!line.contains("windows on the card"), "{line}");
    }
}
