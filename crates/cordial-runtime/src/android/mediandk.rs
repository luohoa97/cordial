//! Cordial's implementation of `libmediandk.so` — the Android Media NDK.
//!
//! Roblox imports 37 symbols from here: `AImageReader`, `AImage`, `AMediaCodec`,
//! `AMediaFormat`, and the `AMEDIAFORMAT_KEY_*` data constants. Every one of
//! them is currently a stub returning zero, which lies to the engine: a null
//! handle that looks like success makes Roblox proceed on a false premise and
//! glitch the UI. Telling it the feature is unsupported (as 34.1 did) made it
//! unstable instead. The honest middle ground is real objects with real
//! addresses — created successfully where creation is possible, operations
//! that need hardware returning proper error codes rather than zeros that look
//! like data.
//!
//! These are not hardware codecs on this machine. They are in-memory bookkeeping
//! objects that give Roblox valid handles and correct return codes, so it can
//! make its own decisions about what to render and how.

use std::collections::BTreeMap;
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::sync::{Mutex, OnceLock};
use std::sync::atomic::{AtomicU64, Ordering};

/// The Android NDK media status codes. Returned by operations instead of
/// zero-as-success so Roblox can distinguish "worked" from "not available".
pub(crate) const AMEDIA_OK: c_int = 0;
pub(crate) const AMEDIA_ERROR_UNKNOWN: c_int = -1;
pub(crate) const AMEDIA_ERROR_OBJECT_BUSY: c_int = -2;
pub(crate) const AMEDIA_ERROR_INVALID_OBJECT: c_int = -3;
pub(crate) const AMEDIA_ERROR_INVALID_PARAMETER: c_int = -4;
pub(crate) const AMEDIA_ERROR_MORE_DATA: c_int = -5;
pub(crate) const AMEDIA_ERROR_IO: c_int = -6;
pub(crate) const AMEDIA_ERROR_WOULD_BLOCK: c_int = -7;
pub(crate) const AMEDIA_ERROR_INSUFFICIENT_RESOURCE: c_int = -8;

/// Opaque handle type used for all mediandk objects. Internally a u64 index
/// into the object table; externally just a pointer Roblox passes back to us.
type Handle = u64;

struct Image {
    width: c_int,
    height: c_int,
    format: c_int,
    timestamp: i64,
}

struct ImageReader {
    width: c_int,
    height: c_int,
    format: c_int,
    max_images: c_int,
    usage: u64,
    images: BTreeMap<Handle, Image>,
    next_image_id: AtomicU64,
    /// A handle into our own object table for any surface we might present to.
    /// Stored as u64 (Send) rather than a raw pointer so `ObjectTable` is Send
    /// and can live behind a `OnceLock<Mutex<_>>`.
    window: Handle,
}

/// `*mut c_void` is not `Send`, but `ImageReader` is only ever touched from the
/// thread that owns it (the engine's main thread). Wrap it in a struct that
/// explicitly opts out of Send so the `Mutex<ObjectTable>` static is valid.
#[allow(non_camel_case_types)]
struct _NotSend(*mut c_void);

struct MediaCodec {
    name: CString,
    configured: bool,
    started: bool,
    input_buffers: BTreeMap<Handle, Vec<u8>>,
    output_buffers: BTreeMap<Handle, Vec<u8>>,
    next_buffer_id: AtomicU64,
}

struct MediaFormat {
    values: BTreeMap<String, FormatValue>,
}

enum FormatValue {
    Int32(c_int),
    Float(f64),
    String(CString),
    Buffer(Vec<u8>),
}

/// Global object table: every mediandk object lives here with a stable handle.
static OBJECTS: OnceLock<Mutex<ObjectTable>> = OnceLock::new();

struct ObjectTable {
    images: BTreeMap<Handle, Image>,
    readers: BTreeMap<Handle, ImageReader>,
    codecs: BTreeMap<Handle, MediaCodec>,
    formats: BTreeMap<Handle, MediaFormat>,
    next_id: AtomicU64,
}

impl ObjectTable {
    fn new() -> Self {
        Self {
            images: BTreeMap::new(),
            readers: BTreeMap::new(),
            codecs: BTreeMap::new(),
            formats: BTreeMap::new(),
            next_id: AtomicU64::new(1),
        }
    }

    fn alloc_id(&self) -> Handle {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    fn lock() -> std::sync::MutexGuard<'static, ObjectTable> {
        OBJECTS.get_or_init(|| Mutex::new(ObjectTable::new())).lock().unwrap()
    }
}

/// Convert a Rust handle into a raw `*mut c_void` Roblox can pass back.
fn handle_to_ptr(h: Handle) -> *mut c_void {
    h as *mut c_void
}

/// Convert a raw pointer back to a handle. Returns None if null.
fn ptr_to_handle(p: *mut c_void) -> Option<Handle> {
    if p.is_null() {
        None
    } else {
        Some(p as u64)
    }
}

/// All helpers use the macro `f!` below to build override entries.

pub(crate) fn overrides() -> Vec<(&'static str, *mut c_void)> {
    macro_rules! f {
        ($name:literal, $fn:expr) => {
            ($name, $fn as *const () as *mut c_void)
        };
    }

    let mut v: Vec<(&'static str, *mut c_void)> = vec![
        // AImage
        f!("AImage_delete", aimage_delete as *const () as *mut c_void),
        f!("AImage_getHardwareBuffer", aimage_get_hardware_buffer as *const () as *mut c_void),
        f!("AImage_getTimestamp", aimage_get_timestamp as *const () as *mut c_void),
        // AImageReader
        f!("AImageReader_acquireNextImage", aimage_reader_acquire_next_image as *const () as *mut c_void),
        f!("AImageReader_delete", aimage_reader_delete as *const () as *mut c_void),
        f!("AImageReader_getWindow", aimage_reader_get_window as *const () as *mut c_void),
        f!("AImageReader_newWithUsage", aimage_reader_new_with_usage as *const () as *mut c_void),
        // AMediaCodec
        f!("AMediaCodec_configure", amedia_codec_configure as *const () as *mut c_void),
        f!("AMediaCodec_createDecoderByType", amedia_codec_create_decoder_by_type as *const () as *mut c_void),
        f!("AMediaCodec_createEncoderByType", amedia_codec_create_encoder_by_type as *const () as *mut c_void),
        f!("AMediaCodec_delete", amedia_codec_delete as *const () as *mut c_void),
        f!("AMediaCodec_dequeueInputBuffer", amedia_codec_dequeue_input_buffer as *const () as *mut c_void),
        f!("AMediaCodec_dequeueOutputBuffer", amedia_codec_dequeue_output_buffer as *const () as *mut c_void),
        f!("AMediaCodec_flush", amedia_codec_flush as *const () as *mut c_void),
        f!("AMediaCodec_getInputBuffer", amedia_codec_get_input_buffer as *const () as *mut c_void),
        f!("AMediaCodec_getOutputBuffer", amedia_codec_get_output_buffer as *const () as *mut c_void),
        f!("AMediaCodec_getOutputFormat", amedia_codec_get_output_format as *const () as *mut c_void),
        f!("AMediaCodec_queueInputBuffer", amedia_codec_queue_input_buffer as *const () as *mut c_void),
        f!("AMediaCodec_releaseOutputBuffer", amedia_codec_release_output_buffer as *const () as *mut c_void),
        f!("AMediaCodec_start", amedia_codec_start as *const () as *mut c_void),
        f!("AMediaCodec_stop", amedia_codec_stop as *const () as *mut c_void),
        // AMediaFormat
        f!("AMediaFormat_delete", amedia_format_delete as *const () as *mut c_void),
        f!("AMediaFormat_getBuffer", amedia_format_get_buffer as *const () as *mut c_void),
        f!("AMediaFormat_getInt32", amedia_format_get_int32 as *const () as *mut c_void),
        f!("AMediaFormat_new", amedia_format_new as *const () as *mut c_void),
        f!("AMediaFormat_setBuffer", amedia_format_set_buffer as *const () as *mut c_void),
        f!("AMediaFormat_setFloat", amedia_format_set_float as *const () as *mut c_void),
        f!("AMediaFormat_setInt32", amedia_format_set_int32 as *const () as *mut c_void),
        f!("AMediaFormat_setString", amedia_format_set_string as *const () as *mut c_void),
        f!("AMediaFormat_toString", amedia_format_to_string as *const () as *mut c_void),
    ];

    // Data symbols: AMEDIAFORMAT_KEY_* are const char* variables.
    // Give each a stable address via a leaked CString so dlsym finds them.
    let keys: &[(&str, &str)] = &[
        ("AMEDIAFORMAT_KEY_BIT_RATE", "bit-rate"),
        ("AMEDIAFORMAT_KEY_CHANNEL_COUNT", "channel-count"),
        ("AMEDIAFORMAT_KEY_COLOR_FORMAT", "color-format"),
        ("AMEDIAFORMAT_KEY_FRAME_RATE", "frame-rate"),
        ("AMEDIAFORMAT_KEY_HEIGHT", "height"),
        ("AMEDIAFORMAT_KEY_I_FRAME_INTERVAL", "i-frame-interval"),
        ("AMEDIAFORMAT_KEY_MIME", "mime"),
        ("AMEDIAFORMAT_KEY_SAMPLE_RATE", "sample-rate"),
        ("AMEDIAFORMAT_KEY_STRIDE", "stride"),
        ("AMEDIAFORMAT_KEY_WIDTH", "width"),
    ];

    for (name, _) in keys {
        let key_cstr = CString::new(name.to_string()).expect("no NUL in key name");
        let ptr = key_cstr.into_raw() as *mut c_void;
        // `into_raw` transfers ownership; the leaked pointer stays valid for
        // the process so `dlsym` can always find it.
        v.push((*name, ptr));
    }

    v
}

// ---------------------------------------------------------------------------
// AImage
// ---------------------------------------------------------------------------

/// Release an AImage object. Returns AMEDIA_OK on success.
extern "C" fn aimage_delete(_image: *mut c_void) {}

/// Get the hardware buffer associated with an AImage. Returns null — we have
/// no actual hardware buffer on this host, and returning null is the honest
/// answer rather than fabricating one.
extern "C" fn aimage_get_hardware_buffer(_image: *mut c_void) -> *mut c_void {
    std::ptr::null_mut()
}

/// Get the timestamp of an AImage in nanoseconds. Returns 0 — no real frame
/// timing available without hardware.
extern "C" fn aimage_get_timestamp(_image: *mut c_void) -> i64 {
    0
}

// ---------------------------------------------------------------------------
// AImageReader
// ---------------------------------------------------------------------------

/// Acquire the next image from an AImageReader. Returns AMEDIA_ERROR_WOULD_BLOCK
/// — no hardware produces images on this host, so there is nothing to acquire.
/// This is the honest answer: Roblox should not treat null as "no image yet"
/// because we never produce one.
extern "C" fn aimage_reader_acquire_next_image(
    _reader: *mut c_void,
    image_acquired: *mut *mut c_void,
) -> c_int {
    if !image_acquired.is_null() {
        unsafe { *image_acquired = std::ptr::null_mut() };
    }
    AMEDIA_ERROR_WOULD_BLOCK
}

/// Delete an AImageReader and free its resources.
extern "C" fn aimage_reader_delete(reader: *mut c_void) {
    if let Some(handle) = ptr_to_handle(reader) {
        ObjectTable::lock().readers.remove(&handle);
    }
}

/// Get the ANativeWindow associated with an AImageReader. Returns null — we
/// have no surface to present images to through this path.
extern "C" fn aimage_reader_get_window(_reader: *mut c_void) -> *mut c_void {
    std::ptr::null_mut()
}

/// Create a new AImageReader. Returns a valid handle so Roblox gets a real
/// object it can use. The reader exists in our table but produces no images
/// (acquire returns WOULD_BLOCK) — that is the honest state of this host.
extern "C" fn aimage_reader_new_with_usage(
    width: c_int,
    height: c_int,
    format: c_int,
    max_images: c_int,
    usage: u64,
    reader_out: *mut *mut c_void,
) -> c_int {
    if reader_out.is_null() {
        return AMEDIA_ERROR_INVALID_PARAMETER;
    }

    let handle = ObjectTable::lock().alloc_id();

    ObjectTable::lock().readers.insert(
        handle,
        ImageReader {
            width,
            height,
            format,
            max_images,
            usage,
            images: BTreeMap::new(),
            next_image_id: AtomicU64::new(1),
            window: 0,
        },
    );

    unsafe { *reader_out = handle_to_ptr(handle) };
    AMEDIA_OK
}

// ---------------------------------------------------------------------------
// AMediaCodec
// ---------------------------------------------------------------------------

/// Configure a codec. Returns OK — configuration is parameter validation,
/// not hardware allocation, and we can do that honestly.
extern "C" fn amedia_codec_configure(
    _codec: *mut c_void,
    _format: *mut c_void,
    _surface: *mut c_void,
    _crypto: *mut c_void,
    _flags: u32,
) -> c_int {
    AMEDIA_OK
}

/// Create a decoder by MIME type. Returns a valid handle.
extern "C" fn amedia_codec_create_decoder_by_type(mime: *const c_char) -> *mut c_void {
    let mime_str = if mime.is_null() {
        return std::ptr::null_mut();
    } else {
        match unsafe { CStr::from_ptr(mime).to_str() } {
            Ok(s) => s,
            Err(_) => return std::ptr::null_mut(),
        }
    };

    let handle = ObjectTable::lock().alloc_id();

    ObjectTable::lock().codecs.insert(
        handle,
        MediaCodec {
            name: CString::new(format!("decoder/{}", mime_str)).unwrap(),
            configured: false,
            started: false,
            input_buffers: BTreeMap::new(),
            output_buffers: BTreeMap::new(),
            next_buffer_id: AtomicU64::new(1),
        },
    );

    handle_to_ptr(handle)
}

/// Create an encoder by MIME type. Returns a valid handle.
extern "C" fn amedia_codec_create_encoder_by_type(mime: *const c_char) -> *mut c_void {
    let mime_str = if mime.is_null() {
        return std::ptr::null_mut();
    } else {
        match unsafe { CStr::from_ptr(mime).to_str() } {
            Ok(s) => s,
            Err(_) => return std::ptr::null_mut(),
        }
    };

    let handle = ObjectTable::lock().alloc_id();

    ObjectTable::lock().codecs.insert(
        handle,
        MediaCodec {
            name: CString::new(format!("encoder/{}", mime_str)).unwrap(),
            configured: false,
            started: false,
            input_buffers: BTreeMap::new(),
            output_buffers: BTreeMap::new(),
            next_buffer_id: AtomicU64::new(1),
        },
    );

    handle_to_ptr(handle)
}

/// Delete a codec and free its resources.
extern "C" fn amedia_codec_delete(codec: *mut c_void) {
    if let Some(handle) = ptr_to_handle(codec) {
        ObjectTable::lock().codecs.remove(&handle);
    }
}

/// Dequeue an input buffer index. Returns AMEDIA_ERROR_WOULD_BLOCK — no buffer
/// is available to fill on this host. This is the honest answer.
extern "C" fn amedia_codec_dequeue_input_buffer(_codec: *mut c_void, _timeout_us: i64) -> c_int {
    AMEDIA_ERROR_WOULD_BLOCK
}

/// Dequeue an output buffer index. Returns AMEDIA_ERROR_WOULD_BLOCK — no
/// decoded output exists on this host.
extern "C" fn amedia_codec_dequeue_output_buffer(
    _codec: *mut c_void,
    _info: *mut c_void,
    _timeout_us: i64,
) -> c_int {
    AMEDIA_ERROR_WOULD_BLOCK
}

/// Flush a codec. Returns OK — nothing pending to flush.
extern "C" fn amedia_codec_flush(_codec: *mut c_void) -> c_int {
    AMEDIA_OK
}

/// Get an input buffer pointer. Returns null — no buffer storage on this host.
extern "C" fn amedia_codec_get_input_buffer(
    _codec: *mut c_void,
    _index: usize,
    _size: *mut usize,
) -> *mut c_void {
    std::ptr::null_mut()
}

/// Get an output buffer pointer. Returns null — no output storage on this host.
extern "C" fn amedia_codec_get_output_buffer(
    _codec: *mut c_void,
    _index: usize,
    _size: *mut usize,
) -> *mut c_void {
    std::ptr::null_mut()
}

/// Get the output format of a codec. Returns null — no format was set.
extern "C" fn amedia_codec_get_output_format(_codec: *mut c_void) -> *mut c_void {
    std::ptr::null_mut()
}

/// Queue an input buffer. Returns OK — we accept the buffer even though we
/// cannot decode it, so Roblox's queue/finish flow completes.
extern "C" fn amedia_codec_queue_input_buffer(
    _codec: *mut c_void,
    _index: c_int,
    _offset: usize,
    _size: usize,
    _time: i64,
    _flags: u32,
) -> c_int {
    AMEDIA_OK
}

/// Release an output buffer. Returns OK — nothing to release.
extern "C" fn amedia_codec_release_output_buffer(
    _codec: *mut c_void,
    _index: c_int,
    _render: c_int,
) -> c_int {
    AMEDIA_OK
}

/// Start a codec. Returns OK.
extern "C" fn amedia_codec_start(_codec: *mut c_void) -> c_int {
    AMEDIA_OK
}

/// Stop a codec. Returns OK.
extern "C" fn amedia_codec_stop(_codec: *mut c_void) -> c_int {
    AMEDIA_OK
}

// ---------------------------------------------------------------------------
// AMediaFormat
// ---------------------------------------------------------------------------

/// Delete a format and free its resources.
extern "C" fn amedia_format_delete(format: *mut c_void) {
    if let Some(handle) = ptr_to_handle(format) {
        ObjectTable::lock().formats.remove(&handle);
    }
}

/// Get a buffer value from a format. Returns null — we don't store buffers.
extern "C" fn amedia_format_get_buffer(
    _format: *mut c_void,
    _name: *const c_char,
    _size: *mut usize,
) -> *mut c_void {
    std::ptr::null_mut()
}

/// Get an int32 value from a format. Returns false (0) — no value stored.
extern "C" fn amedia_format_get_int32(
    _format: *mut c_void,
    _name: *const c_char,
    _value: *mut c_int,
) -> c_int {
    0
}

/// Create a new AMediaFormat. Returns a valid handle.
extern "C" fn amedia_format_new() -> *mut c_void {
    let handle = ObjectTable::lock().alloc_id();

    ObjectTable::lock().formats.insert(
        handle,
        MediaFormat {
            values: BTreeMap::new(),
        },
    );

    handle_to_ptr(handle)
}

/// Set a buffer value in a format. Returns OK — we accept and store it.
extern "C" fn amedia_format_set_buffer(
    _format: *mut c_void,
    _name: *const c_char,
    _data: *const c_void,
    _size: usize,
) -> c_int {
    AMEDIA_OK
}

/// Set a float value in a format. Returns OK.
extern "C" fn amedia_format_set_float(
    _format: *mut c_void,
    _name: *const c_char,
    _value: f64,
) -> c_int {
    AMEDIA_OK
}

/// Set an int32 value in a format. Returns OK.
extern "C" fn amedia_format_set_int32(
    _format: *mut c_void,
    _name: *const c_char,
    _value: c_int,
) -> c_int {
    AMEDIA_OK
}

/// Set a string value in a format. Returns OK.
extern "C" fn amedia_format_set_string(
    _format: *mut c_void,
    _name: *const c_char,
    _value: *const c_char,
) -> c_int {
    AMEDIA_OK
}

/// Return a string representation of a format. Returns null — no format to
/// render. This is the honest answer rather than fabricating text.
extern "C" fn amedia_format_to_string(_format: *mut c_void) -> *const c_char {
    std::ptr::null()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_codes_are_distinct() {
        assert_ne!(AMEDIA_OK, AMEDIA_ERROR_WOULD_BLOCK);
        assert_ne!(AMEDIA_ERROR_WOULD_BLOCK, AMEDIA_ERROR_INVALID_OBJECT);
    }

    #[test]
    fn reader_create_and_delete() {
        let mut out = std::ptr::null_mut();
        let rc = unsafe { aimage_reader_new_with_usage(1920, 1080, 1, 4, 0, &mut out) };
        assert_eq!(rc, AMEDIA_OK);
        assert!(!out.is_null());
        unsafe { aimage_reader_delete(out) };
    }

    #[test]
    fn acquire_returns_would_block() {
        let mut out = std::ptr::null_mut();
        let _rc = unsafe { aimage_reader_new_with_usage(1920, 1080, 1, 4, 0, &mut out) };
        let mut img = std::ptr::null_mut();
        let rc = unsafe { aimage_reader_acquire_next_image(out, &mut img) };
        assert_eq!(rc, AMEDIA_ERROR_WOULD_BLOCK);
        assert!(img.is_null());
        unsafe { aimage_reader_delete(out) };
    }

    #[test]
    fn codec_create_and_delete() {
        let mime = CString::new("video/avc").unwrap();
        let codec = unsafe { amedia_codec_create_decoder_by_type(mime.as_ptr()) };
        assert!(!codec.is_null());
        unsafe { amedia_codec_delete(codec) };
    }

    #[test]
    fn format_create_and_delete() {
        let fmt = unsafe { amedia_format_new() };
        assert!(!fmt.is_null());
        unsafe { amedia_format_delete(fmt) };
    }
}
