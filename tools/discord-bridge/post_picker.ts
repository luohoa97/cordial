#!/usr/bin/env -S deno run --allow-net --allow-env
/**
 * Post (or repost) the message that offers the forms.
 *
 * Run by hand rather than on a schedule, because it puts a message in a
 * channel people read and that is a decision, not a deployment step. Pin the
 * result; Discord has no "pinned by the bot" concept worth automating.
 *
 * Repost it after adding or renaming a template. Old buttons keep working for
 * forms that still exist and say so politely for ones that do not -- see the
 * unknown-form path in `interactions.ts` -- but a stale message offers the
 * wrong list.
 */
import { pickerMessage } from "./picker.ts";
import { Templates } from "./templates.ts";

function required(name: string): string {
  const value = Deno.env.get(name);
  if (!value) {
    console.error(`${name} is not set; see README.md`);
    Deno.exit(2);
  }
  return value;
}

const owner = required("GITHUB_OWNER");
const repo = required("GITHUB_REPO");
const channel = required("DISCORD_PICKER_CHANNEL_ID");
const token = required("DISCORD_BOT_TOKEN");

const templates = new Templates({
  owner,
  repo,
  ref: Deno.env.get("GITHUB_REF_NAME") ?? "main",
  token: Deno.env.get("GITHUB_READ_TOKEN"),
});
const forms = await templates.forms();
const message = pickerMessage(forms, `https://github.com/${owner}/${repo}`);

if (Deno.args.includes("--dry-run")) {
  console.log(JSON.stringify(message, null, 2));
  Deno.exit(0);
}

const response = await fetch(`https://discord.com/api/v10/channels/${channel}/messages`, {
  method: "POST",
  headers: {
    authorization: `Bot ${token}`,
    "content-type": "application/json",
    "user-agent": "cordial-issue-bridge",
  },
  body: JSON.stringify(message),
});

if (!response.ok) {
  console.error(`Discord answered ${response.status}: ${await response.text()}`);
  Deno.exit(1);
}
console.log(`posted to ${channel}; pin it so people can find it`);
