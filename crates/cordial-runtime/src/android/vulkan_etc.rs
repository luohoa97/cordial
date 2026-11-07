use std::collections::HashMap;
use std::ffi::{c_char, c_void, CStr};
use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering::Acquire, Ordering::Relaxed, Ordering::Release};
use std::sync::{Mutex, MutexGuard, OnceLock};

use super::etc_decode::{self, EtcFormat, Surface};
use super::glcount;

const VK_SUCCESS: i32 = 0;
const VK_ERROR_INITIALIZATION_FAILED: i32 = -3;
const VK_ERROR_FORMAT_NOT_SUPPORTED: i32 = -11;

const ST_BUFFER_CREATE_INFO: u32 = 12;
const ST_MEMORY_ALLOCATE_INFO: u32 = 5;
const ST_PHYSICAL_DEVICE_FEATURES_2: u32 = 1_000_059_000;
const ST_FORMAT_PROPERTIES_3: u32 = 1_000_360_000;
const ST_IMAGE_FORMAT_LIST_CREATE_INFO: u32 = 1_000_147_000;

const FEATURE_COUNT: usize = 55;
const FEATURE_ETC2: usize = 20;
const FEATURE_ASTC_LDR: usize = 21;
const FEATURE_BC: usize = 22;

const FORMAT_FEATURE_SAMPLED_IMAGE: u32 = 0x1;
const FORMAT_FEATURE_SAMPLED_IMAGE_FILTER_LINEAR: u32 = 0x1000;
const FORMAT_FEATURE_TRANSFER_DST: u32 = 0x8000;
const EMULATED_FORMAT_FEATURES: u32 =
    FORMAT_FEATURE_SAMPLED_IMAGE | FORMAT_FEATURE_SAMPLED_IMAGE_FILTER_LINEAR | FORMAT_FEATURE_TRANSFER_DST;

const IMAGE_USAGE_TRANSFER_SRC: u32 = 0x1;
const IMAGE_USAGE_TRANSFER_DST: u32 = 0x2;
const IMAGE_USAGE_SAMPLED: u32 = 0x4;
const EMULATED_USAGE: u32 = IMAGE_USAGE_TRANSFER_SRC | IMAGE_USAGE_TRANSFER_DST | IMAGE_USAGE_SAMPLED;
const IMAGE_CREATE_MUTABLE_FORMAT: u32 = 0x8;
const IMAGE_CREATE_CUBE_COMPATIBLE: u32 = 0x10;
const EMULATED_CREATE_FLAGS: u32 = IMAGE_CREATE_MUTABLE_FORMAT | IMAGE_CREATE_CUBE_COMPATIBLE;
const IMAGE_TILING_OPTIMAL: u32 = 0;
const SAMPLE_COUNT_1: u32 = 0x1;

const BUFFER_USAGE_TRANSFER_SRC: u32 = 0x1;
const MEMORY_DEVICE_LOCAL: u32 = 0x1;
const MEMORY_HOST_VISIBLE: u32 = 0x2;
const MEMORY_HOST_COHERENT: u32 = 0x4;
const WHOLE_SIZE: u64 = u64::MAX;

const STAGING_CHUNK: u64 = 8 << 20;
const STAGING_ALIGN: u64 = 16;

pub fn disabled() -> bool {
    static OFF: OnceLock<bool> = OnceLock::new();
    *OFF.get_or_init(|| std::env::var_os("CORDIAL_NO_ETC_EMULATION").is_some_and(|v| v != "0"))
}

/// `CORDIAL_FORCE_ETC_EMULATION=1`: emulate on a driver that has native ETC2
/// too. A test switch, in the family of `CORDIAL_FORCE_GPU_VENDOR`
/// (ADR-046), so that native and decoded output can be compared on one machine
/// without NVIDIA hardware. `CORDIAL_NO_ETC_EMULATION` wins if both are set.
pub fn forced() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("CORDIAL_FORCE_ETC_EMULATION").is_some_and(|v| v != "0"))
}

/// Whether to emulate on a device whose `textureCompressionETC2` is `native`.
/// The gate is the driver lacking the feature -- not the vendor id, which is
/// what ADR-046 gates everything else NVIDIA on -- because the feature bit is
/// the thing the engine reads and the thing that is wrong; see ADR-049.
fn should_emulate(native: bool, forced: bool) -> bool {
    !native || forced
}

fn trace(args: std::fmt::Arguments<'_>) {
    crate::android::trace(format_args!("vulkan-etc: {args}"));
}

fn unhandled(args: std::fmt::Arguments<'_>) {
    glcount::ETC_UNHANDLED.fetch_add(1, Relaxed);
    trace(format_args!("UNHANDLED {args}"));
}

pub fn etc_format(format: u32) -> Option<(EtcFormat, u32, &'static str)> {
    Some(match format {
        147 => (EtcFormat::Rgb8, 37, "ETC2_R8G8B8_UNORM"),
        148 => (EtcFormat::Rgb8, 43, "ETC2_R8G8B8_SRGB"),
        149 => (EtcFormat::Rgb8A1, 37, "ETC2_R8G8B8A1_UNORM"),
        150 => (EtcFormat::Rgb8A1, 43, "ETC2_R8G8B8A1_SRGB"),
        151 => (EtcFormat::Rgba8, 37, "ETC2_R8G8B8A8_UNORM"),
        152 => (EtcFormat::Rgba8, 43, "ETC2_R8G8B8A8_SRGB"),
        153 => (EtcFormat::R11, 70, "EAC_R11_UNORM"),
        154 => (EtcFormat::R11Signed, 71, "EAC_R11_SNORM"),
        155 => (EtcFormat::Rg11, 77, "EAC_R11G11_UNORM"),
        156 => (EtcFormat::Rg11Signed, 78, "EAC_R11G11_SNORM"),
        _ => return None,
    })
}

fn rd<T: Copy>(base: *const c_void, offset: usize) -> T {
    unsafe { std::ptr::read_unaligned((base as *const u8).add(offset) as *const T) }
}

fn wr<T: Copy>(base: *mut c_void, offset: usize, value: T) {
    unsafe { std::ptr::write_unaligned((base as *mut u8).add(offset) as *mut T, value) }
}

fn slice<'a>(base: *const c_void, len: usize) -> &'a [u8] {
    unsafe { std::slice::from_raw_parts(base as *const u8, len) }
}

fn slice_mut<'a>(base: *mut c_void, len: usize) -> &'a mut [u8] {
    unsafe { std::slice::from_raw_parts_mut(base as *mut u8, len) }
}

fn as_fn<F: Copy>(p: usize) -> Option<F> {
    assert_eq!(std::mem::size_of::<F>(), std::mem::size_of::<usize>());
    (p != 0).then(|| unsafe { std::mem::transmute_copy::<usize, F>(&p) })
}

fn chain_find(mut node: *const c_void, s_type: u32) -> *const c_void {
    while !node.is_null() {
        if rd::<u32>(node, 0) == s_type {
            return node;
        }
        node = rd::<*const c_void>(node, 8);
    }
    std::ptr::null()
}

macro_rules! slots {
    ($($name:ident),* $(,)?) => { $(static $name: AtomicUsize = AtomicUsize::new(0);)* };
}

slots!(
    H_FEATURES,
    H_FEATURES2,
    H_FORMAT_PROPS2,
    H_IMAGE_FORMAT_PROPS,
    H_IMAGE_FORMAT_PROPS2,
    H_CREATE_IMAGE,
    H_DEVICE_IMAGE_REQS,
    H_DEVICE_IMAGE_SPARSE_REQS,
    H_DESTROY_IMAGE,
    H_CREATE_IMAGE_VIEW,
    H_CMD_COPY_BUFFER_TO_IMAGE,
    H_CMD_COPY_BUFFER_TO_IMAGE2,
    H_CMD_COPY_IMAGE,
    H_CMD_COPY_IMAGE2,
    H_CMD_BLIT_IMAGE,
    H_CMD_BLIT_IMAGE2,
    H_CMD_COPY_IMAGE_TO_BUFFER,
    H_CMD_COPY_IMAGE_TO_BUFFER2,
    H_BIND_BUFFER_MEMORY,
    H_BIND_BUFFER_MEMORY2,
    H_DESTROY_BUFFER,
    H_MAP_MEMORY,
    H_MAP_MEMORY2,
    H_UNMAP_MEMORY,
    H_FREE_MEMORY,
    H_QUEUE_SUBMIT,
    H_QUEUE_SUBMIT2,
    H_ALLOCATE_COMMAND_BUFFERS,
    H_BEGIN_COMMAND_BUFFER,
    H_RESET_COMMAND_BUFFER,
    H_FREE_COMMAND_BUFFERS,
    H_RESET_COMMAND_POOL,
    H_DESTROY_COMMAND_POOL,
    H_DESTROY_DEVICE,
);

/// Devices whose `vkCreateDevice` was made on an emulating physical device, by
/// handle. Recorded whether or not the staging calls could then be resolved:
/// the engine was told ETC2 works on these, so a call on one that cannot be
/// served has to fail rather than reach the driver with a raw ETC format.
static EMULATING: Mutex<Vec<usize>> = Mutex::new(Vec::new());

fn emulating() -> MutexGuard<'static, Vec<usize>> {
    EMULATING.lock().unwrap_or_else(|e| e.into_inner())
}

fn device_emulates(device: *mut c_void) -> bool {
    !device.is_null() && emulating().contains(&(device as usize))
}

/// `device` is the handle the name was resolved against, null from
/// `vkGetInstanceProcAddr`. A device that is not emulating is not routed
/// through this module: Intel and AMD, and NVIDIA's own native path, get the
/// driver's function directly. The instance-level lookup cannot know which
/// device a pointer will be used on, so it stays hooked, and each hook
/// forwards straight through for a device that is not emulating.
pub fn hook(name: &[u8], host: *mut c_void, device: *mut c_void) -> Option<*mut c_void> {
    if host.is_null() || (disabled() && !crate::android::tracing()) {
        return None;
    }
    if !device.is_null() && !device_emulates(device) && !crate::android::tracing() {
        return None;
    }
    let (slot, ours): (&AtomicUsize, *const ()) = match name {
        b"vkGetPhysicalDeviceFeatures" => (&H_FEATURES, get_features as *const ()),
        b"vkGetPhysicalDeviceFeatures2" | b"vkGetPhysicalDeviceFeatures2KHR" => {
            (&H_FEATURES2, get_features2 as *const ())
        }
        b"vkGetPhysicalDeviceFormatProperties2" | b"vkGetPhysicalDeviceFormatProperties2KHR" => {
            (&H_FORMAT_PROPS2, get_format_props2 as *const ())
        }
        b"vkGetPhysicalDeviceImageFormatProperties" => {
            (&H_IMAGE_FORMAT_PROPS, get_image_format_props as *const ())
        }
        b"vkGetPhysicalDeviceImageFormatProperties2" | b"vkGetPhysicalDeviceImageFormatProperties2KHR" => {
            (&H_IMAGE_FORMAT_PROPS2, get_image_format_props2 as *const ())
        }
        b"vkGetDeviceImageMemoryRequirements" | b"vkGetDeviceImageMemoryRequirementsKHR" => {
            (&H_DEVICE_IMAGE_REQS, get_device_image_requirements as *const ())
        }
        b"vkGetDeviceImageSparseMemoryRequirements" | b"vkGetDeviceImageSparseMemoryRequirementsKHR" => {
            (&H_DEVICE_IMAGE_SPARSE_REQS, get_device_image_sparse_requirements as *const ())
        }
        b"vkCreateImage" => (&H_CREATE_IMAGE, create_image as *const ()),
        b"vkDestroyImage" => (&H_DESTROY_IMAGE, destroy_image as *const ()),
        b"vkCreateImageView" => (&H_CREATE_IMAGE_VIEW, create_image_view as *const ()),
        b"vkCmdCopyBufferToImage" => (&H_CMD_COPY_BUFFER_TO_IMAGE, cmd_copy_buffer_to_image as *const ()),
        b"vkCmdCopyBufferToImage2" | b"vkCmdCopyBufferToImage2KHR" => {
            (&H_CMD_COPY_BUFFER_TO_IMAGE2, cmd_copy_buffer_to_image2 as *const ())
        }
        b"vkCmdCopyImage" => (&H_CMD_COPY_IMAGE, cmd_copy_image as *const ()),
        b"vkCmdCopyImage2" | b"vkCmdCopyImage2KHR" => (&H_CMD_COPY_IMAGE2, cmd_copy_image2 as *const ()),
        b"vkCmdBlitImage" => (&H_CMD_BLIT_IMAGE, cmd_blit_image as *const ()),
        b"vkCmdBlitImage2" | b"vkCmdBlitImage2KHR" => (&H_CMD_BLIT_IMAGE2, cmd_blit_image2 as *const ()),
        b"vkCmdCopyImageToBuffer" => (&H_CMD_COPY_IMAGE_TO_BUFFER, cmd_copy_image_to_buffer as *const ()),
        b"vkCmdCopyImageToBuffer2" | b"vkCmdCopyImageToBuffer2KHR" => {
            (&H_CMD_COPY_IMAGE_TO_BUFFER2, cmd_copy_image_to_buffer2 as *const ())
        }
        b"vkBindBufferMemory" => (&H_BIND_BUFFER_MEMORY, bind_buffer_memory as *const ()),
        b"vkBindBufferMemory2" | b"vkBindBufferMemory2KHR" => {
            (&H_BIND_BUFFER_MEMORY2, bind_buffer_memory2 as *const ())
        }
        b"vkDestroyBuffer" => (&H_DESTROY_BUFFER, destroy_buffer as *const ()),
        b"vkMapMemory" => (&H_MAP_MEMORY, map_memory as *const ()),
        b"vkMapMemory2" | b"vkMapMemory2KHR" => (&H_MAP_MEMORY2, map_memory2 as *const ()),
        b"vkUnmapMemory" => (&H_UNMAP_MEMORY, unmap_memory as *const ()),
        b"vkFreeMemory" => (&H_FREE_MEMORY, free_memory as *const ()),
        b"vkQueueSubmit" => (&H_QUEUE_SUBMIT, queue_submit as *const ()),
        b"vkQueueSubmit2" | b"vkQueueSubmit2KHR" => (&H_QUEUE_SUBMIT2, queue_submit2 as *const ()),
        b"vkAllocateCommandBuffers" => (&H_ALLOCATE_COMMAND_BUFFERS, allocate_command_buffers as *const ()),
        b"vkBeginCommandBuffer" => (&H_BEGIN_COMMAND_BUFFER, begin_command_buffer as *const ()),
        b"vkResetCommandBuffer" => (&H_RESET_COMMAND_BUFFER, reset_command_buffer as *const ()),
        b"vkFreeCommandBuffers" => (&H_FREE_COMMAND_BUFFERS, free_command_buffers as *const ()),
        b"vkResetCommandPool" => (&H_RESET_COMMAND_POOL, reset_command_pool as *const ()),
        b"vkDestroyCommandPool" => (&H_DESTROY_COMMAND_POOL, destroy_command_pool as *const ()),
        b"vkDestroyDevice" => (&H_DESTROY_DEVICE, destroy_device as *const ()),
        _ => return None,
    };
    slot.store(host as usize, Relaxed);
    Some(ours as *mut c_void)
}

fn host_features(pd: *mut c_void) -> Option<[u32; FEATURE_COUNT]> {
    let f: extern "C" fn(*mut c_void, *mut u32) =
        as_fn(super::vulkan::host_instance_proc(c"vkGetPhysicalDeviceFeatures") as usize)?;
    let mut feats = [0u32; FEATURE_COUNT];
    f(pd, feats.as_mut_ptr());
    Some(feats)
}

pub fn emulate_for(pd: *mut c_void) -> bool {
    if disabled() || pd.is_null() {
        return false;
    }
    static SEEN: Mutex<Vec<(usize, bool)>> = Mutex::new(Vec::new());
    let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(&(_, e)) = seen.iter().find(|(p, _)| *p == pd as usize) {
        return e;
    }
    let emulate = match host_features(pd) {
        Some(f) => {
            let e = should_emulate(f[FEATURE_ETC2] != 0, forced());
            trace(format_args!(
                "physical device {pd:p}: host textureCompressionETC2={} ASTC_LDR={} BC={}; {}",
                f[FEATURE_ETC2],
                f[FEATURE_ASTC_LDR],
                f[FEATURE_BC],
                match (e, f[FEATURE_ETC2] != 0) {
                    (true, true) => "native ETC2 present, emulation FORCED (CORDIAL_FORCE_ETC_EMULATION)",
                    (true, false) => "ETC2/EAC will be emulated",
                    _ => "native ETC2, emulation off",
                }
            ));
            if e && f[FEATURE_ETC2] != 0 {
                println!(
                    "[android] vulkan-etc: CORDIAL_FORCE_ETC_EMULATION is set; decoding ETC2/EAC on the CPU \
                     although this driver has it natively"
                );
            }
            e
        }
        None => {
            trace(format_args!("physical device {pd:p}: no vkGetPhysicalDeviceFeatures, emulation off"));
            false
        }
    };
    seen.push((pd as usize, emulate));
    emulate
}

unsafe extern "C" fn get_features(pd: *mut c_void, out: *mut u32) {
    let Some(f) = as_fn::<extern "C" fn(*mut c_void, *mut u32)>(H_FEATURES.load(Relaxed)) else {
        return;
    };
    f(pd, out);
    if out.is_null() {
        return;
    }
    patch_features(pd, out as *mut c_void, "vkGetPhysicalDeviceFeatures");
}

unsafe extern "C" fn get_features2(pd: *mut c_void, out: *mut c_void) {
    let Some(f) = as_fn::<extern "C" fn(*mut c_void, *mut c_void)>(H_FEATURES2.load(Relaxed)) else {
        return;
    };
    f(pd, out);
    if out.is_null() {
        return;
    }
    let features = (out as *mut u8).wrapping_add(16) as *mut c_void;
    patch_features(pd, features, "vkGetPhysicalDeviceFeatures2");
}

fn patch_features(pd: *mut c_void, features: *mut c_void, via: &str) {
    let off = FEATURE_ETC2 * 4;
    let native: u32 = rd(features, off);
    let emulate = native == 0 && emulate_for(pd);
    if emulate {
        wr::<u32>(features, off, 1);
    }
    trace(format_args!(
        "{via}({pd:p}): textureCompressionETC2 host={native} reported={} ASTC_LDR={} BC={}",
        u32::from(emulate) | native,
        rd::<u32>(features, FEATURE_ASTC_LDR * 4),
        rd::<u32>(features, FEATURE_BC * 4),
    ));
}

pub fn patch_format_properties(
    pd: *mut c_void,
    format: u32,
    out: *mut c_void,
    host: extern "C" fn(*mut c_void, u32, *mut c_void),
) {
    let Some((_, sub, name)) = etc_format(format) else { return };
    if out.is_null() || !emulate_for(pd) {
        return;
    }
    let mut s = [0u32; 3];
    host(pd, sub, s.as_mut_ptr() as *mut c_void);
    let optimal = s[1] & EMULATED_FORMAT_FEATURES;
    wr::<[u32; 3]>(out, 0, [0, optimal, 0]);
    trace(format_args!(
        "vkGetPhysicalDeviceFormatProperties({name}): EMULATED from format {sub}, optimalTiling=0x{optimal:x}"
    ));
}

unsafe extern "C" fn get_format_props2(pd: *mut c_void, format: u32, out: *mut c_void) {
    let Some(f) = as_fn::<extern "C" fn(*mut c_void, u32, *mut c_void)>(H_FORMAT_PROPS2.load(Relaxed)) else {
        return;
    };
    let etc = etc_format(format);
    let emulate = etc.is_some() && emulate_for(pd);
    let Some((_, sub, name)) = etc.filter(|_| emulate) else {
        f(pd, format, out);
        if let Some((_, _, name)) = etc {
            if !out.is_null() {
                let p: [u32; 3] = rd(out, 16);
                trace(format_args!(
                    "vkGetPhysicalDeviceFormatProperties2({name}): passthrough linear=0x{:x} optimal=0x{:x} buffer=0x{:x}",
                    p[0], p[1], p[2]
                ));
            }
        }
        return;
    };
    f(pd, sub, out);
    if out.is_null() {
        return;
    }
    let p: [u32; 3] = rd(out, 16);
    let optimal = p[1] & EMULATED_FORMAT_FEATURES;
    wr::<[u32; 3]>(out, 16, [0, optimal, 0]);
    let p3 = chain_find(rd::<*const c_void>(out, 8), ST_FORMAT_PROPERTIES_3) as *mut c_void;
    if !p3.is_null() {
        let o: u64 = rd(p3, 24);
        wr::<[u64; 3]>(p3, 16, [0, o & u64::from(EMULATED_FORMAT_FEATURES), 0]);
    }
    trace(format_args!(
        "vkGetPhysicalDeviceFormatProperties2({name}): EMULATED from format {sub}, optimalTiling=0x{optimal:x}{}",
        if p3.is_null() { "" } else { " (+FormatProperties3)" }
    ));
}

fn image_request_ok(tiling: u32, usage: u32, flags: u32) -> bool {
    tiling == IMAGE_TILING_OPTIMAL && usage & !EMULATED_USAGE == 0 && flags & !EMULATED_CREATE_FLAGS == 0
}

type ImageFormatPropsFn = extern "C" fn(*mut c_void, u32, u32, u32, u32, u32, *mut c_void) -> i32;

unsafe extern "C" fn get_image_format_props(
    pd: *mut c_void,
    format: u32,
    kind: u32,
    tiling: u32,
    usage: u32,
    flags: u32,
    out: *mut c_void,
) -> i32 {
    let Some(f) = as_fn::<ImageFormatPropsFn>(H_IMAGE_FORMAT_PROPS.load(Relaxed)) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    let Some((_, sub, name)) = etc_format(format) else {
        return f(pd, format, kind, tiling, usage, flags, out);
    };
    if !emulate_for(pd) {
        let rc = f(pd, format, kind, tiling, usage, flags, out);
        trace(format_args!(
            "vkGetPhysicalDeviceImageFormatProperties({name}, type={kind}, tiling={tiling}, usage=0x{usage:x}, flags=0x{flags:x}): passthrough rc={rc}"
        ));
        return rc;
    }
    let rc = if image_request_ok(tiling, usage, flags) {
        let rc = f(pd, sub, kind, tiling, usage, flags, out);
        if rc == VK_SUCCESS && !out.is_null() {
            wr::<u32>(out, 20, SAMPLE_COUNT_1);
        }
        rc
    } else {
        VK_ERROR_FORMAT_NOT_SUPPORTED
    };
    trace(format_args!(
        "vkGetPhysicalDeviceImageFormatProperties({name}, type={kind}, tiling={tiling}, usage=0x{usage:x}, flags=0x{flags:x}): EMULATED rc={rc}"
    ));
    rc
}

unsafe extern "C" fn get_image_format_props2(pd: *mut c_void, info: *const c_void, out: *mut c_void) -> i32 {
    let Some(f) =
        as_fn::<extern "C" fn(*mut c_void, *const c_void, *mut c_void) -> i32>(H_IMAGE_FORMAT_PROPS2.load(Relaxed))
    else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    if info.is_null() {
        return f(pd, info, out);
    }
    let format: u32 = rd(info, 16);
    let Some((_, sub, name)) = etc_format(format) else {
        return f(pd, info, out);
    };
    let (kind, tiling, usage, flags): (u32, u32, u32, u32) = (rd(info, 20), rd(info, 24), rd(info, 28), rd(info, 32));
    if !emulate_for(pd) {
        let rc = f(pd, info, out);
        trace(format_args!(
            "vkGetPhysicalDeviceImageFormatProperties2({name}, type={kind}, tiling={tiling}, usage=0x{usage:x}, flags=0x{flags:x}): passthrough rc={rc}"
        ));
        return rc;
    }
    let rc = if image_request_ok(tiling, usage, flags) {
        let mut copy: [u8; 40] = rd(info, 0);
        copy[16..20].copy_from_slice(&sub.to_ne_bytes());
        let rc = f(pd, copy.as_ptr() as *const c_void, out);
        if rc == VK_SUCCESS && !out.is_null() {
            wr::<u32>(out, 16 + 20, SAMPLE_COUNT_1);
        }
        rc
    } else {
        VK_ERROR_FORMAT_NOT_SUPPORTED
    };
    trace(format_args!(
        "vkGetPhysicalDeviceImageFormatProperties2({name}, type={kind}, tiling={tiling}, usage=0x{usage:x}, flags=0x{flags:x}): EMULATED rc={rc}"
    ));
    rc
}

#[repr(C, align(8))]
struct StrippedDeviceInfo {
    info: [u8; 72],
    features: [u32; FEATURE_COUNT],
    features2: [u8; 240],
}

/// What [`strip_device_features`] built for one `vkCreateDevice`: usually a copy
/// of the create info with `textureCompressionETC2` cleared, and, for a
/// `VkPhysicalDeviceFeatures2` that is not first in the chain, the caller's own
/// field cleared for the duration of the call and put back on drop.
pub struct Stripped {
    copy: Option<Box<StrippedDeviceInfo>>,
    restore: Vec<usize>,
}

impl Stripped {
    /// The create info the host driver should be given.
    pub fn as_ptr(&self, original: *const c_void) -> *const c_void {
        self.copy.as_ref().map_or(original, |c| c.info.as_ptr() as *const c_void)
    }
}

impl Drop for Stripped {
    fn drop(&mut self) {
        for &at in &self.restore {
            // SAFETY: `at` is the address of the ETC2 feature word inside the
            // caller's `VkPhysicalDeviceFeatures2`, which outlives the
            // `vkCreateDevice` call this guard lives within.
            unsafe { std::ptr::write_unaligned(at as *mut u32, 1) };
        }
    }
}

pub fn strip_device_features(pd: *mut c_void, info: *const c_void) -> Option<Stripped> {
    if info.is_null() {
        return None;
    }
    strip_with(emulate_for(pd), info)
}

/// The decision and the rewrite, without the physical-device query, so the
/// chain handling can be tested.
fn strip_with(emulate: bool, info: *const c_void) -> Option<Stripped> {
    let enabled: *const c_void = rd(info, 64);
    let head: *const c_void = rd(info, 8);
    let f2 = chain_find(head, ST_PHYSICAL_DEVICE_FEATURES_2);
    let via_features = !enabled.is_null() && rd::<u32>(enabled, FEATURE_ETC2 * 4) != 0;
    let via_features2 = !f2.is_null() && rd::<u32>(f2, 16 + FEATURE_ETC2 * 4) != 0;
    trace(format_args!(
        "vkCreateDevice: pEnabledFeatures={} textureCompressionETC2 via pEnabledFeatures={} via Features2={} (Features2 in chain: {}, first node: {}); emulation {}",
        if enabled.is_null() { "null" } else { "set" },
        via_features,
        via_features2,
        !f2.is_null(),
        !f2.is_null() && f2 == head,
        if emulate { "on" } else { "off" },
    ));
    if !emulate || !(via_features || via_features2) {
        return None;
    }
    let mut s = Stripped { copy: None, restore: Vec::new() };
    if via_features || (via_features2 && f2 == head) {
        let mut c = Box::new(StrippedDeviceInfo { info: rd(info, 0), features: [0; FEATURE_COUNT], features2: [0; 240] });
        if via_features {
            c.features = rd(enabled, 0);
            c.features[FEATURE_ETC2] = 0;
            let p = c.features.as_ptr() as usize;
            c.info[64..72].copy_from_slice(&p.to_ne_bytes());
        }
        if via_features2 && f2 == head {
            c.features2 = rd(f2, 0);
            let at = 16 + FEATURE_ETC2 * 4;
            c.features2[at..at + 4].copy_from_slice(&0u32.to_ne_bytes());
            let p = c.features2.as_ptr() as usize;
            c.info[8..16].copy_from_slice(&p.to_ne_bytes());
        }
        s.copy = Some(c);
    }
    if via_features2 && f2 != head {
        // **Not first in the chain, so the chain cannot be copied.** The nodes
        // ahead of it are structures this module has no reason to know the size
        // of, and relinking around them needs their memory. Handing the driver
        // the feature switched on instead is `VK_ERROR_FEATURE_NOT_PRESENT` from
        // a driver that lacks it, and a client that never starts. So the one
        // word is cleared in place for the length of the call and restored by
        // `Stripped`'s drop; the engine's create info is read by nothing else
        // while the engine is inside this call.
        let at = (f2 as usize) + 16 + FEATURE_ETC2 * 4;
        // SAFETY: `f2` is a live `VkPhysicalDeviceFeatures2` in the caller's
        // chain, and the word is restored before `vkCreateDevice` returns.
        unsafe { std::ptr::write_unaligned(at as *mut u32, 0) };
        s.restore.push(at);
        trace(format_args!("vkCreateDevice: Features2 is not the first pNext node; its ETC2 bit is cleared in place for the call"));
    }
    trace(format_args!("vkCreateDevice: textureCompressionETC2 stripped before the host driver sees it"));
    Some(s)
}

struct Dev {
    device: usize,
    create_buffer: extern "C" fn(usize, *const c_void, *const c_void, *mut u64) -> i32,
    destroy_buffer: extern "C" fn(usize, u64, *const c_void),
    buffer_requirements: extern "C" fn(usize, u64, *mut [u64; 3]),
    allocate_memory: extern "C" fn(usize, *const c_void, *const c_void, *mut u64) -> i32,
    free_memory: extern "C" fn(usize, u64, *const c_void),
    bind_buffer_memory: extern "C" fn(usize, u64, u64, u64) -> i32,
    map_memory: extern "C" fn(usize, u64, u64, u64, u32, *mut *mut c_void) -> i32,
    copy_buffer_to_image: extern "C" fn(*mut c_void, u64, u64, i32, u32, *const c_void),
    memory_types: Vec<u32>,
}

static DEV: AtomicPtr<Dev> = AtomicPtr::new(std::ptr::null_mut());

pub fn device_created(pd: *mut c_void, device: *mut c_void) {
    if !emulate_for(pd) {
        return;
    }
    if !device_emulates(device) {
        emulating().push(device as usize);
    }
    let gdpa = super::vulkan::host_instance_proc(c"vkGetDeviceProcAddr");
    let Some(gdpa) = as_fn::<extern "C" fn(*mut c_void, *const c_char) -> *mut c_void>(gdpa as usize) else {
        unhandled(format_args!("no host vkGetDeviceProcAddr; ETC uploads cannot be decoded"));
        return;
    };
    let get = |n: &CStr| gdpa(device, n.as_ptr()) as usize;
    let memprops = super::vulkan::host_instance_proc(c"vkGetPhysicalDeviceMemoryProperties") as usize;
    let dev = (|| {
        let mut memory_types = Vec::new();
        let mp: extern "C" fn(*mut c_void, *mut c_void) = as_fn(memprops)?;
        let mut raw = [0u8; 520];
        mp(pd, raw.as_mut_ptr() as *mut c_void);
        let count = u32::from_ne_bytes(raw[0..4].try_into().ok()?);
        for i in 0..count.min(32) as usize {
            let o = 4 + i * 8;
            memory_types.push(u32::from_ne_bytes(raw[o..o + 4].try_into().ok()?));
        }
        Some(Dev {
            device: device as usize,
            create_buffer: as_fn(get(c"vkCreateBuffer"))?,
            destroy_buffer: as_fn(get(c"vkDestroyBuffer"))?,
            buffer_requirements: as_fn(get(c"vkGetBufferMemoryRequirements"))?,
            allocate_memory: as_fn(get(c"vkAllocateMemory"))?,
            free_memory: as_fn(get(c"vkFreeMemory"))?,
            bind_buffer_memory: as_fn(get(c"vkBindBufferMemory"))?,
            map_memory: as_fn(get(c"vkMapMemory"))?,
            copy_buffer_to_image: as_fn(get(c"vkCmdCopyBufferToImage"))?,
            memory_types,
        })
    })();
    match dev {
        Some(d) => {
            trace(format_args!(
                "device {device:p}: ETC2/EAC emulation active ({} memory types)",
                d.memory_types.len()
            ));
            forget_device();
            DEV.store(Box::into_raw(Box::new(d)), Release);
        }
        None => unhandled(format_args!("device {device:p}: could not resolve the host calls staging needs")),
    }
}

fn active() -> Option<&'static Dev> {
    let p = DEV.load(Acquire);
    unsafe { p.as_ref() }
}

fn active_device(device: usize) -> Option<&'static Dev> {
    active().filter(|d| d.device == device)
}

fn forget_device() {
    let Some(dev) = active() else { return };
    let cmds: Vec<CmdState> = {
        let mut st = state();
        st.images.clear();
        st.buffers.clear();
        st.mapped.clear();
        st.cmd_pool.clear();
        st.cmds.drain().map(|(_, c)| c).collect()
    };
    LIVE_CMDS.store(0, Relaxed);
    LIVE_IMAGES.store(0, Relaxed);
    let n = cmds.len();
    for c in cmds {
        release_cmd(dev, c);
    }
    DEV.store(std::ptr::null_mut(), Release);
    trace(format_args!("device 0x{:x}: emulation state dropped, {n} command buffer(s) of staging freed", dev.device));
}

unsafe extern "C" fn destroy_device(device: *mut c_void, alloc: *const c_void) {
    if active_device(device as usize).is_some() {
        forget_device();
    }
    emulating().retain(|&d| d != device as usize);
    if let Some(f) = as_fn::<extern "C" fn(*mut c_void, *const c_void)>(H_DESTROY_DEVICE.load(Relaxed)) {
        f(device, alloc);
    }
}

#[derive(Clone, Copy)]
struct EmuImage {
    format: EtcFormat,
    etc: u32,
    substitute: u32,
    width: u32,
    height: u32,
}

struct Chunk {
    buffer: u64,
    memory: u64,
    mapped: usize,
    size: u64,
    used: u64,
}

struct Pending {
    memory: u64,
    mem_offset: u64,
    len: usize,
    hash: u64,
    decoded: bool,
    job: Job,
}

#[derive(Clone, Copy)]
struct Job {
    format: EtcFormat,
    row_blocks: usize,
    rows_of_blocks: usize,
    layer_stride: usize,
    layers: usize,
    width: usize,
    height: usize,
    dst: usize,
    dst_len: usize,
}

#[derive(Default)]
struct CmdState {
    chunks: Vec<Chunk>,
    pending: Vec<Pending>,
}

#[derive(Default)]
struct State {
    images: HashMap<u64, EmuImage>,
    buffers: HashMap<u64, (u64, u64)>,
    mapped: HashMap<u64, (usize, u64, u64)>,
    cmd_pool: HashMap<usize, u64>,
    cmds: HashMap<usize, CmdState>,
}

fn state() -> MutexGuard<'static, State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner())
}

static LIVE_CMDS: AtomicUsize = AtomicUsize::new(0);
static DECODE_US: AtomicUsize = AtomicUsize::new(0);
static LIVE_IMAGES: AtomicUsize = AtomicUsize::new(0);

fn is_emulated(image: u64) -> Option<EmuImage> {
    if LIVE_IMAGES.load(Relaxed) == 0 {
        return None;
    }
    state().images.get(&image).copied()
}

/// A `VkDeviceImageMemoryRequirements` and the `VkImageCreateInfo` it points at,
/// copied with the format swapped for its substitute.
#[repr(C, align(8))]
struct SubstitutedImageInfo {
    info: [u8; 32],
    create: [u8; 88],
}

/// The copy, if `info` asks about an ETC2/EAC image. The memory requirements of
/// the image the driver will actually be given are the substitute's, and the
/// raw format would have the driver sizing a format it does not support.
fn substitute_image_info(info: *const c_void) -> Option<Box<SubstitutedImageInfo>> {
    if info.is_null() {
        return None;
    }
    let create: *const c_void = rd(info, 16);
    if create.is_null() {
        return None;
    }
    let (_, sub, _) = etc_format(rd(create, 24))?;
    let mut c = Box::new(SubstitutedImageInfo { info: rd(info, 0), create: rd(create, 0) });
    c.create[24..28].copy_from_slice(&sub.to_ne_bytes());
    let p = c.create.as_ptr() as usize;
    c.info[16..24].copy_from_slice(&p.to_ne_bytes());
    Some(c)
}

unsafe extern "C" fn get_device_image_requirements(device: *mut c_void, info: *const c_void, out: *mut c_void) {
    let Some(f) = as_fn::<extern "C" fn(*mut c_void, *const c_void, *mut c_void)>(H_DEVICE_IMAGE_REQS.load(Relaxed))
    else {
        return;
    };
    match device_emulates(device).then(|| substitute_image_info(info)).flatten() {
        Some(c) => {
            trace(format_args!("vkGetDeviceImageMemoryRequirements: ETC2/EAC image sized as its substitute"));
            f(device, c.info.as_ptr() as *const c_void, out)
        }
        None => f(device, info, out),
    }
}

unsafe extern "C" fn get_device_image_sparse_requirements(
    device: *mut c_void,
    info: *const c_void,
    count: *mut u32,
    out: *mut c_void,
) {
    let Some(f) = as_fn::<extern "C" fn(*mut c_void, *const c_void, *mut u32, *mut c_void)>(
        H_DEVICE_IMAGE_SPARSE_REQS.load(Relaxed),
    ) else {
        return;
    };
    match device_emulates(device).then(|| substitute_image_info(info)).flatten() {
        Some(c) => f(device, c.info.as_ptr() as *const c_void, count, out),
        None => f(device, info, count, out),
    }
}

unsafe extern "C" fn create_image(device: *mut c_void, info: *const c_void, alloc: *const c_void, out: *mut u64) -> i32 {
    let Some(f) =
        as_fn::<extern "C" fn(*mut c_void, *const c_void, *const c_void, *mut u64) -> i32>(H_CREATE_IMAGE.load(Relaxed))
    else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    if info.is_null() {
        return f(device, info, alloc, out);
    }
    let format: u32 = rd(info, 24);
    let Some((kind, sub, name)) = etc_format(format) else {
        return f(device, info, alloc, out);
    };
    let flags: u32 = rd(info, 16);
    let image_type: u32 = rd(info, 20);
    let extent: [u32; 3] = rd(info, 28);
    let (mips, layers, samples, tiling, usage): (u32, u32, u32, u32, u32) =
        (rd(info, 40), rd(info, 44), rd(info, 48), rd(info, 52), rd(info, 56));
    let describe = format!(
        "{name} type={image_type} {}x{}x{} mips={mips} layers={layers} samples={samples} tiling={tiling} usage=0x{usage:x} flags=0x{flags:x}",
        extent[0], extent[1], extent[2]
    );
    if active_device(device as usize).is_none() {
        if device_emulates(device) {
            // The engine was told ETC2 works on this device and the state to
            // serve it is gone or never came up (`device_created` could not
            // resolve the staging calls, or a second emulating device replaced
            // this one's). Passing the raw format on is a driver that lacks it
            // being asked to create the image; failing here says so by name.
            unhandled(format_args!(
                "vkCreateImage({describe}): device {device:p} is emulating ETC2/EAC but has no emulation state; refused"
            ));
            return VK_ERROR_FORMAT_NOT_SUPPORTED;
        }
        let rc = f(device, info, alloc, out);
        trace(format_args!("vkCreateImage({describe}): passthrough rc={rc}"));
        return rc;
    }
    if !image_request_ok(tiling, usage, flags) {
        unhandled(format_args!("vkCreateImage({describe}): usage/tiling/flags beyond what is advertised"));
    }
    if !chain_find(rd::<*const c_void>(info, 8), ST_IMAGE_FORMAT_LIST_CREATE_INFO).is_null() {
        unhandled(format_args!("vkCreateImage({describe}): VkImageFormatListCreateInfo is forwarded unchanged"));
    }
    let mut copy: [u8; 88] = rd(info, 0);
    copy[24..28].copy_from_slice(&sub.to_ne_bytes());
    let rc = f(device, copy.as_ptr() as *const c_void, alloc, out);
    if rc == VK_SUCCESS && !out.is_null() {
        let image: u64 = rd(out as *const c_void, 0);
        state().images.insert(
            image,
            EmuImage { format: kind, etc: format, substitute: sub, width: extent[0], height: extent[1] },
        );
        LIVE_IMAGES.fetch_add(1, Relaxed);
        glcount::ETC_IMAGE_EMULATED.fetch_add(1, Relaxed);
        trace(format_args!("vkCreateImage({describe}): EMULATED as format {sub}, image=0x{image:x}"));
    } else {
        trace(format_args!("vkCreateImage({describe}): substitute format {sub} FAILED rc={rc}"));
    }
    rc
}

unsafe extern "C" fn destroy_image(device: *mut c_void, image: u64, alloc: *const c_void) {
    if LIVE_IMAGES.load(Relaxed) != 0 && state().images.remove(&image).is_some() {
        LIVE_IMAGES.fetch_sub(1, Relaxed);
    }
    if let Some(f) = as_fn::<extern "C" fn(*mut c_void, u64, *const c_void)>(H_DESTROY_IMAGE.load(Relaxed)) {
        f(device, image, alloc);
    }
}

unsafe extern "C" fn create_image_view(device: *mut c_void, info: *const c_void, alloc: *const c_void, out: *mut u64) -> i32 {
    let Some(f) = as_fn::<extern "C" fn(*mut c_void, *const c_void, *const c_void, *mut u64) -> i32>(
        H_CREATE_IMAGE_VIEW.load(Relaxed),
    ) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    if info.is_null() {
        return f(device, info, alloc, out);
    }
    let image: u64 = rd(info, 24);
    let Some(emu) = is_emulated(image) else {
        return f(device, info, alloc, out);
    };
    let view_format: u32 = rd(info, 36);
    let view_type: u32 = rd(info, 32);
    let sub = match etc_format(view_format) {
        Some((kind, sub, _)) if kind == emu.format => sub,
        _ => {
            unhandled(format_args!(
                "vkCreateImageView on emulated image 0x{image:x} (format {}) with view format {view_format}; forwarded unchanged",
                emu.etc
            ));
            return f(device, info, alloc, out);
        }
    };
    let mut copy: [u8; 80] = rd(info, 0);
    copy[36..40].copy_from_slice(&sub.to_ne_bytes());
    let rc = f(device, copy.as_ptr() as *const c_void, alloc, out);
    trace(format_args!(
        "vkCreateImageView(image=0x{image:x}, viewType={view_type}, format {view_format} -> {sub}): rc={rc}"
    ));
    rc
}

fn hash(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325 ^ data.len() as u64;
    // `chunks_exact`, not `slice::as_chunks`, which needs Rust 1.88 where this
    // workspace declares 1.75. Same words, same remainder, same hash.
    let mut words = data.chunks_exact(8);
    for c in &mut words {
        let w = u64::from_le_bytes([c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]]);
        h = (h ^ w).wrapping_mul(0x0100_0000_01b3).rotate_left(29);
    }
    for &b in words.remainder() {
        h = (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
    }
    h
}

fn run_job(job: &Job, src: &[u8]) -> Result<(), String> {
    let dst = slice_mut(job.dst as *mut c_void, job.dst_len);
    let tb = job.format.texel_bytes();
    let bb = job.format.block_bytes();
    let per_layer = job.width * job.height * tb;
    let block_rows = job.height.div_ceil(4);
    let threads = if job.width * job.height >= 256 * 256 {
        std::thread::available_parallelism().map_or(1, |n| n.get()).min(8)
    } else {
        1
    };
    let band = block_rows.div_ceil(threads).max(1);
    for (layer, out) in dst.chunks_mut(per_layer).take(job.layers).enumerate() {
        let data = src.get(layer * job.layer_stride..).ok_or("layer outside the source")?;
        if threads == 1 {
            let surface = Surface { data, row_blocks: job.row_blocks, rows_of_blocks: job.rows_of_blocks };
            etc_decode::decode_region(job.format, &surface, job.width, job.height, out)?;
            continue;
        }
        let results: Vec<Result<(), String>> = std::thread::scope(|s| {
            let handles: Vec<_> = out
                .chunks_mut(band * 4 * job.width * tb)
                .enumerate()
                .map(|(i, rows)| {
                    let first = i * band;
                    s.spawn(move || {
                        let height = (job.height - first * 4).min(band * 4);
                        let surface = Surface {
                            data: data.get(first * job.row_blocks * bb..).ok_or("band outside the source")?,
                            row_blocks: job.row_blocks,
                            rows_of_blocks: job.rows_of_blocks.saturating_sub(first),
                        };
                        etc_decode::decode_region(job.format, &surface, job.width, height, rows)
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap_or_else(|_| Err("decode thread panicked".into()))).collect()
        });
        results.into_iter().collect::<Result<(), String>>()?;
    }
    Ok(())
}

fn source_bytes(
    mapped: &HashMap<u64, (usize, u64, u64)>,
    memory: u64,
    mem_offset: u64,
    len: usize,
) -> Result<&'static [u8], String> {
    let &(ptr, map_offset, map_size) =
        mapped.get(&memory).ok_or_else(|| format!("memory 0x{memory:x} is not mapped"))?;
    if mem_offset < map_offset
        || (map_size != WHOLE_SIZE && mem_offset + len as u64 > map_offset.saturating_add(map_size))
    {
        return Err(format!(
            "source range {mem_offset}+{len} lies outside the mapping {map_offset}+{map_size} of memory 0x{memory:x}"
        ));
    }
    Ok(slice((ptr + (mem_offset - map_offset) as usize) as *const c_void, len))
}

fn staging(dev: &Dev, cmd: &mut CmdState, len: u64) -> Result<(u64, u64, usize), String> {
    if let Some(c) = cmd.chunks.last_mut() {
        let at = c.used.div_ceil(STAGING_ALIGN) * STAGING_ALIGN;
        if at + len <= c.size {
            c.used = at + len;
            return Ok((c.buffer, at, c.mapped + at as usize));
        }
    }
    let size = len.max(STAGING_CHUNK);
    let mut bci = [0u8; 56];
    bci[0..4].copy_from_slice(&ST_BUFFER_CREATE_INFO.to_ne_bytes());
    bci[24..32].copy_from_slice(&size.to_ne_bytes());
    bci[32..36].copy_from_slice(&BUFFER_USAGE_TRANSFER_SRC.to_ne_bytes());
    let mut buffer = 0u64;
    let rc = (dev.create_buffer)(dev.device, bci.as_ptr() as *const c_void, std::ptr::null(), &mut buffer);
    if rc != VK_SUCCESS {
        return Err(format!("vkCreateBuffer({size}) rc={rc}"));
    }
    let mut req = [0u64; 3];
    (dev.buffer_requirements)(dev.device, buffer, &mut req);
    let bits = req[2] as u32;
    let wanted = MEMORY_HOST_VISIBLE | MEMORY_HOST_COHERENT;
    let pick = |avoid: u32| {
        dev.memory_types
            .iter()
            .enumerate()
            .find(|&(ref i, &f)| bits & (1 << i) != 0 && f & wanted == wanted && f & avoid == 0)
            .map(|(i, _)| i as u32)
    };
    let Some(type_index) = pick(MEMORY_DEVICE_LOCAL).or_else(|| pick(0)) else {
        (dev.destroy_buffer)(dev.device, buffer, std::ptr::null());
        return Err("no host-visible coherent memory type for staging".into());
    };
    let mut mai = [0u8; 32];
    mai[0..4].copy_from_slice(&ST_MEMORY_ALLOCATE_INFO.to_ne_bytes());
    mai[16..24].copy_from_slice(&req[0].to_ne_bytes());
    mai[24..28].copy_from_slice(&type_index.to_ne_bytes());
    let mut memory = 0u64;
    let rc = (dev.allocate_memory)(dev.device, mai.as_ptr() as *const c_void, std::ptr::null(), &mut memory);
    if rc != VK_SUCCESS {
        (dev.destroy_buffer)(dev.device, buffer, std::ptr::null());
        return Err(format!("vkAllocateMemory({}) rc={rc}", req[0]));
    }
    let mut mapped = std::ptr::null_mut();
    let rc_bind = (dev.bind_buffer_memory)(dev.device, buffer, memory, 0);
    let rc_map = (dev.map_memory)(dev.device, memory, 0, WHOLE_SIZE, 0, &mut mapped);
    if rc_bind != VK_SUCCESS || rc_map != VK_SUCCESS || mapped.is_null() {
        (dev.destroy_buffer)(dev.device, buffer, std::ptr::null());
        (dev.free_memory)(dev.device, memory, std::ptr::null());
        return Err(format!("staging bind rc={rc_bind} map rc={rc_map}"));
    }
    cmd.chunks.push(Chunk { buffer, memory, mapped: mapped as usize, size, used: len });
    Ok((buffer, 0, mapped as usize))
}

fn release_cmd(dev: &Dev, cmd: CmdState) {
    for c in cmd.chunks {
        (dev.destroy_buffer)(dev.device, c.buffer, std::ptr::null());
        (dev.free_memory)(dev.device, c.memory, std::ptr::null());
    }
}

fn release_cmds(cmds: &[usize]) {
    if LIVE_CMDS.load(Relaxed) == 0 {
        return;
    }
    let Some(dev) = active() else { return };
    let mut released = Vec::new();
    {
        let mut st = state();
        for c in cmds {
            if let Some(s) = st.cmds.remove(c) {
                released.push(s);
            }
        }
    }
    if released.is_empty() {
        return;
    }
    LIVE_CMDS.fetch_sub(released.len(), Relaxed);
    let bytes: u64 = released.iter().flat_map(|c| c.chunks.iter()).map(|c| c.size).sum();
    let regions: usize = released.iter().map(|c| c.pending.len()).sum();
    trace(format_args!(
        "staging released: {} command buffer(s), {regions} region(s), {} KiB; {} still holding staging",
        released.len(),
        bytes / 1024,
        LIVE_CMDS.load(Relaxed)
    ));
    for s in released {
        release_cmd(dev, s);
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct BufferImageCopy {
    buffer_offset: u64,
    buffer_row_length: u32,
    buffer_image_height: u32,
    aspect_mask: u32,
    mip_level: u32,
    base_array_layer: u32,
    layer_count: u32,
    image_offset: [i32; 3],
    image_extent: [u32; 3],
}

fn emulate_copy(
    dev: &Dev,
    cmd: *mut c_void,
    src_buffer: u64,
    image: u64,
    emu: EmuImage,
    layout: i32,
    region: &BufferImageCopy,
) -> Result<String, String> {
    let fmt = emu.format;
    let [w, h, d] = region.image_extent;
    if d != 1 {
        return Err(format!("depth {d} copies are not emulated"));
    }
    let (w, h) = (w as usize, h as usize);
    let row_texels = if region.buffer_row_length == 0 { w } else { region.buffer_row_length as usize };
    let img_texels = if region.buffer_image_height == 0 { h } else { region.buffer_image_height as usize };
    let row_blocks = row_texels.div_ceil(4);
    let rows_of_blocks = img_texels.div_ceil(4);
    let layer_stride = row_blocks * rows_of_blocks * fmt.block_bytes();
    let layers = region.layer_count.max(1) as usize;
    let last_layer = (h.div_ceil(4).saturating_sub(1) * row_blocks + w.div_ceil(4)) * fmt.block_bytes();
    let src_len = (layers - 1) * layer_stride + last_layer;
    let dst_len = w * h * layers * fmt.texel_bytes();

    let mut guard = state();
    let st = &mut *guard;
    let &(memory, bind_offset) =
        st.buffers.get(&src_buffer).ok_or_else(|| format!("source buffer 0x{src_buffer:x} has no recorded binding"))?;
    let mem_offset = bind_offset + region.buffer_offset;
    let entry = st.cmds.entry(cmd as usize).or_insert_with(|| {
        LIVE_CMDS.fetch_add(1, Relaxed);
        CmdState::default()
    });
    let (buffer, offset, dst) = staging(dev, entry, dst_len as u64)?;
    let job = Job { format: fmt, row_blocks, rows_of_blocks, layer_stride, layers, width: w, height: h, dst, dst_len };
    let note;
    let (decoded, h64, note) = match source_bytes(&st.mapped, memory, mem_offset, src_len) {
        Ok(bytes) => {
            let t = std::time::Instant::now();
            let hv = hash(bytes);
            run_job(&job, bytes)?;
            let us = t.elapsed().as_micros() as u64;
            DECODE_US.fetch_add(us as usize, Relaxed);
            note = format!("decoded at record in {us} us ({} ms total)", DECODE_US.load(Relaxed) / 1000);
            (true, hv, note.as_str())
        }
        Err(e) => {
            slice_mut(dst as *mut c_void, dst_len).fill(0);
            unhandled(format_args!("source not readable at record time ({e}); will retry at submit"));
            (false, 0, "deferred to submit")
        }
    };
    entry.pending.push(Pending { memory, mem_offset, len: src_len, hash: h64, decoded, job });
    drop(guard);
    let out = BufferImageCopy { buffer_offset: offset, buffer_row_length: 0, buffer_image_height: 0, ..*region };
    (dev.copy_buffer_to_image)(cmd, buffer, image, layout, 1, &out as *const BufferImageCopy as *const c_void);
    glcount::ETC_REGION_DECODED.fetch_add(1, Relaxed);
    Ok(format!(
        "{note}: src buffer 0x{src_buffer:x} mem 0x{memory:x}+{mem_offset} {src_len} B, rowLength={} imageHeight={} -> staging 0x{buffer:x}+{offset} {dst_len} B",
        region.buffer_row_length, region.buffer_image_height
    ))
}

type CopyBufferToImageFn = extern "C" fn(*mut c_void, u64, u64, i32, u32, *const c_void);

unsafe extern "C" fn cmd_copy_buffer_to_image(
    cmd: *mut c_void,
    src: u64,
    image: u64,
    layout: i32,
    count: u32,
    regions: *const c_void,
) {
    let Some(f) = as_fn::<CopyBufferToImageFn>(H_CMD_COPY_BUFFER_TO_IMAGE.load(Relaxed)) else { return };
    let (Some(emu), Some(dev)) = (is_emulated(image), active()) else {
        return f(cmd, src, image, layout, count, regions);
    };
    let regions: Vec<BufferImageCopy> =
        (0..count as usize).map(|i| rd(regions, i * std::mem::size_of::<BufferImageCopy>())).collect();
    copy_regions(dev, cmd, src, image, emu, layout, &regions, "vkCmdCopyBufferToImage");
}

#[allow(clippy::too_many_arguments)]
fn copy_regions(
    dev: &Dev,
    cmd: *mut c_void,
    src: u64,
    image: u64,
    emu: EmuImage,
    layout: i32,
    regions: &[BufferImageCopy],
    via: &str,
) {
    for r in regions {
        let what = format!(
            "{via}(image=0x{image:x} {}x{} fmt={}->{} mip={} layers={}+{} offset={:?} extent={:?} bufferOffset={})",
            emu.width,
            emu.height,
            emu.etc,
            emu.substitute,
            r.mip_level,
            r.base_array_layer,
            r.layer_count,
            r.image_offset,
            r.image_extent,
            r.buffer_offset
        );
        match emulate_copy(dev, cmd, src, image, emu, layout, r) {
            Ok(how) => trace(format_args!("{what}: {how}")),
            Err(e) => unhandled(format_args!("{what}: region skipped, {e}")),
        }
    }
}

unsafe extern "C" fn cmd_copy_buffer_to_image2(cmd: *mut c_void, info: *const c_void) {
    let Some(f) = as_fn::<extern "C" fn(*mut c_void, *const c_void)>(H_CMD_COPY_BUFFER_TO_IMAGE2.load(Relaxed))
    else {
        return;
    };
    if info.is_null() {
        return f(cmd, info);
    }
    let image: u64 = rd(info, 24);
    let (Some(emu), Some(dev)) = (is_emulated(image), active()) else {
        return f(cmd, info);
    };
    let src: u64 = rd(info, 16);
    let layout: i32 = rd(info, 32);
    let count: u32 = rd(info, 36);
    let p: *const c_void = rd(info, 40);
    let regions: Vec<BufferImageCopy> = (0..count as usize)
        .map(|i| {
            let r = (p as *const u8).wrapping_add(i * 72) as *const c_void;
            if !rd::<*const c_void>(r, 8).is_null() {
                unhandled(format_args!("vkCmdCopyBufferToImage2 region with a pNext chain; the chain is ignored"));
            }
            BufferImageCopy {
                buffer_offset: rd(r, 16),
                buffer_row_length: rd(r, 24),
                buffer_image_height: rd(r, 28),
                aspect_mask: rd(r, 32),
                mip_level: rd(r, 36),
                base_array_layer: rd(r, 40),
                layer_count: rd(r, 44),
                image_offset: rd(r, 48),
                image_extent: rd(r, 60),
            }
        })
        .collect();
    copy_regions(dev, cmd, src, image, emu, layout, &regions, "vkCmdCopyBufferToImage2");
}

unsafe extern "C" fn cmd_copy_image(
    cmd: *mut c_void,
    src: u64,
    src_layout: i32,
    dst: u64,
    dst_layout: i32,
    count: u32,
    regions: *const c_void,
) {
    let Some(f) = as_fn::<extern "C" fn(*mut c_void, u64, i32, u64, i32, u32, *const c_void)>(
        H_CMD_COPY_IMAGE.load(Relaxed),
    ) else {
        return;
    };
    if check_image_pair("vkCmdCopyImage", src, dst) {
        f(cmd, src, src_layout, dst, dst_layout, count, regions);
    }
}

unsafe extern "C" fn cmd_copy_image2(cmd: *mut c_void, info: *const c_void) {
    let Some(f) = as_fn::<extern "C" fn(*mut c_void, *const c_void)>(H_CMD_COPY_IMAGE2.load(Relaxed)) else {
        return;
    };
    if info.is_null() || check_image_pair("vkCmdCopyImage2", rd(info, 16), rd(info, 32)) {
        f(cmd, info);
    }
}

fn check_image_pair(via: &str, src: u64, dst: u64) -> bool {
    let (s, d) = (is_emulated(src), is_emulated(dst));
    match (s, d) {
        (None, None) => true,
        (Some(a), Some(b)) if a.format == b.format => {
            trace(format_args!(
                "{via}(0x{src:x} -> 0x{dst:x}): both emulated as the same format, copied texel for texel"
            ));
            true
        }
        _ => {
            unhandled(format_args!(
                "{via}(0x{src:x} -> 0x{dst:x}): an emulated ETC image copied against a different format; skipped"
            ));
            false
        }
    }
}

unsafe extern "C" fn cmd_blit_image(
    cmd: *mut c_void,
    src: u64,
    src_layout: i32,
    dst: u64,
    dst_layout: i32,
    count: u32,
    regions: *const c_void,
    filter: i32,
) {
    let Some(f) = as_fn::<extern "C" fn(*mut c_void, u64, i32, u64, i32, u32, *const c_void, i32)>(
        H_CMD_BLIT_IMAGE.load(Relaxed),
    ) else {
        return;
    };
    note_blit("vkCmdBlitImage", src, dst);
    f(cmd, src, src_layout, dst, dst_layout, count, regions, filter);
}

unsafe extern "C" fn cmd_blit_image2(cmd: *mut c_void, info: *const c_void) {
    let Some(f) = as_fn::<extern "C" fn(*mut c_void, *const c_void)>(H_CMD_BLIT_IMAGE2.load(Relaxed)) else {
        return;
    };
    if !info.is_null() {
        note_blit("vkCmdBlitImage2", rd(info, 16), rd(info, 32));
    }
    f(cmd, info);
}

fn note_blit(via: &str, src: u64, dst: u64) {
    if is_emulated(src).is_some() || is_emulated(dst).is_some() {
        unhandled(format_args!(
            "{via}(0x{src:x} -> 0x{dst:x}) touches an emulated ETC image; BLIT is not advertised, forwarded on the substitute"
        ));
    }
}

unsafe extern "C" fn cmd_copy_image_to_buffer(
    cmd: *mut c_void,
    image: u64,
    layout: i32,
    dst: u64,
    count: u32,
    regions: *const c_void,
) {
    let Some(f) = as_fn::<extern "C" fn(*mut c_void, u64, i32, u64, u32, *const c_void)>(
        H_CMD_COPY_IMAGE_TO_BUFFER.load(Relaxed),
    ) else {
        return;
    };
    if is_emulated(image).is_some() {
        unhandled(format_args!(
            "vkCmdCopyImageToBuffer from emulated ETC image 0x{image:x}: the substitute holds decoded texels, skipped"
        ));
        return;
    }
    f(cmd, image, layout, dst, count, regions);
}

unsafe extern "C" fn cmd_copy_image_to_buffer2(cmd: *mut c_void, info: *const c_void) {
    let Some(f) = as_fn::<extern "C" fn(*mut c_void, *const c_void)>(H_CMD_COPY_IMAGE_TO_BUFFER2.load(Relaxed))
    else {
        return;
    };
    if !info.is_null() {
        let image: u64 = rd(info, 16);
        if is_emulated(image).is_some() {
            unhandled(format_args!(
                "vkCmdCopyImageToBuffer2 from emulated ETC image 0x{image:x}: the substitute holds decoded texels, skipped"
            ));
            return;
        }
    }
    f(cmd, info);
}

unsafe extern "C" fn bind_buffer_memory(device: *mut c_void, buffer: u64, memory: u64, offset: u64) -> i32 {
    let Some(f) = as_fn::<extern "C" fn(*mut c_void, u64, u64, u64) -> i32>(H_BIND_BUFFER_MEMORY.load(Relaxed))
    else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    let rc = f(device, buffer, memory, offset);
    if rc == VK_SUCCESS && active_device(device as usize).is_some() {
        state().buffers.insert(buffer, (memory, offset));
    }
    rc
}

unsafe extern "C" fn bind_buffer_memory2(device: *mut c_void, count: u32, infos: *const c_void) -> i32 {
    let Some(f) = as_fn::<extern "C" fn(*mut c_void, u32, *const c_void) -> i32>(H_BIND_BUFFER_MEMORY2.load(Relaxed))
    else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    let rc = f(device, count, infos);
    if rc == VK_SUCCESS && !infos.is_null() && active_device(device as usize).is_some() {
        let mut st = state();
        for i in 0..count as usize {
            let base = (infos as *const u8).wrapping_add(i * 40) as *const c_void;
            st.buffers.insert(rd(base, 16), (rd(base, 24), rd(base, 32)));
        }
    }
    rc
}

unsafe extern "C" fn destroy_buffer(device: *mut c_void, buffer: u64, alloc: *const c_void) {
    if active_device(device as usize).is_some() {
        state().buffers.remove(&buffer);
    }
    if let Some(f) = as_fn::<extern "C" fn(*mut c_void, u64, *const c_void)>(H_DESTROY_BUFFER.load(Relaxed)) {
        f(device, buffer, alloc);
    }
}

unsafe extern "C" fn map_memory(
    device: *mut c_void,
    memory: u64,
    offset: u64,
    size: u64,
    flags: u32,
    out: *mut *mut c_void,
) -> i32 {
    let Some(f) = as_fn::<extern "C" fn(*mut c_void, u64, u64, u64, u32, *mut *mut c_void) -> i32>(
        H_MAP_MEMORY.load(Relaxed),
    ) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    let rc = f(device, memory, offset, size, flags, out);
    if rc == VK_SUCCESS && !out.is_null() && active_device(device as usize).is_some() {
        let p: usize = rd(out as *const c_void, 0);
        state().mapped.insert(memory, (p, offset, size));
    }
    rc
}

unsafe extern "C" fn map_memory2(device: *mut c_void, info: *const c_void, out: *mut *mut c_void) -> i32 {
    let Some(f) =
        as_fn::<extern "C" fn(*mut c_void, *const c_void, *mut *mut c_void) -> i32>(H_MAP_MEMORY2.load(Relaxed))
    else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    let rc = f(device, info, out);
    if rc == VK_SUCCESS && !info.is_null() && !out.is_null() && active_device(device as usize).is_some() {
        let p: usize = rd(out as *const c_void, 0);
        state().mapped.insert(rd(info, 24), (p, rd(info, 32), rd(info, 40)));
    }
    rc
}

unsafe extern "C" fn unmap_memory(device: *mut c_void, memory: u64) {
    if active_device(device as usize).is_some() {
        state().mapped.remove(&memory);
    }
    if let Some(f) = as_fn::<extern "C" fn(*mut c_void, u64)>(H_UNMAP_MEMORY.load(Relaxed)) {
        f(device, memory);
    }
}

unsafe extern "C" fn free_memory(device: *mut c_void, memory: u64, alloc: *const c_void) {
    if active_device(device as usize).is_some() {
        state().mapped.remove(&memory);
    }
    if let Some(f) = as_fn::<extern "C" fn(*mut c_void, u64, *const c_void)>(H_FREE_MEMORY.load(Relaxed)) {
        f(device, memory, alloc);
    }
}

fn check_pending(cmds: impl Iterator<Item = usize>) {
    if LIVE_CMDS.load(Relaxed) == 0 {
        return;
    }
    let mut st = state();
    let st = &mut *st;
    let (mut same, mut gone) = (0usize, 0usize);
    for c in cmds {
        let Some(cs) = st.cmds.get_mut(&c) else { continue };
        for p in &mut cs.pending {
            let bytes = match source_bytes(&st.mapped, p.memory, p.mem_offset, p.len) {
                Ok(b) => b,
                Err(e) => {
                    gone += 1;
                    if !p.decoded {
                        unhandled(format_args!("submit: deferred ETC region still unreadable ({e}); texels stay zero"));
                        p.decoded = true;
                    }
                    continue;
                }
            };
            let h = hash(bytes);
            if p.decoded && h == p.hash {
                same += 1;
                continue;
            }
            match run_job(&p.job, bytes) {
                Ok(()) => {
                    if p.decoded {
                        glcount::ETC_LATE_WRITE.fetch_add(1, Relaxed);
                    }
                    trace(format_args!(
                        "submit: ETC region re-decoded ({}), mem 0x{:x}+{} {} B",
                        if p.decoded { "source changed after recording" } else { "deferred at record" },
                        p.memory,
                        p.mem_offset,
                        p.len
                    ));
                    p.decoded = true;
                    p.hash = h;
                }
                Err(e) => unhandled(format_args!("submit: decode failed: {e}")),
            }
        }
    }
    if same + gone > 0 {
        trace(format_args!(
            "submit: {same} ETC region(s) verified unchanged since recording, {gone} whose source was no longer mapped"
        ));
    }
}

unsafe extern "C" fn queue_submit(queue: *mut c_void, count: u32, submits: *const c_void, fence: u64) -> i32 {
    let Some(f) =
        as_fn::<extern "C" fn(*mut c_void, u32, *const c_void, u64) -> i32>(H_QUEUE_SUBMIT.load(Relaxed))
    else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    if LIVE_CMDS.load(Relaxed) != 0 && !submits.is_null() {
        let mut all = Vec::new();
        for i in 0..count as usize {
            let s = (submits as *const u8).wrapping_add(i * 72) as *const c_void;
            let n: u32 = rd(s, 40);
            let p: *const c_void = rd(s, 48);
            all.extend((0..n as usize).map(|j| rd::<usize>(p, j * 8)));
        }
        check_pending(all.into_iter());
    }
    f(queue, count, submits, fence)
}

unsafe extern "C" fn queue_submit2(queue: *mut c_void, count: u32, submits: *const c_void, fence: u64) -> i32 {
    let Some(f) =
        as_fn::<extern "C" fn(*mut c_void, u32, *const c_void, u64) -> i32>(H_QUEUE_SUBMIT2.load(Relaxed))
    else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    if LIVE_CMDS.load(Relaxed) != 0 && !submits.is_null() {
        let mut all = Vec::new();
        for i in 0..count as usize {
            let s = (submits as *const u8).wrapping_add(i * 64) as *const c_void;
            let n: u32 = rd(s, 32);
            let p: *const c_void = rd(s, 40);
            all.extend((0..n as usize).map(|j| rd::<usize>(p, j * 32 + 16)));
        }
        check_pending(all.into_iter());
    }
    f(queue, count, submits, fence)
}

unsafe extern "C" fn allocate_command_buffers(device: *mut c_void, info: *const c_void, out: *mut usize) -> i32 {
    let Some(f) =
        as_fn::<extern "C" fn(*mut c_void, *const c_void, *mut usize) -> i32>(H_ALLOCATE_COMMAND_BUFFERS.load(Relaxed))
    else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    let rc = f(device, info, out);
    if rc == VK_SUCCESS && !info.is_null() && !out.is_null() && active_device(device as usize).is_some() {
        let pool: u64 = rd(info, 16);
        let n: u32 = rd(info, 28);
        let mut st = state();
        for i in 0..n as usize {
            st.cmd_pool.insert(rd(out as *const c_void, i * 8), pool);
        }
    }
    rc
}

unsafe extern "C" fn begin_command_buffer(cmd: *mut c_void, info: *const c_void) -> i32 {
    let Some(f) = as_fn::<extern "C" fn(*mut c_void, *const c_void) -> i32>(H_BEGIN_COMMAND_BUFFER.load(Relaxed))
    else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    release_cmds(&[cmd as usize]);
    f(cmd, info)
}

unsafe extern "C" fn reset_command_buffer(cmd: *mut c_void, flags: u32) -> i32 {
    let Some(f) = as_fn::<extern "C" fn(*mut c_void, u32) -> i32>(H_RESET_COMMAND_BUFFER.load(Relaxed)) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    release_cmds(&[cmd as usize]);
    f(cmd, flags)
}

unsafe extern "C" fn free_command_buffers(device: *mut c_void, pool: u64, count: u32, cmds: *const usize) {
    let Some(f) = as_fn::<extern "C" fn(*mut c_void, u64, u32, *const usize)>(H_FREE_COMMAND_BUFFERS.load(Relaxed))
    else {
        return;
    };
    if !cmds.is_null() && active_device(device as usize).is_some() {
        let list: Vec<usize> = (0..count as usize).map(|i| rd(cmds as *const c_void, i * 8)).collect();
        release_cmds(&list);
        let mut st = state();
        for c in &list {
            st.cmd_pool.remove(c);
        }
    }
    f(device, pool, count, cmds);
}

fn cmds_of_pool(pool: u64, forget: bool) -> Vec<usize> {
    let mut st = state();
    let list: Vec<usize> = st.cmd_pool.iter().filter(|&(_, &p)| p == pool).map(|(&c, _)| c).collect();
    if forget {
        for c in &list {
            st.cmd_pool.remove(c);
        }
    }
    list
}

unsafe extern "C" fn reset_command_pool(device: *mut c_void, pool: u64, flags: u32) -> i32 {
    let Some(f) = as_fn::<extern "C" fn(*mut c_void, u64, u32) -> i32>(H_RESET_COMMAND_POOL.load(Relaxed)) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    if LIVE_CMDS.load(Relaxed) != 0 && active_device(device as usize).is_some() {
        release_cmds(&cmds_of_pool(pool, false));
    }
    f(device, pool, flags)
}

unsafe extern "C" fn destroy_command_pool(device: *mut c_void, pool: u64, alloc: *const c_void) {
    let Some(f) = as_fn::<extern "C" fn(*mut c_void, u64, *const c_void)>(H_DESTROY_COMMAND_POOL.load(Relaxed)) else {
        return;
    };
    if active_device(device as usize).is_some() {
        release_cmds(&cmds_of_pool(pool, true));
    }
    f(device, pool, alloc);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitutes_keep_srgb_and_signedness() {
        let table: Vec<(u32, u32)> = (147..=156).map(|f| (f, etc_format(f).unwrap().1)).collect();
        assert_eq!(
            table,
            vec![(147, 37), (148, 43), (149, 37), (150, 43), (151, 37), (152, 43), (153, 70), (154, 71), (155, 77), (156, 78)]
        );
        assert!(etc_format(146).is_none());
        assert!(etc_format(157).is_none());
    }

    #[test]
    fn only_sampling_and_transfers_are_emulated() {
        assert!(image_request_ok(IMAGE_TILING_OPTIMAL, 0x7, 0));
        assert!(image_request_ok(IMAGE_TILING_OPTIMAL, 0x4, IMAGE_CREATE_CUBE_COMPATIBLE));
        assert!(!image_request_ok(1, 0x4, 0));
        assert!(!image_request_ok(IMAGE_TILING_OPTIMAL, 0x10, 0));
        assert!(!image_request_ok(IMAGE_TILING_OPTIMAL, 0x8, 0));
        assert!(!image_request_ok(IMAGE_TILING_OPTIMAL, 0x4, 0x80));
    }

    fn job_for(width: usize, height: usize, layers: usize, dst: &mut [u8]) -> Job {
        let row_blocks = width.div_ceil(4);
        let rows_of_blocks = height.div_ceil(4);
        Job {
            format: EtcFormat::Rgba8,
            row_blocks,
            rows_of_blocks,
            layer_stride: row_blocks * rows_of_blocks * 16,
            layers,
            width,
            height,
            dst: dst.as_mut_ptr() as usize,
            dst_len: dst.len(),
        }
    }

    #[test]
    fn a_large_region_decoded_in_bands_matches_one_pass() {
        let (w, h, layers): (usize, usize, usize) = (514, 262, 2);
        let blocks = w.div_ceil(4) * h.div_ceil(4) * layers;
        let mut seed = 0x1234_5678_u32;
        let src: Vec<u8> = (0..blocks * 16)
            .map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (seed >> 24) as u8
            })
            .collect();
        let mut banded = vec![0u8; w * h * 4 * layers];
        let job = job_for(w, h, layers, &mut banded);
        run_job(&job, &src).unwrap();
        let mut single = vec![0u8; w * h * 4 * layers];
        for layer in 0..layers {
            let surface = Surface {
                data: &src[layer * job.layer_stride..],
                row_blocks: job.row_blocks,
                rows_of_blocks: job.rows_of_blocks,
            };
            etc_decode::decode_region(
                EtcFormat::Rgba8,
                &surface,
                w,
                h,
                &mut single[layer * w * h * 4..(layer + 1) * w * h * 4],
            )
            .unwrap();
        }
        assert!(banded == single);
    }

    /// The hash as `as_chunks::<8>` computed it, written out by hand, so that
    /// replacing that call could not have changed what a recorded region hashes
    /// to between record and submit.
    fn reference_hash(data: &[u8]) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325 ^ data.len() as u64;
        let whole = data.len() / 8 * 8;
        let mut i = 0;
        while i < whole {
            let mut w = 0u64;
            for k in 0..8 {
                w |= u64::from(data[i + k]) << (8 * k);
            }
            h = (h ^ w).wrapping_mul(0x0100_0000_01b3).rotate_left(29);
            i += 8;
        }
        for &b in &data[whole..] {
            h = (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
        }
        h
    }

    #[test]
    fn the_hash_is_unchanged_across_every_remainder_length() {
        let data: Vec<u8> = (0..70u32).map(|i| (i * 37 + 11) as u8).collect();
        for len in 0..data.len() {
            assert_eq!(hash(&data[..len]), reference_hash(&data[..len]), "length {len}");
        }
    }

    #[test]
    fn emulation_is_decided_by_the_missing_feature_or_the_test_switch() {
        assert!(should_emulate(false, false));
        assert!(!should_emulate(true, false));
        assert!(should_emulate(true, true));
        assert!(should_emulate(false, true));
    }

    fn word(buf: &mut [u64], byte: usize, v: u32) {
        let at = byte / 8;
        let shift = (byte % 8) * 8;
        buf[at] = (buf[at] & !(0xffff_ffffu64 << shift)) | (u64::from(v) << shift);
    }

    fn word_at(buf: &[u64], byte: usize) -> u32 {
        (buf[byte / 8] >> ((byte % 8) * 8)) as u32
    }

    #[test]
    fn a_device_image_query_for_an_etc_image_is_sized_as_the_substitute() {
        let mut create = [0u64; 11];
        word(&mut create, 0, 14);
        word(&mut create, 24, 147);
        let mut info = [0u64; 4];
        info[2] = create.as_ptr() as u64;

        let sub = substitute_image_info(info.as_ptr() as *const c_void).expect("an ETC2 format is substituted");
        assert_eq!(rd::<u32>(sub.create.as_ptr() as *const c_void, 24), 37);
        assert_eq!(rd::<usize>(sub.info.as_ptr() as *const c_void, 16), sub.create.as_ptr() as usize);
        assert_eq!(word_at(&create, 24), 147, "the engine's own create info is not modified");

        word(&mut create, 24, 37);
        assert!(substitute_image_info(info.as_ptr() as *const c_void).is_none());
        info[2] = 0;
        assert!(substitute_image_info(info.as_ptr() as *const c_void).is_none());
        assert!(substitute_image_info(std::ptr::null()).is_none());
    }

    /// A `VkDeviceCreateInfo` whose chain is `[other node] -> Features2`, with
    /// the ETC2 feature enabled in the Features2.
    fn chain_with_features2_second() -> (Vec<u64>, Vec<u64>, Vec<u64>) {
        let mut other = vec![0u64; 2];
        word(&mut other, 0, 1_000_000_001);
        let mut f2 = vec![0u64; 30];
        word(&mut f2, 0, ST_PHYSICAL_DEVICE_FEATURES_2);
        word(&mut f2, 16 + FEATURE_ETC2 * 4, 1);
        other[1] = f2.as_ptr() as u64;
        let mut info = vec![0u64; 9];
        word(&mut info, 0, 3);
        info[1] = other.as_ptr() as u64;
        (info, other, f2)
    }

    #[test]
    fn features2_behind_another_node_is_cleared_for_the_call_and_put_back() {
        let (info, _other, f2) = chain_with_features2_second();
        let at = 16 + FEATURE_ETC2 * 4;
        let stripped = strip_with(true, info.as_ptr() as *const c_void).expect("an ETC2 feature in the chain is stripped");
        assert_eq!(word_at(&f2, at), 0, "the driver must not see the feature enabled");
        assert_eq!(stripped.as_ptr(info.as_ptr() as *const c_void), info.as_ptr() as *const c_void);
        drop(stripped);
        assert_eq!(word_at(&f2, at), 1, "the engine's structure is restored");
    }

    #[test]
    fn features2_first_in_the_chain_is_copied_and_the_original_left_alone() {
        let mut f2 = vec![0u64; 30];
        word(&mut f2, 0, ST_PHYSICAL_DEVICE_FEATURES_2);
        word(&mut f2, 16 + FEATURE_ETC2 * 4, 1);
        let mut info = [0u64; 9];
        info[1] = f2.as_ptr() as u64;
        let original = info.as_ptr() as *const c_void;
        let stripped = strip_with(true, original).expect("stripped");
        assert_eq!(word_at(&f2, 16 + FEATURE_ETC2 * 4), 1, "no write to the caller's structure");
        let given = stripped.as_ptr(original);
        assert_ne!(given, original);
        let copied_head: *const c_void = rd(given, 8);
        assert_eq!(rd::<u32>(copied_head, 16 + FEATURE_ETC2 * 4), 0);
        assert!(strip_with(false, original).is_none(), "nothing is stripped when not emulating");
    }

    // The hooks keep their host function in process-wide statics, so the tests
    // that install one take turns.
    static TEST_LOCK: Mutex<()> = Mutex::new(());
    static FAKE_CREATE_CALLS: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn fake_create_image(_: *mut c_void, _: *const c_void, _: *const c_void, _: *mut u64) -> i32 {
        FAKE_CREATE_CALLS.fetch_add(1, Relaxed);
        VK_SUCCESS
    }

    #[test]
    fn an_etc_image_on_an_emulating_device_without_state_is_refused_not_forwarded() {
        let _turn = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        H_CREATE_IMAGE.store(fake_create_image as *const () as usize, Relaxed);
        let mut create = [0u64; 11];
        word(&mut create, 24, 147);
        word(&mut create, 40, 1);
        let info = create.as_ptr() as *const c_void;
        let (emulating_device, native_device) = (0x6001usize as *mut c_void, 0x6002usize as *mut c_void);
        emulating().push(emulating_device as usize);
        let unhandled_before = glcount::ETC_UNHANDLED.load(Relaxed);
        let calls_before = FAKE_CREATE_CALLS.load(Relaxed);

        let mut out = 0u64;
        let rc = create_image(emulating_device, info, std::ptr::null(), &mut out);
        assert_eq!(rc, VK_ERROR_FORMAT_NOT_SUPPORTED);
        assert_eq!(FAKE_CREATE_CALLS.load(Relaxed), calls_before, "the driver was never asked");
        assert_eq!(glcount::ETC_UNHANDLED.load(Relaxed), unhandled_before + 1);

        let rc = create_image(native_device, info, std::ptr::null(), &mut out);
        assert_eq!(rc, VK_SUCCESS);
        assert_eq!(FAKE_CREATE_CALLS.load(Relaxed), calls_before + 1, "a native device is passed straight through");
        emulating().retain(|&d| d != emulating_device as usize);
    }

    #[test]
    fn device_level_hooks_are_installed_only_on_an_emulating_device() {
        let host = 0x10usize as *mut c_void;
        let (emulating_device, native_device) = (0x7001usize as *mut c_void, 0x7002usize as *mut c_void);
        emulating().push(emulating_device as usize);
        for name in [
            &b"vkGetDeviceImageMemoryRequirements"[..],
            b"vkGetDeviceImageMemoryRequirementsKHR",
            b"vkGetDeviceImageSparseMemoryRequirements",
            b"vkResetCommandPool",
        ] {
            assert!(hook(name, host, native_device).is_none(), "{}", String::from_utf8_lossy(name));
            assert!(hook(name, host, emulating_device).is_some(), "{}", String::from_utf8_lossy(name));
            assert!(hook(name, host, std::ptr::null_mut()).is_some(), "instance-level lookups stay hooked");
        }
        assert!(hook(b"vkNotAnEtcCall", host, emulating_device).is_none());
        emulating().retain(|&d| d != emulating_device as usize);
    }

    #[test]
    fn hashing_notices_a_single_changed_byte() {
        let mut data = vec![7u8; 4099];
        let before = hash(&data);
        data[4098] = 8;
        assert_ne!(before, hash(&data));
        data[4098] = 7;
        data[0] = 6;
        assert_ne!(before, hash(&data));
    }
}
