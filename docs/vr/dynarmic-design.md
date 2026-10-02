# Phase 2: the Quest engine under dynarmic, inside an x86-64 Cordial

Design, written before anything was built; §9 records what M0 and M1 then measured, and corrections are marked inline. Evidence comes from `readelf`, bit-pattern
counts over `.text`, 12 sampled `llvm-objdump` windows (counted, not read),
dynarmic headers and the fork's source. The lists are beside this file.
**INFERRED** marks anything not observed.

## 0. Verdict

dynarmic is usable, but not for the reason you'd expect. Its A64 frontend lacks
**LSE atomics, LDAPR, FP16 arithmetic, SVE and PAC** (`frontend/A64/decoder/a64.inc`:
643 `INST` lines live, 231 commented out, including `CAS`, `LDADD`, `SWP`, `LDAPR`,
`FADD_1`, `PACIA_*`). This build of `libroblox.so` survives that, because **every
instruction from those families is behind a runtime feature check**, and Cordial is
the one answering that check (§1.3). Being honest about the emulated CPU's features
is enough; no engine memory has to change.

## 1. dynarmic today

### 1.1 Where it lives

`merryhime/dynarmic` on GitHub returns 404. The last upstream tag was 6.7.0 (so the
`SaMeiers/dynarmic` mirror describes itself). Maintained descendants:

| Fork | Licence | Last push | Notes |
|---|---|---|---|
| `azahar-emu/dynarmic` | 0BSD | 2026-09-26 | Keeps A32+A64 and x64/arm64/riscv64 backends. Its CMake is embed-friendly: *"cmake: Do not find_package if target already defined"* (2026-06-24). **Recommended base.** |
| Eden in-tree (`git.eden-emu.dev/eden-emu/eden`, `src/dynarmic`) | Files modified by Eden carry `SPDX: GPL-3.0-or-later` | 2026-09-28 | Most A64 work happens here (Switch). Also no LSE. Compatible with the fork's GPL-3.0-or-later, but it changes the licence of anything taken from it. |
| `suyu-emu/dynarmic`, `yuzu-revived/dynarmic` | 0BSD | 2026 | Less active. |

It builds as a CMake static library on x86-64 Linux. Dependencies: Boost headers
(BSL-1.0), fmt (MIT), mcl, tsl-robin-map (MIT), xbyak (BSD-3) and Zydis (MIT), all
vendored under `externals/`. Cordial uses Cargo, so it would go in through the `cmake`
crate from `cordial-runtime/build.rs`, built with Clang. Rust bindings exist
(`exverge-0/dynarmic-rs`, 0BSD), but a thin C++ shim is simpler (INFERRED).

### 1.2 The API that matters (`interface/A64/config.h`, `a64.h`)

- `A64::Jit(UserConfig)`. `Run()` and `Step()` are documented *"Cannot be recursively
  called"*. You can get and set GPRs, vectors, SP, PC, FPCR/FPSR and PSTATE, and use
  `HaltExecution(HaltReason)`, `InvalidateCacheRange`, `ClearCache` and
  `ClearExclusiveState`.
- `UserCallbacks`:
  - `MemoryRead/Write{8..128}` and `MemoryWriteExclusive*`, plus `MemoryReadCode`.
  - `CallSVC(u32 imm)`.
  - `ExceptionRaised(pc, Exception)`, for BRK, hooked hints, and encodings the
    decoder *matches* but treats as unallocated (FP16 `scvtf h0, x0` is one).
  - `InterpreterFallback(pc, n)`, for system registers it does not model **and
    for every instruction with no decoder entry**: UDF, and all the LSE, LDAPR
    and SVE lines commented out of `a64.inc`. Consecutive ones are merged into
    one call (`ir/opt/a64_merge_interpret_blocks.cpp`), so `n` is not always 1.
    *Corrected at M0*: this list used to send unallocated encodings to
    `ExceptionRaised`, and `.word 0` measurably arrives here instead
    (`crates/cordial-guest/tests/m0.rs`).
  - `InstructionCacheOperationRaised`, `DataCacheOperationRaised`.
  - Ticks: set `enable_cycle_counting=false` and `wall_clock_cntpct=true`.
- Memory: `page_table` (up to 64 address bits) and/or **`fastmem_pointer`** with
  `fastmem_address_space_bits` in the range 12–64. A host fault inside JIT code is
  caught by dynarmic's own SIGSEGV/SIGBUS handler, which recompiles the block without
  fastmem and chains to the previous handler otherwise
  (`backend/exception_handler_posix.cpp`: `old_sa_segv`, `retry_sa->sa_sigaction`).
- `tpidr_el0` and `tpidrro_el0` are **pointers baked into emitted code**, so each `Jit`
  has one storage slot.
- `global_monitor` is an `ExclusiveMonitor(processor_count)` with a fixed count and a
  unique `processor_id` per Jit. Every LDXR/STXR pair goes through **one global
  spinlock** (`exclusive_monitor.h`: `Lock(); ... Unlock();`). There are two escapes:
  `fastmem_exclusive_access` (x64 `cmpxchg` semantics) and
  `Unsafe_IgnoreGlobalMonitor`.
- `code_cache_size` defaults to 128 MiB, *"about 2GiB (x64 host)"* at most, **per Jit**.
  There is no shared code cache between Jit instances (INFERRED from the API).
- System registers modelled (`translate/impl/system.cpp`): FPCR, FPSR, NZCV, TPIDR_EL0,
  TPIDRRO_EL0, CNTFRQ, CNTPCT, CTR, DCZID. Everything else goes to
  `InterpreterFallback`.

### 1.3 What the Quest library actually uses

`libroblox.so` is NDK r28c at API 26 (`.note.android.ident`). It has no
`.note.gnu.property`, no `PT_TLS`, and `ANDROID_RELA` packed relocations. Its
`.text` is 72 MB, or 17,998,492 words. Whole-text bit-pattern counts:

| Class | Count | Consequence |
|---|---|---|
| LSE (`CAS*`, `LDADD*`, `SWP*`, `LDSET*`…) | 53 | **All 53** sit in outline-atomics helpers: `bti c; adrp x16; ldrb w16,[x16,#0xc48]; cbz w16,<LL/SC>; <LSE op>; ret`. All of them test the same byte (`lse-addrs.txt`). This is compiler-rt's `__aarch64_have_lse_atomics`, set from `getauxval(AT_HWCAP)`. The shape is observed; the flag's identity is INFERRED. *Observed at M3*: the first constructor calls `getauxval`, and with `ATOMICS` advertised the second one stops at a `casalb` in a helper of exactly this shape (§9.2). `getauxval` is an import. |
| LDXR/LDAXR | 249 | The fallback path that dynarmic does implement. |
| SVE encoding space | 1,247 | Contiguous blocks (`cntw`, `index`, `xar`, `zip1/2` on `z` regs). The one entry point looked at is gated by `tst w6,#0x4000` on a capability word, so this is a runtime-dispatched SVE2 path (INFERRED to be crypto). |
| AES/PMULL/SHA1/SHA256/SHA512 | present in samples | dynarmic implements all of them. |
| LDAPR | 0 | |
| `MRS TPIDR_EL0` | **1,249** | 1,235 of them are followed by `ldr xN,[xT,#40]`, which is bionic's `TLS_SLOT_STACK_GUARD` (slot 5, `tls_defines.h:90`). |
| other MRS | 22 | 11 × CNTVCT_EL0 (not modelled: needs `InterpreterFallback`), 2+2 × RNDR/RNDRRS (v8.5), 1 each of MIDR_EL1, ID_AA64ISAR1_EL1, CTR_EL0, NZCV, FPCR |
| `SVC #0` | 16 | Raw Linux syscalls with arm64 numbering. The number is computed with `eor`, so it can't be read statically. |
| DC CVAU / IC IVAU | 1 / 1 | One `__clear_cache`. Luau native-codegen strings are present (`LuauCodeGenBlockSize`…), so guest-generated code is possible. |

**What Cordial has to do.** `getauxval(AT_HWCAP)` must report exactly what dynarmic
implements: `FP|ASIMD|AES|PMULL|SHA1|SHA2|CRC32|ASIMDDP|FCMA`. It must never report
`ATOMICS`, `FPHP/ASIMDHP`, `ASIMDRDM`, `JSCVT`, `LRCPC`, `SVE*` or `RNG`. This is the
honest answer, because the CPU the guest runs on lacks those features. The control
experiment is to set `ATOMICS` and expect the first helper to stop with
`InterpreterFallback` at its LSE instruction -- not `ExceptionRaised`, as this
said before M0; `ldaddal` measurably takes the fallback path (`tests/m0.rs`). `/proc/cpuinfo` and `/proc/self/auxv` opened by the guest should
be answered consistently, through the path layer that already exists. For MIDR,
return implementer `0x00` ("reserved for software use") rather than impersonating a
real core.

## 2. Address space: identity-mapped, one process

Keep what Cordial does natively: guest address = host address. Configure
`fastmem_pointer = 0`, `fastmem_address_space_bits = 64`, no `page_table`, and
`silently_mirror_fastmem` off. Pointers then cross the boundary unchanged. That is
what makes most thunks trivial, and it is why a separate guest space (yuzu-style) is
the wrong model here. The costs: a wild guest pointer is a host SIGSEGV, and
dynarmic's recompile-on-fault path then calls the callbacks, which fault again. That
is a crash with a host backtrace, never a silent corruption. Guest `mmap` must not
use `MAP_32BIT`-style assumptions; there aren't any, since both sides are 47-bit.

**The bionic linker can keep loading these libraries, with a small change.**
`linker_relocs.h` selects `R_GENERIC_*` by `#if defined(__aarch64__)`, and
`linker_phdr.cpp:55-63` selects the accepted `e_machine` by host arch.
`linker_relocate.cpp` has `__aarch64__`/`__x86_64__` blocks only for TLSDESC and
`R_X86_64_PC32/32`. `llvm-readelf -r` shows the Quest libraries need only four types:

| | RELATIVE | ABS64 | GLOB_DAT | JUMP_SLOT | other |
|---|---|---|---|---|---|
| libroblox | 570,325 | 22 | 56 | 586 | none (no TLS, no IRELATIVE) |
| libopenxr_loader | 2,420 | – | 3 | 114 | – |
| libovrplatformloader | RELR | – | 1 | 61 | – |

All four are "write a 64-bit address", so they are arch-neutral. The patch is a
`CORDIAL_GUEST_ARCH` macro that replaces `__aarch64__` in those three spots and
accepts `EM_AARCH64`, plus `patches/0005` (RELR), which the fork already carries.
Two behaviours change:

1. Symbol lookup for guest imports must return a **guest stub address**, not a host
   function pointer. `symtab::build` is the seam.
2. **Constructors are guest code.** There are 3,617 `init_array` entries (28,936 B)
   in libroblox, and `call_function` must dispatch into the JIT.

Do **not** load the guest `libopenxr_loader.so` or `libovrplatformloader.so`. Replace
both with host-side virtual libraries (§3). Their 116 and 62 imports then never need
thunks.

## 3. The call boundary

### 3.1 Guest→host

Each resolved import becomes a 16-byte stub in a Cordial-owned executable page:
`svc #id; ret`. That gives 65,535 ids. `svc #0` stays reserved for real syscalls, so
`CallSVC(imm)` tells the two apart for free. The dispatcher reads x0–x7, v0–v7 and SP,
calls the host function, writes x0/v0 back, and returns inside the callback. That is
the fast path; see §4 for re-entry.

**Most imports need no bespoke code.** AAPCS64 and SysV both use separate integer and
FP register sequences. So one assembly trampoline can "pass x0–x7 as
rdi,rsi,rdx,rcx,r8,r9,[stack],[stack]; v0–v7 as xmm0–7; `al=8`". That is correct for
any non-variadic function with at most 8 integer and at most 8 FP scalar arguments
and no by-value aggregates.

*Corrected at M1*: appending the guest's stack arguments after x6 and x7 is
wrong whenever an FP argument overflows before the 7th integer argument, since
both ABIs order overflowed arguments by position but overflow different ones.
`snprintf(buf, n, "%g"×9 "%d"×4, ...)` came out wrong on 1000 of 1000 cases
with the fixed mapping and identical to native with a per-argument classifier
(`tests/m1.rs`, `m1_snprintf_fp_overflow_before_int_overflow`). So the
trampoline only loads a SysV image, and the image is built per call from the
argument types (`crates/cordial-guest/src/abi.rs`). Variadics then need nothing
but a type list, which for printf is the format. The per-function descriptor still needs types for two
fix-ups:

- **narrow integers**, where AAPCS64 leaves upper bits unspecified and Clang's SysV
  callees assume extension to 32 bits;
- **guest stack arguments** beyond 8.

Generate the descriptors with libclang from `third_party/mcpelauncher-linker/bionic/libc/include`
and the NDK headers, in the same spirit as FEX's libclang thunk generator (MIT) and
Berberis's `nogrod` DWARF layout reader (Apache-2.0). The generator must *refuse*
any signature it can't classify.

Counts (`readelf --dyn-syms`, `rest.txt`):

| Group | Count | Mechanism |
|---|---|---|
| libroblox imports | 616 (590 FUNC, 23 OBJECT, 3 NOTYPE) | |
| OpenXR (from loader) | 39 direct + 12 via `xrGetInstanceProcAddr` | generated from `xr.xml` |
| Meta platform (`ovr_*`) | 23 | hand-written, fail honestly |
| GLES/EGL | 91 (+`eglGetProcAddress`) | generated from `gl.xml`/`egl.xml`; likely unused in XR (INFERRED) |
| Vulkan (fetched via GetProcAddr) | 593 `vk*` name strings | generated from `vk.xml` |
| NDK: `A*` | ~49 + 23 AMedia + ~34 dlsym'd (AAudio 26, AThermal 5, AHardwareBuffer 3) | generic, and hand-written where there are callbacks |
| JNI | `JNINativeInterface` 229 + `JNIInvokeInterface` 5 | hand-written |
| libc/libm/libdl/liblog | ~390 | ~320 generic, ~70 hand-written |

**Hand-written, from the list:**

- **Variadic (12):** `printf fprintf snprintf sscanf fscanf __android_log_print open fcntl ioctl prctl mremap syscall`.
  AAPCS64 (Linux, not Apple) passes variadics in registers like named arguments,
  so a format parser pulls them from x/v registers.
- **`va_list` (6):** `vsnprintf vfprintf vasprintf vsscanf __vsnprintf_chk __vsprintf_chk`.
  The AAPCS64 `va_list` is a 32-byte struct `{__stack, __gr_top, __vr_top, __gr_offs, __vr_offs}`;
  SysV's is a 24-byte `__va_list_tag[1]`. The host walks the guest's structure by
  format. It cannot forward it.
- **`long double` (3):** `fmal powl strtold_l`. AAPCS64 uses a 128-bit IEEE quad in
  `q0`, x86-64 uses the 80-bit x87 format. Convert through `__float128`.
- **Callback-taking (12):** `qsort bsearch pthread_create pthread_once pthread_key_create __cxa_atexit __cxa_thread_atexit_impl __register_atfork dl_iterate_phdr ALooper_addFd signal sigaction`,
  plus AAudio data and error callbacks.
- **`setjmp`/`longjmp`.** These are thunks that operate on the *guest register file*:
  save or restore x19–x30, SP and d8–d15 into the guest `jmp_buf`. A `longjmp` that
  would cross a host frame aborts loudly.
- **Where the kernel ABI differs between arm64 and x86-64** (checked in headers):
  - `struct stat`: arm64 uses `asm-generic/stat.h`, x86-64 has its own.
  - `struct epoll_event`: `EPOLL_PACKED` applies only under `__x86_64__`
    (`linux/eventpoll.h:78`). `looper.rs:365` already documents this bug from the
    other direction.
  - `O_DIRECTORY/O_NOFOLLOW/O_DIRECT/O_LARGEFILE`: arm64 `040000/0100000/0200000/0400000`
    against generic `0200000/0400000/040000/0100000`.
  - `sigaction` and `ucontext`.
  - syscall numbers, both for `syscall()` and for raw `svc #0`, with ptrace and clone
    denied honestly.
- **Linker-aware:** `dlopen dlsym dladdr dlerror dlclose dl_iterate_phdr`,
  `getauxval`, `__system_property_get`.

**Refactor needed first.** The shim's ABI switches are keyed on the *host*:
`#[cfg(target_arch = "aarch64")]` in `bionic/pthread.rs` (×14) and in
`android/looper.rs`. In Phase 2 each one has to be classified by whether it describes
the bionic side (now the guest, so arm64) or the glibc side (still x86-64). The
pthread wrappers exist because *glibc aarch64's* `pthread_mutex_t` differs, so they
must **not** switch on. The epoll layout must. Introduce an explicit `GuestAbi`.

**Data imports** (`__sF stdin stdout stderr environ __stack_chk_guard optind optarg timezone daylight tzname in6addr_* AMEDIAFORMAT_KEY_*`)
resolve to host storage directly, which works because of identity mapping, laid out
as bionic does. The x86-64 shim already does this.

### 3.2 Generated APIs

**Vulkan.** Yes, generate it. Berberis already generates guest-arch Vulkan proxies from
`vk.xml` (`android_api/libvulkan/Android.bp`:
`gen_vulkan --guest_arch {riscv64|arm64} --host_arch x86_64`, Apache-2.0; the arm64
genrule is Digitalis's addition). The rules are:

- Every `PFN` handed *to* the guest (`vkGet*ProcAddr` results) becomes a stub.
- Every `PFN` handed *to* the host (`VkAllocationCallbacks`,
  `VkDebugUtilsMessengerCreateInfoEXT`, `XrVulkanInstanceCreateInfoKHR::pfnGetInstanceProcAddr`)
  is either unwrapped, if it is one of our own stubs, to the host function, or
  wrapped in a host→guest trampoline.
- `pNext` chains need no rewriting, because the layouts match (next paragraph).
- The existing `vulkan.rs` interposition (`VK_KHR_android_surface`, `vkCreateSwapchainKHR`)
  keeps its role behind the stubs.

**OpenXR.** Generate it from `xr.xml` (OpenXR-SDK, Apache-2.0) against the host
`libopenxr_loader.so.1` 1.1.47. Special cases:

- `xrInitializeLoaderKHR` is accepted as a no-op; desktop loaders need no init.
- `XR_KHR_android_create_instance` is stripped from the extension list and its
  `XrInstanceCreateInfoAndroidKHR` from the `next` chain before the host
  `xrCreateInstance`, the same pattern `vulkan.rs` uses for `VK_KHR_android_surface`.
  *Corrected at M6*: this said the extension is also added to the enumeration. It is
  not, and did not need to be: enumeration is the host runtime's, and the engine
  chains the Android struct without naming the extension (§9.5).
- `XR_KHR_vulkan_enable2`'s `pfnGetInstanceProcAddr` is unwrapped as above.

The engine names `XR_KHR_vulkan_enable2`, `XR_KHR_composition_layer_depth`,
`XR_FB_display_refresh_rate`, `XR_FB_space_warp` and `XR_META_performance_metrics`.
Enumerate truthfully from the host runtime.

**Layouts.** AAPCS64 LP64 and SysV x86-64 give identical size, alignment and offsets
for these C structs: natural alignment, 8-byte pointers and `uint64_t`, and
equivalent bitfield allocation for `uint32_t` fields (INFERRED for the one bitfield
struct, `VkAccelerationStructureInstanceKHR`). The exceptions are `long double`,
by-value HFA/HVA and ≤16-byte aggregates (none found in the Vulkan or OpenXR entry
points; the generator must assert this), variadics and `va_list`. Verify it
mechanically: compile one probe per API for `aarch64-linux-android26` and
`x86_64-linux-gnu` and diff `sizeof`/`offsetof`. Treat that diff as the gate, not
this paragraph.

**Meta platform.** A host virtual `libovrplatformloader.so` exports the 23 names.
Requests queue an `ovrMessage` whose `IsError` is true and whose message says the
service is unavailable. `GetIntegrityToken`, `GetUserProof` and entitlement
**never** return a value. The engine may refuse to continue; that is the correct
outcome, and it is a risk (§6).

### 3.3 Host→guest

Trampolines come from signatures we already know:

- **JNI.** The 526 exported `Java_*` symbols and anything passed to `RegisterNatives`
  carry a JNI signature, so a trampoline per shorty sets x0–x7/v0–v7 and calls the
  guest. This is the NativeBridge `getTrampoline(handle, name, shorty, len)` shape,
  and Berberis generates the same from `jni/api.json` (`gen_jni_trampolines.py`).
- **Rust call sites** that call engine natives directly (80 `Java_` references in
  `load.rs`) go through a `guest_call!(ptr, sig, args)`.
- **Everything else** gets fixed C signatures.

Calls return through a stub `svc #RET` placed in LR, whose handler halts the Jit.

## 4. Threads, TLS, signals, exceptions

**One `Jit` per (host thread × re-entry depth), created lazily.** `Run()` cannot
recurse, so a host function that calls back into the guest while this thread's Jit
is mid-callback (`qsort`, JNI up-calls, `dlopen` running constructors) takes the
next Jit in a small per-thread stack. Nested *distinct* instances work: measured at M1, three levels (guest →
host `qsort` → guest comparator → host `strlen`) matched native `qsort` on
1000 of 1000 cases with two Jits on the thread.

Host callees do **not** run under the host's MXCSR. dynarmic's `CallSVC`
path does not switch it back, so a callee sees the guest's FPCR as dynarmic
translated it: FPCR.RMode=RZ arrived as MXCSR 0x7fa0, FZ as 0x9fe0 (FTZ and
DAZ), against the host's 0x1fa0 (`m1_host_callee_runs_under_guest_rounding_mode`).
That is left alone deliberately -- bionic's callees would run under the
guest's FPCR too -- but a thunk that must not honour it has to set MXCSR
itself. The
fallback is yuzu's pattern: `HaltExecution`, dispatch outside `Run`, resume.

The cost is recompiling per thread plus a per-Jit code cache (start at 64 MiB,
lazily committed). Measure it, then consider a pool of Jits borrowed only while a
thread executes guest code.

**TLS.** Each guest thread gets a 9-slot bionic arm64 block, and TPIDR_EL0 points
at slot 0:

- slot 5 holds the stack guard. Use the same value as `__stack_chk_guard`, one per
  process, as bionic does.
- slot 1 holds the thread identity.

The guest uses TPIDR_EL0 as a thread identity as well as for the canary. There is no
`PT_TLS`, so `thread_local` goes through emutls to `pthread_getspecific` and needs no
DTV. Guest stacks are Cordial-allocated (8 MiB, as for the Android main thread).
*Corrected at M4*: that is the main thread's; a thread the guest creates gets
the stack size its attr asks for, or bionic's default of 1 MiB less 32 KiB,
and every Cordial-allocated guest stack sits above a 64 KiB guard (§9.3).
`pthread_attr_getstack` and `pthread_getattr_np` report the *guest* stack.

**`pthread_exit`** must not call glibc's `pthread_exit` from inside a callback. Its
forced unwind would hit JIT frames that have no unwind info. Instead: run guest
key destructors and `__cxa_thread_atexit` entries, halt, and return from the
thread's top frame.

**Exceptions.** libc++abi and libunwind are static in libroblox; it imports no
`_Unwind_*`. Unwinding therefore stays entirely in guest code and finds `.eh_frame`
through the `dl_iterate_phdr` thunk, which must list guest objects with guest
`phdr`s. Nothing crosses the boundary. A guest throw out of a host→guest callback
reaches a frame with no FDE and terminates, which is what JNI does on Android too.

**Signals.** The host owns every real handler. The `sigaction` and `signal` thunks
record the guest's handlers virtually and never install them. dynarmic installs its
fault handler after Cordial's, so chaining works. *Checked at M3* (§9.2): the
handler before the first Jit is Rust's `stack_overflow::signal_handler`, after it
dynarmic's `SigAction`, which chains to it.

- **Asynchronous signals** aimed at guest threads set a pending bit and call
  `HaltExecution` (an atomic OR; async-signal-safe INFERRED). The dispatcher then
  builds an arm64 `rt_sigframe` on the guest stack and runs the handler.
- **Synchronous faults in JIT code** cannot be mapped to a precise guest PC, so they
  are fatal. Crashpad (`CrashpadHandlerMain`, `fork`/`execv` imports) must fail
  honestly: `execve` of an arm64 ELF returns `ENOEXEC`.

**Atomics.** With `ATOMICS` unadvertised, every atomic is LL/SC through the global
monitor spinlock. With ~60 engine threads that serialises. Measure both of these:

- `fastmem_exclusive_access` plus `Unsafe_IgnoreGlobalMonitor`: `cmpxchg` against the
  loaded value. It is ABA-tolerant for the add, swap and cas loops that outline
  atomics emit (INFERRED).
- The better fix: implement LSE in dynarmic's A64 frontend. `CAS`→`lock cmpxchg`,
  `LDADD`→`lock xadd`, `SWP`→`xchg`, and `LDSET`/`LDCLR`→a `cmpxchg` loop. Then
  advertise `ATOMICS`. It would also be a worthwhile upstream patch.

**Self-modifying code.** Set `hook_isb` and handle `InstructionCacheOperationRaised`.
Invalidation must reach *every* Jit: queue it per Jit and `HaltExecution` the
others. Better still, turn Luau native codegen off through the engine's own
FastFlags (ADR-005); the exact flag is INFERRED. *Built at M7* (§9.10), keyed
on the guest's `mmap`/`munmap`/`mprotect`/`madvise` rather than on cache
operations, which the engine only issues alongside an `mprotect`.

## 5. Prior art

- **Android NativeBridge** (`libnativebridge`, public AOSP): a `NativeBridgeCallbacks`
  vtable with `loadLibrary`, `getTrampoline` by JNI shorty, `isSupported` and
  signal hooks. libndk_translation itself is closed. Digitalis's README reports that
  Google's x86-64 emulator images ship an arm64 Berberis build as
  `libndk_translation.so`, API 34–37.
- **Berberis** (AOSP, Apache-2.0) is the closest design. It has:
  - proxy libraries per API (`android_api/lib{c,vulkan,EGL,GLESv2,aaudio,…}`);
  - generated Vulkan and JNI trampolines;
  - a guest ABI layer (`guest_abi/arm64/guest_params_arch.h`);
  - syscall translation generated from `kernel_api/tools/kernel_api_arm64.json`.

  **Digitalis** (`DigitalisX64`, Apache-2.0 entry repo; the implementation repo
  shows no licence file, so check headers per file) adds an open arm64→x86-64
  backend. Its coverage table lists `casal`, `ldaddal`, `swpal` and `ldapr` as
  translated. It lives inside Soong and assumes an x86-64 *bionic* host, so
  extracting it is a large porting job. **Use it as the reference for ideas, and
  adapt its code with attribution; Apache-2.0 into GPL-3 is fine.**
- **FEX** (MIT) and **box64** (MIT) run the opposite direction. FEX's libclang
  thunk generator and its layout-compatibility checks are the model for §3.1, and
  box64's wrapped-library tables for printf and `va_list` handling.
- **unidbg** (Apache-2.0) and **linnaria** (GPL-3.0; per its README, not playable)
  both run Android `.so` files through dynarmic plus thunks.
- **Licences to respect.** Citra and Azahar are "GPLv2 or any later version" and may
  be adapted into GPL-3. yuzu's GitHub is DMCA-blocked; read only mirrors whose
  per-file SPDX you have checked. QEMU as a whole is GPL-2.0-only, so check each
  `linux-user` file's header before adapting anything. Eden is GPL-3.0-or-later.

## 6. Performance, honestly

There is no published dynarmic-versus-native figure; Hydra's documentation says only
"slightly slower than the Hypervisor". The nearest numbers are the box86 blog's
same-host comparison (2022, integer 7-zip, percent of native): QEMU 11–16%, FEX
20–26%, Box64 50–53%, Rosetta 2 about 71%. With SIMD-heavy dav1d: QEMU 6%, FEX 15%,
Box64 27–29%. dynarmic is a block JIT with per-block register allocation, guest
state in memory between blocks, and ARM-exact FP/NaN fix-ups unless the `Unsafe_*`
flags are set. **My estimate is 20–45% of native for integer work, lower for NEON FP
with safe flags** (INFERRED).

The host is an i9-13900KS: 8 P-cores and 16 E-cores, no AVX-512, so dynarmic takes
its AVX2 paths. A P-core runs roughly 2.5–3× a Quest 3 XR2 Gen 2 core single-threaded
(INFERRED). Multiply the two and the emulated engine gets about **0.5–1.3× the
Quest's own CPU throughput**.

- 72 Hz is plausible. 90 Hz is doubtful.
- JIT warm-up will hitch for the first minutes of each place. Reprojection hides
  some of it.
- Threads that land on E-cores are much slower, so pin hot guest threads to P-cores.
- Thunk traffic: about 1.5 M Vulkan calls/s at 90 Hz × a few thousand draws × ~50 ns
  ≈ 5–10% of one core (INFERRED).
- The engine uses its own allocator (no `malloc`/`free` imports; ADR-040), which
  helps. `memcpy`/`strlen` do not: consider a guest-side arm64 fast path, such as
  ARM optimized-routines (MIT or Apache-2.0), that takes the `svc` only for large
  sizes.

**This is the largest risk. Measure it at M3, before investing in M5–M7.** The M3
measurement (§9.2) is of cold, run-once code, where dynarmic took 1.9× qemu's time
for the constructors; it says little yet about steady-state throughput, which is
what the estimate above is about.

## 7. ADR line

Running the engine's unmodified instructions in a translator, and supplying its
imports as stubs in Cordial-owned memory, is the class of thing the linker already
does. The design stays inside it only if:

- (a) dispatch is keyed only on Cordial's own stub addresses and SVC ids, never on
  engine addresses. There is no PC-keyed hook table, no breakpoint, and no
  `InvalidateCacheRange`-and-substitute. Enforced in `cordial-guest`: a
  handler can be installed only in the next free stub slot, and an `svc`
  outside its own stub is the guest's syscall (`tests/stub_page.rs`).
- (b) nothing exposes Jit state to plugins.
- (c) the outline-atomics byte, the anti-tamper `svc` sites and the engine's code are
  never written. Behaviour changes only through what a real platform would answer:
  hwcaps, syscalls, FastFlags.

One nuance: `InterpreterFallback` emulating CNTVCT or ID registers is CPU emulation,
not engine patching. Nothing above crosses the line. A translator *could* be turned
into a hooking primitive, so (a) belongs in a fork ADR. That ADR must also supersede
`docs/multiarch.md`'s "do not build a translation layer" for this fork.

## 8. Milestones

| # | Goal | Pass / fail (control) |
|---|---|---|
| M0 | Azahar dynarmic builds from `build.rs`. A `cargo test` runs `add x0,x0,x1; ret` | Returns 5 for (2,3) and halts at the `svc #RET` stub. Control: a `.word 0` gives `InterpreterFallback` (corrected from `ExceptionRaised(UnallocatedEncoding)`, §1.2). **Done.** |
| M1 | Stub, generic trampoline, nested Jit | Guest calls host `strlen`, `snprintf` (variadic, mixed int/double) and `qsort` with a guest comparator. Outputs are byte-identical to native x86 on 1,000 randomised cases. `pthread_create` gives 64 guest threads × 10⁶ LL/SC increments = exact total, with the global monitor and with `IgnoreGlobalMonitor`, timed. |
| M2 | The guest-arch linker links libroblox inside x86-64 `cordial-run` | 0 unresolved. The relocation counts it logs equal the `llvm-readelf` table in §2 exactly. **Done** (§9; the table is re-measured there for 2.740.927). |
| M3 | 3,617 constructors, then `JNI_OnLoad` | `JNI_OnLoad` returns `0x10006`. The class dump is identical to Phase 1's 4,458 lines. 0 unexpected `ExceptionRaised` or `InterpreterFallback`. Control: advertise `ATOMICS` and it stops with `InterpreterFallback` at a helper. **Record wall time against Phase 1's qemu figure**, since this is the first performance evidence. **Done** on 2.740.927's 3,622 (§9.2), with the wall time the wrong way round: slower than qemu on this run-once code. |
| M4 | Engine start-up with threads | 3 × 120 s runs without a crash. `cordial_loopers` and `cordial_info` show progress. Jit count, code-cache bytes and per-thread CPU are logged. **Done** (§9.3), with no frame drawn: the Quest build ships only Vulkan shader packs, so its GLES fallback, which does reach the host GPU, has nothing to draw with. |
| M5 | Vulkan via generated thunks on the NVIDIA ICD | `vkCreateDevice` succeeds on the 4070 Ti. The layout-diff gate passes with 0 differences. A flat or companion frame is non-black in `cordial_screenshot` (INFERRED that the Quest build renders flat at all; if not, go to M6). **Stopped** (§9.4): the gate passes and the generated stubs reach the 4070 Ti, but the engine abandons Vulkan when OpenXR has no runtime, before `vkCreateInstance`, so no frame. |
| M6 | OpenXR on a host runtime, then WiVRn | Session state reaches `FOCUSED`. `xrEndFrame` is submitted with a projection layer. Rate ≥ 72/s sustained over 120 s with input driven, with the headset displaying. Cross-check against the runtime's own frame timing. **Part A done** (§9.5) on Monado's simulated HMD rather than SteamVR: `FOCUSED`, one projection layer per frame, 89.95–90.00/s at 90 Hz over 60 s in 3 of 3 120 s runs and 143.93/s at 144 Hz, a non-black left eye. Part B (WiVRn, the Quest 3 displaying) used by hand, not measured: no frame log has been taken on a headset (ADR-053, "Not established"). |
| M7 | Performance pass | Report p50/p99 CPU frame time with each toggled against a same-session control: `Unsafe_*` FP flags, `IgnoreGlobalMonitor`, P-core pinning, LSE-in-dynarmic. Pass is 72 Hz with under 1% dropped frames in one reference place, 3 runs. **First pass** (§9.8): the stub boundary's bookkeeping and `pthread_getspecific` removed, 35--41 to 44--48 XR frames/s at 90 Hz in place 11256291667 on Monado; the `Unsafe_*` FP flags and P-core pinning measured and bought nothing. Not passed: p50 is two display periods. |

## 9. Measured at M0 and M1

`crates/cordial-guest`, dynarmic `a46601580d55`, i9-13900KS (32 hardware
threads). Every figure is from `tests/m0.rs` or `tests/m1.rs`.

| | Result |
|---|---|
| `add x0,x0,x1; ret` (2,3) | 5, stopped at the `svc #RET` stub + 4 |
| `strlen`, `snprintf` (22 conversions, int and FP both overflowing), `qsort` with a guest comparator, nested `qsort`/`strlen` | 1000/1000 identical to the same host function called natively |
| Guest `snprintf` cost | Indistinguishable from native: 23.50 s against 23.51 s over the 1000 cases, nearly all of it glibc's `%f` on huge doubles |
| LL/SC, 1 thread | 20 ns per increment with `IgnoreGlobalMonitor`, 252 ns with the global monitor (native `lock xadd` 4.2 ns) |
| LL/SC, 64 threads × 10⁶ on one word | exact (64,000,000) in both modes: 14.1 s with `IgnoreGlobalMonitor` (220 ns aggregate per increment; native `lock xadd` 0.71 s, 11 ns), **970 s** with the global monitor (15.2 µs, 28,612 CPU-seconds spinning) |

The global monitor is a spinlock, and with more guest threads than hardware
threads a holder that is descheduled stalls everyone: 950 ns at 8 threads,
5.4 µs at 32, 9.8 µs at 64. With about 60 engine threads that makes
`IgnoreGlobalMonitor` (or LSE in the frontend, §4) close to a requirement
rather than an M7 option. That is INFERRED for the engine, whose atomics are
not all on one word.

Also established: HFA arguments differ, as §3.2 warned, and in a specific way
(clang, both targets): `struct {float x,y,z,w}` is s0--s3 one per register in
AAPCS64 and packed two per xmm in SysV; `struct {double x,y,z}` is d0--d2 in
AAPCS64 and memory in SysV. `va_list` is 32 bytes against 24.

## 9.1 Measured at M2

`cordial-run --guest-arm64` (`patches/0006`, `crates/cordial-runtime/src/guest_link.rs`),
Quest build 2.740.927, which is not the build §2 and §3 were measured on:

| | 2.739.687 (§2, §3) | 2.740.927 |
|---|---|---|
| RELATIVE / ABS64 / GLOB_DAT / JUMP_SLOT | 570,325 / 22 / 56 / 586 | **572,841 / 22 / 56 / 598**, equal to `llvm-readelf -r`, three runs |
| imports | 616 (590 FUNC, 23 OBJECT, 3 NOTYPE) | 629 (603 FUNC, one of them weak; 23 OBJECT; 3 NOTYPE, all weak) |
| `xr*` / `ovr_*` imports | 39 / 23 | 40 / 24 |
| `init_array` entries | 3,617 | 3,622 |

Every one of the 676 symbol relocations was checked against the table from
the linker's own trace: the 648 function-import slots each hold that
import's own stub in the stub page, the 23 data slots the storage named for
them, and the four slots of `__gcov_dump`/`__gcov_flush` zero, as on
Android. One JUMP_SLOT binds to a function libroblox defines itself.

How the 629 are answered: 62 dispatch to Cordial's implementation and 228 to
the host's, through the generic call builder; 8 are thunks (printf family,
`qsort`, `pthread_create`); 40 are the virtual OpenXR loader's honest failures;
266 stop the guest with a fault naming the function when called; 23 are data;
2 are weak and null. The stops are the M3 backlog, and each carries its
reason.

*Corrected at M2*: "the stub dispatches to Cordial's implementation" is not
true of every function Cordial implements. Several of those implementations
translate bionic's **x86-64** layouts (`stat`, `sigaction`, the `O_*` flags),
which are exactly the ones §3.1 lists as differing on arm64. Only functions
whose arguments mean the same on both sides dispatch; the rest stop.

## 9.2 Measured at M3

`cordial-run --guest-arm64 --host-libc --jni-onload`, Quest build 2.740.927,
i9-13900KS. The constructors run in linker order on the main thread's Jit,
with TLS slot 5 equal to `__stack_chk_guard`, then `JNI_OnLoad` with a guest
`JavaVM` (`crates/cordial-runtime/src/guest_jni.rs`).

**Stops met, in order**, each answered by a thunk in
`crates/cordial-runtime/src/guest_libc.rs` unless named otherwise:

| # | Where | Stop | Answer |
|---|---|---|---|
| 1 | constructor 0 | `getauxval` | the §1.3 hwcaps; `AT_HWCAP2` 0; ids and `AT_RANDOM` the host's; unknown types 0 with ENOENT |
| 2 | constructor 1 | `__system_property_get` | Cordial's table, which names no instruction set (moved to the dispatch list) |
| 3 | constructor 2 | `newlocale` | bionic's locale model: `{mb_cur_max}` objects, five names, `_l` functions ignore the locale, multibyte always UTF-8 via the host's C.UTF-8 |
| 4 | constructor 3 | `sysinfo` (and with it `writev`, `select`, `sendmsg`/`recvmsg`/`*mmsg`) | the kernel's own LP64 structures, dispatched |
| 5 | constructor 3 | `__open_2` (and `open`, `fcntl`, `stat`/`lstat`/`fstat`, `statvfs`) | Cordial's path layer, O_* renumbered, arm64's `struct stat` |
| 6 | constructor 3 | `prctl` | passed through; arm64-only options get EINVAL, which is true of the emulated CPU |
| 7 | constructor 3 | `mmap` (and `munmap`, `mprotect`) | the host's, PROT_EXEC withheld and executable ranges tracked; a change to one stops, since cross-Jit invalidation is not built (*built at M7*, §9.10) |
| 8 | constructor 71 | `vsnprintf` (and `__vsnprintf_chk`, `__vsprintf_chk`, `vfprintf`, `vasprintf`) | the AAPCS64 `va_list` walked by format onto the host's plain variadic |
| 9 | constructor 71+ | dynarmic `assertion failed: MayGetNZCVFromOp` | a translator bug, `patches/0007` (below) |
| 10 | constructor 3550 | `syscall` | arm64 numbers mapped to x86-64's for 57 arch-neutral calls; others ENOSYS, named once |
| 11 | constructor 3592 | `fscanf` (and `sscanf`, `vsscanf`) | argument count from the format; `%L` floats and `%m` refused |
| 12 | `JNI_OnLoad` | the host `JavaVM` | a guest `JavaVM`/`JNIEnv`, 5 and 229 stubs generated from jni.h (`tools/vr/gen-guest-jni.py`) |

Also answered before they were reached, because the same work covered them:
`uname`, `__cxa_atexit`/`__cxa_finalize`/`__cxa_thread_atexit_impl`,
`pthread_once`, `pthread_key_create`, `bsearch`, the six `dl*` (the linker's
own `__loader_*` with the guest caller's address; `dlsym` refuses host
addresses; `dl_iterate_phdr` lists guest objects only), and `strtold_l`. And
`fopen`/`open` of `/proc/cpuinfo` and `/proc/self/auxv` now come from memory,
describing the CPU `getauxval` describes (Features `fp asimd aes pmull sha1
sha2 crc32 fcma asimddp`, implementer 0x00): the constructors read cpuinfo,
and qemu-user 10.2, the Phase 1 reference, fakes an arm64 one too (with its own
CPU's much longer list, `atomics` and `sve` included), so the host's x86 file
was the one answer that matched neither.

The import table is now 64 Cordial, 235 host, 75 thunk, 40 OpenXR honest
failure, 190 stop, 23 data, 2 weak-null (629). *Corrected at M3*: the stop
note said bionic's and glibc's `LC_*_MASK` values differ; checked against both
headers, they are the same. What differs is `locale_t` and the behaviour.

**The translator bug.** Constructor 71's successor block, at libroblox+0x1e6214c,
ends `ands x12, x12, x13; b.eq` with x13 all ones. dynarmic's constant folding
turned `x & ~0` into `x` and re-pointed the `ANDS`'s `GetNZCVFromOp` at a shift,
which asserts. `FoldAdd`/`FoldSub` already skip an op with a pseudo-op;
`FoldAND`/`EOR`/`OR`/`NOT` did not (`patches/0007`, applied by
`tools/vr/build-aarch64.sh`). Found through a ring of the last 64 code fetches
that `crates/cordial-guest/native/shim.cpp` now prints when dynarmic terminates.

**Pass**, three runs of three: `CONSTRUCTORS DONE: 3622`, `JNI_OnLoad returned
0x10006`, and a `--dump-classes` file byte-identical (`cmp`) to the qemu Phase 1
binary's for the same build, 4,458 lines; the qemu runs' own dumps agree with
each other. No `ExceptionRaised` or `InterpreterFallback` in any of them: the
Jit stops on either, and none stopped.

**Control.** `CORDIAL_GUEST_HWCAP_ATOMICS=1`: constructor 2 stops with
`InterpreterFallback` at libroblox+0x2be8c60, which is `casalb w0, w1, [x2]` in
`bti c; adrp x16; ldrb w16, [x16, #0x968]; cbz w16; casalb; ret`.

**Timing**, wall clock from the first constructor to `JNI_OnLoad` returning,
dynarmic and qemu-user (Phase 1's aarch64 binary, `CORDIAL_TIME_CTORS=1`, which
needs `patches/0003`) interleaved run by run, 3 each. min / median / max:

| | dynarmic | qemu | ratio (median) |
|---|---|---|---|
| constructors | 1.439 / 1.455 / 1.477 s | 734 / 774 / 822 ms | 1.88× |
| `JNI_OnLoad` | 193.4 / 194.7 / 194.9 ms | 150.7 / 157.0 / 159.9 ms | 1.24× |
| total, first constructor to `JNI_OnLoad` returning | 1.635 / 1.650 / 1.672 s | 1.018 / 1.075 / 1.115 s | 1.53× |

The totals include creating the `JavaVM` between the two, which is native under
dynarmic and emulated under qemu, so the rows above them are the fairer
comparison. The machine was not idle (load average 5–8 of 32 threads, a desktop
session with Roblox Studio under Wine and a chat client busy); both binaries
pinned to one P-core's two threads (`taskset -c 6,7`) gave the same figures
within 2% (dynarmic constructors 1.448 / 1.483 / 1.501 s, qemu 762 / 782 /
783 ms). With `CORDIAL_GUEST_MONITOR=ignore` (`IgnoreGlobalMonitor`),
`JNI_OnLoad` drops to 160.2 / 162.6 / 162.9 ms and the constructors to
1.430 / 1.431 / 1.439 s: about 30 ms of `JNI_OnLoad` is exclusive accesses
through the global monitor, on one thread. An earlier set, before cpuinfo was
served, gave the same picture (constructors 1.41--1.45 s against 745--766 ms).

Per run: 2 Jits (the second for callbacks the host makes back into the guest,
which compiles its own copy of anything it runs), 1.04 M guest instructions
fetched for translation, 41 MB of anonymous executable memory resident (the two
64 MiB code caches, lazily committed), 812,519 guest-to-host calls, 92
distinct. `pthread_getspecific` is 443,670 of them (the engine's `thread_local`,
through emutls), then `strcmp` 117k, `strlen` 68k, `memmove` 48k,
`pthread_mutex_lock`/`unlock` 41k each. With `CORDIAL_GUEST_PROFILE=1` the host
side of all those calls, exclusive of guest code they call back into, is 56 ms.

What that says, INFERRED: the time is translation, not execution or the
boundary. The code runs once, qemu's TCG translates cheaply and dynarmic
optimises every block it compiles, and 1.04 M instructions became about 40
bytes of x86 each. This is the worst case for dynarmic and the best for qemu,
and it measures neither's steady-state throughput -- M4's engine loop is the
first place that can.

**Signals.** dynarmic's `SIGSEGV` handler is installed when the first Jit is
built and chains to whatever was there. In `cordial-run` that is Rust's own
stack-overflow handler, which is the only `SIGSEGV` handler the guest path has:
checked under gdb, `sigaction(SIGSEGV)` names
`std::sys::pal::unix::stack_overflow::imp::signal_handler` at the first
`cg_jit_new` and `Dynarmic::Backend::(anonymous namespace)::SigHandler::SigAction`
at exit. Nothing else is needed while the guest's own `sigaction` stays virtual
(§4). `crates/cordial-guest/tests/m3.rs` checks the three outcomes in child
processes after a Jit exists: a host null dereference and a guest wild pointer
(faulting in translated code) both end in SIGSEGV as before, dynarmic adding an
`Unhandled SIGSEGV at rip` line; a stack overflow on the thread whose alternate
signal stack dynarmic replaced is still named by Rust ("has overflowed its
stack") and aborts.

**Not done, and known:**

- One `pthread_create`, with a stack-size attr, is refused with EINVAL, as M1's
  thunk does for any non-null attr; the engine carried on. qemu created that
  thread. M4 needs it. *Done at M4* (§9.3), and creating it is what exposed
  the shared `JNIEnv`.
- `RegisterNatives` is built (each `fnPtr` behind a host entry from its
  descriptor, env mapped back to the guest's) but this `JNI_OnLoad` never calls
  it; the host entry itself is checked by `m3_host_entry_...` (1000 cases, 21
  arguments overflowing both ABIs' registers, bit-identical).
- `/proc/cpuinfo` has no `BogoMIPS` line, and every processor is listed with
  the same features, which is true of the translator but not of any big.LITTLE
  phone. The class dump came out identical with the host's x86 cpuinfo and
  with this one, so M3 cannot say whether the engine acts on it.

## 9.3 Measured at M4

`cordial-run --guest-arm64 --host-libc --app-bridge`, Quest build 2.740.927,
the same bring-up `load.rs` drives the native and qemu builds through
(`tools/vr/run-quest-dynarmic.sh`). The engine's `Java_*` exports reach the
guest through host entries built from the `native` prototypes the APK's dex
declares (`crates/cordial-runtime/src/guest_dex.rs`; 1,400 names), installed
as `linker::set_symbol_filter`, so no bring-up call site changed.
`JNI_OnLoad` goes through the same filter with the guest `JavaVM` swapped in.

**Stops met, in order** (`guest_sys.rs` unless named):

| # | Where | Stop | Answer |
|---|---|---|---|
| 1 | `JNI_OnLoad`, once `pthread_create` succeeded (the M3 path too) | dynarmic `code is too big` from inside translated code, or, as often, a SIGSEGV | not the code cache (256 MiB changed nothing): the main thread's 8 MiB guest stack had run out and was writing below it. Made visible with a 64 KiB guard page under every guest stack and a check in the SVC path that names the last 64 stub calls when SP nears the guard; the cause is 2 |
| 2 | the same | one engine function at libroblox+0x6385ae4 calling itself about 4,000 deep | **one `JNIEnv` was shared by every thread.** The engine interposes on the env it is given (keeps `env->functions`, installs its own table), so the second thread's interposition kept the first one's table as "the original". Now each thread has its own env and its own copy of the table (`guest_jni.rs`). Found by calling the two thread-local getters the loop used, on the faulted thread: one held a `JNIEnv*` (INFERRED to be the guest's, from its address), the other a copy of a `JNINativeInterface` whose `FindClass` slot was the looping function. Checked: without guest threads (`CORDIAL_GUEST_THREADS=0`, EAGAIN) it did not happen |
| 3 | — | `pthread_create` wrote the handle after the thread started | glibc now writes it through the guest's pointer, before the thread runs, as bionic does. A real race; not the cause of 2, which it was first taken for |
| 4 | worker thread | `ioctl` | the host's for `FIO*`, `TIOCGWINSZ` and `SIOCGIF*` (same numbers and layouts on both); anything else stops by number |
| 5 | worker thread | `getnameinfo` | the host's with bionic's `NI_*` renumbered and glibc's negative `EAI_*` mapped to bionic's |
| 6 | worker thread | `mallinfo` | ten `size_t`s returned through x8, from glibc's `mallinfo2`; `ldiv` in x0/x1 |
| 7 | render thread | `eglGetDisplay` | EGL/GLES signatures generated from Khronos's `gl.xml`/`egl.xml` (`tools/vr/gen-guest-gl.py`, 1,055 commands, 5 left out because they take a callback), dispatched like `FUNCS` to the native table's answers -- the host's libEGL/libGLESv2 and Cordial's EGL overrides. `eglGetProcAddress` hands back a stub per name, or null for a name the table lacks |
| 8 | engine thread | `InterpreterFallback` at libroblox+0x22ef224, `mrs x8, cntvct_el0` | emulated: the same clock and 600 MHz dynarmic gives CNTPCT/CNTFRQ, offset 0 (`jit.rs`) |

Answered before they were reached: `pthread_create` with the attr's stack
size, detach state and explicit scheduling; `pthread_exit` (leaves the guest
frames as `Fault::ThreadExit`, then runs the thread's destructors);
`pthread_getattr_np` describing the guest stack; `thread_local` destructors
and key destructors at guest thread exit; `sigaction`/`signal`/
`pthread_sigmask`/`sigaltstack` held virtually (§4); `setjmp`/`longjmp` on
the guest's callee-saved registers, refusing a jump across a host frame;
`epoll_ctl`/`epoll_wait` between arm64's 16-byte and x86-64's packed 12-byte
`epoll_event`; `ALooper_addFd` with the callback behind a host entry;
`AAssetManager_fromJava`/`ANativeWindow_fromSurface` with the host env;
`sem_*` to Cordial's bionic-layout semaphores; bionic's `__FD_*_chk`,
`__sendto_chk` and POSIX `strerror_r`. In a 70 s run the engine called
`sigaction` 60 times and installed no handler (every call a query,
`SIG_DFL` or `SIG_IGN`), `setjmp` 62 times and `longjmp` never, and no
`xr*` or `ovr_*` import at all.

**Graphics.** The engine asks for Vulkan first, and says so: `Mode 6 failed:
Unable to load Vulkan API`, because the guest's `dlopen("libvulkan.so")`
finds nothing -- no guest Vulkan exists until M5. qemu fails the same mode
differently (`checkXrResult failed OpenXr call`, having created a lavapipe
instance). It then falls back to GLES, and under dynarmic that reaches the
NVIDIA GPU through the generated thunks: `GL Renderer: NVIDIA GeForce RTX
4070 Ti/PCIe/SSE2`, `GL Version: OpenGL ES 3.2 NVIDIA 595.91.07`, a context
and window surface created. Then `Mode 4 failed: Error opening shader pack
glsles3` and `RenderView is NULL`, exactly as under qemu, because the Quest
APK's `shaders/` holds only `shaders_vulkan_mobile.pack` and
`shaders_vulkan_mobile_vr.pack`. **So nothing is drawn on either
translator, and the renderer this build can use is Vulkan: M5.** The engine
logs `SurfaceController::enableVR` and carries on without a RenderView.

**Pass**, three runs of three at 120 s, each `exit=0`, each logging `APP_READY
PlatformAccountRouter`, `Startup` and `Landing` (at 7.71, 7.39 and 7.75 s),
seven more (IgnoreGlobalMonitor, profiling, the default after it changed)
the same. `cordial_info` reads `presents=0` twice five seconds apart in every
run, since there is no swapchain; progress is the looper census (the main
looper at 1,752--1,793 polls 16 s in, moving) and the guest's own counters,
which rise every ten seconds for the whole run. `cordial_screenshot` answers
"no frame was presented within three seconds": there is no frame to read.

**Performance**, per-thread CPU from `/proc/<pid>/task/*/stat` over 30 s
starting 5--10 s after Landing, dynarmic and the qemu Phase 1 binary
interleaved, three each, min / median / max:

| | dynarmic, exact monitor | dynarmic, IgnoreGlobalMonitor | qemu-user |
|---|---|---|---|
| to `APP_READY Landing` | 7.39 / 7.71 / 7.75 s | 6.77 / 6.78 / 6.78 s | 10.16 / 11.97 / 17.17 s |
| whole process, % of one core | 20.8 / 22.1 / 22.7 | 14.9 / 16.2 / 20.0 | 24.4 / 25.4 / 27.4 |
| busiest thread (`RBX Worker B`) | 7.6 / 8.2 / 8.3 | 5.5 / 6.0 / 7.7 | 10.6 / 10.9 / 12.1 |
| threads | 61 | 61 | 87 |

The load average was 3.1--5.1 on 32 hardware threads throughout (a desktop
session; nothing pinned). Per dynarmic run, at 120 s: 62--64 Jits
for 40 guest threads, 279--289 MiB of translated code resident, 2.81--2.95 M
guest instructions translated, 94 host entries, and 266--267 k guest-to-host
calls a second (315 k at 60 s). The five most called:
`pthread_getspecific` about 174 k/s (emutls), `strncmp` 31 k/s,
`clock_gettime` 17 k/s, `pthread_mutex_lock` and `_unlock` 10 k/s each.
`CORDIAL_GUEST_PROFILE=1` reports the host side of those calls as 6--8 cores
of wall time, which is blocking waits (futex, `epoll_wait`) summed over
threads, not CPU; it is not a cost figure.

What that says, INFERRED: this is an **idle** engine -- Landing with no
RenderView, so no render job, no frames and no Luau frame work -- and 20 % of
a core is HTTP retries, the task scheduler and worker bookkeeping. On that
work dynarmic took about 80 % of qemu's CPU (median 22.1 against 25.4 %, and
8.2 against 10.9 % on the busiest thread), and reached Landing 4 s sooner. It
is steady-state translated code, which M3's run-once constructors were not,
and it is the first figure that points dynarmic's way; it says nothing yet
about frame-time throughput, and no frame rate can be taken until M5 draws.

**IgnoreGlobalMonitor**, same session, three runs each: the process's CPU at
Landing fell from a median of 22.1 % to 16.2 % of a core and Landing came
0.9 s sooner, with every run passing. The risk accepted is ABA: with the
monitor ignored, a STXR succeeds whenever memory still holds the value LDXR
read, where hardware would fail it after any intervening write. The engine's
atomics go through the outline-atomics helpers (§1.3), whose LL/SC loops
implement compare-and-swap, fetch-and-op and swap -- and a value compare is
exactly C++'s `compare_exchange`, so for those the result is the same.
Hand-written LL/SC that relies on the reservation itself would not be;
none is known, and that is INFERRED from the helpers' shape, not checked
site by site. It is now the default, and `CORDIAL_GUEST_MONITOR=global`
restores the exact monitor.

**Not done, and known:**

- Vulkan for the guest (M5), without which nothing draws.
- Signals are never delivered to a guest handler. None was installed in
  these runs.
- Guest thread exit runs key and `thread_local` destructors only for threads
  the guest created; a Cordial thread that entered the guest through a host
  entry keeps its values.
- `eglGetProcAddress` answers null for seven names the engine asked for that
  are desktop GL, not GLES (`glMapBuffer`, `glQueryCounter`,
  `glBufferStorage`...). The NVIDIA host has them; Android's GLES would not.
- Each re-entry depth on a thread has its own Jit and its own copy of any
  code it runs: 62 Jits for 40 threads, about 4.6 MiB of code each.
- `getnameinfo` with bionic's flags is right on the guest path; the native
  x86-64 path still passes bionic's `NI_*` to glibc unchanged.

## 9.4 Measured at M5

`cordial-run --guest-arm64 --host-libc --app-bridge`, Quest build 2.740.927,
host Vulkan headers and loader 1.4.341, NVIDIA 595.91.07.

**The guest's `libvulkan.so`** (`crates/cordial-runtime/src/guest_vk.rs`)
exports what the native virtual library does, `vkGetInstanceProcAddr` alone,
registered as `libvulkan.so` and `libvulkan.so.1`. It calls Cordial's native
`vkGetInstanceProcAddr` and hands back a stub per (command, host function),
so `VK_KHR_android_surface`, the present-mode rewrite and the capture behind
`cordial_screenshot` stay behind the stubs. The signatures are generated from
`vk.xml` v1.4.341 (`tools/vr/gen-guest-vk.py`): 804 commands; 3 left out
(NvSci types, which have no Linux meaning). What cannot cross unchanged is a
function pointer the host would call, and the generator finds 9 such members
in 5 structs:

| Struct | Where it reaches the host | Answer |
|---|---|---|
| `VkAllocationCallbacks` | `pAllocator` of 135 commands | each function behind a host-to-guest entry; one host copy per distinct struct, kept, since destroy must see an allocator compatible with create. Never dropped: the specification does not let an implementation ignore one |
| `VkDebugUtilsMessengerCreateInfoEXT`, `VkDebugReportCallbackCreateInfoEXT` | their create commands, and `VkInstanceCreateInfo`'s chain | the chain is copied up to the last such link, relinked, the callback replaced by an entry; the rest of the guest's chain is its own, and no guest struct is written |
| `VkDeviceDeviceMemoryReportCreateInfoEXT` | `VkDeviceCreateInfo`'s chain | the same |
| `VkDirectDriverLoadingInfoLUNARG` (through `...ListLUNARG`) | `VkInstanceCreateInfo`'s chain | refused by name: there is no guest driver to load |

Copying a link needs its size, so the generator compiles the gate's probe for
x86-64 and records every sType-bearing struct's size (1,070); the unit test
checks those against the headers, and that every such struct has `sType` at 0
and `pNext` at 8.

**The layout-diff gate** (`guest_vk::tests::layout_gate`, over the generated
`guest_vk_probe.c`): every struct and union `vulkan_core.h`,
`vulkan_android.h` and `vulkan_wayland.h` declare -- 1,259 types, 10,257
sizes, alignments, sType values and member offsets, and the 27 bitfield
members as byte images -- compiled with clang for `aarch64-linux-android26`
and `x86_64-linux-gnu`: **0 differences**. The control, the same probe for
`i686-linux-gnu`, gives 7,004. The video codec headers' `StdVideo*` structs
are not probed.

**On the GPU**, through the stubs and not the engine
(`guest_vk::tests::guest_vulkan_reaches_the_host_gpu`, ignored by default as
it needs a driver): `vkCreateInstance` with an arm64 debug-utils callback in
its chain returned 0 and the loader's messages reached the guest callback 138
times; `vkEnumeratePhysicalDevices` listed `NVIDIA GeForce RTX 4070 Ti`
(vendor 0x10de, discrete) and llvmpipe; `vkCreateDevice` on the 4070 Ti
returned 0.

**What the engine does with it.** It loads Vulkan now, enumerates the
instance version, layers and extensions, logs `Vulkan: Using extension:
VK_KHR_surface / VK_KHR_get_physical_device_properties2 /
VK_KHR_android_surface`, and then asks OpenXR, before `vkCreateInstance`:

    [guest] openxr: xrGetInstanceProcAddr(instance 0x0, "xrInitializeLoaderKHR") -> -51
    [guest] openxr: xrEnumerateApiLayerProperties -> -51
    Error [FLog::SurfaceController] Mode 6 failed: Roblox OpenXr: checkXrResult failed OpenXr call
    Error [FLog::SurfaceController] Mode 4 failed: Error opening shader pack glsles3 (...)
    Error [FLog::SurfaceController] RenderView is NULL

qemu, running the APK's real Khronos loader, gets further into OpenXR
(initialise and layers succeed; `xrEnumerateInstanceExtensionProperties`
fails, "failed to determine active runtime file path") and ends in the same
two lines. So **this build has no flat Vulkan mode on its default flags**:
without an OpenXR runtime, Mode 6 fails and the next mode is GLES, which it
has no shaders for. `vkCreateDevice` is never reached, nothing is presented,
and `cordial_info` reads `presents=0`. No frame time can be taken.

Stability is unchanged from M4: a 120 s run exits 0 with Landing reached.

**Not done, and known:**

- The virtual OpenXR loader answers `xrInitializeLoaderKHR`'s lookup and
  `xrEnumerateApiLayerProperties` with `XR_ERROR_RUNTIME_UNAVAILABLE`. A real
  loader with no runtime succeeds at both and fails at the first call that
  needs one, as the qemu run shows. The engine ends up in the same place
  either way, but M6 should answer the loader-only calls as a loader does.
  *Done at M6* (§9.5): both go to the host loader, which answers them.
- `DebugEnableVREmulator` and a `DebugDeviceVR` class are in the binary's
  strings. Whether a FastFlag gives a flat or emulated-VR Vulkan path is
  untested, and choosing to try it is a decision, not an M5 step.
- The Vulkan stubs are the generic call builder, about 50 ns a call
  (INFERRED from M1), with no fast path for `vkCmd*`.

## 9.5 Measured at M6, part A

`cordial-run --guest-arm64 --host-libc --app-bridge`, Quest build 2.740.927,
against **Monado** `9950e2a5f575` (v25.1.0-863, BSL-1.0) built from source
into a prefix of its own (`<prefix>` below): in-process (`XRT_FEATURE_SERVICE=OFF`), the
simulated HMD the only driver, selected per run with `XR_RUNTIME_JSON` and
never through `~/.config/openxr`. The host loader is the system's
`libopenxr_loader.so.1` 1.1.47. Run with

    XR_RUNTIME_JSON=<prefix>/share/openxr/1/openxr_monado.json \
    SIMULATED_ENABLE=1 XRT_COMPOSITOR_COMPUTE=0 XRT_COMPOSITOR_DEFAULT_FRAMERATE=90 \
    CORDIAL_PRESENT_MODE=off CORDIAL_XR_FRAME_LOG=<file> tools/vr/run-quest-dynarmic.sh ...

`XRT_COMPOSITOR_COMPUTE=0` because Monado's default compute compositor picks a
queue family that cannot present to a Wayland surface on this host and
crashes, with a stock app too. `CORDIAL_PRESENT_MODE=off` so that Cordial does
not rewrite the present mode of the engine's own (unused) window swapchain.

**Control.** Khronos's `hello_xr -G Vulkan2` (OpenXR-SDK-Source
`release-1.1.47`), against the same Monado, through the system loader:
`RuntimeName=Monado(XRT) ... v25.1.0-863-g9950e2a5f`, `System Properties:
Name=Monado: Simulated HMD VendorId=42`, the compositor on the RTX 4070 Ti,
session states `IDLE` → `READY` → `SYNCHRONIZED` → `VISIBLE` → `FOCUSED`,
568 `xrEndFrame`s at 60.00 Hz over 9.43 s.

**The bridge** (`crates/cordial-runtime/src/guest_xr.rs`) replaces M2's 40
honest failures. Every command's signature is generated from `xr.xml`
(`tools/vr/gen-guest-xr.py`: 336 commands, 2 left out for by-value structs,
`xrSetInputDeviceLocationEXT` and `xrSetInputDeviceStateVector2fEXT`). The
40 imports resolve lazily to the host loader's exports;
`xrGetInstanceProcAddr` hands back a stub per (name, host function).
Hand-written:

| Command | What crosses differently |
|---|---|
| `xrInitializeLoaderKHR` | answered here, `XR_SUCCESS`: the desktop loader needs no initialisation; the Android struct (`applicationVM`, `applicationContext`) is logged and never passed on |
| `xrCreateInstance` | a host copy of the create info and its whole `next` chain, without `XR_KHR_android_create_instance` in the list or `XrInstanceCreateInfoAndroidKHR` in the chain; a debug-utils callback behind a host-to-guest entry |
| `xrCreateVulkanInstanceKHR`, `xrCreateVulkanDeviceKHR` | `pfnGetInstanceProcAddr`, the guest's `vkGetInstanceProcAddr` stub, becomes Cordial's native one; anything else is refused by name. The nested Vulkan create info and allocator go through `guest_vk`'s chain translation |
| `xrCreateSession`, `xrCreateSwapchain`, `xrEnumerateSwapchainImages`, `xrAcquire`/`ReleaseSwapchainImage`, `xrWaitFrame`, `xrEndFrame`, `xrPollEvent` | passed through unchanged; read to record the Vulkan binding, the images, `predictedDisplayPeriod`, frame times and session state |

**The OpenXR layout gate** (`guest_xr::tests::layout_gate`, over the
generated `guest_xr_probe.c` and the vendored `third_party/openxr/include`
1.1.47 headers): every struct `openxr.h` and `openxr_platform.h` declare with
`XR_USE_PLATFORM_ANDROID` and `XR_USE_GRAPHICS_API_VULKAN` -- 499 types, 3,575
sizes, alignments, type values and offsets -- **0 differences** between
`aarch64-linux-android26` and `x86_64-linux-gnu`. The control, i686, gives
2,389. `jobject` is declared as `void*` for the probe, since the headers
expect the application to supply it.

**Stops met, in order:**

| # | Where | Stop | Answer |
|---|---|---|---|
| 1 | `xrCreateVulkanInstanceKHR` | the chain copier refused the top struct's own `pfnGetInstanceProcAddr` as an unknown callback | the top struct's getter is left for the handler to replace |
| 2 | a guest worker, after `xrCreateSession` | `AAsset_openFileDescriptor` had no signature | dispatched to Cordial's (a memfd; `off_t` is 64-bit on both sides) |

**What the engine does.** It checks enumeration and asks for only what is
there: `XR_KHR_vulkan_enable2` and `XR_FB_display_refresh_rate`. It never
names `XR_FB_space_warp`, `XR_META_performance_metrics` (both absent from
Monado) or `XR_KHR_android_create_instance`, which it chains as a struct
anyway. Then, from the bridge's log:

    xrCreateInstance: application "Roblox", engine "Roblox Engine", API 1.0.26, extensions [XR_KHR_vulkan_enable2 XR_FB_display_refresh_rate]; passed on without XR_KHR_android_create_instance and its XrInstanceCreateInfoAndroidKHR link
    xrCreateInstance -> XR_SUCCESS (instance 0x7a4e79921430)
    system "Monado: Simulated HMD", vendor 0x2a, max swapchain 16384x16384, 128 layers
    xrCreateVulkanInstanceKHR: the engine's 3 Vulkan extensions [VK_KHR_surface VK_KHR_get_physical_device_properties2 VK_KHR_android_surface], pfnGetInstanceProcAddr 0x7a4f783175d0 (the guest stub) -> Cordial's native one -> XR_SUCCESS (VkResult 0, handle 0x7a4d693e7550)
    xrCreateVulkanDeviceKHR: ... -> XR_SUCCESS (VkResult 0, handle 0x7a4d694ce630); physical device 0x7a4d6946b250; Cordial's vkCreateDevice saw physical 0x7a4d6946b250, device 0x7a4d694ce630: the same host handles
    xrCreateSession -> XR_SUCCESS (session 0x7a4d694b44f0), Vulkan binding: instance 0x7a4d693e7550, physical device 0x7a4d6946b250, device 0x7a4d694ce630, queue family 0 index 0
    xrCreateSwapchain 0x7a4e7966b800: 896x1007, 2 layers, VkFormat 37, 1 samples, usage 0x21
    xrRequestDisplayRefreshRateFB(90 Hz) -> XR_SUCCESS
    xrRequestDisplayRefreshRateFB(72 Hz) -> XR_ERROR_DISPLAY_REFRESH_RATE_UNSUPPORTED_FB
    session state -> XR_SESSION_STATE_FOCUSED (after 1 XR frames)
    xrEndFrame #1024 on thread 204812: 1 layers [projection, 2 views: swapchain 0x79e551665c80 layer 0 rect 0,0 896x1007; swapchain 0x79e551665c80 layer 1 rect 0,0 896x1007]

and from the engine's FLog: `Vulkan Device: NVIDIA GeForce RTX 4070 Ti`,
`Vendor 10de Device 2782`, `API 1.4.329`, `Loaded 2985 shaders from pack
vulkan_mobile variant default`, `SurfaceController::enableVR`, `RenderView
created[1]`. No `Mode 6 failed`. It renders both eyes with one two-layer
swapchain (multiview, INFERRED from the layer count). It also creates a
1280x721 window swapchain and never presents to it: `presents=0` throughout.
**No `ovr_*` function was called** in any run, so the Meta platform stubs
were never reached and remain stops.

**Capture.** An XR engine never presents, so `cordial_screenshot` reads the
left eye instead: at `xrEndFrame` the bridge notes the projection layer's
first view (swapchain, array layer, rect), and at the next
`xrReleaseSwapchainImage` of that swapchain -- the moment the image is
complete and in `COLOR_ATTACHMENT_OPTIMAL`, as a present's image is in
`PRESENT_SRC` -- copies it on the session's queue. `cordial_info` reports
`xr_frames=` (`xrEndFrame` calls), the session state and the last
`predictedDisplayPeriod`. The left eye 20 s in
(`runs/m6-left-eye.png`, 896x1007): the landing screen -- ROBLOX logo,
Create Account, Sign In, the game-tile collage -- on a panel seen in
perspective, floating in a uniform light grey. **The environment around the
panel is missing**; a person watching the Monado window saw the same, white.
This paragraph used to infer that the cause was the Quest APK's ETC1 sky
cubemaps, which no desktop GPU here can sample. **That was wrong.** The app
shell's own place, `ExtraContent/places/Mobile.rbxl`, sets all six `Skybox*`
properties of its `Sky` to `rbxasset://textures/sky/white.png`, a 16x16 white
PNG the engine loads through `AAssetManager` beside the ETC1 files. Decoding
the ETC1 files to RGBA8 at the asset layer changed nothing; the white void
was `Mobile.rbxl` as authored, and `isVrDevice` (below) is what loads the
Quest's own `Maquettes.rbxl`. That asset-layer decoder was removed again once
upstream's Vulkan ETC2/EAC emulation (`android/vulkan_etc.rs`) arrived: under
it the guest engine reports `ETC1 1 ETC2 1` on an RTX 4070 Ti whose driver
says `textureCompressionETC2 = false`, and a run without the decoder showed
the Maquettes night sky, ridge and floor as before. Whether every ETC texture
in an experience (the default sky, water normals) decodes correctly through
that path has not been looked at.

**Pass**, 3 of 3 at 120 s with Monado at 90 Hz, each `exit=0`, no fault, 0
stops, `FOCUSED` held throughout; plus one input-driven run at 90 Hz and one at
144 Hz, also clean.

**Frame time.** `CORDIAL_XR_FRAME_LOG` timestamps each `xrEndFrame` on the
calling thread; intervals over 60 s from 30 s after the first frame (Landing
at about 7 s). Per-thread CPU from `/proc/<pid>/task/*/stat` over the same
60 s window. The XR thread is the one the bridge logs calling `xrEndFrame`
(an engine thread named `Main`); the process total includes Monado's
in-process compositor. Load average 4.2--6.6 on 32 hardware threads (a
desktop session with Roblox Studio under Wine), nothing pinned. Monado's
simulated HMD has no refresh rate of its own; the compositor's
`XRT_COMPOSITOR_DEFAULT_FRAMERATE` (60 unless set) paces `xrWaitFrame`, and
`predictedDisplayPeriod` was that period exactly in every frame.

| 90 Hz, no input, 3 runs | min | median | max |
|---|---|---|---|
| `xrEndFrame`/s | 89.95 | 89.98 | 90.00 |
| interval p50 | 11.12 ms | 11.12 ms | 11.12 ms |
| interval p95 | 11.39 ms | 11.42 ms | 11.48 ms |
| interval p99 | 11.59 ms | 11.73 ms | 11.91 ms |
| interval max | 13.64 ms | 20.33 ms | 22.36 ms |
| intervals > 1.5 periods (a missed vsync) | 0 | 1 | 3 |
| intervals > 2 periods | 0 | 0 | 1 |
| intervals > 1 period (jitter included) | 2,762 | 2,814 | 2,821 of 5,400 |
| XR thread CPU, % of a core | 11.2 | 11.2 | 12.2 |
| busiest thread (`RBX Worker B`) | 22.1 | 22.3 | 25.5 |
| process CPU, % of a core | 48.9 | 49.8 | 56.1 |

Input made no difference: driving `move` at 29.9/s for the whole window gave
90.00/s, p99 11.75 ms, 0 missed, the XR thread at 12.4 %. XR mode does not
idle-throttle the way the flat client does. At 144 Hz, one run: 143.93/s,
p50 6.95, p95 7.22, p99 7.42, max 20.24 ms, 3 intervals over 1.5 periods, XR
thread 17.6 %, `RBX Worker B` 33.7 %, process 72.4 %. At 60 Hz, one earlier
run: 60.00/s, p99 17.59 ms, 0 missed.

What that says, INFERRED: the XR thread costs about 1.2--1.4 ms of CPU a
frame (11.2 % of a core at 90/s, 17.6 % at 144/s), and the busiest worker
about 2.3--2.8 ms if its work is per frame. On **this scene** -- the
signed-out landing panel, no place loaded, no avatars -- 72 and 90 Hz have a
wide margin and 144 Hz holds. A real place is heavier by an unknown factor,
and that is M7's measurement. GPU time was not measured. For a Quest 3 over
WiVRn the engine still runs on this PC; the headset's own CPU is not in the
path, so these CPU figures carry over and WiVRn's encode and network latency
add to them.

**Not done, and known:**

- The environment around the landing panel (above).
- The engine loads `shaders_vulkan_mobile.pack`, not
  `shaders_vulkan_mobile_vr.pack`; what decides between them is not known.
- ~~The capture's copy is submitted on the session's queue with no lock
  against another engine thread.~~ Not a gap: OpenXR makes the application
  externally synchronise the session's queue across `xrBeginFrame`,
  `xrEndFrame`, `xrAcquireSwapchainImage` and `xrReleaseSwapchainImage`
  (OpenXR 1.1 §12.25.3), so no other engine thread may use it while this one
  is inside the release. Corrected 2026-09-29 (§9.6).
- ~~Meta platform (`ovr_*`) stubs still stop when called; nothing called them.~~
  Nothing called them because nothing called `initMaquettesSDK`, which the
  Quest app runs on its own executor right after `StartAppWithParams`.
  `--app-bridge` now does, and `guest_ovr` answers all 24 names as a host
  with no Meta platform: every request fails through the message queue.
  The engine logs `Quest Platform SDK initialization failed: Meta platform
  services are not available on this host (Cordial)` and the entitlement and
  purchases failures. 2026-09-30.
- The debug-utils callback path is built and unit-tested but the engine never
  requested `XR_EXT_debug_utils`.

## 9.6 Raw `svc #0` in game

Quest build 2.740.927 joined by deep link against Monado's simulated HMD
(`docs/vr/play-button.md`). About 67 s after `launchUGCGame`, in every place
tried, one engine thread executes `svc #0` itself rather than calling the
`syscall()` import. With no answer that stopped the process
(`Fault::Syscall` at libroblox+0x32b8978, `exit=134`), at 67 s on Monado and
at 193 s in a user's WiVRn run, 62 s after that run's join.

`Runtime::set_syscall_handler` now answers `svc #0`, and `guest_sys.rs` has
one translator both routes share (`arm64_syscall`): arch-neutral numbers
renumbered to x86-64 and made with the `syscall` instruction, so errno is
untouched as it is by a real raw call; `openat` through the same path layer
and `O_*` renumbering the `open` imports use; `mmap`/`munmap`/`mprotect`,
`rt_sigaction` (the kernel's `{handler, flags, restorer, mask}` converted to
bionic's), `rt_sigprocmask` and `sigaltstack` through the virtual answers the
imports already get; `clone`, `clone3` and `set_tid_address` refused with
ENOSYS by name; `exit`, `exit_group` and `rt_sigreturn` stopping, since
none can return. Unknown numbers get `-ENOSYS`, named once. The first call of
each number is logged with its arguments; `CORDIAL_GUEST_SVC_LOG=1` logs
every one.

What the engine does, `CORDIAL_GUEST_SVC_LOG=1`, the same in every run:

    svc #0 at ...8978: nr 56 (openat) [18, ..., 0, 0, 0, 0] path "/proc/self/maps" -> 214
    svc #0 at ...0aa0: nr 44 (fstatfs) [d6, ...] -> 0
    svc #0 at ...767c: nr 63 (read) [d6, ..., 1000, 0, 0, 0] -> 4069     (x 36)
    svc #0 at ...b448: nr 62 (lseek) [d6, 0, 0, 0, 0, 0] -> 0
    svc #0 at ...e714: nr 63 (read) ...                                  (x 37)
    svc #0 at ...e760: nr 57 (close) [d6, ...] -> 0

77 calls, on one thread, once per run: it reads `/proc/self/maps` twice
over. What it reads is the real map of this process -- an x86-64 host with
guest objects in it -- which is the honest answer; no map is invented.

**The answer matters to the session, not only to the process.** The first
build answered `openat` with ENOSYS; the next translated `openat` but not
`fstatfs`. Neither stopped, and in both the server disconnected the client
at the same moment, 67 s into the run and into the FLog alike, reason 304: "Roblox has detected missing or
corrupted files. Please uninstall and reinstall Roblox from an official app
store." With `statfs`/`fstatfs` mapped --
the two kernels' LP64 `struct statfs` are the same generic layout -- the read
completes and no run below was disconnected. That an incomplete answer here
is what the server acts on is **INFERRED** from that timing, one run each;
what the engine checks in the map is not looked at.

**Pass**, 300 s runs joined by deep link, Monado at 90 Hz, `exit=0`, no stop,
no disconnect; the raw sequence ran in every one (`svc #0=` in the thunk
report):

| Run | Place | `gameLoadedCallback` | raw `svc #0` calls |
|---|---|---|---|
| p1-1, -2, -3 | 11256291667 | 14.0, 15.0, 18.1 s | 77, 78, 79 |
| p2-1, -2, -3 | 1818 | 8.5, 8.5, 8.9 s | 75, 69, 71 |

A seventh (1818) was cut short at 193 s by an interrupted session, not by the
client, and is not counted.

**Control**, the previous binary, same session: on 1818 for 100 s it stopped
at 66.8 s on `Fault::Syscall` at libroblox+0x32b8978, `exit=134`, as it had
at 67 s and 193 s in the two runs before this change. Once on 11256291667
for 300 s it did **not** stop (`exit=0`); that binary has no counter, so
whether the engine made the call in that run is not known. So the check does
not run in every session, and the control is 1 stop in 2 here.

The engine's saved graphics setting in this profile reads
`GraphicsQualityLevel 0`, `SavedQualityLevel 0` (automatic); the level the
engine reported in its telemetry was 11.9 in the user's run and 1.3--7.6 in
these. The raw-syscall sequence did not depend on it.

## 9.7 Swapchain colour encoding

The engine creates its OpenXR swapchain as `VK_FORMAT_R8G8B8A8_UNORM` (37)
without calling `xrEnumerateSwapchainFormats`. OpenXR reads a UNORM swapchain
as linear light and encodes it for the display, but the engine's bytes are
already sRGB-encoded: blitted straight into Cordial's window the left eye
shows correct colour, while Monado's composited preview of the same frames
looked washed out -- lifted blacks, lower contrast and saturation, which is
what encoding twice does.

`guest_xr::create_swapchain` now asks the runtime for the sRGB twin (43, which
Monado lists third of 13) with `XR_SWAPCHAIN_USAGE_MUTABLE_FORMAT_BIT`, so the
engine's UNORM views stay valid and write the same bytes, and the runtime now
decodes them as what they are. `CORDIAL_XR_SWAPCHAIN_AS_ASKED=1` is the
control. The image-use log shows the engine touching the swapchain images only
through `vkCreateImageView` in format 37; no blit, resolve, clear or copy into
one was logged in a 60 s run on Monado (2,048+ frames), so no path encodes
the bytes on the way in.

Not yet observed: the corrected colour itself, in Monado's preview or in the
headset. A B8G8R8A8_UNORM request would take the same route (44 to 50); none
has been seen.

## 9.8 Measured at M7, first pass

Quest build 2.740.927 joined by deep link to place 11256291667 on Monado's
simulated HMD at 90 Hz (§9.5's environment), i9-13900KS, a desktop session
at load average 4--7 on 32 hardware threads, gamemode's performance
governor on. The harness and analyser were local scripts and are not in the
tree; the method is described below. Runs of 130 s.

**Phases.** `gameLoadedCallback` is not the end of loading: the frame rate
keeps climbing for 9--23 s after it. So each run is split where the frame
log settles. r(t) is `xrEndFrame`s in [t-2 s, t) over 2, on a 0.5 s grid from
`gameLoadedCallback` + 2 s; the stable point is the first t at which every
r(u), u in [t, t+5 s], is within ±10 % of their median. *Loading* is
`gameLoadedCallback` to there, *in-game* the next 60 s, and in-game is the
headline. Missed means an interval over 1.5 display periods. Per-thread CPU
is `/proc/<pid>/task/*/stat` sampled each second, over the in-game window.
Every change below was run interleaved against its control, 2 runs each.

**Profile.** `perf record -g -F 999` for 20 s in game. dynarmic writes a perf
map (`backend/x64/perf_map.cpp`) when `PERF_BUILDID_DIR` is set in the
client's environment -- `/tmp` gives the `/tmp/perf-<pid>.map` perf reads --
naming each block `a64_<guest pc>_fpcr<fpcr>` and the dispatcher and
terminal handlers by name. Its `PerfMapClear` truncates the file on any
Jit's full cache clear, so blocks translated before one are unnamed; at
264b1b6 that was about half of the JIT samples. Share of each thread's
samples:

| | `Main` (XR submit) at 264b1b6 | `RBX Worker C` at 264b1b6 | `Main` now | `RBX Worker C` now |
|---|---|---|---|---|
| stub dispatch (`on_svc`, `Runtime::entry`, the call builder) | 57.5 % | 39.7 % | 13.1 % | 5.7 % |
| guest code (translated blocks) | 23.7 % | 44.4 % | 63.6 % | 76.7 % |
| dynarmic dispatcher, RSB and fast-dispatch handlers | (unnamed) | (unnamed) | 6.9 % | 10.7 % |
| dynarmic translator | 0.7 % | 5.6 % | < 0.5 % | < 0.5 % |
| Rust allocator and std in the dispatch path | 5.0 % | 4.4 % | < 0.5 % | < 0.5 % |
| libc, kernel, GPU driver | 11.3 % | 5.4 % | 14.1 % | 5.7 % |

At 264b1b6 the boundary was the cost, and not its work but its bookkeeping:
`Runtime::entry` took an `RwLock` read and cloned an `Arc` on every call, and
every call bumped one shared counter for the stub and one for the runtime,
at 6--10 M calls a second from a dozen threads. 5 M of those were
`pthread_getspecific`, the engine's `thread_local`s through emutls.

**Changes**, each against its control, in-game phase:

| Change | Control | XR frames/s | p50 / p95 / p99 ms | process CPU | busiest worker |
|---|---|---|---|---|---|
| lock-free stub table, per-thread counts, no allocation per call (`ce17bb0`) | 264b1b6's binary | 45.4, 46.4 against 34.8, 41.4 | 22.1 / 35.1 / 38.1 and 22.0 / 34.1 / 37.4 against 24.2 / 43.6 / 53.0 and 22.5 / 33.6 / 35.9 | 208, 198 % against 283, 269 % | 49 % against 57--82 % |
| `pthread_getspecific`/`setspecific` as guest code over bionic's key map (`f5d1b2e`) | `CORDIAL_GUEST_TLS_KEYS=host` | 45.8, 46.9 against 44.9, 41.2 | 22.0 / 34.6 / 37.6 and 22.1 / 25.1 / 34.6 against 22.2 / 28.5 / 36.5 and 22.4 / 33.9 / 42.9 | 208, 207 % against 230, 243 % | 53--56 % against 59--67 % |
| `Unsafe_InaccurateNaN`, `ReducedErrorFP`, `UnfuseFMA` together (`CORDIAL_GUEST_UNSAFE_FP=nan,recip,fma`) | unset | 45.1, 48.3 against 44.2, 46.0 | no difference | 203, 198 % against 197, 217 % | no difference |
| every thread pinned to the P-cores (`taskset -a -c 0-15`, all 88 threads checked) | unpinned | 45.7, 47.2 against 47.6, 48.2 | no difference | 222, 202 % against 194, 199 % | no difference |

The first is a pure cost removal. The second moves the stub rate from
8.2--10.0 M/s to 3.2--4.0 M/s; its frame-rate gain is inside the noise of
two runs, its CPU gain is not. Its correctness argument is that it is
bionic's algorithm (`libc/bionic/pthread_key.cpp`), data layout and limits,
and `crates/cordial-guest/tests/m7.rs` checks each case -- deleted and
re-created keys, invalid keys, other threads, 128 then EAGAIN, destructors
only for non-null values and cleared first -- through the translator. The
FP flags and pinning bought nothing measurable and stay off; the flags
remain a toggle, since each trades ARM-exact results for speed and none
earned that here.

Also fixed, found by a profiling run that stopped: this dynarmic raises
`WaitForEvent`, `Yield`, `SendEvent` and `SendEventLocal` as exceptions
whatever `hook_hint_instructions` says (`a64_interface.cpp` builds the
translator's options without it, and its default there is true), and Cordial
stopped the process on the first. One run in about a dozen hit a WFE in a
guest thread's key destructor. The hints now complete (`ff8142e`).

**Where that leaves it.** The six control-arm runs with both changes and
no toggle (the FP, pinning and keys-on arms): 44.2, 46.0, 47.6, 48.2, 45.8
and 46.9 XR frames/s at 90 Hz, against 34.8 and 41.4 for 264b1b6 in the
same session (and 41.2 in the earlier report); p50 21.9--22.1 ms, p95
25.1--36.6 ms, p99 34.6--39.2 ms, missed 54--394 of 2,650--2,900 intervals.
Loading, `gameLoadedCallback` to stable, went from 10.5--16.5 s at
9.5--9.8/s to 9.0--13.0 s at 16--24/s. The process uses
about 2.0 cores, down from 2.7--2.8.

p50 sits at two display periods (22.2 ms) in every run, before and after.
`Main` spends 46--53 % of a core at about 46 frames/s, 10--11 ms of CPU a
frame, and `RBX Worker C` about the same: each is at the 11.1 ms budget on
its own, so the engine makes every other vsync. The next frame-rate step
needs both below about 9 ms, which is roughly another 20 % off guest code
on those two threads, not off the boundary. INFERRED from the CPU per frame;
no off-CPU trace was taken, so how much of `Main`'s frame is waiting on
Worker C is not measured.

**Next, largest first** (estimates in agent-run time):

- **dynarmic's own dispatch, 7--11 %** on the two frame threads: returns
  that miss the return-stack buffer and indirect calls through the fast
  dispatch table. Checking the RSB size and whether `BLR`/`RET` pairs
  through the stub page break its prediction is a few hours; a larger RSB
  or better indirect-branch caching inside dynarmic is a day.
- **The remaining boundary, 6--13 %:** `memcpy`, `memmove`, `memcmp`,
  `strlen`, `memset` at 0.2--0.7 M/s each and `clock_gettime` at 0.37 M/s.
  Guest-side arm64 routines (ARM optimized-routines, MIT/Apache-2.0) in the
  stub page, as `keys.rs` does, with no SVC: half a day for the string
  functions. `clock_gettime` wants a vDSO-like read of a host-updated time
  page and is only correct for the monotonic and realtime clocks: half a
  day.
- **Guest code itself, 64--77 %.** What dynarmic emits for the hot blocks
  has not been read. `fpcr00000000` on `Main` against `fpcr01000000` (FZ)
  on the workers means `Main`'s FP runs with denormals honoured, which on
  x86 is slower if they occur; unmeasured. Per-block reading is a day;
  anything it finds, more.
- **LSE in the frontend** (design §4) was not reached: with
  `IgnoreGlobalMonitor` no exclusive-monitor cost appears in the profile,
  so it is not a frame-rate item on this evidence. Two to three days.
- **Code-cache clears.** The perf map truncations show at least one Jit
  hitting its 64 MiB cache in game, and 0.6 M guest instructions were
  translated in one 10 s window at 264b1b6 against about 50 k in the
  others (INFERRED to be a clear's retranslation). `CORDIAL_GUEST_CODE_CACHE_MIB`
  already exists; a run with 256 would say whether that is a hitch source.
  An hour.

## 9.9 Measured at M7, second pass

Same harness, place and host as §9.8, Monado's simulated HMD at 72 Hz
(`XRT_COMPOSITOR_DEFAULT_FRAMERATE=72`) first and then 90 Hz, runs of 100 s,
interleaved one-for-one against the HEAD binary (`184b6ec`, ahead-of-time
code removed) built in its own copy. The machine was shared with another
agent's client; every run waited until no `cordial-run` had existed for 10 s.

**The starting point had moved.** The control was not §9.8's: at 72 Hz it ran
51.7--65.3 XR frames/s in game with p50 at one display period (14.0 ms), not
the 42.5 frames/s at two periods recorded before, and 57.6 at 90 Hz. Nothing
in this pass explains that; the same binary measured the same way is the
control below.

**Configuration audit.** Every `UserConfig` field `native/shim.cpp` sets was
already where speed wants it: cycle counting off (no per-block tick, no
dispatcher return), `wall_clock_cntpct` on, all safe optimisations on
(BlockLinking, ReturnStackBuffer, FastDispatch, GetSetElimination, ConstProp,
MiscIROpt), fastmem at base 0 with `check_halt_on_memory_access` off. The code
cache is not a factor in game: about 10 k guest instructions translated a
second and the translator under 1.5 % of `Main`. `mrs cntvct_el0`, which
dynarmic hands back as an interpreter fallback, ran about 10 times a second
(the new `emulated=` count in the guest report), so it was left alone.

**Profile at the control** (`perf record -g -F 999`, 20 s in game, 72 Hz).
`RBX Worker C` was the busiest thread at 60--68 % of a core: 81.9 % translated
guest code, 9.9 % dynarmic's two terminal handlers (return-stack pop 4.9,
fast dispatch 5.0), 4.7 % the stub boundary. `Main` was 70.4, 6.3 and 7.4 %.
The engine made 1.85 M stub calls a second, 0.9 M of them `memcpy`, `memcmp`,
`memmove` and `memset`, and 0.26 M `clock_gettime`. Worker C's CPU stayed at
55--63 % whatever the frame rate, so it is not proportional to XR frames, and
CPU per frame is not a usable instrument for it (INFERRED that its work is
fixed-rate).

**Changes:**

- *patches/0008:* the return-stack and fast-dispatch hit paths emitted at each
  site instead of in two shared handlers, so each guest return and indirect
  branch gets its own host indirect jump to predict, and the return-stack ring
  from 8 entries to 32. `perf stat` on Worker C, one 20 s window each: 3.11
  host branch misses per 1,000 cycles at the control, 2.89 with 0008. Frame
  rate: 60.8 and 52.8 against 51.7 and 58.5, inside the noise.
- *Guest `memcpy`, `memmove`, `memset`, `memcmp` up to 128 bytes*
  (`cordial-guest` `string.rs`), as `keys.rs` did for `pthread_getspecific`:
  arm64 in the stub page, branching past 128 bytes to the host function's own
  stub. Every byte is loaded before any is stored, so the one routine is
  overlap-safe and serves both copies. `tests/m7.rs` checks every size 0--160
  at every alignment against glibc through the translator, overlap both ways,
  and that no SVC is taken at or under 128 bytes; planting a wrong
  condition code in `memcmp` fails it. Control `CORDIAL_GUEST_STRING=host`.
- *Guest `clock_gettime(CLOCK_MONOTONIC)`* (`clock.rs`) from `mrs cntpct_el0`,
  which dynarmic emits as a direct call to `GetCNTPCT` without leaving
  translated code, scaled back from 600 MHz. It is never ahead of the host
  clock and at most 2 ns behind; the test checks 10,000 reads against host
  reads either side. Other clocks go to the stub. Control
  `CORDIAL_GUEST_CLOCK=host`.

With all three, stub calls in game fell from 1.85 M/s to 1.12 M/s; `memcpy`
still makes 0.2 M/s above 128 bytes. Worker C's stub boundary went from 4.7 %
to 2.1 % of its samples and translated code to 94.4 %; `Main`'s boundary from
7.4 to 5.7 %, most of what remains being Vulkan command recording.

| 72 Hz, in game | XR frames/s | p50 / p95 ms | missed | Worker C / Main CPU |
|---|---|---|---|---|
| all three | 68.1, 65.4, 63.4\*, 61.7 | 14.0 / 19.3, 26.5, 27.6, 28.0 | 196--559 of 3,700--4,085 | 55--60 / 42--45 % |
| control | 62.2, 65.3, 60.9\*, 61.8 | 14.0 / 27.9, 26.5, 28.1, 27.7 | 277--631 of 3,650--3,920 | 59--62 / 40--43 % |

\* The camera was moved by hand during one of these two runs.

At 90 Hz, one complete pair: 58.3 and 61.3 against 57.6 frames/s, p95 23.6
and 23.3 against 23.6 ms; p50 sat at 19.8 and 12.1 against 19.5 ms. The
second control run was stopped before it finished and is not counted.

**Reading.** At 72 Hz the means are 64.7 against 62.6 frames/s, and the
change arm had the two best runs, but the ranges overlap and four pairs do not
separate a 3 % effect from this harness's run-to-run spread. The boundary
reductions are measured; the frame-rate gain is not established. 72 Hz is not
yet at full rate: 5--15 % of intervals still miss, at two periods. Worker C's
time is now almost all translated code and spread thin -- 4,887 blocks in a
20 s profile, the top 100 holding 43 % -- so the next step is the code dynarmic
emits for ordinary C++ (calls, returns and the guest-state traffic around
small blocks), not anything at the boundary.

## 9.10 Code the engine generates

Leaving a game stopped the client: over WiVRn, 0.8 s after
`onGameLeaveBegin`, a guest thread's `mprotect` of a range the guest had made
executable hit the M3 stop. `CORDIAL_GUEST_TRACE_EXEC=1` now logs every call
that makes a range executable or touches one. Three runs on Monado's
simulated HMD, joining place 1818 by deep link and leaving after 30, 30 and
60 s in game through the new `leavegame` devctl verb, all showed the same
calls and nothing else:

| When | Thread | Call | Site (low bits) |
|---|---|---|---|
| app start | an RBX worker | `mprotect` 1 page `R-X` | `…123ba0` |
| join | an RBX worker | `mprotect` 4 pages `R-X`, adjacent to the first | `…123ba0` |
| join | the game's `Main` | `mprotect` 1 page `R-X` | `…123ba0` |
| leave | the game's `Main` | `mprotect` of that last page to `RW-` | `…db9ac8` |

The WiVRn stop's `lr` was `…ddb9ac8`, the same site. Every page was written
while `RW-` and made `R-X` once; nothing asked for `RWX`, nothing mapped a
file executable, and no `munmap`, `madvise` or `MAP_FIXED` touched an
executable range. The guest's 206 `IC IVAU` all came in the first ten
seconds, alongside those `mprotect`s, and none in game. That this is Luau's
native code generator is INFERRED.

So stale translation was not reachable before this: a page made `R-X` is
read-only to the host, a write to it faults, and any change of protection
stopped. With the stop simply removed it would have been, silently: the
tests below, with the invalidation turned off, ran the old code in every
case.

**What is built** (`crates/cordial-guest/src/code.rs`). Any call that unmaps,
maps over, discards or re-protects a range the guest made executable, or makes
a range executable, drops every Jit's translations of it before returning to
the guest; `R-X` to `R-X` is the one change skipped. One lock covers each
call, so another thread's `mmap` cannot be given an unmapped address and run
new code there before the old translations are gone. dynarmic's
`InvalidateCacheRange` is safe from any thread -- it takes the Jit's own
mutex, records the range and sets its halt flag -- and is performed on the
Jit's own thread when `Run` starts or ends. A Jit that is stopped or inside a
callback checks the flag before it runs another block (SVC, exception, cache
and fallback all end their block with `CheckHalt` or a return to the
dispatcher). A Jit running translated code may not for a while, because
returns and indirect branches go to cached code without checking, so the
caller waits for each such Jit to leave translated code or to re-enter `Run`
having read this request's number, published after its ranges were recorded
and followed by a second halt request. `membarrier` orders the flag against
the state word without a fence on the hot path; registering it cost 21 ms
once. In the client the wait took 0.1--9 µs, for at most 2 running Jits of
39--81, measured with an earlier revision that counted exits from `Run`
instead; that revision could take an exit for a previous request as this
one's, and the client has not been run with the final one.

**Limit.** A Jit that loops through returns and indirect branches alone,
never a direct branch or a stub, never reaches a halt check. After 5 s the
call stops by name rather than return with stale code reachable. No such
loop has been seen.

**Tests** (`tests/code.rs`, through the translator): rewrite in place, unmap
and map again at the same address, and both with a second thread that
already ran the old code and is waiting either spinning in guest code or
inside a host call. 21 runs of the six passed. Controls, the same tests with
the change disabled: the old stop fails all six; no invalidation fails all
six with the old code run (`0x1000100` for `0x1000200`); invalidating only
the calling thread's Jits fails the four two-thread tests; not waiting passes
all six, which these tests cannot tell from waiting.

**In the client**, the three runs above: the leave `mprotect` returned 0, the
engine logged `leaveUGCGame`, disconnected with reason 285
(`DisconnectClientInitiated`) and set its stage to `Native`, and the process
ran to its `--run` limit and exited 0. **The menu did not come back:** XR
frames stopped at the leave and stayed stopped for the 50--65 s left, and
`RBX Worker B` ran at 99 % of a core, making about 1.5 M `strncmp` calls a
second while the count of translated instructions stayed flat -- guest work,
not the translator retranslating anything. Whether that is the leave being
driven from the Java side (`nativeAppBridgeV2LeaveGame`) rather than the
in-game Leave button, which over WiVRn also produced `gameDidLeave` and a
`gameLoadedCallback` for place 0 before the stop, is not established.
*Since fixed*: the leave left the engine at stage `Native` with nothing
running, and the Java side's `nativeAppBridgeV2StartAppWithParams` brings the
Lua app back (docs/vr/play-button.md, "Leaving").

## 9.11 Audio

There was no sound in VR at all. Every VR flog with a join had
`FMOD API error, FMOD_RESULT:51 ... functionname:System::init`
(`FMOD_ERR_OUTPUT_INIT`), and every sound after it failed with "Unable to
start loading sound in to fmod".

**The route the Quest build's FMOD takes is AAudio, by `dlopen`.** It imports
no `AAudio*` and no `sl*` (`libOpenSLES.so` is in `DT_NEEDED` with nothing
bound from it). With `CORDIAL_TRACE_DLSYM=1`, which the guest's `dlopen` and
`dlsym` thunks now honour with the native path's line, a signed-out run
joining place 1818 by deep link showed, in order: `org.fmod.FMOD.checkInit`,
`supportsAAudio` (both seen with gdb `dprintf`), then
`dlopen(libaaudio.so, 1) -> 0x0` twice, then `System::init` failing with 51.
No `slCreateEngine`, no `AudioDevice.init`: once `supportsAAudio()` says yes
FMOD does not fall back, as `native/aaudio.cpp` had already measured on the
phone build. The guest's linker only knows guest libraries (§2), and the
x86-64 `libaaudio.so` the phone build uses is a host one, so the guest never
saw it.

**What is built** (`crates/cordial-runtime/src/guest_audio.rs`). A guest
`libaaudio.so` whose names are whatever `aaudio.cpp` exports, each a stub
into that same implementation through the generic call builder; the
signatures are scalars, handles and out-pointers. The two callback setters
swap the guest's data and error callbacks for host entries (§3.3), one per
distinct callback, so PipeWire's realtime thread calls arm64 code on a Jit
of its own (§4). A fresh signed-out run then showed `dlopen(libaaudio.so)`
succeeding and 24 `dlsym`s, all non-null -- the 25 names of
`docs/analysis/aaudio-contract.md` less `setInputPreset`.
`AAudioStream_waitForStateChange` is named in this build's strings beside a
`FmodAAudioWaitForStateChange` flag and was not looked up; `aaudio.cpp` does
not implement it, so the guest library has no such name and its `dlsym`
would be null. FMOD then opened the same four streams the phone build does
(two output probes, the real output with both callbacks, an input probe
closed unstarted) and `System::init` reported no error: the flog's first
audio line became `InputDevice 0: Android audio output ... 48000/1/4`.

**Measured, signed out, Monado simulated HMD, place 1818 by deep link**, two
runs of 90 and 100 s with `CORDIAL_TRACE_AUDIO=1`. PipeWire negotiated F32LE,
2 channels, 48 kHz, a 256-frame burst. `pactl list sink-inputs` and
`pw-dump` showed `cordial-aaudio-2` running. The guest callback ran at the
burst rate, 187.5 a second (18,238 calls in the 100 s run) with no
silence-filled cycle. Each callback took 0.02--0.12 ms of a 5.33 ms burst,
except the first, 17.3 ms, which is that thread's Jit being made and the
callback translated. `pw-top`'s ERR for the node read 5 at 10 s and stayed
5 every 10 s to the end, and 3 in an earlier run: the glitch is at stream
start and does not grow.

**Not measured: a non-silent frame from the engine.** Signed out, nothing
plays after `System::init`. FMOD is first initialised by the game's data
model at the join, about a second after the landing's one sound has already
failed, and the join itself then waits at a loading screen without a
session. Every frame those runs handed PipeWire was zero. That the path
carries non-zero samples is shown with an arm64 callback instead:
`guest_audio_reaches_the_host_backend`, ignored by default because it opens
a playback stream, ran 560 callbacks in 3 s through the guest's stubs and
counted 97,024 of 97,024 frames non-silent at -120 dBFS on the backend's own
meter. Hearing a game in VR is still to be checked signed in.

Not built, and not reached: `AAudioStream_waitForStateChange`; FMOD's
`dlsym(libandroid.so, AAsset_read)` for its asset file system, which is null
on the phone build too; the microphone, whose input stream FMOD only opened
and closed.

## Top risks

1. **CPU throughput** (§6). This could end the project at M3/M7.
2. **Thunk-surface correctness at scale:** about 1,300 entry points, ABI divergences
   in libc, `va_list`, and signals. Mitigate with generators plus the layout-diff
   gate.
3. **Honest Meta-platform failure blocking engine start-up** (entitlement or
   integrity). It cannot be worked around, by the rules.
