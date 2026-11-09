# ADR-066: The late post waits for the home page on a slow network

**Status:** accepted
**Date:** 2026-11-08
**Supersedes:** [ADR-065](ADR-065-the-late-post-waits-for-the-first-render-step.md)
**Issue:** #92

## Context

ADR-065 holds `nativePostClientSettingsLoadedInitialization3` until the engine's
first render step. It fixed the cache-miss freeze in the ordinary case, and its
own "what this does not claim" section says a launch that freezes after
`post gate: ... posting` has a cause elsewhere.

A slow network is that cause. A local HTTP proxy that holds every connection for
12 s, used from a cold flag cache, reproduces a permanent stall at one to two
presents. The client reaches `RootSwitchNavigator`, the gate posts, and then it
stops drawing for the rest of the run. The arms, one launch each, 3840x2159,
nested headless KWin:

| arm | presents at 2 / 12 / 60 s | note |
|---|---|---|
| ADR-065 gate, async | 1 / 1 / 2 | |
| gate off, sync 250 ms | 0 / 0 / 1 | |
| async, Home + 80 ms | 0 / 0 / 1 | |
| no post at all | 2 / 150 / 2935 | healthy, but no `ClientRunInfo` / `RbxStorage::init` block |
| post at Home + 20 s | 1 / 148 / 3036 | 5 s pause at 447 frames, then recovered |
| post at `HOME_PAGE_INTERACTIVE` | 1 / 149 / 4496 | post at 24.4 s, then healthy |

The no-post arm proves the UI can reach `HOME_PAGE_INTERACTIVE` without the call,
but the engine log has no `ClientRunInfo` block in it: skipping the post is not a
fix, it is a different client. The later posts recover; the early ones do not.

On an ordinary network the same gate costs almost nothing: warm and cold-cache
launches saw `HOME_PAGE_INTERACTIVE` at 616 and 675 ms and presented 1355 and
1319 frames in the first 30 s.

## Decision

When the engine reported `onFlagsFailed` and a saved identity was restored at
startup:

- wait up to 2 s for `APP_READY Home`;
- if Home arrives, wait for `HOME_PAGE_INTERACTIVE`, up to 30 s
  (`CORDIAL_POST_INTERACTIVE_MS` overrides the cap), then post;
- if Home does not arrive, use ADR-065's first-render-step gate unchanged.

Signed-out launches and a restored session that still routes to `Landing` never
announce Home, so they keep ADR-065 exactly as it was. The interactive counter is
incremented by the native data-model notification callback Cordial already
handles; no engine memory is touched.

The choice is a trade: on a slow network the post's own block still pauses
rendering for about 5 s before the client recovers. That is visible, but it is a
pause rather than the permanent one-to-two-frame stall.

## What this does not claim

- That every slow network stalls. The proxy is one shape — every connection
  delayed 12 s — and the 30 s cap is a bound, not a measurement of a worse one.
- Anything about a signed-out launch under the same proxy; the step gate is
  unchanged there and was not re-measured.
- That the 5 s pause is avoidable. The post at Home + 20 s paused for the same
  5 s; only skipping the call avoided it, and that loses the block.
- Anything about the swapchain freeze ADR-065 describes. This is the post's
  block racing startup HTTP, not a compositor release.

## Alternatives

**Skip the late post on a slow start.** It renders, but the engine never runs the
`ClientRunInfo`/RbxStorage block; the no-post arm's log has neither. That is a
different client, not a fixed one.

**A fixed longer delay.** Home + 20 s recovered, but the number is arbitrary and
would be wrong on a slower or faster connection; `HOME_PAGE_INTERACTIVE` is the
engine's own statement that the feed is laid out.

**Keep posting at the first render step and retry later.** The early post wedges
the render thread; the later one recovers, but nothing in the first stall
indicates that a second call would land differently, and we did not measure one.
