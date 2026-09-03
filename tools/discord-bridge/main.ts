#!/usr/bin/env -S deno run --allow-net --allow-env
/**
 * The bridge, as one stateless HTTP handler.
 *
 * Two routes and nothing else: `POST /interactions` for Discord and
 * `POST /github` for the issue-comment webhook. No database, no gateway
 * connection, no background work -- so it runs anywhere a request can be
 * served, and losing the instance loses nothing.
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

function required(name: string): string {
  const value = Deno.env.get(name);
  if (!value) {
    console.error(`${name} is not set; see tools/discord-bridge/README.md`);
    Deno.exit(2);
  }
  return value;
}

export async function build() {
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
    ref: Deno.env.get("GITHUB_REF_NAME") ?? "main",
    token: Deno.env.get("GITHUB_READ_TOKEN"),
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
  const selfLogin = Deno.env.get("GITHUB_APP_LOGIN") ?? `${repo}-bridge`;

  return async function serve(request: Request): Promise<Response> {
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
      if (after) {
        // Deliberately not awaited: the three-second acknowledgement budget is
        // the point of deferring, and the follow-up edits the reply when it is
        // done. A rejection here would otherwise be an unhandled one.
        after().catch((error) => console.error(`follow-up: ${error}`));
      }
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

if (import.meta.main) {
  Deno.serve({ port: Number(Deno.env.get("PORT") ?? 8000) }, await build());
}
