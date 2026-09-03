import { assert, assertEquals, assertStringIncludes } from "jsr:@std/assert@^1.0.8";
import { inviteUrl } from "./setup.ts";

Deno.test("the invite asks for what the bridge uses and nothing else", () => {
  const url = new URL(inviteUrl("123456789012345678"));
  assertEquals(url.origin + url.pathname, "https://discord.com/oauth2/authorize");
  assertEquals(url.searchParams.get("client_id"), "123456789012345678");

  const permissions = BigInt(url.searchParams.get("permissions")!);
  const bit = (n: bigint) => (permissions & (1n << n)) !== 0n;

  // What it needs: post, embed, open public threads, post in them, pin.
  for (
    const [name, n] of [
      ["Send Messages", 11n],
      ["Embed Links", 14n],
      ["Manage Messages", 13n],
      ["Create Public Threads", 35n],
      ["Send Messages in Threads", 38n],
    ] as const
  ) {
    assert(bit(n), `${name} is missing from the invite`);
  }

  // And what it must never ask for. The bridge reads no message content and
  // moderates nothing; an invite that asked would be a permission somebody has
  // to justify later, and the honest answer would be "it does not use it".
  for (
    const [name, n] of [
      ["Administrator", 3n],
      ["Manage Guild", 5n],
      ["Kick Members", 1n],
      ["Ban Members", 2n],
      ["Manage Channels", 4n],
      ["Mention Everyone", 17n],
    ] as const
  ) {
    assert(!bit(n), `the invite asks for ${name}, which it does not use`);
  }
});

Deno.test("the invite carries the scopes an interaction bot needs", () => {
  const url = new URL(inviteUrl("1"));
  assertStringIncludes(url.searchParams.get("scope")!, "bot");
  assertStringIncludes(url.searchParams.get("scope")!, "applications.commands");
});
