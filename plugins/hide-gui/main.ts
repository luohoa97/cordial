// Unlock Roblox's own interface-hiding shortcuts, by naming a group.
//
// Roblox ships the shortcuts and gates them: `DFIntCanHideGuiGroupId` names a
// group, and the keys only work for accounts that are in it. Bloxstrap's group
// is the one they are documented against, so it is the default here.
//
// **The flag is real and that was checked rather than taken on trust.**
// `CanHideGuiGroupId` is a string in this build's `libroblox.so`, next to
// `ScreenshotHudHideGuisApi`. A flag name copied out of a forum post and never
// looked for is how a plugin comes to set something the engine has never heard
// of and report success.
//
// **What this cannot do.** It cannot tell whether you are in the group --
// that would mean asking Roblox's web API on your behalf, which is a network
// call this plugin has no business making and no capability to make. So if the
// shortcuts do nothing, the group is the first thing to check, and the
// manifest's description says so where somebody will read it.
//
// No `entry`-less shortcut: this could almost be a static `flags.json` in the
// plugin directory, which `flags.rs` already reads. It is code because the
// group has to be configurable, and a preference is only readable by something
// that runs. See ADR-021 on why that is a property of this plugin and not a
// category it belongs to.

const enc = new TextEncoder();
const dec = new TextDecoder();

let nextId = 1;
const pending = new Map<number, (r: any) => void>();

// A push is not a reply. A reply carries `status` and the `id` it answers; a
// push carries `event` and no id, so a dispatcher that only looks up `id`
// drops every push in silence -- which is how the `cordial/init` handshake
// used to vanish in the other plugins. Copy this shape, not the lookup-only one.
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

/// Bloxstrap's group. Public entry, so joining is one click.
const DEFAULT_GROUP = "32380007";

// Cordial pushes `cordial/init` with this plugin's preference answers before
// the plugin asks for anything. Waiting costs no round trip; waiting forever
// would turn a host that stopped sending it into a plugin that never speaks.
function waitForInit(ms: number): Promise<any | null> {
  return new Promise((resolve) => {
    const timer = setTimeout(() => resolve(null), ms);
    onPush = (p) => {
      if (p.event !== "cordial/init") return;
      clearTimeout(timer);
      onPush = () => {};
      resolve(p.payload ?? null);
    };
  });
}

const init = await waitForInit(2000);
let answers = init?.preferences ?? null;

if (answers === null || typeof answers !== "object") {
  const got = await call("preferences.get");
  if (got.status === "ok") {
    answers = got.result;
  } else {
    await log(
      `could not read your preferences: ${got.status}` +
        (got.capability ? ` (needs ${got.capability})` : "") +
        `. Using ${DEFAULT_GROUP}.`,
    );
    answers = {};
  }
}

const raw = typeof answers.group_id === "string" ? answers.group_id.trim() : "";

// Blank is a choice, not a mistake: the manifest offers it as the way to turn
// the feature off, so it clears the flag rather than falling back to the
// default. Falling back would make the field impossible to empty.
let group: string | null = DEFAULT_GROUP;
if (raw === "") {
  group = answers.group_id === undefined ? DEFAULT_GROUP : null;
} else if (/^\d+$/.test(raw)) {
  group = raw;
} else {
  // Said rather than silently corrected. A setting that looks applied and is
  // not is the failure this project keeps finding in its own code.
  await log(
    `group is ${JSON.stringify(answers.group_id)}, which is not a group id ` +
      `(digits only). Using ${DEFAULT_GROUP}.`,
  );
}

// `{values: {...}}` is what `plugin_host.rs`'s `flags.set` requires; it refuses
// anything else with "flags.set needs a values object".
const set = await call("flags.set", {
  values: group === null ? {} : { DFIntCanHideGuiGroupId: group },
});

if (set.status !== "ok") {
  // Naming the capability, because "denied" without saying what was needed is
  // the error somebody files a bug about instead of granting.
  await log(
    `could not set the flag: ${set.status}` +
      (set.capability ? ` (needs ${set.capability})` : "") +
      (set.message ? ` (${set.message})` : ""),
  );
} else if (group === null) {
  await log("interface hiding turned off; the flag is cleared at the next launch.");
} else {
  await log(
    `interface hiding will use group ${group} from the next launch. The shortcuts ` +
      `only work if your account is in that group -- Cordial cannot check, so if ` +
      `nothing happens, check that first. Ctrl+Shift+G hides the Roblox menus and ` +
      `top bar, Ctrl+Shift+C game-defined screen GUIs, Ctrl+Shift+N names and tags, ` +
      `Ctrl+F+B 3D-space GUIs.`,
  );
}
