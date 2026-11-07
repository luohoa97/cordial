//! The arm64 guest's `libaaudio.so`: the same AAudio bridge the phone build
//! plays through (`native/aaudio.cpp`, over ADR-023's host backend), behind
//! guest stubs.
//!
//! The Quest build does not import AAudio. Its FMOD asks
//! `org.fmod.FMOD.supportsAAudio()` over JNI, which `audio_classes.cpp`
//! answers yes, then `dlopen`s `libaaudio.so` and `dlsym`s each name. Before
//! this module the guest had no library by that name -- only the host one,
//! which the guest linker never sees -- so the `dlopen` returned null after
//! FMOD had already committed to AAudio, and `System::init` failed with
//! `FMOD_ERR_OUTPUT_INIT` (51): no audio at all, and no fallback, which is
//! what `aaudio.cpp` measured FMOD doing when a stream is refused, too.
//!
//! Every function but two takes handles, scalars and out-pointers to
//! `int32_t` or to a handle, which mean the same on both sides, so the generic
//! call builder carries them to the native implementation. The two that do
//! not are the callback setters: the data and error callbacks are guest
//! functions, which the host backend will call from its own realtime thread,
//! so each is handed over as a host entry (`cordial_guest::host_entry`)
//! that runs it on that thread's own Jit. One entry per distinct callback,
//! made once; FMOD installs the same two for every stream.
//!
//! The names are whatever `aaudio.cpp` exports, taken from the native table
//! so the guest and the phone build get the same answer; a name with no
//! signature here is left out of the guest library and said so, rather than
//! guessed at -- a `dlsym` that returns null is a failure FMOD can report,
//! and a call through the wrong signature is not.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::{Arc, Mutex};

use cordial_guest::{Fault, Handler, Ret, Runtime, Ty};

use crate::symtab::{Source, SymbolTable, AAUDIO_LIBRARY_NAME};

use Ty::{I32, I64, Ptr};

/// The AAudio entry points by signature, from AOSP's `AAudio.h`. Enum
/// arguments are `int32_t` typedefs there, so they are `I32` here.
/// `waitForStateChange` is named in the Quest build and was not looked up in
/// any run; `aaudio.cpp` does not implement it, so it is listed only for the
/// day it does.
#[rustfmt::skip]
const SIGS: &[(&str, &[Ty], Ret)] = &[
    ("AAudio_createStreamBuilder", &[Ptr], Ret::Int(I32)),
    ("AAudioStreamBuilder_delete", &[Ptr], Ret::Int(I32)),
    ("AAudioStreamBuilder_openStream", &[Ptr, Ptr], Ret::Int(I32)),
    ("AAudioStreamBuilder_setBufferCapacityInFrames", &[Ptr, I32], Ret::Void),
    ("AAudioStreamBuilder_setDirection", &[Ptr, I32], Ret::Void),
    ("AAudioStreamBuilder_setFormat", &[Ptr, I32], Ret::Void),
    ("AAudioStreamBuilder_setInputPreset", &[Ptr, I32], Ret::Void),
    ("AAudioStreamBuilder_setPerformanceMode", &[Ptr, I32], Ret::Void),
    ("AAudioStreamBuilder_setUsage", &[Ptr, I32], Ret::Void),
    ("AAudioStream_close", &[Ptr], Ret::Int(I32)),
    ("AAudioStream_getBufferCapacityInFrames", &[Ptr], Ret::Int(I32)),
    ("AAudioStream_getBufferSizeInFrames", &[Ptr], Ret::Int(I32)),
    ("AAudioStream_getChannelCount", &[Ptr], Ret::Int(I32)),
    ("AAudioStream_getFormat", &[Ptr], Ret::Int(I32)),
    ("AAudioStream_getFramesPerBurst", &[Ptr], Ret::Int(I32)),
    ("AAudioStream_getSampleRate", &[Ptr], Ret::Int(I32)),
    ("AAudioStream_getState", &[Ptr], Ret::Int(I32)),
    ("AAudioStream_getXRunCount", &[Ptr], Ret::Int(I32)),
    ("AAudioStream_read", &[Ptr, Ptr, I32, I64], Ret::Int(I32)),
    ("AAudioStream_requestPause", &[Ptr], Ret::Int(I32)),
    ("AAudioStream_requestStart", &[Ptr], Ret::Int(I32)),
    ("AAudioStream_requestStop", &[Ptr], Ret::Int(I32)),
    ("AAudioStream_setBufferSizeInFrames", &[Ptr, I32], Ret::Int(I32)),
    ("AAudioStream_waitForStateChange", &[Ptr, I32, Ptr, I64], Ret::Int(I32)),
];

/// `aaudio_data_callback_result_t (*)(AAudioStream*, void* user, void* audioData, int32_t numFrames)`.
const DATA_CALLBACK: (&[Ty], Ret) = (&[Ptr, Ptr, Ptr, I32], Ret::Int(I32));
/// `void (*)(AAudioStream*, void* user, aaudio_result_t error)`.
const ERROR_CALLBACK: (&[Ty], Ret) = (&[Ptr, Ptr, I32], Ret::Void);

/// The guest's `libaaudio.so`, or `None` when the native table has none --
/// `CORDIAL_AUDIO=java`, where `supportsAAudio()` answers false and FMOD
/// never looks for it.
pub fn library(rt: &Arc<Runtime>, native: &SymbolTable) -> Option<Vec<(String, *mut c_void)>> {
    let entries = native.libraries.get(AAUDIO_LIBRARY_NAME)?;
    let natives = entries.iter().filter(|e| e.source == Source::Cordial).map(|e| (e.symbol, e.address as usize));
    Some(stubs(rt, natives))
}

fn stubs(rt: &Arc<Runtime>, natives: impl Iterator<Item = (&'static str, usize)>) -> Vec<(String, *mut c_void)> {
    let mut out = Vec::new();
    for (symbol, f) in natives {
        let handler: Handler = match symbol {
            "AAudioStreamBuilder_setDataCallback" => set_callback(symbol, f, "AAudio data callback", DATA_CALLBACK),
            "AAudioStreamBuilder_setErrorCallback" => {
                set_callback(symbol, f, "AAudio error callback", ERROR_CALLBACK)
            }
            name => match SIGS.iter().find(|(n, _, _)| *n == name) {
                Some(&(_, args, ret)) => Box::new(move |c| c.host(f as *const c_void, args, ret)),
                None => {
                    eprintln!("[guest] audio: {name} is in Cordial's AAudio but has no guest signature; \
                               left out of the guest's libaaudio.so, so its dlsym is null");
                    continue;
                }
            },
        };
        out.push((symbol.to_string(), rt.register(symbol, handler) as *mut c_void));
    }
    out
}

/// `AAudioStreamBuilder_set{Data,Error}Callback(builder, callback, userData)`,
/// with the guest's callback swapped for a host entry that runs it. Null
/// stays null: FMOD's probe streams set none, and `aaudio.cpp` reads a null
/// data callback as "fill with silence".
fn set_callback(name: &'static str, native: usize, label: &'static str, sig: (&'static [Ty], Ret)) -> Handler {
    // Keyed by kind as well as address: an entry carries its signature.
    static ENTRIES: Mutex<Option<HashMap<(&'static str, u64), u64>>> = Mutex::new(None);
    Box::new(move |c| {
        let cb = c.x(1);
        let host_cb = if cb == 0 {
            0
        } else {
            let mut m = ENTRIES.lock().unwrap();
            let m = m.get_or_insert_with(HashMap::new);
            match m.get(&(label, cb)) {
                Some(&e) => e,
                None => {
                    let e = cordial_guest::host_entry(c.runtime(), label, cb, sig.0.to_vec(), sig.1, None)
                        .map_err(|why| Fault::Unsupported { thunk: name.into(), why })? as u64;
                    eprintln!("[guest] audio: {label} at guest {cb:#x} made callable by the host backend");
                    m.insert((label, cb), e);
                    e
                }
            }
        };
        // SAFETY: Cordial's own setter, with the guest's builder and user
        // data and a host-callable callback.
        unsafe {
            cordial_guest::invoke(name, native as *const c_void, &[Ptr, Ptr, Ptr], &[c.x(0), host_cb, c.x(2)])
        }?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every name the native bridge exports has a guest signature, so none
    /// is silently missing from the guest's library.
    #[test]
    fn every_native_name_has_a_guest_answer() {
        if !crate::bionic::aaudio_selected() {
            return;
        }
        for (name, _) in crate::bionic::aaudio_overrides() {
            let known = SIGS.iter().any(|(n, _, _)| *n == name)
                || matches!(name, "AAudioStreamBuilder_setDataCallback" | "AAudioStreamBuilder_setErrorCallback");
            assert!(known, "{name} has no guest signature");
        }
    }

    /// arm64 code in a mapping of its own, for the translator to read.
    fn guest_code(words: &[u32]) -> cordial_guest::Mapping {
        let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        cordial_guest::Mapping::with_contents(&bytes, 0)
    }

    /// A data callback in arm64: fills `channels * numFrames` 32-bit samples
    /// with `user->value`, counts its calls in `user->calls`, and returns
    /// `AAUDIO_CALLBACK_RESULT_CONTINUE`.
    ///
    /// ```text
    ///     ldr w9, [x1, #4]; mul w9, w9, w3; ldr w10, [x1]; cbz w9, 1f
    /// 2:  str w10, [x2], #4; subs w9, w9, #1; b.ne 2b
    /// 1:  ldr x11, [x1, #8]; add x11, x11, #1; str x11, [x1, #8]
    ///     mov w0, wzr; ret
    /// ```
    const FILL: [u32; 12] = [
        0xb940_0429, 0x1b03_7d29, 0xb940_002a, 0x3400_0089, 0xb800_444a, 0x7100_0529,
        0x54ff_ffc1, 0xf940_042b, 0x9100_056b, 0xf900_042b, 0x2a1f_03e0, 0xd65f_03c0,
    ];

    #[repr(C)]
    struct User {
        value: u32,
        channels: u32,
        calls: u64,
    }

    static SET: Mutex<(u64, u64, u64)> = Mutex::new((0, 0, 0));

    unsafe extern "C" fn fake_set_data_callback(builder: u64, cb: u64, user: u64) {
        *SET.lock().unwrap() = (builder, cb, user);
    }

    /// The guest's callback reaches the host as a host function, which a
    /// thread that has never run guest code -- PipeWire's, in the real
    /// thing -- can call, and which writes the host's buffer.
    #[test]
    fn the_data_callback_runs_on_a_host_thread() {
        let rt = Runtime::new(cordial_guest::Options::default());
        let lib = stubs(&rt, [("AAudioStreamBuilder_setDataCallback", fake_set_data_callback as *const () as usize)].into_iter());
        let set = lib[0].1 as u64;
        let code = guest_code(&FILL);
        let mut user = User { value: 0.25f32.to_bits(), channels: 2, calls: 0 };
        let up = &mut user as *mut User as u64;

        cordial_guest::guest_call(&rt, set, &[0xb1, code.addr(), up], &[]).unwrap();
        let (builder, host_cb, got_user) = *SET.lock().unwrap();
        assert_eq!((builder, got_user), (0xb1, up), "builder and userData pass through");
        assert_ne!(host_cb, code.addr(), "the host must not be handed arm64 code");

        // The same callback again is the same entry: FMOD sets it per stream.
        cordial_guest::guest_call(&rt, set, &[0xb2, code.addr(), up], &[]).unwrap();
        assert_eq!(SET.lock().unwrap().1, host_cb);

        let up = up as usize;
        let filled = std::thread::spawn(move || {
            let mut buf = vec![0f32; 256 * 2];
            // SAFETY: a host entry with AAudioStream_dataCallback's signature.
            let f: extern "C" fn(u64, usize, *mut f32, i32) -> i32 = unsafe { std::mem::transmute(host_cb as usize) };
            let mut r = 0;
            for _ in 0..100 {
                r |= f(0x5, up, buf.as_mut_ptr(), 256);
            }
            (r, buf)
        })
        .join()
        .unwrap();
        assert_eq!(filled.0, 0, "AAUDIO_CALLBACK_RESULT_CONTINUE");
        assert!(filled.1.iter().all(|&v| v == 0.25), "the guest did not fill the host's buffer");
        assert_eq!(user.calls, 100);

        // Null stays null: the probe streams set no callback.
        cordial_guest::guest_call(&rt, set, &[0xb3, 0, 0], &[]).unwrap();
        assert_eq!(SET.lock().unwrap().1, 0);
    }

    /// The whole path on the real host: Cordial's AAudio over the session's
    /// audio server, entered through the guest's stubs, pulled by the
    /// server's own thread into an arm64 callback. The callback writes
    /// 1e-6 of full scale (-120 dBFS: inaudible, but not zero), so
    /// `CORDIAL_TRACE_AUDIO=1` counts non-silent frames. Opens a real
    /// playback stream on the default output, so it is not in the default
    /// run: `CORDIAL_TRACE_AUDIO=1 cargo test -p cordial-runtime --lib
    /// guest_audio -- --ignored --nocapture`.
    #[test]
    #[ignore = "opens a playback stream on the host's audio server"]
    fn guest_audio_reaches_the_host_backend() {
        assert!(crate::bionic::aaudio_selected(), "CORDIAL_AUDIO=java leaves no AAudio to test");
        let rt = Runtime::new(cordial_guest::Options::default());
        let native = crate::bionic::aaudio_overrides().into_iter().map(|(n, a)| (n, a as usize));
        let lib: std::collections::HashMap<String, u64> =
            stubs(&rt, native).into_iter().map(|(n, a)| (n, a as u64)).collect();
        let call = |n: &str, a: &[u64]| cordial_guest::guest_call(&rt, lib[n], a, &[]).unwrap().x0 as i32;

        let code = guest_code(&FILL);
        let mut user = User { value: 1e-6f32.to_bits(), channels: 0, calls: 0 };
        let mut builder = 0u64;
        assert_eq!(call("AAudio_createStreamBuilder", &[&mut builder as *mut u64 as u64]), 0);
        call("AAudioStreamBuilder_setDataCallback", &[builder, code.addr(), &mut user as *mut User as u64]);
        let mut stream = 0u64;
        assert_eq!(call("AAudioStreamBuilder_openStream", &[builder, &mut stream as *mut u64 as u64]), 0);
        call("AAudioStreamBuilder_delete", &[builder]);
        let (rate, channels, format, burst) = (call("AAudioStream_getSampleRate", &[stream]),
            call("AAudioStream_getChannelCount", &[stream]), call("AAudioStream_getFormat", &[stream]),
            call("AAudioStream_getFramesPerBurst", &[stream]));
        println!("opened: {rate} Hz, {channels} channel(s), format {format}, burst {burst}");
        assert_eq!(format, 2, "this callback writes AAUDIO_FORMAT_PCM_FLOAT only");
        // SAFETY: the callback reads this field; nothing pulls before requestStart.
        unsafe { std::ptr::write_volatile(&mut user.channels, channels as u32) };
        assert_eq!(call("AAudioStream_requestStart", &[stream]), 0);
        std::thread::sleep(std::time::Duration::from_secs(3));
        assert_eq!(call("AAudioStream_requestStop", &[stream]), 0);
        let xruns = call("AAudioStream_getXRunCount", &[stream]);
        assert_eq!(call("AAudioStream_close", &[stream]), 0);
        // SAFETY: the stream is closed, so the callback no longer runs.
        let calls = unsafe { std::ptr::read_volatile(&user.calls) };
        println!("the arm64 callback ran {calls} times in 3 s; {xruns} silence-filled cycle(s)");
        assert!(calls as f64 > 0.5 * 3.0 * rate as f64 / burst.max(1) as f64, "pulled too rarely: {calls}");
    }
}
