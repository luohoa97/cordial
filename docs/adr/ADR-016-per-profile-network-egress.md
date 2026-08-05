# ADR-016: A profile can refuse to run without a VPN, brokered through `pvpn`

**Status:** accepted
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
in that mode refuses to start at all unless [`pvpn`](https://github.com/luohoa97/protun-unblocked)
reports a tunnel that is actually passing traffic, checked at both of
Cordial's entry points: the shell's `launch.rs`, before the engine process is
even spawned, and `cordial-run`'s own `main`, so that starting the client
directly — which AGENTS.md documents as fully supported — cannot bypass a
requirement the shell would have enforced. A profile with no `network.json`,
which is every profile that exists today, is unaffected.

The check shells out to `pvpn status` and nothing else. It never calls
`pvpn up`, `down`, or `hop` — see `crates/cordial-shell/src/pvpn.rs` for why
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

**`pvpn` would not scope into one even if the privilege existed.** Reading
`bin/pvpn` in the sibling project settles this rather than assuming it:
`cmd_up` drives Proton's own Linux client, which manages its tunnel as a
NetworkManager connection (`nmcli con up`, `nmcli con show --active`, and the
kill-switch device `pvpnksintrf0` NetworkManager leaves behind).
NetworkManager is a system service running in the host's own network
namespace; the interface it brings up is created there regardless of which
namespace the command that asked for it was run inside. Running `pvpn up`
under `ip netns exec cordial-<profile>` would produce the same machine-wide
tunnel `pvpn up` always produces, asked for from a process that happened to be
in a namespace at the time — not a tunnel scoped to that namespace. A
namespace that could actually hold a Proton tunnel of its own would need to
bypass NetworkManager entirely: extract the WireGuard parameters an
established connection actually negotiated, and bring up a second,
namespace-local interface with `wg-quick` directly. `pvpn` does not expose
that today, and this pass did not build it.

## What this ships instead

A coarser, honest guarantee, not the strong one. A `vpn-required` profile
refuses to start at all unless `pvpn status` shows traffic actually passing —
not merely "connected", which `pvpn`'s own `cmd_status` already distinguishes
from a stale, post-suspend tunnel that claims to be up while dead (see
`pvpn.rs` for why `Traffic: passing` is the only string this trusts). It does
not isolate a running profile's traffic from a different profile running
alongside it on the same machine at the same time — ADR-012's own demonstrated
two-windows-at-once case — because the tunnel `pvpn` brings up is one, global,
machine-wide route, not one per profile. What it does guarantee, at both of
Cordial's entry points: this profile will never make even its own
client-settings request on this machine's ordinary route while believing
itself protected, and a profile with no `network.json` behaves exactly as it
always has.

## Evidence

**Measured, this session:**

- `unshare --net -- ip link` on the machine this was written on:
  `unshare: unshare failed: Operation not permitted` (`id` shows an
  unprivileged user; `getpcaps` shows no `CAP_NET_ADMIN`).
- `pvpn version` and `pvpn status`, run for real against a genuinely installed
  `pvpn`, genuinely connected to a free Proton server at the time: `Status:
  Connected`, `Server: SG-FREE#5 in Singapore, Singapore`, `Protocol:
  protun-tls`, `Traffic: passing`, with no ANSI escapes when piped —
  confirming `pvpn.rs`'s parser can rely on plain text under
  `Command::output()`.
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

**Read, not run, and said so in the code that relies on it:** `pvpn`'s
`cmd_status` only prints a `Traffic:` line inside its `if is_connected` branch,
so a disconnected `pvpn status` produces no such line at all — this was
confirmed by reading `bin/pvpn` directly, not by disconnecting the real tunnel
already in use on the machine this was written on, which this session
deliberately avoided disturbing.

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
friction — connect with `pvpn up` before launching a `vpn-required` profile,
same as today, just now enforced rather than merely advised. `pvpn`'s own
README measures ordinary connects at 12 to 45-plus seconds before its grace
period even starts; folding that into a launch button was considered and
rejected as a second surprising thing happening at the moment somebody
expected only a game to open.

**Accepted:** no mid-session monitoring. A tunnel that drops after launch is
not detected by this change; the profile that was checked at launch keeps
running on whatever route is left. Watching for that would need a background
poller this pass did not build — see HANDOVER.md.

**Rejected: a network namespace this pass.** Ruled out analytically rather
than attempted and abandoned — see "Why not a network namespace, yet," above.
Remains the right long-term mechanism once `pvpn` (or a parallel path that
bypasses NetworkManager) can produce a tunnel scoped to one namespace, and
once Cordial's packaging can grant the namespace privilege this session
measured itself not to have.

**Rejected: an `http_proxy`-shaped setting.** Would have looked like it worked
for exactly the traffic that matters least (curl-based HTTP) and done nothing
for the traffic that matters most (`RtcIoRna`'s real-time transport) — see
above.

## What would change this

If `pvpn` grows a way to hand over the WireGuard parameters of an established
connection — or if Proton's Linux client stack moves off NetworkManager for
its tunnel — a namespace-scoped tunnel becomes buildable, and with it true
concurrent per-profile isolation rather than the launch-time gate this ADR
ships. If Cordial's packaging ever grants `CAP_NET_ADMIN` (or runs
unsandboxed with root available), the privilege half of the namespace
argument stops applying; the NetworkManager half would still need answering
first.
