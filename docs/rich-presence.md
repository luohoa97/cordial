# Discord Rich Presence

Cordial ships a Discord Rich Presence plugin, in
[`plugins/discord-presence/`](../plugins/discord-presence). It is first-party in
the sense that it comes with the project and in no other sense: an ordinary
`plugin.json`, ordinary grants, the same isolation as anything you write
yourself — [ADR-006](adr/ADR-006-plugin-events-and-first-party.md) is
explicit that "built in" and "a plugin" are not opposites, and Cordial's own
features are built this way so the API has to be good enough for them. It
requests exactly three capabilities, `lifecycle.read`, `presence.set` and
`log`, and holds nothing else.

What it does is small. It subscribes to the client's lifecycle, publishes a
presence on `launch` and again on `ready`, and clears it on `shutdown`.

**It never learns where Discord's socket is, and that is the point.** The
plugin sends a payload — an application id, `details`, `state`, timestamps and
image keys — and Cordial does the rest: searching `discord-ipc-0` through `-9`
and the nested path Discord's own Flatpak uses, performing the handshake, and
writing the frames. The payload is a closed struct that refuses any field
Discord does not define, so nothing a plugin invents crosses the wire, and
`details` and `state` are refused past Discord's own 128-character limit — the
author hears that from the call rather than from Discord quietly dropping the
whole activity. A plugin cannot read Discord's state and cannot send anything
else down the connection.

That is [ADR-007](adr/ADR-007-host-resources-are-brokered.md) rather than
a detail of this one plugin. A Flatpak permission is app-wide and permanent
while a capability is per-plugin and revocable, so if installing a plugin could
add a permission, uninstalling it could not take one away. Cordial holds the
permission and performs the effect; the plugin sends a payload.

## Turning it on

Plugins are discovered under `~/.local/share/cordial/plugins/`, one directory
each, so installing this one is a copy — and the same `XDG_DATA_HOME` remap
described for `flags.json` in [`docs/fastflags.md`](fastflags.md) applies
inside the Flatpak:

```bash
cp -r plugins/discord-presence ~/.local/share/cordial/plugins/
```

Installing is not approving. Grants are default deny and belong to the profile,
so the plugin gets what you write in
`~/.local/share/cordial/profiles/<profile>/plugin-grants.json` and nothing else:

```json
{ "discord-presence": ["lifecycle.read", "presence.set", "settings.read", "log"] }
```

A plugin with no grants is reported at launch and not started, and a capability
that was requested but withheld is named — so an author can tell "not allowed"
from "broken". Settings has a Plugins page listing what is installed, what each
one requests and what it has been granted; nothing on it writes that file for
you.

## What it does not do yet

Two of these the plugin's own source states plainly rather than hiding, and the
third is not the plugin's fault.

**The Discord application id is a placeholder.** Until somebody registers an
application and replaces the constant in `main.ts`, the activity carries no
Cordial name or icon in Discord's UI.

**The lifecycle push carries no payload**, because which game or place is
running lives in `cordial-runtime` and this plugin was written without touching
it. So the text is generic — "Using Cordial", "In session" — rather than naming
the experience.

**And nothing reaches Discord in an actual session yet.** The broker, the
payload validation and Discord's framing are real, and are covered end to end by
`crates/cordial-plugins/tests/discord_presence_plugin.rs`, which discovers the
shipped plugin, spawns it as a real Deno process, drives real lifecycle pushes
through it and watches the frames land on a stand-in Unix socket. But the plugin
host the *client* runs, `crates/cordial-runtime/src/plugin_host.rs`, serves
`settings.*`, `flags.*` and `log.write` and answers everything else with `not
implemented yet`, and nothing outside that test ever pushes a lifecycle event —
so a granted `discord-presence` starts, asks to subscribe, and is told the
method is not implemented. That is `INFERRED` from reading both hosts rather
than measured in a session, and joining the two up is the first thing to look
at if you want this working.
