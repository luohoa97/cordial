# Why `nativeInitializeNativeFlags` reports `onFlagsFailed` and segfaults

**Status:** investigation only; nothing modified except this file, per instructions.
Disassembly against the same `libroblox.so` (Roblox for Android 2.732.1043) used by
`findings.md`, [`app-bridge.md`](app-bridge.md) and [`render-gate.md`](render-gate.md),
using `objdump`/`nm`/`readelf` plus two small scratch scripts (a direct-`call`-site
scanner and a rip-relative-`lea`-site scanner, same technique `render-gate.md` §2
used for `eglCreateWindowSurface`). Cross-checked against `native/init_params.cpp`
(read, not modified) and reproduced once against the built `cordial-load` binary
without rebuilding it, since `native/*.cpp` is being edited concurrently.

**Bottom line up front:** `nativeInitializeNativeFlags` itself is not the function
that calls `gameActivity_onFlagsFailed`. Its own disassembled body — verified in
full — constructs the `NativeFlagsInitResult` exactly as documented, iterates the
input `String[]`, and returns normally, exception-safe, no matter what is in that
array. The **only** call to `gameActivity_onFlagsFailed` anywhere in the 116 MB
binary lives in a small, separate helper reached through a completely different,
**indirectly-invoked** function that has zero direct callers in `.text` — the same
shape `render-gate.md` §2 already found for `eglCreateWindowSurface`'s trigger, and
for the same reason: something calls it through a function pointer this pass cannot
resolve statically. What *is* pinned down precisely is the gating condition on that
call, and a live, currently-wrong piece of Cordial's own code that feeds
`nativeInitializeNativeFlags` a single "flag name" that is actually Roblox's entire
22,318-flag settings document.

---

## 1. `nativeInitializeNativeFlags` builds its result exactly as documented — verified

`Java_com_roblox_client_flags_FlagJniInterface_nativeInitializeNativeFlags` sits at
file offset `0x215a0b3` in `.dynsym`, 1625 bytes up to the next export
(`nativeRegisterFFlag` at `0x215a70c`). The dex confirms the descriptor:

```
com/roblox/client/flags/FlagJniInterface.nativeInitializeNativeFlags([Ljava/lang/String;)Lcom/roblox/client/flags/NativeFlagsInitResult;
```

Every `call *0xNN(%rax)` in this function was decoded against the exact
`JNINativeInterface` layout Cordial's own libjnivm ships
(`third_party/libjnivm/include/jni.h`), counted field-by-field rather than assumed
from memory — the header has an extra `ToReflectedField` slot that a memorized
table would miss, and getting one offset wrong cascades into every later one:

| Offset | Index | Function | Confirms |
|---|---|---|---|
| `0x30` | 6 | `FindClass` | class `com/roblox/client/flags/NativeFlagsInitResult` (read from `.rodata` at `0x34b965`) |
| `0x108` | 33 | `GetMethodID` | `"<init>"` `"(I)V"` (at `0x445690`/`0x46267b`), then `"addBoolean"` `"(Ljava/lang/String;ZZ)V"` (at `0x4628cc`/`0x507da6`) |
| `0x558` | 171 | `GetArrayLength` | called on the JNI argument itself — **it is a `jarray`, not a `jstring`** |
| `0x568` | 173 | `GetObjectArrayElement` | one call per loop iteration, index `i` |
| `0x720` | 228 | `ExceptionCheck` | after each `addBoolean` call |
| `0x88` | 17 | `ExceptionClear` | called, not `FatalError` — a pending exception here is **swallowed**, not fatal |
| `0xb8` | 23 | `DeleteLocalRef` | cleans up the array element every iteration |

Control flow, verified instruction-by-instruction:

1. `FindClass("com/roblox/client/flags/NativeFlagsInitResult")`.
2. `GetMethodID(class, "<init>", "(I)V")` and `GetMethodID(class, "addBoolean",
   "(Ljava/lang/String;ZZ)V")` — matching `native/init_params.cpp`'s own
   `NativeFlagsInitResult::ctor(ENV*, jint)` and
   `addBoolean(ENV*, shared_ptr<String>, jboolean, jboolean)` hooks exactly, both
   name and descriptor. **Cordial's Java-side stub for this class is shaped
   correctly.**
3. Calls a local helper at `0x215a308` with a *global* pointer (`0x77d9298`, an
   internal engine singleton, unrelated to the JNI argument) to obtain an `int` —
   logged as `"nativeInitializeNativeFlags: flagCount = %d"` (string at `0x34b993`,
   tag `"rbx.JNIRobloxSettings"` at `0x4d0b6b`). This value becomes the constructor's
   sole `int` argument.
4. `NewObject(class, ctor, thatInt)` via a direct-call wrapper at `0x20b7658` — the
   `int` passed here is a **provider ID**, not a count of the array (see §3).
5. `GetArrayLength(env, arg3)` — **confirms the third JNI parameter is read as an
   array**, matching the dex descriptor.
6. Loop `i = 0 .. length-1`:
   - `GetObjectArrayElement(arg3, i)` → one array element (a `jstring`, per the
     descriptor).
   - A local helper (`0x20b6351`) extracts it into a small string wrapper (SSO
     inline buffer or heap-allocated, decided by a size check).
   - A local helper (`0x215a5ce`) looks that name up in an internal table (via a
     shared hash-table `find` at `0x215a48a` — the same low-level `find` also used,
     unrelated to flags, by another function this pass ran into at `0x68fab46`;
     `objdump`'s nearest-export label for it, `...FMOD_OutputAAudioHeadphonesChanged`,
     is nearest-symbol noise per `app-bridge.md` §4.1's caveat, not a real
     relationship).
   - Logs either `"nativeInitializeNativeFlags: ... %d: %s = %s"` (found, `0x3d6727`)
     or `"nativeInitializeNativeFlags: ... %d: %s not found."` (not found,
     `0x40eed1`) — **this is the log line the reproduction run below actually
     produced.**
   - `addBoolean(result, name, found, wasOverridden)`.
   - `ExceptionCheck` → if true, `ExceptionClear` (swallowed, execution continues).
   - `DeleteLocalRef` on the element, next iteration.
7. Stack-canary check, return the constructed `NativeFlagsInitResult`.

**No path through this function — including all three of its own internal, non-exported
helpers, which were disassembled in full — references the strings `"NativeHelper"` or
`"gameActivity_onFlagsFailed"`, and none of them can throw past the per-element
`ExceptionClear`.** Whatever ends up in the array, this function returns a validly
constructed result object. It is not, itself, capable of calling
`gameActivity_onFlagsFailed`, and nothing in it looks capable of segfaulting on its
own account either (SSO/heap string handling is bounds-checked; the hash lookup is a
generic `find`; the JNI calls are all standard, guarded ones).

---

## 2. The live bug: Cordial is passing the entire ClientSettings document as a single flag *name*

`native/init_params.cpp`'s `cordial_init_flags` (the C++ side of `cordial_init_flags`,
called from `crates/cordial-runtime/src/bin/load.rs`) contains this, **as of this
session, unedited by this investigation**:

```cpp
// The array is a list of flag *names to cache*, not a settings document.
//
// This was wrong for several iterations: passing Roblox's ClientSettings
// JSON here made the engine call addBoolean with the entire document as a
// single flag name, which is exactly what the trace showed. ...
// An empty list is therefore correct: cache nothing up front. ...
const bool have = settings_json && *settings_json;
auto arr = std::make_shared<jnivm::Array<jnivm::String>>(have ? 1 : 0);
if (have) {
    arr->Set(0, cordial::S_pub(settings_json));
}
```

The comment states the array should always be empty. **The code directly below it
does not do that** — when `--client-settings` points at a real file (as the
reproduction instructions in this task do), `have` is true and the entire file
contents become element 0 of a one-element `String[]`. This is a live discrepancy
between documented intent and actual behaviour, not a hypothesis: the reproduction
run below shows it happening.

### Reproduced

Ran the exact command given, against the already-built binary (did **not** run
`cargo build --release` first, to avoid racing the concurrent edits to
`native/*.cpp`):

```
CORDIAL_STUB_QUIET=1 timeout 100 ./target/release/cordial-load --lib-dir "$LIBDIR" \
  --apk "$APK" --client-settings /tmp/clientsettings.json --host-libc --game-activity --run 8
```

Result: **exit 139 (SIGSEGV)**, `timeout: the monitored command dumped core`. The
captured output includes:

```
[roblox] flags FAILED — the engine could not load its flag set
I/rbx.JNIRobloxSettings    nativeInitializeNativeFlags: ... 0: {"applicationSettings":{"DFFlagConsumePlatformNameOverAlternateName":"False", ... [4123 bytes, cuts off mid-JSON, no closing brace] ...
```

That second line is exactly step 6's "found"/"not found" log (§1), firing for array
index 0, whose "name" is the raw JSON text — confirming, at runtime, that the array
Cordial builds today is the one-element, whole-document array the comment says is
wrong. The line is truncated at roughly 4 KB, consistent with `__android_log_print`'s
own bounded formatting buffer (not a crash by itself — the line ends cleanly with a
newline).

**Caveat on ordering:** the `onFlagsFailed` print (via `fprintf(stderr, …)`, which
Cordial's `NativeHelper::onFlagsFailed` hook uses unbuffered) appears *before* the
`__android_log_print`-based line in the captured file, even though §1's disassembly
shows the per-element log is produced from inside `nativeInitializeNativeFlags`,
which — per §1 — cannot itself call `onFlagsFailed`. The most likely explanation is
buffering: the two log paths go through different sinks with different flush
timing, so file order does not necessarily reflect wall-clock order. Treat the
printed order as inconclusive; both facts (the call happened, the truncated-JSON
log line happened) are independently confirmed, their relative order is not.

**No usable backtrace could be obtained in this sandbox.** There is no `gdb`
installed, and `coredumpctl` (`core_pattern` is piped to `systemd-coredump`) lists
no core for this crash — its storage is not retaining dumps here (other processes'
crashes show the same `COREFILE missing`). This is worth recording so a future
session does not repeat the attempt: getting a real backtrace requires either
installing a debugger, or Cordial installing its own `SIGSEGV` handler (the
terminate-handler precedent in `native/jni_shim.cpp`, per `findings.md` §8.2, is the
right shape for this).

---

## 3. Where `gameActivity_onFlagsFailed` is actually called from — verified, with one open edge

A whole-binary scan (every rip-relative `lea` in `.text`, ~80.6 MB, checked against
its computed target — the same method `render-gate.md` §2 used for
`eglCreateWindowSurface`) for the string `"gameActivity_onFlagsFailed"` (the only
occurrence of that string in the file, at `0x40f096`) found **exactly one** site
referencing it: `0x29c931e`.

That address is inside a small, non-exported helper starting at `0x29c92eb`. Its
entire body:

```
GetMethodID(cachedClass, "gameActivity_onFlagsFailed", "()V")   ; class ref cached at 0x6eba788
CallVoidMethod(cachedInstance, thatMethodID)                     ; instance cached at 0x6eba780
```

— i.e. this is precisely the JNI call that reaches Cordial's
`NativeHelper::onFlagsFailed` hook (`native/init_params.cpp:279`). A sibling copy
seven bytes later (`0x29c937d`) is structurally identical but calls
`"gameActivity_onEngineInitialized"` instead (`0x462ada`) — both share the
`FLog::JNINativeHelper] FATAL: Java exception occurred in JNI call.` diagnostic
string (`0x40f0d3`), consistent with a generic "call this no-arg `NativeHelper`
callback, log FATAL if it throws" template instantiated per callback name.

**This `onFlagsFailed` helper has exactly one direct caller in the whole
binary**: `0x29c553c`. That call site is gated by:

```
this = <some object>                     ; entered with this in %rbx
if (this->[0x10] == null) goto skip;      // 0x29c54f4
handle = getter(this->[0x10]);            // 0x2932f40 — looks like weak_ptr::lock()
if (handle == null) goto skip;            // 0x29c550d
target = this->[0x8];
if (target != null) {
    target->[0x10] = 11;                    // marks a state/result field FAILED
    obj = target->[0x40]->[0x10]->[0x10];   // three more pointer dereferences
    report_onFlagsFailed(obj);              // 0x29c92eb — the call at 0x29c553c
}
// unconditionally: release `handle` (refcount decrement, virtual dtor if it hits 0)
```

(`skip` reaches the same release/cleanup path with no report — i.e. those two
null-checks are safety guards around an otherwise-unconditional report, not a
separate success/failure branch; whatever decided "this is the failure case"
happened earlier, outside what this pass could see.)

**The containing function (starts at `0x29c52cc`) has zero direct callers anywhere
in `.text`.** Exactly like `render-gate.md` §2's GPU-tier device-cache function, it
is reached only through an indirect call (a function pointer, `std::function`, or
virtual dispatch) that a direct-`call`-site scan cannot follow. This is the
honest edge of what this pass could establish: **what invokes this check, and
under what real-world condition `this->[0x8]` ends up non-null with state
worth marking `11`, is not determined here.**

What can be said with confidence: `this`, `this->[0x10]`, and `this->[0x8]` are
**not** Java objects and are **not** anything Cordial's JNI layer
(`NativeFlagsInitResult`, `NativeHelper`, etc.) constructs or touches — they are
private, internal C++ engine state, invisible to JNI entirely. Nothing in
`native/init_params.cpp` initialises or influences this object graph one way or
the other. If the crash is in the three-deep dereference chain
(`target->[0x40]->[0x10]->[0x10]`) immediately preceding the report call, or in
whatever indirectly invokes `0x29c52cc` at all, it would be because that internal
state was never populated the way the real Android app populates it before this
code path runs — consistent with, but not proven by, the reproduction in §2. This
is inference, not a verified fact; nailing it down needs a live debugger (§2's
caveat) or an instrumented breakpoint/wrapper at `0x29c52cc`'s entry, similar in
spirit to the libc-call wrapper technique `findings.md` §8.1 already uses.

---

## 4. `nativeRegisterJavaFlagProvider` — verified unreachable from Java in this build

`Java_com_roblox_client_flags_FlagJniInterface_nativeRegisterJavaFlagProvider` is
exported at `0x29aba7a` (57 bytes, ending well before the next export,
`MemStorage.setItem`). Checked against **all three** shipping dex files with
`tools/dex_method.py`, both restricted to the `FlagJniInterface` class and with an
unrestricted name search across every class:

```
$ python3 tools/dex_method.py apk/dex/ --class com/roblox/client/flags/FlagJniInterface
# lists 7 methods: nativeGetFFlag, nativeGetFInt, nativeGetFString,
# nativeInitializeNativeFlags, nativeRegisterFFlag, nativeRegisterFInt,
# nativeRegisterFString — no nativeRegisterJavaFlagProvider

$ python3 tools/dex_method.py apk/dex/ nativeRegisterJavaFlagProvider
no match
```

**No Java class in this shipping build declares this native method at all.** It
cannot be reached through the normal JNI static-linkage path (the
`Java_com_roblox_..._methodName` symbol convention only matters if some
`class.native(...)` declaration in the dex causes the JVM to look it up), and
nothing else in the dex calls it either. Its own body confirms it does not expect
to be called through the normal path: it takes the standard `(JNIEnv*, jclass, ...)`
JNI convention but **ignores every incoming argument** and simply:

```
GetOrRegisterProviderId(&globalProviderRegistry)   ; call 0x215a308 — the SAME
                                                     ; helper nativeInitializeNativeFlags's
                                                     ; own preamble calls (§1 step 3)
__android_log_print(INFO, "rbx.JNIRobloxSettings",
    "nativeRegisterJavaFlagProvider: Registered external flag provider from Java with ID: %d",
    result)
return result
```

The log format string (`0x4b41e6`) confirms the semantics: the shared counter at
`0x215a308` is a **provider-ID generator**, and this native is just an alternate
(unused, in this build) entry point into the same registration the constructor
step in §1 already performs inline. **Answer to the task's question: no, the flag
pipeline does not require calling it first — the real shipping app does not call
it either, and `nativeInitializeNativeFlags` already performs the equivalent
registration itself.** This rules it out as the missing piece.

---

## 5. What's verified vs inferred

**Verified** (disassembly, cross-checked against Cordial's own `jni.h` and
`native/init_params.cpp`, and against a live reproduction):
- `nativeInitializeNativeFlags`'s full body, all three of its private helpers,
  builds `NativeFlagsInitResult` exactly as Cordial's Java-side stub expects
  (§1), takes a `String[]` of flag *names* (not a document), and cannot itself
  call `onFlagsFailed` or crash on the input it's given.
- `native/init_params.cpp`'s `cordial_init_flags` currently packs the entire
  settings document as element 0 of that array when `--client-settings` is
  supplied, contradicting its own comment (§2) — reproduced live.
- The sole reference to `"gameActivity_onFlagsFailed"` in the binary, its sole
  caller, and that caller's gating logic (§3).
- `nativeRegisterJavaFlagProvider` is unreachable from Java in this dex set and
  is not a prerequisite (§4).
- The reproduction: exit 139/SIGSEGV, `onFlagsFailed` fires, the truncated
  whole-document-as-flag-name log line fires (§2).

**Inferred, not verified:**
- That the segfault is specifically the `target->[0x40]->[0x10]->[0x10]` chain in
  §3, or specifically triggered by whatever indirectly invokes `0x29c52cc`. No
  backtrace was obtainable in this sandbox (§2) to confirm the exact faulting
  instruction.
- Why `this->[0x8]` (§3) ends up non-null/marked-11 at all in Cordial's run — this
  requires knowing what real Android's flags subsystem normally does to populate
  it, which is internal engine state with no JNI-visible surface.

**Not established:**
- Whether fixing §2 (passing an empty array, as the code's own comment says it
  should) changes the outcome. Given §3's trigger looks independent of the
  array's contents — it's gated on unrelated internal engine state, not on
  anything `nativeInitializeNativeFlags` touches — there is no evidence either
  way that this fixes `onFlagsFailed`, only that it removes a confirmed,
  currently-live bug (a multi-megabyte string being hashed, logged, and searched
  for as if it were a flag name).

---

## 6. Recommendation

1. **Fix the discrepancy in `native/init_params.cpp`'s `cordial_init_flags`
   (§2) to match its own comment** — always construct a zero-length array,
   regardless of whether `settings_json` is non-empty. This is confirmed-wrong
   today and reproducibly wastes a multi-KB log line and a hash lookup on a
   string that can never be a real flag name. Low risk, because §1 shows an
   empty array makes the loop in step 6 simply not execute — the function still
   returns a validly constructed (if empty) `NativeFlagsInitResult`.
2. **Do not add a call to `nativeRegisterJavaFlagProvider`** — §4 shows it is
   unreachable in the real app and redundant with what
   `nativeInitializeNativeFlags` already does internally.
3. **Fix #1 is very unlikely to be sufficient on its own.** §3's trigger for
   `onFlagsFailed` is gated on internal engine object state
   (`this->[0x10]`, `this->[0x8]`) that has nothing to do with the JNI array
   argument — it is reached through an indirect call this pass could not
   resolve. Expect `onFlagsFailed` (and possibly the segfault) to persist after
   fix #1, unless it happens to be timing-sensitive in a way that an empty,
   fast-returning array changes.
4. **The highest-leverage next step is a working debugger or core-dump
   pipeline in this environment** — there is no `gdb` here and
   `systemd-coredump` is not retaining dumps (§2). Without a backtrace, closing
   §3's open edge (what calls `0x29c52cc`, and what exactly is null/dangling at
   the crash) requires either an external debugger, or an instrumented
   breakpoint/wrapper at `0x29c52cc`'s entry — printing `this`, `this->[0x10]`,
   and `this->[0x8]` at runtime, the same wrapper-based instrumentation
   technique `findings.md` §8.1 already used successfully for libc calls.

---

## 7. Follow-up session: `lldb` is available now, and it found a second, real bug

`lldb` (`/home/linuxbrew/.linuxbrew/bin/lldb`) turned out to be present in this
environment (§6's blocker assumed it was not). With `settings set
target.disable-aslr true`, `libroblox.so` loads at a fixed address every run
(`0x7fffefec0000` under the task's exact repro invocation), which makes raw
breakpoint addresses reproducible run to run. This section only reports what
was confirmed by breaking and inspecting live state — per this project's own
hard-won lesson, static disassembly alone has repeatedly produced wrong
conclusions here (see §3 below for a concrete instance of exactly that).

### 7.1 The real, fixed bug: `NativeFlagsInitResult`'s constructor was never reachable

`native/init_params.cpp` registered `NativeFlagsInitResult`'s constructor with:

```cpp
c->HookInstanceFunction(env, "<init>", &NativeFlagsInitResult::ctor);
```

This looks right, and §1's disassembly (`GetMethodID(class, "<init>", "(I)V")`
then `NewObject`) looks like it should call it. **It never did.** The live JNI
trace (`JNI_TRACE` build of `third_party/libjnivm`) showed, on every run before
this fix:

```
[JNIVM]: Constructed Unresolved symbol, Class=`NativeFlagsInitResult`,
    StaticMethod=`<init>`, Signature=`(I)Lcom/roblox/client/flags/NativeFlagsInitResult;`
[JNIVM]: Call Unknown Static Function Class=`NativeFlagsInitResult` Method=`<init>` ...
```

i.e. libjnivm was looking for a **static** method literally named `<init>`
whose signature has the **return type folded in**, not the instance
constructor Cordial registered. The cause is in libjnivm itself
(`third_party/libjnivm/src/jnivm/internal/method.cpp:13-24`,
`jnivm::GetMethodID`):

```cpp
// Rewrite init to Static external function
if(!isStatic && sname == "<init>") {
    // strips everything after ')', appends "L<nativeprefix>;"
    return GetMethodID<true, ReturnNull, AllowNative, trace>(env, cl, str0, ssig.data());
}
```

Every *instance* `GetMethodID(cls, "<init>", sig)` call is unconditionally
rewritten into a **static** lookup with signature `sig-up-to-')'` +
`"L" + nativeprefix + ";"`. So `GetMethodID(class, "<init>", "(I)V")` actually
resolves against `("<init>", "(I)Lcom/roblox/client/flags/NativeFlagsInitResult;")`,
static. `HookInstanceFunction` can never register a match for that lookup —
it registers an *instance* method with the *original* signature. The engine
got back an auto-synthesized unresolved-symbol stub (which `defaultVal<jobject>`
makes return null), called it, and treated the null/degenerate result as a
reason to report `onFlagsFailed`.

This is not specific to `NativeFlagsInitResult` — it is true of *every*
`<init>` this codebase registers via a real `NewObject`/`GetMethodID` path from
the engine's side. It happened not to matter elsewhere because every other
class in `native/init_params.cpp` is constructed by *Cordial's own C++ code*
calling a `Create()` factory directly (never through JNI dispatch), so the
libjnivm rewrite was never exercised for them. `NativeFlagsInitResult` is
the one class the *engine itself* constructs via `NewObject`, which is exactly
why this only showed up here.

**Fix applied:** register the constructor the same way this file's own
`Create()` factories are shaped — as a plain **static** function taking
`(ENV*, Class*, jint)` and returning `std::shared_ptr<NativeFlagsInitResult>`,
via `c->Hook(env, "<init>", &NativeFlagsInitResult::ctor)` (not
`HookInstanceFunction`). `Class::Hook` auto-detects "static" from the
parameter types (second parameter `Class*`, not `Object*`/`jobject`), and its
derived signature is exactly `"(I)L<nativeprefix>;"` — matching libjnivm's
rewritten lookup. Confirmed live: the trace now reads
`Found symbol ... StaticMethod=\`<init>\`` and
`Call Static Function ... Method=\`<init>\`` (not "Unresolved"/"Unknown") —
the constructor genuinely runs now, `NativeFlagsInitResult` is built with a
real backing `JavaMap`, and its return value is a valid, non-null object
reaching the caller. **`gameActivity_onFlagsFailed` still fires afterward
(see §7.3) — this fix was necessary but not sufficient**, exactly as §6.3
warned.

Also fixed in the same pass: §2's confirmed live bug (the whole ClientSettings
document being packed as a single array element) — `cordial_init_flags` now
always builds a zero-length array, matching its own comment, regardless of
whether `--client-settings` is set.

### 7.2 `com.roblox.engine.jni.model.ClientLocalFlags` implemented; `readLocalFlags()` called

A second investigation thread (a parallel review of the render/network path)
found that `NativeGLInterface.readLocalFlags()` — `()Lcom/roblox/engine/jni/
model/ClientLocalFlags;`, exported at `Java_com_roblox_engine_jni_
NativeGLInterface_readLocalFlags` — is the engine's *offline* counterpart to
fetching `ClientSettings` over the network: it reads whatever bundled/cached
flag defaults the engine has and hands them back via the same `new` +
repeated `add(name, value)` idiom `nativeInitializeNativeFlags` uses for its
own result object. Nothing in the shipping dex calls it on the
`ActivityNativeMain` chain Cordial drives (dex xref: its only caller is a
different startup path), so it was entirely dead code here, and its Java
counterpart class was completely unimplemented.

Implemented `ClientLocalFlags` (dex-verified shape: `<init>()V`,
`add(String,String)V`, `getAll()Lorg/json/JSONObject;`, `isEmpty()Z`,
`size()I`) plus a minimal `org.json.JSONObject` stub, using the same
static-factory `<init>` registration §7.1 established is required. Wired a
`cordial_read_local_flags` bridge and call it right after
`nativeInitializeNativeFlags`. **Result: it runs cleanly (no crash, no
unresolved-symbol noise) but calls `add()` zero times** — this build has no
bundled local flag defaults on disk, so the engine constructs an empty
`ClientLocalFlags` and returns. `onFlagsFailed` is unaffected.

### 7.3 The real trigger: `onFlagsFailed` fires from an unrelated background thread, confirmed by breakpoint

§3's static disassembly identified a single, specific address
(`libroblox+0x29c92eb`) as "the helper that calls `gameActivity_
onFlagsFailed`", reached from one caller at `libroblox+0x29c553c`. **Both
addresses were placed as raw hardware breakpoints and neither one was ever
hit before the process crashed** — even though, in the same run without those
breakpoints, `onFlagsFailed` demonstrably fired (Cordial's own hook printed
`[roblox] flags FAILED`). This is exactly the kind of static-analysis error
the top of this file warns about: the string-reference scan found *a*
call site for the string `"gameActivity_onFlagsFailed"`, but not necessarily
*the* one actually exercised at runtime by this code path.

Breaking instead on Cordial's own hook —
`cordial::NativeHelper::onFlagsFailed` (a real symbol in `cordial-load`,
`nm`-verified, so no address guessing needed) — hits reliably, and its
backtrace is unambiguous:

```
frame #0  cordial::NativeHelper::onFlagsFailed
frame #1  jnivm::Wrap<...>::InstanceInvoke
frame #2  jnivm::MDispatchBase2<void>::CallMethod
frame #3  jnivm::MDispatchBase<void,jobject*>::CallMethod(..., va_list)
frame #4  libroblox+0x68a6fe3
frame #5  libroblox+0x29c8fff
frame #6  libroblox+0x29c9349
frame #7  libroblox+0x29c5541      <- return addr; matches §3's claimed
                                       call site (0x29c553c) + 5 bytes exactly
frame #8-10  libroblox (+0x1f9b850, +0x1f9b728, +0x1f9b5a7)
frame #11 libc.so.6`start_thread + 921
frame #12 libc.so.6`__clone3 + 44
```

**This call happens on a separate `pthread`, spawned via `start_thread`/
`__clone3` — not on the thread that runs Cordial's sequential bring-up code
(`nativeInitializeNativeFlags`, `readLocalFlags`, `nativeInitClientSettings`,
etc. all run on the "calling" thread; this backtrace contains none of those
frames).** §3's outer-caller address (frame #7, `0x29c553c`) is confirmed
correct — the return address matches exactly — but §3's claim that the
`onFlagsFailed`-reporting helper itself lives at `0x29c92eb` is off by one
level of the call chain (frame #6 is closer to that address; the actual
`GetMethodID`+`CallVoidMethod` pair is one level deeper, around
`0x29c8fxx`/frame #5). More importantly: **this whole chain runs
independently of, and after, whatever Cordial's own thread has done.**

This was confirmed empirically, not just from one backtrace: `onFlagsFailed`
fires with the *identical* character (same log line, same async-thread
backtrace shape) across every combination tried in this session —
`nativeInitializeNativeFlags` called with an empty array (correct) or the
old buggy whole-document array; `readLocalFlags` called or not; and
`nativeInitClientSettings` (§7.4) called with a real `ClientSettings`
document, with all-empty arguments, or not called at all. **Nothing this
session found how to influence changes whether or when `onFlagsFailed`
fires.** That is consistent with §3's original conclusion — the trigger is
gated on internal engine state this pass could not identify the origin of —
now confirmed live rather than inferred from disassembly alone.

### 7.4 `nativeInitClientSettings` / `nativePostClientSettingsLoadedInitialization3` — wired, with one new hazard found

Per the architecture Roblox ships: these `NativeGLInterface` natives are not
the engine asking Cordial for settings over JNI — they are the interface a
**host app** uses to hand the engine settings *it* already fetched. Cordial
is the host app here, so calling these directly (with real data, no forged
HTTP responses) is the legitimate interface, not a workaround. Dex-verified
descriptors:

```
nativeInitClientSettings(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)I
nativeInitClientSettingsSigned(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)I
nativeInitClientSettingsCached(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;J)I
nativePostClientSettingsLoadedInitialization3(Ljava/util/List;)V
```

Implemented `cordial_init_client_settings` (unsigned variant only — deliberately
not touching `...Signed`, since forging a signature would misrepresent real
account/server state) and called it with the real `--client-settings` document
in the middle argument, empty strings for the other two (their exact roles
were not determined — see below). **It returns `1` — but that value is
*not* a validation result**: it returns exactly `1` for every combination
tried, including all three arguments empty. That is strong evidence the
`int` is a synchronous request handle/"accepted for async processing" code,
not a success/failure flag — consistent with §7.3's finding that the actual
accept/reject decision happens later, on a different thread.

**A new, real hazard found and *not* shipped enabled:**
`nativePostClientSettingsLoadedInitialization3(List)`, called with an empty
`java.util.ArrayList` (a new minimal `JavaList` stub, same static-factory
`<init>` pattern), **crashes synchronously, immediately, on the calling
thread** — verified live under `lldb`: `SIGSEGV`, fault address `0x8`, inside
`libc.so.6\`_IO_fflush`, called from inside the engine's own implementation
of this native. This is a *worse* regression than the pre-existing
asynchronous crash (§3's `libroblox+0x2ccd937`, `SingleSurfaceAppImpl`'s null
`JNIEnv`) — it happens earlier, synchronously, and on Cordial's own thread
instead of an engine-internal one. **The call is implemented (`cordial_
post_client_settings_loaded` / `game_activity::post_client_settings_loaded`)
but gated behind `CORDIAL_TRY_POST_CLIENT_SETTINGS=1` and not run by
default** — an empty list is evidently not what this native expects, and
guessing further was not attempted this session (time-boxed). Whatever real
list contents it wants remain undetermined.

With `nativePostClientSettingsLoadedInitialization3` disabled, the crash
reverts to the original, unrelated `0x2ccd937` (confirmed via the same
breakpoint-on-`onFlagsFailed`-and-`continue` technique) — i.e. §7.4's changes,
as shipped, introduce no new crash.

### 7.5 `nativePreloadFlagOverrides` — a second real bug found, format still undetermined

`--flag-overrides <f>` was **parsed but never wired to any native call at
all** before this session — `opt.flag_overrides` existed as a CLI field with
no corresponding call anywhere in `native/init_params.cpp` or `load.rs`. That
fully explains §3's original "no extra logging" result: nothing was ever
invoked. A `cordial_preload_flag_overrides` bridge (dex-verified descriptor
`MainGameActivity.nativePreloadFlagOverrides(Ljava/lang/String;)V`, an
*instance* native — second JNI argument is an Activity object, following
`cordial_set_init_params`'s precedent of a bare placeholder `jnivm::Object`)
is now wired and called with the file's raw contents.

A second bug was caught in the same pass, while fixing the first: an initial
version of the wiring re-read `opt.flag_overrides` **as if it were a file
path** — but the option parser (`--flag-overrides` in `load.rs`'s argument
loop) already reads the file at parse time and stores its *contents*, not
its path. That re-read silently failed and passed an **empty string**
through. Fixed to use the stored content directly; confirmed by checking the
transmitted byte count matches the source file's size exactly.

**With the call now genuinely delivering real bytes, still no observable
effect was found**: tried a flat `{"FLogChannelName":"7", ...}` map (the
shape suggested by the FLog-channel hypothesis in §3/crash-trace.md) — no new
log lines, no change to the flags verdict or crash. **The correct JSON shape
for this native remains undetermined.** What is now known for certain: the
call itself is reachable and does not throw or crash with a small flat JSON
object as input. Candidates not yet tried: the doubly-wrapped
`{"applicationSettings":{...}}` shape the real `clientsettings.roblox.com`
response uses (same shape as `/tmp/clientsettings.json`, which is ~1.2MB —
worth trying whole, since "preload" suggests it may want the same document
`nativeInitClientSettings` takes); a JSON array of flag names (mirroring
`nativeInitializeNativeFlags`'s actual argument shape); or no JSON at all
(a newline-separated list, as this file's own `--client-settings` help text
describes for a *different*, unrelated option, which may be a hint about the
project's own earlier assumptions rather than the engine's real expectation).

### 7.6 Summary: what changed, what didn't

**Fixed, verified live:**
- `NativeFlagsInitResult`'s constructor now actually runs (§7.1) — this was a
  real, confirmed bug (unreachable native constructor), not a hypothesis.
- `cordial_init_flags` no longer packs the whole ClientSettings document as a
  single array element (§7.1, closing §2/§6.1).
- `readLocalFlags()` / `ClientLocalFlags` implemented and called (§7.2) —
  runs cleanly, contributes nothing (no bundled local defaults in this build).
- `nativeInitClientSettings` implemented and called with the real
  `ClientSettings` document (§7.4) — runs cleanly, returns `1` (an accept
  code, not validation) regardless of payload.
- `--flag-overrides` is now actually wired to `nativePreloadFlagOverrides`
  and delivers real bytes (§7.5) — it was previously a dead CLI option.
- A new synchronous crash (`nativePostClientSettingsLoadedInitialization3`
  with an empty `ArrayList`) was found and **not** shipped enabled (§7.4).

**Not fixed — honest negative result:** `gameActivity_onFlagsFailed` still
fires. It is confirmed, by breakpoint (not inference), to run on a separate
pthread whose creation and decision-making this session could not trace back
to any Cordial-controlled input — every synchronous call this session added
or removed left its behaviour identical. The condition that "chooses failure
over success" is internal engine state on that background thread, still not
identified. The highest-leverage next step is the same as §6.4 named:
tracing what spawns that pthread (breakpoint on `pthread_create`, or on the
`start_thread` return addresses seen in §7.3's backtrace, `libroblox+
0x1f9b5a7`/`+0x1f9b728`/`+0x1f9b850`, to find where that thread's *own*
entry point is, not just its later call stack).

---

## §8. Client settings are not what the flags verdict depends on

Cordial now fetches Roblox's real client-settings document and the engine accepts
it: `nativeInitClientSettings` returns `0`, and
`nativePostClientSettingsLoadedInitialization3` then succeeds. (That call used to
crash synchronously in `_IO_fflush`; the crash was a *consequence* of the
settings not being accepted, not a bad `List`, and it went away on its own once
`nativeInitClientSettings` started returning `0`. It is now unconditional.)

The verdict is still `onFlagsFailed`.

Three orderings were tried, each strictly earlier than the last:

| when the settings are delivered | result |
|---|---|
| after the flag calls (original) | `flags FAILED` before the call |
| before the flag calls | `flags FAILED` still first |
| **before `initializeNativeCode`** — `-> 0` | `flags FAILED` unchanged |

The third is the decisive one. The settings are parsed and accepted before the
engine's own bring-up even starts, and the verdict does not move. So this is not
a race that a better ordering can win, and **the flags verdict does not depend on
the client-settings document.**

That is worth stating plainly because two independent investigations converged on
client settings as the likely root cause of `onFlagsFailed`, and the reasoning
was good: flags *are* client settings, the document really was missing, and the
fetch really was never happening. It was still wrong. The work it produced is
real — the CA bundle, the asset folder, the fetch, the call contract — but none
of it is the answer to this question.

What remains unexplained: `onFlagsFailed` arrives on a background thread, early,
with a full and valid flag set already installed. Whatever it is testing, it is
not "do I have flags".

## §9. Where the verdict is actually made

A breakpoint on Cordial's own `NativeHelper::onFlagsFailed` gives the real call
chain (offsets into `libroblox.so`, ASLR disabled, base `0x7fffefec0000`):

```
onFlagsFailed  (Cordial)
  <- libjnivm CallMethod
  <- 0x29c8fff   JNI varargs wrapper
  <- 0x29c9349   inside a small reporter function starting at 0x29c92eb
  <- 0x29c5541
  <- 0x20db850 / 0x20db728 / 0x20db5a7  = nativeGameGlobalInit
  <- start_thread
```

Two facts worth having:

**There are two separate reporter functions, not one branch.** The string
`gameActivity_onFlagsLoaded` is referenced from exactly one place, `0x29c9182`,
inside a function beginning at `0x29c9120`; `gameActivity_onFlagsFailed` from
exactly one place, `0x29c931e`, inside a *different* function beginning at
`0x29c92eb` (`push %rbp` prologue). So neither reporter chooses anything — the
choice is made by whoever calls one of them.

**The call site is already committed.** At `0x29c553c` the failed reporter is
called unconditionally:

```
29c5519:  mov  0x8(%rbx),%rax     ; an object hanging off the caller's state
29c5527:  je   29c5541            ; skipped entirely if that is null
29c5529:  movl $0xb,0x10(%rax)    ; write 11 into it -- a status, before reporting
29c5530:  mov  0x40(%rax),%rax
29c5538:  mov  0x10(%rax),%rdi
29c553c:  call 29c92eb            ; report FAILED
```

The `movl $0xb` is the useful part: **11 is written into `+0x10` of that object
immediately before the report.** That is a status code being recorded, and it is
a far better instrumentation target than the report itself — a watchpoint on
that word catches the moment the verdict is decided, with the deciding frame
still on the stack. Whatever picks `0xb` is upstream of `0x29c5541` and is the
actual question.

Stopping the static walk here deliberately. This is the point at which previous
investigations on this binary started inferring, and eight consecutive inferences
have been wrong. The next step is a watchpoint on that status word, not more
disassembly.

## §10. Answered — and not the blocker

**Breakpoints inside `libroblox.so` do not work, and never have.** Cordial
`mmap`s the library with its own bionic loader, so the system dynamic linker
never registers it: lldb's `image list` never lists it, and
`breakpoint set --address`/`--shlib` stay permanently `unresolved, hit count 0`.
The only technique that works is writing `0xCC` into the target address with
`memory write`, then on the trap rewinding `$pc` and restoring the original byte.

This is worth knowing before anything else here: **any earlier claim in this repo
of "I set a breakpoint in the engine" is suspect unless it used that method.**
Breakpoints in Cordial's *own* code (`onFlagsFailed`, the Rust driver) resolve
normally, and crash-stop backtraces are genuine — only breakpoints inside the
mapped engine silently never fire.

**What writes the failure status.** RTTI on `%rbx` at `0x29c5529` — not on
`%rax`, whose first qword is a self-pointer rather than a vtable — gives:

```
std::__ndk1::__function::__func<
    RBX::NativeDataModelManager::getFlagsFromEngine()::$_0, ... void()>
```

So the write comes from the completion lambda of
**`RBX::NativeDataModelManager::getFlagsFromEngine()`**, not from generic flag
glue. A sibling type exists for `initEngine()`'s lambda.

**The watchpoint.** `+0x10` holds `2`, and is written exactly once for the whole
run — to `11` (`0xb`) — from that lambda. Nothing else ever touches it, and no
success value is ever written.

**The success path exists but is never reached.** The success reporter has
exactly one static caller in the entire 115 MB binary: `0x29c346d`, inside the
real body of `nativePreloadFlagOverrides`, immediately after a *conditional*
write of status `3` guarded by a byte at `[r15+0x288]`. The reporter itself is
straight-line and unguarded, so if it were reached it would call
`gameActivity_onFlagsLoaded`. Across five runs `onFlagsLoaded` is never resolved
or called. So the success path is gated by something upstream inside
`nativePreloadFlagOverrides` that this harness never satisfies — which is
interesting, because `nativePreloadFlagOverrides` is a function Cordial *does*
call, and whose payload format is still unknown.

**Priority, stated plainly.** A parallel investigation confirmed that the flags
verdict does *not* gate rendering (see `render-gate.md`): the crash address moves
between paths while the verdict stays constant. Both results hold at once — the
flag load really does fail, and that failure really does not block the frame. So
this is a genuine defect worth fixing for correctness, but it is **not** the
render blocker, and it should not be worked before the thread-race deadlock.

## §11. 2026-08-07: two unresolved symbols on the path, both answered, verdict unmoved

Run with `tools/hook_descriptors.py`, a JNI-traced startup, and the engine's own
`FLog` file, which this document had not been using.

### The engine log exists, and it says the flags load

`<profile>/data/files/appData/logs/*.log`. Earlier work looked for it in the
wrong place and concluded none was written; that was wrong. Cordial's `FlagCache`
compresses 1273559 bytes to 366594 and writes them, channel `production`,
tombstone valid. Sober's writes 362169 on the same document. So `onFlagsFailed`
is misleading in the way §7 suspected and now on the engine's own evidence rather
than on a return code.

### Three symbols that could never bind

* `NativeHelper.gameActivity_onFlagsLoaded` — registered `(Ljava/lang/Object;)V`,
  dex declares `(Ljava/nio/ByteBuffer;)V`. **Every prior observation in this
  document that `onFlagsLoaded` "is never resolved or called" was made with this
  bug in place** and cannot distinguish "the engine did not call it" from "it
  could not have been called". §9's reasoning about the success path rests on
  that observation and should be re-taken, not trusted.
* `GameActivity.bootstrapTheApp()V` — the dex declares it on the subclass
  `MainGameActivity`; the engine looks it up on the base
  `com/google/androidgamesdk/GameActivity` and libjnivm walks no superclass
  chain. It was `Constructed Unresolved symbol` on every run this project has
  ever made.
* `java/util/List.size()I` and `get(I)Ljava/lang/Object;` — `JavaList` registered
  on `java/util/ArrayList` only, and the engine asks the interface. The list
  handed to `nativePostClientSettingsLoadedInitialization3` was therefore an
  object whose every method was a stub.

All three are fixed. The sweep is down to one, `getWaterfallInsets`.

### None of them is the cause

`CORDIAL_TRACE_PATHS=1` over a `--run 10` startup: zero paths containing
`rbx-storage`, byte-identical in that respect to the `CORDIAL_NO_BOOTSTRAP=1`
control taken minutes later. `onFlagsFailed` still fires. Delivering in Sober's
order — settings, then post, then flag names, read off its log at 3.700s and
3.796s — did not change it.

### Where the gap now is

Sober's log, immediately after `nativePostClientSettingsLoadedInitialization3`
at 3.796751s: `ClientRunInfo` git hash, base url and channel; then
`AppPlatformQoSEmergencyHandler was instanced`; then the `Mimalloc` block; then
`RbxStorage::init [INIT] user: flagLoaded` at 3.820885s.

Cordial's log contains **none** of those, zero occurrences each, on a run where
the `AndroidGLView` channel is demonstrably open because another line on it
appears. Comparing distinct log channels over the same ten seconds, Sober reaches
ten and Cordial seven; absent are `AppPlatformQoSEmergency`, `KeyRing`,
`Mimalloc`, `NetworkClient`, `RbxStorage` and `RbxTransportDummyClient`.
`NetworkClient` is expected on a startup-only run and `Mimalloc` is explained by
Cordial not linking it. The other four are not explained.

So: Cordial's call to `nativePostClientSettingsLoadedInitialization3` returns
without the engine's own body of it having run. The two symbols it needs on the
way in are now answered and the body still does not run. The one difference left
that this session can point at is the argument — an empty `List` whose `size()`
now truthfully returns 0, where before it returned whatever an unresolved stub
returns. What that list should contain is still unknown, and recovering it means
reading code units rather than declarations, which is out of scope. **That is
the open question, stated as narrowly as the evidence allows.**

### One assertion in the tree corrected

`init_params.cpp` says the real client passes 139 flag names to
`nativeInitializeNativeFlags`, from a Waydroid capture. The capture is not in
doubt, but Sober — which works — logs `flagCount = 0`. Passing 139 is not a
requirement.

### §11.1 The empty `ArrayList` is correct, and 7.4 can be closed

7.4 recorded the list passed to `nativePostClientSettingsLoadedInitialization3`
as unresolved, and §11 above named it as the last difference worth pointing at.
Both are now settled, and the lead dies.

The erased descriptor is `(Ljava/util/List;)V`, which is all the method_ids table
holds and is why two sessions treated the contents as a guess. The dex also
carries the generic signature, in `dalvik.annotation.Signature`:

    com/roblox/engine/jni/NativeGLInterface.nativePostClientSettingsLoadedInitialization3
      (Ljava/util/List<Lcom/roblox/engine/jni/model/ApplicationExitInfoCpp;>;)V

`tools/dex_signature.py` prints it. Two independent confirmations: the identical
signature is on `nativeSetAppPreviousExitReasons`, and a traced startup shows the
engine doing `FindClass com/roblox/engine/jni/model/ApplicationExitInfoCpp`
immediately after this call.

`ApplicationExitInfoCpp` declares three constructors — `(IIJLjava/lang/String;)V`,
`(IJLjava/lang/String;)V`, and a nine-argument form carrying two more strings, two
longs and an int. That is Android's `ApplicationExitInfo`: reason, importance,
timestamp, description, and on the long form process name and more.

**Who populates it:** the app, from
`ActivityManager.getHistoricalProcessExitReasons()`. **What it means empty:** the
previous run recorded no abnormal exit. So Cordial's empty `ArrayList` is not a
placeholder standing in for something unknown — it is the correct value, and
filling it would be telling the engine about crashes that did not happen.

What that leaves: every input to this native is now correct and every symbol on
the way in resolves, and the engine's body of it still does not run — none of the
seven log lines Sober emits from it appear. The remaining gap is inside that
native and this session cannot name it.

### §11.2 `nativeSetAppPreviousExitReasons` — tried, inert, not shipped

It is exported (`0x295cc6c`) and carries the identical
`(Ljava/util/List<Lcom/roblox/engine/jni/model/ApplicationExitInfoCpp;>;)V`, and
Cordial had never called it, which made it the obvious next candidate on this
handshake. Called with the same empty list, before the settings call, on a
`--run 10` startup: it returns cleanly and changes nothing. Zero `RbxStorage`,
zero `ClientRunInfo`, zero `AppPlatformQoS`, zero `[FLog::AndroidGLView] native*`
lines, and zero paths containing `rbx-storage`.

The code is **not** in the tree. A call that reports success and produces no
engine-log line is exactly what this project forbids adding, so it is recorded
here instead of left behind a flag for somebody to find and switch on.

Worth noting for whoever picks this up: the export addresses put
`nativeSetAppPreviousExitReasons` (`0x295cc6c`) next to
`nativeInitClientSettingsSigned` (`0x295c421`), `...Cached` (`0x295c731`) and
`...CachedCompressed` (`0x295c9ad`), while the two natives Cordial actually calls
sit far away at `0x20b6981` and `0x20f2f6d`. That is an observation about layout
and nothing more, but the newer cluster is the one Cordial has never reached.

### §11.3 The strongest form of the remaining question

`[FLog::AndroidGLView] nativeInitClientSettings` appears in Sober's log and never
in Cordial's, and this is not a verbosity difference. Cordial's log does contain
`[FLog::AndroidGLView] rbx.datamodel: setTaskSchedulerBackgroundMode()` at
severity 6 — the same channel at the same level as Sober's line. The log opens at
1.806s and Cordial's third call to that native happens after the Vulkan device
lines at 3.4s, well inside the window.

So: the same channel, the same level, an open log, a call that returns 0, and no
line. Every symbol on the way in resolves and every argument is correct. The
engine's own body of `nativeInitClientSettings` does not appear to execute, and
naming what stops it is where the next session starts.

### §11.4 Retraction: the native's body does run. Four more leads, all dead

**§11.3 above is wrong and is retracted.** It concluded that the engine's own
body of `nativeInitClientSettings` "does not appear to execute", on the strength
of `[FLog::AndroidGLView] nativeInitClientSettings` never appearing. The
reasoning did not survive its own control.

Cordial made that call three times per run, the first before the log file opens
at 1.806s, so the missing line could have been an artifact of ordering. Gating
the pre-`initializeNativeCode` call off and re-running settles it: `FlagCache`
still fires nine times and still writes the document. So the body runs, well
inside the logged window, consumes the 1273559-byte document, and does not emit
its own line. Why it does not is unknown; `FLogAndroidGLView` is absent from the
settings document, so both clients run the compiled-in default and verbosity is
not the difference either. Neither half of §11.3 stands.

Four further leads, each dead, each recorded so it is not re-run:

* **`FFlagStartRbxStorageInitRighAfterFlags=False`.** The premise of this whole
  line is that the store constructs off the flags-loaded event because that flag
  is True. Overriding it to False, which should route construction back to the
  direct call Cordial already makes, applies cleanly (`1 override(s) applied`)
  and produces no `RbxStorage` line and no `rbx-storage` path. Storage does not
  construct on the other path either.
* **`nativeSetAppPreviousExitReasons`** — §11.2, inert.
* **`nativeRegisterJavaFlagProvider`** — exported by the engine and never called
  by Cordial, which made it look like the missing registration step. It is **not
  declared anywhere in the dex**: no Java class in this APK has a counterpart for
  it, so it is not on any path this build takes. A tempting name is not a lead.
* **The natives Cordial does not call.** The engine exports 91 on
  `NativeGLInterface`, `MainGameActivity` and `FlagJniInterface` together;
  `load.rs` calls 42. The 49 it does not are lifecycle (`PauseApp`, `LeaveGame`),
  purchase, VR, text-box and the three unused client-settings variants. Nothing
  in that list is a plausible prerequisite for the settings handshake.

**The one divergence left that is not explained.** Sober logs
`nativeInitializeNativeFlags: Registered Flag Provider ID from Java: 0`.
No Cordial run in this session logs it, and in its place Cordial emits the same
`JNIRobloxSettings nativeInitializeNativeFlags:` prefix with an empty message,
while every other line on that tag formats correctly (`... 0: <name> not
found.`). Whether that empty line is the provider-registration message arriving
without its value, or Cordial's `__android_log_print` shim dropping a format it
does not handle, is **not established** — and the distinction matters, because
one is an engine-state difference and the other is a logging bug in Cordial.
That is the next thing to settle, and it is one instrumented run away.

### §11.5 Retraction: the flag-provider divergence was Cordial's own log filter

§11.4 closed by naming one unexplained divergence — Sober logs
`nativeInitializeNativeFlags: Registered Flag Provider ID from Java: 0` and no
Cordial run does. **That is wrong and is retracted.** It also described an "empty
message" line in its place, which never existed; that was an artifact of the
`grep -o` pattern used to look for it, not of any run.

Sober's line is at `debug:` priority. `native/liblog.cpp`'s `minimum_priority()`
defaults to `ANDROID_LOG_INFO` and drops everything below it, so Cordial had been
discarding the line before it reached a terminal. `CORDIAL_LOG_LEVEL=d` on the
same build:

    Registered Flag Provider ID from Java: 0
    flagCount = 139.
    Registered Flag Provider ID from Java: 1
    flagCount = 139.

Cordial registers a flag provider, exactly as Sober does.

**The general warning, which is worth more than the retraction.** Sober's log
file contains `debug:` lines; Cordial's stderr, at its default level, does not.
Any comparison between the two that concludes something from an *absence* on the
Cordial side is invalid unless it was taken at `CORDIAL_LOG_LEVEL=d`. Two
conclusions in this session were drawn that way and one of them survived only
because it was tested.

This does not touch the `RbxStorage`, `ClientRunInfo` or `AppPlatformQoS`
absences in §11 and §11.4. Those were read from the engine's own `FLog` file in
`<profile>/data/files/appData/logs`, which the engine writes directly and which
`liblog.cpp` does not filter.

One real difference does fall out of the debug run: Cordial registers **three**
flag providers per launch, IDs 0, 1 and 2, because it calls
`nativeInitializeNativeFlags` three times — the early call, `bootstrapTheApp`,
and the original post-init block. Sober registers one. Whether repeated
registration is harmless is not established.

### §11.6 One delivery, not three. Measured, and still not the fix

The debug run in §11.5 showed Cordial registering flag providers 0, 1 and 2 on a
single launch where Sober registers 0 and stops. Three deliveries: the pre-init
call, `bootstrapTheApp`, and the original post-init block. A fourth would have
followed, because the engine calls `bootstrapTheApp` **twice** per launch — two
`Call Member Function ... bootstrapTheApp ()V` in the trace.

Now one. The pre-init call runs only under `CORDIAL_NO_BOOTSTRAP=1`, the
post-init block is skipped when the bootstrap delivered, and `run_bootstrap`
swaps a flag so the engine's second call is a no-op. `Registered Flag Provider
ID from Java: 0`, once, matching Sober exactly.

It is not the fix. Same run: zero `RbxStorage` in the engine log, zero
`rbx-storage` paths, `onFlagsFailed` still reported.

One thing did move, and it is recorded because it is the only quantity in this
whole session that responded to anything: `onFlagsFailed` is reported **twice**
with the single delivery and **four times** in the `CORDIAL_NO_BOOTSTRAP=1`
control taken minutes later. So the verdict is reported once per delivery
attempt, not once per launch. That is consistent with the verdict being a
property of each attempt rather than a latched startup state, which is new, and
nobody should read more into it than that.

### §11.7 The ordering, read file-to-file: Cordial starts the app bridge first

§11 and §11.4 compared Cordial's `FLog` file against Sober's *captured stdout*,
which after §11.5 is not a comparison anyone should trust. Sober writes its own
`FLog` file, `appData/logs/<version>_<ts>_Player_<id>_last.log`, the same sink in
the same format from the same build. Redone file-to-file, the earlier reading
survives: Sober's file carries all seven lines (`AndroidGLView`
`nativeInitClientSettings` ×1, `ClientRunInfo` ×3, `RbxStorage` ×2,
`AppPlatformQoSEmergency` ×1, `Mimalloc` ×43) and Cordial's carries none.

What the file-to-file view adds is the sequence, and it is not what this document
has assumed.

| | Sober | Cordial |
|---|---|---|
| log opens | 1.652 `RobloxChannel has been set to production` | 1.781 |
| | *engine silent for 2.05 s* | |
| `nativeInitClientSettings` | 3.700 | never logged |
| `nativePostClientSettingsLoadedInitialization3` | 3.796 | never logged |
| `RbxStorage::init [INIT] user: flagLoaded` | 3.820 | never |
| `nativeAppBridgeV2Init` | **3.901** | **1.781 — the first line in the file** |
| `initializeWithAppStarter` | 3.906 | 1.781 |
| `InitializedLuaApp` | — | 3.102 |

**Sober brings the app bridge up 200 ms after the content store. Cordial brings
it up first, and it is the very first thing the engine logs.** Sober's engine
does nothing at all between 1.652 s and 3.700 s: it is waiting for the host
application to hand it settings, and only starts once it has them. Cordial's
engine is already into `nativeAppBridgeV2Init`, `initializeWithAppStarter` and
`InitializedLuaApp` while that handshake is still going on.

Channels Sober reaches and Cordial never does, whole file against whole file:
`AppPlatformQoSEmergency`, `KeyRing`, `Mimalloc`, `RbxStorage`, plus
`AssetProvider`, `NetworkClient`, `RbxTransportDummyClient`,
`RbxTransportRnaExpConnection` and `TrackerAnimationStreamSourceTrace`, which are
join-time and expected absent from a startup-only run. `Mimalloc` is explained by
Cordial not linking it. `AppPlatformQoSEmergency`, `KeyRing` and `RbxStorage` all
sit inside the window Cordial skips past.

**This is the named divergence, and it is an ordering one, not a missing
symbol.** Every symbol on the path now resolves and every argument is now known
correct; what differs is when Cordial does the handshake relative to starting the
engine's application layer. The next experiment is to deliver settings after
`nativeAppBridgeV2InitWithParams` rather than inside `initializeNativeCode`, and
watch for `RbxStorage::init`. That is a reordering of `load.rs`, it is not
attempted here, and it should be done with the `--run 8` startup and a control.

### §11.8 The reordering §11.7 proposed was run. It crashes

`CORDIAL_LATE_SETTINGS=1` moves the whole handshake out of
`initializeNativeCode` and into Sober's position, after
`nativeAppBridgeV2InitWithParams`. Two runs, both `--run 8`:

| | SIGSEGV | reached the app bridge |
|---|---|---|
| `CORDIAL_LATE_SETTINGS=1` | 2 of 2 | no |
| default (`bootstrapTheApp` delivers) | 0 of 2 | yes |
| `CORDIAL_NO_BOOTSTRAP=1` | 0 of 1 | yes |

The engine dies before the bridge is reached, so the late delivery never runs at
all. **Cordial cannot adopt Sober's ordering by moving the call.** Sober's engine
can afford to sit idle from 1.652 s to 3.700 s because the Kotlin activity
lifecycle is what will eventually hand it settings; Cordial drives the natives
directly and by that point has advanced past the state in which they can arrive.

So §11.7's ordering observation stands as a description and dies as a fix. The
difference is real and it is not something a reordering of `load.rs` can close.
Whatever Sober's engine is waiting for during those two seconds, Cordial never
enters that state — and identifying *that* state, rather than any further symbol,
is what the next session should chase.

The gate is left in `load.rs` with the crash recorded in the comment beside it,
on the same reasoning 7.4 kept `CORDIAL_TRY_POST_CLIENT_SETTINGS`: the experiment
has a result and the next person should not have to rebuild it to find out.

### §11.9 There is no state. Those two seconds are the application's, not the engine's

§11.8 ended by saying Sober's engine "spends two seconds in a state Cordial never
enters" and that identifying that state was the next thing to chase. There is no
such state, and that closing sentence is retracted.

The two log lines either side of the gap carry the same thread id:

    1.652055,b25302c0,6 [FLog::Output] RobloxChannel has been set to production
    3.700769,b25302c0,6 [FLog::AndroidGLView] nativeInitClientSettings

Same thread, and the engine emits nothing at all in between. So it did not block
inside a call — it **returned to the host application**, and the application
called back in two seconds later. The only entry in the whole window is Sober's
own launcher, not the engine:

    info: state: Applying remote app settings override: FStringRenderTextureBudgetByRam=""

Sober's lifecycle log accounts for the time in its own terms: `fs_init` 1132 ms,
`devices_init` 1716 ms, `gamemode_init` 1409 ms, `app_core` 669 ms,
`check_security` 465 ms, `runtime_handler` 716 ms. That is a launcher doing
launcher work between two engine calls.

Cordial's equivalent stretch is short because Cordial's work is short: the
settings document is already on disk under a six-hour cache, there is no security
check, no gamemode integration and no device enumeration of that weight. The
compressed timeline in §11.7's table is Cordial being faster, not Cordial
skipping a phase. Nothing in the gap is a prerequisite the engine is waiting on,
which is also why §11.8's reordering could not work and should not have been
expected to.

One detail worth carrying, unrelated to the verdict: Sober applies its **own**
`app_settings` manifest on top of Roblox's document —
`{"app_settings":{"FStringRenderTextureBudgetByRam":""}, ...}` — so the two
clients are not running byte-identical flag sets even when they fetch the same
document. Cordial applies no such overlay. That has not been tested against the
verdict and is recorded only so nobody assumes the flag sets match exactly.

### §11.10 Sober's `app_settings` overlay — applied, no effect

§11.9 recorded that Sober applies its own `app_settings` manifest on top of
Roblox's document, so the two clients do not run identical flag sets. Tested,
because "untested" is not a place to leave something that was raised as a
difference.

Sober's manifest carries exactly one entry: `FStringRenderTextureBudgetByRam=""`.
Given to Cordial through `CORDIAL_FLAGS`, `--run 10` startup, against a control
run taken immediately afterwards on the same build:

| | overrides applied | `RbxStorage` | `onFlagsFailed` |
|---|---|---|---|
| with Sober's overlay | 1 | 0 | 2 |
| control, no overlay | 0 | 0 | 2 |

Identical. The overlay is not the difference, and the flag-set discrepancy noted
in §11.9 can be closed: it is one empty `FString` about a render texture budget
and it has nothing to do with the flags verdict or the content store.

### §11.11 A join, after all of the above: 304 at 60.6 s, unchanged

Everything from §11 onwards was startup-only. One instrumented join on the test
profile, after all of it, to answer whether any of it moved the disconnect:

    RESULT postfix server=128.116.50.33 alive=60.6s reason=304 (connections: 1)

Squarely inside the 60.1–60.9 s band recorded across twelve-plus earlier runs.
**Nothing in §11 is a fix for the 304**, which was never claimed but is now
measured rather than assumed. The client is healthy at the moment it is dropped:
`Connection lost: AckTimeout 0, IsOutgoingDataWaiting 1`.

In the same run: `bootstrapTheApp` delivered once, `onFlagsFailed` twice,
`FlagCache` wrote, and `KeyRing` logged two parsed configs — so `KeyRing`, listed
in §11.7 as a channel Cordial never reaches, is simply join-time and is reached
normally. That entry in §11.7's list is wrong and is corrected here.

The store is still not constructed, and for the first time the engine says so in
its own words rather than by absence:

    8.486503 Error [DFLog::CaptureStorage] RbxStorage is not initialized,
                                           cannot access storage interface

Twice, at 8.49 s, on the join path. There is no `rbx-storage.db` under the
profile. So the picture from §11 holds — the store never initialises — and there
is now a named consumer that wanted it and was refused, which is a better handle
than an absent log line.

A caution on how that was nearly misread: `grep -c RbxStorage` on this log
returns 2, and the obvious reading is that the store initialised on a join when
it does not on a startup. It is the opposite; both matches are the error above.
Count then read, in that order.

## §12. What is blocking `RbxStorage::init`: nothing is. It is never asked for

Chased from the `CaptureStorage` error in §11.11. The conclusion is that the
question "what is blocking storage init" has no answer because storage init is
not blocked.

**There is no way to initialise it directly.** `RbxStorage` is engine-internal.
The engine exports `LocalStorageManager_initStorageManagerNative`, `...V3`, the
`memstorage` family and the `localstorageplatforminterface` family — all of which
are *LocalStorage*, a different thing that Cordial already has working
(`appData/LocalStorage/*.json` is populated). Nothing exported, and nothing in the
dex, constructs `RbxStorage`. There is no handle to pull.

**Its only trigger is the flags-loaded event.** `FFlagStartRbxStorageInitRighAfterFlags
= True` in the live set, and Sober's own log names the trigger in the line
itself: `RbxStorage::init [INIT] user: flagLoaded`.

**The routing flags were tried and do nothing.** `FFlagRbxStorageUseStdThread`,
`RunInitInStdThreadLatch`, `BackgroundThread` and `SynchronizeInit2` all False,
plus `StartRbxStorageInitRighAfterFlags` False — seven overrides applied, zero
`RbxStorage` lines, zero `rbx-storage` paths.

**And the overrides are real, which had never been established.** Static flags
were assumed to land and were not tested. `FLogGraphics=0` takes
`[FLog::Graphics]` from **30 lines to 0** on the same build. Static and dynamic
overrides both reach the engine, so every flag experiment in §11 and here is a
genuine negative rather than a no-op. The "settings arrive too late for static
flags" theory is dead.

**The engine says nothing about it at any verbosity.** All 134 `FLog*`/`DFLog*`
keys in the document set to 7: the engine log goes from ~220 lines to **6247**,
reaching 31 distinct channels, and contains zero lines matching `RbxStorage`,
`InitBlocked`, `flagLoaded` or the verdict. `FFlagRbxStorageReportInitBlocked` is
True in the live set and never fires. Storage is not failing to initialise; it is
not being asked to.

### §12.1 `NativeFlagsInitResult` — three methods implemented and never registered

Found while chasing the above, fixed, and **not** the cause. The dex declares
five members; Cordial registered two:

    <init>                   (I)V                        registered
    addBoolean               (Ljava/lang/String;ZZ)V     registered
    getNativeFlagProviderId  ()I                         written, not registered
    getBooleanCachedMap      ()Ljava/util/Map;           written, not registered
    resolveFlagValue         (Ljava/lang/String;)Z       written, not registered

All three had working C++ bodies sitting in the class. `Register()` never hooked
them, so the object answered its constructor and `addBoolean` and returned an
unresolved stub to every question about what it had stored. **This is a third
variant of the silent-hook bug**, after the wrong descriptor and the wrong class:
an implemented method that is never registered. It does not show up in a grep for
the method name, which is why it survived the audit in 2ca7811 that was looking
for precisely this, and `tools/hook_descriptors.py` cannot see it either because
it only checks hooks that exist.

Registered now. It changes nothing, and the trace says why: across a whole
startup the engine calls `<init>` once and `addBoolean` **138 times** and never
looks up any of the three. It builds the result object, fills it, and never reads
it back.

### §12.2 Where that leaves the verdict

The flag handshake completes on Cordial's side. 138 of 139 names accepted, the
flag cache written, static and dynamic overrides demonstrably in effect. The
engine then reports `onFlagsFailed` over JNI without having asked Cordial a single
further question — no unresolved lookup, no failed call, no log line at any
verbosity. Every symbol on the path resolves and every argument is correct.

So the verdict is not a reaction to anything Cordial does or fails to do at the
JNI boundary. That is a much narrower statement than this document has been able
to make before, and it rules out the entire class of fix it has been pursuing
since §7.

## §13. mocktail does not take the 304, and the difference is visible

The experiment that had been deferred for two days. mocktail 1.0.3, Flatpak,
signed in and joined to place 17625359962 — the same place Cordial dies in.

    Joining game ... place 17625359962 at 10.60.0.203     84.048s
    Connection accepted from 128.116.51.33|61655          84.102s
    last engine timestamp                                333.512s
    Disconnect / Peer Disconnected / Connection lost            0

**249 seconds connected and still running**, against Cordial's 60.6. So the 304
is not something every third-party client gets, not a property of the engine on
Linux, and not unavoidable. A working comparison now exists.

### The startup difference, counted on the same place and the same day

| | mocktail | Cordial |
|---|---|---|
| `onFlagsFailed` | **0** | 2 |
| `RbxStorage::init` | **[INIT] 0.164s, [DONE] 1.037s** | never (only "not initialized" errors) |
| `ClientRunInfo` | **3** | 0 |
| `AppPlatformQoS` | **1** | 0 |
| 304 | **no** | at 60.6s |

This is the chain ad985a8 proposed, now with the other side of it. The client
that raises the flags-loaded event builds its content store and keeps its
connection; the client that reports `onFlagsFailed` does neither.

mocktail's order, which Cordial reproduces up to the third line and then stops:

    0.139  nativeInitClientSettings
    0.155  [FlagCache] Deferring flag cache write to post TTI
    0.158  nativePostClientSettingsLoadedInitialization3
    0.158  [ClientRunInfo] RobloxGitHash: 9141bfb7...
    0.158  [ClientRunInfo] The base url is https://www.roblox.com/
    0.158  [ClientRunInfo] The channel is production
    0.159  AppPlatformQoSEmergencyHandler was instanced
    0.164  RbxStorage::init [INIT] user: flagLoaded

**`ClientRunInfo` is the first thing Cordial does not reach.** It is the engine
stating its own run identity — git hash, base url, channel — immediately after
the post-settings call, and six milliseconds before the store is built. Cordial
makes both calls, gets 0 and "ok" from them, and produces none of these lines.

### Two things mocktail does that Cordial does not do at all

Neither is established as the cause. Both are recorded because they are
differences between a client that works and one that does not, which is a much
better position than this document has been in.

**It presents as a PC, not a phone.** `src/runtime/device_profile.cc` reports
`device profile=pc-windows-11 class=pc model="Windows 11 PC"`. Cordial tells the
engine it is an Android tablet — 6d8c280 built a User-Agent saying exactly that,
on the reasoning that the real Android client sends it. mocktail's choice is the
opposite one and mocktail is the client that survives.

**It bootstraps a tracker identity before the engine starts.**
`src/services/browser_tracker_service.cc` calls
`https://apis.roblox.com/browser-tracker-api/device/initialize?suggestedBrowserTrackerId=`
and keeps the `RBXEventTrackerV2` cookie it returns. Cordial has no equivalent
and no such cookie.

This was already half-written down here and never connected to anything:
`docs/analysis/webview-surface.md` records that `libroblox.so` carries the string
`BrowserTrackerIdRequest: No RBXEventTrackerV2 in cookie.`, and
`docs/design/sign-in.md` has the same endpoint in an Android capture. So the
engine has a code path that notices this cookie missing, and Cordial has never
supplied it.

**Not evidence, and worth stating so nobody quotes it as such:** that log string
did **not** appear in Cordial's join log, and the only BrowserTracker line in
mocktail's log is mocktail's own launcher rather than the engine. Neither engine
said anything about a tracker cookie at default verbosity. The string's presence
in the binary shows the check exists; it does not show it fired.

### What to do next, in order

1. **Raise the log level and rerun the Cordial join.** §12 established that flag
   overrides reach the engine, so the channels around `ClientRunInfo`,
   `BrowserTrackerIdRequest` and `AppPlatformQoS` can be turned up. Find out
   whether the engine says anything about the tracker cookie when it is missing.
2. **Supply `RBXEventTrackerV2`.** Cordial owns the cookie jar already
   (`crates/cordial-runtime/src/cookies.rs`) and the endpoint is a documented,
   ordinary HTTPS request the real client makes. Nothing here forges or replays
   anything.
3. **Try the PC device profile.** Cheap, reversible, and the client that works
   uses it.

Take them one at a time with a control each. Three changes at once against a
failure that takes sixty seconds to reproduce would tell us nothing.

### §13.1 The tracker cookie comes through the WebView, not from an API call

Tried, and it does not work the way §13 assumed.

`browser-tracker-api/device/initialize` refuses a plain request. `GET` answers
**404**, which is a route that does not exist. `POST` with mocktail's own headers
— `Accept: application/json`, `Content-Type: application/json`, body `{}` —
answers **500** with an empty body, unchanged whether the User-Agent says
`Mocktail/0.1` or `Cordial/0.5`. So the method is right and something else about
the request is not.

`docs/design/sign-in.md` already recorded why, from a logged-out Android capture:

    Flushed WebViewCookieHandler with Cookies from URL
      https://apis.roblox.com/browser-tracker-api/device/initialize
    OnSetCookieHandlerImpl.b(): Updated WebViewCookieHandler with Cookies from URL
      https://apis.roblox.com/browser-tracker-api/device/initialize

**`WebViewCookieHandler`.** The real client obtains this cookie inside its web
view, during the logged-out sign-in flow, with a browser's full context behind
the request. It is not a standalone API call, and mocktail's succeeds because
mocktail signs in through its web view first.

That joins two gaps that were being tracked separately:

    no WebView -> no sign-in in a browser context -> no RBXEventTrackerV2
                                                  -> no device identity

Which raises the WebView from "account settings and Robux do not work" to a
possible prerequisite for the session surviving at all. **Still `INFERRED` —
nothing here shows the missing cookie causes the 304.** But the ordering is now
the right way round to test: build the web window, sign in through it, and see
whether the cookie arrives on its own.

`crates/cordial-runtime/src/browser_tracker.rs` is written and tested and
**deliberately not called from anywhere**. Wiring in a request that returns 500
would put a line in the log saying Cordial had done something it had not.

## §14. The 304 no longer reproduces, and the device profile is not why

Four join runs on 2026-08-18, place 17625359962, profile CordialTest, 90 seconds
each:

| run | identity | result |
|---|---|---|
| pc | `CORDIAL_DEVICE_PROFILE=pc-windows-11` | connected 1.77s, last log 90.70s, **0 disconnects** |
| control-1 | default android-tablet | still connected at exit |
| control-2 | default android-tablet | still connected at exit |
| control-3 | default android-tablet | still connected at exit |

Every one survived. Against twelve-plus runs that died at 60.1–60.9s with reason
304, the disconnect is gone.

**The control is the point.** The PC identity run came first and looked like the
answer; three runs with the identity left at its Android default did exactly the
same thing. So `CORDIAL_DEVICE_PROFILE` is not what changed, and crediting it
would have been the easiest wrong conclusion available today.

### What actually changed is not established

Two candidates, and nothing here separates them:

**Roblox shipped a new engine.** Every 304 was measured on 2.730.0.790. The APK
updated mid-session and all four of these runs are 2.734.0.917. A server-side
disconnect that stops happening across a client update is exactly what a
server-side change looks like from here.

**Or one of the day's changes.** `hypotf` (which the new build needs to load at
all), the texture-manager default, the webview subscription, the MessageBus
generalisation, the refresh-rate calls, the plugin host work.

The engine update is the stronger candidate on timing alone, and it is also the
one Cordial cannot take credit for. **Nothing in this document should be read as
"the 304 was fixed".** It is not reproducing, on this build, on this day, on four
runs.

### What did not change

    RbxStorage::init=0   ClientRunInfo=0   onFlagsFailed=2   webview-open=0

Identical in all four. So the chain §13 established is untouched: the engine
still reports `onFlagsFailed`, still never builds its content store, still never
prints `ClientRunInfo`. Whatever stopped the disconnect did not fix that, and
§13's correlation between the flags verdict and the 304 is weakened by exactly
this — mocktail had the storage *and* stayed connected; Cordial now stays
connected *without* it.

The honest reading is that the two were never as tightly coupled as the
correlation suggested, and that the storage chain is worth pursuing on its own
merits — a client with no content store re-fetches every asset, which is a real
cost whether or not it ever caused a disconnect.

### Before this is quoted anywhere

Run it again on a different day, and if the 304 returns, the engine update was a
reprieve rather than a fix. `tools/join-run.sh` exists so that is one command.

## §15. Two answers read out of mocktail's source, one of which kills a theory

### The empty `ArrayList` was never the problem

`flag-init.md` §7.4 has recorded since it was written that Cordial passes
`nativePostClientSettingsLoadedInitialization3` an empty `java.util.ArrayList`
and that this is unresolved — the implication being that a real list is what the
engine wants and that the empty one is why nothing follows.

mocktail's `BuildApplicationExitInfoList`, which is what it passes to that same
native:

    jobject BuildApplicationExitInfoList(JNIEnv* env) {
      jobject list = NewObject(env, "java/util/ArrayList");
      if (!list) list = NewObject(env, "java/util/List");
      return list;
    }

**An empty `ArrayList`.** The same thing Cordial passes. And mocktail reaches
`RbxStorage::init [INIT] user: flagLoaded` 6 ms later.

So the argument is not the difference, and §7.4's open question is closed as a
dead end rather than left to be re-investigated. The list's real element type is
`ApplicationExitInfo` — Android's historical process exit reasons — and the
engine evidently does not require any.

### `channelPlatformName` was carrying the wrong string

Cordial passed `AndroidApp` to
`NativeSettingsInterface.nativeOverrideChannelPlatformName`. mocktail passes
`GoogleAndroidApp`. Counted rather than argued, with exact whole-string matches:

| literal | in `libroblox.so` | in the dex |
|---|---|---|
| `AndroidApp` | 3 | 0 |
| `GoogleAndroidApp` | 0 | 2 |

Two strings doing two jobs. `AndroidApp` is the application name in the settings
URL — `v2/settings/application/AndroidApp` serves the real document and the other
spellings return HTTP 400, which is established by experiment and still true.
`GoogleAndroidApp` is what the *application* calls its channel platform, lives in
the dex where the Java side is, and is what that native wants. The two were
conflated and this call had the other's value.

Corrected. **It changes nothing measurable**, which is recorded here so nobody
tries it again expecting more:

    RESULT gplat alive=still connected reason=none
      RbxStorage::init=0 ClientRunInfo=0 onFlagsFailed=2 webview-open=0

Identical to the four runs in §14. A correctness fix with a null result is still
worth having — the value is now the one the dex declares rather than one that
appears nowhere as a channel platform name — but it is not the storage fix.

### Where that leaves the storage question

Both of the concrete differences visible in mocktail's startup path have now been
tried, and neither moves `onFlagsFailed`. The verdict is still reached inside
`initializeNativeCode`, before the settings calls, exactly as §12 measured. What
mocktail does differently to reach `flagLoaded` is still not identified, and it
is not either of these.

## §16. mocktail's startup path, read call by call: the gap is not a missing call

The question was why mocktail has no storage gap. Its whole pre-settings path
has now been read, and the answer is not in the shape this investigation assumed.

**Between `initializeNativeCode` and the settings call, mocktail makes five
calls.** Cordial makes four of them:

| call | Cordial |
|---|---|
| `nativeSetAssetPath` | yes |
| NativeSettings directories | yes |
| `JNIBaseUrlProtocol.init` | yes |
| `nativeGameGlobalInit` | yes |
| `nativeUpdateScreenOrientation` | **no** |

One real gap, and it is a screen-orientation notification rather than anything
that plausibly gates a content store. Worth closing on its own; not this.

**And Cordial calls far more of the engine than mocktail does.** Counting the
`Java_*` symbols each names: mocktail 17, Cordial 54. So the storage gap cannot
be explained by Cordial failing to make a call mocktail makes — the set is the
other way round.

That inverts the hypothesis worth testing next: not a missing call but an
**extra** one, something Cordial does that provokes the verdict. The obvious
candidate was Cordial's early `nativeInitClientSettings`, made before
`initializeNativeCode`, which mocktail does not do — added when the first
`flags FAILED` was seen arriving before any settings call.

**That candidate is dead too.** The call is gated behind `CORDIAL_NO_BOOTSTRAP`
and does not fire in a default run at all; a default run's log contains no
`early client settings` line. So it is not a difference between the two clients
in normal operation, and an earlier reading of that line in this document came
from a traced run with that variable set.

### What has been ruled out, so nobody re-runs it

- the empty `ArrayList` to `nativePostClientSettingsLoadedInitialization3`
  (§15 — mocktail passes an empty one too)
- `channelPlatformName` (§15 — corrected to the dex's value, no effect)
- the device identity (§14 — three controls)
- every flag routing `RbxStorage`'s construction (§12)
- the settings document, in four variants (§7, §11)
- a missing call in mocktail's pre-settings path (this section)
- Cordial's early settings call (this section — it does not run)

**The honest state: the difference is still unidentified.** It is not any of the
things that were visible from the outside, and reading mocktail's source has
removed candidates rather than supplied the answer. That is progress of the
cheaper kind, and it is worth having written down before somebody spends another
day re-testing the same seven things.

## §17. The premise behind Cordial's bootstrap was wrong, and §11.8's crash is the real lead

mocktail's pre-`initializeNativeCode` stretch has now been read — the last part of
its startup nobody had looked at. It does not contain the answer §16 was hoping
for. It contains something more useful: evidence that a theory this project built
on is not true.

### `bootstrapTheApp` does not have to deliver anything

mocktail's implementation, `src/jnivm/jnivm.cc:1702`, in full:

    if (std::strcmp(name, "bootstrapTheApp") == 0) {
      SetBooleanFieldRaw(obj, "bootstrapStarted", JNI_TRUE);
      ... log ...
      return;
    }

**One boolean. Nothing else.** And mocktail reaches
`RbxStorage::init [INIT] user: flagLoaded` regardless.

Cordial's `run_bootstrap` exists on the theory that an unresolved
`bootstrapTheApp` causes an immediate `onFlagsFailed`. §7 established that for the
*unresolved-symbol* case, which is real. What is now clear is that the converse
does not follow: a resolved-but-empty `bootstrapTheApp` does not reproduce the
failure for mocktail, so "deliver enough through bootstrap" was never the shape of
the fix.

### Which puts §11.8's abandoned crash back at the front

mocktail delivers client settings **after `initializeNativeCode` returns**,
sequentially, the ordinary way. Cordial tried exactly that — `CORDIAL_LATE_SETTINGS=1`
— and **crashed twice out of two**, which is why it delivers early instead. §11.8
records the crash and says plainly it was never root-caused.

So the open question is not "what does mocktail do before `initializeNativeCode`".
It is **why does Cordial segfault doing what mocktail does after it**. The client
that works runs through the ordering that kills ours, and that crash was set aside
rather than understood.

**And the crash is worth re-testing before anything else.** It was recorded on
2.730.0.790. The engine is now 2.734.0.917 — a build that also needed `hypotf`
before it would load at all. A crash on an engine two versions old is not evidence
about this one.

    CORDIAL_LATE_SETTINGS=1 tools/join-run.sh late

That is the next experiment, and it is one command.

### Also untried, cheaper, lower odds

Cordial's `Configuration` object is registered and populated with nothing
(`native/game_activity.cpp:128`). It is handed to the same `initializeNativeCode`
call whose next line decides the verdict. mocktail's `CreateAndroidConfiguration`
fills fifteen fields — orientation, touchscreen, keyboard, densityDpi, screen
dimensions, layout, uiMode, colorMode, mcc/mnc, navigation, fontWeightAdjustment.

Never examined; not among §16's seven. **Plausibility honestly moderate-to-low** —
AGDK usually derives its internal `AConfiguration` from the `AssetManager` rather
than reading this object's fields, so it may be inert. Cheap to try, and if it
changes nothing, populate only `orientation` and `touchscreen` rather than
restoring a whole struct of guesses.

### Closed

BrowserTracker is non-fatal in mocktail too — `src/main.cc:711` logs the failure
and continues. It structurally cannot be a hard gate on either side, which
confirms §13.1's own caution and settles that it should stay unwired while the
endpoint returns 500.

### Recorded as better, so nobody "fixes" it

Cordial's `AssetManager`, `Configuration` and window-insets classes are
deliberately stateless with a documented reason each. mocktail populates its Java
objects with synthetic hardware descriptions it cannot verify — `mcc`/`mnc` zero,
`colorMode` zero, `navigation` one, guesses throughout. Keep the minimalism unless
the experiment above proves a specific field load-bearing.

## §18. `__sF` is unfinished, and the signature matches a crash already on record

A comparison of Cordial's bionic/glibc boundary against mocktail's found three
real ABI defects. One of them may be a crash this document has been carrying
since §7.4.

### The three, all verified against the engine's own imports

**`__sF` — the legacy `stdin`/`stdout`/`stderr` array.** `bionic/mod.rs:97`
supplies a zeroed three-element array and its own comment already says this stops
a load-time crash and "does not make the legacy streams work". What it does not
say is what happens next. The engine imports **ten** FILE-taking stdio
functions — `fflush`, `fwrite`, `fread`, `fclose`, `fprintf`, `fputs`, `fseek`,
`ftell`, `setvbuf`, `vfprintf` — and every one is unoverridden passthrough to
host glibc. So a `FILE*` the engine computes as `&__sF[1]` reaches glibc's
`fwrite` or `fflush` and is dereferenced as a real `FILE` against **zeroed
memory**. That is not "no output"; it is a fault at a small offset.

**`mallinfo` — a 40-byte struct filled into an 80-byte expectation.** bionic's
`struct mallinfo` is ten `size_t`; this host's glibc is ten `int`, confirmed by
compiling `sizeof(struct mallinfo)` here and getting **40**. `mallinfo` is
imported by the engine and passes straight through, so the callee writes 40 bytes
of int-strided fields and the caller reads them at 8-byte strides. Every field
after the first is misaligned, and the upper half is never written at all.

**`__cxa_thread_atexit_impl` — a stub that reports success.** Imported by the
engine, no override, so it falls to the generated stub and returns 0. Every
bionic-compiled `thread_local` with a non-trivial destructor is registered
nowhere and never torn down. This is precisely the shape AGENTS.md singles out
`__assert2` for: an answer that is not true, and a failure that surfaces
somewhere unrelated.

### The connection worth testing first

§7.4 records `nativePostClientSettingsLoadedInitialization3` crashing
**synchronously, under lldb, with `SIGSEGV` at fault address `0x8`, inside
`libc.so.6` `_IO_fflush`**.

A zeroed `FILE` handed to `_IO_fflush`, faulting on a pointer field a few bytes
in, produces exactly that. `fflush` is one of the ten the engine imports.

**This is a signature match, not a demonstration.** Nothing here has reproduced
the crash with the `__sF` gap closed and watched it go away, and that is the only
thing that would settle it. But it is a specific, mechanical account of a crash
that has been described as unexplained for weeks, and it costs one experiment.

It matters beyond §7.4 because §17 identified §11.8's `CORDIAL_LATE_SETTINGS`
crash — Cordial segfaulting while doing what mocktail does successfully — as the
strongest remaining lead on the flags verdict. **If both crashes are this bug,
the ordering mocktail uses becomes available to Cordial**, and the thing §17 said
to investigate becomes the thing to fix.

### Order of work

1. Translate the legacy streams. mocktail's `bionic_stdio_runtime.cc` checks each
   FILE-taking entry point and redirects the three `__sF` slots to real
   `stdin`/`stdout`/`stderr`. Roughly 150–200 lines of the same shape here, and
   only for the entry points the engine actually imports.
2. Re-run `nativePostClientSettingsLoadedInitialization3` and see whether the
   `_IO_fflush` crash is gone. That is the test of the whole theory.
3. Then `CORDIAL_LATE_SETTINGS=1`, which §17 wants re-tested on 2.734.0.917
   anyway.
4. `mallinfo` and `__cxa_thread_atexit_impl` independently — each is small,
   neither depends on the above.

### And one thing not to change

`native/netdb_compat.cpp` translates bionic's `AI_*` bits before calling host
`getaddrinfo`, because bionic's `AI_DEFAULT` is `0x600` and handing that to glibc
returns `EAI_BADFLAGS` and fails every lookup. mocktail's `HostHints()` copies
`ai_flags` **unmodified** — the same latent bug, not yet triggered by whatever
its callers pass. Cordial is right here and mocktail is not; nobody should
simplify that file to match theirs.

### §18.1 The same crash, now from a third unrelated call

§18 proposed that `__sF`'s zeroed `FILE` array explains §7.4's `SIGSEGV` at
fault address `0x8` inside `libc.so.6` `_IO_fflush`, and called it a signature
match rather than a demonstration.

Wiring `ILocalStorageHandlerCore.setPlatformImpl` produced **the same crash
again** — same function, same fault address — from a call that has nothing to do
with client settings. Controlled both ways on this machine:

    gate off  exit 0    reaches app ready: Landing
    gate on   exit 139  SIGSEGV

That is three independent paths now: `nativePostClientSettingsLoadedInitialization3`
(§7.4), `CORDIAL_LATE_SETTINGS` (§11.8), and `setPlatformImpl`. Three unrelated
natives cannot share a fault address by coincidence, and the `__sF` gap predicts
exactly this shape for any engine path that touches a legacy stream.

**It is still not demonstrated.** The demonstration is closing the gap and
watching all three stop, and nobody has done that. But §18's order of work now
has three reproducers to test against instead of one, and the cheapest of them is
a one-variable environment flip rather than a join.

One thing seen alongside and *not* explained: with the gate on, the engine's own
djinni glue throws `djinni_support.cpp:529: weakRef` a dozen times before the
segfault. `IPlatformLocalStorageHandler` is djinni-generated — the `$CppProxy`
siblings give it away — so djinni plausibly wants working weak global references
that libjnivm does not provide. **`INFERRED` from the exception name and timing.**
Confirming it would mean reading the engine's own implementation, which is the
line AGENTS.md draws, so it stays inferred. Whether the weak refs and the
`_IO_fflush` fault are one problem or two is open.

## §19. The streams are translated, and the engine finally says what is wrong

`native/legacy_stdio.cpp` maps the three `__sF` slots onto the host's real
`stdin`/`stdout`/`stderr` and routes the ten FILE-taking functions the engine
imports through the translation. In C++ rather than Rust because `fprintf` is
variadic, Rust cannot define a variadic `extern "C"`, and AGENTS.md records that
this project's one previous attempt at wrapping variadics unsafely aborts the
engine.

### All three reproducers changed, and none of them segfaults

| reproducer | before | after |
|---|---|---|
| `setPlatformImpl` | 139, `SIGSEGV` in `_IO_fflush` | 133, `SIGTRAP` |
| `CORDIAL_LATE_SETTINGS` | crashed 2/2, never root-caused | 133, `SIGTRAP` |

§18 predicted the fault and §18.1 found a third instance of it. The prediction
holds: with the legacy streams translated, **no reproducer produces a `SIGSEGV`
at `_IO_fflush` any more**. What each produces instead is a named error, which is
the whole point.

### And the named error is the answer this document has been looking for

`CORDIAL_LATE_SETTINGS=1` now ends:

    RBXCRASH: FatalRuntimeError
      (Can't initialize the TaskScheduler before flags have been loaded)

**The engine has been saying this all along and it was arriving as a memory
fault.** For weeks this was "Cordial crashes when it uses mocktail's ordering,
cause unknown" — §11.8 set it aside on exactly that basis. It is not unknown. The
TaskScheduler is initialised before flags are loaded, and the engine treats that
as fatal.

That reframes the whole storage question. §12 established the flags verdict is
reached inside `initializeNativeCode` before any settings call, and could not say
why. This says why: something on that path brings the TaskScheduler up first, and
the engine's flag machinery will not run behind it.

`setPlatformImpl` ends differently — thirteen
`djinni (djinni_support.cpp:529): weakRef` exceptions and then
`RBXCRASH: JNI: Crashing due to unhandled Java exception`. A separate problem,
now visible as one: djinni wants working weak global references and libjnivm does
not provide them. Still `INFERRED` as to cause, but it is a reported Java
exception rather than a corrupted heap, and it can be worked on.

### What this cost and what it bought

Ten wrapper functions and an address exported from Rust so the constant is not
restated. No behaviour change on the default path — a pointer that is not one of
the three legacy slots is passed through untouched, because a pointer this code
does not recognise is not its to interpret.

What it bought is that two failures which presented as heap corruption now
present as sentences. AGENTS.md's rule is that a stub which reports success is
worse than one that reports failure; the same holds a level down. A crash that
names its cause is worth more than one that does not, and this project spent
weeks on one that did not.

### Next

The TaskScheduler line is the thread to pull. It is the first statement from the
engine itself about *why* flags are not loaded, as opposed to *that* they are
not, and everything in §§12–17 was working without it.

### §19.1 What the TaskScheduler line does and does not explain

**Superseded in part by §22.** The conclusion below that "the gate is
satisfied on the path Cordial actually uses" is wrong: it reads the absence of
the fatal error as the gate being passed, when it is the gate never being
reached. Kept because the narrowing it does to the late-settings ordering holds.

Chased, and the scope is narrower than §19 implied. Worth pinning before anyone
builds on it.

**In a working default run the TaskScheduler is fine.** The only mention in the
engine log of a 90-second join is

    [FLog::AndroidGLView] rbx.datamodel: setTaskSchedulerBackgroundMode()
                          enable:false context:ASMA.start

which is background mode, not initialisation, and no error accompanies it. So the
gate is satisfied on the path Cordial actually uses.

`Can't initialize the TaskScheduler before flags have been loaded` therefore
explains **why the late-settings ordering cannot work**, and nothing more. It is
not an account of `onFlagsFailed`, which still fires twice on the default path
where the scheduler comes up cleanly.

That is still worth having. §17 named §11.8's crash as the strongest remaining
lead precisely because mocktail succeeds with an ordering that killed Cordial,
and the answer turns out to be that the ordering is unavailable rather than
mysterious: something on Cordial's `initializeNativeCode` path brings the
scheduler up, and the engine will not load flags behind it. Adopting mocktail's
ordering would mean also deferring whatever does that, which is a different and
larger change than reordering two calls.

**So the lead is narrowed, not closed.** `onFlagsFailed` on the default path
remains unexplained after eight eliminated candidates, and the honest position is
that §19's framing — "the answer this document has been looking for" — was one
step ahead of the evidence. The engine named the cause of a crash, not the cause
of the verdict.

## §20. A 255-second leg of real gameplay, and the texture flag confirmed

Reported from actual play rather than a harness: Doors, to room 15, stopped
because the player got bored. Numbers from that session's engine log,
`2.734.0.917_20260819T041725Z`:

    connections                     3   (Doors teleports lobby -> run)
    last engine timestamp     381.3s
    last Connection accepted  126.1s
    disconnect events               1

The single disconnect is at 23.4s and is `connectMode: Disconnect ASAP`,
`AckTimeout 0, IsOutgoingDataWaiting 0` — a client-initiated teleport out of the
lobby, not a server drop. **The final leg ran from 126.1s to 381.3s: 255 seconds
with no disconnect at all.**

Against a 60.6s death that reproduced twelve-plus times, and against §14's four
90-second harness runs, this is the first long session and it is four times the
window those covered. The 304 is not merely failing to reproduce inside 90
seconds; it does not reproduce across a real play session either.

That does not change §14's conclusion about *why*. Roblox shipped 2.734.0.917
mid-session and every 304 was measured on 2.730.0.790; the engine update remains
the stronger candidate and Cordial still cannot claim the fix. What this adds is
that the reprieve is not an artefact of short runs.

### And the texture flag is doing something

    14.930483 [FLog::Graphics] Using TM1
    14.930499 [FLog::Graphics] Warning: Using TexturePackGenerator.

**TM1 is TextureManager 1** — the legacy path. `FStringGraphicsTextureManager2DenyPattern2
= ".*"`, shipped as a built-in default, denies every pattern in TextureManager2,
and the engine has fallen back exactly as predicted. That entry was marked
`INFERRED` on the grounds that the flag's absence from Roblox's document and its
effect on mocktail were established while the mechanism was not.

The mechanism is now observed. The engine says which texture manager it chose,
and it chose the one the flag leaves available. Whether the resulting textures
look better is still a judgement nobody has made side by side — but "the flag
reaches the engine and changes which manager runs" is no longer inferred.

## §21. Settings before `initializeNativeCode`, with the bootstrap intact: no change

The early `nativeInitClientSettings` call was added because the first
`flags FAILED` was seen arriving before settings had been delivered at all — the
answer arriving after the question it was meant to inform. It was then wired
behind `CORDIAL_NO_BOOTSTRAP`, so "settings early" and "no bootstrap" have only
ever been true together and the useful half was never tested alone.

`CORDIAL_EARLY_SETTINGS=1` decouples them. Controlled on the same build:

    baseline                   early=0  onFlagsFailed=2  RbxStorage=0  ClientRunInfo=0
    CORDIAL_EARLY_SETTINGS=1   early=1  onFlagsFailed=2  RbxStorage=0  ClientRunInfo=0

The call fires and nothing moves. **Candidate ten, eliminated.**

That one mattered more than the others because §12 measured the verdict being
reached inside `initializeNativeCode` before any settings call, which made "the
engine wants its flags already present when that runs" the obvious reading. It is
wrong: the flags can be present and the verdict is the same.

The switch stays, off by default, because it is now the only way to vary that
ordering without also changing the bootstrap and it will be wanted again.

### The eliminated list, in one place

Ten now. The empty `ArrayList`; `channelPlatformName`; the device identity; every
flag routing `RbxStorage`'s construction; four settings-document variants; a
missing call in mocktail's pre-settings path; Cordial's early settings call as
originally wired; the `Configuration` object being empty; delivering settings
before `initializeNativeCode` with the bootstrap intact; and — from §17 —
`bootstrapTheApp` needing to deliver anything, which mocktail disproves by
reaching `flagLoaded` with a one-line no-op.

### What is left that has not been tried

Two things, and both are larger than a flag.

**The `_IO_fflush` crashes are fixed but their consequences are not explored.**
§19 turned three segfaults into named errors, and one of them said
`Can't initialize the TaskScheduler before flags have been loaded`. §19.1 pinned
that to the late-settings ordering only. Nobody has yet asked what *does* bring
the scheduler up on the default path, or whether the flags machinery runs behind
it there too, quietly, without the fatal error.

**Loud logging is exhausted as a technique.** *(Retracted in §22: the sweep
drew its 135 names from a 30-line list of channels seen on Android, out of 724 in
the binary, and setting a channel in `flags.json` is shown there to silence it.
`FLog::NativeDM` was never in the list and has been printing the answer all
along.)* 135 channels at maximum produce
5961 lines and not one about the verdict — before or after the stdio fix, which
was the last hope that the engine had been trying to tell us and could not. It
had not. Whatever decides this does not log.

## §22. The engine has been naming the state every run, on a channel nobody read

§21 closed with "loud logging is exhausted as a technique". That was wrong, and
the way it was wrong is worth stating before the finding itself.

The 135-channel sweep drew its channel names from `docs/traces/flog-channels.txt`,
which is 30 lines — the channels that happened to appear in the Waydroid capture.
`libroblox.so` defines **724**. `FLog::NativeDM` is not in the 30, was never
enabled, never grepped for, and has been printing twelve lines in every run this
project has ever made.

### What it says

From a plain `--run 12` with no overrides at all:

    [FLog::NativeDM] nativeActivity_onStart:
    [FLog::NativeDM] nativeActivity_onResume:
    [FLog::NativeDM] dataModelBindings_onGameLoaded: placeId = 0.
    [FLog::NativeDM] nativeActivity_onSurfaceChanged: state:11.
    [FLog::NativeDM] nativeActivity_onSurfaceChanged: ... Flags-Not-Received. Return.
    [FLog::NativeDM] nativeActivity_onSurfaceChanged: state:11.
    [FLog::NativeDM] nativeActivity_onSurfaceChanged: ... Flags-Not-Received. Return.
    [FLog::NativeDM] nativeActivity_onKillSurface: state:11.
    [FLog::NativeDM] nativeActivity_onKillSurface: ... Flags-Not-Received. Return.
    [FLog::NativeDM] nativeActivity_onStop:
    [FLog::NativeDM] nativeActivity_onDestroyed:
    [FLog::NativeDM] nativeActivity_onDestroyed: ... Flags-Not-Received. Return.

`NativeDM` is `RBX::NativeDataModelManager` — the class `init_params.cpp` already
names as the writer of `onFlagsFailed`, from `getFlagsFromEngine_`'s completion
lambda. **`Flags-Not-Received` is its own word for the state Cordial is stuck
in**, and every lifecycle callback the engine delivers turns round and returns on
it. That is the mechanism by which nothing downstream happens: not a missing
call, a latched state.

Absent from the log, and they are the rest of that class's vocabulary:
`[Constructor]`, `initialize: state:{}. areFlagsLoaded:{}.`, `getFlagsFromEngine_:`,
`continueAfterFlagsLoaded_:`, `initEngine_:`.

### §19.1 was wrong about the TaskScheduler and this is the retraction

§19.1 concluded "in a working default run the TaskScheduler is fine … the gate is
satisfied on the path Cordial actually uses", reasoning from the absence of the
fatal error. Absence of the error is not evidence the gate was passed; it is
equally what never reaching the gate looks like, and that is the case here.
`RbxStorage::init` and `ClientRunInfo` are absent from every Cordial run, and both
sit downstream of it.

The one scheduler line Cordial does produce —
`setTaskSchedulerBackgroundMode() enable:false context:ASMA.start` — lands at
**0.480s**, while the same line on real Android lands at 0.417s *after*
`RbxStorage::init`. It is background mode rather than initialisation either way,
so it is not itself the fatal path, but it is not the clean bill of health §19.1
read it as.

### The signature theory, killed by a control before it cost anything

Cordial's flag cache write logs `Wrote signatureSize: 0`, and the engine exports
`nativeInitClientSettingsSigned(String, String, String, String)I` alongside the
plain three-argument form Cordial calls. That is a tidy story and it is false:
**Sober logs `Wrote signatureSize: 0` as well**, on the run at
`2.734.0.917_20260819T003213Z`, and Sober reaches `flagLoaded`. Nothing is gated
on a signature Cordial is failing to supply.

### The flags themselves are fine, and this is now measured rather than argued

Same run, no overrides:

    [FlagCache] writeFlagCache: Compressing flag cache data (input size: 1270529 bytes)
    [FlagCache] writeFlagCache: Compression complete. Output size: 328040 bytes, ratio: 3.87x
    [FlagCache] writeFlagCache: Successfully wrote 328045 bytes
    [FLog::TombstoneCache] Tombstone 1, expiry time 360, holdout false, channel 'production', written

1.27 MB of flag data parsed, zstd-compressed and persisted, against Sober's
1301322 → 333489 on the same day. The data loads. The *event* does not fire.
`init_params.cpp`'s comment — "the flag data did load" — is confirmed, and the
question is narrowed to the notification rather than the payload.

### Overriding an `FLog` channel can silence it

Set out here because it invalidates the sweep §21 rested on, and because it will
mislead the next person the same way:

| `flags.json` | `NativeDM` lines |
|---|---|
| absent | 12 |
| `{"DFLogNativeDM": 7}` | 12 (wrong prefix, no effect) |
| `{"FLogNativeDM": 1}` | **0** |
| `{"FLogNativeDM": "100"}` | **0** |
| `{"FLogGraphics": 7}` | 12 — an unrelated override changes nothing |

Controls on both sides: no-override runs before and after read 12. So naming a
channel in `flags.json` set it to *quiet* at every value tried, including 100,
while `FLogAppShellReporter: 7` took that channel 0 → 14 on the same mechanism.
Whatever the semantics are, **"set 135 channels to maximum" is not verified to
have raised anything and may have lowered some of them.** §21's conclusion that
loud logging is exhausted does not follow from that sweep.

### mocktail does not solve this. It patches the byte

The comparison this project has run against mocktail for weeks assumed mocktail
reaches the flags-loaded state legitimately and Cordial fails to. It does not.
`src/legacy/legacy_runtime.cc:13354` (Apache-2.0, read directly):

```cpp
bool ForceNativeFlagsLoadedForTaskScheduler(const char* reason) {
  ...
  auto* flag = reinterpret_cast<unsigned char*>(
      g_libroblox_base + kRobloxNativeFlagsLoadedByteOffset);
  ...
  if (!EnsureWritablePage(flag)) { ... }
  const unsigned int old_value = *flag;
  *flag = 1;
```

`mprotect` the page, write `1` to a fixed offset inside `libroblox.so`, and the
gate opens. It is on by default —
`SetEnvDefault("MOCKTAIL_PATCH_NATIVE_FLAGS_LOADED", "1")` at line 2818 — and it
is not the only one; `MOCKTAIL_PATCH_STAGE6_START_LUA_DM_FORCE_SAME_THREAD` and
`ForceStage6DataModelPatcherForceLocalFlag` sit beside it. The function is named
for our fatal error.

**Cordial cannot do this and will not.** In-process memory patching is exactly
what [ADR-001](../adr/ADR-001-in-process-hooking.md) and
[ADR-003](../adr/ADR-003-plugin-isolation.md) make *absent* rather than disabled,
so that no fork can extract the primitive. That decision stands; this is the
first time it has had a visible cost, and the cost is that the one comparable
implementation's answer is off the table.

What that changes: every "mocktail gets further, find the call we are missing"
inference in §§13–17 was chasing a difference that does not exist at the call
level. mocktail is not further along this path. It is past it by force.

### Sober, however, reaches `flagLoaded` and how is not established

Sober's log for 2026-08-19, at the same landmarks:

    3.001397 [FLog::AndroidGLView] nativeInitClientSettings
    3.064486 [DFLog::FlagCache] Deferring flag cache write to post TTI
    3.067240 [FLog::AndroidGLView] nativePostClientSettingsLoadedInitialization3
    3.067323 [FLog::ClientRunInfo] RobloxGitHash / base url / channel
    3.072664 [DFLog::AppPlatformQoSEmergency] instanced
    3.075039 [DFLog::Mimalloc] ...
    3.082697 [FLog::TombstoneCache] Tombstone 1 ... read from file
    3.091522 [DFLog::RbxStorage] RbxStorage::init [INIT] user: flagLoaded
    3.093661 [FLog::JNIAppBridge] nativeAppBridgeAppStart:

Cordial has none of the block between `nativePostClientSettingsLoadedInitialization3`
and `RbxStorage::init`, and its `nativeAppBridgeV2Init` is the **first** line in
its log at 0.228s where Sober's app bridge starts at 3.093s, after the storage is
up. Sober also *reads* an existing tombstone where Cordial writes a fresh one.

Whether Sober patches memory to get there is **not established** — the reference
tree here (`~/Projects/sober-oss-reference`) contains only `libbadcpu`, and its
`decompiled/` directory is off-limits under AGENTS.md. So "Sober does it
legitimately" is *not* a claim this section makes. What is observed is only that
Sober reaches the state and Cordial does not.

### Where this leaves it

Not fixed. What is now established rather than guessed:

* The stuck state has a name, `Flags-Not-Received`, and it is latched — the
  engine re-checks and re-returns on every lifecycle callback.
* The flag data is not the problem: 1.27 MB loads, compresses and persists.
* No signature is required; the control kills that.
* `getFlagsFromEngine_`'s completion chooses failure with all of that in place,
  and `initialize:` / `continueAfterFlagsLoaded_` never log.
* The only known implementation past the gate forces it with a memory write
  Cordial has permanently ruled out.

The next experiment is the one this section could not run: get
`getFlagsFromEngine_:` and `initialize: state:{}. areFlagsLoaded:{}.` to print.
They are the two lines that would say what the engine thinks the state is at the
moment it decides, and the channel is already open by default — it is the
verbosity of those particular lines that is not. Raising it through `flags.json`
demonstrably does the opposite, so that route needs understanding first.

### §22.1 Four more eliminations, and the shape they make

All measured on the same build with controls, after §22.

**Fourteen: the engine's own compressed flag cache, handed back.** The engine
writes `flag_cache.dat` — 365074 bytes here — and exports three settings natives
besides the plain three-string form Cordial has always used. Cordial had never
handed one back, so every launch looked cold to the engine with the cache sitting
on disk beside it. `nativeInitClientSettingsCachedCompressed([B, String, String,
String, long, boolean)I` now takes it:

    365074 bytes, [||],                     when 1787121646510, flag true  -> 3
    365074 bytes, [||],                     when 1787121696442, flag false -> 2
    365074 bytes, [||],                     when 0,             flag true  -> 3
    365074 bytes, [AndroidApp|production|], when 1787121709184, flag true  -> 3
    365074 bytes, [production||],           when 1787121722001, flag true  -> 3

The **boolean** is the only argument that changes the result; the three strings
and the timestamp are ignored. 2 and 3 are result codes this project has not seen
before — the plain form gives 0 for a good document and 1 for a bad one — so the
engine is reading the cache and rejecting it, probably over the five-byte
signature/compression header the write log describes.

Not pursued, and the reason matters: **the plain path already returns 0.** Making
a second path also return 0 would be a fourteenth way of establishing "the
settings were accepted", which is the one thing never in doubt. The wrapper stays
because it is correct and someone will want it, behind `CORDIAL_CACHED_SETTINGS`.

**Fifteen: not calling `nativeGameGlobalInit` at all.** §9's captured stack
reaches the failure reporter through it, and §22's ordering test only moved it.
`CORDIAL_NO_GLOBAL_INIT=1` skips the pair outright. The run segfaults later,
exactly as the original comment predicted — `StartLuaAppDM` crashes on a null
`JNIEnv` the globals init was supposed to store — but it gets far enough to
answer the question:

    onFlagsFailed=2   RbxStorage=0   Flags-Not-Received=4

**The verdict fires twice with that call never made.** So it is not produced
there. §9's stack was one of two occurrences and removing that path removes
neither.

### What the shape of fifteen eliminations says

Every input the app-facing interface accepts has now been varied, and none of
them moves the verdict:

* the document — four variants, and its absence
* when it arrives — six seconds early, mid-`initializeNativeCode`, and after
* which native takes it — plain, and the compressed-cache form
* preloaded overrides — three shapes, all accepted with no `ParseFailure`
* the flag-name list, the flag provider, the `Configuration`, the `ArrayList`
* call ordering — bootstrap, globals early, globals late, globals absent
* the callbacks the engine can reach — 7 answered, then 19, descriptors verified

That is the whole surface. The verdict is decided inside the engine at a point
§9 pinned precisely — `movl $0xb` written unconditionally at `0x29c5529`, status
11, the same 11 `NativeDM` then reports as `state:11` forever — and whatever
picks `0xb` is upstream of anything the app hands in.

**This is now a policy question rather than an engineering one.** The only
implementation known to be past this gate writes the byte: mocktail's
`ForceNativeFlagsLoadedForTaskScheduler`, one of **98** patch/force/install
functions in `legacy_runtime.cc`, with 116 `PatchCode` call sites and 77
`EnsureWritablePage` calls beside it. Memory patching is not incidental to
mocktail, it is its method. Its git history was squashed on a GitLab migration,
so there is no record of a legitimate route being tried and failing — only that
the one real JNI candidate, `nativeInitializeNativeFlags`, defaults to **off**
there while the patch runs unconditionally straight afterwards.

Sober reaches `flagLoaded` and remains the existence proof that the state is
reachable. **How it does so is not established** and cannot be from here: the
reference tree holds only `libbadcpu`, and its `decompiled/` directory is
off-limits. "Sober does it legitimately" is not a claim this document makes.

So the honest position: Cordial cannot reach this state through the interface
Roblox exposes to its host application, and the alternative on the table is the
one [ADR-001](../adr/ADR-001-in-process-hooking.md) and
[ADR-003](../adr/ADR-003-plugin-isolation.md) make *absent* rather than disabled,
so that a fork cannot extract the primitive. That decision was taken when no
feature depended on it. One does now. Reversing it is a change to Cordial's
security posture, in a public GPL repository with a fork already layering exploit
functionality on it, and it belongs to the project owner rather than to whoever
is next to run out of eliminations.

### §22.2 Sober's mechanism cannot be settled from here, and why

§22.1 left the central question as: is the flags-loaded state reachable at all
through the host-application interface, or does every implementation that gets
there force it? mocktail forces it. Sober reaches `flagLoaded` and is the only
existence proof that the state is reachable by something.

The test designed for this was to compare Sober's in-memory `libroblox.so`
executable text against the file on disk. Executable pages of a PIE carry no
relocations on x86-64, so a faithful loader leaves `.text` byte-identical; a
non-zero difference count is code patching. `tools/`-adjacent scratch script,
`ptrace_scope` is 0 on this machine, and the method is sound.

**It cannot be run.** Sober is a Flatpak and its engine runs inside the
sandbox's PID namespace, so no host `/proc/<pid>/maps` contains the mapping —
a scan of every readable process for an executable mapping over 50 MB finds
Chrome, WebKitGTK and two LLVM copies, and nothing of Sober's. `flatpak enter`
would cross the namespace but `setns` needs `CAP_SYS_ADMIN`, so it exits without
output as an ordinary user. The mapping is also anonymous rather than named,
exactly as Cordial's own loader leaves it, so it could not have been found by
name either.

Recorded rather than quietly dropped, because the next person will have the same
idea. Settling it needs root, or a build of Sober's loader, or Sober's source —
and the reference tree here holds only `libbadcpu`, with its `decompiled/`
directory off-limits under AGENTS.md.

### One inference not made

`CORDIAL_LATE_SETTINGS=1` still ends the way §19 recorded, `SIGTRAP` at 0.231 s
with `RBXCRASH: FatalRuntimeError (Can't initialize the TaskScheduler before
flags have been loaded)`. The default path produces no such error.

It is tempting to read that as the gate being satisfied on the default path, and
therefore as evidence that `Flag::areFlagsLoaded()` is already true there while
only `NativeDataModelManager` is uninformed. **That is precisely the reasoning
§19.1 got wrong and §22 retracted**: absence of the error is equally what never
attempting the initialisation looks like, and the default path shows no sign of
attempting it — no `RbxStorage::init`, no `ClientRunInfo`. Cordial's own
`setTaskSchedulerBackgroundMode` call is background mode, not initialisation, and
does not bear on it.

So what is observed is only this: the late ordering *attempts* the scheduler
initialisation and dies on the gate, and the default ordering does not attempt
it. Which of `Flag::areFlagsLoaded()` and NativeDM's `Flags-Not-Received` is
false on the default path is **not established**, and the two are not known to be
the same bit.

### §22.3 Correction (2026-08-19): the silencing was a value-shape mismatch, not the mechanism

§22's "Overriding an `FLog` channel can silence it" table used one shape for
every value tried — a bare number, `1`, `7` or `100` — and concluded that
naming a channel in `flags.json` sets it to quiet at every value including the
largest tried. That conclusion is wrong as stated, and the sweep it invalidated
(§21) is not thereby exonerated either — this only settles the mechanism.

Roblox's own cached settings document (`~/.cache/cordial/clientsettings.json`)
already answers what §22 did not check: `FLog`/`DFLog` values there are not one
shape. Most are a bare verbosity number (`FLogNetwork = "7"`,
`DFLogHttpTraceError = "12"`), but a real minority are a severity name with an
optional sub-level (`FLogAudio = "Info"`, `FLogWebRTC = "Error"`,
`DFLogWebSocketTraceError = "Warning,6"`, `DFLogRakNetConnectTrace_PlaceFilter =
"Verbose,9"`). Which shape a given channel's C++ declaration wants is not
visible from the settings document or from `flags.rs`, and `flags.rs` never
tries to guess it — see its `read_layer` doc comment, extended alongside this
correction.

Repeating §22's `FLogNativeDM` case with a severity name instead of a number,
each figure the mean of two runs, `--run 20`, own profile, engine's own `FLog`
file read directly (not stderr):

| `flags.json` | `[FLog::NativeDM]` lines | `[FLog::AppShellReporter]` lines |
|---|---|---|
| absent | 29, 29 (repeat) | 0, 0 (repeat) |
| `{"FLogNativeDM": "9"}` (bare number, as §22 used) | 0 | — |
| `{"FLogNativeDM": "100"}` (bare number, as §22 used) | 0 | — |
| `{"FLogNativeDM": "Debug", ...}` (severity name) | 29 | 14 |
| `{"FLogNativeDM": "Verbose", ...}` (severity name) | 30, 30 (repeat) | 16, 14 (repeat) |

A bare number silences `NativeDM` on every value tried, exactly as §22 found.
A severity name does not — it leaves the channel at or above its unset count,
and the *same* override raises `AppShellReporter`, which is silent by default,
from 0 to 14–16 lines, matching what a bare `"7"`/`"9"` already did to that
channel in §22 (0 → 14). So the direction of the effect (raise vs silence) is
not a property of the mechanism or of the number chosen — it is a property of
whether the value's shape matches what that specific channel's declaration
expects. Wrong shape reads as "override present, channel now silent"; right
shape raises it, on two independent channels, repeated.

**What this means for `flags.rs` and `client_settings.rs`: nothing was wrong.**
Both convert a JSON value to a plain string and hand it through unchanged —
`"7"` stays `"7"`, `"Verbose"` stays `"Verbose"` — which is exactly the
behaviour a heterogeneous, string-typed settings document requires. No code
change was needed to "make it work"; using the right value shape was
sufficient, demonstrated above. A doc comment on `read_layer` now says so, so
the next person who reruns §22's experiment with a bare number does not
independently re-arrive at "the override mechanism is broken".

**The `FlagJniInterface.nativeGetFInt` cross-check, resolved.** §22's report
noted every name probed through it, including `FLogGraphics`, reads back as
"not a registered flag", and asked whether that means the probe reads an empty
Java-side registry. Confirmed, and more specifically than suspected: the names
`nativeInitializeNativeFlags` registers and `nativeGetFInt` can answer for are
the **139 Android-app feature flags** in `docs/traces/native-flag-names.txt`
(`EnableAndroidBinaryChannelDownloadTiming`, `PgsTreatmentActive`, and so on) —
an entirely different namespace from the engine's internal `FLog`/`DFLog`
channels, which are read out of the `applicationSettings` document at
`nativeInitClientSettings` time and never touch `FlagJniInterface` at all. The
probe was run again here with `CORDIAL_FLOG_PROBE=DFLogRbxStorage,FLogGraphics,
FLogAppShellReporter` immediately after confirming (by grepping the same run's
stdout) that `nativeInitializeNativeFlags` had already registered its 139
names — so this is not a timing gap, either. `nativeGetFInt` is simply the
wrong instrument for an `FLog`/`DFLog` channel's state; it was never going to
answer this question, for any channel, regardless of ordering.

**`DFLogRbxStorage`, raised correctly, still never appears.** With the shape
mismatch understood and controlled for, `DFLogRbxStorage` was set to `"9"`,
`"100"`, `"Debug"` and `"Verbose"` — bare numbers and severity names, the same
four value-shapes that moved `NativeDM` and `AppShellReporter` above — across
five separate runs. `[DFLog::RbxStorage]` count: **zero, every time**, while in
the same runs `FLogNativeDM` and `FLogAppShellReporter` visibly responded to
their own overrides, proving the mechanism was live and the document was being
read. This is not a new finding — §23.1 already reached zero by a different
route — but it is a second, independent confirmation, ruling out "the channel
is suppressed" as the reason `RbxStorage::init` is absent. Whatever blocks it,
it is not this.

## §23. The answer: the post-settings call was made too early

`nativePostClientSettingsLoadedInitialization3` called once more, after the
surface is handed to the engine, followed by `nativeRetryInit`. That is the whole
fix, and it produces what nineteen sections of this document were looking for:

    [FLog::NativeDM] initialize: state:11. areFlagsLoaded:true.
    [FLog::NativeDM] getFlagsFromEngine_:
    [FLog::NativeDM] bootstrapTheApp_:
    [FLog::Output] settingsUrl: https://clientsettingscdn.roblox.com/v2/settings-compressed/application/GoogleAndroidApp.zst
    [FLog::NativeDM] ... getFlags: success = true, payload's size = 1300800.
    [FLog::NativeDM] continueAfterFlagsLoaded_:
    [FLog::NativeDM] initEngine_:
    [FLog::NativeDM] initializeLuaApp_:
    [FLog::NativeDM] startLuaApp_:

and on Cordial's side `[roblox] flags loaded (1300800 bytes)` —
`gameActivity_onFlagsLoaded`, with a real `ByteBuffer`, for the first time.

Controlled on one build, three consecutive runs each:

| | `flagsLoaded` | `continueAfterFlagsLoaded_` | `Flags-Not-Received` |
|---|---|---|---|
| default | 1 | 1 | 1 |
| `CORDIAL_LATE_POST_MS=off CORDIAL_LATE_RETRY=off` | 0 | 0 | 4 |

### Why fifteen eliminations missed it

Every one of them moved the settings call and the post call **together**. §11
recorded the symptom exactly — "Cordial's call to
`nativePostClientSettingsLoadedInitialization3` returns without the engine's own
body of it having run" — and then spent five sections looking for a missing
argument, a missing prerequisite call, or a wrong document. The body was fine.
The call was early. Nothing that moved both could ever show that, because moving
them together late is `CORDIAL_LATE_SETTINGS`, which dies on the TaskScheduler
gate before the post call matters.

### Three claims in this document were wrong

**§19.1 and §22 on the TaskScheduler.** §19.1 said the gate is satisfied on the
default path; §22 retracted that as reasoning from an absence and declined to
claim the opposite. The engine now states it: `areFlagsLoaded:true`, on the
default path, before anything here changed. The gate was never the blocker.
`NativeDataModelManager` not being told was.

**`client_settings.rs` on the engine never fetching.** It says so on the strength
of breakpoints on `getaddrinfo`, `connect` and `SSL_connect` never being hit
during startup. The engine fetches
`clientsettingscdn.roblox.com/v2/settings-compressed/application/GoogleAndroidApp.zst`
itself, from `bootstrapTheApp_`. Those breakpoints never fired because this code
path had never run. Cordial supplying the document is still correct and still
what makes `areFlagsLoaded` true — but "the engine does not fetch" is false.

**§22 on logging.** It said the verbose `NativeDM` lines were open but not
raised. They were open all along and print at the default level. They never
appeared because the code emitting them never ran. The §22 measurement that
naming an `FLog` channel in `flags.json` can silence it still stands and is still
worth knowing; the conclusion drawn from it was wrong.

### What is still not done

**`RbxStorage::init` is still zero**, on every run, including a real 100-second
join. So the content store is still down and every asset still comes off the
network each session. What is different is that it is now localised rather than
mysterious:

    [DFLog::CaptureStorage] RbxStorage is not initialized, cannot access storage interface
    [DFLog::RbxmFileManager] LocalStorageManager is not available.
    [FLog::LocalStorageHandler] Not available on the current platform.

Storage waits on the platform local-storage handler, which Cordial implements in
`native/local_storage.cpp` but installs only behind
`CORDIAL_LOCAL_STORAGE_SET_PLATFORM_IMPL`. With that on, `setPlatformImpl ok` now
succeeds where §19 recorded it crashing — and the process then dies `SIGTRAP`
after repeated `djinni (djinni_support.cpp:529): weakRef`.

**It is not `NewWeakGlobalRef`.** That was the obvious reading and it is wrong:
instrumenting libjnivm's `NewWeakGlobalRef` to print whenever it returns null
produced **no output at all** across a full run, while the djinni exceptions
carried on. libjnivm implements weak global references and has a test covering
expiry. Whatever djinni asserts on at that line, it is something else, and the
instrument is recorded here as a disproof rather than left in the tree.

Sober logs `LocalStorageHandler] Not available on the current platform.` too and
still reaches `RbxStorage::init`, so that message is not the blocker either.

**The delay is unfinished work, not a constant.** 250 ms, because at 0 ms the run
reaches `Flags-Not-Received=0` — better than any other value tried — and then
segfaults. Something is still racing and the delay hides it.

### §23.1 RbxStorage after the fix: what was tried, and one observation nobody should build on

With the flags chain working, everything §12–§21 eliminated deserved re-testing,
because every one of those eliminations was measured against a baseline where the
chain never ran. Done, and none of it moves storage:

* `FFlagStartRbxStorageInitRighAfterFlags` and `DFFlagRbxStorageInitLatch` set
  true, against a control with neither, on fresh data roots: no storage either
  way. This flag was the premise of §11's whole storage theory.
* Three consecutive launches against one warm root, so the flag cache and
  tombstone are present: no storage on any of them, and the tombstone is never
  *read* on any launch, only written. Sober reads one.
* `CORDIAL_LOCAL_STORAGE_SET_PLATFORM_IMPL=1`: `setPlatformImpl ok` now succeeds
  where §19 recorded it crashing, and the process then dies `SIGTRAP` after
  repeated `djinni (djinni_support.cpp:529): weakRef`. **This is a dead end
  regardless** — Sober logs `[FLog::LocalStorageHandler] Not available on the
  current platform.` too, and reaches `RbxStorage::init` anyway. Storage does not
  need this handler.
* The late-post delay at 250 ms and at 2000 ms, `--run 40`, fresh roots: no
  storage at either.

`[DFLog::CaptureStorage] RbxStorage is not initialized, cannot access storage
interface` fires on a real join, so it is genuinely down rather than quietly
working.

**The observation not to build on.** One data root does contain a real store —
`rbx-storage.db` with a WAL, `rbx-storage-sc`, and partition directories `p14`
and `p15` — created at 17:46:02 during this session. It has not been reproduced:
re-running the two candidates that were live at that moment, on fresh roots with
controls, produces nothing, and a fresh run against that same root does not touch
the database's mtime. The one thing that root has which no fresh root does is 43
`ContentProvider_*` cache directories, so content was being cached there by some
other path. That is a lead, not a result, and it is written down as an
unreproduced observation precisely so nobody quotes it as evidence that storage
works.

**Where this leaves it.** `DFLog::RbxStorage` has never appeared in a Cordial log
at all, on any run, so there is no engine statement about storage to read — the
absence of `RbxStorage::init` is consistent both with "never attempted" and with
"attempted and unlogged", and §22 is the standing warning about which of those
an absence licenses. Establishing which needs the channel genuinely raised, and
§22's measurement is that naming a channel in `flags.json` can silence it rather
than raise it. That mechanism is still not understood and is now the thing
blocking the question, not the storage code.

### §23.2 Storage is never attempted, and this settles it

§23.1 left the question as "never attempted or attempted and unlogged", and said
the logging mechanism was blocking the answer. It is not: the filesystem answers
it without any log channel.

`CORDIAL_TRACE_PATHS=1`, one 35-second run, **19,296 intercepted path calls, zero
of them containing `rbx-storage`**. The absence of `RbxStorage::init` from the
log is therefore "never attempted". Nothing is being initialised quietly.

(That switch is real and works. It is worth saying because an earlier attempt in
this session concluded it produced no output at all — the grep was for `[path]`
and the format is `[paths]`. The tool was fine; the measurement was wrong.)

What the engine does touch, once the 17k `/sys/devices/system/cpu/*/cpufreq`
polls are set aside:

    180  /proc/self
     51  <profile>/data/files/appData
     50  <profile>/data/cache
     30  ./exe
     23  <profile>/data/files/appData/LocalStorage
     14  http                      <- relative
     14  /dev
      8  <profile>/data/cache/wob
      6  cache                     <- relative

So the engine is doing local storage, under `appData/LocalStorage`, and simply
never reaches for the content store.

### The block is short by three steps, and they are consecutive

Against Sober at the same point, Cordial's post-settings block is missing exactly
the three lines that run together at the end of it:

    Sober                                          Cordial
    IxpStorageManager: Failed to open cache file   absent
    TombstoneCache: Tombstone 1 ... read from file absent
    TombstoneCache: Setting holdout state: false   present
    LocalStorageHandler: Not available             present
    RbxStorage::init [INIT] user: flagLoaded       absent

Cordial reaches `Setting holdout` and `LocalStorageHandler`, so it is not that
the block stops early. It skips the Ixp cache open and the tombstone *read* while
still writing a tombstone of its own — and Cordial's write goes to
`cache/tombstone.dat`, **relative**, where Sober's is absolute. Both
`<profile>/run/cache/tombstone.dat` and `<profile>/data/cache/cache/tombstone.dat`
exist on disk here, which is what a relative write and an absolute read look like
when they disagree.

**Whether the tombstone read gates `RbxStorage::init` is not established.** It is
the only difference left inside the block, it is immediately upstream of the
missing line, and the relative-path split is a mechanism that would explain a
silent skip. That is a lead with something behind it rather than another flag to
try, and it is where the next session should start.

### §23.3 The two routes to `flagLoaded` are not the difference

Sober reaches `flagLoaded` from the application handing the settings document
over. Cordial now reaches it from the engine fetching its own inside
`bootstrapTheApp_`. Both end in `continueAfterFlagsLoaded_`, and only Sober's is
followed by `RbxStorage::init [INIT] user: flagLoaded`, which made "the routes
are not equivalent to whatever asks for storage" the obvious next theory.

It is wrong. Delivering the document again on the app's route, immediately before
the late post call, against a control without it, on fresh data roots:

    with late settings     flags loaded = 1   RbxStorage = 0   storage files = 0
    without                flags loaded = 1   RbxStorage = 0   storage files = 0

The switch is kept, off by default, as the record. Sixteen candidates now.

### §23.4 The settings document Cordial supplies is the wrong one, and that is not the storage bug either

Cordial fetches `clientsettingscdn.roblox.com/v2/settings/application/**AndroidApp**`
and separately calls `nativeOverrideChannelPlatformName` to say it is
**`GoogleAndroidApp`**. When the engine went looking for flags itself, it fetched
`.../application/GoogleAndroidApp.zst` — its own name for itself, not the one
Cordial had handed it.

The two documents are not the same. `AndroidApp` carries 22,196 flags,
`GoogleAndroidApp` 22,610; 441 values differ, 27 of them with `Storage`, `Cache`,
`Ixp` or `Tombstone` in the name. So Cordial has been running the client on a
document meant for a slightly different application than the one it claims to be.

**It is not the storage bug.** Supplying `GoogleAndroidApp` via
`--client-settings`, against a control on the stock document, on fresh data
roots:

    GoogleAndroidApp   flags loaded = 1   RbxStorage = 0   storage files = 0
    AndroidApp         flags loaded = 1   RbxStorage = 0   storage files = 0

Candidate seventeen. Worth correcting on its own terms regardless — a client
should be given the flags for the application it says it is — but it is a
separate change from this one and is not made here.

### §23.5 `statvfs` was never intercepted, and §23.2's instrument was blind to it

mocktail's answer to storage is not a flag or an ordering. It is
`EnsureDefaultDataLayout` (`src/libc_shim/libc_shim.cc`, Apache-2.0): it creates
the Android private-data directory tree — including `rbx-storage`,
`appData/rbx-storage`, `files/appData/rbx-storage`, `cache/rbx-storage`,
`appData/LocalStorage`, `files/appData/OTAPatchBackups` — **before** the engine
runs, and its own tests assert that `statvfs` and `statfs` succeed on the
`rbx-storage` path. `RbxStorage::init` reports `availableDiskSpace` as part of
starting, so storage asks the filesystem for room before it builds anything.

Cordial created none of those directories. Two of them,
`appData/LocalStorage` and `appData/OTAPatchBackups`, were already visible as
failed opens in Cordial's own path trace, which should have been the clue.

**And `statvfs` was not intercepted at all.** The engine imports it —
`libroblox.so statvfs` is in `undefined-symbols.tsv`, and `nm -D` shows
`U statvfs@LIBC` — while `native/system_paths.cpp` wrapped `stat`, `lstat`,
`access`, `opendir`, `realpath`, `readlink`, `fopen` and `open`, and not this.
So it was neither path-translated nor traced.

That is a correction to §23.2. Its conclusion — "storage is never attempted,
19,296 intercepted path calls and not one contains `rbx-storage`" — was drawn
from a trace that could not see the call storage actually makes. **A trace that
cannot see a call is not evidence the call did not happen**, and this document
has now made that mistake twice: once reading an absent fatal error as a passed
gate, and once here.

With the interception in place, three `statvfs` calls appear, all succeeding:

    [paths] tid=… statvfs("./appData") = 0
    [paths] tid=… statvfs("…/profiles/default/data/files") = 0
    [paths] tid=… statvfs("…/profiles/default/data/files") = 0

The first is relative, resolved against the working directory, and only succeeds
because the layout above now creates `./appData` there. Before this change it
would have failed.

### The ABI divergence found on the way

bionic's `struct statvfs` runs `f_fsid, f_flag, f_namemax`. glibc's inserts an
`int __f_unused` after `f_fsid`, which on LP64 pushes `f_flag` and `f_namemax`
eight bytes along. The engine reading `f_flag` would get glibc's padding, and
reading `f_namemax` would get glibc's `f_flag`; `ST_RDONLY` lives in `f_flag`.
The free-space fields `f_bsize` through `f_favail` happen to align, so this was
not obviously fatal, which is exactly why it survived. Fixed by filling the
bionic shape field by field, as `sigset_t`, `struct sigaction` and `mallinfo`
already are.

### Still not initialising

`RbxStorage::init` remains zero with the layout created, `statvfs` intercepted
and succeeding, and the flags chain working. So the precondition mocktail
satisfies is necessary-looking but has not proved sufficient here. What is
different now is that the instrument is honest: `statvfs` is traced, so the next
person can see what storage asks for instead of inferring from an absence.

### §23.6 `RbxStorage::init` is entered and declines. It was never "not asked for"

Found by scanning the current binary for references to the
`RbxStorage::init [INIT] user: {}, availableDiskSpace: {} …` format string and
then confirming live under `lldb`.

**The addresses in §3, §9 and §10 are stale.** The build moved:
`gameActivity_onFlagsFailed` is at `0x41e987` here, not `0x40f096`. The scan
technique still works; the numbers do not.

`RbxStorage::init` is `0x230bd3a`, and it is a lazy singleton getter with **63
direct call sites**, each preceded by a `lea` loading its own label string —
`AssetProvider`, `SessionTracking`, `CaptureStorage`, `ClientStorageInterface`,
`LocalRuntimeContentStorage`, `ClientReplicator-init`, `CrashMetric`, `DeviceGL`,
the `http-*` family, `shutdown`, `flagLoaded`, and fifty more. The `user:` field
in the log line is that label. So `flagLoaded` is not *the* trigger, it is
whichever of sixty-three callers happened to get there first on Android.

**Live, with `0xCC` planted at `0x230bd3a` per §10's technique:** it fires. Two
independent runs, `rdi` pointing at `"AssetProvider"`, one of them with three
threads hitting concurrently. And the log-emit branch deeper in the same function
was hit **zero** times in every run.

So storage is entered and returns early. Both previous statements that it was
never attempted were wrong, and they were wrong on two independent pieces of
evidence: §23.2's path trace (retracted in §23.5 for being blind to `statvfs`)
and §22.3's channel sweep. Two lines of evidence agreeing did not make them
right; they were both measuring the same downstream absence.

Two further facts:

* The `flagLoaded` wrapper at `0x230bcc2` has **zero** direct callers in `.text`
  and no raw-pointer reference in `.data`/`.data.rel.ro`, so it is reached only
  by indirect dispatch — the same honest edge §3 and render-gate.md §2 hit — and
  across ~85 s of live breakpoint coverage it was **never hit**. The specific
  call Sober's log shows completing is one this build never issues in Cordial.
* `AssetProvider` fires at **startup** in Cordial. §11 lists it as join-time and
  expected absent from a startup-only run on Sober. Another instance of the
  ordering scramble §11 already names.

A methodological note kept deliberately: the first probe caused a `SIGSEGV` that
looked like an engine crash. It restored a shared `0xCC` and single-stepped only
the selected thread, leaving two others mid-prologue with `push rbp` unexecuted.
The corrected probe hit the same three-thread pattern cleanly and ran to
teardown. **The crash was the instrument.** This document already carries
findings that turned out to be the measuring apparatus; that one was caught
before it became one.

**What is not established:** which of the early-return branches `AssetProvider`
takes, and what writes the condition it tests. That is the next step and it is a
dynamic one — a breakpoint on each branch target, then a hardware watchpoint on
whatever byte the condition reads. Hardware watchpoints do not need the module
registered with the debugger, so unlike breakpoints they work here directly.

### §23.7 The function boundary was wrong, and `.eh_frame` is how to not get this wrong

§23.6 reported `RbxStorage::init` as `0x230bd3a`, with the `[INIT]` log emit at
`0x2312fbc` "further into the same function", and concluded that storage is
entered and returns early. The entry-and-return is real. The attribution is not.

`.eh_frame` carries exact function bounds and survives stripping, so this is a
lookup rather than an investigation — 260,630 FDEs, from
`readelf --debug-dump=frames-interp`:

    0x230bd3a  ->  FDE 0x230bd3a .. 0x230c74a   size  2,576
    0x2312fbc  ->  FDE 0x23121ae .. 0x2315c6a   size 15,036
    0x230bcc2  ->  FDE 0x230bcc2 .. 0x230bd3a   size    120

`0x230bd3a` is a 2,576-byte function that ends at `0x230c74a`. The log emit is in
a **different function**, `0x23121ae`. A backward walk from an address to a
function start has no way to know it has crossed a boundary on a stripped binary,
and it crossed one here — the 29 KB span that reading implies should have been
the tell.

So the fast path traced in §23.6, the `.bss` pointer written by bionic's own
`call_constructors` during `.init_array`, and the conclusion that the branch is
an unconditional singleton-getter check are all **about the getter**. They stand
as facts about `0x230bd3a` and say nothing about storage initialisation.

`0x230bcc2`, the `flagLoaded`-labelled thing, is 120 bytes ending exactly where
the getter begins — a thunk sitting in front of it, not a caller of init.

**Whether `0x23121ae` is entered in Cordial is not established.** That is now the
question, and it is a different one from the last three sections'.

**Use `.eh_frame` for this from now on.** Every address in §3, §9 and §23.6 was
derived by scanning backwards for a prologue, the build has moved at least once
underneath those numbers, and one of them was wrong by a whole function. The FDE
table is authoritative, costs one `readelf`, and is not disassembly.

## §24. Storage initialisation is a scheduled task, which is why this document is about both things

`0x23121ae` — the real `RbxStorage::init`, per §23.7's `.eh_frame` bounds — has
exactly three direct call sites and no raw pointer anywhere in `.data` or
`.data.rel.ro`:

* `0x230c3af`, inside the getter, on the branch taken when its pointer is null;
* `0x6824a78`, in a small function at `0x6824a30`;
* `0x6824b0e`, in a small function at `0x6824af8`.

And immediately before the first of those, at `0x230c393`, the getter loads a
pointer to `0x6824af8`, loads the string **`"RbxStorageInit"`** (`0x4ec600`), and
calls `0x29852f6`. That is a named task being registered, with `0x6824af8` as its
body — the third call site above. **Storage does not initialise inline. It is
scheduled.**

That is the connection this document spent twenty sections not seeing. The
question it opened with was `Can't initialize the TaskScheduler before flags have
been loaded`; the question it has been stuck on is why the content store never
comes up. They are the same question. A store whose initialiser is a scheduled
task cannot come up until something runs the task.

### Live: it never fires

`0xCC` at `0x23121ae`, `probe2.py`, own data root, four full-length runs:

    run 1  real init alone armed                      0 hits, clean exit
    run 2  real init + getter armed                   getter 22 hits, real init 0
    run 3  as run 2, repeated                         getter 22 hits, real init 0
    run 4  real init + both thunks armed              0 hits on all three

The getter's 22 hits per run carry four distinct labels — `AssetProvider`,
`http-available`, `http-write-init-only`, and **`flagLoaded`**.

**That retracts §23.6's claim that the `flagLoaded` call never happens in
Cordial.** It does, twice per run. The earlier reading was an attach race: three
runs missed it because the probe attached after it had already gone past. With
the getter instrumented for longer it appears every time. The rest of §23.6's
getter findings stand; that one does not.

So `flagLoaded` *does* reach the getter, and the getter returns its
already-constructed pointer without ever taking the branch that would register
the task. §23.6 established that pointer becomes non-null during bionic's own
`call_constructors`, running `.init_array` as part of loading the library —
before any caller exists. The slow path is therefore dead from process start.

### What is not established

Neither `0x6824a30` nor `0x6824af8` has a direct caller in `.text` or a raw
pointer in the data sections; both are reached only through indirect dispatch,
the same wall `render-gate.md` §2 and §3 already hit. Whether some third path
reaches storage init was **not found in four runs**, which is not the same as
proven absent, and is stated that way deliberately.

What would make the getter consider its object not-ready at call time is
unresolved, and answering it means reading the layout of an object that is
Roblox's, which is the line AGENTS.md draws.

### The lead this actually opens

mocktail turns the task scheduler on by JNI rather than by patching — two of its
few non-patch knobs, both on by default:
`MOCKTAIL_ASMA_START_TASK_SCHEDULER_FOREGROUND` calls
`setTaskSchedulerBackgroundMode` in foreground mode, and
`MOCKTAIL_TASK_SCHEDULER_FOREGROUND_ON_MAIN_THREAD` routes that call onto the
main thread. Cordial makes the same call — its log carries
`setTaskSchedulerBackgroundMode() enable:false context:ASMA.start` at 0.480s —
but nothing here has ever checked which thread it runs on, and mocktail
considered that worth a dedicated switch.

That is a legitimate JNI call, not a memory write, and it is the first lead in
this document that connects the scheduler to the store by a mechanism rather than
by proximity.

### §24.1 The scheduler is already in foreground mode, and the job still does not run

§24 named mocktail's two non-patch scheduler knobs as the lead. Cordial already
satisfies what they do. From an ordinary run:

    0.554648  [FLog::AndroidGLView] rbx.datamodel: setTaskSchedulerBackgroundMode() enable:false context:ASMA.start
    1.196494  [FLog::NativeDM] startLuaApp_: ... (TaskScheduler) enable-Background = false.
   30.966351  [FLog::NativeDM] pause-LuaApp: ... (TaskScheduler) enable-Background = true.

Foreground from 0.55 s, confirmed again by the data model at 1.20 s, and only
backgrounded at teardown. Giving it threads explicitly
(`FIntTaskSchedulerAutoThreadLimit = 8`) against a control changes nothing:
`RbxStorage` lines stay at zero and no store appears either way.

So the scheduler is up, in the right mode, and the `RbxStorageInit` job still
never executes. Candidate eighteen.

### Where this stops, honestly

The flags half of this document is finished: `onFlagsLoaded` fires, the
`NativeDataModelManager` chain runs to `startLuaApp_`, reproducible with a flat
control. **The store is not up and this session did not get it up.**

What is now known that was not before, all of it measured:

* `RbxStorage::init` is `0x23121ae` (`.eh_frame`, not a prologue scan), and it is
  **never entered** — four runs, `0xCC` planted, including one arming it and both
  of its unreachable callers together.
* Storage initialises as a **scheduled task**, `RbxStorageInit`, registered with a
  function pointer to `0x6824af8`.
* The getter that would register it returns early on every call, because its
  pointer is filled during bionic's `call_constructors` before any caller exists.
* Its callers include `flagLoaded`, twice per run — §23.6 said otherwise and was
  wrong.
* The scheduler is foregrounded and threaded.

Three of this document's own conclusions were retracted getting here — a function
boundary off by a whole function, a path trace blind to `statvfs`, and a channel
sweep that used the wrong value shape. Each was found by measuring something the
previous conclusion had assumed. **That is the method that has worked, and it is
what the next attempt should use** rather than the eighteen candidates already
eliminated.

The one thread with a mechanism behind it and no measurement yet: what makes the
getter's pointer null at the moment `flagLoaded` calls it on a platform where the
store does come up. Answering it means reading the state of an object whose
layout is Roblox's, which is the line AGENTS.md draws, and it should be
approached by observing that object at runtime rather than by decompiling it.

## §25. Storage init *is* entered. It runs too early, fails, and memoises the failure

§24 said `0x23121ae` is never entered, on four instrumented runs. **That is
wrong, and the instrument was the reason.** Every probe in §23 and §24 attached
to an already-running process, and the `lldb` attach handshake is slower than the
moment that matters. Launching `cordial-run` *under* lldb with
`eLaunchFlagStopAtEntry`, then breaking on Cordial's own
`mcpelauncher_linker_notifylldb` (`linker_soinfo.cpp:546`, called the instant
`libroblox.so` is mapped and before one instruction of it runs) and planting the
storage breakpoints in that same stop, changes the answer. Reproduced across six
runs.

The control is the strongest part: the same agent's own attach-based probe got
zero hits on the same getter in the same session. **The difference is the
instrument, not the engine** — which is the third time in this document a
conclusion has turned out to be a property of how it was measured.

### What actually happens

The getter's slow path runs **exactly once**, on the first call, and it takes the
**direct-call branch** (`0x230c3a7`, flag byte zero) straight into `0x23121ae`.
It never takes the schedule-a-task branch: `0x230c373` was hit zero times in
every run, and the registrar `0x29852f6` — which does fire about six times a run
for other subsystems — never once carries the storage body pointer. So §24's
"storage initialises as a scheduled task" is half right: the task branch exists
and is simply not the one taken.

The caller label at that one call is **`"RbxStorage"`**, not `"flagLoaded"`, and
it fires **during `libroblox.so`'s ELF constructors — before `JNI_OnLoad`**.
Backtrace: `notifylldb` ← `soinfo::call_constructors` ← `do_dlopen` ←
`cordial_linker_sys::dlopen` ← `load.rs`.

And it fails. Under `CORDIAL_TRACE_PATHS=1`, same thread, two runs:

    stat("./appData")    = 0
    stat("./appData")    = 0
    statvfs("./appData") = 0
    stat("")             = -1
    stat("")             = -1
    stat("")             = -1

Something it needs resolves to an empty string.

### Why that ends the investigation's confusion

This is a **memoising lazy singleton: first caller wins, permanently.** The
Waydroid capture shows Android's winner:

    [DFLog::RbxStorage] RbxStorage::init [INIT] user: flagLoaded, availableDiskSpace: 60655730688 bytes
    [DFLog::RbxStorage] RbxStorage::init [DONE] … dbOpenCount: 1

On Android `flagLoaded` wins the race and succeeds. In Cordial `"RbxStorage"`
wins it first, during ELF construction, before flags exist — and fails. When
`flagLoaded` arrives later (twice a run, per §24's own count) it is handed the
already-"initialised" broken object and never retries.

So every one of the eighteen eliminated candidates was aimed at the wrong moment.
They were all about the state at `flagLoaded` time. The decision had already been
taken and cached before `JNI_OnLoad` ran.

### The remaining question, now small

**What does the pre-`JNI_OnLoad` caller need that resolves empty?** It cannot be
a JNI or `Context` query — there is no JNI yet. It has to be something native
resolves at constructor time, and every directory Cordial sets
(`nativeSetFilesDirectory` and the rest) is set *after* `dlopen` returns, so none
of them exist yet when this runs.

`INFERRED, not established:` that the empty operand is a per-user or per-session
path component unavailable pre-flags. Nobody has established what builds that
string, and doing so by reading the binary means reading Roblox's own object
layout, which AGENTS.md places off-limits. It should be answered by observation.

The shape of a fix, if the guess is right, is that whatever the engine reads at
constructor time has to be true *before* `dlopen`, not after — which is a
different kind of change from anything tried so far, and cheap to test.

**Reusable, and the most valuable artefact here:** the launch-time race-free
breakpoint technique. `SBTarget.Launch` under a synchronous `SBDebugger` silently
free-runs without `eLaunchFlagStopAtEntry`. Any future probe of anything that
happens during library load must use it; attaching is too late, and this document
has now drawn a wrong conclusion from that twice.

### §25.1 The empty stats come from inside `RbxStorage::init`, after the `[INIT]` emit site

Walked up from the empty `stat("")` by breakpointing Cordial's own `s_stat`
shim — no disassembly needed, since Cordial owns that wrapper — and printing a
backtrace only when the path is empty. Three independent launches, identical
offsets:

    s_stat <- 0x226eea1 <- [0x226ec71|0x226f571] <- [0x231e52b|0x231e53b|0x231e547]
           <- 0x2315ced <- 0x2312fe3 <- 0x230c3b4 <- 0x230bd04  (getter slow path)

The three empty calls are three near-identical sites 16–28 bytes apart inside one
helper, which is exactly the three `stat("")` §25 recorded.

**The control is what makes this trustworthy:** the same thread, in the same
function, makes two *successful* `stat("./appData")` calls at `0x23125ef` and
`0x2312c9d`, and they go through the **same** generic leaf utility as the empty
ones. So this is not a broken subroutine — it is the same "does this path exist"
helper, called from a different point, with an empty argument. Call counts match
the original `CORDIAL_TRACE_PATHS=1` trace line for line: two `./appData`, three
empty, one thread.

### And the ordering raises a bigger question

Those addresses put the observed execution in this order inside
`RbxStorage::init`:

    0x23125ef   stat("./appData")  = 0        observed
    0x2312c9d   stat("./appData")  = 0        observed
    0x2312fbc   [INIT] log emit                not yet checked
    0x2312fe3   helper -> 3x stat("") = -1    observed

`0x2312fe3` is executed — it is a return address in the backtrace — and it is
**0x27 bytes past the `[INIT]` emit site**. `[DFLog::RbxStorage] RbxStorage::init
[INIT]` has never appeared in a Cordial engine log, at any channel setting, in any
run.

If `0x2312fbc` also executes, then that line is being emitted and swallowed, and
this project's evidence that storage "never initialises" is really evidence that
a log channel is silent — with storage in fact running well past that point and
failing later, on the empty paths.

**Not asserted.** `0x2312fe3` executing does not prove `0x2312fbc` did; a branch
between them would explain both. It is one breakpoint to settle and it is being
settled now. Recorded because the possibility changes what several earlier
sections mean, and because a channel read during ELF construction — before
Cordial's settings document is delivered at all — would be unreachable by any
flag, which would explain six clean negatives on `DFLogRbxStorage` that were read
as "storage is not running".

## §26. Storage init runs. It has been running all along, and the log was silent

`0x2312fbc` fires. Five runs, three of them fresh against a wiped data root, all
clean, all in the same order on the same thread:

    0x23121ae   RbxStorage::init entry
    0x2312fbc   [INIT] log emit          <- executed
    0x2312fe3   helper -> 3x stat("")    <- executed, fails

A backward `lea` scan confirms `0x2312fbc`'s argument is exactly
`"[DFLog::RbxStorage] RbxStorage::init [INIT] user: {}, availableDiskSpace: {} bytes, elapsed: {:.3f} ms"`,
so this is the `[INIT]` emit and not a coincidentally nearby address. The call
just before `0x2312fe3` loads the literal `"rbx-storage"`.

**So `RbxStorage::init` executes, emits its `[INIT]` line, and continues into the
empty-path failure.** The line never reaches the log.

### Why the log is silent, and why six flag runs could never have found it

Measured ordering, every run:

    loading libroblox.so …        <- RbxStorage::init runs here, in ELF constructors
    LOADED in N ms
    JNI_OnLoad
    calling GameActivity.initializeNativeCode
    bootstrapTheApp: delivering settings and flags
    nativeInitClientSettings

Settings delivery is unambiguously later than the point where storage init
already ran and failed. **No `DFLogRbxStorage` override at any value could ever
have reached it**, because the mechanism that carries flag values into the engine
fires strictly afterwards. The six clean negatives on that channel were reading a
line that had already been emitted before the flag existed.

`INFERRED` for the gating mechanism itself — that would mean reading Roblox's
logging internals — but the timing underneath it is directly measured.

### What this retracts, and it is most of the document

Every statement in §§12–24 that storage "is never asked for", "is never
attempted" or "is never reached" is wrong. It runs, on every launch, during
library load. Nineteen candidates were eliminated against a question that was
never the right one: they all asked why storage does not start, and it starts.

The three earlier retractions were each an instrument artefact — a path trace
blind to `statvfs`, a channel sweep using the wrong value shape, a function
boundary from a prologue scan, and twice an `lldb` attach that arrived too late.
This is the fourth and largest, and it has the same shape: **an absence in a log
was read as an absence in the engine.**

### The one thing left

`RbxStorage::init` fails on a path component that is empty at ELF-constructor
time, immediately after loading the literal `"rbx-storage"`. It is a memoising
singleton, so that failure is cached and the later `flagLoaded` caller is handed
the broken object.

On Android the winner is `flagLoaded` and it succeeds, so the constructor-time
call either does not happen there or finds that component populated. Which of
those is the question, and it decides the fix: either stop the early call, or
make the component non-empty before `dlopen` — the only window that exists,
since every directory Cordial sets is set after `dlopen` returns.

The `[DONE]` emit site was not located; two mechanical scans found no reference,
and pushing further would have crossed from observing into reading. So how far
init gets past the empty stats is still unknown.

### §26.1 Not getenv, not system properties, and not present on Android at all

Three things established since §26, each with repeats.

**The Android capture settles the shape.** `docs/traces/waydroid-roblox-startup.log.gz`
contains exactly two `RbxStorage` lines in the whole file — `[INIT]` then
`[DONE]`, both `user: flagLoaded`, 28.5 ms apart — and `[INIT]` fires at 0.4158 s,
*after* `nativeInitClientSettings` at 0.3752 s. There is no earlier attempt, no
failed attempt, and nothing labelled `"RbxStorage"` anywhere before it. **The
constructor-time call Cordial makes does not happen on Android.** So this is not
"Android supplies a value we do not"; it is "Android does not take this path".

**`getenv` is ruled out.** Breakpoint on `getenv`, callers classified by whether
the return address falls inside `libroblox.so`, gated against a breakpoint at
`JNI_OnLoad`. Two runs: 1314 and 1309 calls process-wide, 8 from the engine in
each, **all after `JNI_OnLoad`** — `OPENSSL_ia32cap`, `SSLKEYLOGFILE`, proxy
variables. Zero before. Definitive.

**System properties are ruled out too.** `CORDIAL_TRACE_PROPS=1` now names every
`__system_property_get` and what it was told. An unknown key returns the empty
string, which is exactly the `stat("")` shape, and properties are one of the few
things readable that early on Android — so this was the strongest remaining
candidate. It is wrong: **no property is queried before `JNI_OnLoad`.** Every
query happens after, and they are `ro.build.version.sdk`, `ro.product.model`,
`ro.hardware` and `ro.soc.manufacturer`.

`ro.soc.manufacturer` comes back `<empty, not in table>`, twice a run. That is a
real gap and it is **not** this bug — it happens after load — but it is the kind
of empty answer that causes trouble somewhere else eventually, and it is now
visible rather than silent.

**And at the call site itself**, registers at the three empty `stat` calls hold
`"rbx-storage"` as the only printable input; the pointers that come back empty
have the shape of return-by-hidden-pointer output parameters rather than inputs.
So the empty value is computed inside the callee, from state not visible at that
frame at all. `INFERRED` from ABI shape, and the point at which further reading
becomes reading Roblox's implementation rather than observing it.

### Candidate twenty, and the question that is left

Twenty eliminated. What is left is not "what value is missing" — that has been
checked three ways and nothing is being handed in. It is:

**What gates whether this constructor-time call is attempted at all, and why does
that gate evaluate differently here than under Android's zygote-forked process?**

Two routes follow from it, and they are design decisions rather than
measurements: find and match the gate so the early attempt never happens, or find
a way to make the engine retry after settings arrive. The second is implied by
the `[INIT]`/`[DONE]` timing regardless — Android's single successful init is at
0.4158 s, well after settings — so a client that fails at 0.0 s and memoises it
needs a retry no matter what the gate turns out to be.

## §27. Cordial can defer libroblox.so's own constructors past its directory
## setup — and that is coherent but does not help, and going further is not
## coherent at all

Cordial owns its loader (`third_party/mcpelauncher-linker`, vendored), and
`do_dlopen` (`bionic/linker/linker.cpp:2178`) already does its work in two
separable steps: `find_library` maps and relocates the object, and only then,
separately, does `si->call_constructors()` run the ELF constructors —
`RbxStorage::init`'s home per §26. Nothing between those two steps requires
the second; `soinfo::call_constructors()` is idempotent
(`linker_soinfo.cpp:550`, guarded by `constructors_called`), so it can be
called late instead of immediately without changing what a normal load does.

`patches/0003-defer-libroblox-constructors.patch` adds exactly that split:
`mcpelauncher_defer_next_ctors(1)` makes the next `dlopen` skip the
constructor step; `mcpelauncher_run_deferred_ctors(handle)` runs it later.
Both are new exports, called from nowhere in the default path —
`crates/cordial-runtime/src/bin/load.rs` only reaches them behind
`CORDIAL_DEFER_CTORS=1` (defer past the four `NativeSettingsInterface`
directory setters, then run constructors) and `CORDIAL_DEFER_PAST_SETTINGS=1`
(additionally defer past a direct, out-of-band call to
`nativeInitClientSettings` — bypassing `bootstrapTheApp`'s normal callback
route entirely, since that route needs `initializeNativeCode`, which needs
constructors already run).

### Deferring past the four directory setters is coherent, and does nothing

Three plain runs, XDG_DATA_HOME wiped between each: `nativeSetFilesDirectory`,
`nativeSetCacheDirectory`, `nativeSetExternalDirectory` and
`nativeSetBaseDataDirectories` all report `ok (pre-ctors)` — called through
Cordial's own `JNIEnv` (`linker::jni::create_vm()`, independent of
libroblox.so's own state) before a single constructor of libroblox.so has
run — and `mcpelauncher_run_deferred_ctors` then returns without crashing
every time. This is the answer to the question this section set out to
establish: **the four directory setters do not depend on
constructor-initialised state.** Calling them before `.init_array` runs is
safe.

It also changes nothing. All three runs still show the exact §25 signature —

    stat("./appData")  = 0
    stat("./appData")  = 0
    statvfs("./appData") = 0
    stat("")            = -1
    stat("")            = -1
    stat("")            = -1

— and no `rbx-storage.db` appears anywhere under the profile in any of them
(checked with `find ~/.cache/cordial-agent-defer -iname '*rbx-storage*'`;
only the empty directories Cordial itself pre-creates are ever there). A
fourth run, launched under `lldb` with `eLaunchFlagStopAtEntry` per this
document's own rule, breakpointed on `mcpelauncher_run_deferred_ctors`,
`mcpelauncher_linker_notifylldb`, and the two §25/§26 offsets
(`0x23121ae` real-init-entry, `0x2312fbc` the `[INIT]` emit), confirms it
directly: both storage sites are hit exactly once, cleanly, no crash. One
genuine surprise in that run: the hit lands on a thread **different** from
the one that called `mcpelauncher_run_deferred_ctors` — where §25's own
backtrace (`notifylldb ← call_constructors ← do_dlopen ← ... ← load.rs`)
showed the same call same-thread in the default order. Not chased further;
it does not change the verdict below and chasing it would mean reading which
constructor hands the call to a worker thread, which is Roblox's own
implementation rather than its observable behaviour.

**So the directories were never the missing input.** This is the same
conclusion §26.1 reached from register shapes at the call site (the empty
pointers have output-parameter shape, not input) — reached again here by
directly testing it rather than inferring it, and holding up.

A methodology note earned the hard way while getting this measurement: the
first attempt at the lldb-instrumented run reported a SIGSEGV. It was not a
finding — it was the probe script gating its manually-poked `0xCC` restore on
`reason == eStopReasonBreakpoint`, which this document's own rules already
say is wrong (a manual poke reports `eStopReasonSignal`), so the breakpoint
byte was never restored and the instruction it replaced (a bare `push rbp`)
never ran, corrupting the stack. Fixed to check the hit address
unconditionally, matching `probe_launch.py`, and the crash did not recur.
Recorded because it is exactly the "wrong instrument" shape this document
keeps finding, and because the corrected run's *clean* result is what
appears above — the crash was never evidence about the engine.

### Deferring further, past settings delivery, is not coherent

`CORDIAL_DEFER_PAST_SETTINGS=1` fetches the real client-settings document
(`cordial_runtime::client_settings::load`, a genuine HTTP round trip, done
before any of libroblox.so's constructors have run) and calls
`Java_com_roblox_engine_jni_NativeGLInterface_nativeInitClientSettings`
directly on the resolved symbol, out of band, the same way the directory
setters were called. This is the direct test of the actual hypothesis this
document has carried since §26: Android's capture shows `nativeInitClientSettings`
at 0.3752 s strictly before `flagLoaded`'s successful `RbxStorage::init` at
0.4158 s, so matching that order — rather than merely setting directories —
is the natural next thing to try.

It segfaults. Deterministically: two plain runs, both exit 139
(`SIGSEGV`), both dying right after printing that the settings document was
fetched and before any further output. A third run under `lldb` caught it
precisely:

    CRASH: signal SIGSEGV: address not mapped to object (fault address=0x10)
    #3 cordial_init_client_settings
    #4 cordial_linker_sys::game_activity::init_client_settings
    #5 cordial_run::main (load.rs:1214, the CORDIAL_DEFER_PAST_SETTINGS call site)

Fault address `0x10` — a small, fixed offset from a null pointer — is the
shape of a `this`-pointer or vtable read on an object that has not been
constructed. Frames #0–#2, inside libroblox.so/jnivm with no symbol names
resolved, are not read further than that shape; doing so would mean reading
how the native implements itself rather than observing that it faults.

**So `nativeInitClientSettings` does depend on constructor-initialised
state**, unlike the four directory setters, and calling it before
`.init_array` runs is not safe. This closes off the direct route to matching
Android's ordering from outside the engine: `RbxStorage::init`'s broken call
happens *during* constructors (§26), and the settings delivery Android's
capture puts *before* a successful storage init cannot be called until
*after* constructors — so within Cordial's process, as it is built today,
there is no point at which both "constructors have not yet reached
`RbxStorage::init`" and "settings have already been delivered" are true at
once. Deferring the constructor that contains the bug cannot outrun a
dependency that sits inside the same constructor phase.

### Where this leaves candidate twenty

Coherent and inert (directories) is now measured, not inferred. Coherent and
sufficient (settings) is now measured to be impossible by this route,
specifically — not deferral in general, but deferral of *all* of
libroblox.so's construction as a single unit, which is what `do_dlopen`
offers to split. A finer split — running only whatever constructor
`nativeInitClientSettings` itself needs, ahead of the one that reaches
`RbxStorage::init` — would require knowing which entry in `.init_array`
that is, and distinguishing them means reading what each one does, which
is exactly the line AGENTS.md draws. Nobody has done that.

The retry route §26.1 left standing is unaffected by any of this and remains
the only route this document has not shown to be closed: nothing here found
or ruled out a legitimate (non-patching) way to make the engine attempt
`RbxStorage::init` a second time once settings exist. That is still open.

## §28. Candidate twenty was answered by asking it directly, and the answer is
## that §25/§26's "before `JNI_OnLoad`" is wrong — retracted here

§25/§26 concluded the failing "RbxStorage"-labelled call happens "during
`libroblox.so`'s ELF constructors — before `JNI_OnLoad`", and built that on a
backtrace containing `call_constructors`/`do_dlopen` frames. That backtrace
was real, but **it was never checked against a live `JNI_OnLoad` breakpoint**
— it was an inference from symbol names in a call chain, not a timestamp
comparison, and §25.1's own backtrace already contained the fact that
contradicts it: the empty-`stat()` chain bottoms out at `start_thread` ←
`__clone3` (`probe_stat_bt_run1.log`), i.e. a **freshly spawned thread**, not
the thread running `do_dlopen`. A constructor merely spawning a thread does
not make that thread's later work happen before `dlopen()` returns, and
nothing in this document had actually timed the two against each other.

### Directly measured: it happens after `JNI_OnLoad`, right after `nativeInitClientSettings`

Plain runs, no debugger — `CORDIAL_TRACE_PATHS=1 CORDIAL_TRACE_DLSYM=1
XDG_DATA_HOME=~/.cache/cordial-agent-gate ./target/release/cordial-run
--lib-dir ... --apk ... --host-libc --game-activity --run N`, log-line order
read directly off the child's own stdout/stderr (a single file, and the
informational milestones in it are printed by `load.rs`'s own control-flow
thread in the order it reaches them — no cross-process synchronisation
question at all for this specific comparison). Three fresh runs, three
wiped data roots, identical shape every time:

    run1  line 144  JNI_OnLoad returned 0x10006 = JNI 1.6
          line 162      nativeInitClientSettings -> 0
          line 173  [paths] tid=2028287 stat("./appData") = 0
          line 174  [paths] tid=2028287 stat("./appData") = 0
          line 176  [paths] tid=2028287 stat("") = -1
          line 177  [paths] tid=2028287 stat("") = -1
          line 178  [paths] tid=2028287 stat("") = -1

    run2  line 144 / 162 / 174-179  same shape, tid=2031369
    run3  line 144 / 162 / 173-178  same shape, tid=2031640

`task scheduler foregrounded` does not appear until line ~1160-1210 in the
same files — thousands of log-lines-worth of periodic `/sys/devices/system/cpu`
polling later — so the failing sequence is not merely "somewhere after
`JNI_OnLoad`", it is **immediately** after `nativeInitClientSettings` returns,
on a thread (a fresh tid, never seen in the log before that line) that is not
the one running `load.rs`'s own control flow. That thread is almost certainly
the same one §25.1's backtrace already caught mid-`start_thread`; this is the
first time its birth has been placed on the timeline.

**§25 and §26's "before `JNI_OnLoad`" / "during ELF constructors" framing is
retracted.** What is solid and still stands: the caller label is `"RbxStorage"`
(not `"flagLoaded"`), the getter's slow path is taken exactly once and is
memoised (§28.1 below), and the failure is the same three empty `stat("")`
calls after two successful `stat("./appData")` and one `statvfs`. What changes
is *when*: not constructor time, but a few log lines after settings delivery
— structurally much closer to Android's own `flagLoaded` timing (settings
before storage, per §26.1's capture) than previously stated, just reached by a
differently-labelled caller that still hits the same empty-path bug.

### A instrument caveat, recorded because this document has hit the shape before

The launch-under-lldb harness (`probe_gate2.py`/`probe_gate3.py` in the
scratch directory, built the way this document's own rules require —
`eLaunchFlagStopAtEntry`, breakpoints planted in the `notifylldb` stop) was
tried first and **produced a self-contradictory result**: it reported the
`JNI_OnLoad` breakpoint, then the storage getter, both within ~150ms of
`notifylldb`, with the file already containing content that the source proves
cannot exist that early — `load.rs` performs a genuine, unconditional
`std::thread::sleep(Duration::from_millis(250))` before the `late post:
postClientSettingsLoadedInitialization3 ok (after 250 ms)` line
(`crates/cordial-runtime/src/bin/load.rs:2977`), and that line was already
present in a file read 150ms after process launch. Six lldb-instrumented runs
(three with the trace env vars, three as a control without them) all showed
this same impossible compression, consistently. The most likely mechanism:
this harness fully halts every thread in the process on every breakpoint hit,
and the Python-side bookkeeping between `Continue()` calls (file reads,
prints, SBError handling) costs real wall-clock time on the *host* while the
child's own clocks are frozen — so timestamps taken from separate `Continue()`
iterations are not directly comparable to each other or to an undebugged
run's timeline, even though which thread hit which breakpoint, and in what
call-count, remained trustworthy (the getter-slowpath site fired exactly once
with label `"RbxStorage"` in all six runs, matching the plain-run finding).
**Comparing absolute or relative timestamps across separate breakpoint stops
in this specific harness shape is not reliable; log-line order in a plain,
undebugged run is what settled this section.** Filed alongside the other four
"the instrument, not the engine" retractions this document already has.

### §28.1 Categories checked pre-`JNI_OnLoad`: clean elimination, but now in a window that may not matter

The three candidate categories the last session was asked to check —
`dlopen`/`dlsym`, file-existence checks, `/proc` reads, all before
`JNI_OnLoad` — were checked directly, three plain runs, no debugger,
`CORDIAL_TRACE_DLSYM=1 CORDIAL_TRACE_PATHS=1`, boundary taken at the literal
`JNI_OnLoad returned` log line (line 144, identical in all three runs):

* **`dlopen`/`dlsym`**: exactly one of each, every run — `dlopen(libc.so, 2)`
  then `dlsym(..., getauxval)`. Nothing else. No `libcamera2ndk`,
  `libmediandk`, `libvulkan.so.1`, or `libandroid.so` lookups this early —
  those are the five `dlopen`s patch 0002's own record names, all evidently
  later.
* **File-existence / path checks**: `/proc/cpuinfo` (one `fopen`),
  `/dev/urandom` (repeated `open`/`fopen`), `/sys/devices/system/node/node1`
  (one failing `access`), and a sweep of
  `/sys/devices/system/cpu/cpu*/cpufreq/{cpuinfo_max_freq,stats/time_in_state}`
  — CPU topology and frequency-scaling discovery, nothing else. No `/system`,
  no `/data`, no `/proc/self`, nothing Android-shaped, nothing storage- or
  settings-shaped.
* **`/proc` reads**: covered by the same trace (`/proc` paths go through the
  traced `open`/`fopen`/`stat` wrappers) — only `/proc/cpuinfo` pre-`JNI_OnLoad`.

This is a clean, repeatable (3/3) negative for all three categories **in the
pre-`JNI_OnLoad` window specifically**. But §28 just showed the failing call
does not run in that window at all — it runs after `nativeInitClientSettings`,
on a spawned thread. So this negative answers the question exactly as asked,
without answering the question that actually matters now: what does *that*
thread read, in the few log lines between `nativeInitClientSettings -> 0` and
its own `stat("./appData")`, that could differ from Android. Nothing
Cordial-traceable appears there either (no `[paths]` or dlsym-trace line from
that tid before its first `stat`), which is consistent with §26.1's own
ABI-shape finding that the empty value is computed inside the callee from
state not visible at the caller's frame — not new evidence, but no longer
resting on a mistaken "before flags exist" premise.

### §28.2 The retry route: `nativeRetryInit` is already called twice, and it does not retry storage

Cordial's own default run already calls
`Java_com_roblox_client_startup_MainGameActivity_nativeRetryInit` — the one
exported native whose name says "retry" — twice: an early `retryInit ok` and,
250ms-sleep-gated, a `late retry: nativeRetryInit ok`. Both happen inside the
same six lldb runs (§28's runs, and the earlier gate2/gate3 runs, all
`--run 15`) that had the getter's slow-path call site instrumented for the
entire run. **The getter-slowpath-direct-call-site breakpoint fired exactly
once in all six runs, despite two `nativeRetryInit` calls occurring inside
that same window.** `nativeRetryInit` does not cause `RbxStorage::init` to
run again — it is a real, exported, app-facing native, already exercised by
Cordial, and it is a clean negative for this specific candidate, not an
inferred one.

The rest of the app-facing JNI surface was checked by name against
`docs/analysis/jni-natives.tsv` rather than by invoking each one — that is an
`INFERRED` elimination, not an established one. `Java_com_roblox_client_
LocalStorageManager_initStorageManagerNative`(`V3`) and the
`localstorageplatforminterface`/`IPlatformLocalStorageHandler` family
(`getSecureValue`, `setCurrentUser`, `deleteUserValues`, …) are the only other
storage-shaped exports, but their method names are user-credential/secure-value
shaped, not content-cache shaped, and `docs/traces/waydroid-roblox-startup.log.gz`
— which does exercise real login and asset loading for the session it covers —
contains no `LocalStorageManager` or `IPlatformLocalStorageHandler` line
anywhere, alongside its exactly-two `RbxStorage` lines. Consistent with "this
is a different subsystem", not proof of it; actually calling one of these
(the way `nativeRetryInit` and the settings/flags calls already are) is the
obvious next step and was not done here.

**Where this leaves candidate twenty:** the constructor-time framing that
motivated searching ELF-construction-time state is retracted — the failing
call runs after settings delivery, on a spawned thread, much like Android's
own `flagLoaded` timing. The three categories are still cleanly eliminated,
just in a window that turns out not to contain the failing call. The one
tested retry candidate (`nativeRetryInit`) is a clean negative. The
LocalStorageManager family remains an open, untested lead.

## §29. Nothing Cordial can reach is consulted at the moment of failure

Three results, each with a control.

**The directory setters, at the earliest timing that exists.** `CORDIAL_DEFER_CTORS=1`
calls all four `NativeSettingsInterface` setters *before* `libroblox.so`'s own ELF
constructors run — before `JNI_OnLoad`, before `bootstrapTheApp` can fire. They
report `ok (pre-ctors)`. **The identical three `stat("") = -1` still happen.** §27
eliminated the setters; this eliminates them again under a strictly stronger
timing condition, and it is a controlled negative rather than an inference.

**No JNI call happens at the point of failure.** Rebuilt with
`CORDIAL_JNI_TRACE=1` and run with `CORDIAL_TRACE_PATHS=1`: on the failing
thread, between `nativeInitClientSettings -> 0` and its three `stat("")`, there
are **zero** method-ID resolutions and zero invocations. Whatever supplies the
empty root is not being fetched from Java at the moment it is used.

**Angles 1 and 2 are closed by inspection rather than left open.** Every field on
`InitParams`, `StartAppParams`, `PlatformParams` and `DeviceParams` is filled, and
the only path-shaped one is `assetFolderPath`, which is the APK asset root and
unrelated. Both string arguments to `initStorageManagerNativeV3` are real,
non-empty absolute paths whose directories exist on disk before the call. The
recorded ordering bug there — the first call precedes the four setters, against
its own comment — is real and **moot**, because storage has already failed and
memoised before either call site is reached.

The stacks corroborate §28's identification: all three empty-path hits share
callers at `0x226eea1`, `0x226ec71`/`0x226f571`, `0x2315ced`, `0x2312fe3`,
`0x230c3b4`, `0x230bd04`, bottoming at `start_thread`/`__clone3`. `0x2312fe3` is
0x27 bytes past the `[INIT]` emit at `0x2312fbc`. They diverge at exactly one
frame, so it is three distinct call sites stating an empty path, not one site
retried.

### The sixth instrument artefact, and this one is subtle

The first `CORDIAL_JNI_TRACE` run appeared to show JNI activity clustered right
after the failure. It was not real. libjnivm's `LOG` writes to **stdout** via
`printf`; Cordial's path tracer writes to **stderr**. Redirected to a file,
stdout is block-buffered and stderr is not, so the interleaving in the log is not
temporal order. `stdbuf -o0 -e0` and two reruns gave the true ordering, which is
the zero-JNI-calls result above.

Six artefacts now: a path trace blind to `statvfs`, a channel sweep with the
wrong value shape, a function boundary from a prologue scan, two `lldb` attaches
arriving too late, a halting harness that could not be trusted for timing, and
now two output streams with different buffering read as one timeline. **Every
single wrong conclusion in this document has been an instrument, never the
engine.** That is the most reliable pattern here and the next person should
assume it before assuming anything about Roblox.

### Where this leaves the question

Storage fails on a root that is empty at the moment of use, and at that moment
nothing in the JNI surface Cordial implements is being consulted. So the value is
either cached from earlier than any Cordial code runs, or is an engine-internal
default nothing in the app-facing interface can reach.

The next observational angle, and it has not been tried: **trace what runs on
that spawned thread before its first `stat("./appData")`, back to `__clone3`** —
who creates the thread and what it does first. Reading the disassembly at
`0x2315ced`/`0x2312fe3` would answer it faster and is out of scope.

If that angle also comes back empty, the honest conclusion is the one §22.1
reached about the flags gate and had to un-reach: this may not be reachable
through the interface Roblox exposes to a host application, and the remaining
option is the memory write [ADR-001](../adr/ADR-001-in-process-hooking.md) makes
absent. That is a decision for the project owner, not a measurement.

## §30. The thread is a pool worker, and the answer is that this is not reachable

§29 left one observational angle untried: who creates the thread storage fails
on, and what runs on it first. `native/thread_trace.cpp` answers it — an
`pthread_create` interception behind `CORDIAL_TRACE_THREADS=1`, inert otherwise,
logging the caller and start routine as `libroblox.so+offset` read from
`/proc/self/maps`. Three fresh runs, three wiped data roots, identical shape:

    [threads] tid=… spawned by caller=libroblox.so+0x22e7935 start_routine=libroblox.so+0x230bcc2
    [paths]   tid=… stat("./appData") = 0
    [paths]   tid=… stat("./appData") = 0
    [paths]   tid=… statvfs("./appData") = 0
    [paths]   tid=… stat("") = -1        (x3)

**The thread's very first traced libc call is that `./appData` pair.** Nothing
runs on it before, which matches §29's separate finding that zero JNI calls
happen on it in the same window.

**It is a thread pool, not a storage constructor.** The same
`caller=+0x22e7935 → start_routine=+0x230bcc2` pair spawns three different
threads at three different points in one run, and only the first runs the storage
sequence; the other two produce no further path activity. That is a generic
worker-spawn site handing out a queued task — which corroborates §24's
"storage initialisation is a scheduled task" with a mechanism rather than an
inference.

Control, same session: with `CORDIAL_TRACE_THREADS` unset, zero `[threads]` lines
and the identical failure sequence. The interception neither hides nor alters it.

### The conclusion, stated with evidence rather than as a guess

Neither `+0x22e7935` nor `+0x230bcc2` is a JNI entry point, a registered native,
an exported symbol, or anything named in `docs/analysis/jni-natives.tsv`. They are
anonymous engine-internal functions reached only by address. Combined with §29's
zero-JNI-calls result, and with twenty-one candidates eliminated with controls:

**The empty value is produced and consumed entirely inside engine-internal code,
on an engine-internal pool thread, and nothing in the interface Roblox exposes to
a host application names, reaches or influences it.**

That is the ADR-001 decision point §22.1 first reached about the flags gate — and
had to withdraw, because the flags gate turned out to be reachable after all and
§23 fixed it. This time it is not a guess: every route the interface offers has
been tried and measured, and the last one was instrumented by intercepting a libc
symbol in Cordial's own shim rather than by inferring.

So the remaining option is the memory write
[ADR-001](../adr/ADR-001-in-process-hooking.md) and
[ADR-003](../adr/ADR-003-plugin-isolation.md) make deliberately *absent* — the
same thing mocktail does, one of its 98 patch functions. **That is a decision for
the project owner about Cordial's security posture, not a measurement**, and it
carries more than storage: a fork is building a script executor on this codebase,
and the primitive is absent precisely so there is nothing to extract.

### What the investigation is worth even so

Twenty-one candidates eliminated with controls, and **six wrong conclusions
retracted — every one of them an instrument rather than the engine**: a path
trace blind to `statvfs`, a channel sweep with the wrong value shape, a function
boundary from a prologue scan, two `lldb` attaches arriving too late, a halting
harness untrustworthy for timing, and two output streams with different buffering
read as one timeline.

The flags half of the same investigation *was* reachable and is fixed:
`onFlagsLoaded` fires, `areFlagsLoaded:true`, and the `NativeDataModelManager`
chain runs to `startLuaApp_`. The difference between the two halves is the whole
lesson — one gate was an ordering mistake in Cordial, the other is inside the
engine, and only measurement told them apart.

## §31. Sober does not patch, and it loads the engine from a real Android path

`tools/sober-text-check.sh`, run with root by the maintainer:

    mapping names its file; using /proc/191976/root/data/app/~~GeJ39qDZC-GggfI7DN3s8A==/com.roblox.client-RrbELHju2bHBP_kjZ6yvvA==/lib/x86_64/libroblox.so
    pid 191976  7f3aaa0f0000-7f3ab0a83000 r-xp
    compared 110701760 bytes of executable text
    DIFFERING BYTES: 0

**§30 is retracted.** It concluded the value storage fails on is engine-internal
and unreachable through any host-application interface, and that the only
remaining route was the memory write ADR-001 makes absent. Sober reaches
`RbxStorage::init [INIT] user: flagLoaded` with **zero** bytes of its engine text
altered. A legitimate route exists.

The honest limit of this reading, stated because it was stated before the test
was run rather than after: zero rules out *code* patching — mocktail's 116
`PatchCode` sites — and not a forced *data* byte, which is what mocktail's own
flags-loaded patch is. So this does not prove Sober touches nothing. It does
prove Sober is not rewriting the engine's instructions, which is what "nobody has
solved this honestly" would have looked like.

### The path is the lead

Sober's mapping names its file, and the name is an authentic Android application
layout:

    /data/app/~~<base64>/com.roblox.client-<base64>/lib/x86_64/libroblox.so

Cordial loads from `~/.cache/cordial/lib/x86_64/libroblox.so`. Nothing about that
path looks like an installed Android package.

**Hypothesis, untested at the time of writing:** the engine derives its private
data directory — the root storage is built from — by walking up from its own
library path, the way an Android app can locate `/data/user/0/<package>/` from
`/data/app/<package>/lib/<abi>/`. Given a path with no package component to find,
that derivation yields nothing, which is exactly the empty string §25.1 traced
into three `stat("")` calls, and exactly the kind of value §29 found is never
fetched over JNI because it never needed to be.

It also explains a detail nothing else has: why the two *successful* stats in the
same function are `./appData`, resolved against the working directory, while the
failing one is empty. Two different roots, one of which has a fallback and one of
which does not.

**This is testable without any memory write**, and that is the point. Cordial
already remaps `/system/<rest>` onto a host directory in
`native/system_paths.cpp`; the same machinery could present the engine with an
Android-shaped `/data/app/...` path for its own library, and an Android-shaped
private data directory beneath it. Whether the engine then finds a root is a
measurement, not an argument.

Twenty-one candidates died against the wrong question. This is the first one
aimed at the right one.

## §32. The self-path hypothesis is wrong — measured, not one route eliminated by assumption but all three tried

§31 named three ways native code can ask "where am I" before `JNI_OnLoad` and
said to establish which one the engine reads before patching one and hoping.
`patches/0004-android-libpath-experiment.patch` (applied) makes all three
observable and one of them controllable: `mcpelauncher_set_realpath`
overwrites what the linker's `soinfo` reports as a loaded library's own path
— pure metadata, nothing reopened or remapped — and `CORDIAL_TRACE_DLADDR=1`
traces every `dladdr()` and `dl_iterate_phdr()` call. Wired into
`crates/cordial-runtime/src/bin/load.rs` behind `CORDIAL_ANDROID_LIBPATH=1`:
defer `libroblox.so`'s constructors, overwrite its realpath to
`/data/app/~~cordialAAAAAAAAAAAAAA==/com.roblox.client-cordialBBBBBBBBBBBB==
/lib/x86_64/libroblox.so`, run the deferred constructors.

**`dladdr()` is called zero times.** Across two complete runs to `app ready:
Landing` (`CORDIAL_TRACE_DLADDR=1`, one with the override and one without),
`grep -c 'dladdr('` on the trace is 0, both times. The engine's guest-facing
`libdl.so` import — confirmed present via `readelf -d`, so this is not a
question of whether it could reach the symbol — is simply never called this
way, for anything, in a run that starts, loads settings, reports
`onFlagsFailed`, and reaches the third `app ready` stage.

**`dl_iterate_phdr()` is called, but never before the failure.** 698 calls in
one run, and the first one — in both runs, same log line as the last of the
three failing `stat("")` calls — comes immediately *after* `RbxStorage::init`
gives up, not before. It arrives in a burst of about twenty consecutive calls,
which is the shape of a C++ unwinder walking a stack frame by frame to build a
backtrace, not of a single lookup computing a directory ahead of time. Nothing
calls it during library load, during `JNI_OnLoad`, or at any point up to the
exact log line the empty-path failure already occupies.

**The third route was already closed.** `CORDIAL_TRACE_PATHS=1` intercepts
`open`, `fopen`, `stat`, `lstat`, `access`, `opendir`, `realpath`, `readlink`
and `statvfs` — every way to read `/proc/self/maps` or `/proc/self/exe` that
does not go through `dladdr`/`dl_iterate_phdr`. Across a full run, `/proc/self`
appears exactly once, as `fopen("/proc/self/oom_score")`, called repeatedly
for what is visibly a memory-pressure check, never `maps` or `exe`.

**The control.** With the override applied and traced (`CORDIAL_ANDROID_
LIBPATH=1` together with `CORDIAL_TRACE_DLADDR=1` and `CORDIAL_TRACE_PATHS=1`,
two repeats, both clean — `soinfo realpath overridden to …` printed, `con
structors returned without crashing` printed, `app ready: Landing` reached —
the three `stat("")` calls are byte-for-byte identical to the unmodified
baseline, same thread role, same surrounding `./appData` context, same `-1`.
An override that the engine never reads cannot change its output, and it did
not.

**So §31's specific hypothesis — derive the data directory by walking up from
a self-reported library path — is retracted.** This does not retract §31's
own evidence: Sober's engine still maps from a real, unaltered, Android-shaped
path, and that fact is untouched by this result. What is retracted is the
explanation offered for *why* the path might matter — none of the three
channels a native library has for learning its own path are used at the point
where the failure happens, so the shape of the path Cordial hands the linker
is, on this evidence, not what `RbxStorage::init` is missing. Something else
about Sober's environment — most plausibly a value delivered through JNI from
a live `Context` object (`getApplicationInfo().dataDir`, `getFilesDir()` by
method call rather than by the field contents already tried and eliminated),
rather than anything the native library can discover about itself — remains
untested and is candidate twenty-two.

## §33. Candidate twenty-two is dead too: the engine never asks for `ApplicationInfo.dataDir`

§32 narrowed candidate twenty-two to a JNI route: `Context.getApplicationInfo()`
called and `dataDir` read off the result, cached before `RbxStorage::init`'s
first attempt. `getApplicationInfo` is declared in the shipping dex
(`tools/dex_method.py`), Cordial hooks nothing for it, and a field read on an
unresolved placeholder is the exact shape of four earlier bugs here. That made
it worth asking directly, before writing a single line of implementation:
does the engine ever call it?

`native/jni_shim.cpp` and `third_party/libjnivm`'s `GetFieldID`/`GetMethodID`
log every resolution attempt — `Found symbol`, `Unresolved symbol` and
`Constructed Unresolved symbol` — when built with
`-DCORDIAL_JNI_TRACE=ON`/`-DJNIVM_ENABLE_TRACE=ON`, and log every dispatched
call besides. That is a full record of everything the engine asked libjnivm
for, not a sample.

Built with `CORDIAL_JNI_TRACE=1 cargo build --release --bin cordial-run`
(confirmed in the build log: `CORDIAL_JNI_TRACE:BOOL=ON`,
`JNIVM_ENABLE_TRACE:BOOL=ON`, and libjnivm's `internal/field.cpp` and
`internal/method.cpp` recompiled), then run twice against Sober's real APK
with `CORDIAL_JNI_TRACE=1 ./cordial-run --host-libc --game-activity --run
25` (and `--run 30` on the repeat), own data root, `stdbuf -o0 -e0` per the
buffering warning. Both runs are clean and reach `app ready: Landing`, the
same late-startup marker §32 used as its measurement window; `onFlagsFailed`
fires twice in each, matching every other run captured in this document. The
two runs' `[JNIVM]:` lines are identical as sets (606 and 603 raw lines, zero
diff after dedup) — this is deterministic, not a fluke of one run.

Across both full traces, from `JNI_OnLoad` to `Landing`:

    grep -ni 'getApplicationInfo\|ApplicationInfo\|dataDir' run.log
        -> zero real hits. The only substring matches are
           `nativeSetBaseDataDirectories` (a distinct, already-hooked
           Cordial-side call) and `ApplicationExitInfoCpp` (an unrelated
           crash-reporting class the engine does FindClass on).

    grep -n 'android/content/Context\b\|android/content/pm/ApplicationInfo' run.log
        -> zero. FindClass is never called for either class, at any point.

The 15 "did not answer" method/field constructions logged each run (listed
in full in the run output — `GameActivity.finish`, `getWaterfallInsets`,
`getWindowInsets`, `syncCookiesFromEngine`, `getAppUpgradeKey`, four
`NativeGLJavaInterface` statics, `NetworkUtils.getPublicIPv4Addresseses`,
`Class.getClassLoader`, `List.get`) contain nothing from
`android/content/pm/ApplicationInfo` or `android/content/Context`. This
matches, rather than merely fails to contradict, `docs/analysis/undefined-
symbols.tsv`, `unresolved-jni.tsv`, `unanswered-jni-observed.tsv` and
`unresolved-java.md`, none of which have ever listed it either.

**So candidate twenty-two is dead, cleanly, without implementing anything.**
The engine does not read its data directory from `Context.getApplicationInfo
().dataDir` over JNI in this build, this launch path, in either the ELF-
constructor-time attempt or any later one reached by `Landing`. Per the
project's own rule against a stub that lies, nothing was implemented for
`getApplicationInfo` — there is no observed call for it to answer, and
stubbing an unasked-for method would be exactly the kind of plausible-looking
guess this document exists to avoid.

What remains of §32's list is `getFilesDir()` by method call — a live call
on a `Context`/`Activity` object, as opposed to the field contents §23–§29
already traced and eliminated. `Context` is never even `FindClass`'d in
these runs, so if the engine reaches for its data directory by calling
`getFilesDir()`, it does so on some other object shape, or not before
`Landing`, or not by this route either. That is candidate twenty-three,
untested.

## §34. Candidates twenty-three and twenty-four, and one contradiction left open

**`nativeSetExceptionReasonFilename` — dead.** The one path-shaped native in
`jni-natives.tsv` that is exported, has no caller, and was not already accounted
for (`nativeSetBaseUrl`'s omission is explained in `load.rs`'s own comments).
Dex prototype `(Ljava/lang/String;)V`, wired temporarily into the pre-constructor
path with a distinctive marker directory and run under `CORDIAL_TRACE_PATHS=1`.
The call succeeds and prints before storage runs; storage's failure is
byte-for-byte identical to baseline and the marker string appears nowhere in the
trace. Edit reverted.

**`getFilesDir()` by method call — dead**, which closes the candidate §33 named
as untested. Two runs with the JNI trace on, to `app ready: Landing`.
`SessionReporterJavaInterface.getFilesDir()` — a hook Cordial already implements,
`android_classes.cpp` returning `files_dir()` — is *resolved* once during hook
registration and **never dispatched**: zero `Call Static Function … getFilesDir`
lines in either full trace. `Context` and `ApplicationInfo` are still never
`FindClass`'d, consistent with §33.

So the infrastructure to answer is in place before the failure — Cordial's class
and method table is fully populated by then — and the engine simply never places
a call.

### The contradiction, recorded rather than resolved

The agent that ran the above reports storage's attempt as happening "during
`libroblox.so`'s own ELF constructors, at `dlopen` time". **§28 retracted exactly
that claim**, on three clean undebugged runs, and placed the attempt on a freshly
spawned thread immediately after `nativeInitClientSettings` returns 0 — with the
observation that the earlier reading came from `call_constructors`/`do_dlopen`
frames in a backtrace that bottomed out at `start_thread`/`__clone3`.

Both cannot be right, and nothing here distinguishes them, because the two were
established by different methods and neither was re-run against the other. **It
is left open deliberately.** This document's most reliable pattern is that six
wrong conclusions were the instrument rather than the engine, and picking a
winner between two measurements on the strength of which is more recent would be
the seventh.

Whoever takes this next should settle it first, because it decides whether
anything Cordial does before `dlopen` returns can matter at all — and it is one
plain, undebugged run with `stdbuf -o0 -e0`, log ordering, and a marker printed
either side of `dlopen`.

### What is left after twenty-four

Self-path introspection is eliminated (§32), JNI is eliminated as the channel at
the moment of use (§29) and for every specific method anyone has named (§33,
§34). What has not been examined: a value the engine reads out of the APK itself
through `AssetManager`, or a genuinely internal default with no host-observable
input. The first is testable. The second would mean §30's conclusion was right
after all and only §31's evidence — Sober's byte-identical text — stands against
it, which would put this back in front of the project owner as an ADR-001
question rather than an engineering one.

## §35. The contradiction is settled, AssetManager is dead, and that is the surface exhausted

**§28 was right; §34's "at `dlopen` time" is retracted.** Markers either side of
`linker::dlopen`, three plain undebugged repeats, identical in shape:

    line  28   BEFORE dlopen(libroblox.so)
    line 135   AFTER dlopen(libroblox.so) returned
    line 162   nativeInitClientSettings -> 0
    line 174   stat("") = -1   (x3, on a freshly spawned tid)

`dlopen` returns and `JNI_OnLoad` completes long before `nativeInitClientSettings`
runs, which is itself before the failure. So the attempt is **not** during ELF
construction, and everything Cordial does before `dlopen` returns is available to
it by the time it fails.

**`AssetManager` is dead too, and finding that required fixing the instrument
first.** `android/asset.rs` traced overlay *hits* under `CORDIAL_TRACE_ASSETS`,
but the **miss** trace was gated on `CORDIAL_TRACE` — the flag AGENTS.md says
never to set, because it wraps variadics unsafely and aborts the engine. **A miss
had therefore never been observable.** That is the same shape as §23.5's path
trace not wrapping `statvfs`, and it is the seventh instrument fault this
document has recorded. All four outcomes are traced from one place now.

With it fixed: four runs, 328 asset-trace lines, byte-identical as sets across
repeats. Every miss is the engine's normal prefix probe — `android/<path>` then
`ExtraContent/<path>` then `content/<path>` — resolving under a later prefix. Of
56 distinct basenames, exactly one never resolves anywhere,
`fonts/NotoSansCJK-Regular.ttc`, and `unzip -l` confirms it is genuinely absent
from the APK rather than a Cordial gap. Nothing missing is root- or
settings-shaped.

And the timing settles it independently: **AssetManager's first request lands
about a thousand log lines after the three failing `stat("")` calls**, in both
combined runs. The channel is not touched until long after the failure has
happened and been memoised.

### Twenty-five candidates, and the surface is exhausted

Self-path introspection (§32). JNI at the moment of failure (§29) and every
specific method named (§33, §34). Every directory setter, at the earliest timing
that exists (§27, §29). Every storage flag. The settings document. The scheduler.
`getAllocatableBytes`. `statvfs`. The Android directory layout. Channel
verbosity. `getenv`. System properties. `TMPDIR`. Constructor deferral. Retry.
Init params. And now `AssetManager`.

What remains is the one thing with no host-observable input at all: an internal
default. That is §30's conclusion, which §31 knocked down and which now stands
back up — with the important difference that **§31's evidence still holds**.
Sober reaches `flagLoaded` storage with its engine text byte-identical to disk,
so *something* reaches this state without rewriting instructions. Either Sober
forces a data byte, which the text comparison cannot see and mocktail's own
flags patch proves is a real technique, or there is a route none of
twenty-five candidates has touched.

**So this is where it goes to the project owner.** Not as "we ran out of ideas" —
as a measured position: the host-application surface is exhausted, the remaining
route is the memory write [ADR-001](../adr/ADR-001-in-process-hooking.md) and
[ADR-003](../adr/ADR-003-plugin-isolation.md) make deliberately absent, and that
absence exists precisely because a fork is building a script executor on this
codebase. Storage would cost that.

### What the investigation is worth

Twenty-five candidates eliminated with controls. **Seven wrong conclusions
retracted, and every single one was an instrument rather than the engine** — a
path trace blind to `statvfs`, a channel sweep with the wrong value shape, a
function boundary from a prologue scan, two `lldb` attaches arriving too late, a
halting harness untrustworthy for timing, two output streams with different
buffering read as one timeline, and an asset miss-trace behind a flag that kills
the process.

The other half of this same investigation *was* reachable, and is fixed:
`onFlagsLoaded` fires, `areFlagsLoaded:true`, the chain runs to `startLuaApp_`.
One gate was an ordering mistake in Cordial and the other is inside the engine.
Only measurement told them apart, and it took seven retractions to trust the
measurements.

### §35.1 What Sober's successful init looks like, read with everything now known

    0.564820  [FLog::Output]            Build system id: 1
    0.565690  [FLog::AppMemUsageStatus] 923286370
    0.566499  [FLog::IxpStorageManager] Failed to open cache file for reading
    0.566545  [FLog::TombstoneCache]    Tombstone 1 … read from file
    0.566558  [FLog::TombstoneCache]    Setting holdout experiment state to: false
    0.566566  [FLog::LocalStorageHandler] Not available on the current platform.
    0.568104  [DFLog::RbxStorage] RbxStorage::init [INIT] user: flagLoaded, availableDiskSpace: 3284131840 bytes

Three things worth having on record.

**It is the same spawned-thread shape.** Sober's block runs on `80cfb4c0` and its
`RbxStorage::init` lands 1.5 ms later on `a93d86c0` — a different thread, exactly
as Cordial's failure runs on a thread spawned from a pool. So the mechanism is
not different; Sober's simply succeeds.

**`availableDiskSpace` is a live measurement, and Cordial's provider is never
asked.** Sober reports 3,284,131,840 bytes here and 1,972,330,496 in an earlier
run, so it is reading something real and varying. Android reported
60,655,730,688. Cordial's `LocalStorageManager.getAllocatableBytes` — the JNI
method whose own comment claims storage gates on it — is **never called** (§29,
instrumented). So whatever supplies that number to storage, it is not that
method, and `init_params.cpp`'s comment overstates a mechanism that does not
fire. Worth correcting there.

**Two lines Cordial never produces, still.** `IxpStorageManager: Failed to open
cache file` and the tombstone **read**. Cordial writes a tombstone — to a
relative `cache/tombstone.dat` — and no read line ever appears. Both were noted
early and set aside when the framing was "storage is never asked for". Under the
current framing, where storage *is* entered and fails on an empty root, a missing
read immediately upstream of the failure is worth one more look than it has had —
though note Sober's Ixp line is a *failure* that Sober logs and Cordial does not,
which is as likely to be a channel difference as a behavioural one.

None of this is a candidate yet. It is the shape of the successful path recorded
properly, so the next attempt starts from a comparison rather than from a guess.

### §35.2 Cordial's block skips three steps in the middle, not at the end

Channel sets compared over the window from process start to Sober's
`RbxStorage::init`. Exactly three channels appear on Sober and never in Cordial:

    [DFLog::AppPlatformQoSEmergency]
    [FLog::IxpStorageManager]
    [DFLog::RbxStorage]

The third is the failure itself. The other two matter because of *where* they sit.
Sober's order is ClientRunInfo → **QoS handler instanced** → Mimalloc → Build
system id → AppMemUsageStatus → **IxpStorageManager** → tombstone **read** →
holdout → LocalStorageHandler → storage. Cordial produces ClientRunInfo →
Mimalloc → Build system id → AppMemUsageStatus → holdout → LocalStorageHandler,
and then fails.

**So the block is not truncated at the end. It is missing three steps from the
middle** and completing everything around them, which is a different fault from
anything assumed so far.

And it is not a flag. `DFFlagEnableAppPlatformQoSEmergencyOnStartup3` is **True**
in the live document, alongside `DFIntPlatformQoSEmergencyType = 1` and a
`DFStringPlatformQoSEmergencyEndpointList` carrying about thirty endpoint
patterns. The QoS handler is enabled and Cordial still never instantiates it.

Two readings, and nothing here separates them:

* the three steps are attempted and each fails silently, in which case whatever
  they have in common is the lead — all three touch storage or the network, and
  the endpoint list is by far the largest single string in the settings document;
* or they are logged on channels Cordial's build has quiet, in which case this is
  a fourth instrument fault of the same family as the seven already recorded and
  the block is fine.

**Distinguishing them is the next measurement**, and it is cheap: the asset and
path tracers now cover misses, so instrument whether the QoS handler's
constructor and the Ixp cache open are *attempted* at all. §22.3's finding that
channel values are heterogeneous — a bare number silences a channel that wants a
severity name — makes the second reading more plausible than it looks, and the
tombstone case is the tell: Cordial demonstrably *does* open
`cache/tombstone.dat` (it is in the path trace) while never logging a read.

### §35.3 The three missing steps are not missing. Eighth instrument fault

§35.2 proposed two readings and named the measurement. It is already in the path
trace from §29, and it settles it against §35.2's more interesting reading:

    fopen("./appData/ClientSettings/IxpSettings.json")  = null
    fopen("/…/appData/ClientSettings/IxpSettings.json") = null
    fopen("cache/tombstone.dat")                         = null
    fopen("cache/tombstone.dat")                         = ok

**Both steps are attempted.** The Ixp cache open happens twice — once relative,
once absolute — and fails both times because the file genuinely does not exist,
which is precisely what Sober logs as `Failed to open cache file for reading`.
The tombstone is opened and one of those opens succeeds. Cordial does the work
and says nothing; Sober does the same work and logs it.

So `[FLog::IxpStorageManager]` and the tombstone read are absent from Cordial's
log because **those channels are quiet in this build**, not because the steps are
skipped. §35.2's framing — "the block is missing three steps from the middle" —
is wrong, and the only genuinely missing thing in that sequence is
`RbxStorage::init` itself.

That is the **eighth** instrument fault recorded here, and the fastest one to
appear and die: proposed and disproved inside an hour, by data that already
existed. The pattern is now unambiguous enough to state as a rule for this
codebase:

> **An absence in a log is not an absence in the engine.** Eight times, over
> months, every single wrong conclusion in this document has been the
> measurement rather than the thing measured — and channel absence specifically
> has produced three of them.

Which also retires channel comparison as a diagnostic here. Sober and Cordial
having different channel sets says something about which channels are enabled and
nothing about what either client did.

### The position, unchanged and now better supported

Storage is entered, on a pool-spawned thread, after `nativeInitClientSettings`
returns. It fails on a root that is empty, having done every neighbouring step
the working client does. Twenty-five candidates are eliminated with controls, the
host-application surface is exhausted, and the remaining route is the memory
write [ADR-001](../adr/ADR-001-in-process-hooking.md) makes absent — against
which stands §31's measurement that Sober reaches this state with byte-identical
engine text, so *something* gets there without rewriting instructions.

That contradiction is the whole remaining question, and it is a real one rather
than a gap in effort.

### §35.4 Sober's working store sits in `appData`, which Cordial already resolves

Where the working store actually lives, on this machine:

    ~/.var/app/org.vinegarhq.Sober/data/sober/appData/rbx-storage.db      167M
    ~/.var/app/org.vinegarhq.Sober/data/sober/appData/rbx-storage.db-wal   16M
    ~/.var/app/org.vinegarhq.Sober/data/sober/appData/rbx-storage.id
    ~/.var/app/org.vinegarhq.Sober/data/sober/appData/rbx-storage-sc
    ~/.var/app/org.vinegarhq.Sober/data/sober/appData/rbx-storage/         (dir)
    ~/.var/app/org.vinegarhq.Sober/cache/sober/rbx-storage/                (cache side)

**The root is `appData`** — the same directory that holds `ClientSettings`,
`logs` and `LocalStorage`, and the same directory Cordial resolves without
trouble: it stats `./appData` successfully twice in the failing function, and
writes its own engine log into the absolute equivalent.

So the empty root storage dies on is **not** `appData` and not the files
directory. Cordial has both, correctly, and storage still fails — which narrows
what the empty value can be rather than pointing at it.

`rbx-storage.id` is worth noting for whoever picks this up: a separate identity
file beside the database. Whether storage needs an identity that is empty here is
**untested**, and it is written down as an observation rather than proposed as a
candidate, because the last two candidates proposed at this level of evidence
both died within the hour.

### Closing position

Storage is entered, on a pool-spawned thread, after `nativeInitClientSettings`
returns 0. It performs every neighbouring step the working client performs —
including the Ixp cache open and the tombstone open, both confirmed attempted in
§35.3. It fails on a root that is empty, and that root is demonstrably neither
`appData` nor the files directory.

Twenty-five candidates eliminated with controls. Eight instrument faults, three
of them channel absence. The host-application surface is exhausted, and against
the conclusion that would follow stands §31: Sober reaches this state with
byte-identical engine text, so something gets there without rewriting
instructions.

**That contradiction is the question.** Resolving it means comparing Sober's
engine *data* at a known point, which is a different instrument from anything
built here, and it is the honest next step rather than a twenty-sixth candidate
drawn from an exhausted surface.

## §36. Storage has worked. Under Cordial. And it logged nothing

`/var/home/neilluo/.cache/cordial-agent-t/cordial/profiles/default/data/files/appData/`
contains, right now:

    rbx-storage.db        49152 bytes, a real SQLite database
    rbx-storage.db-wal
    rbx-storage/          p14  p15  p16  p19  p20  p30  p36  p5

Eight partition directories. Sober's working store has the same structure
(`p14 p15 p16 p19`). **These are engine-created**: the directory pre-creation
added in §23.5 makes a single empty `rbx-storage/` and nothing inside it.

Created at 17:45:25 on 19 August, which pins it to one run — the engine log
`2.734.0.917_20260819T074525Z_Player_ec3a3_last.log`, from the
`CORDIAL_LATE_POST_MS=2000 CORDIAL_LATE_RETRY=1 --run 35` repeats.

**That log contains zero `RbxStorage` lines.**

### What this retracts, which is most of this document's evidence base

Storage initialised — built a database, built eight partitions — and did not emit
one line on `DFLog::RbxStorage`. So every conclusion of the form "storage never
runs, because no `RbxStorage::init` line appears" was reasoning from an absence
on a channel that is silent **even when storage succeeds**.

> **Superseded by §41 and §44.** The claim in this paragraph that the channel is
> silent even on success is wrong, and it was load-bearing for a lot of scoring.
> `DFLog::RbxStorage` logs both ends of a successful init at **Critical**, which
> prints irrespective of channel level — Sober's log shows it. The belief came
> from Cordial's one working run logging nothing, and one silent success does not
> establish a silent channel. Separately, §44 identifies `DFLog::RbxmFileManager`
> as an on-by-default one-line-per-run marker that scores whether the store came
> up, and it retro-scores every engine log already on disk.


That includes the framing of §§12–24 and the confident negatives in §29–§35. The
*mechanical* findings survive — the empty `stat("")` triple is real and observed,
the timing in §28 is measured, the eliminations of `getenv`, system properties,
self-path and `AssetManager` are real measurements of real channels. What does
not survive is the conclusion those were pointed at. **The right instrument for
storage was always the filesystem, and it was used twice: §23.4 found these files
and could not reproduce them, and this section identifies the run.**

Ninth instrument fault, and by far the most expensive. Eight of the nine have
been an absence read as evidence.

### What is now actually open, and it is tractable

Storage is not unreachable. It has been reached, by this client, on this machine.
`rbx-storage.id` is 8 random bytes — an engine-generated identifier, an output
rather than an input, so not a lead.

The run that worked differed from the fresh-root controls in §23.4 in two ways
that were never separated: a 2000 ms late-post delay rather than the default 250,
and a data root with substantial accumulated state — a flag cache, a tombstone,
and 43 `ContentProvider_*` directories from earlier sessions. The three-launch
warm-root test in §23.1 used the default delay, so **delay and warmth have never
been varied against each other.**

That is a two-by-two matrix, four runs, checked on the filesystem and not on the
log. It should be the next thing anybody does, and it should have been the first.

### §36.1 The matrix is flat, and so is the flag

§36 named the two-by-two nobody had run. Run, on fresh roots, measured on the
filesystem rather than the log:

    delay=250   pass=1   db=0  partitions=0
    delay=250   pass=2   db=0  partitions=0
    delay=2000  pass=1   db=0  partitions=0
    delay=2000  pass=2   db=0  partitions=0

Neither the late-post delay nor a second warm pass reproduces it.

The timeline then pointed at the storage flags — the `.db` in the working root
appeared at 17:46:02, which matches the run that set
`FFlagStartRbxStorageInitRighAfterFlags` and `DFFlagRbxStorageInitLatch`, a run
dismissed at the time because its **log** showed nothing, which §36 has just
established means nothing at all. Retested properly, fresh root, three passes,
long delay:

    flag+2000  pass=1  db=0  partitions=0
    flag+2000  pass=2  db=0  partitions=0
    flag+2000  pass=3  db=0  partitions=0

(The eight `rbx-storage` paths those runs do have are the empty directories
§23.5's layout creates, not engine output.)

So the reproduction is still not found. What the working root has that none of
these do is **43 `ContentProvider_*` directories** — the residue of real content
being fetched across many sessions. Every attempt here has been a short run at
the landing page, and short runs at the landing page do not fetch much.

**The untested variable is therefore sustained content activity**, not
configuration: a session that actually loads an experience and pulls assets,
against a fresh root, checked on the filesystem. That is one join run and nobody
has done it with the filesystem as the instrument — every join in this
investigation was scored on `grep -c 'RbxStorage::init'`, which §36 proves is
blind.

That is the next measurement, it is cheap, and it is the first one in a long
while that is aimed at something the working case actually has.

### §36.2 Content activity is not it either, and the reproduction is not found

§36.1 named sustained content activity as the untested variable, on the grounds
that the working root had 43 `ContentProvider_*` directories where every
reproduction attempt had almost none. The test profile settles it without another
run:

    ~/.local/share/cordial/profiles/CordialTest
      ContentProvider_* directories:  91
      rbx-storage.db:                  0
      engine-created partitions:       0

Ninety-one, more than twice the working root, across many real signed-in sessions
including joins — and no store. **Content activity is not the variable.**

So: the delay is not it, a second warm pass is not it, the storage flags are not
it, and content activity is not it. The working root's store exists, was created
at a known second by a known run, and **has not been reproduced** by any
configuration tried.

What that root has which none of the others do is simply *more of everything* —
around twenty runs under a dozen different environment combinations, several of
which crashed partway. A store created by a run that died mid-way through some
other experiment would explain both the existence and the difficulty of
reproducing it, and would mean the trigger is a state Cordial reaches
occasionally rather than a setting.

**That is speculation and is labelled as such.** It is not a candidate, it has no
measurement behind it, and this document has already recorded nine faults
produced by exactly this kind of reasoning being written down as though it were
evidence.

### Where this genuinely stands

`RbxStorage` **is reachable** — §36 is a real store with a real database and
eight engine-created partitions, made by this client. What is not known is what
made it happen, and the log channel that would say is silent even on success, so
every score taken from it across this document is void.

> **Superseded by §41 and §44.** The claim in this paragraph that the channel is
> silent even on success is wrong, and it was load-bearing for a lot of scoring.
> `DFLog::RbxStorage` logs both ends of a successful init at **Critical**, which
> prints irrespective of channel level — Sober's log shows it. The belief came
> from Cordial's one working run logging nothing, and one silent success does not
> establish a silent channel. Separately, §44 identifies `DFLog::RbxmFileManager`
> as an on-by-default one-line-per-run marker that scores whether the store came
> up, and it retro-scores every engine log already on disk.


The one instrument that works is the filesystem. It has now been pointed at the
delay, at warmth, at the storage flags, and at content activity, and all four are
negative. Whoever continues should keep using it and should not trust a single
`grep -c 'RbxStorage::init'` in this file.

### §36.3 The working store was used, not merely created

Opened it. `rbx-storage.db` carries one table and it has content:

    CREATE TABLE files (id BLOB PRIMARY KEY NOT NULL, content BLOB,
      size INTEGER, hits INTEGER, atime INTEGER, category INTEGER, score …)
    CREATE INDEX files_atime_idx / files_size_idx / files_category_idx / files_score_idx

    rows: 9

Nine cached entries with real `content` blobs — sizes 786, 787, 1673, 1786, 2382,
259040 and 8 bytes, across categories 1, 10 and 11, all sharing one `atime` of
`1787125562730` (the creation moment). Several carry `RBXH` magic, so these are
Roblox's own cache records rather than empty rows.

So this was not a directory tree that happened to appear. **The engine
initialised its content store, opened a database, created eight partitions,
categorised nine assets and wrote them.** Whatever the trigger is, it produced a
fully working store, once.

The schema is worth having for whoever continues, because it says what the store
is for and what a successful run should leave behind: content addressed by an
opaque id, with hit counts, access times, categories and a score — an eviction
cache. A successful reproduction will show rows here, not just files on disk.

### The final state of this question

Reachable — proven twice over now, by structure and by contents. Not reproduced,
against the delay, warmth, the storage flags and content activity, all measured
on the filesystem. And the log channel that would say what happened is silent
even on success, which voids most of the scoring in this document.

> **Superseded by §41 and §44.** The claim in this paragraph that the channel is
> silent even on success is wrong, and it was load-bearing for a lot of scoring.
> `DFLog::RbxStorage` logs both ends of a successful init at **Critical**, which
> prints irrespective of channel level — Sober's log shows it. The belief came
> from Cordial's one working run logging nothing, and one silent success does not
> establish a silent channel. Separately, §44 identifies `DFLog::RbxmFileManager`
> as an on-by-default one-line-per-run marker that scores whether the store came
> up, and it retro-scores every engine log already on disk.


Nine instrument faults, eight of them an absence read as evidence. That is the
finding this file is actually worth, more than any individual candidate: **in
this codebase, when a measurement says nothing happened, suspect the
measurement first.** It has been right eight times out of nine.

## §37. The working run was misattributed by one launch, and twenty-three more attempts stay negative

§36 pins the store to `…T074525Z_Player_ec3a3_last.log` because that is the
nearest-named log to the file's creation. **That attribution is off by one
launch.** Both logs still exist in `cordial-agent-t` and were read directly:

    ec3a3  starts 07:45:25.241Z, last line 07:45:46.131Z (clean APP_CMD_DESTROY)
    08d78  starts 07:45:47.245Z, last line 07:46:06.425Z (clean APP_CMD_DESTROY)
    files.atime in rbx-storage.db: 1787125562730 ms = 07:46:02.730Z

07:46:02.730Z falls inside `08d78`'s window and **16.6 seconds after `ec3a3` had
already torn down**. The process alive at the instant the row was written is
`08d78`, not `ec3a3`. Neither log shows a crash — both reach the same clean
`APP_CMD_PAUSE → …→ APP_CMD_DESTROY → unLoad` sequence — so this is a correction
to which launch gets credited, not a new lead: a channel-stripped diff of the two
logs (`diff` after normalising timestamps/thread ids/pointers) shows no
behavioural difference between them beyond nondeterministic addresses and HTTP
response numbering. Whatever separates the run that worked from the one right
next to it left no trace in either log.

**Twenty-three more reproduction attempts, today's HEAD (`v0.5.2-159-g14861e0`,
some built `-dirty` while other agents held `load.rs`/`webview.rs`), scored on
the filesystem, all negative:**

    fresh root, default delay, --run 30:              6 runs, db=0
    warm root (same profile, runs 2-13 above):        6 runs, db=0
    forced SIGKILL at 3s/6s/6s/10s (simulated crash):  4 runs, db=0
    warm root after freeing disk (below), --run 30:    5 runs, db=0
    (running total in this root: 23, all clean-exit or forced-kill, none
    reproduced)

Killing the process outright (SIGKILL, not a real engine crash) at four
different points in startup does not trigger it either, which weakens §36.2's
"a run that died mid-way" speculation without settling it — an external SIGKILL
and an internal crash are not the same event, and the working root's "several
crashed partway" was never characterised beyond a run count.

### A variable nobody had checked: the disk was nearly full

`/var/home` measured **3.9 GB available on a 728 GB volume (100% used)** at the
start of this session — found only because `RbxStorageAvailableDiskSpaceTriggerMB`,
`RbxStorageMinAvailableDiskSpaceMB`, `RbxStorageLowDiskSpaceMB`,
`RbxStorageCapacityLowMB/HighMB/FixedMB` and
`RbxStorageMinAvailableSpaceBeforeCleanupMB` all turned up scanning the binary's
string table for `RbxStorage`, none of which the delay/warmth/flags/content
matrix in §36.1–§36.2 had touched. `just clean` (no `all`, so it kept `target`)
reclaimed `target-toolbox` and raised availability to ~6.6–6.8 GB. Five more runs
at that level still did not produce a store, so low headroom is at most a
contributing factor and not, on its own, sufficient — though 6.8 GB may still be
below whatever threshold gates it, and this machine is shared with other agents'
concurrent builds, so the number moves under you.

**`getAllocatableBytes` — the Java hook this project already answers via
`statvfs(".")` (§ near `native/init_params.cpp`'s `LocalStorageManager`) — is
never called on a plain landing-page run.** Checked two ways: `CORDIAL_ANDROID_TRACE=1`
shows no call, and a temporary `fprintf` was added directly to
`LocalStorageManager::getAllocatableBytes`, built, run, and printed nothing over
several runs (`cargo build --release` and `cargo test --workspace` both passed
before removing the print; the removal was verified with `git diff --stat`
showing no residual change). So whatever `RbxStorage::init`'s `AssetProvider`
caller declines on (§23.6) is not asking Cordial through that surface.

It does ask the kernel directly: `CORDIAL_TRACE_PATHS=1` shows the engine calling
raw `statvfs()` itself, three times, on `./appData` and the profile's
`data/files` directory, independent of any hook Cordial answers. That call
already sees this machine's real, currently-thin headroom — nothing here spoofs
it — which keeps disk space a live candidate for §23.6's still-open question
even though the JNI route is ruled out as the channel.

### Where this leaves §23.6's open question

Unchanged in substance: which branch `RbxStorage::init`'s `AssetProvider` caller
takes on entry, and what condition byte selects it, is still not established.
This section narrows the search — the condition is not read through
`getAllocatableBytes`, and disk headroom by itself (delay 3.9→6.8 GB) did not
flip the outcome — without answering it. §23.6's own next step, a breakpoint on
each branch target followed by a hardware watchpoint on the condition, is still
the correct next move and is still undone.

Tally for whoever continues: over forty controlled negative reproductions across
this file (delay, warmth, the two storage flags, content activity, disk
headroom, and four simulated crashes) against exactly one positive — this
session confirmed that positive is real (independently reopened `rbx-storage.db`,
9 rows, real content, matches §36.3) — now attributed to `08d78` rather than
`ec3a3`. The trigger is still not known.

## §38. The empty value is written twice: a working `./appData` is built, then wiped, with no host call between

§35's own next step, taken literally: `RbxStorage::init`'s entry (`0x23121ae`,
bounds from `.eh_frame`, per the brief that opened this) is a real function
boundary, so poking it directly needs no disassembly. It fires exactly once per
run, on the same pool-spawned thread the three `stat("")` calls already put on
record, `eStopReasonSignal` as §10 says a manual poke reports — never
`eStopReasonBreakpoint`, checked explicitly this time rather than assumed.

**The buffer's address is fixed relative to the stack pointer at that instant.**
`path_ptr − sp_at_init_entry = −0x3b7`, measured on two calibration runs and then
used as a predictor on two more: in every case the address it names is exactly
the one the three `stat("")` breakpoints (§26's own sites, `0x226eea1` etc.,
reproduced here unchanged) later read as `real`. So a write watchpoint can be
armed on the right byte *before RbxStorage::init's body has executed a single
instruction of its own* — earlier than any of this document's previous
watchpoints, which could only be placed once the empty value was already
observed.

**Two writes happen before the first `stat("")`, both on the pool thread, both
inside the same immediate caller frame (`0x23125ef`, called from `0x230c3b4` ←
`0x230bd04` — frames #6/#7 already on record from §26/§28):**

    0x226ea27  (via 0x2315dd0 ← 0x23125ef)   writes "./appData\0"
    0x23160ec  (directly in 0x23125ef, right after the call above returns)
                                              overwrites it with zero

The first write is not a guess at content — it is the literal bytes read off the
watched address, byte-identical across three separate runs:
`12 2e 2f 61 70 70 44 61 74 61 00 …`, nine characters plus terminator, and the
byte immediately before the data pointer reads `0x12` — `9 << 1`, the libc++
short-string-optimisation size tag for a 9-byte string in the "short" (non-heap)
representation. This is the *same string* Cordial's `stat("./appData")` already
succeeds on twice in this same function (§25). A correct candidate is built and
then discarded, in the same frame, by the very next thing that frame does.

Only after both writes does control reach the three `stat("")` sites this
document has had since §26. All three confirm the buffer is empty by the time
it is used — consistent with everything §26–§37 already measured, now with the
write that produces the emptiness identified rather than inferred from its
absence.

### No Cordial-owned filesystem shim runs between the write and the wipe

`s_stat`, `s_lstat`, `s_access`, `s_opendir`, `s_realpath`, `s_readlink`,
`s_open` and `s_statvfs` were all breakpointed — host code, so these resolve
normally, unlike anything inside `libroblox.so` — armed the instant
`RbxStorage::init`'s entry was caught, before either write happens. Two full
runs recorded the whole sequence end to end: the earliest host call of any kind
(an unrelated `s_lstat`, on a different thread entirely) fires roughly ninety log
lines *after* the wipe, never inside the window. Whatever discards `./appData`
does so without asking the host anything Cordial answers through the symbol
table — not `getAllocatableBytes` (already ruled out, §37), not a path check,
not a directory setter, nothing that would show up as a call out of
`libroblox.so` at all in this window.

**Measured four times.** Three clean runs reproduce the write-then-wipe
sequence byte-for-byte and address-for-address once the ASLR base is subtracted
out (two of them additionally confirmed the no-host-call result). A fourth run
took an unrelated signal before ever reaching `RbxStorage::init`
(`roblox_off 0x226e947`, a different address family, not the poke) — not scored,
consistent with the occasional crash this document has always set aside without
characterising rather than a new finding about the poke itself.

### What this settles, and what it does not

This is §35's contradiction addressed from the write side rather than the read
side. Sober's engine text is byte-identical to Cordial's (§31) because nothing
here is a missing instruction: the code that builds a correct `./appData`
candidate runs in both, measured directly for the first time rather than assumed
from binary equality. What differs is that something immediately downstream
discards it, and that discarding is not conditioned — in this window, by this
measurement — on anything Cordial supplies. Disk headroom (§37), the storage
flags (§36.1) and content activity (§36.2) were already ruled out as gating
variables; none of them appear as a call here either, which is a second,
independent way of ruling them out for this specific step.

**Engine-internal, not a host input Cordial withholds — for this step
specifically.** The question §35 left open — does the empty value come from
something Cordial can supply, or is it discarded inside the engine with nothing
to hand it — is now measured rather than inferred: the wipe happens with no
observable host call in the window, on two independently instrumented runs. Per
the rule this document has needed nine times already, that is not proof against
*any* earlier host influence at all — something read before `RbxStorage::init`'s
own entry could still have set a flag the wipe branches on, and this measurement
cannot see back past the function boundary it started at — but it is a direct
answer to the specific question this section set out to ask, not another
channel-absence argument.

No fix follows from this, and none should be attempted: the wipe is a write
inside `libroblox.so`'s own text, and
[ADR-001](../adr/ADR-001-in-process-hooking.md)/[ADR-003](../adr/ADR-003-plugin-isolation.md)
keep patching it out of scope for the same reason a script executor is out of
scope — the absence is deliberate, and this finding does not change that. What
this section adds is that the "twenty-sixth candidate" §35 predicted was not a
missing host answer: it is a specific, located, four-times-reproduced pair of
writes inside the engine, and nobody needs to guess at its shape again. Whoever
continues from here and wants to go further has one narrower, harder question
left: what, read *before* `0x23121ae`, the branch at `0x23160ec` is conditioned
on — which means placing a watchpoint on whatever that branch reads, not on the
path buffer, and accepting that the read may predate this function entirely.

## §39. Local storage is unavailable because Cordial skips `setPlatformImpl`, and §38's "engine-internal" framing is superseded

§38 ended by proposing a disassembly: find what the branch at `0x23160ec` is
conditioned on. **Do not start there.** Asking the running engine what it thought
was wrong answers it directly, and the answer was in its own log the whole time.

A landing-page run with `AndroidEnableLocalStorage` overridden to `"true"`
(`<profile>/flags.json`, applied — `flags: 1 override(s) applied`) still produced
no store: all eight pre-created `rbx-storage` directories empty, no `.db`
anywhere in the root. That is negative reproduction forty-something and, taken
alone, would have been one more line in §37's tally. What makes it useful is the
engine log beside it:

    t=1.378s  Warning [DFLog::RbxmFileManager]    LocalStorageManager is not available.
    t=2.848s  Warning [FLog::LocalStorageHandler] Not available on the current platform.
    t=3.352s  Warning [FLog::LocalStorageHandler] Not available on the current platform.
    t=3.824s  Warning [DFLog::RbxmFileManager]    LocalStorageManager is not available.

"Not available on the current platform" is what a missing **platform
implementation** reports, and Cordial's own startup log says so in as many words,
three lines below `initStorageManagerNativeV3 ok`:

    setPlatformImpl skipped (measured to crash the process a few calls later;
    set CORDIAL_LOCAL_STORAGE_SET_PLATFORM_IMPL=1 to try it anyway)

So the sequence §26–§38 spent nine sections characterising — a correct
`"./appData"` built and then wiped — sits **downstream of a platform impl the
engine was never given**. §38's measurement is not wrong: no Cordial-owned
filesystem shim is called between the write and the wipe, and that remains true.
Its *framing* is what this supersedes. "Engine-internal, not a host input Cordial
withholds" reads as "there is nothing to supply", and there is: `setPlatformImpl`
is a host input, it is withheld, and it is withheld deliberately by a line of
Cordial's own that predates this whole investigation. The wipe was measured
inside a function that had already been told this platform has no local storage.

This is the ninth time in this document that an absence was read as evidence
about the engine rather than as evidence about Cordial's own instrumentation or
defaults, and it is the most expensive: the "no host call in the window" result
is *true*, was measured four times, and still pointed the wrong way, because the
window it measured began after the decision had been made.

### What happens when it is not skipped

`CORDIAL_LOCAL_STORAGE_SET_PLATFORM_IMPL=1`, same root, same run length:

    setPlatformImpl ok
    [JNIVM]: Exception with Message `djinni (djinni_support.cpp:529): weakRef` was thrown   (x10)
    RBXCRASH: JNI: Crashing due to unhandled Java exception
    exit 133 (SIGTRAP)

Still no store — so **this section does not fix `RbxStorage`**, and nothing here
should be read as claiming it does. What it establishes is that the skip is real,
that lifting it changes the failure from a silent unavailability into a loud
crash, and that the crash has a named mechanism rather than being the
"occasional crash this document has always set aside".

`third_party/libjnivm/src/jnivm/vm.cpp:313`'s `NewWeakGlobalRef` returns
`(jweak)nullptr` whenever `JNITypes<std::shared_ptr<Object>>::JNICast` of the
object it is handed yields no strong pointer, and djinni asserts on a null weak
reference. That the two match is `INFERRED` — the message text and the null
return are consistent, but nobody has yet watched that cast fail on the specific
object `setPlatformImpl` passes. Establishing that is the next step, and it is a
question about libjnivm and djinni, not about `libroblox.so`'s text.

> **Retracted by §40.** That inference is wrong. `NewWeakGlobalRef` is never
> called in a run that throws `weakRef` thirteen times, which a print inside it
> showed on the first attempt. djinni builds a real `java.lang.ref.WeakReference`
> object instead, and libjnivm had no implementation of that class. The
> paragraph is left standing because the reasoning in it is the exact shape this
> document keeps warning about: consistent, plausible, and never run.

### What this changes for whoever continues

The remaining question is no longer "what condition byte selects the wipe". It is
"why does djinni's weak reference to Cordial's platform impl come back null", and
that is answerable in code both projects can read, with a debugger that works,
against sources that are not stripped. `patches/` is where a libjnivm fix would
go — the submodule points at a repository this project cannot push to, and
anything Cordial calls that exists only under a patch must be a weak symbol with
a null check naming the missing patch, per `native/shim.cpp`.

Whether fixing it produces a store is not established and should not be assumed.
Forty-odd negatives say the trigger has resisted every explanation offered so
far, and "the platform impl was missing" is an explanation for the *warnings*,
measured, not yet an explanation for the *store*.

## §40. `setPlatformImpl` works and is now on by default; the weak reference was a missing `java.lang.ref.WeakReference`, not a broken `NewWeakGlobalRef`; storage still does not initialise and the engine says why

§39 left one question: "why does djinni's weak reference to Cordial's platform
impl come back null". It also offered an answer, labelled `INFERRED` —
`third_party/libjnivm/src/jnivm/vm.cpp:313`'s `NewWeakGlobalRef` returning
`(jweak)nullptr`. **That inference is wrong, and this section retracts it.**

The way to find out was to put a print in `NewWeakGlobalRef` and run. In a run
that threw `djinni (djinni_support.cpp:529): weakRef` thirteen times and died on
SIGTRAP, **that function was never called once**. Neither was the other place a
dead weak reference surfaces — `UnpackJObject`'s `weak->wrapped.lock()` failing —
which was instrumented in the same build. libjnivm's weak-reference support is
not involved in this failure at all.

What is involved is visible in a `CORDIAL_JNI_TRACE=ON` run, in the twelve lines
between the last `IPlatformLocalStorageHandler` method binding and the first
throw:

    FindClass java/lang/System
    Constructed Unresolved symbol, Class=`java/lang/System`,
      StaticMethod=`identityHashCode`, Signature=`(Ljava/lang/Object;)I`
    Call Unknown Static Function Class=`java/lang/System` Method=`identityHashCode`
    Call Unknown Static Function Class=`java/lang/System` Method=`identityHashCode`
    FindClass com/roblox/.../ILocalStorageHandlerCore$CppProxy
    ...
    FindClass java/lang/ref/WeakReference
    Constructed Unresolved symbol, Class=`java/lang/ref/WeakReference`,
      StaticMethod=`<init>`, Signature=`(Ljava/lang/Object;)Ljava/lang/ref/WeakReference;`
    Constructed Unresolved symbol, Class=`java/lang/ref/WeakReference`,
      Method=`get`, Signature=`()Ljava/lang/Object;`
    Call Unknown Static Function Class=`java/lang/ref/WeakReference` Method=`<init>`
    FindClass java/lang/Error

djinni does not reach for JNI weak global references here. It builds a real
`java.lang.ref.WeakReference` **object** and keeps that — a class libjnivm does
not implement, so its constructor was an invented stub, the stub returned null,
and `DJINNI_ASSERT(weakRef, ...)` failed on every later call into the interface.
`FindClass java/lang/Error` on the next line is djinni fetching the class it is
about to throw. `System.identityHashCode` is unresolved in the same window for
the same reason; djinni keys its proxy cache on it, and a stub answering 0 for
every object collapses that cache onto one bucket.

Both are now answered in `native/local_storage.cpp`. The reference is a genuine
`std::weak_ptr`: libjnivm has no collector, so a `WeakReference` holding a strong
pointer would pin every object djinni ever wrapped and would never report a
referent gone.

### The second bug, which is general and was silent

Registering those two classes did not work at first, and the reason is worth more
than the fix. libjnivm keeps **one class per C++ type** — `VM::typecheck`, keyed
by `typeid` — and every signature it derives for a hook is built from that map.
`register_shared_preferences` in `native/android_classes.cpp` calls
`env->GetClass<Object>(klass)` for three names in a loop, so the last one won and
`typeid(jnivm::Object)` was left pointing at `android/app/Application`. Printing
the registered method table straight out of the class showed it:

    method static=1 native=0 name=<init> sig=(Landroid/app/Application;)Ljava/lang/ref/WeakReference;
    method static=1 native=0 name=identityHashCode sig=(Landroid/app/Application;)I

against an engine asking for `(Ljava/lang/Object;)...` in both cases. **This
failure mode is silent by construction**: the trace reports `Constructed
Unresolved symbol` with the signature the *engine* asked for, which is the
correct one, and nothing anywhere says the registered side spelled it
differently. Any hook taking or returning a plain `Object` and registered after
`register_shared_preferences` was affected. `register_shared_preferences` now
puts `java/lang/Object` back at the end.

### What it changed, with a control

Same build, six runs, one profile root each, 25 s apiece.

| | djinni `weakRef` | `FLog::LocalStorageHandler` "Not available on the current platform" | `DFLog::RbxmFileManager` "LocalStorageManager is not available" | exit | `rbx-storage.db` |
|---|---|---|---|---|---|
| `setPlatformImpl` made, x3 | 0 | **0** | 2 | 0 | none |
| `setPlatformImpl` skipped, x3 | 0 | **2** | 2 | 0 | none |

Before the fix, the same call on the same tree gave 13 `weakRef` exceptions and
exit 133, reproduced twice. That warning disappearing is the only direct evidence
the engine accepted the implementation rather than taking it and failing quietly,
and it is why `crates/cordial-runtime/src/bin/load.rs` now makes the call
unconditionally instead of behind `CORDIAL_LOCAL_STORAGE_SET_PLATFORM_IMPL`.
Three further runs on the new default path: exit 0, `setPlatformImpl ok`, no
djinni exception, no `LocalStorageHandler` warning.

### And it does not produce a store

**Read this before treating §39 or this section as progress on `RbxStorage`.**
There is still no `.db` anywhere under the profile root, on any of the nine runs
above. `DFLog::RbxmFileManager` `LocalStorageManager is not available` appears
twice a run whether the platform impl is handed over or not — unchanged, which
is the point. That message names `com.roblox.client.LocalStorageManager`, a
different class from the `ILocalStorageHandlerCore`/`IPlatformLocalStorageHandler`
pair this section fixes, exactly as `native/local_storage.cpp`'s own header has
said since it was written.

One more measurement narrows it. In a full `CORDIAL_JNI_TRACE=ON` run that
reached `engine initialised` and `app ready: Startup` — well past both
`RbxmFileManager` warnings, which land at t≈1.1 s and t≈3.4 s — the engine never
asks libjnivm for `com/roblox/client/LocalStorageManager` at all. Not
`getAllocatableBytes`, which Cordial hooks; not a constructor; nothing. Thirty-nine
distinct classes are looked up in that run and it is not among them. So whatever
decides "LocalStorageManager is not available" decides it without asking the
platform, and **adding methods to that class cannot be the fix**. That is a
negative result and it closes off the most obvious next thing to try.

The count of negatives is now forty-something plus nine. What §39 and §40
together establish is that two of them had causes, both in Cordial, both
findable by running the thing rather than reading the binary — and that the
store's own gate is still unexplained and is no longer where anyone has been
looking.

## §41. `RbxStorage::init` is never called under Cordial, and two things this file believed are wrong

Sober is on this machine, its store works, and nobody had read its log. Doing
that answers in one line what §26–§40 approached from the inside:

    5.460278  Critical [DFLog::RbxStorage] RbxStorage::init [INIT]
              user: flagLoaded, availableDiskSpace: 20957790208 bytes, elapsed: 0.038 ms
    5.520204  Critical [DFLog::RbxStorage] RbxStorage::init [DONE]
              name: MultiCache(TelemetryCache(SqliteCache+TelemetryCache(FileCache(temp))),
              TelemetryCache(FileCache(perm)), TelemetryCache(FileCache(session_scoped))),
              duration: 57.007 ms, dbOpenCount: 130

**Two corrections fall straight out of that, both to claims this file and
AGENTS.md have been relying on.**

**`DFLog::RbxStorage` is not silent on success.** AGENTS.md states it is, and
that claim is what "voids every `grep -c 'RbxStorage::init'` in the repo".
Sober logs both ends of a successful init at **Critical**, which prints
irrespective of channel level. The claim came from Cordial's one working run
logging nothing, and one silent success does not establish that the channel is
silent — it establishes that that run did not log, which is a different and
much smaller fact. Greps for `RbxStorage::init` are meaningful after all.

**"Not available on the current platform" is a red herring.** §39 built a case
around it and §40 fixed the crash that lifting the skip caused. Sober's log
contains that same `FLog::LocalStorageHandler` warning, once, in a run whose
storage initialises correctly 2 ms later. So the warning does not gate storage
and never did. §40's work stands on its own — `setPlatformImpl` genuinely
crashed, the djinni `WeakReference` gap and the stolen `typeid` are real bugs
now fixed, and the fifth name-and-descriptor instance was worth finding — but
none of it was the storage blocker, and §39's framing of that warning as the
cause is withdrawn.

### What is now established

Cordial's engine log contains **zero** `RbxStorage` lines, at any severity, in
a run with `DFLogRbxStorage` forced to 7 as well as in runs without it. Since
Sober's init logs at Critical, absence here is not a channel-level artifact:
**`RbxStorage::init` is not being called at all.** Every section from §26
onward measured the behaviour of a function that never runs, which is why the
write-then-wipe sequence looked unconditional — it was reached, four times, on
a path that does not lead to initialisation.

Sober's trigger is named in its own line: `user: flagLoaded`. The two lines
before it are `nativeInitializeNativeFlags: Registered Flag Provider ID from
Java: 0` and `flagCount = 0`, on a different thread from the init itself.

### What is not established, and must not be assumed

Cordial calls `FlagJniInterface.nativeInitializeNativeFlags` — same exported
symbol, confirmed present — and its log shows 59 `... N: <name> not found.`
lines but **no** `Registered Flag Provider ID from Java` and no `flagCount =`.
It is tempting to read that as "Cordial never registers a provider, so
flagLoaded never fires". **Do not.** Sober's two lines are captured at debug
level and Cordial's at Info, so their absence here may be a logging artifact
rather than a behavioural difference. This file has read an absence as
evidence nine times and been wrong; establish it with something that does not
depend on log level before building on it.

Note also that "not found" is normal: `docs/traces/native-flag-names.txt`
records the real Android client passing 139 names and getting misses among
them. Sober passes zero and its storage still works, so the count is not the
variable either.

### The next step

Find what emits the engine's internal flags-loaded notification that
`RbxStorage::init` subscribes to, and establish whether Cordial emits it —
by a means independent of log level. Cordial reaches `[roblox] flags loaded
(1305506 bytes)` and `areFlagsLoaded:true`, so the flags themselves arrive;
what is unknown is whether the *notification* Sober's init hangs off is
raised. That is a question about one signal, not about storage, and it is the
first time this investigation has had a working control to compare against.

## §42. The post-settings block is nearly complete; `IxpStorageManager` and `RbxStorage` are the two that never appear

§41 established that `RbxStorage::init` is never called. This narrows where to
look, by comparing Cordial's engine log against Sober's on the same machine.

**A caveat that must travel with the table.** Sober's `latest.log` covers a much
longer session than an 18-second Cordial run, so the *counts* are not
comparable and no conclusion is drawn from a count difference. Presence versus
absence is what this reads:

    marker              cordial   sober
    Mimalloc                 43      43
    Build system id           1       1
    ClientRunInfo             3       3
    AppMemUsageStatus         1       3
    TombstoneCache            1      20
    IxpStorageManager         0       1     <- absent
    RbxStorage                0      10     <- absent

Cordial produces the block the `CORDIAL_LATE_POST_MS` comment describes almost
in full. Mimalloc's forty-three option lines match exactly, `Build system id`
matches, and `ClientRunInfo` — which this document spent sections on — is
present three times in both. So the late post is doing its job and the block
is not missing wholesale.

**Two markers are absent rather than merely rarer**, and in Sober's log they
are four lines apart:

    Info    [FLog::AppMemUsageStatus]  923286370
    Warning [FLog::IxpStorageManager]  Failed to open cache file for reading
    Info    [FLog::TombstoneCache]     [FlagCache] Tombstone 1, expiry 360 ...
    Warning [FLog::LocalStorageHandler] Not available on the current platform.
    D       nativeInitializeNativeFlags: Registered Flag Provider ID from Java: 0
    Critical[DFLog::RbxStorage]        RbxStorage::init [INIT] user: flagLoaded

Cordial reaches `AppMemUsageStatus`, `TombstoneCache` and the
`LocalStorageHandler` warning, and registers the flag provider identically
(§41, and the registration is confirmed byte-for-byte against the Waydroid
capture). It does not produce the `IxpStorageManager` line, and it does not
produce `RbxStorage::init`.

**Whether those two absences are one fact or two is not established.** IXP is
Roblox's experiment platform and `FIntIxpStorageManagerXxhashSeed` is among the
flags; a shared dependency is plausible and unproven. Proximity in a log is not
causation, and this file has been wrong about exactly that kind of adjacency
before. The value here is that the search has gone from "somewhere in engine
startup" to one named subsystem that is present in a working control and absent
here.

### mocktail is not a source for this, and that is worth writing down

The maintainer suggested taking code from mocktail. For this specific problem
it cannot be done. `src/legacy/legacy_runtime.cc`'s
`ForceNativeFlagsLoadedForTaskScheduler` writes to
`g_libroblox_base + 0x75a8250` — it patches the flags-loaded byte in the
engine's own memory. That is precisely what
[ADR-001](../adr/ADR-001-in-process-hooking.md) and
[ADR-003](../adr/ADR-003-plugin-isolation.md) make *absent* rather than
disabled, so adopting it is not a trade-off to weigh but a line this project
does not cross. `tools/engine-text-diff.py` already recorded the distinction:
mocktail forces the state, Sober reaches it, and Sober is therefore the only
model worth copying. Ideas from mocktail remain fair game — its
`EnsureDefaultDataLayout` is already adopted — but not this one.

## §43. IXP is not the lead. The two absences are separate facts, and §42's adjacency argument is refuted by a capture already in the repository

§42 asked whether `FLog::IxpStorageManager`'s absence and `RbxStorage::init`
never running are one fact or two. They are two, and the evidence that settles
it needed no new instrument — `docs/traces/waydroid-roblox-startup.log.gz` has
been in the repository the whole time and contains both lines.

### The Sober ordering §42 reasoned from is a race, not a sequence

Sober puts `IxpStorageManager` four lines above `RbxStorage::init`. The real
Android client puts them the other way round:

    0.415087  Critical [FLog::Output]           Build system id: 1
    0.415765  Info     [FLog::AppMemUsageStatus] 923286370
    0.415828  Critical [DFLog::RbxStorage]      RbxStorage::init [INIT] user: flagLoaded   <- tid 10025
    0.417668  Warning  [FLog::IxpStorageManager] Failed to open cache file for reading     <- tid 9880
    0.417709  Warning  [FLog::IxpStorageManager] Failed to open random ID file for reading
    0.417866  Error    [FLog::TombstoneCache]   Failed to open tombstone file for reading
    0.417891  Warning  [FLog::LocalStorageHandler] Not available on the current platform.
    0.420     D rbx.JNIRobloxSettings           Registered Flag Provider ID from Java: 0
    0.444492  Critical [DFLog::RbxStorage]      RbxStorage::init [DONE]

`RbxStorage::init` is on a pool thread in both captures — `b5b31cf0` here,
`d1b0b6c0` in Sober — while the block around it is on the main thread. It lands
1.8 ms *before* `IxpStorageManager` on Waydroid and 2.5 ms *after* it on Sober.
The main-thread order is identical in the two (`Build system id` →
`AppMemUsageStatus` → `IxpStorageManager` → tombstone → `LocalStorageHandler`);
only the worker moves. **Proximity in Sober's log is the scheduler, not a
dependency**, which is what §42 warned about and then reasoned from anyway.

The same three lines kill a second inherited belief. §41 read Sober's ordering
as "the two lines before it are `Registered Flag Provider ID from Java: 0` and
`flagCount`". On Waydroid `RbxStorage::init [INIT] user: flagLoaded` fires
**4.5 ms before** the Java flag provider registers. Whatever `flagLoaded` is, it
is not downstream of `nativeInitializeNativeFlags`, and looking for the
notification on that side is looking in the wrong place.

### `IxpStorageManager` hangs off the Lua app, not the flags path

Two of the 223 Cordial engine logs on this machine contain the channel, both in
`~/.cache/cordial-agent-t`, `2.734.0.917_20260819T0745{25,47}Z`. In the second:

    0.281692  Warning [FLog::DataModelPatchConfigurer] getCachedPatch: get patch from content provider for Model
    0.293493          [FLog::DataModelPatchConfigurer] deserializeAndVerifyPatch with blake3
    0.370555  Warning [FLog::IxpStorageManager]        Failed to open cache file for reading
    0.492239  Warning [FLog::SingleSurfaceApp]         Register rendering frequency during startup.
    ...
    1.384317  Critical [FLog::Output]                  Build system id: 1
    1.384453  Info     [FLog::AppMemUsageStatus]       923286370

IXP initialises inside `initializeLuaAppWithLoggedInUser`, **a full second
before that run's post-settings block**. On Android the Lua app starts and the
settings post lands within the same two milliseconds, so the two appear
adjacent; under Cordial they are a second apart and the adjacency dissolves.
`IxpStorageManager` was never in the post-settings block, and §42's table put it
there because Sober's timing put it there.

### §35.3 identified the wrong file, and its conclusion is withdrawn

§35.3 closed the IXP question by pointing at a path trace —
`fopen("./appData/ClientSettings/IxpSettings.json") = null` — and concluding the
Ixp cache open is attempted and the channel is merely quiet. It is not the same
file. A path trace taken today shows that open happening between
`fopen("./exe/ClientSettings/ClientAppSettings.json")` and
`nativeInitClientSettings -> 0`; it belongs to the settings loader, which the
engine logs on `[FLog::ClientSettings]` as `LoadIxpSettingsFromLocal`. The
strings in `libroblox.so` keep the two apart plainly: `IxpSettings.json` sits
beside `LoadIxpSettingsFromLocal path: "{}"`, while `IxpStorageManager`'s own
files are `ixp_cache_v1` and `ixp_cache_random_id`, with
`success_random_id_file_write` and `fail_open_random_id_file_write` beside them.

So §35.3's "eighth instrument fault" was itself a misreading. The rule it
derived — an absence in a log is not an absence in the engine — is still right,
and is what the next paragraph rests on. Only the file identification was wrong.

### The subsystem has run under Cordial many times, and said nothing

`ixp_cache_random_id` appears as a literal in `libroblox.so` and nowhere else:
not in the APK's dex (`unzip -p base.apk '*.dex' | strings | grep -c` is 0) and
not in Cordial's own source. Only the engine writes it. Eleven Cordial profiles
have one, each holding a distinct 36-byte UUID, at
`<profile>/data/files/ixp_cache_random_id` — the same shape as Sober's, which
lives at `data/sober/assets/ixp_cache_random_id` and has held one UUID since
Sober's first launch on 2026-07-24. In `cordial-agent-policy` and
`cordial-agent-crash`, both single-run profiles, the file's mtime is inside the
same second as the profile's only engine log, and neither log mentions the
channel.

**So `IxpStorageManager` ran, generated an ID and wrote it, in runs whose logs
carry no `IxpStorageManager` line at all.** §42's "0 vs 1" was never evidence
that the subsystem does not run here.

### In today's build it does not run, and that is a separate regression

Five runs today, all on fresh data roots, build `v0.6.0-7-ga483b01-dirty`
(the tree gained `a19b945` and `a483b01` from another session between run 1 and
run 3; all five agree):

    run  flags.json                                    Ixp  Tombstone  RbxStorage  ixp_cache_random_id
    1    none                                            0      6           0      not written
    2    FLogIxpStorageManager=7, FLogTombstoneCache=7   0      0           0      not written
    3    both = "Verbose"                                0      3           0      not written
    4    both = "Verbose"                                0      3           0      not written
    5    both = "Verbose"                                0      3           0      not written

`find /var/home/neilluo -name ixp_cache_random_id -newermt 2026-08-20` returns
nothing: no run today wrote one anywhere on this machine, inside a profile or
out of it.

Run 2 is the control that makes the rest readable, and it repeats §22.3 exactly:
setting `FLogTombstoneCache` to the bare number `7` **silences** the channel that
run 1 printed six lines on, and the severity name `Verbose` restores it. So the
value does reach the engine through `flags.json` and does change what it logs —
and `FLogIxpStorageManager` set the same way, in the same runs, produces
nothing. Neither channel is configured in the live settings document
(`FLogIxpStorageManager`, `DFLogRbxStorage`, `FLogTombstoneCache`,
`FLogLocalStorageHandler` are all absent from it), so both were at their engine
default on 2026-08-19 as well, when the line did print.

The window is otherwise unchanged. Today's run 5 reaches
`getCachedPatch: get patch from content provider for Model` at 1.2055 and
`deserializeAndVerifyPatch with blake3` at 1.2218, exactly as 2026-08-19 did at
0.2817 and 0.2935 — and then goes straight to `Register rendering frequency` at
1.9693 with nothing in between, where the older run had the IXP line. One line
did appear in that gap that was absent on 2026-08-19:
`Warning [DFLog::RbxmFileManager] LocalStorageManager is not available.` at
1.2056. Whether that displaced IXP or merely arrived alongside it is **not
established** and is a question for the local-storage thread, not this one.

`FLog::LocalStorageHandler` also stopped appearing today, and that one is
already explained: §40 made `setPlatformImpl` unconditional, and the code
comment records the control — the warning appears twice a run when the call is
skipped and not at all when it is made. §42's table listing Cordial as producing
it was measured before that landed. Not a regression.

### The answer, and the branch this closes

The two absences are **not the same fact**:

* they are raised from different call sites — IXP from
  `initializeLuaAppWithLoggedInUser`, `RbxStorage::init` from a pool worker
  answering `flagLoaded`;
* they are not ordered with respect to each other in the working control, so
  neither can be upstream of the other;
* they dissociate in Cordial's own history. Two of 223 logs carry
  `IxpStorageManager` and eleven profiles carry IXP's own file; **not one log,
  and no profile, has ever carried `RbxStorage::init`**. A shared dependency
  cannot produce that.

**IXP is a dead end for storage.** It is worth writing down that today's build
also stopped running it, because that is a real behavioural change with a date
range around it — between the `cordial-agent-t` run at 2026-08-19 07:45Z and the
`cordial-agent-repro` runs at 2026-08-19 20:50Z, which is the window holding
`5c5266a`, `e8badbb`, `9a0de84` and the 06:39-onwards batch. But it is a
different bug from the one this document is about, and the storage question does
not go through it.

The better thread is the one §41 opened and could not name: what raises
`flagLoaded`. Waydroid says it is not the Java flag registration. Nothing else
in either capture names it.

## §44. The djinni local-storage surface is complete and is not the gate; `RbxmFileManager` is not new, is not silent on success, and is the per-run instrument §36 said did not exist

Two threads here. The first closes the `localstorageplatforminterface` branch
§39–§40 opened. The second is a correction to §43 that turns a datum it
dismissed into the cheapest scoring instrument this investigation has had.

Build `v0.6.0-9-gaf6e108-dirty` throughout, and `-dirty` is not incidental:
`third_party/mcpelauncher-linker`'s `bionic` carries another session's
`CORDIAL_TRACE_DLSYM` additions, and `native/platform_classes.cpp` and
`native/CMakeLists.txt` were being edited by a parallel agent while these runs
were taken. Nothing measured below is in those files.

### §44.1 What the engine expects, read out of the dex rather than assumed

`setPlatformImpl` is `ILocalStorageHandlerCore`'s, confirmed rather than
inferred — `tools/dex_method.py --class localstorageplatforminterface` gives

    ILocalStorageHandlerCore.setPlatformImpl(
        Lcom/roblox/protocols/localstorageplatforminterface/generated/IPlatformLocalStorageHandler;)
        Lcom/roblox/protocols/localstorageplatforminterface/generated/ILocalStorageHandlerCore;

and that class declares nothing else but `<init>`. `IPlatformLocalStorageHandler`
declares twelve methods. The dex also shows both interfaces carrying a
`$CppProxy` with the `native_*` family, which `readelf --dyn-syms` confirms
`libroblox.so` exports — so the scaffolding runs in both directions and only the
`IPlatformLocalStorageHandler` direction is Cordial's to answer.

### §44.2 What Cordial hands over, printed from the registered side

`CORDIAL_TRACE_LOCAL_STORAGE=1` now dumps the descriptors
`native/local_storage.cpp` actually registered, rather than the ones the engine
asks for. That distinction is the whole point: §40's stolen `typeid` was silent
because the JNI trace prints the *engine's* descriptor, which is always correct,
and nothing printed the other side. All twelve match the dex exactly, including
`getUsers()Ljava/util/HashSet;`.

### §44.3 The engine does call through it — measured, not inferred

`deleteSecureValue key=CookieManagerExperiment`, twice a run, on every run taken
here (six, across three builds). Nothing else on the interface is called on a
landing-page run: no `getSecureValue`, no `getCurrentUser`, no `getUsers`.

So the answer to "does anything call through" is yes, and the platform
implementation is live rather than accepted-and-forgotten. **It is also not the
storage gate**: those same runs have no `rbx-storage.db`, no engine-created
partitions and zero `RbxStorage` lines.

### §44.4 `setPlatformImpl` returned null for a reason that was not the engine's

The return value had been discarded since the call was written. Printed, it is
`null` — which reads as "the engine declined to build a core" and is worth
nothing until the two ways to get a null are separated. djinni's `fromCpp`
returns `nullptr` without touching JNI when C++ handed it no object; libjnivm
returns whatever it invents when `NewObject` names a class nobody implemented,
and `ILocalStorageHandlerCore$CppProxy` was exactly such a class.

Registering it settles it, with the control in the same session and one edit
between the arms:

    CppProxy unregistered, x2 runs:   setPlatformImpl returned null
    CppProxy registered,   x4 runs:   ILocalStorageHandlerCore$CppProxy built
                                      setPlatformImpl returned a core

**The null was libjnivm's, not the engine's.** The engine built a core all along,
which the `deleteSecureValue` calls already implied. The registration stays
because an invented stub is the failure mode §40 was bitten by, not because it
changes anything: all four runs still exit 0, still reach `app ready: Landing`,
still produce no store, and still print `RbxmFileManager` twice.

Worth recording against the parallel survey of unanswered classes, which lists
`ILocalStorageHandlerCore$CppProxy` among the `FindClass`-only set: it is not.
It is constructed, once per run, as soon as something implements it. A class the
engine only looks up may still be one the engine would use if it worked.

### §44.5 `DFLog::RbxmFileManager` is not new, and §43's dating of it is withdrawn

§43 recorded `Warning [DFLog::RbxmFileManager] LocalStorageManager is not
available.` as "new as of today and absent on 2026-08-19". It is neither. Every
engine log on this machine, scored (399 of them, `CordialTest` excluded):

    2.730.0.790   64 warned   183 silent
    2.732.0.1043   0 warned    44 silent
    2.734.0.917  105 warned     4 silent

On 2026-08-19 alone it appears in 98 of 103 logs. §43 compared today's run
against two `cordial-agent-t` runs and generalised from a pair; those two happen
to be members of the four-run exception below, which is the least representative
sample on the disk.

### §44.6 It is absent from both working controls, and it is not a channel artifact

    Sober, 2.734.0.917, store works:        0 RbxmFileManager lines
    docs/traces/waydroid-roblox-startup:    0 RbxmFileManager lines
    Cordial, current build, 6 runs:         2 per run

The ordering in the two working captures says what the line means. On Waydroid
`RbxStorage::init [DONE]` lands at 0.4445 and `getCachedPatch: get patch from
content provider for Model` at 0.5135, followed immediately by
`deserializeAndVerifyPatch` with nothing between them. Sober is the same shape,
`[DONE]` at 5.520 and the patch fetch at 5.579. Under Cordial the patch fetch is
at 1.4637 and the warning lands 83 microseconds later, in the slot the working
captures pass through silently.

So `RbxmFileManager` asks for the content store at the moment it needs it, gets
it in both controls, and does not here. That the store it asks for is
`RbxStorage` is **`INFERRED`** — the message names a C++ `LocalStorageManager`
and §40 established the engine never asks libjnivm for
`com/roblox/client/LocalStorageManager`, so the name in the message is not the
Java class. What is measured is that the line is present exactly when the store
is absent, in every capture available.

### §44.7 The four exceptions are the working run and its neighbours

Of 105 current-engine runs that reach the patch fetch, four do not print the
warning. All four are in `~/.cache/cordial-agent-t`, and two of them are
`ec3a3` and `08d78` — §36/§37's one positive.

    062512Z   silent
    063001Z   silent
    074525Z   silent   <- ec3a3
    074547Z   silent   <- 08d78

**This corrects §37's attribution again, in the other direction.** §36 credited
`ec3a3`; §37 moved the credit to `08d78` because `files.atime` = 07:46:02.730Z
falls in `08d78`'s window. Both are half right. The eight partition directories
and `rbx-storage-sc/` carry mtimes of **07:45:25.902Z**, 0.66 s into `ec3a3` —
so `ec3a3` initialised the store and `08d78` wrote the nine rows into it the
following launch. §37's correction is right about the rows and wrong about the
initialisation; §36's original attribution was right about the initialisation.

That is the finding that matters for everyone who continues: **§36's "the log
channel that would say is silent even on success" is true of `DFLog::RbxStorage`
and false of the engine as a whole.** There is a one-line, on-by-default,
t≈0.3–1.5 s marker for whether the content store came up, it needs no
filesystem poll, and it retro-scores every log already on this disk. Forty-odd
negative reproductions in §36–§37 were scored the expensive way for want of it.

### §44.8 What separates the four, and three things that do not reproduce it

The four silent runs share one other property, and it is a clean partition —
where the engine wrote its tombstone:

    silent, x4     .../profiles/default/data/cache/cache/tombstone.dat   absolute
    warned, x101   cache/tombstone.dat                                   relative
    warned, x4     no tombstone line at all

Relative resolves against the process working directory, which is
`<profile>/run` (`load.rs` `set_current_dir`), so the two forms are two
different files and the engine had a cache directory in one case and an empty
one in the other. Cordial calls `nativeSetCacheDirectory` with an absolute path
on every run and prints `ok`, so the value is delivered; in 101 runs out of 105
it is not in effect by the time the flag cache resolves its paths.

Three attempts to reproduce the silent state, all negative, all scored on the
marker and the filesystem together:

    CORDIAL_DEFER_CTORS=1, fresh roots, x3         warned, tombstone REL, db=0
    CORDIAL_LATE_POST_MS=2000 CORDIAL_LATE_RETRY=1,
      one root, three warm passes, --run 35, x3    warned, tombstone REL, db=0
    plain landing-page, fresh roots, x7            warned, tombstone REL, db=0

The second of those is the configuration §36 named for the working run, run
warm rather than fresh this time, and it does not do it. `CORDIAL_DEFER_CTORS`
sets the four directory setters before `libroblox.so`'s constructors run, which
is the most direct way to give the engine a cache directory early, and it does
not do it either — consistent with §27, and now measured on an instrument §27
did not have.

### Where this leaves it

The djinni branch is closed: the surface is complete, correct against the dex,
called by the engine, and unrelated to `RbxStorage`. §39's and §40's work stands
as two real libjnivm bugs fixed, and §44.4 adds a third and smaller one, none of
which was the gate.

The open question is unchanged in substance and much cheaper to attack:
**something makes the engine's cache directory effective early in roughly one
run in twenty-six, and the store comes up exactly then.** Whether the directory
is the cause or another symptom of the same early ordering is not established.
Whoever takes it should score on `grep -c RbxmFileManager` and the tombstone
path form rather than on the filesystem, run enough times to see the rate, and
report the rate rather than a single outcome.

## §45. mocktail does not force the flags-loaded byte on this build; all four working captures run `postClientSettingsLoadedInitialization3` *before* `nativeAppBridgeV2Init` and Cordial runs it after, in 109 logs out of 109

Build `0.6.0-13-gd5eb7e2-dirty` for the two runs at the end; the `-dirty` is
another session's `CORDIAL_EARLY_DIRS` and cacert work in `load.rs` and this
section changed no code. Everything before those two runs is read off files
already on this disk and cost nothing.

### §45.1 The experiment this section was commissioned to run is moot, because its premise is false

The brief was: mocktail's store works and mocktail *forces* the engine's
flags-loaded byte at `g_libroblox_base + 0x75a8250`
(`ForceNativeFlagsLoadedForTaskScheduler`, `src/legacy/legacy_runtime.cc`), so
read that byte in a live Cordial process and find out whether Cordial reaches
the same state.

mocktail does not force it on this build. The write is gated three deep:

    ForceNativeFlagsLoadedForTaskScheduler
      -> IsEnabled("MOCKTAIL_PATCH_NATIVE_FLAGS_LOADED")
        -> returns false for every MOCKTAIL_PATCH_* name unless
           g_allow_legacy_binary_patches
             <- build_profile.allow_legacy_binary_patches, looked up by ELF Build ID

`config/roblox_compatibility.json` carries three profiles.
`allow_legacy_binary_patches` is **true only for 2.721.1108** — the one marked
`legacy-researched`, `default_allowed: false`. Both `supported` builds have it
false, and Cordial's build is not in the manifest at all -- mocktail runs it
as `(experimental)`, which is precisely the state in which the legacy patches
stay off.

The installed Flatpak says so at runtime, in a session whose store works:

    [compat] Roblox 2.734.917 Build ID 63c5109637b7d7b2bdb8ed8f858023ff5ef49326 (experimental)
    [compat] legacy binary patches: disabled
    ...
    Critical [DFLog::RbxStorage] RbxStorage::init [INIT] user: flagLoaded, ...
    Critical [DFLog::RbxStorage] RbxStorage::init [DONE] ... dbOpenCount: 3

That Build ID is byte-identical to the one in `~/.cache/cordial/lib/x86_64/libroblox.so`
(`readelf -n`). All three mocktail session logs on this machine print
`legacy binary patches: disabled` and all three print two `RbxStorage::init`
lines. **mocktail's store comes up with the forcing function inert**, so
"flags-loaded satisfied by force" was never the mechanism, and a byte read
would have been answering a question nobody had.

### §45.2 The offset could not have been validated anyway, and no value from it is reported

Recorded so the next person does not spend the session on it.
`libroblox.so` for 2.734.0.917 is fully stripped: no `.symtab`, and **zero**
dynamic symbols with an address anywhere in `.bss` (which spans
`0x6fd9d00`–`0x7ad4abc`, so `0x75a8250` is at least *inside* it). No
relocation names it. mocktail derived it for a build thirteen releases older
and its own compatibility gate refuses to use it here. There is no way to show
the address means anything on this build short of a differential against a
live known-good process, and per the brief's own caution no value is reported.

### §45.3 mocktail is a third working control, and the most useful one yet

Same Build ID, same host, same `libroblox.so`, and — like Cordial and unlike
Sober — not an Android environment. Its logs are at
`~/.var/app/space.bigrat.mocktail/.local/state/mocktail/logs/`, its store at
`.../data/mocktail/android/data/files/appData/rbx-storage.db`. It prints an
`[engine] <call>` marker for every native it drives, so the whole startup
sequence is readable without instrumenting anything.

### §45.4 The signature: post-settings init runs before the app bridge in every capture where the store works, and after it in every Cordial run on this disk

Three landmarks, from four captures, all read off logs already present:

    capture                        post3     RbxStorage::init   AppBridgeV2Init
    docs/traces/waydroid-...       0.4114    0.4158             0.5045
    Sober, 2026-08-20_14-38-41     7.5676    7.5770             7.5873
    mocktail, 2026-08-19 02:20     2.0976    2.1113             3.2502
    Cordial, ef907 (typical)       2.5938    absent             0.2277

The three working captures have the same shape to the millisecond-pattern:
`nativePostClientSettingsLoadedInitialization3`, then `RbxStorage::init` 4–14 ms
later on a different thread, then `nativeAppBridgeV2Init`. Cordial inverts it —
the bridge is up first and the post-settings block arrives one to three seconds
afterwards, by which time the Lua app is running and Vulkan has a swapchain.

Scored across every current-engine log on this machine (`CordialTest`
excluded), 109 logs, of which 75 carry both timestamps:

    BRIDGE_FIRST  75 / 75
    POST_FIRST     0 / 75
    RbxStorage::init present   0 / 109

`FFlagStartRbxStorageInitRighAfterFlags = True` is in the very settings
document Cordial hands the engine, which is consistent with the trigger the
working captures name in their own line (`user: flagLoaded`). That the
ordering is the *cause* rather than another symptom is **`INFERRED`** — what is
measured is that the two populations separate perfectly on it.

### §45.5 The existing knob does not move it, with a control in the same session

`CORDIAL_EARLY_SETTINGS=1` is the closest thing already in the tree: it calls
`nativeInitClientSettings` before `initializeNativeCode`. One control and one
arm, same binary, same reused data root, minutes apart:

    control              bridge 1.3429   post3 3.3509   BRIDGE_FIRST   rbxinit 0   warn 2   db 0
    CORDIAL_EARLY_SETTINGS=1
                         bridge 1.5683   post3 3.5012   BRIDGE_FIRST   rbxinit 0   warn 2   db 0

The early call reports `early client settings (1274772 bytes) -> 0`, so it is
made and accepted, and the engine still runs its post-settings block after the
bridge. Two runs is not a rate and is not offered as one — ordering is a
deterministic per-run observable and two runs is what it takes to see it did
not change. Reaching the working shape needs
`nativePostClientSettingsLoadedInitialization3` to run its *body* before the
bridge, and §44's note that the early `post` call is "always a no-op" (no
`[FLog::AndroidGLView]` line) is the obstacle, not the call site.

### §45.6 Three things that are not the variable, closed cheaply

**Being signed in is not it.** The maintainer's real signed-in profile has 14
current-engine runs: `RbxStorage::init` 0/14, the `RbxmFileManager` warning
present in all 14. All four working captures are signed in and it does not
distinguish them.

**§44.8's four silent runs are four, not five.** A fifth log with no warning
(`bcba4`, 11 lines) never reached the patch fetch. Of the 109, 104 reached the
fetch and warned, 4 reached it and did not, 1 did not reach it — so the warning
is not simply "did the run get that far".

**`grep -c RbxmFileManager` is the wrong scorer by a hair.** mocktail prints
two `Warning [DFLog::RbxmFileManager] Caching for rbxasset://...InExperience.rbxm
is not enabled` lines in a session whose store works. Score on
`LocalStorageManager is not available`, which is the message §44 meant.

### §45.7 A correction §41 needs, which I cannot resolve

**The historical positive contains no `RbxStorage::init` line.** `ec3a3` ran
21.1 s, exited cleanly, is 263 lines, and has zero `RbxStorage` lines at any
severity — yet the eight partition directories under
`~/.cache/cordial-agent-t/.../appData/rbx-storage/` carry mtimes of
07:45:25.902Z, 0.66 s into that run, and `rbx-storage.db` is a real 49,152-byte
`SQLite format 3` file stamped 07:46:02.73Z, inside `08d78`, which also has zero
`RbxStorage` lines. Only one log file exists per run, so nothing is truncated.

Either the store came up twice without logging the line — which contradicts
§41's "Sober logs it at Critical, so absence means it is not called", the
premise every section since §41 has rested on — or those artefacts were not
made by `RbxStorage::init` and §36/§37/§44.7 have been crediting the wrong
thing for three sections. **I have not established which**, and it should be
settled before anyone builds on §41 again. One candidate worth checking rather
than assuming: `DFIntRbxStorageInitHundredthsPercent = 1000` appears in the
settings document Cordial feeds the engine, and the neighbouring
`...HundredthsPercent` names in that file are sampling rates. Whether it gates
this log line is **`INFERRED` and untested** — and it would have to be
reconciled with all four working captures printing the line.

### Where this leaves it

The byte branch is closed for the reason that it was never open. The ordering
signature is the first thing that separates every working capture from every
Cordial run on this disk with no exceptions on either side, it was established
entirely offline, and it is testable by making the engine's post-settings body
run before `nativeAppBridgeV2Init` — which is what mocktail does by driving the
sequence itself rather than waiting to be asked.

## §46. `RbxStorage` initialises. It wanted the files *and* cache directories before `initializeNativeCode`, not a different ordering — twelve stores out of twelve, ten controls out of ten with none, and §45's ordering signature was measured with an instrument that could not have shown otherwise

Build `0.6.0-15-g8784c1e-dirty` for every run in §46.2–§46.5 and
`0.6.0-16-ge76e4df-dirty` for the cold-root run in §46.6; the tree was dirty
throughout with the `CORDIAL_EARLY_DIRS`/cacert work §45 also ran against, plus
this section's own edits, and from 15:49 with a parallel session's
`native/init_params.cpp` and `flags.rs` changes, which land after every run
except the cold one.

### §46.1 The result

`CORDIAL_EARLY_DIRS=files,cache` — both `nativeSetFilesDirectory` and
`nativeSetCacheDirectory` called before `GameActivity.initializeNativeCode`
rather than only after it returns — brings the content store up. A real
`SQLite format 3` file at `<filesDir>/appData/rbx-storage.db` with the engine's
own schema

    CREATE TABLE files (id BLOB PRIMARY KEY NOT NULL, content BLOB,
      size INTEGER DEFAULT 0 NOT NULL, hits INTEGER DEFAULT 0 NOT NULL,
      atime INTEGER DEFAULT 0 NOT NULL, category INTEGER DEFAULT 0 NOT NULL,
      score INTEGER DEFAULT 0 NOT NULL, ttl INTEGER DEFAULT 0 NOT NULL)

carrying nine to eleven rows of `RBXH`-framed cached responses, beside the eight
`rbx-storage/p*` partition directories and `rbx-storage-sc/`. Cordial pre-creates
the empty `appData/rbx-storage` directory itself; the partitions, the
session-scoped directory and the database are the engine's.

One session, one data root, the store deleted before each run, arms and controls
interleaved:

    files,cache early    store 12 / 12    (incl. two on the plain default and one cold root)
    nothing early        store  0 /  6
    cache only early     store  0 /  2
    files only early     store  0 /  2

Neither directory alone does it, which is the part worth keeping: the engine
builds a `MultiCache` out of a permanent cache under the files directory and a
temporary one under the cache directory, and being handed one of the two is
being handed neither. Against a base rate of roughly one store in twenty-six
launches, twelve for twelve is not the base rate.

`CORDIAL_EARLY_DIRS` now defaults to `files,cache`. It defaulted to `cache`,
which is measured here to do nothing for storage.

### §46.2 The ordering hypothesis was testable, was tested, and is wrong

§45.4's signature — `postClientSettingsLoadedInitialization3` before
`nativeAppBridgeV2Init` in all three working captures and in none of 109 Cordial
logs — is real as a description and is not the mechanism.

`CORDIAL_POST_BEFORE_BRIDGE=<ms>` calls the post-settings native immediately
before the app bridge and then holds the bridge for `<ms>`. It works: the
`[FLog::AndroidGLView] nativePostClientSettingsLoadedInitialization3` line lands
at 2.183367 against a bridge at 2.725852 in one run and 1.632804 against
2.176352 in another, the full `[FLog::ClientRunInfo]`/`Mimalloc` block follows it
exactly as on Android, and these are the first two Cordial logs on this machine
that are POST_FIRST. **Neither produced a store**, with `CORDIAL_EARLY_DIRS=off`
so that nothing else was in play.

The converse holds too: the twelve runs that *did* produce a store are, on the
same scoring, BRIDGE_FIRST — their logged post is the late one at ~2.9 s. So the
store appears without the ordering and the ordering appears without the store.
The two are independent, and §45.4's `INFERRED` causal note is withdrawn.

One curiosity from the pre-bridge site, not chased: the block prints
`The base url is` with nothing after it, where the late call prints
`https://www.roblox.com`.

### §46.3 §45.4's separation could not have come out any other way

**In 104 of the 106 current-engine logs on this machine the first line in the
file is `nativeAppBridgeV2Init`.** Not the first interesting line — the first
line. Cordial calls `nativeOverrideChannelPlatformName`,
`nativeSetRobloxChannel`, the four directory setters, `initStorageManagerNativeV3`,
`setPlatformImpl`, the init params and `nativeReadLocalFlags` before the bridge,
and none of them writes anything to the engine's log file. Sober's log opens on
`RobloxChannel has been set to production` and mocktail's on
`nativeInitClientSettings`, both well before their bridge.

So a Cordial log cannot show POST_FIRST unless something arranges for the log
file to exist earlier, and the score of BRIDGE_FIRST 75/75, POST_FIRST 0/75 was
guaranteed before any run was taken. §45 established a property of Cordial's
logging, not of Cordial's ordering. (The two exceptions are in
`~/.cache/cordial-agent-t`: `b2c9c`, whose log opens on `readLocalFlags` at
0.3275 with a broken `https:///v2/...` settings URL, and `29325`, which opens on
a `RobloxTelemetryFactory` warning.)

Cordial has in fact called `postClientSettingsLoadedInitialization3` before the
bridge on every default run since the bootstrap was written — inside
`bootstrapTheApp`, at around 0.2 s. §44's "the early call was always a no-op"
rests on the absence of a log line it could not have produced. Whether that
early call's body runs is still not established; what is established is that the
absence of the line is not evidence either way.

### §46.4 §45.7 is resolved: the store comes up before the log file exists

In all twelve positive runs the `rbx-storage/p*` partitions are born **before**
the engine's log file:

    files,cache early, early post on     store ~0.93–1.03 s before the log file
    early post off, or post before bridge  store ~10–12 ms before the log file

and every one of those runs has zero `RbxStorage::init` lines. So the answer to
§45.7's "either the store can come up without logging, or earlier sections
credit the wrong run" is the first, with a mechanism: `RbxStorage::init` logs at
Critical, but Critical to a file that does not exist yet goes nowhere. §41's
"absence means it is not called" is therefore only sound for the window after
the bridge, and §36's original instinct that the channel was silent was right
for the wrong reason. `ec3a3` keeps its credit.

This also means nobody has yet *seen* `RbxStorage::init` under Cordial. Getting
the log open before ~0.2 s would settle the last of it; the pre-bridge post is
one way in (it opens the log where it runs) but the store beats it by 10 ms even
then.

### §46.5 Three scorers this file has recommended, and what they are actually worth

**`LocalStorageManager is not available` does not mean the store is absent.**
Ten of the twelve positive runs print it — twice in eight of them. §44.6 flagged
"the store it asks for is `RbxStorage`" as `INFERRED`; it is now refuted. It does
partition something: the two positives that print it *zero* times are the two
where the store came up within 12 ms of the log opening, so it looks like a
timing marker for whether the store was up before whatever asks. As a storage
scorer it is wrong, and §45.6's advice to use it is withdrawn.

**The tombstone path form is a co-symptom of the cache directory, not a marker
for the store.** `cache` alone early gives `tomb=ABS` and no database, twice. It
tracks `nativeSetCacheDirectory` being early and nothing else, so §44.8's clean
four-run partition was a coincidence of those four runs having been taken with
an early cache directory.

**`gameActivity_onFlagsFailed` still fires twice in runs whose store is up.** The
message in `native/init_params.cpp:873` tells the reader "this does not block
startup but it does block the content store", and that is now false. That file
was out of scope for this session; it should be corrected.

The scorer that does work is the filesystem: `rbx-storage.db` exists, is
`SQLite format 3`, and `select count(*) from files` is non-zero.

### §46.6 The uncommitted CA-certificate work: keep it, it is load-bearing

`load.rs` carried an unattributed 126-line diff — the `CORDIAL_EARLY_DIRS` switch
itself and a second `cacert.pem` symlink under the files directory. It builds
(`just build toolbox`, exit 0), and the symlink is not optional for the
configuration that fixes storage. Gating that one link off and changing nothing
else, with `files,cache` early:

    link present    landing reached, store 45,056 bytes, 9 rows
    link absent     `error adding trust anchors from file`, 100 HTTP/flag error
                    lines, landing never reached, store 28,672 bytes with 1 row

So its own comment was right. The store still comes up without the network,
which is worth knowing separately.

Two claims in that diff's comment are withdrawn, both corrected in place: that
25 runs with only `cache` early produced a store against 14 controls that did not
— no data root on this disk holds a store from such a run, and `cache` alone was
re-run twice here with none — and that `cache` is "the only one of the four that
storage needs".

The early `files` call is now skipped, loudly, when `<filesDir>/exe/cacert.pem`
is missing. The bundle is extracted from the APK by `asset_folder`, which does
not run until the app bridge, so a first launch into an empty asset cache has
nothing to link; without the guard that launch would lose HTTPS entirely. A cold
root with the asset cache already populated — the ordinary case — reaches the
landing page and produces a store.

### Where this leaves it

Storage is up. `cargo test --workspace` in the toolbox: 604 passed, 0 failed, 19
suites.

What is not established: why the two directories are needed *before*
`initializeNativeCode` rather than a second later, given that §27 set all four
before `libroblox.so`'s constructors and got nothing; what opens the engine's log
file, and therefore how much of every Cordial run so far has been invisible;
whether the early post inside `bootstrapTheApp` runs its body; and whether the
store is actually *used* for anything beyond the eleven rows a 30-second landing
run puts in it — nobody has yet watched an asset come back out of it.

## §47. The engine re-fetches client settings itself, and DF* overrides last about two seconds

`client_settings.rs` said "The engine never fetches it itself". **That is wrong**,
and it has been quietly limiting every flag experiment in this document.

The engine runs a `DynamicFastVariableReloader` which fetches
`https://clientsettingscdn.roblox.com/v2/settings-compressed/application/GoogleAndroidApp.zst`
— 1,305,506 bytes — at t≈1.6–2.3 s and applies it over the top, reverting every
`DF*` override Cordial merged for any key that document contains.

Measured two-directionally inside one run, which is what makes it a result
rather than an observation:

    override                        Roblox's value   observed
    DFLogHttpTraceLight = "7"       "0"              logs 0.65 s -> 2.25 s, then silent
    DFLogHttpTraceError = "0"       "12"             silent -> 2.25 s, logging again from 4.86 s
    DFLogHttpTrace      = "7"       absent           logs 0.86 s -> 120.1 s

The middle row settles it. An override asking for **silence** went loud again,
at Roblox's own level, mid-run. Nothing Cordial does could cause that.

### What this means for every flag experiment here

**`DFFlag`/`DFInt`/`DFString`/`DFLog` overrides govern roughly the first two
seconds. `FFlag`/`FInt`/`FString` are the durable ones.** That is the opposite
of how `flags.rs`'s module doc framed it, and it means **any experiment whose
effect is not visible inside that window was measuring Roblox's value, not
ours.** Several negatives in this document are `DF*` overrides scored well after
two seconds; they should be re-read with that in mind rather than trusted.

The `RtcIoRna` control that `apply_overrides` cites as proof the mechanism works
only held because its line prints at 0.375 s — inside the window, by luck.

A key absent from Roblox's document survives the reloader, which is why
`DFLogHttpTrace` logged for the full 120 s. That is the reliable way to run a
`DF*` experiment: pick a key Roblox does not ship.

### And the application name differs

The engine asks for **`GoogleAndroidApp`**. `client_settings.rs` asks for
**`AndroidApp`**. Nobody has diffed the two documents. Cordial may be merging
overrides into a different settings set from the one the engine reloads.

## §48: the flag inventory was three orders of magnitude short, and delivery is unverified

`docs/traces/native-flag-names.txt` holds 139 names, and a search across it for
a TaskScheduler lever returned nothing. That was read as "no such flag exists".
It meant the inventory was short.

Roblox publishes the real list over a public endpoint — the same document the
engine fetches for itself at t≈1.6–2.3 s, which is what §47 is about:

    curl -s https://clientsettingscdn.roblox.com/v2/settings/application/GoogleAndroidApp

**22,739 names for `GoogleAndroidApp`, 22,329 for `AndroidApp`, 22,741 together**,
now in `docs/traces/client-settings-flag-names.txt`. Names only; values move
under Roblox continuously and a snapshot here would be a stale claim about live
configuration within days. Seventeen of them carry `TaskScheduler`, including
`DFFlagTaskSchedulerRescheduleAsForeground`, `DFFlagTaskSchedulerMeasuresScheduledRestTime`
and `FFlagTaskSchedulerRescheduleAllowed`; `FFlagEnableAndroidVsync` sits in the
same area. `DFStringTaskSchedulerUnreliableSleepManufacturers` is published
empty, and Cordial reports its manufacturer as `Cordial`, so it is not in play.

**Two were tried against the h2 spin and neither moved it** — 125.5 % baseline,
126.5 % with `FFlagEnableAndroidVsync=false`, 124.8 % with
`FFlagTaskSchedulerRescheduleAllowed=false`, all at 59.6 presents/s.

**Those are not yet negative results and must not be quoted as any.** Nothing in
that run establishes that an override reaches the engine. `flags: 1 override(s)
applied` is `flags::report` printing what Cordial *resolved*, one layer above
delivery, and the `flagCount = 139` enumeration is a different path entirely —
`nativeInitializeNativeFlags` takes a list of names whose *values* the engine
loads itself, so an override would never appear in it whether it worked or not.

**The next thing to establish is delivery, and it blocks every flag experiment
after it.** Set a flag whose effect is loud and unmistakable, confirm it lands,
and only then read a null result as a null result. Until that exists, any
"flag X does nothing" claim from this codebase is untestable — including the two
above, and including this project's older ones.

### §48a: log flags are the wrong probe, and the reason matters

The obvious delivery probe was a log flag, and it failed to distinguish
anything: 339 lines without overrides, 337 with `DFLogHttpTrace=7` and
`FLogNetwork=7`, no HTTP or network tracing in either.

That is not evidence about delivery, because the probe was invalid two ways.
**Roblox already ships `FLogNetwork = 7`**, so setting it to 7 asks for a state
the engine is in already — the null was guaranteed before the run started. And
no network logging appears in *either* arm despite that upstream value, so the
engine's `FLog` output is not reaching Cordial's log at all. Log routing is the
confound, and no log flag can probe past it until that is understood.

`DFLogHttpTrace` is genuinely not shipped by Roblox, which is what made it look
attractive — an unshipped key survives the reloader (§47). It still produced
nothing, for the same routing reason.

Cordial's own half is working: `flags: 2 override(s) applied` against `0` in the
control, so resolution and reporting are fine. What is unestablished is
everything downstream of that.

**A valid delivery probe needs an effect that is not a log line** — something
visible on screen, in a counter, or in a timing. Finding one is the next task,
and every flag experiment in this repository is blocked behind it.

### §48b: a non-log probe, and where the delivery path actually runs

§48a asked for a probe whose effect is not a log line. `FIntTaskSchedulerAutoThreadLimit`
is durable and its effect is countable in `/proc/<pid>/task`. Published value 8;
set to 1, alongside `FIntTaskSchedulerAsyncTasksMinimumThreadCount=1`:

    baseline   threads=60   12 Main, 4 "RBX Worker A", 3 HttpClient, ...
    limit=1    threads=60   12 Main, 4 "RBX Worker A", 3 HttpClient, ...

Identical, composition included. **This is not yet proof that delivery is
broken**, because the probe has no positive control: "no change" is also what a
flag that does not govern thread count in this build would produce. It wants a
flag known to work before it can carry weight — which is the same missing piece
§48a named.

What the read did establish is where overrides are merged. `client_settings::load`
is `load_base(explicit).map(apply_overrides)`, and `apply_overrides` resolves
every layer and merges it into the settings document. So the override path runs
**inside `client_settings::load` and nowhere else.**

An earlier draft of this section said `load.rs` calls that function in exactly
one place, gated behind `CORDIAL_DEFER_PAST_SETTINGS`, and asked whether
`plan.settings` was built through it — calling that the open question that
decided everything. **Both halves were wrong, and the correction is §48c.**

Four independent nulls are consistent with a delivery failure: two TaskScheduler
flags, two log flags, and now a thread-count probe. What they are not consistent
with is the particular delivery failure guessed at here.

### §48c: the override path does run by default, so the nulls stand unexplained

`plan.settings` is built by `client_settings::load`, on the ordinary path, with
no environment variable involved:

    load.rs:1912   if CORDIAL_NO_BOOTSTRAP unset && CORDIAL_LATE_SETTINGS unset
    load.rs:1930       settings: client_settings::load(opt.client_settings…)
    load.rs:582        init_client_settings(…, &plan.settings, "", "")

There are five calls to `client_settings::load` in `load.rs`, not one, and the
gates are complementary rather than nested — `:1463` behind
`CORDIAL_DEFER_PAST_SETTINGS`, `:1714` behind `CORDIAL_EARLY_SETTINGS`, `:2716`
and `:3581` behind others, and `:1930` behind the absence of all of them. Exactly
one runs per launch and by default it is `:1930`. Reading only the first grep hit
and taking it for the whole is how the earlier claim was reached; the file is
long enough that this will happen again to somebody.

So the merge is on the default path. `apply_overrides` has one definition and one
caller, so §48b's other half — that overrides are merged there and nowhere else —
survives.

This also rehabilitates a log line that was written off too fast. Earlier in this
session `flags: 1 override(s) applied` was called "Cordial's own resolution, a
layer above delivery", and refused as evidence. That was too harsh:
`flags::report` is called only from the `Ok` branch of `merge`, so the line means
the settings document parsed, `applicationSettings` was present, and the override
went into the copy handed to `nativeInitClientSettings`. It still does not say the
engine honoured the value — that remains the gap — but it does establish that the
document reaching the engine contained it, which is a great deal more than was
being credited.

One caveat on reading that line: it prints `resolved.len()`, the count before
`is_roblox_flag` filtering, while the merge uses the filtered map. A
Cordial-internal key such as `CordialGraphicsBackend` is therefore counted in the
number but not merged. The number is a resolution count printed at a delivery
moment, so treat it as "the merge ran", not as "this many Roblox flags landed".

**What is now open is narrower and harder.** The document carries the override
and the engine still shows nothing, on five separate probes. That points at the
engine side — the settings reloader at t≈1.6–2.3 s (§47), a flag read before the
document is installed, or `FInt` values that simply do not govern what was being
counted. None of those is distinguishable without a positive control: a flag with
a known, observable effect on this build. **That control is the blocker, and it
has now been the blocker three sections running.** Nothing else here is worth
running until it exists.

## §49: a positive control at last — `DFIntTaskSchedulerTargetFps`

Three sections have ended by saying the blocker is a flag with a known,
observable effect. There is one, and it is unambiguous.

`DFIntTaskSchedulerTargetFps` caps the frame rate, and the cap lands on the
requested number. Measured with input driven for the whole run
(`CORDIAL_SCRIPT=0:focus-on,0:motion-on`, `CORDIAL_INSTR=1`), which is the only
present-rate measurement this project trusts, on `0.6.0-52-gd0469e7-dirty`
(dirty is `Cargo.lock` and the `mcpelauncher-linker` submodule; no tracked source
differs):

    override      last presents/s samples                       runs
    (none)        36.5 47.0 42.0 42.3 47.2 41.6 / 34.8 41.8 34.6    2
    ...=10        10.5  9.9 10.5  9.6  9.5 10.9 / 9.8 9.5 9.7       2
    ...=15        16.0 14.4 14.9 14.9 15.9 15.0                     1
    ...=20        20.0 19.3 20.7 20.0 20.0 20.0 / 20.0 20.5 19.5    3

The control is the uncapped arm in the same session, twice, and it is nowhere
near any of the capped numbers. The effect tracks the *value*, not merely the
presence of an override: 10, 15 and 20 each produce their own number. Six runs.

`FIntTaskSchedulerTargetFps` — the same name with the durable prefix — does
nothing (44.0 40.1 54.1 39.0 39.7, indistinguishable from uncapped). That is the
right answer and it is worth having: it shows the prefix is load-bearing and that
the engine is matching the whole name, so a null from a misspelled flag is
detectable rather than silent.

`DFIntTaskSchedulerTargetFps=45` also showed nothing (36.0 38.8 30.6 28.9 38.0),
and that is **not** a contradiction: the uncapped arm only reaches 35–47, so a
cap at 45 has almost nothing to bind on. It is uninformative, not negative. A
positive control has to be set below what the machine already delivers.

### What this settles

**Flag delivery works.** Overrides written to `CORDIAL_FLAGS` are merged by
`apply_overrides`, carried in `plan.settings`, handed to
`nativeInitClientSettings`, and *acted upon by the engine*. §48c established the
route by reading; this establishes it by running. The theory that no override
had ever reached the engine — which §48b advanced and §48c retracted on a code
read — is now dead on evidence as well.

**A `DFInt` override survives the settings reloader.** §47 records that `DF*`
values are re-read and reverted at t≈1.6–2.3 s, and that was taken to mean a
`DF*` override could not hold. This cap held for all 25 seconds of every run.
The reloader evidently re-reads the same document Cordial supplied, so an
override placed in that document is what it reverts *to*. `DF*` flags are
therefore usable, and §47 should be read as describing where the value comes
from rather than as a reason to avoid the family.

**Every earlier null is now a real null.** The TaskScheduler pair, the two log
flags, the thread-count probe and `FIntGraphicsVulkanMinAndroidVersion` (tested
here at 99 against a reported `SDK_INT` of 33, no effect on Vulkan bring-up —
three swapchains either way) are statements about those flags on this build, not
about the delivery path. They were uninterpretable; they are now interpretable
and they are negative.

### Two instruments retired

`onFlagsLoaded`'s byte count is **a constant, not a measurement.** It reported
1,308,253 bytes identically across every arm tried: an `FInt` lengthened by six
characters, an `FString` lengthened by 1,001, and a document with 903 keys and
87 KB removed. Whatever `buf->capacity` describes, it does not vary with the
document. `native/init_params.cpp:940` prints it, and it reads like a delivery
readout, which is exactly why this is written down.

Handing the engine a 58-byte settings document **segfaults it**, reproducibly
(exit 139, 139, 133 on three runs), at renderer bring-up — it reaches
`game loaded: place 0` and dies where the full-document run creates its
swapchain. Tempting as a control, it is not one: a bisection over the 22,326
keys crashed on *both* halves at the first split, so the engine requires many
flags rather than any single one, and there is no culprit to name. Dropping all
440 `Graphics`/`Vulkan`/`Render` keys, by contrast, runs clean.

### The next thing

The pinned core now has a testable route. The h2/h3 pair — focus-on with motion
at 128.1% CPU against focus-off with motion at 27.5%, both at 59.6 presents/s —
can be re-run against TaskScheduler flag candidates with `DFIntTaskSchedulerTargetFps`
alongside as the control that proves delivery in that same session. That is the
first time the spin has been approachable by flag at all.

## §50: two real users cannot launch at all, on two packaging formats, and §19's
## crash is why — reproduced, not merely inferred, though the machine
## difference is not

2026-08-30. GitHub issue #21 (AppImage, CachyOS) and a second, independent
report (Flatpak, also CachyOS) both hit exactly:

    RBXCRASH: FatalRuntimeError (Can't initialize the TaskScheduler before flags have been loaded)
    shell: the client signal: 5 (SIGTRAP) (core dumped)

This is §19's crash, not a new one, and §19–§23 already explain the mechanism
in full: `nativeGameGlobalInit` (`call_globals` in
`crates/cordial-runtime/src/bin/load.rs`) makes the engine spawn its own "Main"
thread, which "independently races through the same StartLuaAppDM machinery"
Cordial's own explicit `nativeAppBridgeStartLuaAppDM` /
`nativeAppBridgeV2StartAppWithParams` calls drive on the caller's thread — a
race the comment above that function names but nobody had previously connected
to a live user-facing crash. §23's fix (the late
`nativePostClientSettingsLoadedInitialization3` retry, `CORDIAL_LATE_POST_MS`,
default 250 ms) makes flags reliably load *for the default path this project
tests* — but it runs only after `game_activity::start()` hands the surface to
the engine, which is *after* the app-bridge block above. Both reporters' logs
end between `app bridge initialised` (`nativeAppBridgeV2InitWithParams`
returning) and the surface handoff — reporter B's full log
(`nativeGameGlobalInit ok (late)` / `nativeUpdateAdapterInit ok (late)` /
`app bridge initialised`, then nothing) never reaches the point where §23's fix
would run at all. Reporter A's terminal capture looks like it crashes even
earlier, right after `nativeSetBaseDataDirectories ok`, but that is almost
certainly stdout buffering swallowing the same block whole between one flush
and the crash — Rust's stdout is block-buffered off a tty, and a crash mid-run
does not flush it — not a different, earlier failure. Reporter B's log is
better evidence for this reason and should be preferred over A's terminal
transcript for anything about *where* this happens.

### Confirmed by running, not just by reading

`CORDIAL_LATE_SETTINGS=1` (a `load.rs` knob predating this session, used in
§19 to study a related ordering) reproduces the identical message and signal on
this machine, deterministically, on demand:

    XDG_DATA_HOME=~/.cache/cordial-agent-flags CORDIAL_LATE_SETTINGS=1 \
      gdb -q -batch -ex "set pagination off" -ex run -ex "thread apply all bt 25" \
      --args ./target/release/cordial-run --lib-dir ~/.cache/cordial/lib/x86_64 \
      --apk <base.apk> --host-libc --game-activity --profile <p> --run 8

gives, on the crashing ("Main", LWP == PID) thread:

    RBXCRASH: FatalRuntimeError (Can't initialize the TaskScheduler before flags have been loaded)
    Thread 1 "Main" received signal SIGTRAP, Trace/breakpoint trap.
    0x00007fffcaf38ecd in ?? ()   -- libroblox.so base + 0x6af8ecd, no symbols

27 threads total, all identified, nothing else remarkable — a `util_queue`
thread pool from `libvulkan_intel.so`, the async-io reactor, glib/dconf/gdbus
worker threads, and Cordial's own `looper_poll_once` pump on a *different*
thread also named "Main" (the engine renames more than one thread "Main";
AGENTS.md already knew this from `/proc/<pid>/comm`, this is the same fact from
a live backtrace with three "Main"s in one process).

### What was tried and did not reproduce it

The default, zero-env-var path — the one both reporters actually ran — does
not crash on this host. Nine attempts:

* 6 plain launches, fresh profile each time (`XDG_DATA_HOME=~/.cache/cordial-agent-flags`,
  `--profile agent-flags-def{1..6}`), all reached `app ready: Landing` clean.
* 3 more under `stress-ng --cpu 4 --timeout 40s` running concurrently, same
  result.

So the race §19/§23 describe is real and reproducible by name, but *why it
loses on CachyOS and not here* is not established — only guessed at. Candidates
not yet tested: CachyOS's scheduler defaults (several CachyOS installs run a
sched-ext scheduler such as `scx_lavd`/`scx_bore` instead of stock CFS/EEVDF,
which would change thread wake-up latency in exactly the way that would matter
here), a faster CPU shortening the window Cordial's main thread has to reach
the late-post fix before the engine's spawned thread gets there first, and
first-launch state (see below). None of these were run; they are what the next
session should try, ideally on an actual CachyOS box or under a sched-ext
scheduler on this one if `scx` is installable without a reboot.

### The flag-richness difference between the two reports is real and unexplained

Reporter A's initial `nativeInitializeNativeFlags` enumeration (139 flags) is
almost all `not found`, with a handful of explicit `= false` values. Reporter
B's is almost all real `= true`/`= false`. Both still crash. This document's
own model says this list reflects whatever document `client_settings::load()`
(`crates/cordial-runtime/src/client_settings.rs`) handed the engine via
`nativeInitClientSettings` during `bootstrapTheApp` — a live fetch, a fresh
`~/.cache/cordial/clientsettings.json`, a stale one, or nothing — and that this
happens well before the app-bridge race above, on the same thread, before
`nativeGameGlobalInit` is ever called. Nothing here shows that richness
difference changes *when* the app-bridge sequence runs relative to the
engine's own spawned thread, so it is recorded as a difference **without a
demonstrated causal link to the crash**, not ruled in or out. Untested: forcing
a `None` (`load_base` failure, no cache, no network) settings document on this
host and re-running the app-bridge sequence to see whether that alone moves the
race.

### Sober's tracker has nothing

`tools/sober-corpus/data/raw.jsonl` has zero issues matching "initialize the
TaskScheduler" or "TaskScheduler before flags" in title, body or comments,
despite the corpus's 2,000+ issues and heavy overlap with Cordial's other
startup crashes. Consistent with §22/§45's finding that Sober's own startup
ordering (settings → the missing block → `RbxStorage::init`, in that order,
with the app-bridge calls arriving only afterward per §35.1) never puts the two
races Cordial's does next to each other. Not proof Sober is immune — its issue
tracker is not exhaustive and a crash-on-first-launch is exactly the kind of
report a frustrated user files as a generic "won't start" — but it is a real
negative result from the one corpus available.

### mocktail's `ForceNativeFlagsLoadedForTaskScheduler`, read directly, per the standing instruction to check the gate before concluding anything

`src/legacy/legacy_runtime.cc:13354`. The gate is
`IsEnabled("MOCKTAIL_PATCH_NATIVE_FLAGS_LOADED")`, defaulted to `"1"` by
`SetEnvDefault` at line 2818 — on by default, not opt-in. What it does when
enabled: `mprotect`s and writes `1` to `g_libroblox_base +
kRobloxNativeFlagsLoadedByteOffset`, where the offset (`0x75a8250`, line 713)
is a **hardcoded constant with no build-ID check** tying it to whichever
`libroblox.so` is actually loaded. It sits in a block of similarly-named
constants (`kStage6AssetPathNativeSetVtableCallFallbackOffset`,
`kV2StartAppNullBucketTableReadOffset`, and a dozen more) that are unambiguously
reverse-engineered byte offsets for one specific build — the file is named
`legacy_runtime.cc` for a reason. There is a readability check
(`IsReadableMemoryRange`) before the write, so it fails closed rather than
corrupting arbitrary memory on a build where the offset does not land on the
real flag, but nothing here confirms whether that offset is still meaningful,
still misses, or lands on something else entirely for the 2.734.0.917 build
Cordial loads — this project did not run mocktail against that exact library
to watch it. That untested half is exactly what CLAUDE.md's warning about this
function describes: it is easy to over-read this as proof mocktail
memory-patches its way past a check Cordial could too, and ADR-001/ADR-003
forbid that regardless of whether the patch is live on our build.

What the function is worth here is not the patch — Cordial cannot and will not
do this — but the confirmation that a real, singular, hard byte-level
"flags loaded" gate exists inside the engine at all, one that a working
alternative implementation found necessary to force past rather than satisfy
honestly. That corroborates §19's reading of the `RBXCRASH` string as a literal
assertion rather than a red herring, which is the load-bearing fact for
everything above.

### Where this leaves it

Not fixed. Established, by running:

* The crash is §19's `Can't initialize the TaskScheduler before flags have been
  loaded`, reproduced on demand via `CORDIAL_LATE_SETTINGS=1`, matching both
  reporters' error string and signal exactly.
* Both reporters' processes die in the app-bridge block
  (`nativeGameGlobalInit`/`nativeAppBridgeStartLuaAppDM`/
  `nativeAppBridgeV2StartAppWithParams`), which runs *before* §23's late-post
  fix ever gets a chance to mark flags loaded — so that fix cannot be the
  answer here even though it fixed the RbxStorage question it was built for.
* The race is the one `call_globals`'s own comment already named: the engine's
  self-spawned "Main" thread against Cordial's synchronous continuation. This
  had not previously been connected to a crash anyone had actually seen in the
  wild.
* `docs/traces/waydroid-roblox-startup.log.gz` corroborates the shape of the
  fix from the other side: real Android's `rbx.appshell` log says
  `GetClientSettingsTask onPostExecute initialized TaskScheduler` **after**
  `RbxStorage::init [INIT] user: flagLoaded` has already started — i.e. the
  real host app does not call whatever brings the TaskScheduler up until its
  own settings-fetch task has completed. Cordial's `nativeGameGlobalInit` call
  has no equivalent gate; it fires on a fixed point in the bootstrap sequence
  regardless of whether flags are confirmed loaded yet.

**INFERRED, not established:** that this is what tips on CachyOS specifically,
and why. Nine attempts to lose the same race on the default path on this
Fedora host, including under CPU contention, all failed to reproduce it. A
scheduler difference (CachyOS's sched-ext defaults) is the leading candidate
and is untested. The flag-richness difference between the two reports is real
and also unconnected to a mechanism.

**The comment this section corrects** is `native/init_params.cpp`'s
`onFlagsFailed` hook: "this blocks neither startup nor the content store" is
right about the hook itself and wrong as a blanket claim about the state it
reports — corrected in place, same commit as this section.

## §51. §50 conflated two bugs, there is no signal to gate on, and the fix is visibility plus real timeouts, not a wait

2026-08-30, later the same day. §50 read both GitHub issue #21 (AppImage,
CachyOS) and its independent Flatpak report as the same crash, reproduced by
`CORDIAL_LATE_SETTINGS=1`, with a fix shaped as "make the post-settings call
run before `nativeGameGlobalInit`". A first attempt at that fix was built,
measured at 10/10 clean under the reproducer, and was about to be committed.
It was wrong, caught before the commit by reading the actual issue transcript
rather than reasoning from the crash string alone.

### Reporter A and reporter B are two different bugs

`gh issue view 21`, read directly:

* Reporter A: `nativeInitClientSettings -> 1` (the document was **rejected**),
  and the flag enumeration that follows resolves roughly ten of a hundred and
  thirty-nine flags — the rest are `not found`, against eighty-one of a
  hundred and thirty-nine on a healthy machine. The crash lands immediately
  after `nativeSetBaseDataDirectories ok`, hundreds of milliseconds *before*
  `nativeGameGlobalInit` and long before the app bridge.
* Reporter B: a real, accepted document, `app bridge initialised` reached, and
  the crash comes anyway, later, matching §50's original framing.

Reporter A's flags did not fail to load because of a race. They failed to
load because the document was refused, and the engine's assertion —
`Can't initialize the TaskScheduler before flags have been loaded` — was
telling the literal truth. A gate keyed on "wait until flags are loaded" is
permanently shut for reporter A and would have turned their crash into a hang,
which is worse: `cordial-shell/src/window.rs` routes any non-zero exit to a
page headed "Roblox stopped unexpectedly", and a hang shows nothing at all.

### There is no signal to wait on, and the abandoned fix would have raced anyway

Two more things settle it, both checked directly rather than assumed:

* `native/init_params.cpp`'s `onFlagsFailed` (line 1147) and `onFlagsLoaded`
  (line 1165) are both a bare `fprintf(stderr, ...)` and nothing else. Neither
  sets a flag, signals a channel, or does anything Rust code could poll or
  wait on. `onFlagsFailed` fires twice on every healthy run regardless.
* The abandoned fix's own justifying comment claimed `bootstrapTheApp` runs
  "synchronously, from inside `initializeNativeCode`, before Cordial's own
  thread has made a single app-bridge call". This is false, and two log files
  already on this machine prove it without needing a new run:

      ~/.cache/cordial-agent-bisect/logs/pilot-auto.log:
        51:  native handle 0x7f913c584680
        52:  bootstrapTheApp: delivering settings and flags

      ~/.cache/cordial-agent-gamepad-impl/control.log:
        51:  bootstrapTheApp: delivering settings and flags
        52:  native handle 0x7f17a8584680

  Both lines come from Cordial's own single calling thread's `println!`
  sequence at that point in the program; two different orderings across two
  real runs means `bootstrapTheApp` is **not** guaranteed to run before
  Cordial's own thread proceeds — it runs on the engine's own schedule,
  asynchronously, and can land either before or after. The BootstrapPlan is
  also captured once into `static BOOTSTRAP: OnceLock<BootstrapPlan>`
  (§bootstrap plan construction) and never rebuilt, so even a correctly-timed
  wait would be waiting on a value that cannot change. Between an
  unrebuildable value and no completion signal, "add a gate" was retracted
  before it was committed.

### What was built instead, and why each piece is that shape

No wait was added anywhere. Three changes, all either purely additive
(diagnostics) or reusing an existing, already-tested component (timeouts):

1. **`client_settings::fetch` has real timeouts.** It was a bare
   `ureq::get(URL).call()` — no connect timeout, no read timeout, nothing but
   an unset default. It now goes through `cordial_update::http::get_text`,
   the client this project already uses for its version/changelog/APK-metadata
   fetches, built on `cordial-update/src/http.rs`'s `CONNECT` (10s) and
   `TIMEOUT` (20s) constants — taken from that file, not re-derived, so the
   two cannot drift apart. It also gets `url_policy`'s host-locked redirect
   handling, which the bare call never had.
2. **The default launch path now prints what the explicit fallback path
   always has.** `nativeInitClientSettings`'s fallback call site has printed
   `"  client settings: N bytes"` since before this session. `bootstrapTheApp`
   — the path every ordinary launch actually takes — printed nothing. Reporter
   A's log has a silent gap between `nativeSetCacheDirectory ok (early)` and
   `bootstrapTheApp installed` with no way to tell whether an unreadable
   `--client-settings` path, a fetch that never connected, or a fetch that
   connected and was refused produced the empty document that followed.
   `client_settings::Source` names which of those four happened
   (`Explicit`/`FreshCache`/`Fetched`/`StaleCache`/`Nothing(reason)`), a new
   `load_reporting` returns it alongside the document, `BootstrapPlan` carries
   it as `settings_source`, and `run_bootstrap` prints
   `"  client settings: N bytes (SOURCE)"` on every launch.
3. **An unreadable `--client-settings` path now says so.** It was
   `std::fs::read_to_string(path).ok()`, silently: indistinguishable in the log
   from a healthy launch with a genuinely empty document. It now prints the
   `io::Error` and returns `Source::Nothing(reason)`.

None of this can crash a healthy launch or refuse one that would otherwise
succeed: `load_reporting`'s failure paths still resolve to `None` →
`unwrap_or_default()` → `""`, exactly as `load()` did before, so a client that
cannot reach the CDN and has no cache still launches with an empty document
rather than exiting — the comment on `client_settings::load` already said this
was the intent, and nothing here changes it.

### The decisive local reproduction, found by testing what reporter A actually reported rather than reusing the existing knob

`CORDIAL_LATE_SETTINGS=1` reproduces *a* `TaskScheduler`-before-flags crash,
but not reporter A's: it starves the engine of the handshake by moving it
*after the app bridge*, which is a timing story, not a rejected-document
story. The cheap, decisive test is `--client-settings` pointing at an empty
file — a document that reads successfully (so it is not the unreadable-path
case) but is empty (so the engine rejects it, matching reporter A's
`nativeInitClientSettings -> 1`):

    env -u CORDIAL_LATE_SETTINGS ./cordial-run ... --client-settings /path/to/empty.json

    45:  bootstrapTheApp installed
    54:  bootstrapTheApp: delivering settings and flags
    55:  client settings: 0 bytes (--client-settings)
    229:  nativeSetBaseDataDirectories ok
    230:RBXCRASH: FatalRuntimeError (Can't initialize the TaskScheduler before flags have been loaded)

Landing exactly where reporter A's did — immediately after
`nativeSetBaseDataDirectories ok`, before `nativeGameGlobalInit` is ever
reached. This is the reproduction `CORDIAL_LATE_SETTINGS=1` was standing in
for and never actually was.

### Measured, interleaved, same session, same host

A "fix" build (the three changes above) and a "control" build (`git stash`
of just `client_settings.rs` and `load.rs`, rebuilt in the same
`CARGO_TARGET_DIR`, binary copied out before popping the stash back) run
alternately against the same APK and library:

| | default launch (healthy) | `--client-settings <empty file>` |
|---|---|---|
| control, 5–10 runs | 10/10 clean, `app ready` reached | 5/5 `RBXCRASH`, exit 133 |
| fix, 5–10 runs | 10/10 clean, `app ready` reached, `client settings: N bytes (SOURCE)` printed every time | 5/5 `RBXCRASH`, exit 133, plus `client settings: 0 bytes (--client-settings)` printed every time |

The empty-settings crash is identical on both builds, on purpose: it is a
real, correct engine assertion given a genuinely empty document, and nothing
here claims to fix it — only to make it legible instead of a silent gap
followed by a signal. `CORDIAL_LATE_SETTINGS=1` was also re-checked
unaffected (3/3 `RBXCRASH`, unchanged) since neither change touches that code
path.

`cargo build --release` and `cargo test --workspace` both pass on the fix
tree — 332 passed in `cordial-runtime`'s own suite (two new tests:
`an_explicit_path_bypasses_the_network` extended to check `Source::Explicit`,
and `an_unreadable_explicit_path_says_why_rather_than_just_no`), 0 failed,
across two full runs. An earlier run of the full workspace suite showed two
unrelated `secrets::tests` failures (`the secret service did not answer within
5 seconds`) that vanished on retry and passed 10/10 in isolation
(`--test-threads=1`); `df`/`free`/`uptime` at the time showed this host at
100% disk (recovered to single digits of GB free mid-session) and a load
average above 48, so that failure is attributed to D-Bus contention under
concurrent agent load on a shared machine, not to this change — `secrets.rs`
has no diff in this session.

### What this does not fix, and is not claimed to

**Reporter B is still open.** Their document was accepted and they still
crashed, later, past the app bridge. §50's `nativeGameGlobalInit` race theory
was never disproved for reporter B specifically — only shown insufficient as
an account of reporter A, and shown to rest on a false "synchronous" premise
that undermines the specific gate that was proposed. Whether reporter B's
crash is the same race landing later, a second and different bug, or
something the timeout/visibility changes here happen to also help with
(a slow-but-eventually-successful fetch racing the app bridge under the old
unbounded timeout) is **not established** and would need reporter B's actual
log, which this session did not have.

**Why CachyOS specifically loses whichever race remains is still INFERRED,
not established** — unchanged from §50.

**Whether real timeouts would have changed reporter A's own outcome is not
established either.** Their `bootstrapTheApp installed` came 23ms after
`nativeSetCacheDirectory ok (early)`, which is too fast to be a real CDN round
trip timing out — more consistent with a fast DNS/connect failure and no
existing cache on a first launch than with a hang the new timeouts would
interrupt. The timeouts are a defensible, cheap improvement against a
*different* plausible failure (a connection that is accepted and then stalls,
which the old code could wait on indefinitely) rather than a demonstrated fix
for what actually happened to reporter A. What *is* demonstrated is that
reporter A's specific symptom — document rejected, crash at
`nativeSetBaseDataDirectories` — now says so in the log instead of leaving the
gap that cost the previous session most of a day.

### A methodological note, since this file already carries two of its kind

The first attempt at this fix reached 10/10 clean under `CORDIAL_LATE_SETTINGS=1`
and very nearly became a commit on that strength alone. The reproducer was
real and the number was real; what was wrong was believing it was reporter
A's bug. §22 and §23.5 both record an instrument that could not see the thing
it was being asked about; this is a third shape of the same mistake —an
instrument that could see *something*, correctly, and it was the wrong
something. Reading the actual report before trusting a same-shaped local
reproduction would have caught it a session earlier.
