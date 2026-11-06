//! `AImageReader_*`, `AImage_*` and `AHardwareBuffer_acquire/release` -- the
//! NDK surface a hardware video decoder renders into.
//!
//! Roblox 2.746.0.1266 is the first build to import these directly, and with
//! nothing answering them `dlopen` stopped on `AImageReader_delete` before any
//! engine code ran. They sit beside the `AMediaCodec_*` imports and the
//! `*HardwareBuffer*` video flags in `docs/traces/client-settings-flag-names.txt`,
//! so the consumer is video playback decoding into a hardware buffer. INFERRED
//! from the names; no run has yet reached a call.
//!
//! Cordial has no `AImageReader` to give, so the constructor says so with
//! `AMEDIA_ERROR_UNSUPPORTED` and leaves the out-pointer null. A generated stub
//! would have returned zero, which is `AMEDIA_OK`, and the engine would then
//! have used whatever was in its uninitialised `AImageReader*` -- exactly the
//! lying stub AGENTS.md warns about. Every other entry point can only be
//! reached with an object this file never hands out, so they reject it as an
//! invalid object rather than pretending to work.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};

/// `media_status_t` values from `media/NdkMediaError.h`.
const AMEDIA_ERROR_BASE: i32 = -10000;
const AMEDIA_ERROR_UNSUPPORTED: i32 = AMEDIA_ERROR_BASE - 2;
const AMEDIA_ERROR_INVALID_OBJECT: i32 = AMEDIA_ERROR_BASE - 3;

static REPORTED: AtomicBool = AtomicBool::new(false);

extern "C" fn reader_new_with_usage(
    width: i32,
    height: i32,
    format: i32,
    usage: u64,
    max_images: i32,
    reader: *mut *mut c_void,
) -> i32 {
    if !reader.is_null() {
        // SAFETY: the caller passes a writable out-pointer per the NDK contract.
        unsafe { *reader = std::ptr::null_mut() };
    }
    if !REPORTED.swap(true, Ordering::Relaxed) {
        eprintln!(
            "[media] AImageReader_newWithUsage({width}x{height}, format {format:#x}, usage {usage:#x}, \
             {max_images} images) refused with AMEDIA_ERROR_UNSUPPORTED: Cordial has no image reader"
        );
    }
    AMEDIA_ERROR_UNSUPPORTED
}

extern "C" fn reader_delete(_reader: *mut c_void) {}

extern "C" fn reader_get_window(_reader: *mut c_void, window: *mut *mut c_void) -> i32 {
    if !window.is_null() {
        // SAFETY: as above.
        unsafe { *window = std::ptr::null_mut() };
    }
    AMEDIA_ERROR_INVALID_OBJECT
}

extern "C" fn reader_acquire_next_image(_reader: *mut c_void, image: *mut *mut c_void) -> i32 {
    if !image.is_null() {
        // SAFETY: as above.
        unsafe { *image = std::ptr::null_mut() };
    }
    AMEDIA_ERROR_INVALID_OBJECT
}

extern "C" fn image_delete(_image: *mut c_void) {}

extern "C" fn image_get_hardware_buffer(_image: *const c_void, buffer: *mut *mut c_void) -> i32 {
    if !buffer.is_null() {
        // SAFETY: as above.
        unsafe { *buffer = std::ptr::null_mut() };
    }
    AMEDIA_ERROR_INVALID_OBJECT
}

extern "C" fn image_get_timestamp(_image: *const c_void, _timestamp: *mut i64) -> i32 {
    AMEDIA_ERROR_INVALID_OBJECT
}

/// Reference counting on a buffer that can only have come from an `AImage`,
/// which this file never produces. Nothing to count.
extern "C" fn hardware_buffer_acquire(_buffer: *mut c_void) {}
extern "C" fn hardware_buffer_release(_buffer: *mut c_void) {}

pub fn overrides() -> Vec<(&'static str, *mut c_void)> {
    macro_rules! f {
        ($name:literal, $fn:expr) => {
            ($name, $fn as *const () as *mut c_void)
        };
    }
    vec![
        f!("AImageReader_newWithUsage", reader_new_with_usage),
        f!("AImageReader_delete", reader_delete),
        f!("AImageReader_getWindow", reader_get_window),
        f!("AImageReader_acquireNextImage", reader_acquire_next_image),
        f!("AImage_delete", image_delete),
        f!("AImage_getHardwareBuffer", image_get_hardware_buffer),
        f!("AImage_getTimestamp", image_get_timestamp),
        f!("AHardwareBuffer_acquire", hardware_buffer_acquire),
        f!("AHardwareBuffer_release", hardware_buffer_release),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructor_refuses_and_nulls_the_out_pointer() {
        let mut reader = 1usize as *mut c_void;
        let rc = reader_new_with_usage(640, 360, 0x22, 0x100, 2, &mut reader);
        assert_eq!(rc, AMEDIA_ERROR_UNSUPPORTED);
        assert!(reader.is_null());
    }

    #[test]
    fn every_import_is_answered() {
        let names: Vec<_> = overrides().into_iter().map(|(n, _)| n).collect();
        for want in [
            "AHardwareBuffer_acquire",
            "AHardwareBuffer_release",
            "AImageReader_acquireNextImage",
            "AImageReader_delete",
            "AImageReader_getWindow",
            "AImageReader_newWithUsage",
            "AImage_delete",
            "AImage_getHardwareBuffer",
            "AImage_getTimestamp",
        ] {
            assert!(names.contains(&want), "{want} is not answered");
        }
    }
}
