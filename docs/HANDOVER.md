# Handover

Written for whoever picks this up next. It is deliberately not a summary of the
README — it is the things that are true, that cost somebody time to establish,
and that are not obvious from reading the tree.

Start with [AGENTS.md](../AGENTS.md). It is short, it is the contract, and every
rule in it was bought with a wasted afternoon.

## If you forked this

Open a pull request too. Not instead — as well.

Forking is fine and the licence exists to make it fine. What costs everybody is
**divergence**, and divergence is not measured in commits. Ten small fixes
cherry-pick in an afternoon, because the commit messages here say what was
measured and are still legible a year later. One fork that restructured a
subsystem to fix something its own way is not mergeable at all, and at that
point that fork is the real project and this one is a museum.

So the ask is narrow: whatever you are doing in your fork, also put it up as a
PR, and keep the branch rebased on `main`. That turns "somebody has to
reconstruct what you did" into "somebody clicks merge".

The queue will sit for a while. The author has stepped back and intends to come
back to hand the project over rather than to resume it, so PRs opened now are
read when that happens rather than next week. That is a real cost to you and it
is stated plainly rather than dressed up — but the queue is the first thing a
new maintainer inherits, and a PR in it is worth far more than a commit in a
fork nobody knows about.

## Where this stands

Cordial loads Roblox's official Android x86-64 `libroblox.so` natively on Linux
— ported AOSP bionic linker, bionic/glibc shim, libjnivm in place of ART, and a
framework layer answering the platform calls the client makes.

You can sign in, stay signed in across restarts, load a game, move around, turn
the camera and hear sound. That is further than it sounds: for most of this
project's life the client rendered a login form nobody could type into.

**The single most valuable habit here is refusing to state a result you did not
observe.** Several commits exist only to retract an earlier claim, and they are
the good ones. `docs/NEXT.md` is the long-form working record; it is 1700 lines
and it is honest, including about the things that turned out to be wrong.

## What needs an account, and therefore needs you

A large fraction of what is still unverified is unverified for one reason: it
requires a signed-in client inside a running experience. No contributor working
without an account can settle any of it, and no automated agent should try.

- **Does a granted pointer lock actually turn the camera?** The lock is
  requested, the compositor's refusal path is honest, and the release paths
  work. That relative motion reaches the camera is `INFERRED`. Granting requires
  the pointer over the canvas, which requires a real mouse in a real session.
- **Does game audio reach OpenSL ES at all?** Sound demonstrably comes out of
  the bridge, measured off the sink monitor with a zeroed-buffer control. But at
  the Landing screen the engine asks for no audio whatsoever, and
  `--dump-classes` shows Roblox naming `org.fmod.AudioDevice` — FMOD's *Java*
  `AudioTrack` output path. If FMOD picks that inside an experience, the whole
  OpenSL bridge is the wrong door. Unresolved.
- ~~**The 1 fps report.**~~ **Withdrawn, and worth reading as a cautionary
  tale.** A report of 1 fps in an experience looked like a perfect match for the
  documented idle throttle, which drops presents to exactly 1.0/s when nothing
  is happening. A whole theory followed: input not reaching the engine in the
  way its idle heuristic counts, with pointer capture as the likely fix. It was
  the developer's own machine under memory pressure from unrelated applications,
  and the same session on a quiet machine was "buttery smooth" on an Intel iGPU.
  Nothing in Cordial was at fault. Two coincidences did the damage — the number
  matched a real documented behaviour exactly, and the throttle was fresh in
  everyone's mind. **Ask what else was running before believing a performance
  report, including your own.**

**Do not test with an account anyone cares about**, and keep test accounts on a
separate IP. Enforcement is automated, runs in waves, and associates accounts
sharing an address. The risk is collateral rather than causal.

## Per-profile network egress (ADR-016), and what is still missing

A profile's `network.json` can now say `"mode": "vpn-required"`, which refuses
to start the client at all unless the profile's own `check` command exits zero and reports traffic actually
passing — checked at both the shell's `launch.rs` and `cordial-run`'s own
`main`, so starting the client directly cannot skip it. Read ADR-016 before
touching any of this; the short version is below, with what to trust and what
not to.

**What this actually guarantees, and does not.** A `vpn-required` profile will
never make even Cordial's own client-settings request on the machine's
ordinary route while believing itself protected. It does **not** isolate two
profiles running at once from each other — a machine-wide tunnel is one, global,
machine-wide route, and ADR-012's own two-windows-at-once case means a second
profile running alongside a `vpn-required` one shares whatever route is
active, VPN or not. Read the mechanism's name literally: a launch gate, not a
sandbox.

**Why it stops at a gate rather than a namespace, and this is the load-bearing
fact for whoever picks this up next.** the VPN wrapper this originally brokered drove a client
which brings its tunnel up as a NetworkManager connection — confirmed by
reading that client, not assumed. NetworkManager is a
system service in the host's own network namespace, so the interface it
creates lands there regardless of which namespace the command that asked for
it was run inside. bringing such a tunnel up under `ip netns exec cordial-<profile>` would not produce
a namespace-scoped tunnel; it would produce the same machine-wide one such a
client always produces. A real per-profile tunnel needs a client (or something
alongside it) to hand over the WireGuard parameters an established connection
negotiated, so a second, namespace-local interface can be brought up directly
with `wg-quick`, bypassing NetworkManager entirely. Nothing in the client
this was written against exposed
that today. **This is the concrete next step**, not "make the namespace work"
in the abstract — the namespace mechanics themselves (veth, routing, `ip netns
exec`) are ordinary and not the hard part; extracting a usable tunnel
definition out of a NetworkManager-managed Proton connection is.

**`unshare --net -- ip link` fails with `Operation not permitted` in an
ordinary unprivileged shell** — measured directly, on the machine this was
written on, not assumed from `CAP_NET_ADMIN` documentation. Building the
namespace path also means deciding how Cordial's packaging (Flatpak in
particular) gets that capability at all, which ADR-007's existing argument
against broad sandbox permissions bears on directly.

**`INFERRED`, and deliberately not leaned on:** whether Roblox's own curl
usage inside the engine honours `http_proxy`/`HTTPS_PROXY` is unresolved —
the structural path for it to work is real (same process, same `environ`,
`bionic/mod.rs` does not shim `getenv`), but whether Roblox's code explicitly
disables that via `CURLOPT_PROXY` was not observed and cannot be without
tracing a signed-in client. It does not matter for anything built here: the
decision not to ship an `http_proxy`-shaped setting rests on `RtcIoRna` (the
real-time game transport) not being HTTP at all, which holds regardless.

**A measurement trap this work tripped over and is worth adding to the list
above:** `CORDIAL_PROFILE_ROOT` was being guarded by three *different*,
mutually unaware mutexes — one each in `profile.rs`, `profile_switcher.rs`,
and (new, in this change) `launch.rs`. Each looked correct in isolation and
none of them stopped a different file's tests setting the same process-wide
variable at the same moment. It surfaced as one failure in several runs of
`cargo test -p cordial-shell`, reading another test's scratch directory back
mid-assertion — exactly the "passed anyway on the first run" shape
`profile.rs`'s own tests already warn about. Fixed by sharing one mutex
(`crate::PROFILE_ROOT_ENV`, declared in `main.rs`) across every file in that
binary that touches the variable. If a future file needs to point
`CORDIAL_PROFILE_ROOT` at a scratch directory in a test, use that one rather
than adding a fourth private mutex that looks like it works.

## A local Sober issue corpus for triage (ADR-017)

`tools/sober-corpus/` is a Deno tool, not Rust, and it does not touch the
engine — worth saying plainly because everything else in this file does.
It pulls vinegarhq/sober's issue tracker (another Roblox-on-Linux project,
closed source, so its GitHub repo is purely an issue tracker) into a local,
gitignored, PII-redacted corpus. Sober's tracker is prior art: real users
hitting real problems on the same engine Cordial loads, with a
maintainer's actual answer attached. `just sober-corpus-fetch` pulls it;
`just sober-corpus-derive` filters it down to the cases with a substantive
maintainer reply. Read `tools/sober-corpus/README.md` for day-to-day use
and ADR-017 for the reasoning.

**Run and verified, this session, not assumed:** a cold run against the
live tracker completed in 91.9s, 22 pages, ~44 GraphQL points of a
5,000/hour budget, and produced 2,195 issues (2,194 at the time the
original version of this tool was measured elsewhere; the tracker gained
one issue in the interim, which is itself a small proof the fetch is
talking to the real, current API). SIGKILLing it mid-run (confirmed dead:
exit 137) and re-running showed "Resuming an interrupted pass: 3 page(s) /
300 issue(s) already done this pass" and continued from page 4 rather than
restarting — the checkpoint scheme works as designed. A warm re-run with
nothing new to fetch cost one page, 2 points, 4.1s. `grep`ing the full
corpus for `/home/` turned up zero unredacted username paths (1,786
occurrences of the redaction marker, one benign non-match that was a
literal `"me"` string in a user's own malformed `$HOME`, not a real
username). Cross-checked against `gh api repos/vinegarhq/sober/pulls
?state=all` — zero of the repo's 20 real pull request numbers appear among
the 2,195 fetched issue numbers, confirming the GraphQL `issues` connection
this fetcher uses does not leak pull requests the way REST's `/issues`
endpoint would.

**Dropped on purpose:** an LLM-scoring harness (`evaluate.ts`, `model.ts`,
`sampling.ts`, a Cordial-context builder) existed alongside the fetcher in
the project this was ported from. It called an external LLM API to
auto-judge diagnoses against the derived set, and the founder explicitly
cancelled it ("No openrouter"). It is not in this tree and nothing here
depends on it — what ships is the fetcher and the maintainer-reply quality
filter that makes the raw corpus worth searching by hand.

## Open threads, with what is actually known

**Fullscreen offsets the content** — right and down on entering, then bleeding
up and left past the window edge on restore. Issue #7.

**The lead previously recorded here is disproved.** It said the swapchain extent
comes from `wayland::current().geometry()`, that geometry is written only by
`apply_resize`, and that `apply_resize` early-returns when the size is unchanged
— so the thing to measure was whether it fires with the fullscreen size at all.
It does. Measured 2026-08-06 with `CORDIAL_INSTR=1` and temporary logging added
around the early return:

```text
apply_resize(accept) 1370x765  -> 3440x1359      entering fullscreen
surface_caps currentExtent <- geometry() 3440x1359
apply_resize(accept) 3440x1359 -> 1370x765       restoring
surface_caps currentExtent <- geometry() 1370x765   (repeatedly, after)
```

666 `apply_resize` entries over the run, 650 of them early returns and 16 real
changes — but **both** fullscreen transitions are in the 16. The geometry
updates, and the extent handed to `vkGetPhysicalDeviceSurfaceCapabilitiesKHR`
tracks it in both directions. Neither the early return nor a stale extent
explains the offset, and the next person should not spend the afternoon there.

**What is now suspicious instead, and is not yet explained.** The fullscreen
surface was **3440x1359**. `org.gnome.Mutter.DisplayConfig` on the same machine
reports a 1920x1200 mode at scale 1.0, so that surface is both wider than the
display and an odd number of pixels tall. Every height in the run is odd — 721,
765, 1359 — while every width is even. That pattern wants explaining before any
fix is attempted; it is the kind of detail that turns out to be the whole bug or
a complete red herring, and this investigation has produced three of the latter
in one evening.

The measurement to take next is what the *compositor* configured versus what
Cordial applied: log the `xdg_toplevel::configure` width and height alongside
the `apply_resize` call they produce. If those disagree, the fix is in the
configure handling. If they agree, the offset is downstream of the surface size
entirely — canvas placement or the engine's own viewport — and neither of those
is where anyone has been looking.

**Textures and meshes render wrong, and the font is broken until it isn't.**
Unexplained. The leading suspect is Cordial's own MAILBOX-for-FIFO present-mode
substitution, which is what took the frame rate from a variable 35–50 to a flat
60 — if the engine's upload path relied on FIFO's pacing, MAILBOX would let it
sample buffers before their uploads land. `CORDIAL_PRESENT_MODE=fifo` is the
control and it has not been run. Ruled out already: ASTC support (this
developer's Intel iGPU has it) and a fall back to the software rasteriser
(`intel_icd` and `libvulkan_intel.so` are both installed).

**Typing into text fields draws nothing** until the field loses focus. The
per-keystroke sync path is understood; what is missing is an EditText-equivalent
overlay. `CORDIAL_TRACE_TEXT=1`.

**Shift+F5 does not open Roblox's stats menu.** Two candidates, neither
established: the key path sends an evdev keycode with Android `META_*` modifier
bits, which is a mixed vocabulary and mixed vocabulary is what cost four failed
keyboard theories before — or these are desktop-only debug shortcuts that the
Android build never wires at all. Settling it is cheap.

**Web views are unimplemented**, which is why a lot of Roblox's UI does nothing.
Needs `webkitgtk6.0-devel`; it is absent on the developer's host and present in
their distrobox.

**There is no Roblox Android build to download.** ADR-015 permits fetching and
the entire fetcher is built and proven — streaming, SHA-256, zip refusals,
metered detection. It has no URL because Roblox publishes no Android artefact:
`setup.rbxcdn.com/android/DeployHistory.txt` is 403, `client-version/AndroidApp`
is 500, and `roblox.com/download` offers Google Play and the Amazon Appstore and
no file. Sober does not fetch from Roblox either; it routes users through Google
Play. Aptoide is deliberately not wired — a mirror offering only a hash it
supplied itself is verification theatre.

## The application ID, and why it is not org.cordial.Cordial

**It was, until 2026-08-05, and that was a claim on a domain this project does
not own.** `cordial.org` was registered on 1999-04-16, is held through InterNetX
GmbH with `clientTransferProhibited` set, resolves to 185.148.170.48 behind
iwelt.de nameservers and does not expire until 2027-04-16. It is a
twenty-seven-year-old domain in active use. **"Just register cordial.org" is not
on the table**, and anyone who assumes it is will spend an afternoon finding that
out — which is the only reason this section still exists now that the rename is
done.

It mattered because **Flathub requires an application ID over a domain or forge
account you demonstrably control**, and rejects a submission claiming one it does
not. It is not a formality: the ID is what a user's machine trusts for the
lifetime of the install.

The ID is now **`io.github.luohoa97.Cordial`**, which needs no domain, matches
the homepage, and is what `<developer id="io.github.luohoa97">` in the metainfo
had been saying all along.

**It was done on the same day the remote first went live, on purpose.** A rename
after a package has users is a support burden; before, it is a `git mv` and a
sed. If you are reading this because you want to move to a real domain later,
that is fine — buy it first, and expect the cost below rather than the cost
above.

**What a rename touches**, recorded because the next one will need it:

- `app-id` in the manifest, and `APP_ID` in `.github/workflows/flatpak.yml`
- the manifest, desktop, metainfo and **both** icon filenames — all of which must
  match the ID, and one of which is the README banner rather than the app icon
- `<id>` and `<launchable>` in the metainfo
- `const APP_ID` in `crates/cordial-shell/src/main.rs`, which is also what makes
  Cordial single-instance
- two `include_str!` paths that pin the desktop file's contents from tests
- the `Icon=` URL in `packaging/cordial.flatpakrepo`
- the README's install, uninstall and plugin-directory paths
- **user data**, which is the one that bites. A Flatpak's data lives at
  `~/.var/app/<app-id>/`, so a rename orphans every profile, sign-in and
  extracted Roblox build behind it. Nobody had any at the time of this one. Next
  time somebody will, and they are owed either a migration or a release note
  saying plainly what is being left behind — and since [ADR-012](adr/ADR-012-profiles-and-instances.md)
  holds profiles under an `flock`, a migration running while an instance is up is
  its own problem.

**Do not do a blanket `sed` over the tree and call it done.** This rename did,
and it rewrote prose in this very file that was *about* the old identifier,
turning a true sentence into a false one. Grep the diff for the new string in
running text, not just in code.

## Flathub, and why it is not the plan

**Flathub's generative-AI policy does not allow applications containing
AI-generated or AI-assisted code, documentation, or any other content**, and it
extends to the submission itself: the pull request, manifest, metadata, patches,
build scripts and every review comment on it must not be LLM-generated either.
Submissions that violate it can be rejected without further review, and repeat
violations can earn a permanent ban.

**Cordial is squarely inside that.** Large parts of the tree, this document
included, were written with an LLM, and the git history says so in
`Co-Authored-By` trailers on dozens of commits. That was recorded deliberately
and it should stay recorded.

So the honest position is: **the GitHub Pages remote is Cordial's distribution
channel**, not a waiting room. Anything in the tree that reads as "until we get
on Flathub" is wrong and should be corrected where you find it.

The policy does say **exceptions may be granted for mature, well-maintained
projects.** That is the only route, and it is a route that starts with saying
plainly what is in the tree, in a pull request a human wrote. It is not a route
that starts with rewriting history to remove the trailers — that is deceiving the
reviewers the policy exists to serve, and it would cost this project the one
property that makes its claims worth anything.

**If somebody does pursue an exception**, three technical things still stand in
the way and are worth fixing regardless, because they are what a self-hosted
remote wants anyway:

1. ~~**The build needs the network**~~ **Done**
   ([issue #3](https://github.com/luohoa97/cordial/issues/3)). Both halves are
   pinned now: `libjnivm` and `mcpelauncher-linker` as `git` sources by commit,
   and all 212 crates as `archive` sources carrying the sha256 that was already
   in `Cargo.lock`, generated into `packaging/cargo-sources.json` by
   `packaging/cargo-sources.py`. `--share=network` is gone from
   `build-options.build-args`, and the compile now runs with the network
   unshared — measured with a control, as a manifest pair differing only in
   `build-args`: without it a build-command gets `getaddrinfo` failure, a
   refused TCP connect and no routes; with it, all three succeed. Note that
   probing the *build directory* with `flatpak build` answers the wrong
   question, because that reads the finished `metadata` and the finish-args
   include `--share=network` for the game itself; that mistake was made here
   before the manifest pair replaced it.
   The generator is stdlib-only rather than `flatpak-builder-tools`'
   `flatpak-cargo-generator.py`, because every dependency here is a registry
   crate and the checksum for one is the lock file's own; a git dependency
   would make it exit rather than emit a list quietly missing that crate.
   A CI step regenerates the list and fails on a diff, which is the guard
   against the one way this arrangement breaks.
2. ~~**The application ID.**~~ **Done** — `io.github.luohoa97.Cordial`, see the
   section above.
3. **No screenshots.** The metainfo has none, and Flathub's linter requires at
   least one. Cheap, and worth having for the software-centre listing on the
   existing remote whatever happens with Flathub.

**What would not have stopped you, for the record:** being a third-party Roblox
client is not itself disqualifying — Sober ships on Flathub as
`org.vinegarhq.Sober` — and Cordial's case is the easier one, since it ships no
Roblox code, asset or APK at all and the user supplies the client where Sober
fetches it. That was never the obstacle. The AI policy is.

**Checked again on 2026-08-27, against the primary sources rather than repeated
from memory**, because a claim this load-bearing for a maintainer's week is
worth re-verifying rather than trusting the paragraph above on faith:

- `docs.flathub.org/docs/for-app-authors/requirements`, "Generative AI policy"
  section, quoted directly: *"Applications containing AI-generated or
  AI-assisted code, documentation, or any other content are not allowed"*, with
  *"exceptions may be granted for mature, well-maintained projects"* and no
  published criteria or application process for that exception beyond asking —
  case by case, in a GitHub issue on `flathub/flathub` or the Flathub Matrix
  room, per public reporting on the policy (effective 2026-05-29). Confirms the
  paragraph above is still accurate a season later, not that it always will be.
- **Sober's own published manifest**, `flathub/org.vinegarhq.Sober`, fetched and
  read directly: `finish-args` grants `--share=network` with the comment "Both
  Sober & Roblox require network access to work. Non-optional", and the
  manifest contains no `extra-data` source for the Roblox client at all. So
  Sober does not smuggle Roblox in at build time through the one Flathub
  mechanism built for non-redistributable content (`extra-data`) — it fetches
  the proprietary APK itself, over the network, after install, at runtime,
  exactly the shape `crates/cordial-update` uses. This is the strongest
  available evidence that the runtime-download shape is not what Flathub's
  build-from-source rule is policing, because a project doing precisely that
  has been live on the store throughout. It does not settle every question a
  reviewer might raise — a submission is still a human decision — but "we
  cannot know until we ask" is no longer the honest position on this one point.

## Why Roblox reports a client-integrity problem, so far

Open, and the most-asked question about this project. What follows is what has
been **established**, so nobody spends the afternoon twice.

**Play Integrity is not reachable from Cordial, and is not what differs from
Sober.** `docs/traces/` shows the real Android client calling Google Play
Services during startup — `requestIntegrityToken(IntegrityTokenRequest{nonce=…,
cloudProjectNumber=676451317595})` for `com.roblox.client`, binding to
`com.android.vending/…finsky.integrityservice.IntegrityService`, and Finsky
answering `Integrity key attestation record generated successfully`.

That happens in **the APK's Java layer, not in `libroblox.so`**. Cordial runs the
native engine and provides the platform beneath it; it never runs the app's Java
code, so the call is not made and cannot be. Measured rather than reasoned:
`--dump-classes` reports **3249** classes the engine asked Cordial for and **not
one** is Play-Integrity shaped. Sober is in exactly the same position and shows
no integrity error, so whatever differs, it is not this.

**Do not try to produce that token.** It is a device and app attestation from
Google, and the only ways to make one appear are forging it or relaying one from
a real device. Both are circumventing an anti-tamper control rather than making
Cordial an honest client, and both are out of scope here for the same reason
in-process hooking is ([ADR-001](adr/ADR-001-in-process-hooking.md)).

**What was actually different, and is now fixed.** Sober's own log on this
machine, and the capture from real Android, both contain:

```text
rbx.JNIRobloxSettings: Setting default app policy file:
  content/guac/defaultConfigs/GuacDefaultPolicy-GlobalDist.json
```

Cordial never called `nativeSetDefaultAppPolicyFile`, so the engine came up with
no app policy at all. That was issue #5. It is wired now, relative rather than
absolute because both the capture and Sober log it that way, and the engine
emits the identical line and still reaches `APP_READY Landing`.

**Whether that fixes the integrity report is not established.** It needs a
signed-in join to see and that run has not happened. A gap being real is not
evidence it is the gap that broke anything.

### What 304 actually looks like, with a Sober control beside it

Captured 2026-08-06 on the `CordialTest` profile, joining place 17625359962,
against a Sober run from 2026-08-05 that played for seventeen minutes. Both
lines are from the engine's own log, times are seconds since process start.

```text
CORDIAL  248.658  Transport selection: useRbxTransportEnabled=false,
                  selectedTransport=RakNet, rccPort=52398
         248.973  Connection accepted from 128.116.51.33|52398
         253.672  Error RbxTransportDummyClient: Failed to establish
                  connection to 128.116.51.33:41297, reason NoResponse
         309.161  Disconnect reason received: 304

SOBER    258.157  Transport selection: useRbxTransportEnabled=false,
                  selectedTransport=RakNet, rccPort=63935
         258.203  Connection accepted from 128.116.51.33|63935
         260.174  Error RbxTransportDummyClient: Failed to establish
                  connection to 128.116.51.33:43197, reason NoResponse
        1053.358  DisconnectClientInitiated — the player left
```

**`RbxTransportDummyClient … NoResponse` was written up here as benign. That
was wrong, and it is the opposite of benign — it is the strongest lead there
is.** The retraction is left in place rather than tidied away, because the
reasoning that produced it is a trap worth seeing.

The first control used a Sober log in which the *first* join also logged
`NoResponse`, which looked like proof the failure did not matter. Read further
and that join was abandoned 1.2 seconds later for a different place, and the
join that actually stuck had the DummyClient **connected**. One log, one glance,
one wrong conclusion — from a control that was real but not read to the end.

Re-run 2026-08-06 with both clients pointed at the same place by deep link,
same machine, same network, same server IP:

```text
SOBER    54.616  DummyClient will connect to 128.116.51.33:57479
         54.654  Connected to server at 128.116.51.33:57479      (38 ms)
         54.654  Started ping thread (interval 1000 ms, 64 bytes)
         54.654  Started time sync thread (interval 5000 ms)
        306.058  Disconnection Notification. Reason: 285 — the player left
                 no 304, four minutes in the place

CORDIAL   4.121  DummyClient will connect to 128.116.51.33:41297
          4.156  Connection accepted from 128.116.51.33|52398   (RakNet, fine)
          9.122  Error: Failed to establish connection, reason NoResponse
                 — a five second timeout, no connection, no threads
         64.292  Disconnect reason received: 304
```

**Sober connects that socket in 38 milliseconds. Cordial times out after five
seconds and is disconnected 60.1 seconds after its RakNet accept.** RakNet
itself — also UDP — connects fine in both, so this is not "UDP does not work".
Something specific to the `RtcIoRna` path fails only under Cordial.

The ping thread and the time sync thread are the shape to keep in mind: a
liveness channel that never opens, and a kick almost exactly sixty seconds
later. That is a hypothesis and not yet a mechanism — nobody has established
*why* the connect times out.

**What the control does establish.** The two clients are indistinguishable
through connection: same transport selection, same RakNet accept, same
harmless probe failure. The join *succeeds*. Cordial is then disconnected
**60.5 seconds after the connection was accepted** — `connectionTime 248689`
against `timeMS 309172` — while Sober is never disconnected at all.

So 304 is not a handshake or an integrity check at load. **It is something
periodic that Cordial fails roughly a minute into replication**, at which point
the server sends a disconnect whose text talks about missing or corrupted files.
The message is worth reading as a category rather than a diagnosis: nothing in
Cordial's install is missing, and the same binary reaches the same server the
same way that Sober does.

**One behavioural difference worth the next look, and no more than that.** On
Sober's second join the DummyClient *did* connect and started two threads:

```text
RbxTransportDummyClient  Connected to server at 128.116.51.33:54811
RbxTransportDummyClient  Started ping thread (interval: 1000 ms, payload: 64 bytes)
RbxTransportDummyClient  Started time sync thread (interval: 5000 ms, …)
```

Cordial has never reached that on any join observed so far. Whether it matters,
or is simply which server answered on the day, is **unestablished** — it is
recorded because it is the only place the two logs diverge, not because it is
believed to be the cause. A ping thread and a time-sync thread failing to exist
is at least the right shape for something that kills a session sixty seconds in,
and that is exactly the kind of resemblance that has been wrong twice already.

### The transport probe fails on some servers and not others

**A previous version of this section said the transport socket is "written and
never read" and blamed `RtcIoRna`'s event loop. That was wrong, and the very
capture proposed to test it is what disproved it.** The retraction stays visible:
this is the third lead in this investigation to dissolve, and all three dissolved
the same way — a real observation from one run, generalised into a property of
Cordial.

What was observed, across three joins on 2026-08-06:

```text
18:01   server 128.116.51.33   DummyClient NoResponse    304 at 60.1 s
18:18   server 128.116.51.33   DummyClient NoResponse    304 at 64.3 s
18:47   server 128.116.63.33   DummyClient CONNECTED     no 304, ran 96 s
                               ping thread + time sync thread started
```

And with `strace -f -e trace=desc,network` over the third:

```text
fd 140 (DummyClient)   epoll_ctl registrations: 1     recv: yes
```

**The socket is epoll-registered and the event loop works.** The failing runs are
not Cordial failing to poll; two of three joins simply got no answer from one
particular server while a third answered in 432 ms.

**What is established.** Cordial can connect that socket — nothing structural
prevents it. Sober's own logs contain a `NoResponse` too, on a join it abandoned
1.2 seconds later for a different server.

#### Retracted 2026-08-06: 304 does not correlate with the server address

**"The failures correlate with the server address, not with the build" is wrong,
and it was wrong because the sample was three joins that happened to land on two
edges.** This is the fourth lead in this investigation to dissolve. Sweeping
every surviving `*_Player_*.log` across all data roots for a `Connection accepted
from` line and the `Disconnect reason received` that followed it gives:

```text
server           connection lifetime   outcome
128.116.44.33          60.7 s          304
128.116.56.33          60.6 s          304
128.116.51.33          60.2 s          304
128.116.51.33          15.6 s          --     (session ended first)
```

**Three different servers, three 304s, all between 60.2 and 60.7 seconds after
`Connection accepted`.** `128.116.51.33` appears on both sides of the old
correlation, which is what breaks it: the run that escaped was not on a kinder
server, it was a session that ended at 15.6 s and never reached the deadline.
The variable is elapsed time on a connection, not which edge answered.

The reproduction that produced the newest row was not scripted and had nothing
driving it — a plain `cordial-shell` with `CORDIAL_INSTR=1` and no
`CORDIAL_SCRIPT`, with the join made by hand. The engine's own words are
`Disconnect reason received: 304` and `connectMode: Peer Disconnected`, with
`AckTimeout 0, IsOutgoingDataWaiting 0`: **the server sent it, and RakNet's own
health was fine at the moment it arrived.** Nothing was stalled or backed up.

**The `128.116.63.33` row above — "no 304, ran 96 s" — cannot be re-checked.**
No surviving log in any data root contains that address. It is the one recorded
counterexample to a 60-second rule, so it matters, and it should be treated as
unverified rather than quietly dropped: if it is accurate the deadline is not
universal, and if it was mis-transcribed the rule is clean.

#### 2026-08-06, later: 304 reproduces on demand, and four more leads are dead

**The join no longer needs a person.** `cordial-run --join-url
'roblox://experiences/start?placeId=<id>'` joins directly, with the same
`--profile` the shell passes, so the session and cookie store are the real ones.
The shell's own deep-link path holds a link "until you press Roblox" and cannot
be driven headlessly; the runtime's flag can. Two things fall out of that:

- **`roblox-player://` does not work as a `--join-url`.** The runtime says so
  itself — `FStringGameLaunchLinkURL` admits `roblox://` and `robloxmobile://`
  only. Use the translated shape, which is what `deep_link` produces anyway.
- **A join succeeds without the `gameinfo` ticket**, which `deeplink.rs` records
  as "not established". It is established now: six joins, no ticket, all reached
  the place and replicated.

**304 is now 100% reproducible in about 70 seconds.** Six joins on 2026-08-06,
every one of them:

```text
run          connect    alive     reason
baseline3      1.9 s    60.1 s     304
jnitrace       2.1 s    60.1 s     304
strace         1.9 s    60.1 s     304
nodummy        2.0 s    60.1 s     304
(+ the two hand-driven joins earlier: 60.2 s and 89.9→150.2 s)
```

That is a hard 60-second deadline from `Connection accepted`, never anything
else. **Reproducing it is no longer the hard part**, which is what every earlier
entry here was blocked on.

**Ruled out, each with a control rather than an argument:**

| lead | how it died |
|---|---|
| the server address | Sober connected to `128.116.51.33` — the exact server Cordial dies on — and ran **256 s clean** |
| the websocket to `10.110.101.222:5052` | **Sober hits the identical timeout**, same 59998 ms, same URL, and survives. In one Cordial run it fired 89 s *before* the 304 |
| a missing JNI answer during play | JNI-trace build: `Constructed Unresolved symbol` appears only at `JNI_OnLoad` and at shutdown. **Nothing unresolved during the session** |
| `NetworkUtils.getPublicIPv4Addresseses` | unresolved, yes — and **never invoked**, 0 calls in 17,749 traced lines. Implementing it would change nothing |
| client settings not reaching the engine | `nativeInitClientSettings -> 0`, and **0 is documented success** in `client_settings.rs`. The 1.27 MB document is fetched, cached and delivered |

**What the transport actually does**, from `strace -f -e trace=network` on a
failing run — the capture this file has been asking for since the lead was
opened:

```text
connect(146, 128.116.51.33:37074)                       = 0
setsockopt(146, SO_RCVBUF/SO_SNDBUF, 524288)            = 0
getsockname(146)                        -> 192.168.1.112:49485
sendmsg(146, 1231 bytes) x10                            = 1231 each
(no reply ever arrives; no recvmsg on 146 at all)
```

**Cordial sends.** Ten 1231-byte datagrams leave the socket successfully and the
server answers none of them. Sober's equivalent connects in **28 ms** to the same
address. So "the socket is epoll-registered and the event loop works" was right
and remains right — the packets go out and nothing comes back.

**RbxTransport is QUIC**, which the flag names give away
(`DFFlagLogRbxTransportEphemeralEarlyPubKey`,
`DFFlagRbxTransportDummyClientReportPubKeyOnQuicError`,
`DFFlagRbxTransportFixNoQuicFrameCrash`), and 1231 bytes is a QUIC Initial
padded past the 1200-byte floor.

**The DummyClient is a telemetry probe, and that matters.**
`DFFlagReportDummyClientConnectionAttemptResult`,
`DFFlagNetStackDummyClientEnablePingTelemetry` and
`FStringRbxTransportDummyClientEnabledMinorVersions_PlaceFilter` together say
Roblox is shadow-testing a QUIC transport and reporting whether it would have
worked, while `selectedTransport=RakNet` carries the actual game. A probe whose
purpose is to report failure is a poor candidate for *causing* a disconnect.

**The control for that is written and it did not work — say so.** Setting
`FStringRbxTransportDummyClientEnabledMinorVersions_PlaceFilter` to `"0"` and
`DFFlagReportDummyClientConnectionAttemptResult` to `false` via `CORDIAL_FLAGS`
applied cleanly (`flags: 2 override(s) applied`) and **the DummyClient ran
anyway** and failed identically. So whether the probe causes the 304 is still
**not established**; those two flags simply are not its gate. Finding the real
gate, or making the probe succeed, is the experiment that settles it — and
unlike everything before, it can now be run in 70 seconds a time.

**Take the connection lifetime from the engine's own line, not from two greps.**
A run that teleports has several connections, and pairing the *first* `Connection
accepted` with the *last* disconnect reported 77.0 s for a connection that
actually lived 60.8 s — the 60-second rule looked broken when only the
arithmetic was. `timeMS` and `connectionTime` sit on the one `Connection lost`
line and both belong to the same connection:

```bash
grep -m1 "connectMode: Peer Disconnected" "$log"   # alive = (timeMS - connectionTime)/1000
```

Recomputed that way, every capture to date: **60.99, 60.25, 60.14, 60.15, 60.16,
60.80 seconds.** Including one that teleported to an entirely different place
mid-session and was dropped 60.8 s into the *second* connection, which is good
evidence the deadline is per-connection and not per-session or per-place.

**The DummyClient's gate is not client-side, and three attempts say so.**
`FStringRbxTransportDummyClientEnabledMinorVersions_PlaceFilter`,
`DFFlagReportDummyClientConnectionAttemptResult` and
`DFFlagClientReceiveNetStackPortAndToken` were each overridden through
`CORDIAL_FLAGS`, all applied cleanly, and **the probe ran every time**. That is
consistent with where the instruction comes from: the target arrives in the join
payload — `RbxTransport DummyClient will connect to server 128.116.51.33:35731
... rccAddr = 10.60.2.74:35731` — and the flags that decide whether a server
hands one out are RCC-side (`DFFlagRccReportNetStackPort`,
`DFFlagRccReportNetStackConfig`). A client flag cannot stop a server allocating
a port and naming it. **Guessing further flag names is not worth more joins**;
if the probe is to be suppressed the lever is not in this document.

**There is nothing for Cordial to implement in the transport.** Cordial does not
speak this protocol — the engine does, through Cordial's socket calls, and the
strace shows those are ordinary and correct: `socket(AF_INET, SOCK_DGRAM,
IPPROTO_UDP)`, `connect`, `SO_RCVBUF`/`SO_SNDBUF`, no `bind` (ephemeral source
port, which is normal), then ten good `sendmsg`. The framing that leaves
(`01 00 00 1F 01 11 01 …`) is the same shape as what the *working* RakNet socket
receives (`01 00 00 17 01 11 02 …`), so this is Roblox's own RUPP framing rather
than textbook QUIC on the wire, whatever the flag names say. The open question
is why the server ignores those datagrams, and **that cannot be answered from
this side of the connection.**

**The experiment that would answer it, and why it has not been run:** strace
Sober's probe and diff the first datagram against Cordial's. That needs Sober
launched into a game, which is the *main* account — deliberately kept separate
from the test account for ban-correlation reasons. Automated joins on it are not
something to do unattended.

#### Retracted: the DummyClient has nothing to do with 304

**"304 has never been observed on a run where the DummyClient connected" is
false**, and it was false the whole time — the sample was too small and nobody
had tabulated the probe's outcome against the disconnect. Doing that across every
Cordial capture:

```text
probe outcome          alive     reason
Connected to server    60.99 s    304
Connected to server    60.25 s    304
Failed to establish    60.14 s    304
Failed to establish    60.15 s    304
Failed to establish    60.16 s    304
Failed to establish    60.80 s    304
Failed to establish       -       no 304   (session ended before 60 s)
Failed to establish       -       no 304   (session ended before 60 s)
```

**Cordial's probe connects sometimes, and the 304 arrives at 60 seconds either
way.** The probe is irrelevant to the disconnect, which also means Sober's
advantage was never that its probe connects. Every "DummyClient NoResponse"
entry above this line is describing a coincidence.

**This cancelled the experiment that needed the main account.** Stracing Sober to
diff the first datagram was queued as the decisive test; it was decisive for
nothing, and the table that killed it came from logs already on disk. Worth
remembering next time an expensive capture looks necessary: tabulate the cheap
observation across every run first.

For the record, since it cost some effort to establish: Sober's Flatpak cannot be
traced from outside (`ptrace(PTRACE_SEIZE)` gives `EPERM` regardless of
`ptrace_scope=0`, and the distrobox container's own seccomp blocks the tracer),
`--devel` swaps the Platform runtime for the SDK and Sober segfaults on the
different `libcurl`, and `--allow=devel` does not lift the attach block. An
`LD_PRELOAD` shim would work and is **refused**: injecting code into the Roblox
process is exactly the primitive [ADR-001](adr/ADR-001-in-process-hooking.md)
rules out, and a debugging exception would be the thin end of it. A packet
capture needs root and is the honest route if this is ever needed again.

#### Retracted within the hour: the play session report is an *exit* report

**The section below is wrong and is kept only so the mistake is legible.** `Sent
play session success` is not something the client owes after joining. It is the
report a session sends when it **ends**, and reading the FSM timings with labels
on shows it immediately:

```text
Sober     1.176  Entered app session
         28.438  Sent app session success     <- app session ENDS here
         28.438  Entered play session
         49.427  Sent play session success    <- play session ENDS here
         49.427  Entered app session          <- back to the app

Cordial   1.330  Entered app session
          2.032  Sent app session success     <- same pattern, works fine
          2.033  Entered play session
                 (kicked at 62 s -> IASPE)
```

Sober logged the play report because somebody **left the game** at 49.4 s and
went back to the app. Cordial never logs it because Cordial is *disconnected*
rather than leaving — the FSM goes to `E` instead of reporting a clean exit.
Cordial's `Sent app session success` proves the mechanism works: the same
reporting path fires correctly for the session that did end normally, one second
into the run.

So the difference was a consequence of the 304, in exactly the way the `E` state
already was, and the "mechanism" was me pairing two log lines that are a session
apart. **Both clients were being compared at different points in their
lifecycle.** That is now four leads in this investigation killed by the same
error shape — a real observation from one run, generalised without checking what
it is relative to.

Nothing was built on it: the lifecycle callbacks were committed on their own
merits and labelled as not being the fix, which is the only reason this cost an
hour instead of a day.

**Also checked and clean:** the engine never asks for `getPackageCodePath`,
`sourceDir`, `getApplicationInfo`, package signatures or any hash over JNI. There
is **no client-side file integrity check reaching Cordial**, so "Roblox has
detected missing or corrupted files" is a reused generic code and not a
description of what the server actually objected to. Do not go looking for a
checksum to satisfy.

**One real gap did fall out of that sweep**, unrelated to 304:
`android/content/Context.getSharedPreferences` is unresolved, and the engine goes
on to call `edit()` and `putString()` on the `Invalid` class it gets back. The
engine is trying to persist key/value state and every write is going nowhere.

#### WRONG — kept for the record: Cordial never sends the play session report

`SessionTransitionFSM` runs the same way in both up to the last step, and then
diverges:

```text
Sober     Initialized -> Entered app session -> Sent app session success
          -> Entered play session -> Sent play session success
Cordial   Initialized -> Entered app session -> Sent app session success
          -> Entered play session -> (nothing)
```

**Cordial logs `Sent app session success` but never `Sent play session
success`.** The `E` in its `Session history: IASPE` arrives at 62.065 s, in the
same breath as the 304 — so the error state is the *consequence* of the
disconnect, not its cause, and the real gap is the missing report in the 60
seconds before it.

That is the exact shape the 60-second deadline predicts: something the client
owes shortly after joining and never delivers. **It is not a visible HTTP
failure** — the only failing requests in that window are
`users.roblox.com/v1/users` (400 and three 429s), and **Sober gets the same 400**,
so that is shared and not the discriminator.

**Not established:** what `Sent play session success` actually sends, whether its
absence causes the 304, or whether it is a third symptom of one cause. It is a
lead with a mechanism, which is more than anything else here has.

**Tried, and it did not work.** Five experience-lifecycle callbacks were
unresolved — `NativeHelper.gameActivity_onExperienceStart`,
`gameActivity_onGameLoaded`, `gameActivity_onDidLogInReceived`,
`gameActivity_onScreenOrientationChanged` and
`NativeGLJavaInterface.gameLoadedCallback`. The engine announces the experience
starting and the game loading, Cordial was listening to none of it, and Sober's
bridge answers the game-loaded one. Implementing them was the obvious first
attempt at unblocking the report. **They fire — six calls in a run — and the 304
is unchanged**: 60.2 s, and `SessionTransitionFSM` still goes `Entered play
session` straight to `IASPE` with no `Sent play session success` in between. So
the callbacks are answered now, which is worth having on its own, and they are
**not** what gates the report.

**One run in the middle of that looked like a fix and was not.** A 100-second run
showed no 304 at all, the FSM reached `Teleported.` and a `Session history: IASPJ`
— a `J` state Cordial had never produced. It was three connections of 42 s, 45 s
and 10 s: the place teleports roughly every 45 seconds, **and every teleport
starts a new connection with a fresh 60-second deadline**. Nothing survived long
enough to be disconnected. Run for 240 s instead, it teleports less and the 304
returns on the first connection to cross 60 s.

That is a trap worth naming, because it will catch the next person: **on a place
that teleports, absence of a 304 means nothing unless a single connection
exceeded 60 seconds.** Check `Connection accepted from` against the following
`connectMode` line before reading anything into a clean run. A place that does
not teleport would make a better test rig.

Also: **do not run Sober and Cordial at once on a 16 GB machine.** Both were up
during one attempt and the kernel killed Cordial (exit 137) and then Sober (134),
which reads as a Cordial crash and is not one.

**Causality is still open, and both stories fit the evidence.** Either the RCC
allocates a netstack port, waits for the probe, and drops the client at 60 s when
nothing arrives — which fits Sober connecting in 28 ms and living 256 s — or the
probe is pure telemetry and the 60-second deadline belongs to something else not
yet looked at. The second is not idle: a probe whose flags exist to *report*
whether it failed implies failure is an expected, tolerated outcome.

`docs/analysis/unresolved-jni.tsv` is **stale**: it still lists
`org/fmod/AudioDevice`, which is implemented. Regenerate it from a JNI-trace run
before trusting a count from it.

**What this changes about the next step.** Diffing a failing capture against a
known-good one was the plan while the difference was believed to be the server.
If the deadline is time-based, the question is instead *what the client owes the
server within 60 seconds of joining and never sends* — which is the shape of an
unanswered periodic report, and points back at the stub table rather than at the
network. `docs/analysis/unresolved-jni.tsv` is the place to start looking, not
`strace`.

**What is not established, and must not be written down as though it were:** why
`128.116.51.33` does not answer, whether the unanswered probe *causes* the 304 or
merely accompanies it, and whether Sober's advantage is different behaviour or
simply better luck with which edge it lands on.

**The next capture is cheap and the baseline now exists**, which is what was
missing before: rejoin until the client lands on a server that does not answer,
take the same `-e trace=desc,network` capture, and diff `epoll_ctl` and `recv` on
that fd against the good run above. One failing capture against one known-good
capture answers it. Guessing in between does not.

**Still unchecked, and the obvious next candidates.** Comparing Sober's
`appData` with Cordial's, Sober has `ClientSettings/` and `rbx-storage.db`;
Cordial has neither. And [issue #2](https://github.com/luohoa97/cordial/issues/2)
— User-Agent and base URL not matching what the real client sends — is still
open, with the exact string sitting in the capture:

```text
Mozilla/5.0 (…; 13) AppleWebKit/537.36 (KHTML, like Gecko)  ROBLOX Android App
2.732.1043 Tablet Hybrid()  GooglePlayStore RobloxApp/2.732.1043
(GlobalDist; GooglePlayStore)
```

Matching that is honest self-description — it is what the official Android
client sends, and Cordial *is* running that client. Forging an attestation is
not the same kind of thing, and the line between them is the one to hold.

## Traps that have already caught people

**Do not use present counts as a frame rate.** Every figure recorded before
2026-08-02 is an idle throttle integrated over a window, and several were quoted
as evidence.

**Do not measure timing under `WAYLAND_DEBUG=1`.** Three findings taken that way
vanished on untraced repeats minutes later.

**`CORDIAL_TRACE=1` aborts the engine.** It wraps variadic functions ABI-unsafely.

**`/proc/locks` names the wrong process.** Its PID column is whoever *acquired*
the lock, which `Claim::hand_to` makes the launcher — and the launcher has
usually exited. Scan `/proc/*/fd` instead. Observed: `/proc/locks` naming a PID
that no longer existed while a live process held the descriptor.

**Cumulative counters are not rates.** `pgsteal_kswapd` and friends integrate
since boot; reading one as a rate produced a confident and wrong diagnosis of
system-wide thrashing in this very project's history. Sample twice.

**`mutter --headless` segfaults** within a second of Cordial's window mapping,
on unmodified HEAD, with and without GPU rendering, while `gtk4-demo` in the
same nested compositor runs indefinitely. AGENTS.md offers nesting a headless
compositor as the way to drive Cordial's own window without touching the
developer's session, and it does not currently work. That is why
`crates/cordial-runtime/examples/pointer_capture_probe.rs` exists.

**Never synthesise input at the compositor.** `XTestFake*`, `ydotool`,
`wlr-virtual-keyboard`, the RemoteDesktop portal — all land on whatever has
focus, which is the developer's session. This has hijacked a developer's cursor
once already, mid-session.

## Layout

    crates/cordial-runtime   the loader, bionic shim, Android framework layer
    crates/cordial-shell     the launcher; also owns the shared window definition
    crates/cordial-plugins   registry, dependency resolution, unpacking
    crates/cordial-update    version check, download, verification, metered
    native/                  C++ shims: OpenSL ES, PipeWire, Java classes
    third_party/             the AOSP bionic linker port
    docs/adr/                decisions, including the reversed ones
    docs/traces/             a logcat capture of the same APK on real Android

**`docs/traces/` is the single most under-used thing in this repository.** Grep
it before disassembling anything. Note that the startup log is gzipped, so a
plain `grep -r` silently misses it — use `zgrep`, and do not read a zero hit as
evidence of absence.

## The two profile modules

`cordial_shell::profile` and `cordial_runtime::profile` implement the same
contract twice. That is not a design; `cordial-runtime` depends on
`cordial-shell` for `host_window`, so the dependency cannot be inverted without
a cycle. The shell's copy is the live one — `cordial-run` never calls its own.
Unifying them is a genuinely good first contribution.

## House style

Comments explain *why*, anchored in the failure that motivated the code. Commit
messages say what you measured, and they are long here on purpose. British-ish
prose, no emoji, no bullet-list comment blocks. Read the surrounding file before
writing; the voice is consistent and matching it is not optional.

Arguing with an ADR is welcome — ADR-004 was reversed exactly that way. What is
not acceptable is quietly contradicting one in code.

## A whole class of bug: hooks that register and never bind

**libjnivm binds by descriptor, and a mismatch fails silently in both
directions.** The hook registers, the symbol resolves, and the engine still
reports `Constructed Unresolved symbol` — or worse, calls a default and gets a
zero. `tools/dex_method.py`'s docstring has warned about this since it was
written; what is new is that **two live instances were found in one evening**,
both in code that had been committed and believed to be working.

**`LocalStorageManager.getAllocatableBytes` was hooked as an instance method and
is static.** The JNI trace shows the engine calling it three times, every call
logging a `java/lang/Class` receiver, which is what a static call looks like.
`HookInstanceFunction` never matched, so the engine read free space as zero.
Fixed. `docs/analysis/unresolved-jni.tsv` records it as an instance method and is
wrong.

**`GameActivity.getWindowInsets`/`getWaterfallInsets` return the wrong type.**
They are hooked correctly, as instance methods on the right class, and they still
do not bind: the hooks return `std::shared_ptr<Object>`, so libjnivm registers
`(I)Ljava/lang/Object;` while the engine looks up
`(I)Landroidx/core/graphics/Insets;`. **The insets work has therefore never taken
effect**, months after it was committed and while it was being cited in the
fullscreen investigation.

*The fix for that one is not in the tree.* Moving `Insets` into a shared header so
both files have the complete type made the process abort at `JNI_OnLoad` with
"Rust cannot catch foreign exceptions" — almost certainly a duplicate class
registration once the type appears in two translation units. Reverted rather
than shipped. **Anyone attempting it again should check the run actually
started**: the aborted build reported *zero* unanswered JNI methods, which looks
like a total fix and means the opposite.

**The general rule this suggests:** if a hook must return a specific Java class,
the C++ return type has to *be* that class. `Object` is not a wildcard, and
nothing warns you.

## What Cordial does not answer, measured rather than derived

`docs/analysis/unanswered-jni-observed.tsv` — 19 distinct methods across 9
classes, from one traced run that joined a game and played to the 304. Notable:
`NativeGLJavaInterface` accounts for 6 (purchases, advertising id, analytics
callback, VR, save-image), `GameActivity` for 4 (including the two insets above,
which are the bug rather than a gap), and `ActivityThread`, `JNIBaseUrlSetter`,
`JNIAppRestarter`, `CookieProtocol.setCookie` and
`NetworkUtils.getPublicIPv4Addresseses` one each.

Only 2 of the 648 generated libc stubs were called: `ZSTD_trace_compress_begin`
and `ZSTD_trace_decompress_begin`. **Those are not gaps.** They are zstd's
tracing hooks, imported by `libzstd-jni` as well as by the engine, and returning
zero is what "not tracing" means to zstd — demonstrated by 324 calls to
`decompress_begin` in one run with every asset decompressing correctly.

## rbx-storage: what it is not

Five things ruled out, so nobody repeats them:

- **Not the init call.** `LocalStorageManager.initStorageManagerNativeV3` is now
  called with the prototype read from the dex, and returns cleanly.
- **Not free space.** `getAllocatableBytes` now answers a real `statvfs`; storage
  is still down.
- **Not SQLite.** The engine imports zero `sqlite3_*` symbols and has no
  `libsqlite` in `DT_NEEDED` — it is statically linked, so nothing is stubbed.
- **Not the directories.** `files` and `cache` are created before use and exist.
- **Not a flag Sober sets.** Sober's `config.json` `fflags` block is the stock
  `{"FFlagExample": true}`. Whatever brings up its 167 MB `rbx-storage.db` is
  platform-layer completeness, not configuration, so there is nothing to copy.

**And the measurement that reframes it:** under `CORDIAL_TRACE_PATHS=1`, across a
whole run, the engine **never touches a path containing `rbx-storage` at all**.
It is not failing to open the database. It never tries. So the question is not
"why does the open fail" but "what never asks for the store to exist", and the
next instrument is a traced run read forward from `initStorageManagerNativeV3`
rather than another candidate native.

## Voice: RETRACTED — it can be written, and the evidence against it was the wrong table

**Everything below this paragraph, until the `AppRtcDeviceWrapper` section, is
wrong.** Retracted 2026-08-26, kept rather than deleted because the mistake is
more instructive than the correction.

The argument was: `nativeGetPlayoutData` and `nativeCacheDirectBufferAddress` do
not appear among `libroblox.so`'s exports, therefore nothing is behind
`WebRtcAudioTrack`, therefore writing it produces a class with nothing on the
other side. Every step of that is sound except the first, and the first is a
category error: **`docs/analysis/jni-natives.tsv` is an exported-symbol table,
built with `nm -D`, and these natives are not exported. They are registered at
run time through `RegisterNatives`.**

The tell was in the evidence and was read as proof of the opposite. The one
WebRTC symbol that *is* exported is
`Java_com_roblox_universalapp_webrtc_WebRtcLoader_initialize` — **a loader**.
Registering natives is what a loader does.

Three things confirm it, and all three were already in this repository:

- `native/game_activity.cpp` records the identical pattern and depends on it:
  `terminateNativeCode` "is not exported the way `initializeNativeCode` is —
  `nm -D` on the shipping `libroblox.so` shows only
  `Java_com_google_androidgamesdk_GameActivity_initializeNativeCode` among its
  defined symbols. It is one of the 24 natives AGDK registers dynamically
  through `RegisterNatives`". Cordial looks those up by name in `cls->natives`
  and has done for months.
- `third_party/libjnivm/src/jnivm/vm.cpp`'s `RegisterNatives` stores exactly
  that: `clazz->natives[method->name] = method->fnPtr`. The pointers are kept
  and are reachable by the same lookup AGDK's use.
- `docs/analysis/observed-java-surface.md` lists
  `org/webrtc/voiceengine/WebRtcAudioTrack` among the classes the engine
  **reaches for at run time** — not merely among the classes the dex declares.
  A class the engine touches is not a class bundled by accident.

So the downlink half is writable, `WebRtcAudioManager.init()` returns false
because of its absence rather than because the path is dead, and the seven-
eighths of this Cordial already has — a real `CaptureStream` with the microphone
lifetime rule, the whole of `WebRtcAudioManager`, the honest parameter answers,
and the `OutputStream` seam added in `b2837e7` — stops being groundwork for a
dead path.

**This is the failure the top of AGENTS.md is about**, committed with a table
rather than a disassembler: nine consecutive conclusions drawn from reading the
stripped binary were wrong, and this is the tenth. The rule generalises further
than it is written — *an absence in a table only means what the table indexes*.

**The next step is a measurement and not a feature.** Trace `RegisterNatives`
for `org/webrtc/voiceengine/*` on a run that reaches a voice-enabled experience
and print every name and signature registered on `WebRtcAudioRecord` and
`WebRtcAudioTrack`. Seeing `nativeDataIsRecorded (IJ)V` and
`nativeGetPlayoutData (IJ)V` in that log settles it in one run, and either
confirms this retraction or retracts the retraction.

---

*The original section follows, wrong, for the record.*

`org.webrtc.voiceengine.WebRtcAudioManager`/`WebRtcAudioRecord` are implemented
here, and the downlink half (`WebRtcAudioTrack`) was the recorded next step. **It
cannot be written.** `WebRtcAudioTrack`'s whole job is to pull audio from the
engine by calling `nativeGetPlayoutData(int, long)` and
`nativeCacheDirectBufferAddress(ByteBuffer, long)`, and **this engine exports
neither.** Across the whole of `libroblox.so` there is exactly one WebRTC JNI
export:

```text
Java_com_roblox_universalapp_webrtc_WebRtcLoader_initialize
```

The `org/webrtc/voiceengine/*` classes are in the dex because the Android app
bundles Google's WebRTC Java, not because this engine calls into them. Writing
`WebRtcAudioTrack` would produce a class with nothing on the other side of it.

**The path the engine actually drives is `com.roblox.audio.AppRtcDeviceWrapper`,
and Cordial implements none of it.** Its native counterpart *is* exported —
`Java_com_roblox_audio_AppRtcDeviceWrapper_nativeAudioDeviceChanged` — and the
dex declares the Java side:

```text
AppRtcDeviceWrapper.<init>(J)V
AppRtcDeviceWrapper.wrapStartCommunication()V
AppRtcDeviceWrapper.wrapStopCommunication()V
AppRtcDeviceWrapper.wrapSetCommunicationMute(Z)V
AppRtcDeviceWrapper.getSelectedAudioDeviceAsInt()I
AppRtcDeviceWrapper.getSelectedAudioDeviceName()Ljava/lang/String;
AppRtcDeviceWrapper.isValid()Z
AppRtcDeviceWrapper.nativeAudioDeviceChanged(IJ)V
```

That is a communication-device wrapper — start/stop/mute and which device is
selected — which is a much smaller surface than a WebRTC audio backend, and it
sits on top of the PipeWire streams that already exist. **It is the voice work
that is worth doing**, and the seven static-hook fixes in `2ca7811` are not
undone by this: they were real bugs, they just live on a path the engine may
never take.

`WebRtcAudioManager.init` reporting failure therefore remains correct, and for a
better reason than the one recorded beside it: not only is the downlink
unimplemented, it is unimplementable against this build.

## `onFlagsFailed` is not harmless: it is what blocks the content store

`docs/analysis/flag-init.md` and the comment in `native/init_params.cpp` both say
`onFlagsFailed` "is not the same as flag data failing to load, and does not block
startup". The first half is right and the second half is true but was read far
too broadly — it does not block *startup*, and it does block **RbxStorage**.

Three observations, one chain:

```text
Sober    0.494  [FLog::AndroidGLView] nativeInitClientSettings
         0.510  [FLog::AndroidGLView] nativePostClientSettingsLoadedInitialization3
         0.515  [DFLog::RbxStorage] RbxStorage::init [INIT] user: flagLoaded
         0.536  [DFLog::RbxStorage] RbxStorage::init [DONE] dbOpenCount: 69

Cordial         [roblox] flags: engine reported onFlagsFailed
                (no AndroidGLView line, no RbxStorage line, in any of 31 runs)
```

and the live flag set carries `FFlagStartRbxStorageInitRighAfterFlags = True`.
Storage constructs itself off the *flags-loaded* event. Cordial never produces
that event, so the store is never built — which is why
`CORDIAL_TRACE_PATHS=1` shows the engine never touching an `rbx-storage` path:
there is nothing to open because nothing asked for it.

**This retires "why does onFlagsFailed happen" from curiosity to blocking.** It
was investigated with a debugger before and shelved because it appeared to cost
nothing. It costs the content store, and possibly more.

**A new lead the earlier investigation did not have.** The engine exports *five*
client-settings entry points, and Cordial calls the plainest:

```text
nativeInitClientSettings(String,String,String)I                        <- Cordial
nativeInitClientSettingsCached(String,String,String,String,J)I
nativeInitClientSettingsCachedCompressed([B,String,String,String,J,Z)I
nativeInitClientSettingsSigned(String,String,String,String)I
```

Cordial's call returns 0, which `client_settings.rs` establishes means the
document was accepted. But acceptance is evidently not the same as the engine
considering its flags *loaded*, and the modern app very likely uses the signed or
cached-compressed path. Two supporting observations: Cordial's engine later goes
and requests `clientsettingscdn.roblox.com/v2/settings-compressed/application/.zst`
on its own and gets a 403, and the `AndroidGLView` log line Sober emits for this
call never appears in Cordial even though Cordial's log is open well before the
call runs (log opens 0.634s; the call is after `initializeNativeCode`).

**Next experiment, and it is cheap:** call the signed or cached-compressed
variant instead and watch for `RbxStorage::init` in the engine's own log. The
prototypes are above, read from the dex with `tools/dex_method.py` — do not guess
the arity, two crashes this session came from exactly that.

`nativePostClientSettingsLoadedInitialization3(List)` being handed an **empty
ArrayList** is the other candidate at the same seam and is already flagged as
unresolved in `flag-init.md` §7.4.

### Tried live: the compressed settings variant does not accept either

`nativeInitClientSettingsCachedCompressed` was wired up end to end — the real
302 KB `.zst` fetched from
`clientsettingscdn.roblox.com/v2/settings-compressed/application/AndroidApp.zst`,
handed over as a `jbyteArray` with the dex's arity, and the `int` read back. It
is reachable and it does not accept:

```text
compressed client settings (302349 bytes) -> 2      (during a full run)
compressed client settings (302350 bytes) -> 5      (three short runs, all 5)
```

**The return code is not a function of the arguments.** Varying the trailing
`long` and `boolean` across three probes produced 5 every time, and an earlier
run with the *same* arguments produced 2. So the code reflects state or document
content rather than the call, and `client_settings.rs`'s "0 accepted, 1 rejected"
table does not cover it. Neither 2 nor 5 is 0, so the compressed path is not
succeeding, and `onFlagsFailed` and the missing `RbxStorage::init` are unchanged.

The code was **reverted** rather than left in: a startup call that always fails
and changes nothing is noise, and shipping it would imply the path works.

Two things worth keeping from it. The byte count moved between fetches minutes
apart (302349 → 302350), so **the settings document is live and changes
continuously** — any experiment that compares two runs is comparing two different
flag sets, which the 6-hour cache in `client_settings.rs` normally hides.
And the probe loop itself is the cheap way to work on this: the call happens at
startup, so `--run 8` with no `--join-url` reads the return code in seconds
without joining a game or touching an account.

### The signed settings endpoint does not exist at any guessable URL

`nativeInitClientSettingsSigned(String,String,String,String)I` is exported, but
there is nothing to feed it. Probed on both hosts, 2026-08-07:

```text
clientsettings.roblox.com     v2/settings/application/AndroidApp          200  1273628B
clientsettingscdn.roblox.com  v2/settings/application/AndroidApp          200  1273628B
both hosts                    v2/settings-signed/application/AndroidApp   404
both hosts                    v1/settings-signed/...                      404
both hosts                    v2/signed-settings/...                      404
both hosts                    v2/settings/application/AndroidApp/signed   404
both hosts                    v2/settings-compressed-signed/...zst        404
```

The 404 body is `{"errors":[{"code":0,"message":""}]}` — empty, so it names no
correct path. Changing the base URL does not help: the plain document is served
identically by both hosts, and neither has a signed sibling. **Whatever supplies
the signature is not a public CDN path that can be guessed**, so the signed
variant cannot be tried without learning where the real app gets it — and that is
not answerable from the URLs, only from watching a real client's traffic.

So of the four client-settings entry points, the state is: plain **accepted (0)**
but leaves `onFlagsFailed`; compressed **reachable, never accepts (2/5)**; signed
**unfeedable**; cached untried.
