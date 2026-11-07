//! Whether the Wayland compositor offers `zwp_linux_dmabuf_v1`, and what
//! Vulkan does when it does not.
//!
//! Issue #63. On a hardware Vulkan driver, Mesa's Wayland WSI can only present
//! through dma-bufs: `wsi_common_wayland.c` binds `zwp_linux_dmabuf_v1` for a
//! device and `wl_shm` only for a software one (`registry_handle_global`, keyed
//! on `wsi_device->sw`). When the compositor has no dmabuf global -- sway on
//! its pixman renderer, weston headless, cage -- a hardware device therefore
//! ends up with neither, `wsi_wl_display_init` reports
//! `VK_ERROR_SURFACE_LOST_KHR` (`!display->wl_dmabuf && !display->wl_shm`), and
//! every `vkGetPhysicalDeviceSurfaceCapabilitiesKHR` the engine makes on that
//! device fails the same way. Measured with a bare Vulkan probe and no engine:
//! the Intel device failed three queries out of three, llvmpipe passed three
//! out of three, and the Intel device passed once the compositor was sway on
//! its GLES renderer.
//!
//! The engine mishandles the failure: it records the failed call, and in a
//! process whose sampling draw comes up low that recorder reads a
//! graphics-device pointer that does not exist yet and faults. Retrying the
//! query cannot help, because the answer is a property of the device and the
//! compositor and not of timing.
//!
//! The one Vulkan device that can present there is a software one, and the
//! engine refuses those (`Vulkan: Device llvmpipe ... is emulated, skipping`).
//! So Vulkan has nothing to offer on such a compositor, and the honest answer
//! is the one `CORDIAL_NO_VULKAN` already gives: do not offer the loader, and
//! the engine takes its OpenGL ES path, which was measured drawing the landing
//! screen on such a compositor (Mesa's EGL is INFERRED to fall back to software
//! rendering through `wl_shm`). `MESA_VK_WSI_DEBUG=sw` would make Mesa do the
//! same copy for the hardware device, and was tried first; it makes the query
//! succeed and then `vkCreateSwapchainKHR` fails with
//! `VK_ERROR_INVALID_EXTERNAL_HANDLE` for any window whose pixel count is not a
//! multiple of 1024 (Mesa imports the shm pool as a host pointer, which the
//! driver wants page-aligned), once per frame, so it is not used.
//! The reasoning and the measurements are in ADR-061.

use std::ffi::{c_char, c_int, c_void, CStr};
use std::sync::OnceLock;

unsafe extern "C" {
    // The process's own libwayland, found the way `wayland.rs` finds it: GTK has
    // it loaded already, so this takes a reference and does not load a second
    // copy.
    fn dlopen(filename: *const c_char, flags: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}
const RTLD_NOW: c_int = 2;

/// `wl_display.get_registry`'s opcode.
const WL_DISPLAY_GET_REGISTRY: u32 = 1;

/// The version Mesa asks for before it will use the global at all
/// (`version >= 3` in `registry_handle_global`): the default-feedback request
/// arrives in version 4, but 3 is the floor for the format/modifier events.
const DMABUF_MIN_VERSION: u32 = 3;

/// Whether one advertised global is the dma-buf one Mesa can use.
fn is_usable_dmabuf(interface: &CStr, version: u32) -> bool {
    interface.to_bytes() == b"zwp_linux_dmabuf_v1" && version >= DMABUF_MIN_VERSION
}

#[repr(C)]
struct RegistryListener {
    global: unsafe extern "C" fn(*mut c_void, *mut c_void, u32, *const c_char, u32),
    global_remove: unsafe extern "C" fn(*mut c_void, *mut c_void, u32),
}

unsafe extern "C" fn on_global(
    data: *mut c_void,
    _registry: *mut c_void,
    _name: u32,
    interface: *const c_char,
    version: u32,
) {
    // SAFETY: `data` is the `*mut bool` registered in `compositor_offers_dmabuf`
    // and this runs only inside the roundtrip there, while that bool is live.
    let found = unsafe { &mut *(data as *mut bool) };
    // SAFETY: `wl_registry.global`'s interface argument is a NUL-terminated
    // string by protocol.
    if is_usable_dmabuf(unsafe { CStr::from_ptr(interface) }, version) {
        *found = true;
    }
}

unsafe extern "C" fn on_global_remove(_data: *mut c_void, _registry: *mut c_void, _name: u32) {}

static LISTENER: RegistryListener = RegistryListener { global: on_global, global_remove: on_global_remove };

/// Ask the compositor whether it advertises a dma-buf global Mesa can use.
///
/// `None` when the question cannot be asked (no libwayland, no compositor on
/// `WAYLAND_DISPLAY`); every caller treats that as "do not change anything",
/// because the workaround is for a compositor known to lack the global and a
/// failed probe is not that knowledge.
///
/// This opens a connection of its own and closes it before returning. The
/// deliberate absence of `wl_display_connect` in `wayland.rs` is about surfaces
/// -- a second connection cannot present into GTK's window -- and this one only
/// reads the list of globals. It does not go through `wayland.rs` because that
/// file's registry walk is private to window construction.
fn compositor_offers_dmabuf() -> Option<bool> {
    // SAFETY: literal, NUL-terminated soname; the handle is never closed.
    let lib = unsafe { dlopen(c"libwayland-client.so.0".as_ptr(), RTLD_NOW) };
    if lib.is_null() {
        return None;
    }
    macro_rules! sym {
        ($name:literal) => {{
            // SAFETY: a literal, NUL-terminated symbol name looked up in a live handle.
            let p = unsafe { dlsym(lib, concat!($name, "\0").as_ptr().cast()) };
            if p.is_null() {
                return None;
            }
            // SAFETY: the symbol is libwayland's documented export of this name and
            // the declared type is its C signature.
            unsafe { std::mem::transmute(p) }
        }};
    }
    let connect: unsafe extern "C" fn(*const c_char) -> *mut c_void = sym!("wl_display_connect");
    let disconnect: unsafe extern "C" fn(*mut c_void) = sym!("wl_display_disconnect");
    let roundtrip: unsafe extern "C" fn(*mut c_void) -> c_int = sym!("wl_display_roundtrip");
    let marshal_flags: unsafe extern "C" fn(*mut c_void, u32, *const c_void, u32, u32, ...) -> *mut c_void =
        sym!("wl_proxy_marshal_flags");
    let add_listener: unsafe extern "C" fn(*mut c_void, *const c_void, *mut c_void) -> c_int =
        sym!("wl_proxy_add_listener");
    let registry_interface: *const c_void = sym!("wl_registry_interface");

    // SAFETY: a null name makes libwayland use `WAYLAND_DISPLAY`, as GTK did.
    let display = unsafe { connect(std::ptr::null()) };
    if display.is_null() {
        return None;
    }
    let mut found = false;
    // SAFETY: `display` is a live connection; the registry proxy dies with it,
    // so the listener's pointer to `found` is never used after `disconnect`.
    let ok = unsafe {
        let registry = marshal_flags(
            display,
            WL_DISPLAY_GET_REGISTRY,
            registry_interface,
            1,
            0,
            std::ptr::null_mut::<c_void>(),
        );
        !registry.is_null()
            && add_listener(
                registry,
                &LISTENER as *const RegistryListener as *const c_void,
                &mut found as *mut bool as *mut c_void,
            ) == 0
            && roundtrip(display) >= 0
    };
    // SAFETY: `display` came from `connect` above and is not used again.
    unsafe { disconnect(display) };
    ok.then_some(found)
}

/// The switch that keeps Vulkan on a compositor without dmabuf, for measuring
/// the failure this module avoids and for anyone who wants to see it.
pub const FORCE_ENV: &str = "CORDIAL_FORCE_VULKAN_WITHOUT_DMABUF";

/// What to do, given the facts. Separate from the probing so the decision is
/// what a test exercises.
fn withholds_vulkan(wayland: bool, automatic: bool, offers_dmabuf: Option<bool>, forced: bool) -> bool {
    // Only the Wayland backend has this problem: X11 presents through DRI3,
    // which needs no Wayland global. Only Automatic is overridden: a user who
    // asked for Vulkan by name gets what they asked for.
    wayland && automatic && !forced && offers_dmabuf == Some(false)
}

/// Whether the virtual `libvulkan` should be withheld from the engine because
/// the Wayland compositor cannot be presented to by a hardware device.
///
/// Decided once and logged, because it changes which graphics API the engine
/// uses. A probe that could not be asked (no libwayland, no compositor on
/// `WAYLAND_DISPLAY`) leaves Vulkan on: the workaround is for a compositor
/// known to lack the global, and a failed probe is not that knowledge.
pub fn withhold_vulkan() -> bool {
    static DECISION: OnceLock<bool> = OnceLock::new();
    *DECISION.get_or_init(|| {
        let wayland = super::backend() == super::Backend::Wayland;
        let automatic = crate::graphics::choice().backend == crate::graphics::Backend::Automatic;
        let forced = std::env::var_os(FORCE_ENV).is_some();
        if !wayland {
            return false;
        }
        let offers = compositor_offers_dmabuf();
        let withhold = withholds_vulkan(wayland, automatic, offers, forced);
        if withhold {
            eprintln!(
                "[android] vulkan: withheld: the compositor advertises no zwp_linux_dmabuf_v1, so a \
                 hardware Vulkan device cannot present to it (Mesa answers VK_ERROR_SURFACE_LOST_KHR) \
                 and the engine mishandles that at startup (issue #63); using OpenGL ES instead. \
                 {FORCE_ENV}=1 keeps Vulkan."
            );
        } else if offers == Some(false) {
            eprintln!(
                "[android] vulkan: the compositor advertises no zwp_linux_dmabuf_v1; Vulkan is kept \
                 as requested, and a hardware device will report VK_ERROR_SURFACE_LOST_KHR (issue #63)"
            );
        }
        withhold
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dmabuf_global_counts_only_from_the_version_mesa_binds() {
        assert!(is_usable_dmabuf(c"zwp_linux_dmabuf_v1", 4));
        assert!(is_usable_dmabuf(c"zwp_linux_dmabuf_v1", 3));
        // Version 2 is advertised by old compositors and Mesa ignores it, so a
        // hardware device still ends up with no way to present.
        assert!(!is_usable_dmabuf(c"zwp_linux_dmabuf_v1", 2));
        assert!(!is_usable_dmabuf(c"wl_shm", 1));
        assert!(!is_usable_dmabuf(c"zwp_linux_dmabuf_feedback_v1", 4));
    }

    #[test]
    fn vulkan_is_withheld_only_where_a_hardware_device_cannot_present() {
        // The case from the issue: Wayland, Automatic, no dmabuf.
        assert!(withholds_vulkan(true, true, Some(false), false));
        // A compositor that offers it, or a probe that could not tell, changes nothing.
        assert!(!withholds_vulkan(true, true, Some(true), false));
        assert!(!withholds_vulkan(true, true, None, false));
        // X11 never needed the global.
        assert!(!withholds_vulkan(false, true, Some(false), false));
        // A user who chose Vulkan, or set the switch, keeps it.
        assert!(!withholds_vulkan(true, false, Some(false), false));
        assert!(!withholds_vulkan(true, true, Some(false), true));
    }
}
