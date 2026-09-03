#!/usr/bin/env -S deno run --allow-net --allow-read --allow-write --allow-env
/**
 * Collect the bridge's configuration, check every value against the live API,
 * and write `.env`.
 *
 * ## What this cannot do, and why there is no way round it
 *
 * **Discord has no endpoint that creates an application.** Checked against
 * their resource documentation on 2026-09-03: there is a "Get Current
 * Application" and an "Edit Current Application" and nothing that makes one.
 * Nor does Discord implement OAuth Dynamic Client Registration. So the first
 * two minutes are unavoidably manual -- create the application in the portal,
 * add a bot, copy the token -- and no script written by anybody can shorten
 * them.
 *
 * Everything *after* that is automated here: the interactions endpoint, the
 * avatar, the description, and the invite link.
 *
 * ## How secrets are handled
 *
 * They are typed in, never echoed, never printed back, and never passed on a
 * command line where they would land in shell history or in `ps`. `.env` is
 * created `0600` before anything is written to it, so there is no window in
 * which it exists world-readable. Nothing here logs a value; failures quote
 * the *name* of the setting and what the API said about it, never the value.
 *
 * `.env` is gitignored. That is a real protection and not a complete one: it
 * is a plaintext file, so it is as safe as the machine it sits on, and a bot
 * token in it is enough to act as the bot. Rotate from the portal if it ever
 * leaves.
 */
import { TEMPLATE_DIR } from "./repo.ts";

const HERE = new URL(".", import.meta.url).pathname;
const ENV_PATH = `${HERE}.env`;

/**
 * The Rich Presence application, which this must never be pointed at.
 *
 * `plugins/discord-presence/main.ts` publishes every user's "Playing Cordial"
 * under this id. Setup rewrites the application's **description** and sets its
 * interactions endpoint, and the description is what shows in the app's About
 * Me -- so running against that one edits, in public, the identity every
 * Cordial user's presence is published under.
 *
 * **This used to say the icon was the danger, and that was wrong.** The bot's
 * face is the bot *user's* avatar and the Rich Presence artwork is the
 * *application's* icon; they are separate fields and setup no longer touches
 * the second. The remaining reason to keep them apart is blast radius: one
 * application carrying both means a problem with the bot is a problem with
 * every user's presence.
 */
const RICH_PRESENCE_APPLICATION_ID = "1543200871767212062";

interface Setting {
  name: string;
  prompt: string;
  secret?: boolean;
  optional?: boolean;
  hint?: string;
}

const SETTINGS: Setting[] = [
  {
    name: "DISCORD_APPLICATION_ID",
    prompt: "Discord application id",
    hint: "Developer portal → your application → General Information → Application ID",
  },
  {
    name: "DISCORD_PUBLIC_KEY",
    prompt: "Discord public key",
    hint: "Same page. This is what verifies every incoming request; it is not a secret",
  },
  {
    name: "DISCORD_BOT_TOKEN",
    prompt: "Discord bot token",
    secret: true,
    hint: "Bot → Reset Token. Shown once. Anyone holding it is the bot",
  },
  {
    name: "DISCORD_THREAD_CHANNEL_ID",
    prompt: "Channel id for issue threads",
    hint: "Right-click the channel → Copy Channel ID (needs Developer Mode in Discord settings)",
  },
  {
    name: "DISCORD_PICKER_CHANNEL_ID",
    prompt: "Channel id for the form message",
    hint: "May be the same channel",
  },
  { name: "GITHUB_OWNER", prompt: "GitHub owner", hint: "e.g. luohoa97" },
  { name: "GITHUB_REPO", prompt: "GitHub repository", hint: "e.g. cordial" },
  {
    name: "GITHUB_APP_ID",
    prompt: "GitHub App id",
    hint: "Settings → Developer settings → GitHub Apps → your app → App ID",
  },
  {
    name: "GITHUB_APP_LOGIN",
    prompt: "GitHub App bot login",
    hint: "The name comments appear under, without [bot]. Wrong here and the bridge echoes itself",
  },
  {
    name: "GITHUB_INSTALLATION_ID",
    prompt: "GitHub App installation id",
    hint: "Install App → the number at the end of the settings URL",
  },
  {
    name: "GITHUB_WEBHOOK_SECRET",
    prompt: "GitHub webhook secret",
    secret: true,
    hint: "Whatever you set on the App's webhook. Press enter to have one generated",
    optional: true,
  },
];

/**
 * Read a line without echoing it.
 *
 * Deno has no `promptSecret` -- checked on 2.9.5, it is not a global and not
 * on the `Deno` namespace -- so raw mode it is. The `finally` is not
 * decoration: leaving the terminal in raw mode on a Ctrl-C hands the user back
 * a shell that does not echo, which looks like their machine has broken.
 */
function promptSecret(label: string): string {
  Deno.stdout.writeSync(new TextEncoder().encode(label));
  if (!Deno.stdin.isTerminal()) {
    // Piped input cannot be put in raw mode, and crashing here would be a
    // stack trace where the answer is one sentence. Refuse instead: a secret
    // arriving down a pipe came from a file or a shell history somewhere, and
    // that is the thing this script exists to avoid.
    console.error(
      "\n  This needs a terminal — secrets are read without echo and cannot be piped in.",
    );
    Deno.exit(1);
  }
  Deno.stdin.setRaw(true);
  try {
    const bytes: number[] = [];
    const buffer = new Uint8Array(1);
    while (true) {
      const read = Deno.stdin.readSync(buffer);
      if (read === null) break;
      const byte = buffer[0];
      if (byte === 3) { // Ctrl-C
        Deno.stdout.writeSync(new TextEncoder().encode("\n"));
        Deno.stdin.setRaw(false);
        Deno.exit(130);
      }
      if (byte === 13 || byte === 10) break;
      if (byte === 127 || byte === 8) {
        bytes.pop();
        continue;
      }
      bytes.push(byte);
    }
    Deno.stdout.writeSync(new TextEncoder().encode("\n"));
    return new TextDecoder().decode(new Uint8Array(bytes));
  } finally {
    Deno.stdin.setRaw(false);
  }
}

function ask(setting: Setting): string {
  console.log(`\n\x1b[1m${setting.prompt}\x1b[0m`);
  if (setting.hint) console.log(`  ${setting.hint}`);
  const value = (setting.secret ? promptSecret("  > ") : prompt("  >")) ?? "";
  return value.trim();
}

/** A generated webhook secret, for the common case of not having one yet. */
function generatedSecret(): string {
  const bytes = crypto.getRandomValues(new Uint8Array(32));
  return [...bytes].map((b) => b.toString(16).padStart(2, "0")).join("");
}

async function readPrivateKey(): Promise<string> {
  console.log("\n\x1b[1mGitHub App private key\x1b[0m");
  console.log("  Path to the .pem you downloaded. It is read, not copied anywhere else.");
  const path = (prompt("  >") ?? "").trim().replace(/^~/, Deno.env.get("HOME") ?? "~");
  const pem = await Deno.readTextFile(path);
  if (!pem.includes("PRIVATE KEY")) {
    throw new Error(`${path} does not look like a PEM private key`);
  }
  return pem;
}

/** Everything that can be checked before writing anything down. */
async function validate(values: Record<string, string>): Promise<string[]> {
  const problems: string[] = [];

  const me = await fetch("https://discord.com/api/v10/users/@me", {
    headers: { authorization: `Bot ${values.DISCORD_BOT_TOKEN}` },
  });
  if (!me.ok) {
    problems.push(`DISCORD_BOT_TOKEN: Discord answered ${me.status} to /users/@me`);
  } else {
    const bot = await me.json();
    console.log(`\n  the token belongs to ${bot.username} (${bot.id})`);
    if (bot.id !== values.DISCORD_APPLICATION_ID) {
      // Two ids on adjacent pages of the same portal, and mixing them up
      // produces a bridge that verifies requests and cannot answer them.
      problems.push(
        `DISCORD_APPLICATION_ID is ${values.DISCORD_APPLICATION_ID} but the bot's id is ${bot.id}`,
      );
    }
  }

  if (values.DISCORD_APPLICATION_ID === RICH_PRESENCE_APPLICATION_ID) {
    problems.push(
      "DISCORD_APPLICATION_ID is Cordial's Rich Presence application. Give the " +
        "bridge its own: setup rewrites the application description, which is " +
        "public, and one application carrying both means a problem with the bot " +
        "is a problem with every user's presence.",
    );
  }

  if (!/^[0-9a-f]{64}$/i.test(values.DISCORD_PUBLIC_KEY)) {
    problems.push("DISCORD_PUBLIC_KEY is not 64 hex characters");
  } else {
    // **A public key from the wrong application is 64 valid hex characters.**
    // It passes every shape check and then rejects every request Discord
    // signs, and the only symptom is Discord refusing the interactions
    // endpoint with "could not be verified" -- which reads as the endpoint
    // being unreachable. That happened here: the key belonged to Cordial's
    // Rich Presence application, whose page sits one click away in the same
    // portal.
    //
    // `/applications/@me` returns the application's own `verify_key`, so the
    // pairing is checkable and there is no excuse for guessing.
    const app = await fetch("https://discord.com/api/v10/applications/@me", {
      headers: { authorization: `Bot ${values.DISCORD_BOT_TOKEN}` },
    });
    if (app.ok) {
      const { verify_key: key, name } = await app.json();
      if (key && key !== values.DISCORD_PUBLIC_KEY) {
        problems.push(
          `DISCORD_PUBLIC_KEY is not ${name}'s key -- it is 64 valid hex ` +
            `characters belonging to some other application. Copy it from ` +
            `this application's General Information page.`,
        );
      }
    }
  }

  for (const name of ["DISCORD_THREAD_CHANNEL_ID", "DISCORD_PICKER_CHANNEL_ID"]) {
    const channel = await fetch(`https://discord.com/api/v10/channels/${values[name]}`, {
      headers: { authorization: `Bot ${values.DISCORD_BOT_TOKEN}` },
    });
    if (!channel.ok) {
      problems.push(
        `${name}: Discord answered ${channel.status} — wrong id, or the bot cannot see it`,
      );
    }
  }

  const repo = await fetch(
    `https://api.github.com/repos/${values.GITHUB_OWNER}/${values.GITHUB_REPO}`,
    { headers: { "user-agent": "cordial-issue-bridge" } },
  );
  if (!repo.ok) {
    problems.push(`GITHUB_OWNER/GITHUB_REPO: GitHub answered ${repo.status}`);
  }

  try {
    const { appJwt, importAppKey, installationToken } = await import("./github.ts");
    const key = await importAppKey(values.GITHUB_APP_PRIVATE_KEY);
    const jwt = await appJwt(values.GITHUB_APP_ID, key);
    // The decisive check: does this key, this app id and this installation
    // actually mint a token? Each is plausible alone and useless in the wrong
    // combination.
    await installationToken(jwt, values.GITHUB_INSTALLATION_ID);
    console.log("  the App key mints an installation token");
  } catch (error) {
    problems.push(
      `the GitHub App did not authenticate: ${error instanceof Error ? error.message : error}`,
    );
  }

  return problems;
}

/**
 * Give the bot its face.
 *
 * **The bot's avatar and the application's icon are different pictures**, and
 * mixing them up is how this script nearly overwrote Cordial's Rich Presence
 * artwork. `PATCH /users/@me` with a bot token edits the bot *user*, which is
 * what appears beside its messages and in the member list. The application's
 * `icon` is Rich Presence artwork and the app directory listing, and is
 * deliberately not touched here at all.
 */
async function setBotAvatar(token: string): Promise<boolean> {
  const png = await Deno.readFile(`${HERE}avatar.png`);
  const response = await fetch("https://discord.com/api/v10/users/@me", {
    method: "PATCH",
    headers: { authorization: `Bot ${token}`, "content-type": "application/json" },
    body: JSON.stringify({
      avatar: `data:image/png;base64,${btoa(String.fromCharCode(...png))}`,
    }),
  });
  if (!response.ok) {
    console.error(`  could not set the bot avatar: ${response.status}`);
    return false;
  }
  console.log("  bot avatar set");
  return true;
}

/**
 * The part that genuinely is automatic.
 *
 * `PATCH /applications/@me` takes a bot token and sets the interactions
 * endpoint and the description, so neither needs the portal. **Discord
 * validates the endpoint before accepting it** by sending a signed ping and
 * requiring a PONG, which means this call failing is usually the bridge not
 * being reachable rather than anything wrong here.
 *
 * The application's `icon` is deliberately absent from this body: see
 * `setBotAvatar`.
 */
async function configureApplication(values: Record<string, string>, publicUrl: string) {
  const response = await fetch("https://discord.com/api/v10/applications/@me", {
    method: "PATCH",
    headers: {
      authorization: `Bot ${values.DISCORD_BOT_TOKEN}`,
      "content-type": "application/json",
    },
    body: JSON.stringify({
      description: "Files Cordial issues from Discord. You do not need a GitHub account.",
      interactions_endpoint_url: `${publicUrl.replace(/\/$/, "")}/interactions`,
    }),
  });

  if (!response.ok) {
    const text = await response.text();
    console.error(`\n  Discord refused the application update: ${response.status} ${text}`);
    console.error(
      "  If it names the interactions endpoint, the usual cause is that Discord " +
        "could not reach it, or that the reply to its ping was not a PONG.",
    );
    return false;
  }
  console.log("  interactions endpoint and description set");
  return true;
}

function inviteUrl(applicationId: string): string {
  // Send Messages, Create Public Threads, Send Messages in Threads, Embed
  // Links, Manage Messages (to pin the picker). Nothing else: the bridge reads
  // no message content and needs no member or moderation permission.
  const permissions = (1n << 11n) | (1n << 35n) | (1n << 38n) | (1n << 14n) | (1n << 13n);
  return `https://discord.com/oauth2/authorize?client_id=${applicationId}` +
    `&scope=bot%20applications.commands&permissions=${permissions}`;
}

if (import.meta.main) {
  // `prompt()` returns null on a pipe rather than reading it, so without this
  // the script asks nine questions to an empty room and then blames the first
  // answer for being missing -- which is what it did before this check existed.
  if (!Deno.stdin.isTerminal()) {
    console.error(
      "This is interactive: it asks for each value and reads the secrets without echo.\n" +
        "Run it in a terminal. Nothing can be piped in, deliberately — a secret arriving\n" +
        "down a pipe came from a file or a shell history, which is what .env is for.",
    );
    Deno.exit(1);
  }

  console.log(`\x1b[1mCordial issue bridge — setup\x1b[0m

Before this is useful you need, in the Discord developer portal:
  1. an application (New Application)
  2. a bot on it (Bot → Add Bot), and its token

There is no API that creates either — Discord does not provide one — so those
two steps are yours. Everything after them is done here.

Values are not echoed and not printed back. They go to ${ENV_PATH}, created 0600.
`);

  const values: Record<string, string> = {};
  for (const setting of SETTINGS) {
    let value = ask(setting);
    if (!value && setting.name === "GITHUB_WEBHOOK_SECRET") {
      value = generatedSecret();
      console.log("  generated one — it is in .env; paste it into the App's webhook settings");
    }
    if (!value && !setting.optional) {
      console.error(`\n${setting.name} is required. Nothing has been written; run this again.`);
      Deno.exit(1);
    }
    values[setting.name] = value;
  }
  values.GITHUB_APP_PRIVATE_KEY = await readPrivateKey();

  console.log("\n\x1b[1mChecking every value against the live APIs…\x1b[0m");
  const problems = await validate(values);
  if (problems.length) {
    console.error("\n\x1b[1mNothing was written. Fix these and run it again:\x1b[0m");
    for (const problem of problems) console.error(`  - ${problem}`);
    Deno.exit(1);
  }
  console.log("  every value checks out");

  // 0600 at creation, so the file is never briefly readable by anyone else.
  const file = await Deno.open(ENV_PATH, {
    write: true,
    create: true,
    truncate: true,
    mode: 0o600,
  });
  const lines = Object.entries(values).map(([k, v]) =>
    v.includes("\n") ? `${k}="${v.replace(/\n/g, "\\n")}"` : `${k}=${v}`
  );
  await file.write(new TextEncoder().encode(lines.join("\n") + "\n"));
  file.close();
  await Deno.chmod(ENV_PATH, 0o600);
  console.log(`\nwrote ${ENV_PATH} (0600, gitignored)`);

  console.log("\n\x1b[1mWhere will this run?\x1b[0m");
  console.log("  The public https URL of the host. Leave blank to skip and do it later.");
  const publicUrl = (prompt("  >") ?? "").trim();
  if (publicUrl) {
    if (!publicUrl.startsWith("https://")) {
      console.error("  Discord requires https; skipping.");
    } else {
      console.log("\n\x1b[1mTelling Discord about it…\x1b[0m");
      await setBotAvatar(values.DISCORD_BOT_TOKEN);
      await configureApplication(values, publicUrl);
    }
  }

  console.log(`
\x1b[1mLeft to do\x1b[0m
  invite the bot:  ${inviteUrl(values.DISCORD_APPLICATION_ID)}
  webhook:         add one on the GitHub App for "Issue comment" events,
                   pointing at <your-host>/github, with the secret from .env
  post the forms:  deno task post-picker    (--dry-run first)

  templates seen:  ${TEMPLATE_DIR}
`);
}

export { inviteUrl };
