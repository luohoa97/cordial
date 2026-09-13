# intoxicated — FreeBSD port notes (session handoff)

Engine base: **Roblox Android x86_64 `2.721.1108`** (current enough; internals stable across point releases).
Host: FreeBSD 14.4-RELEASE, clang 19, RTX 4070 Ti (Vulkan 1.4 + GLES2 native), X11.

## What works (verified)
- `cargo build -p cordial-runtime --release` → **native FreeBSD binary** `target/release/cordial-run`
  (`ELF … for FreeBSD 14.4`, not linuxulator).
- `--gl-probe`: **GLES2 live via NVIDIA** (`OpenGL ES 3.2 NVIDIA 580.142`), pixel readback OK.
- Bionic linker **loads + relocates the 116MB `libroblox.so`** and runs its C++ static initializers.
- All fixes are on branch `freebsd-port`, commit `c515f53`, cfg-gated to FreeBSD (Linux build intact).

## Reproduce the current stop
```sh
cd ~/intoxicated
# raw APK objects (cordial uses its OWN bionic linker, wants unpatched libs):
./target/release/cordial-run --lib-dir .roblox-libs/lib/x86_64 --gl-probe
```
Crashes **SIGBUS** during static init.

## The bug (well-localized)
- Fault: `movq (%rax,%r10,8),%rdx` at **libroblox file offset `0x1f2487f`**, `rax = 0x15ff000000c1bf50`
  (garbage; bytes contain an `ff 15` opcode → a pointer read out of code/garbage). Deterministic across ASLR.
- The faulting function (offset `0x1f24860`):
  ```
  mov 0x50f7f09(%rip),%rax   # VA 0x701c770   -> rax = P (a global pointer)
  mov (%rax),%rax            # rax = *P       = 0x15ff…c1bf50 (garbage first field)
  test %rax,%rax ; je …      # non-null, so continue
  loop r10=0..0x2710(10000): movq (%rax,%r10,8) ; test ; movb (%rdi) …  <-- SIGBUS at r10=0
  ```
  Shape = walking a **~10,000-entry table of pointers, reading a char per entry** → almost certainly
  **Roblox's reflection registry** (classes/properties), populated by static initializers across many TUs.
- Global `VA 0x701c770`: **read in exactly 2 places, never written by any instruction**; no dynamic
  relocation on it. Sits at the `.init_array`(VA 0x7015af0..0x701c4d0) / `.got`(VA 0x7020700) boundary.
  NB: for this lib **VA − file-offset = 0x8000** (don't conflate the two when dd/readelf-x'ing).

### Leading hypothesis
Static-initialization **ordering**: the registry-walk runs before the constructors that fill the table
(or before the one that sets `P`). Bionic orders `.init_array` differently than glibc; on Linux this
apparently lands fine. Related: cordial `patches/0003-defer-libroblox-constructors.patch` adds
`mcpelauncher_defer_next_ctors` / `mcpelauncher_run_deferred_ctors` to the linker but leaves it
**unwired** — wiring it into the FreeBSD load path (map+relocate, set up dirs, THEN run ctors) is the
first thing to try.

### Next diagnostics (do with IDA — now staged)
1. Name the faulting function + the global at `0x701c770` (xref both read sites; the 2nd is `0x6a78464`).
2. Find which `.init_array` entry initializes `P` / the table, and where it sits in init order.
3. Check whether a `R_X86_64_RELATIVE` for `0x701c770` exists and whether the bionic linker applied it
   (walk `.rela.dyn` RELATIVE range vs the load base; mind the 0x8000 VA/offset delta).
4. If ordering: wire deferred ctors, or force the registry's initializer to run first.

## IDA (staged this session)
- `~/ida-stage/IDA Pro 9.3.260421/installers/ida-pro_93_x64linux.run` (631MB, executable).
- Next session: install under **linuxulator** (`/compat/linux` = Rocky Linux 9.7), then headless:
  `idat64 -B libroblox.so` (or `-A -S<script.py>`) to build the `.i64` and export the reflection area.
- Arch partition mounts read-only at `/mnt/arch` (ext2fs); IDA installer originals live at
  `/mnt/arch/home/pascal/torrents/…` (copy-only, never mount rw).

## Known remaining ABI work (after the init-order bug)
- **pthread_mutex** (bionic 4B vs FreeBSD pointer): implement via a pointer-keyed side-table of real
  FreeBSD mutexes (recursive) — sidesteps the layout mismatch entirely. Currently stubbed (denylist).
- **syscall**: `bionic_syscall` handles gettid/getpid/getrandom/clock_gettime/gettimeofday/sched_yield/
  nanosleep; `futex` returns 0 (no real `_umtx_op` translation yet); rest → `-ENOSYS`.
- **rwlock**: same treatment as mutex when it surfaces.

---

## Session 2 update — environ fixed, now a memory-category abort

**The init-order theory was wrong.** IDA (now installed + working headless on
FreeBSD under linuxulator; DB at `~/libroblox_2.721.so.i64`) named the crashing
global directly:
- `0x701c770` = **`environ_ptr`** (.got) → the `environ` data symbol.
The SIGBUS was the engine iterating `environ` (char**), which cordial resolved
to a **function stub** — because on FreeBSD `environ` lives in crt startup, not
`libc.so`, so cordial's host-libc `defines()` check rejects it.

### Fixes landed this session (past the SIGBUS)
- **`environ`**: data override → address of the real `environ` (bionic/mod.rs).
- **pthread_mutex**: real impl via a pointer-keyed side-table of recursive
  FreeBSD mutexes (`native/freebsd_libc_compat.c`), registered for
  `pthread_mutex_{init,lock,unlock,trylock,destroy}`. Sidesteps bionic's 4-byte
  vs FreeBSD's pointer-sized `pthread_mutex_t`.
- **`__open_2`** (FORTIFY open) and **`prctl`** registered.

Result: **`no stubs were called`** — every symbol resolves. `--game-activity`
brings up the full framework (audio, accessibility, **X11 backend**, 700-symbol
table), loads libroblox, runs deep init (71 mutex locks) — then `abort()`.

### The current wall — "invalid memory category" HardAssert
Backtrace (via lldb + IDA symbolication): the engine calls `abort()` from
Roblox's **per-allocation memory-accounting** hook `sub_1F24103`:
- category array base `0x75a8bc0`, 64-byte entries, valid range `[0, 1024)`.
- it aborts because a **category descriptor's ID field `[descriptor+8]` is ≥ 1024**.
- descriptor comes from `sub_1F24310` → `sub_2C18D80(0x706d228)` (a guarded static).
So a memory-category descriptor has a garbage/invalid ID during early static init.

### Next diagnostics
1. Trace `sub_2C18D80` (guarded-static getter) + the descriptor at `0x706d228`:
   how is the category ID (`+8`) assigned? Likely a global registration counter.
2. Is that counter in .bss (should be 0) or set by a constructor that hasn't run
   / ran wrong on FreeBSD? Check for an uninitialized global or a `__cxa_guard`
   issue in the bionic-loaded lib.
3. Suspect list: any remaining mis-resolved **data** symbol (audit like environ),
   or bionic TLS setup under cordial's linker on FreeBSD.

### IDA headless recipe (works)
```sh
cd ~/ida93 && TVHEADLESS=1 HOME=/home/pascal TERM=xterm \
  ./idat -A -S"script.idc" -Lout.log ~/libroblox_2.721.so.i64 </dev/null
```
IDC helpers: `get_name(ea)`, `get_qword(ea)`, `get_segm_name(ea)`,
`get_func_name(ea)`, `get_first_dref_to/from`, `get_strlit_contents(ea,-1,0)`.
(IDAPython needs libpython3.14 + `idapyswitch`; not set up — IDC is enough.)

---

## Session 2 (cont.) — the abort is Roblox's allocator, not memory categories

Corrected the earlier "invalid memory category" guess (that string was in a
nearby function; breakpoints proved it never runs). Real chain, via lldb
(break `mcpelauncher_linker_notifylldb` → read base in `rsi` → break
`base+off`) + IDA symbolication:

- `abort` is called at **`0x2c18f30`**, inside `sub_2C18D80` — a **per-thread
  storage getter** (lazy-init via `pthread_once` + a mutex-locked section).
- It aborts because `call sub_1F24322` (a **per-thread pool allocator**)
  returned **NULL** for a 128-byte request during first-time init.
- `sub_1F24322`: size ≤ 1024 → pool freelist at `pool[0xe8 + sizeclass]`
  (default pool global `0x7012a00`); empty freelist → `jmp sub_1F251FA`.
- `sub_1F251FA`: the **slab backing allocator** over pool `0x7012a00`; it
  returns NULL → the whole chain fails. So Roblox's own allocator can't get
  backing memory at early init on FreeBSD.

### Also fixed this session (real latent bug, kept)
- **`pthread_once`**: cordial forwarded bionic's 4-byte once-control straight to
  FreeBSD's `pthread_once`, whose `pthread_once_t` is a *struct* (int + mutex
  ptr) — so bionic's zero-init looked "already run" and the init routine was
  SKIPPED. Now implemented directly on the 4-byte slot (2 = done) in
  `pthread.rs::once`, cfg-gated to FreeBSD. Did NOT clear this abort (the
  allocator failure is upstream of it), but it was breaking every
  `pthread_once`-guarded init silently.

### Next: why Roblox's slab allocator returns NULL at init
1. Disassemble `sub_1F251FA` fully + its callee `sub_6A80FB1` — find the
   backing memory source (mmap? sbrk? a global arena?).
2. If mmap: check flags/addr FreeBSD rejects under the bionic linker.
3. Check `sysconf` values cordial returns (`bionic_sysconf`) — a wrong
   `_SC_PAGESIZE`/`_SC_PHYS_PAGES` can make the arena sizing compute to 0.
4. Pool state global `0x7012a00`: is its init constructor running? (partially
   set up — freelist head non-null but empty — so arena refill is the gap.)

---

## Session 2 (cont. 2) — sysconf page-size + sysinfo fixed; allocator still NULLs

Two more real blockers found and fixed (the abort MOVED past each):
- **`sysconf` page size**: cordial's table maps bionic _SC_* → *glibc* numbers,
  but FreeBSD's differ from glibc's too (bionic _SC_PAGESIZE 39 → glibc 30, but
  FreeBSD's is **47**). The allocator asked the page size, got a non-power-of-two,
  aborted (cordial's own comment predicted exactly this). Fixed: `bionic_sysconf`
  maps the critical selectors (PAGESIZE 47, NPROCESSORS 57/58, PHYS_PAGES 121,
  CLK_TCK 3) straight to FreeBSD numbers, cfg-gated. mod.rs.
- **`sysinfo`**: Linux-only, fills `struct sysinfo` (totalram etc.); the allocator
  sizes arenas from it. Stub → garbage → abort. Implemented via sysctl
  (`hw.physmem`, `vm.stats.vm.v_free_count`) in `native/freebsd_libc_compat.c`.
  NB: bionic `struct sysinfo` is **104 bytes** on LP64 (trailing `_f` pad is 0);
  an oversized struct here overruns the caller and trips its stack canary.

### Still stuck: Roblox's pool/slab allocator returns NULL at first alloc
After both fixes the abort returns to the SAME spot (`0x2c18f30` in
`sub_2C18D80`): `sub_1F24322` (per-thread pool) → `sub_1F251FA` (slab) →
`sub_6A80FB1` still yields NULL for a 128-byte request. `sub_6A80FB1` uses
`arc4random_buf`/`clock_gettime`/`pthread_getspecific` (all resolve fine); it
manages existing slabs rather than creating them, so the arena/slab that should
back it was never set up. Pool global is `0x7012a00` (partially initialised:
freelist head non-null but empty).

### Next
1. Runtime-trace `sub_1F251FA`/`sub_6A80FB1`: break at `base+0x1f251fa`, step to
   the NULL return, see which call/branch fails.
2. Find where pool `0x7012a00`'s slabs are first reserved (the arena mmap) — it
   was NOT the file-mapping mmap at `0x23b40ad` (that's JNI). Look for an
   anonymous mmap in the pool-init path.
3. Consider whether cordial's bundled mimalloc is meant to back this and the
   hookup differs on FreeBSD.

### Fixes committed this session (branch freebsd-port)
environ · pthread_mutex (side-table) · __open_2 · prctl · pthread_once
(FreeBSD once protocol) · sysconf page-size · sysinfo. Engine now loads and runs
deep static init before the allocator NULLs.

---

## Session 2 (cont. 3) — BREAKTHROUGH: mmap flag translation → the engine RUNS

ktrace showed the smoking gun: `mmap(...,0x4022,-1,0) → EINVAL`. The engine uses
**Linux MAP_* flag numbers**, which reach FreeBSD's mmap unchanged:
- Linux `MAP_ANONYMOUS=0x20` vs FreeBSD `MAP_ANON=0x1000`
- Linux `MAP_NORESERVE=0x4000` == FreeBSD `MAP_EXCL` (wrong meaning)
- anonymous maps passed `fd=0`; FreeBSD's MAP_ANON needs `fd=-1`.
So every anonymous allocation failed EINVAL → allocator got no memory.

Fix: `bionic_mmap` (native/freebsd_libc_compat.c) translates Linux→FreeBSD mmap
flags and forces fd=-1 for MAP_ANON; registered for `mmap`/`mmap64`. Also
registered the real `getauxval`.

### Result — Roblox's engine runs on FreeBSD (native)
`--game-activity` now reaches, with **no crash** (runs to the timer):
```
LOADED in 30ms (107.1 MB)
JNI_OnLoad returned JNI 1.6
nativeSetFilesDirectory/CacheDirectory ok · bootstrapTheApp installed
GameActivity.initializeNativeCode → GameActivity_register, SDK 33
ALooper_addFd(fd=14 ...) / ALooper_addFd(fd=16 ...)   <- Android event loop live
```

### Next: from "event loop running" to pixels
- It settles into ALooper; confirm a Vulkan/GLES3 surface is created and whether
  a window maps on X11 (watch for eglCreateWindowSurface / vkCreateSwapchain).
- `madvise` advice numbers differ Linux↔FreeBSD — translate if the engine trips.
- Remaining trivial stubs: `pthread_setname_np` (name a thread; harmless no-op).

### All fixes (branch freebsd-port)
environ · pthread_mutex side-table · pthread_once · __open_2 · prctl ·
sysconf page-size · sysinfo · **mmap flag translation** · getauxval.

---

## Session 2 (cont. 4) — a WINDOW opens; through the full native bootstrap

Chain of fixes past the event loop to a real window + full engine bootstrap:
- **profile lock**: `adopt_handed_lock` verified the handed fd via
  `/proc/self/fd` (absent on FreeBSD) -> fell back to re-locking -> collided
  with the shell's flock. Now verifies by fstat identity (dev+ino).
  (cordial-shell/src/profile.rs). Rebuild cordial-RUN too — it links the shell.
- **futex**: real `_umtx_op` WAIT/WAKE (was returning 0 = busy-spin).
- **mmap fd**: force `fd=-1` for `MAP_ANON` (FreeBSD rejects fd=0).
- **cond/mutex livelock (the big one)**: cordial's `pthread_cond_wait` handed
  FreeBSD's cond the *bionic* mutex, but bionic mutexes are backed by our
  side-table of real FreeBSD mutexes — so the wait never blocked/signalled ->
  ~2 threads spun on `clock_gettime` forever. Fix: `bionic_mutex_real()` exposes
  the side-table's real mutex; cond_wait/cond_timedwait translate to it.
- **struct stat**: bionic `struct stat` (Linux x86-64, ~144B) vs FreeBSD's
  (~224B). cordial's `s_stat`/`s_lstat` wrote the native struct into the
  engine's bionic buffer -> stack-canary trip in boost::filesystem::status.
  Added a `bionic_stat` translation + registered `fstat` (system_paths.cpp).

Result: window 1280x720 opens, and the engine runs its ENTIRE native bootstrap
(engine version, device info, refresh, battery, storage manager, and every
nativeSet*Directory / policy / assets / channel call = ok).

### Current wall — TaskScheduler vs flags (cordial-internal, documented)
`RBXCRASH: FatalRuntimeError (Can't initialize the TaskScheduler before flags
have been loaded)`. cordial delivers the client settings ("1281909 bytes cache")
but the engine doesn't register flags as loaded before TaskScheduler init. This
is the `nativeInitializeNativeFlags` / `onFlagsFailed` problem cordial's own
docs/analysis/flag-init.md documents as not-fully-solved (§6.3). Next: determine
if it's the same cordial bug or a FreeBSD-specific variant of the flag delivery.

---

## Session 2 (cont. 5) — PAST the TaskScheduler crash; deep threading spin remains

The TaskScheduler-vs-flags crash is an ORDERING race, not a parse failure:
- `sub_2380400` (TaskScheduler ctor) aborts if the "flags loaded" byte at VA
  `0x75a8250` is 0. It's set by `initClientSettingsAndroid` (`sub_2C2BC07`) on
  success ("SettingsLoad Success-Android", store `movb $1,[0x75a8250]` @0x2c2bfb6).
- Trigger: `LocalStorage::flush` (`sub_255B4A2`) on the MAIN thread touches the
  TaskScheduler while the flag parse runs on a WORKER thread (#14). On FreeBSD
  the main thread wins the race; on Linux the parse does.

**Fix that clears the crash: `CORDIAL_EARLY_SETTINGS=1`** — cordial delivers the
settings synchronously after constructors but before `initializeNativeCode`
(load.rs ~2196), so the flag is set before the race. (Deferred-ctors paths do
NOT work: post-ctors still races; `CORDIAL_DEFER_PAST_SETTINGS` SEGVs because
the parser reads constructor-initialised globals that don't exist yet.)

Required for defer paths at all: **patch `patches/0003` into the linker**
(`cd third_party/mcpelauncher-linker/bionic && patch -p1 < ../../../patches/0003-*.patch`)
— it was NOT applied; without it CORDIAL_DEFER_CTORS silently no-ops.

### New wall: the flag parse spin-waits (with EARLY_SETTINGS)
No crash, but hangs at "flags applied": ~2 threads busy-spin on `clock_gettime`
(923k calls/2s). The spinning thread is inside `initClientSettingsAndroid`
itself, polling a timestamp getter (`sub_2C4B8A2`, a `__cxa_guard` static) in a
timed loop — waiting on a condition (a parallel parse worker / a lock) that
never resolves on FreeBSD. Same class as the earlier cond/mutex livelock, one
layer deeper. mutex(now lock-free reads), cond(real-mutex translation), futex
(_umtx_op), once, sem all check out individually — so it's a subtle timing/
ordering interaction, or a TaskScheduler worker-wake primitive still off.

### This chunk's improvements (kept)
- mutex side-table: lock-free reads (append-only list) — the global guard was
  serialising every lock; the parse locks tens of thousands of times.
- patch 0003 applied to the linker (deferral now actually works).

---

## Session: crash-free to the event loop (Opus 4.8, commit a1f7b38)

Two ABI bugs fixed; the engine now runs natively with **no crashes** all the way
into its event loop and opens a 1280x720 window. It renders black because the
app-settings verdict fails (see below) — a cordial-upstream mystery, not a
FreeBSD ABI problem.

### Fix 1 — getauxval AT_ crosswiring
bionic passes *Linux* AT_* numbers; only 0..14 match FreeBSD. The killer:
Linux `AT_RANDOM=25` == FreeBSD `AT_HWCAP=25`. bionic's `__libc_init` asks
AT_RANDOM for a pointer to 16 stack-canary bytes, got a hwcap bitmask, and
dereferenced it → SIGSEGV (`rax=0x3ffff0`, fault `0x3ffff8` — the value is the
hwcap bits & 0x3ffff0 running through jemalloc's small-region math). Now
translate the numbers and hand AT_RANDOM a real 16-byte arc4random buffer.
`native/freebsd_libc_compat.c` getauxval().

### Fix 2 — bionic pthread_attr_t vs FreeBSD (the big one)
bionic `pthread_attr_t` is a by-value 56-byte struct; FreeBSD's is an opaque
`struct pthread_attr *`. On glibc both are by-value, so cordial forwarded attrs
untouched — correct on Linux, fatal on FreeBSD:
- `pthread_getattr_np`/`pthread_attr_getstack` were stubbed (left the bionic
  struct uninitialised); `pthread_attr_destroy` resolved to host libthr, which
  did `free(*attr)` = `free(bionic flags qword)` = `free(0xffffffff)` →
  jemalloc walked into unmapped memory (`_pthread_create`/`attr_destroy` on the
  stack right above the free frame gave it away).
Fix: implement the whole bionic `pthread_attr_*` family over bionic's layout
(`native/freebsd_libc_compat.c`, registered in `bionic/mod.rs`), AND translate a
bionic attr → a real FreeBSD attr inside `cordial_pthread_create`
(`native/thread_trace.cpp`, FreeBSD-only). Roblox creates its worker threads with
a configured bionic attr; without translation host `pthread_create` deref'd it
(`_pthread_create+274`, fault 0x31).

### Boot now reaches: initializeNativeCode → GameActivity_register →
webview protocol vocabulary → window placement (1280x720) → nativeRetryInit.

### The nativeEngineState_ state machine (retryInit assertion)
`nativeRetryInit` asserts `nativeEngineState_ ∈ {ReadyToBootstrap=1,
FailedAppSettings=0xb}` and segfaults the assert otherwise. Under EARLY_SETTINGS
cordial drives natives synchronously and calls retryInit while the state is still
`2` (an intermediate "processing" state) — hence the abort.

The state is real engine memory at `*(base+0x70811c8)->[0x38]->[0x10]` (i32) on
2.721 (base = `symbol("JNI_OnLoad") - 0x22addd7`). `CORDIAL_PROBE_STATE=1` reads
it; `CORDIAL_STATE_POLL_MS=<n>` polls before retryInit. An async worker flips the
state `2 → 0xb` within ~25 ms — so a short poll makes retryInit pass. **The
verdict is 0xb = FailedAppSettings, not 1 = ReadyToBootstrap.** These offsets are
2.721-specific magic, so the probe stays env-gated, not a default.

### Remaining wall (upstream, not FreeBSD): FailedAppSettings
`nativeInitClientSettings` returns 0, but the engine's async validator sets
`result->error` (`+0x8` of the settings-result object) non-null → state 0xb and
`onFlagsFailed`. cordial's own comment: "what the verdict actually tests is still
unknown" (docs/analysis/flag-init.md). Same wall on Linux. The window opens but
stays black because content won't load without a passing verdict.

- With globals run (`call_globals("late")` → `nativeGameGlobalInit`) execution
  *blocks* after activity-lifecycle at 32% CPU. `CORDIAL_NO_GLOBAL_INIT=1` skips
  it and advances into the app-bridge/init-params/webview-vocab region (still no
  content, FailedAppSettings). Both are bootstrap-sequencing puzzles downstream
  of the settings verdict.

### Next
Crack the FailedAppSettings verdict — trace what sets `result->error` in the
async settings handler (the failure store is at file offset `0x2c5cd4e`,
`movl $0xb,0x10(%rax)`; entered when `[result+0x8]` != 0). That's the gate to a
non-black window. It is a cordial-wide problem, so a fix helps Linux too.

---

## Session 2: the GameGlobalInit block is WAKE-starvation, not a shim bug

With retryInit passing (via the state poll), the with-globals path blocks in
`nativeGameGlobalInit` (`call_globals("late")`, load.rs:3792). `CORDIAL_NO_GLOBAL_INIT=1`
skips it and reaches the app-bridge region, but StartLuaAppDM needs globals, so
that is not a real fix.

### Diagnosis (ktrace + CORDIAL_TRACE_FUTEX)
The main thread spins ~195k/s on one futex:
```
_umtx_op(addr, UMTX_OP_WAIT_UINT_PRIVATE, val=0, tsz=24, &umtx_time) -> ETIMEDOUT
[futex] WAIT addr=0x..3c8cc cmd=9 to={13732.369} flags=1 clk=4 | mono=13745.296
```
cmd=9 = FUTEX_WAIT_BITSET, an *absolute* monotonic deadline ~13s in the PAST that
the engine passes over and over → instant ETIMEDOUT each time. Our WAIT/WAKE
translation is CORRECT: the deadline was a legitimate future time when first
computed; the engine is busy-re-waiting a fixed, now-expired deadline in a
`while(!ready)` loop.

The decisive fact: that spin address receives **zero WAKEs**, and the whole run
issues only **15 WAKEs total**. So it is not a lost wakeup — *nothing ever
signals the condition*. The engine's worker threads sit idle-blocked (many in
infinite `to=NULL` FUTEX_WAIT_BITSET). `nativeGameGlobalInit` posts work and
waits for a completion that never arrives because the engine's own job system
(TaskScheduler) is not pumping.

### Interpretation
This chains to the same root as FailedAppSettings: the TaskScheduler ("Can't
initialize the TaskScheduler before flags have been loaded") depends on the
flags/settings verdict, and with the verdict = FailedAppSettings the job system
never runs posted work → GameGlobalInit's completion never signals → block.
So the settings/flags verdict is very likely the single upstream root gating
everything downstream (black screen, GameGlobalInit block, no content). Cracking
the verdict is the lever; the futex/pthread layer underneath is sound.

### Kept this session
- `native/system_paths.cpp`: redirect `/proc` → `/compat/linux/proc` (linprocfs)
  on FreeBSD, so the engine's Linux-format /proc reads (meminfo, self/maps,
  status) get the layout they expect. Did NOT change the verdict, but it is a
  real correctness fix and is needed for the anti-cheat's process introspection
  later (the "you need linprocfs" tip).
- `CORDIAL_TRACE_FUTEX=1`: env-gated futex WAIT/WAKE trace (cached getenv; safe
  in the hot path). This is what localised the WAKE-starvation and will be the
  tool for the next person on the scheduler question.

---

## Session 3: the sync layer is PROVEN correct; the block is engine-init logic

Deep futex forensics on the GameGlobalInit hang (CORDIAL_TRACE_FUTEX now tags
every WAIT/WAKE with tid). Findings:

- 21 threads; 16 run the same start routine = the TaskScheduler thread pool.
- Symbolicated the blocked main-thread stack (IDA base + robx.dis):
  `nativeGameGlobalInit` -> engine init internals -> a bionic `__futex` wrapper
  (SYS_futex=0xca, op=0x89 FUTEX_WAIT_BITSET|PRIVATE) -> our do_futex.
- Main thread (tid 116683) ends on `WAIT addr=0x…dcc8c to=NULL` — an *infinite*
  wait for its posted init task to complete. No WAKE is ever sent to that addr.
- The pool workers park correctly. Traced a full cycle on one shared address:
  `WAIT(park) -> WAKE(dispatch) -> WAIT(re-park)` — the worker IS woken, runs,
  and re-parks. So WAIT/WAKE delivery is correct; not a lost wakeup.
- The one busy worker spins on an *expired absolute deadline* poll (its job never
  arrives), which is a symptom (no work), not a translation bug — Linux would do
  the same with no work.

Conclusion: **our futex/pthread/clock layer is mechanically correct.** After an
initial burst (~2 dispatches, ~15 total wakes) the engine's scheduler stops
dispatching and the whole process goes idle — main thread waiting on a completion
that the engine's own init logic never produces. This is engine-init logic, not
an ABI/sync bug on our side.

### What this rules in / out
- NOT our sync primitives (proven: WAIT/WAKE/clock all behave).
- Either (a) a cascade from the FailedAppSettings verdict — some subsystem that
  GameGlobalInit waits on only initialises on ReadyToBootstrap — or (b) a
  genuinely FreeBSD-specific engine-logic divergence. cordial lore says Linux
  reaches "app ready: Landing" despite the same onFlagsFailed, which points at
  (b), but that cannot be confirmed from the FreeBSD side alone.

### The unblock path
Get a Linux cordial baseline (the Arch partition can build/run cordial against
the same engine) and A/B: does nativeGameGlobalInit block there too? If it
returns on Linux, diff the thread/futex behaviour at that call to find the
FreeBSD-specific divergence. If it blocks on Linux too, the settings verdict is
the shared root and the fight moves there. Either way the sync layer underneath
is not the suspect.

### Minor note (not the bug, but ABI-imperfect)
do_futex's FUTEX_WAKE returns the *requested* count (`val`, up to INT_MAX for a
broadcast) rather than the actual number woken, because FreeBSD `_umtx_op` WAKE
does not report a count. Harmless for the mutex/cond/semaphore callers that
ignore it (all seen here), but not Linux-faithful for any caller that uses it.

### Force-state experiment (negative)
CORDIAL_FORCE_STATE=<n> overwrites nativeEngineState_ before the init chain. Forcing
1 (ReadyToBootstrap) lets retryInit pass but does NOT unblock the downstream
GameGlobalInit — same hang. So a live `state == ReadyToBootstrap` check is not the
gate. Caveat: forcing the flag late does not redo the settings-SUCCESS *processing*
that would have initialised subsystems earlier, so this does not fully exonerate the
verdict; it only rules out a late state-flag check. Three cheap decisive experiments
now negative: /proc→linprocfs, pre-pump, force-state. The block is robustly inside
GameGlobalInit's engine-init logic and needs a Linux baseline to isolate further.

### Named the block: `wait_until(lock, never())` / "Failed to await Condition"
Disassembling the main thread's deepest engine frame (offset 0x277d2b0 → its
callee chain to the futex) turned up the assert string
`!wait_until(lock, never()) && "Failed to await Condition"`. So GameGlobalInit is
sitting in a C++ `condition_variable::wait_until(lock, never())` on a Roblox
"Condition" wrapper — an infinite wait (matches the `to=NULL` futex) for a
cross-thread notify that never arrives. The producer that should notify is on a
thread that never runs / never reaches the notify. Identifying that producer
precisely needs the IDA db's real symbols (nearest-export names in robx.dis are
misleading here) or a Linux baseline. Grep target for the next session:
"Failed to await Condition" and the `wait_until`/`never()` Condition wrapper.

---

## Session 4 (IDA decompiler): ROOT CAUSE FOUND — getFlags() async ClientAppSettings fetch

Got IDAPython working (idapyswitch → Python 3.9; IDAPython was pointed at a
missing 3.14). Decompiled the whole GameGlobalInit block chain on an isolated
copy of the .i64. The complete causal chain, top to bottom:

1. cordial calls `nativeGameGlobalInit` from its own thread.
2. `sub_2339452` is a "run on the engine's main thread and wait" primitive: the
   engine spawns a dedicated thread `sub_2339A1E` named **"FunctionMarshaller"**
   (a task-queue loop: lock → `while(empty) cond_wait` → dequeue → run),
   recorded as `qword_7081868`. Since GameGlobalInit runs on a *different*
   thread, it posts a task to the FunctionMarshaller and waits (infinite,
   `wait_until(lock, never())`).
3. The FunctionMarshaller (thread #13) dequeues and runs the task, which is
   **`getFlags()`** (`sub_2C5CAF2`, FLog channel "NativeDM"). Confirmed by
   thread-stack walk: FM loop `+0x2339b26` → `getFlags +0x2c5ccac` →
   `sub_5FB52B8` → `sub_6780314` → `boost::condition_variable::wait`
   (`sub_23904A8`, `pthread_cond_wait` with the literal assert string
   "boost::condition_variable::wait failed in pthread_cond_wait").
4. `getFlags()` checks a "flags already loaded" string (`xmmword_7081250`); when
   empty it does an **async fetch of "ClientAppSettings" for "AndroidApp"**
   (strings in the function: `ClientAppSettings`, `getFlags: success = true,
   payload's size = {}`, `getFlags: success = false`) and blocks on a boost cv
   waiting for the result. The ONLY writer of the "loaded" string is getFlags's
   own completion path, so the first call always fetches.
5. On FreeBSD that fetch never completes → getFlags blocks → the FunctionMarshaller
   is stuck inside it → GameGlobalInit's posted task never runs → GameGlobalInit
   waits forever → black screen. The pool's ~16 worker threads sit idle.

**This is the single unified root of both the FailedAppSettings verdict and the
GameGlobalInit hang** — the thing cordial's own notes call "unknown."

### The critical disconnect
cordial delivers ClientAppSettings via `nativeInitClientSettings` (returns 0), but
that feeds a DIFFERENT internal path than the async fetch `getFlags()` awaits.
getFlags issues its own request and waits for a producer to fulfill a future
(`sub_65FA644` waits; result read at `*(future+264)`). That producer never runs
on FreeBSD.

### Next (the fix)
Find what fulfills getFlags()'s ClientAppSettings future — decompile `sub_65FA644`
and the request side to see whether it (a) calls OUT to a host/JNI callback cordial
should answer, or (b) dispatches an HTTP fetch to the engine's own network client
(which may be broken/unrouted on FreeBSD). Then have cordial satisfy that specific
request so getFlags takes the fast path. That unblocks GameGlobalInit and should
finally render.

### Tooling note
IDA headless on FreeBSD: `idapyswitch` → pick Python 3.9; IDAPython works, use it
via `idat -A -Sscript.py db.i64` (TVHEADLESS=1). Analyze a COPY of the .i64 —
never the user's original (it had live unpacked .id0/.id1 files). The Hex-Rays
decompiler is the key tool; nearest-export names in objdump are misleading.

### The producer never runs (no network, no I/O) — dispatch/marshalling deadlock
ktrace during the hang: ZERO connect/socket/sendto and ZERO file namei — the whole
process is idle. So getFlags's ClientAppSettings fetch is not a slow/hung network
call; the producer that should fulfill its boost future never even starts. The
"HttpClient" thread (`sub_23449DA`, `sub_22AD799("HttpClient")`, thread #15) is
alive but idle-blocked in libthr; the ~16 pool workers are parked.

Topology recap: cordial calls nativeGameGlobalInit from ITS OWN thread (not the
engine's FunctionMarshaller = `qword_7081868`), so `sub_2339452` marshals the work
onto the FM thread and waits. getFlags then runs ON the FM thread and dispatches
its own fetch — and whatever it dispatches to is never serviced. On Android,
GameGlobalInit is invoked FROM the engine's main thread, so `pthread_self() ==
qword_7081868` is true and it runs inline with no marshalling — which likely also
keeps getFlags's fetch on a thread that can service it.

### Two concrete fix directions for next session
1. **Run nativeGameGlobalInit on the engine's own main thread.** If cordial can
   post GameGlobalInit onto the FunctionMarshaller queue (or otherwise call it
   such that `pthread_self() == qword_7081868`), sub_2339452 runs it inline and the
   self-marshalling wait disappears. Investigate how the queue is fed
   (`sub_2C7E640` posts; `stru_70818A0`/`cond`/`xmmword_7081890` are the queue) —
   cordial may be able to enqueue GameGlobalInit itself.
2. **Short-circuit getFlags's fetch.** getFlags (`sub_2C5CAF2`) takes the fast path
   when its "loaded" string `xmmword_7081250` is non-empty. If cordial can make the
   engine believe ClientAppSettings is already resident (populate that state, or
   fulfill the pending future directly), getFlags returns without dispatching. The
   store `qword_72893D8` (filled by nativeInitClientSettings) is read by the Lua
   flag APIs but NOT consulted by getFlags's async path — that disconnect is the
   bug to close.

Either path unblocks GameGlobalInit and should finally render. The whole sync/ABI
layer beneath is proven sound; this is the last structural gap.

---

## Session 4 continued: FIX #1 VALIDATED — GameGlobalInit unblocked, DataModel runs!

CORDIAL_HIJACK_MARSHALLER=1 (overwrite qword_7081868 with cordial's own
pthread_self before call_globals) CONFIRMED the self-marshalling deadlock and blew
straight through the wall:

  [hijack] qword_7081868: 0x...b0010 -> 0x...f6010 (self)
  nativeGameGlobalInit ok (late)      <-- the hang is GONE
  nativeUpdateAdapterInit ok (late)
  app bridge initialised
  Lua app DataModel started           <-- the engine's DataModel is RUNNING
  startup recovery armed
  task scheduler foregrounded         <-- TaskScheduler running
  [cordial] app start as nobody signed in

So the root cause diagnosis was correct: cordial calling nativeGameGlobalInit off
the engine's FunctionMarshaller thread caused a self-marshalling deadlock in
getFlags(). Running it "inline" (by making pthread_self()==qword_7081868) fixes it.

The hijack is a hacky global overwrite (env-gated, kept as the validated proof).
The CLEAN fix is fix #1 proper: enqueue nativeGameGlobalInit onto the engine's
FunctionMarshaller queue so it runs on that thread natively, or restore
qword_7081868 immediately after the call to limit blast radius. Worth checking the
hijack doesn't misroute other marshalled work (it survived to DataModel start, so
minimal in practice, but restore-after is cleaner).

### New frontier (past the wall)
After "app start as nobody signed in" a NEW crash: null-pointer deref (fault 0x18,
rax=0) in engine sub_250667E+0x82, reached via a vtable call during DataModel/app
startup (frame #1 is a cordial trampoline, likely a worker/callback). Different bug
class from the deadlock — a null object during app bring-up. This is the next
thing to chase; we are now inside the actual app startup, far past the black-screen
wall.

### restore-after + next crash (AppBridge singleton null)
The hijack now RESTORES qword_7081868 immediately after call_globals (only
GameGlobalInit runs inline; later marshalled work routes to the real FM thread).
GameGlobalInit still completes, DataModel/TaskScheduler still start — so the new
crash is NOT a hijack side-effect.

New crash: `nativeAppBridgeV2StartAppWithParams` null-derefs the AppBridge singleton
at global 0x70b3c20 (`mov 0x18(%rax)` with rax=*(0x70b3c20)==0, in sub_250667E+0x82).
That singleton is created by sub_23CD346 (its only writer), reached via a chain that
dead-ends at sub_278D8E0 — i.e. it's a lazy get-or-create (call_once guard
byte_70B3958) triggered by a getter that something must call before StartApp. On
FreeBSD that trigger never fires, so StartApp derefs null. cordial calls
nativeAppBridgeV2InitWithParams ("app bridge initialised") but that does not
init this singleton. Next: find the getter that triggers sub_23CD346 and why it
isn't reached (likely another thread/event cordial doesn't drive, same family as the
FunctionMarshaller). We are now well inside app startup — DataModel + TaskScheduler
running — one null-subsystem away from a first frame.
