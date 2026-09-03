#!/usr/bin/env -S deno run --allow-net --allow-env --allow-read
/**
 * Register the bridge's message context-menu command.
 *
 * Discord will not invent a command; it has to be uploaded once per
 * application, and again whenever its name changes. `PUT` replaces the whole
 * set, which is deliberate -- it means a command removed from this file
 * disappears from Discord rather than lingering as a button that errors.
 *
 * **Why a context menu and not thread mirroring.** The obvious wish is for
 * every reply in an issue thread to become a comment on the issue. The bridge
 * cannot do that and should not: it holds no gateway connection, so ordinary
 * messages never reach it at all, and getting them would mean a persistent
 * process plus the privileged Message Content intent -- ingesting a channel's
 * whole conversation to catch the few lines meant for the tracker. Right-click
 * → Apps → this command sends one message, chosen by a person, as an
 * interaction. No intent, no gateway, and the issue stays free of "same here".
 */
const MESSAGE_COMMAND = 3;

export const COMMANDS = [
  {
    name: "Add to the issue",
    type: MESSAGE_COMMAND,
    // Guilds only: there is no issue thread in a DM.
    contexts: [0],
    integration_types: [0],
  },
];

if (import.meta.main) {
  const token = Deno.env.get("DISCORD_BOT_TOKEN");
  const application = Deno.env.get("DISCORD_APPLICATION_ID");
  if (!token || !application) {
    console.error("DISCORD_BOT_TOKEN and DISCORD_APPLICATION_ID must be set");
    Deno.exit(2);
  }

  const response = await fetch(
    `https://discord.com/api/v10/applications/${application}/commands`,
    {
      method: "PUT",
      headers: { authorization: `Bot ${token}`, "content-type": "application/json" },
      body: JSON.stringify(COMMANDS),
    },
  );
  if (!response.ok) {
    console.error(`Discord answered ${response.status}: ${await response.text()}`);
    Deno.exit(1);
  }
  for (const command of await response.json()) {
    console.log(`registered: "${command.name}" (type ${command.type}, id ${command.id})`);
  }
}
