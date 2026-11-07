//! The window's picture while the engine renders to OpenXR: the left eye,
//! copied into the engine's own window swapchain and presented.
//!
//! An XR engine never calls `vkQueuePresentKHR`. The Quest build creates a
//! swapchain on the window surface (1280x721 in every run so far) and then
//! draws only into the runtime's images, so the Wayland subsurface kept
//! whatever it last showed and the window read as frozen. Nothing was wrong
//! with the engine; nobody was presenting.
//!
//! **Where it runs, and why there.** At `xrReleaseSwapchainImage` of the
//! left-eye swapchain, on the thread releasing it, before the call is
//! forwarded -- the same point `cordial_screenshot` reads the eye at
//! (`guest_xr::release`). The image is complete there and still the engine's,
//! in `COLOR_ATTACHMENT_OPTIMAL`. It is also the one moment the session's
//! queue is free to use without a lock: OpenXR makes the *application*
//! externally synchronise that queue across `xrBeginFrame`, `xrEndFrame`,
//! `xrAcquireSwapchainImage` and `xrReleaseSwapchainImage`, because the
//! runtime may submit on it inside them (OpenXR 1.1 §12.25.3). So while this
//! thread is inside the release, no other engine thread may be touching the
//! queue, and a submit here is serialised with the engine's by the engine's own
//! contract. A second thread or a second queue would need a lock the engine
//! does not know to take.
//!
//! **It must never slow the XR loop, so it never waits on the GPU.** The
//! obvious shape -- acquire with a semaphore, submit waiting on it -- would put
//! a wait for the presentation engine *on the XR queue*, ahead of the
//! runtime's own work on it, and a slow compositor would stall the headset.
//! Instead the next window image is acquired one frame ahead with a *fence*,
//! and a frame is mirrored only when that fence has already signalled and the
//! image's previous blit has already completed; otherwise the frame is
//! skipped. The submit waits on nothing. Everything is created once and kept,
//! unlike the capture, because this runs up to once a frame.
//!
//! `CORDIAL_NO_XR_MIRROR=1` turns it off, and with it the two usage bits it
//! adds to the swapchains it reads and writes (`guest_xr` and `vulkan.rs`), so
//! the control is the build without it.

use std::ffi::{c_char, c_void};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

const VK_SUCCESS: i32 = 0;
const VK_SUBOPTIMAL_KHR: i32 = 1_000_001_003;
const VK_ERROR_OUT_OF_DATE_KHR: i32 = -1_000_001_004;

const ST_FENCE_CREATE_INFO: u32 = 8;
const ST_SEMAPHORE_CREATE_INFO: u32 = 9;
const ST_COMMAND_POOL_CREATE_INFO: u32 = 39;
const ST_COMMAND_BUFFER_ALLOCATE_INFO: u32 = 40;
const ST_COMMAND_BUFFER_BEGIN_INFO: u32 = 42;
const ST_IMAGE_MEMORY_BARRIER: u32 = 45;
const ST_SUBMIT_INFO: u32 = 4;
const ST_PRESENT_INFO_KHR: u32 = 1_000_001_001;

const FENCE_SIGNALED: u32 = 1;
const POOL_RESET_COMMAND_BUFFER: u32 = 2;
const CMD_ONE_TIME: u32 = 1;

const LAYOUT_UNDEFINED: i32 = 0;
const LAYOUT_COLOR_ATTACHMENT: i32 = 2;
const LAYOUT_TRANSFER_SRC: i32 = 6;
const LAYOUT_TRANSFER_DST: i32 = 7;
const LAYOUT_PRESENT_SRC: i32 = 1_000_001_002;

const ACCESS_COLOR_ATTACHMENT_READ: u32 = 0x80;
const ACCESS_COLOR_ATTACHMENT_WRITE: u32 = 0x100;
const ACCESS_TRANSFER_READ: u32 = 0x800;
const ACCESS_TRANSFER_WRITE: u32 = 0x1000;
const STAGE_TOP: u32 = 0x1;
const STAGE_TRANSFER: u32 = 0x1000;
const STAGE_BOTTOM: u32 = 0x2000;
const STAGE_ALL_COMMANDS: u32 = 0x1_0000;
const ASPECT_COLOR: u32 = 1;
const IGNORED: u32 = u32::MAX;
const FILTER_NEAREST: u32 = 0;
const FILTER_LINEAR: u32 = 1;

const FEATURE_BLIT_SRC: u32 = 0x400;
const FEATURE_BLIT_DST: u32 = 0x800;
const FEATURE_FILTER_LINEAR: u32 = 0x1000;

/// `VK_IMAGE_USAGE_TRANSFER_DST_BIT`, which `vulkan.rs` adds to the window
/// swapchain in an XR session so the blit may write it.
pub const USAGE_TRANSFER_DST: u32 = 0x2;
/// `XR_SWAPCHAIN_USAGE_TRANSFER_SRC_BIT`, which `guest_xr` adds to the
/// engine's colour swapchains so the blit may read them. Monado creates its
/// images with exactly the usage asked for (`vk_compositor_flags.c`), and the
/// engine asks for `COLOR_ATTACHMENT | SAMPLED` (0x21).
pub const XR_USAGE_TRANSFER_SRC: u64 = 0x8;

// Laid out by hand, as in `capture.rs`, to the specification's C layout.

#[repr(C)]
struct SimpleCreateInfo {
    s_type: u32,
    next: *const c_void,
    flags: u32,
}

#[repr(C)]
struct PoolCreateInfo {
    s_type: u32,
    next: *const c_void,
    flags: u32,
    family: u32,
}

#[repr(C)]
struct CmdAllocInfo {
    s_type: u32,
    next: *const c_void,
    pool: u64,
    level: u32,
    count: u32,
}

#[repr(C)]
struct CmdBeginInfo {
    s_type: u32,
    next: *const c_void,
    flags: u32,
    inheritance: *const c_void,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Range {
    aspect: u32,
    base_mip: u32,
    mips: u32,
    base_layer: u32,
    layers: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Barrier {
    s_type: u32,
    next: *const c_void,
    src_access: u32,
    dst_access: u32,
    old_layout: i32,
    new_layout: i32,
    src_family: u32,
    dst_family: u32,
    image: u64,
    range: Range,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Layers {
    aspect: u32,
    mip: u32,
    base_layer: u32,
    layers: u32,
}

#[repr(C)]
struct Blit {
    src: Layers,
    src_offsets: [[i32; 3]; 2],
    dst: Layers,
    dst_offsets: [[i32; 3]; 2],
}

#[repr(C)]
struct Submit {
    s_type: u32,
    next: *const c_void,
    wait_count: u32,
    waits: *const u64,
    wait_stages: *const u32,
    cb_count: u32,
    cbs: *const u64,
    signal_count: u32,
    signals: *const u64,
}

#[repr(C)]
struct Present {
    s_type: u32,
    next: *const c_void,
    wait_count: u32,
    waits: *const u64,
    swapchain_count: u32,
    swapchains: *const u64,
    indices: *const u32,
    results: *mut i32,
}

type Gdpa = extern "C" fn(u64, *const c_char) -> *mut c_void;

struct Fns {
    get_device_queue: extern "C" fn(u64, u32, u32, *mut u64),
    get_swapchain_images: extern "C" fn(u64, u64, *mut u32, *mut u64) -> i32,
    acquire_next_image: extern "C" fn(u64, u64, u64, u64, u64, *mut u32) -> i32,
    create_fence: extern "C" fn(u64, *const SimpleCreateInfo, *const c_void, *mut u64) -> i32,
    create_semaphore: extern "C" fn(u64, *const SimpleCreateInfo, *const c_void, *mut u64) -> i32,
    get_fence_status: extern "C" fn(u64, u64) -> i32,
    reset_fences: extern "C" fn(u64, u32, *const u64) -> i32,
    create_command_pool: extern "C" fn(u64, *const PoolCreateInfo, *const c_void, *mut u64) -> i32,
    allocate_command_buffers: extern "C" fn(u64, *const CmdAllocInfo, *mut u64) -> i32,
    reset_command_buffer: extern "C" fn(u64, u32) -> i32,
    begin_command_buffer: extern "C" fn(u64, *const CmdBeginInfo) -> i32,
    end_command_buffer: extern "C" fn(u64) -> i32,
    cmd_pipeline_barrier:
        unsafe extern "C" fn(u64, u32, u32, u32, u32, *const c_void, u32, *const c_void, u32, *const Barrier),
    cmd_clear_color_image: extern "C" fn(u64, u64, i32, *const [f32; 4], u32, *const Range),
    cmd_blit_image: extern "C" fn(u64, u64, i32, u64, i32, u32, *const Blit, u32),
    queue_submit: extern "C" fn(u64, u32, *const Submit, u64) -> i32,
    queue_present: extern "C" fn(u64, *const Present) -> i32,
}

macro_rules! load {
    ($get:expr, $dev:expr, $name:literal) => {{
        let p = $get($dev, concat!($name, "\0").as_ptr() as *const c_char);
        if p.is_null() {
            return Err(concat!("the driver has no ", $name).to_string());
        }
        // SAFETY: the loader returned a function for exactly this name, whose
        // signature is the one the Vulkan specification gives it.
        unsafe { std::mem::transmute::<*mut c_void, _>(p) }
    }};
}

impl Fns {
    fn load(gdpa: Gdpa, d: u64) -> Result<Fns, String> {
        Ok(Fns {
            get_device_queue: load!(gdpa, d, "vkGetDeviceQueue"),
            get_swapchain_images: load!(gdpa, d, "vkGetSwapchainImagesKHR"),
            acquire_next_image: load!(gdpa, d, "vkAcquireNextImageKHR"),
            create_fence: load!(gdpa, d, "vkCreateFence"),
            create_semaphore: load!(gdpa, d, "vkCreateSemaphore"),
            get_fence_status: load!(gdpa, d, "vkGetFenceStatus"),
            reset_fences: load!(gdpa, d, "vkResetFences"),
            create_command_pool: load!(gdpa, d, "vkCreateCommandPool"),
            allocate_command_buffers: load!(gdpa, d, "vkAllocateCommandBuffers"),
            reset_command_buffer: load!(gdpa, d, "vkResetCommandBuffer"),
            begin_command_buffer: load!(gdpa, d, "vkBeginCommandBuffer"),
            end_command_buffer: load!(gdpa, d, "vkEndCommandBuffer"),
            cmd_pipeline_barrier: load!(gdpa, d, "vkCmdPipelineBarrier"),
            cmd_clear_color_image: load!(gdpa, d, "vkCmdClearColorImage"),
            cmd_blit_image: load!(gdpa, d, "vkCmdBlitImage"),
            queue_submit: load!(gdpa, d, "vkQueueSubmit"),
            // The host's own, not `vulkan.rs`'s counting wrapper: these are
            // Cordial's presents, not the engine's, and counting them there
            // would make `cordial_info`'s present count -- the wedge test --
            // read as though the engine were presenting.
            queue_present: load!(gdpa, d, "vkQueuePresentKHR"),
        })
    }
}

/// The window swapchain the engine created, as `vkCreateSwapchainKHR`
/// recorded it.
#[derive(Clone, Copy)]
struct Window {
    device: u64,
    surface: u64,
    swapchain: u64,
    width: u32,
    height: u32,
    format: u32,
    usage: u32,
}

/// One window image's own objects. Per image rather than per frame in flight
/// because a present's wait semaphore may be reused only once the image it
/// presented has come back, and "the image came back" is what the acquire
/// fence says.
struct Slot {
    cb: u64,
    done: u64,
    submitted: u64,
}

struct Live {
    fns: Fns,
    swapchain: u64,
    queue: u64,
    images: Vec<u64>,
    slots: Vec<Slot>,
    acquire_fence: u64,
    /// An image acquired a frame ahead, not yet known to be free.
    ahead: Option<u32>,
    filter: u32,
}

#[derive(Default)]
struct State {
    window: Option<Window>,
    /// A canvas size differing from the swapchain's, and since when; see
    /// [`resize_due`].
    pending: Option<((u32, u32), std::time::Instant)>,
    last_resize: Option<std::time::Instant>,
    live: Option<Live>,
    /// Why the mirror stopped, said once. A window swapchain that cannot be
    /// written is a setup fact, not a per-frame event.
    dead: Option<String>,
}

// SAFETY: plain handles and function pointers, only touched under `STATE`.
unsafe impl Send for State {}

static STATE: Mutex<Option<State>> = Mutex::new(None);

/// Set by `guest_xr` once `xrCreateSession` succeeds: the window swapchain
/// created after it belongs to an engine that will never present to it.
pub static SESSION: AtomicBool = AtomicBool::new(false);

pub static MIRRORED: AtomicU64 = AtomicU64::new(0);
pub static SKIPPED: AtomicU64 = AtomicU64::new(0);

pub fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        let off = std::env::var_os("CORDIAL_NO_XR_MIRROR").is_some_and(|v| !v.is_empty() && v != "0");
        if off {
            eprintln!("[android] xr mirror: off (CORDIAL_NO_XR_MIRROR)");
        }
        !off
    })
}

/// For `cordial_info`, beside the XR frame count.
pub fn info() -> String {
    format!("xr_mirrored={} xr_mirror_skipped={}", MIRRORED.load(Ordering::Relaxed), SKIPPED.load(Ordering::Relaxed))
}

/// From `vkCreateSwapchainKHR`, after it succeeds.
pub fn note_window_swapchain(device: u64, surface: u64, swapchain: u64, width: u32, height: u32, format: u32, usage: u32) {
    let mut g = STATE.lock().unwrap_or_else(|e| e.into_inner());
    let s = g.get_or_insert_with(State::default);
    s.window = Some(Window { device, surface, swapchain, width, height, format, usage });
    // A new swapchain means new images; the old slots' semaphores and command
    // buffers are left rather than destroyed, because nothing here can tell
    // whether the old swapchain's last present has finished waiting on them.
    // A handful of handles per window resize.
    s.live = None;
    s.dead = None;
}

/// From `vkDestroySwapchainKHR`, before it is forwarded: taking the lock is
/// what stops a mirror frame on the XR thread using a swapchain another thread
/// is destroying.
pub fn forget_swapchain(swapchain: u64) {
    let mut g = STATE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(s) = g.as_mut() {
        if s.window.is_some_and(|w| w.swapchain == swapchain) {
            s.window = None;
            s.live = None;
        }
    }
}

/// The left eye, as `guest_xr::release` found it.
pub struct Eye {
    pub physical_device: u64,
    pub queue_family: u32,
    pub queue_index: u32,
    pub image: u64,
    pub format: u32,
    pub layer: u32,
    pub rect: (i32, i32, u32, u32),
    /// The view's field of view as submitted: angleLeft, angleRight, angleUp,
    /// angleDown, in radians.
    pub fov: [f32; 4],
}

/// Mirror one frame, or skip it. Called on the thread inside
/// `xrReleaseSwapchainImage`; see the module doc for why that is the queue's
/// only safe moment.
pub fn frame(eye: &Eye) {
    let mut g = STATE.lock().unwrap_or_else(|e| e.into_inner());
    let Some(s) = g.as_mut() else { return };
    if s.dead.is_some() {
        return;
    }
    let Some(w) = s.window else { return };
    if let Some((width, height)) = resize_due(s, &w) {
        // Recreated without the lock: the create reports the new swapchain
        // back through `note_window_swapchain`, which takes it. Skipping this
        // frame is the cost of a resize.
        drop(g);
        match super::vulkan::recreate_mirror_swapchain(w.device, w.swapchain, width, height) {
            Ok(_) => eprintln!("[android] xr mirror: window swapchain {}x{} -> {width}x{height}", w.width, w.height),
            Err(e) => eprintln!("[android] xr mirror: could not follow the window to {width}x{height}: {e}"),
        }
        return;
    }
    if s.live.is_none() {
        match setup(&w, eye) {
            Ok(l) => {
                let deg = |f: [f32; 4]| f.map(|a| (a.to_degrees() * 10.0).round() / 10.0);
                eprintln!(
                    "[android] xr mirror: left eye field of view (left, right, up, down, degrees): {:?}",
                    deg(eye.fov)
                );
                eprintln!(
                    "[android] xr mirror: left eye {}x{} (VkFormat {}) into the window swapchain {}x{} \
                     (VkFormat {}, {} images), {} filter",
                    eye.rect.2, eye.rect.3, eye.format, w.width, w.height, w.format, l.images.len(),
                    if l.filter == FILTER_LINEAR { "linear" } else { "nearest" }
                );
                s.live = Some(l);
            }
            Err(e) => {
                eprintln!("[android] xr mirror: off: {e}");
                s.dead = Some(e);
                return;
            }
        }
    }
    let l = s.live.as_mut().unwrap();
    match mirror(l, &w, eye) {
        Ok(true) => {
            MIRRORED.fetch_add(1, Ordering::Relaxed);
        }
        Ok(false) => {
            SKIPPED.fetch_add(1, Ordering::Relaxed);
        }
        // Going fullscreen left the window swapchain out of date at a size the
        // canvas then kept, so no resize followed and a mirror that stopped
        // here stayed frozen while the headset carried on. The surface is
        // fine; the swapchain wants rebuilding at the size it already has.
        Err(e) if e == OUT_OF_DATE => {
            s.live = None;
            drop(g);
            let size = super::wayland::current().map(|c| c.geometry()).map_or((w.width, w.height), |c| {
                (c.0.max(1) as u32, c.1.max(1) as u32)
            });
            match super::vulkan::recreate_mirror_swapchain(w.device, w.swapchain, size.0, size.1) {
                Ok(_) => eprintln!("[android] xr mirror: window swapchain out of date, rebuilt at {}x{}", size.0, size.1),
                Err(e) => eprintln!("[android] xr mirror: window swapchain out of date, and rebuilding it failed: {e}"),
            }
        }
        Err(e) => {
            eprintln!("[android] xr mirror: stopped: {e}");
            s.live = None;
            s.dead = Some(e);
        }
    }
}

const OUT_OF_DATE: &str = "the window swapchain is out of date";

/// The canvas size to recreate the window swapchain at, when it is due.
///
/// A fallback, not the main path: the engine itself rebuilds its window
/// swapchain on most resizes, from the surface extent `vulkan.rs` reports,
/// and the log of a dragged resize showed it doing so at every step but one.
/// So the mirror waits [`SETTLE`] for a size to hold, or [`MAX_LAG`] since its
/// own last rebuild, before doing it itself; acting sooner would race the
/// engine to the same swapchain.
fn resize_due(s: &mut State, w: &Window) -> Option<(u32, u32)> {
    const SETTLE: std::time::Duration = std::time::Duration::from_millis(500);
    const MAX_LAG: std::time::Duration = std::time::Duration::from_millis(500);
    let canvas = super::wayland::current().map(|c| c.geometry())?;
    let size = (canvas.0.max(1) as u32, canvas.1.max(1) as u32);
    if size == (w.width, w.height) {
        s.pending = None;
        return None;
    }
    let now = std::time::Instant::now();
    let since = match s.pending {
        Some((p, t)) if p == size => t,
        _ => {
            s.pending = Some((size, now));
            now
        }
    };
    let lagging = s.last_resize.is_none_or(|t| now.duration_since(t) >= MAX_LAG);
    if now.duration_since(since) >= SETTLE || lagging && s.pending.is_some_and(|(_, t)| t != now) {
        s.pending = None;
        s.last_resize = Some(now);
        return Some(size);
    }
    None
}

fn setup(w: &Window, eye: &Eye) -> Result<Live, String> {
    if w.usage & USAGE_TRANSFER_DST == 0 {
        return Err(format!("the window swapchain's usage {:#x} does not allow a transfer into it", w.usage));
    }
    let gdpa = super::vulkan::device_proc_getter().ok_or("vkGetDeviceProcAddr was never resolved")?;
    let f = Fns::load(gdpa, w.device)?;
    let phys = eye.physical_device;

    let supported: extern "C" fn(u64, u32, u64, *mut u32) -> i32 =
        super::vulkan::instance_fn(c"vkGetPhysicalDeviceSurfaceSupportKHR")
            .map(|p| unsafe { std::mem::transmute::<*mut c_void, _>(p) })
            .ok_or("no vkGetPhysicalDeviceSurfaceSupportKHR")?;
    let mut ok = 0u32;
    if supported(phys, eye.queue_family, w.surface, &mut ok) != VK_SUCCESS || ok == 0 {
        return Err(format!("queue family {} cannot present to the window surface", eye.queue_family));
    }
    let props: extern "C" fn(u64, i32, *mut [u32; 3]) =
        super::vulkan::instance_fn(c"vkGetPhysicalDeviceFormatProperties")
            .map(|p| unsafe { std::mem::transmute::<*mut c_void, _>(p) })
            .ok_or("no vkGetPhysicalDeviceFormatProperties")?;
    let mut src = [0u32; 3];
    let mut dst = [0u32; 3];
    props(phys, eye.format as i32, &mut src);
    props(phys, w.format as i32, &mut dst);
    if src[1] & FEATURE_BLIT_SRC == 0 || dst[1] & FEATURE_BLIT_DST == 0 {
        return Err(format!("VkFormat {} -> {} cannot be blitted with optimal tiling", eye.format, w.format));
    }
    let filter = if src[1] & FEATURE_FILTER_LINEAR != 0 { FILTER_LINEAR } else { FILTER_NEAREST };

    let mut queue = 0u64;
    (f.get_device_queue)(w.device, eye.queue_family, eye.queue_index, &mut queue);
    let mut n = 0u32;
    if (f.get_swapchain_images)(w.device, w.swapchain, &mut n, std::ptr::null_mut()) != VK_SUCCESS {
        return Err("vkGetSwapchainImagesKHR failed to count".into());
    }
    let mut images = vec![0u64; n as usize];
    if (f.get_swapchain_images)(w.device, w.swapchain, &mut n, images.as_mut_ptr()) != VK_SUCCESS {
        return Err("vkGetSwapchainImagesKHR failed".into());
    }
    images.truncate(n as usize);

    let pci = PoolCreateInfo {
        s_type: ST_COMMAND_POOL_CREATE_INFO,
        next: std::ptr::null(),
        flags: POOL_RESET_COMMAND_BUFFER,
        family: eye.queue_family,
    };
    let mut pool = 0u64;
    if (f.create_command_pool)(w.device, &pci, std::ptr::null(), &mut pool) != VK_SUCCESS {
        return Err("vkCreateCommandPool failed".into());
    }
    let ai = CmdAllocInfo { s_type: ST_COMMAND_BUFFER_ALLOCATE_INFO, next: std::ptr::null(), pool, level: 0, count: n };
    let mut cbs = vec![0u64; n as usize];
    if (f.allocate_command_buffers)(w.device, &ai, cbs.as_mut_ptr()) != VK_SUCCESS {
        return Err("vkAllocateCommandBuffers failed".into());
    }
    let signalled = SimpleCreateInfo { s_type: ST_FENCE_CREATE_INFO, next: std::ptr::null(), flags: FENCE_SIGNALED };
    let unsignalled = SimpleCreateInfo { flags: 0, ..signalled };
    let sem = SimpleCreateInfo { s_type: ST_SEMAPHORE_CREATE_INFO, next: std::ptr::null(), flags: 0 };
    let mut slots = Vec::new();
    for cb in cbs {
        let (mut done, mut submitted) = (0u64, 0u64);
        if (f.create_semaphore)(w.device, &sem, std::ptr::null(), &mut done) != VK_SUCCESS
            || (f.create_fence)(w.device, &signalled, std::ptr::null(), &mut submitted) != VK_SUCCESS
        {
            return Err("vkCreateSemaphore or vkCreateFence failed".into());
        }
        slots.push(Slot { cb, done, submitted });
    }
    let mut acquire_fence = 0u64;
    if (f.create_fence)(w.device, &unsignalled, std::ptr::null(), &mut acquire_fence) != VK_SUCCESS {
        return Err("vkCreateFence failed".into());
    }
    Ok(Live { fns: f, swapchain: w.swapchain, queue, images, slots, acquire_fence, ahead: None, filter })
}

/// Ask for the next window image without waiting; the fence says when it is
/// really free.
fn acquire_ahead(l: &mut Live, device: u64) -> Result<(), String> {
    let mut i = 0u32;
    let rc = (l.fns.acquire_next_image)(device, l.swapchain, 0, 0, l.acquire_fence, &mut i);
    match rc {
        VK_SUCCESS | VK_SUBOPTIMAL_KHR => {
            l.ahead = Some(i);
            Ok(())
        }
        // NOT_READY and TIMEOUT: every image is with the presentation engine.
        1 | 2 => Ok(()),
        VK_ERROR_OUT_OF_DATE_KHR => Err(OUT_OF_DATE.into()),
        _ => Err(format!("vkAcquireNextImageKHR returned {rc}")),
    }
}

/// `Ok(true)` if the frame was mirrored, `Ok(false)` if it was skipped
/// because something was not yet free.
fn mirror(l: &mut Live, w: &Window, eye: &Eye) -> Result<bool, String> {
    let device = w.device;
    let Some(index) = l.ahead else {
        acquire_ahead(l, device)?;
        return Ok(false);
    };
    if (l.fns.get_fence_status)(device, l.acquire_fence) != VK_SUCCESS {
        return Ok(false);
    }
    let slot = &l.slots[index as usize];
    if (l.fns.get_fence_status)(device, slot.submitted) != VK_SUCCESS {
        return Ok(false);
    }
    let f = &l.fns;
    (f.reset_fences)(device, 1, &l.acquire_fence);
    (f.reset_fences)(device, 1, &slot.submitted);
    l.ahead = None;
    let (cb, target) = (slot.cb, l.images[index as usize]);

    (f.reset_command_buffer)(cb, 0);
    let begin = CmdBeginInfo { s_type: ST_COMMAND_BUFFER_BEGIN_INFO, next: std::ptr::null(), flags: CMD_ONE_TIME, inheritance: std::ptr::null() };
    (f.begin_command_buffer)(cb, &begin);

    let whole = Range { aspect: ASPECT_COLOR, base_mip: 0, mips: 1, base_layer: 0, layers: 1 };
    let eye_range = Range { base_layer: eye.layer, ..whole };
    let bar = |image, range, src_access, dst_access, old_layout, new_layout| Barrier {
        s_type: ST_IMAGE_MEMORY_BARRIER,
        next: std::ptr::null(),
        src_access,
        dst_access,
        old_layout,
        new_layout,
        src_family: IGNORED,
        dst_family: IGNORED,
        image,
        range,
    };
    let barrier = |src_stage, dst_stage, b: &[Barrier]| {
        (f.cmd_pipeline_barrier)(cb, src_stage, dst_stage, 0, 0, std::ptr::null(), 0, std::ptr::null(), b.len() as u32, b.as_ptr());
    };
    // Into transfer layouts. The eyes' last writer is the engine's colour
    // output, submitted before this release; the window image's old contents
    // are not wanted.
    barrier(STAGE_ALL_COMMANDS | STAGE_TOP, STAGE_TRANSFER, &[
        bar(eye.image, eye_range, ACCESS_COLOR_ATTACHMENT_WRITE, ACCESS_TRANSFER_READ, LAYOUT_COLOR_ATTACHMENT, LAYOUT_TRANSFER_SRC),
        bar(target, whole, 0, ACCESS_TRANSFER_WRITE, LAYOUT_UNDEFINED, LAYOUT_TRANSFER_DST),
    ]);
    // Black bars, then the eye fitted to the window.
    (f.cmd_clear_color_image)(cb, target, LAYOUT_TRANSFER_DST, &[0.0, 0.0, 0.0, 1.0], 1, &whole);
    barrier(STAGE_TRANSFER, STAGE_TRANSFER, &[bar(target, whole, ACCESS_TRANSFER_WRITE, ACCESS_TRANSFER_WRITE, LAYOUT_TRANSFER_DST, LAYOUT_TRANSFER_DST)]);
    let left = View { layer: eye.layer, rect: eye.rect, fov: eye.fov };
    for (layer, src, dst) in compose(left, w.width, w.height) {
        let blit = Blit {
            src: Layers { aspect: ASPECT_COLOR, mip: 0, base_layer: layer, layers: 1 },
            src_offsets: [[src.0, src.1, 0], [src.2, src.3, 1]],
            dst: Layers { aspect: ASPECT_COLOR, mip: 0, base_layer: 0, layers: 1 },
            dst_offsets: [[dst.0, dst.1, 0], [dst.2, dst.3, 1]],
        };
        (f.cmd_blit_image)(cb, eye.image, LAYOUT_TRANSFER_SRC, target, LAYOUT_TRANSFER_DST, 1, &blit, l.filter);
    }
    // Back to where the runtime expects the eye to be, and the window image
    // to where a present expects it.
    barrier(STAGE_TRANSFER, STAGE_ALL_COMMANDS | STAGE_BOTTOM, &[
        bar(eye.image, eye_range, ACCESS_TRANSFER_READ, ACCESS_COLOR_ATTACHMENT_READ | ACCESS_COLOR_ATTACHMENT_WRITE, LAYOUT_TRANSFER_SRC, LAYOUT_COLOR_ATTACHMENT),
        bar(target, whole, ACCESS_TRANSFER_WRITE, 0, LAYOUT_TRANSFER_DST, LAYOUT_PRESENT_SRC),
    ]);
    (f.end_command_buffer)(cb);

    let submit = Submit {
        s_type: ST_SUBMIT_INFO,
        next: std::ptr::null(),
        wait_count: 0,
        waits: std::ptr::null(),
        wait_stages: std::ptr::null(),
        cb_count: 1,
        cbs: &cb,
        signal_count: 1,
        signals: &slot.done,
    };
    let rc = (f.queue_submit)(l.queue, 1, &submit, slot.submitted);
    if rc != VK_SUCCESS {
        return Err(format!("vkQueueSubmit returned {rc}"));
    }
    let present = Present {
        s_type: ST_PRESENT_INFO_KHR,
        next: std::ptr::null(),
        wait_count: 1,
        waits: &slot.done,
        swapchain_count: 1,
        swapchains: &l.swapchain,
        indices: &index,
        results: std::ptr::null_mut(),
    };
    let rc = (f.queue_present)(l.queue, &present);
    if rc == VK_ERROR_OUT_OF_DATE_KHR {
        return Err(OUT_OF_DATE.into());
    }
    if rc != VK_SUCCESS && rc != VK_SUBOPTIMAL_KHR {
        return Err(format!("vkQueuePresentKHR returned {rc}"));
    }
    acquire_ahead(l, device)?;
    Ok(true)
}

/// One eye's picture: array layer, pixel rect, and field of view (angleLeft,
/// angleRight, angleUp, angleDown).
#[derive(Clone, Copy)]
struct View {
    layer: u32,
    rect: (i32, i32, u32, u32),
    fov: [f32; 4],
}

/// The blit that fits the left eye into a `dw`x`dh` window with black bars:
/// (layer, source corners, destination corners).
///
/// Fitted in tangent space, where a projection is linear, so a lopsided field
/// of view keeps its true shape and straight ahead stays where the eye has
/// it. Never cropped: a crop to the window's aspect read as zoomed in. A
/// two-eye composite in SteamVR's style was tried and taken out again at the
/// user's request; the left eye alone is what a person in the headset sees
/// with that eye.
fn compose(eye: View, dw: u32, dh: u32) -> Vec<(u32, (i32, i32, i32, i32), (i32, i32, i32, i32))> {
    let [l, r, u, d] = eye.fov;
    let mut t = [l.tan(), r.tan(), d.tan(), u.tan()];
    // A view with no usable field of view is taken as symmetric about its
    // own aspect, so the mirror still fills the window sensibly.
    if !(t.iter().all(|x| x.is_finite()) && t[1] > t[0] && t[3] > t[2]) {
        let a = eye.rect.2.max(1) as f32 / eye.rect.3.max(1) as f32;
        t = [-a, a, -1.0, 1.0];
    }
    let (w, h) = (t[1] - t[0], t[3] - t[2]);
    let aspect = dw.max(1) as f32 / dh.max(1) as f32;
    let (cw, ch) = if w / h > aspect { (w, w / aspect) } else { (h * aspect, h) };
    let (cx, cy) = ((t[0] + t[1]) / 2.0, (t[2] + t[3]) / 2.0);
    let crop = [cx - cw / 2.0, cx + cw / 2.0, cy - ch / 2.0, cy + ch / 2.0];
    let dx = |x: f32| ((x - crop[0]) / cw * dw as f32).round() as i32;
    let dy = |y: f32| ((crop[3] - y) / ch * dh as f32).round() as i32;
    let (rx, ry, rw, rh) = eye.rect;
    let src = (rx, ry, rx + rw as i32, ry + rh as i32);
    let dst = (dx(t[0]), dy(t[3]), dx(t[1]), dy(t[2]));
    if dst.2 > dst.0 && dst.3 > dst.1 { vec![(eye.layer, src, dst)] } else { Vec::new() }
}

#[cfg(test)]
mod tests {
    use super::{compose, View};

    // Roughly a Quest 3's left eye: wider outwards than inwards.
    const L: [f32; 4] = [-0.90, 0.75, 0.85, -0.90];

    #[test]
    fn the_left_eye_is_fitted_whole_with_bars() {
        let b = compose(View { layer: 0, rect: (0, 0, 2064, 2162), fov: L }, 1920, 1080);
        assert_eq!(b.len(), 1);
        let (layer, src, dst) = b[0];
        assert_eq!(layer, 0);
        assert_eq!(src, (0, 0, 2064, 2162), "never cropped");
        assert_eq!((dst.1, dst.3), (0, 1080), "full height in a wide window");
        assert!(dst.0 > 0 && dst.2 < 1920, "pillarboxed: {dst:?}");
    }

    #[test]
    fn the_blit_stays_inside_the_window() {
        for (dw, dh) in [(1920, 1080), (800, 1200), (1281, 721), (1, 1)] {
            for (_, _, d) in compose(View { layer: 1, rect: (0, 0, 2064, 2162), fov: L }, dw, dh) {
                assert!(d.0 >= 0 && d.1 >= 0 && d.2 <= dw as i32 && d.3 <= dh as i32, "{dw}x{dh}: dst {d:?}");
            }
        }
    }

    #[test]
    fn one_eye_with_no_field_of_view_is_fitted_by_its_own_aspect() {
        let b = compose(View { layer: 0, rect: (0, 0, 896, 1007), fov: [0.0; 4] }, 1280, 720);
        assert_eq!(b.len(), 1);
        // 896x1007 fitted into 1280x720: full height, centred.
        assert_eq!(b[0].2, (320, 0, 960, 720));
    }
}
