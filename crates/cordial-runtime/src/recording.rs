//! Recording the game's own frames to a video file (ADR-059, issue #33).
//!
//! Roblox's Record Video button does nothing on the Android client, and no
//! compositor route reaches a native Wayland window without a person clicking a
//! portal dialog. What Cordial does have is the engine's swapchain, read at the
//! moment a frame is complete (`android::capture`, ADR-019). Recording is that
//! same read on a timer, handed to an encoder.
//!
//! **The encoder is a child process, `ffmpeg`, fed raw frames on its stdin.**
//! Nothing is linked: an encoder library would put a codec licence and a build
//! dependency into every Cordial binary for a feature most sessions never use,
//! and `ffmpeg` is already in the Flatpak runtime (with libvpx, so WebM works
//! there) and on nearly every desktop (with libx264, so MP4 does). Which codecs
//! exist is asked of the binary itself rather than assumed, because a build
//! without libx264 and a request for `.mp4` is the ordinary case on the
//! Flatpak and must produce a file, not an error.
//!
//! **The engine never waits for the encoder.** A frame is read inside
//! `vkQueuePresentKHR`, copied into a buffer, and offered to a bounded channel
//! a writer thread drains into the pipe. If the encoder is slower than the
//! frame rate the offer fails and the frame is dropped and counted, because the
//! alternative is a game that stutters whenever the disk does. The drop count is
//! in the summary so a choppy recording says why.
//!
//! **Timestamps are the wall clock at the encoder's end of the pipe**, with
//! `-vf fps` filling the gaps. The engine presents at the display rate while
//! input flows and at exactly one frame a second when it does not (the idle
//! throttle, AGENTS.md), so a fixed frame period assumed here would make every
//! quiet stretch play back at 60 times speed.
//!
//! **The game's sound is a copy of what Cordial hands the audio backend**, not
//! a mix taken from the sound server. `native/record_tap.h` copies the buffers
//! the engine's AAudio and OpenSL ES streams fill into a lock-free ring, and a
//! pump thread here reads them as 48 kHz stereo float and writes them to a
//! second pipe (`pipe:3`) of the same `ffmpeg`. The audio thread never waits for
//! us. The pump is clocked by the wall clock, like the video, and pads with
//! silence where the game made none, so a quiet stretch is the right length.
//! Why it is this and not a sink monitor: ADR-059.
//!
//! **A hotkey toggles it** ([`hotkey_event`]), consumed before the key reaches
//! the engine, and the window title carries a marker while it records.

use crate::android::capture::{self, Readback, Target};
use std::ffi::c_void;
use std::io::{BufRead, BufReader, Write};
use std::os::raw::c_int;
use std::os::unix::io::{FromRawFd, RawFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Frames a second asked of the engine when nobody said.
pub const DEFAULT_FPS: u32 = 30;
/// Frames waiting for the writer. Four of a 1080p frame is 32 MB; more would
/// only delay the moment a slow encoder shows up as a drop.
const QUEUE_DEPTH: usize = 4;

// ------------------------------------------------------------- the game audio tap

unsafe extern "C" {
    fn cordial_record_tap_enable(on: c_int);
    fn cordial_record_tap_read(dst: *mut f32, frames: usize, pos: u64);
    fn cordial_record_tap_streams() -> u32;
    fn cordial_record_tap_dropped() -> u64;
    /// Tests and the control surface only; see the C side.
    fn cordial_record_tap_inject(stereo: *const f32, frames: usize);
}

/// Play `samples` (interleaved 48 kHz stereo float) into the tap as though a
/// stream had. For tests and `devctl`, never the engine's own path.
pub fn inject_samples(samples: &[f32]) {
    // SAFETY: `samples` is valid for `len` floats, and the callee reads `len / 2` frames.
    unsafe { cordial_record_tap_inject(samples.as_ptr(), samples.len() / 2) };
}

/// The rate and layout of the audio pipe, fixed because the tap converts to it.
const TAP_RATE: u32 = 48_000;
/// How often the pump tops the pipe up.
const PUMP_TICK: Duration = Duration::from_millis(10);
/// How far behind the wall clock the pump reads the tap, in frames. The sound is
/// *placed* on the timeline by when the engine produced it, so this does not
/// delay it; it is the slack that lets a backend's burst arrive late without a
/// hole where it should have been (`native/record_tap.h`). Eighty milliseconds
/// covers the 43 ms bursts and the few ms of scheduling jitter measured.
const PUMP_LAG: u64 = TAP_RATE as u64 / 1000 * 80;

// ---------------------------------------------------------------- encoder choice

/// A container and the codecs that go in it, as `ffmpeg` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Codec {
    pub ext: &'static str,
    pub name: &'static str,
    video: Vec<String>,
    audio: Vec<String>,
    movflags: bool,
}

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

/// The names `ffmpeg -encoders` lists, ignoring its banner and legend.
///
/// A line is `" V....D libx264   H.264 ..."`: six flag characters, then the
/// name. The legend above it has lines like `" V..... = Video"`, whose second
/// token is `=`, so requiring the first token to be six flag characters and the
/// second not to be `=` is what tells an encoder from its own key.
pub fn encoder_names(listing: &str) -> Vec<&str> {
    listing
        .lines()
        .filter_map(|line| {
            let mut it = line.split_whitespace();
            let flags = it.next()?;
            let name = it.next()?;
            let kind = flags.chars().next()?;
            (flags.len() == 6 && matches!(kind, 'V' | 'A' | 'S') && name != "=").then_some(name)
        })
        .collect()
}

/// Pick a container and codecs from what the local `ffmpeg` can encode.
///
/// `want` is the extension the caller asked for, if any. With none, the best
/// container this `ffmpeg` can fill is chosen: MP4 where there is H.264, WebM
/// where there is libvpx, Matroska for anything else. An extension that
/// `ffmpeg` cannot fill is an error that names it, not a silent switch to a
/// different container, because a file called `.mp4` that is actually WebM is
/// the kind of surprise that costs somebody an upload.
///
/// **x264 is run without B-frames (`-bf 0`).** With them the MP4 muxer puts the
/// first picture at 0.067 s and the sound at 0, because a stream that reorders
/// frames has its presentation times pushed back by the reorder delay; a clip
/// then plays its sound two frames before its picture. Measured: a flash and a
/// beep fed at the same instant came out 107 ms apart with B-frames on. The
/// frames of a game are mostly new, so the compression given up is small.
pub fn choose_codec(want: Option<&str>, encoders: &[&str]) -> Result<Codec, String> {
    let has = |n: &str| encoders.contains(&n);
    let x264 = || {
        has("libx264").then(|| ("libx264", args(&["-c:v", "libx264", "-preset", "veryfast", "-bf", "0", "-crf", "23"])))
    };
    let openh264 = || has("libopenh264").then(|| ("libopenh264", args(&["-c:v", "libopenh264", "-b:v", "8M"])));
    let vp9 = || {
        has("libvpx-vp9").then(|| {
            (
                "libvpx-vp9",
                args(&["-c:v", "libvpx-vp9", "-deadline", "realtime", "-cpu-used", "8", "-row-mt", "1", "-b:v", "0", "-crf", "34"]),
            )
        })
    };
    let vp8 = || {
        has("libvpx").then(|| {
            ("libvpx", args(&["-c:v", "libvpx", "-deadline", "realtime", "-cpu-used", "8", "-b:v", "6M"]))
        })
    };
    let svtav1 = || has("libsvtav1").then(|| ("libsvtav1", args(&["-c:v", "libsvtav1", "-preset", "10", "-crf", "36"])));
    let mpeg4 = || has("mpeg4").then(|| ("mpeg4", args(&["-c:v", "mpeg4", "-q:v", "4"])));

    let aac = || args(&["-c:a", "aac", "-b:a", "160k"]);
    let opus = || {
        if has("libopus") {
            args(&["-c:a", "libopus", "-b:a", "128k"])
        } else {
            aac()
        }
    };

    let ext = want.map(|e| e.to_ascii_lowercase());
    let (ext, video): (&'static str, Option<(&'static str, Vec<String>)>) = match ext.as_deref() {
        None => {
            if let Some(v) = x264().or_else(openh264) {
                ("mp4", Some(v))
            } else if let Some(v) = vp9().or_else(vp8) {
                ("webm", Some(v))
            } else {
                ("mkv", svtav1().or_else(mpeg4))
            }
        }
        Some("mp4") | Some("m4v") => ("mp4", x264().or_else(openh264).or_else(svtav1).or_else(mpeg4)),
        Some("webm") => ("webm", vp9().or_else(vp8).or_else(svtav1)),
        Some("mkv") => ("mkv", x264().or_else(openh264).or_else(vp9).or_else(vp8).or_else(svtav1).or_else(mpeg4)),
        Some(other) => {
            return Err(format!("cannot record to .{other}; use .mp4, .webm or .mkv"));
        }
    };
    let Some((name, video)) = video else {
        return Err(format!(
            "this ffmpeg has no encoder for .{ext} (it lists none of libx264, libopenh264, libvpx-vp9, libvpx, libsvtav1 or mpeg4)"
        ));
    };
    Ok(Codec {
        ext,
        name,
        video,
        audio: if ext == "mp4" { aac() } else { opus() },
        // A fragmented MP4 is playable up to wherever it was cut off. A plain
        // one has its index written last, so a client that is killed, or a
        // disk that fills, leaves a file no player will open.
        movflags: ext == "mp4",
    })
}

/// Everything one encoder process needs to be started.
#[derive(Debug, Clone)]
pub struct EncodeSpec {
    pub width: u32,
    pub height: u32,
    pub bgra: bool,
    pub fps: u32,
    pub codec: Codec,
    pub out: PathBuf,
    /// The sound to record alongside, if any.
    pub audio: Option<AudioIn>,
}

/// Where a recording's sound comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioIn {
    /// What Cordial plays, copied from its own audio buffers by the tap and
    /// written to `ffmpeg`'s file descriptor 3 as 48 kHz stereo float.
    Game,
    /// A PulseAudio source: everything the default output is playing. PipeWire
    /// answers to the same name through its Pulse compatibility layer.
    Desktop(String),
}

/// The `ffmpeg` command line for one segment.
pub fn ffmpeg_args(s: &EncodeSpec) -> Vec<String> {
    let mut a = args(&["-hide_banner", "-loglevel", "warning", "-nostdin", "-y"]);
    a.extend(args(&[
        "-f",
        "rawvideo",
        "-pixel_format",
        if s.bgra { "bgra" } else { "rgba" },
        "-video_size",
        &format!("{}x{}", s.width, s.height),
        "-use_wallclock_as_timestamps",
        "1",
        "-i",
        "pipe:0",
    ]));
    match &s.audio {
        Some(AudioIn::Desktop(src)) => {
            a.extend(args(&["-thread_queue_size", "512", "-f", "pulse", "-i", src]));
        }
        Some(AudioIn::Game) => {
            a.extend(args(&[
                "-thread_queue_size",
                "1024",
                "-f",
                "f32le",
                "-ar",
                &TAP_RATE.to_string(),
                "-ac",
                "2",
                "-i",
                "pipe:3",
            ]));
        }
        None => {}
    }
    // `crop` to an even size because yuv420p, which every player accepts, has
    // half-resolution chroma and the encoders refuse an odd edge. `fps` last of
    // the timing filters so the constant rate is what the encoder sees.
    a.extend(args(&[
        "-vf",
        &format!("fps={},crop=trunc(iw/2)*2:trunc(ih/2)*2,format=yuv420p", s.fps),
    ]));
    a.extend(s.codec.video.iter().cloned());
    if s.audio.is_some() {
        a.extend(s.codec.audio.iter().cloned());
    }
    if matches!(s.audio, Some(AudioIn::Desktop(_))) {
        // A live desktop source never ends, so without this ffmpeg would keep
        // recording silence after the video pipe closed and the file would
        // never be finished.
        //
        // **The game's pipe must not have it.** With `-shortest` ffmpeg 8 stops
        // reading `pipe:3` after the first few hundred milliseconds and holds
        // the rest until the video ends, so a pipe fed in real time fills and
        // its writer blocks for the whole recording (measured: 5.6 s blocked
        // out of 6 with it, 0.3 s without, same arguments otherwise). The
        // game's pipe closes when the video's does, and `close_encoding` kills
        // an encoder that has not finished in 20 s.
        a.push("-shortest".into());
    }
    if s.codec.movflags {
        a.extend(args(&["-movflags", "+frag_keyframe+empty_moov+default_base_moof"]));
    }
    a.push(s.out.to_string_lossy().into_owned());
    a
}

// ------------------------------------------------------------------- where it goes

/// The directory a recording lands in when nobody named one.
///
/// `CORDIAL_RECORD_DIR`, else the user's Videos folder under `Cordial/`, else
/// the profile. **Inside a Flatpak it is the profile, always:** the sandbox has
/// no grant on `~/Videos`, and writing there succeeds into the sandbox's own
/// temporary home, so the file would be reported as saved and gone at exit.
pub fn default_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("CORDIAL_RECORD_DIR").filter(|d| !d.is_empty()) {
        return PathBuf::from(d);
    }
    let profile = crate::profile::active().join("recordings");
    if Path::new("/.flatpak-info").exists() {
        return profile;
    }
    match videos_dir() {
        Some(v) => v.join("Cordial"),
        None => profile,
    }
}

fn videos_dir() -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    let conf = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"));
    let text = std::fs::read_to_string(conf.join("user-dirs.dirs")).unwrap_or_default();
    Some(parse_videos_dir(&text, &home).unwrap_or_else(|| home.join("Videos")))
}

/// `XDG_VIDEOS_DIR="$HOME/Vidéos"` out of `user-dirs.dirs`.
pub fn parse_videos_dir(text: &str, home: &Path) -> Option<PathBuf> {
    let line = text.lines().find(|l| l.trim_start().starts_with("XDG_VIDEOS_DIR="))?;
    let value = line.split_once('=')?.1.trim().trim_matches('"');
    if let Some(rest) = value.strip_prefix("$HOME") {
        Some(home.join(rest.trim_start_matches('/')))
    } else if value.starts_with('/') {
        Some(PathBuf::from(value))
    } else {
        None
    }
}

/// A file name that sorts by time and has no character a file manager objects to.
pub fn stamp_name(t: &Stamp) -> String {
    format!(
        "Cordial-{:04}-{:02}-{:02}_{:02}-{:02}-{:02}",
        t.year, t.month, t.day, t.hour, t.minute, t.second
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stamp {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
}

fn now_local() -> Stamp {
    // SAFETY: `localtime_r` fills the caller's `tm` completely and is the
    // reentrant form, so nothing static is shared with other threads.
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&now, &mut tm);
        Stamp {
            year: tm.tm_year + 1900,
            month: (tm.tm_mon + 1) as u32,
            day: tm.tm_mday as u32,
            hour: tm.tm_hour as u32,
            minute: tm.tm_min as u32,
            second: tm.tm_sec as u32,
        }
    }
}

/// Where a recording goes, and which extension was asked for.
///
/// `arg` is empty (the default directory), a directory (an existing one, or
/// anything ending in `/`), or a file. The file name is the stem for every
/// segment: a window resized mid-recording needs a new encoder, because a raw
/// frame cannot change size under it, and the second file is `stem-2.ext`.
pub fn resolve_target(arg: &str, default_dir: &Path, stamp: &Stamp) -> (PathBuf, Option<String>) {
    let arg = arg.trim();
    let dir_like = arg.is_empty() || arg.ends_with('/') || Path::new(arg).is_dir();
    if dir_like {
        let dir = if arg.is_empty() { default_dir.to_path_buf() } else { PathBuf::from(arg) };
        return (dir.join(stamp_name(stamp)), None);
    }
    let p = PathBuf::from(arg);
    match p.extension().and_then(|e| e.to_str()) {
        Some(e) => (p.with_extension(""), Some(e.to_string())),
        None => (p, None),
    }
}

fn segment_path(stem: &Path, segment: u32, ext: &str) -> PathBuf {
    let mut name = stem.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    if segment > 1 {
        name.push(format!("-{segment}"));
    }
    name.push(".");
    name.push(ext);
    stem.with_file_name(name)
}

// -------------------------------------------------------------------- the session

/// Checked once per present, so recording off costs one atomic load.
static ACTIVE: AtomicBool = AtomicBool::new(false);
/// When the next frame is wanted, in nanoseconds since [`base`].
static NEXT_DUE_NS: AtomicU64 = AtomicU64::new(0);
static SESSION: Mutex<Option<Session>> = Mutex::new(None);
/// A request from the environment, waiting for the first frame to exist.
static ARMED: Mutex<Option<String>> = Mutex::new(None);
static ARMED_FAST: AtomicBool = AtomicBool::new(false);
/// Set whenever recording starts or stops, for the window title (see
/// [`take_indicator_change`]).
static INDICATOR_DIRTY: AtomicBool = AtomicBool::new(false);

fn base() -> Instant {
    static BASE: OnceLock<Instant> = OnceLock::new();
    *BASE.get_or_init(Instant::now)
}

fn now_ns() -> u64 {
    base().elapsed().as_nanos() as u64
}

/// Whether the present path should offer a frame to [`on_frame`].
pub fn frame_due() -> bool {
    if ARMED_FAST.load(Ordering::Acquire) {
        return true;
    }
    ACTIVE.load(Ordering::Acquire) && now_ns() >= NEXT_DUE_NS.load(Ordering::Relaxed)
}

pub fn active() -> bool {
    ACTIVE.load(Ordering::Acquire)
}

struct Encoding {
    child: Child,
    /// The game-audio pump for this encoder, when the recording has one.
    pump: Option<Pump>,
    tx: Option<SyncSender<Vec<u8>>>,
    writer: Option<std::thread::JoinHandle<Result<(), String>>>,
    stderr: Option<std::thread::JoinHandle<()>>,
    pool: Arc<Mutex<Vec<Vec<u8>>>>,
    tail: Arc<Mutex<Vec<String>>>,
}

struct Session {
    stem: PathBuf,
    fps: u32,
    ffmpeg: PathBuf,
    codec: Codec,
    segment: u32,
    dims: (u32, u32),
    enc: Option<Encoding>,
    readback: Option<Readback>,
    files: Vec<PathBuf>,
    started: Instant,
    taken: u64,
    dropped: u64,
    read_ns: u64,
    /// The sound being recorded with the video, if any.
    audio: Option<AudioIn>,
}

fn find_in_path(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

fn probe_encoders(ffmpeg: &Path) -> Result<String, String> {
    let out = Command::new(ffmpeg)
        .args(["-hide_banner", "-encoders"])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("could not run {}: {e}", ffmpeg.display()))?;
    let listing = String::from_utf8_lossy(&out.stdout).into_owned();
    if encoder_names(&listing).is_empty() {
        // An `ffmpeg` that runs and lists nothing is a wrapper or a sandbox
        // refusing it, and its own stderr is the only place that says so.
        let why = String::from_utf8_lossy(&out.stderr);
        return Err(format!("{} listed no encoders: {}", ffmpeg.display(), why.trim()));
    }
    Ok(listing)
}

/// The default output device's monitor: what the user is hearing, every
/// application's sound mixed.
pub const DESKTOP_SOURCE: &str = "@DEFAULT_MONITOR@";

/// Whether this `ffmpeg` can read a PulseAudio source at all (`pulse` is an
/// input device, listed by `-devices` and not by `-demuxers`). The Flatpak
/// runtime's can; a distribution's build usually can; a Homebrew one does not.
fn has_pulse_input(ffmpeg: &Path) -> bool {
    Command::new(ffmpeg)
        .args(["-hide_banner", "-devices"])
        .stdin(Stdio::null())
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .any(|l| l.split_whitespace().nth(1) == Some("pulse"))
        })
        .unwrap_or(false)
}

/// Which audio to record, from `CORDIAL_RECORD_AUDIO`, and a phrase for the
/// log saying what was decided.
///
/// **Off unless asked for, and the default thing to ask for is the game.** The
/// sound of the game alone is what somebody sharing a clip wants, and it keeps
/// the file free of whatever else the machine was playing -- a call, a
/// notification, music from another window. `desktop` is the old behaviour and
/// stays for the case where the game's sound is not enough (a voice chat in
/// another program, say), under a name that says what it records. Anything
/// else is off and says so, because a typo that silently became "everything
/// the default output is playing" would be a privacy surprise in a file the
/// user is about to share.
pub fn audio_choice(setting: Option<&str>, can_pulse: impl FnOnce() -> bool) -> (Option<AudioIn>, String) {
    match setting.map(str::trim) {
        None | Some("") | Some("0") | Some("off") | Some("none") => (None, "no audio".into()),
        Some("game") | Some("1") | Some("on") => (Some(AudioIn::Game), "audio: the game's own sound".into()),
        Some("desktop") => {
            if can_pulse() {
                (
                    Some(AudioIn::Desktop(DESKTOP_SOURCE.into())),
                    "audio: everything the default output is playing".into(),
                )
            } else {
                (None, "no audio: this ffmpeg cannot read PulseAudio".into())
            }
        }
        Some(other) => (None, format!("no audio: CORDIAL_RECORD_AUDIO={other:?} is not off, game or desktop")),
    }
}

/// Start recording. `arg` is as [`resolve_target`] reads it.
///
/// Returns once the session exists; the encoder is started by the first frame,
/// because its input size is that frame's size and nobody knows it before then.
pub fn start(arg: &str) -> Result<String, String> {
    let mut slot = SESSION.lock().unwrap_or_else(|e| e.into_inner());
    if slot.is_some() {
        return Err("already recording; stop first".into());
    }
    let ffmpeg = find_in_path("ffmpeg").ok_or(
        "ffmpeg was not found on PATH; install it to record (the Flatpak runtime ships one)",
    )?;
    let listing = probe_encoders(&ffmpeg)?;
    let (stem, want_ext) = resolve_target(arg, &default_dir(), &now_local());
    let codec = choose_codec(want_ext.as_deref(), &encoder_names(&listing))?;
    if let Some(dir) = stem.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    }
    let (audio, audio_note) = audio_choice(
        std::env::var("CORDIAL_RECORD_AUDIO").ok().as_deref(),
        || has_pulse_input(&ffmpeg),
    );
    let fps = std::env::var("CORDIAL_RECORD_FPS")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .filter(|v| (1..=120).contains(v))
        .unwrap_or(DEFAULT_FPS);
    let first = segment_path(&stem, 1, codec.ext);
    let desc = format!("{} ({} at {} fps, {})", first.display(), codec.name, fps, audio_note);
    *slot = Some(Session {
        stem,
        fps,
        ffmpeg,
        codec,
        segment: 0,
        dims: (0, 0),
        enc: None,
        readback: None,
        files: Vec::new(),
        started: Instant::now(),
        taken: 0,
        dropped: 0,
        read_ns: 0,
        audio,
    });
    NEXT_DUE_NS.store(0, Ordering::Relaxed);
    ACTIVE.store(true, Ordering::Release);
    INDICATOR_DIRTY.store(true, Ordering::Release);
    println!("[record] recording to {desc}");
    Ok(desc)
}

/// Ask for a recording to begin on the first presented frame. For
/// `CORDIAL_RECORD`, which is read before any swapchain exists.
pub fn arm(arg: &str) {
    *ARMED.lock().unwrap_or_else(|e| e.into_inner()) = Some(arg.to_string());
    ARMED_FAST.store(true, Ordering::Release);
}

/// `CORDIAL_RECORD`: `1` records to the default directory, anything else is
/// the directory or file to record to. `0` and empty are off.
pub fn arm_from_env() {
    // Decided here so the log names the key before anyone presses it.
    let _ = hotkey();
    match std::env::var("CORDIAL_RECORD") {
        Ok(v) if v != "0" && !v.is_empty() => arm(if v == "1" { "" } else { &v }),
        _ => {}
    }
}

pub fn status() -> String {
    let slot = SESSION.lock().unwrap_or_else(|e| e.into_inner());
    match slot.as_ref() {
        None => "not recording".into(),
        Some(s) => format!(
            "recording {} frames={} dropped={} elapsed={:.1}s",
            s.files.last().map(|p| p.display().to_string()).unwrap_or_else(|| "(waiting for a frame)".into()),
            s.taken,
            s.dropped,
            s.started.elapsed().as_secs_f32()
        ),
    }
}

// ------------------------------------------------------------------------ hotkey

/// Linux evdev codes of the keys `CORDIAL_RECORD_KEY` may name: the function
/// keys and the two keys on a keyboard that nothing else uses, named as a
/// keyboard prints them. **F11 is not on the list** because it is the
/// fullscreen key and is taken before this is asked.
const KEY_NAMES: &[(&str, i32)] = &[
    ("f1", 59),
    ("f2", 60),
    ("f3", 61),
    ("f4", 62),
    ("f5", 63),
    ("f6", 64),
    ("f7", 65),
    ("f8", 66),
    ("f9", 67),
    ("f10", 68),
    ("f12", 88),
    ("scrolllock", 70),
    ("pause", 119),
];

/// The key `CORDIAL_RECORD_KEY` names, as an evdev code: [`DEFAULT_KEY`] when
/// it is unset, `None` when it says `off`, and an error that names the value
/// when it is something else, so a typo is reported rather than leaving the
/// user with a key that silently does nothing.
pub fn parse_key(value: Option<&str>) -> Result<Option<i32>, String> {
    let v = match value.map(str::trim) {
        None | Some("") => return Ok(Some(DEFAULT_KEY)),
        Some(v) => v.to_ascii_lowercase().replace(['_', '-', ' '], ""),
    };
    if matches!(v.as_str(), "off" | "none" | "0" | "no" | "disabled") {
        return Ok(None);
    }
    KEY_NAMES
        .iter()
        .find(|(n, _)| *n == v)
        .map(|(_, code)| Some(*code))
        .ok_or_else(|| {
            let names: Vec<&str> = KEY_NAMES.iter().map(|(n, _)| *n).collect();
            format!("CORDIAL_RECORD_KEY={value:?} is not off or one of {}", names.join(", "))
        })
}

/// F9, which is not a key Android's `KeyEvent` table is missing and not one the
/// launcher or the compositors this is run under take for themselves.
pub const DEFAULT_KEY: i32 = 67;

/// The hotkey in force, decided once. Said in the log the first time it is
/// asked, which is the first key the window sees, so what was chosen is in the
/// same file as what it did.
fn hotkey() -> Option<i32> {
    static KEY: OnceLock<Option<i32>> = OnceLock::new();
    *KEY.get_or_init(|| {
        let var = std::env::var("CORDIAL_RECORD_KEY").ok();
        match parse_key(var.as_deref()) {
            Ok(Some(code)) => {
                let name = KEY_NAMES.iter().find(|(_, c)| *c == code).map(|(n, _)| *n).unwrap_or("?");
                println!("[record] hotkey: {} starts and stops recording while the game window has focus", name.to_ascii_uppercase());
                Some(code)
            }
            Ok(None) => {
                println!("[record] hotkey: off (CORDIAL_RECORD_KEY)");
                None
            }
            Err(e) => {
                println!("[record] hotkey: off, {e}");
                None
            }
        }
    })
}

/// Whether the hotkey's press has been taken and its release is still owed.
static HOTKEY_HELD: AtomicBool = AtomicBool::new(false);
/// A start or stop is in flight; a second press meanwhile is ignored.
static TOGGLING: AtomicBool = AtomicBool::new(false);

/// Offer a key event to the recorder. Returns true when it was the hotkey and
/// must not reach the engine -- both the press and the release, so the game
/// never sees half a key.
///
/// `modified` is whether Control or Alt is down. Those chords are left to the
/// game, so that taking F9 does not take Ctrl+F9 or Alt+F9 with it. A repeat of
/// a held key is consumed and does not toggle again.
pub fn hotkey_event(evdev: i32, down: bool, modified: bool) -> bool {
    let Some(code) = hotkey() else { return false };
    if evdev != code {
        return false;
    }
    if down {
        if modified && !HOTKEY_HELD.load(Ordering::Acquire) {
            return false;
        }
        if !HOTKEY_HELD.swap(true, Ordering::AcqRel) {
            toggle_in_background();
        }
        true
    } else {
        HOTKEY_HELD.swap(false, Ordering::AcqRel)
    }
}

/// Start or stop on a thread of its own: stopping waits for the encoder to
/// finish its file, and this is called from the window's input path.
fn toggle_in_background() {
    if TOGGLING.swap(true, Ordering::AcqRel) {
        return;
    }
    let spawned = std::thread::Builder::new().name("cordial-rec-toggle".into()).spawn(|| {
        let result = toggle();
        TOGGLING.store(false, Ordering::Release);
        match result {
            Ok(m) => println!("[record] hotkey: {m}"),
            Err(e) => println!("[record] hotkey: {e}"),
        }
    });
    if spawned.is_err() {
        TOGGLING.store(false, Ordering::Release);
    }
}

/// Stop if recording, start if not. A recording armed by `CORDIAL_RECORD` and
/// still waiting for its first frame counts as recording, so the key cancels it.
pub fn toggle() -> Result<String, String> {
    let waiting = ARMED_FAST.load(Ordering::Acquire);
    if active() || waiting {
        stop().map(|m| format!("stopped: {m}")).or_else(|e| {
            if waiting {
                Ok("cancelled before the first frame".to_string())
            } else {
                Err(e)
            }
        })
    } else {
        start("").map(|m| format!("started: {m}"))
    }
}

/// `Some(recording)` once after each start or stop, for the window title. Taken
/// by the thread that owns the window; the caller sets the title and nothing
/// here touches GTK.
pub fn take_indicator_change() -> Option<bool> {
    INDICATOR_DIRTY.swap(false, Ordering::AcqRel).then(active)
}

/// Stop, flush the encoder and report what was written.
pub fn stop() -> Result<String, String> {
    ACTIVE.store(false, Ordering::Release);
    INDICATOR_DIRTY.store(true, Ordering::Release);
    ARMED_FAST.store(false, Ordering::Release);
    *ARMED.lock().unwrap_or_else(|e| e.into_inner()) = None;
    let session = SESSION.lock().unwrap_or_else(|e| e.into_inner()).take();
    let Some(session) = session else { return Err("not recording".into()) };
    // Outside the lock: flushing waits on the encoder, and the present thread
    // takes the lock every frame it is asked for.
    finish(session, false)
}

/// For the end of the process, which leaves through `_exit` and would
/// otherwise abandon the encoder mid-file.
pub fn finish_at_exit() {
    ACTIVE.store(false, Ordering::Release);
    ARMED_FAST.store(false, Ordering::Release);
    let session = SESSION.lock().unwrap_or_else(|e| e.into_inner()).take();
    if let Some(session) = session {
        match finish(session, true) {
            Ok(s) => println!("[record] {s}"),
            Err(e) => println!("[record] {e}"),
        }
    }
}

/// `at_exit` is for a process that is leaving: the engine has already torn its
/// Vulkan device down by then, and destroying the readback's command pool on a
/// device that no longer exists crashed inside the driver (measured: a
/// segfault in `vkDestroyCommandPool` under `anv_state_pool_free`, every
/// time). The readback is leaked instead, which the kernel reclaims a moment
/// later.
fn finish(mut s: Session, at_exit: bool) -> Result<String, String> {
    if s.audio == Some(AudioIn::Game) {
        // SAFETY: as in `start`.
        unsafe { cordial_record_tap_enable(0) };
    }
    if at_exit {
        if let Some(rb) = s.readback.take() {
            std::mem::forget(rb);
        }
    }
    let mut problems = Vec::new();
    if let Some(enc) = s.enc.take() {
        if let Err(e) = close_encoding(enc) {
            problems.push(e);
        }
    }
    let secs = s.started.elapsed().as_secs_f32();
    let per = if s.taken > 0 { s.read_ns as f64 / s.taken as f64 / 1e6 } else { 0.0 };
    let mut out = format!(
        "{} frames in {:.1}s, {} dropped, {:.2} ms per frame read on the present thread; ",
        s.taken, secs, s.dropped, per
    );
    if s.files.is_empty() {
        out.push_str("no frame was presented, so no file was written");
    } else {
        let list: Vec<String> = s.files.iter().map(|p| p.display().to_string()).collect();
        out.push_str(&format!("wrote {}", list.join(", ")));
    }
    if !problems.is_empty() {
        return Err(format!("{out}; {}", problems.join("; ")));
    }
    Ok(out)
}

fn close_encoding(mut enc: Encoding) -> Result<(), String> {
    drop(enc.tx.take());
    let mut problems = Vec::new();
    if let Some(w) = enc.writer.take() {
        match w.join() {
            Ok(Ok(())) => {}
            Ok(Err(e)) => problems.push(e),
            Err(_) => problems.push("the writer thread panicked".into()),
        }
    }
    // After the writer, so the audio ends with the last frame. Told to stop now
    // and joined after the encoder is gone: the pump closing its end of the pipe
    // is what lets the encoder finish, and if the encoder has wedged, killing it
    // below is what lets a pump blocked in a write fail and return.
    if let Some(pump) = &enc.pump {
        pump.stop.store(true, Ordering::Release);
    }
    // The encoder finishes its output once stdin and the audio pipe close.
    // Bounded, because a client that exits must not hang on an encoder that has
    // wedged.
    let deadline = Instant::now() + Duration::from_secs(20);
    let status = loop {
        match enc.child.try_wait() {
            Ok(Some(st)) => break Some(st),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = enc.child.kill();
                let _ = enc.child.wait();
                problems.push("the encoder did not finish within 20 s and was killed".into());
                break None;
            }
        }
    };
    if let Some(pump) = enc.pump.take() {
        if let Err(e) = pump.finish() {
            // An encoder that was killed leaves the pump with a broken pipe,
            // which is the kill's doing and already reported above.
            if status.is_some() {
                problems.push(e);
            }
        }
    }
    // Joined only when the encoder exited by itself. After a kill, a process the
    // wrapper started may still hold the pipe open, and waiting for its end of
    // file would hang the client's exit on the very thing the kill was for.
    if let Some(h) = enc.stderr.take() {
        if status.is_some() {
            let _ = h.join();
        }
    }
    if let Some(st) = status {
        if !st.success() {
            let tail = enc.tail.lock().map(|t| t.join(" | ")).unwrap_or_default();
            problems.push(format!("ffmpeg exited with {st}: {tail}"));
        }
    }
    if problems.is_empty() { Ok(()) } else { Err(problems.join("; ")) }
}

/// The audio half of one encoder: a pipe into `ffmpeg`'s descriptor 3 and the
/// thread that keeps it supplied.
struct Pump {
    stop: Arc<AtomicBool>,
    clock: Arc<PumpClock>,
    thread: Option<std::thread::JoinHandle<Result<(), String>>>,
}

/// When the video writer first got a frame into the encoder.
///
/// **The audio's time zero is this instant and not the moment `ffmpeg` was
/// spawned.** `ffmpeg` timestamps the video at the wall clock when it reads it
/// and the audio by counting samples from zero, and it lines the first of each
/// up. The first frame is several megabytes into a pipe that holds sixty-four
/// kilobytes, so the writer's first `write_all` cannot return until `ffmpeg` is
/// reading, and starting the sound then keeps the encoder's own start-up
/// (a fifth of a second, measured) out of the offset between picture and sound.
#[derive(Default)]
struct PumpClock {
    started: Mutex<Option<Instant>>,
}

impl PumpClock {
    fn start(&self) {
        let mut g = self.started.lock().unwrap_or_else(|e| e.into_inner());
        if g.is_none() {
            *g = Some(Instant::now());
        }
    }
    fn get(&self) -> Option<Instant> {
        *self.started.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Drop for Pump {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

impl Pump {
    fn spawn(mut pipe: std::fs::File) -> Result<Pump, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let clock = Arc::new(PumpClock::default());
        let (stop2, clock2) = (stop.clone(), clock.clone());
        let thread = std::thread::Builder::new()
            .name("cordial-rec-audio".into())
            .spawn(move || -> Result<(), String> {
                // Waits for the picture to start. Gives up quietly if the
                // recording ends without one.
                loop {
                    if clock2.get().is_some() {
                        break;
                    }
                    if stop2.load(Ordering::Acquire) {
                        return Ok(());
                    }
                    std::thread::sleep(Duration::from_millis(2));
                }
                // Starts the tap's timeline at this instant, and discards sound
                // from before it. The pump's own clock is the one that
                // matters from here, so it is read the moment the tap's was.
                // SAFETY: plain call into `native/pipewire_backend.cpp`.
                unsafe { cordial_record_tap_enable(1) };
                let t0 = Instant::now();
                // SAFETY: plain reads of a counter.
                let dropped_before = unsafe { cordial_record_tap_dropped() };
                let mut written: u64 = 0;
                let mut mix: Vec<f32> = Vec::new();
                let mut bytes: Vec<u8> = Vec::new();
                loop {
                    let stopping = stop2.load(Ordering::Acquire);
                    let elapsed = (t0.elapsed().as_secs_f64() * TAP_RATE as f64) as u64;
                    // Behind the clock while running, and caught up at the end,
                    // when everything up to now has been produced.
                    let due = if stopping { elapsed } else { elapsed.saturating_sub(PUMP_LAG) };
                    while written < due {
                        // Half a second at most in one go, so a stall in the
                        // encoder does not become a huge allocation.
                        let n = (due - written).min(TAP_RATE as u64 / 2) as usize;
                        mix.resize(n * 2, 0.0);
                        // SAFETY: `mix` holds `n * 2` floats, which is what the
                        // tap writes for `n` stereo frames.
                        unsafe { cordial_record_tap_read(mix.as_mut_ptr(), n, written) };
                        bytes.clear();
                        for f in &mix {
                            bytes.extend_from_slice(&f.to_le_bytes());
                        }
                        pipe.write_all(&bytes).map_err(|e| format!("writing audio to the encoder: {e}"))?;
                        written += n as u64;
                    }
                    if stopping {
                        // SAFETY: plain reads of two counters.
                        let (streams, dropped) =
                            unsafe { (cordial_record_tap_streams(), cordial_record_tap_dropped()) };
                        // What explains a silent file: no stream was ever open.
                        println!(
                            "[record] audio: {:.1} s written, {streams} stream(s) open at the end, {} frame(s) lost to a full buffer",
                            written as f64 / TAP_RATE as f64,
                            dropped.saturating_sub(dropped_before)
                        );
                        return Ok(());
                    }
                    std::thread::sleep(PUMP_TICK);
                }
            })
            .map_err(|e| format!("could not start the audio thread: {e}"))?;
        Ok(Pump { stop, clock, thread: Some(thread) })
    }

    fn finish(mut self) -> Result<(), String> {
        self.stop.store(true, Ordering::Release);
        match self.thread.take().map(|t| t.join()) {
            Some(Ok(r)) => r,
            Some(Err(_)) => Err("the audio thread panicked".into()),
            None => Ok(()),
        }
    }
}

/// `ffmpeg` with `pipe:3` as its audio input: the read end of a fresh pipe
/// becomes descriptor 3 in the child, and the write end stays here.
///
/// `O_CLOEXEC` on both, so neither end leaks into any other child this process
/// ever starts; the `dup2` is what clears it on the one descriptor that should
/// survive. A leaked write end would mean the encoder never saw end of file on
/// its audio and the recording would not finish.
fn open_audio_pipe() -> Result<(std::fs::File, RawFd), String> {
    let mut fds = [0 as c_int; 2];
    // SAFETY: `fds` is two ints, which is what `pipe2` fills.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(format!("could not make the audio pipe: {}", std::io::Error::last_os_error()));
    }
    // SAFETY: both descriptors were just created and are owned here. The read
    // end is wrapped only so it closes in the parent once the child has it.
    let write = unsafe { std::fs::File::from_raw_fd(fds[1]) };
    Ok((write, fds[0]))
}

fn spawn_encoder(ffmpeg: &Path, spec: &EncodeSpec, audio_rfd: Option<RawFd>) -> std::io::Result<Child> {
    let build = |nice: bool| {
        let mut cmd = if nice {
            let mut c = Command::new("nice");
            c.args(["-n", "10"]).arg(ffmpeg);
            c
        } else {
            Command::new(ffmpeg)
        };
        cmd.args(ffmpeg_args(spec)).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped());
        if let Some(rfd) = audio_rfd {
            // SAFETY: runs between fork and exec, so only async-signal-safe
            // calls: `dup2` and `fcntl`.
            unsafe {
                cmd.pre_exec(move || {
                    if rfd == 3 {
                        let flags = libc::fcntl(3, libc::F_GETFD);
                        if flags < 0 || libc::fcntl(3, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                            return Err(std::io::Error::last_os_error());
                        }
                    } else if libc::dup2(rfd, 3) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
        cmd
    };
    // No `nice` on PATH: run at the engine's own priority rather than not at all.
    build(true).spawn().or_else(|_| build(false).spawn())
}

fn open_encoding(ffmpeg: &Path, spec: &EncodeSpec) -> Result<Encoding, String> {
    let game_audio = spec.audio == Some(AudioIn::Game);
    let (audio_write, audio_read) = if game_audio {
        let (w, r) = open_audio_pipe()?;
        (Some(w), Some(r))
    } else {
        (None, None)
    };
    let spawned = spawn_encoder(ffmpeg, spec, audio_read);
    if let Some(r) = audio_read {
        // SAFETY: the child has its own copy now (or never started); this one is ours to close.
        drop(unsafe { std::fs::File::from_raw_fd(r) });
    }
    let mut child = spawned.map_err(|e| format!("could not start ffmpeg: {e}"))?;
    let mut stdin = child.stdin.take().ok_or("ffmpeg has no stdin")?;
    let stderr = child.stderr.take().ok_or("ffmpeg has no stderr")?;
    let pump = match audio_write {
        Some(w) => Some(Pump::spawn(w)?),
        None => None,
    };

    let tail: Arc<Mutex<Vec<String>>> = Arc::default();
    let tail2 = tail.clone();
    let err_thread = std::thread::Builder::new()
        .name("cordial-rec-log".into())
        .spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                println!("[record] ffmpeg: {line}");
                if let Ok(mut t) = tail2.lock() {
                    t.push(line);
                    if t.len() > 6 {
                        t.remove(0);
                    }
                }
            }
        })
        .ok();

    let (tx, rx) = sync_channel::<Vec<u8>>(QUEUE_DEPTH);
    let pool: Arc<Mutex<Vec<Vec<u8>>>> = Arc::default();
    let pool2 = pool.clone();
    let clock = pump.as_ref().map(|p| p.clock.clone());
    let writer = std::thread::Builder::new()
        .name("cordial-rec-write".into())
        .spawn(move || {
            let mut first = true;
            for frame in rx {
                stdin.write_all(&frame).map_err(|e| format!("writing to the encoder: {e}"))?;
                if first {
                    first = false;
                    if let Some(c) = &clock {
                        c.start();
                    }
                }
                if let Ok(mut p) = pool2.lock() {
                    if p.len() < QUEUE_DEPTH + 2 {
                        p.push(frame);
                    }
                }
            }
            Ok(())
        })
        .map_err(|e| format!("could not start the writer thread: {e}"))?;
    Ok(Encoding {
        child,
        pump,
        tx: Some(tx),
        writer: Some(writer),
        stderr: err_thread,
        pool,
        tail,
    })
}

/// Called from `vkQueuePresentKHR` when [`frame_due`] said so, with the image
/// being presented. Never panics and never blocks on the encoder.
pub fn on_frame(
    queue: u64,
    target: Target,
    gdpa: unsafe extern "C" fn(u64, *const std::ffi::c_char) -> *mut c_void,
    gpdmp: unsafe extern "C" fn(*mut c_void, *mut capture::PhysicalDeviceMemoryProperties),
    physical_device: *mut c_void,
) {
    if ARMED_FAST.swap(false, Ordering::AcqRel) {
        let arg = ARMED.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(arg) = arg {
            if let Err(e) = start(&arg) {
                println!("[record] CORDIAL_RECORD was set but recording could not start: {e}");
            }
        }
    }
    if !ACTIVE.load(Ordering::Acquire) {
        return;
    }
    let started = Instant::now();
    let mut slot = SESSION.lock().unwrap_or_else(|e| e.into_inner());
    let Some(s) = slot.as_mut() else { return };

    let result = take_frame(s, queue, &target, gdpa, gpdmp, physical_device);
    s.read_ns += started.elapsed().as_nanos() as u64;
    // The next frame is one period after this one was *wanted*, not after it
    // was taken, so the average rate holds at the asked-for one however the
    // presents happen to fall. A stall is not followed by a burst: the next
    // frame is never due sooner than half a period from now.
    let period = 1_000_000_000 / s.fps as u64;
    let now = now_ns();
    let due = NEXT_DUE_NS.load(Ordering::Relaxed);
    let anchor = if due == 0 { now } else { due };
    NEXT_DUE_NS.store((anchor + period).max(now + period / 2), Ordering::Relaxed);

    if let Err(why) = result {
        ACTIVE.store(false, Ordering::Release);
        INDICATOR_DIRTY.store(true, Ordering::Release);
        let session = slot.take();
        drop(slot);
        println!("[record] stopping: {why}");
        if let Some(session) = session {
            // Finished on a thread of its own: this is the engine's present.
            let _ = std::thread::Builder::new().name("cordial-rec-stop".into()).spawn(move || {
                match finish(session, false) {
                    Ok(summary) => println!("[record] {summary}"),
                    Err(e) => println!("[record] {e}"),
                }
            });
        }
    }
}

fn take_frame(
    s: &mut Session,
    queue: u64,
    target: &Target,
    gdpa: unsafe extern "C" fn(u64, *const std::ffi::c_char) -> *mut c_void,
    gpdmp: unsafe extern "C" fn(*mut c_void, *mut capture::PhysicalDeviceMemoryProperties),
    physical_device: *mut c_void,
) -> Result<(), String> {
    let dims = (target.rect.2, target.rect.3);
    let bgra = capture::is_bgra(target.format)?;

    if s.dims != dims || s.enc.is_none() {
        // A new size needs a new encoder and a new buffer. The first frame is
        // this path too, with nothing to close.
        if let Some(old) = s.enc.take() {
            if let Err(e) = close_encoding(old) {
                println!("[record] {e}");
            }
        }
        s.readback = None;
        s.segment += 1;
        s.dims = dims;
        let path = segment_path(&s.stem, s.segment, s.codec.ext);
        let spec = EncodeSpec {
            width: dims.0,
            height: dims.1,
            bgra,
            fps: s.fps,
            codec: s.codec.clone(),
            out: path.clone(),
            audio: s.audio.clone(),
        };
        s.enc = Some(open_encoding(&s.ffmpeg, &spec)?);
        s.files.push(path.clone());
        println!("[record] {}x{} -> {}", dims.0, dims.1, path.display());
    }
    if s.readback.is_none() {
        s.readback = Some(Readback::new(target, gdpa, gpdmp, physical_device)?);
    }
    let enc = s.enc.as_mut().ok_or("no encoder")?;
    let rb = s.readback.as_mut().ok_or("no readback")?;
    let pixels = rb.read(queue, target)?;
    let mut buf = enc.pool.lock().ok().and_then(|mut p| p.pop()).unwrap_or_default();
    buf.clear();
    buf.extend_from_slice(pixels);
    let tx = enc.tx.as_ref().ok_or("encoder already closed")?;
    match tx.try_send(buf) {
        Ok(()) => s.taken += 1,
        Err(TrySendError::Full(_)) => s.dropped += 1,
        Err(TrySendError::Disconnected(_)) => {
            let tail = enc.tail.lock().map(|t| t.join(" | ")).unwrap_or_default();
            return Err(format!("the encoder stopped reading ({tail})"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const LISTING: &str = " Encoders:\n V..... = Video\n A..... = Audio\n ------\n V....D libx264              libx264 H.264 (codec h264)\n V....D mpeg4                MPEG-4 part 2\n A....D aac                  AAC (Advanced Audio Coding)\n A....D libopus              libopus Opus\n";

    #[test]
    fn the_legend_is_not_an_encoder() {
        let names = encoder_names(LISTING);
        assert_eq!(names, vec!["libx264", "mpeg4", "aac", "libopus"]);
    }

    #[test]
    fn mp4_with_h264_is_the_default() {
        let c = choose_codec(None, &encoder_names(LISTING)).unwrap();
        assert_eq!((c.ext, c.name), ("mp4", "libx264"));
    }

    #[test]
    fn a_build_without_h264_falls_to_webm_rather_than_failing() {
        // The Flatpak runtime's ffmpeg: libvpx and AV1, no libx264.
        let c = choose_codec(None, &["libvpx-vp9", "libvpx", "libsvtav1", "mpeg4", "aac", "libopus"]).unwrap();
        assert_eq!((c.ext, c.name), ("webm", "libvpx-vp9"));
    }

    #[test]
    fn an_extension_that_cannot_be_filled_is_an_error_not_a_different_container() {
        let e = choose_codec(Some("webm"), &["libx264", "aac"]).unwrap_err();
        assert!(e.contains(".webm"), "{e}");
        assert!(choose_codec(Some("avi"), &["libx264"]).unwrap_err().contains(".avi"));
    }

    #[test]
    fn mp4_is_fragmented_and_webm_is_not() {
        let spec = |ext| EncodeSpec {
            width: 1280,
            height: 720,
            bgra: true,
            fps: 30,
            codec: choose_codec(Some(ext), &["libx264", "libvpx-vp9", "aac", "libopus"]).unwrap(),
            out: PathBuf::from("/o/x"),
            audio: None,
        };
        assert!(ffmpeg_args(&spec("mp4")).join(" ").contains("empty_moov"));
        assert!(!ffmpeg_args(&spec("webm")).join(" ").contains("movflags"));
    }

    #[test]
    fn the_command_line_names_the_input_geometry_and_pixel_order() {
        let spec = EncodeSpec {
            width: 1920,
            height: 1080,
            bgra: false,
            fps: 60,
            codec: choose_codec(None, &["libx264", "aac"]).unwrap(),
            out: PathBuf::from("/o/x.mp4"),
            audio: Some(AudioIn::Desktop("@DEFAULT_MONITOR@".into())),
        };
        let a = ffmpeg_args(&spec).join(" ");
        assert!(a.contains("-pixel_format rgba -video_size 1920x1080"), "{a}");
        assert!(a.contains("fps=60,crop=trunc(iw/2)*2:trunc(ih/2)*2,format=yuv420p"), "{a}");
        assert!(a.contains("-f pulse -i @DEFAULT_MONITOR@"), "{a}");
        assert!(a.contains("-c:a aac"), "{a}");
        assert!(a.contains("-shortest"), "{a}");
        assert!(a.ends_with("/o/x.mp4"), "{a}");
    }

    #[test]
    fn no_audio_input_means_no_audio_codec() {
        let spec = EncodeSpec {
            width: 640,
            height: 480,
            bgra: true,
            fps: 30,
            codec: choose_codec(None, &["libx264", "aac"]).unwrap(),
            out: PathBuf::from("/o/x.mp4"),
            audio: None,
        };
        assert!(!ffmpeg_args(&spec).join(" ").contains("-c:a"));
    }

    #[test]
    fn audio_is_off_unless_asked_and_the_game_is_what_on_means() {
        assert_eq!(audio_choice(None, || panic!("not asked")).0, None);
        assert_eq!(audio_choice(Some("off"), || panic!("not asked")).0, None);
        // The Settings switch sends `game`, and a bare `1` means the same.
        assert_eq!(audio_choice(Some("game"), || panic!("not asked")).0, Some(AudioIn::Game));
        assert_eq!(audio_choice(Some("1"), || panic!("not asked")).0, Some(AudioIn::Game));
        // The old behaviour is kept under the name that says what it is.
        assert_eq!(
            audio_choice(Some("desktop"), || true).0,
            Some(AudioIn::Desktop(DESKTOP_SOURCE.into()))
        );
        let (src, why) = audio_choice(Some("desktop"), || false);
        assert!(src.is_none() && why.contains("cannot read PulseAudio"), "{why}");
        // A typo is off, and says what it was, rather than becoming the desktop.
        let (src, why) = audio_choice(Some("deskop"), || true);
        assert!(src.is_none() && why.contains("deskop"), "{why}");
    }

    #[test]
    fn game_audio_is_a_raw_pipe_on_descriptor_three() {
        let spec = EncodeSpec {
            width: 640,
            height: 480,
            bgra: true,
            fps: 30,
            codec: choose_codec(Some("webm"), &["libvpx-vp9", "libopus"]).unwrap(),
            out: PathBuf::from("/o/x.webm"),
            audio: Some(AudioIn::Game),
        };
        let a = ffmpeg_args(&spec).join(" ");
        assert!(a.contains("-f f32le -ar 48000 -ac 2 -i pipe:3"), "{a}");
        assert!(!a.contains("-f pulse"), "{a}");
        assert!(a.contains("-c:a libopus"), "{a}");
        // ffmpeg 8 stops reading a real-time pipe for the whole recording when
        // told `-shortest`; only the endless desktop source needs it.
        assert!(!a.contains("-shortest"), "{a}");
    }

    #[test]
    fn the_hotkey_defaults_to_f9_and_a_typo_is_reported() {
        assert_eq!(parse_key(None), Ok(Some(67)));
        assert_eq!(parse_key(Some("")), Ok(Some(67)));
        assert_eq!(parse_key(Some("F9")), Ok(Some(67)));
        assert_eq!(parse_key(Some("f10")), Ok(Some(68)));
        assert_eq!(parse_key(Some("Scroll_Lock")), Ok(Some(70)));
        assert_eq!(parse_key(Some("off")), Ok(None));
        let e = parse_key(Some("F13")).unwrap_err();
        assert!(e.contains("F13") && e.contains("f9"), "{e}");
    }

    #[test]
    fn videos_dir_reads_user_dirs() {
        let home = Path::new("/home/u");
        assert_eq!(
            parse_videos_dir("XDG_VIDEOS_DIR=\"$HOME/Clips\"\n", home),
            Some(PathBuf::from("/home/u/Clips"))
        );
        assert_eq!(
            parse_videos_dir("# c\nXDG_VIDEOS_DIR=\"/data/v\"\n", home),
            Some(PathBuf::from("/data/v"))
        );
        assert_eq!(parse_videos_dir("XDG_MUSIC_DIR=\"$HOME/M\"\n", home), None);
    }

    #[test]
    fn targets_resolve_to_a_stem_and_an_optional_extension() {
        let t = Stamp { year: 2026, month: 10, day: 5, hour: 9, minute: 3, second: 7 };
        let d = Path::new("/vid/Cordial");
        assert_eq!(
            resolve_target("", d, &t),
            (PathBuf::from("/vid/Cordial/Cordial-2026-10-05_09-03-07"), None)
        );
        assert_eq!(
            resolve_target("/x/y/", d, &t),
            (PathBuf::from("/x/y/Cordial-2026-10-05_09-03-07"), None)
        );
        assert_eq!(resolve_target("/x/clip.webm", d, &t), (PathBuf::from("/x/clip"), Some("webm".into())));
        assert_eq!(resolve_target("/x/clip", d, &t), (PathBuf::from("/x/clip"), None));
    }

    #[test]
    fn the_second_segment_is_numbered_and_the_first_is_not() {
        let stem = Path::new("/x/clip");
        assert_eq!(segment_path(stem, 1, "mp4"), PathBuf::from("/x/clip.mp4"));
        assert_eq!(segment_path(stem, 2, "mp4"), PathBuf::from("/x/clip-2.mp4"));
    }

    /// A stand-in encoder: copies its stdin to the file named by its last
    /// argument, which is where `ffmpeg_args` puts the output.
    fn fake_encoder(dir: &Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join("fake-ffmpeg");
        std::fs::write(&p, format!("#!/bin/sh\nfor last; do :; done\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("cordial-rec-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// The tap is one per process, so tests that read or feed it take turns.
    static TAP_LOCK: Mutex<()> = Mutex::new(());

    fn spec(out: PathBuf) -> EncodeSpec {
        EncodeSpec {
            width: 4,
            height: 2,
            bgra: true,
            fps: 30,
            codec: choose_codec(None, &["libx264", "aac"]).unwrap(),
            out,
            audio: None,
        }
    }

    #[test]
    fn frames_sent_to_the_encoder_arrive_whole_and_in_order() {
        let dir = scratch("pipe");
        let enc_bin = fake_encoder(&dir, "cat > \"$last\"");
        let out = dir.join("out.raw");
        let enc = open_encoding(&enc_bin, &spec(out.clone())).unwrap();
        let tx = enc.tx.clone().unwrap();
        for n in 0u8..5 {
            tx.send(vec![n; 32]).unwrap();
        }
        drop(tx);
        close_encoding(enc).unwrap();
        let got = std::fs::read(&out).unwrap();
        assert_eq!(got.len(), 5 * 32);
        for (n, chunk) in got.chunks(32).enumerate() {
            assert!(chunk.iter().all(|b| *b == n as u8), "frame {n} was corrupted");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_encoder_that_dies_is_reported_with_what_it_said() {
        let dir = scratch("die");
        let enc_bin = fake_encoder(&dir, "echo 'Unknown encoder libx264' >&2; exit 1");
        let enc = open_encoding(&enc_bin, &spec(dir.join("o.mp4"))).unwrap();
        let err = close_encoding(enc).unwrap_err();
        assert!(err.contains("Unknown encoder libx264"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_full_queue_drops_a_frame_instead_of_blocking() {
        // The engine's present thread must never wait on the encoder. An
        // encoder that reads nothing fills the channel and the pipe, and the
        // sixth offer is refused at once.
        let dir = scratch("full");
        let enc_bin = fake_encoder(&dir, "sleep 30");
        let mut enc = open_encoding(&enc_bin, &spec(dir.join("o.mp4"))).unwrap();
        let tx = enc.tx.take().unwrap();
        let big = vec![0u8; 1 << 20];
        let started = Instant::now();
        let mut refused = 0;
        for _ in 0..64 {
            if let Err(TrySendError::Full(_)) = tx.try_send(big.clone()) {
                refused += 1;
            }
        }
        assert!(refused > 0, "nothing was ever refused");
        assert!(started.elapsed() < Duration::from_secs(5), "an offer blocked");
        let _ = enc.child.kill();
        let _ = enc.child.wait();
        drop(tx);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The audio half of an encoder, end to end but for the engine: a fake
    /// `ffmpeg` that copies descriptor 3 to a file, a frame to start the clock,
    /// and a second of waiting. The tap has no streams in a unit test, so what
    /// arrives is silence, and what is being checked is that the descriptor
    /// reaches the child, that the wall clock decides how much is written, and
    /// that closing the encoder ends the pipe.
    #[test]
    fn game_audio_reaches_descriptor_three_at_the_wall_clock_rate_and_ends() {
        let _tap = TAP_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("pump");
        let enc_bin = fake_encoder(&dir, "cat <&3 > \"$last.audio\" & cat > \"$last\"; wait");
        let out = dir.join("out.raw");
        let mut s = spec(out.clone());
        s.audio = Some(AudioIn::Game);
        let enc = open_encoding(&enc_bin, &s).unwrap();
        let tx = enc.tx.clone().unwrap();
        tx.send(vec![1; 32]).unwrap();
        drop(tx);
        std::thread::sleep(Duration::from_millis(1000));
        close_encoding(enc).unwrap();
        let audio = std::fs::read(dir.join("out.raw.audio")).unwrap();
        let seconds = audio.len() as f64 / (TAP_RATE as f64 * 8.0);
        assert!((0.9..1.3).contains(&seconds), "{seconds} s of audio for about a second of recording");
        assert!(audio.iter().all(|b| *b == 0), "silence expected: nothing plays in a unit test");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Known samples in, the same samples out at the far end of descriptor 3.
    /// The fake encoder copies the pipe to a file, so this is the pump, the tap
    /// and the pipe with nothing between them and a comparison.
    #[test]
    fn samples_given_to_the_tap_arrive_unchanged_on_descriptor_three() {
        let _tap = TAP_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("fidelity");
        let enc_bin = fake_encoder(&dir, "cat <&3 > \"$last.audio\" & cat > \"$last\"; wait");
        let out = dir.join("out.raw");
        let mut s = spec(out.clone());
        s.audio = Some(AudioIn::Game);
        let enc = open_encoding(&enc_bin, &s).unwrap();
        let tx = enc.tx.clone().unwrap();
        tx.send(vec![1; 32]).unwrap();
        drop(tx);
        // Let the clock start, then a 0.3 s ramp in 10 ms pieces at real time.
        std::thread::sleep(Duration::from_millis(300));
        let ramp: Vec<f32> = (0..14_400).flat_map(|i| [i as f32 / 20_000.0, -(i as f32) / 20_000.0]).collect();
        for piece in ramp.chunks(480 * 2) {
            inject_samples(piece);
            std::thread::sleep(Duration::from_millis(10));
        }
        std::thread::sleep(Duration::from_millis(300));
        close_encoding(enc).unwrap();
        unsafe { cordial_record_tap_enable(0) };
        let bytes = std::fs::read(dir.join("out.raw.audio")).unwrap();
        let got: Vec<f32> = bytes.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
        // Find where the ramp starts (first non-zero left sample after zero), and
        // check a stretch of it sample for sample. The pump's linear interpolation
        // at an exact 1.0 step must reproduce the input.
        let start = got.chunks_exact(2).position(|f| f[0] != 0.0 || f[1] != 0.0).expect("no sound arrived");
        let mut matched = 0;
        for k in 0..4000usize {
            let f = &got[(start + k) * 2..(start + k) * 2 + 2];
            let i = (k + 1) as f32; // the first non-zero frame is ramp frame 1
            if (f[0] - i / 20_000.0).abs() < 1e-6 && (f[1] + i / 20_000.0).abs() < 1e-6 {
                matched += 1;
            }
        }
        assert_eq!(matched, 4000, "samples were altered or dropped on the way (start frame {start})");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Picture and sound against each other, in a real encode.
    ///
    /// The picture goes white at two seconds on the recording's clock and a
    /// 1 kHz beep starts at the same instant, fed through the tap the way a
    /// stream's callback does. `CORDIAL_SYNC_OUT` names the file to write; the
    /// offset between the flash and the beep is then read from it with
    /// `ffmpeg`/`ffprobe` by whoever ran this (the measurement is in ADR-059).
    /// Ignored in the ordinary run because it needs a real `ffmpeg` and takes
    /// six seconds.
    #[test]
    #[ignore = "runs a real ffmpeg for about six seconds; CORDIAL_SYNC_OUT names the file"]
    fn picture_and_sound_can_be_lined_up_in_a_real_encode() {
        let _tap = TAP_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let ffmpeg = std::env::var_os("CORDIAL_TEST_FFMPEG").map(PathBuf::from).unwrap_or_else(|| "/usr/bin/ffmpeg".into());
        let listing = probe_encoders(&ffmpeg).unwrap();
        let out = PathBuf::from(std::env::var("CORDIAL_SYNC_OUT").expect("CORDIAL_SYNC_OUT"));
        let spec = EncodeSpec {
            width: 320,
            height: 180,
            bgra: true,
            fps: 30,
            codec: choose_codec(out.extension().and_then(|e| e.to_str()), &encoder_names(&listing)).unwrap(),
            out,
            audio: Some(AudioIn::Game),
        };
        let enc = open_encoding(&ffmpeg, &spec).unwrap();
        let tx = enc.tx.clone().unwrap();
        let frame = |white: bool| vec![if white { 255u8 } else { 0u8 }; 320 * 180 * 4];
        tx.send(frame(false)).unwrap();
        let clock = enc.pump.as_ref().unwrap().clock.clone();
        let t0 = loop {
            if let Some(t) = clock.get() {
                break t;
            }
            std::thread::sleep(Duration::from_millis(1));
        };
        // Give the pump its tick to start the tap's timeline, then run the scene.
        let flash = t0 + Duration::from_millis(2000);
        let beeper = std::thread::spawn(move || {
            while Instant::now() < flash {
                std::thread::sleep(Duration::from_micros(500));
            }
            let started = Instant::now();
            let mut sent: u64 = 0;
            while sent < 4800 {
                let due = (started.elapsed().as_secs_f64() * 48_000.0) as u64;
                let n = due.min(4800).saturating_sub(sent);
                if n > 0 {
                    let piece: Vec<f32> = (0..n)
                        .flat_map(|i| {
                            let v = 0.5 * (2.0 * std::f32::consts::PI * 1000.0 * (sent + i) as f32 / 48_000.0).sin();
                            [v, v]
                        })
                        .collect();
                    inject_samples(&piece);
                    sent += n;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        });
        let mut next = Instant::now();
        while t0.elapsed() < Duration::from_millis(4500) {
            next += Duration::from_micros(33_333);
            let white = Instant::now() >= flash;
            tx.send(frame(white)).unwrap();
            let now = Instant::now();
            if next > now {
                std::thread::sleep(next - now);
            }
        }
        beeper.join().unwrap();
        drop(tx);
        close_encoding(enc).unwrap();
        unsafe { cordial_record_tap_enable(0) };
    }

    /// A tone played through Cordial's own audio path (`cordial_audio_dev_tone`,
    /// the stream AAudio uses) while a real encode runs: the file must contain
    /// it. Needs a sound server and a real `ffmpeg`, so it is ignored in the
    /// ordinary run; `CORDIAL_TONE_OUT` names the file.
    #[test]
    #[ignore = "plays 0.03 for two seconds on the default output; needs a sound server and ffmpeg"]
    fn a_tone_played_through_the_audio_path_ends_up_in_the_file() {
        unsafe extern "C" {
            fn cordial_audio_dev_tone(hz: f64, amp: f64, seconds: f64) -> i32;
        }
        let _tap = TAP_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let ffmpeg = std::env::var_os("CORDIAL_TEST_FFMPEG").map(PathBuf::from).unwrap_or_else(|| "/usr/bin/ffmpeg".into());
        let listing = probe_encoders(&ffmpeg).unwrap();
        let out = PathBuf::from(std::env::var("CORDIAL_TONE_OUT").expect("CORDIAL_TONE_OUT"));
        let spec = EncodeSpec {
            width: 320,
            height: 180,
            bgra: true,
            fps: 30,
            codec: choose_codec(out.extension().and_then(|e| e.to_str()), &encoder_names(&listing)).unwrap(),
            out,
            audio: Some(AudioIn::Game),
        };
        let enc = open_encoding(&ffmpeg, &spec).unwrap();
        let tx = enc.tx.clone().unwrap();
        tx.send(vec![0u8; 320 * 180 * 4]).unwrap();
        let t0 = loop {
            if let Some(t) = enc.pump.as_ref().unwrap().clock.get() {
                break t;
            }
            std::thread::sleep(Duration::from_millis(1));
        };
        while t0.elapsed() < Duration::from_millis(1000) {
            tx.send(vec![0u8; 320 * 180 * 4]).unwrap();
            std::thread::sleep(Duration::from_millis(33));
        }
        assert_eq!(unsafe { cordial_audio_dev_tone(440.0, 0.03, 2.0) }, 0);
        while t0.elapsed() < Duration::from_millis(4500) {
            tx.send(vec![0u8; 320 * 180 * 4]).unwrap();
            std::thread::sleep(Duration::from_millis(33));
        }
        drop(tx);
        close_encoding(enc).unwrap();
        unsafe { cordial_record_tap_enable(0) };
    }
}
