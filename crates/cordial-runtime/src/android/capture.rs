//! Reading the frame the engine just presented, out of the swapchain itself.
//!
//! This exists because **nothing else on this host can photograph a Wayland
//! window.** Five routes were tried on 2026-08-21 and every one was refused:
//! `xprop`/`import` see nothing for a native Wayland client, GNOME's
//! `org.gnome.Shell.Screenshot` answers `AccessDenied`, `ffmpeg -f kmsgrab`
//! wants membership of the `video` group, the portal wants a human to click a
//! dialog, and no nested compositor is installable on an immutable host. So
//! every visual check ended with a person being asked to look at the window,
//! and a whole session's worth of bisecting stalled on that.
//!
//! Reading the swapchain sidesteps all of it, and gets a *better* answer than
//! any compositor route could: the image is whatever the engine drew, so it is
//! unaffected by occlusion, by the window being off-screen, by another window
//! covering it, or by the compositor's own colour management. A screenshot
//! taken here is what Roblox rendered rather than what the screen happened to
//! be showing.
//!
//! The copy runs inside `vkQueuePresentKHR`, *before* the frame is handed on,
//! because that is the one moment the image is complete and its layout is
//! known: the engine has finished rendering and has just transitioned it to
//! `VK_IMAGE_LAYOUT_PRESENT_SRC_KHR`. Everything allocated here is created and
//! destroyed per capture. That is deliberately wasteful -- a capture costs a
//! command pool, a buffer and a device-wide wait -- because a screenshot is
//! taken by a human or an agent a handful of times a run, and holding a
//! command pool and a mapped buffer alive for the whole session to save a
//! millisecond on an occasional call would be a permanent cost paid for a
//! rare benefit. It also keeps the failure mode clean: nothing here can leak
//! into a run that never takes a screenshot.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex;

// ---------------------------------------------------------------- constants
const VK_SUCCESS: i32 = 0;
const ST_SUBMIT_INFO: u32 = 4;
const ST_MEMORY_ALLOCATE_INFO: u32 = 5;
const ST_BUFFER_CREATE_INFO: u32 = 12;
const ST_IMAGE_MEMORY_BARRIER: u32 = 45;
const ST_COMMAND_POOL_CREATE_INFO: u32 = 39;
const ST_COMMAND_BUFFER_ALLOCATE_INFO: u32 = 40;
const ST_COMMAND_BUFFER_BEGIN_INFO: u32 = 42;

const BUFFER_USAGE_TRANSFER_DST: u32 = 0x0000_0002;
const MEMORY_HOST_VISIBLE: u32 = 0x0000_0002;
const MEMORY_HOST_COHERENT: u32 = 0x0000_0004;
const CMD_BUFFER_ONE_TIME_SUBMIT: u32 = 0x0000_0001;
const POOL_TRANSIENT: u32 = 0x0000_0001;

const LAYOUT_PRESENT_SRC_KHR: i32 = 1_000_001_002;
const LAYOUT_TRANSFER_SRC_OPTIMAL: i32 = 6;
const ACCESS_TRANSFER_READ: u32 = 0x0000_0800;
const ACCESS_MEMORY_READ: u32 = 0x0000_8000;
const STAGE_TRANSFER: u32 = 0x0000_1000;
const STAGE_ALL_COMMANDS: u32 = 0x0001_0000;
pub const LAYOUT_COLOR_ATTACHMENT_OPTIMAL: i32 = 2;
pub const ACCESS_COLOR_ATTACHMENT_WRITE: u32 = 0x0000_0100;
const ASPECT_COLOR: u32 = 0x0000_0001;
const QUEUE_FAMILY_IGNORED: u32 = u32::MAX;

// ------------------------------------------------------------------ structs
//
// Laid out by hand rather than bindgen'd for the same reason the rest of this
// directory is: the whole Vulkan surface Cordial touches is a dozen calls, and
// a generated crate would bring a build-time dependency and a version to keep
// aligned for no gain. Every field order and padding below follows the Vulkan
// specification's C layout, which is what the driver reads.

#[repr(C)]
struct BufferCreateInfo {
    s_type: u32,
    _pad: u32,
    next: *const c_void,
    flags: u32,
    _pad2: u32,
    size: u64,
    usage: u32,
    sharing_mode: u32,
    queue_family_index_count: u32,
    _pad3: u32,
    queue_family_indices: *const u32,
}

#[repr(C)]
struct MemoryRequirements {
    size: u64,
    alignment: u64,
    memory_type_bits: u32,
    _pad: u32,
}

#[repr(C)]
struct MemoryAllocateInfo {
    s_type: u32,
    _pad: u32,
    next: *const c_void,
    allocation_size: u64,
    memory_type_index: u32,
    _pad2: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct MemoryType {
    property_flags: u32,
    heap_index: u32,
}

#[repr(C)]
pub struct PhysicalDeviceMemoryProperties {
    memory_type_count: u32,
    memory_types: [MemoryType; 32],
    memory_heap_count: u32,
    memory_heaps: [[u64; 2]; 16],
}

#[repr(C)]
struct CommandPoolCreateInfo {
    s_type: u32,
    _pad: u32,
    next: *const c_void,
    flags: u32,
    queue_family_index: u32,
}

#[repr(C)]
struct CommandBufferAllocateInfo {
    s_type: u32,
    _pad: u32,
    next: *const c_void,
    command_pool: u64,
    level: u32,
    command_buffer_count: u32,
}

#[repr(C)]
struct CommandBufferBeginInfo {
    s_type: u32,
    _pad: u32,
    next: *const c_void,
    flags: u32,
    _pad2: u32,
    inheritance_info: *const c_void,
}

#[repr(C)]
struct ImageSubresourceRange {
    aspect_mask: u32,
    base_mip_level: u32,
    level_count: u32,
    base_array_layer: u32,
    layer_count: u32,
}

#[repr(C)]
struct ImageMemoryBarrier {
    s_type: u32,
    _pad: u32,
    next: *const c_void,
    src_access_mask: u32,
    dst_access_mask: u32,
    old_layout: i32,
    new_layout: i32,
    src_queue_family_index: u32,
    dst_queue_family_index: u32,
    image: u64,
    subresource: ImageSubresourceRange,
    _pad2: u32,
}

#[repr(C)]
struct ImageSubresourceLayers {
    aspect_mask: u32,
    mip_level: u32,
    base_array_layer: u32,
    layer_count: u32,
}

#[repr(C)]
struct BufferImageCopy {
    buffer_offset: u64,
    buffer_row_length: u32,
    buffer_image_height: u32,
    image_subresource: ImageSubresourceLayers,
    image_offset: [i32; 3],
    image_extent: [u32; 3],
}

#[repr(C)]
struct SubmitInfo {
    s_type: u32,
    _pad: u32,
    next: *const c_void,
    wait_semaphore_count: u32,
    _pad2: u32,
    wait_semaphores: *const u64,
    wait_dst_stage_mask: *const u32,
    command_buffer_count: u32,
    _pad3: u32,
    command_buffers: *const u64,
    signal_semaphore_count: u32,
    _pad4: u32,
    signal_semaphores: *const u64,
}

// -------------------------------------------------------- recorded per swapchain
//
// Filled by `vulkan.rs` as it interposes the calls that already pass through
// it, so nothing extra is hooked for this: the device and queue family come
// from `vkCreateDevice`, and the swapchain's handle, extent and format from
// `vkCreateSwapchainKHR`.

pub static DEVICE: AtomicUsize = AtomicUsize::new(0);
pub static QUEUE_FAMILY: AtomicU64 = AtomicU64::new(0);
pub static SWAPCHAIN: AtomicU64 = AtomicU64::new(0);
/// Width in the high half, height in the low half, so one atomic carries both.
pub static EXTENT: AtomicU64 = AtomicU64::new(0);
pub static FORMAT: AtomicU64 = AtomicU64::new(0);

pub fn note_swapchain(swapchain: u64, width: u32, height: u32, format: u32) {
    SWAPCHAIN.store(swapchain, Ordering::Relaxed);
    EXTENT.store(((width as u64) << 32) | height as u64, Ordering::Relaxed);
    FORMAT.store(format as u64, Ordering::Relaxed);
}

pub fn extent() -> (u32, u32) {
    let v = EXTENT.load(Ordering::Relaxed);
    ((v >> 32) as u32, (v & 0xffff_ffff) as u32)
}

/// A capture asked for and not yet taken. One at a time: a second request
/// while one is pending replaces it, because a harness that asks twice wants
/// the newer frame.
static PENDING: Mutex<Option<String>> = Mutex::new(None);
/// Lock-free hint for the per-frame present path. The request itself remains
/// behind `PENDING`; this only prevents taking that mutex on every ordinary
/// frame when no capture was requested.
static PENDING_FAST: AtomicBool = AtomicBool::new(false);
/// Set once the pending capture finishes, so the requester can be told.
static RESULT: Mutex<Option<Result<String, String>>> = Mutex::new(None);

pub fn request(path: &str) {
    let mut pending = PENDING.lock().unwrap_or_else(|e| e.into_inner());
    *RESULT.lock().unwrap_or_else(|e| e.into_inner()) = None;
    *pending = Some(path.to_string());
    PENDING_FAST.store(true, Ordering::Release);
}

pub fn take_result() -> Option<Result<String, String>> {
    RESULT.lock().unwrap_or_else(|e| e.into_inner()).take()
}

/// Give up on the pending capture, so a requester waiting on it is told why
/// rather than timing out. Without this every setup failure looked identical
/// to a wedged engine, which is the one thing this surface exists to tell
/// apart.
pub fn abandon(why: &str) {
    let mut pending = PENDING.lock().unwrap_or_else(|e| e.into_inner());
    let had_request = pending.take().is_some();
    PENDING_FAST.store(false, Ordering::Release);
    drop(pending);
    if had_request {
        finish(Err(why.to_string()));
    }
}

fn finish(r: Result<String, String>) {
    *RESULT.lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
}

/// Whether a capture is waiting, checked once per present.
///
/// The full request is mutex-protected, but the normal (no capture) case is a
/// single atomic load so presenting frames never contends with the harness.
pub fn pending() -> bool {
    PENDING_FAST.load(Ordering::Acquire)
}

/// Device-level entry points, resolved through the host's
/// `vkGetDeviceProcAddr` once per capture.
struct DeviceFns {
    create_buffer: extern "C" fn(u64, *const BufferCreateInfo, *const c_void, *mut u64) -> i32,
    get_buffer_memory_requirements: extern "C" fn(u64, u64, *mut MemoryRequirements),
    allocate_memory: extern "C" fn(u64, *const MemoryAllocateInfo, *const c_void, *mut u64) -> i32,
    bind_buffer_memory: extern "C" fn(u64, u64, u64, u64) -> i32,
    create_command_pool:
        extern "C" fn(u64, *const CommandPoolCreateInfo, *const c_void, *mut u64) -> i32,
    allocate_command_buffers:
        extern "C" fn(u64, *const CommandBufferAllocateInfo, *mut u64) -> i32,
    begin_command_buffer: extern "C" fn(u64, *const CommandBufferBeginInfo) -> i32,
    cmd_pipeline_barrier: extern "C" fn(
        u64,
        u32,
        u32,
        u32,
        u32,
        *const c_void,
        u32,
        *const c_void,
        u32,
        *const ImageMemoryBarrier,
    ),
    cmd_copy_image_to_buffer: extern "C" fn(u64, u64, i32, u64, u32, *const BufferImageCopy),
    end_command_buffer: extern "C" fn(u64) -> i32,
    queue_submit: extern "C" fn(u64, u32, *const SubmitInfo, u64) -> i32,
    queue_wait_idle: extern "C" fn(u64) -> i32,
    map_memory: extern "C" fn(u64, u64, u64, u64, u32, *mut *mut c_void) -> i32,
    unmap_memory: extern "C" fn(u64, u64),
    destroy_buffer: extern "C" fn(u64, u64, *const c_void),
    free_memory: extern "C" fn(u64, u64, *const c_void),
    destroy_command_pool: extern "C" fn(u64, u64, *const c_void),
}

macro_rules! load {
    ($get:expr, $dev:expr, $name:literal) => {{
        let n = concat!($name, "\0");
        let p = $get($dev, n.as_ptr() as *const std::ffi::c_char);
        if p.is_null() {
            return Err(concat!("driver has no ", $name).into());
        }
        // SAFETY: the loader returned a function for exactly this name, whose
        // signature is the one the Vulkan specification gives it.
        unsafe { std::mem::transmute(p) }
    }};
}

/// Do the copy. Called from inside `vkQueuePresentKHR`, with the queue the
/// engine is presenting on and the image it is presenting.
pub fn capture(
    queue: u64,
    swapchain: u64,
    image_index: u32,
    get_device_proc_addr: extern "C" fn(u64, *const std::ffi::c_char) -> *mut c_void,
    get_physical_device_memory_properties: extern "C" fn(
        *mut c_void,
        *mut PhysicalDeviceMemoryProperties,
    ),
    physical_device: *mut c_void,
) {
    let mut pending = PENDING.lock().unwrap_or_else(|e| e.into_inner());
    let path = pending.take();
    PENDING_FAST.store(false, Ordering::Release);
    drop(pending);
    let Some(path) = path else { return };
    let r = do_capture(
        queue,
        swapchain,
        image_index,
        get_device_proc_addr,
        get_physical_device_memory_properties,
        physical_device,
        &path,
    );
    if let Err(ref e) = r {
        println!("[android] vulkan: capture failed: {e}");
    }
    finish(r);
}

fn do_capture(
    queue: u64,
    swapchain: u64,
    image_index: u32,
    gdpa: extern "C" fn(u64, *const std::ffi::c_char) -> *mut c_void,
    gpdmp: extern "C" fn(*mut c_void, *mut PhysicalDeviceMemoryProperties),
    physical_device: *mut c_void,
    path: &str,
) -> Result<String, String> {
    let device = DEVICE.load(Ordering::Relaxed) as u64;
    let (width, height) = extent();
    if device == 0 || swapchain == 0 || width == 0 || height == 0 {
        return Err("no swapchain has been created yet".into());
    }

    let get_swapchain_images: extern "C" fn(u64, u64, *mut u32, *mut u64) -> i32 =
        load!(gdpa, device, "vkGetSwapchainImagesKHR");

    // The image being presented, by index into the swapchain's own list.
    let mut count = 0u32;
    if get_swapchain_images(device, swapchain, &mut count, std::ptr::null_mut()) != VK_SUCCESS {
        return Err("vkGetSwapchainImagesKHR failed to count".into());
    }
    let mut images = vec![0u64; count as usize];
    if get_swapchain_images(device, swapchain, &mut count, images.as_mut_ptr()) != VK_SUCCESS {
        return Err("vkGetSwapchainImagesKHR failed".into());
    }
    let image = *images
        .get(image_index as usize)
        .ok_or_else(|| format!("image index {image_index} is outside {count} swapchain images"))?;
    let target = Target {
        device,
        queue,
        queue_family: QUEUE_FAMILY.load(Ordering::Relaxed) as u32,
        image,
        layout: LAYOUT_PRESENT_SRC_KHR,
        src_access: ACCESS_MEMORY_READ,
        layer: 0,
        rect: (0, 0, width, height),
        format: FORMAT.load(Ordering::Relaxed) as u32,
    };
    copy_to_png(&target, gdpa, gpdmp, physical_device, path)
}

/// One image to read back: where it is, the layout it is in and must be
/// left in, and which layer and rectangle of it.
pub struct Target {
    pub device: u64,
    pub queue: u64,
    pub queue_family: u32,
    pub image: u64,
    /// The layout the image is in now, and is put back into after the copy.
    pub layout: i32,
    /// What last touched it, for the barrier into the copy.
    pub src_access: u32,
    pub layer: u32,
    /// x, y, width, height.
    pub rect: (i32, i32, u32, u32),
    pub format: u32,
}

/// The capture an OpenXR frame takes (`guest_xr`): an image the engine has
/// just finished rendering into, read before it is released to the runtime.
/// Same one-at-a-time request as the present path's.
#[cfg(all(feature = "vr", target_arch = "x86_64"))]
pub fn capture_target(
    target: &Target,
    gdpa: extern "C" fn(u64, *const std::ffi::c_char) -> *mut c_void,
    gpdmp: extern "C" fn(*mut c_void, *mut PhysicalDeviceMemoryProperties),
    physical_device: *mut c_void,
) {
    let mut pending = PENDING.lock().unwrap_or_else(|e| e.into_inner());
    let path = pending.take();
    PENDING_FAST.store(false, Ordering::Release);
    drop(pending);
    let Some(path) = path else { return };
    let r = copy_to_png(target, gdpa, gpdmp, physical_device, &path);
    if let Err(ref e) = r {
        println!("[android] vulkan: capture failed: {e}");
    }
    finish(r);
}

fn copy_to_png(
    t: &Target,
    gdpa: extern "C" fn(u64, *const std::ffi::c_char) -> *mut c_void,
    gpdmp: extern "C" fn(*mut c_void, *mut PhysicalDeviceMemoryProperties),
    physical_device: *mut c_void,
    path: &str,
) -> Result<String, String> {
    let (device, queue, image) = (t.device, t.queue, t.image);
    let (x, y, width, height) = t.rect;
    if device == 0 || image == 0 || width == 0 || height == 0 {
        return Err("no image to read".into());
    }
    let f = DeviceFns {
        create_buffer: load!(gdpa, device, "vkCreateBuffer"),
        get_buffer_memory_requirements: load!(gdpa, device, "vkGetBufferMemoryRequirements"),
        allocate_memory: load!(gdpa, device, "vkAllocateMemory"),
        bind_buffer_memory: load!(gdpa, device, "vkBindBufferMemory"),
        create_command_pool: load!(gdpa, device, "vkCreateCommandPool"),
        allocate_command_buffers: load!(gdpa, device, "vkAllocateCommandBuffers"),
        begin_command_buffer: load!(gdpa, device, "vkBeginCommandBuffer"),
        cmd_pipeline_barrier: load!(gdpa, device, "vkCmdPipelineBarrier"),
        cmd_copy_image_to_buffer: load!(gdpa, device, "vkCmdCopyImageToBuffer"),
        end_command_buffer: load!(gdpa, device, "vkEndCommandBuffer"),
        queue_submit: load!(gdpa, device, "vkQueueSubmit"),
        queue_wait_idle: load!(gdpa, device, "vkQueueWaitIdle"),
        map_memory: load!(gdpa, device, "vkMapMemory"),
        unmap_memory: load!(gdpa, device, "vkUnmapMemory"),
        destroy_buffer: load!(gdpa, device, "vkDestroyBuffer"),
        free_memory: load!(gdpa, device, "vkFreeMemory"),
        destroy_command_pool: load!(gdpa, device, "vkDestroyCommandPool"),
    };

    // Four bytes a pixel: every format a swapchain is created with on this
    // path is a 32-bit BGRA or RGBA, and `FORMAT` records which so the channel
    // order can be fixed up on the way out rather than guessed.
    let size = width as u64 * height as u64 * 4;
    let bci = BufferCreateInfo {
        s_type: ST_BUFFER_CREATE_INFO,
        _pad: 0,
        next: std::ptr::null(),
        flags: 0,
        _pad2: 0,
        size,
        usage: BUFFER_USAGE_TRANSFER_DST,
        sharing_mode: 0,
        queue_family_index_count: 0,
        _pad3: 0,
        queue_family_indices: std::ptr::null(),
    };
    let mut buffer = 0u64;
    if (f.create_buffer)(device, &bci, std::ptr::null(), &mut buffer) != VK_SUCCESS {
        return Err("vkCreateBuffer failed".into());
    }

    let mut req = MemoryRequirements { size: 0, alignment: 0, memory_type_bits: 0, _pad: 0 };
    (f.get_buffer_memory_requirements)(device, buffer, &mut req);

    // SAFETY: the driver fills this out entirely; it is read-only afterwards.
    let mut props: PhysicalDeviceMemoryProperties = unsafe { std::mem::zeroed() };
    (gpdmp)(physical_device, &mut props);
    let wanted = MEMORY_HOST_VISIBLE | MEMORY_HOST_COHERENT;
    let type_index = (0..props.memory_type_count)
        .find(|i| {
            req.memory_type_bits & (1 << i) != 0
                && props.memory_types[*i as usize].property_flags & wanted == wanted
        })
        .ok_or("no host-visible coherent memory type accepts this buffer")?;

    let mai = MemoryAllocateInfo {
        s_type: ST_MEMORY_ALLOCATE_INFO,
        _pad: 0,
        next: std::ptr::null(),
        allocation_size: req.size,
        memory_type_index: type_index,
        _pad2: 0,
    };
    let mut memory = 0u64;
    if (f.allocate_memory)(device, &mai, std::ptr::null(), &mut memory) != VK_SUCCESS {
        (f.destroy_buffer)(device, buffer, std::ptr::null());
        return Err("vkAllocateMemory failed".into());
    }
    (f.bind_buffer_memory)(device, buffer, memory, 0);

    let pool_ci = CommandPoolCreateInfo {
        s_type: ST_COMMAND_POOL_CREATE_INFO,
        _pad: 0,
        next: std::ptr::null(),
        flags: POOL_TRANSIENT,
        queue_family_index: t.queue_family,
    };
    let mut pool = 0u64;
    if (f.create_command_pool)(device, &pool_ci, std::ptr::null(), &mut pool) != VK_SUCCESS {
        (f.free_memory)(device, memory, std::ptr::null());
        (f.destroy_buffer)(device, buffer, std::ptr::null());
        return Err("vkCreateCommandPool failed".into());
    }

    let cb_ai = CommandBufferAllocateInfo {
        s_type: ST_COMMAND_BUFFER_ALLOCATE_INFO,
        _pad: 0,
        next: std::ptr::null(),
        command_pool: pool,
        level: 0,
        command_buffer_count: 1,
    };
    let mut cb = 0u64;
    if (f.allocate_command_buffers)(device, &cb_ai, &mut cb) != VK_SUCCESS {
        (f.destroy_command_pool)(device, pool, std::ptr::null());
        (f.free_memory)(device, memory, std::ptr::null());
        (f.destroy_buffer)(device, buffer, std::ptr::null());
        return Err("vkAllocateCommandBuffers failed".into());
    }

    let begin = CommandBufferBeginInfo {
        s_type: ST_COMMAND_BUFFER_BEGIN_INFO,
        _pad: 0,
        next: std::ptr::null(),
        flags: CMD_BUFFER_ONE_TIME_SUBMIT,
        _pad2: 0,
        inheritance_info: std::ptr::null(),
    };
    (f.begin_command_buffer)(cb, &begin);

    // The image arrives in PRESENT_SRC because the engine has just finished
    // with it, and it must go back in PRESENT_SRC because the present that
    // this capture is sitting inside is about to use it. Both barriers are
    // therefore mandatory, and the second one is the easier to forget.
    let to_src = ImageMemoryBarrier {
        s_type: ST_IMAGE_MEMORY_BARRIER,
        _pad: 0,
        next: std::ptr::null(),
        src_access_mask: t.src_access,
        dst_access_mask: ACCESS_TRANSFER_READ,
        old_layout: t.layout,
        new_layout: LAYOUT_TRANSFER_SRC_OPTIMAL,
        src_queue_family_index: QUEUE_FAMILY_IGNORED,
        dst_queue_family_index: QUEUE_FAMILY_IGNORED,
        image,
        subresource: ImageSubresourceRange {
            aspect_mask: ASPECT_COLOR,
            base_mip_level: 0,
            level_count: 1,
            base_array_layer: t.layer,
            layer_count: 1,
        },
        _pad2: 0,
    };
    (f.cmd_pipeline_barrier)(
        cb,
        // All commands rather than transfer: on the OpenXR path the last
        // writer is the engine's colour attachment output, not a transfer.
        STAGE_ALL_COMMANDS,
        STAGE_TRANSFER,
        0,
        0,
        std::ptr::null(),
        0,
        std::ptr::null(),
        1,
        &to_src,
    );

    let region = BufferImageCopy {
        buffer_offset: 0,
        buffer_row_length: 0,
        buffer_image_height: 0,
        image_subresource: ImageSubresourceLayers {
            aspect_mask: ASPECT_COLOR,
            mip_level: 0,
            base_array_layer: t.layer,
            layer_count: 1,
        },
        image_offset: [x, y, 0],
        image_extent: [width, height, 1],
    };
    (f.cmd_copy_image_to_buffer)(cb, image, LAYOUT_TRANSFER_SRC_OPTIMAL, buffer, 1, &region);

    let back = ImageMemoryBarrier {
        src_access_mask: ACCESS_TRANSFER_READ,
        dst_access_mask: ACCESS_MEMORY_READ,
        old_layout: LAYOUT_TRANSFER_SRC_OPTIMAL,
        new_layout: t.layout,
        ..to_src
    };
    (f.cmd_pipeline_barrier)(
        cb,
        STAGE_TRANSFER,
        STAGE_TRANSFER,
        0,
        0,
        std::ptr::null(),
        0,
        std::ptr::null(),
        1,
        &back,
    );
    (f.end_command_buffer)(cb);

    let submit = SubmitInfo {
        s_type: ST_SUBMIT_INFO,
        _pad: 0,
        next: std::ptr::null(),
        wait_semaphore_count: 0,
        _pad2: 0,
        wait_semaphores: std::ptr::null(),
        wait_dst_stage_mask: std::ptr::null(),
        command_buffer_count: 1,
        _pad3: 0,
        command_buffers: &cb,
        signal_semaphore_count: 0,
        _pad4: 0,
        signal_semaphores: std::ptr::null(),
    };
    let submitted = (f.queue_submit)(queue, 1, &submit, 0) == VK_SUCCESS;
    if submitted {
        (f.queue_wait_idle)(queue);
    }

    let mut out = Err("vkQueueSubmit failed".to_string());
    if submitted {
        let mut ptr: *mut c_void = std::ptr::null_mut();
        if (f.map_memory)(device, memory, 0, size, 0, &mut ptr) == VK_SUCCESS && !ptr.is_null() {
            // SAFETY: the driver mapped exactly `size` bytes at `ptr`, and the
            // copy above has completed because the queue was waited on.
            let pixels = unsafe { std::slice::from_raw_parts(ptr as *const u8, size as usize) };
            out = write_png(path, width, height, pixels, t.format)
                .map(|_| format!("{path} {width}x{height}"));
            (f.unmap_memory)(device, memory);
        } else {
            out = Err("vkMapMemory failed".into());
        }
    }

    (f.destroy_command_pool)(device, pool, std::ptr::null());
    (f.free_memory)(device, memory, std::ptr::null());
    (f.destroy_buffer)(device, buffer, std::ptr::null());
    out
}

/// Write the pixels out as a PNG, with no encoder dependency.
///
/// Stored uncompressed, in zlib's "stored" block form, which every PNG reader
/// accepts. A 1920x1200 frame lands around 9 MB, which is fine for something a
/// harness looks at and deletes; pulling in a deflate implementation to make it
/// smaller would add a dependency to the runtime crate for a development aid.
fn write_png(path: &str, width: u32, height: u32, pixels: &[u8], format: u32) -> Result<(), String> {
    // 44/50 are B8G8R8A8_UNORM/SRGB, 37/43 are R8G8B8A8_UNORM/SRGB. Anything
    // else is reported rather than guessed at, because a silently swapped red
    // and blue channel is exactly the kind of wrong-but-plausible output this
    // project keeps having to retract.
    let bgra = match format {
        44 | 50 => true,
        37 | 43 => false,
        other => return Err(format!("unhandled swapchain format {other}")),
    };

    let mut raw = Vec::with_capacity((width as usize * 3 + 1) * height as usize);
    for y in 0..height as usize {
        raw.push(0u8); // filter: none
        let row = &pixels[y * width as usize * 4..][..width as usize * 4];
        for px in row.chunks_exact(4) {
            if bgra {
                raw.extend_from_slice(&[px[2], px[1], px[0]]);
            } else {
                raw.extend_from_slice(&[px[0], px[1], px[2]]);
            }
        }
    }

    let mut png = Vec::new();
    png.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]); // 8-bit, truecolour
    chunk(&mut png, b"IHDR", &ihdr);
    chunk(&mut png, b"IDAT", &zlib_stored(&raw));
    chunk(&mut png, b"IEND", &[]);
    std::fs::write(path, png).map_err(|e| format!("writing {path}: {e}"))
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut c = crc32(kind);
    c = crc32_continue(c, data);
    out.extend_from_slice(&c.to_be_bytes());
}

/// zlib stream whose deflate blocks are all "stored", i.e. uncompressed.
fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01];
    let mut i = 0;
    while i < data.len() {
        let n = usize::min(65535, data.len() - i);
        let last = if i + n >= data.len() { 1 } else { 0 };
        out.push(last);
        out.extend_from_slice(&(n as u16).to_le_bytes());
        out.extend_from_slice(&(!(n as u16)).to_le_bytes());
        out.extend_from_slice(&data[i..i + n]);
        i += n;
    }
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &x in data {
        a = (a + x as u32) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

fn crc_table() -> &'static [u32; 256] {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| {
        let mut t = [0u32; 256];
        for (n, e) in t.iter_mut().enumerate() {
            let mut c = n as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
            }
            *e = c;
        }
        t
    })
}

fn crc32(data: &[u8]) -> u32 {
    // `crc32_continue` takes the *previous finished* CRC and undoes the
    // final XOR-out (`prev ^ 0xffff_ffff`) to recover the raw running
    // register before folding more data in -- see below. For a first call
    // that raw register must start at the standard 0xffff_ffff, which is
    // `prev = 0`. This used to be spelled `0xffff_ffff ^ 0xffff_ffff`, which
    // clippy's `eq_op` flagged as a likely typo; it was not one -- verified
    // against the standard CRC-32/ISO-HDLC check value, `crc32(b"123456789")
    // == 0xcbf4_3926` -- but the constant it reduces to is the honest way to
    // write it.
    crc32_continue(0, data)
}

/// PNG's CRC is over the type and the data together, so it has to be resumable.
fn crc32_continue(prev: u32, data: &[u8]) -> u32 {
    let t = crc_table();
    let mut c = prev ^ 0xffff_ffff;
    for &x in data {
        c = t[((c ^ x as u32) & 0xff) as usize] ^ (c >> 8);
    }
    c ^ 0xffff_ffff
}
