# Game title bar

<!-- description: Choose a default, compact or hidden title bar for the game window. -->
<!-- icon: window-maximize -->

**Settings → General → Game window → Title bar** offers **Default**, **Compact** and **Hidden**. The choice applies to a game that is already running. Default is the default.

| Choice | What you get |
|---|---|
| Default | The normal header bar |
| Compact | A shorter header bar |
| Hidden | No title bar and no window controls. A tiled window keeps its tile, because hiding does not request fullscreen. |

- Entering and leaving fullscreen does not bring a hidden bar back.
- With no bar showing, whether Hidden or fullscreen, the bottom row of the window is a black line and the game draws one row shorter. Without it some compositors treat the window as hidden behind the game and the text-box editor never appears. Why: [ADR-056](adr/ADR-056-the-host-window-keeps-one-pixel-the-engine-does-not-cover.md).
- With the bar hidden, use your desktop's window shortcuts to move or close the game, and the Cordial launcher to change this setting.
- A direct `cordial-run` launch takes `CORDIAL_TITLE_BAR=hidden` for the same mode.

> [!NOTE]
> This applies only to Wayland game windows. The X11 runtime uses a native window with no Cordial title bar, so the setting does nothing there.

Changing the bar resizes the canvas, which reaches the engine as an ordinary window resize. Why: [ADR-044](adr/ADR-044-settings-reach-a-running-game.md#title-bar).
