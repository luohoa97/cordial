//! Says what the launcher was doing when its window did not appear.
//!
//! **Why this exists.** Several reports (#38, #88) describe the launcher
//! starting, using a full core and never mapping a window, on COSMIC, KWin and
//! Mango sessions, with nothing on the terminal. A WAYLAND_DEBUG trace from one
//! of them stops right after GTK binds `xdg_wm_base`, which is where a healthy
//! run goes on to build the launcher and create the toplevel. Five nested
//! compositors here (sway, KWin, weston, cage, Hyprland) all mapped it, so the
//! cause is something about those machines, and the only way to learn what is
//! to make the stall describe itself.
//!
//! What it prints, once, if no window has mapped after [`LIMIT`]: the last
//! step `start()` recorded, whether the main thread was burning CPU or
//! waiting (a spin and a block look the same in a backtrace), and the
//! variables that choose a display backend and renderer. It runs on its own
//! thread precisely because the main thread is the one that is stuck.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// How long the launcher may take to map. A cold start on a slow disk is a few
/// seconds; twenty is a stall.
pub const LIMIT: Duration = Duration::from_secs(20);

/// [`LIMIT`], unless `CORDIAL_STARTUP_WATCH_SECS` says otherwise. The override
/// exists so the report can be provoked in a test session without waiting.
pub fn limit() -> Duration {
    limit_from(std::env::var("CORDIAL_STARTUP_WATCH_SECS").ok().as_deref())
}

fn limit_from(value: Option<&str>) -> Duration {
    value.and_then(|v| v.trim().parse::<u64>().ok()).map(Duration::from_secs).unwrap_or(LIMIT)
}

static MAPPED: AtomicBool = AtomicBool::new(false);
static STAGE: Mutex<&'static str> = Mutex::new("before the shell started");
static ARMED: OnceLock<()> = OnceLock::new();

/// Record the step the shell has reached. Silent: it only matters if the
/// window never appears.
pub fn stage(name: &'static str) {
    *STAGE.lock().unwrap_or_else(|p| p.into_inner()) = name;
}

/// The window has mapped; the watchdog has nothing to say.
pub fn mapped() {
    MAPPED.store(true, Ordering::Relaxed);
}

pub fn is_mapped() -> bool {
    MAPPED.load(Ordering::Relaxed)
}

/// User plus system CPU ticks out of a `/proc/<pid>/task/<tid>/stat` line.
///
/// The command name sits in parentheses and may itself contain spaces or a
/// closing bracket, so the fields are counted from the last `)`.
pub fn cpu_ticks(stat: &str) -> Option<u64> {
    let rest = stat.rsplit_once(')')?.1;
    let fields: Vec<&str> = rest.split_whitespace().collect();
    // After the name the first field is the state (field 3 of the file), so
    // utime (14) and stime (15) are at 11 and 12.
    Some(fields.get(11)?.parse::<u64>().ok()? + fields.get(12)?.parse::<u64>().ok()?)
}

/// The words printed when the window has not appeared. A pure function of what
/// was measured, so the wording is tested rather than discovered in a bug
/// report.
pub fn stalled_report(
    stage: &str,
    waited: Duration,
    busy_percent: Option<u64>,
    env: &[(&str, Option<String>)],
    stack: Option<&[String]>,
) -> String {
    let busy = match busy_percent {
        Some(p) if p >= 50 => format!("The main thread was {p}% busy over the last second, so it is spinning rather than waiting."),
        Some(p) => format!("The main thread was {p}% busy over the last second, so it is waiting on something rather than spinning."),
        None => "The main thread's CPU use could not be read.".to_string(),
    };
    let vars = env
        .iter()
        .map(|(k, v)| format!("{k}={}", v.as_deref().unwrap_or("(unset)")))
        .collect::<Vec<_>>()
        .join(", ");
    // The step names where `start()` was, and the stack names where inside
    // GTK or the driver the main thread actually is. A spin at 99% looks the
    // same from every other angle, and a Flatpak user has no gdb inside the
    // sandbox to ask.
    let stack = match stack {
        Some(frames) if !frames.is_empty() => {
            format!("\nMain thread stack, innermost first:\n{}", frames.iter().map(|f| format!("  {f}")).collect::<Vec<_>>().join("\n"))
        }
        _ => "\nThe main thread's stack could not be read.".to_string(),
    };
    format!(
        "shell: the launcher window has not appeared after {} s. Last step reached: {stage}. {busy} \
         Environment: {vars}. If you are reporting this, include this paragraph. \
         `GSK_RENDERER=cairo cordial` starts GTK without its GPU renderer and is worth one try.{stack}",
        waited.as_secs()
    )
}

/// Deepest stack the capture keeps. The interesting frames are the innermost
/// ones; a GTK main loop is fifteen frames deep before it reaches anything.
const MAX_FRAMES: usize = 32;
static FRAMES: [AtomicUsize; MAX_FRAMES] = [const { AtomicUsize::new(0) }; MAX_FRAMES];
static FRAME_COUNT: AtomicUsize = AtomicUsize::new(0);
static CAPTURED: AtomicBool = AtomicBool::new(false);

/// Runs on the thread being inspected. Writes only into the statics above;
/// `backtrace` is called once from a normal context before the signal is sent,
/// so the lazy load of libgcc it does is not done under a handler.
unsafe extern "C" fn on_capture_signal(_: libc::c_int) {
    let mut buf = [std::ptr::null_mut::<libc::c_void>(); MAX_FRAMES];
    // SAFETY: `buf` holds MAX_FRAMES pointers and MAX_FRAMES is passed as its size.
    let n = unsafe { libc::backtrace(buf.as_mut_ptr(), MAX_FRAMES as libc::c_int) }.max(0) as usize;
    for (slot, frame) in FRAMES.iter().zip(buf.iter()).take(n) {
        slot.store(*frame as usize, Ordering::Relaxed);
    }
    FRAME_COUNT.store(n, Ordering::Relaxed);
    CAPTURED.store(true, Ordering::Release);
}

/// One address as `module+0xoffset (symbol+0xoffset)`. A stripped or static
/// frame has the first part only, which is still enough to look up against the
/// release's debug info.
fn describe_frame(addr: usize) -> String {
    // A return address points after the call; looking up the byte before it
    // keeps a call at the end of a function attributed to that function.
    let probe = addr.saturating_sub(1) as *const libc::c_void;
    // SAFETY: `Dl_info` is plain data; `dladdr` fills it or returns zero.
    let mut info: libc::Dl_info = unsafe { std::mem::zeroed() };
    // SAFETY: `info` is a valid out-pointer and `probe` is only looked up, never read.
    if unsafe { libc::dladdr(probe, &mut info) } == 0 {
        return format!("{addr:#x}");
    }
    let cstr = |p: *const libc::c_char| {
        if p.is_null() {
            None
        } else {
            // SAFETY: dladdr returns NUL-terminated strings owned by the loader.
            Some(unsafe { std::ffi::CStr::from_ptr(p) }.to_string_lossy().into_owned())
        }
    };
    let module = cstr(info.dli_fname).map(|f| f.rsplit('/').next().unwrap_or(&f).to_string()).unwrap_or_else(|| "?".into());
    let base = info.dli_fbase as usize;
    let mut out = format!("{module}+{:#x}", addr.wrapping_sub(base));
    if let Some(sym) = cstr(info.dli_sname) {
        out.push_str(&format!(" ({sym}+{:#x})", addr.wrapping_sub(info.dli_saddr as usize)));
    }
    out
}

/// The user-space stack of another thread of this process, innermost first.
///
/// Sends that thread a signal whose handler records a backtrace, then reads it
/// back. Only ever used after the launcher has failed to map for twenty
/// seconds, so a handler interrupting a syscall that glib retries is not a
/// cost a healthy start pays. `None` when the thread did not answer in a
/// second, which is itself worth knowing and is reported as such.
pub fn thread_stack(tid: i64) -> Option<Vec<String>> {
    // Load libgcc's unwinder here, in a normal context, so the handler does not.
    let mut warm = [std::ptr::null_mut::<libc::c_void>(); 4];
    // SAFETY: four slots, four requested.
    unsafe { libc::backtrace(warm.as_mut_ptr(), 4) };
    CAPTURED.store(false, Ordering::Release);
    // SAFETY: a zeroed `sigaction` with a handler and an empty mask is valid.
    let previous = unsafe {
        let mut act: libc::sigaction = std::mem::zeroed();
        act.sa_sigaction = on_capture_signal as unsafe extern "C" fn(libc::c_int) as usize;
        libc::sigemptyset(&mut act.sa_mask);
        act.sa_flags = libc::SA_RESTART;
        let mut old: libc::sigaction = std::mem::zeroed();
        if libc::sigaction(libc::SIGUSR2, &act, &mut old) != 0 {
            return None;
        }
        old
    };
    // SAFETY: `getpid` cannot fail; `tgkill` with a bad tid returns an error and does nothing.
    let sent = unsafe { libc::syscall(libc::SYS_tgkill, libc::getpid() as i64, tid, libc::SIGUSR2 as i64) } == 0;
    let mut got = false;
    if sent {
        for _ in 0..50 {
            if CAPTURED.load(Ordering::Acquire) {
                got = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    // SAFETY: puts back exactly what `sigaction` returned above.
    unsafe { libc::sigaction(libc::SIGUSR2, &previous, std::ptr::null_mut()) };
    if !got {
        return None;
    }
    let n = FRAME_COUNT.load(Ordering::Relaxed).min(MAX_FRAMES);
    Some((0..n).map(|i| format!("#{i} {}", describe_frame(FRAMES[i].load(Ordering::Relaxed)))).collect())
}

/// Start the watchdog. Call from the main thread, once the window is going to
/// be shown; a second call does nothing.
pub fn arm() {
    ARMED.get_or_init(|| {
        // SAFETY: `gettid` takes no arguments and cannot fail.
        let main_tid = unsafe { libc::syscall(libc::SYS_gettid) } as i64;
        let spawned = std::thread::Builder::new().name("startup-watch".into()).spawn(move || watch(main_tid));
        if let Err(e) = spawned {
            eprintln!("shell: could not start the startup watchdog: {e}");
        }
    });
}

fn ticks_of(tid: i64) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/self/task/{tid}/stat")).ok()?;
    cpu_ticks(&stat)
}

fn watch(main_tid: i64) {
    let step = Duration::from_millis(250);
    let mut waited = Duration::ZERO;
    let limit = limit();
    while waited < limit {
        if is_mapped() {
            return;
        }
        std::thread::sleep(step);
        waited += step;
    }
    // SAFETY: `sysconf` with a valid name has no preconditions.
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) }.max(1) as u64;
    let before = ticks_of(main_tid);
    std::thread::sleep(Duration::from_secs(1));
    if is_mapped() {
        return;
    }
    let busy = match (before, ticks_of(main_tid)) {
        (Some(a), Some(b)) => Some(b.saturating_sub(a) * 100 / hz),
        _ => None,
    };
    let stage = *STAGE.lock().unwrap_or_else(|p| p.into_inner());
    let env: Vec<(&str, Option<String>)> = ["GDK_BACKEND", "GSK_RENDERER", "WAYLAND_DISPLAY", "DISPLAY", "XDG_SESSION_TYPE"]
        .into_iter()
        .map(|k| (k, std::env::var(k).ok()))
        .collect();
    let stack = thread_stack(main_tid);
    eprintln!("{}", stalled_report(stage, waited + Duration::from_secs(1), busy, &env, stack.as_deref()));
}

#[cfg(test)]
mod tests {
    use super::*;

    // A real line, command name with a space and a bracket in it, which is
    // what a naive split on whitespace gets wrong.
    const STAT: &str = "4242 (cordial (shell)) R 1 4242 4242 0 -1 4194560 100 0 0 0 750 250 0 0 20 0 50 0 12345 0 0";

    #[test]
    fn ticks_are_counted_from_the_last_bracket() {
        assert_eq!(cpu_ticks(STAT), Some(1000));
    }

    #[test]
    fn a_truncated_line_is_none_rather_than_a_guess() {
        assert_eq!(cpu_ticks("4242 (x) R 1 2"), None);
        assert_eq!(cpu_ticks("garbage"), None);
    }

    #[test]
    fn the_report_names_the_stage_and_says_spinning_when_busy() {
        let r = stalled_report("building the window", Duration::from_secs(21), Some(99), &[("GDK_BACKEND", None)], None);
        assert!(r.contains("building the window"), "{r}");
        assert!(r.contains("99% busy") && r.contains("spinning"), "{r}");
        assert!(r.contains("GDK_BACKEND=(unset)"), "{r}");
        assert!(r.contains("GSK_RENDERER=cairo"), "{r}");
    }

    #[test]
    fn a_quiet_main_thread_is_called_waiting() {
        let r = stalled_report("x", Duration::from_secs(21), Some(1), &[], None);
        assert!(r.contains("waiting on something"), "{r}");
        assert!(!r.contains("spinning rather"), "{r}");
    }

    #[test]
    fn the_report_lists_the_stack_innermost_first_or_says_it_has_none() {
        let frames = vec!["#0 libvulkan_intel.so+0x1234".to_string(), "#1 libgtk-4.so.1+0x99 (gtk_window_realize+0x4)".to_string()];
        let r = stalled_report("x", Duration::from_secs(21), Some(99), &[], Some(&frames));
        assert!(r.contains("innermost first"), "{r}");
        assert!(r.find("libvulkan_intel").unwrap() < r.find("gtk_window_realize").unwrap(), "{r}");
        let none = stalled_report("x", Duration::from_secs(21), Some(99), &[], None);
        assert!(none.contains("stack could not be read"), "{none}");
    }

    /// The control for the capture itself: a thread parked inside a known
    /// function gives a stack that names that function's module, and the one
    /// asking is not the one inspected.
    #[test]
    fn a_parked_thread_gives_a_stack_through_a_signal() {
        use std::sync::mpsc;
        let (tid_tx, tid_rx) = mpsc::channel();
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let parked = std::thread::spawn(move || {
            // SAFETY: `gettid` takes no arguments and cannot fail.
            tid_tx.send(unsafe { libc::syscall(libc::SYS_gettid) } as i64).unwrap();
            let _ = stop_rx.recv();
        });
        let tid = tid_rx.recv().unwrap();
        std::thread::sleep(Duration::from_millis(100));
        let stack = thread_stack(tid).expect("a parked thread answers the signal");
        drop(stop_tx);
        parked.join().unwrap();
        assert!(stack.len() >= 3, "{stack:?}");
        assert!(stack.iter().any(|f| f.starts_with("#0 ")), "{stack:?}");
        // A tid that does not exist is None, not a hang.
        assert!(thread_stack(i32::MAX as i64).is_none());
    }

    #[test]
    fn the_limit_is_twenty_seconds_unless_overridden() {
        assert_eq!(limit_from(None), LIMIT);
        assert_eq!(limit_from(Some(" 3 ")), Duration::from_secs(3));
        assert_eq!(limit_from(Some("soon")), LIMIT);
    }

    #[test]
    fn stage_is_recorded_and_mapped_silences_it() {
        stage("reading settings");
        assert_eq!(*STAGE.lock().unwrap(), "reading settings");
        mapped();
        assert!(is_mapped());
    }
}
