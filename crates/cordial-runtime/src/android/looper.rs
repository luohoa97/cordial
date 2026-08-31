//! `ALooper_*` — the per-thread event loop the engine polls.
//!
//! `GameActivity`'s native side runs a loop that calls `ALooper_pollOnce` and
//! dispatches whatever comes back: input, lifecycle, and whatever file
//! descriptors the app registered. It is not optional and it cannot be faked —
//! a stub that returns immediately turns the engine's main loop into a busy spin,
//! and one that never returns hangs it.
//!
//! Android's implementation is epoll plus an eventfd for wakeups, which is
//! exactly what is available here.

use std::cell::RefCell;
use std::ffi::{c_int, c_void};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

/// How many times the engine has polled. A game thread that is alive and waiting
/// for work shows up here; one that never started does not.
pub static POLLS: AtomicU64 = AtomicU64::new(0);

/// Set when Cordial itself asked the engine to join a place, so the join
/// watchdog below knows there is something to wait for.
///
/// Only Cordial-initiated joins are watched -- `--join-url`, and the shell's
/// Play button, which passes one. A join the user starts from inside the app
/// shell is invisible here, and claiming to watch it would be worse than not
/// watching it.
pub static JOIN_REQUESTED: AtomicBool = AtomicBool::new(false);

/// Record that a join was asked for. Called once, before the pump starts.
pub fn note_join_requested() {
    JOIN_REQUESTED.store(true, Ordering::Relaxed);
}

/// How long a Cordial-initiated join may take before it is reported as not
/// having completed.
///
/// Sixty seconds because a join is not one operation: a server has to be
/// allocated, the place has to download, and on a slow connection that is
/// legitimately long. A watchdog that cries at fifteen seconds trains people to
/// ignore it, which is worse than not having one. `CORDIAL_JOIN_TIMEOUT`
/// overrides it in seconds for anyone testing the message itself.
fn join_timeout() -> std::time::Duration {
    static SECS: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    std::time::Duration::from_secs(*SECS.get_or_init(|| {
        std::env::var("CORDIAL_JOIN_TIMEOUT")
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|v| *v > 0)
            .unwrap_or(60)
    }))
}

/// How long an "infinite" `ALooper_pollOnce` is actually allowed to sleep.
///
/// Android's contract for a negative timeout is to block until something
/// arrives, and Cordial honoured that literally until 2026-08-23, when two
/// clients were caught frozen at the same time. Both sat at 0.01 cores with
/// their engine looper thread in `epoll_wait(-1)` and their present counts
/// nailed down -- 2198 and 2198 eight seconds apart on one, 1 and 1 on the
/// other. One had rendered for half a minute and one had never finished a
/// frame, which is what a lost wakeup looks like: the race does not care how
/// far in you are. Nothing else in either process was holding a lock, and the
/// main pump was healthily awake at 20 Hz the whole time.
///
/// A ceiling does not fix the race. What it does is convert its consequence
/// from a permanently dead window into one late frame, and make the rate
/// countable via `BLOCK_EXPIRED` instead of leaving it to be noticed by a
/// user. 50 ms matches the cadence `pump` already runs at, so a thread parked
/// here costs 20 wakeups a second and nothing measurable.
const BLOCK_CEILING_MS: c_int = 50;

/// Infinite waits that hit `BLOCK_CEILING_MS` with no event to report.
///
/// **This is not the same as a lost wakeup and must not be read as one.** An
/// idle looper with genuinely nothing to do expires here every 50 ms and is
/// perfectly healthy, so the count climbs steadily on a working client. It is
/// the number to difference against a control, not a fault counter.
pub static BLOCK_EXPIRED: AtomicU64 = AtomicU64::new(0);

/// Who is polling, with what timeout, and what `epoll_wait` hands back.
///
/// A single global count said ten million polls a second and could not say
/// whose they were. Two explanations fit that number and they have opposite
/// owners: the engine asking for a zero timeout in a loop of its own, which is
/// the engine's design and not ours to change, or `epoll_wait` returning
/// instantly because a descriptor is permanently ready and nothing drains it,
/// which would be ours entirely. Only a breakdown by caller, by requested
/// timeout, and by which descriptor came back separates them.
///
/// Per-thread slots, one writer each, so every counter below is a plain load
/// and store rather than a locked read-modify-write. At ten million calls a
/// second on one thread an atomic increment per field would be measuring the
/// instrument rather than the engine.
mod census {
    use super::{c_int, AtomicU64, Ordering};

    /// Enough for every thread the engine has been seen to poll from, with
    /// room to notice if a run ever exceeds it — `overflow` below counts the
    /// calls that found no slot, so a full table reports itself rather than
    /// silently dropping a caller.
    const SLOTS: usize = 24;

    #[repr(align(64))]
    pub struct Slot {
        /// 0 means free. Claimed once, by CAS, by the thread it belongs to.
        pub tid: AtomicU64,
        pub calls: AtomicU64,
        /// Requested timeout, bucketed: negative (block), zero (do not wait),
        /// 1..=9 ms, 10 ms and over.
        pub t_block: AtomicU64,
        pub t_zero: AtomicU64,
        pub t_short: AtomicU64,
        pub t_long: AtomicU64,
        /// `epoll_wait` reported nothing ready — the honest idle answer.
        pub r_empty: AtomicU64,
        pub r_wake: AtomicU64,
        pub r_callback: AtomicU64,
        pub r_ident: AtomicU64,
        /// A descriptor came back ready that no registration claims, so
        /// `pollOnce` returns `POLL_TIMEOUT` without reading it. Level
        /// triggered, so it is ready again immediately: this is the counter
        /// that would prove the spin is ours.
        pub r_unclaimed: AtomicU64,
        pub last_unclaimed_fd: AtomicU64,
        /// Nanoseconds spent inside `epoll_wait`, sampled every 1024th call so
        /// the clock reads do not dominate what they measure.
        pub ns: AtomicU64,
        pub ns_samples: AtomicU64,
        /// What the last report saw, so the pump can print a rate.
        pub prev_calls: AtomicU64,
    }

    impl Slot {
        const fn new() -> Slot {
            const Z: AtomicU64 = AtomicU64::new(0);
            Slot {
                tid: Z,
                calls: Z,
                t_block: Z,
                t_zero: Z,
                t_short: Z,
                t_long: Z,
                r_empty: Z,
                r_wake: Z,
                r_callback: Z,
                r_ident: Z,
                r_unclaimed: Z,
                last_unclaimed_fd: Z,
                ns: Z,
                ns_samples: Z,
                prev_calls: Z,
            }
        }
    }

    #[allow(clippy::declare_interior_mutable_const)]
    const FREE: Slot = Slot::new();
    pub static SLOTS_TABLE: [Slot; SLOTS] = [FREE; SLOTS];
    pub static OVERFLOW: AtomicU64 = AtomicU64::new(0);

    /// One writer per counter, so the read-modify-write does not need to be
    /// atomic — only the visibility does, which `Relaxed` on both halves gives.
    #[inline(always)]
    pub fn bump(c: &AtomicU64) {
        c.store(c.load(Ordering::Relaxed).wrapping_add(1), Ordering::Relaxed);
    }

    extern "C" {
        fn gettid() -> c_int;
    }

    thread_local! {
        /// `usize::MAX` until this thread has looked for a slot.
        static MINE: std::cell::Cell<usize> = const { std::cell::Cell::new(usize::MAX) };
    }

    /// This thread's slot, claiming one on first use. `None` once the table is
    /// full, which `OVERFLOW` then records.
    pub fn slot() -> Option<&'static Slot> {
        let idx = MINE.with(|m| {
            let cached = m.get();
            if cached != usize::MAX {
                return cached;
            }
            // SAFETY: `gettid` takes no arguments and returns the caller's
            // kernel thread id.
            let tid = unsafe { gettid() } as u64;
            for (i, s) in SLOTS_TABLE.iter().enumerate() {
                if s
                    .tid
                    .compare_exchange(0, tid, Ordering::AcqRel, Ordering::Relaxed)
                    .is_ok()
                {
                    m.set(i);
                    return i;
                }
            }
            m.set(SLOTS);
            SLOTS
        });
        SLOTS_TABLE.get(idx)
    }

    /// Whether to account at all. Read once; when off, `pollOnce` pays a
    /// relaxed load and nothing else.
    pub fn on() -> bool {
        static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *ON.get_or_init(|| std::env::var_os("CORDIAL_INSTR").is_some())
    }

    /// One line per polling thread, rates since the previous call.
    pub fn report(dt: f64) -> String {
        let mut out = String::new();
        for s in SLOTS_TABLE.iter() {
            let tid = s.tid.load(Ordering::Relaxed);
            let calls = s.calls.load(Ordering::Relaxed);
            if tid == 0 || calls == 0 {
                continue;
            }
            let prev = s.prev_calls.swap(calls, Ordering::Relaxed);
            let rate = (calls - prev) as f64 / dt;
            let samples = s.ns_samples.load(Ordering::Relaxed).max(1);
            let name = std::fs::read_to_string(format!("/proc/self/task/{tid}/comm"))
                .unwrap_or_default();
            out.push_str(&format!(
                "[instr] poll tid={tid} ({}) {rate:.0}/s timeout[block={} zero={} 1-9={} 10+={}] \
                 ret[empty={} wake={} cb={} ident={} unclaimed={} lastfd={}] epoll_wait~{}ns\n",
                name.trim(),
                s.t_block.load(Ordering::Relaxed),
                s.t_zero.load(Ordering::Relaxed),
                s.t_short.load(Ordering::Relaxed),
                s.t_long.load(Ordering::Relaxed),
                s.r_empty.load(Ordering::Relaxed),
                s.r_wake.load(Ordering::Relaxed),
                s.r_callback.load(Ordering::Relaxed),
                s.r_ident.load(Ordering::Relaxed),
                s.r_unclaimed.load(Ordering::Relaxed),
                s.last_unclaimed_fd.load(Ordering::Relaxed) as i64,
                s.ns.load(Ordering::Relaxed) / samples,
            ));
        }
        let over = OVERFLOW.load(Ordering::Relaxed);
        if over != 0 {
            out.push_str(&format!("[instr] poll census table full; {over} calls unattributed\n"));
        }
        out
    }
}

// Return values from android/looper.h.
pub const POLL_WAKE: c_int = -1;
pub const POLL_CALLBACK: c_int = -2;
pub const POLL_TIMEOUT: c_int = -3;
pub const POLL_ERROR: c_int = -4;

/// How long a zero-timeout poll that found nothing is allowed to answer from
/// memory before going back to the kernel. Microseconds.
///
/// **This exists because the engine busy-polls and it was costing a whole
/// core.** Roblox calls `ALooper_pollOnce(0)` in a tight loop, and answering
/// each one with a real `epoll_wait` meant a syscall per iteration at whatever
/// rate the kernel would sustain. Measured on a live client in Brookhaven --
/// a deliberately light game -- one thread pegged at 99.5% of a core with
/// every stack sample inside `epoll_wait` under `looper_poll_once`, while the
/// GPU sat at 45% and would not even clock up past 967 MHz of its 1.50 GHz
/// boost. The engine was starving the GPU by burning the core that feeds it.
///
/// mocktail sidesteps the same loop by not implementing a looper at all: its
/// `ALooper_pollOnce` returns `POLL_TIMEOUT` immediately, no syscall, no fds
/// (`stubs/libandroid_stub.cc`). Cordial cannot do that -- this looper carries
/// the Wayland connection and the input descriptors, and answering "nothing
/// happened" without looking would drop real events on the floor. So the idea
/// is taken and bounded instead: still look, just not a million times a second.
///
/// 250 microseconds is a thirtieth of a frame at 120 fps, so the worst case an
/// event can sit unnoticed is far below anything a player can perceive, while
/// the syscall ceiling drops to 4000 a second. Any poll that actually finds
/// something clears the damper immediately, so a burst of input is never
/// throttled -- only an idle spin is.
///
/// `CORDIAL_POLL_COALESCE_US=0` turns it off, which is the control this was
/// measured against.
const ZERO_TIMEOUT_COALESCE_US_DEFAULT: u64 = 250;

/// Empty zero-timeout polls in a row before the loop is treated as idle.
///
/// High enough that a normal burst of polling during a frame is never slowed,
/// low enough that an idle spin is caught within a fraction of a millisecond.
const IDLE_SPIN_THRESHOLD: u64 = 64;

/// Do not touch the poll loop until the engine has drawn this many frames.
///
/// The startup freeze (docs/NEXT.md §0) is a race, and the backoff below puts a
/// 250µs sleep into a loop the engine runs millions of times a second while
/// that race is being decided. Twenty-five launches with the backoff on froze
/// nine times; twenty-five with `CORDIAL_POLL_COALESCE_US=0` froze three.
/// Fisher's exact test puts that at p=0.10, the two arms ran one after the
/// other rather than interleaved, and **the freeze then went on happening with
/// this gate in place** -- six of eleven launches, on a build where a frozen
/// client never presents enough frames for the sleep to run at all. So the
/// backoff is very probably not the cause and that comparison was noise.
///
/// The gate stays anyway, because it costs one atomic load per poll and buys
/// the one thing worth having: **the core this saves is burned during play, and
/// the race happens during startup**, so there is no reason for the two to
/// touch. Below this many frames the poll loop behaves exactly as it did before
/// the backoff existed.
///
/// What the backoff does once it is allowed to engage, measured in one session
/// against `CORDIAL_POLL_COALESCE_US=0` as the control:
///
/// ```text
/// gated (default)   3,261 polls/s   1.2% on that thread    9.0% process
/// backoff off   9,670,516 polls/s  99.7% on that thread  108.7% process
/// ```
///
/// with median frame rates of 240 and 237. A whole core, and the frames are the
/// same. `cordial_loopers` is where those poll rates come from.
const BACKOFF_AFTER_PRESENTS: u64 = 120;

#[repr(C)]
struct Timespec {
    tv_sec: i64,
    tv_nsec: i64,
}

extern "C" {
    fn nanosleep(req: *const Timespec, rem: *mut Timespec) -> c_int;
}

fn zero_timeout_coalesce_ns() -> u64 {
    static NS: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *NS.get_or_init(|| {
        std::env::var("CORDIAL_POLL_COALESCE_US")
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .unwrap_or(ZERO_TIMEOUT_COALESCE_US_DEFAULT)
            * 1_000
    })
}

/// Zero-timeout polls answered from memory rather than from the kernel.
pub static POLLS_COALESCED: AtomicU64 = AtomicU64::new(0);


// Event bits.
const EVENT_INPUT: c_int = 1 << 0;
const EVENT_OUTPUT: c_int = 1 << 1;
const EVENT_ERROR: c_int = 1 << 2;
const EVENT_HANGUP: c_int = 1 << 3;

const EPOLLIN: u32 = 0x001;
const EPOLLOUT: u32 = 0x004;
const EPOLLERR: u32 = 0x008;
const EPOLLHUP: u32 = 0x010;
const EPOLL_CTL_ADD: c_int = 1;
const EPOLL_CTL_DEL: c_int = 2;

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct EpollEvent {
    events: u32,
    data: u64,
}

extern "C" {
    fn epoll_create1(flags: c_int) -> c_int;
    fn epoll_ctl(epfd: c_int, op: c_int, fd: c_int, event: *mut EpollEvent) -> c_int;
    fn epoll_wait(epfd: c_int, events: *mut EpollEvent, maxevents: c_int, timeout: c_int)
        -> c_int;
    fn eventfd(initval: u32, flags: c_int) -> c_int;
    fn read(fd: c_int, buf: *mut c_void, count: usize) -> isize;
    fn write(fd: c_int, buf: *const c_void, count: usize) -> isize;
}

/// A registered descriptor. `ident` is what `pollOnce` reports for it; a
/// registration with a callback uses `POLL_CALLBACK` instead and Android runs
/// the callback itself.
struct Registration {
    fd: c_int,
    ident: c_int,
    callback: Option<extern "C" fn(c_int, c_int, *mut c_void) -> c_int>,
    data: *mut c_void,
}

/// Push whatever the watchdog just printed out of the buffer.
///
/// Rust's stdout is line-buffered only when it is a terminal. Every harness in
/// this repository redirects it to a file or a pipe, which makes it block-
/// buffered, and every harness then ends the run with `kill -9` -- which throws
/// the buffer away. A watchdog whose whole purpose is to report the state of a
/// client that is about to be killed has to flush, and this one silently did
/// not: two runs were read as "the recovery never fired" when it may well have.
fn flush_stdout() {
    use std::io::Write;
    let _ = std::io::stdout().flush();
}

/// What to try when the engine stops presenting during startup, installed by
/// whoever holds the library handle. See [`RECOVERY_MAX_PRESENTS`].
pub static STARTUP_RECOVERY: OnceLock<Box<dyn Fn() -> Result<(), String> + Send + Sync>> =
    OnceLock::new();

/// `CORDIAL_STARTUP_RETRY=1`: re-drive the app bridge when the engine stops
/// during startup. **Off, and it must stay off.**
///
/// This is a recorded experiment, not a setting, in the same sense as
/// `CORDIAL_LOOPER_BLOCK`. The reasoning was sound: a healthy run starts the
/// Lua app twice and a frozen one once, so ask the platform's own entry point
/// for the second start. What happens is that `nativeAppBridgeStartLuaAppDM`
/// **never returns** -- observed on the first frozen client it was tried
/// against, with the announcement flushed and no completion line after it -- so
/// the pump thread joins the engine in being stuck and the window stops
/// responding at all, which is strictly worse than the freeze.
///
/// That failure is worth more than the fix would have been. **A call that
/// blocks means the wedged thread is holding something the app bridge needs**,
/// so the freeze is a lock and not a lost message, and the next person should
/// be looking for what the engine's app thread still holds while it sits in
/// `ALooper_pollOnce`. Left in, off, so that reasoning is reproducible in one
/// environment variable rather than rediscovered.
fn startup_retry_enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| matches!(std::env::var("CORDIAL_STARTUP_RETRY").as_deref(), Ok("1") | Ok("on")))
}

/// The most frames a client may have drawn and still be treated as stuck in
/// startup rather than merely idle.
///
/// A frozen client presents between zero and five frames and then nothing, ever
/// -- measured over fifty launches. A client that got past startup is at three
/// hundred within seconds. Sixty is far above the first and far below the
/// second, and the gap matters: **a window the user minimised also stops
/// presenting**, and re-driving the app bridge underneath a client that has
/// been running happily for an hour would be a much worse bug than the one this
/// recovers from. Below sixty frames, no such client exists.
const RECOVERY_MAX_PRESENTS: u64 = 60;

/// Per-looper counters, kept separate from the looper itself so they can be
/// read from another thread.
///
/// **This exists because a frozen client is a thread waiting for a message and
/// there was no way to ask which message.** The engine's own thread sits in
/// `ALooper_pollOnce`, and a backtrace cannot tell an empty looper nobody will
/// ever wake from a busy one between events -- both are `epoll_wait`. The
/// startup freeze (docs/NEXT.md §0) has been argued about for days from
/// backtraces that could not distinguish those two states.
///
/// All atomics, and no borrow of the looper's `RefCell`, because the reader is
/// the development control socket's thread and the writer is the looper's own.
pub struct LooperStats {
    /// The thread this looper was created on. Loopers are per-thread in
    /// Android's contract and in ours, so this names the owner for good.
    pub tid: i64,
    /// Descriptors registered through `ALooper_addFd`, minus removals. **Zero
    /// means nothing but `ALooper_wake` can ever make this poll return**, which
    /// is the reading worth having.
    pub registered: AtomicUsize,
    /// **What is registered, not merely how many.** The count answers "can
    /// anything but a wake return", which is the question that mattered while
    /// the freeze was a mystery. The capture of 2026-08-26 moved it on: an
    /// engine thread was found spinning on a looper with exactly one
    /// registration, and the next question is which descriptor -- so the
    /// census has to name it.
    ///
    /// `fd:ident:cb` per registration, rendered once here rather than held as
    /// a structure, because this is read by a human through a socket and the
    /// alternative is a second lock order between the registry and the census.
    pub registered_detail: Mutex<String>,
    pub polls: AtomicU64,
    /// Polls that came back with something. The gap between this and `polls`
    /// is how idle the looper is; the *time* since the last one is whether it
    /// is stuck.
    pub events: AtomicU64,
    pub wakes: AtomicU64,
    /// Milliseconds since the process's looper clock started, at the last poll
    /// that returned something and at the last wake.
    pub last_event_ms: AtomicU64,
    pub last_wake_ms: AtomicU64,
}

/// Every looper this process has made, so the control socket can report them
/// all. Loopers are leaked for the thread's lifetime, so the references stay
/// valid; only the statistics are shared, never the looper.
pub static LOOPERS: Mutex<Vec<&'static LooperStats>> = Mutex::new(Vec::new());

/// Milliseconds since the first looper was created. Started lazily so a run
/// that never makes one pays nothing.
pub fn clock_ms() -> u64 {
    static START: OnceLock<std::time::Instant> = OnceLock::new();
    START.get_or_init(std::time::Instant::now).elapsed().as_millis() as u64
}

pub struct Looper {
    stats: &'static LooperStats,
    epoll: c_int,
    /// Written to by `ALooper_wake`, so a blocked `pollOnce` returns promptly.
    wake: c_int,
    registrations: RefCell<Vec<Registration>>,
    refs: AtomicUsize,
    /// How many zero-timeout polls in a row have come back with nothing.
    ///
    /// Drives the idle backoff; see [`ZERO_TIMEOUT_COALESCE_US_DEFAULT`].
    /// Atomic rather than a `Cell` because `ALooper_wake` is explicitly a
    /// cross-thread call -- waking a looper from another thread is the entire
    /// point of it -- and that path resets this.
    empty_zero_streak: AtomicU64,
}

thread_local! {
    /// Android's loopers are per-thread and `ALooper_forThread` returns the
    /// calling thread's, so the storage has to be thread-local too. Leaked on
    /// first use: the engine holds the pointer for the thread's lifetime and
    /// there is no point at which returning it would be safe.
    static LOOPER: RefCell<Option<&'static Looper>> = const { RefCell::new(None) };
}

impl Looper {
    fn new() -> Option<&'static Looper> {
        // SAFETY: plain syscall wrappers with no pointer arguments.
        let (epoll, wake) = unsafe { (epoll_create1(0), eventfd(0, 0)) };
        if epoll < 0 || wake < 0 {
            return None;
        }
        // The owning thread, so a `loopers` reading joins onto a backtrace.
        // SAFETY: `gettid` takes no arguments and cannot fail.
        let tid = unsafe {
            extern "C" {
                fn gettid() -> c_int;
            }
            gettid()
        } as i64;
        let stats: &'static LooperStats = Box::leak(Box::new(LooperStats {
            tid,
            registered: AtomicUsize::new(0),
            registered_detail: Mutex::new(String::new()),
            polls: AtomicU64::new(0),
            events: AtomicU64::new(0),
            wakes: AtomicU64::new(0),
            last_event_ms: AtomicU64::new(clock_ms()),
            last_wake_ms: AtomicU64::new(0),
        }));
        LOOPERS.lock().unwrap_or_else(|e| e.into_inner()).push(stats);
        let looper = Box::leak(Box::new(Looper {
            stats,
            epoll,
            wake,
            registrations: RefCell::new(Vec::new()),
            refs: AtomicUsize::new(1),
            empty_zero_streak: AtomicU64::new(0),
        }));

        let mut ev = EpollEvent {
            events: EPOLLIN,
            data: looper.wake as u64,
        };
        // SAFETY: `epoll` and `wake` are open descriptors; `ev` is live.
        unsafe { epoll_ctl(looper.epoll, EPOLL_CTL_ADD, looper.wake, &mut ev) };
        Some(looper)
    }

    fn for_thread() -> Option<&'static Looper> {
        LOOPER.with(|l| *l.borrow())
    }

    fn prepare() -> Option<&'static Looper> {
        LOOPER.with(|slot| {
            let mut slot = slot.borrow_mut();
            if slot.is_none() {
                *slot = Looper::new();
            }
            *slot
        })
    }
}

fn epoll_to_looper_events(events: u32) -> c_int {
    let mut out = 0;
    if events & EPOLLIN != 0 {
        out |= EVENT_INPUT;
    }
    if events & EPOLLOUT != 0 {
        out |= EVENT_OUTPUT;
    }
    if events & EPOLLERR != 0 {
        out |= EVENT_ERROR;
    }
    if events & EPOLLHUP != 0 {
        out |= EVENT_HANGUP;
    }
    out
}

/// Give the calling thread a looper.
///
/// Android's framework prepares one on the UI thread before any application
/// code runs, so `ALooper_forThread` never returns null there. AGDK relies on
/// that: `initializeNativeCode` calls `forThread` and bails out immediately if
/// it gets null, returning a zero handle with nothing logged.
///
/// Cordial has no framework doing this, so the thread that drives the Activity
/// has to prepare its own looper first. `forThread` itself stays faithful —
/// creating on demand there would paper over a real "this thread has no looper"
/// error somewhere else.
pub fn prepare_for_current_thread() -> bool {
    Looper::prepare().is_some()
}

/// Pump this thread's looper, as Android's UI thread does.
///
/// AGDK registers its command and input pipes on the looper belonging to the
/// thread that called `initializeNativeCode`, and expects that thread to keep
/// polling. Sleeping instead means the engine's own messages — including the one
/// that says the window is ready — are queued and never delivered, so it sits
/// with a surface it has not been told about and never draws.
///
/// `game_activity_handle`, when set, is also where host input joins this same
/// loop: every ~50ms iteration — the bounded timeout below — is a chance to
/// drain whatever mouse/keyboard events queued up on the active display
/// backend and deliver them through
/// `onTouchEventNative`/`onKeyDownNative`/`onKeyUpNative`, via
/// `android::pump_input_events`, which dispatches to whichever of `window`
/// (X11) or `wayland` is live — see `android::backend`. That function is
/// non-blocking by construction (see its own doc comment), so folding it into
/// this loop does not change this function's own timing behaviour — it is
/// still bounded by the same 50ms `epoll_wait` timeout either way. `None` (no
/// handle) is the case for callers that never bring AGDK up at all, e.g. the
/// app-bridge-only path driven by `CORDIAL_SKIP_AGDK`.
/// Set from the `SIGTERM`/`SIGINT` handler. The only thing a signal handler is
/// allowed to touch here, and the only thing it needs to: the pump notices
/// within one 50ms iteration and takes the same way out as a closed window.
static SIGNALLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

const SIGINT: c_int = 2;
const SIGTERM: c_int = 15;

extern "C" {
    fn signal(signum: c_int, handler: usize) -> usize;
    #[link_name = "_exit"]
    fn libc_exit(status: c_int) -> !;
}

/// Ask to shut down at the next iteration.
///
/// A second signal does not wait. Teardown drives five calls into the engine
/// and then pumps for half a second, and if any of that hangs — which is a real
/// possibility on a client that is already in trouble, and the reason somebody
/// is sending signals at all — a person pressing Ctrl-C twice expects the
/// process to go, not to be told politely that it is already going. `_exit` is
/// async-signal-safe; nothing else attempted here would be.
extern "C" fn on_terminating_signal(_sig: c_int) {
    if SIGNALLED.swap(true, Ordering::Relaxed) {
        // SAFETY: `_exit` is async-signal-safe by specification, and this is
        // the second signal — the polite path has already been tried.
        unsafe { libc_exit(1) };
    }
}

/// `SIGTERM` and `SIGINT` end the run the same way closing the window does.
///
/// `SIGTERM` is what a plain `kill` sends, what systemd sends, and what the
/// shell sends when it offers to close a client that is holding a profile. It
/// used to kill the process outright, which is survivable — the kernel drops
/// the `flock` on exit however the exit happens — but it also meant the engine
/// never got its shutdown sequence, and Roblox has storage open. Converging on
/// the same teardown as `--run` is what makes a terminated session flush what
/// a timed-out one flushes.
fn install_signal_handlers() {
    // SAFETY: `signal` with a plain `extern "C" fn(c_int)` handler is the
    // oldest interface in C; the handler below touches one atomic and, at
    // worst, `_exit`.
    unsafe {
        signal(SIGTERM, on_terminating_signal as *const () as usize);
        signal(SIGINT, on_terminating_signal as *const () as usize);
    }
}

/// Whether anything has asked this run to end early — a closed window or a
/// terminating signal. `--run` expiring is the third way and is the loop's own
/// condition, so that all three arrive at the same teardown.
fn asked_to_stop() -> Option<&'static str> {
    if SIGNALLED.load(Ordering::Relaxed) {
        return Some("a terminating signal");
    }
    // `CORDIAL_NO_CLOSE_EXIT=1` — the control. With it set, closing the window
    // leaves the process running exactly as it did before any of this existed,
    // so a run that ends can be shown to have ended *because* of the close and
    // not because the timer happened to be short. It is also the reason the
    // signal branch above is not behind the same switch: a control that also
    // disables `kill` is a trap.
    if !no_close_exit() && super::window_closed() {
        return Some("the window closing");
    }
    // Deliberately not behind `no_close_exit`. That switch is the control for
    // "did the *window* closing end this run", and something that asked to
    // quit outright is a different question -- gating it on the same variable
    // would make the control also disable an unrelated feature, which is the
    // trap the comment above notes about the signal branch.
    if QUIT_REQUESTED.load(std::sync::atomic::Ordering::Acquire) {
        return Some("something asking the pump to stop");
    }
    None
}

/// Ask the pump to stop, from anywhere.
///
/// **Backend-agnostic on purpose.** `window_closed` is the Wayland backend's
/// own observation and answers `false` on X11 by design (see
/// `android::mod`'s dispatcher), so hanging close-on-leave off it would make
/// the setting silently do nothing on the diagnostic backend. This flag is the
/// pump's own and works wherever the pump runs.
///
/// Sets a flag rather than exiting. `process::exit` from a caller's thread
/// would drop the engine's cookie jar and storage mid-write; every other way
/// out of this client returns through `stop_reason` and unwinds the same way,
/// and so does this.
pub fn request_quit() {
    QUIT_REQUESTED.store(true, std::sync::atomic::Ordering::Release);
}

static QUIT_REQUESTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn no_close_exit() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("CORDIAL_NO_CLOSE_EXIT").is_some())
}

pub fn pump(duration: std::time::Duration, game_activity_handle: Option<i64>) {
    install_signal_handlers();
    // `--run 0` means no deadline at all: run until the window is closed or a
    // signal arrives. That is what a person playing a game wants — a session
    // should end when they end it, not when a number somebody picked runs out
    // — and it is only safe to offer now that closing the window is a way out.
    // A zero-length run was previously indistinguishable from an instant one,
    // which is not a use anything had.
    let deadline = (!duration.is_zero()).then(|| std::time::Instant::now() + duration);

    // Watch the display connection alongside the engine's own descriptors, so
    // a keypress or a click ends the wait immediately.
    //
    // Without this the loop drained input, then slept in `epoll_wait` for up to
    // 50 ms regardless of what the user did — so an event arriving just after a
    // drain waited out the whole timeout before anything saw it. That is up to
    // 50 ms of latency added to every input, on top of the frame the engine
    // then takes to act on it, and it is pure waiting rather than work.
    //
    // The 50 ms timeout stays, because it is what makes the loop notice
    // `deadline`; it is now the idle period rather than the input period.
    let watching = game_activity_handle.is_some()
        && super::connection_fd().is_some_and(watch_input_fd);

    // TEMPORARY INSTRUMENTATION -- not for commit.
    let instr = std::env::var_os("CORDIAL_INSTR").is_some();
    let start = std::time::Instant::now();
    let mut tick = start;
    let (mut p0, mut q0, mut i0) = (0u64, 0u64, 0u64);
    let mut iters: u64 = 0;
    // `CORDIAL_SCRIPT=60:fullscreen,90:windowed,120:motion-off` -- a timeline of
    // things a human would otherwise have to do by hand, so that one launch
    // covers what would otherwise be several. Fullscreen through
    // `gtk_window_fullscreen` and pointer motion through Cordial's own input
    // path are both allowed; nothing here goes near the compositor.
    let mut script: Vec<(f64, String)> = std::env::var("CORDIAL_SCRIPT")
        .unwrap_or_default()
        .split(',')
        .filter_map(|s| s.split_once(':'))
        .filter_map(|(t, a)| Some((t.trim().parse().ok()?, a.trim().to_string())))
        .collect();
    script.reverse();
    // The focus state last handed to the engine, so a transition can be told
    // from a repeat. Seeded `Some(true)` and not `None` because
    // `cordial_game_activity_start` has already driven
    // `onWindowFocusChangedNative(true)` inline by the time the pump runs --
    // seeding `None` would send a duplicate `true` on the first tick of every
    // launch.
    // Stall detection state; see the block that uses it, below.
    let mut stall_presents: u64 = 0;
    let mut stall_since = std::time::Instant::now();
    let mut stall_reported = false;
    let mut recovery_tried = false;
    let join_watch = JOIN_REQUESTED.load(Ordering::Relaxed);
    let join_started = std::time::Instant::now();
    let mut join_reported = false;
    let mut focus_reported: Option<bool> = Some(true);
    // `focus-off`/`focus-on` -- see the override's own comment at the call site.
    // Latched by the `redraw` script action and consumed in the pump below,
    // where the AGDK handle is in scope. The action cannot call through
    // directly from here for that reason alone.
    let mut redraw_requested = false;
    let mut focus_override: Option<bool> = None;
    // `visible-off`/`visible-on` -- the same override as `focus_override`, for
    // the other half of the policy. See the call site.
    let mut visible_override: Option<bool> = None;
    // Read once here rather than per tick. `CORDIAL_THROTTLE=off` is also the
    // control for every measurement of what this gate saves: it restores the
    // unconditional keepalive in the same binary and the same session.
    let policy = super::input::throttle_policy();
    let mut motion = false;
    // `touch-on`/`touch-off` and `look-on`/`look-off` isolate the two halves
    // `motion-on` drives together, to find which one the idle throttle
    // actually watches: `deliver_mouse`'s AGDK `onTouchEventNative` queue, or
    // `pass_mouse_move`'s `NativeInputInterface.nativePassMouseMove`. Separate
    // from `motion` rather than replacing it, so `motion-on` still means what
    // every existing `CORDIAL_SCRIPT` in this codebase's history already
    // assumes it means.
    let mut motion_touch = false;
    let mut motion_look = false;
    let mut motion_ping = false;
    // Set on `key-on`/`keyrepeat-on`, cleared on the matching `-off` — the
    // down-time of the held key, so the `-off` arm can report the same
    // `down_time_ms` the down event carried rather than inventing one.
    let mut key_down_ms: Option<i64> = None;
    // Whether `keyrepeat-on` is active — see the arm's own comment. Separate
    // from `key_down_ms` because `key-on` needs the down-time held across
    // ticks without resending anything, and this flag is what tells the
    // per-tick block below to resend.
    let mut keyrepeat = false;

    // Subscribe to the engine's `setClipboardText` before the first tick.
    //
    // Here rather than in `load.rs` alongside the cookie and identity wiring
    // because the message bus has to exist first, and by the time this function
    // is reached the app bridge has started. Only when there is an engine to
    // subscribe to: `pump` is also used by the GL probe, which has no bus.
    if game_activity_handle.is_some() {
        super::clipboard::arm();
    }

    while deadline.is_none_or(|d| std::time::Instant::now() < d) {
        if let Some(why) = asked_to_stop() {
            println!("[android] ending the run: {why}");
            break;
        }
        iters += 1;
        if instr {
            let t = start.elapsed().as_secs_f64();
            while script.last().is_some_and(|(at, _)| t >= *at) {
                let (_, action) = script.pop().expect("just peeked");
                eprintln!("[instr] t={t:5.1}s script: {action}");
                match action.as_str() {
                    "fullscreen" => super::backend_set_fullscreen(true),
                    "windowed" => super::backend_set_fullscreen(false),
                    // `minimise`/`unminimise` ask GTK about Cordial's own
                    // window -- the only honest way to make the compositor
                    // change this window's visibility, since every route that
                    // would click the real control injects at the compositor
                    // and lands on the developer's session.
                    "minimise" => super::wayland::instr_set_minimised(true),
                    "unminimise" => super::wayland::instr_set_minimised(false),
                    "visible-off" => visible_override = Some(false),
                    "visible-on" => visible_override = Some(true),
                    "visible-real" => visible_override = None,
                    // `redraw` sends one `onSurfaceRedrawNeededNative`, which
                    // nothing else in this process ever sends at cold start.
                    //
                    // Every other call site for it is a reaction to something
                    // that only happens later -- an X11 Expose, a resize, a
                    // text edit, a clipboard paste (`input.rs:284` and its
                    // callers). A client that is launched and left alone
                    // therefore never sends one at all, and mocktail's attested
                    // bring-up ends with exactly this call before the engine
                    // reaches steady rendering
                    // (`src/legacy/legacy_runtime.cc:31121`, Apache-2.0, read
                    // there and not copied).
                    //
                    // What makes that worth an arm rather than a guess is the
                    // shape of a frozen run measured on 2026-08-27: presents
                    // stop at exactly 1 while the engine spins at 103% CPU on
                    // its own command pipe, nine events delivered and no tenth.
                    // "Drew the first frame and never got permission to draw
                    // the second" is what that looks like from outside, and
                    // this is the call that would grant it.
                    //
                    // As a script action rather than a switch because the
                    // timing is the experiment -- too early and the engine has
                    // not built its renderer, too late and it has already
                    // wedged. `CORDIAL_SCRIPT=2:redraw` and `0.5:redraw` are
                    // different arms, and both are one launch each.
                    "redraw" => redraw_requested = true,
                    "focus-off" => focus_override = Some(false),
                    "focus-on" => focus_override = Some(true),
                    "focus-real" => focus_override = None,
                    "motion-on" => motion = true,
                    "motion-off" => motion = false,
                    "touch-on" => motion_touch = true,
                    "touch-off" => motion_touch = false,
                    "look-on" => motion_look = true,
                    "look-off" => motion_look = false,
                    // `ping-on`/`ping-off` — `pass_mouse_move` driven every
                    // tick at a FIXED position, so after the first call every
                    // delta is (0, 0). Answers whether the idle throttle needs
                    // to see the camera actually move, or just needs the
                    // interface call to keep landing — the difference between
                    // a fix that is invisible (a zero-delta heartbeat while a
                    // key is held) and one that is not (synthesising a real
                    // camera nudge).
                    "ping-on" => motion_ping = true,
                    "ping-off" => motion_ping = false,
                    // `key-on`/`key-off` — a single down/up transition each,
                    // matching a held movement key exactly as Wayland delivers
                    // one today: `keyboard_repeat_info` in `wayland.rs` is a
                    // documented no-op, so a key held between the two script
                    // lines produces exactly one `deliver_key`/`pass_key_event`
                    // call on the way down and one on the way up, with nothing
                    // in between — unlike `motion`, which redrives every tick.
                    // This exists to answer one question `motion` cannot: does
                    // the engine's idle throttle need a live *stream* of input
                    // events, or is it satisfied once and stays satisfied while
                    // a key is simply held down? A W/evdev-17/AKEYCODE-51 key
                    // is used because it is the key `window.rs`/`wayland.rs`
                    // both already map for "walk forward", so this is the
                    // shape a real held-W matches, not a synthetic one.
                    "key-on" => {
                        if let Some(handle) = game_activity_handle {
                            let ms = (t * 1000.0) as i64;
                            super::input::deliver_key(handle, true, 51, 17, 0, 0, 'w' as i32, ms, ms);
                            super::input::pass_key_event(true, 17, 0);
                            key_down_ms = Some(ms);
                        }
                    }
                    "key-off" => {
                        if let Some(handle) = game_activity_handle {
                            let ms = (t * 1000.0) as i64;
                            let down_ms = key_down_ms.take().unwrap_or(ms);
                            super::input::deliver_key(handle, false, 51, 17, 0, 0, 'w' as i32, ms, down_ms);
                            super::input::pass_key_event(false, 17, 0);
                        }
                    }
                    // `keyrepeat-on`/`keyrepeat-off` — throwaway probe, not a
                    // real repeat implementation: while active this resends a
                    // down event on every pump tick (`repeat_count` rising the
                    // way `wl_keyboard`'s own `repeat_info` rate would drive
                    // one), to answer one question before writing real repeat
                    // handling — does *any* live key signal keep the throttle
                    // off, or is the detector specifically watching pointer
                    // events? See `key-on` above for the held-once case this
                    // is contrasted against.
                    "keyrepeat-on" => {
                        key_down_ms = Some((t * 1000.0) as i64);
                        keyrepeat = true;
                    }
                    "keyrepeat-off" => {
                        keyrepeat = false;
                        if let (Some(handle), Some(down_ms)) = (game_activity_handle, key_down_ms.take()) {
                            let ms = (t * 1000.0) as i64;
                            super::input::deliver_key(handle, false, 51, 17, 0, 0, 'w' as i32, ms, down_ms);
                            super::input::pass_key_event(false, 17, 0);
                        }
                    }
                    // The close button, without a button. This is how the
                    // close-to-exit path is tested: `close` at t=10 should end
                    // the process at t=10 whatever `--run` says, and with
                    // `CORDIAL_NO_CLOSE_EXIT=1` set it should not.
                    "close" => super::backend_close_window(),
                    // `click:640x382` and `type:hello` — a click and a
                    // keystroke through Cordial's own input path, which is what
                    // makes the text-entry experiments runnable at all. Every
                    // previous attempt at them stalled on "this needs a
                    // keystroke and no Wayland-safe automation here can supply
                    // one"; nothing about that rule was ever about Cordial's
                    // own window, which is not a window anything has to be
                    // injected into. See `input::script_click`.
                    //
                    // `x` separates the two coordinates rather than a comma
                    // because a comma already separates entries in the
                    // timeline, and `25:click:640,382` silently became a click
                    // at (640, 0) — the engine drew its cursor at the top of
                    // the window, which is how it was noticed.
                    _ if action.starts_with("click:") => {
                        if let Some(handle) = game_activity_handle {
                            let mut it = action["click:".len()..].split(&[',', 'x'][..]);
                            let x: f32 = it.next().and_then(|v| v.trim().parse().ok()).unwrap_or(0.0);
                            let y: f32 = it.next().and_then(|v| v.trim().parse().ok()).unwrap_or(0.0);
                            super::input::script_click(handle, x, y, (t * 1000.0) as i64);
                        }
                    }
                    _ if action.starts_with("type:") => {
                        if let Some(handle) = game_activity_handle {
                            let text = &action["type:".len()..];
                            let n = super::input::script_type(handle, text, (t * 1000.0) as i64);
                            eprintln!("[instr] typed {n}/{} characters into a focused box", text.chars().count());
                        }
                    }
                    // `paste` and `copy:some text` — the two halves of the
                    // clipboard bridge, driven without a compositor and without
                    // an account. `paste` is what Ctrl+V will call once a key
                    // handler is bound to it; `copy:` publishes a
                    // `setClipboardText` on the engine's own message bus, which
                    // is the message a copy inside an experience sends. See
                    // `super::clipboard`, and note what `publish_probe` does not
                    // establish.
                    "paste" => {
                        if let Some(handle) = game_activity_handle {
                            match super::clipboard::paste_into_engine(handle) {
                                Ok(0) => eprintln!("[instr] paste: no box has focus"),
                                Ok(n) => eprintln!("[instr] pasted {n} characters into a focused box"),
                                Err(e) => eprintln!("[instr] paste failed: {e}"),
                            }
                        }
                    }
                    _ if action.starts_with("copy:") => {
                        match super::clipboard::publish_probe(&action["copy:".len()..]) {
                            Ok(()) => eprintln!("[instr] published a setClipboardText on the bus"),
                            Err(e) => eprintln!("[instr] publishing setClipboardText failed: {e}"),
                        }
                    }
                    other => eprintln!("[instr] unknown script action {other}"),
                }
            }
            if motion {
                // Wiggle the pointer inside the canvas through Cordial's own
                // input path. No compositor is involved, so nothing can reach
                // the developer's own session -- see docs/NEXT.md's rule.
                if let Some(handle) = game_activity_handle {
                    let (x, y) = (640.0 + 100.0 * (t as f32).sin(), 360.0 + 100.0 * (t as f32).cos());
                    let ms = (t * 1000.0) as i64;
                    super::input::deliver_mouse(
                        handle,
                        super::input::ACTION_HOVER_MOVE,
                        x,
                        y,
                        0,
                        0,
                        ms,
                        0,
                    );
                    super::input::pass_mouse_move(x, y);
                }
            }
            if motion_touch {
                if let Some(handle) = game_activity_handle {
                    let (x, y) = (640.0 + 100.0 * (t as f32).sin(), 360.0 + 100.0 * (t as f32).cos());
                    let ms = (t * 1000.0) as i64;
                    super::input::deliver_mouse(
                        handle,
                        super::input::ACTION_HOVER_MOVE,
                        x,
                        y,
                        0,
                        0,
                        ms,
                        0,
                    );
                }
            }
            if motion_look {
                if game_activity_handle.is_some() {
                    let (x, y) = (640.0 + 100.0 * (t as f32).sin(), 360.0 + 100.0 * (t as f32).cos());
                    super::input::pass_mouse_move(x, y);
                }
            }
            if motion_ping {
                // Fixed position -- see `ping-on`'s own comment. Deliberately
                // not `pass_mouse_move_delta(x, y, 0.0, 0.0)`, which would ALSO
                // move `MOUSE_LAST`'s idea of where the pointer sits away from
                // wherever the real pointer last was, and desync the very next
                // real move's delta. Calling `pass_mouse_move` with the same
                // position keeps `MOUSE_LAST` honestly at that position too.
                if game_activity_handle.is_some() {
                    super::input::pass_mouse_move(640.0, 360.0);
                }
            }
            if keyrepeat {
                // A down event on every tick, `repeat_count` rising with it —
                // the shape `wl_keyboard.repeat_info`'s rate would produce, not
                // one this file claims is the real cadence. See
                // `keyrepeat-on`'s own comment for what this is testing.
                if let (Some(handle), Some(down_ms)) = (game_activity_handle, key_down_ms) {
                    let ms = (t * 1000.0) as i64;
                    let repeat = ((ms - down_ms) / 33).max(0) as i32;
                    super::input::deliver_key(handle, true, 51, 17, 0, repeat, 'w' as i32, ms, down_ms);
                    super::input::pass_key_event(true, 17, 0);
                }
            }
        }
        // Catch the engine going quiet, and say everything about the moment it
        // did -- once, unconditionally, whether or not anything is tracing.
        //
        // This exists because the freeze it watches for reproduces on one
        // machine and roughly one launch in twenty on another. Twenty cold and
        // warm runs here produced one, which is not enough to bisect against
        // and not enough to test a fix with; the state at the moment it stops
        // is what would settle it, and by the time anybody looks the run is
        // usually over. So the client reports it itself.
        //
        // Deliberately not gated on `CORDIAL_INSTR`: the whole difficulty is
        // that this happens when nobody was measuring. It is one line, at most
        // once per run, and only when presents have genuinely stopped.
        //
        // "Stopped" means no present at all for five seconds *while the pump is
        // still running*. It is not the idle throttle, which holds a steady
        // 1.0/s and therefore keeps this counter moving -- distinguishing those
        // two is the entire point, and reading 1.0/s as a freeze has already
        // wasted a day here.
        {
            let now = super::glcount::QUEUE_PRESENT.load(Ordering::Relaxed);
            if now != stall_presents {
                stall_presents = now;
                stall_since = std::time::Instant::now();
            }
            // **Ask the app bridge to start the Lua app again.**
            //
            // The startup freeze (docs/NEXT.md §0) leaves the engine's own
            // thread parked in `ALooper_pollOnce` with nothing to do, having
            // logged `Lua app running status has been updated to true` and
            // stopped. A healthy run starts the Lua app *twice* -- the second
            // time after the experience coordinator is destructed -- and a
            // frozen one only once; that is 25 for 25 across two surveys, and
            // it is visible from outside as `app ready: PlatformAccountRouter`
            // appearing once instead of twice. So the missing step is the
            // second `initializeLuaAppWithLoggedInUser`, and the platform's own
            // way of asking for it is `nativeAppBridgeStartLuaAppDM`, which
            // Cordial already calls once during bring-up.
            //
            // **This is a recovery, not a cause.** Nothing here knows why the
            // engine did not do it itself; Cordial's own behaviour is
            // byte-identical between a frozen run and a healthy one, checked
            // line by line across fifty logs. It is written as a retry of a
            // call this process already makes, gated so tightly that no client
            // which ever drew a frame properly can reach it, and it says so in
            // the log when it fires -- so a run that needed it is never
            // mistaken for one that did not.
            if std::env::var_os("CORDIAL_RECOVERY_DEBUG").is_some()
                && stall_since.elapsed().as_millis() % 5000 < 40
            {
                println!(
                    "[recovery-debug] presents={now} tried={recovery_tried} armed={} stalled={:.1}s",
                    STARTUP_RECOVERY.get().is_some(),
                    stall_since.elapsed().as_secs_f64()
                );
                flush_stdout();
            }
            if startup_retry_enabled()
                && !recovery_tried
                && now <= RECOVERY_MAX_PRESENTS
                && stall_since.elapsed() >= std::time::Duration::from_secs(6)
            {
                recovery_tried = true;
                if let Some(retry) = STARTUP_RECOVERY.get() {
                    println!(
                        "[android] the engine has drawn {now} frames and stopped; \
                         CORDIAL_STARTUP_RETRY is on, so asking the app bridge to start the Lua \
                         app again. Expect this to be the last line: the call does not return."
                    );
                    // **Before the call, not after.** If the retry itself never
                    // returns -- it goes into an engine that has already stopped
                    // making progress -- then the announcement is the only
                    // evidence there will be, and a buffered one dies with the
                    // process.
                    flush_stdout();
                    match retry() {
                        Ok(()) => println!("[android] app bridge retried"),
                        Err(e) => println!("[android] app bridge retry failed: {e}"),
                    }
                } else {
                    println!(
                        "[android] the engine stopped during startup and no recovery is armed; \
                         nothing to retry."
                    );
                }
                flush_stdout();
            }
            if now > 0
                && !stall_reported
                && stall_since.elapsed() >= std::time::Duration::from_secs(5)
            {
                stall_reported = true;
                println!(
                    "[android] the engine has presented nothing for {:.0}s after {now} frames; {}. The pump is still running, so this is not the idle throttle. Take a backtrace -- `just mcp` then cordial_backtrace, or `lldb -p {} -b -o 'thread backtrace all -c 16'`.",
                    stall_since.elapsed().as_secs_f64(),
                    super::backend_instr_geometry(),
                    std::process::id(),
                );
                flush_stdout();
            }
        }
        // The join watchdog. A join Cordial started and the engine never
        // completed is otherwise invisible: the pump keeps running, the window
        // keeps presenting whatever it was already showing, and the user is
        // left watching a screen that never changes. The present watchdog above
        // says nothing about it, because presents are healthy throughout.
        //
        // **It reports that the join has not completed, not that it failed**,
        // and the difference is not pedantry. Sixty seconds on a slow
        // connection is a join still in progress, and a line saying "failed"
        // would be a claim this code cannot support -- the same shape of lie as
        // a stub returning success. Whoever reads it can tell the difference;
        // this can only report what it waited for.
        if join_watch
            && !join_reported
            && cordial_linker_sys::game_activity::games_loaded() == 0
            && join_started.elapsed() >= join_timeout()
        {
            join_reported = true;
            println!(
                "[android] a join was requested {:.0}s ago and the engine has not reported a \
                 loaded place. It may still be connecting -- this is a timeout, not a failure. \
                 If the window is showing the app shell rather than a place, the join did not \
                 take. Set CORDIAL_JOIN_TIMEOUT to change how long this waits.",
                join_started.elapsed().as_secs_f64(),
            );
        }
        if instr && tick.elapsed() >= std::time::Duration::from_secs(1) {
            let dt = tick.elapsed().as_secs_f64();
            tick = std::time::Instant::now();
            let p = super::glcount::QUEUE_PRESENT.load(Ordering::Relaxed);
            let q = POLLS.load(Ordering::Relaxed);
            eprintln!(
                "[instr] t={:5.1}s presents/s={:6.1} looperpolls/s={:9.0} pumps/s={:6.0} {}",
                start.elapsed().as_secs_f64(),
                (p - p0) as f64 / dt,
                (q - q0) as f64 / dt,
                (iters - i0) as f64 / dt,
                super::backend_instr_geometry(),
            );
            eprintln!(
                "[instr] t={:5.1}s focus={:?} visible={:?} toplevel={} policy={:?}",
                start.elapsed().as_secs_f64(),
                super::backend_focused(),
                super::backend_visible(),
                super::wayland::instr_toplevel_state(),
                policy,
            );
            // Who is doing the polling the line above only counts. See
            // `census`: the global rate cannot tell the engine's own zero-timeout
            // loop from an undrained descriptor of ours.
            eprint!("{}", census::report(dt));
            p0 = p;
            q0 = q;
            i0 = iters;
        }
        if let Some(handle) = game_activity_handle {
            super::pump_input_events(handle);
            // Whatever the development control socket has queued, applied on
            // this thread for the same reason `CORDIAL_SCRIPT`'s actions are:
            // the engine's input natives have only ever been called from the
            // pump, and a socket handler is not the place to find out whether
            // they mind. A no-op unless `CORDIAL_DEV_CONTROL` was set.
            crate::devctl::apply_queued(handle);
            // Tell the engine when the user has switched away, and when they
            // have come back.
            //
            // `onWindowFocusChangedNative` used to reach the engine exactly
            // twice: `true` inline in `cordial_game_activity_start`, and
            // `false` from `teardown` below. Nothing in between, so Roblox
            // spent the whole session believing it was the focused window and
            // kept simulating and rendering at full rate behind whatever the
            // user had switched to -- the "takes away your resources from your
            // other programs when you are unfocused" report.
            //
            // Only on a genuine transition. `window_focus` crosses into the
            // engine through JNI and Android itself only sends this on a
            // change; driving it every tick would be a call the engine has
            // never seen at that rate, which is the shape of change this
            // codebase's history says to avoid making casually.
            //
            // `None` from the backend means "not known" and leaves the last
            // reported state alone -- see `backend_focused`.
            // `CORDIAL_SCRIPT=...,60:focus-off,120:focus-on` overrides what
            // the backend reports, so the two states can be measured against
            // each other inside one run without going near the compositor --
            // AGENTS.md forbids synthesising input at one, and alt-tabbing by
            // hand mid-measurement is not a control. It overrides the *report*
            // and nothing else, so what the engine is told is the same call it
            // would get from a real switch away.
            if redraw_requested {
                redraw_requested = false;
                super::input::deliver_surface_redraw(handle);
                if instr {
                    eprintln!(
                        "[instr] t={:5.1}s onSurfaceRedrawNeededNative sent",
                        start.elapsed().as_secs_f64()
                    );
                }
            }

            let observed = match focus_override {
                Some(f) => Some(f),
                None => super::backend_focused(),
            };
            if let Some(now) = observed {
                if focus_reported != Some(now) {
                    if let Err(e) = cordial_linker_sys::game_activity::window_focus(handle, now) {
                        eprintln!("[android] onWindowFocusChangedNative({now}) failed: {e}");
                    }
                    if instr {
                        eprintln!(
                            "[instr] t={:5.1}s focus -> {now}",
                            start.elapsed().as_secs_f64()
                        );
                    }
                    focus_reported = Some(now);
                }
            }
            // See `input::idle_keepalive`'s own comment: the engine's idle
            // throttle answers to `nativePassMouseMove` landing, not to a key
            // being held, so a player walking without touching the mouse gets
            // throttled mid-play unless something keeps sending that call.
            //
            // Gated, because the background is precisely where the engine's
            // own throttle is wanted. Without a gate this defeats it
            // unconditionally, so a key still held at the moment the user
            // switched away kept the engine at full rate behind them for as
            // long as it stayed held -- and `wl_keyboard.leave` delivers no
            // key-up, so a key held across a focus change stays held in
            // `KEYS_HELD` for the rest of the run.
            //
            // What counts as "the background" is the user's to choose and
            // defaults to "not visible" rather than "not focused"; see
            // `input::ThrottleWhen`.
            let visible = match visible_override {
                Some(v) => Some(v),
                None => super::backend_visible(),
            };
            if super::input::keepalive_wanted(policy, observed, visible) {
                super::input::idle_keepalive();
            }
            // Gamepads, from the host's joydev nodes. **On unless
            // `CORDIAL_GAMEPAD=0`**, and a single `OnceLock` read when it is
            // off. This comment said "off unless `CORDIAL_GAMEPAD=1`" for one
            // commit after the default flipped; `android::gamepad`'s module
            // comment carries why it flipped and what is still unverified.
            //
            // Not inside the `keepalive_wanted` branch above it: that gate is
            // about whether Cordial should defeat the engine's idle throttle,
            // which is a separate question from whether a pad's input should
            // reach the engine at all. Backgrounding the window should slow the
            // game down, not drop a button press.
            super::gamepad::poll();
        }
        looper_poll_once(
            if watching { 50 } else { 8 },
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );
        // The engine's cookie jar is memory-only, so somebody has to write it
        // down. Driven from here rather than from the engine's own `Set-Cookie`
        // callback because that callback arrives on the engine's HTTP thread
        // and reading the jar back from inside it would re-enter the engine on
        // its own thread. Cheap when nothing has changed: one relaxed load.
        crate::cookies::flush_if_dirty();

        // What the engine's own log says it is doing. Here rather than in the
        // input branch above for the same reason as the cookie flush: it is
        // housekeeping that must keep happening while the window is in the
        // background, and it is cheap when nothing has changed -- one
        // `read_dir` a second, and between those a metadata read that usually
        // finds the file no longer than it was.
        crate::game_log::poll();

        // A deep link waiting for the app shell to exist, here for the same
        // reason and on the same thread: `APP_READY` arrives on the engine's
        // own thread, and publishing back into the engine from inside its own
        // callback would re-enter it. One acquire load when no link is waiting,
        // which is every ordinary launch.
        crate::deeplink::tick();

        // Text the engine asked to have put on the clipboard, for the third
        // time the same reason: the engine publishes on whichever thread the
        // copy happened on, and GDK may only be touched from the thread that
        // ran `gtk_init`, which is this one.
        super::clipboard::pump_pending();
    }

    // Clean teardown, however the run ended — the timer expiring, the window
    // closing, or a terminating signal. The three converge here on purpose:
    // there is one shutdown sequence and three ways to reach it, rather than a
    // tidy path for the timer and an abrupt one for everything a person
    // actually does. Cordial previously just fell through
    // to `main`'s `_exit(0)` here, which is indistinguishable from the
    // process being killed mid-frame as far as the engine is concerned — it
    // never got a chance to flush the flag cache and telemetry it writes to
    // disk on the way through this chain.
    //
    // The last cookie flush goes *before* that descent, not after: the jar
    // lives in the engine, and after `terminateNativeCode` there is nothing
    // left to read it out of. Unconditional rather than dirty-gated, because
    // the engine only notifies on `Set-Cookie` and a session that was restored
    // at startup and never changed would otherwise not be written back.
    crate::cookies::flush("teardown");
    if let Some(handle) = game_activity_handle {
        teardown(handle);
    }
}

/// The ordered names driving `teardown`, pulled out as a constant so the
/// sequence itself — not just that *something* runs — is checkable by a test
/// without a live `GameActivity` handle. `onWindowFocusChangedNative` is not
/// in this list: it takes a `bool` the other four don't, so it is driven by
/// its own dedicated call (`game_activity::window_focus`) rather than
/// `game_activity::lifecycle`'s by-name lookup.
const TEARDOWN_LIFECYCLE_SEQUENCE: [&str; 4] =
    ["onPauseNative", "onSurfaceDestroyedNative", "onStopNative", "terminateNativeCode"];

/// Android's own shutdown order: `onWindowFocusChangedNative(false)` ->
/// `onPauseNative` -> `onSurfaceDestroyedNative` -> `onStopNative` ->
/// `terminateNativeCode`. Driven synchronously and back-to-back, the same way
/// `cordial_game_activity_start` drives the mirror-image bring-up sequence
/// with no pumping in between.
///
/// `terminateNativeCode` is not exported like `initializeNativeCode` — it is
/// one of the 24 natives AGDK registers dynamically during
/// `initializeNativeCode`, looked up by name exactly like
/// `onPauseNative`/`onStopNative`/`onSurfaceDestroyedNative` (see
/// `game_activity.cpp`'s own doc comment on `cordial_game_activity_lifecycle`
/// for how that was established — `nm -D` on the shipping `libroblox.so`
/// exports only `initializeNativeCode` by that naming scheme).
fn teardown(handle: i64) {
    use cordial_linker_sys::game_activity;

    fn step(name: &str, result: Result<Option<()>, String>) {
        match result {
            Ok(Some(())) => super::trace(format_args!("{name}")),
            // Not registered — a native that never resolved is worth a
            // trace line during teardown even with tracing off elsewhere,
            // since it is the difference between "the engine did not flush"
            // and "Cordial never asked it to".
            Ok(None) => eprintln!("[android] {name}: not registered"),
            Err(e) => eprintln!("[android] {name} failed: {e}"),
        }
    }

    step("onWindowFocusChangedNative(false)", game_activity::window_focus(handle, false));
    for name in TEARDOWN_LIFECYCLE_SEQUENCE {
        step(name, game_activity::lifecycle(handle, name));
    }

    // A brief grace period. The engine's flag-cache/telemetry writes this
    // chain triggers are not guaranteed to be finished by the time
    // `terminateNativeCode` returns to this thread — at least some of that
    // work is plausibly posted to another thread — and pumping a little
    // longer here is what separates a clean write from a log that just stops
    // mid-sentence when the process exits immediately after making these
    // calls. Bounded, not indefinite: teardown must not hang the process it
    // is trying to end cleanly.
    let grace = std::time::Instant::now() + std::time::Duration::from_millis(500);
    while std::time::Instant::now() < grace {
        looper_poll_once(50, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut());
    }
}

/// Add a descriptor to the calling thread's looper so `pollOnce` returns as soon
/// as it is readable.
///
/// Returns false when there is no looper on this thread or the descriptor
/// cannot be registered, in which case the caller should fall back to polling
/// more often rather than assuming it will be woken.
fn watch_input_fd(fd: c_int) -> bool {
    let Some(l) = Looper::for_thread() else {
        return false;
    };
    let mut ev = EpollEvent { events: EPOLLIN, data: fd as u64 };
    // SAFETY: `l.epoll` is this looper's epoll descriptor and `ev` is live for
    // the call. Re-registering an already-watched fd fails harmlessly.
    let rc = unsafe { epoll_ctl(l.epoll, EPOLL_CTL_ADD, fd, &mut ev) };
    if rc != 0 {
        return false;
    }
    // Registered as well as watched, and the reason is the instrument rather
    // than the behaviour. Adding the descriptor to epoll alone left `pollOnce`
    // unable to find a registration for it, so it counted the return as
    // `unclaimed` -- 829 of them in a 60 s run, naming fd 26. `unclaimed` is
    // the census's name for the expensive bug it exists to catch: a descriptor
    // that keeps reporting ready and that nothing ever drains, which turns any
    // zero-timeout caller into a spin. This descriptor is not that. It is the
    // display connection, and GTK drains it inside `pump_input_events` one call
    // later in the same loop iteration.
    //
    // A false positive in the one instrument built to catch real blackholes
    // costs more than it saves, and this one already cost a session's worth of
    // suspicion. Claiming the registration makes the count mean what it says.
    {
        let mut regs = l.registrations.borrow_mut();
        regs.push(Registration {
            fd,
            ident: IDENT_DISPLAY_CONNECTION,
            callback: None,
            data: std::ptr::null_mut(),
        });
        l.stats.registered.store(regs.len(), Ordering::Relaxed);
        *l.stats.registered_detail.lock().unwrap_or_else(|e| e.into_inner()) = regs
            .iter()
            .map(|r| {
                format!("{}:{}:{}", r.fd, r.ident, if r.callback.is_some() { "cb" } else { "-" })
            })
            .collect::<Vec<_>>()
            .join(",");
    }
    true
}

/// The ident `pollOnce` reports for the display connection registered by
/// [`watch_input_fd`]. Distinctive rather than meaningful: nothing reads it.
/// The pump calls `pollOnce` with three null out-parameters and ignores the
/// return, and the engine's own thread has its own looper and its own epoll, so
/// this ident is never visible to Roblox.
const IDENT_DISPLAY_CONNECTION: c_int = 0x436f_7264;

// ------------------------------------------------------------------- the API

extern "C" fn looper_prepare(_opts: c_int) -> *mut c_void {
    super::trace(format_args!("ALooper_prepare"));
    Looper::prepare().map_or(std::ptr::null_mut(), |l| l as *const Looper as *mut c_void)
}

extern "C" fn looper_for_thread() -> *mut c_void {
    super::trace(format_args!("ALooper_forThread"));
    Looper::for_thread().map_or(std::ptr::null_mut(), |l| l as *const Looper as *mut c_void)
}

fn as_looper(p: *mut c_void) -> Option<&'static Looper> {
    // SAFETY: every pointer handed out came from a leaked Box that is never
    // freed, so a non-null one is always live.
    (!p.is_null()).then(|| unsafe { &*(p as *const Looper) })
}

extern "C" fn looper_acquire(looper: *mut c_void) {
    if let Some(l) = as_looper(looper) {
        l.refs.fetch_add(1, Ordering::Relaxed);
    }
}

extern "C" fn looper_release(looper: *mut c_void) {
    // The count is tracked but never acted on: the looper is thread-local and
    // leaked, so dropping to zero would mean freeing something the thread may
    // still poll. Android's own loopers outlive their refcount reaching zero in
    // the same way.
    if let Some(l) = as_looper(looper) {
        l.refs.fetch_sub(1, Ordering::Relaxed);
    }
}

extern "C" fn looper_add_fd(
    looper: *mut c_void,
    fd: c_int,
    ident: c_int,
    events: c_int,
    callback: Option<extern "C" fn(c_int, c_int, *mut c_void) -> c_int>,
    data: *mut c_void,
) -> c_int {
    // Unconditionally, on the same argument the display backend and the Vulkan
    // present mode are printed on: this is once per registration for the life
    // of the process, and "which descriptors does the engine expect to hear
    // from" is exactly what a report about a starved or spinning loop needs.
    // The census can then be read against it -- an ident that never comes back
    // is a descriptor the engine is waiting on and Cordial never feeds.
    println!(
        "[android] ALooper_addFd(fd={fd}, ident={ident}, events={events}, callback={})",
        if callback.is_some() { "yes" } else { "no" },
    );
    let Some(l) = as_looper(looper) else {
        return -1;
    };
    // A descriptor that was not being watched a moment ago is new state; the
    // backoff's belief that nothing is happening predates it.
    l.empty_zero_streak.store(0, Ordering::Relaxed);

    let mut epoll_events = 0;
    if events & EVENT_INPUT != 0 {
        epoll_events |= EPOLLIN;
    }
    if events & EVENT_OUTPUT != 0 {
        epoll_events |= EPOLLOUT;
    }

    let mut ev = EpollEvent {
        events: epoll_events,
        data: fd as u64,
    };
    // SAFETY: `l.epoll` is open, `fd` is the caller's, `ev` is live.
    if unsafe { epoll_ctl(l.epoll, EPOLL_CTL_ADD, fd, &mut ev) } < 0 {
        return -1;
    }

    {
        let mut regs = l.registrations.borrow_mut();
        regs.push(Registration {
            fd,
            // With a callback Android reports POLL_CALLBACK rather than the ident.
            ident: if callback.is_some() { POLL_CALLBACK } else { ident },
            callback,
            data,
        });
        l.stats.registered.store(regs.len(), Ordering::Relaxed);
        *l.stats.registered_detail.lock().unwrap_or_else(|e| e.into_inner()) = regs
            .iter()
            .map(|r| {
                format!("{}:{}:{}", r.fd, r.ident, if r.callback.is_some() { "cb" } else { "-" })
            })
            .collect::<Vec<_>>()
            .join(",");
    }
    1
}

extern "C" fn looper_remove_fd(looper: *mut c_void, fd: c_int) -> c_int {
    let Some(l) = as_looper(looper) else {
        return -1;
    };
    // SAFETY: EPOLL_CTL_DEL ignores the event argument.
    unsafe { epoll_ctl(l.epoll, EPOLL_CTL_DEL, fd, std::ptr::null_mut()) };
    let mut regs = l.registrations.borrow_mut();
    let before = regs.len();
    regs.retain(|r| r.fd != fd);
    l.stats.registered.store(regs.len(), Ordering::Relaxed);
    if regs.len() < before {
        1
    } else {
        0
    }
}

extern "C" fn looper_wake(looper: *mut c_void) {
    let Some(l) = as_looper(looper) else {
        return;
    };
    // **Flush the busy-poll damper before the write, not after.**
    //
    // The damper lets a zero-timeout poll answer "nothing happened" from
    // memory for a few hundred microseconds. A wake is precisely the event
    // that makes that answer wrong, so the remembered emptiness has to go --
    // otherwise waking a looper could be ignored for the rest of the window,
    // which would turn a latency optimisation into dropped wakeups.
    //
    // Cleared first so there is no instant where the eventfd is readable and
    // this side still believes nothing has happened.
    l.empty_zero_streak.store(0, Ordering::Relaxed);
    l.stats.wakes.fetch_add(1, Ordering::Relaxed);
    l.stats.last_wake_ms.store(clock_ms(), Ordering::Relaxed);
    let one: u64 = 1;
    // SAFETY: writing the eight bytes an eventfd requires to our own descriptor.
    unsafe { write(l.wake, &one as *const u64 as *const c_void, 8) };
}

/// Restore the literal Android contract: a negative timeout blocks forever.
///
/// This exists to be the control. A clamp that is never compared against the
/// unclamped behaviour is a change nobody can attribute, and this repository
/// has four "fixes" that measured nothing because no control was run in the
/// same session. Set `CORDIAL_LOOPER_BLOCK=1` to reproduce the freeze on
/// demand and confirm the ceiling is what stopped it.
fn block_forever() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        let on = matches!(std::env::var("CORDIAL_LOOPER_BLOCK").as_deref(), Ok("1") | Ok("on"));
        if on {
            println!(
                "[looper] CORDIAL_LOOPER_BLOCK=1: an infinite ALooper_pollOnce will sleep with no \
                 ceiling, as it did before 2026-08-23. If a wakeup is lost the window freezes for \
                 good rather than for one frame. This is the control, not a setting to run with."
            );
        }
        on
    })
}

extern "C" fn looper_poll_once(
    timeout_millis: c_int,
    out_fd: *mut c_int,
    out_events: *mut c_int,
    out_data: *mut *mut c_void,
) -> c_int {
    let Some(l) = Looper::for_thread() else {
        super::trace(format_args!("ALooper_pollOnce on a thread with no looper"));
        return POLL_ERROR;
    };
    l.stats.polls.fetch_add(1, Ordering::Relaxed);
    let instrumentation = census::on();
    if instrumentation {
        POLLS.fetch_add(1, Ordering::Relaxed);
    }
    let seat = instrumentation.then(census::slot).flatten();
    if let Some(s) = seat {
        census::bump(&s.calls);
        census::bump(match timeout_millis {
            t if t < 0 => &s.t_block,
            0 => &s.t_zero,
            1..=9 => &s.t_short,
            _ => &s.t_long,
        });
    } else if instrumentation {
        census::bump(&census::OVERFLOW);
    }

    // Clamped rather than passed through; see BLOCK_CEILING_MS. The census above
    // deliberately classifies on the *requested* timeout, so `t_block` still
    // counts what the engine asked for rather than what it got.
    let requested_block = timeout_millis < 0;
    let effective_timeout = if requested_block && !block_forever() {
        BLOCK_CEILING_MS
    } else {
        timeout_millis
    };

    // **The idle backoff.** Only the zero-timeout case, which is the one the
    // engine spins on; a blocking or finite-timeout poll is untouched.
    //
    // The first attempt at this made each call cheaper -- answering from a
    // remembered timestamp instead of the kernel -- and that fixed nothing,
    // which is the measurement worth keeping. The thread stayed pegged at
    // 99.8% and simply moved from `epoll_wait` to the `clock_gettime` the
    // damper itself was doing. **The cost per call was never the problem: the
    // problem is that the loop has no blocking point and free-runs.** Making
    // the body cheaper only lets it spin faster.
    //
    // So once the poll has come back empty enough times in a row to be
    // obviously idle, sleep briefly before looking again. That caps the loop
    // at a few thousand iterations a second instead of millions, which is what
    // actually gives the core back. Any poll that finds something, and any
    // wake, resets the streak to zero, so a burst of real events is never
    // slowed -- only an idle spin is.
    let backoff_ns = zero_timeout_coalesce_ns();
    if timeout_millis == 0
        && backoff_ns > 0
        && super::glcount::QUEUE_PRESENT.load(Ordering::Relaxed) >= BACKOFF_AFTER_PRESENTS
        && l.empty_zero_streak.load(Ordering::Relaxed) >= IDLE_SPIN_THRESHOLD
    {
        POLLS_COALESCED.fetch_add(1, Ordering::Relaxed);
        // SAFETY: a plain relative sleep on this thread; the struct is
        // fully initialised and the call cannot fail in a way that matters.
        let ts = Timespec { tv_sec: 0, tv_nsec: backoff_ns as i64 };
        unsafe { nanosleep(&ts, std::ptr::null_mut()) };
    }

    let mut events = [EpollEvent { events: 0, data: 0 }; 16];
    // Time the syscall on one call in 1024. Timing every call would cost two
    // clock reads against a syscall that turns out to take about as long as
    // one, which is the classic way to measure the instrument.
    let timed = seat.is_some_and(|s| s.calls.load(Ordering::Relaxed) % 1024 == 0);
    let t0 = timed.then(std::time::Instant::now);
    // SAFETY: `events` is a live array of the length passed.
    let n = unsafe { epoll_wait(l.epoll, events.as_mut_ptr(), events.len() as c_int, effective_timeout) };
    if let (Some(s), Some(t0)) = (seat, t0) {
        s.ns.store(
            s.ns.load(Ordering::Relaxed) + t0.elapsed().as_nanos() as u64,
            Ordering::Relaxed,
        );
        census::bump(&s.ns_samples);
    }
    if n < 0 {
        return POLL_ERROR;
    }
    // Remember an empty zero-timeout poll so the next few can be answered
    // without the kernel; forget it the moment anything is actually ready, so
    // input in flight is never delayed by the damper.
    if timeout_millis == 0 {
        if n == 0 {
            l.empty_zero_streak.fetch_add(1, Ordering::Relaxed);
        } else {
            l.empty_zero_streak.store(0, Ordering::Relaxed);
        }
    }
    if n > 0 {
        l.stats.events.fetch_add(1, Ordering::Relaxed);
        l.stats.last_event_ms.store(clock_ms(), Ordering::Relaxed);
    }
    if n == 0 {
        if let Some(s) = seat {
            census::bump(&s.r_empty);
        }
        if requested_block {
            BLOCK_EXPIRED.fetch_add(1, Ordering::Relaxed);
        }
        // POLL_TIMEOUT even though the caller asked for no timeout, and the
        // alternative was worse: returning POLL_WAKE would claim a wake that
        // nobody performed, which is the shape of stub this project refuses to
        // write. A caller that asked for -1 is not expecting POLL_TIMEOUT, but
        // it is a value the same code path already handles for every finite
        // timeout, whereas a fabricated wake sends it looking for work that is
        // not there.
        return POLL_TIMEOUT;
    }

    for ev in events.iter().take(n as usize) {
        let fd = ev.data as c_int;

        if fd == l.wake {
            let mut sink = 0u64;
            // SAFETY: draining the eight bytes written by looper_wake.
            unsafe { read(l.wake, &mut sink as *mut u64 as *mut c_void, 8) };
            if let Some(s) = seat {
                census::bump(&s.r_wake);
            }
            return POLL_WAKE;
        }

        let (ident, callback, data) = {
            let regs = l.registrations.borrow();
            match regs.iter().find(|r| r.fd == fd) {
                Some(r) => (r.ident, r.callback, r.data),
                None => {
                    if let Some(s) = seat {
                        census::bump(&s.r_unclaimed);
                        s.last_unclaimed_fd.store(fd as u64, Ordering::Relaxed);
                    }
                    continue;
                }
            }
        };
        let looper_events = epoll_to_looper_events(ev.events);

        if let Some(cb) = callback {
            // The registration is not borrowed across this call: a callback is
            // entitled to add or remove descriptors, and holding the borrow would
            // panic when it did.
            if cb(fd, looper_events, data) == 0 {
                looper_remove_fd(l as *const Looper as *mut c_void, fd);
            }
            if let Some(s) = seat {
                census::bump(&s.r_callback);
            }
            return POLL_CALLBACK;
        }

        if !out_fd.is_null() {
            // SAFETY: caller-provided out-parameters, checked for null.
            unsafe { *out_fd = fd };
        }
        if !out_events.is_null() {
            // SAFETY: as above.
            unsafe { *out_events = looper_events };
        }
        if !out_data.is_null() {
            // SAFETY: as above.
            unsafe { *out_data = data };
        }
        if let Some(s) = seat {
            census::bump(&s.r_ident);
        }
        return ident;
    }

    POLL_TIMEOUT
}

pub fn overrides() -> Vec<(&'static str, *mut c_void)> {
    macro_rules! f {
        ($name:literal, $fn:expr) => {
            ($name, $fn as *const () as *mut c_void)
        };
    }
    vec![
        f!("ALooper_prepare", looper_prepare),
        f!("ALooper_forThread", looper_for_thread),
        f!("ALooper_acquire", looper_acquire),
        f!("ALooper_release", looper_release),
        f!("ALooper_addFd", looper_add_fd),
        f!("ALooper_removeFd", looper_remove_fd),
        f!("ALooper_pollOnce", looper_poll_once),
        f!("ALooper_wake", looper_wake),
    ]
}

#[cfg(test)]
mod tests {
    // Only the tests close a descriptor, so the binding lives with them rather
    // than in the module's extern block, where it read as dead code.
    extern "C" {
        fn close(fd: c_int) -> c_int;
    }

    use super::*;

    #[test]
    fn an_infinite_poll_returns_instead_of_blocking_forever() {
        // The regression this guards is a frozen window, so the assertion has to
        // be that the call *comes back at all*. Two clients were caught on
        // 2026-08-23 parked in `epoll_wait(-1)` with their present counts
        // stopped dead -- one after 2198 frames, one after 1 -- and the thing
        // that made that unrecoverable rather than a hiccup was the absence of
        // any ceiling here.
        //
        // Nothing is registered on this looper and nobody calls `looper_wake`,
        // so a request to block forever has, genuinely, nothing to wait for.
        // Before BLOCK_CEILING_MS this test would hang the suite rather than
        // fail it.
        let looper = looper_prepare(0);
        assert!(!looper.is_null());

        let before = BLOCK_EXPIRED.load(Ordering::Relaxed);
        let start = std::time::Instant::now();
        let r = looper_poll_once(-1, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut());
        let waited = start.elapsed();

        assert_eq!(r, POLL_TIMEOUT, "an expired infinite wait reports a timeout, not a fabricated wake");
        // Generous upper bound: the point is "bounded", not "precisely 50 ms",
        // and a loaded CI box should not turn a real fix into a flaky test.
        assert!(
            waited < std::time::Duration::from_secs(5),
            "an infinite poll blocked for {waited:?}, so the ceiling is not being applied"
        );
        assert_eq!(
            BLOCK_EXPIRED.load(Ordering::Relaxed),
            before + 1,
            "an expired infinite wait must be countable, or the lost-wakeup rate cannot be measured"
        );
    }

    #[test]
    fn a_woken_infinite_poll_still_reports_a_wake_rather_than_the_ceiling() {
        // The other half, and the one that would catch a clamp applied too
        // eagerly: a genuine wake must still come back as POLL_WAKE well inside
        // the ceiling. Without this, "returns POLL_TIMEOUT promptly" could be
        // satisfied by a poll that had stopped listening altogether.
        let looper = looper_prepare(0);
        assert!(!looper.is_null());

        looper_wake(looper);
        let start = std::time::Instant::now();
        let r = looper_poll_once(-1, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut());

        assert_eq!(r, POLL_WAKE, "a wake written before the poll must not be swallowed");
        assert!(
            start.elapsed() < std::time::Duration::from_millis(BLOCK_CEILING_MS as u64),
            "the wake was already pending, so this must not have waited out the ceiling"
        );
    }

    #[test]
    fn teardown_lifecycle_sequence_matches_androids_shutdown_order() {
        // Regression guard on the ordering itself, not just that `teardown`
        // calls something: onPause before onSurfaceDestroyed before onStop
        // before terminateNativeCode is the order the report specifies
        // Android actually uses, and a reorder here would be a real (if
        // subtle) behaviour change even though every step still ran.
        assert_eq!(
            TEARDOWN_LIFECYCLE_SEQUENCE,
            ["onPauseNative", "onSurfaceDestroyedNative", "onStopNative", "terminateNativeCode"]
        );
    }

    #[test]
    fn teardown_returns_within_its_grace_period_with_no_native_handle() {
        // A test process links no libroblox.so and starts no JavaVM, so every
        // `game_activity::*` call `teardown` makes fails immediately (see
        // `process_env`'s null-VM check) and this exercises only the bounded
        // grace-period loop. Regression guard: that loop must be bounded
        // rather than spin forever if the engine's natives never resolve.
        let looper = looper_prepare(0);
        assert!(!looper.is_null());
        let start = std::time::Instant::now();
        teardown(0);
        assert!(
            start.elapsed() < std::time::Duration::from_secs(2),
            "teardown took too long; its grace period must be bounded"
        );
    }

    #[test]
    fn poll_times_out_rather_than_spinning() {
        let looper = looper_prepare(0);
        assert!(!looper.is_null());
        let start = std::time::Instant::now();
        assert_eq!(looper_poll_once(50, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut()), POLL_TIMEOUT);
        // A stub returning immediately would turn the engine's main loop into a
        // busy spin, which is the failure this implementation exists to avoid.
        assert!(start.elapsed().as_millis() >= 40, "pollOnce returned without waiting");
    }

    #[test]
    fn wake_interrupts_a_blocked_poll() {
        let looper = looper_prepare(0);
        looper_wake(looper);
        assert_eq!(
            looper_poll_once(1000, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut()),
            POLL_WAKE
        );
    }

    #[test]
    fn a_readable_fd_is_reported_with_its_ident() {
        let looper = looper_prepare(0);
        // SAFETY: creating and writing to our own eventfd.
        let fd = unsafe { eventfd(1, 0) };
        assert!(fd >= 0);

        assert_eq!(looper_add_fd(looper, fd, 42, EVENT_INPUT, None, std::ptr::null_mut()), 1);

        let (mut out_fd, mut out_events) = (0, 0);
        let mut out_data = std::ptr::null_mut();
        let rc = looper_poll_once(500, &mut out_fd, &mut out_events, &mut out_data);
        assert_eq!(rc, 42, "pollOnce must report the ident the fd was registered with");
        assert_eq!(out_fd, fd);
        assert_ne!(out_events & EVENT_INPUT, 0);

        assert_eq!(looper_remove_fd(looper, fd), 1);
        // SAFETY: closing the descriptor this test opened.
        unsafe { close(fd) };
    }
}
