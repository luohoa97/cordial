// Discord Presence — a first-party plugin, not a special case.
//
// It listens for client lifecycle events and keeps Discord Rich Presence in
// step with them: presence.set on launch and ready, presence.clear on
// shutdown. It requests lifecycle.read and presence.set like any third-party
// plugin would, and is granted them the same way — see ADR-006's "first-party
// plugins are still plugins": nothing here is special-cased into core.
//
// Note what it does not do. It never learns where Discord's IPC socket is,
// never opens it, and cannot send anything down it except the presence
// payload built below — Cordial owns the connection (ADR-007). It also runs
// with no Deno permissions at all, the same as every other plugin, so none of
// that containment depends on this file behaving.
//
// The application id is Cordial's own by default, and the user may substitute
// their own through the preferences page this plugin declares. An id is not a
// secret -- it only selects whose name and icon Discord shows beside the
// activity -- which is why it can live in a manifest and in a text field
// rather than anywhere more careful.
//
// One honest limitation, stated rather than hidden: `push_lifecycle`'s payload
// is currently empty (see host.rs), because Cordial does not yet thread which
// game or place is running through to the lifecycle push -- that lives in
// cordial-runtime and this plugin was built without touching it. The
// `details`/`state` text below is therefore generic ("Using Cordial") rather
// than naming the game. `cordial_runtime::bloxstrap_rpc` now parses the
// protocol games use to say what they are, so the missing piece is the core
// event carrying it, not the parsing.

// What a running experience asked for, through BloxstrapRPC. Cordial parses
// the game's `print` output, folds the partial updates, and pushes the merged
// picture here -- see `cordial_runtime::game_log`. An empty payload means the
// player left and the game's presence goes with it.
const GAME_PRESENCE = "cordial/game.presence";
let fromGame: {
  details?: string;
  state?: string;
  start?: number;
  end?: number;
  place_id?: number;
  job_id?: string;
  // Cordial's own image keys. An empty string is the game clearing the slot,
  // which is not the same as never setting one -- the broker reads the
  // difference, so this must not collapse them into `undefined`.
  large_image_key?: string;
  large_text?: string;
  small_image_key?: string;
  small_text?: string;
} = {};

const enc = new TextEncoder();
const dec = new TextDecoder();

// Cordial's registered Discord application. Used unless the user has put their
// own id in the preferences page, which `plugin.json` declares.
const DEFAULT_CLIENT_ID = "1543200871767212062";

// Discord's own ids are snowflakes: 17 to 20 digits today, and the range is
// widened here rather than pinned so a future one is not refused by us before
// Discord ever sees it. `presence.set` validates digits again on Cordial's side
// (presence.rs), so this check exists to give the user a reason rather than to
// be the guard.
function usableClientId(value: unknown): value is string {
  return typeof value === "string" && /^[0-9]{17,20}$/.test(value);
}

let clientId = DEFAULT_CLIENT_ID;

// Called once, from the handshake. A blank answer is the documented way to say
// "use Cordial's", so it is not a complaint; anything non-blank that cannot
// work is, because a user who typed an id and silently kept appearing as
// Cordial would have no way to tell it had been ignored.
function adoptPreferences(preferences: Record<string, unknown> | null) {
  // `null` means settings.read was refused rather than that nothing was set.
  // Nothing to say about it: refusing the capability is a legitimate choice and
  // the default still works.
  if (preferences === null || preferences === undefined) return;
  const chosen = preferences["client_id"];
  if (chosen === undefined || chosen === "") return;
  if (usableClientId(chosen)) {
    clientId = chosen;
    log(`using the configured Discord application id ${chosen}`);
  } else {
    log(
      `ignoring the configured Discord application id ${JSON.stringify(chosen)}: ` +
        `it is not 17 to 20 digits, so Discord would refuse it. Appearing as Cordial instead.`,
    );
  }
}

let nextId = 1;
const pending = new Map<number, (r: any) => void>();

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
      // A push (a lifecycle event Cordial sends unasked) has no id; a reply
      // to one of our own calls always does. See protocol.rs's Push type.
      //
      // Deliberately not awaited: onLifecycleEvent makes its own calls back
      // out (presence.set, log.write) whose replies arrive on this very
      // same stdin loop. Awaiting it here would block this loop from ever
      // reading those replies, so it would deadlock against itself on the
      // first lifecycle event.
      if (msg.id === undefined) {
        // `cordial/init` is the handshake every plugin is sent before it has
        // asked for anything, carrying its saved settings and the user's
        // answers to the preferences it declared. Taking the application id
        // from here rather than calling `preferences.get` costs no round trip,
        // and it arrives before any lifecycle event can, so the first
        // `presence.set` already uses the configured id. This branch also has
        // to exist at all so the handshake is not mistaken for a lifecycle
        // event — which it was, and was reported as an unrecognised one, until
        // it did.
        if (msg.event === "cordial/init") {
          adoptPreferences(msg.payload?.preferences ?? null);
        } else if (msg.event === GAME_PRESENCE) {
          onGamePresence(msg.payload);
        } else {
          onLifecycleEvent(msg.event);
        }
      } else {
        pending.get(msg.id)?.(msg);
        pending.delete(msg.id);
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

// Core events are namespaced `cordial/...` (ADR-026). The prefix is reserved
// and cannot be declared by a plugin, so a name that arrives with it is one
// Cordial published -- which is the point of carrying it. These used to arrive
// as bare "launch"/"ready"/"shutdown"; anything still matching those is looking
// for events that no longer exist.
const LAUNCH = "cordial/client.launch";
const READY = "cordial/client.ready";
const SHUTDOWN = "cordial/client.shutdown";

// **The presence has to be re-sent, and this is the whole of why.**
//
// Reported as "on discord Cordial isnt automatically detected as Rich RPC and
// added, its broken", and asked precisely: "what if discord isnt open then the
// user opens discord after a while?"
//
// This plugin used to call `presence.set` exactly twice -- once on launch, once
// on ready -- and then nothing until shutdown. Cordial's broker reconnects to
// Discord's socket on *every* `presence.set` (see `presence.rs`'s
// `ensure_connected`, which deliberately has no retry loop of its own because
// the caller is expected to be the retry). So the retry existed on one side and
// nobody ever exercised it: if Discord was not running at those two moments,
// presence never appeared, and no amount of waiting fixed it.
//
// The same gap covers Discord restarting mid-session. Discord drops the
// activity when the IPC connection goes, and nothing here re-established one.
//
// Twenty seconds because Discord rate-limits activity updates to roughly one
// every fifteen, so this is the slowest cadence that is comfortably inside the
// limit and still picks Discord up within half a minute of it opening.
const RESEND_MS = 20_000;

// Fixed at the first event rather than recomputed per send, so Discord's
// "elapsed" counter keeps counting instead of resetting to zero every twenty
// seconds -- which is what re-sending a fresh `start` would do, and it would
// look like a bug in Cordial rather than in this line.
let startedAt: number | null = null;
// Whether a session is up at all. This was a state string, which no longer
// exists; it is a flag now because the only thing the heartbeat needs to know
// is whether to keep sending.
let running = false;
let lastStatus: string | null = null;
// `ReturnType<typeof setInterval>` rather than `number`: Deno types this as
// `Timeout`, not the browser's numeric handle, and hardcoding `number` fails
// `deno check`.
let heartbeat: ReturnType<typeof setInterval> | null = null;

async function pushPresence(reason: string) {
  if (!running || startedAt === null) return;
  // **No `details` and no `state`, deliberately.**
  //
  // These used to be "Using Cordial" and "Starting up"/"In session", and it
  // was reported stuck: "its stuck on starting up". Two separate faults, one
  // fix. The state only advanced on `client.ready`, so if that event does not
  // arrive the presence says "Starting up" for the whole session -- a status
  // line that can be wrong is worse than no status line. And Discord already
  // renders "Playing Cordial" from the application itself, so both fields were
  // saying, less reliably, something the header said anyway.
  //
  // What is left is the application name, the icon, the elapsed timer, and the
  // button the broker adds. "Playing Cordial", which is what was asked for and
  // is the only claim here that cannot go stale.
  // **The game wins where it said something, and only there.** Cordial's own
  // presence is "Playing Cordial" and an elapsed timer; an experience using
  // BloxstrapRPC replaces the lines it sets and leaves the rest alone, which
  // is what makes a game's own presence look like the game's rather than like
  // Cordial's with a subtitle.
  //
  // `start` is the game's when it gave one, because an experience timing a
  // round means that, and Cordial's session clock would be wrong for it.
  const res = await call("presence.set", {
    client_id: clientId,
    ...(fromGame.details !== undefined ? { details: fromGame.details } : {}),
    ...(fromGame.state !== undefined ? { state: fromGame.state } : {}),
    ...(fromGame.end !== undefined ? { end: fromGame.end } : {}),
    // The broker turns these into the buttons; this plugin never builds a URL.
    // A plugin that could would be publishing an arbitrary link under
    // Cordial's name and icon, which is not what `presence.set` grants.
    ...(fromGame.place_id !== undefined ? { place_id: fromGame.place_id } : {}),
    ...(fromGame.job_id !== undefined ? { job_id: fromGame.job_id } : {}),
    // **The game's picture instead of Cordial's icon, when it set one.** With
    // no assets on the activity Discord falls back to the application's own
    // icon, so a game that asked for cover art got Cordial's logo -- which is
    // the same "the game wins where it said something" rule the text fields
    // already follow, applied to the one field that was not carrying it.
    ...(fromGame.large_image_key !== undefined ? { large_image_key: fromGame.large_image_key } : {}),
    ...(fromGame.large_text !== undefined ? { large_text: fromGame.large_text } : {}),
    ...(fromGame.small_image_key !== undefined ? { small_image_key: fromGame.small_image_key } : {}),
    ...(fromGame.small_text !== undefined ? { small_text: fromGame.small_text } : {}),
    start: fromGame.start ?? startedAt,
  });
  // Logged only when the answer changes. At one send every twenty seconds an
  // unconditional line would be three an hour per state and would bury
  // everything else in the plugin log; a *change* is the interesting event --
  // Discord appearing, or going away.
  if (res.status !== lastStatus) {
    lastStatus = res.status;
    await log(
      `presence.set (${reason}) came back: ${res.status}` +
        (res.status === "ok"
          ? ""
          : `. Retrying every ${RESEND_MS / 1000}s -- if Discord is not running yet, ` +
            `it will be picked up when it is.`),
    );
  }
}

async function onGamePresence(payload: unknown) {
  const p = (payload ?? {}) as Record<string, unknown>;
  fromGame = {
    details: typeof p.details === "string" ? p.details : undefined,
    state: typeof p.state === "string" ? p.state : undefined,
    start: typeof p.start === "number" ? p.start : undefined,
    end: typeof p.end === "number" ? p.end : undefined,
    place_id: typeof p.place_id === "number" ? p.place_id : undefined,
    job_id: typeof p.job_id === "string" ? p.job_id : undefined,
    // Opaque keys Cordial issued, not ids and not URLs. It resolves the
    // picture itself and this only echoes the key back; a plugin that could
    // compose the string would be publishing an arbitrary link under Cordial's
    // name, which is the same reason the buttons are the broker's job.
    large_image_key: typeof p.large_image_key === "string" ? p.large_image_key : undefined,
    large_text: typeof p.large_text === "string" ? p.large_text : undefined,
    small_image_key: typeof p.small_image_key === "string" ? p.small_image_key : undefined,
    small_text: typeof p.small_text === "string" ? p.small_text : undefined,
  };
  // Forced rather than left to the heartbeat: a game setting its presence and
  // Discord showing it up to twenty seconds later would read as broken.
  lastStatus = null;
  await pushPresence("game");
}

async function onLifecycleEvent(event: string) {
  if (event === LAUNCH || event === READY) {
    startedAt ??= Math.floor(Date.now() / 1000);
    running = true;
    // Forced, so a reader sees the answer to this event rather than the answer
    // to the last one up to twenty seconds ago.
    lastStatus = null;
    await pushPresence(event);
    heartbeat ??= setInterval(() => {
      pushPresence("retry").catch(() => {});
    }, RESEND_MS);
  } else if (event === SHUTDOWN) {
    if (heartbeat !== null) {
      clearInterval(heartbeat);
      heartbeat = null;
    }
    running = false;
    const res = await call("presence.clear");
    await log(`presence.clear on shutdown came back: ${res.status}`);
  } else {
    await log(`ignoring unrecognised lifecycle event ${JSON.stringify(event)}`);
  }
}

const subscribed = await call("lifecycle.subscribe");
await log(`lifecycle.subscribe came back: ${subscribed.status}`);
