# Controllers work, and the on-screen button glyphs may name the wrong brand

Controller support is on by default. Cordial reads your pad from
`/dev/input/js*` and hands its buttons and sticks to Roblox, and that part is
tested.

**What is not established is which number tells Roblox your controller's
brand.** Roblox ships separate glyph sets for PlayStation, Xbox and a generic
pad, and picks between them with an integer whose meaning is not published
anywhere we can read. Cordial sends a value; it may be the wrong one. If it is,
**you will see the wrong brand of button prompt and every button will still
work.** Sober has the same fault from the same cause — its issues
[#584](https://github.com/vinegarhq/sober/issues/584) and
[#1810](https://github.com/vinegarhq/sober/issues/1810) are exactly this.

Try other values if the glyphs look wrong:

```bash
CORDIAL_GAMEPAD_TYPE=1 cordial      # then 2, 3, ...
```

**If you find the value that draws your controller's own glyphs, please
[open an issue](https://github.com/luohoa97/cordial/issues) and say which pad
and which number.** That settles it for everyone, and it is the one thing we
cannot work out without a controller in front of the engine. Cordial prints the
value it used at launch, once, the first time it sees a pad.

Force feedback is absent rather than broken: there is no rumble, deliberately,
because a rumble call that silently does nothing is worse than none.

```bash
CORDIAL_MONITOR=1 CORDIAL_FULLSCREEN=1 cargo run --release --bin cordial-run -- \
  --lib-dir /path/to/lib/x86_64 --apk /path/to/base.apk \
  --host-libc --game-activity --run 30
```

`cordial-run --help` lists the rest, and `CORDIAL_GAMEPAD=0` turns controller
support off entirely.
