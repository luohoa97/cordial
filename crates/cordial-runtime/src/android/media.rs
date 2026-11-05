//! `AImageReader`, `AImage` and the two `AHardwareBuffer` reference counters,
//! first imported by Roblox 2.746.0.1266.
//!
//! Cordial has no image reader: nothing here produces frames. Every call that
//! could hand back an object reports `AMEDIA_ERROR_UNSUPPORTED` and leaves the
//! out-pointer null, so the engine sees a refusal it can handle instead of a
//! zero that reads as success (AGENTS.md, "Never make a stub lie"). The
//! `_delete`, `_acquire` and `_release` entry points have nothing to free,
//! because no reader, image or buffer is ever handed out.

use std::ffi::c_void;

/// `media_status_t` from `<media/NdkMediaError.h>`: `AMEDIA_ERROR_BASE - 2`.
const AMEDIA_ERROR_UNSUPPORTED: i32 = -10002;

/// Clears `out` when the caller passed one, so a caller that ignores the status
/// still reads null and not stack garbage.
fn clear(out: *mut *mut c_void) {
    if !out.is_null() {
        // SAFETY: the NDK contract makes `out` a valid pointer-sized slot.
        unsafe { *out = std::ptr::null_mut() };
    }
}

extern "C" fn image_reader_new_with_usage(
    _width: i32,
    _height: i32,
    _format: i32,
    _usage: u64,
    _max_images: i32,
    reader: *mut *mut c_void,
) -> i32 {
    crate::unimplemented::record(
        crate::unimplemented::Kind::Placeholder,
        "AImageReader_newWithUsage: no image reader, answered AMEDIA_ERROR_UNSUPPORTED",
    );
    clear(reader);
    AMEDIA_ERROR_UNSUPPORTED
}

extern "C" fn image_reader_get_window(_reader: *mut c_void, window: *mut *mut c_void) -> i32 {
    clear(window);
    AMEDIA_ERROR_UNSUPPORTED
}

extern "C" fn image_reader_acquire_next_image(
    _reader: *mut c_void,
    image: *mut *mut c_void,
) -> i32 {
    clear(image);
    AMEDIA_ERROR_UNSUPPORTED
}

extern "C" fn image_get_hardware_buffer(_image: *const c_void, buffer: *mut *mut c_void) -> i32 {
    clear(buffer);
    AMEDIA_ERROR_UNSUPPORTED
}

extern "C" fn image_get_timestamp(_image: *const c_void, ts: *mut i64) -> i32 {
    if !ts.is_null() {
        // SAFETY: the NDK contract makes `ts` a valid int64_t slot.
        unsafe { *ts = 0 };
    }
    AMEDIA_ERROR_UNSUPPORTED
}

extern "C" fn nothing_to_free(_object: *mut c_void) {}

pub fn overrides() -> Vec<(&'static str, *mut c_void)> {
    macro_rules! f {
        ($name:literal, $fn:expr) => {
            ($name, $fn as *const () as *mut c_void)
        };
    }
    vec![
        f!("AImageReader_newWithUsage", image_reader_new_with_usage),
        f!("AImageReader_getWindow", image_reader_get_window),
        f!(
            "AImageReader_acquireNextImage",
            image_reader_acquire_next_image
        ),
        f!("AImageReader_delete", nothing_to_free),
        f!("AImage_getHardwareBuffer", image_get_hardware_buffer),
        f!("AImage_getTimestamp", image_get_timestamp),
        f!("AImage_delete", nothing_to_free),
        f!("AHardwareBuffer_acquire", nothing_to_free),
        f!("AHardwareBuffer_release", nothing_to_free),
    ]
}
