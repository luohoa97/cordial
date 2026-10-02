//! The Jit per thread, the stub page, and calls in both directions.
//!
//! **Guest to host.** Every host function the guest can reach is a 16-byte
//! stub in a page Cordial owns: `svc #id; ret`. dynarmic hands the immediate
//! to `CallSVC`, which looks the id up and runs its handler with the guest's
//! registers in reach, and the `ret` then returns to the guest caller as
//! though the stub had been the function. A stub is recognised by where the
//! SVC is as well as by its immediate: only the SVC at the start of a
//! registered slot reaches a handler, and any other -- the engine's own
//! `svc #0`, or an `svc #n` anywhere outside the page -- is a Linux syscall,
//! as on arm64 Linux, which ignores the immediate. Unused slots hold zero
//! words (`udf #0`), so a jump to a stub that was never registered stops
//! with a fault rather than doing anything. dynarmic's decoder has no UDF
//! entry, so that fault arrives as `InterpreterFallback`, not as the
//! `UnallocatedEncoding` exception design §8 expected.
//!
//! **Host to guest.** The registers are loaded, x30 is pointed at the one
//! `svc #RET` stub, and the Jit runs until that stub halts it.
//!
//! **Re-entry.** dynarmic's `Run` may not recurse, and a host function the
//! guest called -- `qsort` with a guest comparator, a JNI up-call, `dlopen`
//! running constructors -- may need to call the guest back while this
//! thread's Jit is still inside the SVC. So each thread has a small stack of
//! Jits and a call at depth *n* runs on the *n*th, created on first use. The
//! nested call's guest stack continues below the outer Jit's SP, as the
//! callee's frame would on hardware. Design §4 marked "distinct Jit
//! instances nest safely" INFERRED; `tests/m1.rs` is the test of it.

use std::cell::RefCell;
use std::ffi::c_void;
use std::sync::atomic::{AtomicPtr, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use crate::abi::{self, Ret, Ty};
use crate::ffi::*;
use crate::mem::{Mapping, PROT_READ, PROT_WRITE};

const RET_ID: u32 = 0xffff;
const STUB_BYTES: u64 = 16;
const STUB_SLOTS: u64 = 0x10000;

/// How LDXR/STXR are made atomic across Jits. The M1 test measures each.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MonitorMode {
    /// dynarmic's global monitor, with exclusive accesses through callbacks.
    Global,
    /// The global monitor, with exclusive accesses emitted inline
    /// (`fastmem_exclusive_access`) but still taking the monitor's spinlock.
    GlobalInline,
    /// Inline `cmpxchg` against the value LDXR loaded, no global lock
    /// (`Unsafe_IgnoreGlobalMonitor`). ABA-tolerant rather than exact.
    Ignore,
}

#[derive(Clone, Debug)]
pub struct Options {
    pub monitor: MonitorMode,
    /// Per Jit. Design §4 proposed 64 MiB to start and measuring.
    pub code_cache: usize,
    /// Per guest thread. Android's main thread gets 8 MiB.
    pub stack_size: usize,
    /// Upper bound on live Jits, which is the exclusive monitor's fixed
    /// processor count.
    pub max_jits: usize,
    /// Stored in TLS slot 5, where bionic's stack protector reads it.
    pub stack_guard: u64,
    /// Account the wall time spent on the host side of guest-to-host calls
    /// (`Stats::svc_nanos`), excluding guest code those calls run in turn.
    /// Off by default: it costs two clock reads per call.
    pub profile: bool,
    /// dynarmic's `Unsafe_*` floating-point optimisations to enable, as
    /// `UNSAFE_FP_*` bits. Each trades exactness against the ARM result for
    /// speed; none is on by default.
    pub unsafe_fp: u32,
}

/// `Unsafe_UnfuseFMA`: fused multiply-add as a multiply and an add.
pub const UNSAFE_FP_UNFUSE_FMA: u32 = 0x0001_0000;
/// `Unsafe_ReducedErrorFP`: reciprocal estimates at host precision.
pub const UNSAFE_FP_REDUCED_ERROR: u32 = 0x0002_0000;
/// `Unsafe_InaccurateNaN`: NaN results as x86 makes them.
pub const UNSAFE_FP_INACCURATE_NAN: u32 = 0x0004_0000;

impl Default for Options {
    fn default() -> Options {
        Options {
            monitor: MonitorMode::Global,
            code_cache: 64 << 20,
            stack_size: 8 << 20,
            max_jits: 1024,
            stack_guard: 0,
            profile: false,
            unsafe_fp: 0,
        }
    }
}

/// dynarmic's `A64::Exception`, in its declaration order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exception {
    UnallocatedEncoding,
    ReservedValue,
    UnpredictableInstruction,
    WaitForInterrupt,
    WaitForEvent,
    SendEvent,
    SendEventLocal,
    Yield,
    Breakpoint,
    NoExecuteFault,
    Other(u32),
}

impl Exception {
    fn from_raw(k: u32) -> Exception {
        use Exception::*;
        [UnallocatedEncoding, ReservedValue, UnpredictableInstruction, WaitForInterrupt,
         WaitForEvent, SendEvent, SendEventLocal, Yield, Breakpoint, NoExecuteFault]
            .get(k as usize)
            .copied()
            .unwrap_or(Other(k))
    }
}

/// Why guest execution stopped other than by returning.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fault {
    Exception { pc: u64, kind: Exception },
    /// An instruction dynarmic's decoder has no entry for. That covers
    /// system registers it does not model (CNTVCT_EL0, MIDR_EL1), but also
    /// every instruction family it does not implement -- LSE atomics, UDF --
    /// which design §1.3 expected to arrive as `UnallocatedEncoding`. Not
    /// emulated, so it stops rather than continuing with a made-up value.
    InterpreterFallback { pc: u64, count: u64 },
    /// An SVC outside the stub page -- a raw Linux syscall with arm64
    /// numbering, whatever its immediate -- on a runtime with no handler for
    /// them (`Runtime::set_syscall_handler`).
    Syscall { pc: u64 },
    /// An SVC inside the stub page that is not a registered stub's own.
    UnknownSvc { imm: u32, pc: u64 },
    /// A thunk was asked for something it cannot do faithfully.
    Unsupported { thunk: String, why: String },
    /// The host function behind a stub threw a C++ exception, caught at the
    /// boundary (`cg_host_call_guarded`).
    HostException { function: String, what: String },
    /// The guest itself asked to stop the process -- `abort`, `exit` -- and
    /// the thunk refused to take the host down from inside a callback.
    Exit { function: String, status: i32 },
    /// `pthread_exit(value)`: not a fault, but the one way to leave every
    /// guest frame on the thread at once without unwinding through the
    /// translator's, which have no unwind information (design §4). It
    /// travels up like a fault to the thread's start routine, which returns
    /// `value` as the thread's result.
    ThreadExit { value: u64 },
}

/// Where the guest was when a call stopped with a fault: its PC, x30 and
/// SP, and the return addresses found by walking the x29 frame chain, which
/// AAPCS64 code built by the NDK keeps. Recorded per thread by
/// `guest_call`, read with `last_fault_context`.
#[derive(Clone, Debug, Default)]
pub struct FaultContext {
    pub pc: u64,
    pub lr: u64,
    pub sp: u64,
    pub fp: u64,
    pub frames: Vec<u64>,
}

/// What the guest returned: x0, x1 and v0.
#[derive(Clone, Copy, Debug, Default)]
pub struct Returned {
    /// Where the Jit stopped: always just past the `svc #RET` stub.
    pub pc: u64,
    pub x0: u64,
    pub x1: u64,
    pub v0: [u64; 2],
}

pub type Handler = Box<dyn Fn(&Call) -> Result<(), Fault> + Send + Sync>;

struct Entry {
    name: String,
    handler: Handler,
}

/// A slot of the stub page, by index. Made only by `Runtime::slot_at`, from
/// an address inside the page, so every key of `Runtime::entries` is one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct StubId(u32);

/// One thread's stub call counts, indexed by stub id: id 0 is the raw
/// `svc #0`, and the `RET_ID` slot, which is never counted as a call, holds
/// the thread's total. Each is written only by its own thread, with a plain
/// load and store rather than a locked add.
///
/// These used to be one `AtomicU64` per stub and one per runtime, bumped by
/// every thread. In game the engine makes 6--7 M stub calls a second from a
/// dozen threads, 5 M of them `pthread_getspecific`, and the cache line
/// holding that one counter moved between cores on nearly every call; with
/// the `RwLock` read and the `Arc` clone around each lookup it made
/// `on_svc` and `Runtime::entry` 40--65 % of the busiest threads' samples
/// under perf (M7, docs/vr/dynarmic-design.md §9.8).
struct Counts(Box<[AtomicU64]>);

impl Counts {
    fn new() -> Counts {
        let n = STUB_SLOTS as usize;
        let layout = std::alloc::Layout::array::<AtomicU64>(n).unwrap();
        // SAFETY: zero is a valid AtomicU64, and a zeroed allocation this
        // size comes from mmap, so the pages a thread never counts in are
        // never made resident. The box takes ownership with the same layout.
        let p = unsafe { std::alloc::alloc_zeroed(layout) } as *mut AtomicU64;
        assert!(!p.is_null(), "out of memory for stub call counts");
        // SAFETY: as above.
        Counts(unsafe { Box::from_raw(std::ptr::slice_from_raw_parts_mut(p, n)) })
    }

    #[inline]
    fn bump(&self, id: u32) {
        let c = &self.0[id as usize];
        c.store(c.load(Ordering::Relaxed) + 1, Ordering::Relaxed);
        let t = &self.0[RET_ID as usize];
        t.store(t.load(Ordering::Relaxed) + 1, Ordering::Relaxed);
    }
}

/// Per-runtime counters, for the performance record (design §8, M3). The
/// stub call total is `Runtime::svc_calls`.
#[derive(Debug, Default)]
pub struct Stats {
    pub jits_created: AtomicU64,
    /// With `Options::profile`: nanoseconds on the host side of stub calls,
    /// exclusive of guest code they call back into.
    pub svc_nanos: AtomicU64,
    /// Instructions dynarmic handed back that were emulated here (CNTVCT_EL0).
    pub emulated: AtomicU64,
    /// `IC IVAU` and `IC IALLU[IS]` the guest executed.
    pub icache_ops: AtomicU64,
}

struct Monitor(*mut c_void);
// SAFETY: dynarmic's ExclusiveMonitor is built to be shared by every Jit in
// the process and takes its own lock.
unsafe impl Send for Monitor {}
// SAFETY: as above.
unsafe impl Sync for Monitor {}

/// One guest world: its stubs, its exclusive monitor, its options.
pub struct Runtime {
    id: u64,
    opts: Options,
    monitor: Monitor,
    stubs: Mapping,
    stub_write: Mutex<()>,
    /// Indexed by stub id, filled once and never replaced, so a lookup is
    /// one load and takes no lock. Owned; freed on drop.
    entries: Box<[AtomicPtr<Entry>]>,
    next_id: AtomicU32,
    /// Every thread's call counts, for `call_counts` and `svc_calls`.
    counts: Mutex<Vec<Arc<Counts>>>,
    /// bionic's pthread key map (`keys.rs`).
    pub(crate) keys: crate::keys::KeyMap,
    /// What answers a raw `svc #0`, which has no stub: the guest executes it
    /// itself. Unset, it stops with `Fault::Syscall`.
    syscall: OnceLock<Entry>,
    free_processors: Mutex<Vec<u64>>,
    stats: Stats,
}

static NEXT_RUNTIME: AtomicU64 = AtomicU64::new(1);

impl Runtime {
    pub fn new(opts: Options) -> Arc<Runtime> {
        let stubs = Mapping::new((STUB_SLOTS * STUB_BYTES) as usize);
        // SAFETY: the mapping is STUB_SLOTS*16 bytes and still writable.
        unsafe { write_stub(stubs.addr() + RET_ID as u64 * STUB_BYTES, RET_ID) };
        stubs.protect(PROT_READ);
        // SAFETY: a fresh monitor for max_jits processors.
        let monitor = Monitor(unsafe { cg_monitor_new(opts.max_jits as u64) });
        Arc::new(Runtime {
            id: NEXT_RUNTIME.fetch_add(1, Ordering::Relaxed),
            free_processors: Mutex::new((0..opts.max_jits as u64).rev().collect()),
            opts,
            monitor,
            stubs,
            stub_write: Mutex::new(()),
            entries: (0..STUB_SLOTS).map(|_| AtomicPtr::new(std::ptr::null_mut())).collect(),
            next_id: AtomicU32::new(1), // id 0 is the real syscall
            counts: Mutex::new(Vec::new()),
            keys: crate::keys::KeyMap::new(),
            syscall: OnceLock::new(),
            stats: Stats::default(),
        })
    }

    pub fn options(&self) -> &Options {
        &self.opts
    }

    pub fn stats(&self) -> &Stats {
        &self.stats
    }

    /// Registers a handler and returns the guest address of its stub.
    pub fn register(&self, name: &str, handler: Handler) -> u64 {
        let _w = self.stub_write.lock().unwrap();
        let at = self.stub_addr(StubId(self.next_id.load(Ordering::Relaxed)));
        let id = self.install(at, 1, Entry { name: name.to_owned(), handler })
            .unwrap_or_else(|why| panic!("registering {name}: {why}"));
        // SAFETY: `install` accepted slot `id`, which is inside the mapping
        // and writable until the protect below.
        self.write_slots(|| unsafe { write_stub(at, id.0) });
        at
    }

    /// Writes guest code into consecutive stub slots and returns its
    /// address, for an import answered by guest instructions rather than an
    /// SVC (`keys.rs`). The first slot is named `name`, so `stub_name` and
    /// fault reports still say what it is; its handler is never reached,
    /// since the code holds no SVC.
    pub fn register_code(&self, name: &str, words: &[u32]) -> u64 {
        let _w = self.stub_write.lock().unwrap();
        let at = self.stub_addr(StubId(self.next_id.load(Ordering::Relaxed)));
        let slots = (words.len() as u64 * 4).div_ceil(STUB_BYTES) as u32;
        let what = name.to_owned();
        let entry = Entry {
            name: name.to_owned(),
            handler: Box::new(move |_| Err(Fault::Unsupported {
                thunk: what.clone(),
                why: "guest code in the stub page was entered by SVC".into(),
            })),
        };
        self.install(at, slots, entry).unwrap_or_else(|why| panic!("registering {name}: {why}"));
        // SAFETY: `install` accepted `slots` unused slots from `at`, and the
        // mapping is writable until the protect below.
        self.write_slots(|| unsafe { std::ptr::copy_nonoverlapping(words.as_ptr(), at as *mut u32, words.len()) });
        at
    }

    /// The one place a handler is attached to anything. It is keyed by a
    /// slot of this runtime's stub page and nothing else: `at` must be the
    /// next unused slot, with room for `slots` of them below the `svc #RET`
    /// stub. ADR-053 keeps the translator inside ADR-001 on the condition
    /// that dispatch is never keyed on an engine address; this refusal is
    /// what makes that a property of the code rather than of its callers,
    /// since no address outside the page can become a key. Called with
    /// `stub_write` held.
    fn install(&self, at: u64, slots: u32, entry: Entry) -> Result<StubId, String> {
        let next = self.next_id.load(Ordering::Relaxed);
        let id = self.slot_at(at).ok_or_else(|| format!("{at:#x} is not a slot of the stub page"))?;
        if id.0 != next {
            return Err(format!("{at:#x} is slot {}, and the next unused slot is {next}", id.0));
        }
        if slots == 0 || u64::from(id.0) + u64::from(slots) > u64::from(RET_ID) {
            return Err("stub page full".into());
        }
        // Published before the stub is written, so a guest that can reach
        // the stub always finds its entry.
        self.entries[id.0 as usize].store(Box::into_raw(Box::new(entry)), Ordering::Release);
        self.next_id.store(id.0 + slots, Ordering::Release);
        Ok(id)
    }

    fn write_slots(&self, write: impl FnOnce()) {
        self.stubs.protect(PROT_READ | PROT_WRITE);
        write();
        self.stubs.protect(PROT_READ);
    }

    fn stub_addr(&self, id: StubId) -> u64 {
        self.stubs.addr() + u64::from(id.0) * STUB_BYTES
    }

    /// The stub slot starting at `addr`, if it is one: inside the page, on a
    /// slot boundary, and neither id 0 (the raw syscall, which has no slot)
    /// nor the `svc #RET` stub. The only conversion from an address to a key
    /// of `entries`.
    fn slot_at(&self, addr: u64) -> Option<StubId> {
        let off = addr.checked_sub(self.stubs.addr())?;
        if off % STUB_BYTES != 0 || off >= STUB_SLOTS * STUB_BYTES {
            return None;
        }
        let id = (off / STUB_BYTES) as u32;
        (id != 0 && id != RET_ID).then_some(StubId(id))
    }

    /// The registered stub an `svc #imm` at `pc` is: only the SVC a stub
    /// itself holds, at the start of its own slot. The immediate alone is
    /// not enough, because the guest can execute an `svc` with any
    /// immediate anywhere in its own code.
    #[inline]
    fn stub_for_svc(&self, pc: u64, imm: u32) -> Option<(StubId, &Entry)> {
        let id = self.slot_at(pc)?;
        if id.0 != imm {
            return None;
        }
        Some((id, self.entry(id)?))
    }

    /// Installs the handler for raw `svc #0`: the guest's own Linux syscall
    /// instruction (any SVC outside the stub page), x8 the arm64 number, x0..x5 the arguments. The handler
    /// leaves the kernel's answer in x0 (a negative errno on failure, errno
    /// itself untouched), and execution continues at the next instruction,
    /// as it does after a real syscall. Once per runtime.
    pub fn set_syscall_handler(&self, handler: Handler) {
        let e = Entry { name: "svc #0".into(), handler };
        if self.syscall.set(e).is_err() {
            panic!("set_syscall_handler called twice");
        }
    }

    /// How many raw `svc #0` the guest has executed.
    pub fn syscall_count(&self) -> u64 {
        self.count_of(0)
    }

    fn count_of(&self, id: u32) -> u64 {
        self.counts.lock().unwrap().iter().map(|c| c.0[id as usize].load(Ordering::Relaxed)).sum()
    }

    /// Guest-to-host calls so far, every thread, raw `svc #0` included.
    pub fn svc_calls(&self) -> u64 {
        self.count_of(RET_ID)
    }

    /// Registers a host function the generic call builder can reach: fixed
    /// arguments, each described by `args`.
    pub fn register_host(&self, name: &str, f: *const c_void, args: &'static [Ty], ret: Ret) -> u64 {
        let f = f as usize;
        self.register(name, Box::new(move |c| abi::call_host(c, f as *const c_void, args, ret)))
    }

    /// The stub page's address range, `[start, end)`. Every function import
    /// a guest object is linked against lands inside it.
    pub fn stub_page(&self) -> (u64, u64) {
        (self.stubs.addr(), self.stubs.addr() + self.stubs.len() as u64)
    }

    pub fn ret_stub(&self) -> u64 {
        self.stubs.addr() + RET_ID as u64 * STUB_BYTES
    }

    /// Whether `addr` is one of this runtime's stubs, and whose.
    pub fn stub_name(&self, addr: u64) -> Option<String> {
        self.entry(self.slot_at(addr)?).map(|e| e.name.clone())
    }

    /// Every stub that has been called, with how many times, most first.
    pub fn call_counts(&self) -> Vec<(String, u64)> {
        let counts = self.counts.lock().unwrap();
        let n = self.next_id.load(Ordering::Acquire);
        let mut v: Vec<(String, u64)> = Vec::new();
        let mut add = |name: &str, id: u32| {
            let k: u64 = counts.iter().map(|c| c.0[id as usize].load(Ordering::Relaxed)).sum();
            if k > 0 {
                v.push((name.to_owned(), k));
            }
        };
        for id in 1..n {
            if let Some(e) = self.entry(StubId(id)) {
                add(&e.name, id);
            }
        }
        v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        v
    }

    #[inline]
    fn entry(&self, id: StubId) -> Option<&Entry> {
        let p = self.entries.get(id.0 as usize)?.load(Ordering::Acquire);
        // SAFETY: a non-null slot holds a leaked Box<Entry> that is never
        // replaced and is freed only when the Runtime drops.
        (!p.is_null()).then(|| unsafe { &*p })
    }

    fn new_counts(&self) -> Arc<Counts> {
        let c = Arc::new(Counts::new());
        self.counts.lock().unwrap().push(c.clone());
        c
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        for e in self.entries.iter() {
            let p = e.swap(std::ptr::null_mut(), Ordering::Relaxed);
            if !p.is_null() {
                // SAFETY: made by Box::into_raw in `register`, freed once.
                drop(unsafe { Box::from_raw(p) });
            }
        }
        // SAFETY: every Jit holds an Arc to this Runtime, so none is left.
        unsafe { cg_monitor_free(self.monitor.0) };
    }
}

/// `svc #id; ret`, then padding that is itself unallocated.
unsafe fn write_stub(addr: u64, id: u32) {
    let p = addr as *mut u32;
    // SAFETY: the caller guarantees 16 writable bytes at `addr`.
    unsafe {
        p.write(0xd400_0001 | (id << 5));
        p.add(1).write(0xd65f_03c0);
        p.add(2).write(0);
        p.add(3).write(0);
    }
}

/// The guest's registers during a guest-to-host call.
pub struct Call<'a> {
    jit: *mut CgJit,
    rt: &'a Arc<Runtime>,
    name: &'a str,
}

impl Call<'_> {
    pub fn x(&self, i: u32) -> u64 {
        // SAFETY: the Jit is live and stopped inside CallSVC for the whole
        // lifetime of this Call.
        unsafe { cg_jit_get_x(self.jit, i) }
    }
    pub fn set_x(&self, i: u32, v: u64) {
        // SAFETY: as for `x`.
        unsafe { cg_jit_set_x(self.jit, i, v) }
    }
    pub fn v(&self, i: u32) -> [u64; 2] {
        let mut out = [0; 2];
        // SAFETY: as for `x`.
        unsafe { cg_jit_get_v(self.jit, i, &mut out) };
        out
    }
    pub fn set_v(&self, i: u32, v: [u64; 2]) {
        // SAFETY: as for `x`.
        unsafe { cg_jit_set_v(self.jit, i, &v) }
    }
    pub fn sp(&self) -> u64 {
        // SAFETY: as for `x`.
        unsafe { cg_jit_get_sp(self.jit) }
    }
    /// Moves the guest's stack pointer, for a thunk that restores a saved
    /// register file (`longjmp`). The stub's `ret` then returns through
    /// whatever x30 holds.
    pub fn set_sp(&self, v: u64) {
        // SAFETY: as for `x`.
        unsafe { cg_jit_set_sp(self.jit, v) }
    }
    pub fn pc(&self) -> u64 {
        // SAFETY: as for `x`.
        unsafe { cg_jit_get_pc(self.jit) }
    }
    /// The `i`th 8-byte stack argument slot of the call.
    pub fn stack_word(&self, i: u64) -> u64 {
        // SAFETY: identity mapping -- the guest's SP is a host address into
        // the guest stack, and the slots above it hold the caller's
        // outgoing arguments.
        unsafe { ((self.sp() + 8 * i) as *const u64).read() }
    }
    pub fn runtime(&self) -> &Arc<Runtime> {
        self.rt
    }
    /// The name the stub was registered under.
    pub fn name(&self) -> &str {
        self.name
    }
    /// Calls `f` with this call's arguments classified by `args`.
    pub fn host(&self, f: *const c_void, args: &[Ty], ret: Ret) -> Result<(), Fault> {
        abi::call_host(self, f, args, ret)
    }
}

struct JitBox {
    raw: *mut CgJit,
    rt: Arc<Runtime>,
    fault: RefCell<Option<Fault>>,
    processor: u64,
    /// The low end of the thread's guest stack.
    stack_low: u64,
    /// The owning thread's call counts.
    counts: Arc<Counts>,
    /// What `invalidate_everywhere` reads of this Jit.
    shared: Arc<Shared>,
}

/// Written only by the owning thread.
#[derive(Default)]
struct Shared {
    /// Odd while the Jit is executing translated code, even while it is
    /// stopped or inside one of its callbacks.
    state: AtomicU64,
    /// The `INVALIDATIONS` count read just before the Jit last entered
    /// `Run`, which performs every invalidation recorded before that count
    /// was published before it runs any translated code.
    done: AtomicU64,
}

/// Cross-Jit invalidation requests so far, published once each request's
/// ranges are recorded in every Jit.
static INVALIDATIONS: AtomicU64 = AtomicU64::new(0);

impl JitBox {
    /// Into translated code: before `cg_jit_run` and on leaving a callback.
    /// Both are followed by a check of the Jit's halt flag before any
    /// translated block runs -- `Run` performs requested invalidations on
    /// entry, and every callback the translator emits (SVC, exception, cache
    /// operation, fallback) ends its block with `CheckHalt` or a return to
    /// the dispatcher, which checks it too.
    #[inline]
    fn enter(&self) {
        let s = &self.shared.state;
        s.store(s.load(Ordering::Relaxed) + 1, Ordering::Release);
    }
    #[inline]
    fn leave(&self) {
        let s = &self.shared.state;
        s.store(s.load(Ordering::Relaxed) + 1, Ordering::Release);
    }
}

/// Marks a callback's extent: the Jit is out of translated code until this
/// drops.
struct InCallback<'a>(&'a JitBox);

impl<'a> InCallback<'a> {
    #[inline]
    fn new(jb: &'a JitBox) -> InCallback<'a> {
        jb.leave();
        InCallback(jb)
    }
}

impl Drop for InCallback<'_> {
    #[inline]
    fn drop(&mut self) {
        self.0.enter();
    }
}

/// A Jit as `invalidate_everywhere` sees it.
struct Live {
    raw: *mut CgJit,
    shared: Arc<Shared>,
    tid: i32,
}

// SAFETY: `raw` is only used under the LIVE lock, through dynarmic's
// `InvalidateCacheRange`, which takes the Jit's own invalidation mutex and
// sets its halt flag atomically -- the call dynarmic's users make on other
// cores' Jits (yuzu, for one). The Jit is removed from LIVE before it is
// freed.
unsafe impl Send for Live {}

/// Every Jit in the process, whatever its Runtime: guest memory is the
/// process's, so a change to guest code is every Jit's concern.
static LIVE: Mutex<Vec<Live>> = Mutex::new(Vec::new());

impl JitBox {
    fn new(rt: &Arc<Runtime>, tpidr: *mut u64, stack_low: u64, counts: Arc<Counts>) -> Box<JitBox> {
        let processor = rt.free_processors.lock().unwrap().pop()
            .expect("more live Jits than Options::max_jits");
        let mut b = Box::new(JitBox {
            raw: std::ptr::null_mut(),
            rt: rt.clone(),
            fault: RefCell::new(None),
            processor,
            stack_low,
            counts,
            shared: Arc::default(),
        });
        let (fastmem_exclusive, ignore) = match rt.opts.monitor {
            MonitorMode::Global => (0, 0),
            MonitorMode::GlobalInline => (1, 0),
            MonitorMode::Ignore => (1, 1),
        };
        let cfg = CgConfig {
            callbacks: CgCallbacks {
                user: &*b as *const JitBox as *mut c_void,
                svc: on_svc,
                exception: on_exception,
                fallback: on_fallback,
                icache: on_icache,
            },
            tpidr_el0: tpidr,
            tpidrro_el0: std::ptr::null(),
            monitor: rt.monitor.0,
            processor_id: processor,
            fastmem_exclusive,
            ignore_global_monitor: ignore,
            code_cache_size: rt.opts.code_cache as u64,
            unsafe_fp: rt.opts.unsafe_fp,
        };
        // SAFETY: cfg outlives the call; `tpidr` points into the owning
        // GuestThread, which drops its Jits before its TLS slot.
        b.raw = unsafe { cg_jit_new(&cfg) };
        rt.stats.jits_created.fetch_add(1, Ordering::Relaxed);
        membarrier_register();
        // SAFETY: plain libc call.
        let tid = unsafe { gettid() };
        LIVE.lock().unwrap().push(Live { raw: b.raw, shared: b.shared.clone(), tid });
        b
    }

    fn fault(&self, f: Fault) {
        *self.fault.borrow_mut() = Some(f);
        // SAFETY: called from inside this Jit's own callback.
        unsafe { cg_jit_halt(self.raw, HALT_FAULT) };
    }
}

impl Drop for JitBox {
    fn drop(&mut self) {
        LIVE.lock().unwrap().retain(|l| l.raw != self.raw);
        // SAFETY: not executing, and out of LIVE, so this is the only owner.
        unsafe { cg_jit_free(self.raw) };
        self.rt.free_processors.lock().unwrap().push(self.processor);
    }
}

extern "C" fn on_svc(user: *mut c_void, jit: *mut CgJit, imm: u32) {
    // SAFETY: `user` is the JitBox this Jit was built with, which outlives it.
    let jb = unsafe { &*(user as *const JitBox) };
    let _out = InCallback::new(jb);
    // SAFETY: inside this Jit's own callback.
    let pc = unsafe { cg_jit_get_pc(jit) } - 4;
    // The PC is already past the SVC; the fault reports the SVC itself.
    if imm == RET_ID && pc == jb.rt.ret_stub() {
        // SAFETY: inside this Jit's own callback.
        unsafe { cg_jit_halt(jit, HALT_RETURNED) };
        return;
    }
    // A stub is found from where the SVC is, not from its immediate alone:
    // an `svc #n` anywhere outside the stub page is the guest's own system
    // call, whatever `n` is, as it is to arm64 Linux, which ignores the
    // immediate. So no instruction the engine holds can reach a handler.
    let (id, entry): (u32, &Entry) = match jb.rt.stub_for_svc(pc, imm) {
        Some((id, e)) => (id.0, e),
        None if (jb.rt.stubs.addr()..jb.rt.stubs.addr() + jb.rt.stubs.len() as u64).contains(&pc) => {
            return jb.fault(Fault::UnknownSvc { imm, pc });
        }
        None => match jb.rt.syscall.get() {
            Some(e) => (0, e),
            None => return jb.fault(Fault::Syscall { pc }),
        },
    };
    // Past the guard the guest would fault inside translated code, with no
    // guest PC to name; this catches the usual cause, runaway recursion
    // through a call, while there is still stack to say so. Only on the
    // thread's own stack: the engine also runs on stacks it maps itself
    // (fibers), whose extent nothing here knows.
    // SAFETY: inside this Jit's own callback.
    let sp = unsafe { cg_jit_get_sp(jit) };
    if sp < jb.stack_low + (64 << 10) && sp + (64 << 10) >= jb.stack_low {
        eprintln!("cordial-guest: guest stack nearly exhausted (sp {sp:#x}, low {:#x}) calling {}; last stub calls:",
                  jb.stack_low, entry.name);
        dump_svc_ring(&jb.rt);
        return jb.fault(Fault::Unsupported {
            thunk: entry.name.clone(),
            why: "the guest stack is nearly exhausted".into(),
        });
    }
    jb.counts.bump(id);
    if TRACE.load(Ordering::Relaxed) {
        // SAFETY: plain libc call.
        // SAFETY: inside this Jit's own callback.
        let x = |i| unsafe { cg_jit_get_x(jit, i) };
        eprintln!("[svc] {} {} x0={:#x} x1={:#x} x2={:#x} x3={:#x} lr={:#x} from {pc:#x}",
                  // SAFETY: plain libc call.
                  unsafe { gettid() }, entry.name, x(0), x(1), x(2), x(3), x(30));
    }
    SVC_RING.with(|r| {
        let mut r = r.borrow_mut();
        let n = r.1;
        r.0[n % 64] = (id, pc);
        r.1 = n + 1;
    });
    let call = Call { jit, rt: &jb.rt, name: &entry.name };
    let profile = jb.rt.opts.profile;
    if profile {
        SVC_CLOCK.with(|c| c.borrow_mut().push(std::time::Instant::now()));
    }
    let r = (entry.handler)(&call);
    if profile {
        let t = SVC_CLOCK.with(|c| c.borrow_mut().pop()).expect("svc clock");
        jb.rt.stats.svc_nanos.fetch_add(t.elapsed().as_nanos() as u64, Ordering::Relaxed);
    }
    if let Err(f) = r {
        jb.fault(f);
    }
}

extern "C" fn on_exception(user: *mut c_void, _jit: *mut CgJit, pc: u64, kind: u32) {
    // SAFETY: as in on_svc.
    let jb = unsafe { &*(user as *const JitBox) };
    let _out = InCallback::new(jb);
    let kind = Exception::from_raw(kind);
    // The hints. This dynarmic raises them whatever `hook_hint_instructions`
    // says -- `a64_interface.cpp` builds its TranslationOptions without that
    // field, whose default there is true -- so a WFE in a contended guest
    // spinlock stopped the process (M7: a thread's key destructor at exit,
    // one run in about a dozen). Architecturally WFE may complete at once and
    // YIELD, SEV and SEVL have no effect a single core can observe, so each
    // completes here; the translator has already set the PC past it. WFE
    // gives up the host CPU, since the guest only reaches it while waiting.
    match kind {
        Exception::WaitForEvent => return std::thread::yield_now(),
        Exception::Yield => return std::hint::spin_loop(),
        Exception::SendEvent | Exception::SendEventLocal => return,
        _ => {}
    }
    jb.fault(Fault::Exception { pc, kind });
}

/// The virtual counter, CNTVCT_EL0, which dynarmic does not model (design
/// §1.3: 11 sites in the engine). It reads the same clock as the physical
/// counter dynarmic does model (`GetCNTPCT` in `native/shim.cpp`), at the
/// same 600 MHz dynarmic reports as CNTFRQ_EL0, with a virtual offset of
/// zero, as Linux sets it. This is CPU emulation of a register the guest
/// reads, not a change to anything it executes (design §7).
fn cntvct() -> u64 {
    let mut ts = [0i64; 2];
    extern "C" {
        fn clock_gettime(clk: i32, ts: *mut [i64; 2]) -> i32;
    }
    // SAFETY: CLOCK_MONOTONIC into a local timespec.
    unsafe { clock_gettime(1, &mut ts) };
    ts[0] as u64 * 600_000_000 + ts[1] as u64 * 3 / 5
}

extern "C" fn on_fallback(user: *mut c_void, jit: *mut CgJit, pc: u64, count: u64) {
    // SAFETY: as in on_svc.
    let jb = unsafe { &*(user as *const JitBox) };
    let _out = InCallback::new(jb);
    if count == 1 {
        // SAFETY: the guest's instruction at pc, which the translator just
        // fetched.
        let insn = unsafe { (pc as *const u32).read() };
        // mrs xT, cntvct_el0
        if insn & 0xffff_ffe0 == 0xd53b_e040 {
            let rt = insn & 31;
            // SAFETY: inside this Jit's own callback; the fallback ends the
            // block, so the dispatcher resumes from the PC set here.
            unsafe {
                if rt != 31 {
                    cg_jit_set_x(jit, rt, cntvct());
                }
                cg_jit_set_pc(jit, pc + 4);
            }
            jb.rt.stats.emulated.fetch_add(1, Ordering::Relaxed);
            return;
        }
    }
    jb.fault(Fault::InterpreterFallback { pc, count });
}

/// `IC IVAU` (op 0, `va` the line) or `IC IALLU[IS]` (ops 1 and 2).
extern "C" fn on_icache(user: *mut c_void, _jit: *mut CgJit, op: u32, va: u64) {
    // SAFETY: as in on_svc.
    let jb = unsafe { &*(user as *const JitBox) };
    let _out = InCallback::new(jb);
    jb.rt.stats.icache_ops.fetch_add(1, Ordering::Relaxed);
    crate::code::on_icache(op, va);
}

/// How long `invalidate_everywhere` waits for a Jit executing translated code
/// to reach a halt check before it gives up and says so.
const INVALIDATE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(5);

/// Drops every Jit's translations of guest code overlapping `ranges`
/// (`[start, end)` each), and returns once none of them can execute one.
/// Callers hold `code`'s lock, so there is one of these at a time.
///
/// dynarmic's `InvalidateCacheRange` only records the range and sets the
/// Jit's halt flag; the Jit performs it on its own thread, on entering
/// `Run` or on leaving it. Recording is safe from any thread, and is the
/// call dynarmic's users make on other cores' Jits. A Jit that is stopped,
/// or inside a callback -- the caller's own is inside the stub that called
/// here -- checks the flag before it runs another translated block. One
/// executing translated code right now may not, for a while: a return (the
/// return stack) and an indirect branch (the fast dispatch table) go
/// straight to cached code without checking it, so until it reaches a
/// direct branch or a callback it could still jump into a stale block. Each
/// of those is waited for, until it leaves translated code or enters `Run`
/// having read this request's number, which was published after the ranges
/// were recorded. The flag is set again after publishing, so a Jit that
/// took the first one and re-entered before the number was out leaves
/// again and reads it.
///
/// Reading a Jit's state after setting its flag is a store-then-load on
/// each side, which x86 may reorder; `membarrier` puts a full barrier on
/// every thread of the process between the two, so the hot path pays only
/// two plain stores per callback.
pub(crate) fn invalidate_everywhere(ranges: &[(u64, u64)]) -> Result<Invalidated, String> {
    let mut done = Invalidated::default();
    if ranges.is_empty() {
        return Ok(done);
    }
    let (request, watched): (u64, Vec<(Arc<Shared>, i32)>) = {
        let live = LIVE.lock().unwrap();
        for l in live.iter() {
            for &(s, e) in ranges {
                // SAFETY: a live Jit (see `Live`); thread-safe in dynarmic.
                unsafe { cg_jit_invalidate(l.raw, s, e - s) };
            }
        }
        let request = INVALIDATIONS.fetch_add(1, Ordering::AcqRel) + 1;
        for l in live.iter() {
            // SAFETY: as above; an atomic OR.
            unsafe { cg_jit_halt(l.raw, HALT_CACHE_INVALIDATION) };
        }
        (request, live.iter().map(|l| (l.shared.clone(), l.tid)).collect())
    };
    membarrier()?;
    let start = std::time::Instant::now();
    done.jits = watched.len();
    for (sh, tid) in &watched {
        let state = sh.state.load(Ordering::Acquire);
        if state & 1 == 0 {
            continue;
        }
        done.running += 1;
        while sh.done.load(Ordering::Acquire) < request && sh.state.load(Ordering::Acquire) == state {
            if start.elapsed() > INVALIDATE_DEADLINE {
                return Err(format!("the Jit on thread {tid} ran translated code for {}s without reaching a halt \
                                    check, so its translations of the changed range could not be dropped",
                                   INVALIDATE_DEADLINE.as_secs()));
            }
            std::thread::yield_now();
        }
    }
    done.waited = start.elapsed();
    Ok(done)
}

/// What one `invalidate_everywhere` did, for the trace.
#[derive(Debug, Default)]
pub(crate) struct Invalidated {
    /// Jits asked to drop their translations.
    pub jits: usize,
    /// Of those, the ones executing translated code, which were waited for.
    pub running: usize,
    /// How long that wait took.
    pub waited: std::time::Duration,
}

const MEMBARRIER_CMD_GLOBAL: i64 = 1;
const MEMBARRIER_CMD_PRIVATE_EXPEDITED: i64 = 8;
const MEMBARRIER_CMD_REGISTER_PRIVATE_EXPEDITED: i64 = 16;
const SYS_MEMBARRIER: i64 = 324;

static MEMBARRIER_EXPEDITED: OnceLock<bool> = OnceLock::new();

fn membarrier_register() {
    MEMBARRIER_EXPEDITED.get_or_init(|| {
        // SAFETY: plain syscall.
        unsafe { syscall(SYS_MEMBARRIER, MEMBARRIER_CMD_REGISTER_PRIVATE_EXPEDITED, 0, 0) == 0 }
    });
}

fn membarrier() -> Result<(), String> {
    let cmd = if *MEMBARRIER_EXPEDITED.get().unwrap_or(&false) {
        MEMBARRIER_CMD_PRIVATE_EXPEDITED
    } else {
        MEMBARRIER_CMD_GLOBAL
    };
    // SAFETY: plain syscall.
    if unsafe { syscall(SYS_MEMBARRIER, cmd, 0, 0) } != 0 {
        return Err(format!("membarrier({cmd}) failed: {}", std::io::Error::last_os_error()));
    }
    Ok(())
}

/// The guest stack a thread runs on: Cordial's own, or one the guest
/// supplied through `pthread_attr_setstack`.
enum Stack {
    Owned(Mapping),
    Given(u64, usize),
}

impl Stack {
    fn addr(&self) -> u64 {
        match self {
            Stack::Owned(m) => m.addr(),
            Stack::Given(a, _) => *a,
        }
    }
    fn len(&self) -> usize {
        match self {
            Stack::Owned(m) => m.len(),
            Stack::Given(_, n) => *n,
        }
    }
}

/// What the next guest thread made on this host thread gets for a stack.
#[derive(Clone, Copy, Debug)]
pub enum StackSpec {
    /// A Cordial-allocated stack of this many bytes.
    Size(usize),
    /// The guest's own memory, `[addr, addr + len)`.
    Given(u64, usize),
}

/// A thread that has run guest code: its host `pthread_t`, kernel tid, and
/// guest stack `[low, high)`.
#[derive(Clone, Copy, Debug)]
pub struct ThreadInfo {
    pub pthread: u64,
    pub tid: i32,
    pub stack: (u64, u64),
}

static THREADS: Mutex<Vec<ThreadInfo>> = Mutex::new(Vec::new());

/// Every thread with live guest state, in the order they first entered the
/// guest.
pub fn guest_threads() -> Vec<ThreadInfo> {
    THREADS.lock().unwrap().clone()
}

/// The guest stack of the thread whose host `pthread_t` is `pthread`, if it
/// has entered the guest.
pub fn guest_stack_of(pthread: u64) -> Option<(u64, u64)> {
    THREADS.lock().unwrap().iter().find(|t| t.pthread == pthread).map(|t| t.stack)
}

/// Sets the stack the calling host thread's guest state will be built with.
/// Must come before the thread's first guest call; ignored after it.
pub fn set_thread_stack(spec: StackSpec) {
    NEXT_STACK.with(|s| s.set(Some(spec)));
}

/// One host thread's guest state.
struct GuestThread {
    rt: Arc<Runtime>,
    // Drop order matters: the Jits hold pointers to `tpidr`. Boxed because
    // each JitBox's address is the `user` pointer its Jit calls back with,
    // and it must not move when the Vec grows.
    #[allow(clippy::vec_box)]
    jits: Vec<Box<JitBox>>,
    depth: usize,
    stack: Stack,
    /// TPIDR_EL0's value, shared by every Jit on the thread since they are
    /// all the same guest thread. Points at `tls`.
    tpidr: Box<u64>,
    /// bionic's arm64 TCB: nine slots, TPIDR_EL0 at slot 0, the stack
    /// protector's guard in slot 5 (`tls_defines.h`), then the thread's
    /// pthread key data (`keys.rs`), `keys::TLS_WORDS` in all.
    tls: Box<[u64]>,
    counts: Arc<Counts>,
}

impl GuestThread {
    fn new(rt: Arc<Runtime>) -> Box<GuestThread> {
        let mut tls = vec![0u64; crate::keys::TLS_WORDS].into_boxed_slice();
        tls[5] = rt.opts.stack_guard;
        // bionic's TLS_SLOT_THREAD_ID holds the thread's pthread_internal_t.
        // There is none for a guest thread; the host pthread_t is the one
        // identity it has, and it is at least unique and stable.
        // SAFETY: plain libc call.
        tls[1] = unsafe { pthread_self() };
        let tpidr = Box::new(tls.as_mut_ptr() as u64);
        let stack = match NEXT_STACK.with(|s| s.take()) {
            Some(StackSpec::Given(a, n)) => Stack::Given(a, n),
            Some(StackSpec::Size(n)) => Stack::Owned(Mapping::stack(n.max(16 << 10))),
            None => Stack::Owned(Mapping::stack(rt.opts.stack_size)),
        };
        // SAFETY: plain libc calls.
        let (pthread, tid) = unsafe { (pthread_self(), gettid()) };
        THREADS.lock().unwrap().push(ThreadInfo {
            pthread,
            tid,
            stack: (stack.addr(), stack.addr() + stack.len() as u64),
        });
        Box::new(GuestThread {
            stack,
            counts: rt.new_counts(),
            rt,
            jits: Vec::new(),
            depth: 0,
            tpidr,
            tls,
        })
    }
}

impl Drop for GuestThread {
    fn drop(&mut self) {
        // SAFETY: plain libc call.
        let me = unsafe { pthread_self() };
        THREADS.lock().unwrap().retain(|t| t.pthread != me);
    }
}

extern "C" {
    fn pthread_self() -> u64;
    fn gettid() -> i32;
    fn syscall(nr: i64, ...) -> i64;
}

thread_local! {
    static THREAD: RefCell<Option<Box<GuestThread>>> = const { RefCell::new(None) };
    /// The last 64 stubs this thread called, with the guest PC of each,
    /// for `dump_svc_ring`.
    static SVC_RING: RefCell<([(u32, u64); 64], usize)> = const { RefCell::new(([(0, 0); 64], 0)) };
    static NEXT_STACK: std::cell::Cell<Option<StackSpec>> = const { std::cell::Cell::new(None) };
    static LAST_FAULT: RefCell<Vec<FaultContext>> = const { RefCell::new(Vec::new()) };
    /// Start times of the stub calls in progress on this thread, innermost
    /// last, for `Options::profile`.
    static SVC_CLOCK: RefCell<Vec<std::time::Instant>> = const { RefCell::new(Vec::new()) };
}

/// Pauses the enclosing stub call's clock while guest code it called runs,
/// and restarts it when that returns.
struct PauseSvcClock<'a>(&'a Runtime);

impl<'a> PauseSvcClock<'a> {
    fn new(rt: &'a Runtime) -> Option<PauseSvcClock<'a>> {
        if !rt.opts.profile {
            return None;
        }
        SVC_CLOCK.with(|c| {
            let c = c.borrow();
            let t = c.last()?;
            rt.stats.svc_nanos.fetch_add(t.elapsed().as_nanos() as u64, Ordering::Relaxed);
            Some(PauseSvcClock(rt))
        })
    }
}

impl Drop for PauseSvcClock<'_> {
    fn drop(&mut self) {
        let _ = self.0;
        SVC_CLOCK.with(|c| {
            if let Some(t) = c.borrow_mut().last_mut() {
                *t = std::time::Instant::now();
            }
        });
    }
}

/// The guest's state when the most recent outermost call on this thread
/// stopped with a fault: one context per re-entry depth the fault passed
/// through, innermost first.
pub fn last_fault_context() -> Vec<FaultContext> {
    LAST_FAULT.with(|f| f.borrow().clone())
}

/// Runs `f` on this thread's guest TLS block (`keys::TLS_WORDS` words from
/// TPIDR_EL0), if the thread has entered the guest of `rt`.
pub(crate) fn with_thread_tls<R>(rt: &Runtime, f: impl FnOnce(*mut u64) -> R) -> Option<R> {
    THREAD.with(|t| {
        let mut t = t.borrow_mut();
        let g = t.as_mut().filter(|g| g.rt.id == rt.id)?;
        Some(f(g.tls.as_mut_ptr()))
    })
}

/// This thread's guest stack, `[low, high)`, once it has entered the guest.
pub fn thread_guest_stack() -> Option<(u64, u64)> {
    THREAD.with(|t| t.borrow().as_ref().map(|g| (g.stack.addr(), g.stack.addr() + g.stack.len() as u64)))
}

fn capture_fault(raw: *mut CgJit, stack: (u64, u64)) -> FaultContext {
    // SAFETY: the Jit has halted and is not executing.
    let (pc, lr, sp, mut fp) = unsafe {
        (cg_jit_get_pc(raw), cg_jit_get_x(raw, 30), cg_jit_get_sp(raw), cg_jit_get_x(raw, 29))
    };
    let mut ctx = FaultContext { pc, lr, sp, fp, frames: Vec::new() };
    // Only frames inside this thread's guest stack, strictly upwards, and at
    // most 32 of them: a frame pointer the guest was using as a scratch
    // register must end the walk, not fault it.
    while fp >= stack.0 && fp + 16 <= stack.1 && fp % 8 == 0 && ctx.frames.len() < 32 {
        // SAFETY: inside the mapping, checked above.
        let (next, ret) = unsafe { ((fp as *const u64).read(), ((fp + 8) as *const u64).read()) };
        if ret == 0 {
            break;
        }
        ctx.frames.push(ret);
        if next <= fp {
            break;
        }
        fp = next;
    }
    ctx
}

fn thread_for(rt: &Arc<Runtime>) -> *mut GuestThread {
    THREAD.with(|t| {
        let mut t = t.borrow_mut();
        let reuse = matches!(&*t, Some(g) if g.rt.id == rt.id);
        if !reuse {
            if let Some(g) = &*t {
                assert_eq!(g.depth, 0, "a second Runtime entered on a thread already running guest code");
            }
            *t = Some(GuestThread::new(rt.clone()));
        }
        &mut **t.as_mut().unwrap() as *mut GuestThread
    })
}

/// Calls the guest function at `pc` with up to eight integer and eight
/// vector arguments, on this thread's Jit at the current re-entry depth.
pub fn guest_call(rt: &Arc<Runtime>, pc: u64, ints: &[u64], vecs: &[[u64; 2]]) -> Result<Returned, Fault> {
    assert!(ints.len() <= 8 && vecs.len() <= 8, "use guest_call_args for stack arguments");
    guest_call_raw(rt, pc, ints, vecs, &[])
}

/// Calls the guest function at `pc` with arguments typed by `args`, placed
/// as AAPCS64 places them: integers in x0..x7 and floats in v0..v7 while
/// those last, then each in its own 8-byte stack slot in argument order.
pub fn guest_call_args(rt: &Arc<Runtime>, pc: u64, args: &[(Ty, u64)]) -> Result<Returned, Fault> {
    let (mut ints, mut vecs, mut stack) = (Vec::new(), Vec::new(), Vec::new());
    for &(ty, v) in args {
        if ty.is_fp() {
            let bits = if ty == Ty::F32 { v & 0xffff_ffff } else { v };
            if vecs.len() < 8 { vecs.push([bits, 0]) } else { stack.push(bits) }
        } else {
            let v = ty.extend(v);
            if ints.len() < 8 { ints.push(v) } else { stack.push(v) }
        }
    }
    guest_call_raw(rt, pc, &ints, &vecs, &stack)
}

fn guest_call_raw(rt: &Arc<Runtime>, pc: u64, ints: &[u64], vecs: &[[u64; 2]], stack_args: &[u64])
    -> Result<Returned, Fault>
{
    let g = thread_for(rt);
    // SAFETY: `g` is this thread's own GuestThread, boxed so it does not
    // move. Re-entrant calls on the same thread reach it again through
    // THREAD, so no reference to it is held across `cg_jit_run`; only the
    // raw pointer and the boxed JitBox, which does not move either.
    let (raw, jb, sp, bounds) = unsafe {
        let t = &mut *g;
        let depth = t.depth;
        if t.jits.len() == depth {
            let tpidr: *mut u64 = &mut *t.tpidr;
            let low = t.stack.addr();
            t.jits.push(JitBox::new(rt, tpidr, low, t.counts.clone()));
        }
        let top = if depth == 0 {
            t.stack.addr() + t.stack.len() as u64
        } else {
            cg_jit_get_sp(t.jits[depth - 1].raw) & !15
        };
        let sp = (top - 8 * stack_args.len() as u64) & !15;
        if depth == 0 {
            LAST_FAULT.with(|l| l.borrow_mut().clear());
        }
        let jb: *const JitBox = &*t.jits[depth];
        t.depth += 1;
        ((*jb).raw, jb, sp, (t.stack.addr(), t.stack.addr() + t.stack.len() as u64))
    };
    struct Depth(*mut GuestThread);
    impl Drop for Depth {
        fn drop(&mut self) {
            // SAFETY: as above; restores the depth taken on entry.
            unsafe { (*self.0).depth -= 1 };
        }
    }
    let _depth = Depth(g);
    let _pause = PauseSvcClock::new(rt);

    // SAFETY: `raw` is not executing -- it is the Jit for a depth nothing
    // else on this thread is using. The stack slots written are below every
    // live frame on this guest stack.
    unsafe {
        for (i, &w) in stack_args.iter().enumerate() {
            ((sp + 8 * i as u64) as *mut u64).write(w);
        }
        for (i, &v) in ints.iter().enumerate() {
            cg_jit_set_x(raw, i as u32, v);
        }
        for (i, v) in vecs.iter().enumerate() {
            cg_jit_set_v(raw, i as u32, v);
        }
        cg_jit_set_x(raw, 30, rt.ret_stub());
        // A frame chain that ends here, so a backtrace stops at the host.
        cg_jit_set_x(raw, 29, 0);
        cg_jit_set_sp(raw, sp);
        cg_jit_set_pc(raw, pc);
        loop {
            (&*jb).shared.done.store(INVALIDATIONS.load(Ordering::Acquire), Ordering::Release);
            (*jb).enter();
            let hr = cg_jit_run(raw);
            (*jb).leave();
            if hr & HALT_FAULT != 0 {
                let f = (*jb).fault.borrow_mut().take();
                let ctx = capture_fault(raw, bounds);
                LAST_FAULT.with(|l| l.borrow_mut().push(ctx));
                return Err(f.expect("halted for a fault without recording one"));
            }
            if hr & HALT_RETURNED != 0 {
                break;
            }
        }
        let mut v0 = [0; 2];
        cg_jit_get_v(raw, 0, &mut v0);
        Ok(Returned { pc: cg_jit_get_pc(raw), x0: cg_jit_get_x(raw, 0), x1: cg_jit_get_x(raw, 1), v0 })
    }
}

static TRACE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Prints every stub call from here on, with the calling thread and its
/// first two arguments, to stderr. A diagnostic, for a window of a run.
pub fn set_trace(on: bool) {
    TRACE.store(on, Ordering::Relaxed);
}

fn dump_svc_ring(rt: &Runtime) {
    SVC_RING.with(|r| {
        let r = r.borrow();
        for i in 0..64 {
            let (id, pc) = r.0[(r.1 + i) % 64];
            // An empty slot is (0, 0); id 0 with a PC is a raw syscall.
            if pc == 0 {
                continue;
            }
            let name = if id == 0 {
                "svc #0 (raw syscall)".to_string()
            } else {
                rt.entry(StubId(id)).map_or_else(|| id.to_string(), |e| e.name.clone())
            };
            eprintln!("  svc {name} from {pc:#x}");
        }
    });
}

/// How many guest calls are running on this thread: 1 inside a stub called
/// from an outermost guest call, more inside a callback's callee.
pub fn thread_depth() -> usize {
    THREAD.with(|t| t.borrow().as_ref().map_or(0, |g| g.depth))
}

/// How many Jits this thread has built, i.e. its deepest re-entry so far.
pub fn thread_jit_count() -> usize {
    THREAD.with(|t| t.borrow().as_ref().map_or(0, |g| g.jits.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(name: &str) -> Entry {
        Entry { name: name.into(), handler: Box::new(|_| Ok(())) }
    }

    /// The registration half of ADR-053's first condition: nothing outside
    /// the stub page, and nothing inside it but the next unused slot, can be
    /// given a handler. `register` and `register_code` both go through
    /// `install`, so this is every way a handler is attached.
    #[test]
    fn a_handler_is_refused_anywhere_but_the_next_stub_slot() {
        let rt = Runtime::new(Options::default());
        let used = rt.register("used", Box::new(|_| Ok(())));
        let (page, end) = rt.stub_page();
        let next = rt.stub_addr(StubId(rt.next_id.load(Ordering::Relaxed)));
        let host_fn = a_handler_is_refused_anywhere_but_the_next_stub_slot as *const () as u64;
        let heap = Box::new(0u64);
        let _w = rt.stub_write.lock().unwrap();
        for (what, at) in [
            ("a host function", host_fn),
            ("a heap address", &*heap as *const u64 as u64),
            ("just below the page", page - STUB_BYTES),
            ("slot 0, the raw syscall's id", page),
            ("an occupied slot", used),
            ("the middle of the next slot", next + 4),
            ("a free slot past the next", next + STUB_BYTES),
            ("the svc #RET stub", rt.ret_stub()),
            ("the end of the page", end),
        ] {
            let r = rt.install(at, 1, named(what));
            assert!(r.is_err(), "{what} ({at:#x}) was accepted as {r:?}");
        }
        assert_eq!(rt.stub_name(used).as_deref(), Some("used"), "a refusal replaced an entry");
        let id = rt.install(next, 1, named("next")).expect("the next slot is accepted");
        assert_eq!(rt.stub_addr(id), next);
        assert_eq!(rt.stub_name(next).as_deref(), Some("next"));
    }
}
