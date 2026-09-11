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
