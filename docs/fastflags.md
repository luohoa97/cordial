# Changing FastFlags

Roblox is configured by FastFlags, and Cordial lets you override any of them.
Create `~/.local/share/cordial/profiles/<profile>/flags.json` (or point
`CORDIAL_FLAGS` at another file) with a flat object. Installed as a Flatpak the
sandbox moves `~/.local/share` to `~/.var/app/io.github.luohoa97.Cordial/data`, so the
same file is `~/.var/app/io.github.luohoa97.Cordial/data/cordial/profiles/<profile>/flags.json`
— `INFERRED` from how Flatpak remaps `XDG_DATA_HOME`, not yet checked against an
installed package.

```json
{
  "DFFlagRbxTransportUseRtcioRna": false,
  "FIntTaskSchedulerAutoThreadLimit": 8,
  "FFlagDebugGraphicsDisableVulkan": false
}
```

**All three of those exist in the Android engine, and this example used to
carry one that does not.** It offered
`"FStringDebugGraphicsPreferredBackend": "Vulkan"`, which reads perfectly and
is not a Roblox flag: `DebugGraphicsPreferredBackend` appears **zero** times in
`libroblox.so`, and nothing resembling it does either — the real names in that
family are `DebugGraphicsDisableVulkan`, `DebugGraphicsDisableOpenGL`,
`DebugGraphicsDisableVulkan11` and so on. Reported by a user, checked against
the binary, and worth stating plainly because a documented example is the first
thing anybody copies.

**A name the engine does not know is accepted and ignored**, silently — it goes
into the settings document like any other key and nothing rejects it, so an
invented flag looks exactly like a working one. If a flag seems to do nothing,
check that it is real before assuming it did not help:

```bash
strings ~/.cache/cordial/lib/x86_64/libroblox.so | grep -x DebugGraphicsDisableVulkan
```

The name in the file carries the `FFlag`/`FInt`/`FString` prefix; the engine's
own table stores it without one, which is why the `grep` above drops it.

To choose a graphics backend, use Settings rather than a flag — Cordial decides
that before the engine starts, and the setting is what it reads.

**Raising the frame rate takes two separate levers, and neither is in Roblox's
own menu.** The in-game settings have no frame-rate row because the *Android*
client has none — the Windows client does, and so do the desktop menus people
remember, but Cordial runs the Android build and nothing here can add a row the
client does not draw. Reported as a missing feature, which is a fair reading of
an interface that simply has no such control.

| What you want | Where it is |
|---|---|
| Stop drawing being pinned to your display's refresh | **Settings → General → Graphics → Frame pacing**, or the FPS Flex plugin — the same lever, so use one or the other |
| Raise the engine's own target frame rate | the `DFIntTaskSchedulerTargetFps` FastFlag |

They are not the same setting and neither substitutes for the other: Frame pacing
is `VkSwapchainCreateInfoKHR::presentMode`, which decides whether a finished
frame waits for the next refresh, and the flag is what the engine's scheduler
aims at. Leaving the first on FIFO caps you at your panel's rate whatever the
flag says.

**One report of the flag not holding**, on a machine that reached 240 and fell
back to 60 after a few minutes. Not reproduced here and not explained; if you
see the same, [say so on the tracker](https://github.com/luohoa97/cordial/issues)
rather than assuming your value was wrong.

Values may be written as booleans, numbers or strings — Roblox stores them all
as strings and Cordial converts. The overrides are merged into the settings
document the engine is given at startup, and the launch log reports how many
were applied.

**`FFlag`, `FInt` and `FString` are read once at startup**, so changing them
needs a relaunch. Only the `DFFlag`/`DFInt`/`DFString` family is re-read while
the client is running. That distinction matters if you are building anything
that changes flags dynamically — a plugin loaded part-way through a session
cannot change a startup flag, whatever it writes.

## Layers and provenance

Flags come from more than one place, and each source owns its own file:

```text
<profile>/flags.json                             user    (always wins)
~/.local/share/cordial/plugins/<id>/flags.json   plugin
the client-settings document from Roblox         base
```

Your overrides live in the profile, so a flag you set while testing something on
one account is not silently still set on the account you play. A file left at
the old `~/.config/cordial/flags.json` is moved into the first profile that goes
looking for one — see [ADR-013](adr/ADR-013-per-profile-configuration.md).

A plugin never writes to your file. That keeps three things true: a plugin
cannot silently overwrite a value you chose, removing a plugin removes its
flags, and "why is this flag set to that?" has an answer. Conflicts are reported
rather than resolved quietly:

```text
flags: FIntTaskSchedulerAutoThreadLimit = 8 from user
       (overrides plugin:fps-tweaks=4, plugin:net-tuner=16)
```

Two plugins disagreeing is a real disagreement, so both are named. The later one
wins so the outcome is deterministic, but nothing is hidden.

**If the interface looks coarse**, it is being laid out for a low-density phone.
Raise both — the render resolution is 720p by default and `dpiScale` is 1.0,
which is what Roblox treats as a cheap handset:

```bash
CORDIAL_MONITOR=1 CORDIAL_RESOLUTION=1920x1200 CORDIAL_DPI_SCALE=1.75 \
cargo run --release --bin cordial-run -- \
  --lib-dir /path/to/lib/x86_64 --apk /path/to/base.apk \
  --host-libc --game-activity --run 30
```

Roblox's graphics-quality FastFlags (`DebugFRMQualityLevelOverride` and the MSAA
overrides) were tested and change nothing here, because they govern 3D scene
rendering and the logged-out landing page is a 2D interface. Resolution and
density are the levers that apply to it.
