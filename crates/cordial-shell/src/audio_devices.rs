//! The audio sinks and sources this machine has, for the Audio rows in settings.
//!
//! One question, asked of the same code the client asks: `enumerate_devices()`
//! in `native/pipewire_backend.cpp`, reached through the small C ABI declared
//! at the bottom of `native/pipewire_backend.h`. Nothing here parses
//! `pw-dump`, shells out to `pactl`, or keeps a second idea of what a device
//! is. That matters more than it sounds — the whole point of storing a
//! `node.name` is that the picker and the thing that opens the stream agree
//! about which string identifies a device, and two implementations of "list
//! the sinks" is exactly how they would come to disagree.
//!
//! **The filtering happens on the C side, and one list never carries the
//! other.** The microphone rule at the top of `native/audio_classes.cpp` says
//! listing a microphone is not using one, but it also says nothing on an
//! enumeration path may construct a `CaptureStream`. So `cordial_audio_sinks`
//! drops the sources before allocating, and `cordial_audio_sources` is the one
//! place a microphone list is built: a registry walk and a copy of two strings,
//! with a test below that reads the process's open-capture count before and
//! after and wants zero both times. Sink monitors are not in it, because the
//! registry reports a monitor as a port on its sink and not as a node.
//!
//! **Why the shell links the client's native archive for this.** It is the
//! only reason it does, and it is not free: `cargo build -p cordial-shell`
//! now needs the `third_party/mcpelauncher-linker` submodule and Clang, where
//! before it needed neither. The alternative was a second registry walk
//! compiled only into the launcher, which would have cost nothing at build
//! time and would have been a second answer to the one question this module
//! exists to have exactly one answer to. Only `pipewire_backend.o` is
//! actually extracted from the archive — a static library contributes nothing
//! for symbols nobody references, so the bionic linker and libjnivm are on the
//! link line and not in the binary.

use std::ffi::CStr;

// Declared rather than called. `cordial-linker-sys`'s build script is what
// puts `libcordial_liblog.a` on this binary's link line, and without naming
// the crate here Cargo has no dependency edge to hang that on. It is not an
// accidental leftover: deleting this line produces an undefined reference to
// `cordial_audio_sinks` at link time, which is a considerably less obvious
// message than this comment.
use cordial_linker_sys as _;

/// One sink, as the settings row shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sink {
    /// `node.name` — the stable routing target that goes into `shell.json`
    /// and comes back out as `CORDIAL_AUDIO_SINK`. Never shown to the user.
    pub node_name: String,
    /// `node.description` — what their own volume control calls this device.
    /// This is the only string a user should ever read.
    pub description: String,
    /// This is the session's current `default.audio.sink`.
    pub is_default: bool,
}

#[repr(C)]
struct CordialAudioSink {
    node_name: *const std::os::raw::c_char,
    description: *const std::os::raw::c_char,
    is_default: std::os::raw::c_int,
}

unsafe extern "C" {
    fn cordial_audio_sinks(out: *mut *mut CordialAudioSink) -> usize;
    fn cordial_audio_sources(out: *mut *mut CordialAudioSink) -> usize;
    fn cordial_audio_sinks_free(sinks: *mut CordialAudioSink, count: usize);
    #[cfg(test)]
    fn cordial_audio_capture_streams() -> std::os::raw::c_uint;
}

/// A microphone or line input, as the settings row shows it. The same shape as
/// a [`Sink`] because the C side hands both back in one struct, so the row
/// builders and the label rule are shared rather than copied.
pub type Source = Sink;

/// Every audio output the PipeWire session currently has.
///
/// Empty means one of three things — no PipeWire at build time, no
/// `libpipewire-0.3.so.0` at run time, or no session behind it — and the
/// caller must present all three the same way: as "no devices found", never
/// as an invented "Default" entry. A picker offering a device that cannot
/// play is worse than an honestly empty one, because only the empty one sends
/// somebody looking for the real problem.
///
/// **This opens no stream.** It walks the registry and disconnects again; see
/// the module header.
pub fn sinks() -> Vec<Sink> {
    list(cordial_audio_sinks)
}

/// Every audio input the PipeWire session currently has, with the same three
/// empty cases as [`sinks`] and the same rule about presenting them.
///
/// **This opens no stream** -- see the module header, and
/// `listing_the_sources_opens_no_capture_stream` below, which is the check.
pub fn sources() -> Vec<Source> {
    list(cordial_audio_sources)
}

/// Process-wide count of open capture streams, from the native backend's own
/// counter. Test-only: nothing in the shell has a reason to ask.
#[cfg(test)]
fn open_capture_streams() -> u32 {
    // Safety: a plain atomic load on the C side.
    unsafe { cordial_audio_capture_streams() }
}

fn list(call: unsafe extern "C" fn(*mut *mut CordialAudioSink) -> usize) -> Vec<Sink> {
    let mut raw: *mut CordialAudioSink = std::ptr::null_mut();
    // Safety: `call` is one of the two list functions, which either leave `raw`
    // null and return 0, or write an array of `count` initialised entries whose
    // two pointers are NUL-terminated and owned by the array. Freed
    // unconditionally below.
    let count = unsafe { call(&mut raw) };
    if raw.is_null() || count == 0 {
        return Vec::new();
    }

    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        // Safety: `i < count`, and the C side never writes a null into either
        // pointer — a description it does not have is filled in with the node
        // name rather than left null, precisely so this loop has no branch.
        let entry = unsafe { &*raw.add(i) };
        // Safety: both pointers are NUL-terminated and live until the free
        // below, and the strings are copied out here.
        let (node_name, description) = unsafe {
            (
                CStr::from_ptr(entry.node_name).to_string_lossy().into_owned(),
                CStr::from_ptr(entry.description).to_string_lossy().into_owned(),
            )
        };
        out.push(Sink { node_name, description, is_default: entry.is_default != 0 });
    }
    // Safety: `raw` and `count` are exactly what `call` returned, and nothing
    // above keeps a pointer into the array.
    unsafe { cordial_audio_sinks_free(raw, count) };
    out
}

/// What the row for `sink` should read.
///
/// The description alone, except for the session's own default, which is
/// marked. Both entries are then visible at once — "System default" and the
/// device it currently resolves to — which is the difference between a picker
/// that tells somebody where their sound is going and one that makes them
/// guess.
pub fn row_label(sink: &Sink) -> String {
    if sink.is_default {
        format!("{} (current system default)", sink.description)
    } else {
        sink.description.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_current_default_is_marked_so_both_entries_can_be_read_together() {
        let d = Sink {
            node_name: "alsa_output.pci-0000_00_1f.3.analog-stereo".into(),
            description: "Built-in Audio Analogue Stereo".into(),
            is_default: true,
        };
        assert_eq!(row_label(&d), "Built-in Audio Analogue Stereo (current system default)");

        let other = Sink { is_default: false, ..d };
        assert_eq!(row_label(&other), "Built-in Audio Analogue Stereo");
    }

    #[test]
    fn asking_for_the_sinks_is_safe_with_or_without_a_session() {
        // Deliberately asserts nothing about the contents. On a developer's
        // machine this returns their real devices and in a container with no
        // PipeWire it returns none, and a test that expected either would fail
        // on the other. What it does check is the part that is the same
        // everywhere and is the part that could actually be wrong: that the
        // C ABI hands back something Rust can own without leaking or reading
        // past the end, which is what running it under the ordinary test
        // harness exercises.
        for sink in sinks() {
            assert!(!sink.node_name.is_empty(), "a sink with no node.name cannot be stored");
            assert!(!sink.description.is_empty(), "a sink with no label cannot be shown");
        }
    }

    /// **The microphone rule, pinned from the shell's side.** Listing the
    /// microphones must not open one: a picker that lit the desktop's
    /// recording indicator every time Settings opened would be the exact harm
    /// the rule at the top of `native/audio_classes.cpp` exists to prevent.
    /// The count read here is the backend's own, the one `audio_classes.cpp`
    /// logs when the engine stops recording, so a change that made enumeration
    /// construct a `CaptureStream` would move it. Held through several calls
    /// and through the sink list as well, because the two share a registry walk.
    #[test]
    fn listing_the_sources_opens_no_capture_stream() {
        assert_eq!(open_capture_streams(), 0, "nothing in this process has opened a microphone yet");
        for _ in 0..3 {
            for source in sources() {
                assert!(!source.node_name.is_empty(), "a source with no node.name cannot be stored");
                assert!(!source.description.is_empty(), "a source with no label cannot be shown");
            }
            let _ = sinks();
            assert_eq!(open_capture_streams(), 0, "listing devices opened a capture stream");
        }
    }

    #[test]
    fn the_two_lists_never_carry_each_others_devices() {
        // Same session, same walk, filtered opposite ways: a name in both would
        // mean a node was reported as an input and an output at once, and the
        // picker would offer a sink to record from.
        let outputs = sinks();
        for source in sources() {
            assert!(
                !outputs.iter().any(|s| s.node_name == source.node_name),
                "{} is listed as both a sink and a source",
                source.node_name
            );
        }
    }
}
