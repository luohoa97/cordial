# Cordial Z — shape guide for tracing by hand

Everything below is plain geometry. No masks, no clipPaths, no gradients baked in.
Draw these as four separate shapes on separate layers (or all on one paint layer).

## Canvas

Work on a **420 × 420 px** canvas, then crop to 340–440 and export square.
Flatpak-validate-icon rejects non-square exports, so keep it square.

Krita default canvas is fine — just set width and height both to 420.

## Centre point

Everything is measured from one origin. Put it at **(340, 240)** in the SVG's
own coordinates. In a fresh 420×420 canvas that centre is at **(210, 210)**.

Use those numbers on your canvas: **centre = 210, 210.**

---

## Shape 1 — the body (rotated rounded square)

- **Size:** 300 × 300
- **Corner radius:** 40
- **Position:** top-left corner at **(90, 60)** on your 420 canvas
  (that is 340−150, 240−150 with the origin shifted to a 420 canvas)
- **Rotation:** **13° clockwise**, about the centre (210, 210)

This is the silhouette. Same as upstream Cordial, on purpose — keep the shape
people already recognise, change only what happens inside it.

## Shape 2 — the bar (rotated, full width)

- **Size:** 380 wide × **24 tall**
- **Position:** top-left at **(30, 198)**
  (centre y = 240, so 240−12 = 228 in SVG coords → 198 in 420 coords)
- **Rotation:** **13° clockwise**, same pivot (210, 210)

The bar must be the same rotation as the body, or the fork marker stops reading.

## Shape 3 — the centre notch (UPRIGHT — this is the change)

- **Size:** 100 × 100
- **Corner radius:** 18
- **Position:** top-left at **(160, 160)** — dead centre, no rotation at all

This square stays axis-aligned while the body is tilted. The disagreement
between the tilted bar and the upright square is the whole point of the fork.

## Shape 4 — this is what makes it a cut, not a stripe

The bar and the notch are **holes**, not paint.

Easiest order:
1. Draw body, bar and notch each on its own layer.
2. Select the bar layer and notch layer.
3. Layer → **Lock Layer** (Ctrl+L) — no wait, you want the opposite.

Correct method:
- Draw the body on layer "Body".
- Draw bar and notch on a layer called "Cut".
- Right-click the **Cut** layer → **Alpha → Lock Transparency**. Now nothing
  can paint into it.
- Select Body + Cut, then **Layer → Merge Down**, or use a mask instead.

**Cleaner alternative, avoids masks entirely:**
1. On the Body layer, draw the rounded square.
2. Switch to the **Rectangle Select** tool, draw a rect over the bar area,
   press **Delete**. That cuts the bar out.
3. Repeat with the Ellipse/Rect tool for the 100×100 notch, **Delete**.
4. Because the body is rotated, you'll want to rotate the *canvas* or build the
   shape rotated to begin with — otherwise your cuts land straight while the
   body is tilted.

**Easiest of all:** build the body already rotated. Draw the rounded square,
then hold **Shift** and rotate it 13° about its own centre before cutting. Then
the cuts are all axis-aligned in the same space and land predictably.

---

## Colours

Linear gradient, top-left → bottom-right, three stops:

| Stop | Colour | Where |
|---|---|---|
| 0% | `#FF1B6B` | raspberry |
| 45% | `#FF7A18` | orange |
| 100% | `#00C2A8` | teal |

Teal is the only change from upstream (they end on lime `#B4E600`).
Change those three hex values and nothing else if you want a different palette.

Sheen, optional — a white overlay at **26% opacity** fading to nothing by 60%,
same direction. Adds depth without hurting the small sizes.

---

## Small-size check

Preview at 32 px before you commit. The upright-notch-vs-tilted-bar read should
still be visible. If it turns to mush: make the bar thicker (24 → 30) and the
notch smaller (100 → 84). Same idea, just bolder.
