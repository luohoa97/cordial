# What Cordial costs at startup and at rest, against two controls

**Status:** measurement only, nothing in the tree modified. Three clients
running the same Roblox 2.734.917 engine on one host, on the landing page, no
experience joined. Sober 1.7.1 and mocktail 1.0.3 (both Flatpak) are the
controls; Cordial is `v0.6.0-32-gd30d7c0-dirty`, where `-dirty` is the
`third_party/mcpelauncher-linker` submodule pointer and nothing else — the
crates were clean at build time (`cargo` reported nothing to rebuild at the
commit).

Host: 13th Gen Intel i7-13620H, **16 logical cores**, 15.7 GB RAM, Intel UHD
(i915), Linux 7.1.8. All runs on the maintainer's own Wayland session at
3440x1359, sequentially, one client at a time.

**Bottom line up front:** Cordial is **not slower to start** than either
control, and it is **not heavier at rest** — except that on **three runs out of
nine it fails to enter the engine's idle throttle and then burns a whole CPU
core indefinitely**, where both controls always settle to 8-9% of one core.
That bimodality, not a uniform slowness, is the thing worth chasing, and it is
the best candidate this measurement offers for "it feels laggy". Two reported
beliefs did **not** survive contact: startup is not getting slower in any way
this data can show, and Cordial does **not** exit faster than the others — by
the one exit measure available it is the slowest of the three.

---

## 1. The table

Every cell states its own method. Cells that cannot be compared say so rather
than carry a number of convenience.

| | **Cordial** | **mocktail** | **Sober** |
|---|---|---|---|
| **Startup — engine milestone** `setStage: (stage:LuaApp)`, from process launch. Wall clock, from the engine's own FastLog ISO timestamps. | **not comparable** — Cordial emits no engine FastLog at all (§4) | **4.45 ± 0.32 s** (n=3) | **4.60 ± 0.42 s** (n=3) |
| **Startup — external proxy.** Time from launch until RSS first reaches 90% of its settled value. Derived only from `/proc` sampling, so identical rule for all three. | **4.47 ± 0.70 s** (n=9) | 5.37 ± 1.06 s (n=3) | 5.89 ± 0.29 s (n=3) |
| **Peak CPU during startup**, max over 0.5 s samples, % of one core. | **342 ± 8 %** (n=3) | 142 ± 4 % (n=3) | 153 ± 5 % (n=3) |
| **Idle CPU**, % of one core, fixed 20–45 s window measured from launch (same wall-clock slice for all three). | **bimodal: 103.5 ± 1.5 % on 3 of 9 runs; 6.3 ± 1.1 % on the other 6** | 8.7 ± 0.1 % (n=3) | 7.8 ± 0.1 % (n=3) |
| **RSS at the landing page**, median over the last 30 s, summed across the client's whole process set. | **802 ± 4 MB** (n=3) | 875 ± 304 MB (n=3) | 1131 ± 80 MB (n=3) |
| **PSS** (same window; avoids double-counting shared pages). | **774 ± 2 MB** (n=3) | 822 ± 303 MB (n=3) | 1023 ± 64 MB (n=3) |
| **Memory growth at rest**, least-squares slope of RSS over the final 60 s. | −2.4 ± 5.7 MB/min | +13.3 ± 12.2 MB/min | −30.7 ± 17.2 MB/min |
| **Exit**, SIGTERM to every process in the client's set → last process reaped. | **0.599 ± 0.216 s** (n=9) | 0.302 ± 0.205 s (n=3) | **0.111 ± 0.001 s** (n=3) |
| **Frame rate** | **not comparable** (§5) | **not comparable** (§5) | **not comparable** (§5) |
| Background load during the measured window (system busy fraction, all 16 cores) | 0.120 ± 0.014 | 0.077 ± 0.013 | 0.064 ± 0.016 |

No growth figure is resolvable: in all three the slope is smaller than its own
standard deviation over a 60 s fit. Treat the row as "nothing detectable at
this window length", not as three measured rates. Sober's and mocktail's large
RSS spreads are real — both *free* several hundred megabytes about a minute in,
so where the window falls matters; Cordial's spread of 4 MB shows it does not.

---

## 2. Why the external startup proxy is trustworthy

"Time until RSS reaches 90% of its settled value" is a proxy, and a proxy is
worth what it was checked against. On the two clients where the true engine
milestone *is* available, the proxy can be calibrated directly: it overshoots
`setStage: (stage:LuaApp)` by **0.92 ± 1.15 s** on mocktail and **1.29 ± 0.18 s**
on Sober. So the proxy consistently lands about a second late, in the same
direction, on both.

Applying that correction to Cordial's 4.47 s proxy puts its landing page at
roughly 3.2–3.6 s — i.e. **Cordial is if anything the fastest of the three to
the landing page**, and is certainly not slower. That is the opposite of the
reported impression, and it is the claim this document is most confident about,
because the proxy is the only measure here computed identically for all three.

Startup timing is also where Cordial spends the most CPU: **342% of one core at
peak against 142% and 153%**. It reaches the landing page at least as fast while
using more than twice the CPU to get there. On a 16-core desktop that is free;
on a laptop on battery it is not.

---

## 3. The finding that matters: Cordial sometimes never idles

Sampling CPU in 10 s buckets over nine Cordial runs splits them cleanly into two
populations with nothing in between:

```
cordial-1   [166, 105, 105, 105,  99]                     pinned   (tail median 102%)
cordial-2   [165, 105, 105, 105, 105, 105, 103, 103, ...] pinned   (tail median 103%)
cordial-5   [163, 106, 105, 105, 105, 106, 104]           pinned   (tail median 105%)
cordial-3   [161, 105,  53,   6,   6,   7,   4, 4, ...]  throttled (tail median   4%)
cordial-4   [ 86,   7,   7,   7,   6,   7,   4]          throttled (tail median 6.5%)
cordial-6   [ 80,   7,   6,   6,   6,   6,   4]          throttled (tail median   6%)
cordial-7   [ 91,   7,   6,   6,   6,   6,   4]          throttled (tail median   6%)
cordial-8   [ 90,   7,   7,   6,   6,   7,   5]          throttled (tail median   6%)
cordial-9   [151,   9,   9,   8,   9,   7,   6]          throttled (tail median 7.5%)
```

**Three of nine pinned at ~104% of a core for as long as the run lasted** (up to
150 s, never recovering). Six of nine dropped to 4–7%. Both controls, every run,
settled to 8–9% and stayed:

```
mocktail-2  [43, 10, 10, 11, 10, 10,  9,  9,  8,  9, ...]  tail 8.7 ± 0.1 %
sober-2     [71,  9, 10,  9,  9,  9, 12,  7,  8,  7, ...]  tail 7.8 ± 0.1 %
```

Two things follow, and they point in opposite directions:

**When Cordial idles, it idles better than either control** — 6.3% against 8.7%
and 7.8%. There is no general inefficiency to find here.

**When it does not, it costs twelve times what the controls do**, permanently,
on the landing page, with nothing happening. One core at 100% is enough to spin
laptop fans, drain a battery, and make everything else on the machine feel
worse — which is the shape of the reported complaint.

`cordial-3` is the informative one: it shows the transition happening *late*
(`161, 105, 53, 6, ...` — pinned for ~20 s, then dropping). So the throttle is
not simply on or off from the start; it is a race that is sometimes lost
outright. The 3-in-9 rate is close to the "roughly one launch in three" figure
AGENTS.md records for another bug, which may or may not be coincidence.

**What I did not establish:** *why*. The obvious candidate is window occlusion
or focus — these ran on the maintainer's live session while they were using it,
so some windows will have been covered and some not, and I had no way to record
which. That confound is real and I cannot exclude it. Equally it may be the
engine's own idle throttle (the one that drops presents to exactly 1.0/s,
recorded in AGENTS.md) failing to engage. **Distinguishing those two is the next
experiment**, and it is cheap: run Cordial with the window deliberately raised
and then deliberately covered, several times each, and see whether the mode
follows visibility. I did not run it because it needs a controlled display the
Flatpaks cannot share (§6).

---

## 4. Cordial emits no engine log, and no timestamps at all

Sober and mocktail both surface the engine's FastLog with absolute ISO
timestamps, which is what makes their startup chains comparable to each other:

```
[I/Roblox] 2026-08-20T09:45:08.136Z,1.136420,... [FLog::SingleSurfaceApp] setStage: (stage:LuaApp)
info: Roblox: 2026-08-20T05:49:29.229Z,19.229647,... [FLog::AndroidGLView] nativeInitClientSettings
```

Cordial prints **zero** `FLog::`/`DFLog::` lines — only its own `[roblox]`,
`[cordial]`, `[android]` prefixes — so the engine-internal phase breakdown
cannot be compared to either control. Its nearest equivalent is
`[roblox] app ready: Startup` / `Home` / `RootSwitchNavigator` from
`NativeHelper::onAppReady` in `native/init_params.cpp`, which is a different
event and is not claimed here to be the same one.

Worse for the specific question asked: **no line of Cordial's output carries a
timestamp**. I checked all 265 `.log` files under `~/.cache/cordial-*`; 20
contain startup markers and **not one is timestamped**. So the claim that
*startup is getting slower as more is implemented* **cannot be tested against
anything already on disk** — there is no clock in the record. It can only be
tested going forward, either by timestamping the output or by timing runs
externally as this document does.

Every timing here is therefore sourced from outside the process (`date +%s.%N`
immediately before `exec`, plus `/proc` sampling), so these numbers stay valid
whether or not internal timestamps are ever added.

---

## 5. Frame rate: not comparable, and not measured

Not reported for any of the three, for two independent reasons, either of which
alone would be sufficient.

**No common frame counter exists.** Cordial has `vkQueuePresentKHR`
instrumentation; Sober and mocktail have none, and neither can be given any —
they are installed Flatpaks and adding a counter would mean modifying somebody
else's shipped binary. The engine itself logs no frame rate in any of the three;
grepping all three logs for `fps|frame rate|framerate|rendering frequency`
returns only `Register/Restoring rendering frequency`, which is a state change
and not a rate.

**Input cannot be driven identically.** AGENTS.md requires input to flow for the
whole measurement, forbids compositor-level injection, and sanctions exactly one
route — a nested headless compositor. That route is unavailable here because
Cordial cannot run in one (§6).

Present counts without input were not used, per AGENTS.md; no proxy was
substituted.

---

## 6. Cordial cannot run under a nested headless compositor

Reported separately because it is a Cordial bug, not merely an obstacle to this
measurement.

Under `mutter 50.4` started as
`mutter --headless --wayland --no-x11 --wayland-display=<name> --virtual-monitor 1280x720@60`,
**the compositor segfaults within about 5–15 s of Cordial connecting**, every
time it was tried (3 observations):

```
mutter[3755670]: Using Wayland display name 'wayland-cordial-bench'   19:56:33
systemd: cordial-bench-mutter.service: Main process exited, code=dumped, status=11/SEGV   19:56:38
```

Cordial then dies of `Gdk-Message: Error flushing display: Broken pipe`. It gets
as far as `app ready: PlatformAccountRouter` and `pumping the looper` first, so
this is not a failure to start.

**mocktail on the identical compositor ran a full 45 s and reported
`display refresh current=60.000 Hz`.** So the crash is specific to Cordial's
Wayland client, not to headless mutter or to this host.

Under `mutter --devkit` (nested rather than headless) Cordial instead panicked
3/3 with `Gtk has to be initialized before using libadwaita` — that one was a
genuine bug at the then-current commit and has since been fixed in `d30d7c0`;
the headless SEGV above was reproduced *after* that fix and is unrelated.

Consequence beyond this document: the sanctioned route for driving input at a
window that is not Cordial's own is a nested compositor, and Cordial cannot
currently live in one. Any future frame-rate or input-latency comparison is
blocked behind this.

---

## 7. Method, and what would invalidate it

Nine runs in the reported matrix (3 per client, interleaved
`cordial, mocktail, sober` × 3 so drift affects all three alike), 150 s each,
plus six extra Cordial runs of 70 s for the idle-mode denominator in §3.
Sampling at 2 Hz throughout.

The process set for each client is resolved differently, and getting this wrong
is the trap worth recording. **Sober runs the engine as a different uid in a
different session** (`/proc/self/exe`, uid 10156, ~1 GB RSS), so walking the
launched process's session or its descendants finds only the launcher and
reports **48 MB** — which reads as an extraordinarily light client rather than a
missed one. The fix is to select by the Flatpak's own systemd scope
(`app-flatpak-org.vinegarhq.Sober-*.scope`, recursively), which reports 1199 MB
across 8 processes. Cordial is selected by setsid session instead, because its
cgroup is the launching shell's scope and would sweep in half the desktop.
Descendant-walking fails for all three: `flatpak run` and `cordial-run` both
hand off to a child and let the parent go.

CPU is `utime+stime` summed across the set, differenced between samples, over
`SC_CLK_TCK`; reported as percent of **one** core, of 16.

Threats to these numbers, in the order I would attack them:

- **Occlusion is uncontrolled.** Runs were on a session in active use. This is
  the live confound for §3 and possibly inflates the variance everywhere else.
  Background load is reported per row so a figure taken during a busy period is
  at least visible as such; it stayed between 0.05 and 0.14 of the machine.
- **Another agent's `cordial-run` was resident throughout**, idling at 2.8% of
  one core. Small against 16 cores, and included in the reported background
  load, but not zero.
- **n=3 per client** for everything except the Cordial idle-mode split (n=9) and
  Cordial exit (n=9). The idle-CPU and exit figures for the controls are tight
  enough (SD ≤ 0.2% and ≤ 0.2 s) that more runs would not move them; the memory
  and growth rows are not.
- **Exit is SIGTERM, not a window close.** A user quits by closing the window;
  SIGTERM was chosen because it is the one stimulus all three accept
  identically. Cordial does real work on the way out (writes
  `cordial-unimplemented.log`, unregisters gamemode), which plausibly explains
  why it is slowest here while still *feeling* fastest. The reported belief that
  Cordial exits faster is **not confirmed by this measure**, and may simply be
  measuring something this measure does not.


## Correction: §3's "3 of 9 never enter the throttle" is not bimodality

Measured afterwards with a per-thread census in `looper_poll_once`, 15 runs.

**The spin is one thread making one syscall.** A single `Main` thread — the AGDK
looper-service thread, not the pump — holds 99–100% of a core. Across one 40 s
run it made **123,885,088** `ALooper_pollOnce` calls, **every one with
`timeout_millis == 0`**, with `empty=123,885,086` and **`unclaimed=0`**. No
descriptor is permanently ready and nothing is undrained: the engine asks not to
wait and Cordial does not wait. At ~110 ns a call, 8.9 M/s is 1.0 core, which is
the number observed.

**It is gated entirely on the focus report, deterministically, both
directions.** Scripted focus, three runs, 136 one-second samples: focus reported
true gives 8.90 M polls/s and 102.2% of a core; focus reported false gives 20
polls/s and 4.3%. Unscripted, 8 of 8 runs split the same way — every run with no
`focus -> false` transition pinned for its whole length, every run that got one
collapsed within a pump tick. One run did both, in sequence.

So the split this document recorded as bimodal was **whether Cordial's window
happened to hold compositor focus during the sample**, not a race in the engine
and not a lost transition. The earlier framing is withdrawn.

Presents sat at 1.0/s throughout both halves, which is why this reads as "the
frame rate idles fine but a core is busy": the renderer and the event loop idle
on different signals.

## The controls idle at 8%. Cordial burns a core. The difference is one call

Measured in one session, three clients, 143 s each, 2 Hz, median over t=60–140 s
(160 samples), as percent of one core of sixteen. System busy 0.057–0.112
throughout. Two independent instruments — `/proc/<pid>/stat` and cgroup
`cpu.stat` — agree within one point on both Flatpaks.

    cordial c1                    103.3 %      focus reported true 146/146
    cordial c2                    103.8 %      focus reported true 138/138
    cordial cf1 (report forced)   103.5 %      window genuinely unfocused
    cordial V-instr                 4.0 %      unfocused 142/146
    mocktail m1 / m2            8.1 / 9.9 %
    sober    s1 / s2            8.0 / 8.0 %

`cf1` is the control that settles Cordial's half: a genuinely unfocused window
with only the *report* forced true still costs 103.5 %. Same compositor state as
the 4.0 % run, opposite CPU. **It is the report, not occlusion and not
visibility.**

### Why the controls are cheap, read from mocktail's source

**`onWindowFocusChangedNative` appears nowhere in mocktail** — not in `src/`,
not in `include/`, not in `stubs/`. Nothing in mocktail can tell the engine that
focus changed, ever. Cordial calls it at `native/game_activity.cpp:818`, plus
`true` inline at start and on every transition since today's work.

**So the engine state that costs Cordial a core is one mocktail never puts its
engine into.** `INFERRED` — read from source, not yet measured — but it is the
only behavioural difference left standing after the obvious ones were checked
and eliminated:

- **The looper is not it.** mocktail's `ALooper_pollOnce` is a bare stub
  (`stubs/libandroid_stub.cc:491`, `return -2`), returning immediately exactly
  as Cordial's does. There is nothing cleverer to copy.
- **AGDK glue is not it.** Neither project links Google's; `game_activity.cpp`
  is Cordial's own. The spinning `Main` thread — 11.8 M polls/s against the
  pump's 36/s — is inside `libroblox` in both.

### The next experiment, and it is a measurement rather than a fix

Stop reporting `focus = true` at `cordial_game_activity_start` and see whether
Cordial idles like mocktail. **It will probably break input**, so it answers the
question without being shippable. If it does idle, the honest fix is not to stop
telling the engine the truth but to find what the engine does with that truth
that costs a core, and whether the two can be separated.

### Focus is granted stochastically, which is why this was not caught earlier

A background-launched window took focus in 2 of 3 identically-launched runs on
this session. "Left alone means unfocused" is not safe, and the original
comparison's silence about focus state is not recoverable after the fact —
which is why its idle column read as bimodal rather than as two different
conditions.

Sober's and mocktail's own focus state could not be observed at all: GNOME's
`Introspect` returns `AccessDenied`, and neither Flatpak appears in the
accessibility tree (Cordial does). So their 8 % is *not* known to be a focused
figure, and the comparison against Cordial's 103 % is suggestive rather than
like-for-like.

## Two candidates checked and killed: the present mode, and the Choreographer

Both were proposed here as the mechanism that paces the engine's loop on real
Android and is missing on this one. Neither survived. Recorded because the
reasoning behind each was plausible, which is exactly why an unrun version of it
would have been believed.

### The present mode is not it

`vk_create_swapchain_khr` substitutes `VK_PRESENT_MODE_MAILBOX_KHR` for the
`VK_PRESENT_MODE_FIFO_KHR` the engine asks for, and that is the one place
Cordial overrides a choice the engine made explicitly. The argument was that
FIFO blocks and MAILBOX does not, so a loop built to be paced by the display
would free-run here and spin — which would have made the spin ours, and the fix
a reversal rather than an addition.

Four runs, alternating, 60 s each, focus held for every sample in all four
(`focus=Some(true)`, 58/58, 58/58, 58/58, 54/54):

    a1  FIFO -> MAILBOX (the override)   9.37 M polls/s   105.2 %
    b1  FIFO (CORDIAL_PRESENT_MODE=fifo) 9.36 M polls/s   105.3 %
    a2  FIFO -> MAILBOX                  9.31 M polls/s   105.2 %
    b2  FIFO                             9.33 M polls/s   105.0 %

Within 0.4 % of each other on both instruments. **The override is not the
cause.**

The reason is in the same logs and should have been read before the hypothesis
was formed: **presents sit at 1.0/s in every run, in both modes.** FIFO only
blocks when the queue is full, and a loop presenting once a second never fills
it, so `vkQueuePresentKHR` returns immediately under FIFO exactly as it does
under MAILBOX. There was never a vsync block here to restore. The renderer and
the spinning loop idle on different signals — as this document already said two
sections above.

**`minImageCount` was the one real defect the hypothesis turned up, and it turned
out not to exist either.** Substituting a replacement mode into a swapchain
sized for a queue would leave two images where three are needed, and the
renderer would stall in `vkAcquireNextImageKHR` waiting for the refresh the
substitution exists to skip. Logged rather than assumed: the engine asks for
`minImageCount 3` on every swapchain it creates here, including the recreation
after the first resize. `vk_create_swapchain_khr` now raises it anyway when a
replacement mode is chosen and the engine asked for fewer, and says so; on
today's build that branch is not taken.

### The Choreographer is not it either

`docs/NEXT.md` records "frame-callback starvation is not it — `AChoreographer_*`
is not imported at all", which is true of the NDK entry points and says nothing
about `android.view.Choreographer` reached through JNI. Those are different
paths and only the first had been checked. `Landroid/view/Choreographer;` and
`Landroid/view/Choreographer$FrameCallback;` both appear in
`docs/analysis/framework-classes.txt`, which read like the engine asking for
them.

It is not. That file is a framework inventory of 3466 classes, not a request
log. `--dump-classes` is the request log, and in a 30 s run the engine reaches
for **182 classes, none of them Choreographer**. Nothing here is starving a
frame callback because nothing here is asked for one.

### What is left

Unchanged from the section above: the focus report is the only known difference,
and mocktail — which idles at 8 % — has no way to send one. Note what that makes
mocktail's 8 %: not a target Cordial should match, but the cost of an engine that
was never told it has focus. Whether that state is one a client can ship is a
separate question from whether it is cheap.

## The two spinning `Main` threads are two different things, and only one is ours

Both threads are named `Main`, which is most of why they read as one problem.

### The pump's, which was not a blackhole and is now fixed

    before   ret[empty=842  cb=2  ident=0    unclaimed=829  lastfd=26]
    after    ret[empty=635  cb=2  ident=679  unclaimed=0    lastfd=0]

`unclaimed` is the census's name for the expensive bug it exists to catch: a
descriptor that keeps reporting ready and that nothing drains, which turns any
zero-timeout caller into a spin. Half this thread's returns were counted that
way, naming fd 26.

**It was never that.** fd 26 is the display connection, added to epoll by
`watch_input_fd` so a keypress ends the wait immediately, and drained by GTK
inside `pump_input_events` one call later in the same loop iteration. What was
missing was the `Registration`, so `pollOnce` could not find an owner for the
descriptor it had just been handed and counted it as nobody's. `watch_input_fd`
now registers it under `IDENT_DISPLAY_CONNECTION`; nothing reads that ident (the
pump passes three null out-parameters and ignores the return, and the engine's
thread has its own looper and its own epoll), so this changes the accounting and
nothing else. Verified on two runs: `unclaimed=0`, `lastfd=0`.

A false positive in the one instrument built to catch real blackholes costs more
than it saves, and this one had already been written up here as a latent spin.

### The engine's, which is not a fallback from being starved

The hypothesis was that Roblox asks with a timeout, gets no answer, and retries
without one. The counters say no, in both directions:

    block=7   zero=586,520      ident=7
    block=6   zero=537,689,701  ident=6   (the 60 s focused run)

The engine passes a blocking timeout six or seven times in an entire run and a
zero timeout for everything else, so it is not falling back to spinning — it
spins by construction. And every blocking call it did make was answered: `block`
and `ident` are equal in every run recorded here. Nothing is being starved into
a spin.

`ALooper_addFd` now prints unconditionally, and the engine registers exactly two
descriptors for the life of the process:

    ALooper_addFd(fd=16, ident=0, events=1, callback=yes)
    ALooper_addFd(fd=18, ident=1, events=1, callback=no)

`ident=1` with no callback is the shape of AGDK's `LOOPER_ID_MAIN`, the pipe the
Java side writes app commands into. What writes to these two in Cordial, and
what Android would write to them that Cordial does not, is the next thing to
establish — and it is a lookup rather than an inference, because both ends are
in this tree.

**This one is not fixed.** It is narrowed: not the present mode, not either
Choreographer, not a starved fallback, not an undrained descriptor of ours.

## The focused engine spins for events it would otherwise be handed, and it costs a core during play

`CORDIAL_SCRIPT=0:focus-on` forces the report true on a window the compositor
never focused, which is the `cf1` control made repeatable — the compositor
refused focus on five consecutive launches on 2026-08-21, so every number below
is from the override rather than from a granted focus.

Three runs, 60 s each, CPU sampled over t=25–53 s:

    h1  focus report on,  no input     1.0 presents/s   1.98 M polls/s   114.5 %
    h2  focus report on,  input flowing 59.6 presents/s  1.83 M polls/s   128.1 %
    h3  focus report off, input flowing 59.6 presents/s  0.00 M polls/s    27.5 %

**h2 against h3 is the control that matters, and it is the first one taken at a
playable frame rate.** Same rendering, same input, same everything; only the
focus report differs. **The report costs about a hundred points of CPU — a whole
core — at 60 fps with input flowing.**

That retires the framing this document has carried since it was written. The
spin is not an idle-only curiosity that a user would never notice. It is present
during play, and on a laptop part like the 13620H a core pinned at turbo is
taking package power and thermal headroom the GPU shares. "Cordial lags worse
than Sober" and "Cordial burns a core" are one report.

### What the engine is actually doing, from its own counters

    h3  block=3452   zero=518,322       ident=3453   epoll_wait~98,821ns
    h2  block=6      zero=110,982,683   ident=3473   epoll_wait~507ns

**`ident` is the same in both — about 3,450, one per frame.** The engine is
handed identical work either way. What the focus report changes is whether it
blocks for that work or spins for it: unfocused it makes ~3,450 blocking calls
and sleeps ~98 µs in each, focused it makes six and busy-polls 111 million times
instead.

So this is a deliberate latency trade inside Roblox — believing itself focused,
it refuses to sleep so that input is picked up in hundreds of nanoseconds rather
than at the next frame boundary. It is not a Cordial bug in the sense of
something answered wrongly, and no amount of feeding the looper better will
change it: the loop is not starved, it is choosing not to wait.

### What is not the cause, all measured

Present mode (MAILBOX vs FIFO, four runs, within 0.4 %), the NDK Choreographer
(not imported), `android.view.Choreographer` (not among the 182 classes the
engine asks for), a starved fallback (`block` and `ident` are equal in every
run), an undrained descriptor of ours (`unclaimed=0` since the pump fix), and a
frame-deadline busy-wait (h3 renders 59.6 fps on 0.00 M polls).

The engine's two registered descriptors are its own: `fd 15`/`fd 18` are the read
ends of two pipes whose write ends are `fd 17`/`fd 19` in the same process. It
signals itself.

### Where this can go

Not "stop reporting focus" — that is a lie, it breaks input, and h3 is not a
shippable configuration. The remaining candidates, in the order they are worth
trying:

1. **A FastFlag.** The spin is the TaskScheduler declining to yield. If Roblox
   governs that with a flag, this is a one-line fix and the machinery to set it
   already exists. `docs/traces/native-flag-names.txt` holds 139 names and none
   of them match; the inventory needs widening before this can be tried, not
   guessed at.
2. **A Cordial setting**, if no flag exists — the trade is real and a user on a
   laptop would take it. It must be described as what it is rather than as a
   performance toggle, because switching it off means telling Roblox something
   untrue.

Both need the flag question answered first.

## The syscall was the pacing, not the cost

The attribution above — 9.3 M `epoll_wait` calls a second at about 99 ns each,
0.92 of a core, some 88% of the idle cost "inside the syscall" — is **wrong**,
and the arithmetic is not where it fails. It assumes that removing the syscall
removes the work.

Tested by removing it. `looper_poll_once` was made to answer up to sixteen
consecutive zero-timeout polls from the previous empty `epoll_wait` rather than
entering the kernel, counted rather than timed because the census timing already
records that a clock read costs about what the syscall costs. Four arms,
alternating with the control in one session, input driven throughout:

    coalescing on    cpu 121.5%  fps 40.6  polls/s 10,650,735
    control (off)    cpu 123.6%  fps 39.9  polls/s  2,534,844
    coalescing on    cpu 124.4%  fps 35.9  polls/s 10,719,074
    control (off)    cpu 124.2%  fps 39.6  polls/s  2,267,928

The change did what it was built to do: one call in seventeen reached the
kernel. **CPU did not move** — 121.5% to 124.4% across every arm, which is
noise. The poll rate went *up* fourfold instead. With the syscall gone the
engine's loop completes each iteration sooner and spins harder for exactly the
same money.

So the loop is CPU-bound and will consume whatever it is given. Nothing done to
the cost of one iteration can help, because the iteration count is not fixed —
it is whatever fits in the time. That retires the whole family of "make
`pollOnce` cheaper" ideas, which is worth more than the two points of noise this
measurement bought.

What remains is the only thing that has ever moved this number: the focus
report. Focused with input costs 128.1%; unfocused with input costs 27.5% at the
same 59.6 presents a second and the same `ident` count. The engine renders
identically and only decides whether to wait or spin. Reporting a focused window
as unfocused would recover a core and would be a lie told to the engine, so it
is a user-facing trade about input latency rather than an optimisation, and it
is not one to make quietly.

## Sober burns the same core, so the comparison this file was built on is wrong

`088e69c` is titled "Sober and mocktail idle at 8%; Cordial burns a core, and it
is one call", and that framing has driven a great deal of work here. It is not a
like-for-like comparison. The 8% was measured on an **idle** Sober; Cordial's
core was measured while **rendering with input driven**. Put both in the same
state and the difference disappears.

Measured 2026-08-21, per-thread from `/proc/<pid>/task/*/stat`:

    Cordial, signed out      111% total   99.7% in one thread named "Main"
    Cordial, signed in       117% total   99.2% in one thread named "Main"
    Sober                    120% total   99.5% in one thread named "Main"
    Sober, two more samples  193%, 197%   99.5%, 99.3% -- same tid throughout

**Sober has not solved this, and Cordial is not worse at it.** The same
single-thread spin is present in both, in a thread with the same name, and
Sober's total was higher in every sample taken. Three samples, one tid,
consistent to a fifth of a percent, with Sober's own log showing live session
heartbeats so it was running rather than parked.

One caveat, stated rather than glossed: the two clients were not in identical
states. Cordial was driven with synthetic input at a known rate; Sober was left
on its own home screen. So the *totals* are not strictly comparable. What is
comparable, and what matters, is the shape: one thread at 100% in both.

### What that thread actually is

`lldb`, on Cordial's hot thread:

    frame #0-3  epoll_wait
    frame #4    looper_poll_once(timeout_millis=0, out_fd=0x0,
                                 out_events=0x7f005fffe184,
                                 out_data=0x7f005fffe178)   looper.rs:1159
    frame #5-7  libroblox, unsymbolised
    frame #8    __clone3

It is **not Cordial's pump**, which calls with a 50 ms timeout and three null
out-parameters. This one passes non-null `out_events` and `out_data` and bottoms
out in `__clone3` under libroblox frames: a thread Roblox created, calling
`ALooper_pollOnce(0, ...)` in a tight loop.

That is the documented Android idiom rather than a defect.
`while ((ident = ALooper_pollOnce(animating ? 0 : -1, ...)) >= 0) { ... }` is
Google's own `native-activity` sample, and a zero timeout while animating means
"drain what is pending, do not block, then draw". It costs nothing on a phone
because presentation paces the loop.

### Two more hypotheses that died

**The display-connection registration is not it.** Removing it entirely -- so
Cordial registers nothing with the looper, as mocktail does -- gives 111.3% at
60 fps, against 111% with it. mocktail's `ALooper_pollOnce` returns a constant
and its `ALooper_addFd` registers nothing, and that is not a fix to copy: a
looper which answers without listening cannot deliver an input event, a wake or
a surface change, which is precisely the shape AGENTS.md's rule about stubs
returning success exists to prevent.

**The present mode is not it either.** `CORDIAL_PRESENT_MODE=fifo` gives 110.2%,
with the frame rate correctly following the output down to 50 Hz -- so
presentation *is* pacing the outer loop, and the core is burned between frames
rather than by unpaced drawing.

---

## Re-measured 2026-09-01, on 0.13.0

Two things above are out of date. Both were checked before writing this, and
the second is the one that matters for what to do next.

### The bimodal idle burn did not reproduce, 0 runs in 9

Nine sequential clients, each given 22 s to reach a steady state, then measured
over a fixed 8 s window. Same host. Every run settled:

```
run 1  4.4%    run 4  5.6%    run 7  5.0%
run 2  5.0%    run 5  4.5%    run 8  4.9%
run 3  3.9%    run 6  4.5%    run 9  5.1%
```

Mean 4.7% of one core, against the 6.3% recorded above for the good runs and
mocktail's 8.7%. **No run burned a core.** If the rate were still the 3-in-9
this document reports, the chance of seeing none in nine runs is about 2.6%, so
this is evidence the fault is gone or much rarer at 0.13.0 rather than proof it
never happens.

**Not a like-for-like refutation, and the differences are worth stating.**
These runs used a throwaway profile that was not signed in, a 1280x721 window
rather than 3440x1359, and `cordial-run` alone rather than the whole process
set. Peak startup CPU came out at **186% at t=1.1 s** here against the 342%
above, which is a large enough gap that the conditions plainly differ. Somebody
should repeat this signed in and full-size before the row at the top is
rewritten; what is established is that the everyday case no longer shows it.

### mocktail's ALooper is no longer a stub, so the reason not to look at it has gone

The section above declines to copy mocktail's looper because "`ALooper_pollOnce`
returns a constant and its `ALooper_addFd` registers nothing... a looper which
answers without listening cannot deliver an input event, a wake or a surface
change". That was true when it was written and is not true now. mocktail's
current `android_stubs` carries a real `epoll` + `eventfd` implementation:
`ALooper_addFd` performs `epoll_ctl`, registrations are kept by generation
token so a removed-and-re-added descriptor cannot surface a stale event,
callbacks are dispatched and a zero return removes the fd, and `ALooper_wake`
writes an eventfd.

That matters because the **111% of a core at 60 fps while animating** measured
above is still unexplained and is not the same condition as the idle figures
here -- the nine runs are idle, not animating. The looper comparison is
therefore the one piece of mocktail worth reading for performance, and the
recorded objection to it no longer applies.

### What the profiling could not answer

Attributing the startup burst to a library failed three times. `perf` is not
installed and `perf_event_paranoid` is 2; `eu-stack` sampling put 87.5% of leaf
frames in `libc` at `__syscall_cancel_arch`, which is precisely the zero-size
symbol AGENTS.md documents the unwinder giving up on. Counting every frame of
every thread, which the first attempt did, measures stack depth rather than CPU
and should not be repeated.

So **there is currently no measured bottleneck to port a fix for**, and the
startup case cannot be attributed without either installing `perf` or adding a
counter to the JNI dispatch path.

### Profiled properly, 2026-09-01: nothing of mocktail's is worth porting

`perf` is installed now, so the questions above are answerable. Two notes on
using it here: this is a hybrid part, and perf defaults to the `cpu_atom` PMU
alone — a busy loop returned **one sample** until the event was named. Use
`-e cpu-clock` for attribution, or `-e cpu_core/cycles/u --call-graph lbr` when
callers matter, because dwarf does not unwind through libc on this host.

**Where startup CPU goes**, `cpu-clock`, dwarf, 15 s run:

```
50.95%  libroblox.so     the engine; not ours to optimise
22.86%  libc.so.6
16.49%  cordial-run      ours
 4.27%  libvulkan_intel.so

 6.51%  looper_poll_once
 5.22%  cordial_update::engine::version_of      } 8.82%
 3.60%  cordial_update::engine::version_in_run  }
```

The version scan was the largest named cost in Cordial's own code and is fixed
separately. What remains is the poll loop.

**The lock traffic is the engine's own.** `pthread_mutex_lock` and its unlock
are 4.7% together, which looked like a candidate for mocktail's lock-free JNI
object resolution. LBR call stacks put **177 of 179 caller frames inside
`libroblox.so`**. It is the engine locking against itself, not jnivm, so there
is nothing on our side of that boundary to make lock-free.

**mocktail's looper has no idle backoff.** Its `ALooper_pollOnce` passes the
caller's timeout straight to `epoll_wait`, so a zero-timeout poll returns
immediately and the loop free-runs — which is what Cordial did before
`ZERO_TIMEOUT_COALESCE_US_DEFAULT` existed, and is consistent with mocktail
idling at 8.7% against our 6.3%. Their implementation is a competent one and
we are ahead of it on the only axis that was in question.

**And the 111% at 60 fps above no longer reproduces.** Measured today through
the client's own input entry points, on the landing page, with pointer motion
driven at about 120 Hz:

```
idle, no input        7.5% of one core
animating (input)    11.7% of one core   median_fps=60, p50 frame 16.7ms
```

Eleven point seven, not a hundred and eleven. The backoff landed after that
section was written and is the obvious explanation; its own note records 9.0%
process with the gate on against 108.7% with it off, which is the same story.
**Not strictly like-for-like** — that measurement may have been in an
experience and was at 3440x1359, where this is the landing page at 1280x721 —
so the row is corrected rather than deleted, and somebody in a real game should
confirm it.

So every candidate for porting died on measurement, and the one real bottleneck
was ours. The remaining poll-loop cost is the startup spin, which is
deliberately ungated below `BACKOFF_AFTER_PRESENTS` frames as a precaution
around the startup freeze; at roughly 0.3 s of CPU that is not worth trading
for a precaution against a bug that is still open.
