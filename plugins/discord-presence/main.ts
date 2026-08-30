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

async function onLifecycleEvent(event: string) {
  if (event === LAUNCH || event === READY) {
    const res = await call("presence.set", {
      client_id: clientId,
      details: "Using Cordial",
      state: event === LAUNCH ? "Starting up" : "In session",
      start: Math.floor(Date.now() / 1000),
    });
    await log(`presence.set on ${event} came back: ${res.status}`);
  } else if (event === SHUTDOWN) {
    const res = await call("presence.clear");
    await log(`presence.clear on shutdown came back: ${res.status}`);
  } else {
    await log(`ignoring unrecognised lifecycle event ${JSON.stringify(event)}`);
  }
}

const subscribed = await call("lifecycle.subscribe");
await log(`lifecycle.subscribe came back: ${subscribed.status}`);
