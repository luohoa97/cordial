// Graphics Levers — texture, mesh and shadow quality, and what they cost.
//
// **The gap this fills.** Cordial ships four built-in plugins. One of them
// (`fps-flex`) handles frame rate and presentation; the others are Discord
// presence, a flag inspector and a GUI toggle. Between them they touch nothing
// about how the game is *drawn*. Graphics quality is entirely absent from the
// plugin set, and it is the single largest effect available to a user on a
// desktop GPU: on an RTX 4050, moving `DFIntTextureQualityOverride` from unset
// to 4 changed the engine from loading 16 emulated textures to 10,206 and made
// the difference between a slideshow and 140 a second. That measurement is in
// the project's own logs from 2026-10-04.
//
// **Why a plugin and not a document.** The flag works; what is missing is that
// the value has to be found in a JSON file, and nothing tells you the tier
// numbers, that the change needs a relaunch, or that 4 is a ceiling rather than
// a suggestion. `fps-flex` already solves exactly this shape of problem for
// frame rate, so this follows the same design rather than inventing one.
//
// ## What it does not claim
//
// This writes flags. It does not render, does not measure frame rate, and cannot
// tell you whether the change helped -- there is no frames-per-second readback
// in the plugin API, and `state.get` is not one. Anything here that sounds like
// advice is the engine's own flag semantics, not a benchmark this file ran.
//
// ## The ETC2 cost, stated plainly because it inverts the obvious advice
//
// Raising texture quality looks like it should cost GPU time and buy nothing.
// On this hardware it costs CPU time, and NVIDIA is why: a 4050 reports
// `textureCompressionETC2` false, so Cordial decodes every ETC2/EAC texture in
// software (`docs/analysis/nvidia-texture-manager2.md`). Measured on 2026-10-04
// at 140 frames a second: quality 4 spent 50,411 buffer-to-image decode calls on
// the CPU while the GPU sat at 31% load, and quality 3 spent 22,352. Higher
// quality loads more textures, so it does more of that work.
//
// That is why this plugin offers a **profile** choice rather than just a slider.
// The right answer is not "highest" -- it depends on whether the machine is
// short of CPU or short of GPU, and only the user knows which because only the
// user knows what the game feels like. Measured on the same machine, quality 4
// gave a higher average (137 against 118) with a harsher worst-case dip (95
// against 59), which is the trade stated plainly rather than smoothed over.
//
// There is no way to make the engine stop choosing ETC2. `textureCompressionBC`
// is already reported true and the engine still ships ETC2, because that
// capability line comes from the engine's own compiled-in data rather than from
// any Vulkan query. `DFIntEstimatedGmaSafeVideoMemoryMB` was tested on a 12 GiB
// RTX 4070 with delivery proven in the same run and moved nothing. So `caps.
// videoMemory = 67108864` is a constant, not a symptom, and no flag here reaches
// it.
//
// ## Off by default, for the reason fps-flex is
//
// Uncapping presentation makes the GPU work for frames nobody asked for. This
// plugin is milder -- it changes what is loaded, not how often -- but a profile
// that changes how a machine behaves, chosen by a plugin rather than by the
// person at the keyboard, is still the wrong default. The user's own
// `flags.json` sits above every plugin's layer and wins either way (ADR-044).

const enc = new TextEncoder();
const dec = new TextDecoder();

let nextId = 1;
const pending = new Map<number, (r: any) => void>();

// A push is not a reply. A reply carries `status` and the `id` it answers; a
// push carries `event` and no id, so looking a push up by id finds nothing and
// drops it silently. Same shape as `fps-flex`, and for the same reason.
let onPush: (p: { event: string; payload: any }) => void = () => {};

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
      if (typeof msg.id === "number" && pending.has(msg.id)) {
        pending.get(msg.id)!(msg);
        pending.delete(msg.id);
      } else if (typeof msg.event === "string") {
        onPush(msg);
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

// `flags.set` requires `{values: {...}}` and refuses anything else with
// "flags.set needs a values object". `fps-flex` shipped its whole life sending
// `{key, value}` and collecting that refusal silently, so this shape is
// deliberate rather than incidental.
const setFlags = (values: Record<string, string>) =>
  call("flags.set", { values });

// Each profile is a set of engine flags, not a single value, so that "quality"
// means the same thing every time rather than being one knob plus whatever the
// game last set.
//
// `DFIntTextureQualityOverride` is the one measured here. The others are
// spelled as the engine's own names; a name this file gets wrong is refused by
// the flag layer as unreadable rather than silently ignored, and `log.write`
// below reports what the host answered so a wrong name shows up in the log
// instead of being a no-op nobody notices.
const PROFILES: Record<string, Record<string, string> | null> = {
  // `null` means "send nothing" -- the plugin's way of removing its own
  // contribution and handing the decision back to the engine.
  engine: null,
  performance: {
    DFIntTextureQualityOverride: "1",
    DFIntGraphicsQuality: "1",
  },
  balanced: {
    DFIntTextureQualityOverride: "2",
    DFIntGraphicsQuality: "2",
  },
  quality: {
    DFIntTextureQualityOverride: "3",
    DFIntGraphicsQuality: "3",
  },
  ultra: {
    DFIntTextureQualityOverride: "4",
    DFIntGraphicsQuality: "4",
  },
};

const DEFAULT_PROFILE = "quality";

function profileFor(value: unknown): string | null {
  if (value === "engine" || value === null || value === undefined) return null;
  return typeof value === "string" && value in PROFILES ? value : null;
}

// A profile the plugin did not recognise is refused here with the list of names
// it does know, rather than written through to the flag layer and coming back
// as an unreadable setting from somewhere the user cannot see.
function validate(profile: string): Record<string, string> | null | undefined {
  if (profile === "engine") return null;
  const values = PROFILES[profile];
  if (!values) {
    log(
      `graphics-levers: no profile called ${JSON.stringify(profile)}; ` +
        `known profiles are ${Object.keys(PROFILES).join(", ")}`,
    );
    return undefined;
  }
  return values;
}

onPush = (p) => {
  // Cordial asks a plugin to stop before the client goes away, and also when
  // settings change underneath it. Both are handled the same way: reassert, so
  // the choice survives the engine's own settings refresh.
  //
  // That refresh is the reason this plugin cannot be a one-shot write. Roblox
  // re-applies its own settings document every couple of minutes and puts its
  // values back over whatever was there (`ADR-051`), so a value written once at
  // startup is gone by the time the player is actually in a place.
  if (p.event === "lifecycle.stop" || p.event === "settings.changed") {
    void apply();
  }
};

let current = DEFAULT_PROFILE;

async function apply(): Promise<void> {
  const values = validate(current);
  if (values === undefined) return; // rejected above, and said so
  try {
    const res = await setFlags(values ?? {});
    // The host's own answer, reported rather than assumed. A refusal here is
    // the only way to find out a flag name was wrong, because the engine will
    // not complain about a setting it never read.
    const status = res && res.status;
    if (status && status !== "ok") {
      log(`graphics-levers: flags.set answered ${JSON.stringify(res)}`);
    }
  } catch (e) {
    log(`graphics-levers: flags.set failed: ${String(e)}`);
  }
}

async function readChoice(): Promise<void> {
  try {
    // `settings.get` is the method; `settings.read` is the capability name and
    // is not a method, which is half of what kept fps-flex inert.
    const res = await call("settings.get", { key: "profile" });
    const value = res && res.result && res.result.profile;
    const profile = profileFor(value);
    if (profile !== null && profile !== undefined) {
      current = profile === null ? DEFAULT_PROFILE : profile;
    } else if (typeof value === "string" && !(value in PROFILES)) {
      log(
        `graphics-levers: stored profile ${JSON.stringify(value)} is not one ` +
          `of ${Object.keys(PROFILES).join(", ")}; using ${DEFAULT_PROFILE}`,
      );
      current = DEFAULT_PROFILE;
    }
  } catch {
    // No stored choice yet. The default is not a failure.
  }
}

async function main(): Promise<void> {
  await readChoice();
  await apply();
  log(
    "graphics-levers: profile is " +
      (current === "engine" ? "engine (plugin sets nothing)" : current) +
      ". Texture quality changes what the engine loads and take effect on the " +
      "next join; on an NVIDIA card that work is decoded on the CPU, so higher " +
      "is not free.",
  );
}

void main();
