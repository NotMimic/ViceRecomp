//! Textures shaders write that the title's own code then reads on the CPU.
//! On the console the CPU and the GPU share memory, so what a compute pass
//! stores into a texture is in title memory for the CPU to read. Here it stays
//! in the texture's host copy. The title's ocean is one: its wave passes write
//! a 256x256 height map (R32_FLOAT) and a displacement map (R32G32_FLOAT), and
//! its CPU code samples both, which is what boats float on; it read memory
//! nothing had written (run rw1, `VICTORY_READ_WATCH=1`: about 30 places in
//! the title read those two kinds), and boats sank.
//!
//! For textures of the kinds in `VICTORY_TEXTURE_BACK` (comma-separated
//! `<width>x<height>f<format>`; unset or `1`: the four water formats found by
//! the upstream trace; `0`: none), a list in which shaders write one copies it
//! into a read-back buffer of ours when it closes, and once the list has run
//! the texels are tiled into the texture's title memory in the console's
//! layout, as the console's GPU would have left them.
//!
//! shortcut: by kind, not by texture: textures of those kinds the title never
//! reads are copied back too (about a megabyte per such list). The copy shows
//! what the list's last write left; a title read in between sees the frame
//! before, as it would before the console's GPU has run.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use windows::Win32::Graphics::Direct3D12::*;

use crate::object::This;

/// The kinds copied back: (width, height, format).
fn kinds() -> &'static [(u64, u32, u32)] {
    static KINDS: OnceLock<Vec<(u64, u32, u32)>> = OnceLock::new();
    KINDS.get_or_init(|| {
        const DEFAULT: &str = "256x256f41,256x256f16,64x64f16,128x128f36";
        let list = match std::env::var("VICTORY_TEXTURE_BACK").as_deref() {
            Ok("1") | Err(_) => DEFAULT.to_owned(),
            Ok(spec) => spec.to_owned(),
        };
        let kinds: Vec<(u64, u32, u32)> = list
            .split(',')
            .filter_map(|k| {
                let (size, format) = k.trim().split_once('f')?;
                let (w, h) = size.split_once('x')?;
                Some((w.parse().ok()?, h.parse().ok()?, format.parse().ok()?))
            })
            .collect();
        if !kinds.is_empty() {
            tracing::info!(?kinds, "textures of these kinds that shaders write are copied back into title memory (VICTORY_TEXTURE_BACK)");
        }
        kinds
    })
}

/// Whether title texture `resource` is of a kind copied back.
pub(crate) fn wanted(resource: This) -> bool {
    let kinds = kinds();
    !kinds.is_empty()
        && crate::resource::texture_kind(resource).is_some_and(|k| kinds.contains(&k))
}

/// An outstanding write to a specific allocation. Mapping and resource
/// changes invalidate only the writes that could touch the replaced memory.
struct Ticket {
    owner: usize,
    at: usize,
    end: usize,
    valid: AtomicBool,
}

static TICKETS: Mutex<Vec<Weak<Ticket>>> = Mutex::new(Vec::new());
static TRACKING: AtomicBool = AtomicBool::new(false);

/// Invalidations asked for so far, whether or not a ticket was there
/// ([`Target::new`] with [`free`] looks whether one came while it read the
/// texture's place).
static ASKED: AtomicU64 = AtomicU64::new(0);

/// Whether texture read-backs are tracked and landed without the runtime's
/// lock on changes of mapping (`crate::watch::title_memory_reading`):
/// `VICTORY_TEXBACK_FREE=1`, off by default; `VICTORY_AB` can turn it on and
/// off while the title runs. That lock is global: the title's page mappings
/// (`CopyPageMappings`, `XMemMapPhysicalPages`, about 14 ms a frame in the
/// world, one call up to 34 ms) hold it to write, whatever pages they
/// change. In a timeline of the world (Series X, RX 7900 XTX, run `chrono`
/// at 300 s) the thread that lands flights waited for it 173 ms in
/// [`land`], about four fifths of its "read-backs, telling the title", and
/// the threads that record lists 151 ms in [`Target::new`]: the title is
/// told its submissions are done that much later. What the lock kept from
/// changing, the ticket's pages, is kept by [`TICKETS`] instead: the
/// runtime says so ([`remapped`]) before it unmaps a page and after it maps
/// one, and that takes [`TICKETS`].
fn free() -> bool {
    #[cfg(feature = "diagnostics")]
    if let Some(on) = crate::ab::pick("VICTORY_TEXBACK_FREE") {
        return on;
    }
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_TEXBACK_FREE").is_some_and(|v| v == "1"))
}

fn track(owner: usize, at: usize, end: usize) -> Arc<Ticket> {
    let ticket = Arc::new(Ticket { owner, at, end, valid: AtomicBool::new(true) });
    let mut tickets = TICKETS.lock().unwrap_or_else(|e| e.into_inner());
    tickets.retain(|weak| weak.strong_count() != 0);
    tickets.push(Arc::downgrade(&ticket));
    TRACKING.store(true, Ordering::SeqCst);
    ticket
}

/// [`track`] if no invalidation was asked for since [`ASKED`] said `since`;
/// `None` if one was (it may be of these pages: the caller looks again).
/// Under [`TICKETS`], as invalidations are: one asked for after this has
/// the ticket to look at.
fn track_since(owner: usize, at: usize, end: usize, since: u64) -> Option<Arc<Ticket>> {
    let mut tickets = TICKETS.lock().unwrap_or_else(|e| e.into_inner());
    if ASKED.load(Ordering::SeqCst) != since {
        return None;
    }
    let ticket = Arc::new(Ticket { owner, at, end, valid: AtomicBool::new(true) });
    tickets.retain(|weak| weak.strong_count() != 0);
    tickets.push(Arc::downgrade(&ticket));
    Some(ticket)
}

fn invalidate(matches: impl Fn(&Ticket) -> bool) {
    // Counted first, and [`TRACKING`] looked at after: a [`Target::new`]
    // that set it before reading [`ASKED`] either sees this count or has its
    // ticket looked at below.
    ASKED.fetch_add(1, Ordering::SeqCst);
    if !TRACKING.load(Ordering::SeqCst) {
        return;
    }
    TICKETS.lock().unwrap_or_else(|e| e.into_inner()).retain(|weak| {
        let Some(ticket) = weak.upgrade() else { return false };
        if matches(&ticket) {
            ticket.valid.store(false, Ordering::Release);
        }
        true
    });
}

pub(crate) fn released(owner: usize) {
    invalidate(|ticket| ticket.owner == owner);
}

pub(crate) fn replaced(at: usize) {
    invalidate(|ticket| ticket.at == at);
}

pub(crate) fn remapped(at: usize, len: usize) {
    let end = at.saturating_add(len);
    invalidate(|ticket| at < ticket.end && ticket.at < end);
}

pub(crate) struct Target {
    pub(crate) copy: Arc<crate::resource::HostTexture>,
    layout: victory_formats::texture::Layout,
    ticket: Arc<Ticket>,
}

impl Target {
    pub(crate) fn new(resource: This) -> Option<Self> {
        // Keep the host copy independently of the title's resource lifetime.
        let copy = crate::resource::host_copy(resource)?;
        // ([`free`]) Without the runtime's lock: the ticket is kept only if
        // no invalidation was asked for while the texture's place was read;
        // a few tries, then the lock as before.
        if free() {
            TRACKING.store(true, Ordering::SeqCst);
            for _ in 0..4 {
                let since = ASKED.load(Ordering::SeqCst);
                let (layout, at, size) = crate::resource::title_texture(resource)?;
                let end = at.checked_add(size).filter(|&end| at != 0 && end > at)?;
                if let Some(ticket) = track_since(resource as usize, at, end, since) {
                    return Some(Self { copy, layout, ticket });
                }
            }
        }
        let _mapping = crate::watch::title_memory_reading();
        let (layout, at, size) = crate::resource::title_texture(resource)?;
        let end = at.checked_add(size).filter(|&end| at != 0 && end > at)?;
        let ticket = track(resource as usize, at, end);
        Some(Self { copy, layout, ticket })
    }

    pub(crate) fn owner(&self) -> usize { self.ticket.owner }
}

/// The readback resources and the allocation the pixels belong to.
pub(crate) struct Back {
    buffer: ID3D12Resource,
    footprints: Vec<D3D12_PLACED_SUBRESOURCE_FOOTPRINT>,
    rows: Vec<u32>,
    mips: u32,
    layout: victory_formats::texture::Layout,
    ticket: Arc<Ticket>,
}

impl Back {
    pub(crate) fn range(&self) -> (usize, usize) {
        (self.ticket.at, self.ticket.end)
    }
}

/// Records a host copy from a texture already in COPY_SOURCE.
pub(crate) fn record(list: &ID3D12GraphicsCommandList, target: Target) -> Option<Back> {
    if !target.ticket.valid.load(Ordering::Acquire) {
        return None;
    }
    let host = crate::host::host()?;
    let texture = &target.copy.texture;
    // SAFETY: a live texture kept through the command list's state tracking.
    let desc = unsafe { texture.GetDesc() };
    let mips = u32::from(desc.MipLevels.max(1));
    let subresources = mips * u32::from(desc.DepthOrArraySize.max(1));
    let n = subresources as usize;
    let mut footprints = vec![D3D12_PLACED_SUBRESOURCE_FOOTPRINT::default(); n];
    let mut rows = vec![0u32; n];
    let mut total = 0u64;
    // SAFETY: out arrays of `n` entries.
    unsafe {
        host.device.GetCopyableFootprints(
            &desc,
            0,
            subresources,
            0,
            Some(footprints.as_mut_ptr()),
            Some(rows.as_mut_ptr()),
            None,
            Some(&mut total),
        )
    };
    // The last footprint's advertised extent can omit row-end padding.
    // The tiler reads a pitched slice, so the mapped buffer must include it.
    for (footprint, &rows) in footprints.iter().zip(&rows) {
        let format = footprint.Footprint;
        let pitched = u64::from(format.RowPitch)
            .checked_mul(u64::from(rows))?
            .checked_mul(u64::from(format.Depth))?;
        total = total.max(footprint.Offset.checked_add(pitched)?);
    }
    let buffer = host.read_back_buffer(total)?;
    for (i, footprint) in footprints.iter().enumerate() {
        let to = D3D12_TEXTURE_COPY_LOCATION {
            // SAFETY: a borrowed pointer the location never releases.
            pResource: unsafe { std::mem::transmute_copy(&buffer) },
            Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
            Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 { PlacedFootprint: *footprint },
        };
        let from = D3D12_TEXTURE_COPY_LOCATION {
            // SAFETY: as above.
            pResource: unsafe { std::mem::transmute_copy(texture) },
            Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
            Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 { SubresourceIndex: i as u32 },
        };
        // SAFETY: an open list; the texture is in the copy-source state.
        unsafe { list.CopyTextureRegion(&to, 0, 0, 0, &from, None) };
    }
    Some(Back { buffer, footprints, rows, mips, layout: target.layout, ticket: target.ticket })
}

/// Once the list has run, tile its texels back into the same title allocation.
pub(crate) fn land(back: &Back) {
    if free() && land_free(back) {
        return;
    }
    let _mapping = crate::watch::title_memory_reading();
    let _ticket_guard = TICKETS.lock().unwrap_or_else(|e| e.into_inner());
    if !back.ticket.valid.load(Ordering::Acquire) {
        stale();
        return;
    }
    let (at, end) = back.range();
    let size = end - at;
    let mut tiled = vec![0u8; size];
    // Keep title bytes the subresources do not cover.
    if !crate::host::read_into(at as *const u8, &mut tiled) {
        return;
    }
    if !tile_into(back, &mut tiled) {
        return;
    }
    crate::host::copy_mapped(at as *mut u8, tiled.as_ptr(), size, true);
    landed(size);
}

/// Texture read-backs landed, and those left for a stale ticket.
static COPIED: AtomicU64 = AtomicU64::new(0);
static STALE: AtomicU64 = AtomicU64::new(0);

fn stale() {
    let skipped = STALE.fetch_add(1, Ordering::Relaxed) + 1;
    if skipped.is_power_of_two() {
        tracing::info!(skipped, "skipping texture readbacks whose title allocation changed");
    }
}

fn landed(size: usize) {
    let copied = COPIED.fetch_add(1, Ordering::Relaxed) + 1;
    if copied == 1 || copied.is_multiple_of(128) {
        tracing::info!(copied, bytes = size, "texture readback landed");
    }
}

/// [`land`] without the runtime's lock on changes of mapping ([`free`]).
/// The texels are read back and tiled with no lock held; the ticket's last
/// look and the write into title memory are done under [`TICKETS`], which
/// the runtime takes ([`remapped`]) before it unmaps a page and after it
/// maps one: no change of the ticket's pages comes between them, and one
/// that came before has made the ticket stale. The write goes through the
/// kernel, once, so that nothing here faults into a handler that waits for
/// a change of mapping (which may be waiting for [`TICKETS`]).
///
/// True if the read-back was landed or left as stale (or could not be
/// tiled, as in [`land`]); false if it could not be done so (title memory
/// away for a moment while the runtime changes a mapping beside it, a guard
/// put back meanwhile): the caller lands it the old way.
fn land_free(back: &Back) -> bool {
    if !back.ticket.valid.load(Ordering::Acquire) {
        stale();
        return true;
    }
    let (at, end) = back.range();
    let size = end - at;
    let mut tiled = vec![0u8; size];
    // Keep title bytes the subresources do not cover.
    if !crate::host::read_into(at as *const u8, &mut tiled) {
        return false;
    }
    if !tile_into(back, &mut tiled) {
        return true;
    }
    // The kernel's write must not meet a guarded block: they are opened
    // first, with no lock of ours held (a refresh that guards one again
    // meanwhile fails the write below, and the old way lands it).
    crate::watch::writing(at, size);
    let written = {
        let _tickets = TICKETS.lock().unwrap_or_else(|e| e.into_inner());
        if !back.ticket.valid.load(Ordering::Acquire) {
            None
        } else {
            Some(crate::host::write_title_once(at as *mut u8, tiled.as_ptr(), size))
        }
    };
    match written {
        None => {
            stale();
            true
        }
        Some(true) => {
            landed(size);
            true
        }
        Some(false) => false,
    }
}

/// Tiles the texels `back` read back into `tiled`, the texture's title
/// memory as it is laid out there; false if they could not be (the
/// read-back buffer not mapped, or a level not tiled: said once).
fn tile_into(back: &Back, tiled: &mut [u8]) -> bool {
    let mut data = std::ptr::null_mut();
    // SAFETY: our read-back buffer, written by the list that has run.
    unsafe {
        if back.buffer.Map(0, None, Some(&mut data)).is_err() || data.is_null() {
            return false;
        }
    }
    for (i, (footprint, &rows)) in back.footprints.iter().zip(&back.rows).enumerate() {
        let (mip, slice) = (i as u32 % back.mips, i as u32 / back.mips);
        let f = footprint.Footprint;
        let pitch = f.RowPitch as usize;
        let len = pitch * rows as usize * f.Depth as usize;
        // SAFETY: the subresource's part of the mapped buffer.
        let linear = unsafe { std::slice::from_raw_parts(data.cast::<u8>().add(footprint.Offset as usize), len) };
        if let Err(e) = back.layout.tile(&mut *tiled, mip, slice, linear, pitch, pitch * rows as usize) {
            static ONCE: std::sync::Once = std::sync::Once::new();
            ONCE.call_once(|| tracing::warn!(?e, mip, slice, "a texture copied back could not be tiled"));
            // SAFETY: mapped above.
            unsafe { back.buffer.Unmap(0, None) };
            return false;
        }
    }
    // SAFETY: mapped above.
    unsafe { back.buffer.Unmap(0, None) };
    true
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_readbacks_for_changed_allocations_are_invalidated() {
        let first = track(91, 0x1000, 0x2000);
        let next = track(92, 0x2000, 0x3000);
        let far = track(93, 0x4000, 0x5000);

        remapped(0x0f00, 0x200);
        assert!(!first.valid.load(Ordering::Acquire));
        assert!(next.valid.load(Ordering::Acquire));
        replaced(0x2000);
        assert!(!next.valid.load(Ordering::Acquire));
        assert!(far.valid.load(Ordering::Acquire));

        released(93);
        assert!(!far.valid.load(Ordering::Acquire));
        assert!(track(94, 0x2000, 0x3000).valid.load(Ordering::Acquire));
    }

    /// [`Target::new`] with [`free`]: a ticket is not kept if an
    /// invalidation was asked for while the texture's place was read; one
    /// that is kept is invalidated by a later change of its pages.
    #[test]
    fn tickets_tracked_without_the_mapping_lock_miss_no_invalidation() {
        const AT: usize = 0x7_7000_0000;
        TRACKING.store(true, Ordering::SeqCst);
        let since = ASKED.load(Ordering::SeqCst);
        // An invalidation between the read of the place and the ticket.
        remapped(AT, 0x1000);
        assert!(track_since(191, AT, AT + 0x1000, since).is_none());
        // Nothing in between (other tests may invalidate meanwhile: tried
        // again, as `Target::new` does).
        let ticket = (0..1000)
            .find_map(|_| track_since(192, AT, AT + 0x1000, ASKED.load(Ordering::SeqCst)))
            .expect("a ticket with no invalidation in between");
        assert!(ticket.valid.load(Ordering::Acquire));
        remapped(AT + 0x2000, 0x1000);
        assert!(ticket.valid.load(Ordering::Acquire));
        remapped(AT + 0xff0, 0x20);
        assert!(!ticket.valid.load(Ordering::Acquire));
    }

    /// The kernel's write [`land`] uses with [`free`]: it fails, with no
    /// fault, into a page that is read-only or not there, and writes all
    /// the bytes into one that is writable.
    #[test]
    fn the_kernels_write_fails_on_pages_it_cannot_write() {
        use windows_sys::Win32::System::Memory::{
            MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_NOACCESS, PAGE_READONLY, PAGE_READWRITE, VirtualAlloc,
            VirtualFree, VirtualProtect,
        };
        const PAGE: usize = 0x1000;
        let bytes: Vec<u8> = (0..2 * PAGE).map(|i| i as u8).collect();
        // SAFETY: a fresh reservation of three pages, the first two committed.
        let base = unsafe {
            let base = VirtualAlloc(std::ptr::null(), 3 * PAGE, MEM_RESERVE, PAGE_NOACCESS);
            assert!(!base.is_null());
            assert!(!VirtualAlloc(base, 2 * PAGE, MEM_COMMIT, PAGE_READWRITE).is_null());
            base.cast::<u8>()
        };
        assert!(crate::host::write_title_once(base, bytes.as_ptr(), 2 * PAGE));
        // SAFETY: committed, readable pages of ours.
        assert_eq!(unsafe { std::slice::from_raw_parts(base, 2 * PAGE) }, &bytes[..]);
        // Into the third page, reserved only.
        // SAFETY: the kernel checks the range.
        assert!(!crate::host::write_title_once(unsafe { base.add(PAGE) }, bytes.as_ptr(), 2 * PAGE));
        let mut old = 0;
        // SAFETY: our own committed page.
        assert!(unsafe { VirtualProtect(base.cast(), PAGE, PAGE_READONLY, &mut old) } != 0);
        assert!(!crate::host::write_title_once(base, bytes.as_ptr(), PAGE));
        assert!(crate::host::write_title_once(base, std::ptr::null(), 0));
        // SAFETY: the reservation made above.
        unsafe { VirtualFree(base.cast(), 0, MEM_RELEASE) };
    }
}
