# Playing in VR

<!-- description: Run Roblox's Meta Quest build on your PC and play it in a headset through an OpenXR runtime. -->
<!-- icon: vr-cardboard -->

Play in VR runs Roblox's Meta Quest build on your PC and shows it in your headset through an OpenXR runtime. The headset is a display; the game runs on the computer.

> [!WARNING]
> **Experimental.** Use an alt account. This is new, and an account you care about is not something to test it with.
>
> This runs Roblox's Quest client on hardware it was not made for. You are responsible for your account under [Roblox's Terms of Use](https://en.help.roblox.com/hc/en-us/articles/115004647846-Roblox-Terms-of-Use) and [Meta's Quest terms](https://www.meta.com/legal/supplemental-terms-of-service/).

## What you need

- An **x86-64 PC**. The button does not appear on other computers.
- **Your own Quest with Roblox installed** from the Meta Horizon Store. Cordial copies Roblox from it. It never downloads a Quest build from anywhere, and it refuses an APK that Roblox did not sign.
- An **OpenXR runtime**. [WiVRn](https://github.com/WiVRn/WiVRn) streams to a Quest over Wi-Fi or USB; SteamVR and Monado also work. Cordial uses one and does not install one.
- For copying Roblox off the headset: a USB-C cable that carries data (a charge-only cable is the commonest reason nothing shows up) and `adb`.

## Get Roblox off your Quest

**Settings → VR → Get It from Your Quest** walks through this and checks each step before the next. Nothing on the headset is changed: Cordial only lists packages and copies the installed APK.

<!-- steps -->

### Install adb

`adb` is Android's tool for talking to the headset. If it is missing, the page names the command for your distribution:

| Distribution | Command |
|---|---|
| Fedora | `sudo dnf install android-tools` |
| Debian, Ubuntu | `sudo apt install adb` |
| Arch | `sudo pacman -S android-tools` |

Cordial's Flatpak cannot run `adb`; see [In Cordial's Flatpak](#in-cordials-flatpak).

### Turn on developer mode

In the Meta Horizon app on your phone: Devices, your headset, Headset settings, Developer mode, on. Meta only shows that switch to accounts in a developer organisation, which is free to create; see [Meta's guide](https://developers.meta.com/horizon/documentation/native/android/mobile-device-setup/). Restart the headset if the setting does not seem to take.

### Connect the headset

Plug it in and put it on. Choose **Allow** on "Allow USB debugging?" and tick **Always allow from this computer**. The page updates by itself.

### Copy Roblox

Cordial shows the version on the headset and compares it with the one it already has. Copying is about 150 MB. Cordial checks Roblox's signature and keeps the build with its other Roblox builds. When it says done, you can unplug the headset.

<!-- /steps -->

<details>
<summary>The headset is not showing up</summary>

- Press **Restart adb** on the page.
- Unplug and replug.
- Revoke USB debugging authorisations in the headset (Settings, System, Developer) and replug.
- Check developer mode is still on.
- Use a port on the computer itself rather than a hub.

</details>

If you already have a Quest build of Roblox copied off your own headset, **I Have the APK File** takes it. `cordial --import-quest-apk FILE` does the same from a terminal.

## Play

<!-- steps -->

### Start your OpenXR runtime

With WiVRn, its server must be running before you press Play in VR; Settings says when it is not. To run it without WiVRn changing your system's active runtime:

```text
flatpak run --command=wivrn-server io.github.wivrn.wivrn --no-manage-active-runtime
```

### Choose the runtime

**Settings → VR → Runtime** lists your system's active runtime, WiVRn's Flatpak, SteamVR and anything in `share/openxr/1/`, or **Another runtime** for a manifest file. The choice is given to the game for that launch only. Cordial never changes your system's active runtime.

### Press Play in VR

It sits under the Roblox button on the launcher.

<!-- /steps -->

A profile is shared between the normal game and VR: you sign in once, and the same profile lock stops both running at once. Each keeps its own game settings and caches, because the two Roblox versions store different things there.

## When Roblox updates

Roblox stops accepting old versions after an update, and the Quest build goes stale with every one.

1. Update Roblox on your Quest from the Meta Horizon Store.
2. Open **Settings → VR → Get It from Your Quest** again. Once your computer is allowed, it goes straight to the Roblox step and copying is one click.

If the headset still has the version Cordial already has, the page says so and asks you to update it on the headset first. The old build stays until the new one is in, so a failed copy loses nothing.

Cordial cannot tell you in advance that your build is too old, and no "too old" signal from the Quest build has been seen yet. A VR session that stops working after a Roblox update gets a note on the crash page pointing here. Why: [ADR-053](adr/ADR-053-vr-is-a-mode-of-the-android-runtime.md).

## In Cordial's Flatpak

<details>
<summary>WiVRn, adb and other runtimes under Flatpak</summary>

VR in the Flatpak is meant for **WiVRn installed as a Flatpak** and needs nothing from you. Cordial's Flatpak can read WiVRn's install and reach its server, and brings its own OpenXR loader. Checked so far: it finds WiVRn, sees its server running and loads its runtime library. **A whole VR session from the Flatpak has not been run yet.**

**Getting Roblox off your Quest.** The Flatpak has no `adb`. With the headset connected and allowed, run these in a terminal, then choose **I Have the APK File** with the file it copied:

```bash
adb shell pm path com.roblox.client
adb pull /data/app/…/base.apk quest-roblox.apk
```

Use the path the first command printed.

**Other runtimes.** A Monado installed on your system does not load inside a Flatpak, because its library is built against your system's libraries, not the Flatpak's. Use Cordial installed another way for Monado.

SteamVR's runtime needs SteamVR already running and shared memory. To try it, grant it yourself. This has not been tested:

```bash
flatpak override --user io.github.luohoa97.Cordial \
  --filesystem=~/.local/share/Steam/steamapps/common/SteamVR:ro \
  --filesystem=xdg-config/openvr:ro --device=shm --share=ipc
```

</details>

## What is broken or untested

- **Sound** goes to the same audio output as the normal game. It has been checked with Cordial's own counters, not by ear in a game, and the microphone in VR has not been tried.
- **Typing** into a text box reaches it, checked through Cordial's own control socket. Typing on a real keyboard while in VR has not been checked.
- **Controller vibration** can buzz on and on with WiVRn 26.9. That is a bug in WiVRn's headset app, fixed in WiVRn's pull request #1131, which no release carries yet.
- **Leaving a game** from the Roblox menu brings the menu back on Monado. The in-game Leave button has been exercised only partly.
- **Joining a second game** after leaving one crashes when the runtime is Monado with its default in-process compositor, inside the graphics driver. It has not been tried on WiVRn, whose compositor runs in its own process.
- **Frame rate in game** is below the headset's refresh rate: 44 to 65 frames a second on Monado's simulated headset. On a Quest 3 over WiVRn, in a game made for VR, it looked mostly above 60 with occasional slight stutters; that was watched, not measured.
- **No warning when your Quest build is too old.** See [When Roblox updates](#when-roblox-updates).
- A session that stopped with MangoHud's overlay loaded into Monado was seen once. If VR crashes with `MANGOHUD=1` set, try without it.

## Reporting a problem

`cordial --diagnostics` has a VR line and `cordial --doctor` has VR checks; paste both into a report. The switches for investigating a VR run are in [`vr/env.md`](vr/env.md).
