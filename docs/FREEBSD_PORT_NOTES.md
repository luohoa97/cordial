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
