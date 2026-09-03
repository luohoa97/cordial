/**
 * The bridge, as one stateless HTTP handler.
 *
 * Two routes and nothing else: `POST /interactions` for Discord and
 * `POST /github` for the issue-comment webhook. No database, no gateway
 * connection, no background work -- so it runs anywhere a request can be
 * served, and losing the instance loses nothing.
 *
 * ## Why the configuration is passed in rather than read here
 *
 * **This module used to call `Deno.env.get` directly, and that quietly tied it
 * to one host.** Cloudflare Workers has no `Deno` and hands configuration to
 * the fetch handler as a second argument instead; a module that reaches for a
 * global at import time cannot run there at all. Passing a plain record in
 * means the same `build` serves both, and the host-specific part is the ten
 * lines of adapter in `serve_deno.ts` and `worker.ts`.
 *
 * See ADR-030 for why it is shaped this way, and `README.md` beside this file
 * for what has to be set up before it will do anything.
 */
import { Discord } from "./discord.ts";
import { appJwt, GitHub, importAppKey, installationToken } from "./github.ts";
import { handle } from "./interactions.ts";
import { Templates } from "./templates.ts";
import { importPublicKey, verifyRequest } from "./verify.ts";
import { relayFor, verifyGitHubSignature } from "./webhook.ts";

/** Whatever the host calls configuration: `Deno.env.toObject()`, or a Worker's `env`. */
export type Env = Record<string, string | undefined>;

export class ConfigError extends Error {}

export async function build(source: Env) {
  // Throws rather than exits, because on a Worker there is no process to exit
  // and the message has to reach a log instead.
  const required = (name: string): string => {
    const value = source[name];
    if (!value) {
      throw new ConfigError(`${name} is not set; see tools/discord-bridge/README.md`);
    }
    return value;
  };

  const owner = required("GITHUB_OWNER");
  const repo = required("GITHUB_REPO");
  const repoUrl = `https://github.com/${owner}/${repo}`;

  const appKey = await importAppKey(required("GITHUB_APP_PRIVATE_KEY"));
  const appId = required("GITHUB_APP_ID");
  const installation = required("GITHUB_INSTALLATION_ID");

  // Installation tokens last an hour. Cached in the instance because it is
  // free to do so, and re-minted from scratch if the instance is cold -- there
  // is deliberately nothing persistent to invalidate.
  let cached: { token: string; until: number } | null = null;
  const token = async (): Promise<string> => {
    if (cached && Date.now() < cached.until) return cached.token;
    const fresh = await installationToken(await appJwt(appId, appKey), installation);
    cached = {
      token: fresh.token,
      until: Date.parse(fresh.expires_at) - 60_000,
    };
    return cached.token;
  };

  const templates = new Templates({
    owner,
    repo,
    ref: source.GITHUB_REF_NAME ?? "main",
    token: source.GITHUB_READ_TOKEN,
  });

  const context = {
    forms: () => templates.forms(),
    github: new GitHub({ owner, repo }, token),
    discord: new Discord(required("DISCORD_BOT_TOKEN"), required("DISCORD_APPLICATION_ID")),
    threadChannelId: required("DISCORD_THREAD_CHANNEL_ID"),
    repoUrl,
  };

  const discordKey = await importPublicKey(required("DISCORD_PUBLIC_KEY"));
  const webhookSecret = required("GITHUB_WEBHOOK_SECRET");
  const selfLogin = source.GITHUB_APP_LOGIN ?? `${repo}-bridge`;

  /**
   * Hand slow follow-up work to the host.
   *
   * **On Cloudflare a promise left running when the handler returns is
   * cancelled**, so the fire-and-forget below -- which is correct under Deno,
   * where the process simply keeps going -- silently dropped every deferred
   * action. The first real bug report filed through the deployed bridge sat on
   * "Cordial Issues is thinking" for ever and no issue was created, because
   * `fileIssue` was killed before its first request went out.
   *
   * A Worker's `ctx.waitUntil` is the contract for exactly this: keep the
   * isolate alive until the promise settles. Hosts that need no such promise
   * pass nothing and get the old behaviour.
   */
  return async function serve(
    request: Request,
    waitUntil?: (promise: Promise<unknown>) => void,
  ): Promise<Response> {
    const background = (promise: Promise<unknown>) => {
      const guarded = promise.catch((error) => console.error(`follow-up: ${error}`));
      // Handed over *after* the catch, so an unhandled rejection can never
      // reach the host and fail the whole request.
      if (waitUntil) waitUntil(guarded);
    };

    const url = new URL(request.url);

    if (request.method === "GET" && url.pathname === "/health") {
      return Response.json({ ok: true, stale: templates.stale ?? null });
    }

    if (request.method !== "POST") return new Response("not found", { status: 404 });

    const body = await request.text();

    if (url.pathname === "/interactions") {
      // Discord requires a 401 specifically for a bad signature and will not
      // register an endpoint that answers anything else.
      const ok = await verifyRequest(
        discordKey,
        request.headers.get("x-signature-ed25519"),
        request.headers.get("x-signature-timestamp"),
        body,
      );
      if (!ok) return new Response("bad signature", { status: 401 });

      const { response, after } = await handle(context, JSON.parse(body));
      // Not awaited: Discord allows three seconds to acknowledge, which is the
      // whole point of deferring, and the follow-up edits the reply when it is
      // done.
      if (after) background(after());
      return Response.json(response);
    }

    if (url.pathname === "/github") {
      if (
        !await verifyGitHubSignature(
          webhookSecret,
          request.headers.get("x-hub-signature-256"),
          body,
        )
      ) {
        return new Response("bad signature", { status: 401 });
      }
      const relay = relayFor(JSON.parse(body), selfLogin);
      if (relay) {
        try {
          await context.discord.post(relay.threadId, relay.content);
        } catch (error) {
          // A deleted thread is the ordinary case and not an error worth
          // retrying: GitHub would redeliver forever against a channel that no
          // longer exists.
          console.error(`relay to ${relay.threadId}: ${error}`);
        }
      }
      return new Response("ok");
    }

    return new Response("not found", { status: 404 });
  };
}
