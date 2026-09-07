# ADR-016: A profile can refuse to run without a VPN, checked by a command it names

**Status:** accepted, amended 2026-09-08

## Amendment, 2026-09-08: the check is the operator's command, not one project's

**What changed.** This ADR originally brokered the check through
[`pvpn`](https://github.com/luohoa97/protun-unblocked), a VPN wrapper by this
project's own maintainer, and `crates/cordial-shell/src/pvpn.rs` shelled out to
its `status` verb. That module is deleted. `Mode::VpnRequired` now runs a
`check` command the profile's own `network.json` names -- argv, exit zero means
the requirement is met -- and Cordial names no tool and ships no default.

**Why.** A client hard-coding a dependency on its maintainer's other project
reads as self-promotion whatever the technical merits, and the merits did not
require it: what a separate address means is the operator's to define, and the
only thing Cordial needed was a yes or no. `CORDIAL_PVPN_BIN` already made the
binary configurable, which is the tell -- the coupling was to a *name*, not to
a capability.

**What did not change.** The decision below stands in full: the reason the mode
exists, the argument that a network namespace is the right long-term answer and
was not shippable in that pass, and the gate being enforced at both entry
points. The namespace reasoning is kept as written even though it is phrased
around one client's behaviour, because it is an argument about NetworkManager
rather than about that client, and it holds for any VPN managed the same way.

**One behaviour is new.** `vpn-required` with no `check` configured now refuses
to launch. Previously the equivalent state was "the tool is not installed",
which also refused. A check that cannot run refuses too: it has established
nothing, and reading "I could not tell" as "yes" would be a stub lying about
the one thing this mode exists to guarantee.
**Extends:** [ADR-013](ADR-013-per-profile-configuration.md)
**Related:** [ADR-007](ADR-007-host-resources-are-brokered.md), [ADR-012](ADR-012-profiles-and-instances.md)

## Context

AGENTS.md already states a constraint this project's own testing guidance
depends on: "Do not test with an account anyone cares about, and keep test
accounts on a separate IP. The risk is collateral rather than causal:
enforcement is automated, runs in waves, and associates accounts sharing an
address." That sentence assumes a mechanism for giving a profile a separate
address. Until this change there was none — every profile, however many are
signed in at once (ADR-012 demonstrates two, side by side), shares this
machine's one route to the internet. This is not a bolt-on convenience; the
project's own contributor guidance already requires the thing this ADR builds.

## Decision

A profile's `network.json` — placed per ADR-013, beside `flags.json` and
`plugin-grants.json`, because network egress is identity-scoped in exactly the
sense that ADR draws the line by — may set `"mode": "vpn-required"`. A profile
in that mode refuses to start at all unless the `check` command it names
exits zero, checked at both of
Cordial's entry points: the shell's `launch.rs`, before the engine process is
even spawned, and `cordial-run`'s own `main`, so that starting the client
directly — which AGENTS.md documents as fully supported — cannot bypass a
requirement the shell would have enforced. A profile with no `network.json`,
which is every profile that exists today, is unaffected.

The check runs the configured command and reads its exit status, and nothing
else. Cordial never brings a tunnel up or down — see the amendment above and
`crates/cordial-shell/src/network.rs` for why
deciding when to connect is left to whoever is running Cordial rather than
folded into a launch button.

## Why not an `http_proxy`/`HTTPS_PROXY` setting

This was the obvious shape and it was rejected on two independent grounds,
either of which would have been enough alone.

**Cordial's own client-settings fetch would never see it.**
`client_settings.rs` calls `ureq::get(URL).call()` directly. `ureq` does not
consult proxy environment variables on its own, so setting them would do
nothing for the one HTTP request Cordial itself makes, before the engine
exists to blame for anything.

**Even where the engine's traffic would see it, it is not the traffic that
matters most.** `client_settings.rs` and `android/asset.rs` both record, from
the engine's own observed behaviour, that its HTTP stack is curl —
`CURLOPT_CAINFO` wanting a real filesystem path is what sent the CA bundle
extraction to a real directory in the first place. curl does honour
`http_proxy`/`HTTPS_PROXY`/`ALL_PROXY` by default, and `bionic/mod.rs`'s
function-override table — consulted directly, not assumed — does not override
`getenv`, `connect`, or `socket`: they are ABI-compatible between bionic and
glibc, so they resolve straight to the host's real libc, in the same process,
sharing the same real `environ`. A proxy variable set on this process would in
principle be visible all the way down to curl's own `getenv` calls. That much
is a structural fact about how the loader resolves symbols.

It still would not be enough. The Waydroid trace
(`docs/traces/waydroid-roblox-startup.log.gz`) and this project's own sign-in
notes both name `DFLog::RbxTransportIoLibContext` and `RtcIoRna` — Roblox's
real-time game transport, which every account's actual join to a game server
goes over, and which the "Rtc" in its own name already says is not an HTTP
request curl is making. `http_proxy` conventions are specific to HTTP(S)
libraries that choose to read them; they do nothing for an arbitrary UDP
socket a transport layer opens for itself. A proxy that genuinely worked for
curl would still leave the one connection enforcement actually watches — the
join to a game server — going out this machine's ordinary route. Shipping an
`http_proxy`-shaped setting here would have been precisely the failure
AGENTS.md calls out by name: one that looks like it does the job and does
not, which is worse than no setting, because it would be believed.

## Why not a network namespace, yet

A namespace is the mechanism that would actually be airtight — it routes by
process, not by which library asks nicely, so it covers curl and `RtcIoRna`
alike. Two things had to be established before it could be ruled in as
buildable this pass, and both came back against it.

**This session measured itself not to have the privilege.** `unshare --net --
ip link` was run directly, in the environment this was written in, and failed
immediately: `unshare: unshare failed: Operation not permitted`.
`CLONE_NEWNET` wants `CAP_NET_ADMIN`, ordinarily root, on an unprivileged
process. This is a real deployment constraint for whoever packages Cordial —
a Flatpak in particular does not hand this out by default, and ADR-007's
argument against broad sandbox permissions applies here exactly as it does to
`--filesystem=host`.

**And the common kind of VPN client would not scope into one even if the
privilege existed.** This was established by reading one such client rather
than assumed, and the conclusion is about NetworkManager rather than about that
client: a tunnel managed as a NetworkManager connection (`nmcli con up`,
`nmcli con show --active`, and the kill-switch device it leaves behind) is
brought up by a system service running in the host's own network namespace.
The interface it creates lands there regardless of which namespace the command
that asked for it was run inside. Bringing such a tunnel up under `ip netns
exec cordial-<profile>` would produce the same machine-wide tunnel it always
produces, asked for from a process that happened to be in a namespace at the
time — not a tunnel scoped to that namespace. A namespace that could hold a
tunnel of its own would have to bypass NetworkManager entirely: extract the
WireGuard parameters an established connection had negotiated, and bring up a
second, namespace-local interface with `wg-quick` directly. That is a
different piece of work and this pass did not build it.

## What this ships instead

A coarser, honest guarantee, not the strong one. A `vpn-required` profile
refuses to start at all unless the `check` command it names exits zero.

**A lesson worth passing to whoever writes that check**, learned from the
client this originally brokered: "connected" and "carrying traffic" are
different questions, and a client can go on reporting the first after a suspend
while the transport underneath is dead. A check that tests only for
"connected" agrees with a tunnel that is not working, which is worse than no
check, because it is acted on. Test for traffic actually passing.

It does not isolate a running profile's traffic from a different profile
running alongside it on the same machine at the same time — ADR-012's own
demonstrated two-windows-at-once case — because a machine-wide tunnel is one
global route, not one per profile. What it does guarantee, at both of
Cordial's entry points: this profile will never make even its own
client-settings request on this machine's ordinary route while believing
itself protected, and a profile with no `network.json` behaves exactly as it
always has.

## Evidence

**Measured, this session:**

- `unshare --net -- ip link` on the machine this was written on:
  `unshare: unshare failed: Operation not permitted` (`id` shows an
  unprivileged user; `getpcaps` shows no `CAP_NET_ADMIN`).
- The status verb of the VPN wrapper this originally brokered, run for real
  against a genuinely connected free server at the time, reported both
  "connected" and "traffic passing" as separate lines and emitted no ANSI
  escapes when piped. **Kept as the record of where the connected-versus-
  passing distinction came from**, which is the one part of that integration
  worth carrying forward; the tool itself is no longer involved, and a check
  command's exit status is now the whole contract, so nothing parses text any
  more.
- `cargo build --release` and `cargo test --workspace`, both green, including
  the new modules' tests.
- A genuine, intermittent test-isolation bug this change's own testing
  surfaced: `install.rs`/`profile_switcher.rs`/`launch.rs` each kept a private
  mutex guarding `CORDIAL_PROFILE_ROOT`, which does not serialise one file's
  env-var writes against another's in the same test binary. Adding
  `launch.rs`'s gate test made this fail for real, once, out of several runs
  — `profile_switcher::tests::the_list_offers_no_profile_that_does_not_exist`
  read back another test's scratch directory mid-assertion. Fixed by sharing
  one mutex (`crate::PROFILE_ROOT_ENV` in `main.rs`) across every file in the
  binary that touches that variable; twelve subsequent runs were clean.

**Read, not run, and said so in the code that relied on it:** the wrapper's
status verb only printed its traffic line while it believed it was connected,
so a disconnected run produced no such line at all — established by reading
that tool's source, not by disconnecting the real tunnel in use on the machine
this was written on, which the session deliberately avoided disturbing. Kept
because the distinction between reading and running is the kind of claim this
project asks to be labelled, and because nothing parses text any more: the
exit status of the operator's own command is the whole contract now.

**`INFERRED`:** that curl inside the engine actually honours
`http_proxy`/`HTTPS_PROXY` the way libcurl's documented default behaviour
says it should. The structural path — same process, same `environ`, `getenv`
unshimmed — was verified by reading `bionic/mod.rs`; whether Roblox's own
curl usage explicitly disables environment-variable proxy detection via
`CURLOPT_PROXY` is not something this pass could observe (that would need
tracing an actual curl call inside a running, signed-in client, which needs an
account and is out of this pass's scope). It does not matter for the decision
above either way, because the `RtcIoRna` transport argument holds regardless
of curl's behaviour — which is exactly why this ADR does not lean on it.

## Consequences

**Accepted:** no true concurrent isolation between profiles running at the
same time. This is the honest limit of a machine-wide tunnel, stated plainly
rather than implied away — see "What this ships instead," above.

**Accepted:** this never brings a tunnel up or down itself, and so adds real
friction — bring the connection up before launching a `vpn-required` profile,
same as today, just now enforced rather than merely advised. An ordinary VPN
connect was measured at tens of seconds on the client this was written
against; folding that into a launch button was considered and
rejected as a second surprising thing happening at the moment somebody
expected only a game to open.

**Accepted:** no mid-session monitoring. A tunnel that drops after launch is
not detected by this change; the profile that was checked at launch keeps
running on whatever route is left. Watching for that would need a background
poller this pass did not build — see HANDOVER.md.

**Rejected: a network namespace this pass.** Ruled out analytically rather
than attempted and abandoned — see "Why not a network namespace, yet," above.
Remains the right long-term mechanism once something can produce a tunnel
scoped to one namespace — which means bypassing NetworkManager — and
once Cordial's packaging can grant the namespace privilege this session
measured itself not to have.

**Rejected: an `http_proxy`-shaped setting.** Would have looked like it worked
for exactly the traffic that matters least (curl-based HTTP) and done nothing
for the traffic that matters most (`RtcIoRna`'s real-time transport) — see
above.

## What would change this

If a VPN client grows a way to hand over the WireGuard parameters of an
established connection — or if the common Linux clients move off
NetworkManager for their tunnels — a namespace-scoped tunnel becomes
buildable, and with it true
concurrent per-profile isolation rather than the launch-time gate this ADR
ships. If Cordial's packaging ever grants `CAP_NET_ADMIN` (or runs
unsandboxed with root available), the privilege half of the namespace
argument stops applying; the NetworkManager half would still need answering
first.
