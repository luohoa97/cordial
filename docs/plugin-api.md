# The Cordial plugin API

A plugin is a directory holding a `plugin.json` and, usually, one TypeScript
module. Cordial runs that module as a separate Deno process with **no
permissions at all** — no file, network, environment or subprocess access — and
talks to it in newline-delimited JSON over its standard input and output.
Everything a plugin can reach arrives down that pipe, and every request is
checked against the capabilities the user granted it, in this profile, before
anything happens.

So a plugin can read and contribute FastFlags and Cordial's own render
settings, keep a settings document, have Cordial draw it a preferences page,
publish Discord Rich Presence, post a desktop notification, open an `http` or
`https` page, talk to other plugins over a small event bus, and shadow Roblox's
own textures, sounds, fonts and models with files of its own.

It cannot execute code inside the Roblox process, read the DataModel or the Lua
state, draw anything on screen, open a socket or a file, or widen Cordial's own
sandbox. Those are absent from the surface rather than switched off
([ADR-001](adr/ADR-001-in-process-hooking.md),
[ADR-003](adr/ADR-003-plugin-isolation.md)). Two things that look implemented
are not, and the sections below say so where they arise: **`flags.setDynamic`
has no effect and never will**, and **two of the five core lifecycle events are
published by nothing** — `cordial/client.ready` and `cordial/window.resized`
have no publisher anywhere outside tests, so a plugin that waits for either
waits forever. The other three do arrive.

## How this document was checked

Every signature, parameter name, refusal message and limit below was read out of
the handler that implements it, at `v0.10.0-8-g767ce98-dirty`, and the file and
function are named wherever the fact is one a reader might want to check.

**The `-dirty` matters here.** The core event bus — `publish_core` in
`crates/cordial-runtime/src/plugin_host.rs` and its three call sites in
`crates/cordial-runtime/src/bin/load.rs` — was read out of an uncommitted
working tree, so it is newer than any tag. Everything below about
`cordial/client.launch`, `cordial/engine.version` and `cordial/client.shutdown`
arriving is true of that tree and false of `v0.10.0` as released, where nothing
published a core event at all.
Nothing here was observed by running a client — this was written under a
read-only brief — so anything that is an inference from a code path rather than
a statement the code makes about itself is labelled **INFERRED** in the text.
The two measurements quoted below — the drop count under a fast publisher, and
the window in which a `DF*` override survives — are readings this repository
already recorded, not ones taken here.
Where this document contradicts a comment in the source, it says so, because a
comment that lies costs more than no comment.

## Contents

- [Getting started](#getting-started) — what a plugin is, the manifest, the
  protocol, the sandbox, how to test one, and a complete plugin you can copy
- [Capabilities and grants](#capabilities-and-grants) — the fourteen
  capabilities, [the table of what each one gates](#the-fourteen-capabilities),
  default deny, and what a refusal looks like on the wire
- [Events](#events) — the two buses, the core-event table, and what plugins may
  say to each other
- [FastFlags](#fastflags) — the layers, `flags.list`/`get`/`set`, and the two
  lifetimes
- [The rest of the surface](#the-rest-of-the-surface) — notifications,
  presence, URLs, settings, preferences, asset overlays
- [What you cannot do](#what-you-cannot-do) — the walls, and the reasoning
  behind each

---

# Getting started

## What a plugin is

A plugin is a directory containing a `plugin.json` and, usually, one TypeScript
module, run as a sandboxed Deno process that speaks newline-delimited JSON over
its standard input and output.

That shape is the whole design rather than an implementation detail. A plugin
cannot open a file, a socket or a subprocess, so a capability system that lets it
ask Cordial to do things is the only surface there is — see
[ADR-003](adr/ADR-003-plugin-isolation.md) for the isolation and
[ADR-007](adr/ADR-007-host-resources-are-brokered.md) for why Cordial performs
the effect rather than handing over the resource.

A plugin does not have to contain code. A manifest with no `entry` is a plugin
made of data — a texture pack, a set of flags, a preferences page — and Cordial
says so at startup rather than treating it as a failure
([ADR-021](adr/ADR-021-everything-is-a-plugin.md)). Everything below is about
the kind that runs.

## The smallest one that works

Three files, and one of them is not yours.

```
~/.local/share/cordial/plugins/
└── hello/
    ├── plugin.json
    └── main.ts
```

`plugin.json`:

```json
{
  "id": "hello",
  "name": "Hello",
  "version": "1.0.0",
  "entry": "main.ts",
  "capabilities": ["log"]
}
```

`main.ts`:

```ts
const enc = new TextEncoder();
const dec = new TextDecoder();

let nextId = 1;
const pending = new Map<number, (msg: any) => void>();

// One reader for the whole process. Cordial's replies and its unsolicited
// pushes arrive on the same stream, and a plugin that stops reading fills the
// pipe, so this loop must never be blocked on anything it is about to receive.
(async () => {
  let buf = "";
  for await (const chunk of Deno.stdin.readable) {
    buf += dec.decode(chunk);
    let i: number;
    while ((i = buf.indexOf("\n")) >= 0) {
      const line = buf.slice(0, i);
      buf = buf.slice(i + 1);
      if (!line.trim()) continue;
      const msg = JSON.parse(line);
      // A push has no `id`; a reply always has one. That is the whole test.
      // This plugin subscribes to nothing, so it drops pushes on purpose —
      // see "The protocol" for what a plugin that wants them does instead.
      if (msg.id === undefined) continue;
      pending.get(msg.id)?.(msg);
      pending.delete(msg.id);
    }
  }
})();

function call(method: string, params: unknown = {}): Promise<any> {
  const id = nextId++;
  const p = new Promise<any>((resolve) => pending.set(id, resolve));
  Deno.stdout.write(enc.encode(JSON.stringify({ id, method, params }) + "\n"));
  return p;
}

await call("log.write", { message: "hello from a plugin" });
```

The third file is the grants file, and it is not yours because it records the
user's decision rather than yours. Until something is granted, this plugin is
not merely refused — it is **never started at all**: `start_all` skips any
plugin whose grant set is empty and prints `plugin hello: no capabilities
granted, not started`. Either flip the `log` switch on the plugin's row in
Settings, or write
`~/.local/share/cordial/profiles/default/plugin-grants.json` by hand:

```json
{ "hello": ["log"] }
```

Grants are per profile
([ADR-013](adr/ADR-013-per-profile-configuration.md)), so approving something
in a profile you made to try it out does not approve it in the one you play on.

The directory name and the manifest `id` should be the same string, and Cordial
does not check that they are. Grants, settings, events and `flags.set` all key
off the manifest `id`; the flag layer Cordial reads back at startup, and the
enablement check that gates it, key off the **directory name**. So a plugin
living in `hello-dev/` while calling itself `hello` will have `flags.set` write
into a `hello/` directory that nothing installed, which the next launch then
reads as a separate plugin. Keep the two identical and none of that can happen.

**Never write to stdout except protocol lines.** `console.log` in Deno writes to
stdout, which is the wire, and a line Cordial cannot parse as a request ends the
conversation: the serving thread prints `plugin hello: sent something
unreadable (…)`, breaks out of its loop and kills the process. Your plugin's
stderr is inherited from Cordial, so `console.error` and Deno's own diagnostics
land in Cordial's output and are the right channel for anything that is not a
request.

## `plugin.json`

Only `id` is genuinely required. Everything else has a default, and the defaults
were chosen so that plugins written before a field existed keep working.

| Key | Required | If it is missing |
|---|---|---|
| `id` | yes | The manifest does not deserialise at all, and discovery reports it as not loadable, quoting serde's own "missing field id". Present but empty is a different refusal, and the one people expect: `id must not be empty`. |
| `name` | no | Empty. Settings and the install prompt show the `id` instead. |
| `entry` | no | The plugin is data, not code. Nothing is spawned and Cordial prints `data only, nothing to start`. |
| `capabilities` | no | Requests nothing. Settings shows `Requests no capabilities`. |
| `version` | no | Loads normally, but cannot be published to an index or depended upon. |
| `dependencies` | no | Depends on nothing. |
| `preferences` | no | No preferences page, and no gear button on the row. |

`id` may contain only ASCII letters, digits, `-` and `_`. That is not
fastidiousness: the id names your installed directory and your settings
directory inside a profile, and it is the key the broker, the grants file and
the event registry all index by, so keeping it to boring characters is what
makes `<profile>/plugins/<id>/settings.json` impossible to talk into being
somewhere else. Anything else is refused with the id quoted back at you.

`entry` is resolved inside your own directory and nowhere else. An absolute path
or one containing `..` parses fine and then fails when Cordial goes to run it,
with `entry "../../../etc/shadow" must be a path inside the plugin directory` —
a manifest arrives with the plugin, so it is treated as input rather than
configuration.

An unrecognised capability name refuses the **whole manifest** —
`unknown capability "process.spawn"` — rather than being skipped. Skipping would
mean a plugin built against a newer Cordial appears to install correctly and
then behaves strangely, which is a much worse afternoon than a refusal at load.
The same applies to a malformed `preferences` declaration.

Two things worth knowing about discovery, because both are silent:

- A directory with no `plugin.json` is passed over without comment. A
  `plugin.json` that will not parse is announced —
  `plugin: /path/plugin.json is not loadable (…)` — and discovery continues with
  everything else.
- A directory whose name starts with `.` is not looked at, because that is where
  the installer stages a half-unpacked plugin before renaming it into place.

Keys Cordial does not recognise are ignored rather than refused. Do not lean on
that; nothing guarantees a future key will not collide with one you invented.

## Capability names are not method names

They look alike and they are different vocabularies. `flags.write` is a
*capability* — the thing a user grants and the string that comes back in a
refusal. `flags.set` is the *method* it gates. There is no method called
`flags.write`, and calling one gets you `unknown method "flags.write"` rather
than a permission error, which is a confusing five minutes if you do not know to
expect it.

There are fourteen capabilities in all: `flags.read`, `flags.write`,
`flags.write.dynamic`, `log`, `lifecycle.read`, `presence.set`, `notify.send`,
`url.open`, `assets.override`, `settings.read`, `settings.write`,
`events.declare`, `events.publish`, `events.subscribe`. `log` is easy to
overlook and is the one almost every plugin wants first.

One of those fourteen currently gates nothing you can observe, and it is better
to hear it here than to spend an evening on it:

- **`flags.write.dynamic`** gates `flags.setDynamic`, which always answers
  `{"status":"error","id":N,"message":"flags.setDynamic is not implemented yet"}`.
  That is permanent rather than pending: writing a `DFFlag` into a running engine
  needs the in-process access ADR-001 and ADR-003 rule out, so it is a capability
  whose effect has nowhere to live rather than a gap waiting to be filled.

`lifecycle.read` is the one to read carefully rather than to avoid. It gates the
five core events under `cordial/`, and three of them arrive: `client.launch`,
`engine.version` and `client.shutdown`. `client.ready` and `window.resized` are
published by nothing. See "Core events" below.

## The protocol

Newline-delimited JSON, both directions, chosen because it is debuggable by eye
and by `cat` and needs no shared memory — a shared-memory transport would be the
first step back toward the in-process access ADR-003 rules out.

A **request** is `id`, `method`, and optionally `params`:

```json
{"id":1,"method":"flags.list","params":{}}
```

`id` is yours to allocate and only has to be unique among your own calls in
flight; several may be outstanding at once. Omitting `params` entirely is legal
and reads as `null` on Cordial's side.

A **response** always carries `status` and the `id` it answers, and comes in
exactly three shapes:

```json
{"status":"ok","id":1,"result":[{"key":"DFFlagExample","value":"false","source":"user"}]}
{"status":"denied","id":2,"capability":"flags.write"}
{"status":"error","id":3,"message":"unknown method \"flags.nonsense\""}
```

The `id` is elided nowhere: an `error` carries one too, and the line above is
shown without it only because the JSON is long. On the wire it is
`{"status":"error","id":3,"message":"unknown method \"flags.nonsense\""}`.

`denied` is deliberately not an `error`. A plugin author needs to tell "I was not
allowed" from "it went wrong", and collapsing the two produces bug reports about
the wrong thing. Check `status` before `result`, and report `capability` when
you refuse to carry on — a plugin that says *denied, needs `flags.write`* has
told the user how to fix it, and one that says *could not set flag* has not.

A **push** is a line Cordial sends that answers nothing. It carries `event` and
`payload`, and crucially carries **neither `id` nor `status`**, which is the
entire mechanism by which your dispatcher tells a push from a reply:

```json
{"event":"cordial/init","payload":{"settings":{},"preferences":{}}}
{"event":"tuner/profile-changed","payload":{"slot":2}}
```

The first push you receive is always `cordial/init`, and it arrives before you
have asked for anything. It carries your saved settings document and the user's
answers to any preferences you declared, so the common case — read your
configuration, then start — costs no round trip. Both fields are `null` if you
were not granted `settings.read`; `{}` means you hold the capability and have
saved nothing yet. The two are worth telling apart, because the first means "ask
for the capability" and the second means "you are new here".

**There are three kinds of push**, and `crates/cordial-runtime/src/plugin_host.rs`
has one site for each: the `cordial/init` handshake, another plugin's
`events.publish` forwarded to its subscribers, and a core event offered to every
plugin holding the capability that gates it. The third does not appear in a grep
for a direct push call, because it goes through a per-plugin `Pump` whose own
thread does the write.

Of the five core events, the client publishes three — `cordial/client.launch`,
`cordial/engine.version` and `cordial/client.shutdown` — and two,
`cordial/client.ready` and `cordial/window.resized`, are published by nothing. If
you are designing around lifecycle events, design around those three.

`cordial/` is reserved. The event registry refuses to let any plugin declare a
type under that prefix, so a line arriving with it is one Cordial published and
no subscriber has to wonder.

**Do not `await` your push handler from inside the read loop.** If handling a
push makes calls of its own, their replies arrive on the same stdin loop you are
blocking, and the plugin deadlocks against itself on the first event.
`plugins/discord-presence/main.ts` has the comment that was written after
exactly that.

### A worked exchange

Here is a whole session for a plugin granted `flags.read` and `log` and not
`flags.write` — one line each way, in the order they cross the pipe:

```
← {"event":"cordial/init","payload":{"settings":null,"preferences":null}}

→ {"id":1,"method":"flags.list","params":{}}
← {"status":"ok","id":1,"result":[
     {"key":"DFFlagRbxTransportUseRtcioRna","value":"false","source":"user"},
     {"key":"FIntTaskSchedulerAutoThreadLimit","value":"8","source":"plugin:tuner"}]}

→ {"id":2,"method":"log.write","params":{"message":"2 flag override(s) in effect"}}
← {"status":"ok","id":2,"result":null}

→ {"id":3,"method":"flags.set","params":{"values":{"FFlagAnything":"true"}}}
← {"status":"denied","id":3,"capability":"flags.write"}
```

The handshake carries `null` for both fields because this plugin was not granted
`settings.read`. The last exchange is refused before the parameters are looked at
at all — authorisation happens ahead of dispatch, so a `flags.set` you were not
granted is denied whatever shape you sent.

`flags.list` returns an array of `{key, value, source}`, where `source` is one of
`user`, `built-in`, `performance mode` or `plugin:<id>`. `log.write` takes
`{message}` and prints it into Cordial's own output. `flags.set` takes a
`values` object of key to value and replaces your plugin's whole flag document —
it is a replace rather than a merge so you have a way to withdraw a flag you have
stopped wanting — and it takes effect at the **next** launch, because
`FFlag`/`FInt`/`FString` are read once at startup (ADR-005). That is the entire
reason `flags.write` and `flags.write.dynamic` are two capabilities rather than
one.

`plugins/flag-inspector/` is the shipped example of this shape, and
`crates/cordial-plugins/tests/flag_inspector.rs` drives that real plugin against
a real broker with real flag data, including the refusal — the inspector asks for
`flags.read` and deliberately not `flags.write` so a denial is demonstrable
rather than hypothetical. (The test skips itself when `deno` is not installed,
so a green run is not by itself proof it executed.)

### Two mistakes worth designing against

Both are easy to make, neither announces itself, and the second is the more
expensive because it looks like the plugin working.

**A dispatcher that does not separate a push from a reply discards every
event.** The common shape is a read loop that hands every line straight to
`pending.get(msg.id)`. A push has no `id`, so that lookup is
`pending.get(undefined)`, which never matches and never throws — the event is
dropped in silence, and the plugin looks like it was simply never subscribed.
Branch on `msg.id === undefined` first, the way the smallest plugin above does
and the way `crates/cordial-plugins/tests/fixtures/settings.ts` and
`fixtures/events_subscriber.ts` both write out in full. Anything that reads one
line per call — `fixtures/roundtrip.ts` is the deliberate example — is a request
and response test rather than a dispatcher, and copying its loop into a plugin
that subscribes to anything will not work.

**`console.log` destroys the protocol.** Deno writes it to stdout, stdout is the
wire, and the first line Cordial cannot parse as a request ends the session with
`plugin hello: sent something unreadable (…)` and kills the process. This is not
a warning that scrolls past; the plugin stops. `console.error` goes to stderr,
which Cordial inherits, and is the right channel for anything that is not a
request. If you want the message in Cordial's own log alongside the startup
lines, that is what `log.write` is for.

Two smaller shapes to check when a call quietly does nothing. A method name
taken from the capability list is not a method — `settings.read` is a
capability, `settings.get` is the method, and it takes no parameters at all
because Cordial already knows which process is on the other end of the pipe. And
`flags.set` wants `{"values": {…}}`; hand it `{key, value}` and the answer is
`flags.set needs a values object`, which is an `error` and not a `denied`, so a
plugin only checking for denials will read it as success it never got.

## The sandbox: what Deno permissions a plugin gets

**None. Not a reduced set — none.** Cordial runs
`deno run --no-prompt --quiet <entry>` and passes no `--allow-*` flag of any
kind, which is asserted by a test that fails if one ever appears. So there is no
file access, no network access, no environment access and no subprocess access.
The only thing your plugin can reach is the pipe.

`--no-prompt` earns its place beside the absent flags. Without it Deno would
*ask* for a permission on first use, and a plugin host has nobody to ask; the
plugin would hang on a prompt nothing will ever answer. With it, touching the
filesystem throws immediately, which is a failure you can catch and report.

Where the host can enforce one, there is a third layer underneath. On a host
install with `bwrap` present, the Deno process runs inside a bubblewrap sandbox
with `--unshare-all --die-with-parent --new-session`, a read-only `/usr`,
`/lib`, `/lib64`, `/bin` and `/etc/ssl`, a private empty `/tmp`, `HOME` and
`DENO_DIR` pointed into that tmpfs, and your entry module — that one file, not
your plugin's directory — bound read-only at `/plugin/entry.ts`. There is no
network namespace to reach anything through.

**It does not follow that nothing of the user's home is visible, and this is the
part to get right if you are reasoning about what a plugin could read.** The
sandbox has to make the interpreter resolvable, so it binds `deno` and the prefix
that contains its libraries, read-only: the parent of `Cellar` for a Homebrew
install, otherwise the parent of the `bin` directory the binary sits in, plus the
parent of a `PATH` entry that turned out to be a symlink. For a distribution
package that is `/usr` and nothing new. For the ordinary user-local installs it
is inside `$HOME` — `~/.deno/bin/deno` binds `~/.deno`, and `~/.local/bin/deno`
binds `~/.local`, which contains `~/.local/share/cordial/profiles/`. Nothing in
the code caps the bind to outside the home directory. The Deno permission layer
is still what stops a plugin opening any of it, so this is a reason to be precise
rather than a reason to be alarmed — but "the OS layer hides your home" is not a
sentence to rely on, and where `deno` came from decides how much is there.

A **Flatpak install deliberately gets no OS layer**. Reaching
`flatpak-spawn --sandbox` requires `--talk-name=org.freedesktop.Flatpak`, and
that same name also grants `--host`, which is arbitrary command execution
outside the sandbox — adding a sandbox escape to Cordial in order to sandbox
plugins is a net loss however it is framed
([ADR-018](adr/ADR-018-plugin-sub-sandboxing.md)).

Its absence is a downgrade rather than a hole, and that is the only reason it is
optional: with `bwrap` missing the plugin still has zero Deno permissions and
still reaches nothing except through the broker. What is not acceptable is
running without the layer and implying it is there, so Cordial prints which one
you actually got, at spawn, every time:

```
[plugin] hello: bwrap + Deno permissions + broker
[plugin] hello: Deno permissions + broker (no OS sandbox available on this install)
```

If you are reasoning about what a plugin could do on someone's machine, that
line is the fact, not the packaging.

One consequence of all this: **`deno` must be on `PATH`, and Cordial packages
none.** A missing interpreter shows up as `plugin hello: could not start (…)`
and nothing else; the client carries on without you, because a plugin that will
not start is never allowed to stop the client.

## How it runs

Plugins are started by the client process (`cordial-run`), not by the shell, and
not until the engine is already up — so that they observe a client that is
running and so a plugin that misbehaves cannot interfere with bring-up.

For each directory found under the plugin root, in sorted order:

1. **Enabled?** If the profile's `plugin-enabled.json` says no, it prints
   `plugin hello: disabled in Settings, not started` and moves on. Absence from
   that file means enabled, with two exceptions. Plugins that ship with Cordial
   may be listed as starting switched off — currently `fps-flex` — so that
   nothing changes how a machine behaves because somebody installed Cordial. And
   the same file carries a **master switch under the key `*`**: with
   `{"*": false}` in it, every plugin is disabled regardless of its own row, or
   its absence from the file. If a plugin will not start and you cannot find a
   row for it, look for that key before looking anywhere else.
2. **Has code?** No `entry` means `plugin hello: data only, nothing to start`.
3. **Granted anything?** Capabilities requested but not granted are named out
   loud — `plugin hello: not granted flags.write` — because a plugin silently
   doing less than it asked for is otherwise indistinguishable from a broken one.
   An empty grant set stops here.
4. **Spawn.** The sandbox line, then the `cordial/init` handshake, then
   `plugin hello: started`. Once every plugin has been considered, the client
   prints `N plugin(s) running` — but only if at least one did. Nothing starting
   prints nothing at all, which is the ordinary case while you are still getting
   a grant in place, so an absent line is not evidence that discovery ran.

Each running plugin gets a thread of its own, blocking on its own stdout. Ids are
unique across the whole root: if two directories claim the same one, the first in
sorted order wins and the second is reported and skipped, because grants, event
namespaces and settings directories are all keyed by id and the second claimant
would otherwise inherit the first's approvals.

Two details that cost time if you do not know them. The grants file is the
authority, not your manifest — the broker is built from what the file says, and
Settings only limits itself to offering switches for capabilities you actually
requested. And `start_all` discovers only under the **user** plugin root; the
system root where first-party plugins ship is read for static flag layers,
asset overlays and the Settings list, but a plugin installed only there is not
spawned — **INFERRED** from that call site rather than from a run. If you are
developing something that will eventually ship with Cordial, develop it in the
user root.

## Testing during development, without packaging

You do not need an archive, an index, or an install step. Cordial discovers
plugins from a directory, so put yours in one it is looking at — copy it, or
symlink it, since discovery follows symlinks to directories.

The tidy way is to give the run its own data root, which redirects both the
plugin root and the profile in one variable and keeps you off the profile you
actually play on:

```bash
export XDG_DATA_HOME=~/.cache/cordial-dev
mkdir -p "$XDG_DATA_HOME/cordial/plugins" "$XDG_DATA_HOME/cordial/profiles/default"
ln -s ~/code/hello "$XDG_DATA_HOME/cordial/plugins/hello"
echo '{"hello":["log"]}' > "$XDG_DATA_HOME/cordial/profiles/default/plugin-grants.json"

cargo run --release --bin cordial-run -- \
  --lib-dir /path/to/lib/x86_64 --apk /path/to/base.apk \
  --host-libc --game-activity --run 60
```

Use a path on real disk rather than `/tmp`, which is tmpfs and comes out of RAM,
and delete it when you are done. `just dev` and `just client` work the same way
with the same variable in front of them.

Two narrower switches exist and are worth knowing, though both are development
switches rather than supported configuration:

| Variable | What it moves |
|---|---|
| `CORDIAL_PLUGIN_DIR` | The plugin root — both discovery and where `flags.set` writes your flag layer. |
| `CORDIAL_PLUGIN_GRANTS` | The grants file, for every profile at once. That is precisely the arrangement per-profile grants exist to end, so use it for a scratch run and not for a machine you play on. |

`--run 60` is worth setting generously the first few times. Plugins start late in
bring-up, after the engine reports itself up, so a short run can exit before your
plugin has said anything. (How short is too short has not been measured here —
INFERRED from where `start_all` sits in the bring-up path, not from timing a run.)

## Seeing its output

There are two channels and they behave differently.

**`log.write` is the one to reach for.** It needs the `log` capability, takes
`{message}`, and appears on the client's own stdout indented and tagged with
your id:

```
  [hello] hello from a plugin
```

Nothing leaves the machine, and it is the same stream every other startup line
goes to, so your plugin's output sits in order beside the grant and spawn lines
above.

**Your stderr is inherited**, so anything you write there — `console.error`, an
uncaught exception, Deno's own complaints about your TypeScript — lands directly
in Cordial's output without passing through the protocol. That is the right place
for a stack trace, and it is the first place to look when a plugin starts and
then goes quiet.

What it is never safe to use is `console.log`. It writes to stdout, stdout is the
protocol, and the first unparseable line ends the plugin's session with
`plugin hello: sent something unreadable (…)`.

Finally, if your plugin subscribes to events and seems to be receiving none, the
first thing to rule out is that there may be none to receive. A plugin event is
delivered by a direct write into your stdin with no queue in between — so if the
plugin you expected to be publishing is not running, or was never granted
`events.publish`, nothing will arrive and nothing will say so. A core event is
different: it is addressed by capability rather than by subscription, so check
that `lifecycle.read` was actually granted in *this* profile, and check which of
the five you are waiting for — `client.ready` and `window.resized` are published
by nothing and will never arrive. See "Core events" below.

## Where to go next

`plugins/README.md` covers versions, dependencies, publishing and the
preferences page. The decisions behind all of it are in `docs/adr/` — ADR-003
for isolation, ADR-007 for brokering, ADR-008 for Deno, ADR-013 for per-profile
grants, ADR-018 for the sub-sandbox, ADR-020 for preferences, ADR-021 for
data-only plugins.

---

# Capabilities and grants

Three things have to be true before a plugin's call does anything: the plugin
asked for the capability in its manifest, you granted it in this profile, and
the broker found the grant when the call arrived. They are three separate acts,
recorded in three separate places, owned by different parties — and keeping them
apart is the whole design. If installing a plugin were enough to grant what it
asked for, the manifest would be a formality and the capability system would be
decorative.

`plugin.json` **requests**. `plugin-grants.json`, inside the profile,
**grants**. `cordial_plugins::broker::Broker` **checks**, on every call, before
any handler runs.

## Default deny

A plugin absent from the grants file gets nothing. A capability it requested but
that you did not grant is refused at the point of use, by name.

This is not a soft default. `grants::load` returns an empty map for a missing
file, and — the part worth knowing — an empty map for a *malformed* one too,
saying so on stdout rather than silently:

    plugin grants: <path> is not usable (<error>); granting nothing

Falling back to "grant what was requested" on a parse error would turn a typo
into a privilege escalation, so a broken grants file denies everything.

Its sibling `plugin-enabled.json` fails in the other direction, though not
quite as either the file or its own comment claims. An unreadable or malformed
enablement file reads as "no opinions recorded" and prints `plugins: <path> is
not usable (<error>); treating every plugin as enabled` — but `is_enabled` then
falls back to `enablement::default_for`, which returns **false** for any id in
`SHIPS_DISABLED`, today `fps-flex`. So a first-party plugin that ships switched
off stays off, and both that message and the module comment above
`enablement::load` ("an enablement file that will not parse enables everything")
are stale in the source as well as in this paragraph's first draft. The
asymmetry they are reaching for is real and is deliberate: the grants file is
the thing that decides what a plugin *can do*, and it is the one that has to
fail closed.

There is no capability meaning "anything", and no grants entry that means "all".
Those are two headers, each giving its own reason. `capability.rs` says the list
is closed and cites ADR-003 — a capability handing over the machine is not a
capability but the absence of one, which is why there is no `process.spawn`, no
filesystem path and no memory access in the enum. `grants.rs` says a "grant
everything" entry would be the one line anybody pastes from a forum.

An unrecognised capability name is an error in both files, and the two errors
have different blast radii. A manifest naming one fails to load with `unknown
capability "process.spawn"`, which affects that plugin. A grants file naming one
grants **nothing to anybody**: `grants::parse` returns `Err` on the first
unknown name and abandons the whole document, so `load` takes exactly the
malformed-file path above with a more specific reason inside the parentheses —

    plugin grants: <path> is not usable (unknown capability "process.spawn" granted to "x"); granting nothing

Skipping the name quietly would mean granting less than you believe you granted,
with no way to tell. This fails harder than that, in the same direction.

## The fourteen capabilities

The list is closed and lives in `crates/cordial-plugins/src/capability.rs`.
Adding one is a design decision, not a convenience: the question is what
*narrow, named effect* a plugin needs, never what access would let it arrange
the effect itself.

| Capability | What it permits | Methods it gates |
|---|---|---|
| `assets.override` | Register a directory whose files resolve ahead of Roblox's own of the same name | `assets.override` |
| `events.declare` | Register event types under the plugin's own id as a namespace | `events.declare` |
| `events.publish` | Broadcast on a type this plugin declared | `events.publish` |
| `events.subscribe` | Receive events, including ones other plugins declared | `events.subscribe` |
| `flags.read` | Read the resolved flag set and which layer set each value | `flags.get`, `flags.list` |
| `flags.write` | Contribute flags that take effect at the next launch | `flags.set` |
| `flags.write.dynamic` | Change a `DFFlag`/`DFInt`/`DFString` while the client runs | `flags.setDynamic` |
| `lifecycle.read` | Hear the core events under `cordial/` | `lifecycle.subscribe` |
| `log` | Write lines into Cordial's own output | `log.write` |
| `notify.send` | Post a desktop notification through the portal | `notify.send` |
| `presence.set` | Publish and clear Discord Rich Presence | `presence.set`, `presence.clear` |
| `settings.read` | Read this plugin's own settings document, and the user's answers to its declared preferences | `settings.get`, `preferences.get` |
| `settings.write` | Replace this plugin's own settings document | `settings.set` |
| `url.open` | Open an `http`/`https` address in the browser | `url.open` |

The mapping is a closed table in `protocol.rs::required_capability`, not a
convention like "`flags.*` needs a flags capability". A typo in a method name
has to fail as *unknown*, not fall through to a check that happens to pass.

One row of that table describes what the capability permits rather than what it
currently delivers, and it is called out below rather than left for you to
discover: **`flags.write.dynamic` reaches a method that is permanently
unimplemented**. `lifecycle.read` is partial rather than absent — it gates five
core events, three of which are published and two of which are not. Everything
else in the table works.

Every capability also carries a second-person sentence,
`Capability::consequence`, which is what the install dialog shows instead of the
wire name. A test asserts that no such sentence contains its own dotted name,
because "this plugin wants `flags.write`, allow?" is accurate, unreadable, and
answered yes by everybody.

### Where the splits are, and why

Four pairs look like they could have been one capability each. Each is split
because collapsing it would let a grant mean more than it read as.

**`flags.write` against `flags.write.dynamic`.** These are two lifetimes, not
two degrees of the same power. `FFlag`/`FInt`/`FString` are read once during
startup, so `flags.set` writes the plugin's own `flags.json` layer and takes
effect at the *next* launch; the `DFFlag`/`DFInt`/`DFString` families are the
only ones that can be changed live. Merging them would produce an API that
accepts calls it cannot honour — the "stub that lies" shape AGENTS.md is about.
See ADR-005.

Read `flags.write`'s consequence sentence before assuming it only touches
Roblox. `flags::write_plugin_layer` does not validate keys, and
`graphics.rs::plugin_request` reads the plugin flag layers deliberately, so any
`Cordial`-prefixed key is in reach — `CordialGraphicsBackend` and
`CordialPresentMode` today, and whatever is added next by construction. A
plugin's request only wins where the user's own Graphics setting is Automatic:
`resolve` returns on an explicit setting without consulting the plugin value.
Note *consulting*, not *reading*. `graphics::choice` calls
`resolve(std::env::var(ENV).ok(), plugin_request())`, so `plugin_request()` —
which runs `flags::resolve(flags::collect())` over every enabled plugin's
`flags.json` — is evaluated as an argument before `resolve` runs at all. The
layer is read either way; the explicit setting only stops it being believed.

**`settings.read` against `settings.write`.** A plugin that only reads its
configuration should not have to be trusted to rewrite it. A user approving
"remember which panel I had open" has not thereby approved "discard everything I
set", and `settings.set` replaces the whole document rather than merging.

`preferences.get` sits under `settings.read` rather than having a capability of
its own. Both are "read what Cordial keeps for you", the data is the plugin's
own by construction, and a separate permission a user could deny would leave a
plugin declaring questions it cannot hear the answers to. There is deliberately
no `preferences.set` at all — those answers are the user's (ADR-020).

**`events.declare` against `events.publish`.** Declaring is what makes a type's
origin a fact the registry checks rather than a claim a plugin makes about
itself, and that check is worth nothing if a plugin can skip straight to
publishing. `EventRegistry::may_publish` returns true only for the plugin that
declared the type; a plugin that could publish on any string it liked could
impersonate another plugin's events and a subscriber would have no way to tell.

**`events.subscribe` against `events.publish`.** Subscribing is deliberately the
broader-reaching of the two — you can subscribe to types other plugins declared
— and it is still the *lesser* power. Hearing that something happened is a
different thing from being believed when you say it did, and a plugin that only
reacts should not have to be trusted to speak.

`presence.set` and `presence.clear` share one capability, going the other way.
Clearing presence is not a lesser power than setting it — both say something
about what the user is doing — so splitting them would only invite a plugin to
ask for two.

### What `lifecycle.read` actually gets you

Three of the five core events, and an acknowledgement that is not what makes
them arrive. That ordering is the honest one, because the grant does the work
the call looks like it is doing.

There are **five** core events in `core_events::ALL`, and `lifecycle.read` gates
all five: `client.launch`, `client.ready`, `client.shutdown`, `engine.version`,
`window.resized`. They sit under the reserved `cordial/` namespace, which
`EventRegistry::declare` refuses to let any plugin declare under, so a
`cordial/…` event a subscriber receives cannot have been minted by another
plugin.

**Three of the five are published by the client; two are published by nothing.**
`crates/cordial-runtime/src/bin/load.rs` calls `plugin_host::publish_core` three
times: `CLIENT_LAUNCH` immediately after `start_all` returns, `ENGINE_VERSION` a
few lines later once the version has been read off `libroblox.so`, and
`CLIENT_SHUTDOWN` on the way out. Nothing anywhere publishes `CLIENT_READY` or
`WINDOW_RESIZED` — each is a `const` and an `ALL` entry and no more. Do not build
a plugin whose behaviour depends on hearing `client.ready`; it will subscribe
successfully and then wait forever.

**The grant is the subscription, not the call.** `plugin_host::publish_core`
looks the event up in `core_events::capability_for`, walks every running
plugin's registered listener, and offers the push to each whose grant contains
that capability — a decision taken at push time, per event, against the grants,
with no subscription list anywhere in it. A plugin that never calls
`lifecycle.subscribe` hears exactly the same events as one that did. An event
absent from the table requires a capability nobody has and therefore reaches
nobody, which is the safe direction and is asserted by a test.

That is also why `lifecycle.subscribe` records nothing: there is no list for it
to join. It returns `Ok` with a `null` result as a definite acknowledgement that
you hold the capability, so a plugin does not have to infer that from the first
push arriving. The comment on that arm in `plugin_host.rs` used to end
"delivery … stays unimplemented rather than silently promised" and now carries
its own retraction of that sentence, kept rather than deleted so that an author
reading only that arm does not conclude a subscription is what makes events
arrive.

`core_events::ALL` is a closed table of name-to-capability, for the same reason
`required_capability` is: an event added without a table entry is delivered to
nobody. The comment on `capability_for` says plainly why the table is per event
family rather than one grant for the whole bus — the events worth adding next
are the ones Cordial is uniquely placed to see, such as which paths the engine
opened or which addresses it connected to, and those are exactly the ones nobody
should receive because they were once granted `lifecycle.read` to show a Discord
status.

## Two things that are not capabilities

A static file a plugin ships is not a request a process is making — it is what
the plugin *is*, and installing and enabling it is the consent. Two consequences
follow, and confusing either with its brokered cousin will send you looking for a
permission that was never involved.

A plugin's own `flags.json` is read by `flags::collect` for every enabled
plugin, with no capability check at all. `flags.write` gates a *running* plugin
rewriting that file through `flags.set`.

A plugin's own `overlay/` directory is registered by `register_static_overlays`
for every enabled plugin, likewise with no capability check. `assets.override`
gates a *running* plugin asking Cordial to register a directory of its choosing
at runtime. Without that distinction a texture pack — which has no entry module
and therefore no process — could not overlay anything at all, and would need a
permission prompt for a process it does not have. ADR-021 and ADR-010.

## What a refusal looks like on the wire

`host::authorise` runs before dispatch and returns the refusal itself, so a
handler receives an already-authorised request or nothing. Its type is
`Result<(), Response>`, and it constructs exactly two responses: an **error**
for a method that does not exist, and a **denial** for a capability you were not
granted. The success arm carries no response at all — the `ok` you eventually
see is the handler's, produced later. Three wire shapes, then, of which
`authorise` accounts for two.

A call for a capability you were not granted comes back **denied**, and the
response names the missing capability:

```json
{"status":"denied","id":3,"capability":"flags.write"}
```

A call to a method that does not exist comes back as an **error**, not a denial:

```json
{"status":"error","id":4,"message":"unknown method \"flags.nonsense\""}
```

And a successful call, from the handler:

```json
{"status":"ok","id":5,"result":[]}
```

`denied` is a distinct status from `error` on purpose. A plugin author needs to
tell "I was not allowed" from "it went wrong", and collapsing them produces bug
reports about the wrong thing — an author who sees an error for a missing
permission goes hunting through their own code, and one who sees a denial for a
typo goes hunting for a capability that was never the problem.

`flags.nonsense` and `flags.delete_everything` are not methods; both appear in
the source only as fixtures for that distinction, and they exercise different
halves of it. `flags.nonsense` is the one tested against the broker: `host.rs`
grants `Capability::all()` before calling `authorise`, so the unknown-method
path is shown to stay an error even for a plugin holding every capability there
is. It appears a second time in `tests/fixtures/roundtrip.ts`, where the plugin
holds only `flags.read` and `log`. `flags.delete_everything` appears once, in
`protocol.rs`'s `an_unknown_method_maps_to_no_capability`, which tests
`required_capability` alone — no broker, no plugin and no grants are involved.

Note the capability field carries the **wire name of the capability**, not the
method. `flags.set` denies as `flags.write`. That is what
`plugins/flag-inspector/main.ts` prints in its last three lines, which exist
precisely so the refusal is visible in a real run and not only in a test:

```ts
const refused = await call("flags.set", { values: { FFlagAnything: "true" } });
await log(`writing a flag came back: ${refused.status}` +
  (refused.capability ? ` (needs ${refused.capability})` : ""));
```

A denial is also recorded, not merely returned. `Broker::allows` pushes a
`Denial { plugin, capability }` onto a list every time it refuses, because a
plugin quietly failing for want of a capability is otherwise indistinguishable
from a plugin that is broken.

### A third outcome: authorised and still refused

Passing the broker is not the same as reaching an effect, and two of these are
worth knowing before you build on them.

**`flags.setDynamic` is not implemented, and is not waiting to be.** Granting
`flags.write.dynamic` gets you past the broker and then straight into the
runtime dispatcher's catch-all:

```json
{"status":"error","id":6,"message":"flags.setDynamic is not implemented yet"}
```

The comment on that arm is explicit that this one lands there *permanently*
rather than temporarily: a live write would mean reaching into the running
engine's own `DFFlag` table, and ADR-001 and ADR-003 rule out the in-process
access that would take. It is not a gap waiting to be filled; it is a capability
whose effect has nowhere to live. Plan around it. (The `plugins/README.md`
sentence describing `flags.write.dynamic` as changing a flag while the client
runs describes the intent, not the current behaviour.)

**Granted calls can still fail honestly.** `url.open` past the capability gate
still validates the scheme and refuses anything that is not `http` or `https`,
so the capability cannot become `file://` traversal or a handler hijack.
`assets.override` defaults its `dir` parameter to `"overlay"`, resolves it
within the plugin's own installed directory, and refuses an absolute path or one
containing `..`, treating it as attacker-controlled exactly as a manifest's
`entry` is treated; `{"clear": true}` unregisters again, and the registration is
torn down unconditionally when the plugin's thread ends, so a disabled or
removed plugin's overlay never outlives it. `presence.set` with no Discord
running returns an error rather than reporting a success it did not have.

If you are driving `cordial_plugins::host::Session` directly — in that crate's
own tests, rather than in the client — a granted method it has no broker for
answers `no broker wired for "flags.set"` instead. `Session` implements
`presence.*`, `notify.send`, `url.open`, `settings.*`, `preferences.get`,
`events.*` and the `lifecycle.subscribe` acknowledgement; `flags.*`, `log.write`
and `assets.override` are only authorised there, and the real client host in
`cordial-runtime`'s `plugin_host.rs` is what serves them. `Session`'s own doc
comment omits `preferences.get` from that list and is wrong to; the match arm
builds a `preferences::Store` from the settings store's profile directory and
calls `preferences::serve`.

## Grants are per profile, not per machine

The file is `<profile>/plugin-grants.json`:

```json
{
  "flag-inspector": ["flags.read", "log"],
  "themer": ["log"]
}
```

Plugin *code* is installed once for the machine; what a plugin is allowed to do
belongs to the account. Approving something in a profile you made to try it out
does not approve it in the profile you actually play on, and that is a security
property rather than tidiness. Grants used to live at
`~/.config/cordial/plugin-grants.json` — one list, every account — which meant an
approval given in a throwaway profile silently held against the account with the
purchases and the friends list, and nothing about approving it there ever
suggested it would apply here. ADR-003's default deny is only worth something if
the thing being denied is the thing the user was asked about.

A pre-existing global file is **moved** into whichever profile first looks for
one — in practice `default` — and every other profile starts at default deny.
Moved rather than copied: copying would faithfully rebuild the global allow-list
the change exists to remove. The migration is skipped entirely if the profile
already has its own file, so it can never widen approvals you have already made,
and a failed move leaves the old file untouched rather than writing half a
document.

`CORDIAL_PLUGIN_GRANTS` overrides the path outright. It is global by nature — it
makes one grants file serve every profile, which is the arrangement per-profile
grants exist to end — so treat it as a development switch, not a supported
configuration.

## Granting, revoking, and the difference from switching off

The install prompt is all-or-nothing. `consent::verdict` decides whether to ask
at all: a plugin with no entry module *and* no capabilities installs silently,
because if every import prompts then the prompt means nothing by the third one.
Anything with code or capabilities gets a dialog listing each capability's
consequence sentence, and pressing Allow writes **every requested capability**
into the grants file at once. "Not now" is both the default response and the
close response, so dismissing the dialog with Escape grants nothing.

Allowing is still not starting. `consent::starts_disabled` returns whether the
plugin has an entry module, and the shell writes a plugin with code into
`plugin-enabled.json` as **off** whatever you told the dialog — the success
subtitle says so: "Installed {name} and allowed what it asked for. It is
switched off until you turn it on." An install dialog's OK button granting the
capabilities *and* starting the process would be one act where there should be
two. Data-only plugins are left absent from the file and therefore on, because
there is nothing to start and a switch with no argument behind it is not a
choice.

Per-capability control comes afterwards, on the plugin's row in Settings: one
switch per capability the plugin requested, each writing through `grants::set`,
which flips one entry and leaves every other plugin — and every other capability
of this one — alone. Revoking a plugin's last capability drops its key from the
file entirely rather than leaving `"id": []`, since an empty set and an absent
key mean the same thing to both `load` and the broker.

**Disabling is not revoking.** `plugin-enabled.json` sits beside the grants file
and answers a different question: is this thing running, as against what it is
allowed to do. Grants survive a disable untouched, so switching a plugin off for
an afternoon costs nothing to undo. Conflating them would mean the price of
turning something off is every approval decision you already made, and the likely
response to that price is leaving a suspect plugin enabled. The Settings subtitle
says so out loud, when there is anything to say it about: `capability_summary`
returns "Off. What you allowed it to do is kept." for a disabled plugin that
holds at least one granted capability, and the bare "Off" for one that holds
none — the sentence exists to answer a fear about losing approvals, and a plugin
with no approvals has none to lose.

Two states look identical from outside and are not, so Cordial reports them
differently. A plugin with code that has been granted nothing is not started at
all, and says `no capabilities granted, not started`; a plugin the user switched
off says `disabled in Settings, not started`. One is "you have not decided what
to allow", the other is "you switched it off", and a plugin installed, enabled,
and never granted anything has been reported as broken by somebody looking at
ADR-003's default deny working exactly as intended.

Whatever is withheld is named at startup, too:

    plugin <id>: not granted flags.write, presence.set

because a plugin silently doing less than it asked for is otherwise
indistinguishable from a plugin that is broken.

One thing the handshake does *not* do is enumerate your grants. `cordial/init`
carries `settings` and `preferences`, and each is `null` when `settings.read`
was not granted and `{}` when it was granted but nothing is saved yet — the two
are told apart deliberately, so a first launch is distinguishable from a missing
capability. (`settings` is also `null` when the session has no profile at all,
or when the read itself failed; each of those prints a line saying which.) There
is no "here is what you hold" field; you learn what you hold by calling and
reading the status.

**The grants file is authoritative, and it is not intersected with the
manifest.** `start_all` reads this profile's entry for the plugin id and hands it
straight to `Broker::grant`; the manifest's request list is used only to compute
the "not granted" message and to decide which switches Settings draws. A
capability written into the grants file by hand that the manifest never requested
is therefore granted at runtime. **INFERRED**: that follows from the code path
rather than from an experiment — no client was run to observe it — but there is
no intersection anywhere between `approved.get(&id)` and `plugin.requested`.

## The boundary: effects, never channels

A plugin never receives a socket, a file descriptor, or a D-Bus connection.
Where a capability needs a host resource, Cordial holds the permission and
performs the effect; the plugin sends a payload describing what it wants. This
is ADR-007, and it is not a stylistic preference.

Discord Rich Presence is the worked example. `presence.set` takes a presence
structure. Cordial owns the connection to Discord's IPC socket — the plugin never
learns where it is, cannot read Discord's state, and cannot send anything else
down it. `notify.send` and `url.open` are the same shape over D-Bus and the
portal: a plugin holding the bus could talk to every other service on it.

Two reasons this cannot be relaxed. First, Flatpak permissions and plugin
capabilities have incompatible lifetimes — a Flatpak permission is static,
app-wide, and granted at install; a capability is dynamic, per-plugin, and
revocable. If installing a plugin could add a permission, uninstalling it would
not take the permission away, and one demanding plugin would permanently widen
the sandbox for every other plugin and for Cordial itself. Second, the Deno host
already makes it impossible: plugins run with **no permissions at all** — no
file, network, environment or subprocess access, with `--no-prompt` so an
attempted access fails immediately rather than hanging on a prompt nothing will
answer. A plugin *cannot* open a socket even if Cordial wanted it to. Where
`bwrap` is available a third layer is applied under those two, and its absence is
printed rather than assumed, because a layer nobody can tell is missing is one
nobody notices went away (ADR-018).

A generic `host.socket.connect` capability was considered and rejected in
ADR-007 as the whole decision undone. So was per-plugin Flatpak sub-sandboxing,
on the grounds that process isolation already provides the containment and the
missing piece was only ever who holds the host resource.

### If you need something the capabilities do not cover

There is no escape hatch, and that is the design rather than an oversight. A
resource Cordial does not already broker needs a change to **Cordial** — read,
reviewed, released — not a change to your manifest. It is slower on purpose: the
Flatpak manifest is the one place a user or a packager can read the whole
sandbox, and it has to stay true.

So open an issue. A broker is a payload type and an effect, which makes adding
one a small change rather than a redesign — and that is also the test. **If a
proposed broker cannot be small, the capability is too broad and wants
splitting.** ADR-007's own rule for every future broker is: expose the *effect*,
never the *channel*.

Some things will not be added at any size. There is no script execution against
the Roblox process, no memory access, and no API by which a plugin could request
one — absent from the surface rather than disabled, so there is no primitive to
extract or re-enable in a fork (ADR-001, ADR-003).

There is no general UI surface either: a plugin has no display and no toolkit,
and one able to draw in Cordial's window could draw something indistinguishable
from Cordial's own sign-in dialog. What exists instead is `notify.send` for
notifications, `assets.override` for replacing textures, sounds, fonts and
models — its consequence sentence is deliberately that wide, and adds that
replacing a model can change more than appearance and Cordial does not check
which is which — and the declarative preferences page Cordial draws from your
manifest (ADR-020).

If you grep `docs/adr/` you will find ADR-027, "Plugins describe an overlay;
Cordial draws it", proposing exactly the surface this section says does not
exist, with three separate capabilities `ui.notify`, `ui.hud` and `ui.panel`
split by what an overlay costs the player. Its status is **proposed**, and none
of those three names exists in `capability.rs`. Read it as a design under
discussion, not as something to call.

---

# Events

There are **two** event buses. They share the wire format and nothing else.
**Core events** are Cordial reporting what it observed at the platform
boundary; **plugin events** are plugins talking to each other. They have
different names, different authorisation, different delivery guarantees, and
neither is reachable through the other's API. Confusing them is the first
mistake available here, so they are kept apart below.

Both arrive as a `Push` — the one message shape that is not a reply:

```json
{"event":"cordial/client.shutdown","payload":null}
```

A `Push` carries neither `id` nor `status`; a `Response` always carries both.
That is the whole of how a plugin tells an event from an answer to one of its
own calls, and `crates/cordial-plugins/src/protocol.rs` has a test asserting
`Push` never grows either field, precisely so this stays true.

The handshake uses the same shape. Every plugin is sent
`{"event":"cordial/init","payload":{"settings":…,"preferences":…}}` before it
has asked for anything. It is not an event anyone published and no capability
gates it — match on the name and ignore it, or you will report it as an
unrecognised event, which every example plugin in this repository did until it
did not.

## Core events: the closed table

`crates/cordial-plugins/src/core_events.rs` holds the entire vocabulary. Five
entries, each a `&'static str` from that file, each gated by a capability from
the same row.

**The client publishes three of the five.** The other two are in the table so
that adding a publisher later is a one-line change that cannot forget its
capability; they are not a feature yet, and a plugin that waits for one waits
forever.

| wire name | capability | payload | published by the client |
|---|---|---|---|
| `cordial/client.launch` | `lifecycle.read` | `{"profile":…}` | yes |
| `cordial/client.ready` | `lifecycle.read` | — | no |
| `cordial/client.shutdown` | `lifecycle.read` | `null` | yes |
| `cordial/engine.version` | `lifecycle.read` | `{"version":…}` | yes |
| `cordial/window.resized` | `lifecycle.read` | — | no |

All three publish sites are in `crates/cordial-runtime/src/bin/load.rs`.
`client.launch` goes out immediately after `start_all` returns, which is the
first moment there is anybody to tell rather than the moment the client
launched. `engine.version` follows once the version has been read off
`libroblox.so`; an unreadable version publishes nothing at all and prints

```
  plugins: engine version not readable, so cordial/engine.version is not published
```

rather than inventing one. `client.shutdown` is the last thing any plugin is
told and is followed by a bounded flush — see "What happens when a subscriber is
slow". `CLIENT_READY` and `WINDOW_RESIZED` have no constructor at all: each
appears at its own `const` declaration and in the `ALL` table, and nothing
anywhere builds a `CoreEvent` from either.

**Two more things about that table are worth saying out loud, because a list of
event names read anywhere else will get them wrong.**

**Two of the three published events carry a payload; one does not.**
`client.launch` carries `{"profile": …}` — the active profile's *name*, not its
path, and `null` if that path has no final component. ADR-007's rule that a
plugin gets the effect rather than the channel reads the same way here: a plugin
may reasonably key what it remembers by which profile is running, and has no
business learning where the user's home directory is. `engine.version` carries
`{"version": …}`. `client.shutdown` carries `null`, because the name is the
whole of the information.

What no core event carries is what is being *played* — not which place is
running, not which user is signed in. The shipped `discord-presence` plugin says
so in its own source and shows generic text for exactly this reason. So read
`profile` and `version` where they are offered, and write the handler so that a
missing field is not an error rather than assuming every core event carries a
document.

**There is no `network.connected`.** If you have seen it in a list of core
events, that list was reading `core_events.rs`'s *negative* test —
`assert_eq!(capability_for("network.connected"), None)` — which exists to prove
that a name absent from the table reaches nobody. It is the counter-example,
not an entry.

That last property is the design and not an accident. `capability_for` returns
`Option<Capability>`, and `publish_core` treats `None` as "delivered to no
one", printing

```
  plugin core event "…" is not in the capability table, so nobody receives it
```

That is the runtime host's wording. `cordial_plugins::host::Session` — the other
host, which nothing the client runs reaches — has the same behaviour and prints
`[plugin] core event "…" has no capability in the table, so nobody receives it`
instead, so match on the shape rather than the string if you are unsure which
one produced a log you are reading.

An event somebody adds and forgets to gate therefore fails closed. A closed
table rather than a `cordial/*` prefix convention, for the same reason
`required_capability` is a closed mapping of methods: a typo has to fail as
unknown rather than fall through to a check that happens to pass.

## Two hosts, and only one of them runs

**This repository contains two plugin hosts, and reading the wrong one is the
easiest mistake available here.** `crates/cordial-runtime/src/plugin_host.rs`
is the host `cordial-run` starts, and it has its own `publish_core`, its own
per-plugin `Pump`, its own `flush_core_events` and its own
`dropped_core_events`. `cordial_plugins::host::Session` is the same shape and is
constructed nowhere outside `cordial-plugins`' own tests — three in
`crates/cordial-plugins/tests/` and the rest inside `host.rs`'s own
`#[cfg(test)] mod tests`. Where this document quotes a printed message or a
limit it is the runtime host's unless it says otherwise, because that is the one
whose output you will be reading.

`lifecycle.subscribe` returns `{"status":"ok","id":N,"result":null}` and
confirms that you hold the capability. **It records nothing, and it is not what
makes events arrive**: `publish_core` picks its recipients out of the grants, so
a plugin that never calls it hears the same three events as one that did.

`lifecycle.subscribe` is worth calling anyway. It is the only way to learn that
your grant went through, and the alternative — inferring it from a push —
cannot distinguish "not granted" from "nothing has happened yet".

## There is no subscription to a core event

`events.subscribe` is refused for any `cordial/…` type:

```
"cordial/client.launch" has not been declared by any plugin
```

That is not a special case; it falls out of the design. `EventRegistry` filters
at subscribe time and only knows types some plugin declared, and core events
are never declared into it — `publish_core` picks its recipients by walking
adopted plugins and asking the broker who holds the required capability. The
capability *is* the subscription. Ask for `lifecycle.read` in your manifest, be
granted it, and the pushes are addressed to you.

The reserved owner closes the other door. `crates/cordial-plugins/src/events.rs`
refuses `declare` from a plugin whose id is `cordial`:

```
"cordial" is reserved for Cordial's own events; a plugin may not declare under it
```

so no plugin can mint a convincing `cordial/…` event, and a subscriber never
has to wonder whether the one it received was real.

## A core event may be observed and never vetoed, delayed or altered

ADR-026 states the rule; the enforcement is structural rather than a check
somebody remembered to write.

**There is no channel to answer on.** Delivery is a `Push`, and a `Push` has no
`id`. A `Response` is only ever matched to a request by its `id`, so a plugin
has nothing to correlate a reply to. `publish_core` returns
`Delivered { sent, dropped }` — two counts for the publisher — and does not
read the plugin's stdout at all. There is no return value to make meaningful,
which is a stronger guarantee than a return value that is documented as ignored.

**Nothing waits.** `publish_core` hands each recipient's event to that plugin's
own `Pump` via `offer`, which is a `try_send` on a
`std::sync::mpsc::sync_channel` 256 deep. `try_send` never blocks. A separate
thread per plugin does the actual write into that plugin's stdin. So the
publisher's cost is a queue push, and it does not track how fast — or whether —
the plugin reads.

That matters because a push is a blocking write into a pipe, 64 KiB on Linux,
and the thread publishing a platform event is a thread the client is waiting
on. The engine's looper is measured in millions of polls a second. A bus that
let it queue behind a wedged plugin would be a worse bug than anything it was
built to observe.

**Cordial's own decisions are explicitly outside this rule.** Whether *Cordial*
shows a toast or opens a URL in its web view is not the engine's behaviour and
could sensibly be influenced one day. If that is ever built it gets its own
name and its own ADR, so nobody reaches for it as a way to make platform events
vetoable after all.

## What happens when a subscriber is slow

It misses events, and the loss is counted rather than silent.

- The queue is `QUEUE_DEPTH = 256` pushes deep, per plugin.
- A publish that finds it full **drops the event** and increments that plugin's
  drop counter. The plugin is not told, and there is no sequence number in a
  `Push` from which it could infer a gap.
- `plugin_host::dropped_core_events()` returns `(id, count)` for every plugin
  that lost something, and **the client calls it at exit**, printing
  `  plugin <id>: <n> core event(s) dropped, its queue was full` for each. A
  plugin that has already exited is not listed — its listener went with it — so
  "no drops reported" is a weaker claim than it looks for a plugin that died
  mid-run. (`Session::dropped_by_plugin()` is the same thing on the host nobody
  runs, and has no non-test caller.)
- `plugin_host::flush_core_events(limit)` waits, bounded, for queued pushes to
  reach every plugin. `limit` is applied **per plugin**, not to the call as a
  whole, so the worst case is `limit` multiplied by the number of running
  plugins. The client calls it with 500 ms immediately after publishing
  `client.shutdown`: delivery is asynchronous, so a publish followed by an exit
  is a race the exit wins, and that is the one event whose whole point is being
  last. It returns `false` if a deadline passed first, and the client then
  prints `  plugins: a plugin did not read its queue within 500 ms; exiting
  without it`, because a plugin that stopped reading must not be able to hold up
  the exit.

Measured, in `host.rs`'s own test: 4000 events of 4 KiB each published in about
6 ms, 3735 of them dropped and counted, with the consumer far behind. That test
is honest about what it does *not* prove — the fixture process exits rather
than wedging, so what is demonstrated is that the publisher's cost does not
track the reader's speed, not that a genuinely wedged consumer was survived.

**Write a plugin that expects to miss things.** A plugin that counts will
undercount under load. A plugin needing exactly-once delivery cannot have it
here and should be asking Cordial for state instead.

## Plugin-declared events

Three methods, three separate capabilities, and the separation is the point.
Unlike the core bus, this one **is** wired up in the host the client runs.

| method | capability | params | `result` on success |
|---|---|---|---|
| `events.declare` | `events.declare` | `{"name": "<bare name>"}` | `{"type": "<your-id>/<bare name>"}` |
| `events.publish` | `events.publish` | `{"type": "<full type>", "payload": <any>}` | `null` |
| `events.subscribe` | `events.subscribe` | `{"type": "<full type>"}` | `null` |

**You supply a bare name; Cordial supplies the namespace.** `declare` builds
the key as `format!("{plugin}/{name}")` where `plugin` is Cordial's own record
of which process is on the other end of the pipe. There is no field in the
request that names a plugin, so there is nothing to set to somebody else's
name. Take the `type` out of the response rather than assembling it yourself.
(The bare name itself is not validated — a name containing a slash gives you
`your-id/a/b`. It still cannot escape your prefix, because a plugin id may only
contain letters, digits, dashes and underscores.)

**Publishing on a type you did not declare is an error, not a denial:**

```
"evil" may not publish on "flag-manager/profile-changed"; it must declare that type before publishing on it
```

`denied` would be wrong here and the distinction is deliberate: the plugin may
well hold `events.publish`, so a denial would send its author looking for a
permission that was never the problem.

**Re-declaring your own type is not a conflict.** A plugin process restarting
must land exactly where it was. Declaring a type another plugin owns is
structurally unreachable, since the key is always prefixed with your own id.

**Subscription is filtered at subscribe time, so you can only subscribe to a
type somebody has already declared.** An undeclared type is refused:

```
"flag-manager/profile-changed" has not been declared by any plugin
```

**And nothing orders the start-up so that the declaration happens first. Read
this before you rely on `dependencies`.** ADR-006 describes dependency-resolved
loading, and `events.rs`'s own module comment leans on it to explain why
subscribe-time filtering is acceptable. **That start ordering is not
implemented.** `plugin_host::start_all` — the only path that starts a plugin in
the running client, called once from `crates/cordial-runtime/src/bin/load.rs` —
iterates `manifest::discover(&manifest::plugin_root())`, which returns plugins
sorted by **directory name** (`dirs.sort()` in `manifest.rs`) and nothing else.
`plugin_host.rs` contains no reference to `dependencies` or `Dependency`
anywhere in the file, and nothing in it reaches `cordial_plugins::resolve`. (The
word `resolve` does appear there, in `crate::flags::resolve` and a local
`resolve_within`; grep for the module rather than the word.) The dependency
planner in
`crates/cordial-plugins/src/resolve.rs` has non-test callers only in
`marketplace.rs` and the settings window's install-confirmation UI: it decides
what gets *installed*, never what starts first. Nothing refuses to start a
plugin whose declared dependency is absent, either, so ADR-006's "must surface
as a named error to the dependent" is unimplemented as well.

So naming a plugin in `dependencies` has no effect on start order, and a
start-up `events.subscribe` on another plugin's type is a race the manifest
cannot influence. Two things actually work:

- **Retry.** Treat `has not been declared by any plugin` as "not yet" rather
  than as a permanent failure, and try again later — on a timer, or when you
  next have reason to.
- **Subscribe late.** Do it at the point you first need the events, by which
  time a plugin that starts earlier in directory-name order has usually
  declared.

Still list a real dependency in `dependencies`: it is what the installer reads,
so it is how the plugin gets onto the machine at all. Just do not read it as a
scheduling promise. (**INFERRED:** `start_all` reads only
`manifest::plugin_root()`, the user root, and never `system_plugin_root()`,
whereas `flags::collect` reads both. On the face of the two call sites a
first-party plugin shipped under the system prefix is never spawned by the
runtime, which would make a first-party publisher unavailable at any time.
Nobody has run the client to confirm the consequence.)

**Subscribing is broader than publishing on purpose.** Hearing that something
happened and being believed when you say it did are different powers. A plugin
that only reacts should not have to be trusted to speak, and one that can
publish must have declared first — which is what makes an event's origin a fact
the registry checks rather than a claim a plugin makes about itself.

**Plugin-to-plugin delivery is not lossy — and that is not automatically good
news.** Unlike core events, `events.publish` writes to each subscriber with a
blocking write rather than through the `Pump`: the real host's `dispatch` calls
`Writer::push`, and `Session::publish` calls `Plugin::push`. Both end at
`Writer::write_line`, which is a `write_all` into a `ChildStdin` with no
timeout. Nothing is dropped and nothing is counted. The trade is the other way
round from the core bus: a subscriber that has stopped reading its stdin can
fill the pipe, and the publisher's `events.publish` call is what waits. Write
failures are ignored, so a subscriber that has *died* costs the publisher
nothing — it is the live-but-not-reading case that is exposed. **INFERRED:** the
stall is readable directly from the code path (a blocking `write_all` into a
full pipe with no timeout); nobody here has produced a wedged subscriber and
measured it, and the equivalent core-bus test explicitly failed to build that
fixture.

## Worked example: a plugin on both buses

A plugin that would hear the client start if anything published it, announces
its own event, and reacts to another plugin's. `flag-manager` below is a
stand-in for a peer plugin — there is no plugin by that name in this
repository — so treat it as the shape rather than something to copy and run.

`plugin.json`:

```json
{
  "id": "session-watch",
  "name": "Session Watch",
  "version": "1.0.0",
  "entry": "main.ts",
  "capabilities": [
    "lifecycle.read",
    "events.declare",
    "events.publish",
    "events.subscribe",
    "log"
  ],
  "dependencies": { "flag-manager": "^1.0.0" }
}
```

`main.ts`:

```ts
const enc = new TextEncoder();
const dec = new TextDecoder();

let nextId = 1;
const pending = new Map<number, (r: any) => void>();

(async () => {
  let buf = "";
  for await (const chunk of Deno.stdin.readable) {
    buf += dec.decode(chunk);
    let i: number;
    while ((i = buf.indexOf("\n")) >= 0) {
      const line = buf.slice(0, i);
      buf = buf.slice(i + 1);
      if (!line.trim()) continue;
      const msg = JSON.parse(line);
      // A push has no id at all; a reply always does. That is the whole of
      // how the two are told apart -- see protocol.rs's Push type.
      //
      // Deliberately not awaited: onPush calls back out, and those replies
      // arrive on this very loop. Awaiting here would deadlock against
      // itself on the first event.
      if (msg.id === undefined) {
        onPush(msg);
      } else {
        pending.get(msg.id)?.(msg);
        pending.delete(msg.id);
      }
    }
  }
})();

function call(method: string, params: unknown = {}): Promise<any> {
  const id = nextId++;
  const p = new Promise<any>((resolve) => pending.set(id, resolve));
  Deno.stdout.write(enc.encode(JSON.stringify({ id, method, params }) + "\n"));
  return p;
}

const log = (message: string) => call("log.write", { message });

// Core events are namespaced `cordial/`, which is reserved -- a name arriving
// with that prefix is one Cordial published, and no plugin can mint one.
//
// `client.launch` and `client.shutdown` are published by the client.
// `client.ready` is published by nothing at all, so the branch below that
// mentions it is written for the day something does rather than for today --
// `client.launch` is what actually marks the start of a session.
const LAUNCH = "cordial/client.launch";
const READY = "cordial/client.ready";
const SHUTDOWN = "cordial/client.shutdown";

// Filled in by declare(); never assembled by hand, because the namespace is
// Cordial's to supply.
let MY_EVENT = "";

async function onPush(push: { event: string; payload: unknown }) {
  // The handshake, sent before this plugin asked for anything. Not an event.
  if (push.event === "cordial/init") return;

  if ((push.event === LAUNCH || push.event === READY) && MY_EVENT) {
    // `client.launch` carries {"profile": name}. `client.ready` carries
    // nothing, and would still carry nothing if it were ever published. So
    // read the field where it is there and never require it.
    const profile =
      (push.payload as { profile?: string | null } | null)?.profile ?? null;
    await call("events.publish", {
      type: MY_EVENT,
      payload: { at: Math.floor(Date.now() / 1000), profile },
    });
  } else if (push.event === SHUTDOWN) {
    await log("client is going away");
  } else {
    // Another plugin's event, or one this plugin has not been taught yet.
    await log(`heard ${push.event} ${JSON.stringify(push.payload)}`);
  }
}

// Confirms the grant, and that is all it does. Delivery is gated on the
// capability rather than on this call -- a plugin holding lifecycle.read hears
// the core events whether or not it ever subscribes -- so an ok here means
// "you hold lifecycle.read" and nothing beyond that.
const life = await call("lifecycle.subscribe");
if (life.status !== "ok") {
  await log(
    `no lifecycle events: ${life.status}` +
      (life.capability ? ` (needs ${life.capability})` : ""),
  );
}

// Declare before publishing, and take the namespaced type from the answer.
const declared = await call("events.declare", { name: "session-started" });
if (declared.status === "ok") {
  MY_EVENT = declared.result.type;          // "session-watch/session-started"
  await log(`declared ${MY_EVENT}`);
} else {
  await log(
    `could not declare: ${declared.status}` +
      (declared.capability ? ` (needs ${declared.capability})` : ""),
  );
}

// Somebody else's type. Nothing starts plugins in dependency order, so the
// publisher may simply not be up yet -- "has not been declared by any plugin"
// means "not yet", not "never". Retrying is the only thing that works;
// listing flag-manager in `dependencies` gets it installed and does not get
// it started first.
async function subscribeWhenAvailable(type: string, attempts = 10) {
  for (let n = 0; n < attempts; n++) {
    const sub = await call("events.subscribe", { type });
    if (sub.status === "ok") return true;
    if (sub.status === "denied") {
      // A permission problem, which no amount of waiting fixes.
      await log(`cannot subscribe to ${type} (needs ${sub.capability})`);
      return false;
    }
    await new Promise((r) => setTimeout(r, 500));
  }
  await log(`${type} was never declared; giving up`);
  return false;
}

await subscribeWhenAvailable("flag-manager/profile-changed");
```

---

# FastFlags

FastFlags reach the engine through a layered resolver in
`crates/cordial-runtime/src/flags.rs`. A plugin gets a read of the resolved set
with its provenance, and a write of one layer — its own.

## The layers, and who wins

`flags::collect()` builds the layers in precedence order, lowest first;
`flags::resolve()` applies them in that order, so **the last one to name a key
wins**:

```text
1  built-in          Cordial's own defaults          (currently empty)
2  performance mode  the CORDIAL_PERFORMANCE tables  (empty unless a mode is chosen)
3  plugins           <plugin dir>/<id>/flags.json    one layer per installed,
                                                     enabled plugin, in
                                                     alphabetical order of id
4  user              <profile>/flags.json            wins over everything
```

**The user always wins, and that is the one rule the whole arrangement exists
to protect.** An explicit setting must not be overridable by software the user
installed to do something else. There is no call, no parameter and no ordering
trick by which a plugin can beat layer 4. `fps-flex` says so in its own header:
"It also cannot overrule you."

**Layer 3 is ordered by plugin id, not by which root the plugin came from.**
Cordial has two plugin roots — a read-only system one shipped with the binary
and the user's writable one — and `collect` walks them system-first, but that
pass only decides *which root owns each id*. The layers themselves are pushed
by iterating a `BTreeMap` keyed by id, so the order is global and alphabetical:
a user plugin `zzz` is applied after, and therefore beats, a first-party plugin
`aaa`. The `BTreeMap` is also what makes the outcome independent of directory
iteration order. (The id here is the plugin's *directory* name, which is what
the installer sets to the manifest id; a hand-placed development directory with
a different name shows that name in `source`.)

**Plugin-against-plugin conflicts are reported, not resolved.** Two plugins
wanting one key set differently is a real disagreement, and picking by
filesystem order would hide it. Both are named in the log and, since layers are
applied in order, **the later one alphabetically wins** — deterministic and
visible rather than arbitrary. `flags.rs`'s own test
`two_plugins_disagreeing_is_recorded_not_hidden` resolves plugin `a` then
plugin `b` and asserts that `b`'s value stands, with `a`'s kept on the side.

**A user plugin may not shadow a first-party one.** The system root is read
first and claims the id; a same-id directory in the writable root is skipped
with both paths named. A first-party plugin can be *disabled* per profile, and
a disabled plugin's `flags.json` is not read either — that half used to be
missed, so "off" meant "its code does not run, but its opinions about the
renderer still do".

**Roblox's own client-settings document is not one of these layers.** It is the
base the resolved set is merged *into*, in
`crates/cordial-runtime/src/client_settings.rs`. So `flags.list` shows you what
Cordial's layers set, and never Roblox's default for a key nobody overrode.

## `flags.list` — everything in effect, with provenance

Capability `flags.read`. No params. `result` is an array, one object per
resolved key:

```json
{"status":"ok","id":1,"result":[
  {"key":"DFFlagRbxTransportUseRtcioRna","value":"false","source":"user"},
  {"key":"FIntTaskSchedulerAutoThreadLimit","value":"8","source":"plugin:tuner"}
]}
```

`value` is always a string, because `Resolved::value` is a `String` and there
is no other shape it could arrive in. The two layers read from disk — plugins
and the user — go through `read_layer`, which converts JSON numbers and
booleans to strings because Roblox stores every setting as a string and a
config file should not require knowing that. The other two layers never touch
`read_layer` at all: `builtin_layer()` and `performance_layer()` build their
maps of strings from tables in the code. `source` is one of exactly four
spellings, from `Source::describe()`:

| `source` | meaning |
|---|---|
| `user` | the user's own `<profile>/flags.json` |
| `plugin:<id>` | that plugin's `flags.json` |
| `built-in` | Cordial's own defaults |
| `performance mode` | the flags the chosen `CORDIAL_PERFORMANCE` mode asks for |

The list carries only the *winning* value. The losing values are kept
internally on `Resolved::overridden` and printed by `flags::report` at launch,
but they are not in what a plugin receives.

## `flags.get` — one key

Capability `flags.read`. Params `{"key": "FFlagSomething"}`.

```json
{"status":"ok","id":2,"result":{"value":"true","source":"plugin:tuner"}}
```

A key no layer sets answers **`{"status":"ok","id":2,"result":null}`** — an
`ok` with a null result, not an error. A missing or non-string `key` parameter
is treated as the empty string and so also answers `null`. Check for `null`
before reading `.value`.

## `flags.set` — replace your own layer

Capability `flags.write`. Params are a **`values` object**, not a key and a
value:

```json
{"id":3,"method":"flags.set","params":{"values":{"FFlagFoo":"true","FIntBar":3}}}
```

A missing or non-object `values` is refused with
`"flags.set needs a values object"`. Numbers and booleans are stringified, so
`3` lands as `"3"`; write the string yourself if the exact spelling matters —
Roblox's own document uses `"True"` for booleans and this conversion produces
lowercase `"true"`.

**It replaces the whole document rather than merging**, for the reason
`settings.set` does: a plugin is the only writer of its own `flags.json`, so it
always knows the complete set it means to leave in place, and a merge would
give it no way to withdraw a flag it has stopped wanting overridden. Send
everything you want, every time.

The file is written to a sibling and renamed in, so a process killed mid-write
leaves the previous valid document rather than one the reader has to report and
skip. `write_plugin_layer` has two refusals of its own, and only one of them is
reachable from a plugin call:

```
<n> bytes of flags is more than the 262144 byte limit
```

`<n>` is the length of the serialised document and is always strictly greater
than the limit, because that is the comparison that produces the message. The
other refusal — `"…" is not a usable plugin id` — is defence in depth rather
than something a plugin can provoke: the id it checks is Cordial's own record
of which process is on the pipe, and `manifest::parse` already rejected any
manifest whose id was not letters, digits, dashes and underscores, so a running
plugin cannot have one.

**Where it lands is machine-global, not per profile.** A plugin's `flags.json`
sits beside its installed code at
`~/.local/share/cordial/plugins/<id>/flags.json`, because a plugin is installed
once for the machine. Grants are per profile; this file is not. An installed
plugin therefore contributes its flags in every profile whether or not it was
granted anything there. That asymmetry is real and is recorded as an open
question in ADR-013 rather than being a thing you have misread.

**And it takes effect at the next launch. Only.** See below.

## `flags.write` reaches Cordial's own settings too

Any key beginning `Cordial` rides the same layering — same precedence, same
provenance — and is filtered out before anything reaches Roblox's settings
document, because the engine has no idea those keys exist. That makes
`flags.write` materially wider than "contribute FastFlags", which is why the
consent text a user reads says so:

> Change how Cordial itself renders and behaves. Sets Roblox FastFlags, and
> also Cordial's own settings including the graphics backend and present mode.
> Takes effect at the next launch. Your own choices in Settings still win.

The keys that exist today are `CordialGraphicsBackend`, `CordialPresentMode`
(the one `fps-flex` exists to write) and `CordialDeviceProfile`. Every future
`Cordial`-prefixed key inherits the same reach, so the question when adding one
is who should be able to set it, not only what it does.

## Two lifetimes, and why they are two capabilities

`FFlag`, `FInt` and `FString` are consumed once, during
`nativeInitClientSettings`, roughly 100 ms into startup. Only the
`DFFlag`/`DFInt`/`DFString` family is re-read while the client runs. That is
what "dynamic" means in Roblox's own naming, and it is the entire constraint
ADR-005 is about.

So there are two capabilities:

- **`flags.write`** contributes to your `flags.json`, which is one of the
  layers read *before the engine process starts*. Effective at the next launch.
  This is the surface `flags.set` implements.
- **`flags.write.dynamic`** is for changing a `DF*` flag while the client runs.
  The static families cannot be changed live at all, so an API that accepted
  them here would appear to succeed and do nothing — the exact failure this
  project keeps finding elsewhere.

The broker enforces the split: a grant of `flags.write` does not admit
`flags.setDynamic`, and `broker.rs`'s
`writing_a_static_flag_is_a_different_capability_from_writing_a_live_one` is
the test that says so.

**`flags.setDynamic` exists as a method name and nothing else, and it is not a
gap waiting to be filled.** It is in `required_capability`, mapped to
`Capability::FlagsWriteDynamic`, and it is the only method whose name is
camel-cased. It is also the only method in that table with no arm in the host's
`dispatch`, so a plugin holding `flags.write.dynamic` gets past the capability
check and lands on the catch-all:

```json
{"status":"error","id":4,"message":"flags.setDynamic is not implemented yet"}
```

Its handler's own comment is the reason it is honest to write this down rather
than treat it as a to-do:

> it needs a live write into the running engine's own `DFFlag` table, and
> nothing in this project has ever reached into the engine process to do that —
> ADR-001 and ADR-003 rule out the in-process access that would take, so this
> is not a gap waiting to be filled, it is a capability whose effect has
> nowhere to live.

Note that `error` here is deliberately not `denied`. A denial would send an
author looking for a permission that was never the problem.

**A `DF*` override in your `flags.json` governs about the first two seconds.**
The engine fetches Roblox's own settings document 1.6–2.3 s in and reapplies it
over the top, so any `DF*` key that document also contains is reverted to
Roblox's value while the client is still starting. Keys the document does not
contain keep your value for the whole run. Measured both directions inside one
run with a control. The durable family is the one read once — so a startup flag
is the *stronger* surface, not the weaker one, which is the opposite of how it
reads at first.

## Worked example: reading and contributing flags

A plugin that reports what is in effect, then contributes one flag of its own
and handles the refusal it may get instead.

`plugin.json`:

```json
{
  "id": "tuner",
  "name": "Tuner",
  "version": "1.0.0",
  "entry": "main.ts",
  "capabilities": ["flags.read", "flags.write", "log"]
}
```

`main.ts` (the `call` scaffold is the same as the Events example above):

```ts
const log = (message: string) => call("log.write", { message });

// What is in effect right now, and who set each one. Cordial's layers only --
// Roblox's own defaults are not in here.
const listed = await call("flags.list");
if (listed.status === "ok") {
  const entries: Array<{ key: string; value: string; source: string }> = listed.result;
  await log(`${entries.length} override(s) in effect`);
  for (const e of entries) {
    await log(`  ${e.key} = ${e.value}  (from ${e.source})`);
  }
} else {
  await log(`could not read flags: ${listed.status}`);
}

// One key. `result` is null -- not an error -- when no layer sets it.
const one = await call("flags.get", { key: "FIntTaskSchedulerAutoThreadLimit" });
if (one.status === "ok" && one.result === null) {
  await log("nothing sets the thread limit; this plugin's value will stand");
} else if (one.status === "ok" && one.result.source === "user") {
  // The user's layer is above every plugin's. Writing here is not an error and
  // is not honoured, so say so rather than reporting a success that has no
  // effect the user will ever see.
  await log(`the user set the thread limit to ${one.result.value}; leaving it alone`);
}

// A whole-document replace, not a merge: send every key this plugin wants
// overridden, every time. Anything omitted is withdrawn.
//
// Strings on purpose. Numbers and booleans are stringified on the way in, and
// `true` becomes "true" rather than Roblox's own "True".
const set = await call("flags.set", {
  values: {
    "FIntTaskSchedulerAutoThreadLimit": "8",
    "CordialPresentMode": "uncapped",
  },
});

if (set.status === "ok") {
  // Not "applied". This wrote a layer that is read before the engine starts.
  await log("flags written; they take effect at the next launch");
} else {
  // Including the capability, because "denied" without saying what was needed
  // is the error message somebody files a bug about instead of fixing.
  await log(
    `could not write flags: ${set.status}` +
      (set.capability ? ` (needs ${set.capability})` : "") +
      (set.message ? ` (${set.message})` : ""),
  );
}
```

The two refusals worth recognising:

```json
{"status":"denied","id":3,"capability":"flags.write"}
{"status":"error","id":3,"message":"flags.set needs a values object"}
```

`denied` means the grant is missing — check `plugin-grants.json` for this
profile. `error` means the call was allowed and the request was wrong. A
plugin author needs to tell those apart, and collapsing them produces bug
reports about the wrong thing.

---

# The rest of the surface

Eight methods and two documents on disk. The methods have nothing in common
underneath — a D-Bus portal, a Unix socket, two JSON files and a filesystem
index — and that is the point.
[ADR-007](adr/ADR-007-host-resources-are-brokered.md) says a plugin
receives the *effect* and never the channel, so from your side of the pipe they
are all one shape: a JSON object goes out, a reply comes back.

Every reply is exactly one of three shapes, and your dispatcher should branch on
`status` rather than on the presence of `result`:

```json
{"status":"ok","id":4,"result":null}
{"status":"denied","id":4,"capability":"presence.set"}
{"status":"error","id":4,"message":"Discord is not running"}
```

**`denied` names the capability, not the method**, and the two are different
vocabularies. Several methods share one capability, so a refused `presence.clear`
comes back as `"capability":"presence.set"`, and a refused `preferences.get`
comes back as `"capability":"settings.read"`. The split between `denied` and
`error` exists because a plugin author needs to tell "I was not allowed" from
"it went wrong"; collapsing them produces bug reports about the wrong thing.

The examples below use the `call` helper that all three shipped example plugins
define — [`plugins/flag-inspector/main.ts`](../plugins/flag-inspector/main.ts) is the
shortest one to read, at 75 lines against `discord-presence`'s 150 and
`fps-flex`'s 177. It writes one line to stdout and resolves when a reply with
the same `id` arrives on stdin.

## Notifications: `notify.send`

| parameter | type | required | what it is |
|---|---|---|---|
| `summary` | string | yes | the notification's title |
| `body` | string | no | the line under it; defaults to `""` |

Returns `null` on success. Capability: **`notify.send`**.

```ts
const res = await call("notify.send", {
  summary: "Flag preset applied",
  body: "12 overrides will take effect at the next launch",
});
```

Cordial sends it through `org.freedesktop.portal.Notification.AddNotification`
rather than talking to `org.freedesktop.Notifications` directly. That is not
incidental: in the Flatpak build a portal interface is reachable from inside the
sandbox with no `--talk-name` entry, because the portal is the door Flatpak
already leaves open, whereas the notification daemon's own bus name is not. So
this capability costs nothing in the Flatpak manifest, which is what ADR-007
means by keeping the manifest's entries few and specific.

Refusals, verbatim:

- `notify.send needs a summary` — `summary` was absent, or was not a string. A
  non-string `body` is not refused; it is read as absent and you get an empty
  body.
- `notify.send needs a non-empty summary` — `summary` was present but empty or
  all whitespace. Checked before the bus is touched, so this one does not depend
  on there being a session bus at all.
- `could not reach the session bus: …` — no session bus in this environment.
- `the notification portal refused the call: …` — the portal answered with an
  error.

**There is no way to withdraw or update a notification you sent.** The portal
identifies notifications by an id the sender picks; Cordial picks one
(`cordial-1`, `cordial-2`, …) and does not tell you what it was, because
withdrawal would be a second capability and nothing has asked for one yet.

## Discord presence: `presence.set` and `presence.clear`

[`plugins/discord-presence/main.ts`](../plugins/discord-presence/main.ts) is the working
example, and it is an ordinary plugin — same manifest, same grants, same
isolation. It never learns where Discord's IPC socket is, never opens one, and
cannot send anything down it except the payload below.

`presence.set` takes a **closed** object. Any field not in this table refuses the
whole call:

| parameter | type | required | what it is |
|---|---|---|---|
| `client_id` | string | yes | a Discord application snowflake — ASCII digits only |
| `details` | string | no | the first line; at most 128 characters |
| `state` | string | no | the second line; at most 128 characters |
| `start` | integer | no | Unix seconds |
| `end` | integer | no | Unix seconds |
| `large_image` | string | no | asset key for the large image |
| `large_text` | string | no | its hover text |
| `small_image` | string | no | asset key for the small image |
| `small_text` | string | no | its hover text |

Returns `null`. Capability: **`presence.set`**.

`start` on its own renders in Discord as an elapsed counter; `start` and `end`
together render as a countdown. Those are Discord's semantics, not Cordial's.
Cordial assembles the `timestamps` and `assets` sub-objects itself from these
flat fields.

```ts
await call("presence.set", {
  client_id: "1234567890123456",
  details: "Using Cordial",
  state: "In session",
  start: Math.floor(Date.now() / 1000),
});
```

**The payload is a closed struct rather than a JSON value forwarded verbatim,
and that is the whole reason this is brokered.** A permissive pass-through would
quietly undo the boundary the moment Discord's IPC grew a field Cordial does not
know about — a plugin could then put it there.

`presence.clear` takes no parameters, returns `null`, and needs the same
`presence.set` capability. Clearing your presence is not a lesser power than
setting it — both say something about what the user is doing — so they share one
capability rather than inviting a plugin to ask for two. If nothing has been set
in this plugin's run yet, `presence.clear` answers `ok` immediately and opens no
connection: there is nothing to clear and nothing worth handshaking to say so.

Refusals, verbatim:

- `bad presence payload: …` — the object did not deserialise, which includes any
  unknown field.
- `client_id must be a Discord application snowflake (digits only)`
- `details must be at most 128 characters, Discord's own limit` (and the same
  sentence for `state`). Refused here rather than at Discord, so you find out
  from the response instead of from Discord silently dropping the activity.
- `Discord is not running` — no IPC socket answered a handshake. This is the
  ordinary case, not an exceptional one; most users do not have Discord open.
  The call fails every time it happens, and only Cordial's *logging* of it is
  throttled to one line per stretch.
- `presence update failed: …` — the connection died mid-write. Cordial drops it
  so your next call starts a fresh handshake.

One connection is held for as long as your plugin runs, rather than opened per
call, so a plugin that sets presence on every tick does not handshake with
Discord on every tick. Passing a different `client_id` drops the old connection
and makes a new one.

## Opening a web page: `url.open`

| parameter | type | required | what it is |
|---|---|---|---|
| `url` | string | yes | an absolute `http://` or `https://` URL |

Returns `null`. Capability: **`url.open`**.

Nothing else opens. The scheme is checked before Cordial's D-Bus connection is
even touched, which is why a refusal never depends on whether a session bus
happens to be reachable. Without that check the capability would be
indistinguishable from handing a plugin `file://` traversal, or a way to hijack
whatever handler the desktop has registered for an arbitrary scheme — some
desktops map schemes onto local applications with their own command-line
parsing.

The check is deliberately strict about `://` and deliberately case-insensitive:
`HTTPS://example.com` is accepted (RFC 3986 §3.1 says schemes are
case-insensitive, and refusing on casing alone would be a spurious refusal),
while `http:example.com` and `javascript:` styled merely to *contain* "http" are
not.

Refusals, verbatim:

- `url.open needs a url` — absent, or not a string.
- `scheme "file" is refused; only http and https may be opened` — the scheme
  name is quoted back to you.
- `not an absolute http or https URL` — there was no `://` at all.
- `could not reach the session bus: …` / `the OpenURI portal refused the call: …`

## Settings and preferences are two different things

These names are close enough to be genuinely confusing, so state the difference
before either:

| | **settings** | **preferences** |
|---|---|---|
| whose answers are they | your plugin's | the user's |
| who writes them | your plugin, via `settings.set` | Cordial, from its own page |
| can your plugin write them | yes | **no, and there is no method that could** |
| what defines the shape | nothing; any JSON object | the `preferences` array in your `plugin.json` |
| where it lives | `<profile>/plugins/<id>/settings.json` | `<profile>/plugins/<id>/preferences.json` |
| how it is updated | whole-document replace | one key at a time, read-modify-write |
| capability to read | `settings.read` | `settings.read` (the same one) |
| capability to write | `settings.write` | none exists |

**They are two files for a reason that is not tidiness.** `settings.set` replaces
the document wholesale, which is right for scratch state and fatal for anything a
person typed. Two writers of one document, one of whom replaces it whole, loses
the user's answers the first time the plugin saves anything.

Both live inside the **profile**, not beside your installed code. Installing a
plugin once for the machine is right; carrying what it remembered about one
account into the account somebody else plays on is not, and a settings document
is exactly where a plugin would record a username, a server or a webhook
([ADR-013](adr/ADR-013-per-profile-configuration.md)).

## Settings: the document your plugin owns

`settings.get` takes **no parameters** and returns your document — `{}` if you
have never saved anything. Capability: **`settings.read`**.

`settings.set` takes one:

| parameter | type | required | what it is |
|---|---|---|---|
| `settings` | JSON object | yes | the complete new document |

Returns `null`. Capability: **`settings.write`**.

```ts
await call("settings.set", { settings: { panel: "flags", opened: 4 } });
const mine = await call("settings.get");   // mine.result is your document
```

The usual case costs no round trip at all, because the document arrives unasked
in the handshake — the one line Cordial sends before you have called anything:

```ts
if (msg.id === undefined && msg.event === "cordial/init") {
  const settings = msg.payload.settings;
  // your document, {} if you have saved nothing,
  // null if you were not granted settings.read
}
```

`null` and `{}` are worth telling apart: the first means "ask the user for the
capability", the second means "you are new here". Delivering the document to a
plugin that was never granted `settings.read` would be routing around the broker
on the one path where the plugin made no request to check, so the handshake
checks the grant too.

**Neither method takes a plugin id, and that absence is the isolation.** Cordial
already knows which process is on the other end of the pipe. Naming another
plugin in your params is not an error and is not honoured — you get your own
document. There is no field to set to somebody else's name, which is the same
defence `events.declare` uses for namespaces, and it is expressed as a missing
parameter rather than as a check that a later refactor could reorder away.

Two capabilities rather than one, for the reason `events.declare` and
`events.publish` are two: a plugin that only reads its configuration should not
have to be trusted to rewrite it. A user approving "remember which panel I had
open" has not thereby approved "discard everything I set".

Limits and refusals, verbatim:

- `settings.set needs a settings object` — no `settings` key in `params`.
- `settings must be a JSON object` — an array, a number or a string. A settings
  page has to render this, and a bare array leaves a UI nothing to show.
- `settings are 1234567 bytes; the limit is 1048576` — the document is capped at
  one mebibyte, measured on the pretty-printed text. This is configuration, not a
  data store. The cap exists because `settings.write` is the only capability that
  lets a plugin consume the user's disk, in a directory they did not choose and
  do not watch; a plugin appending on every launch is an ordinary bug whose first
  symptom would otherwise appear somewhere else entirely.
- `<path> is not a JSON object` / `<path> is not usable (…)` — the file on disk
  is unreadable. Reported rather than answered as "you have nothing saved",
  because telling you the settings are empty invites you to write a fresh
  document straight over whatever the user actually had.

A write goes to a `settings.json.new` sibling and is then renamed, which is
atomic within the directory. A plugin killed mid-write would otherwise leave a
half-document that reads back as malformed, costing the user every setting they
had rather than the one they were changing.

You may also see `settings.get needs an open profile; this Cordial has no
settings store`. The client's own plugin host always has a profile, so this comes
from the `cordial-plugins` crate's `Session` constructed without one — a test
harness, in practice. It is an error rather than an empty document on purpose: a
stub reporting a save that went nowhere is the failure mode this project has the
longest list of afternoons lost to.

## Preferences: the answers the user owns

You declare fields in `plugin.json`; Cordial builds the page. There is no
capability for declaring and no other manifest key — declaring a field *is* how
you get a page, so a gear can never appear with nothing behind it, and there is
no second fact to disagree with the first.

```json
{
  "id": "example",
  "entry": "main.ts",
  "capabilities": ["settings.read"],
  "preferences": [
    { "key": "loud", "type": "bool", "title": "Be loud",
      "description": "Shown under the title.", "default": false },
    { "key": "level", "type": "int", "title": "Level", "default": 3,
      "minimum": 1, "maximum": 10, "step": 1, "group": "Tuning" },
    { "key": "mode", "type": "choice", "title": "Mode", "default": "slow",
      "options": [ { "value": "slow", "label": "Slow" },
                   { "value": "fast", "label": "Fast" } ] },
    { "key": "note", "type": "text", "title": "Note", "default": "" }
  ]
}
```

| `type` | the row it becomes | its own keys |
|---|---|---|
| `bool` | `AdwSwitchRow` | `default` |
| `int` | `AdwSpinRow` | `default`, `minimum`, `maximum`, `step` |
| `choice` | `AdwComboRow` | `default`, `options` of `{value,label}` |
| `text` | `AdwEntryRow` | `default` |

Every field takes `key` and `title`, and optionally `description` and `group`.
Fields sharing a `group` become one group on the page, in the order the groups
first appear; ungrouped fields come first. `value` and `label` are split in a
`choice` because they are not the same thing: `value` is what lands in the
document and what your code compares against, `label` is prose, and renaming a
label must not silently reset everybody's choice.

The manifest is refused at install if any of these does not hold — by name, with
the key quoted:

- at most **64** fields;
- keys of letters, digits, dashes and underscores, at most 64 characters, unique
  within the plugin;
- a non-empty `title` with no control characters, and the same rule for
  `description`, `group` and every option `label` where present;
- for an `int`, `minimum` no greater than `maximum`, a `default` inside its own
  range, and a `step` greater than zero if you give one;
- for a `choice`, at least one option, no two options sharing a `value`, and a
  `default` that is one of them;
- a `text` default of at most 4 KiB with no control characters.

The length caps on drawable text are **bytes, not codepoints** — the check is
`len()` on the UTF-8, so a title of 150 characters in a non-Latin script can be
refused by a message that says "at most 200 characters". Keys are ASCII-only, so
for them the two counts are the same.

Your words are drawn as **text, never as markup**. A control character anywhere
in a title, description, group name or option label refuses the whole plugin at
install rather than being stripped quietly — a title carrying a newline draws
over the row beneath it, and a row that can draw outside itself is the beginning
of the thing ADR-020 exists to prevent.

Reading them needs **`settings.read`**, the same capability as `settings.get`,
and arrives the same two ways:

```ts
if (msg.id === undefined && msg.event === "cordial/init") {
  const prefs = msg.payload.preferences;   // every declared key, always
}
const prefs = (await call("preferences.get")).result;
```

`preferences.get` takes no parameters. **The document you get is always complete
and always valid**: every key you declared is present, and every value fits the
declaration you wrote. No `?? default`, no range checks, no guarding against a
type you did not declare — the parsing, the range check and the fallback happen
once in Cordial rather than once per plugin in whatever way each author thought
of. A value saved against an older version of your manifest that no longer fits
falls back to the current default and Cordial says so in its log
(`plugin <id>: preference …`), because a preference that quietly reverts is
indistinguishable from one that never saved. Keys in the saved file that nothing
declares are dropped, so the document does not only ever grow.

A plugin that declares no fields and holds `settings.read` gets `{}` — the
truthful answer, since it has no answers because it asked nothing, not because it
was refused. `null` still means the capability was not granted.

**There is no `preferences.set`, and there is not going to be one.** These
answers are the user's. A plugin that could rewrite them could set them to
whatever it liked and have the page show its choice back as though the user had
made it. Your own state goes in `settings.json`, which is yours to replace.

**Why you cannot draw the page yourself.** GNOME Shell extensions can, because
they run inside the shell's own process. Your plugin does not: it is a separate
sandboxed process with no display and no toolkit, and a plugin able to draw in
Cordial's window could draw something indistinguishable from Cordial's own
sign-in dialog. [ADR-020](adr/ADR-020-declarative-plugin-preferences.md)
records what the declarative form gives up in exchange.

## Asset overlays: `assets.override`

[ADR-010](adr/ADR-010-plugin-asset-overlays.md) permits a plugin, and the
user directly, to supply files that resolve **in place of** Roblox's own for the
same name. Nothing is written into the APK or into anything Cordial extracts from
it. There is no cleanup step, because there was never a write to undo: stop
consulting a root and the original resolves again.

This reverses ADR-004, which refused plugin asset overrides outright. Two of that
ADR's three supporting claims were checked against primary sources and did not
hold up — the third did, which is why this is scoped to the whole `assets/` tree
rather than to a "safe subset" ADR-004 correctly identified as fictional.

### There are two routes, and the one you want is probably not the method

**A shipped `overlay/` directory needs no capability at all.** If your plugin
directory contains one, Cordial registers it at launch, before the engine reads a
single asset. That is the same precedent `flags.json` already sets: a static file
a plugin ships is not a request a process is making, it is what the plugin *is*,
and installing and enabling it is the consent. It is also the only route
available to a data-only plugin — a texture pack with no `entry` has no process
to make a call from — and the only route that reliably works, because **an asset
served once stays cached for the rest of the process**. The engine is handed a
pointer that has to remain valid, so an overlay registered after a texture has
loaded cannot change it.

The `assets.override` **method** gates something different: a *running* plugin
asking Cordial to register a directory of its own choosing, at runtime, which is
a request from a process and is brokered like every other one. Plugins start
after the client is up — deliberately, so a misbehaving plugin cannot interfere
with bring-up — so a runtime registration usually arrives after the engine has
already read a great deal. Use it to swap a root mid-session, not to ship a
texture pack.

| parameter | type | required | what it is |
|---|---|---|---|
| `dir` | string | no | a relative path inside your own installed directory; defaults to `"overlay"` |
| `clear` | bool | no | `true` unregisters your root and ignores `dir` |

Returns `{"registered": "/absolute/path"}`, or `null` for a clear. Capability:
**`assets.override`**.

```ts
await call("assets.override", { dir: "packs/winter" });
await call("assets.override", { clear: true });
```

The reply quotes the resolved path back at you — your installed directory with
`dir` joined onto it, absolute because installed directories are. That is a
string for your log, not a handle: you still have no file access of any kind, and
the only thing you can do with the directory is have named it.

Refusals, verbatim:

- `"../../etc" must be a path inside the plugin's own directory` — the path was
  absolute or contained `..`. It is treated as attacker-controlled and refused
  rather than quietly rewritten into something safe, the same treatment a
  manifest's `entry` gets.

Note what is **not** checked: the directory does not have to exist. Registering a
missing one succeeds and contributes no files, because the walk that builds the
index fails closed. If your overlay appears to do nothing, check the path before
looking anywhere else.

Your root is unregistered automatically when your process ends, however it ends.
A disabled or removed plugin's overlay never outlives it.

### What resolves, and in what order

An overlay root mirrors the APK's `assets/` layout exactly — the same shape
Sober's `asset_overlay` uses, so a directory built for one is usable, unmodified,
for the other. A file at `<root>/content/textures/wood.png` stands in for
`assets/content/textures/wood.png`.

Roots form a stack, built lowest first: every plugin in registration order, then
the user's own root pushed last. **The user's root therefore beats every
plugin's**, and among plugins the most recently registered wins; re-registering
an id moves it to the end rather than leaving a stale entry where it was. That
mirrors the precedence rule Cordial's FastFlag layering already uses, for the
same reason — an explicit choice the user made must not be silently overridden by
something they installed to do something else.

The user's root is `$XDG_CONFIG_HOME/cordial/overlay`, falling back to
`$HOME/.config/cordial/overlay`, overridable with `CORDIAL_OVERLAY`. It does not
have to exist.

Path resolution drops anything that would escape the root it belongs to — `..`,
an absolute name, or a symlink pointing outside — at index-build time, so no
later code has to remember to re-check it.

### What can be replaced, and what cannot

**Can:** anything under the APK's `assets/` tree that the engine reads through
`AAssetManager` — textures, sounds, fonts, models. A lookup checks this process's
own cache first, then the overlay stack, and the archive only on a miss.

**Cannot, today:** anything the engine opens by a real filesystem path rather
than through `AAssetManager`. The resolver for that route exists and is tested,
but nothing calls it yet — the C ABI shim in `native/system_paths.cpp` has not
been wired, and it has to be wired for `stat`, `open`, `fopen`, `access` and the
rest *together*, because a size call answering about the original while `open`
answers with the overlay is the mismatch that truncates a texture. Assume the
`AAssetManager` route only. `INFERRED` is not the label here — this is the code
saying so about itself, in `cordial_overlay_resolve`'s own doc comment.

**Cannot, ever, by design:** writes. A write is never redirected. ADR-010's whole
claim is that nothing is written into the APK or into anything extracted from it,
and handing the engine a writable descriptor onto a plugin's file would make the
overlay a place the engine can scribble — neither non-destructive nor anything the
plugin's author agreed to. Reads resolve to the overlay; writes go to the
original.

**Cannot:** anything outside the asset tree. The overlay is not a general
filesystem redirect, which is the same line ADR-007 draws between an effect and a
channel.

### What Cordial does not do about it

**Gameplay-affecting substitution is possible and Cordial builds no detection for
it.** Replacing a collision or hitbox mesh with a smaller or absent one is a
substantive advantage rather than a cosmetic change, and nothing "non-destructive"
implies catches it. ADR-010 documents this as the user's own responsibility — the
same posture Sober and Bloxstrap both take — and builds no content inspection, no
allow-list of "safe" asset types, and no attempt to tell a texture from a mesh.
Maintaining a classifier for every asset type Roblox ships, forever, and getting
it wrong silently, is worse than an honest warning. The capability's own consent
text says so to the user in as many words.

**Why interception rather than a mount.** The assets are zip entries inside
`base.apk`, so overlayfs has nothing to overlay without extracting the whole
archive first, and Flatpak cannot mount overlayfs unprivileged in any case.
Interception also yields the diagnostics a mount cannot: `--check-overlays`
reports which of your files match nothing in the current build, and the shadow
report names every case where two layers offered the same file and says which
won:

```
user wins over plugin:winter   content/textures/wood.png
```

Read that line in the direction the stack is built. The user's layer is last, so
it wins every name it offers and can only ever appear on the left; a plugin
appears on the left only against *another* plugin registered before it
(`plugin:winter wins over plugin:autumn   …`). If your plugin's file is on the
right, the fix is not in your plugin. "Why did this file not change" is otherwise
indistinguishable from "the overlay is broken", and it is the question users ask
most.

---

# What you cannot do

This is not a disclaimer. Each line below is a decision with reasoning behind it,
and knowing the reasoning is what stops you designing against a wall.

**No code inside the Roblox process.** No hooking, no memory patching, no
injected script environment, and no capability that could ask for one. Not
disabled — *absent*: there is no injection primitive in the binary, no name in
the capability vocabulary for one, and no placeholder
([ADR-001](adr/ADR-001-in-process-hooking.md)). The reasoning is worth
having in full, because it is the one that generalises. Enforcement must live
outside the boundary it enforces, and nothing lives outside a process from that
process's own perspective — any code with enough authority to patch a function
has enough authority to patch whatever would police it. Every other boundary
Cordial has works because core sits outside it: core closes a portal grant, core
kills a sandboxed process, core stops answering a plugin's messages. None of
those have an in-process analogue. And revocation is the sharp end of it: a fresh
process starts from a known-good state, so restart is the only integrity boundary
there is, and an in-process capability would be the single permission in the
system that a revocation UI could not actually revoke. Shipping a capability that
cannot be governed converts a hard guarantee into a promise, and users cannot
tell the difference until it fails.

**No memory access to Cordial either**, and no shared-memory transport to get
there ([ADR-003](adr/ADR-003-plugin-isolation.md)). Two reasons, both
practical. Capabilities are worth declaring only if declaring less means being
able to do less: a plugin granted nothing but a window title, yet able to reach
into Cordial's memory, can rewrite the broker's own allow-lists, and every other
permission becomes a suggestion. And process isolation is the only kind that
holds — an in-process sandbox is a boundary only as strong as the absence of bugs
in it, whereas an address-space boundary is enforced by the MMU and does not
depend on Cordial being correct. This is also why performance is never an
argument for a hole: if an interaction is too slow across the pipe, the answer is
a better-shaped call, not a shortcut past the broker.

**No general UI surface.** What exists is `notify.send`, asset overlays, and the
preferences page Cordial draws from your declaration. That is the list. A plugin
cannot draw on top of the game and cannot draw inside Cordial's own window. The
distinction ADR-009 draws is between reading output and writing into a process:
capture works today and needs nothing from Cordial, because a recorder receives
frames the compositor already produced and can observe nothing a screenshot
could not. An overlay works by loading itself into the game and hooking the
presentation path — Steam's Linux overlay is an `LD_PRELOAD` of
`gameoverlayrenderer.so` wrapping `glXSwapBuffers`/`vkQueuePresentKHR`. That is a
third party executing inside the engine's address space, hooking the exact call
Cordial uses to present. Cordial refuses to do that itself; shipping a supported
way for someone else to do it would be that refusal in name only. Note that
ADR-010 does not soften this: it changes which file loads *before* the frame
exists, not what is drawn on top of one once it does.

**No reading the DataModel, the Lua state, or anything else inside the engine.**
There is no method for it and there could not be one: those live in the Roblox
process's address space, and reaching them is exactly the in-process access
ADR-001 refuses. Nor does the framework layer answer for them — Cordial answers
the Android platform calls the client makes, and the DataModel is never one of
them.

Be concrete about how little is even *defined*. Five core events exist, and a
plugin only ever sees them under their namespaced wire names:
`cordial/client.launch`, `cordial/client.ready`, `cordial/client.shutdown`,
`cordial/engine.version` and `cordial/window.resized`. The `cordial` namespace is
reserved and no plugin may declare under it, so a name arriving with that prefix
is one Cordial published — which is the point of carrying it. **A dispatcher
comparing `msg.event === "client.launch"` never fires**; `discord-presence`
matches the full strings for exactly this reason, and its comment records that
the bare names are what these used to be. All five are gated on `lifecycle.read`.

Then be concrete about how little of that *arrives*. The client publishes three
of the five — `client.launch`, `engine.version` and `client.shutdown` — and
`client.ready` and `window.resized` are published by nothing at all, so a plugin
waiting for either waits forever. What the three carry is thin: a profile name,
a version string, and nothing. **No core event names a place, a game or a
user.** That is why `discord-presence` publishes "Using Cordial" rather than the
name of the game — not because no event arrives, but because no event's payload
says what is being played. `lifecycle.read`'s consent text — *"Know when the
client launches, becomes ready and shuts down. Not what you play."* — is right
about the launch, the shutdown and the disclaimer, and names a "becomes ready"
that nothing publishes.

**No file, network, environment or subprocess access.** Your plugin is a Deno
process started with **no permissions at all**. That is a second, independent
layer *under* Cordial's capability broker, so a plugin cannot reach the machine
even if the broker had a hole in it. Everything you can do arrives over stdio and
was checked against your grant first.

**No widening the sandbox from your manifest.** A capability Cordial does not
already broker needs a change to Cordial, not a line in your `plugin.json`. This
is slow on purpose: a Flatpak permission is app-wide and permanent while a
capability is per-plugin and revocable, so if installing a plugin could add a
permission, uninstalling it would not take the permission away. Open an issue — a
broker is a payload type and an effect, so adding one is a small change. If a
proposed broker *cannot* be small, that usually means the capability is too broad
and wants splitting.

**No writing your own preference values**, and no reading or writing another
plugin's settings. Both are enforced by an absent parameter rather than a check:
there is no `preferences.set` to call, and `settings.*` has no field in which to
name somebody else.

**No writing into Roblox's own files.** Overlays resolve reads; they never
redirect a write, and nothing is ever copied into the APK or into the extracted
asset tree. That is what makes uninstalling a plugin a complete undo with no
cleanup step.

**Not a rule, but the thing most likely to waste your afternoon:** a method
Cordial knows but has not wired an effect for answers `error`, not `denied` —
`"flags.setDynamic is not implemented yet"` from the client's plugin host, or
`no broker wired for "assets.override"` from the `cordial-plugins` crate's
in-test `Session`. That is deliberate; reporting `denied` would send you looking
for a permission that was never the problem. `flags.setDynamic` in particular is
not a gap waiting to be filled — its effect needs a live write into the running
engine's own flag table, which is the in-process access ADR-001 and ADR-003 rule
out, so it is a capability whose effect has nowhere to live.
