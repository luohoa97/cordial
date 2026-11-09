# ADR-065: The late post waits for the engine's first render step

**Status:** superseded by [ADR-066](ADR-066-the-late-post-waits-for-the-home-page-on-a-slow-network.md)
**Date:** 2026-10-16
**Related:** [ADR-001](ADR-001-in-process-hooking.md), [ADR-003](ADR-003-plugin-isolation.md)
**Issue:** #92

## Context

On a launch with no usable `flag_cache.dat` the engine answers Cordial's bootstrap with `onFlagsFailed`, waits for `nativePostClientSettingsLoadedInitialization3`, fetches its flags and then starts its Lua app. Cordial makes that call itself, 250 ms after handing over the surface. The signed-in startup freeze (presents fixed at 1, `Forcing finalize` logged and `~UgcExperienceController` never) happens when the call lands before the render job's first step (`stepDataModelJob: No DM yet`) and the step arrives while the first app's teardown is under way. Which side of the step the call fell on was an accident of the clock: Home arrives between 0.46 s and 1.05 s after the engine starts, the call a fixed distance after the handoff. `docs/NEXT.md` has the measurements (#92, rounds 1 to 3).

## Decision

On the cache-miss path the post is made from a thread of its own, after the usual 250 ms, and waits until the engine's log shows its first render step. Then it waits 10 ms more and posts. After 3 s it posts anyway and says so on stdout; if no log of this launch can be found within 1 s it posts anyway too. A step already in the log when the gate starts costs nothing. A valid flag cache (the engine ignores the post) is left as it was, except that the call is made from the same thread.

- **The marker is read from the engine's own log file.** FLog lines go to `appData/logs/*_Player_*.log` and do not pass through `__android_log_print`: of 395 lines of Cordial's stderr in a signed-in launch none was a render step. `game_log` already reads that file for the same reason it is not a hook, and ADR-001 and ADR-003 are not touched: Cordial reads a file the engine writes into a directory Cordial made and changes nothing in the process. The gate follows the newest `*.log` modified after the process started, from byte 0, every millisecond.
- **Either of two lines counts**, `stepDataModelJob: No DM yet` and `Bind workspace now`. They carry the same millisecond in every engine log read; the first is earlier by 40 to 100 microseconds.
- **The 10 ms margin** copies the 24 to 46 ms every healthy first-regime launch had between step and post, in less than half the time; `CORDIAL_POST_GATE_SETTLE_MS` changes it. It was not tuned against anything.
- **Never the pump thread.** A post held on the pump thread for 330 ms or more stalls the window for good (`docs/NEXT.md`, round 1).
- **On by default**; `CORDIAL_POST_GATE=0` (or `off`, `false`, `no`) turns it off, parsed by the loader's `switch_on`. `CORDIAL_POST_ASYNC`, `CORDIAL_POST_AFTER_HOME_MS` and `CORDIAL_LATE_POST_MS` are kept: they are how the window is opened for a test.

## What the measurements showed

16 signed-in launches on `CordialTest`, Roblox 2.742.0.1059, nested headless sway. With the gate off and the post forced before the step: 3 launches of 9 froze (one slow start, two under a two-core `stress-ng` load, where the same timing was healthy twice without load). With the gate on at the same timing knobs: 0 of 7, the post 10 to 23 ms after the step every time, 0 of 3 under the load that froze 2 of 2. That is not a significant count (Fisher p = 0.15 one-sided, 3 of 9 against 0 of 7); the claim rests on the order being guaranteed by construction and observed in all seven gate-on launches. Post-before-step is necessary in every frozen launch seen and is not sufficient: 4 of 5 unloaded launches with it were healthy.

## What this does not claim

- That the freeze is gone. Something besides the order decides whether a launch in the window freezes, and it is not in the engine log; load makes it likelier. If a launch freezes with `post gate: ... posting` in its stdout, the order was safe and the cause is elsewhere, which is worth knowing.
- Anything about a signed-in launch with a valid flag cache. Five signed-out ones were healthy.
- That the gate costs nothing on a slow start: it holds the post until the step, which on a signed-out cold profile was 907 and 932 ms.

## Alternatives

**Hook the engine's log from liblog.** Nothing passes through it. **Post from the Home notification.** Home precedes the step by 26 to 30 ms, which is the window; it puts the post on the wrong side. **A fixed longer delay.** Tried once in 2026-09 and changed the failure rather than removing it; Home's time moves by a factor of two between launches.
