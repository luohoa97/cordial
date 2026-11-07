// Display FPS — the engine's own frame counter, on demand.
//
// **Why this exists.** Cordial's present counter reads the host Vulkan
// swapchain, so under a GL ES backend it reports zero and is useless — the
// instrument being blind, not the client stalling. A plugin cannot be handed
// telemetry either: the capability table brokers effects, never channels
// (ADR-007), and nothing in it returns a frame time.
//
// So this does not measure. It writes `FFlagDebugDisplayFPS`, and the engine
// draws its own counter from its own scheduler, on every backend. That is also
// the only reading that answers the question people actually ask, which is
// whether the game feels smooth.
//
// Note the payload shape. `flags.set` takes `{values: {...}}`, not a
// `{key, value}` pair — see the note at the head of `fps-flex/main.ts` for a
// shipped plugin that sent the pair shape and silently did nothing for weeks.

const enc = new TextEncoder();
const dec = new TextDecoder();

let nextId = 1;
const pending = new Map<number, (r: any) => void>();

// A reply carries `id` and `status`; a push carries `event` and no id. A
// dispatcher that looks only at `pending.get(res.id)` drops every event on the
// floor, which is how the handshake below disappeared once before.
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

const FLAG = "FFlagDebugDisplayFPS";

// Cordial pushes `cordial/init` before the plugin asks for anything, carrying
// the answers to the preferences the manifest declares. A short wait then ask:
// waiting forever would turn a host that stopped pushing into a plugin that
// starts and never speaks.
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
const answers = init?.preferences ?? null;

const OFF = "off";
const ON = "on";
const MODES = [OFF, ON];
const DEFAULT_MODE = OFF;

function parse(raw: unknown): string {
  const wanted = typeof raw === "string" ? raw.trim().toLowerCase() : "";
  return MODES.includes(wanted) ? wanted : DEFAULT_MODE;
}

let mode = parse(answers?.displayFps);

async function apply(wanted: string): Promise<void> {
  if (wanted === mode) return;

  const res = await call("flags.set", { values: { [FLAG]: wanted === ON } });
  const status = res?.status ?? res?.result?.status;

  // A refused write leaves the preference reading as chosen while nothing is
  // drawn, which is worse than saying so — so say so.
  if (status !== undefined && status !== "ok" && status !== "success") {
    await log(`Could not set ${FLAG}: ${status}`);
    return;
  }

  mode = wanted;
  await log(
    wanted === ON
      ? "Display FPS on. The engine draws it in the corner of the client."
      : "Display FPS off.",
  );
}

// Cordial pushes when preferences change, so this follows the UI rather than
// only reading the answer once at startup.
onPush = (p) => {
  if (p.event === "cordial/preferencesChanged") {
    void apply(parse(p.payload?.preferences?.displayFps ?? p.payload?.displayFps));
  }
};

await apply(mode);
