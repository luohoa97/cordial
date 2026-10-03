# Changing FastFlags

<!-- description: Set Roblox FastFlags in Cordial, see which layer wins, and find the flags and keys Cordial itself reads. -->
<!-- icon: flag -->

FastFlags are Roblox's internal tuning switches. Cordial lets you override any of them, per profile, from a file or from **Settings → FastFlags**.

## Set a flag

<!-- steps -->

### Open the editor

**Settings → FastFlags** is a text editor for your profile's `flags.json`. Paste a list, or type one, and press **Apply**.

### Write a flat object

```json
{
  "DFFlagRbxTransportUseRtcioRna": false,
  "FIntTaskSchedulerAutoThreadLimit": 8,
  "FFlagDebugGraphicsDisableVulkan": false
}
```

Values can be booleans, numbers or strings. Roblox stores them all as strings and Cordial converts. The format is Bloxstrap's, so a list from a video or a Discord message pastes in as it is.

### Start Roblox again

The editor says it applies the next time you press Roblox. A `DF` flag can also change in a game that is already running; see [When a change takes effect](#when-a-change-takes-effect).

<!-- /steps -->

To edit the file yourself, it is one file per profile:

<!-- tabs -->

### Native install

```text
~/.local/share/cordial/profiles/<profile>/flags.json
```

### Flatpak

```text
~/.var/app/io.github.luohoa97.Cordial/data/cordial/profiles/<profile>/flags.json
```

`INFERRED` from how Flatpak remaps `XDG_DATA_HOME`; not checked against an installed package. The editor prints the real path under the text box.

<!-- /tabs -->

`CORDIAL_FLAGS` points the client at another file. It makes one file serve every profile, so it is for experiments.

> [!WARNING]
> A name the engine does not know is accepted and ignored, silently. An invented flag looks exactly like a working one. If a flag seems to do nothing, check that it exists:
>
> ```bash
> strings ~/.cache/cordial/lib/x86_64/libroblox.so | grep -x DebugGraphicsDisableVulkan
> ```
>
> The file carries the `FFlag`/`FInt`/`FString` prefix; the engine's own table stores the name without one, which is why the `grep` drops it.

To choose a graphics backend, use **Settings → General → Graphics**, not a flag. Cordial decides that before the engine starts.

## When a change takes effect

| Flag family | When it is read |
|---|---|
| `FFlag`, `FInt`, `FString` | Once, at startup. Relaunch to change them. |
| `DFFlag`, `DFInt`, `DFString` | At startup, and again while the game runs: Roblox re-fetches its settings about every two minutes and Cordial hands the engine your overrides again straight after. |

A plugin loaded part-way through a session cannot change a startup flag, whatever it writes.

Cordial puts your overrides back after each Roblox refresh, whatever the family. Each time it prints a `[reapply]` line with what it cost. Two limits:

- The first couple of seconds, before the engine's first fetch, are not covered.
- It re-sends the cached copy of Roblox's document, so a flag Roblox changed during your session goes back to its older value, as at launch.

`CORDIAL_NO_FLAG_REDELIVERY=1` in the client's environment turns the re-apply off. It exists to confirm that a flag reverts without it.

Why: [ADR-051](adr/ADR-051-overrides-are-reapplied-after-the-engines-refresh.md).

## Which layer wins

Flags come from several sources and each owns its own file. From lowest precedence to highest:

| Layer | Source | Notes |
|---|---|---|
| Roblox's settings | The client-settings document from Roblox | The base everything else is merged into. |
| Cordial's default | Built in | Only `FFlagUserLaunchedWithBloxstrap`, see the table below. |
| Graphics optimisation | **Settings → General → Graphics** | The "more cores" and "fewer cores" choices set thread-count flags. Balanced sets none. |
| Plugins | `<plugin>/flags.json`, alphabetical | A plugin switched off contributes nothing. |
| Frame rate limit | **Settings → General → Graphics** | Sets `DFIntTaskSchedulerTargetFps`. Beats a plugin that sets the same flag. |
| Your flags | `<profile>/flags.json` | Always wins. |

A plugin never writes to your file. Removing a plugin removes its flags, and when two layers set the same flag the log names both:

```text
flags: FIntTaskSchedulerAutoThreadLimit = 8 from user
       (overrides plugin:fps-tweaks=4, plugin:net-tuner=16)
```

Two plugins that disagree are both named. The later one wins so the outcome is deterministic.

Your flags live in the profile, so a flag you set while testing on one account is not still set on the account you play. A file left at the old `~/.config/cordial/flags.json` is moved into the first profile that looks for one ([ADR-013](adr/ADR-013-per-profile-configuration.md)). The layering itself: [ADR-005](adr/ADR-005-flag-service.md).

## Flags and keys Cordial reads

Besides Roblox's own flags, these names mean something to Cordial. The `Cordial` ones are never sent to Roblox.

| Name | Values | What it does |
|---|---|---|
| `FFlagUserLaunchedWithBloxstrap` | `True` (default) | Tells games the launcher speaks BloxstrapRPC, which Discord presence needs. Set `"False"` to retract the claim; games can then tell they are not under a launcher that implements it. It is not an engine flag. That a game then answers yes is `INFERRED`. |
| `DFIntTaskSchedulerTargetFps` | whole number | The engine's own frame target. **Settings → General → Graphics → Frame rate limit** sets it for you. |
| `CordialFrameRateLimit` | `display`, or a number of fps | The Frame rate limit row, as a key a plugin can set. The row beats a plugin; your `flags.json` beats the row. |
| `CordialPresentMode` | `off`, `auto`, `mailbox`, `immediate`, `uncapped`, `fifo`, `fifo-relaxed` | Vulkan present mode. The **Frame pacing** row sets it. A mode the driver does not offer leaves the engine's own choice. |
| `CordialGraphicsBackend` | `automatic`, `vulkan`, `gles` | Which graphics backend to offer the engine. The **Graphics** row beats a plugin's request. |
| `CordialDeviceProfile` | `pc-windows-11`, `roblox-app`, `android-tablet`, `meta-quest` | Which device Cordial says it is. **Has no effect from `flags.json` today**; the Graphics optimisation row sets it, through `CORDIAL_DEVICE_PROFILE`. `INFERRED` from reading the code: nothing outside the flag module calls the reader. |

Graphics optimisation sets these flags, sized to your physical core count:

<details>
<summary>What the more-cores and fewer-cores choices set</summary>

"Windows PC - more cores" (`CORDIAL_PERFORMANCE=throughput`):

| Flag | Value |
|---|---|
| `FIntTaskSchedulerThreadMin` | `0` |
| `FIntTaskSchedulerAsyncTasksMinimumThreadCount` | physical cores, at most 3 |
| `FIntTaskSchedulerAutoThreadLimit` | physical cores |
| `FIntSmoothClusterTaskQueueMaxParallelTasks` | physical cores |
| `FIntOcclusionWorkerThreadCount` | half the physical cores, rounded up |
| `FFlagMovePrerenderV2` | `True` |
| `FFlagGcInParallelWithRenderPrepare3` | `True` |
| `DFIntSimMidPhaseContactPipelineBatchSize` | `128` |

"Windows PC - fewer cores" (`CORDIAL_PERFORMANCE=latency`) sets only `DFIntSimMidPhaseContactPipelineBatchSize` to `128` and `FIntTaskSchedulerThreadMin` to `0`.

None of these is measured on this project's hardware, which is why Balanced, which sets nothing, is the default.

</details>

## Raise the frame rate

The Android client has no frame-rate row in its own menu, so there are two separate levers in Cordial's Settings.

| What you want | Where |
|---|---|
| Stop drawing being pinned to your display's refresh | **Settings → General → Graphics → Frame pacing**. Mailbox is the default. The FPS Flex plugin pulls the same lever, so use one or the other. |
| Raise the engine's own target frame rate and keep it there | **Settings → General → Graphics → Frame rate limit** |

Neither replaces the other. Frame pacing on FIFO caps you at your panel's rate whatever the limit says. Frame pacing applies at the next launch.

**Frame rate limit** offers Display refresh (sets nothing), or 90, 120, 144, 165 or 240. A new cap takes effect in a running game at once. Going back to Display refresh takes up to two minutes, because the engine does not unset a flag that Roblox's settings leave out.

> [!WARNING]
> Do not pick a cap above your display's refresh. On a 60 Hz output a cap of 90 presented about 31 frames a second and 240 about 36, against 57 to 60 with nothing set. That is one headless environment, and on a fast monitor the opposite holds: on a 240 Hz output with an NVIDIA GPU, `DFIntTaskSchedulerTargetFps=240` presented 216-232 frames a second against a flat 59.9 with the engine's own target (measured 2026-10-03). Nothing above 240 is offered because a contributor reports the engine stops there.

## Import a list from another launcher

**Settings → FastFlags → Import…** reads a Bloxstrap or Fishstrap `ClientAppSettings.json`, or Sober's `config.json` (only its `fflags` object), and merges the flags into this profile's. From a terminal:

```bash
cordial --import-flags ClientAppSettings.json     # or - for standard input
cordial --import-flags --sober                    # finds Sober's own config
cordial --import-flags list.json --profile NAME --replace
```

Flags already set are kept unless the list sets them again. `--replace` starts from empty instead. Values are checked by prefix: `FFlag` takes `True` or `False`, `FInt` a whole number, and `FLog` anything, because a log channel takes either a number or a severity and that is not visible from outside.

An entry the check refuses is skipped and named, and the rest are imported. A name with no FastFlag prefix is imported and listed, so a typo shows. The Sober path outside the Flatpak (`~/.config/sober/config.json`) is `INFERRED`; only the Flatpak's has been seen.

## If the interface looks coarse

The interface is laid out for a low-density phone: render resolution is 720p and `dpiScale` is 1.0. For a direct `cordial-run` launch, raise both:

```bash
CORDIAL_MONITOR=1 CORDIAL_RESOLUTION=1920x1200 CORDIAL_DPI_SCALE=1.75 \
cordial-run --lib-dir /path/to/lib/x86_64 --apk /path/to/base.apk \
  --host-libc --game-activity --run 30
```

Roblox's graphics-quality flags (`DebugFRMQualityLevelOverride` and the MSAA overrides) change nothing here, because they govern 3D scenes and the signed-out landing page is a 2D interface.
