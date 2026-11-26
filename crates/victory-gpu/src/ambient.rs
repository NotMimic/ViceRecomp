//! A stand-in for indirect light while rays do not run.
//!
//! The title's indirect diffuse light comes from probes it fills by tracing
//! rays, which do not run here yet: the program that resolves them
//! (`CS_ScreenProbes_TemporalResolve_*`) then leaves next to nothing, and
//! whatever the sun does not reach directly is black. With
//! `VICTORY_AMBIENT=<level>` the image that program wrote is replaced, right
//! after it, by a stand-in. The title's own lighting then applies it with
//! the surfaces' colors.
//! Replaced, not raised: without rays the resolve's inputs are never
//! written, and what it leaves is whatever their memory held.
//!
//! The stand-in is the level times a shape read from the title's bent
//! cones (`CS_ScreenSpaceBentCones*`, which need no rays and run every
//! frame): less on what faces down, less where the cones are short. One
//! constant for every pixel (as this was at first, and still is with
//! `VICTORY_AMBIENT_SHAPE=0`, or while no cones of the light's size were
//! written lately) drew walls, clothes and faces flat: runs st-pr3-amb and
//! st-ae-e1 against st-pr3-fast, which has no stand-in.
//!
//! Off by default (and with `VICTORY_AMBIENT=off` or `0`): the title's own
//! output stays.
//! shortcut: the same light from everywhere, whatever the title's exposure
//! and whatever lamps or windows are near; of no use once rays run.
//!
//! From Belobug's VIctory-Belobug-AMD (`crates/victory-gpu/src/ambient.rs`
//! there, where it is on by default), fitted to this tree; the shape is
//! this tree's.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
use windows::Win32::Graphics::Direct3D::ID3DBlob;
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::core::s;

use crate::host::{HostList, host};
use crate::object::This;

/// What both kinds of program's names start with.
const NAMES: &[u8] = b"CS_Screen";
/// The programs that write the indirect light, by the start of their name.
const RESOLVE: &[u8] = b"CS_ScreenProbes_TemporalResolve";
/// The programs that write the bent cones, by the start of their name, and
/// the one of them that does not (it writes a small image from theirs).
const CONES_PROGRAM: &[u8] = b"CS_ScreenSpaceBentCones";
const NOT_CONES: &[u8] = b"CS_ScreenSpaceBentCones_DisocclusionDilate";
/// Where the resolve writes the indirect diffuse light (R9G9B9E5 as 32-bit
/// integers): root parameter and place in its table, in dwords.
pub(crate) const LIGHT: (u8, u32) = (1, 176);
/// Where `CS_ScreenSpaceBentConesCheckerboard` writes the cones at the
/// light's size (R10G10B10A2), the same way. Read from one frame (capture
/// face2, frame 3118): a direction in the world as `n * 0.5 + 0.5` with z
/// up, of length 1 on half the pixels; the shorter ones (under 0.76 on a
/// quarter of the pixels, under 0.25 on a twentieth) lie scattered pixel by
/// pixel along outlines, in hair and on some surfaces, a stool's top among
/// them.
pub(crate) const CONES: (u8, u32) = (1, 56);

/// What a program is to the stand-in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Part {
    /// It writes the indirect light, which the stand-in replaces.
    Resolve,
    /// It writes the bent cones, which shape the stand-in.
    Cones,
}

/// What the program in the container `bytes` is to the stand-in, by its
/// name, wherever it stands in the container.
pub(crate) fn part(bytes: &[u8]) -> Option<Part> {
    bytes
        .windows(NAMES.len())
        .enumerate()
        .filter(|(_, w)| *w == NAMES)
        .find_map(|(at, _)| {
            let name = &bytes[at..];
            if name.starts_with(RESOLVE) {
                Some(Part::Resolve)
            } else if name.starts_with(CONES_PROGRAM) && !name.starts_with(NOT_CONES) {
                Some(Part::Cones)
            } else {
                None
            }
        })
}

/// The level (`VICTORY_AMBIENT`); `None` if off, and until
/// `VICTORY_AMBIENT_FROM=<seconds>` have passed since the start, if that is
/// set: one run then shows a scene without the stand-in and with it.
pub(crate) fn level() -> Option<f32> {
    static LEVEL: OnceLock<(Option<f32>, u64)> = OnceLock::new();
    let (level, from_ms) = *LEVEL.get_or_init(|| {
        let from = std::env::var("VICTORY_AMBIENT_FROM").ok().and_then(|v| v.trim().parse::<u64>().ok());
        (level_of(&std::env::var("VICTORY_AMBIENT").unwrap_or_default()), from.unwrap_or(0).saturating_mul(1000))
    });
    level.filter(|_| crate::host::now_ms() >= from_ms)
}

/// The level `set` asks for: a positive finite number, else none.
fn level_of(set: &str) -> Option<f32> {
    set.trim().parse().ok().filter(|l: &f32| l.is_finite() && *l > 0.0)
}

/// Whether the stand-in follows the title's bent cones: yes unless
/// `VICTORY_AMBIENT_SHAPE=0` (one constant, as before).
fn shaped() -> bool {
    static SHAPED: OnceLock<bool> = OnceLock::new();
    *SHAPED.get_or_init(|| std::env::var("VICTORY_AMBIENT_SHAPE").map_or(true, |v| v.trim() != "0"))
}

/// The share of the level a surface facing straight down gets.
const BELOW: f32 = 0.4;
/// How long after the cones were read a shape still stands for them, in
/// milliseconds: longer than a slow frame, shorter than a change of scene.
const FRESH_MS: u64 = 1000;

/// The constants of both programs and the views they share.
const SHARED: &str = r#"
#define RS "RootConstants(num32BitConstants=8, b0), DescriptorTable(UAV(u0, numDescriptors=2))"
cbuffer Light : register(b0) { float level; float3 tint; float below; uint shaped; };
"#;

/// After the title's cones: our image of what share of the level a pixel
/// gets. The mean of the nine pixels around: the short cones lie scattered
/// pixel by pixel ([`CONES`]).
const SHAPE: &str = r#"
RWTexture2D<float4> cones : register(u0);
RWTexture2D<float> shape : register(u1);
float share(int2 at) {
    float3 bent = cones[at].xyz * 2.0 - 1.0;
    float open = length(bent);
    float up = open > 1e-3 ? bent.z / open : 0.0;
    return saturate(open) * lerp(below, 1.0, up * 0.5 + 0.5);
}
[RootSignature(RS)]
[numthreads(8, 8, 1)]
void main(uint3 at : SV_DispatchThreadID) {
    uint w, h;
    shape.GetDimensions(w, h);
    if (at.x >= w || at.y >= h) return;
    int2 most = int2(w, h) - 1;
    float sum = 0.0;
    for (int y = -1; y <= 1; y++) {
        for (int x = -1; x <= 1; x++) {
            sum += share(clamp(int2(at.xy) + int2(x, y), int2(0, 0), most));
        }
    }
    shape[at.xy] = sum / 9.0;
}
"#;

/// After the title's resolve: the light it wrote, replaced.
const REPLACE: &str = r#"
RWTexture2D<uint> light : register(u0);
RWTexture2D<float> shape : register(u1);
uint pack(float3 c) {
    c = clamp(c, 0.0, 65408.0);
    float most = max(c.r, max(c.g, c.b));
    int e = clamp(int(floor(log2(max(most, 1e-9)))) + 16, 0, 31);
    float scale = exp2(float(24 - e));
    if (most * scale >= 511.5) { e = min(e + 1, 31); scale = exp2(float(24 - e)); }
    uint3 m = min(uint3(c * scale + 0.5), 511u);
    return m.r | (m.g << 9) | (m.b << 18) | (uint(e) << 27);
}
[RootSignature(RS)]
[numthreads(8, 8, 1)]
void main(uint3 at : SV_DispatchThreadID) {
    uint w, h;
    light.GetDimensions(w, h);
    if (at.x >= w || at.y >= h) return;
    // What the resolve left is not kept: without rays its inputs are never
    // written and it holds whatever their memory did (bright magenta on
    // translucent surfaces, blocks of colours indoors).
    float share = shaped != 0 ? shape[at.xy] : 1.0;
    light[at.xy] = pack(tint * (level * share));
}
"#;

/// Views, used in turn, two a pass: more than the passes of every list in
/// flight.
const SLOTS: u32 = 1024;

struct Pass {
    root: ID3D12RootSignature,
    replace: ID3D12PipelineState,
    /// None if the card does not read the cones' format through an image
    /// view: the stand-in is then one constant.
    shape: Option<ID3D12PipelineState>,
    views: ID3D12DescriptorHeap,
    step: u32,
    next: AtomicU32,
}
// SAFETY: D3D12 objects are free-threaded.
unsafe impl Send for Pass {}
// SAFETY: as above.
unsafe impl Sync for Pass {}

/// Our image of the shape for one size of the title's light.
struct Shape {
    width: u64,
    height: u32,
    image: ID3D12Resource,
    /// When the title's cones were last read into it (`now_ms`).
    read: AtomicU64,
}
// SAFETY: a D3D12 object (free-threaded) and plain numbers.
unsafe impl Send for Shape {}
// SAFETY: as above.
unsafe impl Sync for Shape {}

/// The shapes, one a size; the title has a few views of the world at most.
static SHAPES: Mutex<Vec<Arc<Shape>>> = Mutex::new(Vec::new());
const SIZES: usize = 4;

/// `program` compiled with the system's compiler.
fn compile(program: &str) -> windows::core::Result<Vec<u8>> {
    let source = format!("{SHARED}{program}");
    let mut code: Option<ID3DBlob> = None;
    let mut errors: Option<ID3DBlob> = None;
    // SAFETY: the source and names live through the call; out pointers ours.
    let compiled = unsafe {
        D3DCompile(
            source.as_ptr().cast(),
            source.len(),
            s!("ambient"),
            None,
            None,
            s!("main"),
            s!("cs_5_1"),
            0,
            0,
            &mut code,
            Some(&mut errors),
        )
    };
    let text = |b: &ID3DBlob| {
        // SAFETY: the blob's own bytes.
        unsafe { std::slice::from_raw_parts(b.GetBufferPointer().cast::<u8>(), b.GetBufferSize()) }
            .to_vec()
    };
    if let Err(e) = compiled {
        if let Some(errors) = errors {
            tracing::warn!("the stand-in for indirect light does not compile: {}", String::from_utf8_lossy(&text(&errors)));
        }
        return Err(e);
    }
    Ok(text(&code.ok_or_else(windows::core::Error::empty)?))
}

/// Whether a program may read `format` through an image view on this card.
fn reads_images_of(device: &ID3D12Device, format: DXGI_FORMAT) -> bool {
    let mut options = D3D12_FEATURE_DATA_D3D12_OPTIONS::default();
    let mut support = D3D12_FEATURE_DATA_FORMAT_SUPPORT {
        Format: format,
        ..Default::default()
    };
    // SAFETY: plain queries into structures of our own.
    unsafe {
        device
            .CheckFeatureSupport(D3D12_FEATURE_D3D12_OPTIONS, (&raw mut options).cast(), size_of_val(&options) as u32)
            .is_ok()
            && options.TypedUAVLoadAdditionalFormats.as_bool()
            && device
                .CheckFeatureSupport(D3D12_FEATURE_FORMAT_SUPPORT, (&raw mut support).cast(), size_of_val(&support) as u32)
                .is_ok()
            && support.Support2.0 & D3D12_FORMAT_SUPPORT2_UAV_TYPED_LOAD.0 != 0
    }
}

fn make(device: &ID3D12Device) -> windows::core::Result<Pass> {
    let replace = compile(REPLACE)?;
    let pipeline = |root: &ID3D12RootSignature, code: &[u8]| {
        // SAFETY: a live root signature and the program's own bytes.
        unsafe {
            device.CreateComputePipelineState::<ID3D12PipelineState>(&D3D12_COMPUTE_PIPELINE_STATE_DESC {
                pRootSignature: std::mem::transmute_copy(root),
                CS: D3D12_SHADER_BYTECODE {
                    pShaderBytecode: code.as_ptr().cast(),
                    BytecodeLength: code.len(),
                },
                ..Default::default()
            })
        }
    };
    // SAFETY: the root signature is inside the program; plain creations.
    unsafe {
        let root: ID3D12RootSignature = device.CreateRootSignature(0, &replace)?;
        let shape = if reads_images_of(device, DXGI_FORMAT_R10G10B10A2_UNORM) {
            Some(pipeline(&root, &compile(SHAPE)?)?)
        } else {
            tracing::info!("the stand-in for indirect light is one constant: this card does not read the bent cones' format through an image view");
            None
        };
        let kind = D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV;
        let views = device.CreateDescriptorHeap(&D3D12_DESCRIPTOR_HEAP_DESC {
            Type: kind,
            NumDescriptors: SLOTS,
            Flags: D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE,
            NodeMask: 0,
        })?;
        Ok(Pass {
            replace: pipeline(&root, &replace)?,
            root,
            shape,
            views,
            step: device.GetDescriptorHandleIncrementSize(kind),
            next: AtomicU32::new(0),
        })
    }
}

/// The pass, made at its first use; none if it could not be made.
fn pass() -> Option<&'static Pass> {
    static PASS: OnceLock<Option<Pass>> = OnceLock::new();
    PASS.get_or_init(|| {
        make(&host()?.device)
            .inspect_err(|e| tracing::warn!(error = %e, "no stand-in for indirect light (VICTORY_AMBIENT)"))
            .ok()
    })
    .as_ref()
}

/// Our image of the shape for a light of `width` by `height`; with `make`,
/// made if there is none (the one read longest ago gives way past
/// [`SIZES`]; lists that use it keep it).
fn shape_for(device: &ID3D12Device, width: u64, height: u32, make: bool) -> Option<Arc<Shape>> {
    let mut shapes = SHAPES.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(shape) = shapes.iter().find(|s| (s.width, s.height) == (width, height)) {
        return Some(shape.clone());
    }
    if !make {
        return None;
    }
    let heap = D3D12_HEAP_PROPERTIES {
        Type: D3D12_HEAP_TYPE_DEFAULT,
        ..Default::default()
    };
    let desc = D3D12_RESOURCE_DESC {
        Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
        Width: width,
        Height: height,
        DepthOrArraySize: 1,
        MipLevels: 1,
        Format: DXGI_FORMAT_R32_FLOAT,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Flags: D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
        ..Default::default()
    };
    let mut image: Option<ID3D12Resource> = None;
    // SAFETY: plain creation.
    unsafe {
        device
            .CreateCommittedResource(
                &heap,
                D3D12_HEAP_FLAG_NONE,
                &desc,
                D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
                None,
                &mut image,
            )
            .inspect_err(|e| tracing::warn!(error = %e, width, height, "no image for the shape of the stand-in for indirect light"))
            .ok()?;
    }
    let shape = Arc::new(Shape {
        width,
        height,
        image: image.inspect(|i| crate::cardmem::enter(i, crate::cardmem::Owner::Ambient))?,
        read: AtomicU64::new(0),
    });
    if shapes.len() >= SIZES {
        let oldest = (0..shapes.len()).min_by_key(|&i| shapes[i].read.load(Ordering::Relaxed))?;
        shapes.swap_remove(oldest);
    }
    shapes.push(shape.clone());
    Some(shape)
}

fn uav_barrier(list: &ID3D12GraphicsCommandList) {
    let barrier = D3D12_RESOURCE_BARRIER {
        Type: D3D12_RESOURCE_BARRIER_TYPE_UAV,
        ..Default::default()
    };
    // SAFETY: an open list; the barrier names no resource.
    unsafe { list.ResourceBarrier(&[barrier]) };
}

/// Records `pipeline` of the pass over `width` by `height` in `list`, with
/// image views of `first` (in `format`) and `second` (ours, or none) and
/// the constants.
///
/// # Safety
/// `list` is open; `first` and `second` are live, kept by the list, and in
/// the unordered-access state.
unsafe fn run(
    list: &HostList,
    pass: &Pass,
    pipeline: &ID3D12PipelineState,
    (first, format): (&ID3D12Resource, DXGI_FORMAT),
    second: Option<&ID3D12Resource>,
    (width, height): (u64, u32),
    constants: [u32; 6],
) {
    let Some(host) = host() else {
        return;
    };
    // Two slots that no list in flight reads.
    let slot = pass.next.fetch_add(1, Ordering::Relaxed) % (SLOTS / 2) * 2;
    let l = &list.list;
    uav_barrier(l);
    // SAFETY: by the caller, and the slots are the pass's own.
    unsafe {
        let cpu = pass.views.GetCPUDescriptorHandleForHeapStart();
        let gpu = pass.views.GetGPUDescriptorHandleForHeapStart();
        let at = |slot: u32| D3D12_CPU_DESCRIPTOR_HANDLE {
            ptr: cpu.ptr + (slot * pass.step) as usize,
        };
        let view = |format| D3D12_UNORDERED_ACCESS_VIEW_DESC {
            Format: format,
            ViewDimension: D3D12_UAV_DIMENSION_TEXTURE2D,
            ..Default::default()
        };
        host.device
            .CreateUnorderedAccessView(first, None::<&ID3D12Resource>, Some(&view(format)), at(slot));
        host.device.CreateUnorderedAccessView(
            second,
            None::<&ID3D12Resource>,
            Some(&view(DXGI_FORMAT_R32_FLOAT)),
            at(slot + 1),
        );
        l.SetDescriptorHeaps(&[Some(pass.views.clone())]);
        l.SetComputeRootSignature(&pass.root);
        l.SetPipelineState(pipeline);
        l.SetComputeRoot32BitConstants(0, constants.len() as u32, constants.as_ptr().cast(), 0);
        l.SetComputeRootDescriptorTable(
            1,
            D3D12_GPU_DESCRIPTOR_HANDLE {
                ptr: gpu.ptr + u64::from(slot * pass.step),
            },
        );
        l.Dispatch((width as u32).div_ceil(8), height.div_ceil(8), 1);
    }
    uav_barrier(l);
}

/// The constants of both programs: the level, the tint, [`BELOW`] and
/// whether a shape is read.
fn constants(level: f32, shaped: bool) -> [u32; 6] {
    [
        level.to_bits(),
        1.0f32.to_bits(),
        0.98f32.to_bits(),
        0.94f32.to_bits(),
        BELOW.to_bits(),
        u32::from(shaped),
    ]
}

/// Reads the title's bent cones `cones` into our shape of their size, in
/// `list`, after the program that wrote them as an image. Whether it
/// recorded anything (the list's descriptor heap, compute root signature
/// and pipeline are then ours).
pub(crate) fn shape(list: &HostList, cones: This) -> bool {
    let (Some(level), true, Some(host)) = (level(), shaped(), host()) else {
        return false;
    };
    let Some((pass, pipeline)) = pass().and_then(|p| Some((p, p.shape.as_ref()?))) else {
        return false;
    };
    let Some(cones) = crate::resource::host_resource(cones) else {
        return false;
    };
    // SAFETY: a live resource.
    let d = unsafe { cones.GetDesc() };
    static ONCE: std::sync::Once = std::sync::Once::new();
    let ours = d.Dimension == D3D12_RESOURCE_DIMENSION_TEXTURE2D
        && matches!(d.Format, DXGI_FORMAT_R10G10B10A2_UNORM | DXGI_FORMAT_R10G10B10A2_TYPELESS);
    if !ours {
        ONCE.call_once(|| tracing::warn!(format = d.Format.0, "the stand-in for indirect light is one constant: the bent cones have another format"));
        return false;
    }
    let Some(shape) = shape_for(&host.device, d.Width, d.Height, true) else {
        return false;
    };
    ONCE.call_once(|| {
        tracing::info!(width = d.Width, height = d.Height, "the stand-in for indirect light follows the title's bent cones");
    });
    shape.read.store(crate::host::now_ms(), Ordering::Relaxed);
    list.keep(cones);
    list.keep(&shape.image);
    // SAFETY: an open list; the cones as the program before left them, and
    // our image, which is only ever in that state; both kept by the list.
    unsafe {
        run(
            list,
            pass,
            pipeline,
            (cones, DXGI_FORMAT_R10G10B10A2_UNORM),
            Some(&shape.image),
            (d.Width, d.Height),
            constants(level, true),
        );
    }
    true
}

/// Replaces what `light` holds by the stand-in, in `list`, after the
/// program that wrote it as an image. Whether it recorded anything (the
/// list's descriptor heap, compute root signature and pipeline are then
/// ours).
pub(crate) fn replace(list: &HostList, light: This) -> bool {
    let (Some(level), Some(host), Some(pass)) = (level(), host(), pass()) else {
        return false;
    };
    let Some(light) = crate::resource::host_resource(light) else {
        return false;
    };
    // SAFETY: a live resource.
    let d = unsafe { light.GetDesc() };
    static ONCE: std::sync::Once = std::sync::Once::new();
    if d.Format != DXGI_FORMAT_R32_UINT {
        ONCE.call_once(|| tracing::warn!(format = d.Format.0, "no stand-in for indirect light (VICTORY_AMBIENT): its image has another format"));
        return false;
    }
    ONCE.call_once(|| {
        tracing::info!(level, width = d.Width, height = d.Height, "indirect light is replaced by a stand-in (VICTORY_AMBIENT)");
    });
    // The cones of this frame were read by a list submitted before this
    // one; a shape nothing read into lately is of another scene.
    let shape = shape_for(&host.device, d.Width, d.Height, false)
        .filter(|s| crate::host::now_ms().saturating_sub(s.read.load(Ordering::Relaxed)) < FRESH_MS);
    list.keep(light);
    if let Some(shape) = &shape {
        list.keep(&shape.image);
    }
    // SAFETY: an open list; the light as the program before left it, and
    // our image, which is only ever in that state; both kept by the list.
    unsafe {
        run(
            list,
            pass,
            &pass.replace,
            (light, DXGI_FORMAT_R32_UINT),
            shape.as_ref().map(|s| &s.image),
            (d.Width, d.Height),
            constants(level, shape.is_some()),
        );
    }
    true
}

#[cfg(test)]
mod tests {
    use super::Part;

    /// Off unless a positive level is asked for.
    #[test]
    fn the_level_is_asked_for() {
        assert_eq!(super::level_of(""), None);
        assert_eq!(super::level_of("off"), None);
        assert_eq!(super::level_of("0"), None);
        assert_eq!(super::level_of("-1"), None);
        assert_eq!(super::level_of("nan"), None);
        assert_eq!(super::level_of(" 0.6 "), Some(0.6));
    }

    /// The programs are found by name, anywhere in the container; the one
    /// that only reads the cones is not one of them.
    #[test]
    fn the_programs_are_found_by_their_names() {
        assert_eq!(super::part(b"....CS_ScreenProbes_TemporalResolve_Wave64\0"), Some(Part::Resolve));
        assert_eq!(super::part(b"..CS_ScreenSpaceBentConesCheckerboard_d66945e6_Wrapped\0"), Some(Part::Cones));
        assert_eq!(super::part(b"..CS_ScreenSpaceBentCones_d66945e6_Wrapped\0"), Some(Part::Cones));
        assert_eq!(super::part(b"..CS_ScreenSpaceBentCones_DisocclusionDilate_f45318ef\0"), None);
        assert_eq!(super::part(b"CS_Screen....CS_ScreenProbes_ActivateRays\0"), None);
        assert_eq!(super::part(b"CS_ScreenX CS_ScreenProbes_TemporalResolve\0"), Some(Part::Resolve));
        assert_eq!(super::part(b""), None);
    }

    /// Both programs compile with the system's compiler and the host takes
    /// them.
    #[test]
    fn ambient_pass_builds() {
        let Some(host) = crate::host::host() else {
            return;
        };
        super::make(&host.device).expect("the ambient pass");
    }
}
