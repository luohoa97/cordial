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

---

## Session 5 (deep RE): getFlags deadlock fully mapped — it's a thread-model mismatch

Reversed the entire getFlags settings path. The deadlock is STRUCTURAL, not data:

- getFlags (`sub_2C5CAF2`) runs ON the FunctionMarshaller thread (GameGlobalInit
  marshals it there). Its slow path calls
  `sub_5FB512C("ClientAppSettings", &qword_72893D8, ...)` ->
  `sub_5FB52B8` -> sync lookup `sub_5FB3969` (a URL-fetch preparer with a
  once-guard `byte_73CFF88`, logs "[FLog::Output] settingsUrl: {}").
- Sync lookup misses -> async load `sub_5FB535B`, which extracts via a settings
  *provider* (`sub_2C5D182` -> vtable) and stores the ClientAppSettings entry into
  `qword_72893D8` with `sub_23539FC` (the same store nativeInitClientSettings uses).
- That async load is posted to the FM queue and WAITED on (boost future,
  `sub_65FA644` -> `sub_23904A8` cond_wait). The FM is busy running getFlags, so it
  can never run the load -> self-post-and-wait deadlock.

The hijack "fixes" it only by making qword_7081868 == cordial's thread, so the async
load runs INLINE (synchronous) instead of being queued. That is also exactly why it
breaks async subsystem creation (StartupController stays 0x0): with everything
inline, tasks that must run on the real FM thread don't.

### Two real issues, in order
1. THREADING (root): cordial drives GameGlobalInit off the engine's FunctionMarshaller
   thread, so getFlags self-posts-and-waits. The clean fix is a cordial thread-model
   change — run the bootstrap such that getFlags is NOT on the FM while its load needs
   the FM (e.g. drive GameGlobalInit from a context where the async load's producer
   thread is free). This is a design task, not a patch.
2. DOCUMENT FORMAT (downstream): cordial delivers `{"applicationSettings":{...}}` but
   getFlags looks up the `ClientAppSettings` application group, absent from the doc.
   Even with threading fixed, the lookup content must match. This is cordial's
   long-open "which document/key" question — the answer is the engine wants the
   ClientAppSettings *application group*, not a generic applicationSettings wrapper.

### Function map for next session
getFlags sub_2C5CAF2 | fetch sub_5FB512C/sub_5FB52B8 | sync sub_5FB3969 |
async sub_5FB535B | extractor sub_2C5D182 | store sub_23539FC/qword_72893D8 |
future-wait sub_65FA644/sub_23904A8 | FM thread sub_2339A1E ("FunctionMarshaller")
qword_7081868 | marshal primitive sub_2339452 | StartupController singleton 0x70b3c20.

The whole ABI/sync/futex layer beneath remains proven-correct. This is the last
structural gap and it is a thread-model redesign, cleanly scoped above.

---

## Session 5 (cont.): THE UNIFIED ROOT — cordial bypasses android_main

Both remaining blockers (getFlags deadlock, StartupController null-deref) trace to
ONE cause: cordial drives the GameActivity natives directly from its own thread and
never runs the engine's own app-thread entry point.

- `sub_2C53602` is **android_main** ("[FLog::NativeMain] [android_main] Create a new
  NativeEngine"). It creates the NativeEngine and, via sub_2C54790 -> ... ->
  sub_23CD346, the **StartupController** singleton (0x70b3c20). It appears ZERO times
  in every run log — it never executes.
- The GameActivity app thread `sub_278D8E0` IS spawned by initializeNativeCode (via
  sub_278C7D0, tid seen right after GameActivity_register, does its own
  ALooper_prepare(1)), but android_main is never reached — no thread sits in the
  sub_278xxxx/sub_2C54xxx region at any crash, and the "Create a new NativeEngine"
  log never fires.
- cordial's design (looper.rs) has ITS OWN thread prepare a looper and pump it,
  replacing the engine's app thread. So the natives cordial calls directly
  (retryInit, GameGlobalInit, StartApp) run, but android_main's subsystem creation
  does not.

### Why this unifies both walls
- StartupController: created only by android_main -> never created -> StartApp
  (nativeAppBridgeV2StartAppWithParams) null-derefs it.
- getFlags deadlock: getFlags's async load is FunctionMarshaller-bound; the FM/app
  thread model the engine expects isn't the one cordial runs, so the load self-posts
  and waits. On Android the same load is a network fetch off the app thread.

### The real fix (architectural, unblocks BOTH)
cordial should run the engine's actual app-thread bootstrap — let android_main
(sub_2C53602) execute on the spawned app thread (sub_278D8E0) rather than bypassing
it — OR replicate exactly what android_main creates (NativeEngine + StartupController
+ the FM/looper wiring) so the directly-driven natives find the state they need.
This is a cordial thread-model change, not a patch, and it is the single lever for
both remaining crashes. Function map: android_main sub_2C53602 | app thread
sub_278D8E0 | spawner sub_278C7D0 (<- initializeNativeCode) | StartupController init
sub_23CD346/sub_2C54790 | singleton 0x70b3c20.

Everything below this (ABI, sync, futex, pthread) remains proven-correct.

### The StartupController is created by NativeActivity command 3 (can't be faked)
Mapped to the bottom: android_main (sub_2C53602) runs the ALooper loop sub_2C54790,
which calls sub_2C5894E each iteration — a NativeActivity command dispatcher:
  switch(*(app_state+16)) { case 3: sub_2C589B2(app_state) -> ... -> sub_23CD346
  (StartupController) }
So the StartupController is created when the app thread's loop receives command 3
(an APP_CMD_* lifecycle command) on its own ALooper command pipe. This confirms it
CANNOT be replicated piecemeal from cordial: sub_2C54790 is the blocking main loop,
and sub_2C5894E is a command dispatch over a properly-initialised app_state object
that only android_main's own bootstrap builds. A blind code-call would need a
hand-crafted app_state and would crash.

CONCLUSION (final for this line of work): the ONLY correct fix is to run the engine's
app thread bootstrap — let android_main (sub_2C53602) execute and drive its ALooper
loop with the real NativeActivity command sequence (INIT_WINDOW etc.), instead of
cordial driving the JNI natives directly. That is the cordial thread-model redesign,
and it resolves both the StartupController crash and the getFlags deadlock at once.
Complete map: android_main sub_2C53602 | loop sub_2C54790 (ALooper_pollOnce) |
cmd dispatch sub_2C5894E (case 3) | sub_2C589B2 -> sub_2F35C72 -> sub_23CD346
StartupController 0x70b3c20 | app thread sub_278D8E0 | spawner sub_278C7D0.

---

## Session 6: CORRECTION — android_main DOES run; the app-thread command pipe is starved

Last session concluded "android_main never runs." That was WRONG — an lldb unwind
failure (the app thread has no frame pointers) hid it. procstat on the live hung
process shows the app thread's KERNEL stack is:
  sys_ppoll -> kern_poll -> seltdwait -> _cv_timedwait_sig
i.e. it is sitting in an ALooper poll — it IS inside android_main's event loop
(sub_2C54790 -> ALooper_pollOnce), waiting for commands.

### The real gap
The StartupController is created when the app thread processes NativeActivity
command 3 (sub_2C5894E case 3 -> sub_2C589B2 -> sub_23CD346). Those commands are
written to the app thread's command pipe by the engine's GameActivity surface/
lifecycle natives (onSurfaceCreatedNative, onSurfaceChangedNative, onStartNative,
onResumeNative, onWindowFocusChangedNative, ...). Those natives are NOT exported —
they are registered via RegisterNatives INSIDE initializeNativeCode, and on real
AGDK the Java GameActivity class invokes them on lifecycle events.

cordial only ever calls `Java_..._GameActivity_initializeNativeCode` (verified:
it is the sole GameActivity symbol in both cordial's source and the engine's export
list) and drives the engine's OTHER natives directly. It never runs the GameActivity
lifecycle, so the app thread's command pipe gets NO commands — it polls an empty pipe
forever, never processes command 3, never creates the StartupController.

mocktail (which renders) DOES feed this pipe: looper.rs:873 records a mocktail run
where "nine events [were] delivered" to the command pipe. That is the difference.

### The fix direction (tractable, not a thread redesign)
Feed the app thread's command pipe with the GameActivity command sequence — either
by invoking the RegisterNatives-registered surface/lifecycle natives (cordial's
libjnivm captures those function pointers) the way the Java GameActivity would, or by
writing the command bytes to the pipe's write end directly. The app thread's command
pipe read-end is the fd passed to ALooper_addFd with callback=yes (fd 14 in the
observed run). Command 3 must arrive before StartApp so the StartupController exists.

---

## Session N+1: TWO walls fall — the flags-loaded byte and the StartupController — Roblox reaches a Vulkan swapchain

This session corrects the section immediately above (the "command-3 pipe" theory of
StartupController creation was WRONG) and gets the engine all the way to creating a
Vulkan swapchain and running its main work loop. Two distinct blockers, both found in
the disassembly, both now cleared.

### Wall 1 — the "flags loaded" byte (was masquerading as a TaskScheduler crash AND the getFlags deadlock)

Symptom: `RBXCRASH: FatalRuntimeError (Can't initialize the TaskScheduler before flags
have been loaded)`, deterministic, on the engine's app thread during bootstrap.
Confirmed environmental, not a regression (the known-good commit 5cae8e6 crashes
identically now; independent of settings content — full 22k-flag doc, minimal 2-flag
doc, cached — all identical).

The gate (2.721), at the throw site file VA 0x23805cf:

    cmpb $0, 0x75a8250      ; the global "flags loaded" byte
    jne  ok
    lea  "Can't initialize the TaskScheduler before flags have been loaded"
    call <throw FatalRuntimeError>

That byte is read all over the binary (it is the FFlag-ready guard). It is WRITTEN to 1
by exactly two sites; the load-bearing one is 0x2c2bfb6, inside the FFlag *parse*
routine (log markers `parse_flag_begin` / `set_flag_filters_end` at 0x394387). Per
docs/analysis/flag-init.md §1, `nativeInitializeNativeFlags` itself does NOT set it — it
only builds the cached-flags result object; the *parse* sets it. On this bring-up the
engine's app thread reaches TaskScheduler init before the parse has set the byte.

Fix (bring-up scaffolding, CORDIAL_SET_FLAGS_LOADED=1, feature-gated per ADR-001): set
`*(base + 0x75a8250) = 1` once, before initializeNativeCode spawns the app thread. base =
JNI_OnLoad - 0x22addd7. Nothing ever clears it, so pre-setting it pre-satisfies the gate
without racing.

UNIFICATION: this same byte also subsumes the old getFlags "self-marshalling deadlock."
With the byte set, `nativeGameGlobalInit` completes with NO marshaller hijack — getFlags
was awaiting flags-loaded, the scheduler gate was testing it. One write clears both walls.
(The marshaller hijack, ADR-001 experiment, is now redundant for this path.)

Legitimate ship-fix still owed: make the FFlag parse actually complete (and set the byte)
before TaskScheduler init, instead of pre-writing the byte.

### Wall 2 — the StartupController is a lazy static, built by nativeAppBridgeAppStart (NOT a command-3 dispatch)

Once wall 1 fell, the full bootstrap ran (flags, app bridge, DataModel, task scheduler
foregrounded, APP_READY for PlatformAccountRouter and Startup) — then a SIGSEGV right
after `[cordial] app start`, and `[startup] StartupController singleton after 5019ms: 0x0`
(still null). The crash: null-deref inside nativeAppBridgeV2StartAppWithParams at file VA
0x2506700 (`movq 0x18(%rax)`, rax=0 — a virtual dispatch on an object with a null vtable).

The StartupController singleton (global 0x70b3c20 on 2.721) is a FUNCTION-LOCAL STATIC.
Its sole creator (verified: the only write to 0x70b3c20 in .text) is at 0x23cdb31
(`movq %rax, 0x70b3c20`), inside the exported
`NativeAppBridgeInterface.nativeAppBridgeAppStart(String,String,Z,String,String,String)`
(export at 0x23cb767), behind a __cxa_guard at 0x70b3b28. It is built the FIRST time that
overload runs. That overload lives on NativeAppBridgeInterface, NOT NativeGLInterface, and
cordial NEVER called it — it appeared only in a comment (load.rs:2070). So StartApp
dereferenced a controller that was never constructed.

The prior section's "command-3 pipe / RegisterNatives lifecycle" theory of StartupController
creation is SUPERSEDED: the controller has nothing to do with the app-thread command pipe;
it is a plain Meyers singleton gated on one specific JNI bridge call.

Fix (a real bridge call, not a memory hack): new wrapper `cordial_appbridge_app_start`
(native/init_params.cpp) builds the six JNI args (five jstrings + a jboolean; empty values
reach the lazy-static — its construction does not depend on their values) and invokes the
overload. `linker::game_activity::app_start` binds it; load.rs calls it in the default path
just before StartAppWithParams (CORDIAL_NO_APP_START to A/B).

### Result — furthest yet, no crash

    nativeAppBridgeAppStart ok (builds StartupController)
    [startup] StartupController singleton after 0ms: 0x242c23df3b00   (was 0x0 for 5019ms)
    app started with surface                                          (previously SIGSEGV)
    surface+platform params delivered (app) / (game)
    surface handed to the engine
    late retry: nativeRetryInit ok
    InputConnection registered with the engine
    [android] vulkan: vkCreateSwapchainKHR extent 1667x651, minImageCount 3
    pumping the looper for 20s
    D/GameActivity ************** mainWorkCallback *********
    ... clean exit 0

Reached with: CORDIAL_SET_FLAGS_LOADED=1 CORDIAL_PROBE_STATE=1 CORDIAL_STATE_POLL_MS=5000
CORDIAL_STARTUP_POLL_MS=6000. The engine creates a Vulkan swapchain (1667x651, present
mode IMMEDIATE) and runs its GameActivity main work loop. Clean exit, no crash through the
full run window.

### Next frontier — continuous frame presentation (render gate)

`mainWorkCallback` fires only ~2× in a 20s run, so the main-thread work loop is not being
driven per-frame (no Choreographer equivalent; cordial has no Java frame callback). The
engine's own render thread created the swapchain, but per-frame present is not yet observed.
This is the render-gate investigation (docs/analysis/render-gate.md), and it is the path to
actually seeing pixels. Bring-up scaffolding still required to reach here: the
CORDIAL_SET_FLAGS_LOADED byte-write and the retryInit state poll.

---

## Session N+1 (cont.): render pipeline runs, but only a few frames — and the content is a bare clear

After the resume/foreground unlock (previous section), a full investigation of *why
it is not continuous or visible*:

### The renderer draws ~3-5 frames, then idles

Measured with cordial's own present counter (glcount `vkQueuePresentKHR`, incremented
unconditionally in `android::vulkan::vk_queue_present_khr`, and read live over the
dev-control socket's `info` verb — `presents=N`, which sidesteps the badly-buffered
stdout log):

- resume+foreground:                 vkQueuePresentKHR = 5 (one run), 0 (others)
- resume+foreground+drive-redraw:    vkQueuePresentKHR = 3, with 1699 redraw requests sent
- no resume:                          vkQueuePresentKHR = 0 always

So the engine presents a *handful* of frames right after resume, then stops. It is
NOT continuous, and it is nondeterministic (0-5). Driving onSurfaceRedrawNeededNative
at 60Hz does not add frames (confirmed dead path). There is **no per-frame JNI driver
native** — searched the export table: no renderFrame/doFrame/step/heartbeat/tick;
`nativeScheduleOnFirstFrame` exists but is a one-shot. So the frame loop is entirely
engine-internal (TaskScheduler-driven); the app does not tick it. The engine renders
its initial frame(s) on resume and then its render task is not rescheduled — a
TaskScheduler frame-timing problem (likely the frame-pacing condvar/clock on FreeBSD:
the scheduler decides "next frame" via a timed wait and does not wake at ~16ms). This
is the open internal blocker for *continuous* rendering.

### What a captured frame actually shows

The X11 window pixmap is useless for a Vulkan swapchain (the compositor never sees the
presented image — `import -window` returns a 409-byte solid color). The real presented
frame is captured *inside* vkQueuePresentKHR via the dev-control `screenshot <path>`
verb (`android::capture`). One such frame (6.65 MB, real content):

  **a uniform light-gray (#e0e0e0) clear — the app background, with NO UI drawn on it.**

So the engine clears and presents, but the LuaApp/CoreGui GUI is not composited. Given
"nobody signed in" + the pile of WebView FastFlags, the login screen on mobile Roblox
is a **WebView**, and cordial's webview feature is not built (needs webkit2-gtk_60,
which IS packaged on this box as `webkit2-gtk_60-2.46.6_8` but is a heavy build).
Without it the login opens in the external browser instead. So visible UI content is
gated on either (a) building the webview feature, or (b) authenticating so the native
home/in-game render path (not WebView) has something to draw.

### Tooling proven this session (for the next run)

- Real frame capture: `CORDIAL_DEV_CONTROL=1 CORDIAL_DEV_CONTROL_SOCKET=<path>`, then
  `printf 'screenshot /abs/out.png\n' | nc -U <sock>` — captures the next present.
- Live present rate without the log lag: `printf 'info\n' | nc -U <sock>` ->
  `presents=N ... extent=WxH`.
- Full input is already wired in devctl: move/click/down/up/key/tap/text/scroll — ready
  for the "interactable" goal once there are pixels to interact with.
- Reliable reach to app-start needs CORDIAL_SET_FLAGS_LOADED + the retryInit state poll;
  bootstrap timing is variable (swapchain create seen anywhere from ~10s to ~42s).

### Open frontiers, ranked

1. Continuous rendering — the engine's render task is not rescheduled after the first
   frames (internal TaskScheduler frame-pacing/clock on FreeBSD). This is the true
   "continuous" blocker and is internal/tractable.
2. Visible UI — the pre-login screen is a WebView; needs the webview build or auth.
3. Bootstrap timing flakiness (GlobalInit/retryInit/EngineModule asserts at varying
   points) — a threading/scheduling robustness issue, same family as #1.

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_013BsKoQtrUtDDqCD51r5Uxy

---

## Session N+1 (cont.): DTrace cracks the render stall — it is a busy-spin on an unsatisfied predicate, and the futex layer must NOT be touched

`procstat -kk` showed every thread in a umtx/poll wait and read as "all idle" — WRONG.
DTrace showed the opposite: the process **busy-spins ~3 cores**:
- one thread ~1.4M `poll()`/s inside `cordial_runtime::android::looper::looper_poll_once`
  (the ALooper free-run; this engine polls timeout=0 millions/s by design and is capped
  only after 120 presents — see BACKOFF_AFTER_PRESENTS, so during a 0-present startup it
  spins uncapped, which is expected for this engine).
- two engine threads ~340k `_umtx_op`/s each. The return distribution is decisive:
  `op=15 (WAIT_UINT_PRIVATE) ret=-1 errno=60 (ETIMEDOUT)`. The futex trace shows the
  WAIT_BITSET absolute deadline sits ~2-3s in the PAST and is FIXED while the clock
  advances, `val` fixed — i.e. a `while(!pred) wait_until(fixed_deadline)` loop whose
  predicate never becomes true (same class as the getFlags wall), so the expired wait
  returns instantly and it spins.
- the render/GL thread is blocked in `xcb_wait_for_reply64` — an X11 roundtrip via the
  NVIDIA Vulkan driver.

The engine DOES read the clock correctly (DTrace: `bionic_clock_gettime` ~920k/s → real
FreeBSD CLOCK_MONOTONIC), so it is NOT a lagging-clock bug; the deadline is stale simply
because that wait has been pending seconds waiting for `pred`.

**Two futex-layer fixes were tried and BOTH regress the bootstrap — do not retry:**
1. FreeBSD→Linux errno translation on the futex return (ETIMEDOUT 60→110, EAGAIN 35→11).
   Theoretically correct (bionic checks Linux errno), committed as df334fb, but it made
   `resume` hang and the port stopped reaching render → reverted (fbcd9ef).
2. A spin-cap (200µs sleep on an already-elapsed ABSTIME deadline, errno untouched). Also
   regressed bootstrap to no-render.
The fragile bootstrap **relies** on these timed waits spinning fast for its timing races
(the flag-parser locks mutexes tens of thousands of times racing the main thread — see
bm_lookup's lock-free comment). So the fix is NOT at the futex layer.

**The real remaining root:** identify the predicate the render-time spin waits on (the
condvar addr ends ...328c; the spinning libroblox VAs are ~0x2780110 / 0x2783950 /
0x2784a4e / 0x27798ef with base = JNI_OnLoad − 0x22addd7) and make it true — the same
"engine waits for work that never runs on this port" shape as getFlags/flags-loaded,
which was cracked by finding the exact global the engine gated on. This one needs the
decompiler (IDA, ~/libroblox_2.721.so.i64) on those functions. Also intermittent:
`HardAssert (EngineModule not found)` early in bootstrap (timing).

Tooling proven this session: FreeBSD **DTrace** (`ustack()` resolves even without frame
pointers — this is what cracked the stall), and the dev-control socket `info`/`screenshot`
verbs (live present count + real swapchain frame capture, bypassing the X11 pixmap which a
Vulkan swapchain leaves blank).

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_013BsKoQtrUtDDqCD51r5Uxy

---

## Session N+1 (cont.): IDA identifies the spinners as RBX Worker threads; render-phase-gated spin-cap does not help

IDA (~/libroblox_2.721.so.i64, imagebase 0, so VA == file offset) decompiled the spinning
stack:
- sub_27800B0 (the thread entry): `qmemcpy(name,"RBX Worker ",11); name[11]=id+65;
  set_thread_name(name); worker_fn();` — the spinning threads are the engine's **"RBX
  Worker" TaskScheduler threads**.
- Their loop (sub_2781D90) is the worker idle job-wait: it blocks on a cond for the next
  job, and on this port that timed wait sits on an already-elapsed absolute MONOTONIC
  deadline, so _umtx_op returns ETIMEDOUT instantly and the worker re-arms the same stale
  deadline and busy-spins (~340k/s each, three cores). The cond is monotonic-consistent
  (futex trace: clk=4, deadline in the seconds-since-boot range), so this is NOT the
  REALTIME-vs-MONOTONIC bug bionic_cond_init_monotonic already fixes — the deadline is
  stale simply because the worker has been waiting seconds for a job (predicate) that never
  arrives.

Tried and reverted (did NOT help): a futex spin-cap (150-200µs sleep on an already-elapsed
ABSTIME deadline) gated to fire ONLY after StartApp (a `cordial_render_phase` flag set from
load.rs, so bootstrap's fast-spin races are untouched). Runs still hit the same intermittent
hangs — at GameGlobalInit in some runs (pre-cap) and at `resume` in others (post-cap) — so
the CPU-starvation-from-spin theory does not hold: freeing the cores does not make the
missing job/predicate appear.

**Standing conclusion.** The engine boots (major progress this session) but its threading is
unstable on FreeBSD in a way that is intermittent: GameGlobalInit sometimes hangs, `resume`
sometimes hangs, and rendering never sustains (~3-5 frames then the RBX Workers spin idle).
All three are the same shape — an engine thread waiting for work another thread never
produces — and the fix is NOT at the futex/clock layer (proven: three separate futex-layer
edits each either regress bootstrap or fail to help). The next step is to identify, in the
worker loop and the resume path, exactly which job/predicate is expected and which producer
thread never runs on this port — the same method that cracked getFlags (find the specific
global the engine gates on) and the flags-loaded byte. IDA + DTrace `ustack()` are the
tools; the flaky repro (foreground reached ~1 run in 4) is the main friction and argues for
capturing many DTrace samples per state rather than more one-off code experiments.

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_013BsKoQtrUtDDqCD51r5Uxy

---

## Session N+1 (final): render stops at ~5 frames PERMANENTLY; no external stimulus restarts it

Confirmed exhaustively via the dev-control `info` present counter (live, bypasses the
buffered log): after resume+foreground the count climbs 0 → 4 → 5 and then is **stuck at 5
forever**, even while a slow (2s-cadence) resume re-fire keeps running. So this is not
"sparse/slow" rendering — the engine presents its first ~5 frames and the DataModel render
task then stops re-scheduling entirely. None of these restart it (all tested this session):
onSurfaceRedrawNeededNative (1699×), resume re-fire (fast 430× AND slow), applicationForegrounded/
gameForegrounded, live mouse/keyboard input (accepted=14, 0 new frames), a futex spin-cap.

Consequence: the LuaApp GUI finishes building a few seconds *after* those 5 frames (asset
trace shows it loading fonts/icons/sprites/shaders), so it never gets a frame — every
captured frame is the pre-GUI gray clear, and no new present ever happens to capture the
built GUI. IDA shows the spinning threads are the "RBX Worker" pool parked on a lock-free
work-stealing eventcount (sub_2781D90 → sub_2784960 → sub_2779820 futex wait) with no jobs
being pushed — i.e. the per-frame render/step job-driver is not running past the first few
frames on this port.

This, plus the intermittent GameGlobalInit / resume hangs, is one root: on FreeBSD the
engine's frame/step driver does not sustain, so render jobs stop being queued. The fix is a
focused investigation (IDA + DTrace) into what submits the DataModel step/render job each
frame on Android and why it stops here — NOT the futex/clock layer (three edits there each
regressed bootstrap or did nothing) and NOT any external lifecycle kick (all tested, none
work). Bootstrap flakiness (foreground reached ~1 run in 4) and the buffered-log / socket-
suppressed-present measurement friction are the practical obstacles to that investigation
and should be addressed first (e.g. an unbuffered present/heartbeat counter printed on a
timer, independent of the dev socket).

---

## Session N+1 (cont.): the render stall is the vkAcquireNextImageKHR stall — a PRESENTATION bug, not the scheduler

Reframed with cordial's own vulkan.rs (§ around line 1180, pre-existing): the renderer
"stalls in vkAcquireNextImageKHR waiting for a refresh." That fits every observation:
- ~5 presents == swapchain depth (minImageCount 3/4 + in-flight), then permanent freeze;
- the render/GL thread is blocked in an X11 roundtrip (DTrace: _poll -> libxcb
  xcb_wait_for_reply64 -> _XReply -> libGLX_nvidia) — i.e. inside a Vulkan call doing an
  X11 request, consistent with acquire waiting for a presented image to be released;
- the RBX Workers park because the render thread that would queue the next frame's work is
  stuck in acquire (so "workers idle" is a SYMPTOM, not the root).

Present-mode probe (CORDIAL_PRESENT_MODE): IMMEDIATE (the engine's choice) stalls at ~5;
forcing FIFO made resume hang immediately in its first blocking present. BOTH fail, and
they fail in the two different ways you'd expect if **no display-refresh / vblank /
PresentCompleteNotify events are reaching cordial's X11 Vulkan surface** — IMMEDIATE never
gets an image released (acquire blocks after the swapchain fills), FIFO blocks forever in
present waiting for a vsync that never signals. mocktail renders on this same box, so the
difference is cordial's own window/surface: it creates its own Xlib window (dlopen libX11,
window.rs) and a VkXlibSurfaceKHR on it (vulkan.rs). The next step is why the NVIDIA
driver's DRI3/Present on that window never delivers vblank/idle events — compositor state,
window attributes/visual, or missing PresentSelectInput vs what mocktail's surface has.

Tooling added this session (kept): CORDIAL_HEARTBEAT prints the real vkQueuePresentKHR
count to stderr (unbuffered) every second — the reliable present-trajectory probe, since
the dev socket suppressed presents in polled runs and the clean-exit graphics report kept
being cut off by the run timeout.

Practical blocker to continuing: the bootstrap's intermittent GameGlobalInit hang is bad
enough right now (~0-1 render-reaching run in 5) that empirical present-mode/surface
iteration is impractical — stabilising GameGlobalInit (it hangs at "activity lifecycle 9/9
fired") should come first, or all render testing stays a coin flip. System itself is
healthy (load <1, 10 GB free, no leaks), so this is the engine's threading, not the host.

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_013BsKoQtrUtDDqCD51r5Uxy

---

## Session N+1 (unification): ALL the flakiness is ONE root — async TaskScheduler jobs don't reliably run on FreeBSD

The bootstrap fails at a *different random point every run*, and they are the same bug:
- GameGlobalInit hangs (getFlags awaits a producer job that never runs)
- retryInit ASSERTION: nativeEngineState_ stuck at 2 instead of advancing to 1/0xb (the
  state-advance job didn't run)
- HardAssert (EngineModule not found) — a module-registration job hadn't run yet
- resume hangs (a resume-path job never completes)
- render stalls at ~5 frames (the per-frame render job stops being serviced)

Common cause, confirmed by DTrace+IDA this session: the engine's "RBX Worker" TaskScheduler
threads park on a lock-free work-stealing eventcount (sub_2781D90 → sub_2784960, futex
WAIT_BITSET at sub_2779820) and the async jobs queued during bootstrap are not reliably
picked up / the workers spin on expired-deadline waits (op=15 ETIMEDOUT ~340k/s). Whichever
job loses the race that run is the failure you see. The marshaller hijack "fixes" GlobalInit
by running it inline, but it is itself a timing race that now segfaults at full speed
(reaches render cleanly only under lldb's slower timing). Longer CORDIAL_STATE_POLL_MS does
not help because a *different* job is the one that stalls next run.

So this is NOT four separate walls (SIGSEGV / flags / StartupController / render) plus flaky
bootstrap — it is one: **the FreeBSD futex/scheduler mapping does not give the engine's
TaskScheduler reliable job hand-off, so async engine work runs only sometimes.** The
earlier per-symptom fixes (flags-loaded byte, StartupController call, resume) each removed a
*deterministic* blocker and are correct; what remains is this one *nondeterministic* root.

The fix has to make the worker eventcount hand-off reliable on FreeBSD — either the futex
WAIT_BITSET/WAKE_BITSET → _umtx_op mapping (verify WAKE_PRIVATE actually wakes
WAIT_UINT_PRIVATE waiters for the eventcount's exact usage, and that the bitset being
dropped never loses a targeted wake), or the worker parking itself. This needs a *stable*
repro to instrument job push vs pickup, which the flakiness currently denies — the only
reliable-ish run this session was under lldb (its overhead wins the race). A minimal
standalone futex WAIT_BITSET/WAKE_BITSET ping-pong test against _umtx_op (outside the engine)
is the fastest way to prove or clear the mapping without fighting the 1-in-5 boot.

---

## Session N+1: the futex WAIT/WAKE mapping is CORRECT — ruled out as the flakiness root

Repro-independent test (docs/analysis/futex_wake_test.c, standalone, no engine): replicate
cordial's exact mapping — a waiter parks on `_umtx_op(WAIT_UINT_PRIVATE, val, abstime-
monotonic)` (= bionic FUTEX_WAIT_BITSET), the main thread changes the word and calls
`_umtx_op(WAKE_PRIVATE, 1)` (= FUTEX_WAKE_BITSET). Result: **the waiter wakes immediately
(r=0), not ETIMEDOUT.** So WAKE_PRIVATE does wake WAIT_UINT_PRIVATE waiters on the same
address; the bitset being dropped does not lose the wake; the translation is sound.

This RULES OUT the futex layer as the cause of the unreliable job hand-off. Combined with
nativeEngineState_ reaching 0xb on *some* runs (the state-advance producer DOES run
sometimes), the async jobs are not blocked on a lost wake or a missing dependency — they
are **timing-sensitive**: a woken worker competes with the other workers busy-spinning on
expired-deadline waits (op=15 ETIMEDOUT ~340k/s), and which async job wins the race that run
decides whether bootstrap reaches render, asserts (retryInit / EngineModule-not-found), or
hangs (GlobalInit / resume).

So the remaining root is NOT: the per-symptom blockers (fixed), the futex mapping (cleared),
or a missing IO producer (the producers run sometimes). It IS: the RBX Worker threads
burning cores on expired-deadline spins delay/starve prompt async-job pickup, making
bootstrap a race. The futex spin-cap addressed the spin but regressed bootstrap because it
also slowed the deterministic flag-load race — so the cap must be *scoped* to the idle
worker parking only (not every timed wait), or the worker parking should block indefinitely
(no expired-deadline spin) once idle. That is the next concrete lever, and it no longer
needs the flaky engine repro to prototype — the eventcount's park/spin is reproducible in
the standalone harness above.

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_013BsKoQtrUtDDqCD51r5Uxy

---

## Session (post-reboot): resume deadlock SOLVED; render loop is never invoked (engine cyclic scheduler)

MAJOR fix (commit 876745d): the self-marshalling deadlock is beaten for cordial's own calls
via CORDIAL_RESUME_HIJACK — overwrite the FunctionMarshaller handle (qword_7081868) with the
calling thread ONLY around the resume call, so resume runs inline and completes, then restore
so the state-advance keeps the real FM. Result: resume completes reliably ("ok (inline)"),
bootstrap reaches the render stage, Startup UI hits APP_READY, no hang. This is the chaos-
maker of the whole port, resolved for the calls we drive.

Render, mapped precisely (new this session):
- Caller-address logging in vk_create_swapchain_khr (CORDIAL_LOG_SWC_CALLER, reads [rbp+8])
  gives the engine render fn: swapchain SETUP = sub_63BC260 (VkAndroidSurface + swapchain +
  2 semaphores), called by the swapchain REBUILD path sub_63BFE2E (vkDeviceWaitIdle -> recreate
  -> query surface caps), which runs on surface set/resize, NOT per frame.
- The per-frame render LOOP (vkAcquireNextImageKHR + vkQueuePresentKHR) is NEVER invoked:
  DTrace shows no thread in Vulkan/GLX, glcount vkQueuePresentKHR stays 0, no thread in acquire.
  So the render thread sets up the swapchain then the loop is never called.
- It is never called because the engine's DataModel Heartbeat / TaskScheduler cyclic render
  job fires a few times (enough for APP_READY) then stops rescheduling on FreeBSD — the same
  cyclic-job-stall root. The render loop cannot be reached by runtime trace (never called) nor
  by static xref (present/acquire are indirect via driver fn-pointers; render-gate.md §2).

Exhausted external levers (NONE produce a frame): resume (sync/async/inline-hijack),
foreground, StartApp, surface delivery, redraw-drive, resume-drive, the app-thread command
pipe with the full NativeActivity lifecycle (INIT_WINDOW/START/RESUME/GAINED_FOCUS -
processed, 0 frames), three spin-cap variants, settle-timing, pumping-poll (crashes),
present-mode forcing. Futex mapping proven correct (standalone test). So it is not the futex,
not presentation/vblank, not CPU starvation, not the lifecycle — it is the engine's INTERNAL
cyclic frame-job scheduler not sustaining on FreeBSD.

Also still flaky even with the resume fix: HardAssert (EngineModule not found) / retryInit
fire on some runs — the engine's OTHER internal async jobs (module registration, state
advance) hit the same deadlock class that only the engine's own threads (which we cannot
hijack the way we hijack our own call) can resolve.

Bottom line: continuous rendering needs the engine's internal TaskScheduler to keep
rescheduling its per-frame render job on FreeBSD. That is engine-internal and reachable
neither by external driving (all tried) nor by static/runtime tracing of the render loop
(indirect + never-invoked). It is the deep remaining root, and it is a reverse-engineering
effort on the scheduler's cyclic-job re-arm logic, ideally on a warmed, non-flaky repro.

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01DoaDcG56gpFpbmAZ3nMMzw
